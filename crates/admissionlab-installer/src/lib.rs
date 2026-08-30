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
//!
//! Not yet implemented here: the raw-manifest backend (Task 2.3),
//! readiness probing (Task 2.4), and stack installation orchestration
//! (Task 2.6).

use std::process::ExitStatus;
use std::time::{Duration, SystemTime};

use admissionlab_core::{ClusterHandle, CommandContext, Diagnostic, ProcessError};
use admissionlab_spec::ResolvedComponent;
use async_trait::async_trait;
use thiserror::Error;

pub mod helm;

pub use helm::HelmInstaller;

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

/// Failure modes of [`ComponentInstaller::install`].
///
/// Every variant names the failing `component`, and (other than
/// [`InstallError::UnsupportedMethod`]) carries either the underlying
/// [`ProcessError`] or the failed command's full context, exit status,
/// and captured output — so a caller always knows both what failed and
/// at which step: [`ProcessError`] and [`CommandContext`] both render
/// the full argv that was run in their own `Display` implementations.
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
}
