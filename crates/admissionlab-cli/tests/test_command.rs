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

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use admissionlab_admission::{AdmissionDecision, AdmissionOutcome, AdmissionTrace, TraceEvidence};
use admissionlab_cli::pipeline::{
    Console, GatewaySuiteError, GatewaySuiteRunner, LabBackend, MigrationRunOutcome,
    MigrationSuiteError, MigrationSuiteRunner, OutcomeCapture, RunRequest, SideGatewayOutcome,
    run_lab,
};
use admissionlab_core::{
    ArtifactStore, CapturedFixture, ClusterDiagnostics, ClusterError, ClusterHandle,
    ClusterManager, ClusterSpec, Diagnostic, DoctorReport, FixtureCapture, FixtureCaptureError,
    InstalledComponent, RunDisposition, RunId, RunPaths, Side, SideCapture, SideInstall,
    StackInstallError, StackInstaller, ToolName, ToolStatus,
};
use admissionlab_fixtures::FixtureSource;
use admissionlab_gateway::{
    CONDITION_ACCEPTED, CONDITION_PROGRAMMED, CONDITION_RESOLVED_REFS, ConditionState,
    GatewayCaseResult, GatewayClassEvidence, GatewayEvidence, GatewayIdentity, HttpProbeResult,
    MigrationBehaviorChange, MigrationBehaviorKind, MigrationComparability, ObservedCondition,
    ParentIdentity, ReconciliationEvidence, RouteEvidence, RouteParentStatus,
};
use admissionlab_policy::Severity;
use admissionlab_report::{GradedMigrationChange, MigrationCaseComparison, TerminalOptions};
use admissionlab_spec::{GatewaySuiteSpec, ResolvedComponent};
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
                    context: BTreeMap::new(),
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

// ---------------------------------------------------------------------
// The Gateway fake (ROADMAP Task 6.11)
// ---------------------------------------------------------------------

/// What a [`FakeGatewaySuite`] pretends both implementations did with
/// the lab's one route contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayBehavior {
    /// Both sides reconcile identically and both probes answer 200 from
    /// the same backend: no Gateway behavior difference at all.
    Identical,
    /// The candidate's route converges with `ResolvedRefs: False` --
    /// exactly what removing a `ReferenceGrant` produces -- so its probe
    /// is skipped, and the baseline's answered probe has no counterpart.
    CandidateLosesResolvedRefs,
    /// The candidate never converges. Its evidence is current but
    /// unsettled, which is `GatewayComparability::Partial`: the pair is
    /// counted inconclusive rather than compared.
    CandidateNeverConverges,
    /// The suite itself fails on the candidate side.
    CandidateFails,
}

/// The lab configuration every Gateway test below writes.
const GATEWAY_CONTRACT: &str = "echo-route";
const GATEWAY_NAMESPACE: &str = "lab";
const GATEWAY_NAME: &str = "lab-gateway";
const GATEWAY_LISTENER: &str = "http";
const GATEWAY_BACKEND: &str = "echo-a";

/// A `GatewaySuiteRunner` that applies nothing and observes nothing,
/// producing scripted evidence for the suite it was handed.
struct FakeGatewaySuite {
    suite: GatewaySuiteSpec,
    behavior: GatewayBehavior,
    /// Which sides were actually run, so a test can prove both were.
    /// Shared with the backend that built this, because `run_lab` builds
    /// the runner itself and a test never holds the value directly.
    ran: Arc<Mutex<Vec<Side>>>,
}

impl FakeGatewaySuite {
    /// One condition, as a controller would publish it.
    fn condition(type_name: &str, state: ConditionState, reason: &str) -> ObservedCondition {
        ObservedCondition {
            type_name: type_name.to_owned(),
            state,
            reason: Some(reason.to_owned()),
            // Current for the object's own generation below: a stale one
            // would make every absence stop being evidence, which is a
            // different case than any of these behaviors is about.
            observed_generation: Some(1),
        }
    }

    /// The scripted case for one side.
    fn case(&self, side: Side) -> GatewayCaseResult {
        let candidate = side == Side::Candidate;
        let loses_refs = candidate && self.behavior == GatewayBehavior::CandidateLosesResolvedRefs;
        let unconverged = candidate && self.behavior == GatewayBehavior::CandidateNeverConverges;

        let (resolved_state, resolved_reason) = if loses_refs {
            (ConditionState::False, "RefNotPermitted")
        } else {
            (ConditionState::True, "ResolvedRefs")
        };
        let parent = RouteParentStatus {
            parent: ParentIdentity {
                namespace: Some(GATEWAY_NAMESPACE.to_owned()),
                name: GATEWAY_NAME.to_owned(),
                section_name: Some(GATEWAY_LISTENER.to_owned()),
            },
            controller_name: Some("istio.io/gateway-controller".to_owned()),
            conditions: [
                (
                    CONDITION_ACCEPTED.to_owned(),
                    Self::condition(CONDITION_ACCEPTED, ConditionState::True, "Accepted"),
                ),
                (
                    CONDITION_RESOLVED_REFS.to_owned(),
                    Self::condition(CONDITION_RESOLVED_REFS, resolved_state, resolved_reason),
                ),
            ]
            .into_iter()
            .collect(),
        };
        let reconciliation = ReconciliationEvidence {
            gateway_class: Some(GatewayClassEvidence {
                name: "istio".to_owned(),
                accepted: Self::condition(CONDITION_ACCEPTED, ConditionState::True, "Accepted"),
            }),
            gateway: GatewayEvidence {
                identity: GatewayIdentity {
                    namespace: GATEWAY_NAMESPACE.to_owned(),
                    name: GATEWAY_NAME.to_owned(),
                },
                conditions: [CONDITION_ACCEPTED, CONDITION_PROGRAMMED]
                    .into_iter()
                    .map(|type_name| {
                        (
                            type_name.to_owned(),
                            Self::condition(type_name, ConditionState::True, "Programmed"),
                        )
                    })
                    .collect(),
                generation: 1,
                gateway_class_name: Some("istio".to_owned()),
            },
            route: RouteEvidence {
                namespace: GATEWAY_NAMESPACE.to_owned(),
                name: GATEWAY_CONTRACT.to_owned(),
                generation: 1,
                parents: vec![parent],
            },
            elapsed: Duration::from_millis(300),
            converged: !unconverged,
            diagnostics: Vec::new(),
        };

        // The production runner's own rule, reproduced by construction:
        // a probe is sent only for a route that is actually carrying
        // traffic. A side that lost `ResolvedRefs`, and a side that
        // never converged, answer none.
        let probes = if loses_refs || unconverged {
            Vec::new()
        } else {
            vec![HttpProbeResult {
                status: 200,
                backend: Some(GATEWAY_BACKEND.to_owned()),
                response_headers: BTreeMap::new(),
                response_body_sha256: "0".repeat(64),
                elapsed: Duration::from_millis(12),
                attempts: 1,
            }]
        };

        GatewayCaseResult {
            contract_id: self.suite.routes[0].id.clone(),
            reconciliation,
            probes,
        }
    }
}

#[async_trait]
impl GatewaySuiteRunner for FakeGatewaySuite {
    async fn run_side(
        &self,
        _cluster: &ClusterHandle,
        side: Side,
        _paths: &RunPaths,
    ) -> Result<SideGatewayOutcome, GatewaySuiteError> {
        self.ran.lock().expect("poisoned").push(side);
        if self.behavior == GatewayBehavior::CandidateFails && side == Side::Candidate {
            return Err(GatewaySuiteError {
                contract: Some(self.suite.routes[0].id.clone()),
                message: "fake gateway suite failure".to_owned(),
            });
        }
        let case = self.case(side);
        // The skip diagnostic the production runner raises, reproduced
        // in the same shape so the assertions below exercise the real
        // rendering path rather than a test-only one.
        let diagnostics = if case.probes.is_empty() && !self.suite.routes[0].probes.is_empty() {
            vec![Diagnostic {
                code: "gateway.probe_skipped".to_owned(),
                message: format!(
                    "no traffic probe was sent for route contract {:?} on the {side} side: \
                     HTTPRoute lab/echo-route published ResolvedRefs=False (RefNotPermitted)",
                    self.suite.routes[0].id,
                ),
                context: BTreeMap::new(),
            }]
        } else {
            Vec::new()
        };
        Ok(SideGatewayOutcome {
            side,
            cases: vec![case],
            diagnostics,
        })
    }
}

/// The whole fake world one run is driven against.
struct FakeBackend {
    clusters: Arc<FakeClusterManager>,
    install_failure: Option<String>,
    capture_behavior: CaptureBehavior,
    gateway_behavior: GatewayBehavior,
    gateway_runs: Arc<Mutex<Vec<Side>>>,
    migration_behavior: MigrationBehavior,
    prerequisites_met: bool,
}

impl FakeBackend {
    fn new(behavior: CaptureBehavior) -> Self {
        Self {
            clusters: Arc::new(FakeClusterManager::new()),
            install_failure: None,
            capture_behavior: behavior,
            gateway_behavior: GatewayBehavior::Identical,
            gateway_runs: Arc::new(Mutex::new(Vec::new())),
            migration_behavior: MigrationBehavior::Preserved,
            prerequisites_met: true,
        }
    }

    fn with_gateway(mut self, behavior: GatewayBehavior) -> Self {
        self.gateway_behavior = behavior;
        self
    }

    fn with_migration(mut self, behavior: MigrationBehavior) -> Self {
        self.migration_behavior = behavior;
        self
    }

    /// Which sides the Gateway suite was actually run against.
    fn gateway_sides(&self) -> Vec<Side> {
        self.gateway_runs.lock().expect("poisoned").clone()
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
    type Gateway = FakeGatewaySuite;
    type Migration = FakeMigrationSuite;

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

    fn gateway_suite(&self, suite: GatewaySuiteSpec, _store: ArtifactStore) -> Self::Gateway {
        FakeGatewaySuite {
            suite,
            behavior: self.gateway_behavior,
            ran: Arc::clone(&self.gateway_runs),
        }
    }

    fn migration_suite(
        &self,
        suite: admissionlab_spec::MigrationSuiteSpec,
        _store: ArtifactStore,
    ) -> Self::Migration {
        FakeMigrationSuite {
            suite,
            behavior: self.migration_behavior,
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

// ---------------------------------------------------------------------
// The Gateway suite (ROADMAP Task 6.11)
// ---------------------------------------------------------------------

/// Writes a lab configuration carrying a `gateway:` section with one
/// route contract and one probe, plus the same admission fixture
/// [`write_lab`] writes.
///
/// The manifest path need not exist: the fake suite runner applies
/// nothing, and `resolve_lab` validates the *shape* of a suite rather
/// than the presence of its files (the real applier is what reports a
/// missing manifest, and it does so against a real cluster).
fn write_gateway_lab(dir: &Path) -> PathBuf {
    let config = write_lab(dir, "");
    let text = std::fs::read_to_string(&config).expect("the lab must have been written");
    let gateway = format!(
        r#"gateway:
  manifests:
    - gateway/suite.yaml
  gatewayEndpoint:
    type: serviceByName
    namespace: "{{gatewayNamespace}}"
    name: "{{gatewayName}}-istio"
    portName: http
  routes:
    - id: {GATEWAY_CONTRACT}
      gatewayNamespace: {GATEWAY_NAMESPACE}
      gatewayName: {GATEWAY_NAME}
      routeNamespace: {GATEWAY_NAMESPACE}
      routeName: {GATEWAY_CONTRACT}
      listenerName: {GATEWAY_LISTENER}
      probes:
        - host: echo.admissionlab.test
          path: /
          method: GET
          expectedStatus: 200
          expectedBackend: {GATEWAY_BACKEND}
"#
    );
    std::fs::write(&config, format!("{text}{gateway}"))
        .expect("failed to write the gateway lab configuration");
    config
}

/// One run against the gateway fake, with the report written where the
/// caller can read it.
fn run_gateway(dir: &Path, backend: &FakeBackend) -> (RunOutput, serde_json::Value) {
    let config = write_gateway_lab(dir);
    let reports = dir.join("reports");
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };
    let output = run(backend, &request);
    let result = read_result(&reports.join("result.json"));
    (output, result)
}

/// The `fixtures` entry for the one route contract, or `None`.
fn gateway_entry(result: &serde_json::Value) -> Option<&serde_json::Value> {
    result["fixtures"]
        .as_array()
        .expect("fixtures must be an array")
        .iter()
        .find(|entry| entry["fixtureId"] == GATEWAY_CONTRACT)
}

#[test]
fn a_configured_gateway_suite_runs_on_both_sides_and_reaches_the_report() {
    let dir = unique_dir("gateway-both-sides");
    let backend = FakeBackend::new(CaptureBehavior::Identical);
    let (output, result) = run_gateway(&dir, &backend);

    assert_eq!(
        backend.gateway_sides(),
        vec![Side::Baseline, Side::Candidate],
        "a configured suite must be run against both sides, or the two are not comparable"
    );
    assert_eq!(output.disposition, RunDisposition::Passed, "{output:?}");

    let entry = gateway_entry(&result).expect("the route contract must be reported");
    assert!(
        entry["admission"].is_null(),
        "a route contract carries Gateway evidence and no admission evidence"
    );
    // The frozen v1beta1 document splits a route contract's evidence
    // into its two sections (ROADMAP Task 7.2).
    assert_eq!(
        entry["gatewayReconciliation"]["contractId"],
        GATEWAY_CONTRACT
    );
    assert_eq!(entry["traffic"]["contractId"], GATEWAY_CONTRACT);
    assert_eq!(
        entry["traffic"]["pairs"][0]["baseline"]["backend"], GATEWAY_BACKEND,
        "the probe evidence must reach the report verbatim"
    );
    assert!(
        entry["changes"]
            .as_array()
            .expect("changes must be an array")
            .is_empty(),
        "two identical sides produce no Gateway change"
    );

    // The run's own counting: one admission fixture plus one route
    // contract, both identical.
    assert_eq!(result["summary"]["fixturesTotal"], 2);
    assert_eq!(result["summary"]["identical"], 2);
    assert_eq!(result["summary"]["inconclusive"], 0);
    assert!(
        output.stdout.contains("Gateway  1 route contract(s)"),
        "the terminal report must carry a Gateway section: {output:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_candidate_that_loses_resolved_refs_fails_the_run_and_reports_its_skipped_probe() {
    let dir = unique_dir("gateway-resolved-refs");
    let backend = FakeBackend::new(CaptureBehavior::Identical)
        .with_gateway(GatewayBehavior::CandidateLosesResolvedRefs);
    let (output, result) = run_gateway(&dir, &backend);

    assert_eq!(
        output.disposition,
        RunDisposition::PolicyFailed,
        "a backend that stopped resolving is critical in the default table: {output:?}"
    );

    let entry = gateway_entry(&result).expect("the route contract must be reported");
    let kinds: Vec<&str> = entry["changes"]
        .as_array()
        .expect("changes must be an array")
        .iter()
        .map(|classified| classified["change"]["kind"].as_str().expect("a kind"))
        .collect();
    assert!(
        kinds.contains(&"backend_resolution_changed"),
        "the product-level claim must be made: {kinds:?}"
    );
    assert!(
        kinds.contains(&"resolved_refs_condition_changed"),
        "the condition evidence must be claimed beside it: {kinds:?}"
    );
    assert!(
        kinds.contains(&"traffic_status_changed"),
        "the baseline answered a probe the candidate did not; that is a traffic claim, not          silence: {kinds:?}"
    );

    // The reason travels as evidence and is rendered, but nothing
    // *depends* on its wording: the assertion above is on the condition
    // kind, this one only proves the reason reached a reader.
    let resolved = entry["changes"]
        .as_array()
        .expect("changes must be an array")
        .iter()
        .find(|classified| classified["change"]["kind"] == "resolved_refs_condition_changed")
        .expect("the condition change must exist");
    assert_eq!(
        resolved["change"]["candidate"]["reason"], "RefNotPermitted",
        "the controller's own reason is carried in the payload"
    );
    assert_eq!(resolved["severity"], "critical");

    assert!(
        result["diagnostics"]
            .as_array()
            .expect("diagnostics must be an array")
            .iter()
            .any(|diagnostic| diagnostic["code"] == "gateway.probe_skipped"),
        "a skipped probe is evidence, and must be recorded as such"
    );
    assert!(
        output.stdout.contains("traffic: no probe was sent"),
        "the terminal report must say a probe was skipped rather than showing nothing: {output:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_candidate_that_never_converges_is_counted_inconclusive_rather_than_identical() {
    let dir = unique_dir("gateway-unconverged");
    let backend = FakeBackend::new(CaptureBehavior::Identical)
        .with_gateway(GatewayBehavior::CandidateNeverConverges);
    let (output, result) = run_gateway(&dir, &backend);

    assert_eq!(
        result["summary"]["inconclusive"], 1,
        "only one side converged, so an empty change list would not mean the sides agreed:          {output:?}"
    );
    assert_eq!(result["summary"]["identical"], 1, "the admission fixture");
    assert!(
        output
            .stdout
            .contains("only one side converged (baseline converged, candidate unconverged"),
        "the comparability answer must be surfaced at report altitude: {output:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_gateway_suite_failure_ends_the_run_as_a_fixture_failure() {
    let dir = unique_dir("gateway-suite-fails");
    let config = write_gateway_lab(&dir);
    let reports = dir.join("reports");
    let backend =
        FakeBackend::new(CaptureBehavior::Identical).with_gateway(GatewayBehavior::CandidateFails);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(
        output.disposition,
        RunDisposition::FixtureFailed,
        "{output:?}"
    );
    assert!(
        !reports.join("result.json").exists(),
        "a run that never compared both sides has earned no verdict to write"
    );
    assert!(
        reports.join("diagnostics.json").exists(),
        "what it did observe is still written"
    );
    assert_eq!(
        backend.gateway_sides().len(),
        2,
        "one side failing must never abandon the other's in-flight suite"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_lab_with_no_gateway_section_produces_no_gateway_output_at_all() {
    let dir = unique_dir("gateway-absent");
    let config = write_lab(&dir, "");
    let reports = dir.join("reports");
    let backend = FakeBackend::new(CaptureBehavior::Identical);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);
    let result = read_result(&reports.join("result.json"));

    assert!(
        backend.gateway_sides().is_empty(),
        "no `gateway:` section means the suite is never constructed, let alone run"
    );
    assert_eq!(result["summary"]["fixturesTotal"], 1);
    for entry in result["fixtures"].as_array().expect("an array") {
        assert!(
            entry["gatewayReconciliation"].is_null(),
            "nothing may fabricate a Gateway section: {entry}"
        );
    }
    assert!(
        !output.stdout.contains("Gateway"),
        "an admission-only run must print no Gateway heading at all: {output:?}"
    );
    assert!(
        result["timings"].get("gatewaySuite").is_none(),
        "a stage that never ran is absent, never zero"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// Certified compatibility matrix (Task 7.4 step 3)
//
// The rule under test is a warning that must never become a refusal:
// `compatibility/recipes.yaml` certifies Kyverno 3.9.0 on Kubernetes
// 1.35.8 only (Kyverno's own documented window stops at 1.35), so a lab
// that installs it on Admission Lab's Tier-1 primary 1.36.4 is a
// supported Kubernetes version carrying an uncertified recipe
// combination -- exactly the case the roadmap asks to warn about.
//
// Fake-driven, like every other test in this file: no cluster is
// created and nothing is installed, because the check happens before
// either could be. What the run itself does is irrelevant to it, which
// is why these assert the run still reaches its ordinary verdict.
// ---------------------------------------------------------------------

/// Writes a lab whose two sides each install one component, given
/// verbatim as YAML already indented to sit under `components:`.
///
/// Separate from [`write_lab`] rather than a parameter on it: that
/// helper appends to the end of the document, and a component has to go
/// inside an environment.
fn write_lab_with_component(dir: &Path, kubernetes: &str, component: &str) -> PathBuf {
    let config = dir.join("admissionlab.yaml");
    let environment = format!("  kubernetes: \"{kubernetes}\"\n  components:\n{component}");
    std::fs::write(
        &config,
        format!(
            "apiVersion: admissionlab.io/v1alpha1\n\
             kind: Lab\n\
             baseline:\n{environment}\
             candidate:\n{environment}\
             fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n"
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

/// One Helm component, indented to sit under `components:`.
fn helm_component(name: &str, chart: &str, repo: &str, version: &str) -> String {
    format!(
        "    - name: {name}\n      \
         install:\n        \
         type: helm\n        \
         chart: {chart}\n        \
         repo: {repo}\n        \
         version: \"{version}\"\n        \
         namespace: {name}\n"
    )
}

#[test]
fn an_uncertified_recipe_combination_warns_and_still_runs_to_a_verdict() {
    let dir = unique_dir("uncertified");
    let config = write_lab_with_component(
        &dir,
        // Supported (it is the Tier-1 primary), which is what makes this
        // a certification question at all rather than a cluster-creation
        // refusal.
        "1.36.4",
        &helm_component(
            "kyverno",
            "kyverno/kyverno",
            "https://kyverno.github.io/kyverno/",
            "3.9.0",
        ),
    );
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

    // 1. The run is not refused. Global Constraint 6: a user-defined
    //    stack is first-class, and this one still reached a verdict.
    assert_eq!(output.disposition, RunDisposition::Passed, "{output:?}");
    assert_eq!(backend.clusters.created_sides().len(), 2);

    // 2. It said so on the console, once -- not once per side.
    let warnings = output
        .stderr
        .matches("which Admission Lab does not certify")
        .count();
    assert_eq!(warnings, 1, "expected exactly one warning: {output:?}");
    assert!(
        output.stderr.contains("kyverno 3.9.0 on Kubernetes 1.36.4"),
        "the warning must name the combination: {output:?}"
    );
    assert!(
        output
            .stderr
            .contains("Certified: kyverno 3.9.0 on Kubernetes 1.35.8"),
        "the warning must name what IS certified: {output:?}"
    );

    // 3. And it reached the report as a run-level diagnostic.
    let result = read_result(&reports.join("result.json"));
    let diagnostics = result["diagnostics"].as_array().expect("diagnostics array");
    let uncertified: Vec<&serde_json::Value> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "compatibility.uncertified_combination")
        .collect();
    assert_eq!(uncertified.len(), 1, "{result}");
    let context = &uncertified[0]["context"];
    assert_eq!(context["recipe"], serde_json::json!("kyverno"));
    assert_eq!(context["recipeVersion"], serde_json::json!("3.9.0"));
    assert_eq!(context["kubernetes"], serde_json::json!("1.36.4"));
    assert_eq!(
        context["sides"],
        serde_json::json!("baseline, candidate"),
        "both sides asked for it, and one diagnostic says so: {result}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_certified_combination_warns_about_nothing() {
    let dir = unique_dir("certified");
    let config = write_lab_with_component(
        &dir,
        "1.35.8",
        &helm_component(
            "kyverno",
            "kyverno/kyverno",
            "https://kyverno.github.io/kyverno/",
            "3.9.0",
        ),
    );
    let backend = FakeBackend::new(CaptureBehavior::Identical);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: None,
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::Passed, "{output:?}");
    assert!(
        !output.stderr.contains("does not certify"),
        "the certified combination must be silent: {output:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_user_defined_stack_admission_lab_ships_no_recipe_for_is_silent() {
    let dir = unique_dir("user-defined");
    let config = write_lab_with_component(
        &dir,
        "1.36.4",
        &helm_component(
            "my-webhook",
            "internal/my-webhook",
            "https://charts.example.invalid",
            "0.4.2",
        ),
    );
    let backend = FakeBackend::new(CaptureBehavior::Identical);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: None,
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::Passed, "{output:?}");
    assert!(
        !output.stderr.contains("does not certify"),
        "a stack Admission Lab certifies nothing for is not a certification question at all, \
         and warning about every such component would make the warning worthless: {output:?}"
    );
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

// ---------------------------------------------------------------------
// The Ingress-to-Gateway migration suite (ROADMAP Task 8.8)
// ---------------------------------------------------------------------

/// The migration case `write_migration_lab` declares.
const MIGRATION_CASE: &str = "legacy-echo";

/// What the fake migration suite scripts.
///
/// Both arms are shapes `admissionlab_gateway::compare_migration_case`
/// really produces; what the fake replaces is the two clusters, not the
/// comparator's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationBehavior {
    /// The migration preserved every behavior the case contracts, and
    /// its one non-portable feature was declared in writing.
    Preserved,
    /// The candidate's route reached a different backend, undeclared --
    /// the regression `examples/ingress-to-gateway/` demonstrates for
    /// real.
    BackendRegression,
    /// The suite could not be run at all.
    Fails,
}

/// A `MigrationSuiteRunner` that touches no cluster and returns scripted
/// comparisons for the suite it was handed.
struct FakeMigrationSuite {
    suite: admissionlab_spec::MigrationSuiteSpec,
    behavior: MigrationBehavior,
}

#[async_trait]
impl MigrationSuiteRunner for FakeMigrationSuite {
    async fn run(
        &self,
        _baseline: &ClusterHandle,
        _candidate: &ClusterHandle,
        _paths: &RunPaths,
    ) -> Result<MigrationRunOutcome, MigrationSuiteError> {
        if self.behavior == MigrationBehavior::Fails {
            return Err(MigrationSuiteError {
                case: Some(MIGRATION_CASE.to_owned()),
                message: "fake migration failure".to_owned(),
            });
        }

        let case = self
            .suite
            .cases
            .first()
            .expect("the lab declares one migration case");
        // Always present, always declared, always `info`: the shape a
        // real run produces for an `expectedNonportable` entry the
        // baseline manifests really carry.
        let mut changes = vec![GradedMigrationChange {
            change: MigrationBehaviorChange {
                kind: MigrationBehaviorKind::NonPortableFeature,
                detail: "nginx.ingress.kubernetes.io/limit-rps on Ingress lab/echo-ingress has \
                         no portable Gateway API equivalent"
                    .to_owned(),
                expected: true,
            },
            severity: Severity::Info,
        }];
        if self.behavior == MigrationBehavior::BackendRegression {
            changes.push(GradedMigrationChange {
                change: MigrationBehaviorChange {
                    kind: MigrationBehaviorKind::BackendChanged,
                    detail: "probe 0 (GET http://echo.admissionlab.test/reports): the Ingress \
                             reached backend \"echo-b\" and the Gateway reached \"echo-a\""
                        .to_owned(),
                    expected: false,
                },
                severity: Severity::Critical,
            });
        }

        Ok(MigrationRunOutcome {
            cases: vec![MigrationCaseComparison {
                case_id: case.id.clone(),
                comparability: MigrationComparability::Comparable,
                changes,
                probes: Vec::new(),
                unmatched_expectations: Vec::new(),
            }],
            diagnostics: Vec::new(),
        })
    }
}

/// A `v1beta1` lab declaring one migration case with both per-side
/// endpoint blocks.
///
/// `v1beta1` and not `v1alpha1`: `migration:` is a v1beta1-only section
/// (an Alpha document always resolves to `migration: None`), so a lab
/// written at the older version would exercise nothing.
fn write_migration_lab(dir: &Path) -> PathBuf {
    write_migration_lab_with_sides(dir, MIGRATION_BOTH_SIDES)
}

/// Both per-side endpoint blocks, as a runnable suite declares them.
const MIGRATION_BOTH_SIDES: &str = r#"  baseline:
    gatewayEndpoint:
      type: serviceByName
      namespace: ingress-nginx-legacy
      name: ingress-nginx-legacy-controller
      portName: http
  candidate:
    gatewayEndpoint:
      type: serviceBySelector
      namespace: "{gatewayNamespace}"
      selector:
        gateway.networking.k8s.io/gateway-name: "{gatewayName}"
"#;

/// The candidate block alone -- a suite that could probe only one side.
const MIGRATION_CANDIDATE_ONLY: &str = r#"  candidate:
    gatewayEndpoint:
      type: serviceBySelector
      namespace: "{gatewayNamespace}"
      selector:
        gateway.networking.k8s.io/gateway-name: "{gatewayName}"
"#;

/// [`write_migration_lab`] with the per-side endpoint blocks supplied.
fn write_migration_lab_with_sides(dir: &Path, sides: &str) -> PathBuf {
    let config = write_lab(dir, "");
    let text = std::fs::read_to_string(&config)
        .expect("the lab must have been written")
        .replace("admissionlab.io/v1alpha1", "admissionlab.io/v1beta1");
    let migration = format!(
        r"migration:
{sides}  cases:
    - id: {MIGRATION_CASE}
      baselineIngressManifests:
        - migration/ingress.yaml
      candidateGatewayManifests:
        - migration/gateway.yaml
      probes:
        - host: echo.admissionlab.test
          path: /reports
          method: GET
          expectedStatus: 200
          expectedBackend: echo-b
      expectedNonportable:
        - feature: nginx.ingress.kubernetes.io/limit-rps
          reason: rate limiting moves to the platform's edge proxy
"
    );
    std::fs::write(&config, format!("{text}{migration}"))
        .expect("failed to write the migration lab configuration");
    config
}

/// A migration whose candidate reproduces the baseline's behavior passes
/// and still reports what it accounted for.
#[test]
fn a_preserved_migration_passes_and_still_reports_its_declared_nonportability() {
    let dir = unique_dir("migration-preserved");
    let config = write_migration_lab(&dir);
    let reports = dir.join("reports");
    let backend =
        FakeBackend::new(CaptureBehavior::Identical).with_migration(MigrationBehavior::Preserved);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::Passed, "{output:?}");
    let result = read_result(&reports.join("result.json"));
    assert_eq!(result["policy"]["disposition"], "pass");
    let case = &result["migration"][0];
    assert_eq!(case["caseId"], MIGRATION_CASE);
    assert_eq!(case["changes"][0]["kind"], "non_portable_feature");
    assert_eq!(
        case["changes"][0]["severity"], "info",
        "a declared non-portability is visible and accounted for, and must not warn: {case}"
    );
    assert!(
        output
            .stdout
            .contains("Migration  1 Ingress-to-Gateway case(s)"),
        "the terminal report names the suite even when nothing regressed:\n{output:?}"
    );
}

/// An undeclared traffic regression fails the run, and the report
/// explains it with the two observed backends.
///
/// This is the unit-level twin of `tests/migration_demo.rs`, which
/// proves the same three facts against two real clusters. Having both is
/// deliberate: this one runs in milliseconds on every commit and pins
/// the *wiring* (the verdict join, the document section, the terminal
/// section), while the demo proves the wiring is fed by real traffic.
#[test]
fn an_undeclared_migration_regression_fails_the_run_and_names_the_observed_backends() {
    let dir = unique_dir("migration-regression");
    let config = write_migration_lab(&dir);
    let reports = dir.join("reports");
    let backend = FakeBackend::new(CaptureBehavior::Identical)
        .with_migration(MigrationBehavior::BackendRegression);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(
        output.disposition,
        RunDisposition::PolicyFailed,
        "a migration behavior change no expectation accounts for must fail the run: {output:?}"
    );
    let result = read_result(&reports.join("result.json"));
    assert_eq!(
        result["policy"]["disposition"], "fail",
        "the run's verdict is the join of the policy verdict and the migration verdict"
    );
    assert!(
        result["policy"]["changes"]
            .as_array()
            .expect("changes is an array")
            .is_empty(),
        "and it is a join, not a rewrite: no migration finding is smuggled into the graded \
         SemanticChange list: {result}"
    );

    let regression = &result["migration"][0]["changes"][1];
    assert_eq!(regression["kind"], "backend_changed");
    assert_eq!(regression["severity"], "critical");
    assert_eq!(regression["expected"], false);
    let detail = regression["detail"].as_str().expect("a detail");
    assert!(
        detail.contains("echo-b") && detail.contains("echo-a"),
        "ROADMAP Task 8.8 step 2: the report explains the observed traffic difference, not an \
         annotation mismatch: {detail}"
    );
    assert!(
        output.stdout.contains("backend_changed") && output.stdout.contains("echo-b"),
        "the terminal report a human reads carries the same evidence:\n{output:?}"
    );
}

/// A migration suite that cannot be run ends the run with no verdict.
#[test]
fn a_migration_suite_failure_ends_the_run_as_a_fixture_failure() {
    let dir = unique_dir("migration-fails");
    let config = write_migration_lab(&dir);
    let reports = dir.join("reports");
    let backend =
        FakeBackend::new(CaptureBehavior::Identical).with_migration(MigrationBehavior::Fails);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(
        output.disposition,
        RunDisposition::FixtureFailed,
        "{output:?}"
    );
    assert!(
        !reports.join("result.json").exists(),
        "a run that could not observe a migration has earned no verdict to write"
    );
    assert!(reports.join("diagnostics.json").exists());
}

/// A migration suite with no per-side endpoint block is refused before
/// any cluster exists.
///
/// The check that `admissionlab-spec` deliberately cannot make: the
/// fields are optional in the schema so older documents stay valid, and
/// a suite that omits one is refused here instead. Exit 2, and the
/// cluster manager is never called.
#[test]
fn a_migration_suite_with_no_endpoint_is_refused_before_any_cluster_exists() {
    let dir = unique_dir("migration-no-endpoint");
    let config = write_migration_lab_with_sides(&dir, MIGRATION_CANDIDATE_ONLY);
    let reports = dir.join("reports");
    let backend = FakeBackend::new(CaptureBehavior::Identical);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(
        output.disposition,
        RunDisposition::InvalidInput,
        "{output:?}"
    );
    assert!(
        output.stderr.contains("migration.baseline.gatewayEndpoint"),
        "the message names the field that is missing:\n{output:?}"
    );
    assert!(
        backend.clusters.created_sides().is_empty(),
        "nothing may be provisioned for a configuration that cannot produce evidence"
    );
}
