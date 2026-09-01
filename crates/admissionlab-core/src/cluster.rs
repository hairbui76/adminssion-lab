//! The cluster lifecycle abstraction: the trait every ephemeral-cluster
//! backend implements, and the data types that flow through it.
//!
//! # Why this lives in `admissionlab-core`, not `admissionlab-cluster`
//!
//! The plan's crate map (`admissionlab-cluster: kind lifecycle, kubeconfig
//! handling, health checks, diagnostics`) reads as though
//! [`ClusterManager`] belongs in `admissionlab-cluster`. It cannot live
//! there without creating a dependency cycle:
//!
//! - [`ClusterManager::create`] takes `&RunPaths`, and [`ClusterSpec`]
//!   holds a [`crate::Side`] — both are `admissionlab-core` types, so
//!   whichever crate defines the trait must depend on `admissionlab-core`.
//!   That direction (`admissionlab-cluster -> admissionlab-core`) is fine
//!   on its own.
//! - A later task places `LabRunner<C: ClusterManager>` inside
//!   `admissionlab-core` itself (`crates/admissionlab-core/src/run.rs`),
//!   so `admissionlab-core` must be able to name the trait too. If the
//!   trait lived in `admissionlab-cluster`, that would force
//!   `admissionlab-core -> admissionlab-cluster`.
//!
//! Together those two constraints are a cycle, which Cargo rejects
//! outright. Defining [`ClusterManager`] and its data types
//! (`ClusterSpec`, `ClusterHandle`, `ClusterError`, `ClusterDiagnostics`)
//! here instead resolves it in the only direction that works:
//! `admissionlab-cluster` depends on `admissionlab-core`, never the
//! reverse. This mirrors a precedent already in this crate:
//! [`crate::process::ProcessRunner`] (the abstraction) and
//! [`crate::process::TokioProcessRunner`] (one concrete implementation)
//! both live here, while a *different* concrete implementation could live
//! in any downstream crate without ever requiring this crate to depend on
//! it. `admissionlab-cluster`'s `KindClusterManager` is exactly that: a
//! concrete [`ClusterManager`] implementation, defined downstream.
//!
//! # Why `admissionlab-cluster`-specific errors are not named here
//!
//! [`ClusterError`] cannot hold `admissionlab-cluster`'s own error types
//! (for example, the error `render_kind_config` returns) as typed fields,
//! because that would reintroduce the exact dependency this module exists
//! to avoid: `admissionlab-core` must not depend on `admissionlab-cluster`
//! even to name one of its types in a `#[source]` field. Where a concrete
//! [`ClusterManager`] implementation needs to report a failure from its
//! own crate-specific error type, it renders that error to a `String`
//! first (see [`ClusterError::KindConfigRender`]).
//!
//! # `diagnostics` never fails
//!
//! [`ClusterManager::diagnostics`] returns a bare [`ClusterDiagnostics`],
//! not a `Result`: it is a best-effort, point-in-time snapshot, and a
//! failure to determine any one piece of it (for example, `kind` itself
//! being unreachable) is data the snapshot reports, not a reason to fail
//! the whole call. Every field that depends on an external probe
//! degrades to `None`/a note in [`ClusterDiagnostics::notes`] rather than
//! a guessed value when that probe could not run — see Global
//! Constraint 15 ("missing data is unavailable/unknown, never
//! fabricated").

use std::fmt;
use std::path::PathBuf;
use std::process::ExitStatus;

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;

use crate::artifact::{ArtifactError, RunPaths};
use crate::process::{CommandContext, ProcessError};
use crate::side::Side;

/// What cluster to create: which side of the comparison it stands in
/// for, its already-validated name, and the Kubernetes version/node
/// image to provision.
///
/// `kubernetes_version` and `node_image` are carried separately even
/// though a real cluster only needs `node_image` (already resolved and
/// digest-pinned by whoever built this `ClusterSpec`, for example via
/// `admissionlab_cluster::resolve_node_image`): `kubernetes_version` is
/// provenance a caller wants to keep alongside the resolved image
/// (for a run manifest or a report) without needing to re-derive it from
/// the image reference later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterSpec {
    /// Which side of the baseline/candidate comparison this cluster is.
    pub side: Side,
    /// The cluster's name, already assembled and validated (see
    /// `admissionlab_cluster::cluster_name`). [`ClusterManager::create`]
    /// implementations must still validate this defensively — it is a
    /// plain, publicly constructible `String`, so nothing prevents a
    /// caller from building one directly without going through that
    /// helper.
    pub name: String,
    /// The requested Kubernetes version, for provenance (for example
    /// `"1.36.4"`). Not necessarily what ends up running: `node_image`
    /// is what a [`ClusterManager`] implementation actually provisions.
    pub kubernetes_version: String,
    /// The node's container image reference, ideally already
    /// digest-pinned. Passed through verbatim by a `kind`-backed
    /// implementation.
    pub node_image: String,
    /// Container images already present in the operator's local image
    /// store that must be side-loaded into this cluster before it is
    /// handed back (ROADMAP Task 6.11), from
    /// [`admissionlab_spec::ResolvedEnvironment::images`]. Empty for
    /// every lab that names none, which is the overwhelmingly common
    /// case.
    ///
    /// # Why this is part of the cluster's *spec*
    ///
    /// Because it is a statement about what the cluster must be, not a
    /// step someone performs on it afterwards. A manifest referencing a
    /// locally built image with `imagePullPolicy: IfNotPresent` does not
    /// fail at install time — it fails minutes later as an
    /// `ErrImageNeverPull` on a pod nobody was watching, reading as a
    /// broken fixture rather than a missing image. Making the load part
    /// of [`ClusterManager::create`]'s contract means a cluster either
    /// has the images its lab declared or does not exist, and the two
    /// cases are never confused.
    ///
    /// Each entry is passed to the backend as **one argv element**
    /// (Global Constraint 12); nothing here is interpolated into a
    /// shell string. An implementation with no notion of a local image
    /// store may ignore this field, but must then reject a non-empty
    /// list rather than silently proceeding — a run that quietly did not
    /// load what it was told to load is one whose later failure is
    /// unattributable.
    pub images: Vec<String>,
}

/// A successfully created cluster: enough for a caller to use it
/// (`kubeconfig`) and to find its evidence afterward (`audit_log`),
/// without needing to re-derive either path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterHandle {
    /// The spec this cluster was created from.
    pub spec: ClusterSpec,
    /// Absolute path to this cluster's own kubeconfig. Isolated per
    /// cluster (see [`ClusterManager::create`]'s documentation): never
    /// the user's own `~/.kube/config`, and never shared between a
    /// baseline and a candidate cluster from the same run.
    pub kubeconfig: PathBuf,
    /// Absolute path to this cluster's kube-apiserver audit log file, on
    /// the real host (not inside the ephemeral node).
    pub audit_log: PathBuf,
}

/// Best-effort, point-in-time information about one cluster, returned by
/// [`ClusterManager::diagnostics`].
///
/// Kept to what a caller genuinely needs to understand a failure: is the
/// cluster still there (so a caller can tell "no leaked cluster" apart
/// from "cluster leaked"), and are the two files a caller would look at
/// next (`kubeconfig`, the audit log) actually present. See the module
/// documentation's "`diagnostics` never fails" section for why every
/// field here is honest about what could not be determined rather than
/// guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterDiagnostics {
    /// The cluster name this snapshot describes, copied from the handle
    /// for convenience.
    pub cluster_name: String,
    /// Whether the cluster backend currently reports a cluster with this
    /// name as still existing. `None` when that probe itself could not
    /// be run or its output could not be parsed — never guessed.
    pub cluster_exists: Option<bool>,
    /// Whether [`ClusterHandle::kubeconfig`] currently exists as a
    /// non-empty file. Checked directly on the local filesystem, so this
    /// is never itself "unknown" — only `false` (missing, empty, or
    /// unreadable; see `notes` for which) or `true`.
    pub kubeconfig_present: bool,
    /// Whether [`ClusterHandle::audit_log`] currently exists as a
    /// non-empty file. Same caveats as `kubeconfig_present`.
    pub audit_log_present: bool,
    /// Human-readable notes on anything that could not be determined, or
    /// any other detail a reader trying to understand this cluster's
    /// state would want. Empty when nothing needs calling out.
    pub notes: Vec<String>,
}

/// How many object summaries of one kind a [`FailureDiagnostics`] bundle
/// keeps.
///
/// A failed run's bundle is embedded in `diagnostics.json` and read by a
/// human; a cluster with a broken component can easily hold hundreds of
/// pods, and the fiftieth one adds nothing the first fifty did not
/// already say. Whoever fills the bundle is expected to put the
/// *unhealthy* objects first (see [`FailureDiagnostics`]), so the cap
/// falls on the least interesting entries, and a
/// [`FailureDiagnostics::notes`] entry always says how many were
/// dropped — a truncated list never silently reads as a complete one
/// (Global Constraint 15).
pub const MAX_DIAGNOSTIC_OBJECTS: usize = 50;

/// How many events a [`FailureDiagnostics`] bundle keeps. See
/// [`MAX_DIAGNOSTIC_OBJECTS`]; events are noisier still (a
/// `CrashLoopBackOff` alone emits one every few seconds).
pub const MAX_DIAGNOSTIC_EVENTS: usize = 50;

/// How many characters of one free-text string a summary keeps.
///
/// Every string in a summary is either a short Kubernetes enum-like
/// token (`"ImagePullBackOff"`, `"True"`) or controller-authored prose
/// ([`RedactedEvent::message`]). The latter is unbounded at the source,
/// so it is bounded here, at construction, for the same reason
/// `admissionlab_core::output_tail` bounds command output: an artifact
/// a single pathological value can blow up is not bounded at all.
pub const MAX_SUMMARY_TEXT_CHARS: usize = 400;

/// One `status.conditions[]` entry, reduced to the four fields that say
/// what a controller decided and when.
///
/// Deliberately **no `message` field**: a condition's `message` is
/// unbounded, controller-authored prose that can quote arbitrary object
/// content (an admission webhook's rejection text quoting the object it
/// rejected, for instance). `reason` — a short, enum-like token from a
/// fixed per-controller vocabulary (`"MinimumReplicasUnavailable"`) —
/// carries the actionable half without that risk. See
/// [`RedactedObjectSummary`] for the redaction-by-construction argument
/// this omission is part of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConditionSummary {
    /// The condition's `type` (for example `"Ready"`, `"Available"`).
    pub condition_type: String,
    /// Its `status`, verbatim: `"True"`, `"False"`, or `"Unknown"`.
    pub status: String,
    /// Its `reason`, when the controller set one.
    pub reason: Option<String>,
    /// Its `lastTransitionTime`, as the RFC 3339 string the API server
    /// reported. Kept as text rather than parsed: this is evidence to
    /// display, and a parse that could fail would be one more way for a
    /// best-effort snapshot to lose information it already had.
    pub last_transition: Option<String>,
}

/// One container's status inside a Pod summary: enough to name *why* a
/// pod is not running, and nothing more.
///
/// `reason` is where `ImagePullBackOff`, `CrashLoopBackOff`,
/// `CreateContainerConfigError` and friends surface — the single most
/// actionable field in an install-timeout bundle. The matching
/// `message` (which quotes the image reference, a registry URL, and
/// occasionally a credential-bearing pull-secret name) is deliberately
/// not carried; see [`RedactedObjectSummary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerStatusSummary {
    /// The container's name.
    pub name: String,
    /// Whether the kubelet currently considers it ready.
    pub ready: bool,
    /// How many times it has restarted.
    pub restarts: i64,
    /// Which of the three container states it is in: `"running"`,
    /// `"waiting"`, `"terminated"`, or `"unknown"` when the status
    /// carried none of them.
    pub state: String,
    /// The `reason` on that state, when it has one (`"ImagePullBackOff"`,
    /// `"CrashLoopBackOff"`, `"Error"`, …).
    pub reason: Option<String>,
    /// The exit code of a terminated container, when it reported one.
    pub exit_code: Option<i64>,
}

/// One Kubernetes object, reduced to identity plus status.
///
/// # Redacted by construction
///
/// This type is the whole redaction argument for [`FailureDiagnostics`]:
/// it **cannot hold arbitrary object content**, because it has nowhere
/// to put any. Every field is a `String`, `bool`, `i64`, or a `Vec` of
/// the two small status types above; there is no `serde_json::Value`,
/// no map, and no free-text field other than the enum-like `reason`s and
/// `status`es Kubernetes controllers write. A `spec` (with its
/// `env`/`envFrom`/`volumes`/`command`), an `annotation`, a Secret's
/// `data`, and a `managedFields` entry have no representation here at
/// all, so no collector — present or future — can leak one through this
/// type by forgetting a rule. That is a stronger guarantee than running
/// a whole object through a redaction pass, which is only ever as good
/// as its list of known-sensitive shapes.
///
/// Constructing one from a raw API object is
/// `admissionlab_cluster::diagnostics`'s job (this crate stays free of
/// any Kubernetes client — Global Constraint 6), which is why the
/// fields are plain and public rather than hidden behind a parser here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactedObjectSummary {
    /// The object's kind (`"Pod"`, `"Node"`,
    /// `"ValidatingWebhookConfiguration"`). Supplied by the collector
    /// when a list item's own `kind` is empty, as it is for most typed
    /// list responses.
    pub kind: String,
    /// The object's `metadata.name`.
    pub name: String,
    /// Its `metadata.namespace`, or `None` for a cluster-scoped object.
    pub namespace: Option<String>,
    /// Its `status.phase` where it has one (`"Pending"`, `"Running"`,
    /// `"Failed"`), `None` otherwise. Never invented for a kind that has
    /// no phase.
    pub phase: Option<String>,
    /// Its `status.conditions`, in the order the API server reported
    /// them.
    pub conditions: Vec<ConditionSummary>,
    /// Its containers' statuses (init containers included), for a Pod.
    /// Empty for every other kind.
    pub containers: Vec<ContainerStatusSummary>,
    /// Its `metadata.creationTimestamp`, as reported.
    pub created_at: Option<String>,
}

/// One `Event`, reduced to what it says happened.
///
/// # `message` is the one free-text field, and it is not self-redacting
///
/// Unlike [`RedactedObjectSummary`], this type does carry controller
/// prose: an event's `message` *is* the evidence (`Failed to pull image
/// "…": …`), and dropping it would leave the bundle unable to explain
/// the very failures it exists to explain. It is bounded
/// ([`MAX_SUMMARY_TEXT_CHARS`]) and control characters are stripped at
/// construction, but bounding is not redaction: a third-party
/// controller can put anything in a message, including a token it
/// should not have logged.
///
/// So the contract is: **a bundle must go through
/// `admissionlab_report::redact_failure_diagnostics` before it is
/// embedded in any artifact a human reads.** That pass applies Global
/// Constraint 14's string rules (headers, PEM private keys) to this
/// field, exactly as it already does to every diagnostic message in a
/// report. The type documents the requirement; the CLI's failure-artifact
/// path is where it is honored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactedEvent {
    /// The event's namespace.
    pub namespace: Option<String>,
    /// `"Normal"` or `"Warning"`, as reported.
    pub event_type: Option<String>,
    /// The event's short `reason` token (`"Failed"`, `"BackOff"`,
    /// `"FailedScheduling"`).
    pub reason: Option<String>,
    /// The event's `message` — free text; see this type's documentation.
    pub message: String,
    /// `"<kind>/<name>"` for the object the event is about, assembled
    /// from `involvedObject` rather than carrying the whole reference
    /// (which also holds a resource version and a field path nobody
    /// reading a failure bundle needs).
    pub involved_object: Option<String>,
    /// How many times this event has fired, when the API server
    /// aggregated a count.
    pub count: Option<i64>,
    /// When it first fired, as reported.
    pub first_seen: Option<String>,
    /// When it last fired, as reported.
    pub last_seen: Option<String>,
}

/// What a cluster looked like at the moment something failed on it:
/// ROADMAP Task 9.5's bundle.
///
/// # Why this is not [`ClusterDiagnostics`]
///
/// Task 9.5 froze this shape under the name `ClusterDiagnostics`, but
/// that name was already taken *in this module* by Controller Ruling
/// R22's own frozen type — the "is the cluster still there, are its two
/// files present" snapshot [`ClusterManager::diagnostics`] returns, which
/// several implementations and tests already construct by name. Rather
/// than redefine one frozen type or introduce a second type with the
/// same name in a second crate (`admissionlab_core::ClusterDiagnostics`
/// and `admissionlab_cluster::ClusterDiagnostics` would be a genuinely
/// confusing pair to have in one `use` list), the five frozen fields
/// live here verbatim under a name that says what they are for. The two
/// types are complements, not rivals: [`ClusterManager::diagnostics`]
/// answers "does this cluster still exist", this one answers "what was
/// wrong inside it".
///
/// # Ordering and truncation
///
/// A collector fills `pods` and `events` **unhealthy-first** and caps
/// them at [`MAX_DIAGNOSTIC_OBJECTS`]/[`MAX_DIAGNOSTIC_EVENTS`], adding a
/// [`Self::notes`] entry saying how many were dropped. Nothing here is
/// ever fabricated: a list that could not be fetched is empty *and*
/// carries a note saying why (Global Constraint 15), which is what lets
/// a reader tell "no pods were failing" apart from "pods could not be
/// listed".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureDiagnostics {
    /// Every node, summarized. Small and always worth having: a
    /// `NotReady` node explains a whole cluster's worth of pending pods.
    pub nodes: Vec<RedactedObjectSummary>,
    /// Pods in the requested namespaces, unhealthy ones first.
    pub pods: Vec<RedactedObjectSummary>,
    /// The workload objects a readiness check was actually waiting on
    /// (a `Deployment`, `DaemonSet`, `Job`, or custom resource), summarized
    /// from the last observation the probe made before it gave up.
    ///
    /// Not part of Task 9.5's frozen five fields, and added for the
    /// reason Step 3 names: on a readiness timeout the *last observed
    /// conditions* are the evidence, and the installer already holds
    /// them (`ReadinessEvidence::last_observed`) without a single extra
    /// API call. Folding them into `pods` would be a lie about what kind
    /// of object they are; a separate field says exactly what it is.
    pub workloads: Vec<RedactedObjectSummary>,
    /// Events in the requested namespaces, `Warning`s first.
    pub events: Vec<RedactedEvent>,
    /// Every validating and mutating webhook configuration, summarized —
    /// the objects whose presence (or absence, or `failurePolicy`) most
    /// often explains why a cluster stopped accepting writes.
    pub webhook_configurations: Vec<RedactedObjectSummary>,
    /// Where `kind export logs` wrote this cluster's raw logs, when they
    /// were exported.
    ///
    /// **A path, never content.** Raw kind logs are the unfiltered
    /// stdout/stderr of every container in the cluster, including
    /// third-party components that may log secrets. They stay on the
    /// operator's own disk, inside the run workspace, and nothing ever
    /// embeds them in a report — see
    /// `admissionlab_cluster::diagnostics`'s own warning, `docs/security.md`,
    /// and the no-verdict job summary, all of which say so where a
    /// reader will see it.
    pub kind_logs_path: Option<PathBuf>,
    /// Human-readable notes about anything that could not be collected,
    /// or was truncated. Empty when the bundle is complete.
    pub notes: Vec<String>,
}

impl FailureDiagnostics {
    /// Whether this bundle carries no evidence at all (not even a note).
    ///
    /// Used by callers that only attach a bundle when there is something
    /// in it: an empty `cluster` key in `diagnostics.json` tells a reader
    /// nothing that its absence does not.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
            && self.pods.is_empty()
            && self.workloads.is_empty()
            && self.events.is_empty()
            && self.webhook_configurations.is_empty()
            && self.kind_logs_path.is_none()
            && self.notes.is_empty()
    }

    /// Appends a note. See [`Self::notes`].
    pub fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    /// A one-line, already-bounded summary of *why* things look broken,
    /// for an error message that has to stand on its own.
    ///
    /// This exists because the most valuable half of a failure bundle —
    /// "this pod is in `ImagePullBackOff`" — is worth nothing if the
    /// only place it appears is a JSON artifact the reader has not
    /// thought to open. An install timeout's rendered error is printed
    /// to the terminal, written into `diagnostics.json`'s `failure`, and
    /// quoted in the job summary, so the hint travels everywhere the
    /// failure does (PRODUCT.md §33: enough on first failure that a user
    /// need not rerun to learn what happened).
    ///
    /// Empty when there is nothing to say — never a filler phrase that
    /// would read as evidence. At most [`Self::MAX_HINT_ITEMS`] items,
    /// each already bounded by construction
    /// ([`MAX_SUMMARY_TEXT_CHARS`]), so this cannot be the string that
    /// blows up an error message.
    #[must_use]
    pub fn failure_hint(&self) -> String {
        let mut parts = Vec::new();
        for workload in &self.workloads {
            let failing: Vec<String> = workload
                .conditions
                .iter()
                .filter(|condition| condition.status != "True")
                .map(|condition| match &condition.reason {
                    Some(reason) => format!(
                        "{}={} ({reason})",
                        condition.condition_type, condition.status
                    ),
                    None => format!("{}={}", condition.condition_type, condition.status),
                })
                .collect();
            if !failing.is_empty() {
                parts.push(format!(
                    "{} {} conditions: {}",
                    workload.kind,
                    workload.name,
                    failing.join(", ")
                ));
            }
        }
        for pod in &self.pods {
            let reasons: Vec<String> = pod
                .containers
                .iter()
                .filter(|container| !container.ready)
                .filter_map(|container| {
                    container
                        .reason
                        .as_ref()
                        .map(|reason| format!("{}: {reason}", container.name))
                })
                .collect();
            if reasons.is_empty() {
                continue;
            }
            parts.push(format!("pod {} {}", pod.name, reasons.join(", ")));
        }
        let shown: Vec<String> = parts.iter().take(Self::MAX_HINT_ITEMS).cloned().collect();
        if shown.is_empty() {
            return String::new();
        }
        let remaining = parts.len().saturating_sub(shown.len());
        let suffix = if remaining == 0 {
            String::new()
        } else {
            format!(" (+{remaining} more)")
        };
        format!("{}{suffix}", shown.join("; "))
    }

    /// How many items [`Self::failure_hint`] names before it says how
    /// many more there are. Three is what fits on a terminal line
    /// without the message becoming the report.
    pub const MAX_HINT_ITEMS: usize = 3;
}

/// Building the summaries above from raw Kubernetes API JSON.
///
/// # Why the parser lives here, in a client-free crate
///
/// This crate depends on no Kubernetes client and must not (Global
/// Constraint 6) — but *parsing already-fetched JSON* is not talking to
/// a cluster, and two callers need the identical mapping: the cluster
/// backend, which lists Pods/Events/Nodes to build a whole bundle, and
/// the installer, whose readiness probe already holds the last object it
/// observed (`ReadinessEvidence::last_observed`, a
/// `serde_json::Value`) when a component times out. Those two crates
/// cannot share code through each other — `admissionlab-installer`
/// deliberately does not depend on `admissionlab-cluster` (see its
/// `Cargo.toml`) — so a parser in either one would have to be written
/// twice, and two copies of a redaction-relevant mapping is exactly the
/// competing-synonym drift this workspace forbids. One copy, here,
/// beside the types it builds.
///
/// Everything below reads *named paths only*: it never iterates an
/// object's keys, so a field nobody named cannot reach a summary. That
/// is the mechanical half of [`RedactedObjectSummary`]'s
/// redacted-by-construction claim.
impl RedactedObjectSummary {
    /// Summarizes one Kubernetes object.
    ///
    /// `default_kind` is used when the object carries no `kind` of its
    /// own, which is the normal case for an item inside a typed `List`
    /// response (the API server sets `kind` on the list, not on each
    /// item).
    #[must_use]
    pub fn from_api_object(object: &serde_json::Value, default_kind: &str) -> Self {
        let metadata = &object["metadata"];
        let status = &object["status"];
        Self {
            kind: summary_text(object["kind"].as_str())
                .filter(|kind| !kind.is_empty())
                .unwrap_or_else(|| default_kind.to_owned()),
            name: summary_text(metadata["name"].as_str()).unwrap_or_default(),
            namespace: summary_text(metadata["namespace"].as_str()),
            phase: summary_text(status["phase"].as_str()),
            conditions: status["conditions"]
                .as_array()
                .map(|conditions| conditions.iter().map(ConditionSummary::from_api).collect())
                .unwrap_or_default(),
            containers: container_statuses(status),
            created_at: summary_text(metadata["creationTimestamp"].as_str()),
        }
    }

    /// Whether this object looks healthy, used to order a bundle's pods
    /// unhealthy-first before truncation (see [`FailureDiagnostics`]).
    ///
    /// Conservative in the direction that matters: anything this
    /// function cannot positively call healthy sorts as unhealthy, so a
    /// shape it does not understand is kept rather than dropped.
    #[must_use]
    pub fn looks_healthy(&self) -> bool {
        let phase_ok = matches!(self.phase.as_deref(), Some("Running" | "Succeeded") | None);
        let containers_ok = self
            .containers
            .iter()
            .all(|container| container.ready || container.state == "terminated");
        let conditions_ok = self.conditions.iter().all(|condition| {
            // Only the conditions whose *healthy* value is known are
            // judged; anything else (a custom resource's own condition
            // vocabulary) is left alone rather than guessed at.
            match condition.condition_type.as_str() {
                "Ready" | "Available" | "ContainersReady" | "PodScheduled" | "Initialized" => {
                    condition.status == "True"
                }
                "MemoryPressure" | "DiskPressure" | "PIDPressure" | "NetworkUnavailable" => {
                    condition.status == "False"
                }
                _ => true,
            }
        });
        phase_ok && containers_ok && conditions_ok
    }
}

impl ConditionSummary {
    /// Summarizes one `status.conditions[]` entry. See this type's
    /// documentation for why `message` is not among the fields read.
    #[must_use]
    fn from_api(condition: &serde_json::Value) -> Self {
        Self {
            condition_type: summary_text(condition["type"].as_str()).unwrap_or_default(),
            status: summary_text(condition["status"].as_str()).unwrap_or_default(),
            reason: summary_text(condition["reason"].as_str()),
            last_transition: summary_text(condition["lastTransitionTime"].as_str()),
        }
    }
}

impl RedactedEvent {
    /// Summarizes one `Event`.
    ///
    /// Handles both event shapes the API server produces: the classic
    /// `firstTimestamp`/`lastTimestamp` pair and the `eventTime` a
    /// `events.k8s.io` event carries instead, so a bundle never reports
    /// "unknown when" for an event that did say when.
    #[must_use]
    pub fn from_api_object(event: &serde_json::Value) -> Self {
        let involved = &event["involvedObject"];
        let involved_object = match (
            summary_text(involved["kind"].as_str()),
            summary_text(involved["name"].as_str()),
        ) {
            (Some(kind), Some(name)) => Some(format!("{kind}/{name}")),
            (Some(kind), None) => Some(kind),
            (None, Some(name)) => Some(name),
            (None, None) => None,
        };
        let event_time = summary_text(event["eventTime"].as_str());
        Self {
            namespace: summary_text(event["metadata"]["namespace"].as_str()),
            event_type: summary_text(event["type"].as_str()),
            reason: summary_text(event["reason"].as_str()),
            message: summary_text(event["message"].as_str()).unwrap_or_default(),
            involved_object,
            count: event["count"].as_i64(),
            first_seen: summary_text(event["firstTimestamp"].as_str())
                .or_else(|| event_time.clone()),
            last_seen: summary_text(event["lastTimestamp"].as_str()).or(event_time),
        }
    }

    /// Whether this event is one a reader of a failure bundle wants
    /// first: anything the API server did not label `Normal`.
    #[must_use]
    pub fn is_warning(&self) -> bool {
        self.event_type.as_deref() != Some("Normal")
    }
}

/// Both container status lists on a Pod status, init containers first
/// (they run first, and an init container stuck on `ImagePullBackOff` is
/// why the app containers have no status at all yet).
fn container_statuses(status: &serde_json::Value) -> Vec<ContainerStatusSummary> {
    ["initContainerStatuses", "containerStatuses"]
        .iter()
        .filter_map(|key| status[*key].as_array())
        .flatten()
        .map(ContainerStatusSummary::from_api)
        .collect()
}

impl ContainerStatusSummary {
    /// Summarizes one `containerStatuses[]` entry, reading only the
    /// `reason`/`exitCode` out of whichever state block is present. See
    /// this type's documentation for why the state's `message` is not
    /// read.
    #[must_use]
    fn from_api(container: &serde_json::Value) -> Self {
        let state = &container["state"];
        let (state_name, reason, exit_code) = if state["waiting"].is_object() {
            (
                "waiting",
                summary_text(state["waiting"]["reason"].as_str()),
                None,
            )
        } else if state["terminated"].is_object() {
            (
                "terminated",
                summary_text(state["terminated"]["reason"].as_str()),
                state["terminated"]["exitCode"].as_i64(),
            )
        } else if state["running"].is_object() {
            ("running", None, None)
        } else {
            ("unknown", None, None)
        };
        Self {
            name: summary_text(container["name"].as_str()).unwrap_or_default(),
            ready: container["ready"].as_bool().unwrap_or(false),
            restarts: container["restartCount"].as_i64().unwrap_or(0),
            state: state_name.to_owned(),
            reason,
            exit_code,
        }
    }
}

/// One string on its way into a summary: control characters replaced,
/// whitespace trimmed, length capped at [`MAX_SUMMARY_TEXT_CHARS`], and
/// an empty result reported as `None` rather than as `Some("")`.
///
/// Control characters are stripped because these strings are rendered
/// into a terminal report and a Markdown job summary: an ANSI escape
/// sequence in a controller's message would otherwise repaint a user's
/// terminal. Truncation counts [`char`]s, never bytes, so a multi-byte
/// character is never split (the same rule `admissionlab_report`'s own
/// truncation follows).
///
/// This is **bounding, not redaction** — see [`RedactedEvent`].
fn summary_text(value: Option<&str>) -> Option<String> {
    let raw = value?;
    let cleaned: String = raw
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut bounded: String = trimmed.chars().take(MAX_SUMMARY_TEXT_CHARS).collect();
    if trimmed.chars().nth(MAX_SUMMARY_TEXT_CHARS).is_some() {
        bounded.push('…');
    }
    Some(bounded)
}

/// What a caller wants collected into a [`FailureDiagnostics`].
///
/// Kept as a struct rather than two parameters so that a later task
/// which needs a third input (a time window, a label selector) adds a
/// field here instead of changing [`ClusterManager::failure_diagnostics`]'s
/// signature and every implementation of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticsRequest {
    /// Which namespaces to collect Pods and Events from — in practice
    /// the namespaces the run's own components install into, plus
    /// `kube-system`. **Empty means every namespace**, which is the
    /// right default for a failure nobody has localized yet.
    pub namespaces: Vec<String>,
    /// Where to export the backend's own raw cluster logs (for `kind`,
    /// `kind export logs <dir>`), or `None` to skip that export
    /// entirely. The directory is created by the implementation; it must
    /// be inside the run's own workspace, since what lands there is
    /// unredacted (see [`FailureDiagnostics::kind_logs_path`]).
    pub logs_destination: Option<PathBuf>,
}

/// What happened when a [`ClusterManager`] implementation attempted a
/// best-effort cleanup after a create failure it believed might have
/// left a cluster behind (see [`ClusterError::CreateFailedWithRollback`]).
///
/// This exists precisely so that a failed cleanup can never look like a
/// successful one, or silently disappear: whichever variant this is,
/// the *original* create failure is always available too, as the
/// `source` of the enclosing [`ClusterError::CreateFailedWithRollback`].
#[derive(Debug)]
pub enum RollbackOutcome {
    /// The best-effort delete command completed successfully.
    Deleted,
    /// The best-effort delete command itself failed. The original create
    /// failure (the enclosing [`ClusterError::CreateFailedWithRollback`]'s
    /// `source`) is unaffected by this — this only describes whether
    /// *cleanup* additionally failed.
    Failed(Box<ClusterError>),
}

impl fmt::Display for RollbackOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deleted => f.write_str("cleanup deleted the cluster"),
            Self::Failed(source) => write!(f, "cleanup also failed: {source}"),
        }
    }
}

/// Failure modes of [`ClusterManager::create`] and
/// [`ClusterManager::delete`].
#[derive(Debug, Error)]
pub enum ClusterError {
    /// A [`ClusterSpec::name`] (or a name assembled by a helper such as
    /// `admissionlab_cluster::cluster_name`) is not safe to hand to a
    /// cluster backend: not a valid DNS-1123 label, or too long once the
    /// backend's own derived names (for example `kind`'s
    /// `<name>-control-plane` Docker container/Kubernetes node name) are
    /// taken into account.
    #[error("cluster name {name:?} is invalid: {reason}")]
    InvalidName {
        /// The rejected name, exactly as given.
        name: String,
        /// A human-readable explanation of which rule it failed.
        reason: String,
    },
    /// A path this [`ClusterManager`] implementation derived from
    /// [`RunPaths`] was not absolute. A cluster backend that bind-mounts
    /// host paths into a container (as `kind` does for the audit policy
    /// and audit log) needs an absolute host path; a relative one would
    /// otherwise fail late, inside the backend's own tooling, rather
    /// than here.
    #[error("{field} must be an absolute path, got {}", .path.display())]
    NonAbsolutePath {
        /// Which path failed (for example `"RunPaths root"`).
        field: &'static str,
        /// The path that was rejected.
        path: PathBuf,
    },
    /// Rendering this cluster's static configuration failed. Carries
    /// only a rendered message, not the concrete error type, because
    /// that type is owned by the downstream crate that renders cluster
    /// configuration (for example `admissionlab_cluster`'s
    /// `ClusterConfigError`) — see the module documentation for why
    /// `admissionlab-core` cannot name it directly.
    #[error("failed to prepare cluster configuration: {0}")]
    KindConfigRender(String),
    /// Writing a file this cluster needs (its rendered configuration, an
    /// audit policy, a re-secured kubeconfig) through the run's
    /// [`crate::artifact::ArtifactStore`] failed.
    #[error("failed to write {context}: {source}")]
    ArtifactWrite {
        /// A short, human-readable label for what was being written (for
        /// example `"audit policy file"`).
        context: &'static str,
        /// The underlying artifact-store failure.
        #[source]
        source: ArtifactError,
    },
    /// A plain filesystem operation outside
    /// [`crate::artifact::ArtifactStore`]'s API (for example creating a
    /// bind-mount host directory, or reading back a file a cluster
    /// backend wrote directly) failed.
    #[error("failed to {operation} `{}`: {source}", .path.display())]
    Io {
        /// A short description of what was being attempted.
        operation: &'static str,
        /// The path the operation was acting on.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// The external command implementing this operation could not be
    /// run to completion at all: it could not be spawned, it exceeded
    /// its timeout, or some other I/O failure occurred communicating
    /// with it. See [`ProcessError`] for which.
    #[error(transparent)]
    Process(#[from] ProcessError),
    /// The external command implementing this operation ran to
    /// completion but exited with a non-zero status.
    #[error("`{context}` exited with {status}")]
    CommandFailed {
        /// A safe-to-log description of the command that failed.
        context: Box<CommandContext>,
        /// Its exit status.
        status: ExitStatus,
        /// Everything it wrote to stdout.
        stdout: Vec<u8>,
        /// Everything it wrote to stderr.
        stderr: Vec<u8>,
    },
    /// A kubeconfig a cluster backend was expected to have written is
    /// missing, empty, or not structurally valid.
    #[error("kubeconfig at {} is invalid: {reason}", .path.display())]
    InvalidKubeconfig {
        /// The kubeconfig path that failed verification.
        path: PathBuf,
        /// A human-readable explanation of what was wrong with it.
        reason: String,
    },
    /// [`ClusterManager::resolve_node_image`] could not resolve
    /// `requested` to a concrete node image (Controller Ruling R25).
    /// Carries only a rendered message, not the concrete error type, for
    /// the same reason [`ClusterError::KindConfigRender`] does: the
    /// underlying error type (for a `kind`-backed implementation,
    /// `admissionlab_cluster::VersionError`) is owned by the downstream
    /// crate that actually knows how to resolve a version, which this
    /// crate must not depend on — see the module documentation.
    #[error("cannot resolve Kubernetes version {requested:?} to a node image: {reason}")]
    UnresolvableKubernetesVersion {
        /// The Kubernetes version [`ClusterManager::resolve_node_image`]
        /// was asked to resolve (for example `"1.30.4"`).
        requested: String,
        /// A human-readable explanation from the concrete
        /// implementation's own resolution logic (for a `kind`-backed
        /// implementation, `VersionError`'s own `Display`).
        reason: String,
    },
    /// A create attempt failed after the cluster backend reported (or
    /// might have) created a node, so a best-effort deletion was
    /// attempted to avoid leaking it (PRODUCT.md §33: "no leaked cluster
    /// after normal failure paths").
    ///
    /// `source` is always the original failure that triggered the
    /// rollback attempt, preserved unchanged and available as this
    /// variant's [`std::error::Error::source`] — regardless of whether
    /// `rollback` itself succeeded. A failed cleanup can therefore never
    /// hide what actually went wrong.
    #[error("{source} ({rollback})")]
    CreateFailedWithRollback {
        /// The original create failure.
        #[source]
        source: Box<ClusterError>,
        /// What happened when cleanup was attempted.
        rollback: RollbackOutcome,
    },
}

/// The abstraction every ephemeral-cluster backend implements. See the
/// module documentation for why this trait (and its data types) lives in
/// `admissionlab-core` rather than a downstream cluster crate.
///
/// `Send + Sync` (mirroring [`crate::process::ProcessRunner`]'s own
/// bound) so that an implementation can be shared behind an `Arc` and
/// used from concurrent tasks — a later task creates baseline and
/// candidate clusters concurrently, since they are fully isolated from
/// each other.
#[async_trait]
pub trait ClusterManager: Send + Sync {
    /// Resolves `kubernetes_version` (for example `"1.30.4"` or a bare
    /// minor like `"1.30"`) to a concrete node image reference this
    /// implementation's [`ClusterManager::create`] can use directly as
    /// [`ClusterSpec::node_image`] (Controller Ruling R25).
    ///
    /// Version-to-image resolution is implementation-specific — a
    /// `kind`-backed implementation resolves against a `kindest/node`
    /// compatibility matrix; a hypothetical different backend would
    /// resolve differently, or not need this at all — which is exactly
    /// why this lives on the trait rather than in a caller that would
    /// otherwise have to know which concrete backend it was talking to
    /// (Global Constraint 6: the core stays vendor-neutral). A caller
    /// (today, `crate::run::LabRunner::prepare_clusters`) calls this
    /// once per side, before building that side's [`ClusterSpec`], so a
    /// requested version that cannot be resolved is reported clearly
    /// before any cluster is ever created — never passed through to a
    /// backend as an unvalidated, possibly-bogus image reference.
    ///
    /// # Errors
    ///
    /// Returns [`ClusterError::UnresolvableKubernetesVersion`] if
    /// `kubernetes_version` cannot be resolved — for a `kind`-backed
    /// implementation, a version outside its compatibility matrix, or
    /// one explicitly marked no longer supported.
    async fn resolve_node_image(&self, kubernetes_version: &str) -> Result<String, ClusterError>;

    /// Creates one cluster for `spec`, using `paths` to derive every
    /// file this cluster needs (its kubeconfig, audit policy, audit log
    /// directory, and rendered configuration).
    ///
    /// # Errors
    ///
    /// Returns [`ClusterError`] if `spec.name` is not a valid cluster
    /// name, if `paths` is not rooted at an absolute path, if rendering
    /// or writing this cluster's configuration fails, if the backend's
    /// create command could not be run or exited non-zero, or if the
    /// cluster it reports creating does not yield a usable kubeconfig.
    /// In every case after the backend's create command has been
    /// invoked, a failure is reported as
    /// [`ClusterError::CreateFailedWithRollback`] once a best-effort
    /// cleanup has been attempted.
    async fn create(
        &self,
        spec: &ClusterSpec,
        paths: &RunPaths,
    ) -> Result<ClusterHandle, ClusterError>;

    /// Deletes the cluster described by `handle`.
    ///
    /// # Errors
    ///
    /// Returns [`ClusterError`] if the backend's delete command could
    /// not be run or exited non-zero.
    async fn delete(&self, handle: &ClusterHandle) -> Result<(), ClusterError>;

    /// Gathers best-effort, point-in-time information about the cluster
    /// described by `handle`. Never fails; see the module documentation's
    /// "`diagnostics` never fails" section.
    async fn diagnostics(&self, handle: &ClusterHandle) -> ClusterDiagnostics;

    /// Collects the *failure* bundle for `handle` — what was actually
    /// wrong inside the cluster (ROADMAP Task 9.5), as opposed to
    /// [`Self::diagnostics`]'s "is this cluster still there".
    ///
    /// Never fails, for exactly the reasons [`Self::diagnostics`] never
    /// does: this only ever runs on a path that is already failing, and
    /// a bundle that could not be collected must never replace the
    /// failure it was describing. Anything that could not be gathered
    /// becomes a [`FailureDiagnostics::notes`] entry beside an empty
    /// list — never a guess (Global Constraint 15).
    ///
    /// **Defaulted deliberately.** Collecting this needs a Kubernetes
    /// client and a backend-specific log export, neither of which every
    /// [`ClusterManager`] implementation has (a test double has no
    /// cluster at all). The default is the honest empty answer, so an
    /// implementation opts in by having something real to say rather
    /// than being forced to stub a method out. `admissionlab-cluster`'s
    /// `KindClusterManager` is the one implementation that overrides it.
    async fn failure_diagnostics(
        &self,
        handle: &ClusterHandle,
        request: &DiagnosticsRequest,
    ) -> FailureDiagnostics {
        let _ = (handle, request);
        FailureDiagnostics::default()
    }
}
