//! The Public Alpha exit gate, end to end: `examples/kyverno-istio-upgrade`
//! driven through the real `admissionlab` binary, against two real
//! `kind` clusters running a real Kyverno and a real Istio.
//!
//! ```bash
//! cargo test -p admissionlab-cli --test alpha_e2e -- --ignored --nocapture
//! ```
//!
//! # What this asserts, and why it is the whole product
//!
//! `tests/test_command.rs` already drives `pipeline::run_lab` against
//! fake cluster/installer/capture backends, which is the right way to
//! pin the pipeline's *decisions* (which failure maps to which exit
//! code, when reports are written, when cleanup runs). Every one of
//! those fakes answers exactly what it was told to answer, so none of
//! them can establish the one claim the Alpha definition actually makes:
//! that a platform engineer comparing two real admission stacks receives
//! a deterministic verdict with a real semantic regression and a real
//! first observable divergence in it.
//!
//! So this file uses no fakes and no library entry point. It runs the
//! compiled binary, the way a user does, with `--report-dir`, and reads
//! back only what a user can read: the process's exit code, its stdout,
//! and the `result.json`/`report.html` it wrote. `LabResult` is emit-only
//! by design (`admissionlab_report::model` derives `Serialize` and
//! deliberately not `Deserialize`), so `result.json` is parsed here as
//! generic JSON — which is also, usefully, exactly what a CI job
//! consuming it would have to do.
//!
//! # The one run, twice
//!
//! [`alpha_regression_corpus_reports_a_deterministic_critical_regression`]
//! runs the lab **twice**, sequentially, and compares the two
//! `result.json`s' semantic-change sets. Global Constraint 7 requires a
//! deterministic result, and determinism is not a property any single
//! run can demonstrate: two clusters created seconds apart, pulling
//! images concurrently, running four controllers apiece, will disagree
//! about wall-clock everything. What they must not disagree about is
//! what changed. Doing it inside one test rather than two keeps the two
//! runs sequential — four simultaneous `kind` clusters on one machine is
//! a different experiment than the one this file is running.
//!
//! # Cleanup
//!
//! The binary deletes both of its clusters itself, on every ordinary
//! path including failure (`pipeline::run_lab`'s `finish`). This test
//! does not duplicate that; it *verifies* it, with
//! [`assert_no_leaked_clusters`] after each run, and best-effort deletes
//! anything it finds before failing — so a cleanup regression is
//! reported as the cleanup regression it is, rather than as a mysterious
//! failure of whichever test runs next.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

// ---------------------------------------------------------------------
// Locations
// ---------------------------------------------------------------------

/// The workspace root: two levels above this crate's own manifest
/// directory. Every path below is expressed relative to it, so this test
/// is independent of the directory `cargo test` happened to be invoked
/// from.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The canonical demo's lab configuration.
fn example_config() -> PathBuf {
    workspace_root().join("examples/kyverno-istio-upgrade/admissionlab.yaml")
}

/// A fresh, guaranteed-unique directory under the system temp directory.
fn unique_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-alpha-e2e-{}-{label}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create the run's report directory");
    dir
}

// ---------------------------------------------------------------------
// Expected identities
//
// A fixture's identifier is derived, deterministically, from its path
// relative to the lab file, its Kubernetes kind, its namespace, its
// name, and its index within its own file
// (`admissionlab_fixtures::identity::compute_fixture_id`). Spelling the
// three that matter out in full here — rather than searching for a
// substring — is what makes a rename of any of those inputs a loud
// failure in this file instead of a silent change to every report a user
// has ever archived.
// ---------------------------------------------------------------------

/// `examples/kyverno-istio-upgrade/fixtures/regression-pod-init-container.yaml`:
/// the pod carrying the demo's critical regression.
///
/// The `regression-` prefix is what keeps this fixture outside
/// `expectations.yaml`'s `fixtures-policy-*` globs; see that file.
const INIT_CONTAINER_FIXTURE: &str =
    "fixtures-regression-pod-init-container-yaml-pod-admissionlab-alpha-init-alpha-init-target-0";

/// `examples/kyverno-istio-upgrade/fixtures/policy-pod-image.yaml`.
const IMAGE_POD_FIXTURE: &str =
    "fixtures-policy-pod-image-yaml-pod-admissionlab-alpha-alpha-image-target-0";

/// `examples/kyverno-istio-upgrade/fixtures/policy-deployment.yaml`.
const DEPLOYMENT_FIXTURE: &str =
    "fixtures-policy-deployment-yaml-deployment-admissionlab-alpha-alpha-web-0";

/// `examples/kyverno-istio-upgrade/fixtures/policy-job.yaml`.
const JOB_FIXTURE: &str = "fixtures-policy-job-yaml-job-admissionlab-alpha-alpha-batch-0";

/// The init container the baseline stack injects and the candidate stack
/// does not — the `subject` of the demo's one critical finding.
const INJECTED_INIT_CONTAINER: &str = "alpha-audit-init";

/// The literal planted in `fixtures/pod-volume-env.yaml`'s
/// `ALPHA_DEMO_API_TOKEN`. It is a marker, never a credential (see that
/// fixture's own header); finding it in a written artifact means
/// redaction stopped running.
const REDACTION_SENTINEL: &str = "adlab-sentinel-never-real-8f31c2";

/// The unredacted environment value in the same container. Redaction has
/// to blank the credential-shaped entry *and* leave ordinary
/// configuration legible; a pass that blanked everything would satisfy
/// the sentinel check while destroying the report.
const LEGIBLE_ENV_VALUE: &str = "eu-west-1";

// ---------------------------------------------------------------------
// Fast checks — no cluster, no Docker, run by `cargo test --workspace`
// ---------------------------------------------------------------------

/// The example's own inputs, checked without provisioning anything.
///
/// Everything here is knowable in milliseconds and every one of these
/// failures would otherwise surface a quarter of an hour into a real
/// run: a renamed fixture file, a fixture moved to another namespace, a
/// glob that stopped matching, a misspelled key in `admissionlab.yaml`.
/// This is also what pins the three fixture identifiers the `#[ignore]`d
/// test below asserts against, so a rename cannot quietly desynchronize
/// the two.
#[test]
fn the_example_configuration_resolves_and_discovers_its_fixtures() {
    // `load_any_supported_lab`, not `load_lab`: as of Task 7.7 the
    // examples are `admissionlab.io/v1beta1` documents, and this is the
    // loader the binary itself uses -- the one that accepts every
    // supported version and migrates to the current model before
    // resolving.
    let lab = admissionlab_spec::load_any_supported_lab(&example_config()).expect(
        "the canonical example must load and resolve (paths, pinned versions, unique names)",
    );

    assert_eq!(lab.baseline.kubernetes, lab.candidate.kubernetes);
    assert_eq!(
        lab.baseline.components.len(),
        4,
        "baseline installs setup, Kyverno, this lab's policies, and Istio"
    );
    assert_eq!(lab.candidate.components.len(), 4);

    // Every component that serves admission has to have something to
    // wait for; see `admissionlab.yaml`'s own "Why every component
    // declares `readiness`" section. `alpha-setup` is the deliberate
    // exception: a Namespace and a ServiceAccount are ready when they
    // exist.
    for side in [&lab.baseline, &lab.candidate] {
        for component in &side.components {
            if component.name == "alpha-setup" {
                continue;
            }
            assert!(
                !component.readiness.is_empty(),
                "component {:?} installs an admission-path component with no readiness contract",
                component.name
            );
        }
    }

    // The policy escalation is what makes the expected-change path in
    // `expectations.yaml` load-bearing rather than decorative.
    assert!(
        lab.policy.fail_on.contains("image_changed"),
        "the example escalates image_changed so its expectation genuinely holds the verdict up"
    );

    let fixtures = admissionlab_fixtures::discover_fixtures(&lab.fixtures)
        .expect("the canonical corpus must discover cleanly");
    let ids: BTreeSet<&str> = fixtures.iter().map(|fixture| fixture.id.as_str()).collect();

    assert_eq!(
        fixtures.len(),
        10,
        "the canonical corpus is ten fixtures; found {ids:?}"
    );
    for expected in [
        INIT_CONTAINER_FIXTURE,
        IMAGE_POD_FIXTURE,
        DEPLOYMENT_FIXTURE,
        JOB_FIXTURE,
    ] {
        assert!(
            ids.contains(expected),
            "fixture identifier {expected:?} is not in the discovered set {ids:?}; \
             renaming a fixture file, its namespace, or its object name changes it"
        );
    }

    // The setup document is applied as a real component and must never
    // be replayed as a dry-run CREATE -- see
    // `fixtures/core/alpha-corpus/00-setup.yaml`. It lives outside the
    // lab file's own directory, so no `fixtures.include` glob can reach
    // it; this asserts that outcome rather than the mechanism.
    assert!(
        !ids.iter().any(|id| id.contains("namespace")),
        "no Namespace document may be discovered as a replayable fixture: {ids:?}"
    );
}

// ---------------------------------------------------------------------
// The real thing
// ---------------------------------------------------------------------

#[test]
#[ignore = "requires Docker and kind"]
fn alpha_regression_corpus_reports_a_deterministic_critical_regression() {
    let first = run_lab("run-1");
    assert_no_leaked_clusters("after the first run");
    let second = run_lab("run-2");
    assert_no_leaked_clusters("after the second run");

    assert_reports_the_known_regression(&first);
    assert_reports_the_expected_image_change(&first);
    assert_summary_buckets_are_exactly_as_designed(&first);
    assert_no_sentinel_secret_reached_the_artifacts(&first);
    assert_terminal_output_leads_with_the_critical_block(&first);

    // Global Constraint 7. Not "the two runs are byte-identical" -- they
    // cannot be, and should not be: run identifiers, cluster names,
    // durations and latencies all differ by construction. What must be
    // identical is the set of behavior changes the two runs claim, and
    // the buckets those changes were counted into.
    assert_eq!(
        change_signature(&second),
        change_signature(&first),
        "the semantic-change set must be identical across two runs of the same lab"
    );
    assert_eq!(
        second.result["summary"], first.result["summary"],
        "the five bucket counts must be identical across two runs of the same lab"
    );
}

/// One completed `admissionlab test` invocation's observable output.
struct LabRun {
    /// The process's exit code.
    exit_code: Option<i32>,
    /// Everything the run printed on stdout, including the rendered
    /// terminal report.
    stdout: String,
    /// `result.json`, parsed as generic JSON.
    result: Value,
    /// `result.json`'s raw text, for the redaction assertions -- a
    /// substring search over the bytes a user would actually archive,
    /// rather than over a structure this test chose to walk.
    result_text: String,
    /// `report.html`'s raw text, for the same reason.
    html_text: String,
}

/// Runs the canonical example once, end to end, through the compiled
/// binary.
fn run_lab(label: &str) -> LabRun {
    let report_dir = unique_temp_dir(label);
    let binary = cargo_bin_admissionlab();

    eprintln!("alpha_e2e[{label}]: running {} ...", binary.display());
    let started = std::time::Instant::now();
    let output = Command::new(&binary)
        .arg("test")
        .arg(example_config())
        .arg("--report-dir")
        .arg(&report_dir)
        // The terminal report's color decision belongs to the caller
        // that observed the stream (`TerminalOptions::for_stream`).
        // stdout is a pipe here so color would already be off; setting
        // this makes the plain-text assertions below independent of that
        // inference.
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to execute the admissionlab binary");
    let elapsed = started.elapsed();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    eprintln!(
        "alpha_e2e[{label}]: exit {:?} in {:.1}s",
        output.status.code(),
        elapsed.as_secs_f64()
    );
    eprintln!("{stdout}");
    if !stderr.is_empty() {
        eprintln!("--- stderr ---\n{stderr}");
    }

    // A run that fails before the comparison writes `diagnostics.json`
    // instead of `result.json` (`pipeline::report`'s own contract), and
    // that file names the stage and the failure -- surfacing it here is
    // the difference between "result.json missing" and an actionable
    // report of what actually went wrong.
    let result_path = report_dir.join("result.json");
    if !result_path.exists() {
        let diagnostics = std::fs::read_to_string(report_dir.join("diagnostics.json"))
            .unwrap_or_else(|_| "(no diagnostics.json either)".to_owned());
        panic!(
            "the run wrote no result.json to {}; exit {:?}\ndiagnostics: {diagnostics}\n\
             stderr: {stderr}",
            report_dir.display(),
            output.status.code(),
        );
    }

    let result_text = std::fs::read_to_string(&result_path).expect("read result.json");
    let result: Value = serde_json::from_str(&result_text).expect("result.json must be valid JSON");

    let html_path = report_dir.join("report.html");
    let html_text = std::fs::read_to_string(&html_path).unwrap_or_else(|error| {
        panic!(
            "the run must write a standalone {}: {error}",
            html_path.display()
        )
    });
    assert!(
        html_text.contains("<html") || html_text.contains("<!DOCTYPE"),
        "report.html must be a standalone HTML document"
    );

    LabRun {
        exit_code: output.status.code(),
        stdout,
        result,
        result_text,
        html_text,
    }
}

/// The compiled `admissionlab` binary this test drives.
///
/// `cargo` hands an integration test the path to its own test binary in
/// `CARGO_BIN_EXE_<name>`, which is the only reference that cannot
/// disagree with the profile and target directory this test was itself
/// built for.
fn cargo_bin_admissionlab() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_admissionlab"))
}

// ---------------------------------------------------------------------
// Assertions
// ---------------------------------------------------------------------

/// (a) The known critical semantic change, and (b) its observed first
/// divergence.
fn assert_reports_the_known_regression(run: &LabRun) {
    assert_eq!(
        run.exit_code,
        Some(1),
        "an unexpected critical change must exit 1 (RunDisposition::PolicyFailed)"
    );
    assert_eq!(
        run.result["policy"]["disposition"], "fail",
        "the run's own verdict must agree with its exit code"
    );

    let unexpected_criticals: Vec<&Value> = classified_changes(run)
        .filter(|change| change["severity"] == "critical" && change["expected"] == false)
        .collect();
    assert_eq!(
        unexpected_criticals.len(),
        1,
        "the demo is built around exactly one unaccounted-for critical change; got {unexpected_criticals:#?}"
    );

    let change = &unexpected_criticals[0]["change"];
    assert_eq!(change["kind"], "init_container_removed");
    assert_eq!(change["fixture_id"], INIT_CONTAINER_FIXTURE);
    assert_eq!(change["subject"], INJECTED_INIT_CONTAINER);
    // `container_removed`-shaped findings address the *baseline* object,
    // which is the only side the removed element exists on.
    let path = change["object_path"]
        .as_str()
        .expect("a container-list finding always carries an object path");
    assert!(
        path.starts_with("/spec/initContainers/"),
        "unexpected object path {path:?}"
    );

    // (b) The first divergence for that fixture, and its confidence.
    // Read from the fixture's own `admission` block rather than from the
    // change's `origin`: `diff_workload_objects` compares two normalized
    // objects and never sees a webhook trace, so it always reports
    // `origin: null` -- the trace-derived attribution lives here, and is
    // what the terminal renderer falls back to for exactly this reason.
    let fixture = fixture_entry(run, INIT_CONTAINER_FIXTURE);
    let divergence = &fixture["admission"]["firstDivergence"];
    assert_eq!(
        divergence["confidence"], "observed",
        "both sides' traces were captured, so the divergence must be observed rather than \
         inferred or unknown: {divergence:#?}"
    );
    let baseline_webhook = divergence["baseline_webhook"]
        .as_str()
        .expect("an observed divergence names the webhook on at least the baseline side");
    assert!(
        baseline_webhook.contains("kyverno"),
        "the divergence must name Kyverno's own mutating webhook, got {baseline_webhook:?}"
    );
    assert!(
        !divergence["explanation"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "an attribution with no explanation is not usable evidence"
    );
}

/// (c) The expected image change is present, marked expected, and still
/// carries the severity policy gave it.
fn assert_reports_the_expected_image_change(run: &LabRun) {
    let image_changes: Vec<&Value> = classified_changes(run)
        .filter(|change| change["change"]["kind"] == "image_changed")
        .collect();
    assert_eq!(
        image_changes.len(),
        3,
        "the pinned-image bump reaches all three image-pin fixtures -- and not the regression \
         fixture, which lives in its own namespace so that exactly one Kyverno rule can match it"
    );

    for classified in &image_changes {
        assert_eq!(
            classified["expected"], true,
            "every image change in this lab is accounted for by expectations.yaml: {classified:#?}"
        );
        // Escalated by `policy.failOn`, and *kept* escalated: an
        // expectation decides whether a change is counted toward the
        // verdict, never how it is graded. A report that downgraded it
        // to `info` on the way out would be hiding it.
        assert_eq!(
            classified["severity"], "critical",
            "an expected change keeps its real severity: {classified:#?}"
        );
        let change = &classified["change"];
        assert_eq!(change["subject"], "app");
        assert_eq!(change["baseline"], "registry.k8s.io/pause:3.9");
        assert_eq!(change["candidate"], "registry.k8s.io/pause:3.10");
    }

    let fixtures: BTreeSet<&str> = image_changes
        .iter()
        .filter_map(|classified| classified["change"]["fixture_id"].as_str())
        .collect();
    assert_eq!(
        fixtures,
        [IMAGE_POD_FIXTURE, DEPLOYMENT_FIXTURE, JOB_FIXTURE]
            .into_iter()
            .collect::<BTreeSet<&str>>()
    );

    // Every expectation matched something. A stale entry does not change
    // the verdict by design, so nothing else in this test would notice
    // one -- and an expectations file quietly accumulating entries that
    // stopped applying is exactly the drift the field exists to expose.
    assert_eq!(
        run.result["policy"]["staleExpectations"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "no expectation in the canonical example may be stale: {:#?}",
        run.result["policy"]["staleExpectations"]
    );
}

/// (d) The five bucket counts, exactly.
///
/// Asserted as a whole object rather than field by field: the point is
/// that there are no *stray* findings, and a per-field assertion on the
/// four counts this demo cares about would pass just as happily with a
/// warning nobody designed for sitting in the fifth.
fn assert_summary_buckets_are_exactly_as_designed(run: &LabRun) {
    let expected = serde_json::json!({
        // Six fixtures report no change at all (five in the policy-free
        // namespace, plus the mesh one, whose injected telemetry sidecar
        // and declining Istio injector are identical on both sides); the
        // three image-pin fixtures report only accounted-for ones; the
        // init-container pod is the single critical.
        "fixturesTotal": 10,
        "identical": 6,
        "expected": 3,
        "warnings": 0,
        "critical": 1,
        "inconclusive": 0
    });
    assert_eq!(
        run.result["summary"], expected,
        "unexpected bucket counts -- a nonzero `warnings` here is raw nondeterminism leaking \
         into findings, and a nonzero `inconclusive` is a fixture this lab could not replay"
    );
}

/// (e) No test sentinel secret reached either written artifact.
fn assert_no_sentinel_secret_reached_the_artifacts(run: &LabRun) {
    for (name, text) in [
        ("result.json", &run.result_text),
        ("report.html", &run.html_text),
        ("the terminal report", &run.stdout),
    ] {
        assert!(
            !text.contains(REDACTION_SENTINEL),
            "{name} contains the planted credential literal; redaction did not run on this path"
        );
    }

    // The redaction has to be a *redaction*, not an omission: the
    // variable is still named, its neighbour is still readable, and the
    // marker says a value was removed.
    assert!(
        run.result_text.contains("ALPHA_DEMO_API_TOKEN"),
        "the credential-shaped variable's name must survive; only its literal is blanked"
    );
    assert!(
        run.result_text.contains(LEGIBLE_ENV_VALUE),
        "an ordinary environment value must stay legible; redaction is targeted, not blanket"
    );
    assert!(
        run.result_text.contains("[REDACTED]"),
        "the report must say a value was removed rather than silently dropping it"
    );
}

/// The terminal output a user actually reads leads with the regression.
fn assert_terminal_output_leads_with_the_critical_block(run: &LabRun) {
    let critical_heading = run
        .stdout
        .find("Critical")
        .expect("the terminal report always prints a Critical section, even when it is empty");
    let warnings_heading = run
        .stdout
        .find("Warnings")
        .expect("the terminal report always prints a Warnings section");
    assert!(
        critical_heading < warnings_heading,
        "critical findings must be rendered before warnings"
    );

    let critical_block = &run.stdout[critical_heading..warnings_heading];
    for needle in [
        INIT_CONTAINER_FIXTURE,
        INJECTED_INIT_CONTAINER,
        "init_container_removed",
        "first divergence [observed]",
    ] {
        assert!(
            critical_block.contains(needle),
            "the critical block must contain {needle:?}; got:\n{critical_block}"
        );
    }
    assert!(
        run.stdout.contains("wrote "),
        "the run must say where it put its artifacts"
    );
}

// ---------------------------------------------------------------------
// Reading `result.json`
// ---------------------------------------------------------------------

/// Every graded change in the run, in the order policy put them.
fn classified_changes(run: &LabRun) -> impl Iterator<Item = &Value> {
    run.result["policy"]["changes"]
        .as_array()
        .expect("result.json always carries policy.changes")
        .iter()
}

/// One fixture's entry in `result.json`'s per-fixture list.
fn fixture_entry<'run>(run: &'run LabRun, fixture_id: &str) -> &'run Value {
    run.result["fixtures"]
        .as_array()
        .expect("result.json always carries a fixtures array")
        .iter()
        .find(|fixture| fixture["fixtureId"] == fixture_id)
        .unwrap_or_else(|| panic!("no fixture {fixture_id:?} in result.json"))
}

/// The run's semantic-change set, reduced to the fields that must not
/// vary between two runs of the same lab.
///
/// A `BTreeSet` of rendered tuples rather than the JSON array itself:
/// ordering within `policy.changes` is already deterministic, but this
/// test's claim is about the *set* of changes, and comparing sets makes
/// a failure read as "this change appeared / disappeared" rather than as
/// an opaque array mismatch. Run identifiers, cluster names, durations,
/// latencies and captured objects are all deliberately excluded -- they
/// differ between any two runs by construction, and demanding otherwise
/// would be demanding something false.
fn change_signature(run: &LabRun) -> BTreeSet<String> {
    classified_changes(run)
        .map(|classified| {
            let change = &classified["change"];
            format!(
                "{}|{}|{}|{}|{}|{}|{}|{}",
                change["fixture_id"],
                change["kind"],
                change["object_path"],
                change["subject"],
                change["baseline"],
                change["candidate"],
                classified["severity"],
                classified["expected"],
            )
        })
        .collect()
}

// ---------------------------------------------------------------------
// Cleanup verification
// ---------------------------------------------------------------------

/// Fails if any `adlab-*` cluster survived the run, after best-effort
/// deleting whatever it found.
///
/// `cluster_name` (`admissionlab_cluster::kind`) always produces the
/// `adlab-<side>-<short-run-id>` form, so this prefix is what this
/// project's clusters are, and nothing else on the machine is touched.
fn assert_no_leaked_clusters(context: &str) {
    let output = Command::new("kind")
        .arg("get")
        .arg("clusters")
        .output()
        .expect("failed to run `kind get clusters`");
    assert!(
        output.status.success(),
        // Distinguishing "kind is broken" from "kind reported zero
        // clusters" matters: a guard that cannot tell them apart is
        // incapable of failing in the one situation it exists for. Same
        // defect, and same fix, as `scripts/verify-cleanup.sh`'s own
        // `fetch_clusters`.
        "`kind get clusters` failed {context}: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let leaked: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| name.starts_with("adlab-"))
        .map(str::to_owned)
        .collect();
    for name in &leaked {
        let _ = Command::new("kind")
            .args(["delete", "cluster", "--name", name])
            .status();
    }
    assert!(
        leaked.is_empty(),
        "clusters left behind {context}: {leaked:?} (deleted by this test, but the binary \
         should have deleted them itself)"
    );
}

/// Compile-time proof that this file's own paths point at real files, so
/// a moved example fails `cargo test --workspace` rather than only the
/// `#[ignore]`d gate.
#[test]
fn the_example_lab_and_its_expectations_exist_on_disk() {
    for relative in [
        "examples/kyverno-istio-upgrade/admissionlab.yaml",
        "examples/kyverno-istio-upgrade/expectations.yaml",
        "examples/kyverno-istio-upgrade/stacks/baseline/policies.yaml",
        "examples/kyverno-istio-upgrade/stacks/candidate/policies.yaml",
        "fixtures/core/alpha-corpus/00-setup.yaml",
    ] {
        let path: &Path = &workspace_root().join(relative);
        assert!(path.exists(), "missing {relative}");
    }
}
