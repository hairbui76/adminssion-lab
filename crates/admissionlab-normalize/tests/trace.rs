//! Task 4.2: canonicalizing observed webhook trace evidence.
//!
//! Every patch below is built by deserializing real RFC 6902 JSON into
//! `Vec<PatchOperation>`, the same way `admissionlab-admission`'s
//! `correlate.rs` builds one from an audit annotation's `patch` payload.
//! That keeps the operation shapes honest -- a hand-constructed
//! `AddOperation` could not tell us whether the wire form this project
//! actually reads round-trips through normalization unchanged.

use std::time::Duration;

use admissionlab_admission::trace::{
    AdmissionTrace, TraceEvidence, WebhookInvocation, WebhookOutcome,
};
use admissionlab_normalize::{NormalizedTrace, normalize_trace};
use json_patch::PatchOperation;
use serde_json::{Value, json};

// ---------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------

fn operations(patch: Value) -> Vec<PatchOperation> {
    serde_json::from_value(patch).expect("a well-formed RFC 6902 patch")
}

/// A minimal mutating invocation: round 0, index 0, allowed, with the
/// given patch.
fn invocation(patch: Option<Value>) -> WebhookInvocation {
    WebhookInvocation {
        configuration: "admissionlab-test-webhook".to_owned(),
        webhook: "mutate-labels.test-webhook.admissionlab.dev".to_owned(),
        round: 0,
        index: 0,
        mutated: patch.as_ref().map(|_| true),
        patch: patch.map(operations),
        latency: Some(Duration::from_millis(7)),
        outcome: WebhookOutcome::Allowed,
    }
}

fn observed(invocations: Vec<WebhookInvocation>) -> AdmissionTrace {
    AdmissionTrace {
        evidence: TraceEvidence::Observed,
        invocations,
    }
}

/// The single patch of a single-invocation trace, re-serialized so it
/// can be compared as text rather than as a structure whose equality
/// might itself be order-insensitive.
fn only_patch_json(normalized: &NormalizedTrace) -> String {
    let patch = normalized.invocations[0]
        .patch
        .as_ref()
        .expect("a patch was observed");
    serde_json::to_string(patch).expect("serialize patch")
}

// ---------------------------------------------------------------------
// Step 1: identity, round, and index are preserved verbatim
// ---------------------------------------------------------------------

/// Configuration, webhook name, round, index, and outcome all pass
/// through unchanged, including across a reinvocation in a later round.
///
/// The round/index pair is what makes a reinvocation legible at all --
/// `pod-reinvocation.yaml` exists to produce exactly this shape -- so
/// renumbering or re-sorting it would invent a chain that was not
/// observed.
#[test]
fn webhook_identity_round_and_index_pass_through_verbatim() {
    let trace = observed(vec![
        WebhookInvocation {
            webhook: "mutate-labels.test-webhook.admissionlab.dev".to_owned(),
            round: 0,
            index: 1,
            ..invocation(None)
        },
        WebhookInvocation {
            webhook: "mutate-noop.test-webhook.admissionlab.dev".to_owned(),
            round: 0,
            index: 0,
            outcome: WebhookOutcome::Unknown,
            ..invocation(None)
        },
        WebhookInvocation {
            webhook: "mutate-labels.test-webhook.admissionlab.dev".to_owned(),
            round: 1,
            index: 0,
            ..invocation(None)
        },
    ]);
    let normalized = normalize_trace(&trace);

    let observed_chain: Vec<(&str, u32, u32, WebhookOutcome)> = normalized
        .invocations
        .iter()
        .map(|entry| {
            (
                entry.webhook.as_str(),
                entry.round,
                entry.index,
                entry.outcome,
            )
        })
        .collect();
    assert_eq!(
        observed_chain,
        vec![
            (
                "mutate-labels.test-webhook.admissionlab.dev",
                0,
                1,
                WebhookOutcome::Allowed
            ),
            (
                "mutate-noop.test-webhook.admissionlab.dev",
                0,
                0,
                WebhookOutcome::Unknown
            ),
            (
                "mutate-labels.test-webhook.admissionlab.dev",
                1,
                0,
                WebhookOutcome::Allowed
            ),
        ],
        "invocations keep their capture order; nothing is sorted by round/index"
    );
    for entry in &normalized.invocations {
        assert_eq!(entry.configuration, "admissionlab-test-webhook");
    }
}

/// `evidence` is copied through untouched, for every variant. Quietly
/// upgrading a `Partial` or `Unavailable` trace to `Observed` would be
/// the single most damaging fabrication this crate could commit (Global
/// Constraint 15).
#[test]
fn trace_evidence_is_untouched() {
    for evidence in [
        TraceEvidence::Observed,
        TraceEvidence::Partial,
        TraceEvidence::Unavailable,
    ] {
        let trace = AdmissionTrace {
            evidence,
            invocations: vec![invocation(Some(json!([
                {"op": "add", "path": "/metadata/labels/x", "value": "1"}
            ])))],
        };
        assert_eq!(normalize_trace(&trace).evidence, evidence);
    }
}

/// An `Unavailable` trace with no invocations normalizes to exactly
/// that, rather than to anything more confident.
#[test]
fn an_empty_unavailable_trace_stays_empty_and_unavailable() {
    let trace = AdmissionTrace {
        evidence: TraceEvidence::Unavailable,
        invocations: Vec::new(),
    };
    let normalized = normalize_trace(&trace);

    assert_eq!(normalized.evidence, TraceEvidence::Unavailable);
    assert!(normalized.invocations.is_empty());
}

// ---------------------------------------------------------------------
// Step 2: canonicalizing patch values
// ---------------------------------------------------------------------

/// Object keys inside a patch value are sorted, recursively, at every
/// depth -- including inside objects nested in arrays.
#[test]
fn patch_value_object_keys_are_canonicalized_recursively() {
    let trace = observed(vec![invocation(Some(json!([
        {
            "op": "add",
            "path": "/spec/containers/-",
            "value": {
                "name": "sidecar",
                "image": "registry.k8s.io/pause:3.10",
                "env": [
                    {"value": "sidecar", "name": "PROXY_MODE"}
                ],
                "resources": {
                    "requests": {"memory": "64Mi", "cpu": "10m"}
                }
            }
        }
    ])))]);

    assert_eq!(
        only_patch_json(&normalize_trace(&trace)),
        r#"[{"op":"add","path":"/spec/containers/-","value":{"env":[{"name":"PROXY_MODE","value":"sidecar"}],"image":"registry.k8s.io/pause:3.10","name":"sidecar","resources":{"requests":{"cpu":"10m","memory":"64Mi"}}}}]"#
    );
}

/// Two captures whose patch values differ only in object key order
/// normalize to the same bytes -- that equivalence is the entire point
/// of Step 2.
#[test]
fn key_order_alone_does_not_make_two_patches_differ() {
    let one = observed(vec![invocation(Some(json!([
        {"op": "replace", "path": "/spec/x", "value": {"b": 2, "a": 1, "c": {"z": 1, "y": 2}}}
    ])))]);
    let other = observed(vec![invocation(Some(json!([
        {"op": "replace", "path": "/spec/x", "value": {"c": {"y": 2, "z": 1}, "a": 1, "b": 2}}
    ])))]);

    assert_eq!(
        only_patch_json(&normalize_trace(&one)),
        only_patch_json(&normalize_trace(&other))
    );
    assert_eq!(normalize_trace(&one), normalize_trace(&other));
}

/// Operations are never reordered. `remove` then `add` on the same
/// pointer is not the same patch as `add` then `remove`, and a webhook
/// that started emitting one instead of the other changed behavior.
#[test]
fn operation_order_is_never_changed() {
    let patch = json!([
        {"op": "remove", "path": "/spec/z"},
        {"op": "add", "path": "/spec/a", "value": 1},
        {"op": "test", "path": "/spec/a", "value": 1},
        {"op": "add", "path": "/spec/b", "value": 2}
    ]);
    let trace = observed(vec![invocation(Some(patch.clone()))]);
    let normalized = normalize_trace(&trace);

    assert_eq!(
        normalized.invocations[0].patch.as_ref().expect("a patch"),
        &operations(patch),
        "the operations vector is rebuilt in its original order, unchanged"
    );
}

/// Array order *inside* a patch value is semantics and is left alone,
/// even though the objects within those arrays do get canonical keys.
///
/// Init containers are the sharpest case: two patches that install the
/// same two of them in opposite orders produce two different pods.
#[test]
fn array_order_inside_a_patch_value_is_preserved() {
    let forwards = observed(vec![invocation(Some(json!([
        {
            "op": "add",
            "path": "/spec/initContainers",
            "value": [{"name": "await-database"}, {"name": "migrate-schema"}]
        }
    ])))]);
    let backwards = observed(vec![invocation(Some(json!([
        {
            "op": "add",
            "path": "/spec/initContainers",
            "value": [{"name": "migrate-schema"}, {"name": "await-database"}]
        }
    ])))]);

    assert_eq!(
        only_patch_json(&normalize_trace(&forwards)),
        r#"[{"op":"add","path":"/spec/initContainers","value":[{"name":"await-database"},{"name":"migrate-schema"}]}]"#
    );
    assert_ne!(
        normalize_trace(&forwards),
        normalize_trace(&backwards),
        "reordering an array inside a patch value is a different patch"
    );
}

/// Operations with no value -- `remove`, `move`, `copy` -- pass through
/// untouched, pointers and all. Nothing rewrites a `path` or a `from`.
#[test]
fn valueless_operations_and_their_pointers_pass_through() {
    let patch = json!([
        {"op": "remove", "path": "/metadata/labels/admissionlab.dev~1mutated"},
        {"op": "move", "from": "/spec/a", "path": "/spec/b"},
        {"op": "copy", "from": "/spec/b", "path": "/spec/c"}
    ]);
    let trace = observed(vec![invocation(Some(patch.clone()))]);

    assert_eq!(
        only_patch_json(&normalize_trace(&trace)),
        serde_json::to_string(&operations(patch)).expect("serialize patch")
    );
}

/// The escaped-pointer form the test webhook actually emits survives
/// verbatim: `~1` is RFC 6901's escape for the `/` in the label key
/// `admissionlab.dev/mutated`, and canonicalization must not re-encode
/// it.
#[test]
fn an_escaped_operation_path_is_not_rewritten() {
    let trace = observed(vec![invocation(Some(json!([
        {"op": "add", "path": "/metadata/labels/admissionlab.dev~1mutated", "value": "true"}
    ])))]);

    assert_eq!(
        only_patch_json(&normalize_trace(&trace)),
        r#"[{"op":"add","path":"/metadata/labels/admissionlab.dev~1mutated","value":"true"}]"#
    );
}

// ---------------------------------------------------------------------
// Step 3: a changed patch always survives
// ---------------------------------------------------------------------

/// Two traces whose patches reach the *same* final object by different
/// routes keep both patches in full, and normalize to different traces.
///
/// One webhook replaces the whole labels map; the other adds the two
/// keys individually. Applied to a pod with no labels, both leave
/// exactly `{"a": "1", "b": "2"}` -- so a normalizer that compared final
/// objects could talk itself into calling these equivalent and blanking
/// one. They are not equivalent: a webhook that switched from one to the
/// other changed what it does to any object that already had labels.
#[test]
fn patches_with_the_same_net_effect_both_survive_in_full() {
    let replace_map = json!([
        {"op": "add", "path": "/metadata/labels", "value": {"a": "1", "b": "2"}}
    ]);
    let add_each_key = json!([
        {"op": "add", "path": "/metadata/labels/a", "value": "1"},
        {"op": "add", "path": "/metadata/labels/b", "value": "2"}
    ]);
    let whole_map = observed(vec![invocation(Some(replace_map.clone()))]);
    let per_key = observed(vec![invocation(Some(add_each_key.clone()))]);

    let normalized_whole = normalize_trace(&whole_map);
    let normalized_per_key = normalize_trace(&per_key);

    assert_eq!(
        normalized_whole.invocations[0]
            .patch
            .as_ref()
            .expect("a patch"),
        &operations(replace_map)
    );
    assert_eq!(
        normalized_per_key.invocations[0]
            .patch
            .as_ref()
            .expect("a patch"),
        &operations(add_each_key)
    );
    assert_ne!(normalized_whole, normalized_per_key);
}

/// A patch that sets a field to the value it already had is still a
/// patch that was observed, and is kept in full. (Kubernetes reaches
/// this combination legitimately: such a patch applies cleanly and
/// leaves the object `DeepEqual`, so the audit annotation says
/// `mutated: false` while a patch annotation is nevertheless present --
/// see `admissionlab-admission`'s `correlate.rs`.)
#[test]
fn a_no_op_patch_is_not_blanked() {
    let patch =
        json!([{"op": "replace", "path": "/spec/schedulerName", "value": "default-scheduler"}]);
    let trace = observed(vec![WebhookInvocation {
        mutated: Some(false),
        ..invocation(Some(patch.clone()))
    }]);
    let normalized = normalize_trace(&trace);

    assert_eq!(
        normalized.invocations[0].patch.as_ref().expect("a patch"),
        &operations(patch)
    );
    assert_eq!(normalized.invocations[0].mutated, Some(false));
}

/// An observed but empty patch stays `Some(vec![])`. Collapsing it to
/// `None` would turn "the webhook answered with a patch containing no
/// operations" into "no patch was observed" -- two different facts.
#[test]
fn an_observed_empty_patch_is_not_collapsed_to_none() {
    let trace = observed(vec![invocation(Some(json!([])))]);
    let normalized = normalize_trace(&trace);

    assert_eq!(normalized.invocations[0].patch, Some(Vec::new()));
}

// ---------------------------------------------------------------------
// Step 4: unknowns stay unknown
// ---------------------------------------------------------------------

/// `mutated: None` and `latency: None` pass through as `None`, never as
/// a fabricated `Some(false)` or `Some(0)` (Global Constraint 15).
///
/// The two hazards are different and both real: `Some(false)` would
/// claim a webhook was watched and found not to mutate when it was never
/// watched, and `Some(0)` would read as an instantaneous call, which
/// Phase 4's latency comparison would score as a genuine improvement.
#[test]
fn unknown_mutated_and_latency_stay_unknown() {
    let trace = observed(vec![WebhookInvocation {
        mutated: None,
        patch: None,
        latency: None,
        outcome: WebhookOutcome::Unknown,
        ..invocation(None)
    }]);
    let normalized = normalize_trace(&trace);

    assert_eq!(normalized.invocations[0].mutated, None);
    assert_eq!(normalized.invocations[0].latency, None);
    assert_eq!(normalized.invocations[0].patch, None);
    assert_eq!(normalized.invocations[0].outcome, WebhookOutcome::Unknown);
}

/// A measured latency and a known `mutated` pass through with their
/// exact values.
#[test]
fn known_mutated_and_latency_pass_through_exactly() {
    let trace = observed(vec![WebhookInvocation {
        mutated: Some(false),
        latency: Some(Duration::from_micros(1_234)),
        outcome: WebhookOutcome::Denied,
        ..invocation(None)
    }]);
    let normalized = normalize_trace(&trace);

    assert_eq!(normalized.invocations[0].mutated, Some(false));
    assert_eq!(
        normalized.invocations[0].latency,
        Some(Duration::from_micros(1_234)),
        "no rounding, no truncation to milliseconds"
    );
    assert_eq!(normalized.invocations[0].outcome, WebhookOutcome::Denied);
}

/// An errored invocation stays `Errored`, distinct from `Denied`. The
/// two mean opposite things under the two `failurePolicy` settings, and
/// normalization has no business merging them.
#[test]
fn an_errored_invocation_is_not_merged_into_denied() {
    let trace = observed(vec![WebhookInvocation {
        outcome: WebhookOutcome::Errored,
        mutated: Some(false),
        latency: None,
        ..invocation(None)
    }]);
    assert_eq!(
        normalize_trace(&trace).invocations[0].outcome,
        WebhookOutcome::Errored
    );
}

// ---------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------

/// The same trace normalizes to the same result every time.
#[test]
fn normalization_is_deterministic() {
    let trace = observed(vec![
        invocation(Some(json!([
            {"op": "add", "path": "/spec/containers/-", "value": {"name": "s", "image": "i"}}
        ]))),
        invocation(None),
    ]);

    assert_eq!(normalize_trace(&trace), normalize_trace(&trace));
    assert_eq!(
        only_patch_json(&normalize_trace(&trace)),
        only_patch_json(&normalize_trace(&trace))
    );
}
