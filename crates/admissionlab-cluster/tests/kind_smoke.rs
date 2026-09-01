//! Real, non-mocked two-cluster integration smoke test (Task 1.9).
//!
//! Every other test in this crate (`tests/lifecycle_unit.rs`,
//! `tests/kind_config.rs`, `tests/version.rs`) drives
//! [`KindClusterManager`] through a fake [`ProcessRunner`] that never
//! spawns a real process. This file is the one place in the whole
//! workspace that actually creates real `kind` clusters through real
//! Docker, proving the abstractions those fake-backed tests exercise in
//! isolation also compose correctly against the real thing. It is
//! `#[ignore]`d so `cargo test --workspace` never requires Docker/kind;
//! CI's separate integration workflow (`.github/workflows/integration.yml`)
//! runs it explicitly with `-- --ignored`.
//!
//! # Cluster identity: `kube-system`'s namespace UID
//!
//! "Genuinely isolated" needs a proxy for cluster identity distinct from
//! the kubeconfig file path (which is trivially distinct by construction
//! -- see `kubeconfig.rs`'s per-side namespacing -- and would prove
//! nothing about the *clusters themselves*, only about this project's
//! own file layout). Vanilla Kubernetes has no single first-class
//! "cluster ID" object, but every cluster's control plane bootstraps a
//! `kube-system` `Namespace` object with a fresh, server-assigned
//! `metadata.uid` the instant its API server and controller-manager come
//! up -- before any user workload exists. Two separate `kind` nodes never
//! share an etcd store, so two separate bootstraps can never produce the
//! same UID by construction (a UUID collision aside, which is as
//! vanishingly unlikely here as anywhere else this codebase already
//! trusts UUIDs, for example `RunId::generate`). That makes it a stable,
//! always-present, zero-configuration proxy for "these are two distinct
//! API server backing stores" -- simpler than alternatives considered
//! (for example diffing the generated CA certificate, which is
//! reasonable but requires parsing a kubeconfig's embedded PEM rather
//! than one `kubectl get` call).
//!
//! Read via `kubectl --kubeconfig <path> get namespace kube-system -o
//! jsonpath='{.metadata.uid}'`, run through this project's own
//! [`ProcessRunner`] (never a direct `std::process`/shell call), once per
//! cluster's own isolated kubeconfig.
//!
//! # Cleanup guard and its documented limitation
//!
//! [`ClusterGuard`] holds one created cluster's [`ClusterHandle`] and is
//! the *only* thing this file trusts to avoid leaking a real cluster
//! (PRODUCT.md §33: "no leaked cluster after normal failure paths").
//! Its `cleanup` method is the primary mechanism and is always called
//! explicitly, on every path, by [`run_isolation_check`] -- including
//! when the isolation assertions themselves fail. That is only possible
//! because [`run_isolation_check`] reports failure through `Result`
//! rather than an `assert!`/`panic!`: a panicking assertion would unwind
//! the stack *before* an explicit `cleanup().await` placed after it ever
//! ran, and `Drop` cannot safely perform `.await`ed async work (blocking
//! a runtime from inside `drop` risks a panic or a deadlock, and `drop`
//! itself cannot be `async`). [`ClusterGuard`]'s own `Drop` implementation
//! therefore does *not* attempt deletion at all -- it only emits a
//! warning, containing the exact `kind delete cluster --name <name>`
//! command, if a handle still survives when it drops. That is a
//! deliberately narrower guarantee than automatic cleanup: a genuine
//! panic *inside* [`run_isolation_check`] before its own explicit cleanup
//! runs (as opposed to the controlled, `Result`-based failures this test
//! is written to produce instead) would still only be caught by this
//! warning, not by an automatic delete -- which is exactly why a leaked
//! cluster always remains recoverable by hand, never silently invisible.
//!
//! Note the same duality [`RollbackOutcome`](admissionlab_core::RollbackOutcome)
//! already encodes for `KindClusterManager::create`'s own rollback: a
//! cleanup *attempt* is not the same as a cleanup *success*.
//! [`ClusterGuard::cleanup`] only clears its tracked handle (silencing
//! `Drop`) when the delete genuinely succeeds; a failed delete leaves the
//! handle in place specifically so `Drop`'s warning still fires with the
//! manual recovery command.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use admissionlab_cluster::{KindClusterManager, cluster_name, load_matrix, resolve_node_image};
use admissionlab_core::{
    ArtifactStore, ClusterError, ClusterHandle, ClusterManager, ClusterSpec, CommandSpec,
    ProcessRunner, RunId, RunPaths, Side, TokioProcessRunner,
};

/// The Kubernetes version this smoke test provisions both clusters at:
/// Tier 1's primary supported version per `compatibility/kubernetes.yaml`
/// (see that file's own comments), resolved through
/// [`resolve_node_image`] -- never a second hardcoded copy of its pinned
/// digest -- so this test tracks the checked-in matrix rather than
/// silently drifting from whatever it actually resolves to.
const KUBERNETES_VERSION: &str = "1.36.4";

/// Timeout for one `kubectl get namespace kube-system` read against an
/// already-healthy, already-created cluster: a single, offline-ish,
/// already-warm API call, not a provisioning step -- generous for a slow
/// CI runner without letting a genuine hang stall the whole test.
const KUBECTL_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------
// Cleanup guard (Task 1.9 brief Step 2).
// ---------------------------------------------------------------------

/// Holds one created cluster's [`ClusterHandle`] until [`Self::cleanup`]
/// is called explicitly. See this module's documentation for why `Drop`
/// may only warn, never delete.
struct ClusterGuard {
    handle: Option<ClusterHandle>,
}

impl ClusterGuard {
    fn new(handle: ClusterHandle) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    /// This guard's tracked handle, for callers that need to inspect the
    /// cluster (its kubeconfig, its name) without yet cleaning it up.
    ///
    /// # Panics
    ///
    /// Panics if called after [`Self::cleanup`] has already taken the
    /// handle -- a programmer error in this test file, never a condition
    /// a real cluster's behavior could trigger.
    fn handle(&self) -> &ClusterHandle {
        self.handle
            .as_ref()
            .expect("ClusterGuard::handle called after cleanup")
    }

    /// Deletes the guarded cluster through `manager`, the project's own
    /// [`ClusterManager`] -- never a direct shell-out. On success, clears
    /// the tracked handle so `Drop` finds nothing to warn about. On
    /// failure, the handle is deliberately left in place so `Drop`'s
    /// warning still fires with the exact manual recovery command: a
    /// failed cleanup *attempt* must never look the same as a successful
    /// one.
    async fn cleanup(mut self, manager: &KindClusterManager) -> Result<(), ClusterError> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        match manager.delete(&handle).await {
            Ok(()) => Ok(()),
            Err(error) => {
                self.handle = Some(handle);
                Err(error)
            }
        }
    }
}

impl Drop for ClusterGuard {
    /// Never performs async cleanup (see this module's documentation):
    /// only a synchronous, best-effort warning naming the exact command a
    /// user can paste to delete a cluster this guard never confirmed was
    /// deleted.
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            let name = &handle.spec.name;
            eprintln!(
                "warning: cluster {name:?} was not confirmed deleted by this test; if it \
                 still exists, delete it manually with: kind delete cluster --name {name}"
            );
        }
    }
}

// ---------------------------------------------------------------------
// The test itself.
// ---------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Docker and kind"]
async fn baseline_and_candidate_are_isolated() {
    let outcome = run_isolation_check().await;
    outcome.expect("baseline/candidate isolation check");
}

/// Creates baseline and candidate clusters concurrently, checks that they
/// are genuinely isolated, and -- regardless of what that check finds --
/// always calls [`ClusterGuard::cleanup`] on both before returning. See
/// this module's documentation for why reporting failure through `Result`
/// rather than panicking is what makes that "always" true.
async fn run_isolation_check() -> Result<(), String> {
    let manager = KindClusterManager::new(Arc::new(TokioProcessRunner::new()));

    let root = unique_root();
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();
    let paths = store
        .create_run(&run_id)
        .await
        .map_err(|error| format!("failed to prepare a run workspace: {error}"))?;

    let matrix = load_matrix()
        .map_err(|error| format!("failed to load the compatibility matrix: {error}"))?;
    let resolved = resolve_node_image(KUBERNETES_VERSION, &matrix)
        .map_err(|error| format!("failed to resolve a node image: {error}"))?;

    let baseline_spec = build_spec(
        Side::Baseline,
        &run_id,
        &resolved.version,
        &resolved.pinned_image,
    )?;
    let candidate_spec = build_spec(
        Side::Candidate,
        &run_id,
        &resolved.version,
        &resolved.pinned_image,
    )?;

    // Created concurrently (Task 1.9 brief Step 1): both `kind create
    // cluster` child processes run at once, since baseline and candidate
    // share no mutable state (PRODUCT.md §10.2).
    let (baseline_result, candidate_result) = tokio::join!(
        create_and_guard(&manager, &paths, baseline_spec),
        create_and_guard(&manager, &paths, candidate_spec),
    );

    let check_result = match (&baseline_result, &candidate_result) {
        (Ok(baseline_guard), Ok(candidate_guard)) => {
            verify_isolation(baseline_guard.handle(), candidate_guard.handle()).await
        }
        (Err(baseline_error), Ok(_)) => Err(format!(
            "baseline cluster failed to create: {baseline_error}"
        )),
        (Ok(_), Err(candidate_error)) => Err(format!(
            "candidate cluster failed to create: {candidate_error}"
        )),
        (Err(baseline_error), Err(candidate_error)) => Err(format!(
            "both clusters failed to create: baseline: {baseline_error}; candidate: {candidate_error}"
        )),
    };

    // Whatever the isolation check found, explicitly clean up whichever
    // cluster(s) actually exist -- this is the "finally" for the whole
    // check (Task 1.9 brief Step 2: explicit orchestration, not `Drop`,
    // is what must call cleanup).
    let mut problems: Vec<String> = check_result.err().into_iter().collect();

    if let Ok(guard) = baseline_result
        && let Err(error) = guard.cleanup(&manager).await
    {
        problems.push(format!("failed to delete the baseline cluster: {error}"));
    }
    if let Ok(guard) = candidate_result
        && let Err(error) = guard.cleanup(&manager).await
    {
        problems.push(format!("failed to delete the candidate cluster: {error}"));
    }

    // Best-effort only: the run workspace under the OS temp directory is
    // disposable scratch space, not evidence this test needs to keep or
    // a cluster this project must guarantee gets deleted -- unlike the
    // two cleanup calls above, a failure here is not itself a test
    // failure.
    let _ = tokio::fs::remove_dir_all(&root).await;

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// Creates one cluster and immediately wraps its handle in a
/// [`ClusterGuard`] -- there is no window in which a created cluster
/// exists without a guard tracking it, even though [`create_and_guard`]
/// itself runs concurrently with its counterpart for the other side.
async fn create_and_guard(
    manager: &KindClusterManager,
    paths: &RunPaths,
    spec: ClusterSpec,
) -> Result<ClusterGuard, ClusterError> {
    let handle = manager.create(&spec, paths).await?;
    Ok(ClusterGuard::new(handle))
}

fn build_spec(
    side: Side,
    run_id: &RunId,
    kubernetes_version: &str,
    node_image: &str,
) -> Result<ClusterSpec, String> {
    let name = cluster_name(side, run_id)
        .map_err(|error| format!("failed to build a cluster name for {side}: {error}"))?;
    Ok(ClusterSpec {
        side,
        name,
        kubernetes_version: kubernetes_version.to_owned(),
        node_image: node_image.to_owned(),
        images: Vec::new(),
    })
}

/// Checks that `baseline` and `candidate` are genuinely isolated: distinct
/// kubeconfig files (by path and by content) and distinct cluster
/// identities (distinct `kube-system` namespace UIDs -- see this module's
/// documentation for why that proxy was chosen).
async fn verify_isolation(
    baseline: &ClusterHandle,
    candidate: &ClusterHandle,
) -> Result<(), String> {
    if baseline.kubeconfig == candidate.kubeconfig {
        return Err(format!(
            "baseline and candidate must not share a kubeconfig path, both got {}",
            baseline.kubeconfig.display()
        ));
    }

    let baseline_kubeconfig_bytes = tokio::fs::read(&baseline.kubeconfig)
        .await
        .map_err(|error| format!("failed to read baseline kubeconfig: {error}"))?;
    let candidate_kubeconfig_bytes = tokio::fs::read(&candidate.kubeconfig)
        .await
        .map_err(|error| format!("failed to read candidate kubeconfig: {error}"))?;
    if baseline_kubeconfig_bytes == candidate_kubeconfig_bytes {
        return Err(
            "baseline and candidate kubeconfigs have identical content, which should be \
             impossible for two independently created clusters (distinct endpoints/certs)"
                .to_owned(),
        );
    }

    let runner = TokioProcessRunner::new();
    let baseline_uid = kube_system_uid(&runner, &baseline.kubeconfig).await?;
    let candidate_uid = kube_system_uid(&runner, &candidate.kubeconfig).await?;

    if baseline_uid == candidate_uid {
        return Err(format!(
            "baseline and candidate unexpectedly report the same kube-system namespace UID \
             ({baseline_uid}); clusters are not isolated"
        ));
    }

    Ok(())
}

/// Reads the `kube-system` namespace's `metadata.uid` from the cluster
/// named by `kubeconfig`, through `runner` -- this project's own
/// [`ProcessRunner`], never a direct shell call.
async fn kube_system_uid(runner: &dyn ProcessRunner, kubeconfig: &Path) -> Result<String, String> {
    let spec = CommandSpec {
        program: "kubectl".into(),
        args: vec![
            "--kubeconfig".into(),
            kubeconfig.as_os_str().to_owned(),
            "get".into(),
            "namespace".into(),
            "kube-system".into(),
            "-o".into(),
            "jsonpath={.metadata.uid}".into(),
        ],
        cwd: None,
        env: BTreeMap::new(),
        sensitive_env_keys: BTreeSet::new(),
        timeout: KUBECTL_TIMEOUT,
        spill_dir: None,
    };

    let result = runner
        .run(spec)
        .await
        .map_err(|error| format!("kubectl could not be run: {error}"))?;
    if !result.status.success() {
        return Err(format!(
            "kubectl get namespace kube-system exited with {}: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }

    let uid = String::from_utf8_lossy(&result.stdout).trim().to_owned();
    if uid.is_empty() {
        return Err("kubectl returned an empty kube-system namespace UID".to_owned());
    }
    Ok(uid)
}

/// A fresh, guaranteed-unique scratch directory under the OS temp dir.
/// Mirrors `tests/lifecycle_unit.rs`'s own `unique_root` helper.
fn unique_root() -> PathBuf {
    let unique = RunId::generate();
    std::env::temp_dir().join(format!("admissionlab-kind-smoke-{}", unique.as_str()))
}
