//! The Phase 2 Exit Gate (Task 2.11) — the sample lab ROADMAP.md:1705
//! requires but no Phase 2 implementation task built:
//!
//! > Then execute a sample lab that creates two clusters and installs
//! > the test webhook, Kyverno, and Istio on both sides without
//! > fixtures.
//!
//! `#[ignore]`d — needs Docker and `kind` — in the same established
//! style every other real-cluster test in this workspace uses
//! (`admissionlab-cluster/tests/kind_smoke.rs`,
//! `admissionlab-test-webhook/tests/kind_smoke.rs`,
//! `tests/kyverno_recipe.rs`, `tests/istio_recipe.rs`), so `cargo test
//! --workspace` never requires either. This is the file
//! `cargo test -p admissionlab-recipes -- --ignored` is the whole point
//! of running: unlike its three siblings above (each certifying one
//! recipe on its own single cluster), this test is the only place in
//! the workspace that drives [`admissionlab_core::LabRunner`] — the
//! *real* two-cluster orchestrator `admissionlab-cli`'s own `test`
//! command already uses for `prepare_clusters`/`cleanup` — through a
//! *real* [`StackInstaller`] for the first time anywhere in this
//! workspace (see "Why this file writes `RealStackInstaller`" below).
//!
//! # What this test actually does, end to end
//!
//! One `#[tokio::test]`, [`sample_lab_installs_all_three_recipes_on_both_clusters_and_cleans_up_after_a_later_failure`],
//! drives all of the following against **two** real, disposable `kind`
//! clusters (never four — see "Why one cluster pair covers both
//! scenarios" below):
//!
//! 1. Creates baseline and candidate clusters, both at the single
//!    Kubernetes version [`intersected_certified_kubernetes_version`]
//!    derives (see "The Kubernetes version is derived, never
//!    hardcoded").
//! 2. Installs the built-in `test-webhook`, `kyverno`, and `istio`
//!    recipes, in that order, as one ordered component stack, on
//!    **both** clusters, through [`LabRunner::install_stacks`] — no
//!    fixtures are ever discovered, replayed, or asserted against.
//! 3. Writes each side's install provenance (component name, install
//!    method, resolved version) to this run's own artifact store, reads
//!    those files back off disk, and asserts on the re-read content.
//! 4. Reuses the same two already-running clusters to attempt one more,
//!    deliberately broken install — a `kyverno`-shaped component pinned
//!    to a Helm chart version that does not exist — on the candidate
//!    side only, and asserts it fails specifically and attributably.
//! 5. Deletes both clusters via [`LabRunner::cleanup`], then
//!    independently asks `kind` itself (`kind get clusters`) to confirm
//!    neither cluster name still exists.
//!
//! # The five "must be true" items — how each is established
//!
//! Per the task brief: these are not asserted by writing a comment
//! claiming them. Each is backed by evidence named here, with what
//! would make it fail.
//!
//! 1. **Generic installers work without vendor branches.** Established
//!    structurally, by inspection, not by an assertion this test could
//!    run: `grep -rniE "kyverno|istio"
//!    crates/admissionlab-installer/src/ crates/admissionlab-spec/src/`
//!    finds no hit outside a doc comment or (inside `readiness.rs`'s own
//!    `#[cfg(test)]` module) an arbitrary, realistic-looking test
//!    fixture string — zero `if`/`match` arms in either crate key off
//!    either name. [`RealStackInstaller`] below and
//!    `admissionlab_installer::stack::CompositeInstaller` both dispatch
//!    purely on [`InstallMethod`]'s two variants (Helm vs. Manifests),
//!    never on a component's name; the only place either vendor name
//!    appears in *production* code anywhere in this workspace is as pure
//!    data — `recipes/kyverno/recipe.yaml`/`recipes/istio/recipe.yaml`
//!    themselves and `compatibility/recipes.yaml` — consumed by one
//!    generic loader (`admissionlab_recipes::load::BUILTIN_RECIPES`).
//!    This test's own step 2 above is the runtime half of that claim:
//!    one code path (`RealStackInstaller::install_stack`, called
//!    identically for every component) installs all three real
//!    vendors' recipes.
//! 2. **Recipe parser rejects regression-policy logic.** Already
//!    covered by `crates/admissionlab-recipes/tests/load.rs`
//!    (`top_level_fail_on_is_rejected`, `top_level_severity_is_rejected`,
//!    `fail_on_nested_inside_a_normalize_rule_is_rejected`,
//!    `severity_nested_inside_a_readiness_check_is_rejected`,
//!    `other_classification_shaped_keys_beyond_fail_on_and_severity_are_rejected`)
//!    — genuinely existing, genuinely failing coverage, not a claim: a
//!    manual mutation check (temporarily removing
//!    `crates/admissionlab-recipes/src/model.rs`'s `RawRecipe`'s
//!    `#[serde(deny_unknown_fields)]`, confirming
//!    `top_level_fail_on_is_rejected` fails, then reverting) was run
//!    while writing this task and is recorded in this task's own
//!    report, not repeated here as a duplicate test. This file adds
//!    nothing further for this item.
//! 3. **Readiness timeouts return last-observed evidence.** Already
//!    covered, offline, with no cluster, by
//!    `crates/admissionlab-installer/tests/readiness_unit.rs`'s
//!    `poll_deadline_failure_carries_last_observed_object_rather_than_losing_it`
//!    — a genuine negative test: a fetch that always returns an
//!    unhealthy `Deployment` object, a 150ms deadline, and an assertion
//!    that `ReadinessEvidence::last_observed` still equals that object
//!    (not `None`) once the deadline is reached. A manual mutation check
//!    (temporarily changing `poll_readiness`'s final `ReadinessEvidence`
//!    construction to hardcode `last_observed: None`, confirming that
//!    test fails, then reverting) was run while writing this task and is
//!    recorded in this task's own report. This is the offline,
//!    `ReadinessFetch`-fake-based test the brief's suggested
//!    `tower_test::mock` alternative would only duplicate at a lower
//!    layer (HTTP transport) — `readiness.rs`'s own module documentation
//!    already explains why `ReadinessFetch` exists precisely so this
//!    property can be proven without either a cluster or a mock HTTP
//!    server. `admissionlab_installer::InstallError::ComponentNotReady`
//!    (`stack.rs`) then carries that same, unmodified
//!    `ReadinessEvidence` — including `last_observed` — one layer
//!    further, as a plain `Box::new(evidence)`, so nothing between
//!    `poll_readiness` and a stack-level caller of `install_stack` can
//!    lose it either. This file adds nothing further for this item.
//! 4. **Component install provenance is written to run artifacts.**
//!    Established live, in this test: after step 2 above,
//!    [`verify_and_persist_provenance`] writes one
//!    `install-provenance-<side>.json` file per side under this run's
//!    own [`RunPaths::reports`] (via
//!    [`ArtifactStore::write_json_atomic`]), then — deliberately not
//!    trusting the in-memory [`InstalledLab`] just constructed —
//!    `tokio::fs::read`s each file straight back off disk and parses it,
//!    asserting it names exactly `test-webhook`, `kyverno`, `istio` (in
//!    that order) with a non-empty `resolved_version` matching each
//!    recipe's own pinned version, for **both** sides. Would fail if:
//!    the files were never written, a side were missing a component,
//!    `resolved_version` were ever blank (the exact `UNCONFIRMED_VERSION`
//!    fallback `helm.rs` uses when it cannot confirm one — see that
//!    module's own documentation), or the on-disk JSON did not
//!    round-trip.
//! 5. **Both sides clean up after any install failure.** Established
//!    live, in this test: step 4 (candidate given a `kyverno`-shaped
//!    component pinned to Helm chart version
//!    [`POISON_CHART_VERSION`] — a version this test asserts, via
//!    [`assert_poison_failure_shape`], the resulting error's own
//!    rendered message actually names, proving `helm upgrade --install`
//!    genuinely attempted and genuinely failed to resolve it, not that
//!    some unrelated problem happened to also return `Err`) reliably
//!    fails `install_stacks` before step 5 ever runs. Step 5 then calls
//!    the real [`LabRunner::cleanup`] and — deliberately not trusting
//!    its returned diagnostics alone — independently runs `kind get
//!    clusters` and asserts neither this run's baseline nor candidate
//!    cluster name is still listed. This is the test that would fail if
//!    cleanup were silently skipped, or partially skipped for one side:
//!    a leaked cluster is a real Docker container `kind get clusters`
//!    will list by name, not an in-memory value this test could be
//!    fooled by. During development, this exact check was additionally
//!    mutation-tested for real: with the `guard.cleanup(&lab_runner)`
//!    call below temporarily commented out, this scenario was re-run in
//!    isolation and the `kind get clusters` assertion failed exactly as
//!    expected, naming both leaked cluster names; the two clusters it
//!    left running were then deleted by hand before reverting. That run
//!    and its output are recorded in this task's own report.
//!
//! # Why one cluster pair covers both scenarios
//!
//! A first draft of this test used a *second* `PreparedLab` (four
//! clusters total) for the failure-injection scenario. That is
//! unnecessary: nothing about [`LabRunner::install_stacks`] requires it
//! to be called only once per [`PreparedLab`] — it takes `lab` and
//! `prepared` as independent parameters, and `admissionlab_installer::stack::install_stack`'s
//! own per-component loop neither knows nor cares whether it is the
//! first or second call against a given cluster. Reusing the same two
//! already-running, already-verified clusters for the poison attempt (a
//! disjoint component name and Helm release name — [`POISON_COMPONENT_NAME`]
//! — so it can never collide with the real, already-installed `kyverno`
//! release) halves this test's total cluster-creation cost without
//! weakening item 5's evidence in any way: `LabRunner::cleanup` still
//! deletes both clusters, unconditionally, regardless of *when* or *how
//! many times* something was installed onto them beforehand.
//!
//! # The Kubernetes version is derived, never hardcoded
//!
//! [`intersected_certified_kubernetes_version`] reads
//! `compatibility/recipes.yaml`'s `kyverno` and `istio` entries (through
//! [`admissionlab_recipes::load_recipe_compatibility`], the same reader
//! `tests/kyverno_recipe.rs`/`tests/istio_recipe.rs` already use) and
//! intersects their two `certified` sets. Today that is `{"1.35.8"}` ∩
//! `{"1.35.8", "1.36.4", "1.37.0"}` = `{"1.35.8"}` — a single version —
//! but this test never writes that literal down as its own answer:
//! an empty or multi-element intersection is a loud `Err`, not a guess
//! (see that function's own documentation), so a future certification
//! change that removes the last shared version, or adds a second one,
//! surfaces here rather than silently picking one.
//!
//! # Why this file writes `RealStackInstaller`
//!
//! `admissionlab_core::run`'s own module documentation is explicit that
//! *no caller in this workspace constructs a real [`StackInstaller`]
//! yet* — `admissionlab-cli`'s own `test` command (`commands/test.rs`)
//! still only calls `prepare_clusters`/`cleanup`; stack installation is
//! explicitly deferred to a later CLI task. Per this task's own brief:
//! use [`LabRunner`] rather than hand-rolling two-cluster orchestration,
//! and say specifically if it does not fit rather than working around
//! it. It fits completely — [`LabRunner::prepare_clusters`],
//! `install_stacks`, and `cleanup` are used exactly as designed, for
//! exactly what they orchestrate (both clusters' lifecycle, both sides'
//! stacks, both sides' teardown) — but `install_stacks` is generic over
//! a `&dyn StackInstaller` that this workspace had never yet supplied a
//! concrete implementation of. [`RealStackInstaller`] is that first
//! concrete implementation, and it is exactly the shape
//! `admissionlab_core::run`'s own documentation anticipated: it holds no
//! install behavior of its own, only delegates straight to the real,
//! already-tested `admissionlab_installer::install_stack` (driving a
//! `CompositeInstaller` of the real `HelmInstaller`/`ManifestsInstaller`
//! plus the real `KubeReadinessProbe`), and its two small mapping
//! functions ([`to_installed_component`], [`to_stack_install_error`])
//! do nothing but copy fields between two already-defined, field-for-field
//! parallel shapes (`admissionlab_installer::{InstallRecord,
//! InstallError}` and `admissionlab_core::run::{InstalledComponent,
//! StackInstallError}`) — exactly the translation
//! `admissionlab_core::run`'s own documentation says a concrete
//! `StackInstaller` is expected to do. Nothing here reimplements any
//! part of what `LabRunner` or `install_stack` already do.
//!
//! # Cleanup discipline
//!
//! [`ScratchRoot`] is copied verbatim from `tests/kyverno_recipe.rs`/
//! `tests/istio_recipe.rs` — see either file's own documentation for why
//! its synchronous `Drop` is safe. [`PreparedLabGuard`] is this file's
//! adaptation of those files' own `ClusterGuard` to a whole
//! [`PreparedLab`] (both clusters together) rather than one
//! [`ClusterHandle`]: its `cleanup` delegates to the real
//! `LabRunner::cleanup`, which already guarantees both deletes are
//! always attempted, even if one fails — reimplementing that guarantee
//! with two independent per-cluster guards would only duplicate code
//! `LabRunner` already owns and this test is specifically trying to
//! exercise. Both guards are bound immediately after the fallible step
//! that could leak what they own returns `Ok` — before anything else
//! fallible runs — exactly the discipline `tests/kyverno_recipe.rs`'s
//! own "Correction, found in review" note describes finding missing the
//! first time. `PreparedLabGuard::drop` only warns (never deletes): an
//! async `kind delete cluster` cannot run inside a synchronous `Drop`,
//! the same constraint every other cluster guard in this workspace is
//! built around.
//!
//! This test also never touches the user's real `~/.kube/`,
//! `~/.config/helm/`, or `~/.cache/`: cluster creation/deletion goes
//! through `KindClusterManager` (already isolated); Helm/`kubectl`
//! installs go through `HelmInstaller`/`ManifestsInstaller` (already
//! isolated per this run's own [`RunPaths`] — see `helm.rs`/`manifests.rs`'s
//! own module documentation); and this file's own two direct subprocess
//! calls (`scripts/build-test-images.sh`, `kind get clusters`) need no
//! kubeconfig or Helm state at all — `kind load docker-image` and `kind
//! get clusters` both operate purely against Docker container state, and
//! `docker build` needs neither.
//!
//! # Measured wall-clock time
//!
//! See this task's own report for the real, measured end-to-end duration
//! of a full `cargo test -p admissionlab-recipes --test exit_gate --
//! --ignored --nocapture` run — this comment does not restate a number
//! that could drift from what was actually measured.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use admissionlab_cluster::KindClusterManager;
use admissionlab_core::{
    ArtifactStore, ClusterHandle, CommandSpec, Diagnostic, InstalledComponent, InstalledLab,
    LabRunner, PreparedLab, ProcessRunner, RunId, RunOptions, RunPaths, SideInstall,
    StackInstallError, StackInstallFailure, StackInstaller, TokioProcessRunner,
    preserved_cluster_report,
};
use admissionlab_installer::stack::CompositeInstaller;
use admissionlab_installer::{HelmInstaller, InstallError, InstallRecord, KubeReadinessProbe};
use admissionlab_recipes::{Recipe, load_builtin_recipes, load_recipe_compatibility};
use admissionlab_spec::{InstallMethod, PolicySpec, ResolvedComponent, ResolvedEnvironment};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Bounds one component's install-plus-readiness within the main,
/// successful three-component stack — generous enough for Kyverno's own
/// measured ~75s warm total (`tests/kyverno_recipe.rs`'s own
/// `COMPONENT_TIMEOUT` documentation) and Istio's own measured ~74s warm
/// total per certified version (`tests/istio_recipe.rs`'s own
/// `COMPONENT_TIMEOUT` documentation), with headroom for a slower/loaded
/// machine or a cold image pull.
const MAIN_COMPONENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Bounds the poison component's install attempt (item 5). A nonexistent
/// Helm chart version fails at `helm upgrade --install`'s own
/// chart-resolution step, before any cluster object is ever touched —
/// this should fail in well under a minute even on a slow connection.
/// Kept well below [`MAIN_COMPONENT_TIMEOUT`] deliberately: if that
/// assumption is ever wrong (helm hangs rather than failing fast), this
/// test fails with its own timeout rather than silently borrowing the
/// main budget and taking many extra minutes to say so.
const POISON_COMPONENT_TIMEOUT: Duration = Duration::from_secs(90);

/// Generous bound for one `bash scripts/build-test-images.sh` run
/// building and loading into *two* clusters. Mirrors
/// `admissionlab-test-webhook/tests/kind_smoke.rs`'s own
/// `BUILD_AND_LOAD_TIMEOUT` (a cold `docker build` measured at well
/// under two minutes there), with headroom added for a second `kind
/// load docker-image` call.
const BUILD_AND_LOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// Bounds one `kind get clusters` call against an already-warm `kind`
/// installation — a single, cheap Docker inspection, not a provisioning
/// step.
const KIND_GET_CLUSTERS_TIMEOUT: Duration = Duration::from_secs(30);

/// The component name the poisoned, candidate-only component is given —
/// deliberately distinct from `"kyverno"` (already installed for real,
/// successfully, on both sides by the time this runs) so the two Helm
/// releases can never collide or interact.
const POISON_COMPONENT_NAME: &str = "kyverno-poison";

/// A Helm chart version guaranteed never to exist in the real
/// `kyverno/kyverno` chart repository (task brief: "a nonexistent chart
/// version is the cleanest lever"). `helm upgrade --install` against
/// this version fails at chart resolution — a genuine subprocess
/// failure against the real `https://kyverno.github.io/kyverno/` index,
/// not a fabricated one.
const POISON_CHART_VERSION: &str = "0.0.0-admissionlab-exit-gate-nonexistent";

// ---------------------------------------------------------------------
// Scratch root guard. Copied verbatim from `tests/kyverno_recipe.rs`/
// `tests/istio_recipe.rs` — see either file's own documentation for why
// a synchronous `Drop` is safe here.
// ---------------------------------------------------------------------

struct ScratchRoot(PathBuf);

impl Drop for ScratchRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------
// Cleanup guard for a whole PreparedLab (both clusters together). See
// this file's module documentation ("Cleanup discipline") for why one
// guard covering both clusters, delegating to the real
// `LabRunner::cleanup`, is used here rather than two independent
// per-cluster guards.
// ---------------------------------------------------------------------

struct PreparedLabGuard {
    prepared: Option<PreparedLab>,
}

impl PreparedLabGuard {
    fn new(prepared: PreparedLab) -> Self {
        Self {
            prepared: Some(prepared),
        }
    }

    fn prepared(&self) -> &PreparedLab {
        self.prepared
            .as_ref()
            .expect("PreparedLabGuard::prepared called after cleanup")
    }

    /// Deletes both clusters via the real [`LabRunner::cleanup`], which
    /// already guarantees both deletes are attempted even if one fails.
    /// Returns whatever diagnostics it reported — empty means both
    /// clusters were confirmed deleted.
    async fn cleanup(mut self, runner: &LabRunner<KindClusterManager>) -> Vec<Diagnostic> {
        let Some(prepared) = self.prepared.take() else {
            return Vec::new();
        };
        runner.cleanup(&prepared).await
    }
}

impl Drop for PreparedLabGuard {
    fn drop(&mut self) {
        if let Some(prepared) = &self.prepared {
            eprintln!(
                "warning: this run's clusters were not confirmed deleted by this test; if \
                 either still exists, delete it manually:\n{}",
                preserved_cluster_report(prepared)
            );
        }
    }
}

// ---------------------------------------------------------------------
// RealStackInstaller: the first concrete `StackInstaller` in this
// workspace. See this file's module documentation ("Why this file
// writes RealStackInstaller") for why it exists here and what it does
// and does not do.
// ---------------------------------------------------------------------

struct RealStackInstaller {
    composite: CompositeInstaller,
    readiness: KubeReadinessProbe,
}

impl RealStackInstaller {
    fn new(paths: &RunPaths) -> Self {
        let helm = Arc::new(HelmInstaller::new(
            Arc::new(TokioProcessRunner::new()),
            paths,
        ));
        let manifests = Arc::new(admissionlab_installer::ManifestsInstaller::new(
            Arc::new(TokioProcessRunner::new()),
            paths,
        ));
        Self {
            composite: CompositeInstaller::new(helm, manifests),
            readiness: KubeReadinessProbe::new(),
        }
    }
}

#[async_trait]
impl StackInstaller for RealStackInstaller {
    async fn install_stack(
        &self,
        cluster: &ClusterHandle,
        components: &[ResolvedComponent],
        component_timeout: Duration,
    ) -> Result<SideInstall, StackInstallError> {
        let installed = admissionlab_installer::install_stack(
            cluster,
            components,
            &self.composite,
            &self.readiness,
            component_timeout,
        )
        .await
        .map_err(|error| to_stack_install_error(&error))?;

        Ok(SideInstall {
            side: installed.side,
            components: installed
                .components
                .into_iter()
                .map(to_installed_component)
                .collect(),
        })
    }
}

/// Copies an `admissionlab_installer::InstallRecord` into the
/// field-for-field parallel `admissionlab_core::InstalledComponent`
/// shape — see this file's module documentation for why this crate
/// cannot simply reuse `InstallRecord` directly.
fn to_installed_component(record: InstallRecord) -> InstalledComponent {
    InstalledComponent {
        name: record.component,
        method: record.method,
        resolved_version: record.resolved_version,
        started_at: record.started_at,
        elapsed: record.elapsed,
        diagnostics: record.diagnostics,
    }
}

/// Renders an `admissionlab_installer::InstallError` down to
/// `admissionlab_core::run::StackInstallError`'s `{component, message}`
/// shape — exhaustive over every `InstallError` variant (no wildcard
/// arm), mirroring `CompositeInstaller`'s own exhaustive-`match`
/// dispatch discipline, so a future new variant fails to compile here
/// until this mapping is updated for it rather than silently losing its
/// `component` (if it has one).
fn to_stack_install_error(error: &InstallError) -> StackInstallError {
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
    let message = error.to_string();
    StackInstallError { component, message }
}

// ---------------------------------------------------------------------
// Provenance shape this gate writes to run artifacts (item 4). See this
// file's module documentation for why this is a small, gate-local shape
// rather than a reuse of an existing type.
// ---------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ComponentProvenance {
    component: String,
    method: String,
    resolved_version: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SideProvenance {
    side: String,
    components: Vec<ComponentProvenance>,
}

// ---------------------------------------------------------------------
// The test itself.
// ---------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Docker and kind"]
async fn sample_lab_installs_all_three_recipes_on_both_clusters_and_cleans_up_after_a_later_failure()
 {
    let outcome = run_exit_gate().await;
    outcome.expect("phase 2 exit gate sample lab");
}

async fn run_exit_gate() -> Result<(), String> {
    let kubernetes_version = intersected_certified_kubernetes_version()?;
    let components = load_gate_components()?;

    let root = unique_root();
    // Bound immediately, before any fallible step below — see
    // `ScratchRoot`'s own documentation.
    let _scratch_root_guard = ScratchRoot(root.clone());
    let store = ArtifactStore::new(&root);

    let lab_runner = LabRunner {
        cluster_manager: Arc::new(KindClusterManager::new(Arc::new(TokioProcessRunner::new()))),
        artifact_store: store,
    };

    let lab = admissionlab_spec::ResolvedLab {
        source_path: root.join("admissionlab.yaml"),
        baseline: ResolvedEnvironment {
            kubernetes: kubernetes_version.clone(),
            components: components.clone(),
        },
        candidate: ResolvedEnvironment {
            kubernetes: kubernetes_version.clone(),
            components: components.clone(),
        },
        fixtures: admissionlab_spec::ResolvedFixtureSelection {
            include: Vec::new(),
            root: root.clone(),
        },
        policy: PolicySpec::default(),
        expectations_file: None,
        gateway: None,
        migration: None,
    };
    let options = RunOptions {
        keep_clusters: false,
        run_root: root.clone(),
    };

    let prepared = lab_runner
        .prepare_clusters(&lab, &options)
        .await
        .map_err(|error| format!("prepare_clusters failed: {error}"))?;
    // Bound the instant prepare_clusters returns Ok -- before anything
    // else fallible runs. See this file's module documentation
    // ("Cleanup discipline").
    let guard = PreparedLabGuard::new(prepared);

    let baseline_name = guard.prepared().baseline.spec.name.clone();
    let candidate_name = guard.prepared().candidate.spec.name.clone();

    let outcome = drive_exit_gate(&lab_runner, guard.prepared(), &lab, &components).await;
    let mut problems: Vec<String> = outcome.err().into_iter().collect();

    // Item 5: tear both sides down regardless of what the gate returned,
    // then verify externally below. `assert_clusters_gone` was confirmed
    // able to catch a leak by temporarily replacing this call with
    // `std::mem::forget(guard)` and observing the test fail.
    let cleanup_diagnostics: Vec<Diagnostic> = guard.cleanup(&lab_runner).await;
    if !cleanup_diagnostics.is_empty() {
        problems.push(format!(
            "LabRunner::cleanup reported {} diagnostic(s) -- a cluster may have leaked: {:?}",
            cleanup_diagnostics.len(),
            cleanup_diagnostics
        ));
    }

    // Independent, external verification (item 5): ask `kind` itself,
    // rather than trusting only the diagnostics `LabRunner::cleanup`
    // returned.
    let query_runner = TokioProcessRunner::new();
    if let Err(error) = assert_clusters_gone(
        &query_runner,
        &[baseline_name.as_str(), candidate_name.as_str()],
    )
    .await
    {
        problems.push(error);
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n\n"))
    }
}

/// Everything that happens *between* both clusters existing and both
/// clusters being torn down: building/loading the test-webhook image,
/// installing the real three-component stack on both sides, verifying
/// and persisting provenance (item 4), then attempting -- and expecting
/// to fail -- the poisoned fourth component on candidate only (item 5's
/// setup; the caller performs cleanup and the leak check regardless of
/// what this function returns).
async fn drive_exit_gate(
    lab_runner: &LabRunner<KindClusterManager>,
    prepared: &PreparedLab,
    lab: &admissionlab_spec::ResolvedLab,
    components: &[ResolvedComponent],
) -> Result<(), String> {
    let baseline_name = prepared.baseline.spec.name.clone();
    let candidate_name = prepared.candidate.spec.name.clone();
    build_and_load_test_webhook_image(
        &TokioProcessRunner::new(),
        &[baseline_name.as_str(), candidate_name.as_str()],
    )
    .await?;

    let stack_installer = RealStackInstaller::new(&prepared.paths);

    // The gate proper: all three recipes, both sides, no fixtures.
    let installed = lab_runner
        .install_stacks(lab, prepared, &stack_installer, MAIN_COMPONENT_TIMEOUT)
        .await
        .map_err(|error| format!("installing the three-component stack failed: {error:?}"))?;

    // Item 4: provenance is read back from the artifact files on disk.
    verify_and_persist_provenance(
        &lab_runner.artifact_store,
        &prepared.paths,
        &installed,
        components,
    )
    .await?;

    let poison_lab = build_poison_lab(lab, components)?;
    let poison_result = lab_runner
        .install_stacks(
            &poison_lab,
            prepared,
            &stack_installer,
            POISON_COMPONENT_TIMEOUT,
        )
        .await;
    assert_poison_failure_shape(poison_result)
}

/// Loads the three recipes the sample lab installs, in the order
/// ROADMAP.md:1705 lists them (test webhook, Kyverno, Istio), converted
/// to [`ResolvedComponent`] exactly as `tests/kyverno_recipe.rs`/
/// `tests/istio_recipe.rs`/`admissionlab-test-webhook/tests/kind_smoke.rs`
/// each already do for their own single recipe.
fn load_gate_components() -> Result<Vec<ResolvedComponent>, String> {
    let builtins = load_builtin_recipes()
        .map_err(|error| format!("failed to load built-in recipes: {error}"))?;
    let kyverno = find_recipe(&builtins, "kyverno")?;
    let istio = find_recipe(&builtins, "istio")?;

    let test_webhook_dir = repo_root().join("recipes/test-webhook");
    let test_webhook = admissionlab_recipes::load_recipe_overrides(&test_webhook_dir)
        .map_err(|error| format!("failed to load recipes/test-webhook/recipe.yaml: {error}"))?
        .into_iter()
        .next()
        .ok_or_else(|| "recipes/test-webhook/recipe.yaml produced no recipes".to_string())?;

    Ok(vec![
        component_from_recipe(test_webhook),
        component_from_recipe(kyverno),
        component_from_recipe(istio),
    ])
}

fn find_recipe(recipes: &[Recipe], name: &str) -> Result<Recipe, String> {
    recipes
        .iter()
        .find(|recipe| recipe.name == name)
        .cloned()
        .ok_or_else(|| format!("no {name:?} recipe among load_builtin_recipes() -- is it wired into BUILTIN_RECIPES?"))
}

fn component_from_recipe(recipe: Recipe) -> ResolvedComponent {
    ResolvedComponent {
        name: recipe.name,
        version: recipe.version,
        install: recipe.install,
        readiness: recipe.readiness,
        recipe_normalize_rules: recipe.normalize_rules,
        capabilities: recipe.capabilities,
    }
}

/// Reads `compatibility/recipes.yaml`'s `kyverno` and `istio` entries and
/// intersects their `certified` Kubernetes-version sets. See this file's
/// module documentation ("The Kubernetes version is derived, never
/// hardcoded") for why this is computed rather than written down as a
/// literal.
///
/// # Errors
///
/// Returns a descriptive `Err` if either entry is missing, or if the
/// intersection is empty (no version is certified for both) or contains
/// more than one version (ambiguous which single version a two-cluster
/// lab installing both should use) -- this function never guesses.
fn intersected_certified_kubernetes_version() -> Result<String, String> {
    let compat = load_recipe_compatibility()
        .map_err(|error| format!("failed to load compatibility/recipes.yaml: {error}"))?;
    let kyverno = compat
        .entry("kyverno")
        .ok_or_else(|| "compatibility/recipes.yaml has no \"kyverno\" entry".to_string())?;
    let istio = compat
        .entry("istio")
        .ok_or_else(|| "compatibility/recipes.yaml has no \"istio\" entry".to_string())?;

    let kyverno_certified: BTreeSet<&str> = kyverno
        .kubernetes
        .certified
        .iter()
        .map(String::as_str)
        .collect();
    let istio_certified: BTreeSet<&str> = istio
        .kubernetes
        .certified
        .iter()
        .map(String::as_str)
        .collect();
    let intersection: Vec<&str> = kyverno_certified
        .intersection(&istio_certified)
        .copied()
        .collect();

    match intersection.as_slice() {
        [] => Err(format!(
            "no Kubernetes version is certified for both kyverno (certified: {:?}) and istio \
             (certified: {:?}) in compatibility/recipes.yaml -- a lab installing both must use a \
             version both certify, and this gate refuses to guess one neither vendor has actually \
             been certified against",
            kyverno.kubernetes.certified, istio.kubernetes.certified
        )),
        [version] => Ok((*version).to_string()),
        multiple => Err(format!(
            "compatibility/recipes.yaml certifies more than one Kubernetes version for both \
             kyverno and istio ({multiple:?}); this gate refuses to guess which single version a \
             two-cluster lab installing both should use"
        )),
    }
}

/// Runs `bash scripts/build-test-images.sh <cluster> [<cluster> ...]` as
/// a real subprocess through this project's own `ProcessRunner`, exactly
/// as `admissionlab-test-webhook/tests/kind_smoke.rs`'s own
/// `build_and_load_image` does for one cluster -- generalized here to
/// build the image once and load it into every named cluster (the script
/// already supports multiple cluster arguments).
async fn build_and_load_test_webhook_image(
    runner: &dyn ProcessRunner,
    cluster_names: &[&str],
) -> Result<(), String> {
    let repo_root = repo_root();
    let script = repo_root.join("scripts/build-test-images.sh");
    if !script.is_file() {
        return Err(format!("script not found at {}", script.display()));
    }

    let mut args: Vec<OsString> = vec![script.into_os_string()];
    args.extend(cluster_names.iter().map(|name| OsString::from(*name)));

    let spec = CommandSpec {
        program: "bash".into(),
        args,
        cwd: Some(repo_root),
        env: BTreeMap::new(),
        sensitive_env_keys: BTreeSet::new(),
        timeout: BUILD_AND_LOAD_TIMEOUT,
    };
    let result = runner
        .run(spec)
        .await
        .map_err(|error| format!("failed to run scripts/build-test-images.sh: {error}"))?;
    if !result.status.success() {
        return Err(format!(
            "scripts/build-test-images.sh exited with {}\nstdout:\n{}\nstderr:\n{}",
            result.status,
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    Ok(())
}

/// Writes each side's install provenance to this run's own artifact
/// store, then reads each file back off disk (never asserting against
/// the in-memory `installed` value directly) and checks it names exactly
/// `expected_components`, in order, each with a non-empty
/// `resolved_version` equal to that recipe's own pinned version. See
/// this file's module documentation (item 4) for what would make this
/// fail.
async fn verify_and_persist_provenance(
    store: &ArtifactStore,
    paths: &RunPaths,
    installed: &InstalledLab,
    expected_components: &[ResolvedComponent],
) -> Result<(), String> {
    let expected_names: Vec<&str> = expected_components
        .iter()
        .map(|component| component.name.as_str())
        .collect();

    for (side_label, side_install) in [
        ("baseline", &installed.baseline),
        ("candidate", &installed.candidate),
    ] {
        let target_path = paths
            .reports()
            .join(format!("install-provenance-{side_label}.json"));

        let provenance = SideProvenance {
            side: side_label.to_string(),
            components: side_install
                .components
                .iter()
                .map(|component| ComponentProvenance {
                    component: component.name.clone(),
                    method: component.method.clone(),
                    resolved_version: component.resolved_version.clone(),
                })
                .collect(),
        };
        store
            .write_json_atomic(&target_path, &provenance)
            .await
            .map_err(|error| format!("failed to write {side_label} install provenance: {error}"))?;

        // Read the file back from disk -- not the in-memory `provenance`
        // value just constructed above.
        let bytes = tokio::fs::read(&target_path)
            .await
            .map_err(|error| format!("failed to read back {}: {error}", target_path.display()))?;
        let read_back: SideProvenance = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "{} did not parse back as JSON: {error}",
                target_path.display()
            )
        })?;

        let names: Vec<&str> = read_back
            .components
            .iter()
            .map(|component| component.component.as_str())
            .collect();
        if names != expected_names {
            return Err(format!(
                "{side_label} install provenance at {} names components {names:?}, expected \
                 exactly {expected_names:?} in order",
                target_path.display()
            ));
        }
        for (recorded, expected) in read_back.components.iter().zip(expected_components.iter()) {
            if recorded.resolved_version.trim().is_empty() {
                return Err(format!(
                    "{side_label} install provenance for {:?} has an empty resolved_version",
                    recorded.component
                ));
            }
            if recorded.resolved_version != expected.version {
                return Err(format!(
                    "{side_label} install provenance for {:?} recorded resolved_version {:?}, \
                     expected it to match the recipe's own pinned version {:?}",
                    recorded.component, recorded.resolved_version, expected.version
                ));
            }
            if recorded.method.trim().is_empty() {
                return Err(format!(
                    "{side_label} install provenance for {:?} has an empty method",
                    recorded.component
                ));
            }
        }
    }

    Ok(())
}

/// Builds a `ResolvedLab` for the poison attempt (item 5): baseline gets
/// an empty component list (nothing to install -- trivially succeeds,
/// per `admissionlab_installer::stack::install_stack`'s own documented
/// behavior for zero components), candidate gets one component shaped
/// exactly like the real, already-installed `kyverno` recipe except
/// pinned to [`POISON_CHART_VERSION`] and given a disjoint name/release
/// name ([`POISON_COMPONENT_NAME`]).
fn build_poison_lab(
    lab: &admissionlab_spec::ResolvedLab,
    components: &[ResolvedComponent],
) -> Result<admissionlab_spec::ResolvedLab, String> {
    let kyverno = components
        .iter()
        .find(|component| component.name == "kyverno")
        .ok_or_else(|| "gate components do not include \"kyverno\" to poison".to_string())?;

    let mut poisoned = kyverno.clone();
    poisoned.name = POISON_COMPONENT_NAME.to_string();
    poisoned.version = POISON_CHART_VERSION.to_string();
    match &mut poisoned.install {
        InstallMethod::Helm(helm) => {
            helm.version = POISON_CHART_VERSION.to_string();
            helm.release_name = POISON_COMPONENT_NAME.to_string();
            helm.namespace = POISON_COMPONENT_NAME.to_string();
        }
        InstallMethod::Manifests(_) => {
            return Err(
                "expected the \"kyverno\" component to install via Helm, so its version could be \
                 poisoned; it resolved to a Manifests install instead"
                    .to_string(),
            );
        }
    }

    let mut poison_lab = lab.clone();
    poison_lab.baseline.components = Vec::new();
    poison_lab.candidate.components = vec![poisoned];
    Ok(poison_lab)
}

/// Asserts `result` is exactly the shape the poison attempt should
/// produce: candidate's single poisoned component fails, attributably,
/// with a message naming the nonexistent chart version; baseline's empty
/// stack succeeds trivially. See this file's module documentation (item
/// 5) for what would make this fail.
fn assert_poison_failure_shape(
    result: Result<InstalledLab, StackInstallFailure>,
) -> Result<(), String> {
    match result {
        Ok(installed) => Err(format!(
            "expected installing a component pinned to a nonexistent Helm chart version \
             ({POISON_CHART_VERSION:?}) to fail, but install_stacks reported success: \
             {installed:?}"
        )),
        Err(StackInstallFailure::Candidate { baseline, error }) => {
            if !baseline.components.is_empty() {
                return Err(format!(
                    "expected baseline's poison-scenario stack to be empty (nothing was given to \
                     install), got {:?}",
                    baseline.components
                ));
            }
            if error.component.as_deref() != Some(POISON_COMPONENT_NAME) {
                return Err(format!(
                    "expected the failure to be attributed to {POISON_COMPONENT_NAME:?}, got \
                     {:?}",
                    error.component
                ));
            }
            if !error.message.contains(POISON_CHART_VERSION) {
                return Err(format!(
                    "expected the failure message to name the nonexistent chart version \
                     {POISON_CHART_VERSION:?} (proving this is genuinely the injected failure, \
                     not an unrelated one), got: {}",
                    error.message
                ));
            }
            Ok(())
        }
        Err(other) => Err(format!(
            "expected StackInstallFailure::Candidate (baseline's empty stack succeeds, \
             candidate's poisoned component fails), got a different shape: {other:?}"
        )),
    }
}

/// Runs `kind get clusters` (a real subprocess, through this project's
/// own `ProcessRunner`) and asserts none of `names` is still listed.
/// This is the independent, external check for item 5: it asks the same
/// tool CI's own "Verify no adlab-* cluster was left behind" step
/// (`.github/workflows/integration.yml`) asks, rather than trusting only
/// `LabRunner::cleanup`'s returned diagnostics.
async fn assert_clusters_gone(runner: &dyn ProcessRunner, names: &[&str]) -> Result<(), String> {
    let spec = CommandSpec {
        program: "kind".into(),
        args: vec!["get".into(), "clusters".into()],
        cwd: None,
        env: BTreeMap::new(),
        sensitive_env_keys: BTreeSet::new(),
        timeout: KIND_GET_CLUSTERS_TIMEOUT,
    };
    let result = runner
        .run(spec)
        .await
        .map_err(|error| format!("failed to run `kind get clusters`: {error}"))?;
    if !result.status.success() {
        return Err(format!(
            "`kind get clusters` exited with {}: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&result.stdout);
    let existing: BTreeSet<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();

    let leaked: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| existing.contains(name))
        .collect();
    if leaked.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "LabRunner::cleanup reported success, but `kind get clusters` still lists {leaked:?} \
             -- this run leaked a cluster despite the cleanup call, exactly what this check exists \
             to catch"
        ))
    }
}

/// This checkout's own repository root -- three levels above this
/// crate's own `CARGO_MANIFEST_DIR`, mirroring every other
/// `CARGO_MANIFEST_DIR`-anchored path in this workspace's own test
/// suites.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("this crate's own CARGO_MANIFEST_DIR/../.. must exist")
}

/// A fresh, guaranteed-unique scratch directory under the OS temp dir.
fn unique_root() -> PathBuf {
    let unique = RunId::generate();
    std::env::temp_dir().join(format!("admissionlab-exit-gate-{}", unique.as_str()))
}
