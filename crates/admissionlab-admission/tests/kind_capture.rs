//! The real, non-mocked capture-pipeline test for Task 3.10:
//! `fixtures/core/admission/`'s whole corpus, replayed through a real
//! `kind` cluster running `recipes/test-webhook`, captured through
//! `admissionlab_admission::capture`, and asserted against the evidence
//! bundles it wrote to disk.
//!
//! `#[ignore]`d — it needs Docker and `kind` — in the same established
//! style `admissionlab-cluster/tests/kind_smoke.rs` (Task 1.9) and
//! `admissionlab-test-webhook/tests/kind_smoke.rs` (Task 2.7) use, so
//! `cargo test --workspace` never requires either. `.github/workflows/integration.yml`
//! runs it as its own matrix entry.
//!
//! # One cluster, not two
//!
//! Phase 3's exit gate asks whether captures are *correct*, not whether
//! two stacks differ — comparison is Phase 4's subject and has no code
//! yet to exercise. A second cluster would therefore double the slowest
//! part of this test (cluster creation, image build, stack install) to
//! re-observe the same fixture corpus against the same webhook build. So
//! this runs one cluster, and takes the one thing a second cluster would
//! genuinely have proven — that the two sides' captures land in disjoint
//! `raw/<side>/` trees — from
//! `admissionlab_core::LabRunner::capture_fixtures`'s own unit coverage
//! instead.
//!
//! # What is asserted from the artifacts rather than from memory
//!
//! Every per-fixture assertion below reads the JSON this run actually
//! wrote under `raw/<side>/<fixture-id>/`, not the in-memory values the
//! pipeline returned. That is deliberate: those files are Phase 4's real
//! input and the only thing a human ever inspects after a run, so a bug
//! that produced a correct `AdmissionOutcome` and then wrote something
//! else would pass a test written against memory. The one exception is
//! `verify_frozen_signature`, which exists to pin the roadmap's frozen
//! `capture_fixture` entry point and its typed return value.
//!
//! # Keeping the artifacts for a manual look
//!
//! The scratch run directory is deleted at the end, like every other
//! real-cluster test here. Setting `ADMISSIONLAB_KEEP_RUN_ARTIFACTS` to
//! any value keeps it and prints its path — how the Phase 3 exit gate's
//! "inspect raw artifacts manually once" step was performed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use admissionlab_admission::capture::{
    AUDIT_ARTIFACT, KubeFixtureCapture, METRICS_AFTER_ARTIFACT, METRICS_BEFORE_ARTIFACT,
    OUTCOME_ARTIFACT, REQUEST_ARTIFACT, RESPONSE_ARTIFACT, capture_fixture,
};
use admissionlab_admission::{
    AdmissionDecision, FileAuditLogReader, KubeAdmissionExecutor, KubeMetricsSource, TraceEvidence,
};
use admissionlab_cluster::{KindClusterManager, cluster_name, load_matrix, resolve_node_image};
use admissionlab_core::{
    ArtifactStore, CapturedFixture, ClusterError, ClusterHandle, ClusterManager, ClusterSpec,
    CommandSpec, FixtureCapture, ProcessRunner, RunId, RunPaths, Side, TokioProcessRunner,
};
use admissionlab_fixtures::{FixtureSource, KubeResourceResolver, discover_fixtures};
use admissionlab_installer::{KubeReadinessProbe, ManifestsInstaller, install_stack};
use admissionlab_recipes::load_recipe_overrides;
use admissionlab_spec::{ResolvedComponent, ResolvedFixtureSelection};
use globset::Glob;
use k8s_openapi::api::core::v1::{ConfigMap, Container, Namespace, Pod, PodSpec, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::Value;

/// The Kubernetes version this test provisions its cluster at — Tier 1's
/// primary supported version, the same `compatibility/kubernetes.yaml`
/// constant every other real-cluster test in this workspace uses.
const KUBERNETES_VERSION: &str = "1.36.4";

/// Bound for one `bash scripts/build-test-images.sh` run (a cold
/// `docker build` plus `kind load docker-image`). Same value, for the
/// same measured reason, as `admissionlab-test-webhook/tests/kind_smoke.rs`.
const BUILD_AND_LOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// Bounds the test-webhook component's install-plus-readiness. Same
/// value and reasoning as `admissionlab-test-webhook/tests/kind_smoke.rs`.
const COMPONENT_TIMEOUT: Duration = Duration::from_secs(120);

/// The namespace `fixtures/core/admission/00-namespace.yaml` declares,
/// and therefore the namespace every pod fixture is created into.
const FIXTURE_NAMESPACE: &str = "admissionlab-fixtures";

/// The include pattern a lab configuration selecting this corpus writes.
/// See `fixtures/core/admission/00-namespace.yaml`'s own comments for
/// why the setup document is deliberately outside it.
const FIXTURE_INCLUDE: &str = "pod-*.yaml";

/// The mutating webhook that adds labels — the one with no
/// `objectSelector`, so it is invoked for every fixture here.
const LABELS_WEBHOOK: &str = "mutate-labels.test-webhook.admissionlab.dev";

/// The mutating webhook that edits containers/init containers/volumes —
/// gated behind the `test.admissionlab.io/containers` label.
const CONTAINERS_WEBHOOK: &str = "mutate-containers.test-webhook.admissionlab.dev";

// ---------------------------------------------------------------------
// Cleanup guard -- mirrors admissionlab-cluster/tests/kind_smoke.rs's
// own `ClusterGuard` exactly (see that file for why `Drop` may only
// warn, never delete).
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
async fn capture_records_real_admission_behavior_for_every_fixture() {
    let outcome = run_capture_test().await;
    outcome.expect("fixture capture pipeline against a real kind cluster");
}

async fn run_capture_test() -> Result<(), String> {
    let manager = KindClusterManager::new(Arc::new(TokioProcessRunner::new()));
    let runner = TokioProcessRunner::new();

    let root = unique_scratch_dir("kind-capture");
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

    let check_result = install_and_capture(&runner, guard.handle(), &store, &paths).await;

    let mut problems: Vec<String> = check_result.err().into_iter().collect();
    if let Err(error) = guard.cleanup(&manager).await {
        problems.push(format!("failed to delete the cluster: {error}"));
    }
    if std::env::var_os("ADMISSIONLAB_KEEP_RUN_ARTIFACTS").is_some() {
        println!(
            "ADMISSIONLAB_KEEP_RUN_ARTIFACTS is set; leaving this run's artifacts at {}",
            paths.root().display()
        );
    } else {
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

async fn install_and_capture(
    runner: &dyn ProcessRunner,
    cluster: &ClusterHandle,
    store: &ArtifactStore,
    paths: &RunPaths,
) -> Result<(), String> {
    build_and_load_image(runner, &cluster.spec.name).await?;
    install_test_webhook(cluster).await?;

    let client = client_for(cluster).await?;
    apply_fixture_namespace(&client).await?;
    wait_until_admission_is_reachable(&client).await?;

    let fixtures = discover_corpus()?;
    println!(
        "discovered {} fixtures under fixtures/core/admission (include {FIXTURE_INCLUDE:?})",
        fixtures.len()
    );

    // The real core-declared seam: one `FixtureCapture` implementation,
    // one side, fixtures replayed serially inside it.
    let capture = KubeFixtureCapture::new(fixtures.clone(), store.clone())
        .with_metrics(Arc::new(KubeMetricsSource::new()));
    let side_capture = capture
        .capture_side(cluster, Side::Baseline, paths)
        .await
        .map_err(|error| format!("capture_side failed: {error}"))?;

    if side_capture.fixtures.len() != fixtures.len() {
        return Err(format!(
            "capture_side returned {} results for {} fixtures",
            side_capture.fixtures.len(),
            fixtures.len()
        ));
    }

    let bundles = load_bundles(&fixtures, &side_capture.fixtures)?;
    dump_bundles(&bundles);

    let mut problems = Vec::new();
    for (file, bundle) in &bundles {
        if let Err(error) = verify_common(file, bundle) {
            problems.push(error);
        }
        if let Err(error) = verify_fixture(file, bundle) {
            problems.push(error);
        }
    }
    if let Err(error) = verify_no_cross_correlation(&bundles) {
        problems.push(error);
    }
    if let Err(error) = verify_secret_bodies_are_not_audited(cluster, &client).await {
        problems.push(error);
    }
    if let Err(error) = verify_frozen_signature(cluster, &fixtures).await {
        problems.push(error);
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

// ---------------------------------------------------------------------
// Setup helpers.
// ---------------------------------------------------------------------

/// Runs `bash scripts/build-test-images.sh <cluster>` as a real
/// subprocess through this project's own `ProcessRunner` — the same
/// approach (and the same reason) as
/// `admissionlab-test-webhook/tests/kind_smoke.rs`.
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

/// Installs `recipes/test-webhook` from its real checked-in directory
/// through the Task 2.4/2.6 install-and-wait pipeline.
async fn install_test_webhook(cluster: &ClusterHandle) -> Result<(), String> {
    let recipe_dir = repo_root().join("recipes/test-webhook");
    let recipes = load_recipe_overrides(&recipe_dir)
        .map_err(|error| format!("failed to load recipes/test-webhook/recipe.yaml: {error}"))?;
    let recipe = recipes
        .into_iter()
        .next()
        .ok_or_else(|| "recipe.yaml produced no recipes".to_string())?;
    let component = ResolvedComponent {
        name: recipe.name,
        version: recipe.version,
        install: recipe.install,
        readiness: recipe.readiness,
        recipe_normalize_rules: recipe.normalize_rules,
        capabilities: recipe.capabilities,
    };

    // The installer's own scratch workspace (in particular `kubectl`'s
    // isolated `--cache-dir`), kept separate from the run workspace this
    // test asserts against so an installer artifact can never be mistaken
    // for a captured one.
    let installer_root = unique_scratch_dir("installer");
    let installer_store = ArtifactStore::new(&installer_root);
    let installer_paths = installer_store
        .create_run(&RunId::generate())
        .await
        .map_err(|error| format!("failed to prepare the installer's workspace: {error}"))?;
    let manifests_installer =
        ManifestsInstaller::new(Arc::new(TokioProcessRunner::new()), &installer_paths);
    let readiness_probe = KubeReadinessProbe::new();

    install_stack(
        cluster,
        std::slice::from_ref(&component),
        &manifests_installer,
        &readiness_probe,
        COMPONENT_TIMEOUT,
    )
    .await
    .map_err(|error| format!("install_stack failed: {error}"))?;

    let _ = tokio::fs::remove_dir_all(&installer_root).await;
    Ok(())
}

/// Applies `fixtures/core/admission/00-namespace.yaml` — the corpus's own
/// setup document, read from its real checked-in location — as a genuine
/// CREATE, so the pod fixtures have a namespace (carrying the webhook
/// opt-in label) to be admitted into.
async fn apply_fixture_namespace(client: &Client) -> Result<(), String> {
    let path = repo_root().join("fixtures/core/admission/00-namespace.yaml");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let document: Value = serde_norway::from_str(&text)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    let namespace: Namespace = serde_json::from_value(document)
        .map_err(|error| format!("{} is not a Namespace object: {error}", path.display()))?;

    let api: Api<Namespace> = Api::all(client.clone());
    api.create(&PostParams::default(), &namespace)
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to create the fixture namespace: {error}"))
}

/// Waits until the installed webhook stack actually answers an admission
/// request, not merely until its Deployment reports `Available`.
///
/// This is not belt-and-braces. It was added after a real failure: on a
/// run where the component install finished slightly faster than
/// `kube-proxy` programmed the Service's rules, the first two fixtures
/// came back
///
/// ```text
/// Internal error occurred: failed calling webhook
/// "mutate-containers.test-webhook.admissionlab.dev": failed to call
/// webhook: ... dial tcp 10.96.58.253:443: connect: connection refused
/// ```
///
/// and then `context deadline exceeded`, while every later fixture
/// passed. `install_stack`'s readiness gates
/// (`deploymentAvailable` plus `webhookConfigurationPresent`) are
/// satisfied at that point -- the pod really is `Ready` and the
/// configurations really do exist -- but a `ClusterIP` with no
/// programmed backend yet rejects the connection, and `failurePolicy:
/// Fail` turns that into a rejected fixture. Capturing it would be a
/// perfectly honest capture of a broken cluster, which is not what this
/// test is for.
///
/// The probe is itself a dry-run CREATE, so it persists nothing, and it
/// carries the gate label so all three webhook configurations
/// (`...-mutate-containers`, `...-mutate-labels`, and the validating
/// one) have to answer for it to succeed. Only a *call failure* is
/// retried; the deadline is bounded (Global Constraint 13) and expiring
/// fails the test with the last error rather than proceeding into a run
/// whose results would be about the network.
async fn wait_until_admission_is_reachable(client: &Client) -> Result<(), String> {
    let probe = Pod {
        metadata: ObjectMeta {
            name: Some("admission-readiness-probe".to_string()),
            namespace: Some(FIXTURE_NAMESPACE.to_string()),
            labels: Some(BTreeMap::from([(
                "test.admissionlab.io/containers".to_string(),
                "enabled".to_string(),
            )])),
            ..ObjectMeta::default()
        },
        spec: Some(PodSpec {
            containers: vec![Container {
                name: "app".to_string(),
                image: Some("registry.k8s.io/pause:3.10".to_string()),
                ..Container::default()
            }],
            ..PodSpec::default()
        }),
        ..Pod::default()
    };
    let pods: Api<Pod> = Api::namespaced(client.clone(), FIXTURE_NAMESPACE);
    let params = PostParams {
        dry_run: true,
        field_manager: Some("admissionlab-readiness-probe".to_string()),
    };

    let started = std::time::Instant::now();
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        match pods.create(&params, &probe).await {
            Ok(_) => {
                println!(
                    "admission is reachable after {attempts} probe(s) in {:?}",
                    started.elapsed()
                );
                return Ok(());
            }
            Err(error) => {
                if started.elapsed() >= ADMISSION_READY_TIMEOUT {
                    return Err(format!(
                        "the webhook stack never answered an admission request within \
                         {ADMISSION_READY_TIMEOUT:?} ({attempts} probes); last error: {error}"
                    ));
                }
                tokio::time::sleep(ADMISSION_READY_POLL_INTERVAL).await;
            }
        }
    }
}

/// How long [`wait_until_admission_is_reachable`] waits for the stack to
/// start answering. Generous next to the sub-second gap actually
/// observed, because the cost of expiring early is a run whose captures
/// describe the network rather than the admission stack.
const ADMISSION_READY_TIMEOUT: Duration = Duration::from_secs(120);

/// Interval between admission readiness probes. Each probe is a real
/// request against the API server, so this is not a busy loop.
const ADMISSION_READY_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Discovers the corpus exactly as a lab configuration would: the real
/// directory, the documented include pattern, and
/// `admissionlab_fixtures::discover_fixtures` itself.
fn discover_corpus() -> Result<Vec<FixtureSource>, String> {
    let selection = ResolvedFixtureSelection {
        include: vec![
            Glob::new(FIXTURE_INCLUDE)
                .map_err(|error| format!("invalid include pattern: {error}"))?,
        ],
        root: repo_root().join("fixtures/core/admission"),
    };
    discover_fixtures(&selection).map_err(|error| format!("fixture discovery failed: {error}"))
}

async fn client_for(cluster: &ClusterHandle) -> Result<Client, String> {
    let kubeconfig = Kubeconfig::read_from(&cluster.kubeconfig)
        .map_err(|error| format!("failed to read kubeconfig: {error}"))?;
    let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
        .await
        .map_err(|error| format!("failed to build kube config: {error}"))?;
    Client::try_from(config).map_err(|error| format!("failed to build kube client: {error}"))
}

// ---------------------------------------------------------------------
// Reading the bundles back off disk.
// ---------------------------------------------------------------------

/// One fixture's written evidence bundle, read back as JSON.
struct Bundle {
    /// The pod name this fixture's document declares.
    pod: String,
    /// The bundle's own directory.
    directory: PathBuf,
    /// `request.json`.
    request: Value,
    /// `response.json`.
    response: Value,
    /// `audit.json`.
    audit: Value,
    /// `outcome.json`.
    outcome: Value,
}

/// Reads every captured fixture's bundle back off disk, keyed by the
/// fixture file's own name (`pod-allow.yaml` and so on) so the
/// assertions below name fixtures the way a reader of the corpus does
/// rather than by a derived id.
fn load_bundles(
    fixtures: &[FixtureSource],
    captured: &[CapturedFixture],
) -> Result<BTreeMap<String, Bundle>, String> {
    let mut bundles = BTreeMap::new();
    for fixture in fixtures {
        let file = fixture
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("fixture path {} has no file name", fixture.path.display()))?
            .to_string();
        let entry = captured
            .iter()
            .find(|entry| entry.fixture_id == fixture.id)
            .ok_or_else(|| format!("no capture result for fixture {file}"))?;
        let pod = fixture
            .object
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("fixture {file} has no metadata.name"))?
            .to_string();
        bundles.insert(
            file,
            Bundle {
                pod,
                directory: entry.artifact_dir.clone(),
                request: read_json(&entry.artifact_dir.join(REQUEST_ARTIFACT))?,
                response: read_json(&entry.artifact_dir.join(RESPONSE_ARTIFACT))?,
                audit: read_json(&entry.artifact_dir.join(AUDIT_ARTIFACT))?,
                outcome: read_json(&entry.outcome_path)?,
            },
        );
    }
    Ok(bundles)
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))
}

/// Prints every bundle's decision, trace and diagnostics, plus one
/// bundle's full contents — the Phase 3 Exit Gate's "inspect raw
/// artifacts manually" step, reproducible by anyone running this test
/// with `--nocapture`.
fn dump_bundles(bundles: &BTreeMap<String, Bundle>) {
    println!("\n=== captured bundles ===");
    for (file, bundle) in bundles {
        println!(
            "\n--- {file} ({})\n  decision:   {}\n  evidence:   {}\n  latency_ms: {}\n  \
             invocations: {}\n  diagnostics: {}",
            bundle.directory.display(),
            bundle.outcome["decision"],
            bundle.outcome["trace"]["evidence"],
            bundle.outcome["total_latency"],
            serde_json::to_string(&bundle.outcome["trace"]["invocations"])
                .unwrap_or_else(|_| "<unrenderable>".to_string()),
            serde_json::to_string(&bundle.outcome["diagnostics"])
                .unwrap_or_else(|_| "<unrenderable>".to_string()),
        );
        println!(
            "  audit: selected={} error={}, {} event(s) in window",
            bundle.audit["selected"]["auditID"],
            bundle.audit["correlationError"],
            bundle.audit["events"].as_array().map_or(0, Vec::len),
        );
    }
    if let Some(bundle) = bundles.get("pod-reinvocation.yaml") {
        println!(
            "\n=== full bundle for pod-reinvocation.yaml ===\nrequest.json:\n{}\n\
             response.json:\n{}\naudit.json (selected event only):\n{}\noutcome.json:\n{}",
            to_pretty(&bundle.request),
            to_pretty(&bundle.response),
            to_pretty(&bundle.audit["selected"]),
            to_pretty(&bundle.outcome),
        );
    }
}

fn to_pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "<unrenderable>".to_string())
}

// ---------------------------------------------------------------------
// Assertions.
// ---------------------------------------------------------------------

/// Properties every captured fixture must have, whatever it exercises.
fn verify_common(file: &str, bundle: &Bundle) -> Result<(), String> {
    let mut problems = Vec::new();

    // The bundle is complete. `metrics-*.prom` are included because this
    // test enables metric collection; without it they would (correctly)
    // be absent.
    for artifact in [
        REQUEST_ARTIFACT,
        RESPONSE_ARTIFACT,
        AUDIT_ARTIFACT,
        OUTCOME_ARTIFACT,
        METRICS_BEFORE_ARTIFACT,
        METRICS_AFTER_ARTIFACT,
    ] {
        let path = bundle.directory.join(artifact);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.len() > 0 => {}
            Ok(_) => problems.push(format!("{file}: {artifact} is empty")),
            Err(error) => problems.push(format!("{file}: {artifact} is missing: {error}")),
        }
    }

    // `request.json` is the submitted object, untouched.
    if bundle
        .request
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        != Some(bundle.pod.as_str())
    {
        problems.push(format!(
            "{file}: request.json does not carry the fixture's own metadata.name"
        ));
    }

    // Correlation succeeded, and found *this* fixture's own event.
    let selected = &bundle.audit["selected"];
    if selected.is_null() {
        problems.push(format!(
            "{file}: no audit event was correlated ({}); the trace would be reported as \
             unavailable rather than observed",
            bundle.audit["correlationError"]
        ));
    } else {
        if selected["objectRef"]["name"].as_str() != Some(bundle.pod.as_str()) {
            problems.push(format!(
                "{file}: the correlated audit event names object {} rather than {}",
                selected["objectRef"]["name"], bundle.pod
            ));
        }
        if selected["objectRef"]["namespace"].as_str() != Some(FIXTURE_NAMESPACE) {
            problems.push(format!(
                "{file}: the correlated audit event names namespace {} rather than \
                 {FIXTURE_NAMESPACE}",
                selected["objectRef"]["namespace"]
            ));
        }
        if selected["stage"].as_str() != Some("ResponseComplete") {
            problems.push(format!(
                "{file}: the correlated audit event is at stage {} rather than ResponseComplete",
                selected["stage"]
            ));
        }
        if !selected["requestURI"]
            .as_str()
            .is_some_and(|uri| uri.contains("dryRun=All"))
        {
            problems.push(format!(
                "{file}: the correlated audit event's requestURI {} is not a dry-run request",
                selected["requestURI"]
            ));
        }
    }

    // Global Constraint 15: a trace that was not observed must never
    // present as an observed empty chain.
    let evidence = bundle.outcome["trace"]["evidence"].as_str().unwrap_or("");
    if evidence == TRACE_UNAVAILABLE && !bundle.audit["selected"].is_null() {
        problems.push(format!(
            "{file}: an event was correlated yet the trace is reported as unavailable"
        ));
    }

    // The exit gate's "no fabricated validating-webhook invocation
    // list": this project reconstructs mutating invocations only, so no
    // invocation may ever name the validating webhook.
    for invocation in invocations(bundle) {
        if invocation["webhook"]
            .as_str()
            .is_some_and(|name| name.starts_with("validate."))
        {
            problems.push(format!(
                "{file}: the trace invented an invocation for validating webhook {}",
                invocation["webhook"]
            ));
        }
        // An attributed per-webhook latency can never exceed the whole
        // request it was measured inside.
        if let Some(latency) = invocation["latency"].as_u64()
            && let Some(total) = bundle.outcome["total_latency"].as_u64()
            && latency > total
        {
            problems.push(format!(
                "{file}: webhook {} reports {latency} ms of latency inside a {total} ms request",
                invocation["webhook"]
            ));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// The wire value of `TraceEvidence::Unavailable`.
const TRACE_UNAVAILABLE: &str = "unavailable";

/// Per-fixture behaviour: what each document in the corpus is *for*.
fn verify_fixture(file: &str, bundle: &Bundle) -> Result<(), String> {
    match file {
        "pod-allow.yaml" => verify_allow(bundle),
        "pod-deny.yaml" => verify_deny(bundle),
        "pod-add-label.yaml" => verify_add_label(bundle),
        "pod-add-container.yaml" => verify_add_container(bundle),
        "pod-add-init-container.yaml" => verify_add_init_container(bundle),
        "pod-remove-init-container.yaml" => verify_remove_init_container(bundle),
        "pod-delay.yaml" => verify_delay(bundle),
        "pod-timeout.yaml" => verify_timeout(bundle),
        "pod-webhook-failure.yaml" => verify_webhook_failure(bundle),
        "pod-reinvocation.yaml" => verify_reinvocation(bundle),
        other => Err(format!(
            "{other}: the corpus grew a fixture this test does not assert anything about"
        )),
    }
    .map_err(|error| format!("{file}: {error}"))
}

fn verify_allow(bundle: &Bundle) -> Result<(), String> {
    require_accepted(bundle)?;
    // The final dry-run response object really was captured, and it is
    // the admitted object rather than the input echoed back.
    if bundle.outcome["final_object"]["metadata"]["name"].as_str() != Some(bundle.pod.as_str()) {
        return Err("outcome.json carries no final response object".to_string());
    }
    if bundle.response["object"]["metadata"]["name"].as_str() != Some(bundle.pod.as_str()) {
        return Err("response.json carries no response object".to_string());
    }
    // Exactly the labels webhook ran, and it changed nothing: the
    // containers webhook's objectSelector does not match this pod.
    let invocation = only_invocation(bundle, LABELS_WEBHOOK)?;
    if invocation["mutated"] != Value::Bool(false) {
        return Err(format!(
            "expected the labels webhook to report mutated: false, got {}",
            invocation["mutated"]
        ));
    }
    if invocations(bundle)
        .iter()
        .any(|entry| entry["webhook"] == CONTAINERS_WEBHOOK)
    {
        return Err(
            "the containers webhook was invoked for a pod carrying no gate label".to_string(),
        );
    }
    Ok(())
}

fn verify_deny(bundle: &Bundle) -> Result<(), String> {
    let (code, message) = require_rejected(bundle)?;
    if code != Some(403) {
        return Err(format!("expected status code 403, got {code:?}"));
    }
    if !message.contains("denied by the admissionlab test webhook") {
        return Err(format!(
            "the rejection message does not carry the fixture's own denial text: {message:?}"
        ));
    }
    if !message.contains("validate.test-webhook.admissionlab.dev") {
        return Err(format!(
            "the rejection message does not name the denying webhook: {message:?}"
        ));
    }
    // A rejection has no persisted form to report, and the input object
    // must never be substituted for one.
    if !bundle.outcome["final_object"].is_null() {
        return Err("a rejected fixture reported a final object".to_string());
    }
    Ok(())
}

fn verify_add_label(bundle: &Bundle) -> Result<(), String> {
    require_accepted(bundle)?;
    let invocation = only_invocation(bundle, LABELS_WEBHOOK)?;
    require_mutated(invocation)?;
    require_patch_op(
        invocation,
        "add",
        "/metadata/labels/admissionlab.dev~1mutated",
    )?;
    if bundle.outcome["final_object"]["metadata"]["labels"]["admissionlab.dev/mutated"].as_str()
        != Some("true")
    {
        return Err("the admitted object does not carry the label the webhook added".to_string());
    }
    Ok(())
}

fn verify_add_container(bundle: &Bundle) -> Result<(), String> {
    require_accepted(bundle)?;
    let invocation = find_invocation(bundle, CONTAINERS_WEBHOOK)?;
    require_mutated(invocation)?;
    require_patch_op(invocation, "add", "/spec/containers/-")?;
    require_container(bundle, "/spec/containers", "sidecar")
}

fn verify_add_init_container(bundle: &Bundle) -> Result<(), String> {
    require_accepted(bundle)?;
    let invocation = find_invocation(bundle, CONTAINERS_WEBHOOK)?;
    require_mutated(invocation)?;
    // The array is absent on this pod, so the patch creates it rather
    // than appending to it.
    require_patch_op(invocation, "add", "/spec/initContainers")?;
    require_container(bundle, "/spec/initContainers", "setup")
}

fn verify_remove_init_container(bundle: &Bundle) -> Result<(), String> {
    require_accepted(bundle)?;
    let invocation = find_invocation(bundle, CONTAINERS_WEBHOOK)?;
    require_mutated(invocation)?;
    require_patch_op(invocation, "remove", "/spec/initContainers/0")?;
    if bundle
        .outcome
        .pointer("/final_object/spec/initContainers")
        .and_then(Value::as_array)
        .is_some_and(|entries| entries.iter().any(|entry| entry["name"] == "legacy-init"))
    {
        return Err("the admitted object still carries the removed init container".to_string());
    }
    Ok(())
}

fn verify_delay(bundle: &Bundle) -> Result<(), String> {
    require_accepted(bundle)?;
    let total = bundle.outcome["total_latency"]
        .as_u64()
        .ok_or_else(|| "outcome.json carries no total_latency".to_string())?;
    if total < 250 {
        return Err(format!(
            "a fixture asking the validating webhook to sleep 250 ms completed in {total} ms"
        ));
    }
    Ok(())
}

/// `pod-timeout.yaml`: the validating webhook is asked to sleep for
/// 15 000 ms against its own `timeoutSeconds: 10`, so the API server
/// gives up waiting and `failurePolicy: Fail` turns that into a
/// rejection.
///
/// The two assertions are chosen so that this stays a real check even if
/// upstream rewords its webhook errors:
///
///  * **The elapsed time.** A request that really waited out a ten-second
///    webhook budget cannot come back in less than ten seconds, whatever
///    the message says. This is the wording-independent proof that a
///    timeout is what happened, and it is what distinguishes this fixture
///    from `pod-webhook-failure.yaml`, whose call failure is immediate.
///  * **The message is not a denial.** A timeout and a deny are
///    categorically different observations — the webhook refused nothing,
///    it never answered — and a capture that reported the first as the
///    second would be exactly the fabrication Global Constraint 15
///    forbids.
///
/// The specific "deadline exceeded" phrasing is checked leniently,
/// against several spellings, because it belongs to kube-apiserver rather
/// than to this project; the timing assertion above is the one that
/// carries the weight.
fn verify_timeout(bundle: &Bundle) -> Result<(), String> {
    let (code, message) = require_rejected(bundle)?;

    let total = bundle.outcome["total_latency"]
        .as_u64()
        .ok_or_else(|| "outcome.json carries no total_latency".to_string())?;
    // `timeoutSeconds: 10` in
    // `recipes/test-webhook/manifests/20-webhook-configuration.yaml`.
    if total < 10_000 {
        return Err(format!(
            "a fixture asking the validating webhook to sleep past its 10s timeout was \
             rejected after only {total} ms, so something other than a timeout rejected it"
        ));
    }

    if message.contains("denied the request") {
        return Err(format!(
            "a webhook timeout was reported as an ordinary denial: {message:?}"
        ));
    }
    if !message.contains("failed calling webhook") {
        return Err(format!(
            "the rejection message does not describe a webhook call failure: {message:?}"
        ));
    }
    let lowercase = message.to_lowercase();
    if ![
        "deadline exceeded",
        "timeout",
        "timed out",
        "context canceled",
    ]
    .iter()
    .any(|phrase| lowercase.contains(phrase))
    {
        return Err(format!(
            "the rejection message does not describe a timeout: {message:?}"
        ));
    }
    if code == Some(403) {
        return Err(
            "a webhook timeout was reported with a denial's own 403 status code".to_string(),
        );
    }
    if !bundle.outcome["final_object"].is_null() {
        return Err("a rejected fixture reported a final object".to_string());
    }
    Ok(())
}

fn verify_webhook_failure(bundle: &Bundle) -> Result<(), String> {
    let (code, message) = require_rejected(bundle)?;
    // Distinguishable from `pod-deny.yaml` by the observed evidence, not
    // by anything this test assumes: a call failure is not a verdict.
    if message.contains("denied the request") {
        return Err(format!(
            "a webhook-call failure was reported as an ordinary denial: {message:?}"
        ));
    }
    if !message.contains("failed calling webhook") {
        return Err(format!(
            "the rejection message does not describe a webhook call failure: {message:?}"
        ));
    }
    if code == Some(403) {
        return Err(
            "a webhook-call failure was reported with a denial's own 403 status code".to_string(),
        );
    }
    if !bundle.outcome["final_object"].is_null() {
        return Err("a rejected fixture reported a final object".to_string());
    }
    Ok(())
}

fn verify_reinvocation(bundle: &Bundle) -> Result<(), String> {
    require_accepted(bundle)?;

    // Round 0: the containers webhook appends the volume, and the labels
    // webhook then changes the object after it — which is what makes the
    // containers webhook eligible for reinvocation at all.
    let first = find_invocation(bundle, CONTAINERS_WEBHOOK)?;
    require_mutated(first)?;
    require_patch_op(first, "add", "/spec/volumes/-")?;
    require_mutated(find_invocation(bundle, LABELS_WEBHOOK)?)?;

    // Round 1: the containers webhook called a second time. Its action is
    // idempotent, so what proves the reinvocation is the invocation
    // record itself, never a second patch.
    let rounds: BTreeSet<u64> = invocations(bundle)
        .iter()
        .filter_map(|invocation| invocation["round"].as_u64())
        .collect();
    if !rounds.iter().any(|round| *round > 0) {
        return Err(format!(
            "no invocation ran in a round after the first; observed rounds {rounds:?}"
        ));
    }
    if !invocations(bundle).iter().any(|invocation| {
        invocation["webhook"] == CONTAINERS_WEBHOOK
            && invocation["round"].as_u64().is_some_and(|round| round > 0)
    }) {
        return Err(format!(
            "the containers webhook was never reinvoked; the trace holds {}",
            serde_json::to_string(&bundle.outcome["trace"]["invocations"])
                .unwrap_or_else(|_| "<unrenderable>".to_string())
        ));
    }

    // The admitted object carries each webhook's work exactly once —
    // reinvocation converged rather than appending a second volume.
    require_container(bundle, "/spec/volumes", "scratch")?;
    let volumes = bundle
        .outcome
        .pointer("/final_object/spec/volumes")
        .and_then(Value::as_array)
        .ok_or_else(|| "the admitted object has no volumes".to_string())?;
    if volumes
        .iter()
        .filter(|volume| volume["name"] == "scratch")
        .count()
        != 1
    {
        return Err("the reinvoked webhook appended its volume more than once".to_string());
    }
    if bundle.outcome["final_object"]["metadata"]["labels"]["admissionlab.dev/reinvoked"].as_str()
        != Some("true")
    {
        return Err(
            "the admitted object does not carry the label the labels webhook added".to_string(),
        );
    }
    Ok(())
}

/// The exit gate's "serial requests never cross-correlate": every
/// fixture's evidence is its own.
fn verify_no_cross_correlation(bundles: &BTreeMap<String, Bundle>) -> Result<(), String> {
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for (file, bundle) in bundles {
        let Some(audit_id) = bundle.audit["selected"]["auditID"].as_str() else {
            return Err(format!("{file}: no correlated audit event to check"));
        };
        if let Some(previous) = seen.insert(audit_id, file) {
            return Err(format!(
                "{file} and {previous} correlated to the same audit event {audit_id}"
            ));
        }
    }
    if seen.len() != bundles.len() {
        return Err("two fixtures shared one audit event".to_string());
    }
    Ok(())
}

/// The exit gate's "secret request bodies are not logged at Request
/// level by the audit policy" — checked against the audit log this run
/// actually produced, with a Secret this check creates itself.
///
/// Three properties, each a different failure this could have:
///
/// 1. **The check is not vacuous.** It creates a real `Secret` carrying
///    a unique canary string in `stringData`, so a request whose body
///    contains that canary demonstrably reached the API server. A policy
///    regression would therefore have something to record.
/// 2. **The exclusion holds.** `admissionlab_cluster::audit`'s rule 1
///    excludes core `secrets` at level `None`, ahead of the
///    `Request`-level rule that would otherwise match them, so the log
///    must contain no event for a `secrets` resource at all — strictly
///    stronger than "no body was recorded" — and the canary must appear
///    nowhere in the log's bytes.
/// 3. **`Request` level really is in force.** At least one non-Secret
///    event must carry a `requestObject`, and none may carry a
///    `responseObject`. Without the first, property 2 could pass on a
///    cluster that was quietly auditing at `Metadata` and recording no
///    bodies at all; the second pins that the policy is `Request` and not
///    `RequestResponse`.
///
/// Ordering is handled by a marker: a `ConfigMap` created *after* the
/// Secret, which the same policy records at `Request` level. Once the
/// marker's own event is in the log, the API server has necessarily
/// already written whatever it was going to write for the Secret, so an
/// absence observed afterwards is a real absence rather than a race.
async fn verify_secret_bodies_are_not_audited(
    cluster: &ClusterHandle,
    client: &Client,
) -> Result<(), String> {
    let canary = format!("admissionlab-secret-canary-{}", RunId::generate());
    let secret = Secret {
        metadata: ObjectMeta {
            name: Some("audit-probe".to_string()),
            namespace: Some(FIXTURE_NAMESPACE.to_string()),
            ..ObjectMeta::default()
        },
        string_data: Some(BTreeMap::from([("canary".to_string(), canary.clone())])),
        ..Secret::default()
    };
    let secrets: Api<Secret> = Api::namespaced(client.clone(), FIXTURE_NAMESPACE);
    secrets
        .create(&PostParams::default(), &secret)
        .await
        .map_err(|error| format!("failed to create the audit probe Secret: {error}"))?;

    let marker = ConfigMap {
        metadata: ObjectMeta {
            name: Some(AUDIT_MARKER_NAME.to_string()),
            namespace: Some(FIXTURE_NAMESPACE.to_string()),
            ..ObjectMeta::default()
        },
        data: Some(BTreeMap::from([(
            "marker".to_string(),
            "audit-ordering-marker".to_string(),
        )])),
        ..ConfigMap::default()
    };
    let config_maps: Api<ConfigMap> = Api::namespaced(client.clone(), FIXTURE_NAMESPACE);
    config_maps
        .create(&PostParams::default(), &marker)
        .await
        .map_err(|error| format!("failed to create the audit ordering marker: {error}"))?;

    let log = wait_for_audit_marker(cluster).await?;

    let mut events = 0usize;
    let mut with_request_object = 0usize;
    for line in log.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        events += 1;
        if event["objectRef"]["resource"] == "secrets" {
            return Err(format!(
                "the audit log records an event for a secrets resource: auditID {}",
                event["auditID"]
            ));
        }
        if !event["responseObject"].is_null() {
            return Err(format!(
                "the audit log records a response body (auditID {}), which a Request-level policy                  never should",
                event["auditID"]
            ));
        }
        if !event["requestObject"].is_null() {
            with_request_object += 1;
        }
    }
    if log.contains(&canary) {
        return Err("the audit log contains the Secret's own canary value".to_string());
    }
    if with_request_object == 0 {
        return Err(
            "no audit event carried a requestObject, so this cluster is not auditing at Request              level and the Secret exclusion above proves nothing"
                .to_string(),
        );
    }

    println!(
        "audit policy check: {events} audit events, {with_request_object} carrying a \
         requestObject, none for a secrets resource, none carrying a responseObject, and no trace \
         of the probe Secret's canary value"
    );
    Ok(())
}

/// The `ConfigMap` whose own audit event orders the Secret check — see
/// [`verify_secret_bodies_are_not_audited`].
const AUDIT_MARKER_NAME: &str = "audit-ordering-marker";

/// Reads the audit log until it holds the marker `ConfigMap`'s own
/// event, and returns it.
///
/// Bounded (Global Constraint 13). A marker that never arrives is a
/// failure, not a silently skipped check: without it the absence
/// assertions would be racing the API server's own writer.
async fn wait_for_audit_marker(cluster: &ClusterHandle) -> Result<String, String> {
    let deadline = std::time::Instant::now() + AUDIT_MARKER_TIMEOUT;
    loop {
        let log = tokio::fs::read_to_string(&cluster.audit_log)
            .await
            .map_err(|error| {
                format!(
                    "failed to read the audit log {}: {error}",
                    cluster.audit_log.display()
                )
            })?;
        if log.contains(AUDIT_MARKER_NAME) {
            return Ok(log);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "the audit ordering marker {AUDIT_MARKER_NAME:?} never appeared in the audit log \
                 within {AUDIT_MARKER_TIMEOUT:?}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// How long [`wait_for_audit_marker`] waits for the marker's own audit
/// event. Generous next to the milliseconds a `kind` API server actually
/// takes, because the cost of expiring early is a false failure.
const AUDIT_MARKER_TIMEOUT: Duration = Duration::from_secs(30);

/// Pins Task 3.10's frozen `capture_fixture` signature and its typed
/// return value — the one assertion here that is made against memory
/// rather than against a written artifact.
async fn verify_frozen_signature(
    cluster: &ClusterHandle,
    fixtures: &[FixtureSource],
) -> Result<(), String> {
    let fixture = fixtures
        .iter()
        .find(|fixture| fixture.path.ends_with("pod-add-label.yaml"))
        .ok_or_else(|| "pod-add-label.yaml was not discovered".to_string())?;

    let resolver = KubeResourceResolver::new();
    let executor = KubeAdmissionExecutor::new();
    let audit = FileAuditLogReader::for_cluster(cluster);
    let outcome = capture_fixture(
        cluster,
        Side::Candidate,
        fixture,
        &resolver,
        &executor,
        &audit,
        None,
    )
    .await
    .map_err(|error| format!("capture_fixture failed: {error}"))?;

    if outcome.decision != AdmissionDecision::Accepted {
        return Err(format!(
            "capture_fixture reported {:?} for the add-label fixture",
            outcome.decision
        ));
    }
    if outcome.side != Side::Candidate {
        return Err("capture_fixture ignored the side it was given".to_string());
    }
    if outcome.trace.evidence == TraceEvidence::Unavailable {
        return Err(
            "capture_fixture could not correlate the fixture's own audit event".to_string(),
        );
    }
    // Metrics were not enabled for this call, so no invocation may claim
    // a latency (Global Constraint 15: never a fabricated zero).
    if outcome
        .trace
        .invocations
        .iter()
        .any(|invocation| invocation.latency.is_some())
    {
        return Err(
            "an invocation reported a latency although metric collection was disabled".to_string(),
        );
    }
    println!(
        "capture_fixture (frozen signature): {:?}, {} invocation(s), evidence {:?}",
        outcome.decision,
        outcome.trace.invocations.len(),
        outcome.trace.evidence
    );
    Ok(())
}

// ---------------------------------------------------------------------
// Small assertion helpers over the written JSON.
// ---------------------------------------------------------------------

fn invocations(bundle: &Bundle) -> &[Value] {
    bundle.outcome["trace"]["invocations"]
        .as_array()
        .map_or(&[], Vec::as_slice)
}

fn require_accepted(bundle: &Bundle) -> Result<(), String> {
    if bundle.outcome["decision"] == "accepted" {
        Ok(())
    } else {
        Err(format!(
            "expected an accepted decision, got {}",
            bundle.outcome["decision"]
        ))
    }
}

/// The observed rejection's `(code, message)`.
fn require_rejected(bundle: &Bundle) -> Result<(Option<u64>, String), String> {
    let rejected = bundle
        .outcome
        .pointer("/decision/rejected")
        .ok_or_else(|| format!("expected a rejection, got {}", bundle.outcome["decision"]))?;
    let message = rejected["message"]
        .as_str()
        .ok_or_else(|| "the rejection carries no message".to_string())?
        .to_string();
    Ok((rejected["code"].as_u64(), message))
}

fn find_invocation<'a>(bundle: &'a Bundle, webhook: &str) -> Result<&'a Value, String> {
    invocations(bundle)
        .iter()
        .find(|invocation| invocation["webhook"] == webhook)
        .ok_or_else(|| {
            format!(
                "no invocation of {webhook} was observed; the trace holds {}",
                serde_json::to_string(&bundle.outcome["trace"]["invocations"])
                    .unwrap_or_else(|_| "<unrenderable>".to_string())
            )
        })
}

fn only_invocation<'a>(bundle: &'a Bundle, webhook: &str) -> Result<&'a Value, String> {
    let found = find_invocation(bundle, webhook)?;
    if invocations(bundle).len() != 1 {
        return Err(format!(
            "expected exactly one invocation, observed {}",
            invocations(bundle).len()
        ));
    }
    Ok(found)
}

fn require_mutated(invocation: &Value) -> Result<(), String> {
    if invocation["mutated"] == Value::Bool(true) {
        Ok(())
    } else {
        Err(format!(
            "webhook {} did not report mutating the object (mutated: {})",
            invocation["webhook"], invocation["mutated"]
        ))
    }
}

/// Asserts the invocation's observed JSON Patch contains an operation
/// with `op` on `path` — the audit-recorded patch, not a reconstruction.
fn require_patch_op(invocation: &Value, op: &str, pointer: &str) -> Result<(), String> {
    let operations = invocation["patch"]
        .as_array()
        .ok_or_else(|| format!("webhook {} recorded no patch", invocation["webhook"]))?;
    if operations
        .iter()
        .any(|operation| operation["op"] == op && operation["path"] == pointer)
    {
        Ok(())
    } else {
        Err(format!(
            "webhook {} recorded no {op} operation on {pointer}; its patch is {}",
            invocation["webhook"],
            serde_json::to_string(&invocation["patch"])
                .unwrap_or_else(|_| "<unrenderable>".to_string())
        ))
    }
}

/// Asserts the admitted object's array at `pointer` contains an entry
/// named `name`.
fn require_container(bundle: &Bundle, pointer: &str, name: &str) -> Result<(), String> {
    let entries = bundle
        .outcome
        .pointer(&format!("/final_object{pointer}"))
        .and_then(Value::as_array)
        .ok_or_else(|| format!("the admitted object has no {pointer} array"))?;
    if entries.iter().any(|entry| entry["name"] == name) {
        Ok(())
    } else {
        Err(format!(
            "the admitted object's {pointer} does not contain {name}"
        ))
    }
}

// ---------------------------------------------------------------------
// Paths.
// ---------------------------------------------------------------------

/// This checkout's own repository root — `CARGO_MANIFEST_DIR/../..`,
/// matching every other `CARGO_MANIFEST_DIR`-anchored path in this
/// workspace's test suites.
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
        "admissionlab-admission-{label}-{}",
        unique.as_str()
    ))
}
