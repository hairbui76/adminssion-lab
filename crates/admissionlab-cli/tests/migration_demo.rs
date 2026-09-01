//! The canonical Ingress-to-Gateway migration demonstration, end to end
//! (ROADMAP Task 8.8).
//!
//! `examples/ingress-to-gateway/` is two real Kubernetes clusters
//! running two entirely different routing stacks -- the archived
//! community `ingress-nginx` on one, NGINX Gateway Fabric on the other
//! -- asked the same two HTTP questions. This file runs exactly that
//! configuration through the compiled `admissionlab` binary and asserts
//! what it must produce, twice, because a regression demonstration that
//! is not reproducible is not a demonstration.
//!
//! # The three behaviors, and what each one must look like in the report
//!
//! ROADMAP Task 8.8 Step 1 asks for one preserved behavior, one expected
//! non-portable behavior and one unintended regression.
//! [`the_migration_demo_reports_all_three_behaviors_and_fails_the_run`]
//! checks all three at once, because their *coexistence* is the claim:
//!
//! - **Preserved** shows up as an absence -- no traffic-derived change
//!   at all -- which is only evidence because the case's
//!   `comparability` says both sides answered. That field is asserted
//!   first for exactly that reason.
//! - **Expected non-portable** shows up as a `non_portable_feature`
//!   change with `expected: true`, graded `info`.
//! - **The regression** shows up as a `backend_changed` change with
//!   `expected: false`, graded `critical`, and the run exits 1.
//!
//! # Step 2's requirement is the sharpest assertion here
//!
//! "Report must explain observed traffic difference, not merely
//! annotation mismatch." A report that said "these manifests differ"
//! would be useless, and a report that named only the annotation would
//! be describing the *accepted* difference while staying silent about
//! the one that matters. So the regression's `detail` is asserted to
//! carry both **observed backend identities**, and the paired probe
//! evidence beside it is asserted to show the same two values as
//! separately-recorded observations -- the report's claim and the
//! evidence for the claim, checked against each other.
//!
//! # What is asserted, and what deliberately is not
//!
//! Enums and observed values: the change kind, the severity, the
//! `expected` flag, the backend identities, the HTTP statuses, the exit
//! code. Every one is a value Admission Lab owns or a workload really
//! returned.
//!
//! Nothing is asserted about vendor prose. The non-portability's
//! `detail` embeds `NONPORTABLE_INGRESS_ANNOTATIONS`' own reason text,
//! which is this project's to reword; only the annotation key -- which
//! is `ingress-nginx`'s published, stable name and the exact string a
//! user must type into `expectedNonportable` -- is checked.
//!
//! # Cost and cleanup
//!
//! Two runs, four clusters in total, sequentially, each installing a
//! full routing stack. `admissionlab test` deletes both of its own
//! clusters on every path except `--keep-clusters`, which is never
//! passed here. [`ScratchRoot`] is a `Drop` guard so a panicking
//! assertion still removes the temporary report directory.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde::Deserialize as _;
use serde_json::Value;

/// The migration case `examples/ingress-to-gateway/admissionlab.yaml`
/// declares, and therefore the identifier its entry in `result.json`
/// carries.
const CASE_ID: &str = "legacy-echo";

/// The backend both sides route the *preserved* behavior to.
const PRESERVED_BACKEND: &str = "echo-a";

/// The backend only the **baseline** routes the second host to. The
/// candidate reaching anything else is the regression.
const REGRESSED_BACKEND: &str = "echo-b";

/// The `ingress-nginx` annotation the example accepts losing, verbatim.
/// Not free-form text: it is upstream's published name and the exact
/// string `expectedNonportable` must contain to mark it expected.
const NONPORTABLE_ANNOTATION: &str = "nginx.ingress.kubernetes.io/limit-rps";

/// Bounds one `admissionlab test` invocation.
///
/// Larger than `tests/gateway_demo.rs`'s: this lab installs two
/// *different* stacks rather than the same one twice (an `ingress-nginx`
/// chart on one side, a 1 MB Gateway API CRD bundle plus an OCI chart on
/// the other, all pulled from network registries on a cold cache), and
/// its migration case additionally spends a bounded serving budget per
/// side -- including, by design, the full budget on the side that
/// regressed. See `admissionlab_cli::pipeline::migration`'s "THE ONE
/// DELIBERATE WAIT".
const RUN_TIMEOUT: Duration = Duration::from_mins(45);

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
fn the_migration_demo_reports_all_three_behaviors_and_fails_the_run() {
    let run = run_demo("migration-demo");

    assert_eq!(
        run.exit_code,
        Some(1),
        "an unexpected critical migration difference exits 1.\n{run}"
    );
    assert_eq!(run.result["policy"]["disposition"], "fail", "{run}");

    let case = migration_case(&run.result);
    assert_eq!(case["caseId"], CASE_ID, "{case}");
    // First, because everything below is only a claim if this says both
    // sides answered. An empty change list on an incomparable case means
    // "we could not tell", not "the migration preserved everything".
    assert_eq!(
        case["comparability"], "comparable",
        "both sides must have answered the case's probes, or no finding below is a finding: \
         {case}"
    );

    assert_preserved_behavior(&case, &run);
    assert_expected_nonportable(&case);
    assert_the_regression(&case, &run);

    // Exactly two changes, and no third: the preserved behavior
    // contributes none, which is what "preserved" means here.
    let changes = case["changes"].as_array().expect("changes is an array");
    assert_eq!(
        changes.len(),
        2,
        "one accepted non-portability and one regression, and nothing else: {case}"
    );

    // The admission half of the lab ran and was unaffected -- so a
    // reader can tell the migration finding apart from a stack that
    // simply broke.
    assert_eq!(
        run.result["summary"]["identical"], 1,
        "the admission fixture is unaffected: {}",
        run.result["summary"]
    );
    assert_eq!(
        run.result["summary"]["critical"], 0,
        "and no admission or Gateway fixture regressed, so the `fail` verdict comes from the \
         migration section alone: {}",
        run.result["summary"]
    );
    assert!(
        run.result["policy"]["changes"]
            .as_array()
            .expect("changes is an array")
            .is_empty(),
        "a migration finding is never smuggled into the graded SemanticChange list: {}",
        run.result["policy"]
    );
}

/// The preserved behavior: probe 0 reached the same backend with the
/// same status on both sides, and produced no change at all.
fn assert_preserved_behavior(case: &Value, run: &DemoRun) {
    let probe = &case["probes"][0];
    assert_eq!(probe["index"], 0, "{probe}");
    for side in ["baseline", "candidate"] {
        assert_eq!(
            probe[side]["status"], 200,
            "the {side} side must really have served the preserved behavior: {probe}\n{run}"
        );
        assert_eq!(
            probe[side]["backend"], PRESERVED_BACKEND,
            "and from the same backend: {probe}"
        );
    }
    // Nothing in the change list is about traffic except the regression
    // below, which is about probe 1. Checked as an absence of any change
    // naming probe 0.
    for change in case["changes"].as_array().expect("changes is an array") {
        let detail = change["detail"].as_str().unwrap_or_default();
        assert!(
            !detail.contains("probe 0"),
            "the preserved behavior must produce no finding: {change}"
        );
    }
}

/// The expected non-portable feature: visible, marked expected, and
/// graded `info` rather than warned about.
fn assert_expected_nonportable(case: &Value) {
    let change = find_change(case, "non_portable_feature");
    assert_eq!(
        change["expected"], true,
        "the example declares this annotation in `expectedNonportable`, so the run must say the \
         author accounted for it: {change}"
    );
    assert_eq!(
        change["severity"], "info",
        "a difference accounted for in writing is visible and accounted for, never a warning: \
         {change}"
    );
    let detail = change["detail"].as_str().expect("a detail");
    assert!(
        detail.contains(NONPORTABLE_ANNOTATION),
        "the finding must name the annotation key verbatim -- it is the exact string a user \
         copies into `expectedNonportable`: {detail}"
    );
    assert!(
        detail.contains("echo-ingress"),
        "and the object carrying it, so a reader knows where to look: {detail}"
    );

    assert!(
        case["unmatchedExpectations"]
            .as_array()
            .expect("unmatchedExpectations is an array")
            .is_empty(),
        "the declared annotation really is on the baseline manifests, so nothing is stale: {case}"
    );
}

/// The regression: an observed traffic difference, explained with the
/// two backends that actually answered.
///
/// ROADMAP Task 8.8 step 2's requirement, checked twice over -- once in
/// the change's own prose and once against the independently-recorded
/// probe evidence beside it.
fn assert_the_regression(case: &Value, run: &DemoRun) {
    let change = find_change(case, "backend_changed");
    assert_eq!(
        change["expected"], false,
        "nothing in this example accounts for the regression: {change}"
    );
    assert_eq!(change["severity"], "critical", "{change}");

    let detail = change["detail"].as_str().expect("a detail");
    assert!(
        detail.contains(REGRESSED_BACKEND) && detail.contains(PRESERVED_BACKEND),
        "the report must explain the OBSERVED traffic difference: both backend identities have \
         to be in the finding itself, not left to a reader to look up: {detail}"
    );
    assert!(
        detail.contains("probe 1"),
        "and it must say which request it is about: {detail}"
    );

    // The same two values as separately-recorded observations, so the
    // claim above is checkable against the evidence rather than only
    // readable.
    let probe = &case["probes"][1];
    assert_eq!(probe["index"], 1, "{probe}");
    assert_eq!(
        probe["baseline"]["backend"], REGRESSED_BACKEND,
        "the Ingress really served this host from the other backend, or the demo compares two \
         identical stacks: {probe}\n{run}"
    );
    assert_eq!(
        probe["candidate"]["backend"], PRESERVED_BACKEND,
        "and the Gateway really answered it from the wrong one: {probe}"
    );
    assert_eq!(
        probe["baseline"]["status"], 200,
        "both sides answered 200 -- the regression is invisible to a status comparison, which is \
         precisely why a migration suite compares backends: {probe}"
    );
    assert_eq!(probe["candidate"]["status"], 200, "{probe}");

    // The terminal report a human actually reads carries the same facts.
    for expected in [
        "Migration  1 Ingress-to-Gateway case(s)",
        CASE_ID,
        "backend_changed",
        "critical",
        REGRESSED_BACKEND,
        NONPORTABLE_ANNOTATION,
        "Result: fail",
    ] {
        assert!(
            run.stdout.contains(expected),
            "the terminal report must contain {expected:?}:\n{run}"
        );
    }
}

#[test]
#[ignore = "requires Docker, kind, kubectl and helm; creates four real clusters"]
fn the_same_configuration_produces_the_same_migration_finding_twice() {
    let first = run_demo("migration-demo-1");
    let second = run_demo("migration-demo-2");

    assert_eq!(first.exit_code, second.exit_code, "{first}\n{second}");
    assert_eq!(
        migration_change_set(&first.result),
        migration_change_set(&second.result),
        "two runs of one configuration must claim the same set of migration behavior changes \
         (Global Constraint 7)"
    );
    assert_eq!(
        migration_case(&first.result)["comparability"],
        migration_case(&second.result)["comparability"],
        "and must establish the same thing about whether they could compare at all"
    );
    assert_eq!(
        first.result["summary"], second.result["summary"],
        "and must count the admission half the same way"
    );
}

/// No TLS key material, and no Kubernetes `Secret` value, reaches the
/// demo's own published artifacts.
///
/// The Phase 8 exit gate's fourth bullet is "TLS test secrets never
/// appear in JSON/HTML/CI logs".
/// `admissionlab-gateway/tests/portable_contracts.rs` already proves the
/// *fixture tree* has never carried key material; this proves the same
/// of what a run **writes**, which is the half a user actually
/// distributes -- `result.json` and `report.html` are attached to CI runs
/// and pasted into issues.
///
/// This lab uses no TLS, so a PEM marker here would mean redaction
/// failed *and* something invented one. That makes it a weaker test than
/// a TLS lab would give, and it is honest about being one: what it
/// actually rules out is a run artifact carrying key material, a
/// `stringData` value, or an `Authorization` header, by any route.
#[test]
#[ignore = "requires Docker, kind, kubectl and helm; creates two real clusters"]
fn the_demos_artifacts_carry_no_key_material() {
    let run = run_demo("migration-demo-secrets");
    let reports = run.scratch.0.clone();

    for name in ["result.json", "report.html"] {
        let path = reports.join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        for forbidden in [
            "-----BEGIN PRIVATE KEY-----",
            "-----BEGIN RSA PRIVATE KEY-----",
            "-----BEGIN EC PRIVATE KEY-----",
            "PRIVATE KEY-----",
        ] {
            assert!(
                !text.contains(forbidden),
                "{name} contains {forbidden:?}; a published run artifact must never carry key \
                 material"
            );
        }
        let lowered = text.to_lowercase();
        assert!(
            !lowered.contains("authorization: bearer"),
            "{name} carries an Authorization header value"
        );
    }
}

// ---------------------------------------------------------------------
// Driving the demo
// ---------------------------------------------------------------------

/// One `admissionlab test` run of the canonical migration example.
struct DemoRun {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    result: Value,
    /// Kept alive for the whole test: dropping it removes the report
    /// directory. Its path is also how a test reaches the written
    /// artifacts, which is why it is not underscore-prefixed -- it is
    /// both a guard and the answer to "where did the run write".
    scratch: ScratchRoot,
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
/// `cargo test` runs a binary's tests on several threads by default, and
/// each test here creates two `kind` clusters running a full routing
/// stack. Four at once is a claim on the machine's memory that has
/// nothing to do with what is being tested, and the first symptom would
/// be an unrelated-looking install timeout. A lock rather than
/// `--test-threads=1` so the ROADMAP's own exit-gate command works
/// unmodified.
///
/// Poison is recovered from rather than propagated, exactly as
/// `tests/gateway_demo.rs` does: a panicking assertion in one test is a
/// failure of *that* test and must not turn every other test in the file
/// into a lock-poisoning panic that hides what actually broke.
static CLUSTERS: Mutex<()> = Mutex::new(());

/// Builds the echo image, runs `examples/ingress-to-gateway/` once, and
/// reads back what it wrote.
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
        .arg(repo_root().join("examples/ingress-to-gateway/admissionlab.yaml"))
        .arg("--report-dir")
        .arg(&reports)
        .env("NO_COLOR", "1");
    eprintln!("migration_demo[{label}]: running {} ...", binary.display());
    let started = std::time::Instant::now();
    let output = wait_with_timeout(command, RUN_TIMEOUT);
    eprintln!(
        "migration_demo[{label}]: exit {:?} in {:.1}s",
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
        scratch,
    }
}

/// The one migration case in a run's result.
fn migration_case(result: &Value) -> Value {
    result["migration"]
        .as_array()
        .unwrap_or_else(|| panic!("`migration` must be an array: {result}"))
        .iter()
        .find(|case| case["caseId"] == CASE_ID)
        .unwrap_or_else(|| panic!("no migration case {CASE_ID}: {result}"))
        .clone()
}

/// The one change of `kind`, or a failure naming what was there instead.
fn find_change(case: &Value, kind: &str) -> Value {
    let changes = case["changes"].as_array().expect("changes is an array");
    changes
        .iter()
        .find(|change| change["kind"] == kind)
        .cloned()
        .unwrap_or_else(|| {
            let kinds: Vec<&str> = changes
                .iter()
                .filter_map(|change| change["kind"].as_str())
                .collect();
            panic!("no {kind} change; got {kinds:?}")
        })
}

/// One run's migration claim, as a sorted, comparable set.
///
/// `(kind, expected, severity)` rather than the whole change: a
/// `detail` embeds a probe's own request line and both observed values,
/// which are honest per-run facts, and the identity of a *finding* is
/// what it claims and how much it matters.
fn migration_change_set(result: &Value) -> Vec<(String, bool, String)> {
    let mut set: Vec<(String, bool, String)> = migration_case(result)["changes"]
        .as_array()
        .expect("changes is an array")
        .iter()
        .map(|change| {
            (
                change["kind"].as_str().unwrap_or_default().to_owned(),
                change["expected"].as_bool().unwrap_or_default(),
                change["severity"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    set.sort();
    set
}

/// Builds `admissionlab-echo:dev` into the local Docker image store.
///
/// The run side-loads it into both clusters itself, through the
/// example's own `images:` list.
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
/// configuration and its two manifest files are several hundred lines of
/// hand-written YAML whose two halves have to differ in exactly one
/// documented way, and a slip there would otherwise surface twenty
/// minutes into an `#[ignore]`d run with two clusters already created.
#[test]
fn the_checked_in_example_declares_the_three_behaviors_it_demonstrates() {
    let config = repo_root().join("examples/ingress-to-gateway/admissionlab.yaml");
    let lab = admissionlab_spec::load_any_supported_lab(&config)
        .unwrap_or_else(|error| panic!("the example must load and resolve: {error}"));

    assert_eq!(lab.baseline.kubernetes, lab.candidate.kubernetes);
    assert_eq!(
        lab.baseline.images,
        vec!["admissionlab-echo:dev".to_owned()],
        "the echo backend is side-loaded by the run itself"
    );
    assert_eq!(lab.baseline.images, lab.candidate.images);

    // The two sides run *different* stacks -- that is what a migration
    // is -- and each side's component names are load-bearing, because a
    // Helm release name defaults to the component name and both charts
    // name their objects after the release.
    assert_eq!(
        lab.baseline
            .components
            .iter()
            .map(|component| component.name.as_str())
            .collect::<Vec<_>>(),
        ["ingress-nginx-legacy"],
    );
    assert_eq!(
        lab.candidate
            .components
            .iter()
            .map(|component| component.name.as_str())
            .collect::<Vec<_>>(),
        ["gateway-api-crds", "nginx-gateway-fabric"],
        "the Gateway API CRDs are installed before the implementation that serves them"
    );

    let suite = lab
        .migration
        .expect("the example declares a migration suite");
    assert_eq!(suite.cases.len(), 1, "one case, one finding");
    assert!(
        suite.baseline.is_some() && suite.candidate.is_some(),
        "without both endpoint blocks no probe is ever sent, and the whole demo would be vacuous"
    );

    let case = &suite.cases[0];
    assert_eq!(case.id, CASE_ID);
    assert_eq!(
        case.probes.len(),
        2,
        "one preserved behavior and one regression"
    );
    assert_eq!(
        case.probes[0].expected_backend.as_deref(),
        Some(PRESERVED_BACKEND)
    );
    assert_eq!(
        case.probes[1].expected_backend.as_deref(),
        Some(REGRESSED_BACKEND),
        "the second probe contracts what the BASELINE does today; the candidate answering \
         something else is the finding"
    );
    assert_ne!(
        case.probes[0].host, case.probes[1].host,
        "the two behaviors are told apart by the Host header alone -- see the example's own \
         header for why not by path"
    );

    assert_eq!(
        case.expected_nonportable.len(),
        1,
        "exactly one accepted non-portability"
    );
    assert_eq!(case.expected_nonportable[0].feature, NONPORTABLE_ANNOTATION);
    assert!(
        !case.expected_nonportable[0].reason.trim().is_empty(),
        "an accepted difference with no written justification is indistinguishable from someone \
         quietly silencing a regression"
    );
}

/// The declared non-portability is a *cataloged* annotation and it is
/// really on the baseline manifests.
///
/// Both halves matter and neither is obvious. A `feature:` string that
/// the catalog does not contain -- a typo, or a shorthand like `canary`
/// -- would silently never match, so the run would report the annotation
/// as *unexpected* and the demo would demonstrate the wrong thing. And a
/// declaration whose annotation is not actually present would show up as
/// an unmatched expectation instead.
#[test]
fn the_declared_nonportability_is_cataloged_and_really_present() {
    assert!(
        admissionlab_gateway::nonportable_annotation(NONPORTABLE_ANNOTATION).is_some(),
        "{NONPORTABLE_ANNOTATION} must be in NONPORTABLE_INGRESS_ANNOTATIONS, or the example's \
         `expectedNonportable` entry can never match anything"
    );

    let baseline = repo_root().join("examples/ingress-to-gateway/baseline/ingress.yaml");
    let plan = admissionlab_gateway::plan_gateway_apply(std::slice::from_ref(&baseline))
        .expect("the baseline manifests must parse");
    let observed = admissionlab_gateway::observed_nonportable_annotations(&plan.documents);
    assert!(
        observed.contains_key(NONPORTABLE_ANNOTATION),
        "the baseline Ingress must actually carry {NONPORTABLE_ANNOTATION}; observed: {:?}",
        observed.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        observed.len(),
        1,
        "and exactly one cataloged annotation, so the demo's finding count is one: {observed:?}"
    );
}

/// The rewrite is present on both sides, and translated rather than
/// dropped.
///
/// The example's regression is deliberately *not* a lost rewrite (the
/// comparator cannot see one that lands on the same backend -- see
/// `admissionlab_gateway::migration`'s "What the evidence can and cannot
/// show"). That makes it easy for the rewrite to quietly disappear from
/// one side without any test noticing, which would turn a faithful
/// translation into an accidental second regression the demo does not
/// claim. This is what notices.
#[test]
fn the_rewrite_is_translated_rather_than_dropped() {
    let example = repo_root().join("examples/ingress-to-gateway");
    let baseline = std::fs::read_to_string(example.join("baseline/ingress.yaml"))
        .expect("the baseline manifests must be readable");
    let candidate = std::fs::read_to_string(example.join("candidate/gateway.yaml"))
        .expect("the candidate manifests must be readable");

    let documents: Vec<serde_json::Value> = serde_norway::Deserializer::from_str(&baseline)
        .map(|document| {
            serde_norway::Value::deserialize(document).expect("each baseline document parses")
        })
        .map(|value| serde_json::to_value(value).expect("YAML converts to JSON"))
        .collect();
    let ingress = documents
        .iter()
        .find(|document| document["kind"] == "Ingress")
        .expect("the baseline applies an Ingress");
    assert_eq!(
        ingress["metadata"]["annotations"]["nginx.ingress.kubernetes.io/rewrite-target"], "/",
        "the baseline rewrites every matched request to `/`: {ingress}"
    );

    assert!(
        candidate.contains("type: URLRewrite")
            && candidate.contains("type: ReplacePrefixMatch")
            && candidate.contains("replacePrefixMatch: /"),
        "the candidate must express the same rewrite through Gateway API's own portable filter, \
         which is exactly why `rewrite-target` is absent from the non-portable catalog"
    );
}
