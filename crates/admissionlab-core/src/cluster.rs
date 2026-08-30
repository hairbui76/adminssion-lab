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
}
