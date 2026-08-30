//! Task 3.3 model tests.
//!
//! Global Constraint 15 says missing data is represented as
//! unavailable/unknown and never fabricated. Three fields in this
//! crate's model are where that rule can be quietly lost during
//! serialization (see `src/lib.rs`'s module documentation), so most
//! tests here assert on the actual serialized JSON *text*, not merely
//! that a value survives a round trip -- a round trip passes for almost
//! any consistent-but-wrong encoding (controller supplement §5, Task
//! 3.3). Each test's doc comment names what would make it fail.

use std::collections::BTreeMap;
use std::time::Duration;

use admissionlab_admission::{
    AdmissionDecision, AdmissionOutcome, AdmissionTrace, TraceEvidence, WebhookInvocation,
    WebhookOutcome,
};
use admissionlab_core::{Diagnostic, FixtureId, Side};
use json_patch::jsonptr::PointerBuf;
use json_patch::{AddOperation, PatchOperation};
use serde_json::json;

/// A `WebhookInvocation` with every optional field present, for tests
/// that only care about one field's absence.
fn full_invocation() -> WebhookInvocation {
    WebhookInvocation {
        configuration: "policy.example.com".to_owned(),
        webhook: "check.policy.example.com".to_owned(),
        round: 0,
        index: 0,
        mutated: Some(true),
        patch: Some(vec![PatchOperation::Add(AddOperation {
            path: PointerBuf::parse("/metadata/labels/injected").unwrap(),
            value: json!("true"),
        })]),
        latency: Some(Duration::from_millis(42)),
        outcome: WebhookOutcome::Allowed,
    }
}

/// Fails if `AdmissionDecision::Rejected`'s `code: None` and
/// `code: Some(404)` serialize to the same JSON text, or if `code: None`
/// serializes as anything other than literal `null` (for example a
/// fabricated `0`, which `u16`'s own zero value would look like).
#[test]
fn admission_decision_rejected_distinguishes_absent_code_from_present() {
    let without_code = AdmissionDecision::Rejected {
        code: None,
        message: "denied".to_owned(),
    };
    let with_code = AdmissionDecision::Rejected {
        code: Some(404),
        message: "denied".to_owned(),
    };

    let without_json = serde_json::to_string(&without_code).unwrap();
    let with_json = serde_json::to_string(&with_code).unwrap();

    assert!(
        without_json.contains(r#""code":null"#),
        "expected literal null for an absent code, got: {without_json}"
    );
    assert!(
        with_json.contains(r#""code":404"#),
        "expected the observed code verbatim, got: {with_json}"
    );
    assert_ne!(without_json, with_json);

    // Round trip: each variant, including the unit variant, survives
    // serialize/deserialize unchanged.
    for decision in [
        AdmissionDecision::Accepted,
        without_code,
        with_code,
        AdmissionDecision::UnsupportedDryRun {
            message: "no dry-run support on this side".to_owned(),
        },
    ] {
        let text = serde_json::to_string(&decision).unwrap();
        let parsed: AdmissionDecision = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, decision);
    }
}

/// Fails if `AdmissionDecision::Accepted` (a unit variant) does not
/// serialize as the bare string `"accepted"` -- the pinned wire tag a
/// later report reader depends on.
#[test]
fn admission_decision_accepted_serializes_as_pinned_bare_string() {
    let text = serde_json::to_string(&AdmissionDecision::Accepted).unwrap();
    assert_eq!(text, r#""accepted""#);
}

/// Fails if any `TraceEvidence` variant's wire string drifts from its
/// pinned name, or if two variants collide.
#[test]
fn trace_evidence_variant_names_are_pinned_and_distinct() {
    let observed = serde_json::to_string(&TraceEvidence::Observed).unwrap();
    let partial = serde_json::to_string(&TraceEvidence::Partial).unwrap();
    let unavailable = serde_json::to_string(&TraceEvidence::Unavailable).unwrap();

    assert_eq!(observed, r#""observed""#);
    assert_eq!(partial, r#""partial""#);
    assert_eq!(unavailable, r#""unavailable""#);
}

/// Fails if any `WebhookOutcome` variant's wire string drifts from its
/// pinned name, or if two variants collide.
#[test]
fn webhook_outcome_variant_names_are_pinned_and_distinct() {
    let allowed = serde_json::to_string(&WebhookOutcome::Allowed).unwrap();
    let denied = serde_json::to_string(&WebhookOutcome::Denied).unwrap();
    let errored = serde_json::to_string(&WebhookOutcome::Errored).unwrap();
    let unknown = serde_json::to_string(&WebhookOutcome::Unknown).unwrap();

    assert_eq!(allowed, r#""allowed""#);
    assert_eq!(denied, r#""denied""#);
    assert_eq!(errored, r#""errored""#);
    assert_eq!(unknown, r#""unknown""#);
}

/// The load-bearing test for controller supplement §1: deserializing an
/// `AdmissionTrace` document that omits `evidence` must fail, not
/// silently produce `TraceEvidence::Observed`.
///
/// Fails (goes green when it should be red) if `TraceEvidence` or the
/// `evidence` field is given a default -- exactly the mutation this test
/// exists to catch. Mutation-tested: temporarily adding `#[serde(default)]`
/// to `AdmissionTrace::evidence` (paired with a temporary
/// `impl Default for TraceEvidence` returning `Observed`) made this test
/// fail, as expected; see the Task 3.3 report for the transcript. Both
/// changes were reverted before this file was committed.
#[test]
fn deserializing_admission_trace_without_evidence_fails() {
    let document = json!({ "invocations": [] });
    let result: Result<AdmissionTrace, _> = serde_json::from_value(document);
    assert!(
        result.is_err(),
        "a document with no `evidence` key must not deserialize, got: {result:?}"
    );
}

/// Applies controller supplement §2's rule to `WebhookOutcome`: a
/// document that omits `outcome` must fail to deserialize, not silently
/// read as any variant (least of all `Allowed`, which would fabricate
/// success for a webhook this project never actually observed).
///
/// Mutation-tested the same way as
/// `deserializing_admission_trace_without_evidence_fails`: temporarily
/// adding `#[serde(default)]` to `WebhookInvocation::outcome` (paired
/// with a temporary `impl Default for WebhookOutcome` returning
/// `Allowed`) made this test fail, as expected; reverted before commit.
#[test]
fn deserializing_webhook_invocation_without_outcome_fails() {
    let document = json!({
        "configuration": "policy.example.com",
        "webhook": "check.policy.example.com",
        "round": 0,
        "index": 0,
        "mutated": null,
        "patch": null,
        "latency": null,
    });
    let result: Result<WebhookInvocation, _> = serde_json::from_value(document);
    assert!(
        result.is_err(),
        "a document with no `outcome` key must not deserialize, got: {result:?}"
    );
}

/// Fails if an unmeasured `latency` serializes as anything other than
/// literal JSON `null` (in particular, a fabricated `0`, which would
/// read to Phase 4's latency comparison as "instantaneous").
#[test]
fn webhook_invocation_latency_none_serializes_as_null_not_zero() {
    let mut invocation = full_invocation();
    invocation.latency = None;

    let text = serde_json::to_string(&invocation).unwrap();
    assert!(
        text.contains(r#""latency":null"#),
        "expected literal null for an unmeasured latency, got: {text}"
    );
    assert!(
        !text.contains(r#""latency":0"#),
        "an unmeasured latency must never serialize as 0, got: {text}"
    );

    let parsed: WebhookInvocation = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed.latency, None);
}

/// Fails if a measured latency does not round-trip to the exact
/// millisecond count it was constructed with, or if it is not written
/// as a plain JSON number (for example serde's default `{secs, nanos}`
/// object shape for `Duration`, which would break the `null`-for-absent
/// contract the sibling test relies on).
#[test]
fn webhook_invocation_latency_some_serializes_as_millisecond_number() {
    let mut invocation = full_invocation();
    invocation.latency = Some(Duration::from_millis(150));

    let text = serde_json::to_string(&invocation).unwrap();
    assert!(
        text.contains(r#""latency":150"#),
        "expected a plain millisecond integer, got: {text}"
    );

    let parsed: WebhookInvocation = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed.latency, Some(Duration::from_millis(150)));
}

/// The load-bearing test for controller supplement §1's third instance:
/// fails if "could not tell whether it mutated" (`None`) and "did not
/// mutate" (`Some(false)`) serialize to indistinguishable JSON text --
/// which is exactly what a `None -> Some(false)` collapse would produce.
#[test]
fn webhook_invocation_mutated_none_is_distinguishable_from_false() {
    let mut unknown = full_invocation();
    unknown.mutated = None;
    let mut did_not_mutate = full_invocation();
    did_not_mutate.mutated = Some(false);

    let unknown_json = serde_json::to_string(&unknown).unwrap();
    let did_not_mutate_json = serde_json::to_string(&did_not_mutate).unwrap();

    assert!(
        unknown_json.contains(r#""mutated":null"#),
        "expected literal null for unknown mutation status, got: {unknown_json}"
    );
    assert!(
        did_not_mutate_json.contains(r#""mutated":false"#),
        "expected literal false for a confirmed non-mutation, got: {did_not_mutate_json}"
    );
    assert_ne!(unknown_json, did_not_mutate_json);

    let parsed_unknown: WebhookInvocation = serde_json::from_str(&unknown_json).unwrap();
    let parsed_did_not_mutate: WebhookInvocation =
        serde_json::from_str(&did_not_mutate_json).unwrap();
    assert_eq!(parsed_unknown.mutated, None);
    assert_eq!(parsed_did_not_mutate.mutated, Some(false));
}

/// A standard round-trip test, kept as a baseline sanity check alongside
/// the JSON-text assertions above (which are the tests that actually
/// discriminate a fabricated encoding -- see this file's module
/// documentation). Fails if serializing then deserializing a trace with
/// two invocations (including a real JSON Patch operation) produces a
/// value unequal to the original.
#[test]
fn admission_trace_round_trips_with_multiple_invocations() {
    let trace = AdmissionTrace {
        evidence: TraceEvidence::Partial,
        invocations: vec![
            full_invocation(),
            WebhookInvocation {
                configuration: "other.example.com".to_owned(),
                webhook: "check.other.example.com".to_owned(),
                round: 1,
                index: 0,
                mutated: None,
                patch: None,
                latency: None,
                outcome: WebhookOutcome::Unknown,
            },
        ],
    };

    let text = serde_json::to_string(&trace).unwrap();
    let parsed: AdmissionTrace = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed, trace);
}

/// Fails if `AdmissionOutcome` serializes `fixture_id`/`side` as
/// anything other than their plain string forms (for example a
/// newtype-wrapper object shape, or `Side`'s Rust variant name instead
/// of its pinned lowercase name), or if `total_latency` is not a plain
/// millisecond integer.
#[test]
fn admission_outcome_serializes_fixture_id_side_and_latency_as_plain_values() {
    let outcome = AdmissionOutcome {
        fixture_id: FixtureId::parse("checked-pod").unwrap(),
        side: Side::Candidate,
        decision: AdmissionDecision::Accepted,
        warnings: vec![],
        total_latency: Duration::from_millis(1234),
        final_object: None,
        trace: AdmissionTrace {
            evidence: TraceEvidence::Unavailable,
            invocations: vec![],
        },
        diagnostics: vec![Diagnostic {
            code: "capture.ok".to_owned(),
            message: "captured cleanly".to_owned(),
            context: BTreeMap::new(),
        }],
    };

    let text = serde_json::to_string(&outcome).unwrap();
    assert!(text.contains(r#""fixture_id":"checked-pod""#), "{text}");
    assert!(text.contains(r#""side":"candidate""#), "{text}");
    assert!(text.contains(r#""total_latency":1234"#), "{text}");
    assert!(text.contains(r#""final_object":null"#), "{text}");
    assert!(text.contains(r#""evidence":"unavailable""#), "{text}");
    // `Diagnostic` already implements `Serialize` in `admissionlab-core`;
    // this confirms it is reachable through `AdmissionOutcome` unchanged.
    assert!(text.contains(r#""code":"capture.ok""#), "{text}");
}
