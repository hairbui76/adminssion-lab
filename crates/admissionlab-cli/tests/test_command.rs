//! `admissionlab test`'s pipeline, driven end to end against fakes.
//!
//! `tests/cli.rs` runs the compiled binary and proves it wires arguments,
//! streams, and exit codes to this pipeline. This file proves the
//! pipeline's own decisions — which failure maps to which exit code, what
//! is written before a failure gives up, when cleanup runs, what the
//! three reports contain — by substituting an
//! `admissionlab_cli::pipeline::LabBackend` whose clusters, installs, and
//! captures are in-memory. That is the only way to reach outcomes that
//! otherwise need two real API servers *disagreeing with each other*: a
//! policy failure, a `result.json` with a critical finding in it, a
//! cleanup that fails after an otherwise successful run.
//!
//! Every fake here follows the pattern `admissionlab-core`'s own
//! `tests/run_lifecycle.rs` established: a whole-trait test double with
//! configurable outcomes that records what it was actually asked to do,
//! and a hand-rolled current-thread runtime rather than `#[tokio::test]`.
//!
//! The configuration and the fixtures are **real**: each test writes an
//! `admissionlab.yaml` and one or more fixture documents to a temporary
//! directory and lets the pipeline load, resolve, validate, and discover
//! them for real. Only the three external systems are faked, so a fixture
//! identifier a test asserts on is the identifier discovery actually
//! computes — which is why the fake capture derives its scripted
//! outcomes from the fixtures it is handed rather than from hardcoded
//! identifiers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use admissionlab_admission::{AdmissionDecision, AdmissionOutcome, AdmissionTrace, TraceEvidence};
use admissionlab_cli::pipeline::{Console, LabBackend, OutcomeCapture, RunRequest, run_lab};
use admissionlab_core::{
    ArtifactStore, CapturedFixture, ClusterDiagnostics, ClusterError, ClusterHandle,
    ClusterManager, ClusterSpec, Diagnostic, DoctorReport, FixtureCapture, FixtureCaptureError,
    InstalledComponent, RunDisposition, RunId, RunPaths, Side, SideCapture, SideInstall,
    StackInstallError, StackInstaller, ToolName, ToolStatus,
};
use admissionlab_fixtures::FixtureSource;
use admissionlab_report::TerminalOptions;
use admissionlab_spec::ResolvedComponent;
use async_trait::async_trait;

// ---------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build test tokio runtime")
        .block_on(future)
}

/// A fresh, guaranteed-unique scratch directory. Mirrors
/// `admissionlab-core`'s own `tests/run_lifecycle.rs`.
fn unique_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-cli-pipeline-{label}-{}",
        RunId::generate().as_str()
    ));
    std::fs::create_dir_all(&dir).expect("failed to create scratch dir");
    dir
}

/// Writes a lab configuration whose `policy` section is `policy`
/// (already indented, or empty for none), plus one fixture, and returns
/// the configuration's path.
fn write_lab(dir: &Path, policy: &str) -> PathBuf {
    let config = dir.join("admissionlab.yaml");
    std::fs::write(
        &config,
        format!(
            "apiVersion: admissionlab.io/v1alpha1\n\
             kind: Lab\n\
             baseline:\n  kubernetes: \"1.36.4\"\n\
             candidate:\n  kubernetes: \"1.36.4\"\n\
             fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n\
             {policy}"
        ),
    )
    .expect("failed to write lab configuration");

    let fixtures = dir.join("fixtures");
    std::fs::create_dir_all(&fixtures).expect("failed to create fixtures dir");
    std::fs::write(
        fixtures.join("pod.yaml"),
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: probe\nspec:\n  containers:\n\
         \x20\x20\x20\x20- name: app\n      image: registry.k8s.io/pause:3.10\n",
    )
    .expect("failed to write fixture");
    config
}

/// What one run reported: its disposition plus both captured streams.
struct RunOutput {
    disposition: RunDisposition,
    stdout: String,
    stderr: String,
}

/// Drives one whole run against `backend`, capturing both streams
/// instead of writing to the process's own.
fn run(backend: &FakeBackend, request: &RunRequest<'_>) -> RunOutput {
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let disposition = {
        let mut console = Console {
            out: &mut out,
            err: &mut err,
            // Never colored: an assertion on report text should not
            // have to step over ANSI escapes.
            terminal: TerminalOptions::for_stream(false, true),
        };
        block_on(run_lab(backend, request, &mut console))
    };
    RunOutput {
        disposition,
        stdout: String::from_utf8(out).expect("stdout must be UTF-8"),
        stderr: String::from_utf8(err).expect("stderr must be UTF-8"),
    }
}

// ---------------------------------------------------------------------
// The fakes
// ---------------------------------------------------------------------

/// A `ClusterManager` that creates and deletes nothing, recording what
/// it was asked for.
struct FakeClusterManager {
    fail_delete: HashSet<Side>,
    created: Mutex<Vec<Side>>,
    deleted: Mutex<Vec<Side>>,
}

impl FakeClusterManager {
    fn new() -> Self {
        Self {
            fail_delete: HashSet::new(),
            created: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
        }
    }

    fn failing_delete(mut self) -> Self {
        self.fail_delete.insert(Side::Baseline);
        self
    }

    fn created_sides(&self) -> Vec<Side> {
        self.created.lock().expect("poisoned").clone()
    }

    fn deleted_sides(&self) -> Vec<Side> {
        self.deleted.lock().expect("poisoned").clone()
    }
}

#[async_trait]
impl ClusterManager for FakeClusterManager {
    async fn resolve_node_image(&self, version: &str) -> Result<String, ClusterError> {
        Ok(format!("kindest/node:v{version}"))
    }

    async fn create(
        &self,
        spec: &ClusterSpec,
        paths: &RunPaths,
    ) -> Result<ClusterHandle, ClusterError> {
        self.created.lock().expect("poisoned").push(spec.side);
        Ok(ClusterHandle {
            spec: spec.clone(),
            kubeconfig: paths
                .kubeconfigs()
                .join(format!("{}.yaml", spec.side.as_str())),
            audit_log: paths.logs().join(spec.side.as_str()).join("audit.log"),
        })
    }

    async fn delete(&self, handle: &ClusterHandle) -> Result<(), ClusterError> {
        let side = handle.spec.side;
        self.deleted.lock().expect("poisoned").push(side);
        if self.fail_delete.contains(&side) {
            return Err(ClusterError::InvalidKubeconfig {
                path: handle.kubeconfig.clone(),
                reason: "fake delete failure".to_owned(),
            });
        }
        Ok(())
    }

    async fn diagnostics(&self, handle: &ClusterHandle) -> ClusterDiagnostics {
        ClusterDiagnostics {
            cluster_name: handle.spec.name.clone(),
            cluster_exists: Some(false),
            kubeconfig_present: false,
            audit_log_present: false,
            notes: Vec::new(),
        }
    }
}

/// A `StackInstaller` that installs nothing.
struct FakeStackInstaller {
    /// When set, every install fails with this message.
    failure: Option<String>,
}

#[async_trait]
impl StackInstaller for FakeStackInstaller {
    async fn install_stack(
        &self,
        cluster: &ClusterHandle,
        components: &[ResolvedComponent],
        _component_timeout: Duration,
    ) -> Result<SideInstall, StackInstallError> {
        if let Some(message) = &self.failure {
            return Err(StackInstallError {
                component: components.first().map(|component| component.name.clone()),
                message: message.clone(),
            });
        }
        Ok(SideInstall {
            side: cluster.spec.side,
            components: components
                .iter()
                .map(|component| InstalledComponent {
                    name: component.name.clone(),
                    method: "fake".to_owned(),
                    resolved_version: component.version.clone(),
                    started_at: std::time::SystemTime::UNIX_EPOCH,
                    elapsed: Duration::from_millis(1),
                    diagnostics: Vec::new(),
                })
                .collect(),
        })
    }
}

/// What a [`FakeCapture`] pretends both API servers did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureBehavior {
    /// Both sides admit the fixture unchanged: no behavior difference.
    Identical,
    /// The candidate rejects what the baseline admitted — a
    /// `newly_denied`, which the Alpha default severity table grades
    /// critical.
    CandidateDenies,
    /// The candidate injects a sidecar — a `container_added`, graded
    /// `warning`, which is the case that must still exit 0.
    CandidateInjectsSidecar,
    /// The baseline captures normally and the candidate fails outright,
    /// carrying a diagnostic on the side that did work.
    CandidateFails,
}

/// A `FixtureCapture` that replays nothing, producing scripted outcomes
/// for whatever fixtures it was handed.
struct FakeCapture {
    fixtures: Vec<FixtureSource>,
    behavior: CaptureBehavior,
    outcomes: Mutex<Vec<AdmissionOutcome>>,
}

impl FakeCapture {
    fn outcome(&self, fixture: &FixtureSource, side: Side) -> AdmissionOutcome {
        let denied = self.behavior == CaptureBehavior::CandidateDenies && side == Side::Candidate;
        let sidecar =
            self.behavior == CaptureBehavior::CandidateInjectsSidecar && side == Side::Candidate;
        AdmissionOutcome {
            fixture_id: fixture.id.clone(),
            side,
            decision: if denied {
                AdmissionDecision::Rejected {
                    code: Some(403),
                    message: "denied by the candidate policy".to_owned(),
                }
            } else {
                AdmissionDecision::Accepted
            },
            warnings: Vec::new(),
            total_latency: Duration::from_millis(7),
            final_object: if denied {
                None
            } else {
                Some(admitted_pod(sidecar))
            },
            trace: AdmissionTrace {
                evidence: TraceEvidence::Observed,
                invocations: Vec::new(),
            },
            diagnostics: if side == Side::Baseline {
                vec![Diagnostic {
                    code: "admission.webhook_rejection_metric".to_owned(),
                    message: "kube-apiserver's rejection counter for webhook w rose by 1"
                        .to_owned(),
                    context: std::collections::BTreeMap::new(),
                }]
            } else {
                Vec::new()
            },
        }
    }
}

/// The object a fake API server "admitted", with an extra sidecar
/// container when `sidecar`.
fn admitted_pod(sidecar: bool) -> serde_json::Value {
    let mut containers = vec![serde_json::json!({
        "name": "app",
        "image": "registry.k8s.io/pause:3.10"
    })];
    if sidecar {
        containers.push(serde_json::json!({
            "name": "istio-proxy",
            "image": "docker.io/istio/proxyv2:1.99.0"
        }));
    }
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": "probe",
            // Server-generated and different on every cluster: the
            // built-in normalization rules must remove it, or every
            // fixture would report a difference.
            "uid": RunId::generate().as_str(),
        },
        "spec": {"containers": containers}
    })
}

#[async_trait]
impl FixtureCapture for FakeCapture {
    async fn capture_side(
        &self,
        _cluster: &ClusterHandle,
        side: Side,
        paths: &RunPaths,
    ) -> Result<SideCapture, FixtureCaptureError> {
        if self.behavior == CaptureBehavior::CandidateFails && side == Side::Candidate {
            return Err(FixtureCaptureError {
                fixture: self
                    .fixtures
                    .first()
                    .map(|fixture| fixture.id.as_str().to_owned()),
                message: "fake capture failure".to_owned(),
            });
        }

        let mut captured = Vec::new();
        for fixture in &self.fixtures {
            let outcome = self.outcome(fixture, side);
            let artifact_dir = paths.raw().join(side.as_str()).join(fixture.id.as_str());
            self.outcomes
                .lock()
                .expect("poisoned")
                .push(outcome.clone());
            captured.push(CapturedFixture {
                fixture_id: fixture.id.clone(),
                side,
                outcome_path: artifact_dir.join("outcome.json"),
                artifact_dir,
                diagnostics: outcome.diagnostics,
            });
        }
        Ok(SideCapture {
            side,
            fixtures: captured,
        })
    }
}

impl OutcomeCapture for FakeCapture {
    fn captured_outcomes(&self) -> Vec<AdmissionOutcome> {
        self.outcomes.lock().expect("poisoned").clone()
    }
}

/// The whole fake world one run is driven against.
struct FakeBackend {
    clusters: Arc<FakeClusterManager>,
    install_failure: Option<String>,
    capture_behavior: CaptureBehavior,
    prerequisites_met: bool,
}

impl FakeBackend {
    fn new(behavior: CaptureBehavior) -> Self {
        Self {
            clusters: Arc::new(FakeClusterManager::new()),
            install_failure: None,
            capture_behavior: behavior,
            prerequisites_met: true,
        }
    }

    fn with_clusters(mut self, clusters: FakeClusterManager) -> Self {
        self.clusters = Arc::new(clusters);
        self
    }

    fn failing_install(mut self) -> Self {
        self.install_failure = Some("fake install failure".to_owned());
        self
    }

    fn without_prerequisites(mut self) -> Self {
        self.prerequisites_met = false;
        self
    }
}

#[async_trait]
impl LabBackend for FakeBackend {
    type Clusters = FakeClusterManager;
    type Installer = FakeStackInstaller;
    type Capture = FakeCapture;

    async fn doctor_report(&self) -> DoctorReport {
        DoctorReport {
            tools: ToolName::ALL
                .iter()
                .map(|name| ToolStatus {
                    name: *name,
                    found: self.prerequisites_met,
                    version: self.prerequisites_met.then(|| "1.0.0".to_owned()),
                    diagnostic: (!self.prerequisites_met).then(|| "not found".to_owned()),
                })
                .collect(),
            docker_reachable: self.prerequisites_met,
            disk_warning: None,
        }
    }

    fn cluster_manager(&self) -> Arc<Self::Clusters> {
        Arc::clone(&self.clusters)
    }

    fn stack_installer(&self, _paths: &RunPaths) -> Self::Installer {
        FakeStackInstaller {
            failure: self.install_failure.clone(),
        }
    }

    fn fixture_capture(
        &self,
        fixtures: Vec<FixtureSource>,
        _store: ArtifactStore,
    ) -> Self::Capture {
        FakeCapture {
            fixtures,
            behavior: self.capture_behavior,
            outcomes: Mutex::new(Vec::new()),
        }
    }

    fn component_timeout(&self) -> Duration {
        Duration::from_millis(1)
    }
}

/// Reads and parses a written `result.json`.
///
/// Parsed as generic JSON rather than into `LabResult`: that type is
/// `Serialize`-only by design (several of the foreign types it carries
/// implement no `Deserialize`), so a typed reader does not exist to use.
fn read_result(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).expect("result.json must exist");
    serde_json::from_str(&text).expect("result.json must be valid JSON")
}

// ---------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------

#[test]
fn a_run_with_no_differences_passes_and_writes_all_three_reports() {
    let dir = unique_dir("happy");
    let config = write_lab(&dir, "");
    let reports = dir.join("artifacts");
    let backend = FakeBackend::new(CaptureBehavior::Identical);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::Passed, "{output:?}");
    // 1. The terminal report went to stdout, verdict and all.
    assert!(
        output.stdout.contains("Admission Lab result"),
        "stdout:\n{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("Result: pass"),
        "stdout:\n{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("Summary  1 fixtures"),
        "stdout:\n{}",
        output.stdout
    );

    // 2. `result.json` parses and describes this run.
    let result = read_result(&reports.join("result.json"));
    assert_eq!(
        result["schemaVersion"],
        serde_json::json!(admissionlab_report::SCHEMA_VERSION)
    );
    assert_eq!(result["summary"]["fixturesTotal"], serde_json::json!(1));
    assert_eq!(result["summary"]["identical"], serde_json::json!(1));
    assert_eq!(result["policy"]["disposition"], serde_json::json!("pass"));
    assert_eq!(
        result["environments"]["baseline"]["kubernetes"],
        serde_json::json!("1.36.4")
    );
    assert!(
        result["fixtures"][0]["admission"]["baseline"].is_object(),
        "both sides' captured evidence must reach the report: {result}"
    );

    // 3. The standalone HTML page exists and is self-contained.
    let html = std::fs::read_to_string(reports.join("report.html")).expect("report.html");
    assert!(html.contains("<html"), "report.html must be a page");
    assert!(
        !html.contains("<script src"),
        "the HTML report must never reference an external script"
    );

    // The metric-sourced evidence is surfaced as a diagnostic and never
    // as a fabricated finding (Global Constraint 15).
    let diagnostics = result["diagnostics"].as_array().expect("diagnostics array");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic["code"] == "admission.webhook_rejection_metric"),
        "expected the rejection-metric evidence to be surfaced: {result}"
    );
    assert!(
        result["policy"]["changes"]
            .as_array()
            .expect("changes array")
            .is_empty(),
        "metric evidence must never become a semantic change: {result}"
    );

    assert_eq!(backend.clusters.created_sides().len(), 2);
    assert_eq!(backend.clusters.deleted_sides().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reports_default_to_the_runs_own_directory_when_no_report_dir_is_given() {
    let dir = unique_dir("default-report-dir");
    let config = write_lab(&dir, "");
    let run_root = dir.join("runs");
    let backend = FakeBackend::new(CaptureBehavior::Identical);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: None,
        github_summary: None,
        run_root: run_root.clone(),
    };

    let output = run(&backend, &request);
    assert_eq!(output.disposition, RunDisposition::Passed);

    // `<run_root>/<run id>/reports/result.json`, and nowhere else.
    let written: Vec<PathBuf> = std::fs::read_dir(&run_root)
        .expect("run root must exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("reports").join("result.json"))
        .filter(|path| path.is_file())
        .collect();
    assert_eq!(
        written.len(),
        1,
        "expected exactly one run's reports under {}",
        run_root.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// Verdicts
// ---------------------------------------------------------------------

#[test]
fn a_critical_regression_fails_the_run_and_still_writes_the_reports() {
    let dir = unique_dir("policy-fail");
    let config = write_lab(&dir, "");
    let reports = dir.join("artifacts");
    let backend = FakeBackend::new(CaptureBehavior::CandidateDenies);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::PolicyFailed);
    assert!(
        output.stdout.contains("Result: fail"),
        "stdout:\n{}",
        output.stdout
    );

    let result = read_result(&reports.join("result.json"));
    assert_eq!(result["policy"]["disposition"], serde_json::json!("fail"));
    assert_eq!(result["summary"]["critical"], serde_json::json!(1));
    let changes = result["policy"]["changes"].as_array().expect("changes");
    assert_eq!(changes.len(), 1, "{result}");
    assert_eq!(
        changes[0]["change"]["kind"],
        serde_json::json!("newly_denied")
    );
    assert_eq!(changes[0]["severity"], serde_json::json!("critical"));
    assert_ne!(
        changes[0]["change"]["fixtureId"],
        serde_json::json!("unattributed"),
        "the caller must stamp the real fixture id: {result}"
    );

    // A failing verdict never skips cleanup.
    assert_eq!(backend.clusters.deleted_sides().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_warning_only_run_still_exits_passed_with_the_warning_in_the_report() {
    let dir = unique_dir("warn");
    let config = write_lab(&dir, "");
    let reports = dir.join("artifacts");
    let backend = FakeBackend::new(CaptureBehavior::CandidateInjectsSidecar);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    // ROADMAP §0.4 has no exit code for "completed with warnings", so a
    // warn is a pass — and the warning is not hidden by that.
    assert_eq!(output.disposition, RunDisposition::Passed);
    let result = read_result(&reports.join("result.json"));
    assert_eq!(result["policy"]["disposition"], serde_json::json!("warn"));
    assert_eq!(result["summary"]["warnings"], serde_json::json!(1));
    assert!(
        output.stdout.contains("container_added"),
        "the warning must be visible in the terminal report:\n{}",
        output.stdout
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fail_on_escalates_a_warning_into_a_failing_run() {
    let dir = unique_dir("fail-on");
    let config = write_lab(&dir, "policy:\n  failOn:\n    - container_added\n");
    let backend = FakeBackend::new(CaptureBehavior::CandidateInjectsSidecar);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: None,
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);
    assert_eq!(
        output.disposition,
        RunDisposition::PolicyFailed,
        "`policy.failOn` must reach the evaluation: {output:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// Input failures: exit 2, before any cluster exists
// ---------------------------------------------------------------------

#[test]
fn an_unreadable_configuration_exits_invalid_input_without_creating_a_cluster() {
    let dir = unique_dir("bad-config");
    let config = dir.join("missing.yaml");
    let backend = FakeBackend::new(CaptureBehavior::Identical);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: None,
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::InvalidInput);
    assert!(
        output.stderr.contains("failed to load lab configuration"),
        "stderr:\n{}",
        output.stderr
    );
    assert!(
        backend.clusters.created_sides().is_empty(),
        "a configuration that never loaded must not cost a cluster"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_semantic_kind_in_fail_on_exits_invalid_input_before_any_cluster() {
    let dir = unique_dir("unknown-kind");
    // `image_change` is the plausible near miss of the real
    // `image_changed`, which is exactly the typo
    // `admissionlab_policy::kind_from_name` refuses to guess at.
    let config = write_lab(&dir, "policy:\n  failOn:\n    - image_change\n");
    let backend = FakeBackend::new(CaptureBehavior::Identical);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: None,
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::InvalidInput);
    assert!(
        output.stderr.contains("unknown semantic change kind"),
        "stderr:\n{}",
        output.stderr
    );
    assert!(
        output.stderr.contains("policy.failOn"),
        "the message must locate the offending entry:\n{}",
        output.stderr
    );
    assert!(
        backend.clusters.created_sides().is_empty(),
        "an unknown kind must be caught before any cluster is created"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_host_missing_its_prerequisites_exits_invalid_input_before_any_cluster() {
    let dir = unique_dir("no-prereqs");
    let config = write_lab(&dir, "");
    let backend = FakeBackend::new(CaptureBehavior::Identical).without_prerequisites();
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: None,
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(
        output.disposition,
        RunDisposition::InvalidInput,
        "`test` must agree with `doctor`'s own answer for a bad host"
    );
    assert!(
        backend.clusters.created_sides().is_empty(),
        "the prerequisite gate must run before any cluster is created"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// Later-stage failures: artifacts first, then cleanup
// ---------------------------------------------------------------------

#[test]
fn an_install_failure_exits_four_writes_diagnostics_and_still_cleans_up() {
    let dir = unique_dir("install-fail");
    let config = write_lab(&dir, "");
    let reports = dir.join("artifacts");
    let backend = FakeBackend::new(CaptureBehavior::Identical).failing_install();
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::InstallationFailed);
    assert!(reports.join("diagnostics.json").is_file());
    assert!(!reports.join("result.json").exists());
    assert_eq!(backend.clusters.deleted_sides().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// The GitHub job summary (Task 5.4)
//
// The action that consumes this file `cat`s it unconditionally, so what
// matters here is that the file exists on every path with something to
// say -- a verdict when the run reached one, an honest "no result" naming
// the failed stage when it did not -- and that the second kind never
// contains the first kind's verdict.
// ---------------------------------------------------------------------

/// Reads a written job summary, failing with the directory listing when
/// it is not there (the whole point of the flag is that it always is).
fn read_summary(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("the job summary must exist at {}: {error}", path.display()))
}

#[test]
fn a_passing_run_writes_its_verdict_to_the_github_summary() {
    let dir = unique_dir("summary-pass");
    let config = write_lab(&dir, "");
    let reports = dir.join("artifacts");
    // Deliberately two levels below anything that exists: the flag
    // creates its parent directories rather than requiring a `mkdir`
    // step in the workflow.
    let summary = dir.join("summaries").join("github-summary.md");
    let backend = FakeBackend::new(CaptureBehavior::Identical);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: Some(&summary),
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);
    assert_eq!(output.disposition, RunDisposition::Passed, "{output:?}");

    let text = read_summary(&summary);
    assert!(
        text.starts_with("## Admission Lab: PASS"),
        "summary:\n{text}"
    );
    assert!(text.contains("| identical | 1 |"), "summary:\n{text}");
    // The summary is the same value the other artifacts render, so it
    // must never disagree with them about the verdict.
    let result = read_result(&reports.join("result.json"));
    assert_eq!(result["policy"]["disposition"], serde_json::json!("pass"));
    // And the run says where it put it, so a workflow author can find it
    // in the log.
    assert!(
        output.stdout.contains(&summary.display().to_string()),
        "stdout:\n{}",
        output.stdout
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_policy_failure_still_writes_the_github_summary() {
    let dir = unique_dir("summary-fail");
    let config = write_lab(&dir, "");
    let reports = dir.join("artifacts");
    let summary = reports.join("github-summary.md");
    let backend = FakeBackend::new(CaptureBehavior::CandidateDenies);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: Some(&summary),
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);
    assert_eq!(
        output.disposition,
        RunDisposition::PolicyFailed,
        "{output:?}"
    );

    let text = read_summary(&summary);
    assert!(
        text.starts_with("## Admission Lab: FAIL"),
        "summary:\n{text}"
    );
    assert!(
        text.contains("### Critical findings (1)"),
        "summary:\n{text}"
    );
    // Exit 1 is the case the action exists to make visible: the summary,
    // the JSON, and the HTML are all there for the upload step.
    assert!(reports.join("result.json").is_file());
    assert!(reports.join("report.html").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_run_that_never_reaches_a_verdict_writes_a_summary_naming_the_stage() {
    let dir = unique_dir("summary-install-fail");
    let config = write_lab(&dir, "");
    let reports = dir.join("artifacts");
    let summary = reports.join("github-summary.md");
    let backend = FakeBackend::new(CaptureBehavior::Identical).failing_install();
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: Some(&summary),
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);
    assert_eq!(
        output.disposition,
        RunDisposition::InstallationFailed,
        "{output:?}"
    );

    let text = read_summary(&summary);
    assert!(
        text.starts_with("## Admission Lab: NO RESULT"),
        "summary:\n{text}"
    );
    assert!(
        text.contains("`install`"),
        "the summary must name the stage that failed:\n{text}"
    );
    // Global Constraint 15, in the one place a reader is most likely to
    // skim: a run that compared nothing states no verdict at all.
    for verdict in ["PASS", "WARN", "FAIL"] {
        assert!(
            !text.contains(verdict),
            "a no-verdict summary must not contain {verdict}:\n{text}"
        );
    }
    // It matches what the machine-readable failure artifact says.
    assert!(reports.join("diagnostics.json").is_file());
    assert!(!reports.join("result.json").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_invalid_configuration_writes_a_summary_before_any_cluster_exists() {
    let dir = unique_dir("summary-invalid-input");
    let summary = dir.join("artifacts").join("github-summary.md");
    let backend = FakeBackend::new(CaptureBehavior::Identical);
    let request = RunRequest {
        config: &dir.join("missing.yaml"),
        keep_clusters: false,
        report_dir: None,
        github_summary: Some(&summary),
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);
    assert_eq!(
        output.disposition,
        RunDisposition::InvalidInput,
        "{output:?}"
    );

    let text = read_summary(&summary);
    assert!(text.contains("`configuration`"), "summary:\n{text}");
    assert!(text.contains("no pass/fail verdict"), "summary:\n{text}");
    // No run workspace exists to name, and the summary says so rather
    // than inventing a run id.
    assert!(text.contains("not started"), "summary:\n{text}");
    assert!(backend.clusters.created_sides().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The end-to-end half of Task 5.2, proved through the real pipeline
/// rather than by driving the writer by hand (which
/// `admissionlab-core`'s own `tests/run_manifest_failure.rs` does):
/// `run_lab` itself writes `run.json` before it creates a cluster,
/// advances it as stages pass, and leaves it valid — with
/// `completedAt: null` and the failed stage named — when the install
/// stage dies.
#[test]
fn a_failed_install_leaves_a_run_manifest_naming_the_stage() {
    let dir = unique_dir("manifest-install-fail");
    let config = write_lab(&dir, "");
    let run_root = dir.join("runs");
    let backend = FakeBackend::new(CaptureBehavior::Identical).failing_install();
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&dir.join("artifacts")),
        github_summary: None,
        run_root: run_root.clone(),
    };

    let output = run(&backend, &request);
    assert_eq!(output.disposition, RunDisposition::InstallationFailed);

    let manifest = read_result(&sole_run_manifest(&run_root));
    assert_eq!(manifest["status"], serde_json::json!("failed"));
    assert_eq!(manifest["stage"], serde_json::json!("installation"));
    assert_eq!(manifest["completedAt"], serde_json::Value::Null);
    assert_eq!(
        manifest["schemaVersion"],
        serde_json::json!(admissionlab_core::run_manifest::SCHEMA_VERSION)
    );

    // The provenance gathered before the failure survived it: the tool
    // versions the run was gated on, both sides' resolved images, and
    // every input digest.
    assert_eq!(manifest["tools"]["kind"], serde_json::json!("1.0.0"));
    assert_eq!(
        manifest["baseline"]["kubernetesVersion"],
        serde_json::json!("1.36.4")
    );
    assert_eq!(
        manifest["baseline"]["nodeImage"],
        serde_json::json!("kindest/node:v1.36.4")
    );
    assert_eq!(
        manifest["fixtureHashes"]
            .as_object()
            .expect("fixtureHashes is an object")
            .len(),
        1
    );
    for key in ["configSha256", "normalizationSha256", "policySha256"] {
        assert_eq!(
            manifest[key]
                .as_str()
                .unwrap_or_else(|| panic!("{key} must be a string"))
                .len(),
            64,
            "{key} must be a SHA-256 hex digest"
        );
    }
    // No expectations file was configured, so this is honestly absent
    // rather than an empty-string digest (Global Constraint 15).
    assert_eq!(manifest["expectationsSha256"], serde_json::Value::Null);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A run that reaches a verdict marks its manifest completed — including
/// one whose *policy* failed, because "completed" is not "passed".
#[test]
fn a_completed_run_marks_its_manifest_completed_even_when_the_policy_failed() {
    let dir = unique_dir("manifest-complete");
    let config = write_lab(&dir, "");
    let run_root = dir.join("runs");
    let backend = FakeBackend::new(CaptureBehavior::CandidateDenies);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: None,
        github_summary: None,
        run_root: run_root.clone(),
    };

    let output = run(&backend, &request);
    assert_eq!(output.disposition, RunDisposition::PolicyFailed);

    let manifest = read_result(&sole_run_manifest(&run_root));
    assert_eq!(manifest["status"], serde_json::json!("completed"));
    assert_eq!(manifest["stage"], serde_json::json!("completed"));
    assert!(
        manifest["completedAt"].is_string(),
        "a completed run records a real completion time, got {:?}",
        manifest["completedAt"]
    );
    // The install stage re-recorded what was actually installed; the
    // minimal configuration installs nothing, so both sides are empty.
    assert_eq!(manifest["baseline"]["components"], serde_json::json!([]));

    let _ = std::fs::remove_dir_all(&dir);
}

/// `<run_root>/<run id>/run.json`, asserting there is exactly one run
/// under `run_root` so a test can never silently read a stale one.
fn sole_run_manifest(run_root: &Path) -> PathBuf {
    let mut manifests: Vec<PathBuf> = std::fs::read_dir(run_root)
        .expect("run root must exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("run.json"))
        .filter(|path| path.is_file())
        .collect();
    assert_eq!(
        manifests.len(),
        1,
        "expected exactly one run manifest under {}",
        run_root.display()
    );
    manifests.remove(0)
}

#[test]
fn a_capture_failure_exits_five_writes_what_it_knows_and_still_cleans_up() {
    let dir = unique_dir("capture-fail");
    let config = write_lab(&dir, "");
    let reports = dir.join("artifacts");
    let backend = FakeBackend::new(CaptureBehavior::CandidateFails);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::FixtureFailed);

    // ROADMAP step 3: the diagnostics that *were* collected land on
    // disk, naming the stage that failed, before cleanup runs.
    let artifact = read_result(&reports.join("diagnostics.json"));
    assert_eq!(artifact["stage"], serde_json::json!("capture"));
    assert!(
        artifact["failure"]
            .as_str()
            .is_some_and(|failure| failure.contains("fake capture failure")),
        "the artifact must carry the failure itself: {artifact}"
    );
    let diagnostics = artifact["diagnostics"].as_array().expect("diagnostics");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic["code"] == "admission.webhook_rejection_metric"),
        "the side that did capture keeps its evidence: {artifact}"
    );
    assert!(
        !reports.join("result.json").exists(),
        "a run that never compared both sides must claim no verdict"
    );

    // ROADMAP step 4.
    assert_eq!(backend.clusters.deleted_sides().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// Cleanup
// ---------------------------------------------------------------------

#[test]
fn keep_clusters_skips_cleanup_and_prints_the_exact_delete_commands() {
    let dir = unique_dir("keep");
    let config = write_lab(&dir, "");
    let backend = FakeBackend::new(CaptureBehavior::Identical);
    let request = RunRequest {
        config: &config,
        keep_clusters: true,
        report_dir: None,
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::Passed);
    assert!(
        backend.clusters.deleted_sides().is_empty(),
        "--keep-clusters must never delete a cluster"
    );
    assert!(
        output
            .stdout
            .contains("kind delete cluster --name adlab-baseline-"),
        "stdout:\n{}",
        output.stdout
    );
    assert!(
        output
            .stdout
            .contains("kind delete cluster --name adlab-candidate-"),
        "stdout:\n{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("kubeconfig:"),
        "stdout:\n{}",
        output.stdout
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cleanup_failure_after_a_passing_run_reports_infrastructure_but_keeps_the_reports() {
    let dir = unique_dir("cleanup-fail");
    let config = write_lab(&dir, "");
    let reports = dir.join("artifacts");
    let backend = FakeBackend::new(CaptureBehavior::Identical)
        .with_clusters(FakeClusterManager::new().failing_delete());
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    // A machine left with a cluster running has not completed cleanly,
    // so this can never be a success code — but the comparison that did
    // happen is still fully reported.
    assert_eq!(output.disposition, RunDisposition::InfrastructureFailed);
    assert!(
        output.stderr.contains("kind delete cluster --name"),
        "the exact manual recovery command must be printed:\n{}",
        output.stderr
    );
    let result = read_result(&reports.join("result.json"));
    assert_eq!(
        result["policy"]["disposition"],
        serde_json::json!("pass"),
        "the run's own verdict is preserved in the report it already wrote"
    );
    assert!(reports.join("report.html").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cleanup_failure_never_masks_a_policy_failure() {
    let dir = unique_dir("cleanup-fail-policy");
    let config = write_lab(&dir, "");
    let backend = FakeBackend::new(CaptureBehavior::CandidateDenies)
        .with_clusters(FakeClusterManager::new().failing_delete());
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: None,
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(
        output.disposition,
        RunDisposition::PolicyFailed,
        "the regression is the more specific and more actionable finding; the leaked cluster is \
         reported loudly on stderr either way"
    );
    assert!(output.stderr.contains("kind delete cluster --name"));
    let _ = std::fs::remove_dir_all(&dir);
}

impl std::fmt::Debug for RunOutput {
    /// Prints both streams, because every failing assertion in this file
    /// is easier to diagnose with the run's own output attached than
    /// with the disposition alone.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.disposition, self.stdout, self.stderr
        )
    }
}
