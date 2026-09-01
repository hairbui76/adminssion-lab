//! `admissionlab test` with a Gateway suite, end to end against two real
//! `kind` clusters (ROADMAP Task 6.11).
//!
//! `tests/test_command.rs` proves the pipeline's *decisions* against
//! fakes: which failure maps to which exit code, that a suite runs on
//! both sides, that a skipped probe is recorded. This file proves the
//! one thing fakes structurally cannot — that the whole Gateway engine
//! works when the clusters, the controllers, and the HTTP request are
//! real:
//!
//! - two `kind` clusters created by `admissionlab test` itself;
//! - the certified `recipes/istio-gateway/` stack installed on each, as
//!   two components in order (the vendored Gateway API CRD bundle, then
//!   `istiod`);
//! - `admissionlab-echo:dev` side-loaded into both by the run itself,
//!   through the configuration's own `images:` list;
//! - `fixtures/gateway/istio/same-namespace.yaml` applied to both;
//! - the route's status observed, the data-plane endpoint resolved from
//!   the recipe's own `gatewayEndpoint` strategy, a `kubectl
//!   port-forward` opened, and one real HTTP request sent through Envoy.
//!
//! Both sides are **identical**. That is the assertion: two identical
//! stacks must produce two identical Gateway results, no semantic
//! change, and exit 0. `tests/gateway_demo.rs` (Task 6.12) is the
//! opposite experiment, and it is only meaningful because this one
//! passes.
//!
//! # Why the image is built here and side-loaded by the run
//!
//! The echo backend is built from this repository and never pushed
//! anywhere, and the fixture references it with `imagePullPolicy:
//! IfNotPresent`. Every earlier real-cluster test in this workspace
//! created its own cluster and then ran
//! `scripts/build-test-images.sh <cluster>` against it — which is
//! impossible here, because the cluster names do not exist until
//! `admissionlab test` is already running and creating them.
//!
//! ROADMAP Task 6.11 is where that became a real product gap rather
//! than a test inconvenience, and it is closed as a product feature:
//! [`admissionlab_spec::EnvironmentSpec::images`] lists local images a
//! side's cluster must be given, and `KindClusterManager::create`
//! side-loads them inside the cluster-creation rollback window. This
//! test therefore builds the image *before* the run (the script's
//! no-cluster mode) and lets the run load it, which is exactly the path
//! a user dogfooding a locally built workload takes.
//!
//! # Why the suite declares readiness
//!
//! Two `Deployment`s, and they are two different kinds of wait.
//!
//! `echo-a` is the fixture's **own** backend: applying its manifest
//! returns when the object exists, and a request routed to a backend
//! with no ready pod is answered by Envoy's own 503 — a statement about
//! this run's timing, not about the route. That is exactly what
//! [`admissionlab_spec::GatewaySuiteSpec::readiness`] exists for.
//!
//! `lab-gateway-istio` is the data plane **Istio provisions** from the
//! `Gateway` object, and gating on it is the trade that field's own
//! documentation describes: it exchanges "reported as a behavior
//! difference" for "fails the run". It is taken deliberately here
//! because this test's two sides are byte-identical, so the data plane
//! coming up is not the thing being compared — and without it, the
//! `kubectl port-forward` this run opens can race a Service that has no
//! ready endpoint yet and fail for a reason that has nothing to do with
//! the Gateway engine.
//!
//! # Cleanup
//!
//! `admissionlab test` deletes both clusters itself on every path except
//! `--keep-clusters`, which this test never passes. [`ScratchRoot`]
//! removes the temporary workspace afterwards; it is a `Drop` guard so
//! that a panicking assertion cannot leak the directory (the workspace's
//! own `unique_temp_dir` helpers do leak, deliberately, and are a
//! separate cleanup task's to fix — this file does not copy them).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

/// The Kubernetes version both sides run.
///
/// Admission Lab's own Tier-1 primary, and one of the three versions
/// `compatibility/recipes.yaml` certifies `istio-gateway` against. The
/// exit gate's "Istio Gateway recipe passes on primary Kubernetes"
/// bullet is about this version specifically.
const KUBERNETES_VERSION: &str = "1.36.4";

/// The route contract this lab declares, and therefore the identifier
/// its entry in `result.json` carries.
const CONTRACT_ID: &str = "istio-same-namespace";

/// The namespace `fixtures/gateway/istio/same-namespace.yaml` puts its
/// `Gateway`, `HTTPRoute` and backend in.
const NAMESPACE: &str = "admissionlab-istio-gateway-same";

/// The backend the echo response must identify itself as.
const BACKEND: &str = "echo-a";

/// The `Host` the fixture's route matches on.
const HOST: &str = "same.gateway.admissionlab.test";

/// Bounds the whole `admissionlab test` invocation: two clusters, two
/// two-component stacks (one of them a Helm install of `istiod`), a
/// fixture replay, a Gateway suite on each side, and cleanup. Measured
/// at well under half of this on a warm machine; sized for a cold image
/// cache on a loaded runner, because a timeout here kills a run
/// mid-cleanup and leaks two clusters.
const RUN_TIMEOUT: Duration = Duration::from_mins(40);

/// Bounds `scripts/build-test-images.sh`, whose cost is a cold
/// `docker build` of two Rust binaries. Mirrors the bound
/// `crates/admissionlab-recipes/tests/istio_gateway_recipe.rs` uses for
/// the same script.
const BUILD_TIMEOUT: Duration = Duration::from_mins(15);

/// Removes a temporary directory when dropped.
///
/// Bound before the first fallible step in every test below, so a failed
/// assertion still cleans up. Deliberately not the workspace's existing
/// `unique_temp_dir` helpers, which leak by construction.
struct ScratchRoot(PathBuf);

impl Drop for ScratchRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore = "requires Docker, kind, kubectl and helm; creates two real clusters"]
fn two_identical_istio_gateway_stacks_route_real_traffic_and_report_no_regression() {
    let root = scratch_dir("gateway-e2e");
    let _guard = ScratchRoot(root.clone());
    std::fs::create_dir_all(&root).expect("the scratch directory must be creatable");

    build_echo_image();

    let config = write_lab(&root);
    let reports = root.join("reports");
    let output = run_admissionlab_test(&config, &reports);

    assert!(
        output.status.success(),
        "two identical stacks must produce no regression and exit 0.\n--- stdout ---\n{}\n--- \
         stderr ---\n{}",
        output.stdout,
        output.stderr,
    );

    let result = read_result(&reports.join("result.json"));
    let entry = gateway_entry(&result);

    // 1. The Gateway sections exist at all, on both sides. The frozen
    // v1beta1 document keeps reconciliation and traffic apart (ROADMAP
    // Task 7.2).
    let gateway = &entry["gatewayReconciliation"];
    let traffic = &entry["traffic"];
    assert!(
        gateway.is_object() && traffic.is_object(),
        "the route contract must carry Gateway evidence: {entry}"
    );
    assert!(
        entry["admission"].is_null(),
        "a route contract carries Gateway evidence and no admission evidence: {entry}"
    );

    // 2. Every certified condition is True and current, on both sides.
    for side in ["baseline", "candidate"] {
        let case = &gateway[side];
        assert!(
            case["converged"].as_bool() == Some(true),
            "the {side} route must have converged: {case}"
        );
        assert_condition(&case["gatewayClass"]["accepted"], "Accepted");
        for type_name in ["Accepted", "Programmed"] {
            assert_condition(&case["gateway"]["conditions"][type_name], type_name);
        }
        let parents = case["route"]["parents"]
            .as_array()
            .unwrap_or_else(|| panic!("the {side} route must publish parent status: {case}"));
        assert_eq!(
            parents.len(),
            1,
            "the fixture declares exactly one listener: {case}"
        );
        for type_name in ["Accepted", "ResolvedRefs"] {
            assert_condition(&parents[0]["conditions"][type_name], type_name);
        }
        assert_eq!(
            case["route"]["generation"], parents[0]["conditions"]["Accepted"]["observedGeneration"],
            "a condition observed at an earlier generation describes a spec that is not the one \
             under test: {case}"
        );
    }

    // 3. A real request reached the expected backend through Envoy, on
    // both sides -- which is what makes the probe a *pair*.
    assert_eq!(traffic["evidence"], "observed", "{traffic}");
    let pairs = traffic["pairs"]
        .as_array()
        .unwrap_or_else(|| panic!("the case must carry probe results: {traffic}"));
    assert_eq!(pairs.len(), 1, "the contract declares one probe: {traffic}");
    for side in ["baseline", "candidate"] {
        assert_eq!(pairs[0][side]["status"], 200, "{traffic}");
        assert_eq!(pairs[0][side]["backend"], BACKEND, "{traffic}");
    }

    // 4. Two identical stacks produced no Gateway claim of any kind.
    assert!(
        entry["changes"]
            .as_array()
            .expect("changes must be an array")
            .is_empty(),
        "two identical Istio installs must produce no Gateway semantic change: {entry}"
    );
    assert_eq!(
        result["summary"]["inconclusive"], 0,
        "both sides converged, so the pair is comparable: {}",
        result["summary"]
    );
    assert_eq!(result["policy"]["disposition"], "pass");

    // 5. The raw evidence bundles are on disk, per side and per contract.
    let run_id = result["runId"].as_str().expect("a run id");
    for side in ["baseline", "candidate"] {
        let directory = gateway_evidence_dir(run_id, side);
        for artifact in ["reconciliation.json", "probes.json"] {
            assert!(
                directory.join(artifact).is_file(),
                "{} must exist",
                directory.join(artifact).display()
            );
        }
    }
}

/// The one check in this file that needs no cluster, and therefore runs
/// under plain `cargo test --workspace`.
///
/// The configuration above is assembled by string formatting, which is
/// exactly the kind of thing that rots silently: a mis-indented
/// `readiness:` block or a stray tab would otherwise be discovered
/// twenty minutes into an `#[ignore]`d run, after two clusters had
/// already been created. Loading and resolving it for real — the same
/// two functions `admissionlab test` calls before it provisions
/// anything — catches that in milliseconds, and additionally proves the
/// `gateway:` section's contract ids, probe method, status and endpoint
/// placeholders all satisfy `resolve_lab`'s own validation.
#[test]
fn the_lab_configuration_this_test_writes_is_valid() {
    let root = scratch_dir("gateway-e2e-config");
    let _guard = ScratchRoot(root.clone());
    std::fs::create_dir_all(&root).expect("the scratch directory must be creatable");

    let config = write_lab(&root);
    let loaded = admissionlab_spec::load_lab(&config)
        .unwrap_or_else(|error| panic!("the generated lab must parse: {error}"));
    let resolved = admissionlab_spec::resolve_lab(loaded)
        .unwrap_or_else(|error| panic!("the generated lab must resolve: {error}"));

    assert_eq!(
        resolved.baseline.images,
        vec!["admissionlab-echo:dev".to_owned()],
        "the echo image is side-loaded by the run itself"
    );
    assert_eq!(resolved.baseline.images, resolved.candidate.images);
    assert_eq!(
        resolved.baseline.components.len(),
        2,
        "the CRD bundle, then istiod"
    );
    let suite = resolved.gateway.expect("the lab declares a gateway suite");
    assert_eq!(suite.routes.len(), 1);
    assert_eq!(suite.routes[0].id, CONTRACT_ID);
    assert_eq!(suite.routes[0].probes.len(), 1);
    assert!(
        suite.gateway_endpoint.is_some(),
        "without an endpoint strategy every probe would be skipped, and this test would prove          nothing about traffic"
    );
}

/// Asserts one serialized condition is `True`, and names it in the
/// failure.
///
/// Reads the state rather than the reason: a reason is
/// implementation-authored text this project does not control, and
/// `admissionlab_gateway::diff` is explicit that it is evidence and
/// never a claim.
fn assert_condition(condition: &Value, type_name: &str) {
    assert_eq!(
        condition["state"], "True",
        "condition {type_name} must be True: {condition}"
    );
    assert_eq!(condition["typeName"], type_name, "{condition}");
}

/// Writes the lab configuration this test runs: two identical sides,
/// each installing the certified two-component `istio-gateway` stack,
/// each side-loading the echo image, and one Gateway route contract with
/// one probe.
///
/// Both sides are written from one template because they must be
/// identical: a difference introduced by a copy-paste slip would make
/// this test fail for a reason that has nothing to do with the engine.
fn write_lab(root: &Path) -> PathBuf {
    let repo = repo_root();
    let crd_bundle = repo
        .join("recipes/istio-gateway/gateway-api/standard-install-v1.5.1.yaml")
        .display()
        .to_string();
    let suite_manifest = repo
        .join("fixtures/gateway/istio/same-namespace.yaml")
        .display()
        .to_string();

    // One admission fixture, because `resolve_lab` requires the
    // `fixtures` selection to match something: a lab that replayed
    // nothing has nothing to compare, and that rule is not relaxed by a
    // Gateway suite being present. Deliberately trivial — the admission
    // half of this run is not what is under test.
    let fixtures = root.join("fixtures");
    std::fs::create_dir_all(&fixtures).expect("the fixture directory must be creatable");
    std::fs::write(
        fixtures.join("configmap.yaml"),
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: gateway-e2e-probe\n  \
         namespace: default\ndata:\n  key: value\n",
    )
    .expect("the fixture must be writable");

    let side = format!(
        "  kubernetes: \"{KUBERNETES_VERSION}\"\n\
         \x20 images:\n\
         \x20   - admissionlab-echo:dev\n\
         \x20 components:\n\
         \x20   - name: gateway-api-crds\n\
         \x20     version: \"1.5.1\"\n\
         \x20     install:\n\
         \x20       type: manifests\n\
         \x20       paths:\n\
         \x20         - {crd_bundle}\n\
         \x20     readiness:\n{crd_readiness}\
         \x20   - name: istio-gateway\n\
         \x20     install:\n\
         \x20       type: helm\n\
         \x20       chart: istio/istiod\n\
         \x20       repo: https://istio-release.storage.googleapis.com/charts\n\
         \x20       repoName: istio\n\
         \x20       version: \"1.30.4\"\n\
         \x20       namespace: istio-system\n\
         \x20     readiness:\n\
         \x20       - type: deploymentAvailable\n\
         \x20         namespace: istio-system\n\
         \x20         name: istiod\n\
         \x20       - type: customResourceCondition\n\
         \x20         apiVersion: gateway.networking.k8s.io/v1\n\
         \x20         kind: GatewayClass\n\
         \x20         name: istio\n\
         \x20         conditionType: Accepted\n\
         \x20         status: \"True\"\n",
        crd_readiness = established_checks(),
    );

    let config = root.join("admissionlab.yaml");
    std::fs::write(
        &config,
        format!(
            "apiVersion: admissionlab.io/v1alpha1\n\
             kind: Lab\n\
             baseline:\n{side}\
             candidate:\n{side}\
             fixtures:\n\
             \x20 include:\n\
             \x20   - \"fixtures/*.yaml\"\n\
             gateway:\n\
             \x20 manifests:\n\
             \x20   - {suite_manifest}\n\
             \x20 gatewayEndpoint:\n\
             \x20   type: serviceBySelector\n\
             \x20   namespace: \"{{gatewayNamespace}}\"\n\
             \x20   selector:\n\
             \x20     gateway.networking.k8s.io/gateway-name: \"{{gatewayName}}\"\n\
             \x20   portName: http\n\
             \x20 readiness:\n\
             \x20   - type: deploymentAvailable\n\
             \x20     namespace: {NAMESPACE}\n\
             \x20     name: {BACKEND}\n\
             \x20   - type: deploymentAvailable\n\
             \x20     namespace: {NAMESPACE}\n\
             \x20     name: lab-gateway-istio\n\
             \x20 routes:\n\
             \x20   - id: {CONTRACT_ID}\n\
             \x20     gatewayNamespace: {NAMESPACE}\n\
             \x20     gatewayName: lab-gateway\n\
             \x20     routeNamespace: {NAMESPACE}\n\
             \x20     routeName: echo-route\n\
             \x20     listenerName: http\n\
             \x20     probes:\n\
             \x20       - host: {HOST}\n\
             \x20         path: /gateway-probe\n\
             \x20         method: GET\n\
             \x20         expectedStatus: 200\n\
             \x20         expectedBackend: {BACKEND}\n"
        ),
    )
    .expect("the lab configuration must be writable");
    config
}

/// The four `Established` readiness checks the `gateway-api-crds`
/// component declares, transcribed from
/// `recipes/istio-gateway/gateway-api-crds.yaml`.
fn established_checks() -> String {
    [
        "gatewayclasses",
        "gateways",
        "httproutes",
        "referencegrants",
    ]
    .iter()
    .fold(String::new(), |mut checks, plural| {
        use std::fmt::Write as _;
        let _: Result<(), std::fmt::Error> = write!(
            checks,
            "         - type: customResourceCondition\n\
             \x20          apiVersion: apiextensions.k8s.io/v1\n\
             \x20          kind: CustomResourceDefinition\n\
             \x20          name: {plural}.gateway.networking.k8s.io\n\
             \x20          conditionType: Established\n\
             \x20          status: \"True\"\n"
        );
        checks
    })
}

/// What one `admissionlab test` invocation reported.
struct RunOutput {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

/// Runs the compiled `admissionlab` binary as a black box, exactly as a
/// user would.
///
/// `NO_COLOR` is set so the captured output has no ANSI escapes to step
/// over; nothing else about the environment is touched.
fn run_admissionlab_test(config: &Path, reports: &Path) -> RunOutput {
    let binary = binary_path();
    let mut command = Command::new(&binary);
    command
        .arg("test")
        .arg(config)
        .arg("--report-dir")
        .arg(reports)
        // The terminal report's color decision belongs to the caller
        // that observed the stream. stdout is a pipe here so color would
        // already be off; setting this makes the assertions independent
        // of that inference, exactly as `tests/alpha_e2e.rs` does.
        .env("NO_COLOR", "1");
    let output = wait_with_timeout(command, RUN_TIMEOUT);
    RunOutput {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Builds `admissionlab-echo:dev` into the local Docker image store,
/// loading it nowhere.
///
/// The run itself side-loads it into both clusters through the lab
/// configuration's `images:` list — see this file's module documentation
/// for why that indirection exists and why it is a product feature
/// rather than a test workaround.
fn build_echo_image() {
    let repo = repo_root();
    let script = repo.join("scripts/build-test-images.sh");
    assert!(script.is_file(), "{} must exist", script.display());
    let mut command = Command::new("bash");
    command.arg(&script).current_dir(&repo);
    let output = wait_with_timeout(command, BUILD_TIMEOUT);
    assert!(
        output.status.success(),
        "scripts/build-test-images.sh failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Runs `command` to completion, failing the test if it outlives
/// `timeout`.
///
/// A bound rather than a bare `output()` because every external
/// interaction in this project has one (Global Constraint 13), and
/// because a hung `admissionlab test` would otherwise hold two `kind`
/// clusters until CI's own job timeout killed the whole runner.
fn wait_with_timeout(mut command: Command, timeout: Duration) -> std::process::Output {
    use std::io::Read as _;
    use std::process::Stdio;

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the command must be spawnable");
    // Drained on their own threads: a child that fills a pipe buffer
    // while this thread sleeps would deadlock, which is the classic way
    // a naive wait-with-timeout hangs on a chatty process — and
    // `admissionlab test` is very chatty.
    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let stdout_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buffer);
        buffer
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buffer);
        buffer
    });

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait().expect("the child must be waitable") {
            Some(status) => break status,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the command did not finish within {timeout:?}");
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    std::process::Output {
        status,
        stdout: stdout_reader.join().unwrap_or_default(),
        stderr: stderr_reader.join().unwrap_or_default(),
    }
}

/// Reads and parses a written `result.json`.
///
/// Parsed as generic JSON: `LabResult` is `Serialize`-only by design, so
/// there is no typed reader to use.
fn read_result(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&text).expect("result.json must be valid JSON")
}

/// The one `fixtures` entry for this lab's route contract.
fn gateway_entry(result: &Value) -> Value {
    result["fixtures"]
        .as_array()
        .expect("fixtures must be an array")
        .iter()
        .find(|entry| entry["fixtureId"] == CONTRACT_ID)
        .unwrap_or_else(|| panic!("no entry for {CONTRACT_ID}: {result}"))
        .clone()
}

/// `<run root>/<run id>/raw/<side>/gateway/<contract id>/`.
///
/// The run root is `admissionlab test`'s own documented default
/// (`commands::test::default_run_root`) rather than something this test
/// chose: there is no `--run-root` flag, and inventing one for a test
/// would make this file exercise a surface a user does not have.
fn gateway_evidence_dir(run_id: &str, side: &str) -> PathBuf {
    std::env::temp_dir()
        .join("admissionlab-runs")
        .join(run_id)
        .join("raw")
        .join(side)
        .join("gateway")
        .join(CONTRACT_ID)
}

/// The compiled `admissionlab` binary this test drives.
fn binary_path() -> PathBuf {
    // `CARGO_BIN_EXE_<name>` is set by Cargo for every binary target in
    // this crate when its integration tests are built, so the test drives
    // the binary this build produced rather than whatever is on `PATH`.
    PathBuf::from(env!("CARGO_BIN_EXE_admissionlab"))
}

/// The repository root, from this crate's own manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("this crate's own CARGO_MANIFEST_DIR/../.. must exist")
}

/// A fresh, unique scratch directory under the OS temp directory.
fn scratch_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "admissionlab-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos()),
    ))
}
