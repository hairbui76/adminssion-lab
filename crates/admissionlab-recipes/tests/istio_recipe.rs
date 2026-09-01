//! Real, non-mocked Istio certification test (Task 2.9). `#[ignore]`d —
//! needs Docker and `kind` — in the same established style
//! `admissionlab-cluster/tests/kind_smoke.rs` (Task 1.9),
//! `admissionlab-test-webhook/tests/kind_smoke.rs` (Task 2.7), and
//! `tests/kyverno_recipe.rs` (Task 2.8) use, so `cargo test --workspace`
//! never requires either.
//!
//! End to end, once per certified Kubernetes version (see below): creates
//! a real `kind` cluster, loads the now-built-in `istio` recipe
//! (`recipes/istio/recipe.yaml`, wired into
//! [`admissionlab_recipes::load_builtin_recipes`]'s `BUILTIN_RECIPES`)
//! through the real Task 2.6 install-and-wait pipeline
//! (`admissionlab_installer::install_stack`), then applies both fixture
//! Pods in `fixtures/istio/smoke/` and proves — against the live cluster,
//! not by inspecting YAML — that the one in the injection-labelled
//! namespace actually gains a native Envoy sidecar in the right place,
//! while the one in the unlabelled namespace does not.
//!
//! # Only `istio/istiod` — no `istio/base` — and why that is not a shortcut
//!
//! `recipes/istio/recipe.yaml` installs exactly one Helm chart:
//! `istio/istiod`. Unlike almost every Istio installation guide (and this
//! project's own prior research), `istio/base` (the chart that installs
//! Istio's `CustomResourceDefinitions`) is never installed by this recipe
//! at all. This was verified, not assumed: `recipes/istio/README.md`'s
//! own "What this recipe pins" section documents the live-cluster method
//! (install `istiod` alone, confirm `Available`, confirm clean logs,
//! confirm sidecar injection still works identically) and result. This
//! test's own assertions below are the proof that holds on every
//! certified Kubernetes version, not only the one used during that
//! investigation.
//!
//! # The Kubernetes version(s) are derived, never hardcoded
//!
//! Unlike [`kyverno_recipe.rs`][crate]'s own
//! `kyverno_certified_kubernetes_version` (which requires — and finds —
//! exactly one certified version and refuses to guess if there were
//! more), `compatibility/recipes.yaml`'s `istio` entry records **three**
//! certified versions: Istio declares no vendor-documented Kubernetes
//! range, so nothing narrows its certified set below Admission Lab's
//! entire supported matrix (`1.35.8`, `1.36.4` Tier-1 primary, `1.37.0`
//! — see `recipes/istio/README.md`). [`istio_certified_kubernetes_versions`]
//! reads that list through
//! [`admissionlab_recipes::load_recipe_compatibility`] and
//! [`run_smoke_test`] installs and verifies this recipe **once per
//! listed version**, each in its own disposable cluster: an edit to
//! `certified` in that file changes how many clusters, and at which
//! versions, this test creates on its next run — not merely which single
//! version it hardcodes. Every version is attempted regardless of an
//! earlier one failing (see [`run_smoke_test`]'s own doc comment), so a
//! single run reports which certified version(s), if any, actually
//! regressed rather than stopping at the first failure and leaving the
//! rest unknown.
//!
//! # THE FINDING: the injected proxy is an `initContainers` entry, not a `containers` entry
//!
//! Controller Ruling R27. On every Kubernetes version this project
//! supports, the injected Envoy proxy lands as a **native sidecar**: an
//! entry in `spec.initContainers` (`istio-proxy`, `restartPolicy:
//! Always`), never in `spec.containers`. [`assert_pod_injected`] asserts
//! on `spec.initContainers` by name, and separately asserts
//! `spec.containers` is unchanged — asserting only the former without the
//! latter would miss a regression that injected into *both* places, and
//! asserting only against `spec.containers` (the mistake nearly every
//! Istio tutorial's placement would suggest) finds nothing on a cluster
//! where injection genuinely happened.
//!
//! **Mutation-checked specifically, per the controller supplement's own
//! instruction:** [`assert_pod_not_injected`] is the negative counterweight
//! — the identical Pod spec, created in a namespace with no
//! `istio-injection` label at all
//! (`fixtures/istio/smoke/11-noinject-pod.yaml`), must come back with no
//! `spec.initContainers` and no `sidecar.istio.io/status` annotation.
//! Without this fixture, a webhook that injected every Pod regardless of
//! namespace would make [`assert_pod_injected`] pass for the wrong
//! reason.
//!
//! # The caBundle sequencing question — settled here, not just asserted
//!
//! The injector `MutatingWebhookConfiguration` renders `failurePolicy:
//! Fail` with no `caBundle` at all; Istio's own webhook-cert controller
//! patches it in once ready. `recipes/istio/README.md`'s "Sequencing"
//! section documents the live-cluster method used to answer whether
//! `Deployment/istiod` reaching `Available` (this recipe's own readiness
//! gate, already waited on by [`install_stack`] before this test applies
//! any fixture) is itself sufficient to guarantee the `caBundle` patch
//! already landed: **yes, by a wide and repeatable margin (~3.3s,
//! measured twice independently)**, so [`install_and_verify`] applies no
//! namespace label or fixture Pod before `install_stack` returns.
//! [`wait_for_injector_ca_bundle`] is a single, bounded, *documented*
//! sanity check of that finding (not a poll that silently retries past a
//! real regression, and not a sleep) run once per cluster immediately
//! after — see its own doc comment.
//!
//! # Cleanup discipline
//!
//! [`ScratchRoot`] and [`ClusterGuard`] are copied verbatim from
//! `tests/kyverno_recipe.rs` (itself mirroring
//! `admissionlab-test-webhook/tests/kind_smoke.rs` and
//! `admissionlab-cluster/tests/kind_smoke.rs`) — see that file's own
//! documentation for why [`ClusterGuard`]'s `Drop` only warns (never
//! deletes: an async `kind delete cluster` cannot run inside a
//! synchronous `Drop`) and why [`ScratchRoot`]'s synchronous
//! (`std::fs::remove_dir_all`) `Drop` has no such hazard. Both guards are
//! bound before any fallible step that could leak what they own, exactly
//! as `tests/kyverno_recipe.rs`'s own "Correction, found in review" note
//! describes finding missing the first time.
//!
//! This test uses exactly **one** [`ScratchRoot`]/[`ArtifactStore`] for
//! every certified-version iteration, not one per iteration: each
//! iteration gets its own [`RunId`] (hence its own `<root>/<run_id>`
//! subtree — cluster kubeconfig, Helm's isolated state directory, this
//! test's own `kubectl` cache directory — [`admissionlab_core::RunPaths::new`]
//! namespaces every one of those by `run_id`), so nothing is shared
//! *between* iterations, while the single outer [`ScratchRoot`] still
//! guarantees the whole tree is removed on any exit path, including a
//! `?` that returns before the loop even starts.
//!
//! This test also never touches the user's real `~/.kube/`,
//! `~/.config/helm/`, or `~/.cache/`: every `helm`/`kubectl` invocation
//! below is either routed through [`admissionlab_installer::HelmInstaller`]
//! (already isolated) or built by this file's own `kubectl_command`,
//! which always passes an explicit `--kubeconfig`/`--cache-dir` and never
//! relies on `$KUBECONFIG`/`$KUBECACHEDIR`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use admissionlab_cluster::{KindClusterManager, cluster_name};
use admissionlab_core::{
    ArtifactStore, ClusterError, ClusterHandle, ClusterManager, ClusterSpec, CommandResult,
    CommandSpec, ProcessRunner, RunId, RunPaths, Side, TokioProcessRunner,
};
use admissionlab_installer::{HelmInstaller, KubeReadinessProbe, install_stack};
use admissionlab_recipes::{Capability, Recipe, load_builtin_recipes, load_recipe_compatibility};
use admissionlab_spec::ResolvedComponent;

/// Bounds one `install_stack` call for the `istio` component alone: the
/// `helm upgrade --install` submission plus `recipe.yaml`'s
/// `DeploymentAvailable{istio-system, istiod}` readiness check.
///
/// Measured empirically while writing this test, warm (a `kind` node
/// with `istiod`'s image already pulled once before): `helm upgrade
/// --install` itself returns in about two seconds, and `istiod` reaches
/// `Available` roughly five to eight seconds after that — dramatically
/// faster than Kyverno's own measured ~75-second warm total
/// (`tests/kyverno_recipe.rs`), since this recipe installs a single
/// chart with a single Deployment rather than four. Consistent with
/// that: a full real run of this test (three certified-version
/// iterations, two of them a genuinely cold `kind` node pulling both
/// the node image and `istiod`'s image for the first time) completed
/// end to end — cluster create, install, readiness, both fixture
/// Pods, cluster delete, all three iterations — in 222.00 seconds
/// total, roughly 74 seconds per iteration, of which
/// `admissionlab_cluster::kind`'s own measured `kind create cluster`
/// figures (≈31s warm / ≈105s cold) account for most. This constant
/// bounds only the install-plus-readiness portion of one iteration (not
/// cluster creation, which has its own budget), and is sized well above
/// even a generous estimate of that portion for a slower/loaded CI
/// runner.
const COMPONENT_TIMEOUT: Duration = Duration::from_secs(180);

/// Bounds [`wait_for_injector_ca_bundle`]'s sanity poll. Sized generously
/// even though the live-cluster measurement documented in
/// `recipes/istio/README.md` ("Sequencing") found the `caBundle` already
/// non-empty for ~3.3 seconds by the time `Available` is reached — so in
/// the overwhelming common case this poll's very first attempt succeeds
/// and this budget is never approached at all.
const CA_BUNDLE_SANITY_TIMEOUT: Duration = Duration::from_secs(30);

/// The delay between [`wait_for_injector_ca_bundle`]'s poll attempts.
const CA_BUNDLE_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Bounds one `kubectl` invocation against an already-healthy,
/// already-warm cluster (an apply of a handful of small objects, or a
/// single `get`) — not a provisioning step. Mirrors
/// `tests/kyverno_recipe.rs`'s own `KUBECTL_TIMEOUT`.
const KUBECTL_TIMEOUT: Duration = Duration::from_secs(30);

/// This `kubectl`'s own isolated schema/discovery cache directory name,
/// under each iteration's own `RunPaths::logs()` directory. Mirrors
/// `admissionlab_installer::manifests`'s own `kubectl_command` isolation
/// discipline: an explicit `--cache-dir`, never `$KUBECACHEDIR` or the
/// operator's real `~/.kube/cache`.
const KUBECTL_CACHE_SUBDIR: &str = "istio-recipe-test-kubectl-cache";

/// The injector `MutatingWebhookConfiguration`'s name, exactly as
/// `recipe.yaml`'s own `webhookConfigurationPresent` readiness check and
/// [`wait_for_injector_ca_bundle`] both name it.
const INJECTOR_WEBHOOK_NAME: &str = "istio-sidecar-injector";

// ---------------------------------------------------------------------
// Scratch root guard. Copied verbatim from `tests/kyverno_recipe.rs` —
// see that file's module documentation ("Cleanup discipline") and this
// file's own ("Cleanup discipline") for why a synchronous `Drop` is safe
// here and why it must be bound before any fallible step.
// ---------------------------------------------------------------------

struct ScratchRoot(PathBuf);

impl Drop for ScratchRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------
// Cleanup guard — copied verbatim from `tests/kyverno_recipe.rs` (itself
// mirroring `admissionlab-test-webhook/tests/kind_smoke.rs` and
// `admissionlab-cluster/tests/kind_smoke.rs`).
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
async fn istio_recipe_installs_and_injects_sidecar_for_every_certified_kubernetes_version() {
    let outcome = run_smoke_test().await;
    outcome.expect("istio recipe certification test");
}

/// Validates the built-in `istio` recipe's own metadata (cheap, no
/// cluster needed, fails fast before any of the — comparatively
/// expensive — per-version cluster work below), then runs one full
/// install-and-inject smoke test **per Kubernetes version**
/// [`istio_certified_kubernetes_versions`] lists, in its own disposable
/// cluster.
///
/// Every listed version is attempted regardless of an earlier one
/// failing — this loop collects every [`run_one_version`] error rather
/// than stopping at the first, so a single run reports exactly which
/// certified version(s) regressed instead of leaving later ones
/// unknown. Regardless of how many succeed or fail, this test always
/// removes its single scratch root before returning (see this file's
/// own module documentation, "Cleanup discipline").
async fn run_smoke_test() -> Result<(), String> {
    let recipe = load_istio_recipe()?;
    check_recipe_metadata(&recipe)?;
    let versions = istio_certified_kubernetes_versions()?;

    let root = unique_root();
    // Bound immediately, before any fallible step below — see
    // `ScratchRoot`'s own documentation for why its `Drop` alone is both
    // necessary and sufficient to guarantee `root` never leaks.
    let _scratch_root_guard = ScratchRoot(root.clone());
    let store = ArtifactStore::new(&root);

    let mut problems = Vec::new();
    for version in &versions {
        if let Err(error) = run_one_version(&store, recipe.clone(), version).await {
            problems.push(format!("[kubernetes {version}]\n{error}"));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n\n"))
    }
}

/// Creates one cluster at `kubernetes_version`, installs `istio`, runs
/// the injection scenario, and — regardless of what any of that finds,
/// including a failure before the cluster is even created — always
/// deletes the cluster (if one exists) before returning. Mirrors
/// `tests/kyverno_recipe.rs`'s own `run_smoke_test` shape, parameterized
/// by version and given its own fresh [`RunId`]/[`RunPaths`] via `store`.
async fn run_one_version(
    store: &ArtifactStore,
    recipe: Recipe,
    kubernetes_version: &str,
) -> Result<(), String> {
    let manager = KindClusterManager::new(Arc::new(TokioProcessRunner::new()));
    let runner = TokioProcessRunner::new();

    let run_id = RunId::generate();
    let paths = store
        .create_run(&run_id)
        .await
        .map_err(|error| format!("failed to prepare a run workspace: {error}"))?;

    let matrix = admissionlab_cluster::load_matrix()
        .map_err(|error| format!("failed to load the Kubernetes compatibility matrix: {error}"))?;
    let resolved =
        admissionlab_cluster::resolve_node_image(kubernetes_version, &matrix).map_err(|error| {
            format!("failed to resolve a node image for {kubernetes_version:?}: {error}")
        })?;
    let name = cluster_name(Side::Baseline, &run_id)
        .map_err(|error| format!("failed to build a cluster name: {error}"))?;
    let spec = ClusterSpec {
        side: Side::Baseline,
        name,
        kubernetes_version: resolved.version.clone(),
        node_image: resolved.pinned_image.clone(),
        images: Vec::new(),
    };

    let handle = manager
        .create(&spec, &paths)
        .await
        .map_err(|error| format!("failed to create cluster: {error}"))?;
    let guard = ClusterGuard::new(handle);

    let check_result = install_and_verify(&runner, guard.handle(), &paths, recipe).await;

    let mut problems: Vec<String> = check_result.err().into_iter().collect();
    if let Err(error) = guard.cleanup(&manager).await {
        problems.push(format!("failed to delete the cluster: {error}"));
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// Installs `recipe` through the real Task 2.6 pipeline, sanity-checks
/// the injector's `caBundle`, then runs the injection scenario against
/// both fixture Pods.
async fn install_and_verify(
    runner: &dyn ProcessRunner,
    cluster: &ClusterHandle,
    paths: &RunPaths,
    recipe: Recipe,
) -> Result<(), String> {
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

    // See this file's module documentation ("The caBundle sequencing
    // question") for why this is a single bounded sanity check, not the
    // primary readiness gate (recipe.yaml's own DeploymentAvailable
    // check, already satisfied by `install_stack` above, is).
    wait_for_injector_ca_bundle(runner, &cluster.kubeconfig, &kubectl_cache_dir).await?;

    let fixtures = fixtures_dir();
    kubectl_apply_expect_success(
        runner,
        &cluster.kubeconfig,
        &kubectl_cache_dir,
        &fixtures.join("00-namespaces.yaml"),
    )
    .await?;

    run_injection_scenario(runner, &cluster.kubeconfig, &kubectl_cache_dir, &fixtures).await
}

/// Applies both fixture Pods and proves injection happened exactly where
/// it should have, and nowhere else. See this file's module
/// documentation ("THE FINDING") for why both directions are asserted.
async fn run_injection_scenario(
    runner: &dyn ProcessRunner,
    kubeconfig: &Path,
    kubectl_cache_dir: &Path,
    fixtures: &Path,
) -> Result<(), String> {
    kubectl_apply_expect_success(
        runner,
        kubeconfig,
        kubectl_cache_dir,
        &fixtures.join("10-inject-pod.yaml"),
    )
    .await?;
    let injected_pod = kubectl_get_json(
        runner,
        kubeconfig,
        kubectl_cache_dir,
        &[
            "get",
            "pod",
            "admissionlab-smoke-inject",
            "--namespace",
            "admissionlab-istio-smoke-inject",
        ],
    )
    .await?;
    assert_pod_injected(&injected_pod)?;

    kubectl_apply_expect_success(
        runner,
        kubeconfig,
        kubectl_cache_dir,
        &fixtures.join("11-noinject-pod.yaml"),
    )
    .await?;
    let noninjected_pod = kubectl_get_json(
        runner,
        kubeconfig,
        kubectl_cache_dir,
        &[
            "get",
            "pod",
            "admissionlab-smoke-noinject",
            "--namespace",
            "admissionlab-istio-smoke-noinject",
        ],
    )
    .await?;
    assert_pod_not_injected(&noninjected_pod)
}

/// Asserts `pod` (read back from a real API server, not this repository's
/// own fixture file) was actually injected: `istio-init` and
/// `istio-proxy` (the latter carrying `restartPolicy: "Always"`) present
/// in `spec.initContainers`, `spec.containers` unchanged (still exactly
/// `["app"]`), and the `sidecar.istio.io/status` annotation present.
///
/// # Errors
///
/// Returns a specific, descriptive message naming exactly which of the
/// above did not hold — never a generic "injection failed" — so a
/// regression in any one of them (for example a future Istio release
/// moving the proxy back into `spec.containers`, or adding a second
/// container Admission Lab does not expect) is diagnosable directly from
/// this test's failure output.
fn assert_pod_injected(pod: &serde_json::Value) -> Result<(), String> {
    let init_container_names = container_names(pod, "initContainers");
    if !init_container_names.iter().any(|name| name == "istio-init") {
        return Err(format!(
            "expected the injected Pod's spec.initContainers to include \"istio-init\", got \
             names {init_container_names:?}"
        ));
    }
    if !init_container_names
        .iter()
        .any(|name| name == "istio-proxy")
    {
        return Err(format!(
            "expected the injected Pod's spec.initContainers to include \"istio-proxy\" (the \
             Envoy sidecar — Controller Ruling R27: it lands in spec.initContainers, not \
             spec.containers, on every Kubernetes version this project supports), got names \
             {init_container_names:?} -- injection did not happen"
        ));
    }

    let restart_policy = pod
        .pointer("/spec/initContainers")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .find(|container| {
            container.get("name").and_then(serde_json::Value::as_str) == Some("istio-proxy")
        })
        .and_then(|container| container.get("restartPolicy"))
        .and_then(serde_json::Value::as_str);
    if restart_policy != Some("Always") {
        return Err(format!(
            "expected the \"istio-proxy\" initContainer's restartPolicy to be \"Always\" (a \
             native sidecar, per Controller Ruling R27), got {restart_policy:?}"
        ));
    }

    let container_names = container_names(pod, "containers");
    if container_names != ["app"] {
        return Err(format!(
            "expected spec.containers to remain exactly [\"app\"] after injection (the proxy \
             must land in spec.initContainers only, per Controller Ruling R27), got \
             {container_names:?}"
        ));
    }

    let status_annotation = pod
        .pointer("/metadata/annotations")
        .and_then(|annotations| annotations.get("sidecar.istio.io/status"));
    if status_annotation.is_none() {
        return Err(
            "expected the injected Pod to carry a \"sidecar.istio.io/status\" annotation, but \
             it was absent"
                .to_string(),
        );
    }

    Ok(())
}

/// Asserts `pod` (read back from a real API server) was **not** injected
/// — the negative counterweight to [`assert_pod_injected`]; see this
/// file's module documentation ("THE FINDING") for why this fixture
/// exists and what it catches.
///
/// # Errors
///
/// Returns a specific message if `spec.initContainers` is non-empty or
/// the `sidecar.istio.io/status` annotation is present — either would
/// mean injection happened in a namespace that carries no
/// `istio-injection` label, which is exactly the false-positive this
/// counterweight exists to catch.
fn assert_pod_not_injected(pod: &serde_json::Value) -> Result<(), String> {
    let init_container_names = container_names(pod, "initContainers");
    if !init_container_names.is_empty() {
        return Err(format!(
            "expected the Pod in the unlabelled namespace to receive NO initContainers (no \
             injection expected), but spec.initContainers named {init_container_names:?} -- \
             injection is not actually scoped to the injection-labelled namespace"
        ));
    }

    let status_annotation = pod
        .pointer("/metadata/annotations")
        .and_then(|annotations| annotations.get("sidecar.istio.io/status"));
    if status_annotation.is_some() {
        return Err(format!(
            "expected the Pod in the unlabelled namespace to carry no \
             \"sidecar.istio.io/status\" annotation, but found {status_annotation:?}"
        ));
    }

    Ok(())
}

/// Returns the `name` of every entry in `pod`'s `spec.<field>` array
/// (`"initContainers"` or `"containers"`), or an empty `Vec` if that
/// field is absent — which for `initContainers` on an uninjected Pod is
/// the expected, unremarkable case (Kubernetes omits the field entirely
/// rather than serializing an empty array), not an error this helper
/// needs to distinguish from "field present but empty".
fn container_names(pod: &serde_json::Value, field: &str) -> Vec<String> {
    pod.pointer(&format!("/spec/{field}"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|container| container.get("name").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// Polls `MutatingWebhookConfiguration/istio-sidecar-injector` for a
/// non-empty `caBundle` on its first webhook entry, up to
/// [`CA_BUNDLE_SANITY_TIMEOUT`].
///
/// This is deliberately **not** the primary readiness gate —
/// `recipe.yaml`'s own `DeploymentAvailable{istio-system, istiod}` check
/// (already waited on by `install_stack` before this is called) is, per
/// the controller supplement's own instruction. This is a single,
/// bounded, documented sanity check of the live-cluster finding recorded
/// in `recipes/istio/README.md` ("Sequencing") that `Available` alone is
/// already sufficient, by a wide and repeatable margin: it polls (with a
/// real deadline, failing loudly and specifically if that deadline is
/// reached) rather than either sleeping a fixed amount (which either
/// wastes time or, sized wrong, does not actually close a real race) or
/// skipping the check entirely (which would let a future regression in
/// that finding surface only as a much more confusing failure later —
/// `kubectl apply` against a connection-refused, fail-closed webhook).
///
/// # Errors
///
/// Returns a descriptive `Err` naming the webhook and the timeout if
/// `caBundle` is still empty once [`CA_BUNDLE_SANITY_TIMEOUT`] elapses.
async fn wait_for_injector_ca_bundle(
    runner: &dyn ProcessRunner,
    kubeconfig: &Path,
    cache_dir: &Path,
) -> Result<(), String> {
    let deadline = Instant::now() + CA_BUNDLE_SANITY_TIMEOUT;
    loop {
        let ca_bundle = kubectl_get_jsonpath(
            runner,
            kubeconfig,
            cache_dir,
            &[
                "get",
                "mutatingwebhookconfigurations",
                INJECTOR_WEBHOOK_NAME,
            ],
            "{.webhooks[0].clientConfig.caBundle}",
        )
        .await?;
        if !ca_bundle.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "MutatingWebhookConfiguration {INJECTOR_WEBHOOK_NAME:?}'s caBundle was still \
                 empty {CA_BUNDLE_SANITY_TIMEOUT:?} after Deployment/istiod reported Available \
                 -- this contradicts the live-cluster measurement recorded in \
                 recipes/istio/README.md (\"Sequencing\"); applying the fixture Pod now would be \
                 admitted against a fail-closed webhook with no working backend"
            ));
        }
        tokio::time::sleep(CA_BUNDLE_POLL_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------
// kubectl helpers — always an explicit `--kubeconfig`/`--cache-dir`,
// never `$KUBECONFIG`/`$KUBECACHEDIR`, mirroring
// `tests/kyverno_recipe.rs`'s identical helpers (`kubectl_command`,
// `kubectl_run`, `kubectl_apply_expect_success`, `kubectl_get_jsonpath`
// are copied verbatim; `kubectl_get_json` is new, used for this test's
// own nested-structure Pod assertions, where a jsonpath filter
// expression would be harder to get right than parsing the object
// directly — the same approach `admissionlab_installer::readiness`
// itself takes for every readiness check). Every invocation goes through
// this project's own `ProcessRunner`, never a direct `std::process`/
// shell call (Global Constraint 12), and every one is timeout-bounded
// (Global Constraint 13).
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

/// Runs `kubectl <args...> -o json` and parses stdout as
/// [`serde_json::Value`]. Requires the command to succeed.
async fn kubectl_get_json(
    runner: &dyn ProcessRunner,
    kubeconfig: &Path,
    cache_dir: &Path,
    args: &[&str],
) -> Result<serde_json::Value, String> {
    let mut full_args: Vec<std::ffi::OsString> = args.iter().map(|arg| (*arg).into()).collect();
    full_args.push("-o".into());
    full_args.push("json".into());

    let result = kubectl_run(runner, kubeconfig, cache_dir, full_args).await?;
    if !result.status.success() {
        return Err(format!(
            "kubectl {} -o json exited with {}: {}",
            args.join(" "),
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    serde_json::from_slice(&result.stdout).map_err(|error| {
        format!(
            "kubectl {} -o json produced output that did not parse as JSON: {error}",
            args.join(" ")
        )
    })
}

// ---------------------------------------------------------------------
// Small standalone helpers.
// ---------------------------------------------------------------------

/// Loads the built-in `istio` recipe.
///
/// # Errors
///
/// Returns a descriptive `Err` if built-in recipes fail to load at all,
/// or if no recipe named `"istio"` is among them.
fn load_istio_recipe() -> Result<Recipe, String> {
    load_builtin_recipes()
        .map_err(|error| format!("failed to load built-in recipes: {error}"))?
        .into_iter()
        .find(|recipe| recipe.name == "istio")
        .ok_or_else(|| {
            "no \"istio\" recipe among load_builtin_recipes() -- is it wired into \
             BUILTIN_RECIPES?"
                .to_string()
        })
}

/// Cheap, meaningful checks on `recipe`'s own resolved metadata,
/// independent of installing anything — catches a hand-edit to
/// `recipe.yaml` drifting from what this test (and
/// `recipes/istio/README.md`) documents, before ever touching a cluster,
/// and cross-checks it against `compatibility/recipes.yaml` so the two
/// independently-maintained files cannot silently drift apart.
fn check_recipe_metadata(recipe: &Recipe) -> Result<(), String> {
    if recipe.version != "1.30.4" {
        return Err(format!(
            "expected the istio recipe to pin chart version 1.30.4, got {:?}",
            recipe.version
        ));
    }
    if !recipe.capabilities.contains(&Capability::Admission) {
        return Err("expected the istio recipe to declare the admission capability".to_string());
    }
    if recipe.readiness.len() != 2 {
        return Err(format!(
            "expected exactly 2 readiness checks (1 deploymentAvailable + 1 \
             webhookConfigurationPresent), got {}: {:?}",
            recipe.readiness.len(),
            recipe.readiness
        ));
    }

    let compat = load_recipe_compatibility()
        .map_err(|error| format!("failed to load compatibility/recipes.yaml: {error}"))?;
    let compat_entry = compat
        .entry("istio")
        .ok_or_else(|| "compatibility/recipes.yaml has no \"istio\" entry".to_string())?;
    if compat_entry.version != recipe.version {
        return Err(format!(
            "recipes/istio/recipe.yaml pins version {:?} but compatibility/recipes.yaml's istio \
             entry pins {:?} -- these two checked-in files must agree",
            recipe.version, compat_entry.version
        ));
    }
    Ok(())
}

/// Reads `compatibility/recipes.yaml`'s `istio` entry and returns every
/// Kubernetes patch version this test must certify against — see this
/// file's own module documentation ("The Kubernetes version(s) are
/// derived, never hardcoded").
///
/// # Errors
///
/// Returns a descriptive `Err` if the `istio` entry is missing, or if its
/// `certified` list is empty — this test refuses to silently run zero
/// certifications and report an unqualified pass.
fn istio_certified_kubernetes_versions() -> Result<Vec<String>, String> {
    let compat = load_recipe_compatibility()
        .map_err(|error| format!("failed to load compatibility/recipes.yaml: {error}"))?;
    let istio = compat
        .entry("istio")
        .ok_or_else(|| "compatibility/recipes.yaml has no \"istio\" entry".to_string())?;
    if istio.kubernetes.certified.is_empty() {
        return Err(
            "compatibility/recipes.yaml's \"istio\" entry has an empty certified list -- this \
             test does not silently skip certification"
                .to_string(),
        );
    }
    Ok(istio.kubernetes.certified.clone())
}

/// `fixtures/istio/smoke/`, resolved from this checkout's own repository
/// root.
fn fixtures_dir() -> PathBuf {
    repo_root().join("fixtures/istio/smoke")
}

/// This checkout's own repository root — three levels above this crate's
/// own `CARGO_MANIFEST_DIR`, mirroring `tests/kyverno_recipe.rs`'s
/// identical helper.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("this crate's own CARGO_MANIFEST_DIR/../.. must exist")
}

/// A fresh, guaranteed-unique scratch directory under the OS temp dir —
/// the single root every certified-version iteration in this test uses
/// (see this file's own module documentation, "Cleanup discipline").
fn unique_root() -> PathBuf {
    let unique = RunId::generate();
    std::env::temp_dir().join(format!(
        "admissionlab-istio-recipe-test-{}",
        unique.as_str()
    ))
}

// ---------------------------------------------------------------------
// Fast, no-cluster unit tests for the pure JSON-inspection logic above
// ([`assert_pod_injected`], [`assert_pod_not_injected`],
// [`container_names`]). These run under plain `cargo test --workspace`
// (not `#[ignore]`d, unlike this file's one real end-to-end test) and
// exist specifically so a bug in the *assertion* logic itself is caught
// in milliseconds rather than only after a multi-minute real cluster
// run. Every fixture below is shaped exactly like a real object read
// back from a live cluster while writing this recipe (see
// `recipes/istio/README.md`), not an idealized guess.
//
// Per this project's own testing standard, each test below is paired
// with the specific mutation of the fixture that would make it fail, so
// a regression in the assertion functions themselves — not just in
// Istio's own behavior — is caught.
// ---------------------------------------------------------------------

#[cfg(test)]
mod assertion_unit_tests {
    use serde_json::json;

    use super::{assert_pod_injected, assert_pod_not_injected, container_names};

    /// A Pod exactly as the real API server returns it after injection:
    /// `istio-init` (no `restartPolicy`) then `istio-proxy`
    /// (`restartPolicy: "Always"`) in `spec.initContainers`,
    /// `spec.containers` unchanged, `sidecar.istio.io/status` present.
    fn injected_pod_fixture() -> serde_json::Value {
        json!({
            "metadata": {
                "name": "admissionlab-smoke-inject",
                "namespace": "admissionlab-istio-smoke-inject",
                "annotations": {
                    "sidecar.istio.io/status": "{\"initContainers\":[\"istio-init\",\"istio-proxy\"]}"
                }
            },
            "spec": {
                "initContainers": [
                    {"name": "istio-init"},
                    {"name": "istio-proxy", "restartPolicy": "Always"}
                ],
                "containers": [
                    {"name": "app"}
                ]
            }
        })
    }

    /// A Pod exactly as the real API server returns it when injection did
    /// not apply: no `initContainers` field at all (not an empty array —
    /// Kubernetes omits it entirely), `containers` unchanged, no
    /// `sidecar.istio.io/status` annotation.
    fn uninjected_pod_fixture() -> serde_json::Value {
        json!({
            "metadata": {
                "name": "admissionlab-smoke-noinject",
                "namespace": "admissionlab-istio-smoke-noinject"
            },
            "spec": {
                "containers": [
                    {"name": "app"}
                ]
            }
        })
    }

    #[test]
    fn assert_pod_injected_accepts_a_genuinely_injected_pod() {
        assert_pod_injected(&injected_pod_fixture()).expect("a genuinely injected Pod must pass");
    }

    #[test]
    fn assert_pod_injected_rejects_a_pod_with_no_init_containers_at_all() {
        // Mutation: delete spec.initContainers entirely -- the exact
        // shape "injection never happened" takes on a real cluster. If
        // this passed, the function could never actually detect a
        // regression where Istio's injector stops firing.
        let mut pod = injected_pod_fixture();
        pod["spec"]
            .as_object_mut()
            .unwrap()
            .remove("initContainers");

        let error = assert_pod_injected(&pod).expect_err("must fail with no initContainers");
        assert!(error.contains("istio-init") || error.contains("istio-proxy"));
    }

    #[test]
    fn assert_pod_injected_rejects_istio_proxy_placed_in_containers_instead_of_init_containers() {
        // Mutation: the exact false-negative Controller Ruling R27 warns
        // about -- the proxy present, but in the pre-native-sidecar
        // location. Both spec.initContainers[*].name checks below must
        // report the sidecar missing from spec.initContainers, not be
        // satisfied by finding it elsewhere.
        let pod = json!({
            "metadata": {
                "name": "admissionlab-smoke-inject",
                "namespace": "admissionlab-istio-smoke-inject",
                "annotations": {"sidecar.istio.io/status": "..."}
            },
            "spec": {
                "containers": [
                    {"name": "app"},
                    {"name": "istio-proxy"}
                ]
            }
        });

        let error = assert_pod_injected(&pod)
            .expect_err("istio-proxy in spec.containers must not satisfy this assertion");
        assert!(error.contains("istio-init") || error.contains("istio-proxy"));
    }

    #[test]
    fn assert_pod_injected_rejects_wrong_restart_policy() {
        // Mutation: istio-proxy present in the right place, but without
        // the native-sidecar restartPolicy -- distinguishes "the proxy
        // container exists" from "the proxy is a native sidecar",
        // exactly the distinction Controller Ruling R27 turns on.
        let mut pod = injected_pod_fixture();
        pod["spec"]["initContainers"][1]
            .as_object_mut()
            .unwrap()
            .remove("restartPolicy");

        let error = assert_pod_injected(&pod).expect_err("must fail without restartPolicy Always");
        assert!(error.contains("restartPolicy") || error.contains("Always"));
    }

    #[test]
    fn assert_pod_injected_rejects_an_unexpected_extra_container() {
        // Mutation: something (a regression, a mis-scoped mutating
        // webhook) added to spec.containers, which Controller Ruling R27
        // says must stay exactly the fixture's own "app" container.
        let mut pod = injected_pod_fixture();
        pod["spec"]["containers"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "unexpected"}));

        let error =
            assert_pod_injected(&pod).expect_err("an unexpected extra container must be caught");
        assert!(error.contains("app"));
    }

    #[test]
    fn assert_pod_injected_rejects_a_missing_status_annotation() {
        // Mutation: initContainers correct, but the status annotation
        // istiod itself always sets on a genuinely injected Pod is
        // missing -- a second, independent signal this function checks
        // rather than relying on initContainers alone.
        let mut pod = injected_pod_fixture();
        pod["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("annotations");

        let error = assert_pod_injected(&pod).expect_err("must fail without the status annotation");
        assert!(error.contains("sidecar.istio.io/status"));
    }

    #[test]
    fn assert_pod_not_injected_accepts_a_genuinely_uninjected_pod() {
        assert_pod_not_injected(&uninjected_pod_fixture())
            .expect("a genuinely uninjected Pod must pass");
    }

    #[test]
    fn assert_pod_not_injected_rejects_a_pod_that_was_actually_injected() {
        // Mutation: the negative counterweight's own failure mode --
        // if injection somehow applied in the unlabelled namespace (for
        // example a webhook that ignores its namespaceSelector), this
        // must catch it rather than silently accepting whatever
        // initContainers turned up.
        let error = assert_pod_not_injected(&injected_pod_fixture())
            .expect_err("an actually-injected Pod must fail this assertion");
        assert!(error.contains("istio-init") || error.contains("istio-proxy"));
    }

    #[test]
    fn assert_pod_not_injected_rejects_a_stray_status_annotation_even_without_init_containers() {
        // Mutation: no initContainers, but the status annotation is
        // present anyway -- an inconsistent state a real cluster should
        // never produce, but this assertion checks both signals
        // independently rather than trusting initContainers alone.
        let mut pod = uninjected_pod_fixture();
        pod["metadata"]["annotations"] = json!({"sidecar.istio.io/status": "unexpected"});

        let error = assert_pod_not_injected(&pod)
            .expect_err("a stray status annotation must be caught even with no initContainers");
        assert!(error.contains("sidecar.istio.io/status"));
    }

    #[test]
    fn container_names_returns_empty_for_an_absent_field() {
        let pod = uninjected_pod_fixture();
        assert_eq!(
            container_names(&pod, "initContainers"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn container_names_returns_names_in_order() {
        let pod = injected_pod_fixture();
        assert_eq!(
            container_names(&pod, "initContainers"),
            vec!["istio-init".to_string(), "istio-proxy".to_string()]
        );
    }
}
