//! Behavioral tests for [`LabRunner`]'s top-level orchestration: bringing
//! baseline/candidate clusters up together and guaranteeing they come
//! down again, even when one side fails while the other is still being
//! created (PRODUCT.md §33, §10.4).
//!
//! Every test here drives [`LabRunner`] against [`FakeClusterManager`]
//! (this file's own whole-trait [`ClusterManager`] test double) — never
//! real Docker or `kind` — mirroring how earlier tasks exercise
//! `KindClusterManager` against a fake `ProcessRunner`
//! (`admissionlab-cluster`'s `tests/lifecycle_unit.rs`) rather than the
//! real external tool.
//!
//! The single most important property under test is
//! `candidate_creation_is_not_abandoned_when_baseline_fails_immediately`:
//! `prepare_clusters` must use `tokio::join!`, never `try_join!`, when
//! creating both clusters concurrently. `try_join!` would abandon the
//! still-in-flight side's future the instant the other side fails,
//! which would leak exactly the cluster this whole task exists to never
//! leak.
//!
//! The `install_stacks_*` tests (Task 2.6) cover
//! [`LabRunner::install_stacks`] the same way, against
//! [`FakeStackInstaller`]: both sides are installed, `tokio::join!`
//! (never `try_join!`) means a slow side is never abandoned when the
//! other fails, and a failure on either/both sides is reported through
//! [`StackInstallFailure`] naming the right side(s).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use admissionlab_core::{
    ArtifactStore, ClusterCreationFailure, ClusterDiagnostics, ClusterError, ClusterHandle,
    ClusterManager, ClusterSpec, InstalledComponent, LabRunner, RunError, RunId, RunOptions,
    RunPaths, Side, SideInstall, StackInstallError, StackInstallFailure, StackInstaller,
    preserved_cluster_report,
};
use admissionlab_spec::{ResolvedComponent, ResolvedLab, load_lab, resolve_lab};
use async_trait::async_trait;

// ---------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------

/// Mirrors `tests/artifact.rs`'s own hand-rolled runtime rather than
/// adopting `#[tokio::test]`, which would need a new `tokio` feature
/// (`macros`) this crate's production code has no other reason to
/// depend on.
fn test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build test tokio runtime")
}

/// A fresh, guaranteed-unique scratch directory under the OS temp dir.
/// Mirrors `tests/artifact.rs`'s/`tests/domain.rs`'s own unique-temp-dir
/// pattern rather than pulling in a new dependency for this alone.
fn unique_run_root(label: &str) -> PathBuf {
    let unique = RunId::generate();
    std::env::temp_dir().join(format!(
        "admissionlab-core-run-lifecycle-test-{label}-{}",
        unique.as_str()
    ))
}

/// Loads and resolves the workspace's checked-in minimal valid lab
/// configuration. Going through the real `load_lab`/`resolve_lab` is
/// simpler than hand-building a `ResolvedLab`: `ResolvedFixtureSelection`
/// holds compiled `globset::Glob`s, and `globset` is a dependency only
/// `admissionlab-spec` itself needs, not this crate.
fn minimal_resolved_lab() -> ResolvedLab {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/configs/minimal-valid.yaml");
    let loaded = load_lab(&path).expect("minimal-valid.yaml must load");
    resolve_lab(loaded).expect("minimal-valid.yaml must resolve")
}

// ---------------------------------------------------------------------
// FakeClusterManager: a whole-trait `ClusterManager` test double.
// ---------------------------------------------------------------------

/// How [`FakeClusterManager::create`] behaves for one side.
#[derive(Clone)]
enum CreatePlan {
    /// Succeed immediately (no `.await` point ever yields).
    Succeed,
    /// Sleep for the given duration, then succeed. Used to prove the
    /// *other* side's creation is never abandoned when this side takes
    /// longer to come up.
    SucceedAfter(Duration),
    /// Fail immediately.
    Fail,
}

/// Any [`ClusterError`] variant works as this file's fake failure: every
/// test here only cares that `create`/`delete` returned `Err`, never
/// which variant. [`ClusterError::InvalidKubeconfig`] is used because
/// its fields (a path and a reason string) are the simplest to
/// construct without needing a real `std::process::ExitStatus`.
fn fake_error(reason: String) -> ClusterError {
    ClusterError::InvalidKubeconfig {
        path: PathBuf::from("/fake/kubeconfig"),
        reason,
    }
}

/// Builds a plausible, non-filesystem-touching [`ClusterHandle`] for a
/// successful fake create: derived entirely from `spec`/`paths`, which
/// are themselves just computed paths (no I/O).
fn fake_handle(spec: &ClusterSpec, paths: &RunPaths) -> ClusterHandle {
    ClusterHandle {
        spec: spec.clone(),
        kubeconfig: paths
            .kubeconfigs()
            .join(format!("{}.yaml", spec.side.as_str())),
        audit_log: paths.logs().join(spec.side.as_str()).join("audit.log"),
    }
}

/// A [`ClusterManager`] test double. Never spawns a process or touches
/// Docker/kind: `create`/`delete` are plain, configurable, in-memory
/// outcomes. Records which sides `create`/`delete` were actually called
/// for, so tests can assert not just the returned `Result`/
/// `Vec<Diagnostic>` but that the *right* clusters were actually
/// attempted/torn down.
struct FakeClusterManager {
    baseline_create: CreatePlan,
    candidate_create: CreatePlan,
    fail_delete: HashSet<Side>,
    fail_resolve: bool,
    created: Mutex<Vec<Side>>,
    deleted: Mutex<Vec<Side>>,
    resolved: Mutex<Vec<String>>,
}

impl FakeClusterManager {
    fn new(baseline_create: CreatePlan, candidate_create: CreatePlan) -> Self {
        Self {
            baseline_create,
            candidate_create,
            fail_delete: HashSet::new(),
            fail_resolve: false,
            created: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
            resolved: Mutex::new(Vec::new()),
        }
    }

    /// Configures `delete` to fail for `side`'s cluster.
    fn failing_delete_for(mut self, side: Side) -> Self {
        self.fail_delete.insert(side);
        self
    }

    /// Configures `resolve_node_image` to fail for every version it is
    /// asked to resolve (Controller Ruling R25).
    fn failing_resolve(mut self) -> Self {
        self.fail_resolve = true;
        self
    }

    /// The sides `create` was actually invoked for, in call order.
    fn created_sides(&self) -> Vec<Side> {
        self.created.lock().expect("created mutex poisoned").clone()
    }

    /// The sides `delete` was actually invoked for, in call order.
    fn deleted_sides(&self) -> Vec<Side> {
        self.deleted.lock().expect("deleted mutex poisoned").clone()
    }

    /// The Kubernetes version strings `resolve_node_image` was actually
    /// asked to resolve, in call order.
    fn resolved_versions(&self) -> Vec<String> {
        self.resolved
            .lock()
            .expect("resolved mutex poisoned")
            .clone()
    }
}

#[async_trait]
impl ClusterManager for FakeClusterManager {
    async fn resolve_node_image(&self, kubernetes_version: &str) -> Result<String, ClusterError> {
        self.resolved
            .lock()
            .expect("resolved mutex poisoned")
            .push(kubernetes_version.to_owned());
        if self.fail_resolve {
            Err(fake_error(format!(
                "fake: cannot resolve Kubernetes version {kubernetes_version:?}"
            )))
        } else {
            Ok(format!("fake-resolved-image:{kubernetes_version}"))
        }
    }

    async fn create(
        &self,
        spec: &ClusterSpec,
        paths: &RunPaths,
    ) -> Result<ClusterHandle, ClusterError> {
        self.created
            .lock()
            .expect("created mutex poisoned")
            .push(spec.side);

        let plan = match spec.side {
            Side::Baseline => &self.baseline_create,
            Side::Candidate => &self.candidate_create,
        };
        match plan {
            CreatePlan::Succeed => Ok(fake_handle(spec, paths)),
            CreatePlan::SucceedAfter(duration) => {
                tokio::time::sleep(*duration).await;
                Ok(fake_handle(spec, paths))
            }
            CreatePlan::Fail => Err(fake_error(format!("fake {} create failure", spec.side))),
        }
    }

    async fn delete(&self, handle: &ClusterHandle) -> Result<(), ClusterError> {
        self.deleted
            .lock()
            .expect("deleted mutex poisoned")
            .push(handle.spec.side);
        if self.fail_delete.contains(&handle.spec.side) {
            Err(fake_error(format!(
                "fake {} delete failure",
                handle.spec.side
            )))
        } else {
            Ok(())
        }
    }

    async fn diagnostics(&self, handle: &ClusterHandle) -> ClusterDiagnostics {
        ClusterDiagnostics {
            cluster_name: handle.spec.name.clone(),
            cluster_exists: Some(true),
            kubeconfig_present: false,
            audit_log_present: false,
            notes: Vec::new(),
        }
    }
}

/// Builds a `LabRunner<FakeClusterManager>` rooted at a fresh, unique
/// temp directory, alongside the matching `RunOptions` — both built from
/// the same root, matching the invariant `prepare_clusters` documents.
/// Returns the root too, so a test can remove it afterward.
fn runner_with(
    label: &str,
    manager: FakeClusterManager,
) -> (LabRunner<FakeClusterManager>, RunOptions, PathBuf) {
    let run_root = unique_run_root(label);
    let runner = LabRunner {
        cluster_manager: Arc::new(manager),
        artifact_store: ArtifactStore::new(&run_root),
    };
    let options = RunOptions {
        keep_clusters: false,
        run_root: run_root.clone(),
    };
    (runner, options, run_root)
}

/// Sorts a `Vec<Side>` by its stable string form, so an assertion that
/// both sides appear (regardless of which finished first under
/// concurrent execution) is never flaky.
fn sorted(mut sides: Vec<Side>) -> Vec<Side> {
    sides.sort_by_key(|side| side.as_str());
    sides
}

// ---------------------------------------------------------------------
// Cleanup on baseline create failure.
// ---------------------------------------------------------------------

#[test]
fn cleanup_on_baseline_create_failure_deletes_the_orphaned_candidate() {
    let (runner, options, root) = runner_with(
        "baseline-fails",
        FakeClusterManager::new(CreatePlan::Fail, CreatePlan::Succeed),
    );
    let lab = minimal_resolved_lab();

    let result = test_runtime().block_on(runner.prepare_clusters(&lab, &options));

    match result {
        Err(RunError::ClusterCreationFailed { failure, rollback }) => {
            assert!(
                matches!(failure, ClusterCreationFailure::Baseline(_)),
                "expected a Baseline creation failure, got {failure:?}"
            );
            assert!(
                rollback.is_empty(),
                "deleting the orphaned candidate succeeded, so there should be no rollback \
                 diagnostics, got {rollback:?}"
            );
        }
        other => panic!("expected Err(ClusterCreationFailed), got {other:?}"),
    }

    assert_eq!(
        runner.cluster_manager.deleted_sides(),
        vec![Side::Candidate],
        "the candidate cluster that DID come up must be deleted; baseline never came up, so \
         nothing should be deleted for it"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// Cleanup on candidate create failure.
// ---------------------------------------------------------------------

#[test]
fn cleanup_on_candidate_create_failure_deletes_the_orphaned_baseline() {
    let (runner, options, root) = runner_with(
        "candidate-fails",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Fail),
    );
    let lab = minimal_resolved_lab();

    let result = test_runtime().block_on(runner.prepare_clusters(&lab, &options));

    match result {
        Err(RunError::ClusterCreationFailed { failure, rollback }) => {
            assert!(
                matches!(failure, ClusterCreationFailure::Candidate(_)),
                "expected a Candidate creation failure, got {failure:?}"
            );
            assert!(rollback.is_empty(), "got {rollback:?}");
        }
        other => panic!("expected Err(ClusterCreationFailed), got {other:?}"),
    }

    assert_eq!(
        runner.cluster_manager.deleted_sides(),
        vec![Side::Baseline],
        "the baseline cluster that DID come up must be deleted; candidate never came up, so \
         nothing should be deleted for it"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn both_sides_failing_to_create_needs_no_rollback_deletes() {
    let (runner, options, root) = runner_with(
        "both-fail",
        FakeClusterManager::new(CreatePlan::Fail, CreatePlan::Fail),
    );
    let lab = minimal_resolved_lab();

    let result = test_runtime().block_on(runner.prepare_clusters(&lab, &options));

    match result {
        Err(RunError::ClusterCreationFailed { failure, rollback }) => {
            assert!(
                matches!(failure, ClusterCreationFailure::Both { .. }),
                "expected Both, got {failure:?}"
            );
            assert!(rollback.is_empty(), "got {rollback:?}");
        }
        other => panic!("expected Err(ClusterCreationFailed), got {other:?}"),
    }

    // Neither side ever came up: each `ClusterManager::create` failure is
    // contracted to have already handled its own potential leak (see
    // `ClusterError::CreateFailedWithRollback`'s documentation), so
    // `prepare_clusters` itself must not call `delete` for either side.
    assert!(
        runner.cluster_manager.deleted_sides().is_empty(),
        "neither side ever came up, so prepare_clusters must not attempt any delete"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// The concurrency guarantee: tokio::join!, never try_join!.
// ---------------------------------------------------------------------

#[test]
fn candidate_creation_is_not_abandoned_when_baseline_fails_immediately() {
    // If `prepare_clusters` used `try_join!` instead of `tokio::join!`,
    // this test would fail: `try_join!` returns the instant baseline's
    // future resolves to `Err`, dropping candidate's still-sleeping
    // future before it ever produces a handle — so candidate's cluster
    // would never even be recorded as created, let alone deleted, and
    // would leak. `tokio::join!` waits for both futures regardless of
    // either's outcome, so candidate's `create` always runs to
    // completion and its resulting cluster is always deleted here.
    let (runner, options, root) = runner_with(
        "concurrency",
        FakeClusterManager::new(
            CreatePlan::Fail,
            CreatePlan::SucceedAfter(Duration::from_millis(75)),
        ),
    );
    let lab = minimal_resolved_lab();

    let result = test_runtime().block_on(runner.prepare_clusters(&lab, &options));

    assert!(
        result.is_err(),
        "baseline failed, so prepare_clusters must fail overall"
    );
    assert_eq!(
        runner.cluster_manager.deleted_sides(),
        vec![Side::Candidate],
        "candidate's slow create must have been awaited to completion (tokio::join!, not \
         try_join!) and then deleted, since prepare_clusters failed overall"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// run_root validation.
// ---------------------------------------------------------------------

#[test]
fn prepare_clusters_rejects_a_relative_run_root_before_creating_anything() {
    let manager = FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed);
    let runner = LabRunner {
        cluster_manager: Arc::new(manager),
        artifact_store: ArtifactStore::new(Path::new("relative-root")),
    };
    let options = RunOptions {
        keep_clusters: false,
        run_root: PathBuf::from("relative-root"),
    };
    let lab = minimal_resolved_lab();

    let result = test_runtime().block_on(runner.prepare_clusters(&lab, &options));

    assert!(
        matches!(result, Err(RunError::NonAbsoluteRunRoot(_))),
        "expected Err(NonAbsoluteRunRoot), got {result:?}"
    );
    assert!(
        runner.cluster_manager.created_sides().is_empty(),
        "a rejected run root must fail before any cluster creation is attempted"
    );
}

// ---------------------------------------------------------------------
// Node image resolution (Controller Ruling R25): an unresolvable
// version must surface as a clear error, never be passed through to
// `create` as a bogus image.
// ---------------------------------------------------------------------

#[test]
fn prepare_clusters_reports_an_unresolvable_version_and_never_calls_create() {
    let (runner, options, root) = runner_with(
        "unresolvable-version",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed).failing_resolve(),
    );
    let lab = minimal_resolved_lab();

    let result = test_runtime().block_on(runner.prepare_clusters(&lab, &options));

    match result {
        Err(RunError::NodeImageResolutionFailed { side, .. }) => {
            assert_eq!(
                side,
                Side::Baseline,
                "resolution is attempted baseline-first; the first failure is the one reported"
            );
        }
        other => panic!("expected Err(NodeImageResolutionFailed), got {other:?}"),
    }

    // The whole point: a version that cannot be resolved must never
    // reach `create` as an unvalidated, possibly-bogus image reference.
    assert!(
        runner.cluster_manager.created_sides().is_empty(),
        "an unresolvable version must fail before any cluster creation is attempted"
    );
    // Resolution was attempted (and failed) for baseline; candidate's
    // resolution is never even attempted once baseline's already failed
    // -- there is nothing to roll back either way, since resolving
    // creates no resource, so failing fast here (unlike `create`, which
    // must never abandon the other side) loses nothing.
    assert_eq!(
        runner.cluster_manager.resolved_versions(),
        vec![lab.baseline.kubernetes.clone()]
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn prepare_clusters_resolves_both_sides_node_images_before_creating_either() {
    let (runner, options, root) = runner_with(
        "resolve-both",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed),
    );
    let lab = minimal_resolved_lab();

    let prepared = test_runtime()
        .block_on(runner.prepare_clusters(&lab, &options))
        .expect("both sides succeeding must succeed overall");

    assert_eq!(
        runner.cluster_manager.resolved_versions(),
        vec![
            lab.baseline.kubernetes.clone(),
            lab.candidate.kubernetes.clone(),
        ]
    );
    // The resolved image -- not the bare requested version -- is what
    // ends up in the `ClusterSpec` a real `ClusterManager::create` would
    // receive; `kubernetes_version` still carries the original request,
    // for provenance.
    assert_eq!(
        prepared.baseline.spec.node_image,
        format!("fake-resolved-image:{}", lab.baseline.kubernetes)
    );
    assert_eq!(
        prepared.baseline.spec.kubernetes_version,
        lab.baseline.kubernetes
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// Successful preparation.
// ---------------------------------------------------------------------

#[test]
fn successful_preparation_returns_isolated_handles_for_both_sides() {
    let (runner, options, root) = runner_with(
        "success",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed),
    );
    let lab = minimal_resolved_lab();

    let prepared = test_runtime()
        .block_on(runner.prepare_clusters(&lab, &options))
        .expect("both sides succeeding must succeed overall");

    assert_eq!(prepared.baseline.spec.side, Side::Baseline);
    assert_eq!(prepared.candidate.spec.side, Side::Candidate);
    assert_ne!(
        prepared.baseline.kubeconfig, prepared.candidate.kubeconfig,
        "baseline and candidate must never share a kubeconfig path"
    );
    assert_eq!(prepared.baseline.spec.kubernetes_version, "1.29.4");
    assert_eq!(prepared.candidate.spec.kubernetes_version, "1.29.4");
    assert!(prepared.paths.root().starts_with(&root));
    assert!(prepared.paths.root().ends_with(prepared.run_id.as_str()));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cluster_names_follow_the_adlab_side_run_convention_and_share_a_run_suffix() {
    let (runner, options, root) = runner_with(
        "naming",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed),
    );
    let lab = minimal_resolved_lab();

    let prepared = test_runtime()
        .block_on(runner.prepare_clusters(&lab, &options))
        .expect("both sides succeeding must succeed overall");

    let baseline_suffix = prepared
        .baseline
        .spec
        .name
        .strip_prefix("adlab-baseline-")
        .expect("baseline cluster name must start with adlab-baseline-");
    let candidate_suffix = prepared
        .candidate
        .spec
        .name
        .strip_prefix("adlab-candidate-")
        .expect("candidate cluster name must start with adlab-candidate-");

    assert_eq!(
        baseline_suffix, candidate_suffix,
        "both clusters of the same run must share the same run-id suffix"
    );
    assert_eq!(
        baseline_suffix.len(),
        12,
        "the run-id suffix should be the same short, kind-safe length \
         admissionlab_cluster::kind::cluster_name uses"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// cleanup: a "later simulated run failure" always tears down both
// clusters, attempting both deletes even when one fails.
// ---------------------------------------------------------------------

#[test]
fn cleanup_deletes_both_and_reports_nothing_when_both_succeed() {
    let (runner, options, root) = runner_with(
        "cleanup-success",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed),
    );
    let lab = minimal_resolved_lab();
    let runtime = test_runtime();
    let prepared = runtime
        .block_on(runner.prepare_clusters(&lab, &options))
        .expect("setup: both sides must succeed");

    // Simulates cleanup being invoked after some later, not-yet-existing
    // pipeline stage (fixture execution, Phase 3) failed: cleanup itself
    // doesn't know or care why it was called.
    let diagnostics = runtime.block_on(runner.cleanup(&prepared));

    assert!(diagnostics.is_empty(), "got {diagnostics:?}");
    assert_eq!(
        sorted(runner.cluster_manager.deleted_sides()),
        vec![Side::Baseline, Side::Candidate]
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cleanup_attempts_candidate_even_when_baseline_delete_fails() {
    let (runner, options, root) = runner_with(
        "cleanup-baseline-delete-fails",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed)
            .failing_delete_for(Side::Baseline),
    );
    let lab = minimal_resolved_lab();
    let runtime = test_runtime();
    let prepared = runtime
        .block_on(runner.prepare_clusters(&lab, &options))
        .expect("setup: both sides must succeed");

    let diagnostics = runtime.block_on(runner.cleanup(&prepared));

    assert_eq!(diagnostics.len(), 1, "got {diagnostics:?}");
    assert_eq!(diagnostics[0].code, "cluster.delete_failed");
    assert_eq!(
        sorted(runner.cluster_manager.deleted_sides()),
        vec![Side::Baseline, Side::Candidate],
        "candidate must still be deleted even though baseline's delete failed"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cleanup_attempts_baseline_even_when_candidate_delete_fails() {
    let (runner, options, root) = runner_with(
        "cleanup-candidate-delete-fails",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed)
            .failing_delete_for(Side::Candidate),
    );
    let lab = minimal_resolved_lab();
    let runtime = test_runtime();
    let prepared = runtime
        .block_on(runner.prepare_clusters(&lab, &options))
        .expect("setup: both sides must succeed");

    let diagnostics = runtime.block_on(runner.cleanup(&prepared));

    assert_eq!(diagnostics.len(), 1, "got {diagnostics:?}");
    assert_eq!(diagnostics[0].code, "cluster.delete_failed");
    assert_eq!(
        sorted(runner.cluster_manager.deleted_sides()),
        vec![Side::Baseline, Side::Candidate],
        "baseline must still be deleted even though candidate's delete failed"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cleanup_reports_both_failures_when_both_deletes_fail() {
    let (runner, options, root) = runner_with(
        "cleanup-both-delete-fail",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed)
            .failing_delete_for(Side::Baseline)
            .failing_delete_for(Side::Candidate),
    );
    let lab = minimal_resolved_lab();
    let runtime = test_runtime();
    let prepared = runtime
        .block_on(runner.prepare_clusters(&lab, &options))
        .expect("setup: both sides must succeed");

    let diagnostics = runtime.block_on(runner.cleanup(&prepared));

    assert_eq!(diagnostics.len(), 2, "got {diagnostics:?}");
    assert_eq!(
        sorted(runner.cluster_manager.deleted_sides()),
        vec![Side::Baseline, Side::Candidate],
        "both deletes must still be attempted even though both fail"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// --keep-clusters reporting.
// ---------------------------------------------------------------------

#[test]
fn preserved_cluster_report_includes_names_kubeconfigs_and_exact_delete_commands() {
    let (runner, options, root) = runner_with(
        "preserve",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed),
    );
    let lab = minimal_resolved_lab();
    let prepared = test_runtime()
        .block_on(runner.prepare_clusters(&lab, &options))
        .expect("setup: both sides must succeed");

    let report = preserved_cluster_report(&prepared);

    for handle in [&prepared.baseline, &prepared.candidate] {
        assert!(
            report.contains(&handle.spec.name),
            "missing cluster name in report:\n{report}"
        );
        assert!(
            report.contains(&handle.kubeconfig.display().to_string()),
            "missing kubeconfig path in report:\n{report}"
        );
        let expected_command = format!("kind delete cluster --name {}", handle.spec.name);
        assert!(
            report.contains(&expected_command),
            "missing exact delete command {expected_command:?} in report:\n{report}"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// A failed rollback must never hide the original creation failure.
// ---------------------------------------------------------------------

#[test]
fn rollback_failure_after_a_creation_failure_still_reports_the_original_error() {
    let (runner, options, root) = runner_with(
        "rollback-fails",
        FakeClusterManager::new(CreatePlan::Fail, CreatePlan::Succeed)
            .failing_delete_for(Side::Candidate),
    );
    let lab = minimal_resolved_lab();

    let result = test_runtime().block_on(runner.prepare_clusters(&lab, &options));

    match result {
        Err(RunError::ClusterCreationFailed { failure, rollback }) => {
            assert!(
                matches!(failure, ClusterCreationFailure::Baseline(_)),
                "the original baseline failure must still be reported, got {failure:?}"
            );
            assert_eq!(
                rollback.len(),
                1,
                "the failed rollback delete of the orphaned candidate must be reported too: \
                 {rollback:?}"
            );
        }
        other => panic!("expected Err(ClusterCreationFailed), got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// FakeStackInstaller: a whole-trait `StackInstaller` test double
// (Task 2.6).
// ---------------------------------------------------------------------

/// How [`FakeStackInstaller::install_stack`] behaves for one side.
#[derive(Clone)]
enum StackPlan {
    /// Succeed immediately (no `.await` point ever yields).
    Succeed,
    /// Sleep for the given duration, then succeed. Used to prove the
    /// *other* side's install is never abandoned when this side takes
    /// longer.
    SucceedAfter(Duration),
    /// Fail immediately.
    Fail,
}

/// Any [`StackInstallError`] works as this file's fake failure; `side`
/// only makes the fake, synthetic failure identifiable in assertions.
fn fake_stack_error(side: Side) -> StackInstallError {
    StackInstallError {
        component: Some(format!("fake-{}-component", side.as_str())),
        message: format!("fake {side} stack install failure"),
    }
}

/// A plausible [`SideInstall`] for `side`, with one [`InstalledComponent`]
/// per entry in `components` — enough to prove
/// [`LabRunner::install_stacks`] handed each side its own component
/// list, without needing any real installer behind it.
fn fake_side_install(side: Side, components: &[ResolvedComponent]) -> SideInstall {
    SideInstall {
        side,
        components: components
            .iter()
            .map(|component| InstalledComponent {
                name: component.name.clone(),
                method: "fake".to_owned(),
                resolved_version: component.version.clone(),
                started_at: SystemTime::now(),
                elapsed: Duration::ZERO,
                diagnostics: Vec::new(),
            })
            .collect(),
    }
}

/// A [`StackInstaller`] test double. Never spawns a process or touches a
/// real cluster: `install_stack` is a plain, configurable, in-memory
/// outcome. Records which sides it was actually called for, with which
/// component names and `component_timeout`, in call order, so tests can
/// assert not just the returned `Result` but exactly what
/// [`LabRunner::install_stacks`] handed it.
struct FakeStackInstaller {
    baseline_plan: StackPlan,
    candidate_plan: StackPlan,
    calls: Mutex<Vec<(Side, Vec<String>, Duration)>>,
}

impl FakeStackInstaller {
    fn new(baseline_plan: StackPlan, candidate_plan: StackPlan) -> Self {
        Self {
            baseline_plan,
            candidate_plan,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// The sides `install_stack` was actually invoked for, in call
    /// order.
    fn called_sides(&self) -> Vec<Side> {
        self.calls
            .lock()
            .expect("calls mutex poisoned")
            .iter()
            .map(|(side, _, _)| *side)
            .collect()
    }

    /// The exact `(side, component names, component_timeout)` triples
    /// `install_stack` was actually invoked with, in call order.
    fn calls(&self) -> Vec<(Side, Vec<String>, Duration)> {
        self.calls.lock().expect("calls mutex poisoned").clone()
    }
}

#[async_trait]
impl StackInstaller for FakeStackInstaller {
    async fn install_stack(
        &self,
        cluster: &ClusterHandle,
        components: &[ResolvedComponent],
        component_timeout: Duration,
    ) -> Result<SideInstall, StackInstallError> {
        let side = cluster.spec.side;
        self.calls.lock().expect("calls mutex poisoned").push((
            side,
            components.iter().map(|c| c.name.clone()).collect(),
            component_timeout,
        ));

        let plan = match side {
            Side::Baseline => &self.baseline_plan,
            Side::Candidate => &self.candidate_plan,
        };
        match plan {
            StackPlan::Succeed => Ok(fake_side_install(side, components)),
            StackPlan::SucceedAfter(duration) => {
                tokio::time::sleep(*duration).await;
                Ok(fake_side_install(side, components))
            }
            StackPlan::Fail => Err(fake_stack_error(side)),
        }
    }
}

// ---------------------------------------------------------------------
// install_stacks (Task 2.6): both sides, concurrently.
// ---------------------------------------------------------------------

#[test]
fn install_stacks_installs_each_sides_own_components_concurrently_and_reports_both_sides() {
    let (runner, options, root) = runner_with(
        "install-both",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed),
    );
    let lab = minimal_resolved_lab();
    let runtime = test_runtime();
    let prepared = runtime
        .block_on(runner.prepare_clusters(&lab, &options))
        .expect("setup: both sides must succeed");
    let stack_installer = FakeStackInstaller::new(StackPlan::Succeed, StackPlan::Succeed);

    let installed = runtime
        .block_on(runner.install_stacks(&lab, &prepared, &stack_installer, Duration::from_secs(5)))
        .expect("both sides succeeding must succeed overall");

    assert_eq!(installed.baseline.side, Side::Baseline);
    assert_eq!(installed.candidate.side, Side::Candidate);
    assert_eq!(
        sorted(stack_installer.called_sides()),
        vec![Side::Baseline, Side::Candidate]
    );
    for (side, components, timeout) in stack_installer.calls() {
        let expected_components: Vec<String> = match side {
            Side::Baseline => lab
                .baseline
                .components
                .iter()
                .map(|c| c.name.clone())
                .collect(),
            Side::Candidate => lab
                .candidate
                .components
                .iter()
                .map(|c| c.name.clone())
                .collect(),
        };
        assert_eq!(
            components, expected_components,
            "{side} must be installed with exactly its own side's resolved components"
        );
        assert_eq!(
            timeout,
            Duration::from_secs(5),
            "component_timeout must be passed through unchanged"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn install_stacks_reports_a_baseline_only_failure() {
    let (runner, options, root) = runner_with(
        "install-baseline-fails",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed),
    );
    let lab = minimal_resolved_lab();
    let runtime = test_runtime();
    let prepared = runtime
        .block_on(runner.prepare_clusters(&lab, &options))
        .expect("setup: both sides must succeed");
    let stack_installer = FakeStackInstaller::new(StackPlan::Fail, StackPlan::Succeed);

    let result = runtime.block_on(runner.install_stacks(
        &lab,
        &prepared,
        &stack_installer,
        Duration::from_secs(5),
    ));

    match result {
        Err(StackInstallFailure::Baseline(error)) => {
            assert_eq!(error.component.as_deref(), Some("fake-baseline-component"));
        }
        other => panic!("expected Err(StackInstallFailure::Baseline), got {other:?}"),
    }
    assert_eq!(
        sorted(stack_installer.called_sides()),
        vec![Side::Baseline, Side::Candidate],
        "candidate must still be attempted even though baseline fails"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn install_stacks_reports_a_candidate_only_failure() {
    let (runner, options, root) = runner_with(
        "install-candidate-fails",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed),
    );
    let lab = minimal_resolved_lab();
    let runtime = test_runtime();
    let prepared = runtime
        .block_on(runner.prepare_clusters(&lab, &options))
        .expect("setup: both sides must succeed");
    let stack_installer = FakeStackInstaller::new(StackPlan::Succeed, StackPlan::Fail);

    let result = runtime.block_on(runner.install_stacks(
        &lab,
        &prepared,
        &stack_installer,
        Duration::from_secs(5),
    ));

    match result {
        Err(StackInstallFailure::Candidate(error)) => {
            assert_eq!(error.component.as_deref(), Some("fake-candidate-component"));
        }
        other => panic!("expected Err(StackInstallFailure::Candidate), got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn install_stacks_reports_both_sides_failing() {
    let (runner, options, root) = runner_with(
        "install-both-fail",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed),
    );
    let lab = minimal_resolved_lab();
    let runtime = test_runtime();
    let prepared = runtime
        .block_on(runner.prepare_clusters(&lab, &options))
        .expect("setup: both sides must succeed");
    let stack_installer = FakeStackInstaller::new(StackPlan::Fail, StackPlan::Fail);

    let result = runtime.block_on(runner.install_stacks(
        &lab,
        &prepared,
        &stack_installer,
        Duration::from_secs(5),
    ));

    assert!(
        matches!(result, Err(StackInstallFailure::Both { .. })),
        "expected Err(StackInstallFailure::Both), got {result:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn install_stacks_does_not_abandon_a_still_installing_side_when_the_other_fails_immediately() {
    // Mirrors `candidate_creation_is_not_abandoned_when_baseline_fails_immediately`
    // above, one lifecycle stage later: if `install_stacks` used
    // `try_join!` instead of `tokio::join!`, this test would fail —
    // `try_join!` returns the instant baseline's future resolves to
    // `Err`, dropping candidate's still-sleeping future before it is
    // ever recorded as called.
    let (runner, options, root) = runner_with(
        "install-concurrency",
        FakeClusterManager::new(CreatePlan::Succeed, CreatePlan::Succeed),
    );
    let lab = minimal_resolved_lab();
    let runtime = test_runtime();
    let prepared = runtime
        .block_on(runner.prepare_clusters(&lab, &options))
        .expect("setup: both sides must succeed");
    let stack_installer = FakeStackInstaller::new(
        StackPlan::Fail,
        StackPlan::SucceedAfter(Duration::from_millis(75)),
    );

    let result = runtime.block_on(runner.install_stacks(
        &lab,
        &prepared,
        &stack_installer,
        Duration::from_secs(5),
    ));

    assert!(
        result.is_err(),
        "baseline failed, so install_stacks must fail overall"
    );
    assert_eq!(
        sorted(stack_installer.called_sides()),
        vec![Side::Baseline, Side::Candidate],
        "candidate's slower install must have been awaited to completion (tokio::join!, not \
         try_join!), never abandoned the instant baseline failed"
    );

    let _ = std::fs::remove_dir_all(&root);
}
