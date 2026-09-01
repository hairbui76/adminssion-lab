//! The canonical Istio Gateway regression demonstration, end to end
//! (ROADMAP Task 6.12).
//!
//! `examples/gateway-istio/` is two real Kubernetes clusters, each
//! running a real Istio serving the Gateway API, told apart by one line
//! of YAML in a `ReferenceGrant`. This file runs exactly that
//! configuration through the compiled `admissionlab` binary and asserts
//! what it must produce — twice, because a regression demonstration that
//! is not reproducible is not a demonstration.
//!
//! # What is asserted, and what is deliberately not
//!
//! The assertions are on **enums**: the change kind, the condition type,
//! the condition state, the machine-readable `reason`, the HTTP status,
//! the backend identity, and the exit code. Every one of those is a
//! value Gateway API itself specifies or that Admission Lab owns.
//!
//! Nothing is asserted about free-form message text. A controller's
//! `message` is prose the implementation is free to reword between
//! patch releases, and a test that pinned it would fail on an Istio
//! upgrade that changed nothing observable. The messages are still
//! *carried* — they are in `reconciliation.json` and in the report — and
//! [`raw_controller_messages_are_preserved_but_never_asserted_on`]
//! proves they survive the round trip without any assertion depending
//! on their content.
//!
//! `reason` is the one string that *is* asserted, and it is not
//! free-form: `RefNotPermitted` is a value Gateway API's own
//! specification names for exactly this case, alongside the
//! `ResolvedRefs: False` it requires.
//!
//! # Determinism
//!
//! [`the_same_configuration_produces_the_same_finding_twice`] runs the
//! whole lab twice and compares the two runs' semantic change sets as
//! sorted `(kind, subject)` pairs. Phase 4's lesson was Istio's
//! patch-operation ordering, which made a naive admission comparison
//! non-deterministic; this comparator never looks at a patch. It
//! compares condition states and backend identities, so it should be
//! stable by construction — this test is what turns "should be" into
//! "is", at the cost of a second full run.
//!
//! # Cost and cleanup
//!
//! Two runs, four clusters in total, sequentially. `admissionlab test`
//! deletes both of its own clusters on every path except
//! `--keep-clusters`, which is never passed here. [`ScratchRoot`] is a
//! `Drop` guard so a panicking assertion still removes the temporary
//! report directory.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

/// The route contract `examples/gateway-istio/admissionlab.yaml`
/// declares, and therefore the identifier its entry in `result.json`
/// carries.
const CONTRACT_ID: &str = "echo-route";

/// The `HTTPRoute` under contract, which every user-facing finding must
/// name.
const ROUTE_NAME: &str = "echo-route";

/// The backend the baseline's probe reaches, as the echo response
/// identifies it.
const BACKEND: &str = "echo-b";

/// The reason Gateway API specifies for a `backendRef` that no
/// `ReferenceGrant` permits. Not free-form text: this is a value the
/// specification names.
const REF_NOT_PERMITTED: &str = "RefNotPermitted";

/// Bounds one `admissionlab test` invocation. See
/// `tests/gateway_e2e.rs` for the same bound and the same argument: two
/// clusters, two four-component stacks, a Gateway suite per side, and
/// cleanup, sized for a cold image cache rather than the happy path.
const RUN_TIMEOUT: Duration = Duration::from_mins(40);

/// Bounds `scripts/build-test-images.sh`.
const BUILD_TIMEOUT: Duration = Duration::from_mins(15);

/// Removes a temporary directory when dropped.
struct ScratchRoot(PathBuf);

impl Drop for ScratchRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore = "requires Docker, kind, kubectl and helm; creates two real clusters"]
fn the_candidates_broken_reference_grant_fails_the_run_and_names_the_route() {
    let run = run_demo("gateway-demo");

    assert_eq!(
        run.exit_code,
        Some(1),
        "an unexpected critical difference exits 1.\n{run}"
    );
    assert_eq!(run.result["policy"]["disposition"], "fail", "{run}");

    let entry = gateway_entry(&run.result);
    let changes = classified_changes(&entry);

    assert_condition_findings(&changes);
    assert_traffic_finding(&changes, &run);
    assert_gateway_unaffected(&entry);

    // Both sides converged: the pair is comparable, so an empty change
    // list would have meant agreement rather than ignorance -- which is
    // what makes the findings above claims rather than guesses.
    assert_eq!(
        run.result["summary"]["inconclusive"], 0,
        "{}",
        run.result["summary"]
    );
    assert_eq!(run.result["summary"]["critical"], 1);
    assert_eq!(
        run.result["summary"]["identical"], 1,
        "the admission fixture is unaffected"
    );

    // The terminal report a human actually reads carries the same facts.
    for expected in [
        "backend_resolution_changed",
        "resolved_refs_condition_changed",
        "traffic_status_changed",
        ROUTE_NAME,
        REF_NOT_PERMITTED,
        "candidate answered nothing",
        "Result: fail",
    ] {
        assert!(
            run.stdout.contains(expected),
            "the terminal report must contain {expected:?}:\n{run}"
        );
    }
}

/// The first user-facing failure names the route, the condition that
/// changed, and its reason (ROADMAP Task 6.12 step 2).
///
/// Task 6.9 pairs two claims here: the product-level one, that this
/// route's backends stopped resolving, and the condition evidence for
/// it. A policy must be able to grade and except them separately, so
/// both are made and both are checked.
fn assert_condition_findings(changes: &[Value]) {
    let resolution = find_change(changes, "backend_resolution_changed");
    let condition = find_change(changes, "resolved_refs_condition_changed");

    for change in [&resolution, &condition] {
        assert_eq!(
            change["change"]["subject"], ROUTE_NAME,
            "every Gateway finding must name the route it is about: {change}"
        );
        assert_eq!(change["severity"], "critical", "{change}");
        assert_eq!(
            change["expected"], false,
            "nothing in this demo accounts for the regression: {change}"
        );

        let baseline = &change["change"]["baseline"];
        let candidate = &change["change"]["candidate"];
        assert_eq!(baseline["object"], "HTTPRoute", "{change}");
        assert_eq!(baseline["name"], ROUTE_NAME, "{change}");
        assert_eq!(baseline["condition"], "ResolvedRefs", "{change}");
        assert_eq!(baseline["state"], "True", "{change}");
        assert_eq!(candidate["condition"], "ResolvedRefs", "{change}");
        assert_eq!(candidate["state"], "False", "{change}");
        assert_eq!(
            candidate["reason"], REF_NOT_PERMITTED,
            "the reason Gateway API specifies for a backendRef no grant permits: {change}"
        );
        assert_eq!(
            candidate["direction"], "regression",
            "leaving a True the baseline published is a regression, and the comparator must \
             encode that rather than leave it to a reader: {change}"
        );
    }
}

/// The skipped traffic behavior is part of the same failure, and the
/// skip carries the condition that caused it.
fn assert_traffic_finding(changes: &[Value], run: &DemoRun) {
    let traffic = find_change(changes, "traffic_status_changed");
    assert_eq!(traffic["change"]["subject"], ROUTE_NAME, "{traffic}");
    assert_eq!(traffic["change"]["baseline"]["status"], 200, "{traffic}");
    assert_eq!(
        traffic["change"]["baseline"]["backend"], BACKEND,
        "the baseline's request reached the real backend: {traffic}"
    );
    assert!(
        traffic["change"]["candidate"].is_null(),
        "the candidate answered no probe at all; a fabricated status here would be a claim about \
         a request that was never sent: {traffic}"
    );

    let skip = run.result["diagnostics"]
        .as_array()
        .expect("diagnostics must be an array")
        .iter()
        .find(|diagnostic| diagnostic["code"] == "gateway.probe_skipped")
        .unwrap_or_else(|| panic!("no probe-skip diagnostic:\n{run}"));
    let message = skip["message"].as_str().expect("a message");
    assert!(
        message.contains(CONTRACT_ID) && message.contains("candidate"),
        "the skip must name the contract and the side: {message}"
    );
    assert!(
        message.contains("ResolvedRefs") && message.contains(REF_NOT_PERMITTED),
        "the skip must name the condition that caused it: {message}"
    );
}

/// What did **not** change is as much a part of the demonstration: the
/// `Gateway` is unaffected on both sides and the route still attaches,
/// which is what makes the one finding unambiguous.
fn assert_gateway_unaffected(entry: &Value) {
    let gateway = &entry["gatewayReconciliation"];
    for side in ["baseline", "candidate"] {
        let reconciliation = &gateway[side];
        for type_name in ["Accepted", "Programmed"] {
            assert_eq!(
                reconciliation["gateway"]["conditions"][type_name]["state"], "True",
                "the {side} Gateway must be unaffected: {reconciliation}"
            );
        }
        assert_eq!(
            reconciliation["route"]["parents"][0]["conditions"]["Accepted"]["state"], "True",
            "the {side} route still attaches; only its backend reference is refused: \
             {reconciliation}"
        );
    }
    // The candidate's probe was skipped, so the baseline's is unpaired
    // and the traffic section says so in a written word rather than by
    // leaving `pairs` empty (ROADMAP Task 7.2, Global Constraint 15).
    let traffic = &entry["traffic"];
    assert_eq!(traffic["evidence"], "partial", "{traffic}");
    assert_eq!(
        traffic["unpairedBaseline"][0]["status"], 200,
        "the baseline must actually carry traffic, or this demo compares two failures"
    );
    assert_eq!(traffic["unpairedBaseline"][0]["backend"], BACKEND);
    assert_eq!(
        traffic["unpairedCandidate"]
            .as_array()
            .expect("unpaired candidate probes must be an array")
            .len(),
        0,
        "the candidate's probe was skipped"
    );
}

#[test]
#[ignore = "requires Docker, kind, kubectl and helm; creates four real clusters"]
fn the_same_configuration_produces_the_same_finding_twice() {
    let first = run_demo("gateway-demo-1");
    let second = run_demo("gateway-demo-2");

    assert_eq!(first.exit_code, second.exit_code, "{first}\n{second}");
    assert_eq!(
        change_set(&first.result),
        change_set(&second.result),
        "two runs of one configuration must claim the same set of behavior changes \
         (Global Constraint 7)"
    );
    assert_eq!(
        first.result["summary"], second.result["summary"],
        "and must count them the same way"
    );
}

#[test]
#[ignore = "requires Docker, kind, kubectl and helm; creates two real clusters"]
fn raw_controller_messages_are_preserved_but_never_asserted_on() {
    let run = run_demo("gateway-demo-messages");
    let entry = gateway_entry(&run.result);

    // Istio does write a message for this condition, and the report
    // carries whatever it wrote, verbatim. This test proves the
    // evidence survives -- and it is the only place this file looks at a
    // message at all, deliberately asserting nothing about its content
    // beyond that it exists.
    let candidate = &entry["gatewayReconciliation"]["candidate"];
    let condition = &candidate["route"]["parents"][0]["conditions"]["ResolvedRefs"];
    assert_eq!(condition["state"], "False", "{candidate}");
    assert_eq!(condition["reason"], REF_NOT_PERMITTED, "{candidate}");
    assert!(
        condition["observedGeneration"].is_number(),
        "a condition with no observedGeneration cannot be held current for the spec under test: \
         {condition}"
    );

    // The raw evidence bundle on disk carries the same observation, so a
    // reader who wants the implementation's own words has somewhere to
    // find them.
    let run_id = run.result["runId"].as_str().expect("a run id");
    let reconciliation = std::env::temp_dir()
        .join("admissionlab-runs")
        .join(run_id)
        .join("raw/candidate/gateway")
        .join(CONTRACT_ID)
        .join("reconciliation.json");
    assert!(
        reconciliation.is_file(),
        "{} must exist",
        reconciliation.display()
    );
    let raw: Value = serde_json::from_str(
        &std::fs::read_to_string(&reconciliation).expect("the evidence must be readable"),
    )
    .expect("the evidence must be valid JSON");
    assert_eq!(
        raw["route"]["parents"][0]["conditions"]["ResolvedRefs"]["reason"],
        REF_NOT_PERMITTED
    );

    // And the skipped probe is recorded beside it, with the request that
    // was not sent.
    let probes = std::env::temp_dir()
        .join("admissionlab-runs")
        .join(run_id)
        .join("raw/candidate/gateway")
        .join(CONTRACT_ID)
        .join("probes.json");
    let probes: Value = serde_json::from_str(
        &std::fs::read_to_string(&probes).expect("the probe evidence must be readable"),
    )
    .expect("the probe evidence must be valid JSON");
    assert!(
        probes["sent"]
            .as_array()
            .expect("sent must be an array")
            .is_empty(),
        "{probes}"
    );
    let skipped = &probes["skipped"][0];
    assert_eq!(skipped["probeIndex"], 0, "{probes}");
    assert!(
        skipped["request"]
            .as_str()
            .is_some_and(|request| request.contains("GET")),
        "the request that was not sent is recorded: {probes}"
    );
    assert!(
        skipped["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains(REF_NOT_PERMITTED)),
        "with the condition that caused the skip: {probes}"
    );
}

// ---------------------------------------------------------------------
// Driving the demo
// ---------------------------------------------------------------------

/// One `admissionlab test` run of the canonical example.
struct DemoRun {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    result: Value,
    /// Kept alive for the whole test: dropping it removes the report
    /// directory.
    _scratch: ScratchRoot,
}

impl std::fmt::Display for DemoRun {
    /// Prints both streams, because every failing assertion here is far
    /// easier to diagnose with the run's own output attached.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "exit {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.exit_code, self.stdout, self.stderr
        )
    }
}

/// Serializes the real-cluster tests in this file.
///
/// `cargo test` runs a binary's tests on several threads by default,
/// and each test here creates two `kind` clusters running a full Istio.
/// Four of them at once is a claim on the machine's memory that has
/// nothing to do with what is being tested, and the first symptom would
/// be an unrelated-looking install timeout. A lock rather than
/// `--test-threads=1` so the ROADMAP's own exit-gate command works
/// unmodified.
///
/// Poison is recovered from rather than propagated: a panicking
/// assertion in one test is a failure of *that* test, and must not turn
/// every other test in the file into a lock-poisoning panic that hides
/// what actually broke.
static CLUSTERS: Mutex<()> = Mutex::new(());

/// Builds the echo image, runs `examples/gateway-istio/` once, and reads
/// back what it wrote.
fn run_demo(label: &str) -> DemoRun {
    let _serialized = CLUSTERS.lock().unwrap_or_else(PoisonError::into_inner);
    build_echo_image();

    let reports = scratch_dir(label);
    let scratch = ScratchRoot(reports.clone());
    std::fs::create_dir_all(&reports).expect("the report directory must be creatable");

    let binary = binary_path();
    let mut command = Command::new(&binary);
    command
        .arg("test")
        .arg(repo_root().join("examples/gateway-istio/admissionlab.yaml"))
        .arg("--report-dir")
        .arg(&reports)
        .env("NO_COLOR", "1");
    eprintln!("gateway_demo[{label}]: running {} ...", binary.display());
    let started = std::time::Instant::now();
    let output = wait_with_timeout(command, RUN_TIMEOUT);
    eprintln!(
        "gateway_demo[{label}]: exit {:?} in {:.1}s",
        output.status.code(),
        started.elapsed().as_secs_f64()
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let result_path = reports.join("result.json");
    let result = match std::fs::read_to_string(&result_path) {
        Ok(text) => serde_json::from_str(&text).expect("result.json must be valid JSON"),
        Err(error) => panic!(
            "failed to read {}: {error}\nexit {:?}\n--- stdout ---\n{stdout}\n--- stderr \
             ---\n{stderr}",
            result_path.display(),
            output.status.code(),
        ),
    };

    DemoRun {
        exit_code: output.status.code(),
        stdout,
        stderr,
        result,
        _scratch: scratch,
    }
}

/// The `fixtures` entry for the demo's route contract.
fn gateway_entry(result: &Value) -> Value {
    result["fixtures"]
        .as_array()
        .expect("fixtures must be an array")
        .iter()
        .find(|entry| entry["fixtureId"] == CONTRACT_ID)
        .unwrap_or_else(|| panic!("no entry for {CONTRACT_ID}: {result}"))
        .clone()
}

/// Every graded change on one entry.
fn classified_changes(entry: &Value) -> Vec<Value> {
    entry["changes"]
        .as_array()
        .expect("changes must be an array")
        .clone()
}

/// The one change of `kind`, or a failure naming what was there instead.
fn find_change(changes: &[Value], kind: &str) -> Value {
    changes
        .iter()
        .find(|classified| classified["change"]["kind"] == kind)
        .cloned()
        .unwrap_or_else(|| {
            let kinds: Vec<&str> = changes
                .iter()
                .filter_map(|classified| classified["change"]["kind"].as_str())
                .collect();
            panic!("no {kind} change; got {kinds:?}")
        })
}

/// One run's whole claim, as a sorted, comparable set.
///
/// `(kind, fixture, subject)` rather than the full change: a payload
/// carries elapsed times and observed generations, which are honest
/// per-run values and are not part of what "the same finding" means.
fn change_set(result: &Value) -> Vec<(String, String, String)> {
    let mut set: Vec<(String, String, String)> = result["policy"]["changes"]
        .as_array()
        .expect("changes must be an array")
        .iter()
        .map(|classified| {
            let change = &classified["change"];
            (
                change["kind"].as_str().unwrap_or_default().to_owned(),
                change["fixture_id"].as_str().unwrap_or_default().to_owned(),
                change["subject"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    set.sort();
    set
}

/// Builds `admissionlab-echo:dev` into the local Docker image store.
///
/// The run side-loads it into both clusters itself, through the
/// example's own `images:` list — see `tests/gateway_e2e.rs` for why
/// that indirection exists.
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
/// Both pipes are drained on their own threads: a child that fills a
/// pipe buffer while this thread sleeps would deadlock, and
/// `admissionlab test` is very chatty.
fn wait_with_timeout(mut command: Command, timeout: Duration) -> std::process::Output {
    use std::io::Read as _;
    use std::process::Stdio;

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the command must be spawnable");
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

/// The compiled `admissionlab` binary this test drives.
fn binary_path() -> PathBuf {
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

// ---------------------------------------------------------------------
// The fast half: what the checked-in example declares, with no cluster.
// ---------------------------------------------------------------------

/// The example must load, resolve, and declare the shape every
/// assertion above depends on.
///
/// Runs under plain `cargo test --workspace`. Not decoration: the
/// configuration is 400 lines of hand-written YAML whose per-side halves
/// must stay identical except for one component, and a slip there would
/// otherwise surface twenty minutes into an `#[ignore]`d run with two
/// clusters already created.
#[test]
fn the_checked_in_example_declares_exactly_one_per_side_difference() {
    let config = repo_root().join("examples/gateway-istio/admissionlab.yaml");
    // The version-aware loader, because the examples are `v1beta1`
    // documents as of Task 7.7 and because it is what the binary uses.
    let lab = admissionlab_spec::load_any_supported_lab(&config)
        .unwrap_or_else(|error| panic!("the example must load and resolve: {error}"));

    assert_eq!(lab.baseline.kubernetes, lab.candidate.kubernetes);
    assert_eq!(lab.baseline.images, lab.candidate.images);
    assert_eq!(
        lab.baseline.images,
        vec!["admissionlab-echo:dev".to_owned()],
        "the echo backend is side-loaded by the run itself"
    );

    let names: Vec<&str> = lab
        .baseline
        .components
        .iter()
        .map(|component| component.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "demo-namespaces",
            "gateway-api-crds",
            "istio-gateway",
            "gateway-permissions"
        ],
        "the permission set is installed last, after the CRDs that define its kind"
    );
    assert_eq!(
        names,
        lab.candidate
            .components
            .iter()
            .map(|component| component.name.as_str())
            .collect::<Vec<_>>(),
        "both sides run the same components in the same order"
    );

    // Every component but the last is byte-identical across the two
    // sides. That is the single-variable property this demo rests on,
    // and it is checked rather than asserted in a comment.
    for (baseline, candidate) in lab
        .baseline
        .components
        .iter()
        .zip(lab.candidate.components.iter())
        .take(3)
    {
        assert_eq!(
            baseline, candidate,
            "component {:?} must be identical on both sides",
            baseline.name
        );
    }
    let baseline_grant = &lab.baseline.components[3];
    let candidate_grant = &lab.candidate.components[3];
    assert_ne!(
        baseline_grant, candidate_grant,
        "the permission component is the whole of the difference"
    );

    let suite = lab.gateway.expect("the example declares a gateway suite");
    assert_eq!(suite.routes.len(), 1, "one route, one finding");
    assert_eq!(suite.routes[0].id, CONTRACT_ID);
    assert_eq!(suite.routes[0].probes.len(), 1);
    assert_eq!(
        suite.routes[0].probes[0].expected_backend.as_deref(),
        Some(BACKEND)
    );
    assert!(
        suite.gateway_endpoint.is_some(),
        "without an endpoint strategy no probe is ever sent, and the traffic half of this demo \
         would be vacuous"
    );
    assert_eq!(
        suite.readiness.len(),
        2,
        "the backend and the provisioned data plane are both waited on"
    );
}

/// The two `ReferenceGrant`s differ in exactly one field, and it is the
/// one that makes the reference impermissible.
///
/// Parsed rather than diffed as text: a comment change must not fail
/// this, and the assertion is about the objects, not the files.
#[test]
fn the_two_reference_grants_differ_only_in_the_service_they_permit() {
    let baseline = grant("baseline");
    let candidate = grant("candidate");

    for (field, value) in [
        ("apiVersion", "gateway.networking.k8s.io/v1beta1"),
        ("kind", "ReferenceGrant"),
    ] {
        assert_eq!(baseline[field].as_str(), Some(value));
        assert_eq!(candidate[field].as_str(), Some(value));
    }
    assert_eq!(baseline["metadata"], candidate["metadata"]);
    assert_eq!(
        baseline["spec"]["from"], candidate["spec"]["from"],
        "both sides permit references from the same namespace and kind"
    );

    assert_eq!(
        baseline["spec"]["to"][0]["name"].as_str(),
        Some(BACKEND),
        "the baseline permits the Service the route actually references"
    );
    assert_eq!(
        candidate["spec"]["to"][0]["name"].as_str(),
        Some("echo-legacy"),
        "the candidate permits a Service the route does not reference, and which does not exist"
    );
    assert_ne!(
        baseline["spec"]["to"][0]["name"], candidate["spec"]["to"][0]["name"],
        "one field, one regression"
    );
}

/// One side's `ReferenceGrant`, parsed.
fn grant(side: &str) -> Value {
    let path = repo_root()
        .join("examples/gateway-istio/stacks")
        .join(side)
        .join("reference-grant.yaml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_norway::from_str(&text)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}
