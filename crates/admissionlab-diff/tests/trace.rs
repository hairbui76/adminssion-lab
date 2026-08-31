//! Task 4.6 webhook-trace classification tests.
//!
//! The interesting assertions here are again the negative ones. A
//! webhook whose patch changed is easy; what this task has to get right
//! is everything it must *refuse* to claim:
//!
//! - a dimension only one side observed is not a behavior difference;
//! - a missing latency is never a zero, and therefore never a
//!   comparison;
//! - an invocation missing from a *partial* trace is not a removal;
//! - a trace with no usable evidence is not an empty trace.
//!
//! Each test's doc comment names what would make it fail.

use std::time::Duration;

use admissionlab_admission::trace::{TraceEvidence, WebhookOutcome};
use admissionlab_diff::{
    SemanticChangeKind, TraceComparability, UNATTRIBUTED_FIXTURE, diff_admission_trace,
    trace_comparability,
};
use admissionlab_normalize::{NormalizedTrace, NormalizedWebhookInvocation};
use admissionlab_spec::LatencyPolicy;
use json_patch::PatchOperation;
use serde_json::json;

// ---------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------

/// The Alpha latency policy the roadmap states for Task 4.6 Step 3: at
/// least 100ms slower *and* at least twice the baseline.
///
/// Written out here rather than taken from `LatencyPolicy::default()`
/// (which is now this same pair) so these tests keep pinning the
/// roadmap's numbers even if the spec crate's `Default` drifts --
/// `default_policy_matches_the_alpha_thresholds` asserts the two agree.
fn alpha_policy() -> LatencyPolicy {
    LatencyPolicy {
        absolute_increase: Duration::from_millis(100),
        relative_multiplier: 2.0,
    }
}

/// The zero-tolerance policy a user may legitimately configure
/// (`absoluteIncrease: 0` / `relativeMultiplier: 1.0`) --
/// `zero_tolerance_policy_still_ignores_an_unchanged_duration` exists
/// for it.
fn zero_tolerance_policy() -> LatencyPolicy {
    LatencyPolicy {
        absolute_increase: Duration::ZERO,
        relative_multiplier: 1.0,
    }
}

/// Fails if `LatencyPolicy::default()` stops being the Alpha 100ms/2x
/// pair ROADMAP Task 4.6 Step 3 fixes (reconciled by the PM after Task
/// 4.6 found the original `Default` was zero-tolerance).
#[test]
fn default_policy_matches_the_alpha_thresholds() {
    assert_eq!(LatencyPolicy::default(), alpha_policy());
}

/// One invocation of `webhook` in round 0 at index 0 that allowed the
/// request, mutated nothing, and whose latency was not measured.
///
/// Every test starts from this and changes exactly the dimension it is
/// about, so an observed difference is the one the test introduced.
fn invocation(webhook: &str) -> NormalizedWebhookInvocation {
    NormalizedWebhookInvocation {
        configuration: "policy.example.com".to_owned(),
        webhook: webhook.to_owned(),
        round: 0,
        index: 0,
        mutated: Some(false),
        patch: None,
        latency: None,
        outcome: WebhookOutcome::Allowed,
    }
}

/// A fully observed trace of `invocations`.
fn observed(invocations: Vec<NormalizedWebhookInvocation>) -> NormalizedTrace {
    NormalizedTrace {
        evidence: TraceEvidence::Observed,
        invocations,
    }
}

/// The same trace with a weaker evidence level.
fn with_evidence(trace: &NormalizedTrace, evidence: TraceEvidence) -> NormalizedTrace {
    NormalizedTrace {
        evidence,
        invocations: trace.invocations.clone(),
    }
}

/// A JSON Patch that adds an init container -- the canonical mutation
/// this project exists to watch.
fn init_container_patch(image: &str) -> Vec<PatchOperation> {
    serde_json::from_value(json!([{
        "op": "add",
        "path": "/spec/initContainers",
        "value": [{"name": "setup", "image": image}]
    }]))
    .expect("a well-formed JSON Patch document")
}

// ---------------------------------------------------------------------
// Step 1: invocation, sequence, and patch changes
// ---------------------------------------------------------------------

/// Fails if two identical chains produce any claim at all.
#[test]
fn identical_traces_produce_no_change() {
    let trace = observed(vec![invocation("mutate.example.com")]);

    assert_eq!(
        diff_admission_trace(&trace, &trace, &alpha_policy()),
        Vec::new()
    );
    assert_eq!(
        trace_comparability(&trace, &trace),
        TraceComparability::Comparable
    );
}

/// Fails if a webhook that now answers with a different patch is not
/// reported, if the claim is not scoped to that webhook by name, or if
/// either side's patch is missing from the payload a reader compares.
#[test]
fn changed_patch_is_one_invocation_change() {
    let mut before = invocation("mutate.example.com");
    before.mutated = Some(true);
    before.patch = Some(init_container_patch("setup:1"));
    let mut after = before.clone();
    after.patch = Some(init_container_patch("setup:2"));

    let changes = diff_admission_trace(
        &observed(vec![before]),
        &observed(vec![after]),
        &alpha_policy(),
    );

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(
        changes[0].kind,
        SemanticChangeKind::WebhookInvocationChanged
    );
    assert_eq!(changes[0].subject.as_deref(), Some("mutate.example.com"));
    assert_eq!(
        changes[0].object_path, None,
        "a webhook invocation is not a field of the compared object"
    );
    assert_eq!(changes[0].fixture_id.as_str(), UNATTRIBUTED_FIXTURE);
    let baseline = changes[0].baseline.as_ref().expect("baseline payload");
    let candidate = changes[0].candidate.as_ref().expect("candidate payload");
    assert_eq!(baseline["patch"][0]["value"][0]["image"], json!("setup:1"));
    assert_eq!(candidate["patch"][0]["value"][0]["image"], json!("setup:2"));
}

/// Fails if a webhook that changed position within its round is not
/// reported: a mutating webhook sees whatever ran before it, so order is
/// behavior.
#[test]
fn changed_index_within_a_round_is_an_invocation_change() {
    let before = invocation("mutate.example.com");
    let mut after = before.clone();
    after.index = 2;

    let changes = diff_admission_trace(
        &observed(vec![before]),
        &observed(vec![after]),
        &alpha_policy(),
    );

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(
        changes[0].kind,
        SemanticChangeKind::WebhookInvocationChanged
    );
}

/// Fails if a webhook that only one side invoked is missed, or if the
/// side that never invoked it acquires a fabricated payload.
#[test]
fn invocation_present_on_one_side_only_is_reported_once() {
    let baseline = observed(vec![invocation("mutate.example.com")]);
    let candidate = observed(vec![
        invocation("mutate.example.com"),
        invocation("inject.example.com"),
    ]);

    let changes = diff_admission_trace(&baseline, &candidate, &alpha_policy());

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(
        changes[0].kind,
        SemanticChangeKind::WebhookInvocationChanged
    );
    assert_eq!(changes[0].subject.as_deref(), Some("inject.example.com"));
    assert_eq!(
        changes[0].baseline, None,
        "the baseline never invoked this webhook, and has no payload to show"
    );
    assert!(changes[0].candidate.is_some());
}

/// Fails if a dimension only one side observed is reported as a
/// behavior change.
///
/// Both cases here are *evidence* differences: the baseline recorded a
/// patch and a mutated flag, the candidate's evidence could not say.
/// Claiming a regression would tell a user their stack changed when only
/// this lab's visibility into it did.
#[test]
fn a_dimension_only_one_side_observed_is_never_a_difference() {
    let mut before = invocation("mutate.example.com");
    before.mutated = Some(true);
    before.patch = Some(init_container_patch("setup:1"));
    let mut after = before.clone();
    after.mutated = None;
    after.patch = None;

    assert_eq!(
        diff_admission_trace(
            &observed(vec![before]),
            &observed(vec![after]),
            &alpha_policy()
        ),
        Vec::new()
    );
}

/// Fails if a `mutated` flag both sides observed, and which disagrees,
/// is not reported -- the complement of the test above, and the reason
/// that one is a rule about evidence rather than a blanket exemption.
#[test]
fn a_mutated_flag_both_sides_observed_is_compared() {
    let mut before = invocation("mutate.example.com");
    before.mutated = Some(true);
    let mut after = before.clone();
    after.mutated = Some(false);

    let changes = diff_admission_trace(
        &observed(vec![before]),
        &observed(vec![after]),
        &alpha_policy(),
    );

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(
        changes[0].kind,
        SemanticChangeKind::WebhookInvocationChanged
    );
}

// ---------------------------------------------------------------------
// Step 2: failures
// ---------------------------------------------------------------------

/// Fails if a webhook that the candidate could not call is not reported
/// as a failure, or if it is *also* reported as a generic invocation
/// change -- one fact must produce one claim.
#[test]
fn candidate_side_error_is_a_webhook_failure() {
    let before = invocation("mutate.example.com");
    let mut after = before.clone();
    after.outcome = WebhookOutcome::Errored;

    let changes = diff_admission_trace(
        &observed(vec![before]),
        &observed(vec![after]),
        &alpha_policy(),
    );

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(changes[0].kind, SemanticChangeKind::WebhookFailed);
    assert_eq!(changes[0].subject.as_deref(), Some("mutate.example.com"));
}

/// Fails if a webhook that errored in the baseline and answers cleanly
/// in the candidate is called a failure.
///
/// Nothing failed on the candidate side, so `webhook_failed` would be
/// simply untrue. The webhook's observed behavior did change, which is
/// what is reported; whether an improvement matters is
/// `admissionlab-policy`'s decision.
#[test]
fn baseline_side_error_that_clears_is_a_change_not_a_failure() {
    let mut before = invocation("mutate.example.com");
    before.outcome = WebhookOutcome::Errored;
    let after = invocation("mutate.example.com");

    let changes = diff_admission_trace(
        &observed(vec![before]),
        &observed(vec![after]),
        &alpha_policy(),
    );

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(
        changes[0].kind,
        SemanticChangeKind::WebhookInvocationChanged,
        "an improvement is not a failure"
    );
}

/// Fails if a candidate error is called *newly* failing when the
/// baseline's own outcome was never established.
#[test]
fn candidate_error_against_an_unknown_baseline_claims_nothing() {
    let mut before = invocation("mutate.example.com");
    before.outcome = WebhookOutcome::Unknown;
    let mut after = invocation("mutate.example.com");
    after.outcome = WebhookOutcome::Errored;

    assert_eq!(
        diff_admission_trace(
            &observed(vec![before]),
            &observed(vec![after]),
            &alpha_policy()
        ),
        Vec::new()
    );
}

// ---------------------------------------------------------------------
// Steps 3 and 4: latency
// ---------------------------------------------------------------------

/// Builds a pair of traces for one webhook with the two given latencies.
fn latency_pair(
    before: Option<Duration>,
    after: Option<Duration>,
) -> (NormalizedTrace, NormalizedTrace) {
    let mut baseline = invocation("mutate.example.com");
    baseline.latency = before;
    let mut candidate = invocation("mutate.example.com");
    candidate.latency = after;
    (observed(vec![baseline]), observed(vec![candidate]))
}

/// Fails if a candidate that is both 400ms slower and five times slower
/// is not reported, or if the payload does not carry both observed
/// durations.
#[test]
fn latency_regression_clearing_both_thresholds_is_reported() {
    let (baseline, candidate) = latency_pair(
        Some(Duration::from_millis(100)),
        Some(Duration::from_millis(500)),
    );

    let changes = diff_admission_trace(&baseline, &candidate, &alpha_policy());

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(changes[0].kind, SemanticChangeKind::WebhookLatencyChanged);
    assert_eq!(changes[0].subject.as_deref(), Some("mutate.example.com"));
    assert_eq!(changes[0].baseline, Some(json!({"latency_millis": 100})));
    assert_eq!(changes[0].candidate, Some(json!({"latency_millis": 500})));
}

/// Fails if either threshold alone is enough: the roadmap's Alpha policy
/// is conjunctive, and a slow-but-proportionate or fast-but-multiplied
/// call is not a regression.
#[test]
fn one_threshold_alone_is_not_a_latency_regression() {
    // +400ms, but only 1.66x.
    let (baseline, candidate) = latency_pair(
        Some(Duration::from_millis(600)),
        Some(Duration::from_millis(1000)),
    );
    assert_eq!(
        diff_admission_trace(&baseline, &candidate, &alpha_policy()),
        Vec::new(),
        "clearing only the absolute threshold is not a regression"
    );

    // 5x, but only +20ms.
    let (baseline, candidate) = latency_pair(
        Some(Duration::from_millis(5)),
        Some(Duration::from_millis(25)),
    );
    assert_eq!(
        diff_admission_trace(&baseline, &candidate, &alpha_policy()),
        Vec::new(),
        "clearing only the relative threshold is not a regression"
    );
}

/// Fails if a missing latency on either side produces a latency claim.
///
/// This is Task 4.6 Step 4 and Global Constraint 15 in one assertion: a
/// `None` substituted with `Duration::ZERO` would make the first case a
/// spectacular fabricated regression and the second a fabricated
/// improvement.
#[test]
fn a_missing_latency_is_never_compared_and_never_a_zero() {
    let (baseline, candidate) = latency_pair(None, Some(Duration::from_secs(30)));
    assert_eq!(
        diff_admission_trace(&baseline, &candidate, &alpha_policy()),
        Vec::new(),
        "an unmeasured baseline cannot be exceeded"
    );

    let (baseline, candidate) = latency_pair(Some(Duration::from_millis(5)), None);
    assert_eq!(
        diff_admission_trace(&baseline, &candidate, &alpha_policy()),
        Vec::new(),
        "an unmeasured candidate cannot have got slower"
    );

    let (baseline, candidate) = latency_pair(None, None);
    assert_eq!(
        diff_admission_trace(&baseline, &candidate, &alpha_policy()),
        Vec::new()
    );
}

/// Fails if a zero-tolerance policy turns an unchanged (or improved)
/// latency into a regression.
///
/// Under `absoluteIncrease: 0` / `relativeMultiplier: 1.0` -- a
/// configuration a user may legitimately write -- both configured
/// thresholds are satisfied by an identical duration. A regression is an
/// increase, which is why the comparison requires one before it consults
/// either threshold. (The `Default` is the Alpha 100ms/2x pair, so the
/// zero-tolerance policy is spelled out explicitly here.)
#[test]
fn zero_tolerance_policy_still_ignores_an_unchanged_duration() {
    let (baseline, candidate) = latency_pair(
        Some(Duration::from_millis(40)),
        Some(Duration::from_millis(40)),
    );
    assert_eq!(
        diff_admission_trace(&baseline, &candidate, &zero_tolerance_policy()),
        Vec::new()
    );

    let (baseline, candidate) = latency_pair(
        Some(Duration::from_millis(40)),
        Some(Duration::from_millis(10)),
    );
    assert_eq!(
        diff_admission_trace(&baseline, &candidate, &zero_tolerance_policy()),
        Vec::new(),
        "an improvement is never a latency regression"
    );
}

/// Fails if a webhook whose behavior *and* latency both moved does not
/// produce both claims, in the documented order.
#[test]
fn behavior_and_latency_are_independent_claims() {
    let mut before = invocation("mutate.example.com");
    before.mutated = Some(true);
    before.latency = Some(Duration::from_millis(50));
    let mut after = before.clone();
    after.mutated = Some(false);
    after.latency = Some(Duration::from_millis(400));

    let changes = diff_admission_trace(
        &observed(vec![before]),
        &observed(vec![after]),
        &alpha_policy(),
    );

    let kinds: Vec<SemanticChangeKind> = changes.iter().map(|change| change.kind).collect();
    assert_eq!(
        kinds,
        vec![
            SemanticChangeKind::WebhookInvocationChanged,
            SemanticChangeKind::WebhookLatencyChanged,
        ]
    );
}

// ---------------------------------------------------------------------
// Step 5: evidence completeness
// ---------------------------------------------------------------------

/// Fails if a trace with no usable evidence is compared as though it
/// were an empty chain -- which would report every invocation on the
/// other side as removed, a whole fabricated regression built out of a
/// missing audit backend.
#[test]
fn unavailable_evidence_produces_no_claims_at_all() {
    let baseline = observed(vec![invocation("mutate.example.com")]);
    let candidate = with_evidence(&observed(Vec::new()), TraceEvidence::Unavailable);

    assert_eq!(
        diff_admission_trace(&baseline, &candidate, &alpha_policy()),
        Vec::new()
    );
    assert_eq!(
        trace_comparability(&baseline, &candidate),
        TraceComparability::Incomparable {
            baseline: TraceEvidence::Observed,
            candidate: TraceEvidence::Unavailable,
        },
        "the silence must be reported as incomparability, not as agreement"
    );
}

/// Fails if an invocation missing from a *partial* trace is reported as
/// removed.
///
/// "Missing" is exactly what partial evidence is allowed to be, so its
/// absence is not proof that the webhook stopped running. The
/// comparability seam is what tells a caller the silence is qualified.
#[test]
fn partial_evidence_never_turns_an_absence_into_a_removal() {
    let baseline = observed(vec![
        invocation("mutate.example.com"),
        invocation("inject.example.com"),
    ]);
    let candidate = with_evidence(
        &observed(vec![invocation("mutate.example.com")]),
        TraceEvidence::Partial,
    );

    assert_eq!(
        diff_admission_trace(&baseline, &candidate, &alpha_policy()),
        Vec::new()
    );
    assert_eq!(
        trace_comparability(&baseline, &candidate),
        TraceComparability::Partial {
            baseline: TraceEvidence::Observed,
            candidate: TraceEvidence::Partial,
        }
    );
    assert!(!trace_comparability(&baseline, &candidate).absence_is_evidence());
}

/// Fails if partial evidence suppresses a comparison between two
/// invocations both sides actually observed.
///
/// Partiality weakens absence-based claims and nothing else: what was
/// seen on both sides was still seen.
#[test]
fn partial_evidence_still_compares_what_both_sides_observed() {
    let mut before = invocation("mutate.example.com");
    before.mutated = Some(true);
    let mut after = before.clone();
    after.mutated = Some(false);

    let changes = diff_admission_trace(
        &with_evidence(&observed(vec![before]), TraceEvidence::Partial),
        &observed(vec![after]),
        &alpha_policy(),
    );

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(
        changes[0].kind,
        SemanticChangeKind::WebhookInvocationChanged
    );
}

// ---------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------

/// Fails if the result depends on the order the capture pipeline
/// happened to record invocations in, which Global Constraint 7 and a
/// stable report both rest on.
#[test]
fn output_order_is_by_webhook_and_round_not_by_capture_order() {
    let mut zebra = invocation("zebra.example.com");
    zebra.index = 1;
    let alpha = invocation("alpha.example.com");

    let baseline = observed(vec![zebra.clone(), alpha.clone()]);
    let mut zebra_after = zebra.clone();
    zebra_after.index = 0;
    let mut alpha_after = alpha.clone();
    alpha_after.index = 1;
    let candidate = observed(vec![alpha_after, zebra_after]);

    let changes = diff_admission_trace(&baseline, &candidate, &alpha_policy());

    let subjects: Vec<&str> = changes
        .iter()
        .filter_map(|change| change.subject.as_deref())
        .collect();
    assert_eq!(subjects, vec!["alpha.example.com", "zebra.example.com"]);
}
