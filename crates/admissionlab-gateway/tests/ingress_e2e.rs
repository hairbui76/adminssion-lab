//! ROADMAP Task 8.4: the legacy `Ingress` behavior runner, against a
//! real cluster running a real archived `ingress-nginx`.
//!
//! **The upstream this exercises is retired and its repository is
//! archived.** `recipes/ingress-nginx-legacy/README.md` carries the
//! dates and Admission Lab's stance: the stack is installed so a
//! migration *away* from it can be measured, and for no other reason.
//!
//! Two halves in one file, the layout
//! `admissionlab-recipes`' own `tests/ingress_nginx_legacy.rs` uses:
//!
//! - An **integration half** (`#[ignore]`d -- needs Docker, `kind` and
//!   `helm`) that creates a disposable cluster, installs the pinned
//!   legacy recipe through the real installer pipeline, and drives
//!   [`admissionlab_gateway::run_ingress_case`] over both migration
//!   fixtures: the routing one must come back admitted, ready, and
//!   answered by `echo-a`; the deny one must come back `admitted:
//!   false` with the validating webhook's own message preserved.
//! - A **fast half** that needs no cluster and runs under plain
//!   `cargo test --workspace`: the two rules the integration half can
//!   only observe indirectly -- which errors are admission evidence and
//!   which are failures, and what "this probe matched its contract"
//!   means -- driven directly against synthesized values.
//!
//! # What the integration half proves that Task 8.2's does not
//!
//! `admissionlab-recipes`' certification test proves the *recipe*
//! works: the chart installs, an `Ingress` routes, the webhook denies.
//! It does so with a hand-written probe loop and a hand-written
//! `kubectl apply`, because at Task 8.2 there was no runner to use.
//!
//! This test proves the *runner* works, over the same cluster and the
//! same two fixtures: that [`admissionlab_gateway::run_ingress_case`]
//! reaches the same conclusions through the shipped code path, that a
//! denial arrives as data rather than as an error, and that an
//! `IngressCaseResult`'s four fields say what actually happened. The
//! duplication is the point -- if the two ever disagree, one of them is
//! wrong about a real cluster.
//!
//! # Cleanup discipline
//!
//! Copied from that test, for the same reasons: a `ScratchRoot` guard
//! removes the artifact directory, a `ClusterGuard` deletes the cluster
//! on every path (and warns loudly if it could not), and both are bound
//! before any fallible step. The runner itself closes its own
//! `kubectl port-forward` on every path -- that is
//! `ingress.rs`'s job, not this file's, and
//! `admissionlab-cli`'s `pipeline::gateway` tests already pin it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use admissionlab_cluster::{KindClusterManager, cluster_name};
use admissionlab_core::{
    ArtifactStore, ClusterError, ClusterHandle, ClusterManager, ClusterSpec, CommandSpec,
    ProcessRunner, RunId, Side, TokioProcessRunner,
};
use admissionlab_gateway::{
    DIAGNOSTIC_INGRESS_DENIED, HttpProbeContract, MigrationCaseSpec, run_ingress_case,
};
use admissionlab_installer::{
    CompositeInstaller, HelmInstaller, KubeReadinessProbe, ManifestsInstaller, install_stack,
};
use admissionlab_recipes::{
    GatewayEndpointStrategy, Recipe, load_recipe_compatibility, load_recipe_overrides,
};
use admissionlab_spec::ResolvedComponent;

/// The recipe under test. Loaded from disk rather than restated: every
/// pin it carries is already asserted by
/// `admissionlab-recipes`' own certification test, and a second copy
/// here could only drift from it.
const RECIPE_DIR: &str = "recipes/ingress-nginx-legacy";

/// The recipe's own name, which is also its Helm release name and its
/// namespace. The one literal this file needs, because
/// `install_stack`'s result is checked against it.
const RECIPE_NAME: &str = "ingress-nginx-legacy";

/// The routing fixture and everything its probe asserts.
const BASIC_FIXTURE: &str = "basic-routing.yaml";
const BASIC_HOST: &str = "basic.ingress.admissionlab.test";
const BACKEND: &str = "echo-a";

/// The deny fixture, its host, and the webhook whose refusal must be
/// preserved verbatim in the diagnostic.
const DENY_FIXTURE: &str = "webhook-deny.yaml";
const DENY_HOST: &str = "denied.ingress.admissionlab.test";
const DENY_NAMESPACE: &str = "admissionlab-ingress-nginx-deny";
const DENIED_INGRESS: &str = "echo-ingress-denied";
const WEBHOOK: &str = "validate.nginx.ingress.kubernetes.io";
const DENY_PATH: &str = "/etc/nginx";

/// Bounds the chart install plus both of the recipe's readiness gates.
/// Sized from the measurement in the recipe's README (install returned
/// after 22.5 s; the controller reported `Available` 7.6 s later), with
/// room for a cold image pull on a loaded runner.
const COMPONENT_TIMEOUT: Duration = Duration::from_secs(300);

/// The serving deadline handed to the runner: how long the legacy stack
/// has to actually answer the case's probes. This is the bound that
/// stands in for a status wait -- see `ingress.rs`'s "THE FINDING".
/// Generous because it also covers the echo backend's own scheduling,
/// which this test deliberately does not gate on separately: letting the
/// runner's retry loop absorb it is what exercises that loop.
const SERVING_TIMEOUT: Duration = Duration::from_secs(180);

/// Bounds one `bash scripts/build-test-images.sh <cluster>` run, matching
/// the other integration tests in this workspace: a cold `docker build`
/// of two Rust binaries dominates it.
const BUILD_AND_LOAD_TIMEOUT: Duration = Duration::from_mins(15);

// ---------------------------------------------------------------------
// Cleanup guards.
// ---------------------------------------------------------------------

/// Removes a directory tree on drop.
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
// The real, end-to-end test.
// ---------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Docker, kind and helm"]
async fn the_legacy_ingress_runner_records_traffic_and_preserves_a_webhook_denial() {
    run_scenario().await.expect("legacy Ingress runner");
}

/// Creates one cluster at the recipe's primary certified Kubernetes
/// version, installs the recipe, and runs both migration cases through
/// the runner. The cluster is deleted on every path.
///
/// One cluster for both cases rather than one each: the two fixtures use
/// separate namespaces on purpose (`webhook-deny.yaml`'s own header says
/// why), an `Ingress` controller is one shared data plane, and a second
/// `kind` cluster would double a multi-minute test to prove nothing new.
async fn run_scenario() -> Result<(), String> {
    let recipe = legacy_recipe()?;
    let strategy = recipe
        .gateway_endpoint
        .clone()
        .ok_or_else(|| format!("the {RECIPE_NAME:?} recipe declares no gatewayEndpoint"))?;
    let component = component(recipe);
    let kubernetes_version = primary_certified_version()?;

    let root = unique_root("run");
    let _scratch_root_guard = ScratchRoot(root.clone());
    let store = ArtifactStore::new(&root);

    let manager = KindClusterManager::new(Arc::new(TokioProcessRunner::new()));
    let run_id = RunId::generate();
    let paths = store
        .create_run(&run_id)
        .await
        .map_err(|error| format!("failed to prepare a run workspace: {error}"))?;
    let spec = cluster_spec(&run_id, &kubernetes_version)?;
    let handle = manager
        .create(&spec, &paths)
        .await
        .map_err(|error| format!("failed to create cluster: {error}"))?;
    let guard = ClusterGuard::new(handle);

    let outcome = install_and_run(guard.handle(), &paths, &component, &strategy).await;

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

/// Loads the echo image, installs the recipe, then runs both cases.
async fn install_and_run(
    cluster: &ClusterHandle,
    paths: &admissionlab_core::RunPaths,
    component: &ResolvedComponent,
    strategy: &GatewayEndpointStrategy,
) -> Result<(), String> {
    build_and_load_test_images(&TokioProcessRunner::new(), &cluster.spec.name).await?;

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
        std::slice::from_ref(component),
        &dispatcher,
        &readiness,
        COMPONENT_TIMEOUT,
    )
    .await
    .map_err(|error| format!("install_stack failed: {error}"))?;

    check_routing_case(cluster, strategy).await?;
    check_deny_case(cluster, strategy).await
}

/// The routing case: admitted, ready, and answered by `echo-a`.
async fn check_routing_case(
    cluster: &ClusterHandle,
    strategy: &GatewayEndpointStrategy,
) -> Result<(), String> {
    let case = routing_case();
    let spawner = TokioProcessRunner::new();
    let result = run_ingress_case(
        cluster,
        &spawner,
        &case,
        strategy,
        Instant::now() + SERVING_TIMEOUT,
    )
    .await
    .map_err(|error| format!("run_ingress_case failed for the routing case: {error}"))?;

    println!(
        "[routing] admitted={} ready={} probes={} diagnostics={:?}",
        result.admitted,
        result.ready,
        result.probes.len(),
        result
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect::<Vec<_>>()
    );

    if !result.admitted {
        return Err(format!(
            "the routing fixture must be ADMITTED by the pinned release's webhook; got {result:?}"
        ));
    }
    if !result.ready {
        return Err(format!(
            "the legacy stack never served the routing case within {SERVING_TIMEOUT:?}; the \
             runner's diagnostics say: {:?}",
            result
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.clone())
                .collect::<Vec<_>>()
        ));
    }
    if !result.diagnostics.is_empty() {
        return Err(format!(
            "an admitted, ready case must carry no diagnostics; got {:?}",
            result.diagnostics
        ));
    }

    // One result per contract probe, in contract order -- the invariant
    // Task 8.5's index pairing rests on.
    let [probe] = result.probes.as_slice() else {
        return Err(format!(
            "expected exactly one probe result, one per contract probe; got {}",
            result.probes.len()
        ));
    };
    println!(
        "[routing] probe -> status {} backend {:?} in {:?} ({} connection attempt(s))",
        probe.status, probe.backend, probe.elapsed, probe.attempts
    );
    if probe.status != 200 {
        return Err(format!(
            "expected HTTP 200 through the Ingress, got {} (headers: {:?})",
            probe.status, probe.response_headers
        ));
    }
    if probe.backend.as_deref() != Some(BACKEND) {
        return Err(format!(
            "expected the request to reach backend {BACKEND:?}, but the echo response identified \
             {:?} -- the Ingress resolved to the wrong workload",
            probe.backend
        ));
    }
    Ok(())
}

/// The deny case: not admitted, and the webhook's own words preserved.
async fn check_deny_case(
    cluster: &ClusterHandle,
    strategy: &GatewayEndpointStrategy,
) -> Result<(), String> {
    let case = deny_case();
    let spawner = TokioProcessRunner::new();
    let result = run_ingress_case(
        cluster,
        &spawner,
        &case,
        strategy,
        Instant::now() + SERVING_TIMEOUT,
    )
    .await
    .map_err(|error| {
        format!(
            "a validating webhook's refusal must be DATA, not an error: run_ingress_case returned \
             {error}"
        )
    })?;

    if result.admitted {
        return Err(format!(
            "the deny fixture must be REJECTED by the pinned release's validating webhook; a run \
             in which it is admitted is a failure, not a pass. Got {result:?}"
        ));
    }
    if result.ready || !result.probes.is_empty() {
        return Err(format!(
            "a case that was never admitted has no traffic to report; got ready={} and {} \
             probe(s)",
            result.ready,
            result.probes.len()
        ));
    }

    let [diagnostic] = result.diagnostics.as_slice() else {
        return Err(format!(
            "a denial must be preserved as exactly one diagnostic; got {:?}",
            result.diagnostics
        ));
    };
    println!(
        "[deny] admission evidence, verbatim:\n{}",
        diagnostic.message
    );

    if diagnostic.code != DIAGNOSTIC_INGRESS_DENIED {
        return Err(format!(
            "expected the diagnostic code {DIAGNOSTIC_INGRESS_DENIED:?}, got {:?}",
            diagnostic.code
        ));
    }
    // The same four independent claims about one message the Task 8.2
    // certification makes, each of which a different accident could
    // break: that it was an admission denial at all, that THIS webhook
    // made it, which object it was about, and what in that object was
    // refused. Asserted against the diagnostic rather than against
    // `kubectl`'s stderr, which is the whole point of this test.
    let rendered = format!("{diagnostic:?}");
    for needle in [
        "denied the request",
        WEBHOOK,
        &format!("{DENY_NAMESPACE}/{DENIED_INGRESS}"),
        DENY_PATH,
    ] {
        if !rendered.contains(needle) {
            return Err(format!(
                "the refusal was preserved, but it does not mention {needle:?} -- so it is not \
                 attributable to this recipe's webhook denying this input. The diagnostic was:\n\
                 {rendered}"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// The two migration cases this test drives.
// ---------------------------------------------------------------------

/// The routing case: `basic-routing.yaml`, probed the way
/// `admissionlab-recipes`' certification probes it.
fn routing_case() -> MigrationCaseSpec {
    MigrationCaseSpec {
        id: "basic-routing".to_owned(),
        baseline_ingress_manifests: vec![fixtures_dir().join(BASIC_FIXTURE)],
        // Required by the type and never read by this runner, which
        // observes the baseline side only. A real lab pairs this with
        // the `HTTPRoute` written to replace the `Ingress`; Task 8.8
        // wires both sides together.
        candidate_gateway_manifests: vec![
            repo_root().join("fixtures/gateway/istio/same-namespace.yaml"),
        ],
        probes: vec![HttpProbeContract {
            host: BASIC_HOST.to_owned(),
            // A non-root path under the fixture's `/` prefix match, so
            // the request has to actually be matched and forwarded
            // rather than merely arriving.
            path: "/ingress-probe".to_owned(),
            method: "GET".to_owned(),
            headers: BTreeMap::new(),
            expected_status: 200,
            expected_backend: Some(BACKEND.to_owned()),
        }],
        expected_nonportable: Vec::new(),
    }
}

/// The deny case: `webhook-deny.yaml`, whose probe is never sent because
/// the object never exists.
fn deny_case() -> MigrationCaseSpec {
    MigrationCaseSpec {
        id: "webhook-deny".to_owned(),
        baseline_ingress_manifests: vec![fixtures_dir().join(DENY_FIXTURE)],
        candidate_gateway_manifests: vec![
            repo_root().join("fixtures/gateway/istio/same-namespace.yaml"),
        ],
        // `MigrationCaseSpec::probes` must be non-empty (a case with no
        // probes asserts nothing), so this declares the request the
        // denied `Ingress` would have served. It is never sent: the
        // runner returns as soon as the API server refuses.
        probes: vec![HttpProbeContract {
            host: DENY_HOST.to_owned(),
            path: "/never-sent".to_owned(),
            method: "GET".to_owned(),
            headers: BTreeMap::new(),
            expected_status: 200,
            expected_backend: Some(BACKEND.to_owned()),
        }],
        expected_nonportable: Vec::new(),
    }
}

// ---------------------------------------------------------------------
// Cluster and recipe plumbing.
// ---------------------------------------------------------------------

fn legacy_recipe() -> Result<Recipe, String> {
    let dir = repo_root().join(RECIPE_DIR);
    let recipes = load_recipe_overrides(&dir)
        .map_err(|error| format!("failed to load {RECIPE_DIR}: {error}"))?;
    let [recipe] = <[Recipe; 1]>::try_from(recipes).map_err(|recipes| {
        format!(
            "expected exactly one recipe document in {RECIPE_DIR}, got {}",
            recipes.len()
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

/// The first Kubernetes version `compatibility/recipes.yaml` certifies
/// this recipe against.
///
/// One version, not all of them: certifying the *recipe* across every
/// certified version is `admissionlab-recipes`' own test's job, and
/// repeating that multi-minute matrix here to re-prove one runner's
/// behavior would buy nothing. Read from the compatibility file rather
/// than written out, so this test cannot outlive the version it names.
fn primary_certified_version() -> Result<String, String> {
    let compat = load_recipe_compatibility()
        .map_err(|error| format!("failed to load compatibility/recipes.yaml: {error}"))?;
    let entry = compat
        .entry(RECIPE_NAME)
        .ok_or_else(|| format!("compatibility/recipes.yaml has no {RECIPE_NAME:?} entry"))?;
    entry
        .kubernetes
        .certified
        .first()
        .map(|certified| certified.version.clone())
        .ok_or_else(|| {
            format!(
                "compatibility/recipes.yaml's {RECIPE_NAME:?} entry certifies no Kubernetes \
                 version -- this test does not silently skip"
            )
        })
}

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
        sensitive_env_keys: std::collections::BTreeSet::new(),
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

fn fixtures_dir() -> PathBuf {
    repo_root().join("fixtures/migration/ingress-nginx")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("this crate's own CARGO_MANIFEST_DIR/../.. must exist")
}

fn unique_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "admissionlab-ingress-e2e-{label}-{}",
        RunId::generate().as_str()
    ))
}

// ---------------------------------------------------------------------
// The fast half: the two rules the cluster half can only observe
// indirectly.
// ---------------------------------------------------------------------

#[cfg(test)]
mod rules {
    use admissionlab_admission::ObjectKey;
    use admissionlab_core::RedactedValue;
    use admissionlab_gateway::{
        AppliedGatewayFixture, GatewayError, GatewayIdentity, HttpProbeResult, admission_denial,
        applied_ingress_identity, probe_matches_contract,
    };

    use super::{BACKEND, BASIC_HOST, DENIED_INGRESS, DENY_NAMESPACE, WEBHOOK};
    use std::collections::BTreeMap;
    use std::time::Duration;

    /// The exact refusal `webhook-deny.yaml`'s own header records from a
    /// live cluster running the pinned release. Restated here as the
    /// input to the classification rule, so the fast half tests the rule
    /// against the message the slow half will really see.
    const REAL_DENIAL: &str = "admission webhook \"validate.nginx.ingress.kubernetes.io\" denied \
                               the request: invalid object: invalid rule in ingress \
                               admissionlab-ingress-nginx-deny/echo-ingress-denied: invalid http \
                               path: invalid value found: /etc/nginx";

    fn rejected() -> GatewayError {
        GatewayError::ApplyRejected {
            cluster: "adlab-baseline-test".to_owned(),
            object: format!("ingresses {DENY_NAMESPACE}/{DENIED_INGRESS}"),
            code: Some(400),
            reason: Some("BadRequest".to_owned()),
            message: REAL_DENIAL.to_owned(),
        }
    }

    /// The denial rule, in both directions: the API server's own
    /// decision becomes evidence, and everything else stays a failure.
    #[test]
    fn only_a_decision_from_the_api_server_is_admission_evidence() {
        let diagnostic = admission_denial("webhook-deny", &rejected())
            .expect("a refused apply is admission evidence");
        assert_eq!(diagnostic.code, "ingress.admission_denied");
        assert!(
            diagnostic.message.contains(WEBHOOK),
            "the webhook's own words must survive into the message: {}",
            diagnostic.message
        );
        assert!(diagnostic.message.contains("webhook-deny"));
        for (key, expected) in [
            ("case", "webhook-deny"),
            ("code", "400"),
            ("reason", "BadRequest"),
            ("message", REAL_DENIAL),
        ] {
            assert_eq!(
                diagnostic.context.get(key),
                Some(&RedactedValue::Public(expected.to_owned())),
                "context key {key:?}"
            );
        }

        // Everything that is not a decision. A transport failure is the
        // case that matters most: reporting it as `admitted: false`
        // would claim a webhook rejected an object nobody ever asked
        // about.
        for not_a_decision in [
            GatewayError::ApplyUnavailable {
                cluster: "adlab-baseline-test".to_owned(),
                object: "ingresses demo/echo".to_owned(),
                reason: "connection refused".to_owned(),
            },
            GatewayError::IngressCaseWithoutIngress {
                case: "webhook-deny".to_owned(),
            },
        ] {
            assert!(
                admission_denial("webhook-deny", &not_a_decision).is_none(),
                "{not_a_decision} must stay an error"
            );
        }
    }

    /// An absent `code`/`reason` is an absent context key, never a
    /// stand-in value -- see `admission_denial`'s own documentation and
    /// Global Constraint 15.
    #[test]
    fn an_unreported_code_or_reason_is_omitted_rather_than_invented() {
        let diagnostic = admission_denial(
            "webhook-deny",
            &GatewayError::ApplyRejected {
                cluster: "adlab-baseline-test".to_owned(),
                object: "ingresses demo/echo".to_owned(),
                code: None,
                reason: None,
                message: "denied the request".to_owned(),
            },
        )
        .expect("still a decision");
        assert!(!diagnostic.context.contains_key("code"));
        assert!(!diagnostic.context.contains_key("reason"));
    }

    fn result(status: u16, backend: Option<&str>) -> HttpProbeResult {
        HttpProbeResult {
            status,
            backend: backend.map(ToOwned::to_owned),
            response_headers: BTreeMap::new(),
            response_body_sha256: String::new(),
            elapsed: Duration::from_millis(1),
            attempts: 1,
        }
    }

    fn contract(expected_status: u16, expected_backend: Option<&str>) -> super::HttpProbeContract {
        super::HttpProbeContract {
            host: BASIC_HOST.to_owned(),
            path: "/ingress-probe".to_owned(),
            method: "GET".to_owned(),
            headers: BTreeMap::new(),
            expected_status,
            expected_backend: expected_backend.map(ToOwned::to_owned),
        }
    }

    /// The readiness rule this runner's whole deadline is spent on:
    /// status always, backend only when the contract constrains it, and
    /// an unidentifiable backend never satisfies a contract that names
    /// one.
    #[test]
    fn a_probe_matches_its_contract_on_status_and_on_a_named_backend_only() {
        for (probe, contract, expected, why) in [
            (
                result(200, Some(BACKEND)),
                contract(200, Some(BACKEND)),
                true,
                "the contracted status from the contracted backend",
            ),
            (
                result(404, Some(BACKEND)),
                contract(200, Some(BACKEND)),
                false,
                "nginx has not reloaded yet: a good connection answering 404",
            ),
            (
                result(200, Some("echo-b")),
                contract(200, Some(BACKEND)),
                false,
                "the right status from the wrong workload",
            ),
            (
                result(200, None),
                contract(200, Some(BACKEND)),
                false,
                "an unidentifiable answer does not satisfy a named backend",
            ),
            (
                result(404, None),
                contract(404, None),
                true,
                "a contract that constrains only the status is satisfied by it",
            ),
        ] {
            assert_eq!(probe_matches_contract(&probe, &contract), expected, "{why}");
        }
    }

    /// The identity handed to the endpoint resolver comes from the
    /// applied `Ingress` itself, and only from a `networking.k8s.io`
    /// one.
    #[test]
    fn the_endpoint_identity_names_the_applied_ingress() {
        let key = |group: &str, resource: &str, namespace: &str, name: &str| ObjectKey {
            group: group.to_owned(),
            version: "v1".to_owned(),
            resource: resource.to_owned(),
            namespace: Some(namespace.to_owned()),
            name: name.to_owned(),
        };
        let applied = AppliedGatewayFixture {
            objects: vec![
                key("", "namespaces", "demo", "demo"),
                key("", "services", "demo", "echo-a"),
                key("networking.k8s.io", "ingresses", "demo", "echo-ingress"),
            ],
            source_hashes: BTreeMap::new(),
        };
        assert_eq!(
            applied_ingress_identity(&applied),
            Some(GatewayIdentity {
                namespace: "demo".to_owned(),
                name: "echo-ingress".to_owned(),
            })
        );

        // No Ingress at all -- which is what makes
        // `GatewayError::IngressCaseWithoutIngress` reachable rather
        // than defensive.
        let without = AppliedGatewayFixture {
            objects: vec![key("", "namespaces", "demo", "demo")],
            source_hashes: BTreeMap::new(),
        };
        assert_eq!(applied_ingress_identity(&without), None);

        // A different group's `ingresses` is not this one. Nothing in
        // the workspace serves such a resource today; the rule is
        // asserted so that "matched on the plural alone" cannot be
        // introduced by accident.
        let foreign = AppliedGatewayFixture {
            objects: vec![key("example.test", "ingresses", "demo", "echo-ingress")],
            source_hashes: BTreeMap::new(),
        };
        assert_eq!(applied_ingress_identity(&foreign), None);
    }
}
