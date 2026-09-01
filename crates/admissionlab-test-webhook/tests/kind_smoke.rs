//! Real, non-mocked recipe-install smoke test for `recipes/test-webhook`
//! (Task 2.7 brief Step 3). `#[ignore]`d — needs Docker and `kind` — in
//! the same established style `admissionlab-cluster/tests/kind_smoke.rs`
//! (Task 1.9) uses, so `cargo test --workspace` never requires either.
//!
//! End to end, this test: builds the `admissionlab-test-webhook` image
//! and loads it into a fresh `kind` cluster by actually running
//! `scripts/build-test-images.sh` as a real subprocess (proving that
//! script itself works, not only that its logic *would*); loads
//! `recipes/test-webhook/recipe.yaml` directly, in place, through
//! `admissionlab_recipes::load_recipe_overrides` (Task 2.10: the
//! recipe's relative `install.paths` entries resolve against its own
//! checked-in directory — no test-side path rewriting of any kind);
//! drives it through the real Task 2.4/2.6 install-and-wait pipeline
//! (`admissionlab_installer::install_stack`); and — beyond what
//! `install_stack`'s own success already proves — independently confirms
//! the `bootstrap` init container actually did its job by fetching every
//! live webhook configuration the recipe declares (Task 3.9 added two
//! `MutatingWebhookConfiguration`s alongside the original
//! `ValidatingWebhookConfiguration`) and checking each one's `caBundle`
//! is now populated with real PEM certificate text, not merely that the
//! objects exist (which `install_stack`'s own `webhookConfigurationPresent`
//! readiness checks already established). With `failurePolicy: Fail` on
//! all three, one un-patched `caBundle` would fail every fixture
//! request routed to it, so "the validating one is fine" is no longer a
//! sufficient check.
//!
//! What this test does *not* separately verify: that `/healthz` itself
//! returns `200 OK` to an outside caller. `install_stack`'s own
//! `deploymentAvailable` readiness check already cannot succeed unless
//! the main container's `readinessProbe` (`GET /healthz` over HTTPS,
//! `recipes/test-webhook/manifests/30-deployment.yaml`) has itself
//! already succeeded at least `failureThreshold` times fewer than
//! needed to fail the pod — so `DeploymentAvailable` passing is already
//! live, real evidence `/healthz` answered correctly; a second,
//! independent HTTP call (needing its own port-forward or `NodePort`
//! plumbing this test does not otherwise need) would be duplicate
//! coverage of the same underlying fact, not a new one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use admissionlab_cluster::{KindClusterManager, cluster_name, load_matrix, resolve_node_image};
use admissionlab_core::{
    ArtifactStore, ClusterError, ClusterHandle, ClusterManager, ClusterSpec, CommandSpec,
    ProcessRunner, RunId, RunPaths, Side, TokioProcessRunner,
};
use admissionlab_installer::{KubeReadinessProbe, ManifestsInstaller, install_stack};
use admissionlab_recipes::load_recipe_overrides;
use admissionlab_spec::{ReadinessCheck, ResolvedComponent};
use k8s_openapi::api::admissionregistration::v1::{
    MutatingWebhookConfiguration, ValidatingWebhookConfiguration,
};
use kube::api::Api;
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};

/// The Kubernetes version this smoke test provisions its cluster at —
/// Tier 1's primary supported version, the same
/// `compatibility/kubernetes.yaml`-resolved constant
/// `admissionlab-cluster/tests/kind_smoke.rs` and
/// `scripts/verify-cleanup.sh` both already use.
const KUBERNETES_VERSION: &str = "1.36.4";

/// Generous bound for one `bash scripts/build-test-images.sh` run: a
/// cold `docker build` (no layer cache) plus `kind load docker-image`.
/// Measured on the reference machine at well under two minutes with the
/// Rust builder base image not yet cached; this leaves comfortable
/// headroom for a slower/loaded CI runner without letting a genuine hang
/// stall the test indefinitely.
const BUILD_AND_LOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// Bounds one component's install-plus-readiness
/// (`admissionlab_installer::stack::install_stack`'s own
/// `component_timeout` parameter) — generous for a slow/loaded CI
/// runner given the image is already loaded locally (no registry pull
/// wait), covering four small `kubectl apply` invocations plus pod
/// scheduling, the init container's (sub-second) certificate generation,
/// and the main container reaching `Ready`.
const COMPONENT_TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------
// Cleanup guard -- mirrors admissionlab-cluster/tests/kind_smoke.rs's
// own `ClusterGuard` exactly (see that file's module documentation for
// why `Drop` may only warn, never delete).
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
async fn test_webhook_recipe_installs_and_becomes_ready() {
    let outcome = run_smoke_test().await;
    outcome.expect("test-webhook recipe install smoke test");
}

async fn run_smoke_test() -> Result<(), String> {
    let manager = KindClusterManager::new(Arc::new(TokioProcessRunner::new()));
    let runner = TokioProcessRunner::new();

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

    let check_result = install_and_verify(&runner, guard.handle()).await;

    let mut problems: Vec<String> = check_result.err().into_iter().collect();
    if let Err(error) = guard.cleanup(&manager).await {
        problems.push(format!("failed to delete the cluster: {error}"));
    }
    let _ = tokio::fs::remove_dir_all(&root).await;

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

async fn install_and_verify(
    runner: &dyn ProcessRunner,
    cluster: &ClusterHandle,
) -> Result<(), String> {
    build_and_load_image(runner, &cluster.spec.name).await?;

    // Loaded straight from its real, checked-in location -- no scratch
    // copy, no path rewriting. `install.paths` inside this file is
    // written relative (`manifests/00-namespace.yaml` and so on);
    // `load_recipe_overrides` resolves each entry against this exact
    // directory, the same directory it found `recipe.yaml` in (Task
    // 2.10).
    let recipe_dir = repo_root().join("recipes/test-webhook");
    let recipes = load_recipe_overrides(&recipe_dir)
        .map_err(|error| format!("failed to load recipes/test-webhook/recipe.yaml: {error}"))?;
    let recipe = recipes
        .into_iter()
        .next()
        .ok_or_else(|| "recipe.yaml produced no recipes".to_string())?;
    // The K8s object names the `webhookConfigurationPresent` readiness
    // checks name -- not `recipe.name` (the recipe's own identifier,
    // "test-webhook", a different string entirely from any webhook
    // configuration object's own `metadata.name`). Read from the
    // recipe's own declared readiness checks rather than assumed, so
    // this assertion stays correct as objects are renamed or added
    // without this test file being updated by hand -- Task 3.9's two
    // mutating configurations were picked up here for free.
    let webhook_configuration_names: Vec<String> = recipe
        .readiness
        .iter()
        .filter_map(|check| match check {
            ReadinessCheck::WebhookConfigurationPresent { name } => Some(name.clone()),
            _ => None,
        })
        .collect();
    if webhook_configuration_names.is_empty() {
        return Err(
            "recipe.yaml declares no webhookConfigurationPresent readiness check".to_string(),
        );
    }

    let component = ResolvedComponent {
        name: recipe.name,
        version: recipe.version,
        install: recipe.install,
        readiness: recipe.readiness,
        recipe_normalize_rules: recipe.normalize_rules,
        capabilities: recipe.capabilities,
    };

    let (run_paths_root, run_paths) = prepare_run_paths().await?;
    let manifests_installer =
        ManifestsInstaller::new(Arc::new(TokioProcessRunner::new()), &run_paths);
    let readiness_probe = KubeReadinessProbe::new();

    let installed = install_stack(
        cluster,
        std::slice::from_ref(&component),
        &manifests_installer,
        &readiness_probe,
        COMPONENT_TIMEOUT,
    )
    .await
    .map_err(|error| format!("install_stack failed: {error}"))?;

    if installed.components.len() != 1 || installed.components[0].component != component.name {
        return Err(format!("unexpected InstalledStack contents: {installed:?}"));
    }

    assert_ca_bundles_populated(cluster, &webhook_configuration_names).await?;

    let _ = tokio::fs::remove_dir_all(&run_paths_root).await;
    Ok(())
}

/// Runs `bash scripts/build-test-images.sh <cluster_name>` as a real
/// subprocess through this project's own `ProcessRunner` (never a
/// direct shell call) — see this module's own documentation for why
/// this test drives the real script rather than reimplementing its
/// `docker build`/`kind load` steps in Rust.
async fn build_and_load_image(
    runner: &dyn ProcessRunner,
    cluster_name: &str,
) -> Result<(), String> {
    let repo_root = repo_root();
    let script = repo_root.join("scripts/build-test-images.sh");
    if !script.is_file() {
        return Err(format!("script not found at {}", script.display()));
    }

    let spec = CommandSpec {
        program: "bash".into(),
        args: vec![script.into_os_string(), cluster_name.into()],
        cwd: Some(repo_root),
        env: BTreeMap::new(),
        sensitive_env_keys: BTreeSet::new(),
        timeout: BUILD_AND_LOAD_TIMEOUT,
        spill_dir: None,
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

/// A fresh `RunPaths` scratch workspace for the installer's own
/// artifacts (in particular `kubectl`'s isolated `--cache-dir` —
/// `admissionlab_installer::manifests`'s own module documentation
/// explains why that isolation matters). Unrelated to
/// `recipes/test-webhook`'s own directory, which is loaded directly, in
/// place, from the checked-in repository — Task 2.10 removed the
/// scratch copy of the recipe file this helper used to also produce;
/// only the installer's own run workspace is left to prepare here.
/// Returns the scratch root alongside `RunPaths` so the caller can
/// clean it up once the install finishes.
async fn prepare_run_paths() -> Result<(PathBuf, RunPaths), String> {
    let root = unique_scratch_dir("run-paths");
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();
    let paths = store
        .create_run(&run_id)
        .await
        .map_err(|error| format!("failed to prepare a run workspace for the installer: {error}"))?;
    Ok((root, paths))
}

/// Fetches each named webhook configuration from `cluster` and confirms
/// its `caBundle` was actually populated by the `bootstrap` init
/// container — not merely that the object exists (that much is already
/// covered by `install_stack`'s own `webhookConfigurationPresent`
/// readiness checks).
///
/// Each name is looked up as a `ValidatingWebhookConfiguration` first
/// and then as a `MutatingWebhookConfiguration`, exactly as
/// `admissionlab_installer::readiness` resolves the same check type, so
/// the recipe's readiness list stays the single place object names are
/// written.
async fn assert_ca_bundles_populated(
    cluster: &ClusterHandle,
    names: &[String],
) -> Result<(), String> {
    let kubeconfig = Kubeconfig::read_from(&cluster.kubeconfig)
        .map_err(|error| format!("failed to read kubeconfig: {error}"))?;
    let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
        .await
        .map_err(|error| format!("failed to build kube config: {error}"))?;
    let client = Client::try_from(config)
        .map_err(|error| format!("failed to build kube client: {error}"))?;

    let validating: Api<ValidatingWebhookConfiguration> = Api::all(client.clone());
    let mutating: Api<MutatingWebhookConfiguration> = Api::all(client);

    for name in names {
        let bundles = match validating.get_opt(name).await {
            Ok(Some(found)) => ca_bundles(found.webhooks.map(|hooks| {
                hooks
                    .into_iter()
                    .map(|hook| hook.client_config.ca_bundle)
                    .collect()
            })),
            Ok(None) => match mutating.get_opt(name).await {
                Ok(Some(found)) => ca_bundles(found.webhooks.map(|hooks| {
                    hooks
                        .into_iter()
                        .map(|hook| hook.client_config.ca_bundle)
                        .collect()
                })),
                Ok(None) => return Err(format!("no webhook configuration named {name:?} exists")),
                Err(error) => return Err(format!("failed to fetch {name:?}: {error}")),
            },
            Err(error) => return Err(format!("failed to fetch {name:?}: {error}")),
        };

        let bundles = bundles.ok_or_else(|| format!("{name:?} has no `webhooks` entries"))?;
        if bundles.is_empty() {
            return Err(format!("{name:?} has no `webhooks` entries"));
        }
        for bundle in bundles {
            let bundle = bundle.ok_or_else(|| {
                format!("{name:?}: caBundle was never populated by the bootstrap init container")
            })?;
            if bundle.is_empty() {
                return Err(format!("{name:?}: caBundle is present but empty"));
            }
            let text = String::from_utf8_lossy(&bundle);
            if !text.contains("BEGIN CERTIFICATE") {
                return Err(format!(
                    "{name:?}: caBundle does not look like a PEM certificate: {text:?}"
                ));
            }
        }
    }
    Ok(())
}

/// Flattens a fetched configuration's `webhooks[].clientConfig.caBundle`
/// fields into plain byte vectors, dropping the two generated webhook
/// entry types (`ValidatingWebhook`/`MutatingWebhook`) that have no
/// common trait to abstract over here.
fn ca_bundles(
    webhooks: Option<Vec<Option<k8s_openapi::ByteString>>>,
) -> Option<Vec<Option<Vec<u8>>>> {
    Some(
        webhooks?
            .into_iter()
            .map(|bundle| bundle.map(|bytes| bytes.0))
            .collect(),
    )
}

/// This checkout's own repository root — three levels above this
/// crate's own `CARGO_MANIFEST_DIR`
/// (`crates/admissionlab-test-webhook/tests` -> `crates/admissionlab-test-webhook`
/// -> `crates` -> repo root — `CARGO_MANIFEST_DIR` already points at the
/// crate directory, so two `..` reach the root, matching every other
/// `CARGO_MANIFEST_DIR`-anchored path in this workspace's own test
/// suites).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("this crate's own CARGO_MANIFEST_DIR/../.. must exist")
}

/// A fresh, guaranteed-unique scratch directory under the OS temp dir.
fn unique_scratch_dir(label: &str) -> PathBuf {
    let unique = RunId::generate();
    std::env::temp_dir().join(format!(
        "admissionlab-test-webhook-{label}-{}",
        unique.as_str()
    ))
}

fn unique_root() -> PathBuf {
    unique_scratch_dir("kind-smoke")
}
