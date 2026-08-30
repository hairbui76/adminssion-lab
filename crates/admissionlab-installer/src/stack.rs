//! Stack installation orchestration (Task 2.6): installing a whole,
//! ordered list of [`ResolvedComponent`]s onto one cluster.
//!
//! [`install_stack`] is the entry point: it drives
//! [`ComponentInstaller::install`] then, for every one of that
//! component's [`admissionlab_spec::ReadinessCheck`]s,
//! [`readiness::ReadinessProbe::wait`][crate::readiness::ReadinessProbe::wait]
//! — one component fully installed *and* confirmed ready before the
//! next one's install even begins. [`CompositeInstaller`] is the one
//! [`ComponentInstaller`] implementation [`install_stack`] is actually
//! given in production: it owns no install behavior of its own, only
//! dispatch (see "Installer dispatch" below).
//!
//! # Why installing is sequential within a side (Constraint 1)
//!
//! [`install_stack`] takes exactly one [`ClusterHandle`] — one side — and
//! its component loop is a plain, single `for` loop: one `.await` chain,
//! no `tokio::join!`/`futures::future::join_all`/`tokio::spawn` anywhere
//! in it. **This is deliberate and load-bearing, not an oversight to
//! optimize later.** `helm.rs`'s own module documentation ("Helm state
//! isolation") establishes that Helm's repository config/cache is
//! isolated *per side*, not per component: every component installed
//! onto the same side shares one `<side>-helm/repositories.yaml`, and
//! `helm repo add` does a read-modify-write on that one file. Two Helm
//! components installed concurrently on the *same* side would race on
//! that file — corrupting or losing a repository entry — even though
//! two components on *different* sides (baseline vs candidate) never
//! collide, because each side already has its own isolated directory.
//!
//! Task 2.6 brief Step 1 (component order preserved exactly) and Step 3
//! ("component order within a side remains deterministic") both name
//! the same requirement this sequential loop satisfies as a side effect
//! of its own simplicity: a sequential loop cannot reorder its own
//! input, and cannot let two of its own iterations run concurrently
//! either. **If a future change ever parallelizes this loop's body
//! (`join_all`, a `JoinSet`, anything that lets two components of the
//! same side install at once), it silently reintroduces the Helm race
//! this paragraph describes — this is the one thing about this
//! function's structure that must never change without re-solving that
//! race first.** Step 3's *cross-side* concurrency (baseline install
//! running at the same time as candidate's) is safe and expected — see
//! `admissionlab_core::run`'s `StackInstaller`/`LabRunner::install_stacks`
//! wiring — precisely because it never involves two calls to *this*
//! function sharing one [`ClusterHandle`]/one side's Helm state
//! directory.
//!
//! # Installer dispatch: one composite, not a generic parameter
//!
//! [`install_stack`]'s signature (fixed by Task 2.6's own interface
//! registry) takes exactly one `installer: &dyn ComponentInstaller`, but
//! there are two concrete installers
//! ([`helm::HelmInstaller`][crate::helm::HelmInstaller],
//! [`manifests::ManifestsInstaller`][crate::manifests::ManifestsInstaller])
//! and [`admissionlab_spec::InstallMethod`] has two variants
//! (`Helm`/`Manifests`). [`CompositeInstaller`] is the resolution: a
//! third [`ComponentInstaller`] implementation whose own `install` does
//! no installing itself, only routes `component` to whichever of its two
//! held installers actually matches `component.install`'s variant, via
//! an exhaustive `match` (no wildcard arm — a third
//! [`admissionlab_spec::InstallMethod`] variant would fail to compile
//! here until this `match` is updated, the same "force a deliberate
//! choice" discipline `admissionlab-cli`'s own `RunError` dispatch
//! already uses).
//!
//! This was chosen over the alternative the brief itself raises
//! ("or something else") — making [`install_stack`] generic over two
//! installers, or accepting a `HashMap`/`Vec` of installers keyed by
//! method — for three reasons:
//!
//! 1. **[`install_stack`]'s signature is fixed.** It already takes a
//!    single `&dyn ComponentInstaller` trait object, not a generic type
//!    parameter or a second collection argument; a composite is the only
//!    shape that fits through that one parameter without changing it.
//! 2. **Both concrete installers already reject the wrong method
//!    themselves.** [`helm::HelmInstaller::install`][crate::helm::HelmInstaller]
//!    and
//!    [`manifests::ManifestsInstaller::install`][crate::manifests::ManifestsInstaller]
//!    each already return [`InstallError::UnsupportedMethod`] for a
//!    component whose install method isn't theirs (Task 2.2/2.3). A
//!    composite's `match` means that guard is never actually reached
//!    through this dispatch path — each component is routed to the one
//!    installer that already claims to support it — but it remains live,
//!    unremoved defense-in-depth for any other caller that holds
//!    [`helm::HelmInstaller`][crate::helm::HelmInstaller]/[`manifests::ManifestsInstaller`][crate::manifests::ManifestsInstaller]
//!    directly (both installers' own test suites already cover that
//!    guard).
//! 3. **It stays testable with fakes.** [`CompositeInstaller`] holds
//!    `Arc<dyn ComponentInstaller>` for each method, not the concrete
//!    installer types directly — so a test can inject two independent
//!    fakes and assert dispatch routes correctly, with no real `helm`/
//!    `kubectl` process ever spawned (`tests/stack.rs`'s
//!    `composite_installer_dispatches_helm_and_manifests_components_to_their_own_installer`),
//!    while production code (a later task's CLI wiring) constructs it
//!    from the two real installers.
//!
//! # `component_timeout`: one shared install-plus-readiness budget
//!
//! [`ComponentInstaller::install`]'s own signature (frozen since Task
//! 2.2/2.3) takes no timeout parameter at all — each concrete installer
//! already owns its install-phase timeout budget internally
//! ([`helm::HelmInstaller`][crate::helm::HelmInstaller]'s
//! `HELM_UPGRADE_TIMEOUT`/`UPGRADE_PROCESS_TIMEOUT` constants are a
//! fixed, already-audited kill-and-reap guarantee; see that module's "Two
//! timeouts, deliberately different" section). [`install_stack`] does
//! **not** additionally wrap the `install` call in an outer
//! `tokio::time::timeout`: doing so would forcibly cancel a future that
//! owns an in-flight subprocess from *outside* the installer that spawned
//! it, and this crate has no evidence that doing so is safe for every
//! current and future [`ComponentInstaller`] implementation — each
//! installer's own internal timeout-and-kill discipline is the
//! correctness boundary Task 2.2/2.3 already built, audited, and tested
//! for exactly this purpose, and re-wrapping it here would be redundant
//! at best and unsound at worst.
//!
//! What `component_timeout` *does* bound is deliberately the combination
//! of install-plus-readiness, computed as one deadline
//! (`Instant::now() + component_timeout`) right before `install` is
//! called for that component, and reused unchanged for every one of that
//! component's readiness checks — not reset to a fresh window per check,
//! and not computed fresh only once `install` returns. Two reasons:
//!
//! - **Readiness is the only phase left for this parameter to bound.**
//!   `helm.rs`'s own documentation is explicit that Helm's `--timeout`
//!   "does **not** bound... the main Deployment/DaemonSet/StatefulSet's
//!   pods actually scheduling or pulling their images" — that wait
//!   happens during readiness probing instead (Task 2.4). Since
//!   [`install_stack`] is given exactly one [`Duration`], and nothing
//!   else in its signature offers a second one, that one value is the
//!   only candidate for bounding the very wait Helm's own timeout
//!   deliberately excludes; treating it as install-only would leave
//!   image-pull wait time completely unbounded here, which "bounds each
//!   component" cannot mean.
//! - **One shared clock keeps the meaning of "`component_timeout`" honest
//!   as a single number.** A component with several readiness checks
//!   splits one budget across them (whatever a slow first check
//!   consumes, a later check receives less of), rather than each check
//!   getting its own full-length window — which would let a
//!   many-checks component silently cost a multiple of the stated
//!   timeout. Computing the deadline before `install` is called, rather
//!   than after it returns, extends the same idea to the install phase
//!   itself: in the overwhelming common case (an installer well within
//!   its own internal bound), most of `component_timeout` is still
//!   available for readiness once `install` returns; in the rare case an
//!   installer's own internal timeout is close to or exceeds
//!   `component_timeout`, readiness simply receives whatever is left —
//!   which [`readiness::ReadinessProbe::wait`][crate::readiness::ReadinessProbe::wait]'s
//!   own contract already handles gracefully (it always attempts at
//!   least once, even against an already-past deadline; see that
//!   trait's documentation), never a panic or an unbounded wait.
//!
//! `tests/stack.rs`'s
//! `install_stack_shares_one_deadline_across_a_components_multiple_readiness_checks`
//! is the regression test for the "one shared deadline, not reset per
//! check" half of this; the "install-plus-readiness, not install-only"
//! half follows from there being no second `Duration` in the signature
//! to bound readiness with instead.
//!
//! # A readiness timeout is a stack-level installation failure
//!
//! Kyverno creates its `ValidatingWebhookConfiguration`/
//! `MutatingWebhookConfiguration` objects at runtime, after `helm
//! install` already returns (`helm.rs`'s own dependency comment and
//! `readiness.rs`'s module documentation both establish this). A stack
//! that moved on to installing its next component while the current
//! one's webhooks were still registering could produce a genuinely wrong
//! comparison later — this is why [`install_stack`] awaits every
//! readiness check before moving on at all. Doing so is only meaningful
//! if a check that *never* becomes satisfied is also treated as a
//! failure: [`readiness::ReadinessProbe::wait`][crate::readiness::ReadinessProbe::wait]
//! itself reports "waited out the whole deadline, never satisfied" as
//! data (`Ok(ReadinessEvidence { satisfied: false, .. })`), not an
//! error — but [`install_stack`] converts that into
//! [`InstallError::ComponentNotReady`] and stops the stack, exactly like
//! any other installation failure (Task 2.6 brief Step 2). Continuing
//! past an unsatisfied check would be the same miscomparison risk this
//! whole design exists to prevent, just reached through a timeout
//! instead of skipping the wait outright.

use std::sync::Arc;
use std::time::{Duration, Instant};

use admissionlab_core::{ClusterHandle, Side};
use admissionlab_spec::{InstallMethod, ResolvedComponent};
use async_trait::async_trait;

use crate::readiness::ReadinessProbe;
use crate::{ComponentInstaller, InstallError, InstallRecord};

/// A [`ComponentInstaller`] that installs no component itself: it holds
/// one installer per [`InstallMethod`] variant and routes each
/// component to whichever one actually matches its own resolved method.
/// See this module's documentation ("Installer dispatch") for why this
/// shape was chosen over the alternatives.
///
/// Holds `Arc<dyn ComponentInstaller>` rather than the concrete
/// [`helm::HelmInstaller`][crate::helm::HelmInstaller]/[`manifests::ManifestsInstaller`][crate::manifests::ManifestsInstaller]
/// types directly, so a test can inject fakes for both (`tests/stack.rs`)
/// while production code wires in the two real installers, all through
/// the same constructor.
pub struct CompositeInstaller {
    helm: Arc<dyn ComponentInstaller>,
    manifests: Arc<dyn ComponentInstaller>,
}

impl CompositeInstaller {
    /// Creates a dispatcher that routes a Helm-method component to
    /// `helm` and a Manifests-method component to `manifests`.
    #[must_use]
    pub fn new(helm: Arc<dyn ComponentInstaller>, manifests: Arc<dyn ComponentInstaller>) -> Self {
        Self { helm, manifests }
    }
}

#[async_trait]
impl ComponentInstaller for CompositeInstaller {
    async fn install(
        &self,
        cluster: &ClusterHandle,
        component: &ResolvedComponent,
    ) -> Result<InstallRecord, InstallError> {
        // Exhaustive, no wildcard arm: a third `InstallMethod` variant
        // must fail to compile here until this dispatch is updated for
        // it, rather than silently falling into one bucket (mirroring
        // `admissionlab-cli`'s own `RunError` dispatch discipline).
        match &component.install {
            InstallMethod::Helm(_) => self.helm.install(cluster, component).await,
            InstallMethod::Manifests(_) => self.manifests.install(cluster, component).await,
        }
    }
}

/// What a successful [`install_stack`] call reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledStack {
    /// Which side this stack was installed onto — [`ClusterHandle::spec`]'s
    /// own [`Side`], copied here for convenience.
    pub side: Side,
    /// One [`InstallRecord`] per installed component, in the same order
    /// as the `components` slice [`install_stack`] was called with.
    pub components: Vec<InstallRecord>,
}

/// Installs `components` onto `cluster`, in order: for each component,
/// calls `installer.install`, then awaits every one of that component's
/// readiness checks (via `readiness.wait`) before moving on to the next
/// component. See this module's documentation for why the loop is
/// sequential (never parallelized within a side), how `component_timeout`
/// is spent, and why an unsatisfied readiness check stops the stack the
/// same as an installation failure.
///
/// # Errors
///
/// Returns the first [`InstallError`] encountered, and does not attempt
/// any component after the one that failed (Task 2.6 brief Step 2):
///
/// - Whatever [`InstallError`] `installer.install` itself returned, for
///   the first component whose install failed.
/// - [`InstallError::ComponentReadinessUnavailable`] if `readiness.wait`
///   itself errored for one of an installed component's checks.
/// - [`InstallError::ComponentNotReady`] if `readiness.wait` completed
///   but reported a check as never satisfied before this component's
///   share of `component_timeout` elapsed.
pub async fn install_stack(
    cluster: &ClusterHandle,
    components: &[ResolvedComponent],
    installer: &dyn ComponentInstaller,
    readiness: &dyn ReadinessProbe,
    component_timeout: Duration,
) -> Result<InstalledStack, InstallError> {
    let mut records = Vec::with_capacity(components.len());

    // Deliberately a plain, sequential `for` loop -- see this module's
    // documentation ("Why installing is sequential within a side") for
    // why this must never become a `join_all`/`JoinSet`/similar without
    // re-solving the same-side Helm repository race that guards
    // against.
    for component in components {
        // One deadline for this whole component -- install, then every
        // one of its readiness checks -- computed once, before `install`
        // is even called. See this module's documentation
        // ("component_timeout") for why.
        let component_deadline = Instant::now() + component_timeout;

        let record = installer.install(cluster, component).await?;

        for check in &component.readiness {
            let evidence = readiness
                .wait(cluster, check, component_deadline)
                .await
                .map_err(|source| InstallError::ComponentReadinessUnavailable {
                    component: component.name.clone(),
                    source: Box::new(source),
                })?;
            if !evidence.satisfied {
                return Err(InstallError::ComponentNotReady {
                    component: component.name.clone(),
                    evidence: Box::new(evidence),
                });
            }
        }

        records.push(record);
    }

    Ok(InstalledStack {
        side: cluster.spec.side,
        components: records,
    })
}
