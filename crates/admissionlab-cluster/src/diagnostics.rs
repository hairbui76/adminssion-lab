//! Collecting the failure bundle a run leaves behind when a cluster —
//! or something installed onto it — does not come up (ROADMAP Task 9.5).
//!
//! Two independent halves, deliberately kept apart:
//!
//! - **Summaries**, collected through the Kubernetes API into
//!   [`admissionlab_core::FailureDiagnostics`]: nodes, pods, events, and
//!   webhook configurations, each reduced to
//!   [`admissionlab_core::RedactedObjectSummary`]/[`admissionlab_core::RedactedEvent`].
//!   These are safe to embed in a report (after the report crate's own
//!   redaction pass runs over the one free-text field — see
//!   `RedactedEvent`'s own documentation).
//! - **Raw `kind` logs**, exported to a directory inside the run
//!   workspace and referenced *by path only*. See "Raw logs never leave
//!   the machine" below.
//!
//! # Raw logs never leave the machine
//!
//! `kind export logs` writes every node's kubelet journal, containerd
//! log, and the full stdout/stderr of every container that ran in the
//! cluster. That is the most useful thing in existence when a component
//! will not start, and also the most dangerous thing to attach to a bug
//! report: **third-party components may log secrets** — a chart that
//! echoes its values on startup, a webhook that logs the request body it
//! rejected, an operator that prints a token on an auth failure.
//! Admission Lab therefore:
//!
//! - writes the export inside the run's own workspace, next to the other
//!   raw evidence whose protection is filesystem permissions rather than
//!   redaction (`docs/security.md`);
//! - records only its **path** in
//!   [`admissionlab_core::FailureDiagnostics::kind_logs_path`], never its
//!   content, so nothing in `diagnostics.json`, the job summary, or any
//!   report can carry a byte of it;
//! - states the warning ([`KIND_LOGS_WARNING`]) everywhere the path is
//!   surfaced, so the person who decides to upload it decides knowingly.
//!
//! # The offline seam
//!
//! Everything that talks to a cluster goes through
//! [`ClusterObjectSource`], with [`KubeObjectSource`] as the one
//! production implementation — the same shape (and the same
//! `client_for` chokepoint discipline) `admissionlab-installer`'s
//! `readiness` module already uses, and for the same reason: the
//! collection logic (which namespaces, what order, what gets truncated,
//! what becomes a note) is the part worth testing, and it is testable
//! with no cluster, no network, and no `kind` at all against a fake
//! source. The seam carries `serde_json::Value`s rather than typed
//! `k8s-openapi` objects so that a test double is a JSON literal instead
//! of a hand-built generated struct, and so that this crate's public API
//! does not grow a `k8s-openapi` version in its signatures.
//!
//! # Why summaries and not objects
//!
//! [`admissionlab_core::RedactedObjectSummary`] cannot hold a `spec`, an
//! annotation, or a Secret's `data` — it has no field that could. See
//! its own documentation for why that is a stronger guarantee than
//! redacting whole objects after the fact.

use std::path::Path;
use std::time::Duration;

use admissionlab_core::{
    ClusterHandle, DiagnosticsRequest, FailureDiagnostics, MAX_DIAGNOSTIC_EVENTS,
    MAX_DIAGNOSTIC_OBJECTS, RedactedEvent, RedactedObjectSummary,
};
use async_trait::async_trait;
use k8s_openapi::api::admissionregistration::v1::{
    MutatingWebhookConfiguration, ValidatingWebhookConfiguration,
};
use k8s_openapi::api::core::v1::{Event, Node, Pod};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Api, Client, Config};
use serde::Serialize;

/// The warning that accompanies a `kind_logs_path` wherever it is
/// surfaced — the terminal, the no-verdict job summary, the docs.
///
/// A single constant rather than three prose copies, so the three cannot
/// drift into saying different things about the same directory.
pub const KIND_LOGS_WARNING: &str = "raw kind logs are unredacted: third-party components may have logged secrets into them. \
     They stay in this run's workspace and are never embedded in a report — read one before \
     attaching it to an issue.";

/// How long one Kubernetes list call may take before it is abandoned and
/// reported as a note.
///
/// This whole module runs on a path that is *already* failing, often
/// against a cluster that is itself unhealthy, so every call needs a
/// bound (Global Constraint 13). Ten seconds is generous for a single
/// list against a local `kind` API server (milliseconds warm) while
/// keeping a whole bundle — at most six calls — well inside the time a
/// user will wait for a failure to be explained.
const OBJECT_LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the client waits to establish a connection to the API
/// server. Short deliberately: the common failure this module runs
/// against is a cluster whose API server is gone, and waiting the
/// `kube` default on every call would turn "explain the failure" into a
/// multi-minute hang.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The most objects one list call asks the API server for. A cap on the
/// *response*, complementing [`MAX_DIAGNOSTIC_OBJECTS`]'s cap on what is
/// kept: a cluster with ten thousand pods should not have all of them
/// serialized into this process's memory just so all but fifty can be
/// dropped.
const LIST_LIMIT: u32 = 500;

/// Where the summaries in a [`FailureDiagnostics`] come from.
///
/// Each method returns raw Kubernetes objects as JSON, or a
/// human-readable reason it could not — never a partial answer
/// presented as a whole one. See this module's documentation for why the
/// seam is shaped this way.
#[async_trait]
pub trait ClusterObjectSource: Send + Sync {
    /// Every node in the cluster.
    async fn nodes(&self) -> Result<Vec<serde_json::Value>, String>;

    /// Pods in `namespace`, or in every namespace when it is `None`.
    async fn pods(&self, namespace: Option<&str>) -> Result<Vec<serde_json::Value>, String>;

    /// Events in `namespace`, or in every namespace when it is `None`.
    async fn events(&self, namespace: Option<&str>) -> Result<Vec<serde_json::Value>, String>;

    /// Every validating and mutating webhook configuration. Each item
    /// carries its own `kind`, since the two are collected together.
    async fn webhook_configurations(&self) -> Result<Vec<serde_json::Value>, String>;
}

/// Collects `request`'s summaries from `source`.
///
/// Never fails: every unavailable piece becomes a
/// [`FailureDiagnostics::notes`] entry beside an empty list, so a reader
/// can always tell "there were no failing pods" apart from "pods could
/// not be listed" (Global Constraint 15). Truncation is reported the
/// same way — see [`FailureDiagnostics`] for the ordering rules this
/// applies before capping.
///
/// Does not export raw logs: that is backend-specific and lives on
/// [`crate::KindClusterManager`] itself.
pub async fn collect(
    source: &dyn ClusterObjectSource,
    request: &DiagnosticsRequest,
) -> FailureDiagnostics {
    let mut bundle = FailureDiagnostics::default();

    match source.nodes().await {
        Ok(nodes) => {
            bundle.nodes = nodes
                .iter()
                .map(|node| RedactedObjectSummary::from_api_object(node, "Node"))
                .collect();
        }
        Err(reason) => bundle.note(format!("could not list nodes: {reason}")),
    }

    // An empty namespace list means "every namespace" (see
    // `DiagnosticsRequest::namespaces`), which the source expresses as
    // `None` rather than as a namespace literally named "".
    let scopes: Vec<Option<&str>> = if request.namespaces.is_empty() {
        vec![None]
    } else {
        request
            .namespaces
            .iter()
            .map(|namespace| Some(namespace.as_str()))
            .collect()
    };

    for scope in scopes {
        let where_ = scope.map_or_else(
            || "all namespaces".to_owned(),
            |ns| format!("namespace {ns:?}"),
        );
        match source.pods(scope).await {
            Ok(pods) => bundle.pods.extend(
                pods.iter()
                    .map(|pod| RedactedObjectSummary::from_api_object(pod, "Pod")),
            ),
            Err(reason) => bundle.note(format!("could not list pods in {where_}: {reason}")),
        }
        match source.events(scope).await {
            Ok(events) => bundle
                .events
                .extend(events.iter().map(RedactedEvent::from_api_object)),
            Err(reason) => bundle.note(format!("could not list events in {where_}: {reason}")),
        }
    }

    match source.webhook_configurations().await {
        Ok(configurations) => {
            bundle.webhook_configurations = configurations
                .iter()
                .map(|configuration| {
                    RedactedObjectSummary::from_api_object(configuration, "WebhookConfiguration")
                })
                .collect();
        }
        Err(reason) => bundle.note(format!("could not list webhook configurations: {reason}")),
    }

    // Unhealthy first, then capped: the cap has to fall on the entries a
    // reader would have skipped anyway.
    bundle
        .pods
        .sort_by_key(|pod| i32::from(pod.looks_healthy()));
    cap(
        &mut bundle.pods,
        MAX_DIAGNOSTIC_OBJECTS,
        "pod",
        &mut bundle.notes,
    );
    bundle
        .events
        .sort_by_key(|event| i32::from(!event.is_warning()));
    cap(
        &mut bundle.events,
        MAX_DIAGNOSTIC_EVENTS,
        "event",
        &mut bundle.notes,
    );

    bundle
}

/// Truncates `items` to `limit`, recording what was dropped. A silent
/// truncation would leave a reader counting a capped list as the whole
/// population.
fn cap<T>(items: &mut Vec<T>, limit: usize, label: &str, notes: &mut Vec<String>) {
    if items.len() > limit {
        let dropped = items.len() - limit;
        items.truncate(limit);
        notes.push(format!(
            "{dropped} further {label} summar{} omitted: this bundle keeps at most {limit}, \
             unhealthy ones first",
            if dropped == 1 { "y was" } else { "ies were" },
        ));
    }
}

/// The production [`ClusterObjectSource`]: the real Kubernetes API of
/// one cluster, reached through that cluster's own kubeconfig.
pub struct KubeObjectSource {
    /// The client every list call below is issued through.
    client: Client,
}

impl KubeObjectSource {
    /// Builds a source for `cluster` from **its own** kubeconfig — never
    /// the operator's ambient `~/.kube/config`/`$KUBECONFIG`.
    ///
    /// This is this crate's only place a `kube::Client` is built,
    /// mirroring `admissionlab-installer`'s `readiness::client_for` and
    /// `lifecycle.rs`'s own `--kubeconfig`-always discipline: isolation
    /// that lives in one function cannot be silently dropped by a later
    /// call site reaching for `Client::try_default()`.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason when the kubeconfig is missing,
    /// unreadable, or malformed, or when a client cannot be built from
    /// it. Callers turn that into a note; nothing here is fatal.
    pub async fn for_cluster(cluster: &ClusterHandle) -> Result<Self, String> {
        let kubeconfig = Kubeconfig::read_from(&cluster.kubeconfig).map_err(|error| {
            format!(
                "could not read kubeconfig {}: {error}",
                cluster.kubeconfig.display()
            )
        })?;
        let mut config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
            .await
            .map_err(|error| format!("could not build a client configuration: {error}"))?;
        // Bounded twice over: `connect_timeout` catches an API server
        // that is gone (the common case here), `OBJECT_LIST_TIMEOUT`
        // catches one that accepts the connection and then stalls.
        config.connect_timeout = Some(CONNECT_TIMEOUT);
        config.read_timeout = Some(OBJECT_LIST_TIMEOUT);
        let client = Client::try_from(config)
            .map_err(|error| format!("could not build a Kubernetes client: {error}"))?;
        Ok(Self { client })
    }

    /// Lists one API's objects as JSON, bounded by
    /// [`OBJECT_LIST_TIMEOUT`] and [`LIST_LIMIT`].
    async fn list<K>(&self, api: Api<K>, what: &str) -> Result<Vec<serde_json::Value>, String>
    where
        K: Clone + std::fmt::Debug + serde::de::DeserializeOwned + Serialize,
    {
        let params = kube::api::ListParams::default().limit(LIST_LIMIT);
        let listed = tokio::time::timeout(OBJECT_LIST_TIMEOUT, api.list(&params))
            .await
            .map_err(|_elapsed| {
                format!(
                    "listing {what} did not complete within {}s",
                    OBJECT_LIST_TIMEOUT.as_secs()
                )
            })?
            .map_err(|error| error.to_string())?;
        listed
            .items
            .iter()
            .map(|item| serde_json::to_value(item).map_err(|error| error.to_string()))
            .collect()
    }
}

/// Sets `kind` on an object the API server returned without one (list
/// items carry no `kind` of their own), so that a summary of a
/// validating configuration is not indistinguishable from a mutating
/// one once both are in the same list.
fn with_kind(mut object: serde_json::Value, kind: &str) -> serde_json::Value {
    if let Some(map) = object.as_object_mut() {
        map.insert(
            "kind".to_owned(),
            serde_json::Value::String(kind.to_owned()),
        );
    }
    object
}

#[async_trait]
impl ClusterObjectSource for KubeObjectSource {
    async fn nodes(&self) -> Result<Vec<serde_json::Value>, String> {
        self.list(Api::<Node>::all(self.client.clone()), "nodes")
            .await
    }

    async fn pods(&self, namespace: Option<&str>) -> Result<Vec<serde_json::Value>, String> {
        let api = match namespace {
            Some(namespace) => Api::<Pod>::namespaced(self.client.clone(), namespace),
            None => Api::<Pod>::all(self.client.clone()),
        };
        self.list(api, "pods").await
    }

    async fn events(&self, namespace: Option<&str>) -> Result<Vec<serde_json::Value>, String> {
        let api = match namespace {
            Some(namespace) => Api::<Event>::namespaced(self.client.clone(), namespace),
            None => Api::<Event>::all(self.client.clone()),
        };
        self.list(api, "events").await
    }

    async fn webhook_configurations(&self) -> Result<Vec<serde_json::Value>, String> {
        let validating = self
            .list(
                Api::<ValidatingWebhookConfiguration>::all(self.client.clone()),
                "validating webhook configurations",
            )
            .await?;
        let mutating = self
            .list(
                Api::<MutatingWebhookConfiguration>::all(self.client.clone()),
                "mutating webhook configurations",
            )
            .await?;
        Ok(validating
            .into_iter()
            .map(|object| with_kind(object, "ValidatingWebhookConfiguration"))
            .chain(
                mutating
                    .into_iter()
                    .map(|object| with_kind(object, "MutatingWebhookConfiguration")),
            )
            .collect())
    }
}

/// The namespaces a failure bundle collects from when the caller knows
/// which ones its own components use: those, plus `kube-system`.
///
/// `kube-system` is always included because the failures this bundle
/// exists to explain frequently *originate* there — a CNI pod
/// crash-looping, a `coredns` pod stuck `Pending` on a node that never
/// became ready — while the component that visibly timed out is in its
/// own namespace looking merely unscheduled.
///
/// Returns an empty list for an empty input, which
/// [`DiagnosticsRequest::namespaces`] reads as "every namespace": a
/// caller that knows nothing about the run's layout should get
/// everything (capped) rather than `kube-system` alone.
#[must_use]
pub fn relevant_namespaces(component_namespaces: &[String]) -> Vec<String> {
    if component_namespaces.is_empty() {
        return Vec::new();
    }
    let mut namespaces: Vec<String> = component_namespaces.to_vec();
    namespaces.sort();
    namespaces.dedup();
    if !namespaces
        .iter()
        .any(|namespace| namespace == "kube-system")
    {
        namespaces.push("kube-system".to_owned());
    }
    namespaces
}

/// Whether `destination` is usable as the directory `kind export logs`
/// writes into: it must be absolute, for the same reason every other
/// host path this crate hands to `kind` must be (see
/// [`admissionlab_core::ClusterError::NonAbsolutePath`]) — a relative
/// path would otherwise be resolved against whatever working directory
/// the `kind` child happens to inherit.
///
/// # Errors
///
/// Returns a human-readable reason when it is not.
pub(crate) fn validate_logs_destination(destination: &Path) -> Result<(), String> {
    if destination.is_absolute() {
        Ok(())
    } else {
        Err(format!(
            "log export destination {} must be an absolute path",
            destination.display()
        ))
    }
}
