//! Top-level orchestration of one lab run's two ephemeral clusters:
//! bringing baseline and candidate up together, and guaranteeing they
//! come down again — on success, on a create failure on either side, and
//! on any later failure a caller reports by calling [`LabRunner::cleanup`]
//! (PRODUCT.md §33 "no leaked cluster after normal failure paths";
//! §10.4 "clusters are deleted on success and failure by default").
//!
//! [`LabRunner`] is generic over [`ClusterManager`] rather than naming
//! `admissionlab_cluster::KindClusterManager` directly: Controller Ruling
//! R22 (see [`crate::cluster`]'s module documentation) requires
//! `admissionlab-core` to never depend on `admissionlab-cluster`, and
//! staying generic over the trait is what makes that possible while
//! still living in this crate. A caller (today, only `admissionlab-cli`,
//! which depends on both crates) supplies the concrete
//! `KindClusterManager`.
//!
//! # Concurrency: `tokio::join!`, never `try_join!`
//!
//! [`LabRunner::prepare_clusters`] creates both clusters with
//! `tokio::join!`, and [`LabRunner::cleanup`] deletes both the same way.
//! `try_join!` would abandon the still-in-flight side's future the
//! instant the other side's future resolves to an error — dropping it
//! before it ever produces a [`ClusterHandle`] — which would leak
//! exactly the cluster this module exists to never leak: baseline and
//! candidate are isolated (PRODUCT.md §10.2), so there is nothing to
//! gain and a real cluster to lose by racing to bail out early.
//! `tests/run_lifecycle.rs`'s
//! `candidate_creation_is_not_abandoned_when_baseline_fails_immediately`
//! is the regression test for this exact property.
//!
//! # Rollback responsibility is split cleanly in two
//!
//! A single [`ClusterManager::create`] call is already contracted to
//! clean up after *itself*: from the moment its backend's create command
//! is invoked onward, any failure triggers that implementation's own
//! best-effort delete before the error is returned (see
//! [`ClusterError::CreateFailedWithRollback`]). So when `create` returns
//! `Err`, [`LabRunner::prepare_clusters`] trusts that the failing side
//! has already handled its own potential leak and does not call `delete`
//! for it again.
//!
//! What no single `create` call can do is clean up the *other*, healthy
//! side once the overall `prepare_clusters` call decides to fail. That
//! is this module's own responsibility: when exactly one side's `create`
//! succeeds while the other fails, `prepare_clusters` deletes the
//! side that came up before returning an error, and reports whatever
//! that deletion attempt found via [`RunError::ClusterCreationFailed`]'s
//! `rollback` diagnostics — never silently, and never in a way that
//! could hide the original creation failure (mirroring
//! [`ClusterError::CreateFailedWithRollback`]'s own guarantee at this
//! higher level).
//!
//! # Node image resolution (Controller Ruling R25)
//!
//! [`ClusterSpec::node_image`] must be a concrete, resolved image
//! reference before [`ClusterManager::create`] can use it — but
//! resolving a requested Kubernetes version into one is genuinely
//! implementation-specific (a `kind`-backed implementation resolves
//! against a `kindest/node` compatibility matrix; a different backend
//! would resolve differently), so `admissionlab-core` must not embed
//! that knowledge itself without violating Global Constraint 6 (the
//! core stays vendor-neutral) the same way depending on
//! `admissionlab-cluster` directly would violate Controller Ruling R22.
//! [`ClusterManager::resolve_node_image`] is the resolution: an
//! additional trait method, alongside `create`/`delete`/`diagnostics`,
//! that every [`ClusterManager`] implementation supplies. Adding it does
//! not reopen the R22 cycle — the trait was already the injected
//! abstraction `admissionlab-cli` wires a concrete backend through, and
//! `ClusterSpec`'s `{side, name, kubernetes_version, node_image}` shape
//! is unchanged.
//!
//! [`LabRunner::prepare_clusters`] therefore resolves each side's
//! requested version through `resolve_node_image` *before* building
//! that side's [`ClusterSpec`], sequentially rather than concurrently:
//! unlike `create`, resolving allocates or provisions nothing that could
//! be leaked by bailing out on the first failure, so there is no
//! `try_join!`-style abandonment risk to guard against, and failing fast
//! on baseline's own bad version is strictly simpler than describing a
//! `Baseline`/`Candidate`/`Both` outcome space for a step that can never
//! need a rollback. A version that cannot be resolved is reported as
//! [`RunError::NodeImageResolutionFailed`] before any cluster is ever
//! created — never silently passed through to a backend as an
//! unvalidated, possibly-bogus image reference.
//!
//! # Stack installation (Task 2.6): [`StackInstaller`]
//!
//! [`LabRunner::install_stacks`] is this module's next lifecycle stage
//! after [`LabRunner::prepare_clusters`]: installing each side's whole
//! ordered component stack onto its already-created cluster. The actual
//! install behavior — driving `admissionlab-installer`'s
//! `ComponentInstaller`/`ReadinessProbe` — cannot be called from here
//! directly: `admissionlab-installer` depends on `admissionlab-core`
//! (for [`ClusterHandle`], [`Diagnostic`], and more), exactly the same
//! shape of edge `admissionlab-cluster` has to this crate, and
//! [`crate::cluster`]'s own module documentation already explains why
//! that edge forces [`ClusterManager`] to live here rather than
//! downstream (Controller Ruling R22): this crate hosts [`LabRunner`],
//! so it must be able to name whatever trait drives `LabRunner`'s own
//! lifecycle, and a trait living in a crate that itself depends on
//! `admissionlab-core` would close `admissionlab-core ->
//! admissionlab-installer -> admissionlab-core` into a cycle Cargo
//! rejects outright.
//!
//! [`StackInstaller`] is the identical shape of abstraction, for the
//! identical reason: defined here so [`LabRunner`] can drive it,
//! implemented by a concrete type in a downstream crate that already
//! depends on both `admissionlab-core` and `admissionlab-installer` —
//! delegating to `admissionlab_installer::stack::install_stack` —
//! without ever requiring this crate to depend on that one, or to name
//! `ComponentInstaller`/`ReadinessProbe`/`InstallRecord`/`InstallError`
//! directly. [`InstalledComponent`]/[`SideInstall`] mirror
//! `admissionlab_installer::{InstallRecord, stack::InstalledStack}`'s
//! own shape field-for-field instead of reusing those types (which this
//! crate cannot name) or collapsing to something lossy like `()` — every
//! field here is already a plain, core-owned type
//! (`String`/`SystemTime`/`Duration`/[`Diagnostic`]), so a concrete
//! [`StackInstaller`] copying its own richer `InstallRecord`s into these
//! loses nothing. [`StackInstallError`] instead renders its
//! implementation's own richer `InstallError` down to a `component` name
//! plus a `message` string — the same "render to a `String`" pattern
//! [`ClusterError::KindConfigRender`] already establishes for
//! `admissionlab-cluster`'s own crate-specific errors that this crate
//! cannot name either.
//!
//! No caller in this workspace constructs a real [`StackInstaller`] yet
//! (that is CLI wiring, deliberately out of scope for Task 2.6 — see
//! `admissionlab-cli`'s `commands::test` module documentation); this
//! trait and [`LabRunner::install_stacks`] exist so a later caller has
//! one, tested, DRY entry point for "both sides, concurrently, in
//! deterministic per-side order" rather than needing to hand-write its
//! own `tokio::join!` over `admissionlab_installer::stack::install_stack`
//! every time.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use admissionlab_spec::{ResolvedComponent, ResolvedLab};
use async_trait::async_trait;
use thiserror::Error;

use crate::artifact::{ArtifactError, ArtifactStore, RunPaths};
use crate::cluster::{ClusterError, ClusterHandle, ClusterManager, ClusterSpec};
use crate::diagnostic::{Diagnostic, RedactedValue};
use crate::ids::RunId;
use crate::side::Side;

/// Number of leading [`RunId`] characters used as a cluster name's run
/// suffix. Mirrors `admissionlab_cluster::kind`'s own `SHORT_RUN_ID_LEN`
/// (12 — see that module's "Why 12 characters" documentation for the
/// full rationale: enough to make a collision implausible, short enough
/// to fit `kind`'s derived `-control-plane` container-name length
/// budget). Duplicated here, rather than shared, because
/// `admissionlab-core` must not depend on `admissionlab-cluster`
/// (Controller Ruling R22) — kept in sync by hand, the same way this
/// crate's own [`crate::cluster`] module already accepts for other
/// cross-crate literals it cannot import (see that module's "Why
/// `admissionlab-cluster`-specific errors are not named here" section).
const SHORT_RUN_ID_LEN: usize = 12;

/// Caller-controlled behavior for one [`LabRunner::prepare_clusters`]
/// call.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// When `true`, a successful [`LabRunner::prepare_clusters`] leaves
    /// both clusters running instead of being deleted: the caller is
    /// expected to call [`preserved_cluster_report`] and print it rather
    /// than calling [`LabRunner::cleanup`] (PRODUCT.md §10.4).
    pub keep_clusters: bool,
    /// The root directory this run's on-disk workspace is created
    /// under. Must be absolute — [`LabRunner::prepare_clusters`] rejects
    /// a relative one immediately, before creating anything, since a
    /// `kind`-backed [`ClusterManager`] needs an absolute host path for
    /// its Docker bind mounts (see [`ClusterError::NonAbsolutePath`]).
    ///
    /// This must name the same directory the caller constructed
    /// [`LabRunner::artifact_store`] from:
    /// [`LabRunner::prepare_clusters`] creates this run's workspace
    /// through `artifact_store` (which has no accessor for its own
    /// root), and only reads `run_root` itself for the up-front
    /// absoluteness check.
    pub run_root: PathBuf,
}

/// Both clusters of one run, ready for whatever comes next.
///
/// Kept to exactly what two later concerns need: [`LabRunner::cleanup`]
/// needs `baseline`/`candidate` to delete them after a partial failure
/// occurring downstream of `prepare_clusters` (fixture execution,
/// installation — neither exists yet); a later task replaying fixtures
/// needs those same handles (kubeconfig, audit log) plus `paths` to know
/// where to write captured evidence. No Phase 2/3 fields are anticipated
/// here.
#[derive(Debug, Clone)]
pub struct PreparedLab {
    /// This run's identifier, shared by both clusters' names and by
    /// `paths`.
    pub run_id: RunId,
    /// This run's on-disk artifact layout, as created by
    /// [`ArtifactStore::create_run`].
    pub paths: RunPaths,
    /// The created baseline cluster.
    pub baseline: ClusterHandle,
    /// The created candidate cluster.
    pub candidate: ClusterHandle,
}

/// Which side(s) failed to create a cluster, and why. See
/// [`RunError::ClusterCreationFailed`].
///
/// Each [`ClusterError`] is boxed for the same reason
/// [`ClusterError::CreateFailedWithRollback`] itself boxes its own
/// `source`: `ClusterError` is a large enum (it carries, among other
/// things, captured process output), so holding one or two of them
/// inline here would make `RunError` itself large enough for
/// `clippy::result_large_err` to flag every `Result<_, RunError>`
/// return.
#[derive(Debug)]
pub enum ClusterCreationFailure {
    /// Only the baseline side failed; candidate came up (and, by the
    /// time this is constructed, has already been deleted or a failed
    /// deletion attempt has been recorded in the enclosing
    /// [`RunError::ClusterCreationFailed`]'s `rollback`).
    Baseline(Box<ClusterError>),
    /// Only the candidate side failed; baseline came up and was handled
    /// the same way.
    Candidate(Box<ClusterError>),
    /// Both sides failed. Neither ever came up, so there is nothing for
    /// `prepare_clusters` itself to roll back — each failure already
    /// went through its own implementation's best-effort self-cleanup
    /// (see this module's documentation).
    Both {
        /// The baseline side's creation failure.
        baseline: Box<ClusterError>,
        /// The candidate side's creation failure.
        candidate: Box<ClusterError>,
    },
}

impl fmt::Display for ClusterCreationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Baseline(error) => write!(f, "baseline cluster failed to create: {error}"),
            Self::Candidate(error) => write!(f, "candidate cluster failed to create: {error}"),
            Self::Both {
                baseline,
                candidate,
            } => write!(
                f,
                "both clusters failed to create: baseline: {baseline}; candidate: {candidate}"
            ),
        }
    }
}

/// Failure modes of [`LabRunner::prepare_clusters`].
#[derive(Debug, Error)]
pub enum RunError {
    /// [`RunOptions::run_root`] was not an absolute path.
    #[error("run root {} must be an absolute path", .0.display())]
    NonAbsoluteRunRoot(PathBuf),
    /// Creating this run's on-disk workspace failed.
    #[error("failed to prepare run workspace: {0}")]
    Workspace(#[from] ArtifactError),
    /// `side`'s configured Kubernetes version could not be resolved to a
    /// node image (Controller Ruling R25) — reported before any cluster
    /// is created. See this module's documentation ("Node image
    /// resolution") for why only one side is ever named here.
    #[error("failed to resolve a node image for the {side} cluster: {source}")]
    NodeImageResolutionFailed {
        /// Which side's configured version could not be resolved.
        side: Side,
        /// The underlying resolution failure.
        #[source]
        source: Box<ClusterError>,
    },
    /// One or both clusters failed to create. `rollback` reports what
    /// happened when `prepare_clusters` attempted to delete whichever
    /// side(s) actually came up before returning this error — empty
    /// when nothing needed cleaning up (see this module's documentation
    /// on the split rollback responsibility).
    #[error("{failure}")]
    ClusterCreationFailed {
        /// Which side(s) failed, and why.
        failure: ClusterCreationFailure,
        /// Diagnostics from cleaning up whichever side(s) came up
        /// despite the overall failure. Never hides `failure`: this is
        /// always present as `ClusterCreationFailure`'s own field,
        /// regardless of whether this rollback attempt itself
        /// succeeded, failed, or was not needed at all.
        rollback: Vec<Diagnostic>,
    },
}

/// Assembles a [`ClusterSpec`] for `side` in `run_id`'s run, requesting
/// `kubernetes_version` and using the already-resolved `node_image` (see
/// [`ClusterManager::resolve_node_image`]).
fn build_cluster_spec(
    side: Side,
    run_id: &RunId,
    kubernetes_version: &str,
    node_image: String,
) -> ClusterSpec {
    let short_run_id: String = run_id.as_str().chars().take(SHORT_RUN_ID_LEN).collect();
    ClusterSpec {
        side,
        name: format!("adlab-{}-{short_run_id}", side.as_str()),
        kubernetes_version: kubernetes_version.to_owned(),
        node_image,
    }
}

/// Builds a [`Diagnostic`] reporting that deleting `handle`'s cluster
/// failed, including the exact `kind delete cluster --name <name>`
/// command a user can run by hand — mirroring the wording already
/// established for this exact situation by `admissionlab-cluster`'s
/// `KindClusterManager` rollback guard and `admissionlab-cli`'s
/// `doctor --deep` probe guard.
fn delete_failure_diagnostic(
    side: Side,
    handle: &ClusterHandle,
    error: &ClusterError,
) -> Diagnostic {
    let name = &handle.spec.name;
    let mut context = BTreeMap::new();
    context.insert("side".to_owned(), RedactedValue::Public(side.to_string()));
    context.insert(
        "cluster_name".to_owned(),
        RedactedValue::Public(name.clone()),
    );
    Diagnostic {
        code: "cluster.delete_failed".to_owned(),
        message: format!(
            "failed to delete {side} cluster {name:?}: {error}; if it still exists, delete it \
             manually with: kind delete cluster --name {name}"
        ),
        context,
    }
}

/// Renders the human-readable report a `--keep-clusters` run should
/// print instead of calling [`LabRunner::cleanup`]: both cluster names,
/// their kubeconfig paths, and the exact
/// `kind delete cluster --name <name>` command that removes each —
/// copy-pasteable without reconstructing anything (PRODUCT.md §10.4;
/// Task 1.10 brief Step 3).
#[must_use]
pub fn preserved_cluster_report(prepared: &PreparedLab) -> String {
    use std::fmt::Write as _;

    let mut report =
        String::from("Clusters preserved (--keep-clusters was set); nothing was deleted.\n");
    for handle in [&prepared.baseline, &prepared.candidate] {
        let name = &handle.spec.name;
        // `write!` into the existing `String`, not
        // `push_str(&format!(...))`, which would allocate a second,
        // immediately-discarded `String` for every cluster.
        let _: fmt::Result = write!(
            report,
            "  {side} cluster {name:?}\n    kubeconfig: {kubeconfig}\n    delete with: kind delete cluster --name {name}\n",
            side = handle.spec.side,
            kubeconfig = handle.kubeconfig.display(),
        );
    }
    report
}

/// Orchestrates one lab run's two ephemeral clusters against a concrete
/// [`ClusterManager`] implementation `C`.
pub struct LabRunner<C: ClusterManager> {
    /// The concrete cluster backend both clusters are created/deleted
    /// through. `Arc`-wrapped so it can be shared across the concurrent
    /// baseline/candidate calls this module makes.
    pub cluster_manager: Arc<C>,
    /// This run's artifact store. [`LabRunner::prepare_clusters`] calls
    /// [`ArtifactStore::create_run`] on it to obtain this run's
    /// [`RunPaths`]; it must be rooted at the same directory as whatever
    /// [`RunOptions::run_root`] a given call is made with.
    pub artifact_store: ArtifactStore,
}

impl<C: ClusterManager> LabRunner<C> {
    /// Creates both the baseline and candidate clusters for `lab`,
    /// concurrently.
    ///
    /// # Errors
    ///
    /// Returns [`RunError::NonAbsoluteRunRoot`] if `options.run_root` is
    /// not absolute, before anything is created. Returns
    /// [`RunError::Workspace`] if this run's on-disk workspace could not
    /// be created. Returns [`RunError::NodeImageResolutionFailed`] if
    /// either side's configured Kubernetes version cannot be resolved to
    /// a node image — before any cluster is created. Returns
    /// [`RunError::ClusterCreationFailed`] if either cluster failed to
    /// create — see this module's documentation for exactly how the
    /// other, successfully created side is handled in that case.
    pub async fn prepare_clusters(
        &self,
        lab: &ResolvedLab,
        options: &RunOptions,
    ) -> Result<PreparedLab, RunError> {
        if !options.run_root.is_absolute() {
            return Err(RunError::NonAbsoluteRunRoot(options.run_root.clone()));
        }

        let run_id = RunId::generate();
        let paths = self.artifact_store.create_run(&run_id).await?;

        // Resolved sequentially (not `tokio::join!`): resolving
        // allocates or provisions nothing, so failing fast on baseline's
        // own bad version loses nothing and needs no rollback — see this
        // module's documentation ("Node image resolution").
        let baseline_image = self
            .cluster_manager
            .resolve_node_image(&lab.baseline.kubernetes)
            .await
            .map_err(|source| RunError::NodeImageResolutionFailed {
                side: Side::Baseline,
                source: Box::new(source),
            })?;
        let candidate_image = self
            .cluster_manager
            .resolve_node_image(&lab.candidate.kubernetes)
            .await
            .map_err(|source| RunError::NodeImageResolutionFailed {
                side: Side::Candidate,
                source: Box::new(source),
            })?;

        let baseline_spec = build_cluster_spec(
            Side::Baseline,
            &run_id,
            &lab.baseline.kubernetes,
            baseline_image,
        );
        let candidate_spec = build_cluster_spec(
            Side::Candidate,
            &run_id,
            &lab.candidate.kubernetes,
            candidate_image,
        );

        // `tokio::join!`, never `try_join!` — see this module's
        // documentation for why: baseline and candidate are isolated, so
        // one side failing must never cut short the other's still
        // in-flight creation.
        let (baseline_result, candidate_result) = tokio::join!(
            self.cluster_manager.create(&baseline_spec, &paths),
            self.cluster_manager.create(&candidate_spec, &paths),
        );

        match (baseline_result, candidate_result) {
            (Ok(baseline), Ok(candidate)) => Ok(PreparedLab {
                run_id,
                paths,
                baseline,
                candidate,
            }),
            (Err(baseline_error), Ok(candidate)) => {
                let rollback = self.delete_orphan(Side::Candidate, &candidate).await;
                Err(RunError::ClusterCreationFailed {
                    failure: ClusterCreationFailure::Baseline(Box::new(baseline_error)),
                    rollback,
                })
            }
            (Ok(baseline), Err(candidate_error)) => {
                let rollback = self.delete_orphan(Side::Baseline, &baseline).await;
                Err(RunError::ClusterCreationFailed {
                    failure: ClusterCreationFailure::Candidate(Box::new(candidate_error)),
                    rollback,
                })
            }
            (Err(baseline_error), Err(candidate_error)) => Err(RunError::ClusterCreationFailed {
                failure: ClusterCreationFailure::Both {
                    baseline: Box::new(baseline_error),
                    candidate: Box::new(candidate_error),
                },
                rollback: Vec::new(),
            }),
        }
    }

    /// Deletes both of `prepared`'s clusters, always attempting both
    /// even if one delete fails.
    ///
    /// Returns diagnostics rather than a `Result` deliberately: a
    /// failure to delete baseline must never prevent the attempt on
    /// candidate, and this shape makes that impossible to get wrong by
    /// short-circuiting on `?`. An empty result means both deletes
    /// succeeded; each failure is reported as its own [`Diagnostic`],
    /// including the exact manual recovery command.
    pub async fn cleanup(&self, prepared: &PreparedLab) -> Vec<Diagnostic> {
        let (baseline_result, candidate_result) = tokio::join!(
            self.cluster_manager.delete(&prepared.baseline),
            self.cluster_manager.delete(&prepared.candidate),
        );

        let mut diagnostics = Vec::new();
        if let Err(error) = baseline_result {
            diagnostics.push(delete_failure_diagnostic(
                Side::Baseline,
                &prepared.baseline,
                &error,
            ));
        }
        if let Err(error) = candidate_result {
            diagnostics.push(delete_failure_diagnostic(
                Side::Candidate,
                &prepared.candidate,
                &error,
            ));
        }
        diagnostics
    }

    /// Deletes a cluster that came up on `side` after the *other* side
    /// failed to create, so `prepare_clusters` doesn't leak it. Reports
    /// the outcome as a `Diagnostic` rather than propagating a
    /// `Result`, matching [`LabRunner::cleanup`]'s own "always attempt,
    /// always report" contract (PRODUCT.md §33).
    async fn delete_orphan(&self, side: Side, handle: &ClusterHandle) -> Vec<Diagnostic> {
        match self.cluster_manager.delete(handle).await {
            Ok(()) => Vec::new(),
            Err(error) => vec![delete_failure_diagnostic(side, handle, &error)],
        }
    }
}

// =========================================================================
// Stack installation (Task 2.6). See this module's documentation
// ("Stack installation (Task 2.6)") for why this abstraction lives here
// rather than in `admissionlab-installer`.
// =========================================================================

/// An abstraction over installing one side's whole ordered component
/// stack onto an already-created cluster. See this module's
/// documentation for why this lives in `admissionlab-core` rather than
/// `admissionlab-installer`, and what a concrete implementation is
/// expected to delegate to.
///
/// `Send + Sync` for the same reason [`ClusterManager`] is: a concrete
/// implementation is driven concurrently for both sides by
/// [`LabRunner::install_stacks`].
#[async_trait]
pub trait StackInstaller: Send + Sync {
    /// Installs `components`, in order, onto `cluster`. A real
    /// implementation is expected to delegate directly to
    /// `admissionlab_installer::stack::install_stack`, which this
    /// method's own `cluster`/`components`/`component_timeout`
    /// parameters mirror exactly — see that function's documentation
    /// for what "in order" and `component_timeout` mean.
    ///
    /// # Errors
    ///
    /// Returns [`StackInstallError`] if any component failed to install
    /// or never became ready.
    async fn install_stack(
        &self,
        cluster: &ClusterHandle,
        components: &[ResolvedComponent],
        component_timeout: Duration,
    ) -> Result<SideInstall, StackInstallError>;
}

/// One component's outcome within a [`StackInstaller::install_stack`]
/// call. Field-for-field the same shape as
/// `admissionlab_installer::InstallRecord` — see this module's
/// documentation for why this crate holds its own copy of that shape
/// rather than naming that type directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledComponent {
    /// The installed component's name.
    pub name: String,
    /// A short, stable label for which install method actually ran (for
    /// example `"helm"`).
    pub method: String,
    /// The version actually installed, confirmed against the cluster
    /// when possible — never fabricated when it could not be confirmed
    /// (Global Constraint 15).
    pub resolved_version: String,
    /// Wall-clock time this component's install began.
    pub started_at: SystemTime,
    /// Wall-clock time this component's install (and readiness wait)
    /// took.
    pub elapsed: Duration,
    /// Non-fatal findings surfaced while installing this component.
    /// Empty when there is nothing to report.
    pub diagnostics: Vec<Diagnostic>,
}

/// What [`StackInstaller::install_stack`] reports for one successfully
/// installed side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideInstall {
    /// Which side this stack was installed onto.
    pub side: Side,
    /// One [`InstalledComponent`] per installed component, in the order
    /// they were installed.
    pub components: Vec<InstalledComponent>,
}

/// Both sides' stacks, once [`LabRunner::install_stacks`] has installed
/// both successfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledLab {
    /// The installed baseline stack.
    pub baseline: SideInstall,
    /// The installed candidate stack.
    pub candidate: SideInstall,
}

/// [`StackInstaller::install_stack`] could not install every component
/// it was given onto one side's cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackInstallError {
    /// Which component (by name) failed to install or never became
    /// ready. `None` only if a [`StackInstaller`] implementation fails
    /// before it can identify one.
    pub component: Option<String>,
    /// A human-readable, safe-to-print explanation. See this module's
    /// documentation for why this crate holds a rendered `String` here
    /// rather than a typed `admissionlab_installer::InstallError`.
    pub message: String,
}

impl fmt::Display for StackInstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.component {
            Some(component) => write!(
                f,
                "component {component:?} failed to install: {}",
                self.message
            ),
            None => write!(f, "stack installation failed: {}", self.message),
        }
    }
}

impl std::error::Error for StackInstallError {}

/// Which side(s) failed to install its stack, and why. See
/// [`LabRunner::install_stacks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StackInstallFailure {
    /// Only the baseline side failed; candidate installed successfully.
    Baseline(StackInstallError),
    /// Only the candidate side failed; baseline installed successfully.
    Candidate(StackInstallError),
    /// Both sides failed.
    Both {
        /// The baseline side's failure.
        baseline: StackInstallError,
        /// The candidate side's failure.
        candidate: StackInstallError,
    },
}

impl fmt::Display for StackInstallFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Baseline(error) => write!(f, "baseline stack failed to install: {error}"),
            Self::Candidate(error) => write!(f, "candidate stack failed to install: {error}"),
            Self::Both {
                baseline,
                candidate,
            } => write!(
                f,
                "both stacks failed to install: baseline: {baseline}; candidate: {candidate}"
            ),
        }
    }
}

impl<C: ClusterManager> LabRunner<C> {
    /// Installs both sides' component stacks onto `prepared`'s
    /// already-created clusters, concurrently — `lab.baseline.components`
    /// onto `prepared.baseline`, and `lab.candidate.components` onto
    /// `prepared.candidate`, each side through its own call to
    /// `stack_installer.install_stack` with the same `component_timeout`.
    ///
    /// Uses `tokio::join!`, never `try_join!` — the same rule
    /// [`LabRunner::prepare_clusters`] follows and this module's
    /// documentation explains for that method. It applies here for a
    /// related but distinct reason: unlike an orphaned *cluster*, an
    /// abandoned in-flight stack install leaks no additional external
    /// resource on its own (both clusters are torn down by
    /// [`LabRunner::cleanup`] regardless of how this method concludes),
    /// but forcibly dropping a still-running install's future from
    /// outside the [`StackInstaller`] that owns it is not a guarantee
    /// this module can make for every current and future implementation
    /// — the same reason `admissionlab_installer::stack::install_stack`
    /// itself never wraps a component's own `install` call in an outer
    /// cancelling timeout. Waiting for both sides to reach their own
    /// natural conclusion is the only shape that avoids relying on that
    /// unproven guarantee.
    ///
    /// # Errors
    ///
    /// Returns [`StackInstallFailure`] if either side's stack failed to
    /// install.
    pub async fn install_stacks(
        &self,
        lab: &ResolvedLab,
        prepared: &PreparedLab,
        stack_installer: &dyn StackInstaller,
        component_timeout: Duration,
    ) -> Result<InstalledLab, StackInstallFailure> {
        let (baseline_result, candidate_result) = tokio::join!(
            stack_installer.install_stack(
                &prepared.baseline,
                &lab.baseline.components,
                component_timeout,
            ),
            stack_installer.install_stack(
                &prepared.candidate,
                &lab.candidate.components,
                component_timeout,
            ),
        );

        match (baseline_result, candidate_result) {
            (Ok(baseline), Ok(candidate)) => Ok(InstalledLab {
                baseline,
                candidate,
            }),
            (Err(error), Ok(_)) => Err(StackInstallFailure::Baseline(error)),
            (Ok(_), Err(error)) => Err(StackInstallFailure::Candidate(error)),
            (Err(baseline), Err(candidate)) => Err(StackInstallFailure::Both {
                baseline,
                candidate,
            }),
        }
    }
}
