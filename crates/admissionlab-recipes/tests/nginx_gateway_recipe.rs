//! NGINX Gateway Fabric certification test (ROADMAP Task 8.1) -- the
//! second Gateway API implementation this project certifies, and a
//! deliberate near-clone of `tests/istio_gateway_recipe.rs` so that a
//! diff of the two files is the list of things that genuinely differ
//! between two implementations of one API.
//!
//! Two halves, deliberately in one file:
//!
//! - A **metadata half** that needs no cluster and runs under plain
//!   `cargo test --workspace`: both recipe documents in
//!   `recipes/nginx-gateway-fabric/` load, the chart pin is the OCI
//!   reference NGF actually publishes, the vendored Gateway API CRD
//!   bundle is still byte-identical both to the upstream release it
//!   claims *and* to `recipes/istio-gateway/`'s own copy of it, the
//!   fixtures' embedded echo backends still match
//!   `fixtures/gateway/backends/`, the portable fixtures still carry no
//!   vendor object, and no classification field can enter the recipe
//!   schema.
//! - An **integration half** (`#[ignore]`d -- needs Docker and `kind`)
//!   that creates a real cluster per certified Kubernetes version,
//!   installs the real two-component stack through
//!   `admissionlab_installer::install_stack`, and drives all three
//!   fixtures end to end: apply, wait for reconciliation, resolve the
//!   data-plane endpoint from the recipe's own `gatewayEndpoint`
//!   strategy, port-forward to it, and send one real HTTP request.
//!
//! # What differs from Istio, measured rather than assumed
//!
//! Three things, and each one is why this file is not simply
//! `istio_gateway_recipe.rs` with two constants changed:
//!
//! 1. **NGF programs a Gateway on `kind` with no vendor override.**
//!    Istio's provisioned data-plane `Service` defaults to
//!    `type: LoadBalancer`, gets no address on a bare `kind` cluster, and
//!    Istio therefore reports `Programmed=False (AddressNotAssigned)`
//!    permanently -- which is why every Istio fixture carries a
//!    `ConfigMap` forcing `ClusterIP`. NGF's Service defaults to
//!    `LoadBalancer` too and its external address stays `<pending>` just
//!    the same, but NGF reports `Programmed=True` regardless: its
//!    `Programmed` is about the data plane it started, not about an
//!    address. So `fixtures/gateway/nginx/{same,cross}-namespace.yaml`
//!    need no vendor object at all, and the NGF equivalent of Istio's
//!    override gets its own labeled fixture instead
//!    ([`SCENARIOS`]`[2]`).
//! 2. **The endpoint strategy names no port.** Istio's Service exposes
//!    two ports (the listener's, plus `status-port` 15021), so its recipe
//!    must say `portName: http`. NGF's exposes exactly the listener
//!    ports, named `port-<number>` -- derived from the port rather than
//!    chosen by the vendor -- so there is no stable name to select on and
//!    a single-listener Gateway needs no selection at all. See
//!    `recipes/nginx-gateway-fabric/recipe.yaml`'s own comment.
//! 3. **The chart is only published to an OCI registry.** NGF gives no
//!    `helm repo add` command and F5's classic repository does not carry
//!    the chart, so this is the first recipe in the project whose
//!    `install.chart` is an `oci://` reference. `helm repo add` cannot
//!    parse one, and `admissionlab_installer::helm` skips that step for
//!    it. The alternative install method, raw manifests, is closed to NGF
//!    for a separate measured reason recorded in
//!    `recipes/nginx-gateway-fabric/README.md` (its `NginxProxy` CRD is
//!    320,611 bytes, and client-side `kubectl apply` cannot store an
//!    object over 262,144 bytes in its own annotation).
//!
//! # Two components, one stack
//!
//! `recipes/nginx-gateway-fabric/` holds **two** recipe documents, and
//! this test installs both, in order: `gateway-api-crds-nginx` (the
//! vendored Gateway API CRD bundle, applied as raw manifests) and then
//! `nginx-gateway-fabric` (the chart via Helm). NGF's own documentation
//! requires that order. NGF's *own* CRDs are not a third component: the
//! chart carries them in its `crds/` directory and Helm installs them
//! directly.
//!
//! Order is asserted here rather than inherited from the loader's
//! filename sort: [`ordered_components`] looks each recipe up by name
//! and builds the list explicitly.
//!
//! # "Converged" means stable, not finished
//!
//! Inherited wholesale from `tests/istio_gateway_recipe.rs`, and it
//! matters here for a second reason. `wait_for_route_reconciliation`
//! answers "has this route's status stopped changing?", which a status
//! can satisfy while the implementation is still working. NGF reaches
//! exactly such a state in [`SCENARIOS`]`[2]`: because
//! `admissionlab_gateway::apply` applies unknown kinds last, the
//! `Gateway` there is created before the `NginxProxy` it references, and
//! NGF publishes a settled `ResolvedRefs=False (ParametersRefInvalid)`
//! for about a second until the `NginxProxy` arrives.
//! [`observe_until_reconciled`] re-runs the whole convergence rule until
//! every certified condition is `True`, or until
//! `RECONCILIATION_TIMEOUT`, at which point it reports the specific
//! condition that never became true rather than a bare timeout.
//!
//! # Readiness before traffic: two Deployments, not a sleep
//!
//! The data-plane `Deployment` NGF provisions and each fixture's echo
//! backend are both waited on before the status assertion and therefore
//! before any traffic, through `admissionlab_installer::KubeReadinessProbe`
//! -- the same probe the recipe pipeline itself uses.
//!
//! # Cleanup discipline
//!
//! `ScratchRoot` and `ClusterGuard` are copied from
//! `tests/istio_gateway_recipe.rs` -- see that file for why
//! `ClusterGuard`'s `Drop` only warns and why both guards are bound
//! before any fallible step. [`probe_through_port_forward`] closes its
//! `kubectl port-forward` child on every path, including the one where
//! the probe itself failed, so a failed assertion never leaks a process.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use admissionlab_cluster::{KindClusterManager, cluster_name};
use admissionlab_core::{
    ArtifactStore, ClusterError, ClusterHandle, ClusterManager, ClusterSpec, CommandSpec,
    ProcessRunner, RunId, RunPaths, Side, TokioProcessRunner, sha256_hex,
};
use admissionlab_gateway::{
    CONDITION_ACCEPTED, CONDITION_PROGRAMMED, CONDITION_RESOLVED_REFS, ConditionState,
    GatewayEndpoint, GatewayEndpointResolver, HttpProbeContract, KubeGatewayEndpointResolver,
    ObservedCondition, ParentLookup, ReconciliationEvidence, RouteContract,
    apply_gateway_manifests, contract_gateway_identity, execute_http_probe,
    start_service_port_forward, wait_for_route_reconciliation,
};
use admissionlab_installer::{
    CompositeInstaller, HelmInstaller, KubeReadinessProbe, ManifestsInstaller, ReadinessProbe,
    install_stack,
};
use admissionlab_recipes::{
    CERTIFY_KUBERNETES_ENV, Capability, GATEWAY_NAME_LABEL, GatewayEndpointStrategy, InstallMethod,
    ReadinessCheck, Recipe, load_recipe_compatibility, load_recipe_overrides,
    narrow_certified_versions,
};
use admissionlab_spec::ResolvedComponent;

// ---------------------------------------------------------------------
// What this recipe pins, restated here so a hand-edit to any of it fails
// a millisecond-scale test rather than a multi-minute cluster run.
// ---------------------------------------------------------------------

/// The recipe that carries the `gatewayApi` capability.
const RECIPE_NAME: &str = "nginx-gateway-fabric";

/// The recipe that installs the Gateway API CRDs, composed first.
///
/// Named distinctly from `recipes/istio-gateway/`'s own
/// `gateway-api-crds` even though the two install byte-identical
/// bundles: recipe names are unique within one loaded set, and two
/// components that could ever be composed into one stack must be
/// nameable apart. See `recipes/nginx-gateway-fabric/README.md`'s
/// coexistence note.
const CRDS_RECIPE_NAME: &str = "gateway-api-crds-nginx";

/// The Gateway API release the vendored bundle is taken from, chosen
/// because `nginx/nginx-gateway-fabric` at tag `v2.6.7` declares
/// `sigs.k8s.io/gateway-api v1.5.1` in its own `go.mod`.
const GATEWAY_API_BUNDLE_VERSION: &str = "1.5.1";

/// SHA-256 of the upstream `standard-install.yaml` for
/// [`GATEWAY_API_BUNDLE_VERSION`], as downloaded from
/// `https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.5.1/standard-install.yaml`.
const GATEWAY_API_BUNDLE_SHA256: &str =
    "751002b3b91a87f7ae3bd2517c79a47a8d7ed6702901808a1cf9bd97d284f9b8";

/// This recipe's own vendored copy of that bundle, relative to the
/// repository root.
const VENDORED_BUNDLE: &str =
    "recipes/nginx-gateway-fabric/gateway-api/standard-install-v1.5.1.yaml";

/// `recipes/istio-gateway/`'s copy of the same bundle. Checked
/// byte-for-byte against [`VENDORED_BUNDLE`] so that the duplication the
/// path-confinement rule forces is duplication a machine notices.
const ISTIO_VENDORED_BUNDLE: &str =
    "recipes/istio-gateway/gateway-api/standard-install-v1.5.1.yaml";

/// The OCI chart reference this recipe installs.
const CHART: &str = "oci://ghcr.io/nginx/charts/nginx-gateway-fabric";

/// The registry path [`CHART`] is rooted at, recorded in `install.repo`.
const CHART_REPO: &str = "oci://ghcr.io/nginx/charts";

/// The pinned chart version, which is also the recipe's version and the
/// `compatibility/recipes.yaml` entry's version.
const CHART_VERSION: &str = "2.6.7";

/// The namespace the chart installs into.
const CONTROL_PLANE_NAMESPACE: &str = "nginx-gateway";

/// The control-plane `Deployment`. Named after the Helm RELEASE, which
/// this recipe leaves unset and therefore defaults to the recipe's own
/// name -- so this constant is `RECIPE_NAME` by construction, and
/// [`check_helm_install`] asserts that coupling rather than trusting it.
const CONTROL_PLANE_DEPLOYMENT: &str = "nginx-gateway-fabric";

/// The `GatewayClass` the chart creates, named by every fixture and
/// gated on by `recipe.yaml`'s readiness.
const GATEWAY_CLASS: &str = "nginx";

/// The `Gateway` every fixture creates (in different namespaces).
const GATEWAY_NAME: &str = "lab-gateway";

/// The `HTTPRoute` every fixture creates.
const ROUTE_NAME: &str = "echo-route";

/// The listener every fixture declares, and the `sectionName` every
/// route contract names.
const LISTENER_NAME: &str = "http";

/// The data-plane `Service` NGF provisions for [`GATEWAY_NAME`]:
/// `<gateway name>-nginx`. Never used to *find* the Service (the recipe
/// selects it by label), only to assert that the label selector found
/// the Service this test expects.
const DATA_PLANE_SERVICE: &str = "lab-gateway-nginx";

// ---------------------------------------------------------------------
// Timeouts. Every one is an absolute bound on a real operation, sized
// from a measurement recorded in `recipes/nginx-gateway-fabric/README.md`.
// ---------------------------------------------------------------------

/// Bounds one component's install-plus-readiness. The heavier of the two
/// components is `nginx-gateway-fabric` (an OCI chart pull plus a
/// cert-generator `Job` plus the control plane reaching `Available`,
/// measured at 12.5 seconds for `helm upgrade --install` alone on a warm
/// node); `gateway-api-crds-nginx` is a single `kubectl apply` of one
/// 1 MB file plus four CRD `Established` checks, measured at well under
/// a second. 300 seconds leaves room for a cold image pull on a loaded
/// CI runner.
const COMPONENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Bounds one route's convergence. See this file's module documentation
/// for why this is generous against a measured convergence of seconds.
const RECONCILIATION_TIMEOUT: Duration = Duration::from_secs(120);

/// How long [`observe_until_reconciled`] waits before re-running the
/// convergence rule after an observation that was stable but not yet the
/// status this recipe certifies.
const REOBSERVE_INTERVAL: Duration = Duration::from_millis(500);

/// Bounds the wait for one `Deployment` to report `Available` -- the
/// gateway data plane, and each fixture's echo backend. The echo backend
/// pulls an already-loaded local image; the data plane pulls
/// `ghcr.io/nginx/nginx-gateway-fabric/nginx` from the network on first
/// use, which is why this is minutes rather than seconds.
const DEPLOYMENT_READY_TIMEOUT: Duration = Duration::from_secs(180);

/// Bounds one `bash scripts/build-test-images.sh <cluster>` run. Mirrors
/// `tests/istio_gateway_recipe.rs`'s own bound for the same script.
const BUILD_AND_LOAD_TIMEOUT: Duration = Duration::from_mins(15);

/// Bounds one `kubectl get service` this test runs to read back the data
/// plane's `spec.type`. One namespaced read against a cluster that has
/// already answered several.
const KUBECTL_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Bounds how long [`check_data_plane_service_type`] waits for NGF to
/// have provisioned the `Service` a fixture asked for. Measured at about
/// a second for the one fixture that changes it, and already elapsed by
/// the time this runs; sized like [`RECONCILIATION_TIMEOUT`] because it
/// is the same kind of bound on the same controller.
const SERVICE_TYPE_TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------
// The three fixtures, and what each one is contracted to do.
// ---------------------------------------------------------------------

/// One fixture: which file, which namespaces, which backend a request
/// through it must reach, and what NGF is expected to have provisioned
/// for it.
struct Scenario {
    /// The `RouteContract` id, and the label this test's own error
    /// messages use.
    id: &'static str,
    /// The fixture file, relative to `fixtures/gateway/nginx/`.
    manifest: &'static str,
    /// The namespace holding the `Gateway` and the `HTTPRoute`.
    route_namespace: &'static str,
    /// The namespace holding the echo backend.
    backend_namespace: &'static str,
    /// The backend's name, which is also the identity it answers with.
    backend: &'static str,
    /// The `Host` header the probe sends, matching the route's
    /// `hostnames` entry.
    host: &'static str,
    /// The `spec.type` of the data-plane `Service` NGF must have
    /// provisioned for this fixture's `Gateway`.
    ///
    /// This is the whole point of the third scenario and the reason it
    /// is not a duplicate of the first: `LoadBalancer` is NGF's default
    /// (and, on `kind`, an address that never arrives -- which NGF
    /// programs the Gateway despite), `ClusterIP` is what an
    /// `NginxProxy` referenced from `Gateway.spec.infrastructure
    /// .parametersRef` changes it to. A test that only ever probed
    /// traffic could not tell the two apart, because both serve it.
    expected_service_type: &'static str,
}

/// Every fixture, in the order this test runs them. The second is the
/// first with the backend moved across a namespace boundary and a
/// `ReferenceGrant` added; the third is the first with NGF's own
/// data-plane override attached. Running them in this order means a
/// failure in the simplest one is diagnosed before either of the others
/// is exercised.
const SCENARIOS: [Scenario; 3] = [
    Scenario {
        id: "nginx-same-namespace",
        manifest: "same-namespace.yaml",
        route_namespace: "admissionlab-nginx-gateway-same",
        backend_namespace: "admissionlab-nginx-gateway-same",
        backend: "echo-a",
        host: "same.nginx.gateway.admissionlab.test",
        expected_service_type: "LoadBalancer",
    },
    Scenario {
        id: "nginx-cross-namespace",
        manifest: "cross-namespace.yaml",
        route_namespace: "admissionlab-nginx-gateway-cross-route",
        backend_namespace: "admissionlab-nginx-gateway-cross-backend",
        backend: "echo-b",
        host: "cross.nginx.gateway.admissionlab.test",
        expected_service_type: "LoadBalancer",
    },
    Scenario {
        id: "nginx-infrastructure-override",
        manifest: "nginx-infrastructure-override.yaml",
        route_namespace: "admissionlab-nginx-gateway-override",
        backend_namespace: "admissionlab-nginx-gateway-override",
        backend: "echo-a",
        host: "override.nginx.gateway.admissionlab.test",
        expected_service_type: "ClusterIP",
    },
];

/// The one scenario whose fixture is deliberately NGF-specific
/// (ROADMAP Task 8.1 Step 4). Everything before it in [`SCENARIOS`] is
/// the portable pack.
const NGINX_SPECIFIC_SCENARIO: usize = 2;

// ---------------------------------------------------------------------
// Cleanup guards. Copied from `tests/istio_gateway_recipe.rs` -- see
// this file's module documentation ("Cleanup discipline").
// ---------------------------------------------------------------------

struct ScratchRoot(PathBuf);

impl Drop for ScratchRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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
// The real, end-to-end certification test.
// ---------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Docker and kind"]
async fn nginx_gateway_recipe_routes_real_traffic_for_every_certified_kubernetes_version() {
    let outcome = run_certification().await;
    outcome.expect("nginx-gateway-fabric recipe certification test");
}

/// Validates every piece of checked-in metadata first (cheap, no
/// cluster), then runs the full install-and-route scenario once per
/// Kubernetes version `compatibility/recipes.yaml` certifies, each in
/// its own disposable cluster.
///
/// Every listed version is attempted even if an earlier one failed, so
/// one run says which certified versions regressed rather than stopping
/// at the first.
async fn run_certification() -> Result<(), String> {
    let recipes = load_nginx_gateway_recipes()?;
    check_recipe_metadata(&recipes)?;
    let components = ordered_components(&recipes)?;
    let strategy = gateway_endpoint_strategy(&recipes)?;
    let versions = certified_kubernetes_versions()?;

    let root = unique_root();
    // Bound before any fallible step below, so `root` cannot leak.
    let _scratch_root_guard = ScratchRoot(root.clone());
    let store = ArtifactStore::new(&root);

    let mut problems = Vec::new();
    for version in &versions {
        if let Err(error) = run_one_version(&store, &components, &strategy, version).await {
            problems.push(format!("[kubernetes {version}]\n{error}"));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n\n"))
    }
}

/// Creates one cluster at `kubernetes_version`, runs the whole scenario
/// against it, and always deletes the cluster before returning --
/// including when the scenario failed.
async fn run_one_version(
    store: &ArtifactStore,
    components: &[ResolvedComponent],
    strategy: &GatewayEndpointStrategy,
    kubernetes_version: &str,
) -> Result<(), String> {
    let manager = KindClusterManager::new(Arc::new(TokioProcessRunner::new()));
    let runner = TokioProcessRunner::new();

    let run_id = RunId::generate();
    let paths = store
        .create_run(&run_id)
        .await
        .map_err(|error| format!("failed to prepare a run workspace: {error}"))?;
    let spec = cluster_spec(&run_id, kubernetes_version)?;

    let handle = manager
        .create(&spec, &paths)
        .await
        .map_err(|error| format!("failed to create cluster: {error}"))?;
    let guard = ClusterGuard::new(handle);

    let outcome = install_and_route(&runner, guard.handle(), &paths, components, strategy).await;

    let mut problems: Vec<String> = outcome.err().into_iter().collect();
    if let Err(error) = guard.cleanup(&manager).await {
        problems.push(format!("failed to delete the cluster: {error}"));
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// Builds the [`ClusterSpec`] for one certified Kubernetes version,
/// resolving its pinned node image through `compatibility/kubernetes.yaml`
/// exactly as every other real-cluster test in this workspace does.
fn cluster_spec(run_id: &RunId, kubernetes_version: &str) -> Result<ClusterSpec, String> {
    let matrix = admissionlab_cluster::load_matrix()
        .map_err(|error| format!("failed to load the Kubernetes compatibility matrix: {error}"))?;
    let resolved =
        admissionlab_cluster::resolve_node_image(kubernetes_version, &matrix).map_err(|error| {
            format!("failed to resolve a node image for {kubernetes_version:?}: {error}")
        })?;
    let name = cluster_name(Side::Baseline, run_id)
        .map_err(|error| format!("failed to build a cluster name: {error}"))?;
    Ok(ClusterSpec {
        side: Side::Baseline,
        name,
        kubernetes_version: resolved.version.clone(),
        node_image: resolved.pinned_image.clone(),
        images: Vec::new(),
    })
}

/// Loads the echo image into `cluster`, installs both components through
/// the real installer pipeline, then runs every scenario in
/// [`SCENARIOS`] against the result.
async fn install_and_route(
    runner: &dyn ProcessRunner,
    cluster: &ClusterHandle,
    paths: &RunPaths,
    components: &[ResolvedComponent],
    strategy: &GatewayEndpointStrategy,
) -> Result<(), String> {
    // Before the stack, not after: `kind load docker-image` is
    // independent of anything NGF does, and doing it first means an
    // image build failure is reported without having installed a gateway
    // nobody is going to use.
    build_and_load_test_images(runner, &cluster.spec.name).await?;

    let helm = Arc::new(HelmInstaller::new(
        Arc::new(TokioProcessRunner::new()),
        paths,
    ));
    let manifests = Arc::new(ManifestsInstaller::new(
        Arc::new(TokioProcessRunner::new()),
        paths,
    ));
    let dispatcher = CompositeInstaller::new(helm, manifests);
    let readiness = KubeReadinessProbe::new();

    let installed = install_stack(
        cluster,
        components,
        &dispatcher,
        &readiness,
        COMPONENT_TIMEOUT,
    )
    .await
    .map_err(|error| format!("install_stack failed: {error}"))?;

    let installed_summary: Vec<(&str, &str)> = installed
        .components
        .iter()
        .map(|record| (record.component.as_str(), record.method.as_str()))
        .collect();
    if installed_summary != [(CRDS_RECIPE_NAME, "manifests"), (RECIPE_NAME, "helm")] {
        return Err(format!(
            "expected install_stack to install {CRDS_RECIPE_NAME:?} via manifests then \
             {RECIPE_NAME:?} via helm, got {installed_summary:?}"
        ));
    }

    for scenario in &SCENARIOS {
        run_scenario(cluster, &readiness, strategy, scenario)
            .await
            .map_err(|error| format!("[fixture {}]\n{error}", scenario.id))?;
    }
    Ok(())
}

/// Applies one fixture, proves it reconciled, proves NGF provisioned the
/// data plane this fixture asked for, then proves a real HTTP request
/// through it reaches the expected backend.
async fn run_scenario(
    cluster: &ClusterHandle,
    readiness: &KubeReadinessProbe,
    strategy: &GatewayEndpointStrategy,
    scenario: &Scenario,
) -> Result<(), String> {
    let manifest = fixtures_dir().join(scenario.manifest);
    let applied = apply_gateway_manifests(cluster, std::slice::from_ref(&manifest))
        .await
        .map_err(|error| format!("apply_gateway_manifests failed: {error}"))?;
    if !applied.objects.iter().any(|key| {
        key.resource == "httproutes"
            && key.name == ROUTE_NAME
            && key.namespace.as_deref() == Some(scenario.route_namespace)
    }) {
        return Err(format!(
            "the applied fixture did not include HTTPRoute {}/{ROUTE_NAME}; applied: {:?}",
            scenario.route_namespace,
            applied
                .objects
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        ));
    }

    // Both Deployments first -- before the status assertion, not merely
    // before the probe. See this file's module documentation
    // ("Readiness before traffic").
    for (namespace, name) in [
        (scenario.route_namespace, DATA_PLANE_SERVICE),
        (scenario.backend_namespace, scenario.backend),
    ] {
        wait_for_deployment(readiness, cluster, namespace, name).await?;
    }

    let contract = route_contract(scenario);
    let started = Instant::now();
    let (evidence, observations) = observe_until_reconciled(cluster, &contract).await?;
    println!(
        "[{}] reconciled in {:?} (wall clock {:?} over {observations} observation(s)), \
         converged={}",
        scenario.id,
        evidence.elapsed,
        started.elapsed(),
        evidence.converged
    );

    let identity = contract_gateway_identity(&contract);
    let endpoint = KubeGatewayEndpointResolver::new()
        .resolve(cluster, &identity, strategy)
        .await
        .map_err(|error| {
            format!("resolving the data-plane endpoint for Gateway {identity} failed: {error}")
        })?;
    let expected = GatewayEndpoint {
        namespace: scenario.route_namespace.to_owned(),
        service: DATA_PLANE_SERVICE.to_owned(),
        port: 80,
    };
    if endpoint != expected {
        return Err(format!(
            "the recipe's gatewayEndpoint strategy resolved to {endpoint}, expected {expected} \
             (the Service NGINX Gateway Fabric provisions for this Gateway)"
        ));
    }
    println!("[{}] endpoint resolved to {endpoint}", scenario.id);

    check_data_plane_service_type(cluster, scenario).await?;

    probe_through_port_forward(cluster, &endpoint, scenario, &contract).await
}

/// Waits until the provisioned data-plane `Service`'s `spec.type` is
/// what this fixture asked NGF for, or until
/// [`SERVICE_TYPE_TIMEOUT`] elapses.
///
/// This is the one assertion that distinguishes the NGF-specific fixture
/// from the portable one it is otherwise a copy of: both serve traffic
/// identically, and only the provisioned `Service` records whether the
/// `NginxProxy` reference took effect.
///
/// **Polled rather than read once, for the same reason
/// [`observe_until_reconciled`] exists.** The route's certified
/// conditions can all be `True` while the `NginxProxy` has not been
/// applied yet: `Accepted` is `True` even in NGF's
/// `InvalidParameters` state (the *reason* changes, the status does
/// not), and this recipe deliberately does not certify the Gateway's own
/// `ResolvedRefs`, which is the condition that actually moves. Measured
/// on a live cluster, NGF converts the already-provisioned Service from
/// `LoadBalancer` to `ClusterIP` about a second after the `NginxProxy`
/// appears -- comfortably inside the several seconds this test spends
/// waiting for two `Deployment`s first, which is why a single read
/// passed every time it was tried. A single read that passes every time
/// it is tried on one machine is still a race, and polling costs one
/// `kubectl` call in the common case because the first read already
/// matches.
///
/// Read with `kubectl` through the project's own [`ProcessRunner`]
/// (Global Constraint 12: argv, never a shell string) rather than by
/// adding a Kubernetes client to this test, because one field of one
/// object does not justify a second way of talking to a cluster in a
/// file that already has three.
async fn check_data_plane_service_type(
    cluster: &ClusterHandle,
    scenario: &Scenario,
) -> Result<(), String> {
    let deadline = Instant::now() + SERVICE_TYPE_TIMEOUT;
    let mut reads: u32 = 0;
    loop {
        reads = reads.saturating_add(1);
        let actual = read_data_plane_service_type(cluster, scenario).await?;
        if actual == scenario.expected_service_type {
            println!(
                "[{}] data-plane Service {}/{DATA_PLANE_SERVICE} is type {actual} \
                 ({reads} read(s))",
                scenario.id, scenario.route_namespace
            );
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "expected NGINX Gateway Fabric to provision Service \
                 {}/{DATA_PLANE_SERVICE} with type {:?} within {SERVICE_TYPE_TIMEOUT:?}, and it \
                 was still {actual:?} after {reads} read(s) -- for the override fixture this \
                 means the Gateway.spec.infrastructure.parametersRef NginxProxy never took \
                 effect",
                scenario.route_namespace, scenario.expected_service_type
            ));
        }
        tokio::time::sleep(REOBSERVE_INTERVAL).await;
    }
}

/// One `kubectl get service … -o jsonpath={.spec.type}` against the
/// cluster, returning the trimmed value.
async fn read_data_plane_service_type(
    cluster: &ClusterHandle,
    scenario: &Scenario,
) -> Result<String, String> {
    let spec = CommandSpec {
        program: "kubectl".into(),
        args: vec![
            "get".into(),
            "service".into(),
            DATA_PLANE_SERVICE.into(),
            "--namespace".into(),
            scenario.route_namespace.into(),
            "--kubeconfig".into(),
            cluster.kubeconfig.clone().into_os_string(),
            "-o".into(),
            "jsonpath={.spec.type}".into(),
        ],
        cwd: None,
        env: BTreeMap::new(),
        sensitive_env_keys: BTreeSet::new(),
        timeout: KUBECTL_READ_TIMEOUT,
    };
    let result = TokioProcessRunner::new()
        .run(spec)
        .await
        .map_err(|error| format!("failed to read the data-plane Service's type: {error}"))?;
    if !result.status.success() {
        return Err(format!(
            "`kubectl get service {DATA_PLANE_SERVICE}` exited with {}\nstderr:\n{}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&result.stdout).trim().to_owned())
}

/// Opens a port-forward, sends the contract's one probe through it, and
/// closes the forward on every path -- including the one where the probe
/// failed or its assertions did.
async fn probe_through_port_forward(
    cluster: &ClusterHandle,
    endpoint: &GatewayEndpoint,
    scenario: &Scenario,
    contract: &RouteContract,
) -> Result<(), String> {
    // A fresh `TokioProcessRunner` rather than the pipeline's: this call
    // takes a `ProcessSpawner` (a long-lived child whose stdout is read
    // line by line), not a `ProcessRunner` (a command run to
    // completion).
    let forward = start_service_port_forward(&TokioProcessRunner::new(), cluster, endpoint)
        .await
        .map_err(|error| format!("start_service_port_forward to {endpoint} failed: {error}"))?;
    let probe = contract
        .probes
        .first()
        .ok_or_else(|| "this test's own route contract must carry one probe".to_string())?;
    let outcome = execute_http_probe(forward.local_addr, probe).await;

    let closed = forward.close().await;

    let result = outcome.map_err(|error| format!("execute_http_probe failed: {error}"));
    let checked = result.and_then(|response| {
        println!(
            "[{}] probe -> status {} backend {:?} in {:?} ({} attempt(s))",
            scenario.id, response.status, response.backend, response.elapsed, response.attempts
        );
        if response.status != probe.expected_status {
            return Err(format!(
                "expected HTTP {} through the Gateway, got {} (headers: {:?})",
                probe.expected_status, response.status, response.response_headers
            ));
        }
        if response.backend.as_deref() != probe.expected_backend.as_deref() {
            return Err(format!(
                "expected the request to reach backend {:?}, but the echo response identified \
                 {:?} -- the route resolved to the wrong workload",
                probe.expected_backend, response.backend
            ));
        }
        Ok(())
    });

    match (checked, closed) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(format!("failed to close the port-forward: {error}")),
        (Err(probe_error), Err(close_error)) => Err(format!(
            "{probe_error}\nadditionally, failed to close the port-forward: {close_error}"
        )),
    }
}

/// Observes the route until its status is not merely *stable* but
/// *correct*, or until [`RECONCILIATION_TIMEOUT`] elapses. See this
/// file's module documentation for the settled-but-not-finished state
/// NGF publishes for the override fixture, which is what this loop
/// exists to ride out.
async fn observe_until_reconciled(
    cluster: &ClusterHandle,
    contract: &RouteContract,
) -> Result<(ReconciliationEvidence, u32), String> {
    let deadline = Instant::now() + RECONCILIATION_TIMEOUT;
    let mut observations: u32 = 0;
    loop {
        observations = observations.saturating_add(1);
        let evidence = wait_for_route_reconciliation(cluster, contract, deadline)
            .await
            .map_err(|error| format!("wait_for_route_reconciliation failed: {error}"))?;
        match assert_reconciled(contract, &evidence) {
            Ok(()) => return Ok((evidence, observations)),
            Err(reason) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "the route never reached the certified status within \
                         {RECONCILIATION_TIMEOUT:?} ({observations} observation(s)); the last \
                         one said: {reason}"
                    ));
                }
                tokio::time::sleep(REOBSERVE_INTERVAL).await;
            }
        }
    }
}

/// Asserts the full convergence claim: the route converged, its
/// `GatewayClass` is `Accepted`, its `Gateway` is `Accepted` and
/// `Programmed`, and its own parent entry is `Accepted` with
/// `ResolvedRefs` -- every one of them `True` **and** observed at the
/// object's current generation.
fn assert_reconciled(
    contract: &RouteContract,
    evidence: &ReconciliationEvidence,
) -> Result<(), String> {
    if !evidence.converged {
        return Err(format!(
            "the route never converged within {RECONCILIATION_TIMEOUT:?}; diagnostics: {:?}, \
             gateway conditions: {:?}, route parents: {:?}",
            evidence
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.clone())
                .collect::<Vec<_>>(),
            evidence.gateway.conditions,
            evidence.route.parents
        ));
    }

    let class = evidence.gateway_class.as_ref().ok_or_else(|| {
        format!(
            "the Gateway named no GatewayClass, or {GATEWAY_CLASS:?} does not exist on this cluster"
        )
    })?;
    if class.name != GATEWAY_CLASS {
        return Err(format!(
            "expected the Gateway to name GatewayClass {GATEWAY_CLASS:?}, got {:?}",
            class.name
        ));
    }
    // `None`, deliberately: `GatewayClassEvidence` carries no
    // `metadata.generation` of its own, so there is no current
    // generation to hold its `Accepted` fresh against.
    assert_true(&class.accepted, None, "GatewayClass")?;

    for condition in [CONDITION_ACCEPTED, CONDITION_PROGRAMMED] {
        assert_true(
            &evidence.gateway.condition(condition),
            Some(evidence.gateway.generation),
            "Gateway",
        )?;
    }

    let parent = match evidence.route.parent_for(contract) {
        ParentLookup::Found(parent) => parent,
        ParentLookup::Absent => {
            return Err(format!(
                "the HTTPRoute published no status entry for Gateway {}/{} listener {:?}; it has \
                 {} parent entr(ies)",
                contract.gateway_namespace,
                contract.gateway_name,
                contract.listener_name,
                evidence.route.parents.len()
            ));
        }
        ParentLookup::Ambiguous(count) => {
            return Err(format!(
                "the HTTPRoute published {count} status entries matching this contract's parent \
                 -- the contract cannot say which one it means"
            ));
        }
    };
    for condition in [CONDITION_ACCEPTED, CONDITION_RESOLVED_REFS] {
        assert_true(
            &parent.condition(condition),
            Some(evidence.route.generation),
            "HTTPRoute",
        )?;
    }
    Ok(())
}

/// Asserts one condition is `True` and, when `current_generation` is
/// known, that it was observed at that generation rather than an earlier
/// one.
fn assert_true(
    condition: &ObservedCondition,
    current_generation: Option<i64>,
    owner: &str,
) -> Result<(), String> {
    if condition.state != ConditionState::True {
        return Err(format!(
            "expected {owner} condition {:?} to be True, got {:?} (reason {:?})",
            condition.type_name, condition.state, condition.reason
        ));
    }
    match (condition.observed_generation, current_generation) {
        (Some(observed), Some(current)) if observed != current => Err(format!(
            "{owner} condition {:?} is True but stale: it was observed at generation {observed}, \
             and the object is now at generation {current}",
            condition.type_name
        )),
        _ => Ok(()),
    }
}

/// Waits for one `Deployment` to report `Available`, through the same
/// readiness probe the installer pipeline uses.
async fn wait_for_deployment(
    readiness: &KubeReadinessProbe,
    cluster: &ClusterHandle,
    namespace: &str,
    name: &str,
) -> Result<(), String> {
    let check = ReadinessCheck::DeploymentAvailable {
        namespace: namespace.to_owned(),
        name: name.to_owned(),
    };
    let evidence = readiness
        .wait(cluster, &check, Instant::now() + DEPLOYMENT_READY_TIMEOUT)
        .await
        .map_err(|error| format!("waiting for Deployment {namespace}/{name} failed: {error}"))?;
    if evidence.satisfied {
        Ok(())
    } else {
        Err(format!(
            "Deployment {namespace}/{name} never became Available within \
             {DEPLOYMENT_READY_TIMEOUT:?} -- a probe sent now would be answered by the data \
             plane's own error page, not by the backend"
        ))
    }
}

/// Runs `bash scripts/build-test-images.sh <cluster>`, exactly as
/// `tests/istio_gateway_recipe.rs` already does.
async fn build_and_load_test_images(
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
    };
    let result = runner
        .run(spec)
        .await
        .map_err(|error| format!("failed to run scripts/build-test-images.sh: {error}"))?;
    if result.status.success() {
        Ok(())
    } else {
        Err(format!(
            "scripts/build-test-images.sh exited with {}\nstdout:\n{}\nstderr:\n{}",
            result.status,
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        ))
    }
}

// ---------------------------------------------------------------------
// Loading and shaping what the recipes declare.
// ---------------------------------------------------------------------

/// Loads both recipe documents from `recipes/nginx-gateway-fabric/`.
///
/// Through `load_recipe_overrides`, not `load_builtin_recipes`: the CRD
/// half installs raw manifests by a path relative to its own directory,
/// and a built-in recipe's text is `include_str!`-ed into the binary
/// with no filesystem location to resolve one against.
fn load_nginx_gateway_recipes() -> Result<Vec<Recipe>, String> {
    let dir = repo_root().join("recipes/nginx-gateway-fabric");
    let recipes = load_recipe_overrides(&dir)
        .map_err(|error| format!("failed to load recipes/nginx-gateway-fabric: {error}"))?;
    if recipes.len() != 2 {
        return Err(format!(
            "expected exactly 2 recipe documents in recipes/nginx-gateway-fabric (the Gateway \
             API CRD bundle and NGINX Gateway Fabric itself), got {}: {:?}",
            recipes.len(),
            recipes.iter().map(|r| r.name.clone()).collect::<Vec<_>>()
        ));
    }
    Ok(recipes)
}

/// Finds one recipe by name.
fn find(recipes: &[Recipe], name: &str) -> Result<Recipe, String> {
    recipes
        .iter()
        .find(|recipe| recipe.name == name)
        .cloned()
        .ok_or_else(|| format!("no {name:?} recipe among the {} loaded", recipes.len()))
}

/// The two components, in install order: the Gateway API CRDs first,
/// then the implementation.
fn ordered_components(recipes: &[Recipe]) -> Result<Vec<ResolvedComponent>, String> {
    Ok(vec![
        component(find(recipes, CRDS_RECIPE_NAME)?),
        component(find(recipes, RECIPE_NAME)?),
    ])
}

fn component(recipe: Recipe) -> ResolvedComponent {
    ResolvedComponent {
        name: recipe.name,
        version: recipe.version,
        install: recipe.install,
        readiness: recipe.readiness,
        recipe_normalize_rules: recipe.normalize_rules,
        capabilities: recipe.capabilities,
    }
}

/// The `gatewayEndpoint` strategy the `nginx-gateway-fabric` component
/// declares.
fn gateway_endpoint_strategy(recipes: &[Recipe]) -> Result<GatewayEndpointStrategy, String> {
    find(recipes, RECIPE_NAME)?
        .gateway_endpoint
        .ok_or_else(|| format!("the {RECIPE_NAME:?} recipe declares no gatewayEndpoint"))
}

/// The `RouteContract` for one scenario: which Gateway, which route,
/// which listener, and the single probe its traffic assertion uses.
fn route_contract(scenario: &Scenario) -> RouteContract {
    RouteContract {
        id: scenario.id.to_owned(),
        gateway_namespace: scenario.route_namespace.to_owned(),
        gateway_name: GATEWAY_NAME.to_owned(),
        route_namespace: scenario.route_namespace.to_owned(),
        route_name: ROUTE_NAME.to_owned(),
        listener_name: Some(LISTENER_NAME.to_owned()),
        probes: vec![HttpProbeContract {
            host: scenario.host.to_owned(),
            // Any path under the route's `/` PathPrefix match. A
            // non-root path makes the echo response's own `path` field
            // meaningful evidence that the request arrived intact.
            path: "/gateway-probe".to_owned(),
            method: "GET".to_owned(),
            headers: BTreeMap::new(),
            expected_status: 200,
            expected_backend: Some(scenario.backend.to_owned()),
        }],
    }
}

/// Reads `compatibility/recipes.yaml`'s `nginx-gateway-fabric` entry and
/// returns every Kubernetes version this test must certify against.
fn certified_kubernetes_versions() -> Result<Vec<String>, String> {
    let compat = load_recipe_compatibility()
        .map_err(|error| format!("failed to load compatibility/recipes.yaml: {error}"))?;
    let entry = compat
        .entry(RECIPE_NAME)
        .ok_or_else(|| format!("compatibility/recipes.yaml has no {RECIPE_NAME:?} entry"))?;
    if entry.kubernetes.certified.is_empty() {
        return Err(format!(
            "compatibility/recipes.yaml's {RECIPE_NAME:?} entry has an empty certified list -- \
             this test does not silently skip certification"
        ));
    }
    // `ADMISSIONLAB_CERTIFY_KUBERNETES`, when set, narrows this to the
    // single certified version a generated CI matrix job owns.
    let requested = std::env::var(CERTIFY_KUBERNETES_ENV).ok();
    let certified = narrow_certified_versions(entry, requested.as_deref())
        .map_err(|error| error.to_string())?;
    Ok(certified
        .into_iter()
        .map(std::borrow::ToOwned::to_owned)
        .collect())
}

// ---------------------------------------------------------------------
// Metadata assertions, shared by the integration test above (which runs
// them before touching a cluster) and by the fast tests below.
// ---------------------------------------------------------------------

/// Every cheap check on what the two recipes declare.
fn check_recipe_metadata(recipes: &[Recipe]) -> Result<(), String> {
    let crds = find(recipes, CRDS_RECIPE_NAME)?;
    let gateway = find(recipes, RECIPE_NAME)?;
    check_helm_install(&gateway)?;
    check_crd_install(&crds)?;
    check_capability_and_endpoint(&gateway)?;
    check_readiness(&crds, &gateway)?;
    check_compatibility_entry(&gateway)
}

/// The chart pin, in full: the OCI reference, the registry it is rooted
/// at, the exact version, the namespace, and the two derived names the
/// readiness check below depends on.
fn check_helm_install(gateway: &Recipe) -> Result<(), String> {
    let InstallMethod::Helm(helm) = &gateway.install else {
        return Err(format!(
            "expected {RECIPE_NAME:?} to install via Helm, got {:?}",
            gateway.install
        ));
    };
    for (field, actual, expected) in [
        ("chart", helm.chart.as_str(), CHART),
        ("repo", helm.repo_url.as_str(), CHART_REPO),
        ("version", helm.version.as_str(), CHART_VERSION),
        (
            "namespace",
            helm.namespace.as_str(),
            CONTROL_PLANE_NAMESPACE,
        ),
    ] {
        if actual != expected {
            return Err(format!(
                "recipes/nginx-gateway-fabric/recipe.yaml's install.{field} is {actual:?}, \
                 expected {expected:?}"
            ));
        }
    }
    if !helm.chart.starts_with("oci://") {
        return Err(format!(
            "install.chart must be an oci:// reference -- NGINX Gateway Fabric publishes its \
             chart to no classic Helm repository at all; got {:?}",
            helm.chart
        ));
    }
    // The coupling the readiness check depends on: an unset
    // `releaseName` defaults to the recipe's own name, and the chart
    // names every object it creates after the release. Setting it would
    // rename the control-plane Deployment out from under
    // `check_readiness` below.
    if helm.release_name != CONTROL_PLANE_DEPLOYMENT {
        return Err(format!(
            "the Helm release name is {:?} but the readiness check gates on Deployment \
             {CONTROL_PLANE_NAMESPACE}/{CONTROL_PLANE_DEPLOYMENT} -- the chart names its objects \
             after the release, so these two must stay equal; leave install.releaseName unset",
            helm.release_name
        ));
    }
    if gateway.version != CHART_VERSION {
        return Err(format!(
            "the recipe pins version {:?} but its chart pins {CHART_VERSION:?}",
            gateway.version
        ));
    }
    Ok(())
}

/// The CRD component installs the vendored bundle; that bundle is still
/// byte-identical to the upstream release it claims to be.
fn check_crd_install(crds: &Recipe) -> Result<(), String> {
    if crds.version != GATEWAY_API_BUNDLE_VERSION {
        return Err(format!(
            "expected the {CRDS_RECIPE_NAME:?} recipe to pin Gateway API \
             {GATEWAY_API_BUNDLE_VERSION}, got {:?}",
            crds.version
        ));
    }
    let InstallMethod::Manifests(manifests) = &crds.install else {
        return Err(format!(
            "expected {CRDS_RECIPE_NAME:?} to install raw manifests, got {:?}",
            crds.install
        ));
    };
    let expected = repo_root().join(VENDORED_BUNDLE);
    if manifests.paths != [expected.clone()] {
        return Err(format!(
            "expected {CRDS_RECIPE_NAME:?} to install exactly [{}], got {:?}",
            expected.display(),
            manifests.paths
        ));
    }

    let bytes = std::fs::read(&expected)
        .map_err(|error| format!("failed to read {}: {error}", expected.display()))?;
    let digest = sha256_hex(&bytes);
    if digest != GATEWAY_API_BUNDLE_SHA256 {
        return Err(format!(
            "{} has sha256 {digest}, expected {GATEWAY_API_BUNDLE_SHA256} -- this file is a \
             byte-identical copy of the upstream Gateway API v{GATEWAY_API_BUNDLE_VERSION} \
             standard-channel release and must not be edited; re-download it if it needs to \
             change",
            expected.display()
        ));
    }
    Ok(())
}

/// The capability and the endpoint strategy are exactly what Task 6.6's
/// metadata is for, and exactly what NGF's provisioned Service can be
/// found by.
fn check_capability_and_endpoint(gateway: &Recipe) -> Result<(), String> {
    let expected_capabilities: BTreeSet<Capability> =
        [Capability::GatewayApi].into_iter().collect();
    if gateway.capabilities != expected_capabilities {
        return Err(format!(
            "expected the {RECIPE_NAME:?} recipe to declare exactly [gatewayApi], got {:?}",
            gateway.capabilities
        ));
    }
    let expected = GatewayEndpointStrategy::ServiceBySelector {
        namespace: "{gatewayNamespace}".to_owned(),
        selector: [(GATEWAY_NAME_LABEL.to_owned(), "{gatewayName}".to_owned())]
            .into_iter()
            .collect(),
        // Neither, deliberately -- see `recipe.yaml`'s own comment and
        // this file's module documentation ("What differs from Istio").
        port_name: None,
        port: None,
    };
    let actual = gateway
        .gateway_endpoint
        .as_ref()
        .ok_or_else(|| format!("the {RECIPE_NAME:?} recipe declares no gatewayEndpoint"))?;
    if *actual != expected {
        return Err(format!(
            "the gatewayEndpoint strategy is {actual:?}, expected {expected:?} -- NGINX Gateway \
             Fabric labels the Service it provisions with {GATEWAY_NAME_LABEL} and exposes only \
             the Gateway's own listener ports, so no port needs naming"
        ));
    }
    Ok(())
}

/// Readiness gates the NGF control plane, the Gateway API being served,
/// and NGF's own `GatewayClass` being accepted -- and the CRD component
/// gates every kind the fixtures use.
fn check_readiness(crds: &Recipe, gateway: &Recipe) -> Result<(), String> {
    let crd_kinds = established_crd_names(&crds.readiness);
    let expected_kinds = [
        "gatewayclasses.gateway.networking.k8s.io",
        "gateways.gateway.networking.k8s.io",
        "httproutes.gateway.networking.k8s.io",
        "referencegrants.gateway.networking.k8s.io",
    ];
    if crd_kinds != expected_kinds {
        return Err(format!(
            "expected {CRDS_RECIPE_NAME:?} to gate on every CRD kind the fixtures use \
             ({expected_kinds:?}), got {crd_kinds:?}"
        ));
    }

    let has_control_plane = gateway.readiness.iter().any(|check| {
        matches!(
            check,
            ReadinessCheck::DeploymentAvailable { namespace, name }
                if namespace == CONTROL_PLANE_NAMESPACE && name == CONTROL_PLANE_DEPLOYMENT
        )
    });
    if !has_control_plane {
        return Err(format!(
            "expected the {RECIPE_NAME:?} recipe to gate on Deployment \
             {CONTROL_PLANE_NAMESPACE}/{CONTROL_PLANE_DEPLOYMENT}, got {:?}",
            gateway.readiness
        ));
    }
    let gateway_crds = established_crd_names(&gateway.readiness);
    if gateway_crds
        != [
            "gateways.gateway.networking.k8s.io",
            "httproutes.gateway.networking.k8s.io",
        ]
    {
        return Err(format!(
            "expected the {RECIPE_NAME:?} recipe to independently gate on the Gateway/HTTPRoute \
             CRDs being Established, got {gateway_crds:?}"
        ));
    }
    let has_class = gateway.readiness.iter().any(|check| {
        matches!(
            check,
            ReadinessCheck::CustomResourceCondition { api_version, kind, name, condition_type, status, .. }
                if api_version == "gateway.networking.k8s.io/v1"
                    && kind == "GatewayClass"
                    && name == GATEWAY_CLASS
                    && condition_type == CONDITION_ACCEPTED
                    && status == "True"
        )
    });
    if has_class {
        Ok(())
    } else {
        Err(format!(
            "expected the {RECIPE_NAME:?} recipe to gate on GatewayClass {GATEWAY_CLASS:?} being \
             Accepted -- the only check that proves NGF's controller is actually reconciling on \
             this cluster"
        ))
    }
}

/// Every `CustomResourceDefinition`/`Established` check's target name,
/// in declaration order.
fn established_crd_names(checks: &[ReadinessCheck]) -> Vec<&str> {
    checks
        .iter()
        .filter_map(|check| match check {
            ReadinessCheck::CustomResourceCondition {
                api_version,
                kind,
                name,
                condition_type,
                status,
                ..
            } if api_version == "apiextensions.k8s.io/v1"
                && kind == "CustomResourceDefinition"
                && condition_type == "Established"
                && status == "True" =>
            {
                Some(name.as_str())
            }
            _ => None,
        })
        .collect()
}

/// `compatibility/recipes.yaml` and the recipe cannot silently disagree.
fn check_compatibility_entry(gateway: &Recipe) -> Result<(), String> {
    let compat = load_recipe_compatibility()
        .map_err(|error| format!("failed to load compatibility/recipes.yaml: {error}"))?;
    let entry = compat
        .entry(RECIPE_NAME)
        .ok_or_else(|| format!("compatibility/recipes.yaml has no {RECIPE_NAME:?} entry"))?;
    if entry.version != gateway.version {
        return Err(format!(
            "recipes/nginx-gateway-fabric/recipe.yaml pins version {:?} but \
             compatibility/recipes.yaml's {RECIPE_NAME:?} entry pins {:?}",
            gateway.version, entry.version
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Small standalone helpers.
// ---------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    repo_root().join("fixtures/gateway/nginx")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("this crate's own CARGO_MANIFEST_DIR/../.. must exist")
}

fn unique_root() -> PathBuf {
    let unique = RunId::generate();
    std::env::temp_dir().join(format!(
        "admissionlab-nginx-gateway-recipe-test-{}",
        unique.as_str()
    ))
}

// ---------------------------------------------------------------------
// Fast, no-cluster tests. These run under plain `cargo test --workspace`
// and are what actually stops the checked-in metadata rotting between
// integration runs.
// ---------------------------------------------------------------------

#[cfg(test)]
mod metadata_tests {
    use serde::Deserialize as _;

    use super::{
        CRDS_RECIPE_NAME, GATEWAY_API_BUNDLE_SHA256, GATEWAY_CLASS, GATEWAY_NAME,
        ISTIO_VENDORED_BUNDLE, LISTENER_NAME, NGINX_SPECIFIC_SCENARIO, RECIPE_NAME, ROUTE_NAME,
        SCENARIOS, ScratchRoot, VENDORED_BUNDLE, certified_kubernetes_versions,
        check_recipe_metadata, find, fixtures_dir, load_nginx_gateway_recipes, ordered_components,
        repo_root,
    };

    /// Everything `check_recipe_metadata` covers, in one place: the OCI
    /// chart pin, the release-name/Deployment-name coupling, the
    /// vendored bundle's digest, the capability and endpoint strategy,
    /// every readiness gate, and the `compatibility/recipes.yaml`
    /// cross-check. The integration test runs this same function before
    /// creating a cluster; this is what makes it also run in CI's fast
    /// lane.
    #[test]
    fn the_checked_in_recipe_metadata_is_exactly_what_this_task_certified() {
        let recipes = load_nginx_gateway_recipes().expect("both recipe documents must load");
        check_recipe_metadata(&recipes).expect("recipe metadata");
    }

    #[test]
    fn the_two_components_install_the_crds_before_the_implementation() {
        let recipes = load_nginx_gateway_recipes().expect("both recipe documents must load");
        let components = ordered_components(&recipes).expect("component list");
        let names: Vec<&str> = components
            .iter()
            .map(|component| component.name.as_str())
            .collect();
        assert_eq!(
            names,
            [CRDS_RECIPE_NAME, RECIPE_NAME],
            "NGINX Gateway Fabric's own documentation requires the Gateway API CRDs first"
        );
    }

    #[test]
    fn certification_targets_at_least_one_kubernetes_version() {
        let versions = certified_kubernetes_versions().expect("certified versions");
        assert!(!versions.is_empty());
    }

    /// The CRD recipe carries no capability at all: it installs an API,
    /// not an implementation of one.
    #[test]
    fn the_crd_component_claims_no_capability() {
        let recipes = load_nginx_gateway_recipes().expect("both recipe documents must load");
        let crds = find(&recipes, CRDS_RECIPE_NAME).expect("the CRD recipe");
        assert!(crds.capabilities.is_empty(), "got {:?}", crds.capabilities);
    }

    /// The two vendored Gateway API bundles -- this recipe's and
    /// `recipes/istio-gateway/`'s -- are the same bytes.
    ///
    /// They are separate files because `install.paths` is confined to
    /// the recipe's own directory tree, so a recipe that installs a
    /// vendored artifact must vendor it inside itself. This test is what
    /// makes that forced duplication safe: an edit to either copy, or a
    /// version bump applied to only one, fails here in milliseconds and
    /// names both paths.
    #[test]
    fn the_vendored_gateway_api_bundle_is_byte_identical_to_the_istio_gateway_copy() {
        let ours = repo_root().join(VENDORED_BUNDLE);
        let theirs = repo_root().join(ISTIO_VENDORED_BUNDLE);
        let ours_bytes = std::fs::read(&ours).expect("this recipe's vendored bundle must exist");
        let theirs_bytes =
            std::fs::read(&theirs).expect("recipes/istio-gateway's vendored bundle must exist");
        assert_eq!(
            admissionlab_core::sha256_hex(&ours_bytes),
            admissionlab_core::sha256_hex(&theirs_bytes),
            "{} and {} must hold byte-identical copies of the same upstream Gateway API release \
             ({GATEWAY_API_BUNDLE_SHA256}); if one recipe's implementation has moved to a \
             different Gateway API version, that is a deliberate change to make here, not a \
             drift to discover later",
            ours.display(),
            theirs.display()
        );
    }

    /// Global Constraint 6, proven by mutation rather than asserted by
    /// comment: adding a classification field to this recipe is a parse
    /// error, because the schema's `deny_unknown_fields` allow-list has
    /// no place for one.
    #[test]
    fn a_severity_field_cannot_be_added_to_this_recipe() {
        let original =
            std::fs::read_to_string(repo_root().join("recipes/nginx-gateway-fabric/recipe.yaml"))
                .expect("the recipe file must be readable");
        let dir = std::env::temp_dir().join(format!(
            "admissionlab-nginx-gateway-severity-{}",
            std::process::id()
        ));
        // Bound before the write, so the directory cannot outlive this
        // test even if an assertion below panics.
        let _guard = ScratchRoot(dir.clone());
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(
            dir.join("recipe.yaml"),
            format!("{original}\nseverity: critical\n"),
        )
        .expect("write mutated recipe");

        let error = admissionlab_recipes::load_recipe_overrides(&dir)
            .expect_err("a severity field must be rejected");
        let message = error.to_string();
        assert!(
            message.contains("severity"),
            "the error must name the offending field; got: {message}"
        );
    }

    /// Each fixture's inlined echo backend is
    /// `fixtures/gateway/backends/echo-{a,b}.yaml`'s own objects plus a
    /// namespace and nothing else.
    #[test]
    fn fixture_backends_match_the_shared_echo_backend_definition() {
        for scenario in &SCENARIOS {
            let shared = documents(
                &repo_root()
                    .join("fixtures/gateway/backends")
                    .join(format!("{}.yaml", scenario.backend)),
            );
            let fixture = documents(&fixtures_dir().join(scenario.manifest));

            for (kind, name) in [
                ("Service", scenario.backend),
                ("Deployment", scenario.backend),
            ] {
                let mut expected = document(&shared, kind, name)
                    .unwrap_or_else(|| panic!("{kind}/{name} must exist in the shared backend"));
                let mut actual = document(&fixture, kind, name)
                    .unwrap_or_else(|| panic!("{kind}/{name} must exist in {}", scenario.manifest));

                let namespace = take_namespace(&mut actual);
                assert_eq!(
                    namespace.as_deref(),
                    Some(scenario.backend_namespace),
                    "{kind}/{name} in {} must name its own namespace",
                    scenario.manifest
                );
                // The shared definition deliberately carries none; both
                // sides are compared with the field absent.
                assert_eq!(take_namespace(&mut expected), None);
                assert_eq!(
                    actual, expected,
                    "{kind}/{name} in {} has drifted from \
                     fixtures/gateway/backends/{}.yaml",
                    scenario.manifest, scenario.backend
                );
            }
        }
    }

    /// The fixtures and the recipe agree about the names this test's own
    /// constants encode: the class the recipe gates on, and the
    /// Gateway/route/listener the contracts name.
    #[test]
    fn the_fixtures_name_the_gateway_class_the_recipe_gates_on() {
        for scenario in &SCENARIOS {
            let docs = documents(&fixtures_dir().join(scenario.manifest));
            let gateway = document(&docs, "Gateway", GATEWAY_NAME)
                .unwrap_or_else(|| panic!("{} must declare a Gateway", scenario.manifest));
            assert_eq!(
                gateway
                    .get("spec")
                    .and_then(|spec| spec.get("gatewayClassName"))
                    .and_then(serde_norway::Value::as_str),
                Some(GATEWAY_CLASS)
            );

            let route = document(&docs, "HTTPRoute", ROUTE_NAME)
                .unwrap_or_else(|| panic!("{} must declare an HTTPRoute", scenario.manifest));
            let parent = route
                .get("spec")
                .and_then(|spec| spec.get("parentRefs"))
                .and_then(serde_norway::Value::as_sequence)
                .and_then(|refs| refs.first())
                .expect("the route must declare a parentRef");
            assert_eq!(
                parent.get("name").and_then(serde_norway::Value::as_str),
                Some(GATEWAY_NAME)
            );
            assert_eq!(
                parent
                    .get("sectionName")
                    .and_then(serde_norway::Value::as_str),
                Some(LISTENER_NAME)
            );
        }
    }

    /// Only the cross-namespace fixture carries a `ReferenceGrant`, and
    /// it grants exactly the reference its route makes.
    #[test]
    fn only_the_cross_namespace_fixture_grants_a_cross_namespace_reference() {
        for (index, scenario) in SCENARIOS.iter().enumerate() {
            if index == 1 {
                continue;
            }
            let docs = documents(&fixtures_dir().join(scenario.manifest));
            assert!(
                docs.iter()
                    .all(|doc| kind_of(doc) != Some("ReferenceGrant")),
                "{} keeps its backend beside its route and must need no ReferenceGrant",
                scenario.manifest
            );
        }

        let cross = &SCENARIOS[1];
        let docs = documents(&fixtures_dir().join(cross.manifest));
        let grant = docs
            .iter()
            .find(|doc| kind_of(doc) == Some("ReferenceGrant"))
            .expect("the cross-namespace fixture must carry a ReferenceGrant");
        assert_eq!(
            grant
                .get("metadata")
                .and_then(|metadata| metadata.get("namespace"))
                .and_then(serde_norway::Value::as_str),
            Some(cross.backend_namespace),
            "the grant belongs in the namespace that owns the referenced Service"
        );
        let from = grant
            .get("spec")
            .and_then(|spec| spec.get("from"))
            .and_then(serde_norway::Value::as_sequence)
            .and_then(|entries| entries.first())
            .expect("the grant must name what it permits references from");
        assert_eq!(
            from.get("namespace").and_then(serde_norway::Value::as_str),
            Some(cross.route_namespace)
        );
        let to = grant
            .get("spec")
            .and_then(|spec| spec.get("to"))
            .and_then(serde_norway::Value::as_sequence)
            .and_then(|entries| entries.first())
            .expect("the grant must name what it permits references to");
        assert_eq!(
            to.get("name").and_then(serde_norway::Value::as_str),
            Some(cross.backend),
            "the grant must name the one Service, not every Service in the namespace"
        );
    }

    /// ROADMAP Task 8.1 Steps 3 and 4, enforced rather than described:
    /// the portable fixtures contain **no** object from a vendor API
    /// group and **no** `infrastructure.parametersRef`, and the one
    /// fixture that does is the labeled, NGF-specific one.
    ///
    /// This is the test that keeps "run the same core contracts wherever
    /// portable" true over time. Without it, the natural next change --
    /// adding one small NGF-only knob to `same-namespace.yaml` because
    /// it is convenient -- would silently turn the portable pack into a
    /// second vendor pack, and the only trace would be a comment nobody
    /// re-reads.
    #[test]
    fn only_the_labeled_nginx_specific_fixture_carries_a_vendor_object() {
        for (index, scenario) in SCENARIOS.iter().enumerate() {
            let docs = documents(&fixtures_dir().join(scenario.manifest));
            let vendor_groups: Vec<String> = docs
                .iter()
                .filter_map(|doc| {
                    doc.get("apiVersion")
                        .and_then(serde_norway::Value::as_str)
                        .map(|api_version| {
                            api_version
                                .split_once('/')
                                .map_or("", |(group, _)| group)
                                .to_owned()
                        })
                })
                .filter(|group| {
                    !group.is_empty() && group != "apps" && group != "gateway.networking.k8s.io"
                })
                .collect();
            let has_parameters_ref = docs.iter().any(|doc| {
                doc.get("spec")
                    .and_then(|spec| spec.get("infrastructure"))
                    .and_then(|infrastructure| infrastructure.get("parametersRef"))
                    .is_some()
            });

            if index == NGINX_SPECIFIC_SCENARIO {
                assert_eq!(
                    vendor_groups,
                    ["gateway.nginx.org"],
                    "{} is the NGF-specific fixture and must carry exactly one NginxProxy",
                    scenario.manifest
                );
                assert!(
                    has_parameters_ref,
                    "{} must attach its NginxProxy through \
                     Gateway.spec.infrastructure.parametersRef",
                    scenario.manifest
                );
            } else {
                assert!(
                    vendor_groups.is_empty(),
                    "{} is part of the PORTABLE pack and must contain only core, apps and \
                     Gateway API objects; found objects in {vendor_groups:?}. Anything \
                     implementation-specific belongs in its own labeled fixture (ROADMAP Task \
                     8.1 Step 4)",
                    scenario.manifest
                );
                assert!(
                    !has_parameters_ref,
                    "{} is part of the PORTABLE pack and must not reference vendor \
                     infrastructure parameters",
                    scenario.manifest
                );
            }
        }
    }

    // -----------------------------------------------------------------
    // YAML helpers for the fixture assertions above.
    // -----------------------------------------------------------------

    fn documents(path: &std::path::Path) -> Vec<serde_norway::Value> {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        serde_norway::Deserializer::from_str(&text)
            .map(|document| {
                serde_norway::Value::deserialize(document)
                    .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
            })
            .filter(|value| !value.is_null())
            .collect()
    }

    fn kind_of(document: &serde_norway::Value) -> Option<&str> {
        document.get("kind").and_then(serde_norway::Value::as_str)
    }

    fn document(
        documents: &[serde_norway::Value],
        kind: &str,
        name: &str,
    ) -> Option<serde_norway::Value> {
        documents
            .iter()
            .find(|document| {
                kind_of(document) == Some(kind)
                    && document
                        .get("metadata")
                        .and_then(|metadata| metadata.get("name"))
                        .and_then(serde_norway::Value::as_str)
                        == Some(name)
            })
            .cloned()
    }

    /// Removes `metadata.namespace` and returns it, so two documents can
    /// be compared for equality in every other field.
    fn take_namespace(document: &mut serde_norway::Value) -> Option<String> {
        document
            .get_mut("metadata")
            .and_then(serde_norway::Value::as_mapping_mut)
            .and_then(|metadata| metadata.remove("namespace"))
            .and_then(|value| value.as_str().map(str::to_owned))
    }
}
