//! Task 3.6's behavioural suite for
//! [`admissionlab_admission::correlate::reconstruct_mutating_trace`].
//!
//! Two kinds of input, and the split between them is deliberate.
//!
//! `testdata/audit/mutation-rounds.jsonl` holds only events kube-apiserver
//! can actually write: one round of two webhooks, a reinvocation with the
//! index gaps a real dispatcher leaves behind, a webhook that did not
//! mutate, a `mutated: true` whose patch annotation is missing (what a
//! `Metadata`-level audit policy produces), a fail-open call, a request
//! that ran only validating webhooks, and one that ran none at all. It is
//! checked in rather than built in code so the exact annotation shapes
//! this project claims to parse are reviewable in the diff and cannot
//! drift to match a bug in the parser.
//!
//! Everything upstream *cannot* produce -- a patch annotation with no
//! mutation annotation beside it, a key with non-decimal digits, a
//! payload that is not the documented shape -- is assembled in the test
//! that needs it, by editing a parsed fixture event. Putting those in a
//! file called "a realistic audit log" would be a lie about what a real
//! API server writes, which is exactly the property the fixture exists to
//! preserve.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use admissionlab_admission::correlate::{
    FAILED_OPEN_MUTATION_ANNOTATION_PREFIX, MUTATION_ANNOTATION_PREFIX, PATCH_ANNOTATION_PREFIX,
    TraceError, reconstruct_mutating_trace,
};
use admissionlab_admission::{
    AuditEvent, STAGE_RESPONSE_COMPLETE, TraceEvidence, WebhookInvocation, WebhookOutcome,
};

// ---------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------

/// Path to `testdata/audit/mutation-rounds.jsonl`, which lives at the
/// workspace root rather than inside this crate, mirroring
/// `tests/audit_reader.rs`'s own `basic_jsonl_path` helper.
fn mutation_rounds_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/audit/mutation-rounds.jsonl")
}

/// Every event in `testdata/audit/mutation-rounds.jsonl`, in file order.
fn fixture_events() -> Vec<AuditEvent> {
    let text = std::fs::read_to_string(mutation_rounds_path())
        .expect("read testdata/audit/mutation-rounds.jsonl");
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("parse a mutation-rounds.jsonl line"))
        .collect()
}

/// The `ResponseComplete` event for the fixture object called `name`.
fn response_complete_for(name: &str) -> AuditEvent {
    fixture_events()
        .into_iter()
        .find(|event| {
            event.is_response_complete()
                && event
                    .object_ref
                    .as_ref()
                    .and_then(|object_ref| object_ref.name.as_deref())
                    == Some(name)
        })
        .unwrap_or_else(|| panic!("mutation-rounds.jsonl has a ResponseComplete event for {name}"))
}

/// The `(round, index)` pairs of `invocations`, in order.
fn rounds_and_indexes(invocations: &[WebhookInvocation]) -> Vec<(u32, u32)> {
    invocations
        .iter()
        .map(|invocation| (invocation.round, invocation.index))
        .collect()
}

/// One invocation's patch rendered as JSON, so a test can state the
/// expected operations literally instead of building
/// `json_patch::PatchOperation` values by hand.
fn patch_json(invocation: &WebhookInvocation) -> serde_json::Value {
    serde_json::to_value(&invocation.patch).expect("serialize an observed patch")
}

/// The annotation key naming `(round, index)` under `prefix`.
fn annotation_key(prefix: &str, round: u32, index: u32) -> String {
    format!("{prefix}round_{round}_index_{index}")
}

// ---------------------------------------------------------------------
// Step 2/3: parse the payloads, merge invocation and patch evidence
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: merging invocation and patch
/// evidence by anything other than `(round, index, configuration,
/// webhook)` -- most plausibly by `(round, index)` alone, which would
/// attach index 0's patch to index 1's invocation the moment the two
/// annotations were walked in a different order.
///
/// Also pins the two decisions that make the whole module worth reading:
/// index 0 mutated and is therefore provably `Allowed`, while index 1 was
/// invoked, reported `mutated: false`, and is therefore `Unknown` -- not
/// `Allowed`, because kube-apiserver writes that same annotation from a
/// `defer` after a timeout or a denial.
#[test]
fn one_round_merges_each_patch_onto_its_own_invocation() {
    let trace =
        reconstruct_mutating_trace(&response_complete_for("fixture-one-round")).expect("trace");

    assert_eq!(trace.evidence, TraceEvidence::Observed);
    assert_eq!(rounds_and_indexes(&trace.invocations), [(0, 0), (0, 1)]);

    let mutating = &trace.invocations[0];
    assert_eq!(mutating.configuration, "admissionlab-test-webhook");
    assert_eq!(
        mutating.webhook,
        "mutate-label.test-webhook.admissionlab.dev"
    );
    assert_eq!(mutating.mutated, Some(true));
    assert_eq!(mutating.outcome, WebhookOutcome::Allowed);
    assert_eq!(
        patch_json(mutating),
        serde_json::json!([{
            "op": "add",
            "path": "/metadata/labels/admissionlab.dev~1mutated",
            "value": "true",
        }]),
    );

    let unmutating = &trace.invocations[1];
    assert_eq!(
        unmutating.webhook,
        "mutate-noop.test-webhook.admissionlab.dev"
    );
    assert_eq!(unmutating.mutated, Some(false));
    assert_eq!(
        unmutating.patch, None,
        "no patch annotation was written for this webhook, and none is invented"
    );
    assert_eq!(
        unmutating.outcome,
        WebhookOutcome::Unknown,
        "`mutated: false` is emitted from a deferred call on every exit path -- including a \
         timeout and an explicit denial -- so it proves only that the webhook was invoked"
    );
}

/// The mutation this test exists to kill: ordering invocations by the
/// annotation key's text rather than by its parsed `(round, index)`.
/// `mutation.webhook.admission.k8s.io/round_0_index_10` sorts *before*
/// `...round_0_index_2` as a string, so the fixture deliberately contains
/// both, and a `BTreeMap<String, _>` walk would report them backwards.
///
/// Also pins that an index gap is not evidence of anything: the fixture
/// jumps 0, 2, 10 because kube-apiserver's dispatcher `continue`s past
/// hooks whose match conditions no longer hold without reusing their
/// index. Nothing may be inferred about indexes 1 and 3..=9.
#[test]
fn a_reinvocation_is_ordered_numerically_by_round_then_index() {
    let trace =
        reconstruct_mutating_trace(&response_complete_for("fixture-reinvocation")).expect("trace");

    assert_eq!(trace.evidence, TraceEvidence::Observed);
    assert_eq!(
        rounds_and_indexes(&trace.invocations),
        [(0, 0), (0, 2), (0, 10), (1, 0)],
    );
    assert_eq!(
        trace.invocations[0].webhook, trace.invocations[3].webhook,
        "round 1 is the same webhook reinvoked, not a different one"
    );
    assert_eq!(
        patch_json(&trace.invocations[3]),
        serde_json::json!([{
            "op": "add",
            "path": "/metadata/labels/admissionlab.dev~1reinvoked",
            "value": "true",
        }]),
        "the reinvocation carries its own patch, not round 0's"
    );
    assert_eq!(trace.invocations[2].mutated, Some(false));
    assert_eq!(trace.invocations[2].outcome, WebhookOutcome::Unknown);
}

/// The mutation this test exists to kill: treating a lone
/// `mutated: false` as a webhook that ran and allowed the request, which
/// would report a timed-out or rejecting webhook as a healthy one.
#[test]
fn an_invoked_webhook_that_did_not_mutate_is_unknown_not_allowed() {
    let trace =
        reconstruct_mutating_trace(&response_complete_for("fixture-unmutated")).expect("trace");

    assert_eq!(
        trace.evidence,
        TraceEvidence::Observed,
        "a webhook that ran and did not mutate is a complete observation, not a partial one"
    );
    assert_eq!(trace.invocations.len(), 1);
    assert_eq!(trace.invocations[0].mutated, Some(false));
    assert_eq!(trace.invocations[0].patch, None);
    assert_eq!(trace.invocations[0].outcome, WebhookOutcome::Unknown);
}

// ---------------------------------------------------------------------
// Step 4: partial evidence when a patch is absent though mutated=true
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: reporting a `mutated: true`
/// invocation whose patch annotation never arrived as a complete
/// observation -- or, worse, fabricating an empty patch for it so the
/// field is populated.
///
/// This is what a `Metadata`-level audit policy produces for every
/// mutating webhook, which is the entire reason Global Constraint 18
/// specifies `Request` level.
#[test]
fn a_mutation_without_its_patch_is_partial_and_invents_no_patch() {
    let trace =
        reconstruct_mutating_trace(&response_complete_for("fixture-missing-patch")).expect("trace");

    assert_eq!(trace.evidence, TraceEvidence::Partial);
    assert_eq!(trace.invocations.len(), 1);
    assert_eq!(
        trace.invocations[0].mutated,
        Some(true),
        "the invocation keeps the one fact its annotation does prove"
    );
    assert_eq!(
        trace.invocations[0].patch, None,
        "the patch is unavailable, never an empty patch standing in for it"
    );
    assert_eq!(
        trace.invocations[0].outcome,
        WebhookOutcome::Allowed,
        "`mutated: true` is only reachable past kube-apiserver's own `if !result.Allowed` return"
    );
}

/// The mutation this test exists to kill: dropping a patch annotation
/// that has no mutation annotation beside it, which would silently shrink
/// the chain, or claiming `mutated: false` for it, which would fabricate
/// the one fact the patch annotation cannot establish (whether applying
/// the patch actually changed the object).
///
/// Assembled here rather than checked in, because kube-apiserver writes
/// the mutation annotation from a `defer` on every exit path and so
/// cannot emit this pairing.
#[test]
fn a_patch_without_its_mutation_annotation_is_partial_with_unknown_mutation() {
    let mut event = response_complete_for("fixture-one-round");
    event
        .annotations
        .remove(&annotation_key(MUTATION_ANNOTATION_PREFIX, 0, 0))
        .expect("the fixture has a round 0 index 0 mutation annotation to remove");

    let trace = reconstruct_mutating_trace(&event).expect("trace");

    assert_eq!(trace.evidence, TraceEvidence::Partial);
    assert_eq!(rounds_and_indexes(&trace.invocations), [(0, 0), (0, 1)]);
    assert_eq!(
        trace.invocations[0].mutated, None,
        "a patch annotation proves a patch was applied, never that the object changed"
    );
    assert!(trace.invocations[0].patch.is_some());
    assert_eq!(
        trace.invocations[0].outcome,
        WebhookOutcome::Allowed,
        "the patch annotation is written only past the `if !result.Allowed` return"
    );
}

// ---------------------------------------------------------------------
// Fail-open evidence
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: classifying annotation keys
/// with `contains` rather than `strip_prefix`, and reporting a webhook
/// that could not be reached as `Unknown` (or, worse, `Allowed`) when the
/// event states outright that the call failed and was failed open.
#[test]
fn a_failed_open_call_is_errored_not_unknown() {
    let trace =
        reconstruct_mutating_trace(&response_complete_for("fixture-failed-open")).expect("trace");

    assert_eq!(trace.evidence, TraceEvidence::Observed);
    assert_eq!(trace.invocations.len(), 1);
    assert_eq!(
        trace.invocations[0].webhook,
        "mutate-unreachable.test-webhook.admissionlab.dev"
    );
    assert_eq!(
        trace.invocations[0].mutated,
        Some(false),
        "the deferred mutation annotation is still written, and is still reported verbatim"
    );
    assert_eq!(
        trace.invocations[0].outcome,
        WebhookOutcome::Errored,
        "an errored call must stay distinguishable from a denial: `failurePolicy: Ignore` treats \
         it as an allow while `Fail` treats it as a deny"
    );
}

/// The mutation this test exists to kill: silently discarding a
/// failed-open annotation whose invocation is otherwise unrecorded. Its
/// value is the bare webhook name with no `configuration`, so no
/// invocation can be built from it -- but the event still proves one
/// happened, so the trace must not claim to be complete.
#[test]
fn an_orphan_failed_open_annotation_is_partial() {
    let mut event = response_complete_for("fixture-failed-open");
    event
        .annotations
        .remove(&annotation_key(MUTATION_ANNOTATION_PREFIX, 0, 0))
        .expect("the fixture has a round 0 index 0 mutation annotation to remove");

    let trace = reconstruct_mutating_trace(&event).expect("trace");

    assert_eq!(trace.evidence, TraceEvidence::Partial);
    assert!(
        trace.invocations.is_empty(),
        "a configuration name is never borrowed from another invocation to fill the gap"
    );
}

// ---------------------------------------------------------------------
// Step 5: never infer validating invocations
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: parsing a validating webhook's
/// annotation into an invocation. A validating webhook's failure is not a
/// mutating webhook's anything, and Task 3.6 Step 5 forbids inferring one
/// from the other.
///
/// The other half of the assertion is the reason the prefix is looked at
/// at all: the event proves a chain ran that this trace does not
/// describe, so `Observed` -- "the full webhook chain was watched" --
/// would be a false claim.
///
/// The fixture event carries a `failed-open.validating.webhook.admission.k8s.io/`
/// key, which Task 3.10 confirmed is the *only* annotation under that
/// family a real kube-apiserver writes: an earlier version of this
/// fixture invented a per-invocation `validation.webhook...` key that no
/// Kubernetes release has ever emitted, and that the old (misspelled)
/// constant could not have matched even if one did. See
/// `admissionlab_admission::correlate`'s own module documentation for
/// the upstream source and the live evidence.
#[test]
fn validating_annotations_create_no_invocations_but_do_downgrade_the_evidence() {
    let event = response_complete_for("fixture-validating-failed-open");
    assert!(
        event
            .annotations
            .keys()
            .any(|key| key.starts_with("failed-open.validating.webhook.admission.k8s.io/")),
        "precondition: the fixture event carries a validating-webhook annotation"
    );

    let trace = reconstruct_mutating_trace(&event).expect("trace");

    assert!(trace.invocations.is_empty());
    assert_eq!(trace.evidence, TraceEvidence::Partial);
}

/// The mutation this test exists to kill: reporting a request that ran no
/// mutating webhook as [`TraceEvidence::Unavailable`], which tells a
/// reader to disregard `invocations` entirely -- making "the candidate's
/// mutating webhook stopped running" indistinguishable from "we did not
/// look", and erasing the regression this product exists to detect.
#[test]
fn an_event_with_no_webhook_annotations_is_an_observed_empty_trace() {
    let trace =
        reconstruct_mutating_trace(&response_complete_for("fixture-no-webhooks")).expect("trace");

    assert!(trace.invocations.is_empty());
    assert_eq!(trace.evidence, TraceEvidence::Observed);
}

/// The mutation this test exists to kill: accepting any stage, which
/// would turn a `RequestReceived` event -- written before admission runs,
/// so never carrying a single admission annotation -- into a confident
/// claim that no mutating webhook was invoked.
#[test]
fn a_request_received_event_is_refused_rather_than_read_as_an_empty_chain() {
    let event = fixture_events()
        .into_iter()
        .find(|event| !event.is_response_complete())
        .expect("mutation-rounds.jsonl has a RequestReceived event");
    assert!(event.annotations.is_empty(), "precondition");

    let error = reconstruct_mutating_trace(&event).expect_err("a non-final stage is refused");

    let TraceError::NotResponseComplete { audit_id, stage } = &error else {
        panic!("expected NotResponseComplete, got {error:?}");
    };
    assert_eq!(audit_id, &event.audit_id);
    assert_eq!(stage, "RequestReceived");
    assert!(error.to_string().contains(STAGE_RESPONSE_COMPLETE));
}

// ---------------------------------------------------------------------
// Step 6: malformed evidence is fatal, and never quoted
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: falling back to a default
/// `(round, index)` -- or skipping the annotation -- when the key's
/// digits will not parse. Either would silently move or drop an
/// invocation, and Phase 4 would then report a webhook-chain difference
/// that never happened.
#[test]
fn a_malformed_annotation_key_is_fatal() {
    for suffix in [
        "round_x_index_0",
        "round_0_index_",
        "round_0",
        "round_+0_index_0",
        "",
    ] {
        let mut event = response_complete_for("fixture-unmutated");
        let value = event
            .annotations
            .remove(&annotation_key(MUTATION_ANNOTATION_PREFIX, 0, 0))
            .expect("the fixture has a mutation annotation to rewrite");
        let key = format!("{MUTATION_ANNOTATION_PREFIX}{suffix}");
        event.annotations.insert(key.clone(), value);

        let Err(error) = reconstruct_mutating_trace(&event) else {
            panic!("the key suffix {suffix:?} must not reconstruct");
        };
        let TraceError::AnnotationKey { key: reported } = &error else {
            panic!("expected AnnotationKey for {suffix}, got {error:?}");
        };
        assert_eq!(reported, &key);
    }
}

/// The mutation this test exists to kill: building the payload error from
/// `serde_json::Error`'s own `Display`, which embeds the offending
/// *value* on a type mismatch. A patch annotation carries fragments of
/// the request object, so Global Constraint 14 keeps it out of anything
/// this project renders -- the same rule `audit_reader`'s unparsable-line
/// diagnostic already follows.
#[test]
fn a_malformed_annotation_payload_is_fatal_and_never_quotes_the_value() {
    let secret = "super-secret-token-value";
    let mut event = response_complete_for("fixture-one-round");
    let key = annotation_key(PATCH_ANNOTATION_PREFIX, 0, 0);
    event.annotations.insert(
        key.clone(),
        format!(
            r#"{{"configuration":"c","webhook":"w","patch":"{secret}","patchType":"JSONPatch"}}"#
        ),
    );

    let error = reconstruct_mutating_trace(&event).expect_err("a bad payload is refused");

    let TraceError::AnnotationPayload {
        key: reported,
        expected,
        category,
        ..
    } = &error
    else {
        panic!("expected AnnotationPayload, got {error:?}");
    };
    assert_eq!(reported, &key);
    assert_eq!(*expected, "patch");
    assert_eq!(*category, "data");
    assert!(
        !error.to_string().contains(secret),
        "the rendered error must not carry the annotation's own value"
    );
}

/// The mutation this test exists to kill: accepting whatever `patchType`
/// a payload declares and presenting it as a JSON Patch anyway.
#[test]
fn an_unsupported_patch_type_is_fatal() {
    let mut event = response_complete_for("fixture-one-round");
    let key = annotation_key(PATCH_ANNOTATION_PREFIX, 0, 0);
    event.annotations.insert(
        key.clone(),
        r#"{"configuration":"c","webhook":"w","patch":[],"patchType":"SomeFuturePatch"}"#
            .to_string(),
    );

    let error = reconstruct_mutating_trace(&event).expect_err("an unknown dialect is refused");

    let TraceError::UnsupportedPatchType {
        key: reported,
        patch_type,
    } = &error
    else {
        panic!("expected UnsupportedPatchType, got {error:?}");
    };
    assert_eq!(reported, &key);
    assert_eq!(patch_type, "SomeFuturePatch");
}

/// The mutation this test exists to kill: reading a failed-open
/// annotation's bare webhook-name value as JSON, which is what happens
/// the moment the mutation prefix is matched with `contains` instead of
/// `strip_prefix` -- the failed-open prefix ends with the mutation
/// prefix.
#[test]
fn the_failed_open_prefix_is_never_matched_as_a_mutation_prefix() {
    let event = response_complete_for("fixture-failed-open");
    let key = annotation_key(FAILED_OPEN_MUTATION_ANNOTATION_PREFIX, 0, 0);
    assert!(
        key.contains(MUTATION_ANNOTATION_PREFIX),
        "precondition: the failed-open key contains the mutation prefix"
    );
    assert_eq!(
        event.annotations.get(&key).map(String::as_str),
        Some("mutate-unreachable.test-webhook.admissionlab.dev"),
        "precondition: its value is a bare webhook name, not JSON"
    );

    reconstruct_mutating_trace(&event).expect("a bare-string failed-open value is not JSON-parsed");
}

// ---------------------------------------------------------------------
// Global Constraint 15: no fabricated latency
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: filling
/// [`WebhookInvocation::latency`] with a plausible `0` because the field
/// exists. These annotations carry no timing whatsoever; Task 3.8 reads
/// per-webhook latency from API server metrics instead, and a `0` here
/// would read to Phase 4 as a real, instantaneous measurement.
#[test]
fn no_reconstructed_invocation_ever_carries_a_latency() {
    let mut seen = 0usize;
    for event in fixture_events() {
        if !event.is_response_complete() {
            continue;
        }
        let trace = reconstruct_mutating_trace(&event).expect("trace");
        for invocation in &trace.invocations {
            assert_eq!(invocation.latency, None);
            seen += 1;
        }
    }
    assert!(seen >= 7, "the fixture exercises several invocations");
}

/// The mutation this test exists to kill: letting an unrelated annotation
/// -- `authorization.k8s.io/decision`, or a webhook's own
/// `AdmissionResponse.auditAnnotations`, which kube-apiserver records as
/// `<webhook name>/<key>` -- reach the mutating-annotation parser and
/// become either an invocation or a [`TraceError`].
#[test]
fn unrelated_annotations_are_neither_parsed_nor_reported() {
    let mut event = response_complete_for("fixture-unmutated");
    let extra: BTreeMap<String, String> = [
        (
            "mutate-noop.test-webhook.admissionlab.dev/decision".to_string(),
            "not json at all".to_string(),
        ),
        (
            "admission.example.com/round_0_index_0".to_string(),
            "also not json".to_string(),
        ),
    ]
    .into_iter()
    .collect();
    event.annotations.extend(extra);

    let trace = reconstruct_mutating_trace(&event).expect("trace");

    assert_eq!(trace.invocations.len(), 1);
    assert_eq!(trace.evidence, TraceEvidence::Observed);
}
