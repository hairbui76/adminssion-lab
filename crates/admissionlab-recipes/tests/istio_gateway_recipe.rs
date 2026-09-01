//! Istio Gateway API certification test (ROADMAP Task 6.10).
//!
//! Two halves, deliberately in one file:
//!
//! - A **metadata half** that needs no cluster and runs under plain
//!   `cargo test --workspace`: both recipe documents in
//!   `recipes/istio-gateway/` load, their install metadata still matches
//!   `recipes/istio/recipe.yaml`'s, the vendored Gateway API CRD bundle
//!   is still byte-identical to the upstream release it claims, the
//!   fixtures' embedded echo backends still match
//!   `fixtures/gateway/backends/`, and no classification field can enter
//!   the recipe schema.
//! - An **integration half** (`#[ignore]`d -- needs Docker and `kind`,
//!   the same convention `tests/istio_recipe.rs` and
//!   `tests/kyverno_recipe.rs` use) that creates a real cluster per
//!   certified Kubernetes version, installs the real two-component
//!   stack through `admissionlab_installer::install_stack`, and drives
//!   both fixtures end to end through the real Task 6.2/6.4/6.6/6.7/6.8
//!   machinery: apply, wait for reconciliation, resolve the data-plane
//!   endpoint from the recipe's own `gatewayEndpoint` strategy,
//!   port-forward to it, and send one real HTTP request.
//!
//! The metadata half is not decoration. Every value it pins was read off
//! a live cluster or an upstream artifact while writing this recipe, and
//! each one is exactly the kind of thing that silently rots: a chart
//! version bumped in one recipe and not the other, a vendored CRD bundle
//! edited "just a little", a fixture's inlined backend drifting from the
//! shared definition it was copied from. Catching those in milliseconds
//! is worth more than catching them in a four-minute cluster run.
//!
//! # What the integration half proves, and what it deliberately does not
//!
//! It proves that a real Istio, installed by this recipe, reconciles a
//! real `Gateway`/`HTTPRoute` pair to `Accepted`/`ResolvedRefs`/
//! `Programmed` **all True and all current for the object's own
//! generation**, and that a real HTTP request through the resulting
//! Envoy data plane returns 200 from the expected backend -- in both the
//! same-namespace and the cross-namespace (`ReferenceGrant`)
//! configuration.
//!
//! It does not compare a baseline against a candidate, does not
//! normalize anything, and does not decide whether any difference is a
//! regression. That is Task 6.9/6.11's job in `admissionlab-diff`/
//! `admissionlab-policy`, and Global Constraint 6 keeps it out of a
//! recipe entirely.
//!
//! # Two components, one stack
//!
//! `recipes/istio-gateway/` holds **two** recipe documents, and this
//! test installs both, in order: `gateway-api-crds` (the vendored
//! Gateway API CRD bundle, applied as raw manifests) and then
//! `istio-gateway` (`istio/istiod` via Helm). The recipe schema has one
//! `install:` per recipe and no shape for two, so "install the CRDs,
//! then Istio" is expressed the way this project already expresses it --
//! as an ordered component list handed to `install_stack`, which fully
//! waits out each component's readiness before the next one's install
//! begins. See `recipes/istio-gateway/README.md` for why the split lands
//! this way rather than in a single recipe.
//!
//! Order is asserted here rather than inherited from the loader's
//! filename sort: [`ordered_components`] looks each recipe up by name
//! and builds the list explicitly, so renaming a file cannot silently
//! reorder an install.
//!
//! # THE FINDING: on `kind`, a Gateway is never `Programmed` by default
//!
//! Istio provisions one `Deployment` + `Service` per `Gateway`, and that
//! `Service` defaults to `type: LoadBalancer`. A bare `kind` cluster has
//! no load-balancer controller, so the address is never assigned and
//! Istio reports -- correctly, and permanently:
//!
//! ```text
//! Programmed=False (AddressNotAssigned: ... address pending for
//! hostname "lab-gateway-istio.<ns>.svc.cluster.local")
//! ```
//!
//! This is a terminal state, not a slow one: waiting longer never fixes
//! it. `fixtures/gateway/istio/*.yaml` therefore each carry a
//! `ConfigMap` referenced from `Gateway.spec.infrastructure.parametersRef`
//! requesting a `ClusterIP` data-plane Service -- Istio's own documented
//! per-Gateway override. With it, the same Gateway reached
//! `Programmed=True` within 2 seconds. That override lives in the
//! fixture rather than in the recipe on purpose; see the fixture's own
//! header for why.
//!
//! # THE SECOND FINDING: "converged" means stable, not finished
//!
//! `wait_for_route_reconciliation` answers "has this route's status
//! stopped changing?" -- settled conditions, current for the object's
//! generation, identical across two polls at least 250 ms apart. That is
//! the right question for an evidence engine and the wrong one for a
//! certification assertion, and the difference is not theoretical.
//! Measured on three real clusters (Kubernetes 1.35.8, 1.36.4 and
//! 1.37.0), the first version of this test called it immediately after
//! apply and got, every single time:
//!
//! ```text
//! reconciled in 270ms, converged=true
//! expected Gateway condition "Programmed" to be True,
//!   got False (reason AddressNotAssigned)
//! ```
//!
//! Istio had already written a *stable, current, settled* status --
//! `Accepted=True`, `Programmed=False` -- because the data plane it was
//! waiting on did not exist yet. Nothing was wrong with the recipe, the
//! fixture, or the convergence rule; the test was asking a
//! stability question and reading the answer as a finality one.
//!
//! Two changes, both of which this test now makes:
//!
//! 1. **Wait for the data plane before asking about the status.** A
//!    Gateway's `Programmed` condition is a statement *about* its data
//!    plane, so waiting for the `Deployment` Istio provisions before
//!    asserting on `Programmed` is the correct ordering rather than a
//!    workaround. The backend `Deployment` is waited on at the same
//!    point, which the probe needs anyway.
//! 2. **Re-observe until correct, with a deadline.**
//!    [`observe_until_reconciled`] re-runs the whole convergence rule --
//!    not a hand-rolled poll of its parts -- until every certified
//!    condition is `True`, or until `RECONCILIATION_TIMEOUT`, at which
//!    point it reports the specific condition that never became true
//!    rather than a bare timeout.
//!
//! `RECONCILIATION_TIMEOUT` is 120 seconds against a measured
//! convergence of a few hundred milliseconds once the data plane is up.
//! The gap is not timidity about Istio's speed: what varies is
//! everything around the status write -- a cold `istiod` still electing
//! to reconcile, a loaded CI runner. A timeout is evidence in this
//! project, not a verdict (`admissionlab_gateway::reconcile` returns
//! `converged: false` rather than an error), so an over-generous bound
//! costs nothing on the happy path while a tight one turns a slow runner
//! into a false certification failure.
//!
//! # Readiness before traffic: two Deployments, not a sleep
//!
//! The same two waits also stand between the route's status and any
//! traffic, for a second, independent reason: a route can be fully
//! `Programmed` while no pod behind it is ready.
//! `admissionlab_gateway::probe` retries only *connection* failures,
//! within a 5-second window, and treats an HTTP 503 as a real answer
//! (which it is). Both waits go through
//! `admissionlab_installer::KubeReadinessProbe`, the same probe the
//! recipe pipeline itself uses, rather than a bespoke poll or a sleep.
//!
//! # Cleanup discipline
//!
//! `ScratchRoot` and `ClusterGuard` are copied from
//! `tests/istio_recipe.rs` (itself copied from `tests/kyverno_recipe.rs`)
//! -- see that file for why `ClusterGuard`'s `Drop` only warns and why
//! both guards are bound before any fallible step. This test adds one
//! more thing to clean up that those do not have: a live `kubectl
//! port-forward` child. [`probe_through_port_forward`] closes it on
//! every path, including the one where the probe itself failed, so a
//! failed assertion never leaks a process.

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
    Capability, GATEWAY_NAME_LABEL, GatewayEndpointStrategy, InstallMethod, ReadinessCheck, Recipe,
    load_builtin_recipes, load_recipe_compatibility, load_recipe_overrides,
};
use admissionlab_spec::ResolvedComponent;

// ---------------------------------------------------------------------
// What this recipe pins, restated here so a hand-edit to any of it fails
// a millisecond-scale test rather than a four-minute cluster run.
// ---------------------------------------------------------------------

/// The recipe that carries the `gatewayApi` capability.
const RECIPE_NAME: &str = "istio-gateway";

/// The recipe that installs the Gateway API CRDs, composed first.
const CRDS_RECIPE_NAME: &str = "gateway-api-crds";

/// The certified Istio recipe whose install metadata `istio-gateway`
/// shares -- the source of truth for the chart pin.
const ADMISSION_RECIPE_NAME: &str = "istio";

/// The Gateway API release the vendored bundle is taken from, chosen
/// because `istio/istio` at tag 1.30.4 declares `sigs.k8s.io/gateway-api
/// v1.5.1` in its own `go.mod`.
const GATEWAY_API_BUNDLE_VERSION: &str = "1.5.1";

/// SHA-256 of the upstream `standard-install.yaml` for
/// [`GATEWAY_API_BUNDLE_VERSION`], as downloaded from
/// `https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.5.1/standard-install.yaml`.
///
/// This is the whole verification story for a vendored third-party
/// artifact: the file in the repository is byte-identical to that URL's
/// response, and this constant is what proves it stayed that way.
const GATEWAY_API_BUNDLE_SHA256: &str =
    "751002b3b91a87f7ae3bd2517c79a47a8d7ed6702901808a1cf9bd97d284f9b8";

/// The vendored bundle's path, relative to the repository root.
const VENDORED_BUNDLE: &str = "recipes/istio-gateway/gateway-api/standard-install-v1.5.1.yaml";

/// The `GatewayClass` `istiod` creates for itself, named by both
/// fixtures and gated on by `recipe.yaml`'s readiness.
const GATEWAY_CLASS: &str = "istio";

/// The `Gateway` both fixtures create (in different namespaces).
const GATEWAY_NAME: &str = "lab-gateway";

/// The `HTTPRoute` both fixtures create.
const ROUTE_NAME: &str = "echo-route";

/// The listener both fixtures declare, and the `sectionName` both route
/// contracts name.
const LISTENER_NAME: &str = "http";

/// The data-plane `Service` Istio provisions for [`GATEWAY_NAME`]:
/// `<gateway name>-<class name>`. Never used to *find* the Service (the
/// recipe selects it by label -- see `gatewayEndpoint`), only to assert
/// that the label selector found the Service this test expects.
const DATA_PLANE_SERVICE: &str = "lab-gateway-istio";

// ---------------------------------------------------------------------
// Timeouts. Every one is an absolute bound on a real operation, sized
// from a measurement recorded in `recipes/istio-gateway/README.md`.
// ---------------------------------------------------------------------

/// Bounds one component's install-plus-readiness. The heavier of the two
/// components is `istio-gateway` (a Helm install plus `istiod` reaching
/// `Available`, measured at ~12.5 seconds after `helm upgrade --install`
/// returned on a warm node); `gateway-api-crds` is a single `kubectl
/// apply` of one 1 MB file plus four CRD `Established` checks, measured
/// at well under a second. 300 seconds leaves room for a cold image pull
/// on a loaded CI runner.
const COMPONENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Bounds one route's convergence. See this file's module documentation
/// ("Reconciliation timing, measured") for why this is 120 seconds
/// against a measured 2-3.
const RECONCILIATION_TIMEOUT: Duration = Duration::from_secs(120);

/// How long [`observe_until_reconciled`] waits before re-running the
/// convergence rule after an observation that was stable but not yet
/// the status this recipe certifies. Short enough that the common case
/// (already correct on the first observation) is unaffected, long enough
/// that a slow implementation is not polled in a tight loop.
const REOBSERVE_INTERVAL: Duration = Duration::from_millis(500);

/// Bounds the wait for one `Deployment` to report `Available` -- the
/// gateway data plane, and each fixture's echo backend. Both pull
/// already-loaded local images, so this is scheduling time, not download
/// time.
const DEPLOYMENT_READY_TIMEOUT: Duration = Duration::from_secs(180);

/// Bounds one `bash scripts/build-test-images.sh <cluster>` run. Mirrors
/// `tests/exit_gate.rs`'s own bound for the same script, with headroom:
/// a cold `docker build` of two Rust binaries dominates it.
const BUILD_AND_LOAD_TIMEOUT: Duration = Duration::from_mins(15);

// ---------------------------------------------------------------------
// The two fixtures, and what each one is contracted to do.
// ---------------------------------------------------------------------

/// One fixture: which file, which namespaces, and which backend a
/// request through it must reach.
struct Scenario {
    /// The `RouteContract` id, and the label this test's own error
    /// messages use.
    id: &'static str,
    /// The fixture file, relative to `fixtures/gateway/istio/`.
    manifest: &'static str,
    /// The namespace holding the `Gateway` and the `HTTPRoute`. The same
    /// for both scenarios' own two objects: what differs between them is
    /// where the *backend* lives.
    route_namespace: &'static str,
    /// The namespace holding the echo backend.
    backend_namespace: &'static str,
    /// The backend's name, which is also the identity it answers with.
    backend: &'static str,
    /// The `Host` header the probe sends, matching the route's
    /// `hostnames` entry.
    host: &'static str,
}

/// Both fixtures, in the order this test runs them. The second is the
/// first with the backend moved across a namespace boundary and a
/// `ReferenceGrant` added; running them in this order means a failure in
/// the simpler one is diagnosed before the cross-namespace machinery is
/// ever exercised.
const SCENARIOS: [Scenario; 2] = [
    Scenario {
        id: "istio-same-namespace",
        manifest: "same-namespace.yaml",
        route_namespace: "admissionlab-istio-gateway-same",
        backend_namespace: "admissionlab-istio-gateway-same",
        backend: "echo-a",
        host: "same.gateway.admissionlab.test",
    },
    Scenario {
        id: "istio-cross-namespace",
        manifest: "cross-namespace.yaml",
        route_namespace: "admissionlab-istio-gateway-cross-route",
        backend_namespace: "admissionlab-istio-gateway-cross-backend",
        backend: "echo-b",
        host: "cross.gateway.admissionlab.test",
    },
];

// ---------------------------------------------------------------------
// Cleanup guards. Copied from `tests/istio_recipe.rs` -- see this file's
// module documentation ("Cleanup discipline").
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
async fn istio_gateway_recipe_routes_real_traffic_for_every_certified_kubernetes_version() {
    let outcome = run_certification().await;
    outcome.expect("istio-gateway recipe certification test");
}

/// Validates every piece of checked-in metadata first (cheap, no
/// cluster), then runs the full install-and-route scenario once per
/// Kubernetes version `compatibility/recipes.yaml` certifies, each in
/// its own disposable cluster.
///
/// Every listed version is attempted even if an earlier one failed --
/// the same discipline `tests/istio_recipe.rs` uses -- so one run says
/// which certified versions regressed rather than stopping at the first.
async fn run_certification() -> Result<(), String> {
    let recipes = load_istio_gateway_recipes()?;
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
/// including when the scenario failed, and including when the cluster
/// was never successfully created.
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
/// resolving its pinned node image through
/// `compatibility/kubernetes.yaml` exactly as every other real-cluster
/// test in this workspace does.
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
    // independent of anything Istio does, and doing it first means an
    // image build failure is reported without having installed a service
    // mesh nobody is going to use.
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

/// Applies one fixture, proves it reconciled, then proves a real HTTP
/// request through the resulting data plane reaches the expected
/// backend.
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
    // before the probe. See this file's module documentation ("THE
    // SECOND FINDING") for the measurement that moved this up here.
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
             (the Service Istio provisions for this Gateway)"
        ));
    }
    println!("[{}] endpoint resolved to {endpoint}", scenario.id);

    probe_through_port_forward(cluster, &endpoint, scenario, &contract).await
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
    // completion). The two traits are both implemented by that type; the
    // distinction is which one this API needs.
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
/// *correct*, or until [`RECONCILIATION_TIMEOUT`] elapses.
///
/// `wait_for_route_reconciliation` answers "has this route's status
/// stopped changing?", which is the right question for an evidence
/// engine and the wrong one for a certification assertion: a status that
/// is settled, current for the object's generation, and identical across
/// two polls 250 ms apart satisfies the convergence rule *whether or not
/// the implementation has finished*. Istio reaches exactly such a state
/// within ~270 ms of apply, with `Programmed=False (AddressNotAssigned)`
/// -- see this file's module documentation ("THE SECOND FINDING") for
/// the measured evidence.
///
/// So this loop re-observes, with a real deadline, until every condition
/// this recipe certifies is `True` and current. Each attempt is a full
/// `wait_for_route_reconciliation` call, so what is retried is the whole
/// convergence rule and not a hand-rolled poll of its parts.
///
/// Returns the accepted evidence and how many observations it took; on
/// the deadline it returns the *last* assertion failure, which is the
/// specific condition that never became true rather than a generic
/// timeout.
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
///
/// The generation half is what makes this more than a status read: a
/// condition can be `True` and stale (published for an earlier
/// `metadata.generation`), which means the controller has not yet
/// reacted to the spec this fixture actually applied.
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
    // `metadata.generation` of its own (a `GatewayClass`'s spec is
    // written once by istiod and never edited by a fixture), so there is
    // no current generation to hold its `Accepted` fresh against. State
    // only, and said so rather than passing a value that would make the
    // freshness check vacuously pass while looking like a real one.
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
/// `tests/exit_gate.rs` and `admissionlab-admission/tests/kind_capture.rs`
/// already do.
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

/// Loads both recipe documents from `recipes/istio-gateway/`.
///
/// Through `load_recipe_overrides`, not `load_builtin_recipes`: the
/// `gateway-api-crds` half installs raw manifests by a path relative to
/// its own directory, and a built-in recipe's text is `include_str!`-ed
/// into the binary with no filesystem location to resolve one against
/// (see `admissionlab_recipes::load`'s own documentation). Loading both
/// halves the same way keeps one stack loaded by one mechanism, which is
/// also how `recipes/test-webhook/` is already loaded.
fn load_istio_gateway_recipes() -> Result<Vec<Recipe>, String> {
    let dir = repo_root().join("recipes/istio-gateway");
    let recipes = load_recipe_overrides(&dir)
        .map_err(|error| format!("failed to load recipes/istio-gateway: {error}"))?;
    if recipes.len() != 2 {
        return Err(format!(
            "expected exactly 2 recipe documents in recipes/istio-gateway (the CRD bundle and \
             istiod), got {}: {:?}",
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
///
/// Written out explicitly rather than taken from the loader's own
/// filename ordering -- see this file's module documentation ("Two
/// components, one stack").
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

/// The `gatewayEndpoint` strategy the `istio-gateway` component
/// declares, taken from the component list the install actually used --
/// so the strategy exercised against the cluster is the one that was
/// installed, not a second copy loaded separately.
fn gateway_endpoint_strategy(recipes: &[Recipe]) -> Result<GatewayEndpointStrategy, String> {
    // Taken from the `Recipe`, not from the `ResolvedComponent` built
    // out of it: `ResolvedComponent` carries a component's capabilities
    // but not its endpoint strategy, which is recipe metadata Task 6.11
    // threads through the run separately.
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
        // Named, not left to default: both fixtures declare exactly one
        // listener today, and stating it means adding a second one
        // later cannot silently make this contract's parent lookup
        // ambiguous.
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

/// Reads `compatibility/recipes.yaml`'s `istio-gateway` entry and
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
    Ok(entry.kubernetes.certified.clone())
}

// ---------------------------------------------------------------------
// Metadata assertions, shared by the integration test above (which runs
// them before touching a cluster) and by the fast tests below.
// ---------------------------------------------------------------------

/// Every cheap check on what the two recipes declare.
fn check_recipe_metadata(recipes: &[Recipe]) -> Result<(), String> {
    let crds = find(recipes, CRDS_RECIPE_NAME)?;
    let gateway = find(recipes, RECIPE_NAME)?;
    check_shared_istio_install(&gateway)?;
    check_crd_install(&crds)?;
    check_capability_and_endpoint(&gateway)?;
    check_readiness(&crds, &gateway)?;
    check_compatibility_entry(&gateway)
}

/// The `istio-gateway` recipe installs exactly what `recipes/istio`
/// installs. See `recipes/istio-gateway/recipe.yaml`'s own "Chart pin"
/// note for why this is a test rather than a comment.
fn check_shared_istio_install(gateway: &Recipe) -> Result<(), String> {
    let builtins = load_builtin_recipes()
        .map_err(|error| format!("failed to load built-in recipes: {error}"))?;
    let admission = find(&builtins, ADMISSION_RECIPE_NAME)?;
    let (InstallMethod::Helm(theirs), InstallMethod::Helm(ours)) =
        (&admission.install, &gateway.install)
    else {
        return Err("both istio recipes must install via Helm".to_string());
    };
    for (field, theirs, ours) in [
        ("chart", &theirs.chart, &ours.chart),
        ("repo", &theirs.repo_url, &ours.repo_url),
        ("version", &theirs.version, &ours.version),
        ("namespace", &theirs.namespace, &ours.namespace),
    ] {
        if theirs != ours {
            return Err(format!(
                "recipes/istio-gateway/recipe.yaml's install.{field} is {ours:?} but \
                 recipes/istio/recipe.yaml's is {theirs:?} -- these must stay identical; \
                 recipes/istio is the source of truth for the Istio pin"
            ));
        }
    }
    if gateway.version != admission.version {
        return Err(format!(
            "recipes/istio-gateway pins version {:?} but recipes/istio pins {:?}",
            gateway.version, admission.version
        ));
    }
    // The one field that must NOT be shared -- see `recipe.yaml`.
    if ours.repo_name != "istio" {
        return Err(format!(
            "install.repoName must be \"istio\" (the alias install.chart's own \"istio/istiod\" \
             prefix names), got {:?} -- helm would add an alias this chart reference never uses",
            ours.repo_name
        ));
    }
    Ok(())
}

/// The CRD component installs the vendored bundle, and that bundle is
/// still byte-identical to the upstream release it claims to be.
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
/// metadata is for, and exactly what Istio's provisioned Service can be
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
        port_name: Some("http".to_owned()),
        port: None,
    };
    let actual = gateway
        .gateway_endpoint
        .as_ref()
        .ok_or_else(|| format!("the {RECIPE_NAME:?} recipe declares no gatewayEndpoint"))?;
    if *actual != expected {
        return Err(format!(
            "the gatewayEndpoint strategy is {actual:?}, expected {expected:?} -- Istio labels \
             the Service it provisions with {GATEWAY_NAME_LABEL} and exposes two ports, so the \
             named port is required"
        ));
    }
    Ok(())
}

/// Readiness gates istiod, the Gateway API being served, and Istio's own
/// `GatewayClass` being accepted -- and the CRD component gates every kind
/// the fixtures use.
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

    let has_istiod = gateway.readiness.iter().any(|check| {
        matches!(
            check,
            ReadinessCheck::DeploymentAvailable { namespace, name }
                if namespace == "istio-system" && name == "istiod"
        )
    });
    if !has_istiod {
        return Err(format!(
            "expected the {RECIPE_NAME:?} recipe to gate on Deployment istio-system/istiod, got \
             {:?}",
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
             Accepted -- the only check that proves istiod's Gateway controller is actually \
             reconciling on this cluster"
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
            "recipes/istio-gateway/recipe.yaml pins version {:?} but \
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
    repo_root().join("fixtures/gateway/istio")
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
        "admissionlab-istio-gateway-recipe-test-{}",
        unique.as_str()
    ))
}

// ---------------------------------------------------------------------
// Fast, no-cluster tests. These run under plain `cargo test --workspace`
// and are what actually stops the checked-in metadata rotting between
// integration runs -- see this file's module documentation.
// ---------------------------------------------------------------------

#[cfg(test)]
mod metadata_tests {
    use serde::Deserialize as _;

    use super::{
        CRDS_RECIPE_NAME, GATEWAY_CLASS, GATEWAY_NAME, LISTENER_NAME, RECIPE_NAME, ROUTE_NAME,
        SCENARIOS, certified_kubernetes_versions, check_recipe_metadata, find, fixtures_dir,
        load_istio_gateway_recipes, ordered_components, repo_root,
    };

    /// Everything `check_recipe_metadata` covers, in one place: the
    /// shared Istio pin, the vendored bundle's digest, the capability
    /// and endpoint strategy, every readiness gate, and the
    /// `compatibility/recipes.yaml` cross-check. The integration test
    /// runs this same function before creating a cluster; this is what
    /// makes it also run in CI's fast lane.
    #[test]
    fn the_checked_in_recipe_metadata_is_exactly_what_this_task_certified() {
        let recipes = load_istio_gateway_recipes().expect("both recipe documents must load");
        check_recipe_metadata(&recipes).expect("recipe metadata");
    }

    #[test]
    fn the_two_components_install_the_crds_before_the_implementation() {
        let recipes = load_istio_gateway_recipes().expect("both recipe documents must load");
        let components = ordered_components(&recipes).expect("component list");
        let names: Vec<&str> = components
            .iter()
            .map(|component| component.name.as_str())
            .collect();
        assert_eq!(
            names,
            [CRDS_RECIPE_NAME, RECIPE_NAME],
            "the Gateway API CRDs must be installed before istiod"
        );
    }

    #[test]
    fn certification_targets_at_least_one_kubernetes_version() {
        let versions = certified_kubernetes_versions().expect("certified versions");
        assert!(!versions.is_empty());
    }

    /// The `gateway-api-crds` recipe carries no capability at all: it
    /// installs an API, not an implementation of one, and a `Gateway`
    /// created with only these CRDs present would sit forever with no
    /// status because no controller is watching.
    #[test]
    fn the_crd_component_claims_no_capability() {
        let recipes = load_istio_gateway_recipes().expect("both recipe documents must load");
        let crds = find(&recipes, CRDS_RECIPE_NAME).expect("the CRD recipe");
        assert!(crds.capabilities.is_empty(), "got {:?}", crds.capabilities);
    }

    /// Global Constraint 6, proven by mutation rather than asserted by
    /// comment: adding a classification field to this recipe is a parse
    /// error, because the schema's `deny_unknown_fields` allow-list has
    /// no place for one.
    #[test]
    fn a_severity_field_cannot_be_added_to_this_recipe() {
        let original =
            std::fs::read_to_string(repo_root().join("recipes/istio-gateway/recipe.yaml"))
                .expect("the recipe file must be readable");
        let dir = std::env::temp_dir().join(format!(
            "admissionlab-istio-gateway-severity-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(
            dir.join("recipe.yaml"),
            format!("{original}\nseverity: critical\n"),
        )
        .expect("write mutated recipe");

        let error = admissionlab_recipes::load_recipe_overrides(&dir)
            .expect_err("a severity field must be rejected");
        let _ = std::fs::remove_dir_all(&dir);
        let message = error.to_string();
        assert!(
            message.contains("severity"),
            "the error must name the offending field; got: {message}"
        );
    }

    /// Each fixture's inlined echo backend is
    /// `fixtures/gateway/backends/echo-{a,b}.yaml`'s own objects plus a
    /// namespace and nothing else. See the fixtures' own headers for why
    /// they are copied rather than included, and why this test is the
    /// thing that makes that copy safe.
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
    /// it grants exactly the reference its route makes -- the property
    /// that makes fixture (b) a test of cross-namespace permission
    /// rather than a second copy of fixture (a).
    #[test]
    fn only_the_cross_namespace_fixture_grants_a_cross_namespace_reference() {
        let same = documents(&fixtures_dir().join(SCENARIOS[0].manifest));
        assert!(
            same.iter()
                .all(|doc| kind_of(doc) != Some("ReferenceGrant")),
            "the same-namespace fixture must need no ReferenceGrant"
        );

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
