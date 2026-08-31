//! Task 4.7 first-divergence attribution tests.
//!
//! Attribution is the most tempting place in this product to turn a
//! correlation into a cause, so most of what follows is about the
//! grading rather than the finding: the same divergence must come back
//! `observed` from two complete chains and `inferred` from a partial
//! one, and no chain that could not be read may produce a position at
//! all.
//!
//! Each test's doc comment names what would make it fail.

use admissionlab_admission::trace::{TraceEvidence, WebhookOutcome};
use admissionlab_diff::{DivergenceConfidence, first_divergence, first_divergence_with_objects};
use admissionlab_normalize::{NormalizedTrace, NormalizedWebhookInvocation};
use json_patch::PatchOperation;
use serde_json::json;

// ---------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------

/// One invocation of `webhook` at `(round, index)` that allowed the
/// request, was observed not to mutate, and carried no patch.
fn invocation(webhook: &str, round: u32, index: u32) -> NormalizedWebhookInvocation {
    NormalizedWebhookInvocation {
        configuration: "injector.example.com".to_owned(),
        webhook: webhook.to_owned(),
        round,
        index,
        mutated: Some(false),
        patch: None,
        latency: None,
        outcome: WebhookOutcome::Allowed,
    }
}

/// The same invocation, observed to have mutated the object with
/// `patch`.
fn mutating(
    webhook: &str,
    round: u32,
    index: u32,
    patch: Vec<PatchOperation>,
) -> NormalizedWebhookInvocation {
    NormalizedWebhookInvocation {
        mutated: Some(true),
        patch: Some(patch),
        ..invocation(webhook, round, index)
    }
}

fn trace(
    evidence: TraceEvidence,
    invocations: Vec<NormalizedWebhookInvocation>,
) -> NormalizedTrace {
    NormalizedTrace {
        evidence,
        invocations,
    }
}

/// The canonical mutation this project exists to watch: a webhook that
/// injects an init container.
fn add_init_containers() -> Vec<PatchOperation> {
    serde_json::from_value(json!([{
        "op": "add",
        "path": "/spec/initContainers",
        "value": [{"name": "setup", "image": "setup:1"}]
    }]))
    .expect("a well-formed JSON Patch document")
}

/// The candidate-side patch that no longer injects it: the same webhook
/// now removes the field instead.
fn remove_init_containers() -> Vec<PatchOperation> {
    serde_json::from_value(json!([{"op": "remove", "path": "/spec/initContainers"}]))
        .expect("a well-formed JSON Patch document")
}

// ---------------------------------------------------------------------
// Steps 1, 2 and 5: locating the divergence
// ---------------------------------------------------------------------

/// Fails if two identical chains produce an attribution.
#[test]
fn identical_chains_have_no_first_divergence() {
    let chain = trace(
        TraceEvidence::Observed,
        vec![mutating("inject", 0, 0, add_init_containers())],
    );

    assert_eq!(first_divergence(&chain, &chain), None);
}

/// The canonical Task 4.7 Step 5 case: the same webhook, at the same
/// position, stops injecting `/spec/initContainers` and removes them
/// instead.
///
/// Fails if the divergence is missed, attributed to the wrong position,
/// graded below `observed` when both chains were fully observed, or
/// explained in a way that does not name the webhook and the round a
/// human would go looking at.
#[test]
fn patch_that_stops_injecting_init_containers_is_observed_at_that_webhook() {
    let baseline = trace(
        TraceEvidence::Observed,
        vec![mutating("inject", 0, 0, add_init_containers())],
    );
    let candidate = trace(
        TraceEvidence::Observed,
        vec![mutating("inject", 0, 0, remove_init_containers())],
    );

    let evidence = first_divergence(&baseline, &candidate).expect("a divergence");

    assert_eq!(evidence.confidence, DivergenceConfidence::Observed);
    assert_eq!(evidence.baseline_position, Some((0, 0)));
    assert_eq!(evidence.candidate_position, Some((0, 0)));
    assert_eq!(evidence.baseline_webhook.as_deref(), Some("inject"));
    assert_eq!(evidence.candidate_webhook.as_deref(), Some("inject"));
    assert!(
        evidence.explanation.contains("`inject`")
            && evidence.explanation.contains("round 0")
            && evidence.explanation.contains("different patch"),
        "the explanation must name the webhook, the round, and what differed: {}",
        evidence.explanation
    );
}

/// Fails if two different webhooks occupying the same position are not
/// reported, or if the explanation names only one of them.
#[test]
fn different_webhooks_at_the_same_position_diverge() {
    let baseline = trace(TraceEvidence::Observed, vec![invocation("validate", 0, 0)]);
    let candidate = trace(TraceEvidence::Observed, vec![invocation("inject", 0, 0)]);

    let evidence = first_divergence(&baseline, &candidate).expect("a divergence");

    assert_eq!(evidence.confidence, DivergenceConfidence::Observed);
    assert_eq!(evidence.baseline_webhook.as_deref(), Some("validate"));
    assert_eq!(evidence.candidate_webhook.as_deref(), Some("inject"));
    assert!(
        evidence.explanation.contains("`validate`") && evidence.explanation.contains("`inject`"),
        "both webhooks must be named: {}",
        evidence.explanation
    );
}

/// Fails if a webhook observed to mutate on one side and not the other
/// is missed, or if the explanation does not say which side mutated.
#[test]
fn a_mutated_flag_that_disagrees_diverges() {
    let baseline = trace(
        TraceEvidence::Observed,
        vec![mutating("inject", 0, 0, add_init_containers())],
    );
    let mut quiet = invocation("inject", 0, 0);
    quiet.patch = Some(add_init_containers());
    let candidate = trace(TraceEvidence::Observed, vec![quiet]);

    let evidence = first_divergence(&baseline, &candidate).expect("a divergence");

    assert_eq!(evidence.confidence, DivergenceConfidence::Observed);
    assert!(
        evidence
            .explanation
            .contains("on the baseline side but not on the candidate side"),
        "the direction must be stated: {}",
        evidence.explanation
    );
}

/// Fails if an invocation only one chain contains is not a divergence at
/// its position, or if the side that has no invocation there acquires a
/// fabricated position or webhook name.
#[test]
fn an_invocation_one_chain_does_not_have_diverges_at_that_position() {
    let baseline = trace(
        TraceEvidence::Observed,
        vec![invocation("validate", 0, 0), invocation("inject", 0, 1)],
    );
    let candidate = trace(TraceEvidence::Observed, vec![invocation("validate", 0, 0)]);

    let evidence = first_divergence(&baseline, &candidate).expect("a divergence");

    assert_eq!(evidence.confidence, DivergenceConfidence::Observed);
    assert_eq!(evidence.baseline_position, Some((0, 1)));
    assert_eq!(evidence.candidate_position, None);
    assert_eq!(evidence.baseline_webhook.as_deref(), Some("inject"));
    assert_eq!(evidence.candidate_webhook, None);
    assert!(
        evidence
            .explanation
            .contains("has no invocation at that position"),
        "the explanation must say what was absent: {}",
        evidence.explanation
    );
}

/// Fails if a later divergence is reported ahead of an earlier one:
/// "first" is by `(round, index)`, and a second-round difference is
/// usually a consequence of a first-round one.
#[test]
fn the_earliest_position_wins() {
    let baseline = trace(
        TraceEvidence::Observed,
        vec![
            invocation("validate", 0, 0),
            invocation("inject", 0, 1),
            invocation("finalize", 1, 0),
        ],
    );
    let candidate = trace(
        TraceEvidence::Observed,
        vec![
            invocation("validate", 0, 0),
            invocation("rewrite", 0, 1),
            invocation("audit", 1, 0),
        ],
    );

    let evidence = first_divergence(&baseline, &candidate).expect("a divergence");

    assert_eq!(evidence.baseline_position, Some((0, 1)));
    assert_eq!(evidence.baseline_webhook.as_deref(), Some("inject"));
}

/// Fails if a dimension only one side observed is treated as a
/// divergence.
///
/// The candidate here recorded no patch at all, which is an evidence gap
/// rather than a webhook that stopped patching -- and the `mutated`
/// flags agree, so nothing was established.
#[test]
fn a_dimension_only_one_side_observed_is_not_a_divergence() {
    let mut before = invocation("inject", 0, 0);
    before.patch = Some(add_init_containers());
    let after = invocation("inject", 0, 0);

    assert_eq!(
        first_divergence(
            &trace(TraceEvidence::Observed, vec![before]),
            &trace(TraceEvidence::Observed, vec![after])
        ),
        None
    );
}

// ---------------------------------------------------------------------
// Step 4: partial and unavailable evidence
// ---------------------------------------------------------------------

/// Fails if an absence-based finding drawn from a partial chain is
/// presented as directly observed.
///
/// An invocation the evidence simply failed to record shifts every
/// position after it, so this finding may be an artifact of the gap. It
/// is still worth reporting -- it is just not proof.
#[test]
fn partial_evidence_caps_an_absence_at_inferred() {
    let baseline = trace(
        TraceEvidence::Observed,
        vec![invocation("validate", 0, 0), invocation("inject", 0, 1)],
    );
    let candidate = trace(TraceEvidence::Partial, vec![invocation("validate", 0, 0)]);

    let evidence = first_divergence(&baseline, &candidate).expect("a divergence");

    assert_eq!(evidence.confidence, DivergenceConfidence::Inferred);
    assert_eq!(evidence.baseline_position, Some((0, 1)));
    assert!(
        evidence.explanation.contains("partial"),
        "the explanation must say why the claim is weaker: {}",
        evidence.explanation
    );
}

/// Fails if a patch difference on the same webhook is downgraded under
/// partial evidence.
///
/// This is Task 4.7 Step 4's own exception, and the boundary this file
/// exists to pin: the claim is that one named webhook responded with two
/// different patches, both of which were seen. Nothing about it depends
/// on the positions lining up, so an incomplete chain elsewhere does not
/// weaken it.
#[test]
fn a_directly_observed_patch_difference_stays_observed_under_partial_evidence() {
    let baseline = trace(
        TraceEvidence::Partial,
        vec![mutating("inject", 0, 0, add_init_containers())],
    );
    let candidate = trace(
        TraceEvidence::Partial,
        vec![mutating("inject", 0, 0, remove_init_containers())],
    );

    let evidence = first_divergence(&baseline, &candidate).expect("a divergence");

    assert_eq!(evidence.confidence, DivergenceConfidence::Observed);
    assert!(
        !evidence.explanation.contains("partial"),
        "an observed finding must not carry the inferred caveat: {}",
        evidence.explanation
    );
}

/// Fails if the exception above is quietly widened to the `mutated`
/// flag.
///
/// The flag is a summary the audit reconstruction derived; the patch is
/// the observed response content itself, and Step 4 names the patch.
/// Keeping the boundary where it is means the difference between
/// `observed` and `inferred` stays legible.
#[test]
fn a_mutated_flag_difference_is_only_inferred_under_partial_evidence() {
    let baseline = trace(
        TraceEvidence::Partial,
        vec![mutating("inject", 0, 0, add_init_containers())],
    );
    let mut quiet = invocation("inject", 0, 0);
    quiet.patch = Some(add_init_containers());
    let candidate = trace(TraceEvidence::Partial, vec![quiet]);

    let evidence = first_divergence(&baseline, &candidate).expect("a divergence");

    assert_eq!(evidence.confidence, DivergenceConfidence::Inferred);
}

/// Fails if a chain with no usable evidence is walked as though it were
/// an empty chain, which would attribute a divergence at every position
/// of the other side -- a finding built entirely out of a missing audit
/// backend.
///
/// `Unknown` with no position is the honest answer, and it is returned
/// rather than `None` precisely so a caller cannot read the silence as
/// agreement.
#[test]
fn unavailable_evidence_yields_unknown_with_no_position() {
    let baseline = trace(
        TraceEvidence::Observed,
        vec![mutating("inject", 0, 0, add_init_containers())],
    );
    let candidate = trace(TraceEvidence::Unavailable, Vec::new());

    let evidence = first_divergence(&baseline, &candidate).expect("an explicit unknown");

    assert_eq!(evidence.confidence, DivergenceConfidence::Unknown);
    assert_eq!(evidence.baseline_position, None);
    assert_eq!(evidence.candidate_position, None);
    assert_eq!(evidence.baseline_webhook, None);
    assert_eq!(evidence.candidate_webhook, None);
    assert!(
        evidence.explanation.contains("candidate side's")
            && evidence.explanation.contains("unavailable"),
        "the explanation must name the side that could not be read: {}",
        evidence.explanation
    );
}

// ---------------------------------------------------------------------
// Step 3: identical chains, different objects
// ---------------------------------------------------------------------

/// Fails if two identical chains that nevertheless left different
/// objects behind are reported as "no divergence".
///
/// Something mutated the object outside the captured mutating-webhook
/// evidence, or that evidence is incomplete -- and saying exactly that,
/// as `Unknown`, is the whole point of the confidence enum.
#[test]
fn identical_chains_with_differing_objects_are_unknown() {
    let chain = trace(
        TraceEvidence::Observed,
        vec![mutating("inject", 0, 0, add_init_containers())],
    );

    let evidence =
        first_divergence_with_objects(&chain, &chain, true).expect("an explicit unknown");

    assert_eq!(evidence.confidence, DivergenceConfidence::Unknown);
    assert_eq!(evidence.baseline_position, None);
    assert_eq!(evidence.candidate_position, None);
    assert!(
        evidence
            .explanation
            .contains("outside captured mutating-webhook evidence"),
        "the explanation must state the two possibilities: {}",
        evidence.explanation
    );
}

/// Fails if identical chains and identical objects produce anything at
/// all.
#[test]
fn identical_chains_with_identical_objects_have_no_divergence() {
    let chain = trace(
        TraceEvidence::Observed,
        vec![mutating("inject", 0, 0, add_init_containers())],
    );

    assert_eq!(first_divergence_with_objects(&chain, &chain, false), None);
}

/// Fails if the object-level fallback shadows a real, located
/// divergence: a position in the chain is always the better answer.
#[test]
fn a_located_divergence_wins_over_the_object_level_fallback() {
    let baseline = trace(
        TraceEvidence::Observed,
        vec![mutating("inject", 0, 0, add_init_containers())],
    );
    let candidate = trace(
        TraceEvidence::Observed,
        vec![mutating("inject", 0, 0, remove_init_containers())],
    );

    let evidence =
        first_divergence_with_objects(&baseline, &candidate, true).expect("a divergence");

    assert_eq!(evidence.confidence, DivergenceConfidence::Observed);
    assert_eq!(evidence.baseline_position, Some((0, 0)));
}
