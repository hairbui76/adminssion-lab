//! ROADMAP Task 9.5 Step 4: the cluster failure bundle is redacted
//! before it is embedded in a failure artifact.
//!
//! `admissionlab_core::FailureDiagnostics` is redacted *by construction*
//! for everything structural — its summary types have no field an
//! object's `spec`, annotations, or a Secret's `data` could land in, as
//! `admissionlab-cluster`'s own `tests/diagnostics_unit.rs` proves. This
//! suite covers the one thing construction cannot: an event's `message`
//! is free text a third-party controller wrote, and a controller that
//! logs an `Authorization:` header or pastes a private key into an event
//! is exactly what Global Constraint 14's string rules exist for.
//!
//! The split is deliberate and load-bearing: the redaction rules live in
//! this crate, and `admissionlab-cluster` must never depend on it
//! (`report -> admission -> cluster` already exists, so the reverse edge
//! would be a cycle). That is why the bundle's *bounding* is tested
//! there and its *redaction* is tested here.

use std::path::PathBuf;

use admissionlab_core::{
    ConditionSummary, ContainerStatusSummary, FailureDiagnostics, RedactedEvent,
    RedactedObjectSummary,
};
use admissionlab_report::{REDACTED, REDACTED_PRIVATE_KEY, redact_failure_diagnostics};

/// A private key that a badly behaved controller pasted into an event —
/// deliberately assembled from the same markers `redact.rs`'s rule 3
/// matches on, since a test that used a different marker would prove
/// nothing about the rule.
const LEAKED_KEY: &str =
    "-----BEGIN PRIVATE KEY-----\nSENTINEL-KEY-BODY\n-----END PRIVATE KEY-----";

fn hostile_bundle() -> FailureDiagnostics {
    FailureDiagnostics {
        nodes: vec![RedactedObjectSummary {
            kind: "Node".to_owned(),
            name: "adlab-baseline-abc-control-plane".to_owned(),
            namespace: None,
            phase: None,
            conditions: vec![ConditionSummary {
                condition_type: "Ready".to_owned(),
                status: "False".to_owned(),
                reason: Some("KubeletNotReady".to_owned()),
                last_transition: Some("2026-09-01T10:00:00Z".to_owned()),
            }],
            containers: Vec::new(),
            created_at: None,
        }],
        pods: vec![RedactedObjectSummary {
            kind: "Pod".to_owned(),
            name: "lab-webhook-0".to_owned(),
            namespace: Some("lab".to_owned()),
            phase: Some("Pending".to_owned()),
            conditions: Vec::new(),
            containers: vec![ContainerStatusSummary {
                name: "app".to_owned(),
                ready: false,
                restarts: 3,
                state: "waiting".to_owned(),
                reason: Some("ImagePullBackOff".to_owned()),
                exit_code: None,
            }],
            created_at: None,
        }],
        workloads: Vec::new(),
        events: vec![
            RedactedEvent {
                namespace: Some("lab".to_owned()),
                event_type: Some("Warning".to_owned()),
                reason: Some("Failed".to_owned()),
                message: "registry refused: Authorization: Bearer SENTINEL-TOKEN".to_owned(),
                involved_object: Some("Pod/lab-webhook-0".to_owned()),
                count: Some(4),
                first_seen: None,
                last_seen: None,
            },
            RedactedEvent {
                namespace: Some("lab".to_owned()),
                event_type: Some("Warning".to_owned()),
                reason: Some("Unhealthy".to_owned()),
                message: format!("could not load serving cert: {LEAKED_KEY}"),
                involved_object: Some("Pod/lab-webhook-0".to_owned()),
                count: None,
                first_seen: None,
                last_seen: None,
            },
        ],
        webhook_configurations: Vec::new(),
        kind_logs_path: Some(PathBuf::from(
            "/tmp/admissionlab-runs/r1/logs/baseline-kind-logs",
        )),
        notes: vec!["`kind export logs` said: Authorization: Bearer SENTINEL-TOKEN".to_owned()],
    }
}

#[test]
fn an_event_message_carrying_a_header_or_a_private_key_is_redacted() {
    let redacted = redact_failure_diagnostics(&hostile_bundle());

    let serialized = serde_json::to_string(&redacted).expect("a bundle must serialize");
    assert!(
        !serialized.contains("SENTINEL-TOKEN"),
        "an Authorization header survived into a bundle: {serialized}"
    );
    assert!(
        !serialized.contains("SENTINEL-KEY-BODY"),
        "a private key survived into a bundle: {serialized}"
    );

    assert!(redacted.events[0].message.contains(REDACTED));
    assert!(redacted.events[1].message.contains(REDACTED_PRIVATE_KEY));
    // A note quotes command output, which is vendor-derived text like
    // any other -- so it goes through the same rules.
    assert!(redacted.notes[0].contains(REDACTED));
}

#[test]
fn redaction_removes_values_and_never_structure() {
    let bundle = hostile_bundle();
    let redacted = redact_failure_diagnostics(&bundle);

    // Global Constraint 15, as `redact.rs` applies it to a `LabResult`:
    // every rule replaces a value in place. Nothing is dropped, so the
    // reader can still see *that* an event fired, on what, how often --
    // only the credential inside the message is gone.
    assert_eq!(redacted.events.len(), bundle.events.len());
    assert_eq!(redacted.nodes.len(), bundle.nodes.len());
    assert_eq!(redacted.pods.len(), bundle.pods.len());
    assert_eq!(redacted.events[0].reason, bundle.events[0].reason);
    assert_eq!(redacted.events[0].count, bundle.events[0].count);
    assert_eq!(
        redacted.events[0].involved_object,
        bundle.events[0].involved_object
    );
    assert_eq!(
        redacted.pods[0].containers[0].reason.as_deref(),
        Some("ImagePullBackOff")
    );
    assert_eq!(redacted.pods[0].containers[0].restarts, 3);
    assert_eq!(redacted.nodes[0].conditions, bundle.nodes[0].conditions);
    // A path, not content: the raw logs are never embedded, so the path
    // that points at them is carried through untouched.
    assert_eq!(redacted.kind_logs_path, bundle.kind_logs_path);
}

#[test]
fn redaction_is_idempotent() {
    let once = redact_failure_diagnostics(&hostile_bundle());
    let twice = redact_failure_diagnostics(&once);
    assert_eq!(
        once, twice,
        "nothing in this pass may match its own replacement"
    );
}

#[test]
fn an_already_clean_bundle_is_returned_unchanged() {
    let clean = FailureDiagnostics {
        pods: vec![RedactedObjectSummary {
            kind: "Pod".to_owned(),
            name: "lab-webhook-0".to_owned(),
            namespace: Some("lab".to_owned()),
            phase: Some("Pending".to_owned()),
            conditions: Vec::new(),
            containers: Vec::new(),
            created_at: None,
        }],
        notes: vec!["8 further pod summaries were omitted".to_owned()],
        ..FailureDiagnostics::default()
    };

    assert_eq!(redact_failure_diagnostics(&clean), clean);
}
