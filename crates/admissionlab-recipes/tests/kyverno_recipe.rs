//! Real, non-mocked Kyverno certification test (Task 2.8). `#[ignore]`d —
//! needs Docker and `kind` — in the same established style
//! `admissionlab-cluster/tests/kind_smoke.rs` (Task 1.9) and
//! `admissionlab-test-webhook/tests/kind_smoke.rs` (Task 2.7) use, so
//! `cargo test --workspace` never requires either.
//!
//! End to end: creates a real `kind` cluster, loads the now-built-in
//! `kyverno` recipe (`recipes/kyverno/recipe.yaml`, wired into
//! [`admissionlab_recipes::load_builtin_recipes`]'s `BUILTIN_RECIPES`)
//! through the real Task 2.6 install-and-wait pipeline
//! (`admissionlab_installer::install_stack`), then applies every fixture
//! policy in `fixtures/kyverno/smoke/` and proves — against the live
//! cluster, not by inspecting YAML — that the validating policy actually
//! denies a violating Pod (attributably to that policy) while admitting
//! a conforming one, and that the mutating policy actually adds its
//! label to a created Pod, not merely that applying it did not error.
//!
//! # The Kubernetes version is derived, never hardcoded
//!
//! `compatibility/recipes.yaml` had no production consumer before this
//! task — only a test-local struct parsed it. This test is that
//! consumer: [`kyverno_certified_kubernetes_version`] reads the
//! `kyverno` entry's `kubernetes.certified` list through
//! [`admissionlab_recipes::load_recipe_compatibility`] (the newly
//! promoted `pub` API — see `crates/admissionlab-recipes/src/compat.rs`)
//! and provisions the `kind` cluster at whatever patch version that list
//! names, today `"1.35.8"` — deliberately narrower than Admission Lab's
//! Tier-1 primary (`1.36.4`), because Kyverno's own docs for this
//! chart/appVersion line state "Kubernetes Versions Supported:
//! v1.33 - v1.35" (see `recipes/kyverno/README.md`). An edit to
//! `certified` in that file changes what this test installs against on
//! its next run; a hardcoded copy would not.
//!
//! # THE RACE: webhook-configuration existence is not policy enforcement
//!
//! `recipe.yaml`'s own `webhookConfigurationPresent` readiness checks
//! (which [`admissionlab_installer::install_stack`] already waits on as
//! part of installing this recipe) confirm only that
//! `kyverno-resource-validating-webhook-cfg`/
//! `kyverno-resource-mutating-webhook-cfg` **exist**. Kyverno creates
//! both almost immediately after `kyverno-admission-controller` starts,
//! but each starts with an empty `webhooks: []` list — a per-policy rule
//! is appended only after that policy is applied and the controller's
//! own workqueue processes the event. Verified directly against a real
//! cluster while writing this test: immediately after
//! `kyverno-admission-controller` reports `Available`, both webhook
//! configuration objects already exist but their `webhooks` field is
//! entirely absent (not even an empty array).
//!
//! A version of this test that applied a fixture `ClusterPolicy` and
//! immediately sent a resource expecting denial — trusting
//! `webhookConfigurationPresent` as "the policy is live" — would be
//! flaky (sometimes the controller's workqueue wins the race) or
//! silently green (if the assertion were weak enough not to notice the
//! violating resource was admitted instead of denied). **Every scenario
//! below applies its `ClusterPolicy` first, then explicitly waits for
//! that specific policy's own `status.conditions[type=Ready]` to become
//! `"True"`** (via [`admissionlab_installer::KubeReadinessProbe`] and
//! [`admissionlab_spec::ReadinessCheck::CustomResourceCondition`] against
//! `kyverno.io/v1`/`ClusterPolicy`) **before sending any resource it
//! expects to be denied or mutated.** This is a real signal — Kyverno
//! populates it once a policy's rules are actually folded into the live
//! webhook configuration — not a sleep. Measured empirically while
//! writing this test: well under two seconds from apply to `Ready`, so
//! [`POLICY_READY_TIMEOUT`] below has wide headroom.
//!
//! # `failureAction`, not `validationFailureAction`, at rule level
//!
//! The spec-level `spec.validationFailureAction` is deprecated, defaults
//! to `Audit`, and never blocks anything by itself; its own CRD
//! description names the wrong field for its replacement (it says "use
//! `validationFailureAction` under the validate rule instead", but the
//! actual rule-level field — confirmed directly against the real
//! `clusterpolicies.kyverno.io` v1 CRD schema pulled from chart 3.9.0 —
//! is named `failureAction`). `fixtures/kyverno/smoke/10-validate-policy.yaml`
//! sets `spec.rules[].validate.failureAction: Enforce` at exactly that
//! path. [`run_validate_scenario`] proves this actually enforces by
//! asserting the denial happens **and** is attributable to this specific
//! policy by name (`stderr` naming
//! `admissionlab-require-team-label`) — a test that only checked "the
//! apply exited non-zero" could not distinguish this policy's rejection
//! from an unrelated failure. Empirically confirmed while writing this
//! test (not merely reasoned about): patching a live copy of this same
//! policy's `failureAction` from `Enforce` to `Audit` and resubmitting
//! the identical violating Pod caused the API server to admit it — proof
//! this test's assertion genuinely distinguishes Enforce from Audit,
//! rather than passing regardless.
//!
//! # Cleanup discipline
//!
//! [`ClusterGuard`] is copied verbatim from
//! `admissionlab-test-webhook/tests/kind_smoke.rs` (itself mirroring
//! `admissionlab-cluster/tests/kind_smoke.rs`) — see that pattern's own
//! documentation for why `Drop` only warns, never deletes. This test
//! uses exactly **one** [`admissionlab_core::RunPaths`] root for
//! everything — the cluster, the Helm installer's isolated state
//! directory, and this test's own `kubectl` cache directory — unlike
//! `admissionlab-test-webhook/tests/kind_smoke.rs`, which leaks two
//! separate scratch directories by not applying the same discipline
//! outside its own `ClusterGuard`.
//!
//! That one root is owned by [`ScratchRoot`], a small guard whose
//! `Drop` removes it synchronously (`std::fs::remove_dir_all`, not
//! `tokio::fs`): no subprocess and no `.await` are involved in deleting
//! a local directory tree, so — unlike [`ClusterGuard`] — there is no
//! async-in-`Drop` hazard here to design around.
//!
//! **Correction, found in review:** an earlier version of this file
//! instead called `tokio::fs::remove_dir_all` exactly once, explicitly,
//! at the very tail of [`run_smoke_test`], after
//! [`ClusterGuard::cleanup`] had already run — genuinely unconditional
//! for any failure *inside* [`install_and_verify`] (every one of those
//! returns through that same tail), but **not** for the five fallible
//! steps between creating the root directory and constructing
//! [`ClusterGuard`] itself (`kyverno_certified_kubernetes_version`,
//! `load_matrix`, `resolve_node_image`, `cluster_name`,
//! `manager.create`): each is a `?` that returns before `guard` exists,
//! bypassing that tail entirely and leaking `root`. `manager.create`
//! failing — a `kind create cluster` failure on a loaded runner — is
//! the operationally realistic way to trigger this, and is exactly the
//! situation in which a test gets re-run repeatedly, compounding the
//! leak each time. [`ScratchRoot`]'s `Drop` covers every one of those
//! paths too, because the guard is constructed before any of them run —
//! there is no window in this file, of any length, in which `root`
//! exists but nothing owns removing it.
//!
//! This test also never touches the user's real `~/.kube/`,
//! `~/.config/helm/`, or `~/.cache/`: every `helm`/`kubectl` invocation
//! below is either routed through [`admissionlab_installer::HelmInstaller`]
//! (already isolated — see that module's own documentation) or built by
//! this file's own `kubectl_command`, which always passes an explicit
//! `--kubeconfig`/`--cache-dir` and never relies on `$KUBECONFIG`/
//! `$KUBECACHEDIR`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use admissionlab_cluster::{KindClusterManager, cluster_name};
use admissionlab_core::{
    ArtifactStore, ClusterError, ClusterHandle, ClusterManager, ClusterSpec, CommandResult,
    CommandSpec, ProcessRunner, RunId, RunPaths, Side, TokioProcessRunner,
};
use admissionlab_installer::{HelmInstaller, KubeReadinessProbe, ReadinessProbe, install_stack};
use admissionlab_recipes::{
    Capability, ReadinessCheck, load_builtin_recipes, load_recipe_compatibility,
};
use admissionlab_spec::ResolvedComponent;

/// Bounds one `install_stack` call for the `kyverno` component alone:
/// the `helm upgrade --install` submission plus every one of
/// `recipe.yaml`'s readiness checks (`DeploymentAvailable` on
/// `kyverno-admission-controller`, `WebhookConfigurationPresent` on
/// both resource webhook configurations). Measured empirically while
/// writing this test on an already-warm `kind` node: `helm upgrade
/// --install` itself returns in under ten seconds (this recipe's schema
/// has no `setValues`, so all four chart Deployments install at their
/// defaults — see `recipes/kyverno/README.md` — not only the one this
/// recipe gates readiness on), and `kyverno-admission-controller`
/// reaches `Available` roughly a minute after that. 300 seconds is
/// generous headroom over that measured ~75-second total for a
/// slower/loaded CI runner or a cold image pull, without letting a
/// genuine hang stall the test indefinitely.
const COMPONENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Bounds waiting for one fixture `ClusterPolicy`'s own
/// `status.conditions[type=Ready]` to become `"True"` after it is
/// applied. Measured empirically at well under two seconds; Kyverno's
/// own admission-controller Lease-based health watchdog (see
/// `.superpowers/sdd/ROADMAP/research-kyverno.md` §5.3) has a
/// documented 100-second idle deadline as its own worst case, so this is
/// sized comfortably above that rather than the measured common case.
const POLICY_READY_TIMEOUT: Duration = Duration::from_secs(120);

/// Bounds one `kubectl` invocation against an already-healthy,
/// already-warm cluster (an apply of a handful of small objects, or a
/// single `get`) — not a provisioning step. Mirrors
/// `admissionlab-cluster/tests/kind_smoke.rs`'s own `KUBECTL_TIMEOUT`.
const KUBECTL_TIMEOUT: Duration = Duration::from_secs(30);

/// This `kubectl`'s own isolated schema/discovery cache directory name,
/// under this test's single shared `RunPaths::logs()` directory. Mirrors
/// `admissionlab_installer::manifests`'s own `kubectl_command`
/// discipline: an explicit `--cache-dir`, never `$KUBECACHEDIR` or the
/// operator's real `~/.kube/cache`.
const KUBECTL_CACHE_SUBDIR: &str = "kyverno-recipe-test-kubectl-cache";

// ---------------------------------------------------------------------
// Scratch root guard. See this file's own module documentation
// ("Cleanup discipline") for the leak this replaced and why a
// synchronous `Drop` is safe here (unlike `ClusterGuard`'s, below).
// ---------------------------------------------------------------------

/// Owns this test's single scratch root ([`unique_root`]) and removes
/// it, best-effort, the moment it goes out of scope -- covering every
/// exit path from [`run_smoke_test`], including a `?` return before
/// [`ClusterGuard`] even exists yet. Construct this immediately after
/// computing the root path and before any fallible step, and hold it
/// for the rest of the function; nothing else about its placement
/// matters, since Rust drops every live local variable when a function
/// returns through any path (an early `?`, a panic unwind, or normal
/// completion) -- not only the one at the bottom of the source text.
struct ScratchRoot(PathBuf);

impl Drop for ScratchRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------
// Cleanup guard -- copied verbatim from
// `admissionlab-test-webhook/tests/kind_smoke.rs` (itself mirroring
// `admissionlab-cluster/tests/kind_smoke.rs`). See this file's own
// module documentation ("Cleanup discipline") for why `Drop` only
// warns, and why this test does not repeat that file's two leaked
// scratch directories.
// ---------------------------------------------------------------------

struct ClusterGuard {
    handle: Option<ClusterHandle>,
}

impl ClusterGuard {
    fn new(handle: ClusterHandle) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    fn handle(&self) -> &ClusterHandle {
        self.handle
            .as_ref()
            .expect("ClusterGuard::handle called after cleanup")
    }

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
async fn kyverno_recipe_installs_and_enforces_fixture_policies() {
    let outcome = run_smoke_test().await;
    outcome.expect("kyverno recipe certification test");
}

/// Creates one cluster, installs `kyverno`, runs both fixture scenarios,
/// and -- regardless of what any of that finds, including a failure
/// before a cluster is even created -- always deletes the cluster (if
/// one exists) and removes this test's single scratch root before
/// returning. See this file's own module documentation ("Cleanup
/// discipline").
async fn run_smoke_test() -> Result<(), String> {
    let manager = KindClusterManager::new(Arc::new(TokioProcessRunner::new()));
    let runner = TokioProcessRunner::new();

    let root = unique_root();
    // Bound immediately, before any fallible step below -- see
    // `ScratchRoot`'s own documentation for why its `Drop` alone is
    // both necessary and sufficient to guarantee `root` never leaks,
    // regardless of which `?` (if any) this function returns through.
    let _scratch_root_guard = ScratchRoot(root.clone());
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();
    let paths = store
        .create_run(&run_id)
        .await
        .map_err(|error| format!("failed to prepare a run workspace: {error}"))?;

    let kubernetes_version = kyverno_certified_kubernetes_version()?;
    let matrix = admissionlab_cluster::load_matrix()
        .map_err(|error| format!("failed to load the Kubernetes compatibility matrix: {error}"))?;
    let resolved = admissionlab_cluster::resolve_node_image(&kubernetes_version, &matrix).map_err(
        |error| format!("failed to resolve a node image for {kubernetes_version:?}: {error}"),
    )?;
    let name = cluster_name(Side::Baseline, &run_id)
        .map_err(|error| format!("failed to build a cluster name: {error}"))?;
    let spec = ClusterSpec {
        side: Side::Baseline,
        name,
        kubernetes_version: resolved.version.clone(),
        node_image: resolved.pinned_image.clone(),
    };

    let handle = manager
        .create(&spec, &paths)
        .await
        .map_err(|error| format!("failed to create cluster: {error}"))?;
    let guard = ClusterGuard::new(handle);

    let check_result = install_and_verify(&runner, guard.handle(), &paths).await;

    let mut problems: Vec<String> = check_result.err().into_iter().collect();
    if let Err(error) = guard.cleanup(&manager).await {
        problems.push(format!("failed to delete the cluster: {error}"));
    }
    // No explicit removal of `root` here: `_scratch_root_guard`
    // (bound above, before any fallible step) removes it on `Drop`,
    // which fires here regardless of whether this is a normal return
    // or (with `root` never having reached this point at all) an
    // earlier `?`.

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// Loads the built-in `kyverno` recipe, installs it through the real
/// Task 2.6 pipeline, and runs both fixture scenarios against it.
async fn install_and_verify(
    runner: &dyn ProcessRunner,
    cluster: &ClusterHandle,
    paths: &RunPaths,
) -> Result<(), String> {
    let recipe = load_builtin_recipes()
        .map_err(|error| format!("failed to load built-in recipes: {error}"))?
        .into_iter()
        .find(|recipe| recipe.name == "kyverno")
        .ok_or_else(|| {
            "no \"kyverno\" recipe among load_builtin_recipes() -- is it wired into \
             BUILTIN_RECIPES?"
                .to_string()
        })?;

    // Cheap, meaningful checks on the recipe's own resolved metadata,
    // independent of installing anything -- catches a hand-edit to
    // recipe.yaml drifting from what this test (and
    // recipes/kyverno/README.md) documents, before ever touching a
    // cluster.
    if recipe.version != "3.9.0" {
        return Err(format!(
            "expected the kyverno recipe to pin chart version 3.9.0, got {:?}",
            recipe.version
        ));
    }
    if !recipe.capabilities.contains(&Capability::Admission) {
        return Err("expected the kyverno recipe to declare the admission capability".to_string());
    }
    if recipe.readiness.len() != 3 {
        return Err(format!(
            "expected exactly 3 readiness checks (1 deploymentAvailable + 2 \
             webhookConfigurationPresent), got {}: {:?}",
            recipe.readiness.len(),
            recipe.readiness
        ));
    }

    // Cross-checks the two independently-maintained files agree: a
    // chart-version bump in recipe.yaml with no matching update to
    // compatibility/recipes.yaml would otherwise drift silently.
    let compat = load_recipe_compatibility()
        .map_err(|error| format!("failed to load compatibility/recipes.yaml: {error}"))?;
    let compat_entry = compat
        .entry("kyverno")
        .ok_or_else(|| "compatibility/recipes.yaml has no \"kyverno\" entry".to_string())?;
    if compat_entry.version != recipe.version {
        return Err(format!(
            "recipes/kyverno/recipe.yaml pins version {:?} but compatibility/recipes.yaml's \
             kyverno entry pins {:?} -- these two checked-in files must agree",
            recipe.version, compat_entry.version
        ));
    }

    let component = ResolvedComponent {
        name: recipe.name,
        version: recipe.version,
        install: recipe.install,
        readiness: recipe.readiness,
        recipe_normalize_rules: recipe.normalize_rules,
        capabilities: recipe.capabilities,
    };

    let helm_installer = HelmInstaller::new(Arc::new(TokioProcessRunner::new()), paths);
    let readiness_probe = KubeReadinessProbe::new();

    let installed = install_stack(
        cluster,
        std::slice::from_ref(&component),
        &helm_installer,
        &readiness_probe,
        COMPONENT_TIMEOUT,
    )
    .await
    .map_err(|error| format!("install_stack failed: {error}"))?;

    if installed.components.len() != 1
        || installed.components[0].component != component.name
        || installed.components[0].method != "helm"
    {
        return Err(format!("unexpected InstalledStack contents: {installed:?}"));
    }

    let kubectl_cache_dir = paths.logs().join(KUBECTL_CACHE_SUBDIR);
    let fixtures = fixtures_dir();

    kubectl_apply_expect_success(
        runner,
        &cluster.kubeconfig,
        &kubectl_cache_dir,
        &fixtures.join("00-namespaces.yaml"),
    )
    .await?;

    run_validate_scenario(runner, cluster, &kubectl_cache_dir, &fixtures).await?;
    run_mutate_scenario(runner, cluster, &kubectl_cache_dir, &fixtures).await?;

    Ok(())
}

/// The validating half of the fixture pack
/// (`fixtures/kyverno/smoke/10-validate-policy.yaml` through
/// `12-validate-denied-pod.yaml`): applies the policy, waits for its
/// `Ready` condition (see this file's own module documentation for why
/// that wait is not optional), then proves both directions -- a
/// conforming Pod is admitted, a violating one is denied attributably to
/// this policy by name.
async fn run_validate_scenario(
    runner: &dyn ProcessRunner,
    cluster: &ClusterHandle,
    kubectl_cache_dir: &Path,
    fixtures: &Path,
) -> Result<(), String> {
    const POLICY_NAME: &str = "admissionlab-require-team-label";

    kubectl_apply_expect_success(
        runner,
        &cluster.kubeconfig,
        kubectl_cache_dir,
        &fixtures.join("10-validate-policy.yaml"),
    )
    .await?;
    wait_for_cluster_policy_ready(cluster, POLICY_NAME).await?;

    kubectl_apply_expect_success(
        runner,
        &cluster.kubeconfig,
        kubectl_cache_dir,
        &fixtures.join("11-validate-allowed-pod.yaml"),
    )
    .await
    .map_err(|error| {
        format!(
            "the conforming Pod (carries the required label) was unexpectedly rejected -- a \
             policy that denies every Pod regardless of its labels would also make the \
             violating-Pod check below pass, for the wrong reason: {error}"
        )
    })?;

    let stderr = kubectl_apply_expect_denied(
        runner,
        &cluster.kubeconfig,
        kubectl_cache_dir,
        &fixtures.join("12-validate-denied-pod.yaml"),
    )
    .await
    .map_err(|error| {
        format!(
            "the violating Pod (no team label) was unexpectedly admitted -- \
         either 10-validate-policy.yaml's failureAction is not Enforce, or the policy's rule is \
         not yet live in the webhook configuration: {error}"
        )
    })?;
    if !stderr.contains(POLICY_NAME) {
        return Err(format!(
            "the violating Pod was rejected, but the rejection is not attributable to \
             {POLICY_NAME:?} by name -- stderr was:\n{stderr}"
        ));
    }

    Ok(())
}

/// The mutating half of the fixture pack
/// (`fixtures/kyverno/smoke/20-mutate-policy.yaml`/
/// `21-mutate-input-pod.yaml`): applies the policy, waits for its
/// `Ready` condition, applies the unlabeled input Pod, then reads the
/// *created* object back and asserts the label was actually added --
/// not merely that the apply succeeded, which a no-op policy would also
/// satisfy.
async fn run_mutate_scenario(
    runner: &dyn ProcessRunner,
    cluster: &ClusterHandle,
    kubectl_cache_dir: &Path,
    fixtures: &Path,
) -> Result<(), String> {
    const POLICY_NAME: &str = "admissionlab-add-managed-by-label";
    const NAMESPACE: &str = "admissionlab-kyverno-smoke-mutate";
    const POD_NAME: &str = "admissionlab-smoke-mutate-input";

    kubectl_apply_expect_success(
        runner,
        &cluster.kubeconfig,
        kubectl_cache_dir,
        &fixtures.join("20-mutate-policy.yaml"),
    )
    .await?;
    wait_for_cluster_policy_ready(cluster, POLICY_NAME).await?;

    kubectl_apply_expect_success(
        runner,
        &cluster.kubeconfig,
        kubectl_cache_dir,
        &fixtures.join("21-mutate-input-pod.yaml"),
    )
    .await?;

    let label_value = kubectl_get_jsonpath(
        runner,
        &cluster.kubeconfig,
        kubectl_cache_dir,
        &["get", "pod", POD_NAME, "--namespace", NAMESPACE],
        "{.metadata.labels.app\\.kubernetes\\.io/managed-by}",
    )
    .await?;

    if label_value != "admissionlab" {
        return Err(format!(
            "expected the created Pod {POD_NAME:?} to carry \
             app.kubernetes.io/managed-by=admissionlab after admission (added by \
             {POLICY_NAME:?}), but its label value was {label_value:?} -- the apply succeeding \
             is not itself proof of mutation, since an unlabeled input Pod with no mutation at \
             all would also apply successfully"
        ));
    }

    Ok(())
}

/// Waits for `policy_name`'s (a `kyverno.io/v1` `ClusterPolicy`, always
/// cluster-scoped) own `status.conditions[type=Ready]` to become
/// `"True"`, via the real [`KubeReadinessProbe`] -- the same production
/// mechanism `admissionlab_installer::install_stack` itself uses for
/// every other readiness check, reused here rather than a hand-rolled
/// poll loop. See this file's own module documentation ("THE RACE") for
/// why this wait is what actually closes the gap
/// `webhookConfigurationPresent` leaves open.
async fn wait_for_cluster_policy_ready(
    cluster: &ClusterHandle,
    policy_name: &str,
) -> Result<(), String> {
    let check = ReadinessCheck::CustomResourceCondition {
        api_version: "kyverno.io/v1".to_string(),
        kind: "ClusterPolicy".to_string(),
        namespace: None,
        name: policy_name.to_string(),
        condition_type: "Ready".to_string(),
        status: "True".to_string(),
    };
    let deadline = Instant::now() + POLICY_READY_TIMEOUT;

    let evidence = KubeReadinessProbe::new()
        .wait(cluster, &check, deadline)
        .await
        .map_err(|error| format!("failed to wait for ClusterPolicy {policy_name:?}: {error}"))?;

    if !evidence.satisfied {
        return Err(format!(
            "ClusterPolicy {policy_name:?} never reached Ready=True within \
             {POLICY_READY_TIMEOUT:?}; last observed: {:?}",
            evidence.last_observed
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// kubectl helpers -- always an explicit `--kubeconfig`/`--cache-dir`,
// never `$KUBECONFIG`/`$KUBECACHEDIR`, mirroring
// `admissionlab_installer::manifests`'s own `kubectl_command` isolation
// discipline (see this file's module documentation, "Cleanup
// discipline"). Every invocation goes through this project's own
// `ProcessRunner`, never a direct `std::process`/shell call (Global
// Constraint 12), and every one is timeout-bounded (Global
// Constraint 13).
// ---------------------------------------------------------------------

fn kubectl_command(
    kubeconfig: &Path,
    cache_dir: &Path,
    mut args: Vec<std::ffi::OsString>,
) -> CommandSpec {
    args.push("--kubeconfig".into());
    args.push(kubeconfig.as_os_str().to_owned());
    args.push("--cache-dir".into());
    args.push(cache_dir.as_os_str().to_owned());
    CommandSpec {
        program: "kubectl".into(),
        args,
        cwd: None,
        env: BTreeMap::new(),
        sensitive_env_keys: BTreeSet::new(),
        timeout: KUBECTL_TIMEOUT,
    }
}

async fn kubectl_run(
    runner: &dyn ProcessRunner,
    kubeconfig: &Path,
    cache_dir: &Path,
    args: Vec<std::ffi::OsString>,
) -> Result<CommandResult, String> {
    let spec = kubectl_command(kubeconfig, cache_dir, args);
    let context = spec.context();
    runner
        .run(spec)
        .await
        .map_err(|error| format!("failed to run `{context}`: {error}"))
}

/// Runs `kubectl apply --server-side=false -f <path>` and requires it to
/// succeed.
async fn kubectl_apply_expect_success(
    runner: &dyn ProcessRunner,
    kubeconfig: &Path,
    cache_dir: &Path,
    path: &Path,
) -> Result<(), String> {
    let args = vec![
        "apply".into(),
        "--server-side=false".into(),
        "-f".into(),
        path.as_os_str().to_owned(),
    ];
    let result = kubectl_run(runner, kubeconfig, cache_dir, args).await?;
    if result.status.success() {
        Ok(())
    } else {
        Err(format!(
            "kubectl apply -f {} exited with {}: {}",
            path.display(),
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        ))
    }
}

/// Runs `kubectl apply --server-side=false -f <path>` and requires it to
/// fail (a non-zero exit -- the expected outcome for a fixture this
/// scenario is deliberately sending to be denied). Returns `stderr` so
/// the caller can assert the rejection is attributable to a specific
/// policy, not merely "some error happened".
async fn kubectl_apply_expect_denied(
    runner: &dyn ProcessRunner,
    kubeconfig: &Path,
    cache_dir: &Path,
    path: &Path,
) -> Result<String, String> {
    let args = vec![
        "apply".into(),
        "--server-side=false".into(),
        "-f".into(),
        path.as_os_str().to_owned(),
    ];
    let result = kubectl_run(runner, kubeconfig, cache_dir, args).await?;
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    if result.status.success() {
        Err(format!(
            "kubectl apply -f {} unexpectedly succeeded (expected the API server to reject \
             it); stdout: {}",
            path.display(),
            String::from_utf8_lossy(&result.stdout).trim()
        ))
    } else {
        Ok(stderr)
    }
}

/// Runs `kubectl <args...> -o jsonpath=<jsonpath>` and returns the
/// resulting stdout, trimmed. Requires the command to succeed.
async fn kubectl_get_jsonpath(
    runner: &dyn ProcessRunner,
    kubeconfig: &Path,
    cache_dir: &Path,
    args: &[&str],
    jsonpath: &str,
) -> Result<String, String> {
    let mut full_args: Vec<std::ffi::OsString> = args.iter().map(|arg| (*arg).into()).collect();
    full_args.push("-o".into());
    full_args.push(format!("jsonpath={jsonpath}").into());

    let result = kubectl_run(runner, kubeconfig, cache_dir, full_args).await?;
    if !result.status.success() {
        return Err(format!(
            "kubectl {} -o jsonpath={jsonpath} exited with {}: {}",
            args.join(" "),
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&result.stdout).trim().to_owned())
}

// ---------------------------------------------------------------------
// Small standalone helpers.
// ---------------------------------------------------------------------

/// Reads `compatibility/recipes.yaml`'s `kyverno` entry and returns the
/// single Kubernetes patch version this test must certify against. See
/// this file's own module documentation ("The Kubernetes version is
/// derived, never hardcoded").
///
/// # Errors
///
/// Returns a descriptive `Err` if the `kyverno` entry is missing, or if
/// its `certified` list does not contain exactly one version -- today's
/// file always has exactly one, and a future edit adding a second
/// version is a real decision (which one does a single-cluster
/// certification test install against?) this function deliberately
/// refuses to guess at, rather than silently picking the first entry.
fn kyverno_certified_kubernetes_version() -> Result<String, String> {
    let compat = load_recipe_compatibility()
        .map_err(|error| format!("failed to load compatibility/recipes.yaml: {error}"))?;
    let kyverno = compat
        .entry("kyverno")
        .ok_or_else(|| "compatibility/recipes.yaml has no \"kyverno\" entry".to_string())?;
    match kyverno.kubernetes.certified.as_slice() {
        [version] => Ok(version.clone()),
        other => Err(format!(
            "expected exactly one certified Kubernetes version for kyverno in \
             compatibility/recipes.yaml, found {}: {other:?} -- this test does not guess which \
             one to install against",
            other.len()
        )),
    }
}

/// `fixtures/kyverno/smoke/`, resolved from this checkout's own
/// repository root.
fn fixtures_dir() -> PathBuf {
    repo_root().join("fixtures/kyverno/smoke")
}

/// This checkout's own repository root -- three levels above this
/// crate's own `CARGO_MANIFEST_DIR`, mirroring
/// `admissionlab-test-webhook/tests/kind_smoke.rs`'s identical helper.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("this crate's own CARGO_MANIFEST_DIR/../.. must exist")
}

/// A fresh, guaranteed-unique scratch directory under the OS temp dir --
/// the single root this whole test uses (see this file's own module
/// documentation, "Cleanup discipline").
fn unique_root() -> PathBuf {
    let unique = RunId::generate();
    std::env::temp_dir().join(format!(
        "admissionlab-kyverno-recipe-test-{}",
        unique.as_str()
    ))
}
