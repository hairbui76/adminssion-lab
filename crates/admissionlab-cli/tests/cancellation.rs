//! Interrupting a run: what it stops, what it still writes, and what it
//! leaves behind (ROADMAP Task 9.6).
//!
//! Two halves, and they answer different questions.
//!
//! The **fake-driven** half drives `pipeline::run_lab` against in-memory
//! cluster/install/capture backends, the same way `tests/test_command.rs`
//! does, and pins the decisions: a cancellation observed during install
//! must stop the run before capture starts, a cancellation during capture
//! must stop it before the comparison, every one of those paths must
//! still write a `diagnostics.json` and must never write a `result.json`,
//! and cleanup must run either way. None of that needs Docker, and all of
//! it is deterministic — the fake stages request the cancellation
//! themselves, at the exact moment a real signal would have arrived, so
//! there is no sleep and no race anywhere in this half.
//!
//! The **real** half is `#[ignore]`d and needs Docker, `kind`, and a few
//! minutes:
//!
//! ```bash
//! cargo test -p admissionlab-cli --test cancellation -- \
//!     --ignored --test-threads=1 --nocapture
//! ```
//!
//! `--test-threads=1` is not optional there: each of these tests owns the
//! host's `kind` state — it starts by asserting no `adlab-*` cluster
//! exists and ends by asserting the same — so running two at once would
//! have each of them watching the other's clusters.
//!
//! It sends real `SIGINT`s to the real compiled binary running a real
//! lab, because the one claim the fakes cannot make is the one that
//! matters most: that pressing Ctrl-C on a run with two `kind` clusters
//! up leaves no cluster behind. It drives the interrupt off the binary's
//! own progress output rather than off a sleep, so "during cluster setup"
//! and "after install" are decided by what the run has actually printed.
//!
//! # The double-interrupt test asserts a leak, on purpose
//!
//! A second interrupt is the operator saying they are not waiting for
//! teardown, and `std::process::exit` runs no destructor — so clusters
//! *do* survive it. That is the contract, not a defect, and the thing
//! Admission Lab owes them there is the exact `kind delete cluster`
//! commands on stderr before it goes. So the test asserts those lines,
//! then sweeps with `scripts/verify-cleanup.sh --after-interrupt`, which
//! exists for exactly this and reports what it removed.

use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use admissionlab_admission::{AdmissionDecision, AdmissionOutcome, AdmissionTrace, TraceEvidence};
use admissionlab_cli::pipeline::{
    Console, GatewaySuiteError, GatewaySuiteRunner, LabBackend, MigrationRunOutcome,
    MigrationSuiteError, MigrationSuiteRunner, OutcomeCapture, RunRequest, SideGatewayOutcome,
    run_lab,
};
use admissionlab_core::{
    ArtifactStore, CancelSignal, Cancellation, CapturedFixture, ClusterDiagnostics, ClusterError,
    ClusterHandle, ClusterManager, ClusterSpec, DoctorReport, FixtureCapture, FixtureCaptureError,
    InstalledComponent, RunDisposition, RunId, RunPaths, Side, SideCapture, SideInstall,
    StackInstallError, StackInstaller, ToolName, ToolStatus,
};
use admissionlab_fixtures::FixtureSource;
use admissionlab_report::TerminalOptions;
use admissionlab_spec::{GatewaySuiteSpec, MigrationSuiteSpec, ResolvedComponent};
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

/// A fresh, guaranteed-unique scratch directory.
fn unique_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-cancellation-{label}-{}",
        RunId::generate().as_str()
    ));
    std::fs::create_dir_all(&dir).expect("failed to create scratch dir");
    dir
}

/// Writes a minimal real lab configuration and one real fixture, and
/// returns the configuration's path. Only the external systems are
/// faked; discovery, resolution and validation all run for real.
fn write_lab(dir: &Path) -> PathBuf {
    let config = dir.join("admissionlab.yaml");
    std::fs::write(
        &config,
        "apiVersion: admissionlab.io/v1\n\
         kind: Lab\n\
         baseline:\n  kubernetes: \"1.36.4\"\n\
         candidate:\n  kubernetes: \"1.36.4\"\n\
         fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n",
    )
    .expect("failed to write lab configuration");

    let fixtures = dir.join("fixtures");
    std::fs::create_dir_all(&fixtures).expect("failed to create fixtures dir");
    std::fs::write(
        fixtures.join("configmap.yaml"),
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: probe\ndata:\n  key: value\n",
    )
    .expect("failed to write fixture");
    config
}

/// What one run reported.
struct RunOutput {
    disposition: RunDisposition,
    stderr: String,
}

/// Drives one whole run against `backend`, capturing both streams.
fn run(backend: &FakeBackend, request: &RunRequest<'_>) -> RunOutput {
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let disposition = {
        let mut console = Console {
            out: &mut out,
            err: &mut err,
            terminal: TerminalOptions::for_stream(false, true),
        };
        block_on(run_lab(backend, request, &mut console))
    };
    RunOutput {
        disposition,
        stderr: String::from_utf8(err).expect("stderr must be UTF-8"),
    }
}

// ---------------------------------------------------------------------
// The fakes
// ---------------------------------------------------------------------

/// A `ClusterManager` that creates and deletes nothing, recording what it
/// was asked for.
struct FakeClusterManager {
    created: Mutex<Vec<Side>>,
    deleted: Mutex<Vec<Side>>,
    /// When set, both deletes fail — the case where a canceled run's
    /// registered recovery commands must *stay* registered.
    fail_delete: bool,
}

impl FakeClusterManager {
    fn new() -> Self {
        Self {
            created: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
            fail_delete: false,
        }
    }

    fn failing_delete() -> Self {
        Self {
            fail_delete: true,
            ..Self::new()
        }
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
        self.deleted
            .lock()
            .expect("poisoned")
            .push(handle.spec.side);
        if self.fail_delete {
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

/// A `StackInstaller` that installs nothing, and — when it was given a
/// handle — asks the run to stop while it is doing so.
///
/// Requesting the cancellation from *inside* the stage is what makes
/// these tests a faithful model of a signal: the flag is set while the
/// install is in flight, exactly where a Ctrl-C would land, and the
/// install still returns normally rather than being aborted.
struct FakeStackInstaller {
    cancel_here: Option<Cancellation>,
    installed: Arc<Mutex<Vec<Side>>>,
}

#[async_trait]
impl StackInstaller for FakeStackInstaller {
    async fn install_stack(
        &self,
        cluster: &ClusterHandle,
        components: &[ResolvedComponent],
        _component_timeout: Duration,
    ) -> Result<SideInstall, StackInstallError> {
        self.installed
            .lock()
            .expect("poisoned")
            .push(cluster.spec.side);
        if let Some(cancellation) = &self.cancel_here {
            // The request count is the signal watch's business
            // (`admissionlab_cli::cancel`); a stage only sets the flag.
            let _ = cancellation.request(CancelSignal::Interrupt);
        }
        Ok(SideInstall {
            side: cluster.spec.side,
            components: components
                .iter()
                .map(|component| InstalledComponent {
                    name: component.name.clone(),
                    method: "fake".to_owned(),
                    resolved_version: component.version.clone(),
                    started_at: SystemTime::UNIX_EPOCH,
                    elapsed: Duration::from_millis(1),
                    diagnostics: Vec::new(),
                })
                .collect(),
        })
    }
}

/// A `FixtureCapture` that replays nothing, recording which sides it was
/// asked for and — when it was given a handle — asking the run to stop
/// mid-capture.
struct FakeCapture {
    fixtures: Vec<FixtureSource>,
    captured_sides: Arc<Mutex<Vec<Side>>>,
    cancel_here: Option<Cancellation>,
    outcomes: Mutex<Vec<AdmissionOutcome>>,
}

#[async_trait]
impl FixtureCapture for FakeCapture {
    async fn capture_side(
        &self,
        _cluster: &ClusterHandle,
        side: Side,
        paths: &RunPaths,
    ) -> Result<SideCapture, FixtureCaptureError> {
        self.captured_sides.lock().expect("poisoned").push(side);
        if let Some(cancellation) = &self.cancel_here {
            let _ = cancellation.request(CancelSignal::Interrupt);
        }
        let mut captured = Vec::new();
        for fixture in &self.fixtures {
            let outcome = AdmissionOutcome {
                fixture_id: fixture.id.clone(),
                side,
                decision: AdmissionDecision::Accepted,
                warnings: Vec::new(),
                total_latency: Duration::from_millis(3),
                final_object: Some(serde_json::json!({
                    "apiVersion": "v1",
                    "kind": "ConfigMap",
                    "metadata": {"name": "probe"},
                    "data": {"key": "value"},
                })),
                trace: AdmissionTrace {
                    evidence: TraceEvidence::Observed,
                    invocations: Vec::new(),
                },
                diagnostics: Vec::new(),
            };
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
                diagnostics: Vec::new(),
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

/// Never constructed: the labs below declare no `gateway:` section, so
/// `run_lab` never asks for one. It exists because `LabBackend` requires
/// the associated type, and a type that cannot run is a more honest
/// stand-in than a second fake with scripted behavior nothing reads.
struct UnusedGatewaySuite;

#[async_trait]
impl GatewaySuiteRunner for UnusedGatewaySuite {
    async fn run_side(
        &self,
        _cluster: &ClusterHandle,
        _side: Side,
        _paths: &RunPaths,
    ) -> Result<SideGatewayOutcome, GatewaySuiteError> {
        unreachable!("no lab in this file declares a gateway suite")
    }
}

/// Never constructed, for the same reason as [`UnusedGatewaySuite`].
struct UnusedMigrationSuite;

#[async_trait]
impl MigrationSuiteRunner for UnusedMigrationSuite {
    async fn run(
        &self,
        _baseline: &ClusterHandle,
        _candidate: &ClusterHandle,
        _paths: &RunPaths,
    ) -> Result<MigrationRunOutcome, MigrationSuiteError> {
        unreachable!("no lab in this file declares a migration suite")
    }
}

/// Which stage asks the run to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancelAt {
    /// Nobody interrupts this run.
    Never,
    /// Before `run_lab` is even entered — a signal that arrives while
    /// the configuration is still being read.
    BeforeTheRun,
    /// While both stacks are installing.
    Install,
    /// While both sides' fixtures are being captured.
    Capture,
}

/// The whole fake world one run is driven against.
struct FakeBackend {
    clusters: Arc<FakeClusterManager>,
    cancellation: Cancellation,
    cancel_at: CancelAt,
    installed_sides: Arc<Mutex<Vec<Side>>>,
    captured_sides: Arc<Mutex<Vec<Side>>>,
}

impl FakeBackend {
    fn new(cancel_at: CancelAt) -> Self {
        let cancellation = Cancellation::new();
        if cancel_at == CancelAt::BeforeTheRun {
            assert_eq!(
                cancellation.request(CancelSignal::Interrupt),
                1,
                "the first request is the cooperative one"
            );
        }
        Self {
            clusters: Arc::new(FakeClusterManager::new()),
            cancellation,
            cancel_at,
            installed_sides: Arc::new(Mutex::new(Vec::new())),
            captured_sides: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_clusters(mut self, clusters: FakeClusterManager) -> Self {
        self.clusters = Arc::new(clusters);
        self
    }

    /// The handle to give a stage that should ask the run to stop.
    fn trigger_for(&self, stage: CancelAt) -> Option<Cancellation> {
        (self.cancel_at == stage).then(|| self.cancellation.clone())
    }

    fn installed_sides(&self) -> Vec<Side> {
        self.installed_sides.lock().expect("poisoned").clone()
    }

    fn captured_sides(&self) -> Vec<Side> {
        self.captured_sides.lock().expect("poisoned").clone()
    }
}

#[async_trait]
impl LabBackend for FakeBackend {
    type Clusters = FakeClusterManager;
    type Installer = FakeStackInstaller;
    type Capture = FakeCapture;
    type Gateway = UnusedGatewaySuite;
    type Migration = UnusedMigrationSuite;

    async fn doctor_report(&self) -> DoctorReport {
        DoctorReport {
            tools: ToolName::ALL
                .iter()
                .map(|name| ToolStatus {
                    name: *name,
                    found: true,
                    version: Some("1.0.0".to_owned()),
                    diagnostic: None,
                })
                .collect(),
            docker_reachable: true,
            disk_warning: None,
        }
    }

    fn cluster_manager(&self) -> Arc<Self::Clusters> {
        Arc::clone(&self.clusters)
    }

    fn stack_installer(&self, _paths: &RunPaths) -> Self::Installer {
        FakeStackInstaller {
            cancel_here: self.trigger_for(CancelAt::Install),
            installed: Arc::clone(&self.installed_sides),
        }
    }

    fn gateway_suite(&self, _suite: GatewaySuiteSpec, _store: ArtifactStore) -> Self::Gateway {
        UnusedGatewaySuite
    }

    fn migration_suite(
        &self,
        _suite: MigrationSuiteSpec,
        _store: ArtifactStore,
    ) -> Self::Migration {
        UnusedMigrationSuite
    }

    fn fixture_capture(
        &self,
        fixtures: Vec<FixtureSource>,
        _store: ArtifactStore,
    ) -> Self::Capture {
        FakeCapture {
            fixtures,
            captured_sides: Arc::clone(&self.captured_sides),
            cancel_here: self.trigger_for(CancelAt::Capture),
            outcomes: Mutex::new(Vec::new()),
        }
    }

    fn component_timeout(&self) -> Duration {
        Duration::from_millis(1)
    }

    fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }
}

/// Reads the `diagnostics.json` a canceled run wrote, failing with the
/// directory listing when it is not there.
fn read_diagnostics(report_dir: &Path) -> serde_json::Value {
    let path = report_dir.join("diagnostics.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        let listing: Vec<String> = std::fs::read_dir(report_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        panic!(
            "a canceled run must write {}: {error} (directory holds {listing:?})",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("diagnostics.json must be valid JSON")
}

// ---------------------------------------------------------------------
// The fake-driven half
// ---------------------------------------------------------------------

/// A cancellation observed while the stacks install stops the run at the
/// next boundary: capture never starts, the diagnostics artifact names
/// the interruption, no `result.json` is written, and both clusters are
/// still deleted.
#[test]
fn a_cancellation_during_install_stops_before_capture_and_still_cleans_up() {
    let dir = unique_dir("install");
    let config = write_lab(&dir);
    let reports = dir.join("reports");
    let backend = FakeBackend::new(CancelAt::Install);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::InfrastructureFailed);
    assert_eq!(
        backend.installed_sides().len(),
        2,
        "the install already in flight runs to completion on both sides"
    );
    assert!(
        backend.captured_sides().is_empty(),
        "no capture may start after the run was asked to stop"
    );
    assert_eq!(
        backend.clusters.deleted_sides().len(),
        2,
        "a canceled run deletes both clusters"
    );
    assert!(
        !reports.join("result.json").exists(),
        "a canceled run states no verdict, so it writes no result.json"
    );

    let diagnostics = read_diagnostics(&reports);
    assert_eq!(diagnostics["stage"], "canceled");
    let failure = diagnostics["failure"]
        .as_str()
        .expect("the artifact names the failure");
    assert!(
        failure.contains("SIGINT") && failure.contains("capture"),
        "the artifact says what interrupted the run and where it stopped: {failure}"
    );
    assert!(
        output.stderr.contains("canceled by SIGINT"),
        "the console says the run was canceled: {}",
        output.stderr
    );

    std::fs::remove_dir_all(&dir).expect("scratch dir must be removable");
}

/// The same shape one stage later: a cancellation observed mid-capture
/// stops the run before it compares anything.
#[test]
fn a_cancellation_during_capture_stops_before_the_comparison() {
    let dir = unique_dir("capture");
    let config = write_lab(&dir);
    let reports = dir.join("reports");
    let backend = FakeBackend::new(CancelAt::Capture);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::InfrastructureFailed);
    assert_eq!(
        backend.captured_sides().len(),
        2,
        "the capture already in flight runs to completion on both sides"
    );
    assert_eq!(backend.clusters.deleted_sides().len(), 2);
    assert!(
        !reports.join("result.json").exists(),
        "a partial corpus must never be graded"
    );

    let diagnostics = read_diagnostics(&reports);
    assert_eq!(diagnostics["stage"], "canceled");
    let failure = diagnostics["failure"]
        .as_str()
        .expect("the artifact names the failure");
    assert!(
        failure.contains("SIGINT"),
        "the artifact names the signal: {failure}"
    );
    assert!(
        failure.contains("states no verdict"),
        "the artifact is explicit that there is no verdict: {failure}"
    );

    std::fs::remove_dir_all(&dir).expect("scratch dir must be removable");
}

/// A signal that arrives before any cluster exists provisions nothing at
/// all — there is no cluster to delete and no workspace to write a
/// diagnostics artifact into, and the run says so rather than creating
/// two clusters it is about to throw away.
#[test]
fn a_cancellation_before_provisioning_creates_no_cluster() {
    let dir = unique_dir("pre-cluster");
    let config = write_lab(&dir);
    let summary = dir.join("summary.md");
    let backend = FakeBackend::new(CancelAt::BeforeTheRun);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: None,
        github_summary: Some(&summary),
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::InfrastructureFailed);
    assert!(
        backend.clusters.created_sides().is_empty(),
        "nothing may be provisioned after the run was asked to stop"
    );
    assert!(backend.installed_sides().is_empty());
    assert!(backend.captured_sides().is_empty());
    assert!(
        output.stderr.contains("nothing was provisioned"),
        "the console says nothing was created: {}",
        output.stderr
    );

    let written = std::fs::read_to_string(&summary).expect("the job summary is always written");
    assert!(
        written.contains("canceled"),
        "the job summary names the canceled stage: {written}"
    );

    std::fs::remove_dir_all(&dir).expect("scratch dir must be removable");
}

/// The manual-recovery list is registered when the clusters come up,
/// retired when they are deleted, and — this is the case that matters —
/// kept when the deletion failed, because that is exactly when an
/// operator needs it.
#[test]
fn cluster_deletion_commands_survive_a_failed_cleanup() {
    let dir = unique_dir("recovery");
    let config = write_lab(&dir);
    let reports = dir.join("reports");
    let backend =
        FakeBackend::new(CancelAt::Install).with_clusters(FakeClusterManager::failing_delete());
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    let pending = backend.cancellation.pending_cleanup_commands();
    assert_eq!(
        pending.len(),
        2,
        "both clusters are still out there: {pending:?}"
    );
    for command in &pending {
        assert!(
            command.starts_with("kind delete cluster --name adlab-"),
            "a recovery command is the exact command to paste: {command}"
        );
    }
    assert_eq!(
        output.disposition,
        RunDisposition::InfrastructureFailed,
        "a failed cleanup never reports a verdict either"
    );

    std::fs::remove_dir_all(&dir).expect("scratch dir must be removable");
}

/// The same run with nobody interrupting it reaches its verdict, and
/// leaves nothing registered for manual cleanup: the cancellation
/// plumbing must be invisible to a run that is never canceled.
#[test]
fn an_uninterrupted_run_reaches_its_verdict_and_registers_no_recovery() {
    let dir = unique_dir("uninterrupted");
    let config = write_lab(&dir);
    let reports = dir.join("reports");
    let backend = FakeBackend::new(CancelAt::Never);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&reports),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let output = run(&backend, &request);

    assert_eq!(output.disposition, RunDisposition::Passed);
    assert!(reports.join("result.json").exists());
    assert!(!reports.join("diagnostics.json").exists());
    assert!(
        backend.cancellation.pending_cleanup_commands().is_empty(),
        "both clusters were deleted, so nothing is left to recover by hand"
    );
    assert!(backend.cancellation.signal().is_none());

    std::fs::remove_dir_all(&dir).expect("scratch dir must be removable");
}

/// The exit codes themselves, stated as literals beside the signals they
/// belong to — the same way `tests/exit_codes.rs` states the frozen
/// seven. `130`/`143` are additive and frozen too: canceled, no verdict.
#[test]
fn a_canceled_run_exits_128_plus_the_signal() {
    assert_eq!(
        admissionlab_cli::exit::code_for_cancellation(CancelSignal::Interrupt),
        std::process::ExitCode::from(130)
    );
    assert_eq!(
        admissionlab_cli::exit::code_for_cancellation(CancelSignal::Terminate),
        std::process::ExitCode::from(143)
    );
    assert_eq!(CancelSignal::Interrupt.exit_code(), 130);
    assert_eq!(CancelSignal::Terminate.exit_code(), 143);
    // And the fallback a caller that ignores cancellation would read is
    // never one a CI gate could mistake for a result.
    assert!(matches!(
        admissionlab_cli::exit::CANCELED_DISPOSITION,
        RunDisposition::InfrastructureFailed
    ));
}

/// The double-interrupt decision, through the public state machine: the
/// first signal cancels, the second forces, and the signal that stopped
/// the run is the first one — which is the one whose exit code the
/// process answers with.
#[test]
fn the_second_signal_forces_and_the_first_one_names_the_exit_code() {
    use admissionlab_cli::cancel::{InterruptAction, observe_signal};

    let cancellation = Cancellation::new();
    assert_eq!(
        observe_signal(&cancellation, CancelSignal::Interrupt),
        InterruptAction::Cancel
    );
    assert_eq!(
        observe_signal(&cancellation, CancelSignal::Terminate),
        InterruptAction::Force
    );
    assert_eq!(cancellation.signal(), Some(CancelSignal::Interrupt));
}

// ---------------------------------------------------------------------
// The real half: a real binary, real clusters, real signals
// ---------------------------------------------------------------------

/// The workspace root, independent of where `cargo test` was invoked.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The smallest real lab: two bare clusters, two fixtures, no components
/// — about a minute, and every one of its stages is real.
fn example_config() -> PathBuf {
    workspace_root().join("examples/admission-basic/admissionlab.yaml")
}

/// The compiled binary under test, as `cargo test` built it.
fn binary() -> PathBuf {
    // `CARGO_BIN_EXE_<name>` is set by Cargo for every binary in this
    // crate when it builds an integration test, which is what makes this
    // independent of the profile and target directory in use.
    PathBuf::from(env!("CARGO_BIN_EXE_admissionlab"))
}

/// Spawns a real `admissionlab test` with both streams piped.
fn spawn_lab(report_dir: &Path) -> Child {
    Command::new(binary())
        .arg("test")
        .arg(example_config())
        .arg("--report-dir")
        .arg(report_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Never colored: these assertions read the text, not the escapes.
        .env("NO_COLOR", "1")
        .spawn()
        .expect("the admissionlab binary must be spawnable")
}

/// Reads the child's stdout until a line contains `marker`, returning
/// everything read so far.
///
/// Driving the interrupt off the run's own progress output is what makes
/// "during cluster setup" and "after install" real rather than a guess: a
/// sleep long enough to be reliable on a cold machine is long enough to
/// overshoot the stage on a warm one.
fn read_until(child: &mut Child, marker: &str) -> Vec<String> {
    let stdout = child.stdout.take().expect("stdout was piped");
    let mut lines = Vec::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.expect("the run's stdout must be UTF-8");
        println!("[lab] {line}");
        let matched = line.contains(marker);
        lines.push(line);
        if matched {
            return lines;
        }
    }
    panic!("the run ended without ever printing {marker:?}; saw: {lines:#?}");
}

/// Sends `SIGINT` to one process. Spelled as `kill` rather than through a
/// signal crate so this test adds no dependency to send the one signal it
/// needs.
fn interrupt(child: &Child) {
    let status = Command::new("kill")
        .arg("-INT")
        .arg(child.id().to_string())
        .status()
        .expect("kill must be runnable");
    assert!(status.success(), "kill -INT failed: {status}");
}

/// Every `adlab-*` cluster `kind` currently reports.
fn lab_clusters() -> Vec<String> {
    let output = Command::new("kind")
        .arg("get")
        .arg("clusters")
        .output()
        .expect("kind must be on PATH for the ignored tests");
    assert!(
        output.status.success(),
        "kind get clusters failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| name.starts_with("adlab-"))
        .map(str::to_owned)
        .collect()
}

/// Runs `scripts/verify-cleanup.sh --after-interrupt`, the sweep that
/// exists for the one path that deliberately leaks (a forced exit), and
/// returns what it printed.
fn sweep_after_interrupt() -> String {
    let output = Command::new(workspace_root().join("scripts/verify-cleanup.sh"))
        .arg("--after-interrupt")
        .output()
        .expect("scripts/verify-cleanup.sh must be executable");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("[sweep] {text}");
    assert!(output.status.success(), "the sweep must not fail: {text}");
    text
}

/// Waits for the child and returns its exit code, failing if it outlives
/// `budget`.
fn wait_for_exit(child: &mut Child, budget: Duration) -> i32 {
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait().expect("waiting on the child must work") {
            Some(status) => {
                return status.code().unwrap_or_else(|| {
                    panic!("the run was killed by a signal rather than exiting: {status}")
                });
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("the run did not exit within {budget:?}");
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// Ctrl-C during cluster setup: the run finishes bringing the clusters
/// up, refuses to start the install, writes its diagnostics, deletes both
/// clusters, and exits `130`.
#[test]
#[ignore = "requires Docker and kind; run with --ignored"]
fn a_real_interrupt_during_setup_tears_down_and_leaves_no_cluster() {
    assert!(
        lab_clusters().is_empty(),
        "this test starts from a host with no adlab-* cluster"
    );
    let dir = unique_dir("real-setup");
    let reports = dir.join("reports");
    let mut child = spawn_lab(&reports);

    // `run.json` is written immediately before the two `kind create`
    // calls, so the signal lands while both clusters are coming up --
    // the moment a user's Ctrl-C most often does.
    read_until(&mut child, "run.json");
    interrupt(&child);

    let code = wait_for_exit(&mut child, Duration::from_secs(600));
    let mut stderr = String::new();
    if let Some(mut handle) = child.stderr.take() {
        let _ = handle.read_to_string(&mut stderr);
    }
    println!("[lab stderr] {stderr}");

    assert_eq!(
        code, 130,
        "a canceled run exits 128 + SIGINT; stderr: {stderr}"
    );
    assert!(
        stderr.contains("SIGINT received"),
        "the run acknowledges the signal: {stderr}"
    );
    let diagnostics = read_diagnostics(&reports);
    assert_eq!(diagnostics["stage"], "canceled");
    assert!(
        !reports.join("result.json").exists(),
        "no verdict was reached, so no result.json"
    );
    assert_eq!(
        lab_clusters(),
        Vec::<String>::new(),
        "a cooperatively canceled run leaves no cluster behind"
    );

    std::fs::remove_dir_all(&dir).expect("scratch dir must be removable");
}

/// Ctrl-C after the stacks are installed: the same shape one stage
/// later. Whether the run stops before its capture or before its
/// comparison depends on how fast the signal beats the next boundary,
/// and both are the same promise — no verdict, diagnostics written,
/// clusters deleted.
#[test]
#[ignore = "requires Docker and kind; run with --ignored"]
fn a_real_interrupt_after_install_produces_no_verdict() {
    assert!(
        lab_clusters().is_empty(),
        "this test starts from a host with no adlab-* cluster"
    );
    let dir = unique_dir("real-install");
    let reports = dir.join("reports");
    let mut child = spawn_lab(&reports);

    read_until(&mut child, "component(s).");
    interrupt(&child);

    let code = wait_for_exit(&mut child, Duration::from_secs(600));
    let mut stderr = String::new();
    if let Some(mut handle) = child.stderr.take() {
        let _ = handle.read_to_string(&mut stderr);
    }
    println!("[lab stderr] {stderr}");

    assert_eq!(
        code, 130,
        "a canceled run exits 128 + SIGINT; stderr: {stderr}"
    );
    let diagnostics = read_diagnostics(&reports);
    assert_eq!(diagnostics["stage"], "canceled");
    assert!(!reports.join("result.json").exists());
    assert_eq!(
        lab_clusters(),
        Vec::<String>::new(),
        "a cooperatively canceled run leaves no cluster behind"
    );

    std::fs::remove_dir_all(&dir).expect("scratch dir must be removable");
}

/// Two interrupts in quick succession: the process gives up immediately
/// and prints the exact commands for what it is abandoning.
///
/// This test asserts a leak on purpose — see this file's documentation —
/// and cleans it up through the sweep the script provides.
#[test]
#[ignore = "requires Docker and kind; run with --ignored"]
fn a_real_double_interrupt_exits_at_once_and_prints_the_cleanup_commands() {
    assert!(
        lab_clusters().is_empty(),
        "this test starts from a host with no adlab-* cluster"
    );
    let dir = unique_dir("real-force");
    let reports = dir.join("reports");
    let mut child = spawn_lab(&reports);

    // Both clusters exist by the time this line is printed, so their
    // deletion commands are registered and the forced exit has something
    // real to confess.
    read_until(&mut child, "created baseline cluster");
    interrupt(&child);
    std::thread::sleep(Duration::from_millis(300));
    interrupt(&child);

    // Ten seconds is generous for "immediately" and still far below the
    // minutes a real teardown can take, so this bound is what actually
    // distinguishes a forced exit from a cooperative one.
    let code = wait_for_exit(&mut child, Duration::from_secs(10));
    let mut stderr = String::new();
    if let Some(mut handle) = child.stderr.take() {
        let _ = handle.read_to_string(&mut stderr);
    }
    println!("[lab stderr] {stderr}");

    assert_eq!(code, 130, "a forced exit still answers 128 + SIGINT");
    assert!(
        stderr.contains("second SIGINT"),
        "the process says why it is leaving: {stderr}"
    );
    assert!(
        stderr.contains("kind delete cluster --name adlab-"),
        "a forced exit prints the exact recovery commands: {stderr}"
    );

    // The contract is the printed commands, not a clean host: whatever
    // survived is swept here, and the sweep says what it found.
    let swept = sweep_after_interrupt();
    assert!(
        swept.contains("verify-cleanup:"),
        "the sweep reports what it did: {swept}"
    );
    assert!(
        lab_clusters().is_empty(),
        "the sweep leaves the host clean for the next test"
    );

    std::fs::remove_dir_all(&dir).expect("scratch dir must be removable");
}
