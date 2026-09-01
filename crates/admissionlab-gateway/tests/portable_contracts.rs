//! The portable v1 Gateway behavior contracts (ROADMAP Task 8.7).
//!
//! One corpus — `fixtures/gateway/portable/` — run against **both**
//! certified Gateway API implementations: Istio 1.30.4 and NGINX Gateway
//! Fabric 2.6.7, each in its own disposable `kind` cluster, each pinned
//! to Gateway API v1.5.1. That is Phase 8's first must-be-true, and this
//! file is what makes it a measurement rather than a claim.
//!
//! Two halves, deliberately in one file, the same split
//! `tests/istio_gateway_recipe.rs` and `tests/nginx_gateway_recipe.rs`
//! already use:
//!
//! - A **fixture-tree half** that needs no cluster and runs under plain
//!   `cargo test --workspace`: the shared files really are shared (no
//!   `gatewayClassName`, no vendor kind, in any of them), the two
//!   overlays differ in exactly the two documented ways, the embedded
//!   echo backends still match `fixtures/gateway/backends/`, every
//!   contract in [`CONTRACTS`] names a route that exists with the
//!   hostname and listener it claims, and no TLS key material has ever
//!   been checked in.
//! - An **integration half** ([`#[ignore]`]d — needs Docker and `kind`)
//!   that installs each implementation for real and drives all seven
//!   contracts through it.
//!
//! # What "portable" is allowed to mean here
//!
//! Exactly one thing: the same `HTTPRoute` documents, applied unchanged
//! to both implementations, produce the same observable behavior. Two
//! consequences the corpus is built around:
//!
//! - **The per-implementation surface is one file.** `backends.yaml` and
//!   `routes.yaml` are shared byte for byte; `gateway-istio.yaml` and
//!   `gateway-nginx.yaml` are the whole of what differs, and
//!   [`the_two_overlays_differ_only_in_the_documented_ways`] holds them
//!   to that. There is no templating engine — see the fixture tree's
//!   `README.md` for why two complete objects beat one templated one.
//! - **Vendor-specific normalization is named, not silent.** The one
//!   place two conforming implementations produce different bytes for
//!   the same fixture is the redirect `Location` header's port (Istio
//!   strips `:80`, NGF keeps it), so the contract compares scheme, host
//!   and path through
//!   `admissionlab_gateway::probe::normalize_location` and says so.
//!
//! # Timeout is absent on purpose
//!
//! ROADMAP Task 8.7 Step 6 asks for a portable timeout contract *if it
//! proves stable on both implementations*, and prefers a documented
//! deferral to a flaky test. NGINX Gateway Fabric 2.6.7 documents
//! `HTTPRoute` `rules[].timeouts` as **not supported** — it accepts the
//! route (`Accepted: True`, `reason: UnsupportedField`) and ignores the
//! field — while Istio 1.30.4 implements it. A contract here would
//! therefore reconcile cleanly on both and then fail its traffic
//! assertion on one, which is the worst possible shape. The deferral,
//! with its provenance, is in the fixture tree's `README.md`.
//!
//! # One Kubernetes version, two implementations
//!
//! The per-implementation certification suites already sweep every
//! certified Kubernetes version. This suite's subject is a *different*
//! axis — two implementations of one API — so it runs on one version:
//! `compatibility/kubernetes.yaml`'s Tier-1 primary, overridable with
//! `ADMISSIONLAB_CERTIFY_KUBERNETES` exactly as those suites allow. Two
//! clusters per run is already the cost of the question being asked;
//! multiplying it by three minors would buy coverage this suite is not
//! the right place to own.
//!
//! # Cleanup discipline
//!
//! `ScratchRoot` and `ClusterGuard` are copied from
//! `tests/istio_gateway_recipe.rs` — see that file for why
//! `ClusterGuard`'s `Drop` only warns and why both guards are bound
//! before any fallible step. This suite adds two more things to clean
//! up: live `kubectl port-forward` children (closed on every path,
//! including the one where an assertion failed) and the generated TLS
//! Secret manifest, whose [`SecretManifest`] guard deletes the file the
//! moment the apply that needed it has returned.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use admissionlab_cluster::{KindClusterManager, cluster_name};
use admissionlab_core::{
    ArtifactStore, ClusterError, ClusterHandle, ClusterManager, ClusterSpec, CommandSpec,
    ProcessRunner, RunId, RunPaths, Side, TokioProcessRunner,
};
use admissionlab_gateway::{
    CONDITION_ACCEPTED, CONDITION_PROGRAMMED, CONDITION_RESOLVED_REFS, ConditionState,
    GatewayEndpoint, GatewayEndpointResolver, GatewayIdentity, HttpProbeContract,
    KubeGatewayEndpointResolver, MIN_WEIGHTED_ROUTING_SAMPLES, ObservedCondition, ParentLookup,
    PortForwardHandle, ProbeObservation, ProbeTally, ProbeTransport, ReconciliationEvidence,
    RouteContract, TEST_TLD, TestCertificate, apply_gateway_manifests, execute_probe,
    generate_test_certificate, probe_many, redirect_location, response_header,
    start_service_port_forward, test_certificate_client_config, wait_for_route_reconciliation,
    weighted_routing_tolerance, weighted_routing_within_tolerance,
};
use admissionlab_installer::{
    CompositeInstaller, HelmInstaller, KubeReadinessProbe, ManifestsInstaller, ReadinessProbe,
    install_stack,
};
use admissionlab_recipes::{
    CERTIFY_KUBERNETES_ENV, GATEWAY_NAME_LABEL, GatewayEndpointStrategy, ReadinessCheck, Recipe,
    load_recipe_overrides,
};
use admissionlab_spec::ResolvedComponent;

// ---------------------------------------------------------------------
// What the fixture tree declares, restated here so a hand-edit to either
// side fails a millisecond-scale test rather than a multi-minute cluster
// run.
// ---------------------------------------------------------------------

/// The namespace holding the `Gateway`, every `HTTPRoute`, and two of
/// the three echo backends.
const NAMESPACE: &str = "admissionlab-portable";

/// The namespace holding the third echo backend — the one the
/// cross-namespace contract reaches through a `ReferenceGrant`.
const REMOTE_NAMESPACE: &str = "admissionlab-portable-remote";

/// The one `Gateway` every contract is parented to.
const GATEWAY_NAME: &str = "lab-gateway";

/// The plaintext listener, and the `sectionName` six of the seven
/// contracts name.
const HTTP_LISTENER: &str = "http";

/// The TLS-terminating listener, and the `sectionName` the TLS contract
/// names.
const HTTPS_LISTENER: &str = "https";

/// The `Secret` both overlays' `certificateRefs` name. Generated per
/// run — see [`generate_tls_secret_manifest`].
const TLS_SECRET: &str = "portable-tls";

/// The hostname the TLS contract probes, the certificate's only DNS
/// subject alternative name, and the SNI the probe presents. One
/// constant because all three must agree.
const TLS_HOST: &str = "tls.portable.gateway.admissionlab.test";

/// The `ReferenceGrant` in [`REMOTE_NAMESPACE`].
const REFERENCE_GRANT: &str = "allow-portable-routes";

/// The identity the remote echo backend answers with — the whole of the
/// cross-namespace contract's evidence. See `backends.yaml`'s header.
const REMOTE_BACKEND_ID: &str = "echo-b-remote";

/// The fixture files shared, byte for byte, by both implementations.
const SHARED_FIXTURES: [&str; 2] = ["backends.yaml", "routes.yaml"];

// ---------------------------------------------------------------------
// Timeouts. Every one is an absolute bound on a real operation, sized
// from the two per-implementation certification suites' own measured
// bounds.
// ---------------------------------------------------------------------

/// Bounds one component's install-plus-readiness. Both stacks' heavier
/// component was measured at ~12.5 seconds for the Helm install alone on
/// a warm node; 300 seconds leaves room for a cold image pull.
const COMPONENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Bounds one route's convergence.
const RECONCILIATION_TIMEOUT: Duration = Duration::from_secs(120);

/// How long [`observe_until_reconciled`] waits before re-running the
/// convergence rule after an observation that was stable but not yet
/// correct.
const REOBSERVE_INTERVAL: Duration = Duration::from_millis(500);

/// Bounds the wait for one `Deployment` to report `Available`.
const DEPLOYMENT_READY_TIMEOUT: Duration = Duration::from_secs(180);

/// Bounds one `bash scripts/build-test-images.sh <cluster>` run.
const BUILD_AND_LOAD_TIMEOUT: Duration = Duration::from_mins(15);

// ---------------------------------------------------------------------
// The contract model.
//
// This is Task 8.7's richer contract, and it lives HERE — in the test
// crate — rather than in `admissionlab-spec`. The full argument is in
// `fixtures/gateway/portable/README.md` ("Where the contract model
// lives, and why not in the spec"); the short form is that
// `admissionlab.io/v1` is a user-facing frozen schema and this
// corpus is repo-internal certification tooling, so the honest home for
// "expected Location", "expected backend-observed path", "over TLS" and
// "these weights" is the suite that asserts them. Every engine
// capability they need is public in `admissionlab_gateway::probe`, so a
// future user-facing extension would be wiring rather than new
// behavior.
// ---------------------------------------------------------------------

/// One portable behavior contract: one route, one hostname, and what
/// probing it must show.
struct PortableContract {
    /// Stable id, used as the `RouteContract` id and as the label in
    /// every line this suite prints.
    id: &'static str,
    /// The `HTTPRoute` in `routes.yaml` this contract is about.
    route: &'static str,
    /// Which listener the route names, and therefore which port-forward
    /// its probes go through.
    listener: &'static str,
    /// The `Host` header every probe sends — one hostname per contract,
    /// so a probe reaches exactly one route by construction.
    host: &'static str,
    /// Whether the probes speak TLS. `true` only for the TLS
    /// termination contract, which is also the only contract on
    /// [`HTTPS_LISTENER`].
    tls: bool,
    /// The single-request expectations, in order. Empty for the
    /// weighted contract, whose subject cannot be observed one request
    /// at a time.
    probes: &'static [ProbeExpectation],
    /// The statistical expectation, for the weighted contract only.
    weighted: Option<WeightedExpectation>,
}

/// What one request through a contract's route must show.
struct ProbeExpectation {
    /// The request path.
    path: &'static str,
    /// Request headers the probe sends, beyond `Host`.
    send_headers: &'static [(&'static str, &'static str)],
    /// The status the data plane must return.
    status: u16,
    /// The backend that must answer, or `None` when no backend should
    /// (a 404 from the Gateway itself, a redirect it answers directly).
    backend: Option<&'static str>,
    /// The path the BACKEND must observe, when there is a backend. This
    /// is the rewrite contract's whole evidence, and for every other
    /// contract it is the request's own path — asserted rather than
    /// assumed, because "the request arrived intact" is worth pinning.
    observed_path: Option<&'static str>,
    /// Request headers the backend must report having received.
    observed_headers: &'static [(&'static str, &'static str)],
    /// Response headers the probe must receive.
    response_headers: &'static [(&'static str, &'static str)],
    /// The normalized `Location` a redirect must carry: scheme, host,
    /// path. The port is deliberately not part of this — see
    /// `normalize_location`, and the fixture tree's `README.md`.
    location: Option<(&'static str, &'static str, &'static str)>,
}

/// The weighted-routing contract's statistical expectation.
struct WeightedExpectation {
    /// The request path every sample uses.
    path: &'static str,
    /// How many requests to send. At least
    /// [`MIN_WEIGHTED_ROUTING_SAMPLES`], which
    /// [`the_weighted_contract_meets_the_roadmaps_sample_floor`] holds
    /// this to.
    samples: u32,
    /// Each backend and the proportion of requests it should answer.
    splits: &'static [(&'static str, f64)],
}

/// An empty [`ProbeExpectation`] list's worth of defaults, so a contract
/// states only what it means.
const NO_HEADERS: &[(&str, &str)] = &[];

/// Every portable contract, in the order this suite runs them.
///
/// Ordered so that a failure in a simpler contract is diagnosed before a
/// more elaborate one is exercised: basic routing first (everything
/// depends on it), then the cross-namespace and TLS variants of the same
/// plain routing, then the three filters, then the statistical one.
const CONTRACTS: [PortableContract; 7] = [
    // 1. Basic host/path routing.
    PortableContract {
        id: "basic-routing",
        route: "basic-route",
        listener: HTTP_LISTENER,
        host: "basic.portable.gateway.admissionlab.test",
        tls: false,
        probes: &[
            ProbeExpectation {
                path: "/basic/probe",
                send_headers: NO_HEADERS,
                status: 200,
                backend: Some("echo-a"),
                observed_path: Some("/basic/probe"),
                observed_headers: NO_HEADERS,
                response_headers: NO_HEADERS,
                location: None,
            },
            // The half that makes this a PATH contract and not only a
            // host one: the same hostname, a path outside the rule's
            // prefix, and no rule to match it.
            ProbeExpectation {
                path: "/not-basic/probe",
                send_headers: NO_HEADERS,
                status: 404,
                backend: None,
                observed_path: None,
                observed_headers: NO_HEADERS,
                response_headers: NO_HEADERS,
                location: None,
            },
        ],
        weighted: None,
    },
    // 2. ReferenceGrant cross-namespace backend.
    PortableContract {
        id: "cross-namespace",
        route: "cross-namespace-route",
        listener: HTTP_LISTENER,
        host: "cross.portable.gateway.admissionlab.test",
        tls: false,
        probes: &[ProbeExpectation {
            path: "/cross-probe",
            send_headers: NO_HEADERS,
            status: 200,
            // Not `echo-b`: the LOCAL echo-b answers with that. Only
            // the workload in the remote namespace answers with this,
            // which is what makes the boundary crossing observable.
            backend: Some(REMOTE_BACKEND_ID),
            observed_path: Some("/cross-probe"),
            observed_headers: NO_HEADERS,
            response_headers: NO_HEADERS,
            location: None,
        }],
        weighted: None,
    },
    // 3. TLS termination.
    PortableContract {
        id: "tls-termination",
        route: "tls-route",
        listener: HTTPS_LISTENER,
        host: TLS_HOST,
        tls: true,
        probes: &[ProbeExpectation {
            path: "/tls-probe",
            send_headers: NO_HEADERS,
            status: 200,
            backend: Some("echo-a"),
            observed_path: Some("/tls-probe"),
            observed_headers: NO_HEADERS,
            response_headers: NO_HEADERS,
            location: None,
        }],
        weighted: None,
    },
    // 4. RequestHeaderModifier + ResponseHeaderModifier.
    PortableContract {
        id: "header-modifiers",
        route: "header-route",
        listener: HTTP_LISTENER,
        host: "headers.portable.gateway.admissionlab.test",
        tls: false,
        probes: &[ProbeExpectation {
            path: "/header-probe",
            // Sent so that `set` has something to overwrite. Without
            // it, `set` and `add` would be indistinguishable and an
            // implementation that ignored `set` would pass.
            send_headers: &[("x-admissionlab-request-set", "from-client")],
            status: 200,
            backend: Some("echo-a"),
            observed_path: Some("/header-probe"),
            observed_headers: &[
                ("x-admissionlab-request-added", "request-added"),
                ("x-admissionlab-request-set", "request-set"),
            ],
            response_headers: &[
                ("x-admissionlab-response-added", "response-added"),
                ("x-admissionlab-response-set", "response-set"),
            ],
            location: None,
        }],
        weighted: None,
    },
    // 5. HTTP redirect.
    PortableContract {
        id: "http-redirect",
        route: "redirect-route",
        listener: HTTP_LISTENER,
        host: "redirect.portable.gateway.admissionlab.test",
        tls: false,
        probes: &[ProbeExpectation {
            path: "/redirect-probe",
            send_headers: NO_HEADERS,
            status: 301,
            // Answered by the Gateway itself; no backend is involved,
            // and `None` here is asserted rather than ignored.
            backend: None,
            observed_path: None,
            observed_headers: NO_HEADERS,
            response_headers: NO_HEADERS,
            // The path is the request's own: the filter sets only
            // `hostname`, so a redirect that dropped or rewrote the
            // path would be a different behavior.
            location: Some((
                "http",
                "redirected.portable.gateway.admissionlab.test",
                "/redirect-probe",
            )),
        }],
        weighted: None,
    },
    // 6. URL rewrite.
    PortableContract {
        id: "url-rewrite",
        route: "rewrite-route",
        listener: HTTP_LISTENER,
        host: "rewrite.portable.gateway.admissionlab.test",
        tls: false,
        probes: &[ProbeExpectation {
            path: "/rewrite-me/probe",
            send_headers: NO_HEADERS,
            status: 200,
            backend: Some("echo-a"),
            // The whole contract. A rewrite is invisible from the
            // client side, so the only evidence is what the backend
            // says it received.
            observed_path: Some("/rewritten/probe"),
            observed_headers: NO_HEADERS,
            response_headers: NO_HEADERS,
            location: None,
        }],
        weighted: None,
    },
    // 7. Two-backend weighted routing.
    PortableContract {
        id: "weighted-routing",
        route: "weighted-route",
        listener: HTTP_LISTENER,
        host: "weighted.portable.gateway.admissionlab.test",
        tls: false,
        // No single-request expectation, deliberately: ROADMAP Task 8.7
        // Step 5 forbids classifying one request as weighted-routing
        // correctness, and a `probes` entry here would be exactly that.
        probes: &[],
        weighted: Some(WeightedExpectation {
            path: "/weighted-probe",
            samples: MIN_WEIGHTED_ROUTING_SAMPLES,
            splits: &[("echo-a", 0.8), ("echo-b", 0.2)],
        }),
    },
];

// ---------------------------------------------------------------------
// The two implementations.
// ---------------------------------------------------------------------

/// One certified Gateway API implementation, and everything this suite
/// needs to know that differs between them.
///
/// Five fields, and that is the honest measure of the per-implementation
/// surface: where the recipes are, what the two components are called,
/// which overlay to apply, and what the data plane it provisions is
/// named. Note what is NOT here: nothing about routes, filters,
/// hostnames, backends, or expectations.
struct Implementation {
    /// Stable id, used as the label in every line this suite prints.
    id: &'static str,
    /// The recipe directory, relative to the repository root.
    recipe_dir: &'static str,
    /// The Gateway API CRD component, installed first.
    crds_recipe: &'static str,
    /// The implementation component, installed second.
    recipe: &'static str,
    /// The `GatewayClass` the implementation creates for itself, which
    /// the overlay names.
    gateway_class: &'static str,
    /// The overlay file in `fixtures/gateway/portable/`.
    overlay: &'static str,
    /// The `Deployment`/`Service` the implementation provisions for
    /// [`GATEWAY_NAME`]. Never used to *find* the Service — that is
    /// done by label, see [`endpoint_strategy`] — only to wait for the
    /// data plane and to say what was expected when the lookup
    /// disagrees.
    data_plane: &'static str,
}

/// Both implementations, in the order this suite runs them.
const IMPLEMENTATIONS: [Implementation; 2] = [
    Implementation {
        id: "istio",
        recipe_dir: "recipes/istio-gateway",
        crds_recipe: "gateway-api-crds",
        recipe: "istio-gateway",
        gateway_class: "istio",
        overlay: "gateway-istio.yaml",
        // `<gateway name>-<class name>`.
        data_plane: "lab-gateway-istio",
    },
    Implementation {
        id: "nginx-gateway-fabric",
        recipe_dir: "recipes/nginx-gateway-fabric",
        crds_recipe: "gateway-api-crds-nginx",
        recipe: "nginx-gateway-fabric",
        gateway_class: "nginx",
        overlay: "gateway-nginx.yaml",
        // `<gateway name>-nginx`.
        data_plane: "lab-gateway-nginx",
    },
];

// ---------------------------------------------------------------------
// Cleanup guards. `ScratchRoot` and `ClusterGuard` are copied from
// `tests/istio_gateway_recipe.rs` -- see this file's module
// documentation ("Cleanup discipline").
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

/// The generated TLS Secret manifest, deleted the moment it is no longer
/// needed.
///
/// The file holds a private key in plaintext, which
/// `admissionlab_gateway::tls` permits in exactly two places — a
/// `0600` file inside the run workspace, and a `Secret` in the
/// disposable cluster. This guard is what keeps the first of those as
/// short-lived as it can be: the file exists from just before
/// `apply_gateway_manifests` to just after, and `Drop` removes it on
/// every path including a panic.
struct SecretManifest(PathBuf);

impl SecretManifest {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for SecretManifest {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A `kubectl port-forward` bound to one of the Gateway's ports, closed
/// on every path by [`close_forwards`].
struct Forward {
    port: u16,
    handle: PortForwardHandle,
}

// ---------------------------------------------------------------------
// The real, end-to-end test.
// ---------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Docker and kind"]
async fn the_portable_contract_corpus_runs_against_both_certified_implementations() {
    let outcome = run_corpus().await;
    outcome.expect("portable Gateway contract corpus");
}

/// Validates the fixture tree first (cheap, no cluster), then runs the
/// whole corpus against each implementation in its own disposable
/// cluster, then prints one combined report.
///
/// Both implementations are attempted even if the first fails — the same
/// discipline the per-implementation suites use for Kubernetes versions,
/// and here it is load-bearing: "the corpus runs on both" is the claim,
/// so one run must say which contracts hold on which implementation
/// rather than stopping at the first disagreement.
async fn run_corpus() -> Result<(), String> {
    check_fixture_tree()?;
    let version = kubernetes_version();

    let root = unique_root();
    // Bound before any fallible step below, so `root` cannot leak.
    let _scratch_root_guard = ScratchRoot(root.clone());
    let store = ArtifactStore::new(&root);

    let mut reports: Vec<ImplementationReport> = Vec::new();
    for implementation in &IMPLEMENTATIONS {
        let report = match run_one_implementation(&store, implementation, &version).await {
            Ok(report) => report,
            Err(error) => ImplementationReport {
                id: implementation.id,
                setup_error: Some(error),
                contracts: Vec::new(),
            },
        };
        reports.push(report);
    }

    print_report(&version, &reports);

    let problems: Vec<String> = reports
        .iter()
        .filter_map(ImplementationReport::failure)
        .collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n\n"))
    }
}

/// What one implementation's run observed.
struct ImplementationReport {
    id: &'static str,
    /// Set when the run never got as far as probing contracts (an
    /// install failed, a cluster could not be created). Distinct from a
    /// contract failure, and reported as such: "the corpus does not run
    /// here" and "the corpus runs and one contract disagrees" are
    /// different findings.
    setup_error: Option<String>,
    contracts: Vec<ContractRecord>,
}

impl ImplementationReport {
    fn failure(&self) -> Option<String> {
        if let Some(error) = &self.setup_error {
            return Some(format!("[{}] setup failed\n{error}", self.id));
        }
        let failed: Vec<String> = self
            .contracts
            .iter()
            .filter_map(|record| {
                record
                    .outcome
                    .as_ref()
                    .err()
                    .map(|error| format!("  [{}] {error}", record.id))
            })
            .collect();
        if failed.is_empty() {
            None
        } else {
            Some(format!(
                "[{}] {} of {} contracts failed:\n{}",
                self.id,
                failed.len(),
                self.contracts.len(),
                failed.join("\n")
            ))
        }
    }
}

/// One contract's result on one implementation.
struct ContractRecord {
    id: &'static str,
    /// `Ok` carries the one-line record of what was observed; `Err`
    /// carries why the contract did not hold.
    outcome: Result<String, String>,
}

/// Creates one cluster, installs one implementation, runs every contract
/// against it, and always deletes the cluster before returning.
async fn run_one_implementation(
    store: &ArtifactStore,
    implementation: &Implementation,
    kubernetes_version: &str,
) -> Result<ImplementationReport, String> {
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

    let outcome = install_and_run(&runner, guard.handle(), &paths, implementation).await;
    let deleted = guard.cleanup(&manager).await;

    match (outcome, deleted) {
        (Ok(contracts), Ok(())) => Ok(ImplementationReport {
            id: implementation.id,
            setup_error: None,
            contracts,
        }),
        (Ok(contracts), Err(error)) => Ok(ImplementationReport {
            id: implementation.id,
            setup_error: Some(format!("failed to delete the cluster: {error}")),
            contracts,
        }),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(delete_error)) => Err(format!(
            "{error}\nadditionally, failed to delete the cluster: {delete_error}"
        )),
    }
}

/// Builds the [`ClusterSpec`] for one Kubernetes version, resolving its
/// pinned node image through `compatibility/kubernetes.yaml` exactly as
/// every other real-cluster test in this workspace does.
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

/// Loads the echo image, installs the implementation, applies the
/// corpus, and runs every contract.
async fn install_and_run(
    runner: &dyn ProcessRunner,
    cluster: &ClusterHandle,
    paths: &RunPaths,
    implementation: &Implementation,
) -> Result<Vec<ContractRecord>, String> {
    build_and_load_test_images(runner, &cluster.spec.name).await?;

    let recipes = load_implementation_recipes(implementation)?;
    let components = ordered_components(implementation, &recipes)?;

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

    install_stack(
        cluster,
        &components,
        &dispatcher,
        &readiness,
        COMPONENT_TIMEOUT,
    )
    .await
    .map_err(|error| format!("install_stack failed: {error}"))?;
    println!("[{}] stack installed", implementation.id);

    // The certificate is minted per run, and its private key never
    // leaves this scope other than into the `0600` manifest the guard
    // below deletes and into the cluster's own Secret. Nothing here
    // prints it, and `TestCertificate`'s own `Debug` could not.
    let certificate = generate_test_certificate(TLS_HOST)
        .map_err(|error| format!("failed to generate the test certificate: {error}"))?;

    apply_corpus(cluster, paths, implementation, &certificate).await?;
    println!("[{}] corpus applied", implementation.id);

    for (namespace, name) in [
        (NAMESPACE, implementation.data_plane),
        (NAMESPACE, "echo-a"),
        (NAMESPACE, "echo-b"),
        (REMOTE_NAMESPACE, "echo-b"),
    ] {
        wait_for_deployment(&readiness, cluster, namespace, name).await?;
    }

    // Built before the forwards, so a trust-configuration failure
    // cannot leak a `kubectl port-forward` child.
    let client_config = test_certificate_client_config(&certificate.ca_pem)
        .map_err(|error| format!("failed to build the probe's TLS trust: {error}"))?;
    let tls = ProbeTransport::Tls(Arc::new(client_config));
    let forwards = open_forwards(cluster, implementation).await?;

    let mut records = Vec::new();
    for contract in &CONTRACTS {
        let outcome = run_contract(cluster, implementation, contract, &forwards, &tls).await;
        if let Err(error) = &outcome {
            println!("[{}] {} FAILED: {error}", implementation.id, contract.id);
        }
        records.push(ContractRecord {
            id: contract.id,
            outcome,
        });
    }

    close_forwards(forwards).await?;
    Ok(records)
}

/// Applies the shared fixtures, this implementation's overlay, and the
/// generated TLS Secret, in one call.
///
/// One call rather than four because
/// `admissionlab_gateway::apply::apply_gateway_manifests` parses and
/// hashes every file before applying any object, and sorts every
/// document from every file into one category order — so a single call
/// is both the atomic-failure behavior and the correct ordering, while
/// four calls would be neither.
async fn apply_corpus(
    cluster: &ClusterHandle,
    paths: &RunPaths,
    implementation: &Implementation,
    certificate: &TestCertificate,
) -> Result<(), String> {
    let secret = generate_tls_secret_manifest(paths, certificate)?;

    let mut manifests: Vec<PathBuf> = SHARED_FIXTURES
        .iter()
        .map(|name| fixtures_dir().join(name))
        .collect();
    manifests.push(fixtures_dir().join(implementation.overlay));
    manifests.push(secret.path().to_path_buf());

    let applied = apply_gateway_manifests(cluster, &manifests)
        .await
        .map_err(|error| format!("apply_gateway_manifests failed: {error}"))?;
    // `secret` is dropped here, deleting the file, whatever happened
    // above.
    drop(secret);

    for contract in &CONTRACTS {
        if !applied.objects.iter().any(|key| {
            key.resource == "httproutes"
                && key.name == contract.route
                && key.namespace.as_deref() == Some(NAMESPACE)
        }) {
            return Err(format!(
                "the applied corpus did not include HTTPRoute {NAMESPACE}/{}; applied: {:?}",
                contract.route,
                applied
                    .objects
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            ));
        }
    }
    Ok(())
}

/// Writes the generated certificate into a `kubernetes.io/tls` Secret
/// manifest in the run workspace, with mode `0600`.
///
/// This is the one call site of
/// `admissionlab_gateway::tls::TestCertificate::expose_key_pem` in this
/// suite, and it is exactly one of the two destinations that module
/// permits for a generated key (the other being the Secret in the
/// disposable cluster, which is what this manifest creates). The
/// returned guard deletes the file as soon as the apply that needs it
/// has returned.
///
/// `stringData` rather than `data`, so no base64 encoding is involved:
/// the API server converts it on write, and a hand-rolled encoding step
/// between a private key and a file is a step that could go wrong
/// silently.
fn generate_tls_secret_manifest(
    paths: &RunPaths,
    certificate: &TestCertificate,
) -> Result<SecretManifest, String> {
    let directory = paths.root().join("gateway-tls");
    create_private_dir(&directory)?;
    let path = directory.join("portable-tls-secret.yaml");

    let document = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": { "name": TLS_SECRET, "namespace": NAMESPACE },
        "type": "kubernetes.io/tls",
        "stringData": {
            "tls.crt": String::from_utf8_lossy(&certificate.cert_pem),
            "tls.key": String::from_utf8_lossy(certificate.expose_key_pem()),
        }
    });
    let rendered = serde_norway::to_string(&document)
        .map_err(|error| format!("failed to render the TLS Secret manifest: {error}"))?;

    // The guard is bound BEFORE the write, so a partially written file
    // is still removed.
    let guard = SecretManifest(path.clone());
    write_private_file(&path, rendered.as_bytes())?;
    Ok(guard)
}

/// Creates a directory only its owner can enter.
fn create_private_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt as _;

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|error| format!("failed to create {}: {error}", path.display()))
}

/// Writes a file only its owner can read, creating it with that mode
/// rather than relaxing it afterwards — a `chmod` after the fact leaves
/// a window in which the key was world-readable.
fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
    file.write_all(contents)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

/// Resolves both of the Gateway's ports and opens a port-forward to
/// each.
///
/// Two forwards for the whole corpus rather than one per contract: a
/// forward is a child process and a local socket, both of which cost
/// more to create than every probe that goes through them, and nothing
/// about a contract's meaning depends on having its own.
async fn open_forwards(
    cluster: &ClusterHandle,
    implementation: &Implementation,
) -> Result<Vec<Forward>, String> {
    let identity = GatewayIdentity {
        namespace: NAMESPACE.to_owned(),
        name: GATEWAY_NAME.to_owned(),
    };

    let mut forwards = Vec::new();
    for port in [80_u16, 443] {
        let endpoint = KubeGatewayEndpointResolver::new()
            .resolve(cluster, &identity, &endpoint_strategy(port))
            .await
            .map_err(|error| {
                format!(
                    "resolving the data-plane endpoint for Gateway {identity} port {port} \
                     failed: {error}"
                )
            });
        let endpoint = match endpoint {
            Ok(endpoint) => endpoint,
            Err(error) => {
                close_forwards(forwards).await?;
                return Err(error);
            }
        };
        let expected = GatewayEndpoint {
            namespace: NAMESPACE.to_owned(),
            service: implementation.data_plane.to_owned(),
            port,
        };
        if endpoint != expected {
            close_forwards(forwards).await?;
            return Err(format!(
                "the data-plane lookup resolved to {endpoint}, expected {expected}"
            ));
        }

        match start_service_port_forward(&TokioProcessRunner::new(), cluster, &endpoint).await {
            Ok(handle) => {
                println!(
                    "[{}] port {port} forwarded from {endpoint} to {}",
                    implementation.id, handle.local_addr
                );
                forwards.push(Forward { port, handle });
            }
            Err(error) => {
                close_forwards(forwards).await?;
                return Err(format!(
                    "start_service_port_forward to {endpoint} failed: {error}"
                ));
            }
        }
    }
    Ok(forwards)
}

/// Closes every forward, reporting all failures rather than the first.
async fn close_forwards(forwards: Vec<Forward>) -> Result<(), String> {
    let mut problems = Vec::new();
    for forward in forwards {
        if let Err(error) = forward.handle.close().await {
            problems.push(format!(
                "failed to close the port-forward for port {}: {error}",
                forward.port
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// How the portable corpus finds a data-plane `Service` port.
///
/// By the standard `gateway.networking.k8s.io/gateway-name` label and by
/// PORT NUMBER, never by port name — port names are the one genuinely
/// vendor-specific part of a provisioned Service (Istio names the
/// listener port `http` and adds `status-port` 15021; NGF derives
/// `port-<number>`). Each recipe's own `gatewayEndpoint` strategy
/// encodes its vendor's answer and is certified by that vendor's own
/// suite; a corpus about portability selects on the thing Gateway API
/// itself fixes.
fn endpoint_strategy(port: u16) -> GatewayEndpointStrategy {
    GatewayEndpointStrategy::ServiceBySelector {
        namespace: "{gatewayNamespace}".to_owned(),
        selector: [(GATEWAY_NAME_LABEL.to_owned(), "{gatewayName}".to_owned())]
            .into_iter()
            .collect(),
        port_name: None,
        port: Some(port),
    }
}

// ---------------------------------------------------------------------
// Running one contract.
// ---------------------------------------------------------------------

/// Proves one contract's route reconciled, then proves its probes show
/// what the contract says they must.
async fn run_contract(
    cluster: &ClusterHandle,
    implementation: &Implementation,
    contract: &PortableContract,
    forwards: &[Forward],
    tls: &ProbeTransport,
) -> Result<String, String> {
    let route = route_contract(contract);
    let (evidence, observations) =
        observe_until_reconciled(implementation, cluster, &route).await?;

    let forward = forwards
        .iter()
        .find(|forward| forward.port == contract.port())
        .ok_or_else(|| format!("no port-forward for port {}", contract.port()))?;
    let transport = if contract.tls {
        tls.clone()
    } else {
        ProbeTransport::Plaintext
    };

    let mut notes = vec![format!(
        "reconciled in {:?} over {observations} observation(s)",
        evidence.elapsed
    )];

    for expectation in contract.probes {
        let probe = probe_contract(contract, expectation);
        let observation = execute_probe(forward.handle.local_addr, &probe, &transport)
            .await
            .map_err(|error| format!("probe {} failed: {error}", expectation.path))?;
        notes.push(check_probe(expectation, &observation)?);
    }

    if let Some(weighted) = &contract.weighted {
        let probe = weighted_probe(contract, weighted);
        let tally = probe_many(
            forward.handle.local_addr,
            &probe,
            &transport,
            weighted.samples,
        )
        .await
        .map_err(|error| format!("weighted sampling failed: {error}"))?;
        notes.push(check_weighted(weighted, &tally)?);
    }

    Ok(notes.join("; "))
}

/// Which of the two forwarded ports a contract's probes go through.
impl PortableContract {
    fn port(&self) -> u16 {
        if self.tls { 443 } else { 80 }
    }
}

/// The `RouteContract` one portable contract reconciles under.
///
/// Its `probes` list is deliberately empty: this project's
/// `RouteContract` carries the six-field `HttpProbeContract` that
/// Task 6.1 froze, and every expectation this corpus adds beyond it
/// lives in [`ProbeExpectation`]. Reconciliation is what the
/// `RouteContract` is used for here, and reconciliation reads none of
/// its probes.
fn route_contract(contract: &PortableContract) -> RouteContract {
    RouteContract {
        id: contract.id.to_owned(),
        gateway_namespace: NAMESPACE.to_owned(),
        gateway_name: GATEWAY_NAME.to_owned(),
        route_namespace: NAMESPACE.to_owned(),
        route_name: contract.route.to_owned(),
        listener_name: Some(contract.listener.to_owned()),
        probes: Vec::new(),
    }
}

/// The `HttpProbeContract` for one expectation.
fn probe_contract(
    contract: &PortableContract,
    expectation: &ProbeExpectation,
) -> HttpProbeContract {
    HttpProbeContract {
        host: contract.host.to_owned(),
        path: expectation.path.to_owned(),
        method: "GET".to_owned(),
        headers: expectation
            .send_headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        expected_status: expectation.status,
        expected_backend: expectation.backend.map(ToOwned::to_owned),
    }
}

/// The `HttpProbeContract` every weighted sample uses.
///
/// `expected_status`/`expected_backend` are set to the only honest
/// values available: 200, and *no* expected backend. Naming one would be
/// a per-request classification of a contract that is explicitly not
/// per-request (ROADMAP Task 8.7 Step 5), and `probe_many` reads neither
/// field — it tallies.
fn weighted_probe(
    contract: &PortableContract,
    weighted: &WeightedExpectation,
) -> HttpProbeContract {
    HttpProbeContract {
        host: contract.host.to_owned(),
        path: weighted.path.to_owned(),
        method: "GET".to_owned(),
        headers: BTreeMap::new(),
        expected_status: 200,
        expected_backend: None,
    }
}

/// Every assertion one [`ProbeExpectation`] makes, in the order a reader
/// would want them to fail: status, then who answered, then what they
/// saw, then what came back.
fn check_probe(
    expectation: &ProbeExpectation,
    observation: &ProbeObservation,
) -> Result<String, String> {
    let result = &observation.result;
    if result.status != expectation.status {
        return Err(format!(
            "{}: expected HTTP {}, got {} (headers: {:?})",
            expectation.path, expectation.status, result.status, result.response_headers
        ));
    }
    if result.backend.as_deref() != expectation.backend {
        return Err(format!(
            "{}: expected backend {:?}, the echo response identified {:?}",
            expectation.path, expectation.backend, result.backend
        ));
    }

    check_what_the_backend_saw(expectation, observation)?;
    check_response_headers(expectation, observation)?;
    let location = check_location(expectation, observation)?;

    let backend = expectation
        .backend
        .map_or_else(|| "(no backend)".to_owned(), str::to_owned);
    Ok(format!(
        "{} -> {} {backend}{}",
        expectation.path,
        result.status,
        location.map_or_else(String::new, |raw| format!(" (raw Location: {raw})"))
    ))
}

/// The half of a contract that can only be seen from the backend: the
/// path it observed, and the request headers it received.
fn check_what_the_backend_saw(
    expectation: &ProbeExpectation,
    observation: &ProbeObservation,
) -> Result<(), String> {
    if expectation.observed_path.is_none() && expectation.observed_headers.is_empty() {
        return Ok(());
    }
    let echo = observation.echo.as_ref().ok_or_else(|| {
        format!(
            "{}: this contract asserts what the backend observed, but the response was not an \
             identifiable echo answer",
            expectation.path
        )
    })?;

    if let Some(expected) = expectation.observed_path
        && echo.path != expected
    {
        return Err(format!(
            "{}: the backend observed path {:?}, expected {:?}",
            expectation.path, echo.path, expected
        ));
    }

    for (name, value) in expectation.observed_headers {
        match echo.headers.get(*name) {
            Some(actual) if actual == value => {}
            Some(actual) => {
                return Err(format!(
                    "{}: the backend received {name}: {actual:?}, expected {value:?}",
                    expectation.path
                ));
            }
            None => {
                return Err(format!(
                    "{}: the backend received no {name} header at all; it received {:?}",
                    expectation.path,
                    echo.headers.keys().collect::<Vec<_>>()
                ));
            }
        }
    }
    Ok(())
}

/// The response headers a `ResponseHeaderModifier` filter must have
/// produced.
fn check_response_headers(
    expectation: &ProbeExpectation,
    observation: &ProbeObservation,
) -> Result<(), String> {
    let result = &observation.result;
    for (name, value) in expectation.response_headers {
        match response_header(result, name) {
            Some(actual) if actual == *value => {}
            Some(actual) => {
                return Err(format!(
                    "{}: the response carried {name}: {actual:?}, expected {value:?}",
                    expectation.path
                ));
            }
            None => {
                return Err(format!(
                    "{}: the response carried no {name} header at all; it carried {:?}",
                    expectation.path,
                    result.response_headers.keys().collect::<Vec<_>>()
                ));
            }
        }
    }
    Ok(())
}

/// The redirect contract's `Location`, compared in normal form.
///
/// Returns the RAW header value on success, so the caller can record it.
/// That is deliberate: this is the one header where two conforming
/// implementations are known to differ -- Istio strips the default port,
/// NGF keeps it -- and the comparison below is on a normalized form. A
/// run that printed only "the contract held" would leave the divergence
/// as something someone once read in two vendors' source; printing the
/// raw value makes each run its own provenance.
fn check_location(
    expectation: &ProbeExpectation,
    observation: &ProbeObservation,
) -> Result<Option<String>, String> {
    let Some((scheme, host, path)) = expectation.location else {
        return Ok(None);
    };
    let result = &observation.result;
    let raw = response_header(result, "location")
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            format!(
                "{}: expected a redirect to {scheme}://{host}{path}, but the response carried no \
                 Location header at all; it carried {:?}",
                expectation.path,
                result.response_headers.keys().collect::<Vec<_>>()
            )
        })?;
    let location = redirect_location(result).ok_or_else(|| {
        format!(
            "{}: the response carried Location {raw:?}, which is not a URI this can normalize",
            expectation.path
        )
    })?;

    // Scheme, host and path only. The port is normalized away because
    // two conforming implementations disagree about it and because the
    // probe's own origin port is a port-forward artifact -- see
    // `normalize_location`, and the fixture tree's `README.md`.
    let matches = location.scheme.as_deref() == Some(scheme)
        && location.host.as_deref() == Some(host)
        && location.path == path;
    if matches {
        Ok(Some(raw))
    } else {
        Err(format!(
            "{}: expected a redirect to {scheme}://{host}{path}, got {location} (raw: {raw:?})",
            expectation.path
        ))
    }
}

/// The weighted contract's whole assertion, and the numbers it records.
///
/// ROADMAP Task 8.7 Step 5, clause by clause: the bound is
/// `abs(observed - p) <= max(0.05, 4 * sqrt(p * (1-p) / n))`; `n` is at
/// least 1000; the counts and the tolerance are recorded; and no single
/// request is ever classified.
fn check_weighted(weighted: &WeightedExpectation, tally: &ProbeTally) -> Result<String, String> {
    if weighted.samples < MIN_WEIGHTED_ROUTING_SAMPLES {
        return Err(format!(
            "a weighted contract must sample at least {MIN_WEIGHTED_ROUTING_SAMPLES} requests, \
             this one asked for {}",
            weighted.samples
        ));
    }

    // Every sample must have been a real 200 from a real backend. A
    // split that "held" while a tenth of the requests were answered by
    // the data plane's own error page is not a split.
    if tally.unidentified > 0 {
        return Err(format!(
            "{} of {} requests were not answered by an identifiable backend (statuses: {:?}) -- \
             a weighted split cannot be read off a sample that includes them",
            tally.unidentified, tally.requests, tally.by_status
        ));
    }
    let non_200: BTreeMap<u16, u32> = tally
        .by_status
        .iter()
        .filter(|(status, _)| **status != 200)
        .map(|(status, count)| (*status, *count))
        .collect();
    if !non_200.is_empty() {
        return Err(format!(
            "{} of {} requests did not return 200: {non_200:?}",
            non_200.values().sum::<u32>(),
            tally.requests
        ));
    }

    let mut records = Vec::new();
    let mut failures = Vec::new();
    for (backend, expected) in weighted.splits {
        let count = tally.by_backend.get(*backend).copied().unwrap_or(0);
        let observed = tally.share(backend);
        let tolerance = weighted_routing_tolerance(*expected, tally.requests);
        let delta = (observed - expected).abs();
        let held = weighted_routing_within_tolerance(observed, *expected, tally.requests);
        records.push(format!(
            "{backend}={count}/{} (observed {observed:.4}, expected {expected:.4}, \
             |delta| {delta:.4}, tolerance {tolerance:.4}, margin {:.4}, {})",
            tally.requests,
            tolerance - delta,
            if held { "within" } else { "OUTSIDE" }
        ));
        if !held {
            failures.push(format!(
                "{backend}: observed {observed:.4} of {} requests ({count}), expected \
                 {expected:.4}; |delta| {delta:.4} exceeds the tolerance {tolerance:.4}",
                tally.requests
            ));
        }
    }

    // Recorded whether it held or not: the roadmap asks for the counts
    // and the tolerance, and a failing run is exactly when a reader
    // most needs them.
    let record = format!(
        "weighted over {} requests: {}",
        tally.requests,
        records.join(", ")
    );
    println!("    {record}");

    let unexpected: Vec<&String> = tally
        .by_backend
        .keys()
        .filter(|backend| {
            !weighted
                .splits
                .iter()
                .any(|(expected, _)| *expected == backend.as_str())
        })
        .collect();
    if !unexpected.is_empty() {
        failures.push(format!(
            "requests reached backends this contract names no weight for: {unexpected:?}"
        ));
    }

    if failures.is_empty() {
        Ok(record)
    } else {
        Err(format!("{record}\n    {}", failures.join("\n    ")))
    }
}

// ---------------------------------------------------------------------
// Reconciliation, copied in shape from the two per-implementation
// certification suites -- see `tests/istio_gateway_recipe.rs`'s
// "THE SECOND FINDING" for why a stable status is not a finished one.
// ---------------------------------------------------------------------

/// Observes a route until its status is not merely *stable* but
/// *correct*, or until [`RECONCILIATION_TIMEOUT`] elapses.
async fn observe_until_reconciled(
    implementation: &Implementation,
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
        match assert_reconciled(implementation, contract, &evidence) {
            Ok(()) => return Ok((evidence, observations)),
            Err(reason) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "the route never reached the expected status within \
                         {RECONCILIATION_TIMEOUT:?} ({observations} observation(s)); the last \
                         one said: {reason}"
                    ));
                }
                tokio::time::sleep(REOBSERVE_INTERVAL).await;
            }
        }
    }
}

/// The full convergence claim: converged, the `GatewayClass` accepted,
/// the `Gateway` accepted and programmed, and the route's own parent
/// entry accepted with resolved references — every one `True` and
/// observed at the object's current generation.
fn assert_reconciled(
    implementation: &Implementation,
    contract: &RouteContract,
    evidence: &ReconciliationEvidence,
) -> Result<(), String> {
    if !evidence.converged {
        return Err(format!(
            "the route never converged; diagnostics: {:?}, gateway conditions: {:?}, route \
             parents: {:?}",
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
            "the Gateway named no GatewayClass, or {:?} does not exist on this cluster",
            implementation.gateway_class
        )
    })?;
    if class.name != implementation.gateway_class {
        return Err(format!(
            "expected the Gateway to name GatewayClass {:?}, got {:?}",
            implementation.gateway_class, class.name
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
                "the HTTPRoute published {count} status entries matching this contract's parent"
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

/// Runs `bash scripts/build-test-images.sh <cluster>`, exactly as the
/// two per-implementation certification suites already do.
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
        spill_dir: None,
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
// Loading recipes, choosing a Kubernetes version, printing the report.
// ---------------------------------------------------------------------

/// Loads both recipe documents for one implementation.
fn load_implementation_recipes(implementation: &Implementation) -> Result<Vec<Recipe>, String> {
    let dir = repo_root().join(implementation.recipe_dir);
    let recipes = load_recipe_overrides(&dir)
        .map_err(|error| format!("failed to load {}: {error}", implementation.recipe_dir))?;
    if recipes.len() != 2 {
        return Err(format!(
            "expected exactly 2 recipe documents in {} (the Gateway API CRD bundle and the \
             implementation), got {}",
            implementation.recipe_dir,
            recipes.len()
        ));
    }
    Ok(recipes)
}

/// The two components, in install order: the Gateway API CRDs first,
/// then the implementation. Written out explicitly rather than taken
/// from the loader's filename ordering, exactly as both
/// per-implementation suites do.
fn ordered_components(
    implementation: &Implementation,
    recipes: &[Recipe],
) -> Result<Vec<ResolvedComponent>, String> {
    Ok(vec![
        component(find(recipes, implementation.crds_recipe)?),
        component(find(recipes, implementation.recipe)?),
    ])
}

fn find(recipes: &[Recipe], name: &str) -> Result<Recipe, String> {
    recipes
        .iter()
        .find(|recipe| recipe.name == name)
        .cloned()
        .ok_or_else(|| format!("no {name:?} recipe among the {} loaded", recipes.len()))
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

/// The Kubernetes version this suite runs on: `ADMISSIONLAB_CERTIFY_KUBERNETES`
/// when set, else `compatibility/kubernetes.yaml`'s Tier-1 primary.
///
/// See this file's module documentation for why one version rather than
/// the certified sweep the per-implementation suites do.
fn kubernetes_version() -> String {
    if let Ok(requested) = std::env::var(CERTIFY_KUBERNETES_ENV) {
        let requested = requested.trim().to_owned();
        if !requested.is_empty() {
            return requested;
        }
    }
    PRIMARY_KUBERNETES_VERSION.to_owned()
}

/// `compatibility/kubernetes.yaml`'s Tier-1 primary, restated here so
/// that changing it there without thinking about this suite fails
/// [`the_primary_kubernetes_version_is_still_supported`] in
/// milliseconds.
const PRIMARY_KUBERNETES_VERSION: &str = "1.36.4";

/// Prints the combined per-contract, per-implementation report.
///
/// This is the artifact ROADMAP Task 8.7 and the Phase 8 gate actually
/// want: one table saying which contracts hold on which implementation,
/// with the weighted contract's counts and tolerance already printed
/// beside it by [`check_weighted`].
fn print_report(kubernetes_version: &str, reports: &[ImplementationReport]) {
    println!(
        "\n=== portable Gateway contract corpus (Kubernetes {kubernetes_version}) ===\n\
         {} contracts x {} implementations",
        CONTRACTS.len(),
        IMPLEMENTATIONS.len()
    );
    for report in reports {
        println!("\n[{}]", report.id);
        if let Some(error) = &report.setup_error {
            println!("  setup: {error}");
        }
        for record in &report.contracts {
            match &record.outcome {
                Ok(detail) => println!("  PASS {:<18} {detail}", record.id),
                Err(error) => println!("  FAIL {:<18} {error}", record.id),
            }
        }
    }
    println!();
}

// ---------------------------------------------------------------------
// Small standalone helpers.
// ---------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    repo_root().join("fixtures/gateway/portable")
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
        "admissionlab-portable-contracts-test-{}",
        unique.as_str()
    ))
}

// =====================================================================
// The fixture-tree half: everything that can be checked without a
// cluster, and what actually stops this corpus rotting between
// integration runs.
// =====================================================================

/// Every cheap check on the fixture tree. The integration test runs this
/// before creating a cluster; the tests below are what make it also run
/// in CI's fast lane.
fn check_fixture_tree() -> Result<(), String> {
    let directory = fixtures_dir();
    if !directory.is_dir() {
        return Err(format!("{} is not a directory", directory.display()));
    }
    for name in SHARED_FIXTURES
        .iter()
        .chain(IMPLEMENTATIONS.iter().map(|i| &i.overlay))
    {
        let path = directory.join(name);
        if !path.is_file() {
            return Err(format!("{} is missing", path.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod fixture_tests {
    use serde::Deserialize as _;

    use super::{
        CONTRACTS, GATEWAY_NAME, HTTP_LISTENER, HTTPS_LISTENER, IMPLEMENTATIONS,
        MIN_WEIGHTED_ROUTING_SAMPLES, NAMESPACE, PRIMARY_KUBERNETES_VERSION, REFERENCE_GRANT,
        REMOTE_BACKEND_ID, REMOTE_NAMESPACE, SHARED_FIXTURES, TEST_TLD, TLS_HOST, TLS_SECRET,
        check_fixture_tree, fixtures_dir, repo_root,
    };

    /// The corpus is present and complete.
    #[test]
    fn the_fixture_tree_is_complete() {
        check_fixture_tree().expect("fixture tree");
    }

    /// The whole portability claim in one assertion: the shared files
    /// name no `GatewayClass`, no vendor CRD, and nothing else an
    /// implementation owns.
    ///
    /// This is the test that would fail if someone "fixed" a portable
    /// route by reaching for a vendor annotation, which is the single
    /// most likely way this corpus stops being portable.
    #[test]
    fn the_shared_fixtures_name_no_implementation() {
        let portable_kinds = [
            "Namespace",
            "ConfigMap",
            "Service",
            "Deployment",
            "ReferenceGrant",
            "HTTPRoute",
        ];
        for name in SHARED_FIXTURES {
            let text = read(&fixtures_dir().join(name));
            for vendor in ["istio", "nginx", "gatewayClassName", "NginxProxy"] {
                assert!(
                    !uncommented(&text).contains(vendor),
                    "{name} mentions {vendor:?} outside a comment; the shared fixtures must name \
                     no implementation -- that is what gateway-istio.yaml and gateway-nginx.yaml \
                     are for"
                );
            }
            for document in documents(&fixtures_dir().join(name)) {
                let kind = kind_of(&document)
                    .expect("every document declares a kind")
                    .to_owned();
                assert!(
                    portable_kinds.contains(&kind.as_str()),
                    "{name} declares a {kind}, which is not one of the portable kinds \
                     {portable_kinds:?}"
                );
            }
        }
    }

    /// The two overlays differ in exactly the two ways the fixture
    /// tree's `README.md` says they do: the class name, and Istio's
    /// data-plane `ConfigMap` plus the `parametersRef` naming it.
    ///
    /// Asserted by comparing the two `Gateway` objects field by field
    /// after removing those two things, so a third difference sneaking
    /// in -- a listener port, a `tls.mode`, an `allowedRoutes` -- fails
    /// here rather than showing up as a mysterious per-implementation
    /// probe result an hour into a cluster run.
    #[test]
    fn the_two_overlays_differ_only_in_the_documented_ways() {
        let mut gateways = Vec::new();
        for implementation in &IMPLEMENTATIONS {
            let docs = documents(&fixtures_dir().join(implementation.overlay));
            let mut gateway = document(&docs, "Gateway", GATEWAY_NAME)
                .unwrap_or_else(|| panic!("{} must declare a Gateway", implementation.overlay));

            let class = gateway
                .get("spec")
                .and_then(|spec| spec.get("gatewayClassName"))
                .and_then(serde_norway::Value::as_str)
                .unwrap_or_else(|| {
                    panic!("{} must name a gatewayClassName", implementation.overlay)
                });
            assert_eq!(
                class, implementation.gateway_class,
                "{} names the wrong GatewayClass",
                implementation.overlay
            );

            let spec = gateway
                .get_mut("spec")
                .and_then(serde_norway::Value::as_mapping_mut)
                .expect("a Gateway has a spec");
            spec.remove("gatewayClassName");
            // Istio's only extra field. Removed here so what remains is
            // the part that MUST be identical.
            let infrastructure = spec.remove("infrastructure");
            assert_eq!(
                infrastructure.is_some(),
                implementation.id == "istio",
                "only the Istio overlay may carry spec.infrastructure -- see its own header for \
                 the kind cluster finding that forces it, and gateway-nginx.yaml's for why NGF \
                 needs no counterpart"
            );

            let vendor_objects: Vec<String> = docs
                .iter()
                .filter_map(|doc| kind_of(doc).map(ToOwned::to_owned))
                .filter(|kind| kind != "Gateway")
                .collect();
            assert_eq!(
                vendor_objects,
                if implementation.id == "istio" {
                    vec!["ConfigMap".to_owned()]
                } else {
                    Vec::new()
                },
                "{} carries unexpected vendor objects",
                implementation.overlay
            );

            gateways.push((implementation.overlay, gateway));
        }

        let (first_name, first) = &gateways[0];
        let (second_name, second) = &gateways[1];
        assert_eq!(
            first, second,
            "{first_name} and {second_name} differ in something other than gatewayClassName and \
             Istio's spec.infrastructure -- every listener, port, TLS setting and allowedRoutes \
             rule must be identical, because that identity IS the portability claim"
        );
    }

    /// Both overlays declare the two listeners the corpus needs, on the
    /// ports it resolves, with TLS terminating against the generated
    /// Secret.
    #[test]
    fn both_overlays_declare_the_same_two_listeners() {
        for implementation in &IMPLEMENTATIONS {
            let docs = documents(&fixtures_dir().join(implementation.overlay));
            let gateway = document(&docs, "Gateway", GATEWAY_NAME).expect("a Gateway");
            let listeners = gateway
                .get("spec")
                .and_then(|spec| spec.get("listeners"))
                .and_then(serde_norway::Value::as_sequence)
                .expect("a Gateway declares listeners");
            assert_eq!(listeners.len(), 2, "{}", implementation.overlay);

            let http = &listeners[0];
            assert_eq!(field(http, "name"), Some(HTTP_LISTENER));
            assert_eq!(
                http.get("port").and_then(serde_norway::Value::as_u64),
                Some(80)
            );
            assert_eq!(field(http, "protocol"), Some("HTTP"));

            let https = &listeners[1];
            assert_eq!(field(https, "name"), Some(HTTPS_LISTENER));
            assert_eq!(
                https.get("port").and_then(serde_norway::Value::as_u64),
                Some(443)
            );
            assert_eq!(field(https, "protocol"), Some("HTTPS"));
            let tls = https.get("tls").expect("the HTTPS listener declares tls");
            assert_eq!(
                field(tls, "mode"),
                Some("Terminate"),
                "Passthrough is Extended support and would defeat the contract"
            );
            let certificate = tls
                .get("certificateRefs")
                .and_then(serde_norway::Value::as_sequence)
                .and_then(|refs| refs.first())
                .expect("the HTTPS listener names a certificate");
            assert_eq!(field(certificate, "kind"), Some("Secret"));
            assert_eq!(field(certificate, "name"), Some(TLS_SECRET));
        }
    }

    /// No TLS key material has ever been checked into this directory --
    /// the Secret the overlays name is generated per run.
    ///
    /// Two sweeps, and the split is deliberate. The PEM-marker scan is
    /// pure text and runs over EVERY file, because a leaked key would
    /// not necessarily be in a well-formed manifest. The `kind: Secret`
    /// scan parses, and therefore runs over `*.yaml` only: this
    /// directory also holds a `README.md`, and handing arbitrary
    /// Markdown to a YAML document iterator is not a thing to do (an
    /// unparseable document can leave the iterator unable to advance).
    #[test]
    fn no_key_material_is_checked_in() {
        for entry in std::fs::read_dir(fixtures_dir()).expect("the fixture directory is readable") {
            let path = entry.expect("a readable directory entry").path();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                !text.contains("PRIVATE KEY"),
                "{} contains what looks like private key material; the portable TLS Secret is \
                 generated per run and must never be committed",
                path.display()
            );
            if path.extension().and_then(std::ffi::OsStr::to_str) != Some("yaml") {
                continue;
            }
            for document in documents(&path) {
                assert_ne!(
                    kind_of(&document),
                    Some("Secret"),
                    "{} declares a Secret; the portable TLS Secret is generated per run",
                    path.display()
                );
            }
        }
    }

    /// Every contract in [`CONTRACTS`] names a route that exists, with
    /// the hostname and listener it claims -- so the Rust table and the
    /// YAML cannot drift apart.
    #[test]
    fn every_contract_matches_the_route_it_names() {
        let docs = documents(&fixtures_dir().join("routes.yaml"));
        let routes: Vec<&serde_norway::Value> = docs
            .iter()
            .filter(|doc| kind_of(doc) == Some("HTTPRoute"))
            .collect();
        assert_eq!(
            routes.len(),
            CONTRACTS.len(),
            "routes.yaml declares {} HTTPRoutes but there are {} contracts -- every route must \
             be under contract and every contract must have a route",
            routes.len(),
            CONTRACTS.len()
        );

        for contract in &CONTRACTS {
            let route = document(&docs, "HTTPRoute", contract.route)
                .unwrap_or_else(|| panic!("routes.yaml must declare HTTPRoute {}", contract.route));
            assert_eq!(
                route
                    .get("metadata")
                    .and_then(|metadata| metadata.get("namespace"))
                    .and_then(serde_norway::Value::as_str),
                Some(NAMESPACE),
                "{} must live beside the Gateway",
                contract.route
            );

            let parent = route
                .get("spec")
                .and_then(|spec| spec.get("parentRefs"))
                .and_then(serde_norway::Value::as_sequence)
                .and_then(|refs| refs.first())
                .unwrap_or_else(|| panic!("{} must declare a parentRef", contract.route));
            assert_eq!(field(parent, "name"), Some(GATEWAY_NAME));
            assert_eq!(
                field(parent, "sectionName"),
                Some(contract.listener),
                "{} names a different listener than its contract",
                contract.route
            );

            let hostnames = route
                .get("spec")
                .and_then(|spec| spec.get("hostnames"))
                .and_then(serde_norway::Value::as_sequence)
                .unwrap_or_else(|| panic!("{} must declare a hostname", contract.route));
            assert_eq!(hostnames.len(), 1, "one hostname per contract");
            assert_eq!(
                hostnames[0].as_str(),
                Some(contract.host),
                "{} answers on a different hostname than its contract probes",
                contract.route
            );
            // `TEST_TLD` rather than a literal, so this rule and the
            // one `generate_test_certificate` enforces are the same
            // constant -- a hostname this accepts is a hostname a
            // certificate can be issued for.
            assert!(
                contract.host.to_ascii_lowercase().ends_with(TEST_TLD),
                "{} probes {:?}, which is not under the reserved {TEST_TLD} TLD",
                contract.id,
                contract.host
            );
            assert_eq!(
                contract.tls,
                contract.listener == HTTPS_LISTENER,
                "{} disagrees with itself about whether it is a TLS contract",
                contract.id
            );
        }
    }

    /// The TLS contract's hostname is the one the certificate is issued
    /// for -- three places (the constant, the route, the probe) that
    /// must agree, held together here.
    #[test]
    fn the_tls_contract_uses_the_certificate_hostname() {
        let tls = CONTRACTS
            .iter()
            .find(|contract| contract.tls)
            .expect("one TLS contract");
        assert_eq!(tls.host, TLS_HOST);
        assert_eq!(
            CONTRACTS.iter().filter(|contract| contract.tls).count(),
            1,
            "a second TLS contract would need a second certificate or a shared one; neither is \
             wired, so this is a deliberate tripwire"
        );
    }

    /// The weighted contract meets ROADMAP Task 8.7 Step 5's floor, and
    /// its weights are the ones the route actually declares.
    #[test]
    fn the_weighted_contract_meets_the_roadmaps_sample_floor() {
        let contract = CONTRACTS
            .iter()
            .find(|contract| contract.weighted.is_some())
            .expect("one weighted contract");
        let weighted = contract.weighted.as_ref().expect("checked above");
        assert!(
            weighted.samples >= MIN_WEIGHTED_ROUTING_SAMPLES,
            "the roadmap requires at least {MIN_WEIGHTED_ROUTING_SAMPLES} samples, this asks for \
             {}",
            weighted.samples
        );
        let total: f64 = weighted.splits.iter().map(|(_, share)| share).sum();
        assert!(
            (total - 1.0).abs() < 1e-9,
            "the expected proportions must sum to 1.0, they sum to {total}"
        );
        assert!(
            contract.probes.is_empty(),
            "a weighted contract must not carry a single-request expectation -- ROADMAP Task 8.7 \
             Step 5 forbids classifying one request as weighted-routing correctness"
        );

        let docs = documents(&fixtures_dir().join("routes.yaml"));
        let route = document(&docs, "HTTPRoute", contract.route).expect("the weighted route");
        let backends = route
            .get("spec")
            .and_then(|spec| spec.get("rules"))
            .and_then(serde_norway::Value::as_sequence)
            .and_then(|rules| rules.first())
            .and_then(|rule| rule.get("backendRefs"))
            .and_then(serde_norway::Value::as_sequence)
            .expect("the weighted route declares backendRefs");
        let declared: f64 = backends
            .iter()
            .map(|backend| {
                backend
                    .get("weight")
                    .and_then(serde_norway::Value::as_f64)
                    .expect("every weighted backendRef declares a weight")
            })
            .sum();
        for (backend, expected) in weighted.splits {
            let weight = backends
                .iter()
                .find(|declared| field(declared, "name") == Some(backend))
                .and_then(|declared| declared.get("weight"))
                .and_then(serde_norway::Value::as_f64)
                .unwrap_or_else(|| panic!("the route must declare a weight for {backend}"));
            let share = weight / declared;
            assert!(
                (share - expected).abs() < 1e-9,
                "the route gives {backend} weight {weight} of {declared} (= {share}), but the \
                 contract expects {expected}"
            );
        }
    }

    /// The cross-namespace contract's `ReferenceGrant` is in the
    /// namespace that owns the referenced Service and names exactly the
    /// one Service the route refers to.
    #[test]
    fn the_reference_grant_permits_exactly_the_reference_the_route_makes() {
        let backends = documents(&fixtures_dir().join("backends.yaml"));
        let grant = document(&backends, "ReferenceGrant", REFERENCE_GRANT)
            .expect("backends.yaml must declare the ReferenceGrant");
        assert_eq!(
            grant
                .get("metadata")
                .and_then(|metadata| metadata.get("namespace"))
                .and_then(serde_norway::Value::as_str),
            Some(REMOTE_NAMESPACE),
            "the grant belongs in the namespace that owns the referenced Service"
        );
        let from = grant
            .get("spec")
            .and_then(|spec| spec.get("from"))
            .and_then(serde_norway::Value::as_sequence)
            .and_then(|entries| entries.first())
            .expect("the grant names what it permits references from");
        assert_eq!(field(from, "namespace"), Some(NAMESPACE));
        assert_eq!(field(from, "kind"), Some("HTTPRoute"));
        let to = grant
            .get("spec")
            .and_then(|spec| spec.get("to"))
            .and_then(serde_norway::Value::as_sequence)
            .and_then(|entries| entries.first())
            .expect("the grant names what it permits references to");
        assert_eq!(
            field(to, "name"),
            Some("echo-b"),
            "the grant must name the one Service, not every Service in the namespace"
        );

        let routes = documents(&fixtures_dir().join("routes.yaml"));
        let route = document(&routes, "HTTPRoute", "cross-namespace-route")
            .expect("the cross-namespace route");
        let backend = route
            .get("spec")
            .and_then(|spec| spec.get("rules"))
            .and_then(serde_norway::Value::as_sequence)
            .and_then(|rules| rules.first())
            .and_then(|rule| rule.get("backendRefs"))
            .and_then(serde_norway::Value::as_sequence)
            .and_then(|refs| refs.first())
            .expect("the cross-namespace route names a backend");
        assert_eq!(field(backend, "namespace"), Some(REMOTE_NAMESPACE));
        assert_eq!(field(backend, "name"), Some("echo-b"));
    }

    /// Every echo backend is `fixtures/gateway/backends/echo-{a,b}.yaml`
    /// plus a namespace -- and, for the remote copy only, a changed
    /// `ADMISSIONLAB_BACKEND_ID`. See `backends.yaml`'s own header for
    /// why those two exemptions and no others.
    #[test]
    fn the_portable_backends_match_the_shared_echo_definitions() {
        // (source definition, namespace, expected backend id)
        let expected = [
            ("echo-a", NAMESPACE, "echo-a"),
            ("echo-b", NAMESPACE, "echo-b"),
            ("echo-b", REMOTE_NAMESPACE, REMOTE_BACKEND_ID),
        ];
        let fixture = documents(&fixtures_dir().join("backends.yaml"));

        for (source, namespace, backend_id) in expected {
            let shared = documents(
                &repo_root()
                    .join("fixtures/gateway/backends")
                    .join(format!("{source}.yaml")),
            );
            for kind in ["Service", "Deployment"] {
                let mut want = document(&shared, kind, source)
                    .unwrap_or_else(|| panic!("{kind}/{source} must exist in the shared backend"));
                let mut got = documents_in_namespace(&fixture, kind, source, namespace)
                    .unwrap_or_else(|| {
                        panic!("{kind}/{source} must exist in backends.yaml for {namespace}")
                    });

                assert_eq!(take_namespace(&mut got).as_deref(), Some(namespace));
                // The shared definition deliberately carries none.
                assert_eq!(take_namespace(&mut want), None);

                if kind == "Deployment" {
                    assert_eq!(
                        backend_id_of(&got).as_deref(),
                        Some(backend_id),
                        "{kind}/{source} in {namespace} must answer with {backend_id:?}"
                    );
                    set_backend_id(&mut got, backend_id_of(&want).expect("shared backend id"));
                }

                assert_eq!(
                    got, want,
                    "{kind}/{source} in {namespace} has drifted from \
                     fixtures/gateway/backends/{source}.yaml in a field other than \
                     metadata.namespace and ADMISSIONLAB_BACKEND_ID"
                );
            }
        }
    }

    /// The Kubernetes version this suite pins is still one
    /// `compatibility/kubernetes.yaml` supports.
    #[test]
    fn the_primary_kubernetes_version_is_still_supported() {
        let matrix = admissionlab_cluster::load_matrix().expect("the Kubernetes matrix loads");
        admissionlab_cluster::resolve_node_image(PRIMARY_KUBERNETES_VERSION, &matrix)
            .unwrap_or_else(|error| {
                panic!(
                    "this suite pins Kubernetes {PRIMARY_KUBERNETES_VERSION}, which \
                     compatibility/kubernetes.yaml no longer resolves: {error}"
                )
            });
    }

    // -----------------------------------------------------------------
    // YAML helpers.
    // -----------------------------------------------------------------

    fn read(path: &std::path::Path) -> String {
        std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
    }

    /// The file with every `#` comment line removed, so a test that
    /// looks for a vendor name is not tripped by a comment explaining
    /// why that vendor's name is absent.
    fn uncommented(text: &str) -> String {
        text.lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn documents(path: &std::path::Path) -> Vec<serde_norway::Value> {
        let text = read(path);
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

    fn field<'a>(value: &'a serde_norway::Value, key: &str) -> Option<&'a str> {
        value.get(key).and_then(serde_norway::Value::as_str)
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

    fn documents_in_namespace(
        documents: &[serde_norway::Value],
        kind: &str,
        name: &str,
        namespace: &str,
    ) -> Option<serde_norway::Value> {
        documents
            .iter()
            .find(|document| {
                let metadata = document.get("metadata");
                kind_of(document) == Some(kind)
                    && metadata
                        .and_then(|metadata| metadata.get("name"))
                        .and_then(serde_norway::Value::as_str)
                        == Some(name)
                    && metadata
                        .and_then(|metadata| metadata.get("namespace"))
                        .and_then(serde_norway::Value::as_str)
                        == Some(namespace)
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

    /// The `ADMISSIONLAB_BACKEND_ID` env value of a Deployment's one
    /// container.
    fn backend_id_of(document: &serde_norway::Value) -> Option<String> {
        backend_id_entry(document)?
            .get("value")
            .and_then(serde_norway::Value::as_str)
            .map(str::to_owned)
    }

    fn set_backend_id(document: &mut serde_norway::Value, value: String) {
        let entry = containers_mut(document)
            .and_then(|containers| containers.first_mut())
            .and_then(|container| container.get_mut("env"))
            .and_then(serde_norway::Value::as_sequence_mut)
            .and_then(|env| {
                env.iter_mut().find(|entry| {
                    entry.get("name").and_then(serde_norway::Value::as_str)
                        == Some("ADMISSIONLAB_BACKEND_ID")
                })
            })
            .expect("a Deployment declares ADMISSIONLAB_BACKEND_ID");
        if let Some(mapping) = entry.as_mapping_mut() {
            mapping.insert("value".into(), value.into());
        }
    }

    fn backend_id_entry(document: &serde_norway::Value) -> Option<&serde_norway::Value> {
        document
            .get("spec")?
            .get("template")?
            .get("spec")?
            .get("containers")?
            .as_sequence()?
            .first()?
            .get("env")?
            .as_sequence()?
            .iter()
            .find(|entry| {
                entry.get("name").and_then(serde_norway::Value::as_str)
                    == Some("ADMISSIONLAB_BACKEND_ID")
            })
    }

    fn containers_mut(document: &mut serde_norway::Value) -> Option<&mut Vec<serde_norway::Value>> {
        document
            .get_mut("spec")?
            .get_mut("template")?
            .get_mut("spec")?
            .get_mut("containers")?
            .as_sequence_mut()
    }
}
