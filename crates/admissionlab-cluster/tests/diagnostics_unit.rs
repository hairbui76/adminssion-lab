//! Unit tests for `admissionlab_cluster::diagnostics` (ROADMAP Task
//! 9.5): the failure bundle a run leaves behind when a cluster, or
//! something installed onto it, does not come up.
//!
//! No test here contacts a Kubernetes API server, spawns a real `kind`,
//! or touches Docker. Two fakes carry the whole suite:
//!
//! - [`FakeSource`], a [`ClusterObjectSource`] returning canned
//!   Kubernetes JSON (or a canned failure), which is what makes the
//!   collection logic — ordering, truncation, what becomes a note —
//!   testable at all. This is the seam `diagnostics.rs`'s own module
//!   documentation exists to justify.
//! - [`FakeProcessRunner`], a [`ProcessRunner`] that records the exact
//!   argv `KindClusterManager` builds for `kind export logs` without
//!   ever running it (the same fake shape `tests/lifecycle_unit.rs`
//!   uses).
//!
//! What is proven here:
//!
//! - **Statuses become summaries.** A `Pending` pod whose container is
//!   waiting on `ImagePullBackOff` reaches the bundle as exactly that,
//!   which is the single most actionable fact an install timeout
//!   produces (`collect_summarizes_pod_phase_conditions_and_the_kubelet_waiting_reason`).
//! - **Redacted by construction.** A pod carrying a sentinel in every
//!   place a secret realistically hides — an environment variable's
//!   value, an annotation, an image pull secret's name — produces a
//!   summary whose *entire serialization* contains none of them, because
//!   the summary type has nowhere to put them
//!   (`a_summary_cannot_carry_spec_env_or_annotations_at_all`).
//! - **A hostile event message is bounded and sanitized at
//!   construction**, and carried (not dropped) so the report crate's
//!   redaction pass has something to redact — see that test's own note
//!   for where the redaction half is proven
//!   (`a_hostile_event_message_is_bounded_and_stripped_of_control_characters`).
//! - **Missing data is missing, never fabricated.** A source that cannot
//!   list produces empty lists *and* a note per failed list
//!   (`collect_reports_every_unavailable_list_as_a_note_beside_an_empty_result`);
//!   a cluster that no longer exists produces one note and no API calls
//!   at all (`failure_diagnostics_for_an_absent_cluster_is_empty_and_says_so`).
//! - **The `kind export logs` argv**, exactly, including that the
//!   destination is one argv element and that the export is skipped
//!   entirely for a cluster that is gone.

use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use admissionlab_cluster::diagnostics::{ClusterObjectSource, collect};
use admissionlab_cluster::{KindClusterManager, relevant_namespaces};
use admissionlab_core::{
    ClusterHandle, ClusterManager, ClusterSpec, CommandResult, CommandSpec, DiagnosticsRequest,
    MAX_DIAGNOSTIC_EVENTS, MAX_DIAGNOSTIC_OBJECTS, MAX_SUMMARY_TEXT_CHARS, OutputOverflow,
    ProcessError, ProcessRunner, RedactedObjectSummary, RunId,
};
use async_trait::async_trait;
use serde_json::json;

// ---------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------

/// A fresh `tokio` runtime for one test's async calls, mirroring
/// `tests/lifecycle_unit.rs`'s own helper (this crate's dev `tokio` is
/// deliberately without `macros` for `#[tokio::test]`).
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build test tokio runtime")
        .block_on(future)
}

/// A [`ClusterObjectSource`] that answers from canned JSON, or fails
/// every call with a canned reason.
#[derive(Default)]
struct FakeSource {
    nodes: Vec<serde_json::Value>,
    pods: Vec<serde_json::Value>,
    events: Vec<serde_json::Value>,
    webhooks: Vec<serde_json::Value>,
    /// When set, every method fails with this reason instead of
    /// answering — standing in for an API server that is gone, which is
    /// the situation this whole module runs in.
    failure: Option<&'static str>,
    /// Every `(method, namespace)` pair the collector asked for, so a
    /// test can assert *which* namespaces were consulted rather than
    /// only what came back.
    asked: Mutex<Vec<String>>,
}

impl FakeSource {
    fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("asked mutex poisoned").clone()
    }

    fn record(&self, what: &str, namespace: Option<&str>) {
        self.asked
            .lock()
            .expect("asked mutex poisoned")
            .push(namespace.map_or_else(|| format!("{what}:*"), |ns| format!("{what}:{ns}")));
    }

    fn answer(&self, items: &[serde_json::Value]) -> Result<Vec<serde_json::Value>, String> {
        match self.failure {
            Some(reason) => Err(reason.to_owned()),
            None => Ok(items.to_vec()),
        }
    }
}

#[async_trait]
impl ClusterObjectSource for FakeSource {
    async fn nodes(&self) -> Result<Vec<serde_json::Value>, String> {
        self.record("nodes", None);
        self.answer(&self.nodes)
    }

    async fn pods(&self, namespace: Option<&str>) -> Result<Vec<serde_json::Value>, String> {
        self.record("pods", namespace);
        self.answer(&self.pods)
    }

    async fn events(&self, namespace: Option<&str>) -> Result<Vec<serde_json::Value>, String> {
        self.record("events", namespace);
        self.answer(&self.events)
    }

    async fn webhook_configurations(&self) -> Result<Vec<serde_json::Value>, String> {
        self.record("webhooks", None);
        self.answer(&self.webhooks)
    }
}

/// A [`ProcessRunner`] that never spawns anything: it records every
/// [`CommandSpec`] and returns a scripted exit, keyed by the `kind`
/// subcommand (`args.first()`).
struct FakeProcessRunner {
    /// `kind get clusters` stdout: the cluster names it should report.
    clusters: &'static str,
    /// Whether a scripted `kind export logs` exits 0.
    export_succeeds: bool,
    calls: Mutex<Vec<CommandSpec>>,
}

impl FakeProcessRunner {
    fn new(clusters: &'static str) -> Self {
        Self {
            clusters,
            export_succeeds: true,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn with_failing_export(mut self) -> Self {
        self.export_succeeds = false;
        self
    }

    fn calls(&self) -> Vec<CommandSpec> {
        self.calls.lock().expect("calls mutex poisoned").clone()
    }

    /// The argv of the one `kind export logs` invocation, or `None` if
    /// the manager never made one.
    fn export_call(&self) -> Option<Vec<OsString>> {
        self.calls()
            .into_iter()
            .find(|spec| spec.args.first().is_some_and(|arg| arg == "export"))
            .map(|spec| spec.args)
    }
}

#[async_trait]
impl ProcessRunner for FakeProcessRunner {
    async fn run(&self, spec: CommandSpec) -> Result<CommandResult, ProcessError> {
        self.calls
            .lock()
            .expect("calls mutex poisoned")
            .push(spec.clone());
        let subcommand = spec
            .args
            .first()
            .map(|arg| arg.to_string_lossy().into_owned())
            .unwrap_or_default();
        match subcommand.as_str() {
            "get" => Ok(result(0, self.clusters.as_bytes(), b"")),
            "export" if self.export_succeeds => Ok(result(0, b"", b"")),
            "export" => Ok(result(1, b"", b"ERROR: failed to export logs")),
            _ => Err(ProcessError::Spawn {
                context: Box::new(spec.context()),
                source: io::Error::new(io::ErrorKind::NotFound, "No such file or directory"),
            }),
        }
    }
}

fn result(code: i32, stdout: &[u8], stderr: &[u8]) -> CommandResult {
    CommandResult {
        status: ExitStatus::from_raw(code << 8),
        stdout: stdout.to_vec(),
        stderr: stderr.to_vec(),
        elapsed: Duration::from_millis(1),
        overflow: OutputOverflow::default(),
    }
}

/// A handle whose `kubeconfig` deliberately points at a path that does
/// not exist: every test here drives the *collection* logic, and a
/// kubeconfig that cannot be read is how the manager's Kubernetes half
/// degrades to a note without any network involvement.
fn handle_named(name: &str) -> ClusterHandle {
    let root = std::env::temp_dir().join(format!(
        "admissionlab-cluster-diagnostics-test-{}",
        RunId::generate().as_str()
    ));
    ClusterHandle {
        spec: ClusterSpec {
            side: admissionlab_core::Side::Baseline,
            name: name.to_owned(),
            kubernetes_version: "1.36.4".to_owned(),
            node_image: "kindest/node:v1.36.4".to_owned(),
            images: Vec::new(),
        },
        kubeconfig: root.join("kubeconfigs/baseline.kubeconfig"),
        audit_log: root.join("logs/baseline/audit/kube-apiserver-audit.log"),
    }
}

/// A pod stuck pulling its image — the exact shape an install timeout
/// against an unreachable registry produces.
fn image_pull_backoff_pod(name: &str) -> serde_json::Value {
    json!({
        "metadata": {
            "name": name,
            "namespace": "lab",
            "creationTimestamp": "2026-09-01T10:00:00Z",
            "annotations": { "lab/notes": "SENTINEL-ANNOTATION" }
        },
        "spec": {
            "imagePullSecrets": [{ "name": "SENTINEL-PULL-SECRET" }],
            "containers": [{
                "name": "app",
                "image": "registry.invalid/nope:1",
                "command": ["/bin/sh", "-c", "SENTINEL-COMMAND"],
                "env": [{ "name": "DB_PASSWORD", "value": "SENTINEL-ENV-VALUE" }]
            }]
        },
        "status": {
            "phase": "Pending",
            "conditions": [
                { "type": "Ready", "status": "False", "reason": "ContainersNotReady",
                  "lastTransitionTime": "2026-09-01T10:00:05Z",
                  "message": "containers with unready status: [app] SENTINEL-CONDITION-MESSAGE" }
            ],
            "containerStatuses": [{
                "name": "app",
                "ready": false,
                "restartCount": 0,
                "state": { "waiting": {
                    "reason": "ImagePullBackOff",
                    "message": "Back-off pulling image \"registry.invalid/nope:1\" SENTINEL-WAITING-MESSAGE"
                }}
            }]
        }
    })
}

fn healthy_pod(name: &str) -> serde_json::Value {
    json!({
        "metadata": { "name": name, "namespace": "lab" },
        "status": {
            "phase": "Running",
            "conditions": [{ "type": "Ready", "status": "True" }],
            "containerStatuses": [{
                "name": "app", "ready": true, "restartCount": 0,
                "state": { "running": { "startedAt": "2026-09-01T10:00:00Z" } }
            }]
        }
    })
}

fn request_for(namespaces: &[&str]) -> DiagnosticsRequest {
    DiagnosticsRequest {
        namespaces: namespaces.iter().map(|ns| (*ns).to_owned()).collect(),
        logs_destination: None,
    }
}

// ---------------------------------------------------------------------
// Step 2/3: statuses become summaries
// ---------------------------------------------------------------------

#[test]
fn collect_summarizes_pod_phase_conditions_and_the_kubelet_waiting_reason() {
    let source = FakeSource {
        pods: vec![image_pull_backoff_pod("lab-webhook-0")],
        ..FakeSource::default()
    };

    let bundle = block_on(collect(&source, &request_for(&["lab"])));

    assert_eq!(bundle.pods.len(), 1);
    let pod = &bundle.pods[0];
    assert_eq!(pod.kind, "Pod");
    assert_eq!(pod.name, "lab-webhook-0");
    assert_eq!(pod.namespace.as_deref(), Some("lab"));
    assert_eq!(pod.phase.as_deref(), Some("Pending"));
    assert_eq!(pod.created_at.as_deref(), Some("2026-09-01T10:00:00Z"));
    assert_eq!(pod.conditions.len(), 1);
    assert_eq!(pod.conditions[0].condition_type, "Ready");
    assert_eq!(pod.conditions[0].status, "False");
    assert_eq!(
        pod.conditions[0].reason.as_deref(),
        Some("ContainersNotReady")
    );
    assert_eq!(pod.containers.len(), 1);
    assert!(!pod.containers[0].ready);
    assert_eq!(pod.containers[0].state, "waiting");
    assert_eq!(
        pod.containers[0].reason.as_deref(),
        Some("ImagePullBackOff"),
        "the kubelet's waiting reason is the whole point of this bundle"
    );
    assert!(
        !pod.looks_healthy(),
        "a Pending pod with an unready container must never sort as healthy"
    );
}

#[test]
fn a_summary_cannot_carry_spec_env_or_annotations_at_all() {
    let source = FakeSource {
        pods: vec![image_pull_backoff_pod("lab-webhook-0")],
        ..FakeSource::default()
    };

    let bundle = block_on(collect(&source, &request_for(&["lab"])));
    let serialized =
        serde_json::to_string(&bundle).expect("a failure bundle must always serialize");

    // Redaction by construction: these are not stripped by a rule that
    // could be forgotten -- the summary types have no field any of them
    // could land in. A future field that *did* carry object content
    // would fail this test the moment it was added, which is the point.
    for sentinel in [
        "SENTINEL-ENV-VALUE",
        "SENTINEL-ANNOTATION",
        "SENTINEL-PULL-SECRET",
        "SENTINEL-COMMAND",
        "SENTINEL-CONDITION-MESSAGE",
        "SENTINEL-WAITING-MESSAGE",
    ] {
        assert!(
            !serialized.contains(sentinel),
            "{sentinel} reached a redacted summary: {serialized}"
        );
    }
    // ... while the actionable half did survive.
    assert!(serialized.contains("ImagePullBackOff"));
    assert!(serialized.contains("ContainersNotReady"));
}

#[test]
fn collect_summarizes_nodes_and_both_webhook_configuration_kinds() {
    let source = FakeSource {
        nodes: vec![json!({
            "metadata": { "name": "adlab-baseline-abc-control-plane" },
            "status": { "conditions": [
                { "type": "Ready", "status": "False", "reason": "KubeletNotReady" },
                { "type": "DiskPressure", "status": "True", "reason": "KubeletHasDiskPressure" }
            ]}
        })],
        webhooks: vec![
            json!({ "kind": "ValidatingWebhookConfiguration", "metadata": { "name": "kyverno-policy" } }),
            json!({ "kind": "MutatingWebhookConfiguration", "metadata": { "name": "kyverno-mutate" } }),
        ],
        ..FakeSource::default()
    };

    let bundle = block_on(collect(&source, &request_for(&["kyverno"])));

    assert_eq!(bundle.nodes.len(), 1);
    assert_eq!(bundle.nodes[0].kind, "Node");
    assert!(
        !bundle.nodes[0].looks_healthy(),
        "a NotReady node under disk pressure must not read as healthy"
    );
    let kinds: Vec<&str> = bundle
        .webhook_configurations
        .iter()
        .map(|summary| summary.kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "ValidatingWebhookConfiguration",
            "MutatingWebhookConfiguration"
        ],
        "each configuration keeps the kind its source labeled it with"
    );
}

#[test]
fn a_hostile_event_message_is_bounded_and_stripped_of_control_characters() {
    // A controller-authored message is free text: unbounded, and able to
    // carry an ANSI escape into a terminal report. Both are handled at
    // construction. The *redaction* half -- an `Authorization:` header or
    // a PEM private key inside such a message -- is deliberately not
    // this crate's job and is proven in
    // `admissionlab-report`'s `tests/failure_diagnostics_redaction.rs`,
    // because redaction rules live in the report crate and this crate
    // must not depend on it (`report -> admission -> cluster` already
    // exists; the reverse would be a cycle).
    let hostile = format!(
        "\u{1b}[2Jwiped your terminal\u{0}\n{}",
        "A".repeat(MAX_SUMMARY_TEXT_CHARS * 2)
    );
    let source = FakeSource {
        events: vec![json!({
            "metadata": { "namespace": "lab" },
            "type": "Warning",
            "reason": "Failed",
            "message": hostile,
            "count": 7,
            "involvedObject": { "kind": "Pod", "name": "lab-webhook-0" },
            "firstTimestamp": "2026-09-01T10:00:01Z",
            "lastTimestamp": "2026-09-01T10:04:00Z"
        })],
        ..FakeSource::default()
    };

    let bundle = block_on(collect(&source, &request_for(&["lab"])));

    assert_eq!(bundle.events.len(), 1);
    let event = &bundle.events[0];
    assert_eq!(event.reason.as_deref(), Some("Failed"));
    assert_eq!(event.involved_object.as_deref(), Some("Pod/lab-webhook-0"));
    assert_eq!(event.count, Some(7));
    assert_eq!(event.last_seen.as_deref(), Some("2026-09-01T10:04:00Z"));
    assert!(event.is_warning());
    assert!(
        !event.message.contains('\u{1b}') && !event.message.contains('\u{0}'),
        "control characters must not survive into a rendered report: {:?}",
        event.message
    );
    assert!(
        event.message.chars().count() <= MAX_SUMMARY_TEXT_CHARS + 1,
        "an unbounded controller message must be capped, got {} chars",
        event.message.chars().count()
    );
    assert!(
        event.message.ends_with('…'),
        "a truncated message must say it was truncated: {:?}",
        event.message
    );
}

// ---------------------------------------------------------------------
// Ordering, truncation, and honesty about what is missing
// ---------------------------------------------------------------------

#[test]
fn collect_puts_unhealthy_pods_first_and_says_how_many_it_dropped() {
    let mut pods: Vec<serde_json::Value> = (0..MAX_DIAGNOSTIC_OBJECTS + 5)
        .map(|index| healthy_pod(&format!("healthy-{index}")))
        .collect();
    // Deliberately last in the source's own order: the collector must
    // not depend on the API server having sorted anything for it.
    pods.push(image_pull_backoff_pod("the-broken-one"));
    let source = FakeSource {
        pods,
        ..FakeSource::default()
    };

    let bundle = block_on(collect(&source, &request_for(&["lab"])));

    assert_eq!(bundle.pods.len(), MAX_DIAGNOSTIC_OBJECTS);
    assert_eq!(
        bundle.pods[0].name, "the-broken-one",
        "the one pod that explains the failure must survive truncation"
    );
    assert!(
        bundle
            .notes
            .iter()
            .any(|note| note.contains("6 further pod summaries were omitted")),
        "a truncated list must say so: {:?}",
        bundle.notes
    );
}

#[test]
fn collect_puts_warning_events_first_and_caps_them_too() {
    let mut events: Vec<serde_json::Value> = (0..MAX_DIAGNOSTIC_EVENTS + 2)
        .map(|index| {
            json!({
                "metadata": { "namespace": "lab" },
                "type": "Normal",
                "reason": "Pulled",
                "message": format!("routine {index}")
            })
        })
        .collect();
    events.push(json!({
        "metadata": { "namespace": "lab" },
        "type": "Warning",
        "reason": "FailedScheduling",
        "message": "0/1 nodes are available"
    }));
    let source = FakeSource {
        events,
        ..FakeSource::default()
    };

    let bundle = block_on(collect(&source, &request_for(&["lab"])));

    assert_eq!(bundle.events.len(), MAX_DIAGNOSTIC_EVENTS);
    assert_eq!(bundle.events[0].reason.as_deref(), Some("FailedScheduling"));
    assert!(
        bundle
            .notes
            .iter()
            .any(|note| note.contains("further event summaries were omitted")),
        "{:?}",
        bundle.notes
    );
}

#[test]
fn collect_reports_every_unavailable_list_as_a_note_beside_an_empty_result() {
    let source = FakeSource {
        failure: Some("connection refused"),
        ..FakeSource::default()
    };

    let bundle = block_on(collect(&source, &request_for(&["lab"])));

    assert!(bundle.nodes.is_empty() && bundle.pods.is_empty() && bundle.events.is_empty());
    assert!(bundle.webhook_configurations.is_empty());
    // Global Constraint 15: four lists could not be fetched, so four
    // notes say so. An empty bundle with no notes would read as "the
    // cluster was fine", which is the one thing it must never mean.
    assert_eq!(
        bundle.notes.len(),
        4,
        "expected one note per failed list: {:?}",
        bundle.notes
    );
    assert!(
        bundle
            .notes
            .iter()
            .all(|note| note.contains("connection refused"))
    );
    assert!(!bundle.is_empty(), "notes alone make a bundle non-empty");
}

#[test]
fn collect_asks_each_requested_namespace_and_falls_back_to_every_namespace() {
    let scoped = FakeSource::default();
    block_on(collect(&scoped, &request_for(&["kyverno", "kube-system"])));
    assert_eq!(
        scoped.asked(),
        vec![
            "nodes:*",
            "pods:kyverno",
            "events:kyverno",
            "pods:kube-system",
            "events:kube-system",
            "webhooks:*",
        ]
    );

    let unscoped = FakeSource::default();
    block_on(collect(&unscoped, &request_for(&[])));
    assert_eq!(
        unscoped.asked(),
        vec!["nodes:*", "pods:*", "events:*", "webhooks:*"],
        "an empty namespace list means every namespace, not a namespace named \"\""
    );
}

#[test]
fn relevant_namespaces_always_includes_kube_system_but_invents_nothing() {
    assert_eq!(
        relevant_namespaces(&["kyverno".to_owned(), "kyverno".to_owned()]),
        vec!["kyverno".to_owned(), "kube-system".to_owned()],
        "duplicates collapse and kube-system is added once"
    );
    assert_eq!(
        relevant_namespaces(&["kube-system".to_owned()]),
        vec!["kube-system".to_owned()],
        "kube-system is never added twice"
    );
    assert!(
        relevant_namespaces(&[]).is_empty(),
        "knowing no namespaces must mean \"look everywhere\", not \"look only in kube-system\""
    );
}

// ---------------------------------------------------------------------
// Step 1: `kind export logs`
// ---------------------------------------------------------------------

#[test]
fn failure_diagnostics_exports_kind_logs_with_the_exact_argv_and_records_the_path() {
    let runner = Arc::new(FakeProcessRunner::new("adlab-baseline-logtest01\n"));
    let manager = KindClusterManager::new(runner.clone());
    let handle = handle_named("adlab-baseline-logtest01");
    let destination = std::env::temp_dir().join(format!(
        "admissionlab-kind-logs-{}",
        RunId::generate().as_str()
    ));
    let request = DiagnosticsRequest {
        namespaces: vec!["lab".to_owned()],
        logs_destination: Some(destination.clone()),
    };

    let bundle = block_on(manager.failure_diagnostics(&handle, &request));

    assert_eq!(
        runner.export_call(),
        Some(vec![
            OsString::from("export"),
            OsString::from("logs"),
            destination.clone().into_os_string(),
            OsString::from("--name"),
            OsString::from("adlab-baseline-logtest01"),
        ]),
        "the destination is one argv element and the cluster is named explicitly"
    );
    assert_eq!(bundle.kind_logs_path.as_ref(), Some(&destination));
    // The Kubernetes half could not run (this handle's kubeconfig does
    // not exist), and says so rather than reporting an empty cluster.
    assert!(
        bundle
            .notes
            .iter()
            .any(|note| note.contains("could not reach cluster")),
        "{:?}",
        bundle.notes
    );

    let _ = std::fs::remove_dir_all(&destination);
}

#[test]
fn failure_diagnostics_records_a_failed_export_as_a_note_and_no_path() {
    let runner =
        Arc::new(FakeProcessRunner::new("adlab-baseline-logtest02\n").with_failing_export());
    let manager = KindClusterManager::new(runner);
    let handle = handle_named("adlab-baseline-logtest02");
    let destination = std::env::temp_dir().join(format!(
        "admissionlab-kind-logs-{}",
        RunId::generate().as_str()
    ));

    let bundle = block_on(manager.failure_diagnostics(
        &handle,
        &DiagnosticsRequest {
            namespaces: Vec::new(),
            logs_destination: Some(destination.clone()),
        },
    ));

    assert!(
        bundle.kind_logs_path.is_none(),
        "a path must never be recorded for an export that failed"
    );
    assert!(
        bundle
            .notes
            .iter()
            .any(|note| note.contains("exited with") && note.contains("failed to export logs")),
        "the export failure must survive, tail and all: {:?}",
        bundle.notes
    );

    let _ = std::fs::remove_dir_all(&destination);
}

#[test]
fn failure_diagnostics_for_an_absent_cluster_is_empty_and_says_so() {
    // `kind get clusters` lists somebody else's cluster, not this one.
    let runner = Arc::new(FakeProcessRunner::new("some-other-cluster\n"));
    let manager = KindClusterManager::new(runner.clone());
    let handle = handle_named("adlab-baseline-gone000001");

    let bundle = block_on(manager.failure_diagnostics(
        &handle,
        &DiagnosticsRequest {
            namespaces: Vec::new(),
            logs_destination: Some(std::env::temp_dir().join("never-created")),
        },
    ));

    assert!(bundle.nodes.is_empty() && bundle.pods.is_empty() && bundle.events.is_empty());
    assert!(bundle.kind_logs_path.is_none());
    assert_eq!(
        bundle.notes.len(),
        1,
        "one note, naming the cluster: {:?}",
        bundle.notes
    );
    assert!(bundle.notes[0].contains("adlab-baseline-gone000001"));
    assert!(
        runner.export_call().is_none(),
        "there is nothing to export from a cluster that no longer exists"
    );
    assert!(
        !std::env::temp_dir().join("never-created").exists(),
        "a skipped export must not create its destination directory"
    );
}

#[test]
fn failure_diagnostics_skips_the_export_entirely_when_no_destination_is_given() {
    let runner = Arc::new(FakeProcessRunner::new("adlab-baseline-nodest0001\n"));
    let manager = KindClusterManager::new(runner.clone());

    let bundle = block_on(manager.failure_diagnostics(
        &handle_named("adlab-baseline-nodest0001"),
        &request_for(&[]),
    ));

    assert!(runner.export_call().is_none());
    assert!(bundle.kind_logs_path.is_none());
}

// ---------------------------------------------------------------------
// The default trait implementation
// ---------------------------------------------------------------------

/// A [`ClusterManager`] that implements nothing beyond the trait's
/// required methods — the shape every test double in this workspace has.
struct BareManager;

#[async_trait]
impl ClusterManager for BareManager {
    async fn resolve_node_image(
        &self,
        _kubernetes_version: &str,
    ) -> Result<String, admissionlab_core::ClusterError> {
        unreachable!("not exercised by this test")
    }

    async fn create(
        &self,
        _spec: &ClusterSpec,
        _paths: &admissionlab_core::RunPaths,
    ) -> Result<ClusterHandle, admissionlab_core::ClusterError> {
        unreachable!("not exercised by this test")
    }

    async fn delete(&self, _handle: &ClusterHandle) -> Result<(), admissionlab_core::ClusterError> {
        unreachable!("not exercised by this test")
    }

    async fn diagnostics(&self, handle: &ClusterHandle) -> admissionlab_core::ClusterDiagnostics {
        admissionlab_core::ClusterDiagnostics {
            cluster_name: handle.spec.name.clone(),
            cluster_exists: None,
            kubeconfig_present: false,
            audit_log_present: false,
            notes: Vec::new(),
        }
    }
}

#[test]
fn a_backend_that_collects_nothing_returns_an_empty_bundle_rather_than_failing_to_compile() {
    // The whole point of defaulting `failure_diagnostics`: a backend
    // with no Kubernetes client (every test double in this workspace)
    // keeps compiling, and answers honestly.
    let bundle = block_on(
        BareManager
            .failure_diagnostics(&handle_named("adlab-baseline-default01"), &request_for(&[])),
    );
    assert!(bundle.is_empty());
    assert_eq!(bundle.failure_hint(), "");
}

#[test]
fn the_failure_hint_names_the_stuck_containers_and_conditions() {
    let source = FakeSource {
        pods: vec![image_pull_backoff_pod("lab-webhook-0")],
        ..FakeSource::default()
    };
    let mut bundle = block_on(collect(&source, &request_for(&["lab"])));
    bundle.workloads.push(RedactedObjectSummary::from_api_object(
        &json!({
            "metadata": { "name": "lab-webhook", "namespace": "lab" },
            "status": { "conditions": [
                { "type": "Available", "status": "False", "reason": "MinimumReplicasUnavailable" }
            ]}
        }),
        "Deployment",
    ));

    let hint = bundle.failure_hint();

    assert!(
        hint.contains(
            "Deployment lab-webhook conditions: Available=False \
                       (MinimumReplicasUnavailable)"
        ),
        "unexpected hint: {hint}"
    );
    assert!(
        hint.contains("pod lab-webhook-0 app: ImagePullBackOff"),
        "unexpected hint: {hint}"
    );
}
