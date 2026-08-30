#![forbid(unsafe_code)]
//! Installer behavior for the components an [`admissionlab_spec::LabSpec`]
//! resolves into.
//!
//! This crate provides *behavior* only. The vocabulary that behavior
//! operates on — [`admissionlab_spec::InstallMethod`],
//! [`admissionlab_spec::ReadinessCheck`], and the fully resolved
//! [`admissionlab_spec::ResolvedComponent`] — is defined in
//! `admissionlab-spec`, not here (Controller Ruling R30): `spec` must
//! stay a leaf crate, and this crate needs [`admissionlab_core::Diagnostic`]
//! and [`admissionlab_core::ClusterHandle`] for the behavior it *does*
//! own. Both come from `admissionlab-core`, not `admissionlab-cluster`:
//! see `admissionlab-core`'s own `cluster` module documentation for why
//! `ClusterHandle` and the `ClusterManager` trait live there rather than
//! in a downstream cluster-lifecycle crate. This crate has no dependency
//! on `admissionlab-cluster` at all — pulling in the whole
//! cluster-lifecycle crate just to name the one type an installer needs
//! from a running cluster would be an unnecessary edge, and defining the
//! resolved install vocabulary here instead of in `admissionlab-spec`
//! would close `spec -> installer -> core -> spec` into a cycle the
//! moment `spec::resolve_lab` needed to produce one.
//!
//! [`ComponentInstaller`] is the one contract every install backend
//! implements; [`InstallRecord`] is what a successful install reports;
//! [`InstallError`] is what an unsuccessful one reports.
//!
//! - [`helm`] implements [`ComponentInstaller`] for a Helm chart install
//!   (Task 2.2; [`admissionlab_spec::component::HelmInstallSpec`]).
//! - [`manifests`] implements [`ComponentInstaller`] for a raw
//!   Kubernetes manifest install (Task 2.3;
//!   [`admissionlab_spec::ManifestInstallSpec`]), and separately exposes
//!   [`manifests::load_manifest_bundle`] for parsing and hashing a
//!   component's manifest files with no cluster interaction at all.
//! - [`readiness`] implements [`readiness::ReadinessProbe`] (Task 2.4):
//!   deciding when an installed component is actually ready, given an
//!   [`admissionlab_spec::ReadinessCheck`], by polling the cluster's own
//!   Kubernetes API with a capped exponential backoff and an absolute
//!   deadline.
//!
//! Not yet implemented here: stack installation orchestration (Task
//! 2.6), which drives [`ComponentInstaller::install`] and
//! [`readiness::ReadinessProbe::wait`] together over a component's own
//! readiness checks.

use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::{Duration, SystemTime};

use admissionlab_core::{ClusterHandle, CommandContext, Diagnostic, ProcessError};
use admissionlab_spec::{ReadinessCheck, ResolvedComponent};
use async_trait::async_trait;
use thiserror::Error;

pub mod helm;
pub mod manifests;
pub mod readiness;

pub use helm::HelmInstaller;
pub use manifests::{ManifestBundle, ManifestsInstaller, load_manifest_bundle};
pub use readiness::{KubeReadinessProbe, ReadinessEvidence, ReadinessProbe};

/// The contract every component install backend implements: given a
/// running cluster and a fully resolved component, install it and
/// report what happened.
///
/// `Send + Sync` so an implementation can be shared behind an `Arc` and
/// driven concurrently — the same bound
/// [`admissionlab_core::ClusterManager`] and
/// [`admissionlab_core::ProcessRunner`] already carry, for the same
/// reason: a later task installs components across two independently
/// isolated clusters (baseline and candidate) at once.
#[async_trait]
pub trait ComponentInstaller: Send + Sync {
    /// Installs `component` onto `cluster`.
    ///
    /// # Errors
    ///
    /// Returns [`InstallError`] if `component`'s resolved install
    /// method is not one this implementation supports, if an external
    /// command implementing the install could not be run to completion
    /// (it could not be spawned, it exceeded its timeout, or some other
    /// I/O failure occurred), or if it ran to completion but exited
    /// non-zero.
    async fn install(
        &self,
        cluster: &ClusterHandle,
        component: &ResolvedComponent,
    ) -> Result<InstallRecord, InstallError>;
}

/// What a successful [`ComponentInstaller::install`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRecord {
    /// The installed component's name, copied from
    /// [`ResolvedComponent::name`].
    pub component: String,
    /// A short, stable label for which install method actually ran (for
    /// example `"helm"`).
    pub method: String,
    /// The version actually installed. Confirmed against the cluster
    /// when possible, rather than merely echoing back what was
    /// requested — see [`helm::HelmInstaller`]'s module documentation
    /// for exactly how this is captured for a Helm install, and what it
    /// holds when it cannot be confirmed.
    pub resolved_version: String,
    /// Wall-clock time this install began.
    pub started_at: SystemTime,
    /// Wall-clock time this install took, from
    /// [`ComponentInstaller::install`] being called to it returning.
    pub elapsed: Duration,
    /// Non-fatal findings surfaced while installing — for example, an
    /// installed component whose version could not be confirmed. Empty
    /// when there is nothing to report.
    pub diagnostics: Vec<Diagnostic>,
}

/// Failure modes of [`ComponentInstaller::install`], and also of
/// [`manifests::load_manifest_bundle`], which is a plain function rather
/// than a [`ComponentInstaller`] method and so cannot itself name a
/// failing `component` (there is no [`ResolvedComponent`] in scope at
/// the point a bundle is loaded independently of an install).
///
/// Every variant produced *by an install* names the failing `component`,
/// and (other than [`InstallError::UnsupportedMethod`]) carries either
/// the underlying [`ProcessError`] or the failed command's full context,
/// exit status, and captured output — so a caller always knows both what
/// failed and at which step: [`ProcessError`] and [`CommandContext`]
/// both render the full argv that was run in their own `Display`
/// implementations. [`InstallError::ManifestRead`] and
/// [`InstallError::ManifestParse`] are the exception: both can be
/// produced directly by [`manifests::load_manifest_bundle`] itself
/// (with no component in scope to name), as well as by
/// [`manifests::ManifestsInstaller::install`] calling it internally, so
/// neither carries a `component` field.
#[derive(Debug, Error)]
pub enum InstallError {
    /// `component`'s resolved install method is not one this
    /// [`ComponentInstaller`] implementation supports — for example, an
    /// [`admissionlab_spec::InstallMethod::Manifests`] component handed
    /// to [`helm::HelmInstaller`].
    #[error(
        "component {component:?} has a {actual} install method, but this installer only \
         supports {expected}"
    )]
    UnsupportedMethod {
        /// The component that could not be installed.
        component: String,
        /// The install method this implementation supports.
        expected: &'static str,
        /// The install method `component` actually resolved to.
        actual: &'static str,
    },
    /// The external command implementing one step of `component`'s
    /// install could not be run to completion at all: it could not be
    /// spawned, it exceeded its timeout, or some other I/O failure
    /// occurred communicating with it. See [`ProcessError`] for which,
    /// and its own `Display` for which command this was.
    #[error("installing component {component:?} failed: {source}")]
    Process {
        /// The component that could not be installed.
        component: String,
        /// The underlying process failure.
        #[source]
        source: ProcessError,
    },
    /// The external command implementing one step of `component`'s
    /// install ran to completion but exited non-zero.
    #[error("installing component {component:?} failed: `{context}` exited with {status}")]
    CommandFailed {
        /// The component that could not be installed.
        component: String,
        /// A safe-to-log description of the command that failed — its
        /// `Display` names the exact step (for example `helm repo add`
        /// versus `helm upgrade --install`) via the argv it ran.
        context: Box<CommandContext>,
        /// Its exit status.
        status: ExitStatus,
        /// Everything it wrote to stdout.
        stdout: Vec<u8>,
        /// Everything it wrote to stderr.
        stderr: Vec<u8>,
    },
    /// A manifest file named in a component's
    /// [`admissionlab_spec::ManifestInstallSpec::paths`] could not be
    /// read from local disk at all — for example it does not exist, is
    /// not readable, or (Task 2.3 does not walk directories; see
    /// [`manifests`]'s module documentation) names a directory rather
    /// than a file. Always returned before any cluster operation runs
    /// (Task 2.3 brief Step 2).
    #[error("failed to read manifest file {}: {source}", .path.display())]
    ManifestRead {
        /// The manifest file path that could not be read.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// One document inside a manifest file named in a component's
    /// [`admissionlab_spec::ManifestInstallSpec::paths`] is not
    /// syntactically valid YAML or JSON. Always returned before any
    /// cluster operation runs (Task 2.3 brief Step 2): a malformed
    /// manifest fails locally, never partway through a `kubectl apply`
    /// sequence.
    #[error(
        "manifest file {} is invalid: document {document_number} is not valid {format}: {reason}",
        .path.display()
    )]
    ManifestParse {
        /// The manifest file containing the malformed document.
        path: PathBuf,
        /// Which document within `path`, counting from 1 in file order.
        /// A JSON file (which has no multi-document syntax) is always
        /// document 1; a YAML file's first `---`-separated document is
        /// document 1, its second is document 2, and so on.
        document_number: usize,
        /// Which format `path` was parsed as (`"YAML"` or `"JSON"`),
        /// chosen from its extension — see [`manifests`]'s module
        /// documentation.
        format: &'static str,
        /// A human-readable explanation from the underlying parser.
        reason: String,
    },
    /// `component`'s manifest install failed because a `kubectl apply
    /// --server-side=false` invocation for one manifest file would
    /// exceed Kubernetes's hard-coded 262144-byte `metadata.annotations`
    /// size limit. Client-side apply stores the whole applied object in
    /// the `kubectl.kubernetes.io/last-applied-configuration`
    /// annotation, so this is most commonly hit by a large
    /// `CustomResourceDefinition`. See [`manifests`]'s module
    /// documentation for how this is detected and why it is reported as
    /// its own variant rather than a plain [`InstallError::CommandFailed`].
    ///
    /// This never causes an automatic retry with `--server-side=true`:
    /// silently switching which apply mode actually ran would make this
    /// installer's behavior unpredictable in a way a user could not
    /// detect from its output, so the remedy (install this component a
    /// different way, or reduce the manifest's size) is left to the
    /// user, with the original, unmodified `stderr` still attached below
    /// rather than hidden behind this variant's own plain-language
    /// explanation.
    #[error(
        "installing component {component:?} failed: applying {} via client-side `kubectl apply \
         --server-side=false` would exceed Kubernetes's 262144-byte metadata.annotations size \
         limit (most commonly hit by a large CustomResourceDefinition) -- install this \
         component via Helm instead, or reduce the manifest's size; Admission Lab will not \
         silently retry with `--server-side=true`",
        .path.display()
    )]
    ManifestExceedsAnnotationLimit {
        /// The component that could not be installed.
        component: String,
        /// The manifest file whose `kubectl apply` invocation hit the
        /// limit.
        path: PathBuf,
        /// A safe-to-log description of the failed `kubectl apply`
        /// invocation.
        context: Box<CommandContext>,
        /// Its exit status.
        status: ExitStatus,
        /// Everything it wrote to stdout.
        stdout: Vec<u8>,
        /// Everything it wrote to stderr, including Kubernetes's own raw
        /// "Too long" validation message — preserved here rather than
        /// discarded even though this variant's own message already
        /// explains the cause in plain language.
        stderr: Vec<u8>,
    },
    /// [`readiness::ReadinessProbe::wait`] could not even attempt
    /// `check` — not "not yet ready" (that outcome is data, reported as
    /// `Ok(ReadinessEvidence { satisfied: false, .. })`, never as an
    /// error; see [`readiness::poll_readiness`]'s documentation) but
    /// unable to observe the cluster at all. Both of this variant's
    /// causes are permanent, discovered once, before any poll attempt:
    /// the Kubernetes client could not be built from the cluster's own
    /// kubeconfig ([`readiness`]'s `client_for`), or (for
    /// [`admissionlab_spec::ReadinessCheck::CustomResourceCondition`])
    /// its `api_version` could not be parsed into a group/version.
    /// Retrying either would not help, which is why they short-circuit
    /// here rather than consuming the deadline retrying something that
    /// can never succeed.
    #[error("cannot wait for {check:?} to become ready: {reason}")]
    ReadinessUnavailable {
        /// The readiness check that could not even be attempted. Boxed
        /// (mirroring `CommandFailed::context`, `ManifestExceedsAnnotationLimit::context`
        /// above): [`ReadinessCheck::CustomResourceCondition`] carries
        /// six `String`/`Option<String>` fields, large enough that
        /// clippy's `result_large_err` flags every `Result<_,
        /// InstallError>`-returning function in this crate (not only
        /// this variant's own constructors) once it is inlined directly.
        check: Box<ReadinessCheck>,
        /// A human-readable explanation of what went wrong, taken from
        /// the underlying `kube`/kubeconfig-parsing failure's own
        /// `Display`.
        reason: String,
    },
}
