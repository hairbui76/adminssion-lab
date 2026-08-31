//! Task 4.4 admission-decision classification tests.
//!
//! Two of these tests assert that *nothing* is produced, and those are
//! the important ones. A verdict flip is easy to get right; the failure
//! modes that matter are the false positives -- claiming a regression
//! because a rejection message was reworded, or because this lab could
//! not replay the fixture on one side at all. Global Constraint 15 makes
//! "no change" a positive claim, so an empty result is only honest when
//! both sides were genuinely observed and genuinely agreed.
//!
//! Each test's doc comment names what would make it fail.

use std::time::Duration;

use admissionlab_admission::{AdmissionDecision, AdmissionOutcome, AdmissionTrace, TraceEvidence};
use admissionlab_core::{FixtureId, Side};
use admissionlab_diff::{
    DecisionComparability, SemanticChangeKind, decision_comparability, diff_admission_decision,
    raw_decision_diff,
};
use serde_json::{Value, json};

/// Builds an outcome for the fixture `pod-basic` on `side` with
/// `decision`.
///
/// Every field this task does not compare is held identical across both
/// sides, so a test that observes a difference is observing one the
/// decision classifier itself produced.
fn outcome(side: Side, decision: AdmissionDecision) -> AdmissionOutcome {
    AdmissionOutcome {
        fixture_id: FixtureId::parse("pod-basic").unwrap(),
        side,
        decision,
        warnings: Vec::new(),
        total_latency: Duration::from_millis(12),
        final_object: None,
        trace: AdmissionTrace {
            evidence: TraceEvidence::Observed,
            invocations: Vec::new(),
        },
        diagnostics: Vec::new(),
    }
}

/// A rejection with a status code and a message.
fn rejected(code: u16, message: &str) -> AdmissionDecision {
    AdmissionDecision::Rejected {
        code: Some(code),
        message: message.to_owned(),
    }
}

/// Fails if a baseline that admitted the object and a candidate that
/// rejected it does not produce exactly one `newly_denied` change -- the
/// single most important claim this tool makes -- or if that change's
/// payloads are anything other than the two decisions verbatim.
#[test]
fn accepted_then_rejected_is_newly_denied() {
    let baseline = outcome(Side::Baseline, AdmissionDecision::Accepted);
    let candidate = outcome(Side::Candidate, rejected(403, "pods must set runAsNonRoot"));

    let changes = diff_admission_decision(&baseline, &candidate);

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    let change = &changes[0];
    assert_eq!(change.kind, SemanticChangeKind::ObjectNewlyDenied);
    assert_eq!(change.fixture_id, FixtureId::parse("pod-basic").unwrap());
    assert_eq!(
        change.baseline,
        Some(json!("accepted")),
        "the baseline payload must be the observed decision verbatim"
    );
    assert_eq!(
        change.candidate,
        Some(json!({"rejected": {"code": 403, "message": "pods must set runAsNonRoot"}})),
        "the candidate payload must carry the observed code and message verbatim"
    );
    assert_eq!(
        change.object_path, None,
        "a verdict flip is not about one field"
    );
    assert_eq!(change.subject, None);
    assert_eq!(
        change.origin, None,
        "attribution is Task 4.7's; None means unattributed, not undiverged"
    );
}

/// Fails if a rejection observed *without* a status code acquires a
/// plausible-looking one on its way into the change payload -- the
/// fabrication `AdmissionDecision` itself is built to prevent, which
/// would be undone by rebuilding the payload by hand in this crate.
#[test]
fn newly_denied_payload_preserves_an_absent_status_code() {
    let baseline = outcome(Side::Baseline, AdmissionDecision::Accepted);
    let candidate = outcome(
        Side::Candidate,
        AdmissionDecision::Rejected {
            code: None,
            message: "denied".to_owned(),
        },
    );

    let changes = diff_admission_decision(&baseline, &candidate);

    assert_eq!(changes.len(), 1);
    assert_eq!(
        changes[0].candidate.as_ref().unwrap()["rejected"]["code"],
        Value::Null
    );
}

/// Fails if the reverse flip is missed or misclassified. A candidate
/// that starts admitting something the baseline denied is just as much a
/// behavior change as the other direction, even though it "loosens"
/// rather than "tightens".
#[test]
fn rejected_then_accepted_is_newly_allowed() {
    let baseline = outcome(Side::Baseline, rejected(403, "pods must set runAsNonRoot"));
    let candidate = outcome(Side::Candidate, AdmissionDecision::Accepted);

    let changes = diff_admission_decision(&baseline, &candidate);

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(changes[0].kind, SemanticChangeKind::ObjectNewlyAllowed);
    assert_eq!(
        changes[0].baseline,
        Some(json!({"rejected": {"code": 403, "message": "pods must set runAsNonRoot"}}))
    );
    assert_eq!(changes[0].candidate, Some(json!("accepted")));
}

/// Fails if a reworded rejection is reported as a regression. The object
/// is denied on both sides, which is the behavior a user's policy cares
/// about; claiming `newly_denied` (or anything else) here is a false
/// positive that would make every message-tweaking upgrade look like a
/// break.
#[test]
fn rejected_then_rejected_with_a_different_message_is_not_a_semantic_change() {
    let baseline = outcome(Side::Baseline, rejected(403, "pods must set runAsNonRoot"));
    let candidate = outcome(
        Side::Candidate,
        rejected(403, "violates PodSecurity restricted:latest"),
    );

    let changes = diff_admission_decision(&baseline, &candidate);

    assert!(
        changes.is_empty(),
        "a reworded rejection is not a behavior change: {changes:?}"
    );
    assert_eq!(
        decision_comparability(&baseline, &candidate),
        DecisionComparability::Comparable,
        "both sides were really observed, so the empty list is a claim, not an absence"
    );
}

/// Fails if a differing rejection message becomes invisible. The
/// semantic list is empty by design, so the difference has to remain
/// reachable through the diagnostic channel -- this is the test that
/// pins that channel actually carrying it.
#[test]
fn rejected_then_rejected_message_drift_is_visible_in_the_raw_diff() {
    let baseline = outcome(Side::Baseline, rejected(403, "pods must set runAsNonRoot"));
    let candidate = outcome(
        Side::Candidate,
        rejected(422, "violates PodSecurity restricted:latest"),
    );

    let raw = raw_decision_diff(&baseline, &candidate);
    let value = serde_json::to_value(&raw).unwrap();

    assert_eq!(
        value,
        json!([
            {"op": "replace", "path": "/rejected/code", "value": 422},
            {
                "op": "replace",
                "path": "/rejected/message",
                "value": "violates PodSecurity restricted:latest",
            },
        ]),
        "the raw channel must show both the code and the message change"
    );
}

/// Fails if two sides that agreed produce a raw diff -- the property
/// that lets a reader treat a non-empty raw decision diff as "something
/// about the verdict really differs, even if it was not classified".
#[test]
fn identical_decisions_have_an_empty_raw_diff() {
    let baseline = outcome(Side::Baseline, rejected(403, "denied"));
    let candidate = outcome(Side::Candidate, rejected(403, "denied"));

    assert!(raw_decision_diff(&baseline, &candidate).is_empty());
}

/// Fails if an unsupported dry-run on the candidate side is reported as
/// a regression. `UnsupportedDryRun` says this lab could not replay the
/// fixture there; it says nothing at all about the stack under test, and
/// turning a lab capability limit into a `newly_denied` claim would be
/// the worst kind of false positive -- one that blames the user's
/// change for the tool's own gap.
#[test]
fn unsupported_dry_run_on_the_candidate_is_not_a_regression() {
    let baseline = outcome(Side::Baseline, AdmissionDecision::Accepted);
    let candidate = outcome(
        Side::Candidate,
        AdmissionDecision::UnsupportedDryRun {
            message: "a matching webhook declares side effects".to_owned(),
        },
    );

    assert!(
        diff_admission_decision(&baseline, &candidate).is_empty(),
        "a lab capability limit is not a semantic change"
    );
    assert_eq!(
        decision_comparability(&baseline, &candidate),
        DecisionComparability::Incomparable {
            baseline: None,
            candidate: Some("a matching webhook declares side effects".to_owned()),
        },
        "the incomparable side and its own explanation must be reported"
    );
}

/// Fails if an unsupported dry-run on the *baseline* side is treated
/// differently from one on the candidate side, or if it is allowed to
/// produce `newly_allowed` merely because the candidate happened to
/// admit the object. There is no baseline verdict to have flipped from.
#[test]
fn unsupported_dry_run_on_the_baseline_is_not_a_regression() {
    let baseline = outcome(
        Side::Baseline,
        AdmissionDecision::UnsupportedDryRun {
            message: "dry-run replay not attempted".to_owned(),
        },
    );
    let candidate = outcome(Side::Candidate, AdmissionDecision::Accepted);

    assert!(diff_admission_decision(&baseline, &candidate).is_empty());
    assert_eq!(
        decision_comparability(&baseline, &candidate),
        DecisionComparability::Incomparable {
            baseline: Some("dry-run replay not attempted".to_owned()),
            candidate: None,
        }
    );
}

/// Fails if a fixture unsupported on *both* sides is quietly reported as
/// comparable-and-identical. Two absences agreeing is not an
/// observation, and Phase 4 must be able to count this as
/// `inconclusive` rather than as a passing fixture.
#[test]
fn unsupported_dry_run_on_both_sides_is_incomparable_not_identical() {
    let baseline = outcome(
        Side::Baseline,
        AdmissionDecision::UnsupportedDryRun {
            message: "unsupported on baseline".to_owned(),
        },
    );
    let candidate = outcome(
        Side::Candidate,
        AdmissionDecision::UnsupportedDryRun {
            message: "unsupported on candidate".to_owned(),
        },
    );

    assert!(diff_admission_decision(&baseline, &candidate).is_empty());
    assert_eq!(
        decision_comparability(&baseline, &candidate),
        DecisionComparability::Incomparable {
            baseline: Some("unsupported on baseline".to_owned()),
            candidate: Some("unsupported on candidate".to_owned()),
        }
    );
}

/// Fails if two identical admissions produce any change at all -- the
/// overwhelmingly common case, and the one an over-eager classifier
/// would spam.
#[test]
fn accepted_then_accepted_is_empty() {
    let baseline = outcome(Side::Baseline, AdmissionDecision::Accepted);
    let candidate = outcome(Side::Candidate, AdmissionDecision::Accepted);

    assert!(diff_admission_decision(&baseline, &candidate).is_empty());
    assert_eq!(
        decision_comparability(&baseline, &candidate),
        DecisionComparability::Comparable
    );
}

/// Fails if this function starts opining on the admitted object. Two
/// sides can both admit while mutating the object differently; that is
/// Task 4.5's `diff_workload_objects` to classify, and reporting it here
/// would produce a duplicate change with no field path attached.
#[test]
fn accepted_then_accepted_ignores_a_differing_final_object() {
    let mut baseline = outcome(Side::Baseline, AdmissionDecision::Accepted);
    let mut candidate = outcome(Side::Candidate, AdmissionDecision::Accepted);
    baseline.final_object = Some(json!({"spec": {"containers": [{"image": "nginx:1.25"}]}}));
    candidate.final_object = Some(json!({"spec": {"containers": [{"image": "nginx:1.27"}]}}));

    assert!(
        diff_admission_decision(&baseline, &candidate).is_empty(),
        "object-level differences belong to Task 4.5, not to decision classification"
    );
}

/// Fails if `DecisionComparability`'s wire tags drift. Phase 4's summary
/// serializes this to explain why a fixture was counted as
/// inconclusive.
#[test]
fn decision_comparability_wire_tags_are_pinned() {
    assert_eq!(
        serde_json::to_string(&DecisionComparability::Comparable).unwrap(),
        r#""comparable""#
    );
    assert_eq!(
        serde_json::to_value(DecisionComparability::Incomparable {
            baseline: None,
            candidate: Some("unsupported".to_owned()),
        })
        .unwrap(),
        json!({"incomparable": {"baseline": null, "candidate": "unsupported"}})
    );
}
