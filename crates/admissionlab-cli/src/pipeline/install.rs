//! The production [`StackInstaller`]: `admissionlab-core`'s
//! install abstraction, backed by `admissionlab-installer`.
//!
//! `admissionlab_core::StackInstaller` is declared in `core` and
//! implemented downstream for the dependency reason
//! `admissionlab_core::run`'s own module documentation gives at length
//! (`admissionlab-installer` depends on `admissionlab-core`, so the
//! reverse edge would be a cycle). Its documentation names exactly what
//! a concrete implementation is expected to do: delegate to
//! `admissionlab_installer::stack::install_stack`, whose
//! `cluster`/`components`/`component_timeout` parameters the trait
//! method mirrors one for one. That is all this module does.
//!
//! Two conversions happen here and nowhere else:
//!
//! - `InstalledStack`/`InstallRecord` → [`SideInstall`]/
//!   [`InstalledComponent`], which are the same shape field for field
//!   (`core` holds its own copy because it cannot name the installer's
//!   types); nothing is dropped or invented in the copy.
//! - [`InstallError`] → [`StackInstallError`], which renders the
//!   installer's richer typed error down to a component name plus a
//!   safe-to-print message — the "render to a `String`" pattern `core`
//!   documents for every crate-specific error it cannot name. The whole
//!   `source` chain is rendered, not just the outermost message, so a
//!   `helm` exit status or a Kubernetes validation message reaches the
//!   user rather than being swallowed by a wrapper.

use std::sync::Arc;
use std::time::Duration;

use admissionlab_core::{
    ClusterHandle, InstalledComponent, ProcessRunner, RunPaths, SideInstall, StackInstallError,
    StackInstaller,
};
use admissionlab_installer::stack::{CompositeInstaller, install_stack};
use admissionlab_installer::{
    HelmInstaller, InstallError, InstallRecord, KubeReadinessProbe, ManifestsInstaller,
};
use admissionlab_spec::ResolvedComponent;
use async_trait::async_trait;

/// Installs a side's stack with the real `helm`/`kubectl` backends and
/// the real Kubernetes readiness probe.
pub struct KubeStackInstaller {
    /// Dispatches each component to whichever of the two installers its
    /// resolved [`admissionlab_spec::InstallMethod`] names.
    installer: CompositeInstaller,
    /// Waits on each installed component's readiness checks.
    readiness: KubeReadinessProbe,
}

impl std::fmt::Debug for KubeStackInstaller {
    /// Hand-written: [`CompositeInstaller`] holds `dyn ComponentInstaller`
    /// values that are not `Debug`. There is nothing else to report, so
    /// this prints the type's name and says so.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KubeStackInstaller")
            .finish_non_exhaustive()
    }
}

impl KubeStackInstaller {
    /// Creates an installer driving `helm` and `kubectl` through
    /// `runner`, with both tools' client-side state (Helm's repository
    /// config and cache, `kubectl`'s discovery cache) directed under
    /// `paths`'s own `logs` directory rather than the operator's
    /// `~/.helm`/`~/.kube` — that isolation is
    /// `HelmInstaller::new`/`ManifestsInstaller::new`'s own contract, and
    /// taking `paths` here is what lets them honor it (PRODUCT.md §5:
    /// the default flow copies no production configuration).
    #[must_use]
    pub fn new(runner: Arc<dyn ProcessRunner>, paths: &RunPaths) -> Self {
        Self {
            installer: CompositeInstaller::new(
                Arc::new(HelmInstaller::new(Arc::clone(&runner), paths)),
                Arc::new(ManifestsInstaller::new(runner, paths)),
            ),
            readiness: KubeReadinessProbe::new(),
        }
    }
}

#[async_trait]
impl StackInstaller for KubeStackInstaller {
    async fn install_stack(
        &self,
        cluster: &ClusterHandle,
        components: &[ResolvedComponent],
        component_timeout: Duration,
    ) -> Result<SideInstall, StackInstallError> {
        match install_stack(
            cluster,
            components,
            &self.installer,
            &self.readiness,
            component_timeout,
        )
        .await
        {
            Ok(installed) => Ok(SideInstall {
                side: installed.side,
                components: installed
                    .components
                    .iter()
                    .map(installed_component)
                    .collect(),
            }),
            Err(error) => Err(stack_install_error(&error)),
        }
    }
}

/// Copies one [`InstallRecord`] into `core`'s own identically-shaped
/// [`InstalledComponent`].
fn installed_component(record: &InstallRecord) -> InstalledComponent {
    InstalledComponent {
        name: record.component.clone(),
        method: record.method.clone(),
        resolved_version: record.resolved_version.clone(),
        started_at: record.started_at,
        elapsed: record.elapsed,
        diagnostics: record.diagnostics.clone(),
    }
}

/// Renders an [`InstallError`] down to the core-visible
/// [`StackInstallError`], naming the failing component where the error
/// itself knows one.
///
/// The two variants that carry a `path` rather than a `component`
/// (`ManifestRead`, `ManifestParse`) genuinely have no component to
/// name: they are raised by `load_manifest_bundle`, which can be called
/// with no `ResolvedComponent` in scope at all. Reporting `None` there
/// is honest — `StackInstallError`'s `Display` says "stack installation
/// failed: ..." in that case — and the message still names the file.
fn stack_install_error(error: &InstallError) -> StackInstallError {
    let component = match error {
        InstallError::UnsupportedMethod { component, .. }
        | InstallError::Process { component, .. }
        | InstallError::CommandFailed { component, .. }
        | InstallError::ManifestExceedsAnnotationLimit { component, .. }
        | InstallError::ComponentReadinessUnavailable { component, .. }
        | InstallError::ComponentNotReady { component, .. } => Some(component.clone()),
        InstallError::ManifestRead { .. }
        | InstallError::ManifestParse { .. }
        | InstallError::ReadinessUnavailable { .. } => None,
    };
    StackInstallError {
        component,
        message: render_chain(error),
    }
}

/// Renders `error` plus its whole `source` chain into one line, skipping
/// a cause whose text the outer message already contains (several
/// `InstallError` variants interpolate their source into their own
/// `#[error]` string). Mirrors `admissionlab_admission::capture`'s own
/// `capture_error`, which solves the identical problem at the identical
/// seam.
fn render_chain(error: &InstallError) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        let rendered = cause.to_string();
        if !message.contains(&rendered) {
            message.push_str(": ");
            message.push_str(&rendered);
        }
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn a_component_scoped_failure_names_its_component() {
        let error = InstallError::UnsupportedMethod {
            component: "kyverno".to_owned(),
            actual: "manifests",
            expected: "helm",
        };
        let rendered = stack_install_error(&error);
        assert_eq!(rendered.component.as_deref(), Some("kyverno"));
        assert!(
            rendered.message.contains("kyverno"),
            "unexpected message: {}",
            rendered.message
        );
    }

    #[test]
    fn a_manifest_read_failure_reports_no_component_but_still_names_the_file() {
        let error = InstallError::ManifestRead {
            path: PathBuf::from("/lab/manifests/webhook.yaml"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        };
        let rendered = stack_install_error(&error);
        assert_eq!(
            rendered.component, None,
            "a bundle load has no component in scope to blame"
        );
        assert!(
            rendered.message.contains("webhook.yaml"),
            "unexpected message: {}",
            rendered.message
        );
        assert!(
            rendered.message.contains("no such file"),
            "the source chain must survive: {}",
            rendered.message
        );
    }
}
