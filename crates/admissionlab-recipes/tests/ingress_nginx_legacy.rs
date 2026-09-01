//! Legacy community `ingress-nginx` migration-recipe certification
//! (ROADMAP Task 8.2).
//!
//! **The upstream project this certifies is retired and its repository
//! is archived.** `recipes/ingress-nginx-legacy/README.md` carries the
//! dates, the upstream's own wording, and Admission Lab's stance: this
//! stack is installed so a migration *away* from it can be measured, and
//! for no other reason.
//!
//! Two halves, deliberately in one file, exactly as
//! `tests/istio_gateway_recipe.rs` is laid out:
//!
//! - A **metadata half** that needs no cluster and runs under plain
//!   `cargo test --workspace`: the recipe loads, every pin is still the
//!   exact one this task measured, "legacy" is still marked in a way a
//!   program can read, the fixtures still name what the recipe installs,
//!   and the `gatewayEndpoint` pairing rule this task widened still
//!   rejects both of its mismatches.
//! - An **integration half** (`#[ignore]`d — needs Docker and `kind`)
//!   that creates a real cluster, installs the real chart through
//!   `admissionlab_installer::install_stack`, routes real HTTP traffic
//!   through a real `Ingress`, and then proves the validating admission
//!   webhook denies a deliberately invalid one.
//!
//! # What the integration half proves
//!
//! Two independent claims, in this order, because the first is a
//! precondition for trusting the second:
//!
//! 1. **The data plane works.** An `Ingress` applied to a cluster
//!    running this recipe routes `Host: basic.ingress.admissionlab.test`
//!    to the `echo-a` backend, answering HTTP 200 with `echo-a`'s own
//!    identity in the body — reached through the `Service` the recipe's
//!    own `gatewayEndpoint` strategy resolved, over a real `kubectl
//!    port-forward`.
//! 2. **The admission webhook works.** `webhook-deny.yaml` is rejected
//!    by the API server, and the rejection names
//!    `validate.nginx.ingress.kubernetes.io` and the offending path.
//!
//! It does not compare a baseline against a candidate and does not
//! decide whether any difference is a regression — Global Constraint 6
//! keeps that out of a recipe and out of a recipe's certification.
//!
//! # THE FINDING: an `Ingress` has no status worth waiting on
//!
//! `tests/istio_gateway_recipe.rs` can wait for a `Gateway`/`HTTPRoute`
//! to report `Accepted`/`ResolvedRefs`/`Programmed` before it probes,
//! and that wait is what makes its probe deterministic. The `Ingress`
//! API offers no equivalent. Its only status field is
//! `status.loadBalancer`, which this controller populates from the
//! address of its own `Service` — and on `kind` that `LoadBalancer`
//! Service never gets an address, so the field stays empty *forever*, on
//! a cluster where routing demonstrably works. Waiting on it would hang;
//! asserting on it would fail.
//!
//! So this test proves routing with the traffic itself, and
//! [`probe_until_routed`] is what makes that deterministic instead of
//! racy: `execute_http_probe` deliberately retries only a refused
//! *connection* (a 404 is an observation, not a failure — see
//! `admissionlab_gateway::probe`'s own module documentation), and
//! "nginx has not reloaded its configuration yet" presents as a 404 over
//! a perfectly good connection. The loop re-runs the whole probe under a
//! deadline until the contract's status and backend both match,
//! reporting the last real response if the deadline passes rather than a
//! bare timeout.
//!
//! # THE SECOND FINDING: the deny input had to be chosen, not guessed
//!
//! Three candidate deny inputs were tried against a live cluster running
//! exactly this pin before [`DENY_PATH`] was settled on; one of them was
//! silently *admitted*, and the most obvious one is racy. The evidence
//! is in `fixtures/migration/ingress-nginx/webhook-deny.yaml`'s own
//! header and in the recipe README. The short version: the controller's
//! deep inspector runs *before* `CheckIngress` filters on the ingress
//! class, and every other candidate runs after — where an `Ingress`
//! whose class the controller's informer has not yet synced is returned
//! as allowed.
//!
//! # Cleanup discipline
//!
//! Copied from `tests/istio_gateway_recipe.rs`: a `ScratchRoot` guard
//! removes the artifact directory, a `ClusterGuard` deletes the cluster
//! on every path (and warns loudly if it could not), both are bound
//! before any fallible step, and [`probe_until_routed`]'s caller closes
//! the `kubectl port-forward` child even when the probe or its
//! assertions failed. The metadata half's two mutation tests write into
//! their own uniquely-named temporary directories and remove them
//! through the same guard type.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use admissionlab_cluster::{KindClusterManager, cluster_name};
use admissionlab_core::{
    ArtifactStore, ClusterError, ClusterHandle, ClusterManager, ClusterSpec, CommandResult,
    CommandSpec, ProcessRunner, RunId, RunPaths, Side, TokioProcessRunner,
};
use admissionlab_gateway::{
    GatewayEndpoint, GatewayEndpointResolver, GatewayIdentity, HttpProbeContract,
    KubeGatewayEndpointResolver, apply_gateway_manifests, execute_http_probe,
    start_service_port_forward,
};
use admissionlab_installer::{
    CompositeInstaller, HelmInstaller, KubeReadinessProbe, ManifestsInstaller, ReadinessProbe,
    install_stack,
};
use admissionlab_recipes::{
    CERTIFY_KUBERNETES_ENV, Capability, GatewayEndpointStrategy, InstallMethod, ReadinessCheck,
    Recipe, load_builtin_recipes, load_recipe_compatibility, load_recipe_overrides,
    narrow_certified_versions,
};
use admissionlab_spec::ResolvedComponent;

// ---------------------------------------------------------------------
// What this recipe pins, restated here so a hand-edit to any of it fails
// a millisecond-scale test rather than a multi-minute cluster run. Every
// value was read off a live cluster or an upstream artifact; see
// `recipes/ingress-nginx-legacy/README.md` for each one's provenance.
// ---------------------------------------------------------------------

/// The recipe's name. Its `-legacy` suffix is half of how "this upstream
/// is archived" is marked machine-readably — see
/// [`the_recipe_is_marked_legacy_in_a_way_a_program_can_read`].
const RECIPE_NAME: &str = "ingress-nginx-legacy";

/// The pinned CHART version, and the recipe's own `version`. Final: the
/// project was archived four days after 4.15.1 was cut, so there will
/// not be a 4.15.2.
const CHART_VERSION: &str = "4.15.1";

/// The controller release chart [`CHART_VERSION`] carries
/// (`Chart.yaml`'s `appVersion`). Not a field of the recipe — the recipe
/// pins a chart — but asserted against the `README.md` table so the two
/// halves of the pin cannot drift apart in documentation.
const CONTROLLER_APP_VERSION: &str = "1.15.1";

/// `<repo_name>/<chart_name>`, helm's own argv form.
const CHART: &str = "ingress-nginx/ingress-nginx";

/// The HTTPS chart repository. Deliberately not the OCI mirror, whose
/// tags are `v`-prefixed — see the recipe README.
const CHART_REPO: &str = "https://kubernetes.github.io/ingress-nginx";

/// The alias `helm repo add` registers. Must equal [`CHART`]'s own
/// prefix, and cannot be defaulted from the recipe name here.
const CHART_REPO_NAME: &str = "ingress-nginx";

/// The namespace the chart installs into, and the Helm release name
/// (both default to the recipe name; both are written out in the
/// recipe).
const NAMESPACE: &str = RECIPE_NAME;

/// The controller `Deployment` and its data-plane `Service`, which share
/// a name. `<release>-controller`, because this chart's `fullname`
/// helper collapses `<release>-<chart>` when the release name already
/// contains the chart name.
const CONTROLLER: &str = "ingress-nginx-legacy-controller";

/// The `ValidatingWebhookConfiguration` the chart creates.
const WEBHOOK_CONFIGURATION: &str = "ingress-nginx-legacy-admission";

/// The webhook entry inside it, and the name every denial message this
/// test accepts must carry.
const WEBHOOK: &str = "validate.nginx.ingress.kubernetes.io";

/// The `IngressClass` the chart creates and every fixture names.
const INGRESS_CLASS: &str = "nginx";

/// The routing fixture, relative to `fixtures/migration/ingress-nginx/`.
const BASIC_FIXTURE: &str = "basic-routing.yaml";

/// The deny fixture, in the same directory.
const DENY_FIXTURE: &str = "webhook-deny.yaml";

/// The routing fixture's namespace, backend, `Ingress` and `Host`.
const BASIC_NAMESPACE: &str = "admissionlab-ingress-nginx-basic";
const BACKEND: &str = "echo-a";
const INGRESS: &str = "echo-ingress";
const HOST: &str = "basic.ingress.admissionlab.test";

/// The deny fixture's namespace and `Ingress`, both of which the
/// controller embeds in its rejection message.
const DENY_NAMESPACE: &str = "admissionlab-ingress-nginx-deny";
const DENIED_INGRESS: &str = "echo-ingress-denied";

/// The deny input itself: one of the literals the controller's deep
/// inspector refuses (`/etc/(passwd|shadow|group|nginx|ingress-controller)`).
const DENY_PATH: &str = "/etc/nginx";

/// Admission Lab's Tier-1 primary Kubernetes version, which Global
/// Constraint 10 requires this archived release to pass before the
/// migration recipe may ship at v1. A constant because
/// `compatibility/kubernetes.yaml` states which version is primary in a
/// comment rather than a field — `admissionlab-cluster`'s own
/// `tests/version.rs` names it the same way, for the same reason. See
/// [`check_compatibility_entry`], which additionally requires the matrix
/// to still resolve it.
const PRIMARY_KUBERNETES: &str = "1.36.4";

// ---------------------------------------------------------------------
// Timeouts. Every one bounds a real operation, sized from a measurement
// recorded in the recipe's README.
// ---------------------------------------------------------------------

/// Bounds the chart install plus both readiness gates. Measured on a
/// warm `kind` node: `helm upgrade --install` returned after 22.5 s and
/// the controller reported `Available` 7.6 s later. 300 s leaves room
/// for a cold pull of the controller image on a loaded CI runner.
const COMPONENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Bounds the wait for one `Deployment` to report `Available` — used for
/// the fixture's echo backend, whose image is already loaded into the
/// node, so this is scheduling time rather than download time.
const DEPLOYMENT_READY_TIMEOUT: Duration = Duration::from_secs(180);

/// Bounds [`probe_until_routed`]: how long the `Ingress` has to start
/// actually serving before this test calls it a failure. See this file's
/// module documentation for why a traffic deadline is what stands in for
/// a status wait here. Measured: the first probe already succeeded.
const ROUTING_TIMEOUT: Duration = Duration::from_secs(120);

/// How long [`probe_until_routed`] waits between two probes.
const REPROBE_INTERVAL: Duration = Duration::from_millis(500);

/// Bounds one `kubectl` invocation. Only the deny fixture's `apply` goes
/// through `kubectl` at all.
const KUBECTL_TIMEOUT: Duration = Duration::from_secs(120);

/// Bounds one `bash scripts/build-test-images.sh <cluster>` run, matching
/// `tests/istio_gateway_recipe.rs`: a cold `docker build` of two Rust
/// binaries dominates it.
const BUILD_AND_LOAD_TIMEOUT: Duration = Duration::from_mins(15);

// ---------------------------------------------------------------------
// Cleanup guards. Copied from `tests/istio_gateway_recipe.rs` -- see this
// file's module documentation ("Cleanup discipline").
// ---------------------------------------------------------------------

/// Removes a directory tree on drop. Used both for the integration
/// half's artifact root and for the metadata half's mutation-test
/// directories, so no test in this file can leave a temporary directory
/// behind even when it panics.
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
async fn ingress_nginx_legacy_recipe_routes_and_denies_for_every_certified_kubernetes_version() {
    let outcome = run_certification().await;
    outcome.expect("ingress-nginx-legacy recipe certification test");
}

/// Validates every piece of checked-in metadata first (cheap, no
/// cluster), then runs the full install-route-deny scenario once per
/// Kubernetes version `compatibility/recipes.yaml` certifies, each in its
/// own disposable cluster.
///
/// Every listed version is attempted even if an earlier one failed — the
/// same discipline the other recipe certifications use — so one run says
/// which certified versions regressed rather than stopping at the first.
async fn run_certification() -> Result<(), String> {
    let recipe = load_legacy_recipe()?;
    check_recipe_metadata(&recipe)?;
    let strategy = recipe
        .gateway_endpoint
        .clone()
        .ok_or_else(|| format!("the {RECIPE_NAME:?} recipe declares no gatewayEndpoint"))?;
    let component = component(recipe);
    let versions = certified_kubernetes_versions()?;

    let root = unique_root("run");
    // Bound before any fallible step below, so `root` cannot leak.
    let _scratch_root_guard = ScratchRoot(root.clone());
    let store = ArtifactStore::new(&root);

    let mut problems = Vec::new();
    for version in &versions {
        if let Err(error) = run_one_version(&store, &component, &strategy, version).await {
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
/// against it, and always deletes the cluster before returning —
/// including when the scenario failed.
async fn run_one_version(
    store: &ArtifactStore,
    component: &ResolvedComponent,
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

    let outcome =
        install_route_and_deny(&runner, guard.handle(), &paths, component, strategy).await;

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

/// Loads the echo image, installs the recipe through the real installer
/// pipeline, proves routing, then proves the deny.
async fn install_route_and_deny(
    runner: &dyn ProcessRunner,
    cluster: &ClusterHandle,
    paths: &RunPaths,
    component: &ResolvedComponent,
    strategy: &GatewayEndpointStrategy,
) -> Result<(), String> {
    // Before the stack, not after: `kind load docker-image` is
    // independent of anything the chart does, and doing it first means
    // an image build failure is reported without having installed an
    // ingress controller nobody is going to use.
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
        std::slice::from_ref(component),
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
    if installed_summary != [(RECIPE_NAME, "helm")] {
        return Err(format!(
            "expected install_stack to install {RECIPE_NAME:?} via helm, got {installed_summary:?}"
        ));
    }

    run_routing_scenario(cluster, &readiness, strategy).await?;
    run_deny_scenario(runner, cluster, paths).await
}

/// Applies the routing fixture and proves a real HTTP request through
/// the recipe's own resolved endpoint reaches the expected backend.
async fn run_routing_scenario(
    cluster: &ClusterHandle,
    readiness: &KubeReadinessProbe,
    strategy: &GatewayEndpointStrategy,
) -> Result<(), String> {
    let manifest = fixtures_dir().join(BASIC_FIXTURE);
    let applied = apply_gateway_manifests(cluster, std::slice::from_ref(&manifest))
        .await
        .map_err(|error| format!("apply_gateway_manifests failed: {error}"))?;

    // The `Ingress` is applied LAST, after its `Namespace`, `Service`
    // and `Deployment`, because `ApplyCategory::for_kind` places an
    // unrecognized kind in `Unknown` and that category sorts after every
    // known one. This assertion is what turns that into a checked
    // contract rather than a fixture author's assumption -- if a future
    // task gives `Ingress` a category of its own, ahead of `Workload`,
    // this fails here rather than as a flaky 503 later.
    let ingress_position = applied
        .objects
        .iter()
        .position(|key| key.resource == "ingresses" && key.name == INGRESS)
        .ok_or_else(|| {
            format!(
                "the applied fixture did not include Ingress {BASIC_NAMESPACE}/{INGRESS}; \
                 applied: {:?}",
                applied
                    .objects
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            )
        })?;
    if ingress_position + 1 != applied.objects.len() {
        return Err(format!(
            "the Ingress must be applied after everything it depends on, but it was applied at \
             position {ingress_position} of {}: {:?}",
            applied.objects.len(),
            applied
                .objects
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        ));
    }

    wait_for_deployment(readiness, cluster, BASIC_NAMESPACE, BACKEND).await?;

    // The recipe's endpoint strategy carries no placeholder (an Ingress
    // controller is one shared data plane, not one Service per object),
    // so this identity is unused by the resolution -- passed because the
    // resolver's signature takes one, and naming the Ingress under test
    // makes any error message it appears in point at the right object.
    let identity = GatewayIdentity {
        namespace: BASIC_NAMESPACE.to_owned(),
        name: INGRESS.to_owned(),
    };
    let endpoint = KubeGatewayEndpointResolver::new()
        .resolve(cluster, &identity, strategy)
        .await
        .map_err(|error| format!("resolving the controller endpoint failed: {error}"))?;
    let expected = GatewayEndpoint {
        namespace: NAMESPACE.to_owned(),
        service: CONTROLLER.to_owned(),
        port: 80,
    };
    if endpoint != expected {
        return Err(format!(
            "the recipe's gatewayEndpoint strategy resolved to {endpoint}, expected {expected} \
             (the controller Service the chart installs)"
        ));
    }
    println!("[routing] endpoint resolved to {endpoint}");

    probe_through_port_forward(cluster, &endpoint).await
}

/// Opens a port-forward, drives [`probe_until_routed`] through it, and
/// closes the forward on every path — including the one where the probe
/// failed or its assertions did.
async fn probe_through_port_forward(
    cluster: &ClusterHandle,
    endpoint: &GatewayEndpoint,
) -> Result<(), String> {
    // A fresh `TokioProcessRunner` rather than the pipeline's: this call
    // takes a `ProcessSpawner` (a long-lived child whose stdout is read
    // line by line), not a `ProcessRunner`.
    let forward = start_service_port_forward(&TokioProcessRunner::new(), cluster, endpoint)
        .await
        .map_err(|error| format!("start_service_port_forward to {endpoint} failed: {error}"))?;

    let checked = probe_until_routed(forward.local_addr).await;
    let closed = forward.close().await;

    match (checked, closed) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(format!("failed to close the port-forward: {error}")),
        (Err(probe_error), Err(close_error)) => Err(format!(
            "{probe_error}\nadditionally, failed to close the port-forward: {close_error}"
        )),
    }
}

/// Sends the probe until the `Ingress` is actually serving it, or until
/// [`ROUTING_TIMEOUT`] elapses.
///
/// See this file's module documentation ("THE FINDING") for why a
/// deadline on traffic is what stands in for a status wait here, and why
/// `execute_http_probe` alone is not enough: it retries a refused
/// connection but (deliberately) returns a 404 as the observation it is,
/// and "nginx has not reloaded yet" is a 404 over a healthy connection.
///
/// On the deadline this reports the last real response — status,
/// backend and headers — rather than a bare timeout, because which of
/// those was wrong is the whole diagnosis.
async fn probe_until_routed(local_addr: std::net::SocketAddr) -> Result<(), String> {
    let probe = HttpProbeContract {
        host: HOST.to_owned(),
        // A non-root path under the fixture's `/` prefix match, so the
        // echo response's own `path` field is meaningful evidence that
        // the request arrived intact.
        path: "/ingress-probe".to_owned(),
        method: "GET".to_owned(),
        headers: BTreeMap::new(),
        expected_status: 200,
        expected_backend: Some(BACKEND.to_owned()),
    };

    let deadline = Instant::now() + ROUTING_TIMEOUT;
    let mut attempts: u32 = 0;
    loop {
        attempts = attempts.saturating_add(1);
        let response = execute_http_probe(local_addr, &probe)
            .await
            .map_err(|error| format!("execute_http_probe failed: {error}"))?;

        let mismatch = if response.status != probe.expected_status {
            Some(format!(
                "expected HTTP {} through the Ingress, got {} (headers: {:?})",
                probe.expected_status, response.status, response.response_headers
            ))
        } else if response.backend.as_deref() != probe.expected_backend.as_deref() {
            Some(format!(
                "expected the request to reach backend {:?}, but the echo response identified \
                 {:?} -- the Ingress resolved to the wrong workload",
                probe.expected_backend, response.backend
            ))
        } else {
            None
        };

        match mismatch {
            None => {
                println!(
                    "[routing] probe -> status {} backend {:?} in {:?} ({attempts} probe(s))",
                    response.status, response.backend, response.elapsed
                );
                return Ok(());
            }
            Some(reason) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "the Ingress never served the expected response within {ROUTING_TIMEOUT:?} \
                         ({attempts} probe(s)); the last one said: {reason}"
                    ));
                }
                tokio::time::sleep(REPROBE_INTERVAL).await;
            }
        }
    }
}

/// Applies the deny fixture and requires the API server to REJECT it,
/// attributably to this recipe's webhook and to the offending path.
///
/// Through plain `kubectl apply`, not `apply_gateway_manifests`, for the
/// same reason `tests/kyverno_recipe.rs` uses `kubectl` for its own deny
/// scenario: what this fixture produces is the API server's own
/// rejection *text*, and `kubectl`'s stderr is where that arrives
/// verbatim. The evidence is printed, not merely matched, so a run's log
/// carries the admission decision itself.
async fn run_deny_scenario(
    runner: &dyn ProcessRunner,
    cluster: &ClusterHandle,
    paths: &RunPaths,
) -> Result<(), String> {
    let manifest = fixtures_dir().join(DENY_FIXTURE);
    let args = vec![
        "apply".into(),
        "--server-side=false".into(),
        "-f".into(),
        manifest.as_os_str().to_owned(),
    ];
    let result = kubectl_run(runner, cluster, paths, args).await?;
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();

    if result.status.success() {
        return Err(format!(
            "kubectl apply -f {} unexpectedly SUCCEEDED. The pinned release's validating \
             admission webhook must reject an Ingress whose path is {DENY_PATH:?}; a run in \
             which it is admitted is a certification failure, not a pass. stdout: {}",
            manifest.display(),
            String::from_utf8_lossy(&result.stdout).trim()
        ));
    }

    println!("[deny] admission evidence, verbatim:\n{}", stderr.trim());

    // Four independent claims about one message, each of which a
    // different accident could break: that it was an admission denial at
    // all (rather than, say, a connection error to an API server that
    // had gone away), that THIS webhook made it, which object it was
    // about, and what in that object was refused. A test asserting only
    // "the apply failed" would pass on a typo in the fixture's
    // `apiVersion`.
    for needle in [
        "denied the request",
        WEBHOOK,
        &format!("{DENY_NAMESPACE}/{DENIED_INGRESS}"),
        DENY_PATH,
    ] {
        if !stderr.contains(needle) {
            return Err(format!(
                "the Ingress was rejected, but the rejection does not mention {needle:?} -- so it \
                 is not attributable to this recipe's webhook denying this input. stderr was:\n\
                 {stderr}"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Cluster-facing helpers.
// ---------------------------------------------------------------------

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
             {DEPLOYMENT_READY_TIMEOUT:?} -- a probe sent now would be answered by the \
             controller's own 503, not by the backend"
        ))
    }
}

/// Runs one `kubectl` command against `cluster`, with its own isolated
/// kubeconfig and a discovery cache confined to the run's artifact
/// directory (never `~/.kube/cache`), matching
/// `tests/kyverno_recipe.rs`.
async fn kubectl_run(
    runner: &dyn ProcessRunner,
    cluster: &ClusterHandle,
    paths: &RunPaths,
    mut args: Vec<std::ffi::OsString>,
) -> Result<CommandResult, String> {
    args.push("--kubeconfig".into());
    args.push(cluster.kubeconfig.as_os_str().to_owned());
    args.push("--cache-dir".into());
    args.push(paths.root().join("kubectl-cache").into_os_string());
    let spec = CommandSpec {
        program: "kubectl".into(),
        args,
        cwd: None,
        env: BTreeMap::new(),
        sensitive_env_keys: BTreeSet::new(),
        timeout: KUBECTL_TIMEOUT,
    };
    let context = spec.context();
    runner
        .run(spec)
        .await
        .map_err(|error| format!("failed to run `{context}`: {error}"))
}

/// Runs `bash scripts/build-test-images.sh <cluster>`, exactly as
/// `tests/istio_gateway_recipe.rs` does.
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
// Loading and shaping what the recipe declares.
// ---------------------------------------------------------------------

/// Loads `recipes/ingress-nginx-legacy/`.
///
/// Through `load_recipe_overrides`, not `load_builtin_recipes` — and
/// that is a decision, not an accident of mechanism: this recipe *could*
/// be a built-in (it installs purely via Helm, so it has no relative
/// path to resolve). It deliberately is not. `load_builtin_recipes` is
/// the set a binary offers without being pointed anywhere, and an
/// archived upstream whose own maintainers write "if you are not already
/// using ingress-nginx, you should not be deploying it" is precisely
/// what does not belong in a default set. A team that wants it names the
/// directory, exactly as `recipes/istio-gateway/` and
/// `recipes/test-webhook/` are already named.
/// [`the_legacy_recipe_is_deliberately_not_a_builtin`] pins that.
fn load_legacy_recipe() -> Result<Recipe, String> {
    let dir = repo_root().join("recipes/ingress-nginx-legacy");
    let recipes = load_recipe_overrides(&dir)
        .map_err(|error| format!("failed to load recipes/ingress-nginx-legacy: {error}"))?;
    let [recipe] = <[Recipe; 1]>::try_from(recipes).map_err(|recipes| {
        format!(
            "expected exactly one recipe document in recipes/ingress-nginx-legacy, got {}: {:?}",
            recipes.len(),
            recipes.iter().map(|r| r.name.clone()).collect::<Vec<_>>()
        )
    })?;
    if recipe.name != RECIPE_NAME {
        return Err(format!(
            "expected the recipe to be named {RECIPE_NAME:?}, got {:?}",
            recipe.name
        ));
    }
    Ok(recipe)
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

/// Reads `compatibility/recipes.yaml`'s `ingress-nginx-legacy` entry and
/// returns every Kubernetes version this test must certify against,
/// narrowed by `ADMISSIONLAB_CERTIFY_KUBERNETES` when a generated CI
/// matrix job sets it (Task 7.5).
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

/// Every cheap check on what the recipe declares.
fn check_recipe_metadata(recipe: &Recipe) -> Result<(), String> {
    check_install(recipe)?;
    check_capability_and_endpoint(recipe)?;
    check_readiness(recipe)?;
    check_compatibility_entry(recipe)
}

/// The pin, field by field. ROADMAP Task 8.2 Step 1: "do not use
/// floating or 'latest' legacy chart versions". `resolve_recipe` already
/// rejects a non-exact Helm version for every recipe; this additionally
/// pins *which* exact version, so a bump is a visible test failure
/// rather than a silently different install of an archived project.
fn check_install(recipe: &Recipe) -> Result<(), String> {
    if recipe.version != CHART_VERSION {
        return Err(format!(
            "the recipe's own version is {:?}, expected the pinned chart version {CHART_VERSION:?}",
            recipe.version
        ));
    }
    let InstallMethod::Helm(helm) = &recipe.install else {
        return Err("this recipe must install via Helm".to_owned());
    };
    for (field, actual, expected) in [
        ("chart", helm.chart.as_str(), CHART),
        ("repo", helm.repo_url.as_str(), CHART_REPO),
        ("repoName", helm.repo_name.as_str(), CHART_REPO_NAME),
        ("version", helm.version.as_str(), CHART_VERSION),
        ("namespace", helm.namespace.as_str(), NAMESPACE),
        ("releaseName", helm.release_name.as_str(), RECIPE_NAME),
    ] {
        if actual != expected {
            return Err(format!(
                "install.{field} is {actual:?}, expected {expected:?}"
            ));
        }
    }
    // The finding recorded in the recipe and its README: the chart is
    // installed at its DEFAULT values, because a `kind`-friendly install
    // needs none. Asserted rather than assumed, so that a future values
    // override arrives with a reason attached instead of silently.
    if !helm.values_files.is_empty() || !helm.set_values.is_empty() {
        return Err(format!(
            "this recipe is certified at the chart's default values (see its README): got \
             values_files {:?} and set_values {:?}",
            helm.values_files, helm.set_values
        ));
    }
    Ok(())
}

/// The capability and the endpoint strategy, which this recipe couples
/// (Task 8.2 Step 3) and which the loader now requires together.
fn check_capability_and_endpoint(recipe: &Recipe) -> Result<(), String> {
    let expected_capabilities: BTreeSet<Capability> = [Capability::LegacyIngress].into();
    if recipe.capabilities != expected_capabilities {
        return Err(format!(
            "expected exactly {expected_capabilities:?}, got {:?} -- note that `admission` is \
             deliberately NOT claimed here even though this stack serves a validating webhook \
             (see the recipe's own comment)",
            recipe.capabilities
        ));
    }

    let expected = GatewayEndpointStrategy::ServiceByName {
        namespace: NAMESPACE.to_owned(),
        name: CONTROLLER.to_owned(),
        port_name: Some("http".to_owned()),
        port: None,
    };
    match &recipe.gateway_endpoint {
        Some(strategy) if *strategy == expected => Ok(()),
        other => Err(format!(
            "expected the endpoint strategy {expected:?} (a literal Service, with no \
             {{gatewayName}} placeholder -- an Ingress controller is one shared data plane), got \
             {other:?}"
        )),
    }
}

/// Both readiness gates, and nothing else. The second one is the
/// load-bearing one — see the recipe's README for why a `failurePolicy:
/// Fail` webhook without its `caBundle` would make the deny scenario
/// pass for the wrong reason.
fn check_readiness(recipe: &Recipe) -> Result<(), String> {
    let expected = vec![
        ReadinessCheck::DeploymentAvailable {
            namespace: NAMESPACE.to_owned(),
            name: CONTROLLER.to_owned(),
        },
        ReadinessCheck::WebhookConfigurationPresent {
            name: WEBHOOK_CONFIGURATION.to_owned(),
        },
    ];
    if recipe.readiness == expected {
        Ok(())
    } else {
        Err(format!(
            "expected exactly {expected:?}, got {:?}",
            recipe.readiness
        ))
    }
}

/// `compatibility/recipes.yaml` and the recipe cannot silently disagree,
/// and Global Constraint 10's own condition for shipping this recipe at
/// all — "when that archived release passes the primary supported
/// Kubernetes integration job" — is checked here rather than trusted: the
/// entry must certify Admission Lab's own primary Kubernetes version.
fn check_compatibility_entry(recipe: &Recipe) -> Result<(), String> {
    let compat = load_recipe_compatibility()
        .map_err(|error| format!("failed to load compatibility/recipes.yaml: {error}"))?;
    let entry = compat
        .entry(RECIPE_NAME)
        .ok_or_else(|| format!("compatibility/recipes.yaml has no {RECIPE_NAME:?} entry"))?;
    if entry.version != recipe.version {
        return Err(format!(
            "recipes/ingress-nginx-legacy/recipe.yaml pins version {:?} but \
             compatibility/recipes.yaml's {RECIPE_NAME:?} entry pins {:?}",
            recipe.version, entry.version
        ));
    }

    // `compatibility/kubernetes.yaml` records which version is Tier 1's
    // primary in a comment ("1.36.4 is Tier 1's primary supported
    // version") and not in a field, so `PRIMARY_KUBERNETES` is a
    // constant here -- the same way `admissionlab-cluster`'s own
    // `tests/version.rs` names it. It is not left dangling: the matrix
    // must still resolve it, so a future edit that retired 1.36.4
    // without revisiting this recipe fails here rather than certifying
    // a version nothing can create a cluster at.
    let matrix = admissionlab_cluster::load_matrix()
        .map_err(|error| format!("failed to load the Kubernetes compatibility matrix: {error}"))?;
    admissionlab_cluster::resolve_node_image(PRIMARY_KUBERNETES, &matrix).map_err(|error| {
        format!(
            "compatibility/kubernetes.yaml no longer supports {PRIMARY_KUBERNETES:?}, which this \
             test treats as Tier 1's primary: {error}"
        )
    })?;
    if !entry
        .kubernetes
        .certified
        .iter()
        .any(|certified| certified.version == PRIMARY_KUBERNETES)
    {
        return Err(format!(
            "Global Constraint 10 admits this recipe at v1 only \"when that archived release \
             passes the primary supported Kubernetes integration job\", so its compatibility \
             entry must certify the primary version {PRIMARY_KUBERNETES:?}; it certifies {:?}",
            entry
                .kubernetes
                .certified
                .iter()
                .map(|certified| certified.version.clone())
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Small standalone helpers.
// ---------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    repo_root().join("fixtures/migration/ingress-nginx")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("this crate's own CARGO_MANIFEST_DIR/../.. must exist")
}

/// A uniquely-named temporary directory. Never reused between tests, and
/// always paired with a [`ScratchRoot`] guard by its caller.
fn unique_root(label: &str) -> PathBuf {
    let unique = RunId::generate();
    std::env::temp_dir().join(format!(
        "admissionlab-ingress-nginx-legacy-{label}-{}",
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
    use super::{
        BACKEND, BASIC_FIXTURE, BASIC_NAMESPACE, CHART_VERSION, CONTROLLER_APP_VERSION,
        DENIED_INGRESS, DENY_FIXTURE, DENY_NAMESPACE, DENY_PATH, INGRESS, INGRESS_CLASS,
        RECIPE_NAME, Recipe, ScratchRoot, WEBHOOK, certified_kubernetes_versions,
        check_recipe_metadata, fixtures_dir, load_builtin_recipes, load_legacy_recipe, repo_root,
        unique_root,
    };

    /// Everything `check_recipe_metadata` covers, in one place: the pin,
    /// the capability and endpoint strategy, both readiness gates, and
    /// the `compatibility/recipes.yaml` cross-check (including Global
    /// Constraint 10's primary-version condition). The integration test
    /// runs this same function before creating a cluster; this is what
    /// makes it also run in CI's fast lane.
    #[test]
    fn the_checked_in_recipe_metadata_is_exactly_what_this_task_certified() {
        let recipe = load_legacy_recipe().expect("the recipe document must load");
        check_recipe_metadata(&recipe).expect("recipe metadata");
    }

    /// **How "legacy" is machine-readable**, which ROADMAP Task 8.2's
    /// interface note requires ("certification metadata clearly marks the
    /// upstream project as legacy/archived").
    ///
    /// Two existing mechanisms, and deliberately no new schema field:
    ///
    /// - `Capability::LegacyIngress` — already in the type system, and
    ///   already the value a consumer branches on to pick fixtures. It
    ///   says "this serves the legacy `Ingress` API".
    /// - The recipe's `name`, ending in `-legacy`. A *maintained*
    ///   `Ingress` controller would also carry the capability above; the
    ///   name is what says this particular upstream is archived.
    ///
    /// A `legacy: true` field was considered and rejected: nothing would
    /// read it, and a field no code consumes is a claim rather than a
    /// mechanism. Should that change — should some later task genuinely
    /// need to branch on "archived upstream" independently of the name —
    /// this test is where the decision is recorded and the right place to
    /// revisit it.
    #[test]
    fn the_recipe_is_marked_legacy_in_a_way_a_program_can_read() {
        let recipe = load_legacy_recipe().expect("the recipe document must load");
        assert!(
            recipe.name.ends_with("-legacy"),
            "the recipe name {:?} must mark the archived upstream",
            recipe.name
        );
        assert!(
            recipe
                .capabilities
                .contains(&admissionlab_recipes::Capability::LegacyIngress),
            "got {:?}",
            recipe.capabilities
        );
        // And the human-readable half, which is what a user actually
        // reads before installing an archived project. Asserted because
        // a banner that can be deleted in passing is not a banner.
        let readme =
            std::fs::read_to_string(repo_root().join("recipes/ingress-nginx-legacy/README.md"))
                .expect("the recipe README must be readable");
        for needle in ["RETIRED", "ARCHIVED", CHART_VERSION, CONTROLLER_APP_VERSION] {
            assert!(
                readme.contains(needle),
                "the README must state {needle:?} prominently"
            );
        }
    }

    /// This recipe is deliberately not embedded into the shipped binary.
    /// See `load_legacy_recipe`'s own documentation for the reasoning;
    /// this is what keeps it from being wired in by a later
    /// one-line-looking change without that reasoning being revisited.
    #[test]
    fn the_legacy_recipe_is_deliberately_not_a_builtin() {
        let builtins = load_builtin_recipes().expect("the built-in recipes must load");
        assert!(
            !builtins.iter().any(|recipe| recipe.name == RECIPE_NAME),
            "an archived upstream must not be in the default recipe set; got {:?}",
            builtins.iter().map(|r| &r.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn certification_targets_at_least_one_kubernetes_version() {
        let versions = certified_kubernetes_versions().expect("certified versions");
        assert!(!versions.is_empty());
    }

    /// Global Constraint 6, proven by mutation rather than asserted by
    /// comment: adding a classification field to this recipe is a parse
    /// error, because the schema's `deny_unknown_fields` allow-list has
    /// no place for one.
    #[test]
    fn a_severity_field_cannot_be_added_to_this_recipe() {
        let error = load_mutated_recipe("severity", |original| {
            format!("{original}\nseverity: critical\n")
        })
        .expect_err("a severity field must be rejected");
        assert!(
            error.to_string().contains("severity"),
            "the error must name the offending field; got: {error}"
        );
    }

    /// The `gatewayEndpoint` pairing rule this task widened, in the
    /// direction that matters most: a `legacyIngress` recipe with no
    /// endpoint is rejected, rather than resolving to "this stack serves
    /// no traffic" — which would be a fabricated observation rather than
    /// a configuration error (Global Constraint 15).
    #[test]
    fn a_legacy_ingress_recipe_without_an_endpoint_is_rejected() {
        let error = load_mutated_recipe("no-endpoint", |original| {
            // Drop the trailing `gatewayEndpoint:` block, which is the
            // last thing in the file.
            let cut = original
                .find("\ngatewayEndpoint:")
                .expect("recipe.yaml must still declare a gatewayEndpoint block");
            format!("{}\n", &original[..cut])
        })
        .expect_err("a legacyIngress recipe with no gatewayEndpoint must be rejected");
        let message = error.to_string();
        assert!(message.contains("gatewayEndpoint"), "got: {message}");
        assert!(message.contains("legacyIngress"), "got: {message}");
    }

    /// And the other direction, unchanged in force from Task 6.6: an
    /// endpoint on a recipe claiming no traffic-serving capability is
    /// metadata for a data plane it never said it had.
    #[test]
    fn an_endpoint_without_a_traffic_serving_capability_is_rejected() {
        let error = load_mutated_recipe("wrong-capability", |original| {
            original.replace("  - legacyIngress", "  - admission")
        })
        .expect_err("an endpoint without a traffic-serving capability must be rejected");
        let message = error.to_string();
        assert!(message.contains("gatewayEndpoint"), "got: {message}");
        // The message must name spellings the parser actually accepts,
        // which is why it is built from the capability vocabulary rather
        // than written out as literals.
        assert!(message.contains("gatewayApi"), "got: {message}");
        assert!(message.contains("legacyIngress"), "got: {message}");
    }

    /// Loads `recipe.yaml` with `mutate` applied to its text, from a
    /// uniquely-named temporary directory that is always removed.
    fn load_mutated_recipe(
        label: &str,
        mutate: impl FnOnce(&str) -> String,
    ) -> Result<Vec<Recipe>, admissionlab_recipes::RecipeError> {
        let original =
            std::fs::read_to_string(repo_root().join("recipes/ingress-nginx-legacy/recipe.yaml"))
                .expect("the recipe file must be readable");
        let dir = unique_root(label);
        let _guard = ScratchRoot(dir.clone());
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join("recipe.yaml"), mutate(&original)).expect("write mutated recipe");
        admissionlab_recipes::load_recipe_overrides(&dir)
    }

    // -----------------------------------------------------------------
    // The fixtures.
    // -----------------------------------------------------------------

    /// The routing fixture's inlined echo backend must still be a copy
    /// of the shared definition with nothing but `metadata.namespace`
    /// added — the same machine-checked duplication
    /// `tests/istio_gateway_recipe.rs` enforces for the Gateway
    /// fixtures, and for the same reason: a fixture that is readable in
    /// one place is worth a copy only if the copy cannot drift.
    #[test]
    fn fixture_backend_matches_the_shared_echo_backend_definition() {
        let shared = documents(&repo_root().join("fixtures/gateway/backends/echo-a.yaml"));
        let fixture = documents(&fixtures_dir().join(BASIC_FIXTURE));

        for kind in ["Service", "Deployment"] {
            let expected = document(&shared, kind, BACKEND)
                .unwrap_or_else(|| panic!("echo-a.yaml must declare a {kind}"));
            let mut actual = document(&fixture, kind, BACKEND)
                .unwrap_or_else(|| panic!("{BASIC_FIXTURE} must declare a {kind}"));
            let namespace = take_namespace(&mut actual);
            assert_eq!(
                namespace.as_deref(),
                Some(BASIC_NAMESPACE),
                "the copied {kind} must name this fixture's namespace"
            );
            assert_eq!(
                actual, expected,
                "{BASIC_FIXTURE}'s {kind} has drifted from \
                 fixtures/gateway/backends/echo-a.yaml in a field other than metadata.namespace"
            );
        }
    }

    /// Both fixtures must name the `IngressClass` the pinned chart
    /// installs. With `controller.watchIngressWithoutClass` at its
    /// default `false`, an `Ingress` that names none is ignored by the
    /// controller silently -- no event, no status -- so this is the
    /// difference between a fixture that routes and one that times out.
    #[test]
    fn both_fixtures_name_the_ingress_class_the_chart_installs() {
        for fixture in [BASIC_FIXTURE, DENY_FIXTURE] {
            let documents = documents(&fixtures_dir().join(fixture));
            let ingress = documents
                .iter()
                .find(|document| kind_of(document) == Some("Ingress"))
                .unwrap_or_else(|| panic!("{fixture} must declare an Ingress"));
            assert_eq!(
                ingress
                    .get("spec")
                    .and_then(|spec| spec.get("ingressClassName"))
                    .and_then(serde_norway::Value::as_str),
                Some(INGRESS_CLASS),
                "{fixture}'s Ingress must name the chart's IngressClass"
            );
        }
    }

    /// The deny fixture must actually carry the deny input, in the
    /// namespace and under the name this test's own message assertions
    /// expect the controller to echo back. Without this, a well-meaning
    /// edit to the fixture would turn the integration test's four
    /// message assertions into assertions about a message that can never
    /// be produced.
    #[test]
    fn the_deny_fixture_still_carries_the_deny_input() {
        let documents = documents(&fixtures_dir().join(DENY_FIXTURE));
        let ingress = documents
            .iter()
            .find(|document| kind_of(document) == Some("Ingress"))
            .expect("the deny fixture must declare an Ingress");
        let metadata = ingress.get("metadata").expect("metadata");
        assert_eq!(
            metadata.get("name").and_then(serde_norway::Value::as_str),
            Some(DENIED_INGRESS)
        );
        assert_eq!(
            metadata
                .get("namespace")
                .and_then(serde_norway::Value::as_str),
            Some(DENY_NAMESPACE)
        );
        let rendered =
            serde_norway::to_string(ingress).expect("the Ingress document must re-serialize");
        assert!(
            rendered.contains(DENY_PATH),
            "the deny fixture must submit the path {DENY_PATH:?} the webhook refuses; got:\n\
             {rendered}"
        );
        // The routing fixture must NOT contain it -- otherwise the
        // routing half of this certification would be denied too, and
        // the deny half would prove nothing about the deny input in
        // particular.
        let basic = std::fs::read_to_string(fixtures_dir().join(BASIC_FIXTURE))
            .expect("the routing fixture must be readable");
        assert!(
            !basic.lines().any(|line| line
                .trim_start()
                .starts_with(&format!("- path: {DENY_PATH}"))),
            "the routing fixture must not submit the deny input"
        );
    }

    /// The routing fixture's `Ingress` names the backend the probe
    /// expects, on the host the probe sends. A mismatch here is a test
    /// that could only ever fail on a cluster.
    #[test]
    fn the_routing_fixture_matches_what_the_probe_asserts() {
        let documents = documents(&fixtures_dir().join(BASIC_FIXTURE));
        let ingress = documents
            .iter()
            .find(|document| kind_of(document) == Some("Ingress"))
            .expect("the routing fixture must declare an Ingress");
        assert_eq!(
            ingress
                .get("metadata")
                .and_then(|metadata| metadata.get("name"))
                .and_then(serde_norway::Value::as_str),
            Some(INGRESS)
        );
        let rendered =
            serde_norway::to_string(ingress).expect("the Ingress document must re-serialize");
        assert!(rendered.contains(super::HOST), "got:\n{rendered}");
        assert!(rendered.contains(BACKEND), "got:\n{rendered}");
    }

    /// The webhook this recipe gates readiness on is the one the deny
    /// scenario attributes its rejection to. Both are literals in this
    /// file; this asserts they are the literals the chart actually uses,
    /// by reading them out of the recipe rather than trusting the
    /// constants to agree with it.
    #[test]
    fn the_recipe_gates_on_the_webhook_the_deny_scenario_names() {
        let recipe = load_legacy_recipe().expect("the recipe document must load");
        assert!(
            recipe.readiness.iter().any(|check| matches!(
                check,
                admissionlab_recipes::ReadinessCheck::WebhookConfigurationPresent { name }
                    if name == super::WEBHOOK_CONFIGURATION
            )),
            "got {:?}",
            recipe.readiness
        );
        assert!(
            WEBHOOK.starts_with("validate.nginx.ingress"),
            "the webhook entry name is upstream's, not ours"
        );
    }

    // -----------------------------------------------------------------
    // YAML helpers, mirroring `tests/istio_gateway_recipe.rs`'s.
    // -----------------------------------------------------------------

    fn documents(path: &std::path::Path) -> Vec<serde_norway::Value> {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
        serde_norway::Deserializer::from_str(&text)
            .map(|document| {
                serde::Deserialize::deserialize(document).unwrap_or_else(|error| {
                    panic!("{} must be valid YAML: {error}", path.display())
                })
            })
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

    fn take_namespace(document: &mut serde_norway::Value) -> Option<String> {
        let metadata = document.get_mut("metadata")?;
        let mapping = metadata.as_mapping_mut()?;
        mapping
            .remove(serde_norway::Value::String("namespace".to_owned()))
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
    }
}
