//! Task 4.3 contract tests.
//!
//! The names in this file are a public contract: they appear in JSON
//! reports and in `expectations.yaml` files users keep in their own
//! repositories, so a rename is a breaking change for people who never
//! read this crate's source. These tests therefore assert on serialized
//! JSON *text*, not on round trips -- a round trip passes for almost any
//! consistent-but-wrong encoding, including one where every variant
//! silently acquired a new name at once.
//!
//! Each test's doc comment names what would make it fail.

use admissionlab_core::FixtureId;
use admissionlab_diff::{
    DivergenceConfidence, DivergenceEvidence, RawChange, RawChangeOp, SemanticChange,
    SemanticChangeKind, raw_object_diff,
};
use serde_json::{Value, json};

/// Every [`SemanticChangeKind`] variant paired with the exact wire string
/// it must serialize as.
///
/// The length is asserted below, and [`expected_wire_name`]'s exhaustive
/// `match` makes adding an eighteenth variant a compile error rather than
/// a silently untested one -- between them, "all seventeen are covered"
/// is enforced by the compiler and the test, not by review.
const ALL_KINDS: [(SemanticChangeKind, &str); 17] = [
    (SemanticChangeKind::ObjectNewlyDenied, "newly_denied"),
    (SemanticChangeKind::ObjectNewlyAllowed, "newly_allowed"),
    (SemanticChangeKind::ContainerAdded, "container_added"),
    (SemanticChangeKind::ContainerRemoved, "container_removed"),
    (
        SemanticChangeKind::InitContainerAdded,
        "init_container_added",
    ),
    (
        SemanticChangeKind::InitContainerRemoved,
        "init_container_removed",
    ),
    (SemanticChangeKind::VolumeAdded, "volume_added"),
    (SemanticChangeKind::VolumeRemoved, "volume_removed"),
    (
        SemanticChangeKind::VolumeMountChanged,
        "volume_mount_changed",
    ),
    (
        SemanticChangeKind::EnvironmentChanged,
        "environment_changed",
    ),
    (SemanticChangeKind::ImageChanged, "image_changed"),
    (
        SemanticChangeKind::ServiceAccountChanged,
        "service_account_changed",
    ),
    (
        SemanticChangeKind::SecurityContextChanged,
        "security_context_changed",
    ),
    (
        SemanticChangeKind::ResourceRequirementChanged,
        "resource_requirement_changed",
    ),
    (SemanticChangeKind::WebhookFailed, "webhook_failed"),
    (
        SemanticChangeKind::WebhookInvocationChanged,
        "webhook_invocation_changed",
    ),
    (
        SemanticChangeKind::WebhookLatencyChanged,
        "webhook_latency_changed",
    ),
];

/// The wire name each variant must have, restated as an exhaustive
/// `match` so the compiler rejects a new variant that nobody pinned.
///
/// Deliberately a second, independent transcription of the contract: if
/// this and the `#[serde(rename)]` attributes ever disagree, the test
/// below fails rather than both drifting together.
fn expected_wire_name(kind: SemanticChangeKind) -> &'static str {
    match kind {
        SemanticChangeKind::ObjectNewlyDenied => "newly_denied",
        SemanticChangeKind::ObjectNewlyAllowed => "newly_allowed",
        SemanticChangeKind::ContainerAdded => "container_added",
        SemanticChangeKind::ContainerRemoved => "container_removed",
        SemanticChangeKind::InitContainerAdded => "init_container_added",
        SemanticChangeKind::InitContainerRemoved => "init_container_removed",
        SemanticChangeKind::VolumeAdded => "volume_added",
        SemanticChangeKind::VolumeRemoved => "volume_removed",
        SemanticChangeKind::VolumeMountChanged => "volume_mount_changed",
        SemanticChangeKind::EnvironmentChanged => "environment_changed",
        SemanticChangeKind::ImageChanged => "image_changed",
        SemanticChangeKind::ServiceAccountChanged => "service_account_changed",
        SemanticChangeKind::SecurityContextChanged => "security_context_changed",
        SemanticChangeKind::ResourceRequirementChanged => "resource_requirement_changed",
        SemanticChangeKind::WebhookFailed => "webhook_failed",
        SemanticChangeKind::WebhookInvocationChanged => "webhook_invocation_changed",
        SemanticChangeKind::WebhookLatencyChanged => "webhook_latency_changed",
    }
}

/// Fails if any of the seventeen kinds serializes as anything other than
/// its pinned `snake_case` name -- including the four whose wire name is
/// deliberately *not* the Rust identifier lowercased
/// (`ObjectNewlyDenied` -> `newly_denied`, `ObjectNewlyAllowed` ->
/// `newly_allowed`), which is exactly what a `rename_all` refactor would
/// break. Also fails if the table above stops covering all seventeen
/// distinct variants.
#[test]
fn every_semantic_change_kind_serializes_to_its_pinned_name() {
    let mut seen = std::collections::BTreeSet::new();

    for (kind, expected) in ALL_KINDS {
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(
            json,
            format!("\"{expected}\""),
            "{kind:?} must serialize as the stable name {expected:?}"
        );
        assert_eq!(
            expected_wire_name(kind),
            expected,
            "{kind:?}'s table entry and exhaustive-match entry disagree"
        );
        assert_eq!(
            kind.as_str(),
            expected,
            "{kind:?}::as_str must return exactly the serialized name"
        );
        assert!(seen.insert(expected), "{expected:?} listed twice");
    }

    assert_eq!(seen.len(), 17, "all seventeen kinds must be covered");
}

/// Fails if a pinned wire name does not deserialize back to its own
/// variant -- the direction Task 4.9's user-authored `expectations.yaml`
/// depends on, and the direction the serialization test above cannot
/// prove on its own.
#[test]
fn every_semantic_change_kind_deserializes_from_its_pinned_name() {
    for (kind, expected) in ALL_KINDS {
        let parsed: SemanticChangeKind =
            serde_json::from_str(&format!("\"{expected}\"")).expect("pinned name must parse");
        assert_eq!(parsed, kind);
    }
}

/// Fails if an unknown kind name is silently accepted -- for example if
/// a future `#[serde(other)]` catch-all variant let a typo in a user's
/// expectations file quietly match nothing instead of erroring.
#[test]
fn unknown_semantic_change_kind_name_is_rejected() {
    let parsed = serde_json::from_str::<SemanticChangeKind>("\"newly_denyed\"");
    assert!(parsed.is_err(), "a misspelled kind must not parse");
}

/// Fails if [`DivergenceConfidence`]'s three wire tags drift. These
/// serialize into the same reports the kind names do, so they are
/// Alpha-stable too.
#[test]
fn divergence_confidence_wire_names_are_pinned() {
    for (confidence, expected) in [
        (DivergenceConfidence::Observed, "observed"),
        (DivergenceConfidence::Inferred, "inferred"),
        (DivergenceConfidence::Unknown, "unknown"),
    ] {
        assert_eq!(
            serde_json::to_string(&confidence).unwrap(),
            format!("\"{expected}\"")
        );
    }
}

/// Fails if a `(round, index)` position stops serializing as a
/// two-element array, or if an absent position/webhook reaches the wire
/// as anything other than literal `null` (a fabricated `[0,0]` would
/// claim the divergence was located at the first invocation).
#[test]
fn divergence_evidence_serializes_positions_and_absences_honestly() {
    let located = DivergenceEvidence {
        confidence: DivergenceConfidence::Observed,
        baseline_position: Some((1, 2)),
        candidate_position: None,
        baseline_webhook: Some("check.policy.example.com".to_owned()),
        candidate_webhook: None,
        explanation: "candidate has no invocation at round 1 index 2".to_owned(),
    };

    let value = serde_json::to_value(&located).unwrap();
    assert_eq!(value["baseline_position"], json!([1, 2]));
    assert_eq!(value["candidate_position"], Value::Null);
    assert_eq!(value["candidate_webhook"], Value::Null);
    assert_eq!(value["confidence"], json!("observed"));

    let round_tripped: DivergenceEvidence = serde_json::from_value(value).unwrap();
    assert_eq!(round_tripped, located);
}

/// Fails if `SemanticChange` stops serializing its fixture identifier as
/// a bare string (the shape `AdmissionOutcome` already uses), or if an
/// absent optional field reaches the wire as anything but `null` -- an
/// omitted `origin` key would read as "attribution succeeded and found
/// nothing" to a consumer that treats missing as false.
#[test]
fn semantic_change_serializes_with_a_bare_fixture_id_and_explicit_nulls() {
    let change = SemanticChange {
        kind: SemanticChangeKind::ImageChanged,
        fixture_id: FixtureId::parse("pod-basic").unwrap(),
        object_path: Some("/spec/containers/0/image".to_owned()),
        subject: Some("app".to_owned()),
        baseline: Some(json!("nginx:1.25")),
        candidate: Some(json!("nginx:1.27")),
        origin: None,
    };

    let value = serde_json::to_value(&change).unwrap();
    assert_eq!(value["kind"], json!("image_changed"));
    assert_eq!(value["fixture_id"], json!("pod-basic"));
    assert_eq!(value["object_path"], json!("/spec/containers/0/image"));
    assert_eq!(value["subject"], json!("app"));
    assert_eq!(value["baseline"], json!("nginx:1.25"));
    assert_eq!(value["candidate"], json!("nginx:1.27"));
    assert!(
        value.get("origin").is_some(),
        "an unattributed origin must be an explicit null, not an omitted key"
    );
    assert_eq!(value["origin"], Value::Null);
}

/// Fails if a `SemanticChange` carrying attribution does not nest the
/// evidence object under `origin`.
#[test]
fn semantic_change_carries_divergence_evidence_under_origin() {
    let change = SemanticChange {
        kind: SemanticChangeKind::InitContainerRemoved,
        fixture_id: FixtureId::parse("pod-sidecar").unwrap(),
        object_path: Some("/spec/initContainers".to_owned()),
        subject: Some("inject".to_owned()),
        baseline: Some(json!([{"name": "inject"}])),
        candidate: None,
        origin: Some(DivergenceEvidence {
            confidence: DivergenceConfidence::Inferred,
            baseline_position: Some((0, 0)),
            candidate_position: Some((0, 0)),
            baseline_webhook: Some("inject.example.com".to_owned()),
            candidate_webhook: Some("inject.example.com".to_owned()),
            explanation: "baseline trace evidence was partial".to_owned(),
        }),
    };

    let value = serde_json::to_value(&change).unwrap();
    assert_eq!(value["origin"]["confidence"], json!("inferred"));
    assert_eq!(value["candidate"], Value::Null);
}

/// Fails if [`raw_object_diff`] stops producing RFC 6902 operation
/// objects -- the shape external tooling can apply directly, and the one
/// this project's own reports render.
#[test]
fn raw_object_diff_emits_rfc_6902_operations() {
    let baseline = json!({"spec": {"replicas": 1, "paused": true}});
    let candidate = json!({"spec": {"replicas": 2, "selector": "app"}});

    let changes = raw_object_diff(&baseline, &candidate);
    let value = serde_json::to_value(&changes).unwrap();

    assert_eq!(
        value,
        json!([
            {"op": "replace", "path": "/spec/replicas", "value": 2},
            {"op": "add", "path": "/spec/selector", "value": "app"},
            {"op": "remove", "path": "/spec/paused"},
        ]),
        "raw diff must be a valid JSON Patch document, in this order"
    );
}

/// Fails if a `remove` operation grows a `value` key or a non-`move`
/// operation grows a `from` key -- RFC 6902 defines neither, and a
/// serialized raw diff is meant to be directly applicable.
#[test]
fn raw_change_omits_fields_the_operation_does_not_define() {
    let removal = RawChange {
        op: RawChangeOp::Remove,
        path: "/metadata/labels/tmp".to_owned(),
        value: None,
        from: None,
    };

    let json = serde_json::to_string(&removal).unwrap();
    assert_eq!(json, r#"{"op":"remove","path":"/metadata/labels/tmp"}"#);
}

/// Fails if a genuine JSON `null` value is confused with an absent one --
/// setting a field to `null` is a real observable difference and must
/// reach the wire as `"value":null`, not vanish alongside the operations
/// that define no value at all.
#[test]
fn raw_change_distinguishes_a_null_value_from_an_absent_one() {
    let baseline = json!({"a": 1});
    let candidate = json!({"a": Value::Null});

    let changes = raw_object_diff(&baseline, &candidate);
    let json = serde_json::to_string(&changes).unwrap();
    assert_eq!(json, r#"[{"op":"replace","path":"/a","value":null}]"#);
}

/// Fails if the raw diff's output ever depends on the order keys happened
/// to appear in the source JSON text -- which is exactly what would
/// happen if some dependency turned `serde_json`'s `preserve_order`
/// feature on, swapping the backing map for an insertion-ordered one.
/// This is the regression test `raw.rs`'s determinism argument names.
#[test]
fn raw_object_diff_ignores_source_key_order() {
    let baseline: Value = serde_json::from_str(r#"{"a": 1, "b": 2, "c": 3}"#).unwrap();
    let candidate_one: Value = serde_json::from_str(r#"{"a": 9, "b": 2, "d": 4}"#).unwrap();
    let candidate_two: Value = serde_json::from_str(r#"{"d": 4, "b": 2, "a": 9}"#).unwrap();

    let one = raw_object_diff(&baseline, &candidate_one);
    let two = raw_object_diff(&baseline, &candidate_two);

    assert_eq!(one, two, "diff order must not depend on source key order");
    assert_eq!(
        one,
        raw_object_diff(&baseline, &candidate_one),
        "repeating the same diff must give an identical result"
    );
}

/// Fails if equal documents produce anything other than an empty diff --
/// the property every caller relies on to mean "nothing differs here".
#[test]
fn raw_object_diff_of_equal_documents_is_empty() {
    let document = json!({"spec": {"containers": [{"name": "app", "image": "nginx"}]}});
    assert!(raw_object_diff(&document, &document).is_empty());
}
