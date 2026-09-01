//! `admissionlab reproduce`, driven two ways (ROADMAP Task 5.3).
//!
//! 1. **Through `pipeline::run_lab` against fakes**, to pin the one claim
//!    the command exists to make and that no real run can demonstrate
//!    cheaply: that a reproduction hands `ClusterManager::create` the
//!    *recorded* `image@digest` and never asks the compatibility matrix
//!    what it thinks today (step 4). The fake cluster manager here
//!    records the whole `ClusterSpec` it was given — not just the side,
//!    as `tests/test_command.rs`'s does — because the node image
//!    reference *is* the subject.
//! 2. **Through the compiled binary**, for the argument surface and the
//!    plan-time refusals (step 1), which need no cluster at all and must
//!    therefore cost no Docker.
//!
//! `tests/reproduce_e2e.rs` is the third way — two real reproductions of
//! a real run — and is `#[ignore]`d.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use admissionlab_admission::{AdmissionDecision, AdmissionOutcome, AdmissionTrace, TraceEvidence};
use admissionlab_cli::pipeline::{
    Console, GatewaySuiteError, GatewaySuiteRunner, LabBackend, OutcomeCapture, RunRequest,
    SideGatewayOutcome, run_lab,
};
use admissionlab_core::run_manifest::{SCHEMA_VERSION, SUPPORTED_SCHEMA_VERSIONS};
use admissionlab_core::{
    ArtifactStore, CapturedFixture, ClusterDiagnostics, ClusterError, ClusterHandle,
    ClusterManager, ClusterSpec, ComponentProvenance, DoctorReport, EnvironmentProvenance,
    FixtureCapture, FixtureCaptureError, HostProvenance, InstalledComponent, ReproductionPin,
    RunDisposition, RunId, RunManifest, RunPaths, RunStage, RunStatus, Side, SideCapture,
    SideInstall, StackInstallError, StackInstaller, ToolName, ToolProvenance, ToolStatus,
    UNCONFIRMED_COMPONENT_VERSION, file_sha256, sha256_hex,
};
use admissionlab_fixtures::FixtureSource;
use admissionlab_report::TerminalOptions;
use admissionlab_spec::ResolvedComponent;
use async_trait::async_trait;
use predicates::prelude::*;

// ---------------------------------------------------------------------
// The duplicated literal this whole design leans on
// ---------------------------------------------------------------------

/// `admissionlab_core::reproduce` cannot import
/// `admissionlab_installer::UNCONFIRMED_VERSION` — the installer sits
/// above core — so it duplicates the literal and documents that it does.
/// This crate can see both, which makes it the one place the duplication
/// can be checked. Without this, a rename in the installer would silently
/// turn "the recorded run could not confirm this version" into "install
/// version `unknown`".
#[test]
fn the_unconfirmed_version_sentinel_is_the_same_string_in_both_crates() {
    assert_eq!(
        UNCONFIRMED_COMPONENT_VERSION,
        admissionlab_installer::UNCONFIRMED_VERSION
    );
}

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

fn unique_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-cli-reproduce-{label}-{}",
        RunId::generate().as_str()
    ));
    std::fs::create_dir_all(&dir).expect("failed to create scratch dir");
    dir
}

/// The recorded node image, split as a manifest stores it.
const NODE_IMAGE: &str = "kindest/node:v1.31.0";
/// Its recorded content digest — deliberately *not* the digest a fresh
/// resolution would produce for any version this repository knows.
const NODE_DIGEST: &str = "sha256:53df588e04085fd41ae12de0c3fe4c72f0013bbe88223377593fd9d46ec434dc";

/// Writes a minimal lab (one manifests-installed component, one fixture)
/// and returns the configuration's path.
fn write_lab(dir: &Path) -> PathBuf {
    let config = dir.join("admissionlab.yaml");
    std::fs::write(
        &config,
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: Lab\n\
         baseline:\n\
         \x20\x20kubernetes: \"1.31.0\"\n\
         \x20\x20components:\n\
         \x20\x20\x20\x20- name: setup\n\
         \x20\x20\x20\x20\x20\x20version: \"1\"\n\
         \x20\x20\x20\x20\x20\x20install:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20type: manifests\n\
         \x20\x20\x20\x20\x20\x20\x20\x20paths:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20- stack.yaml\n\
         candidate:\n\
         \x20\x20kubernetes: \"1.31.0\"\n\
         \x20\x20components:\n\
         \x20\x20\x20\x20- name: setup\n\
         \x20\x20\x20\x20\x20\x20version: \"1\"\n\
         \x20\x20\x20\x20\x20\x20install:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20type: manifests\n\
         \x20\x20\x20\x20\x20\x20\x20\x20paths:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20- stack.yaml\n\
         fixtures:\n\
         \x20\x20include:\n\
         \x20\x20\x20\x20- \"fixtures/**/*.yaml\"\n",
    )
    .expect("write lab configuration");
    std::fs::write(
        dir.join("stack.yaml"),
        "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: probe\n",
    )
    .expect("write stack");
    let fixtures = dir.join("fixtures");
    std::fs::create_dir_all(&fixtures).expect("create fixtures dir");
    std::fs::write(
        fixtures.join("pod.yaml"),
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: probe\nspec:\n  containers:\n\
         \x20\x20\x20\x20- name: app\n      image: registry.k8s.io/pause:3.10\n",
    )
    .expect("write fixture");
    config
}

/// The manifest a recorded run over [`write_lab`]'s tree would have left,
/// with its own component version and both digests filled in for real.
fn manifest_for(config: &Path, component_version: &str) -> RunManifest {
    let environment = || EnvironmentProvenance {
        kubernetes_version: "1.31.0".to_owned(),
        node_image: NODE_IMAGE.to_owned(),
        node_image_digest: Some(NODE_DIGEST.to_owned()),
        images: Some(Vec::new()),
        components: vec![ComponentProvenance {
            name: "setup".to_owned(),
            version: component_version.to_owned(),
            source_sha256: None,
        }],
    };
    RunManifest {
        schema_version: SCHEMA_VERSION.to_owned(),
        run_id: RunId::generate(),
        admissionlab_version: "0.1.0".to_owned(),
        status: RunStatus::Completed,
        stage: RunStage::Completed,
        host: HostProvenance::detect(),
        tools: ToolProvenance {
            kind: Some("v0.33.0".to_owned()),
            kubectl: Some("v1.31.0".to_owned()),
            helm: Some("v3.16.2".to_owned()),
            docker: Some("27.5.0".to_owned()),
        },
        baseline: environment(),
        candidate: environment(),
        config_api_version: Some("admissionlab.io/v1alpha1".to_owned()),
        config_sha256: file_sha256(config).expect("hash config"),
        fixture_hashes: BTreeMap::new(),
        expectations_sha256: None,
        normalization_sha256: sha256_hex(b"normalization"),
        policy_sha256: sha256_hex(b"policy"),
        gateway: None,
        started_at: SystemTime::UNIX_EPOCH,
        completed_at: Some(SystemTime::UNIX_EPOCH),
    }
}

// ---------------------------------------------------------------------
// The fakes
// ---------------------------------------------------------------------

/// A `ClusterManager` that records the full [`ClusterSpec`] it was asked
/// to create, and how many times its node image resolution was consulted.
struct RecordingClusterManager {
    created: Mutex<Vec<ClusterSpec>>,
    resolutions: Mutex<usize>,
}

impl RecordingClusterManager {
    fn new() -> Self {
        Self {
            created: Mutex::new(Vec::new()),
            resolutions: Mutex::new(0),
        }
    }

    fn created(&self) -> Vec<ClusterSpec> {
        self.created.lock().expect("poisoned").clone()
    }

    fn resolutions(&self) -> usize {
        *self.resolutions.lock().expect("poisoned")
    }
}

#[async_trait]
impl ClusterManager for RecordingClusterManager {
    async fn resolve_node_image(&self, version: &str) -> Result<String, ClusterError> {
        *self.resolutions.lock().expect("poisoned") += 1;
        // Deliberately a *different* digest from the recorded one: if a
        // reproduction ever consulted this, the assertion below would
        // catch the substituted reference rather than an absent one.
        Ok(format!(
            "kindest/node:v{version}@sha256:\
             0000000000000000000000000000000000000000000000000000000000000000"
        ))
    }

    async fn create(
        &self,
        spec: &ClusterSpec,
        paths: &RunPaths,
    ) -> Result<ClusterHandle, ClusterError> {
        self.created.lock().expect("poisoned").push(spec.clone());
        Ok(ClusterHandle {
            spec: spec.clone(),
            kubeconfig: paths
                .kubeconfigs()
                .join(format!("{}.yaml", spec.side.as_str())),
            audit_log: paths.logs().join(spec.side.as_str()).join("audit.log"),
        })
    }

    async fn delete(&self, _handle: &ClusterHandle) -> Result<(), ClusterError> {
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

/// A `StackInstaller` that installs nothing and reports back exactly the
/// versions it was handed, so a test can read what the pin did to them.
struct RecordingInstaller {
    installed: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait]
impl StackInstaller for RecordingInstaller {
    async fn install_stack(
        &self,
        cluster: &ClusterHandle,
        components: &[ResolvedComponent],
        _component_timeout: Duration,
    ) -> Result<SideInstall, StackInstallError> {
        let mut installed = self.installed.lock().expect("poisoned");
        for component in components {
            installed.push((component.name.clone(), component.version.clone()));
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

/// A `FixtureCapture` that produces one identical outcome per side.
struct FakeCapture {
    fixtures: Vec<FixtureSource>,
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
        let mut captured = Vec::new();
        for fixture in &self.fixtures {
            let outcome = AdmissionOutcome {
                fixture_id: fixture.id.clone(),
                side,
                decision: AdmissionDecision::Accepted,
                warnings: Vec::new(),
                total_latency: Duration::from_millis(7),
                final_object: Some(serde_json::json!({
                    "apiVersion": "v1",
                    "kind": "Pod",
                    "metadata": {"name": "probe"},
                    "spec": {"containers": [
                        {"name": "app", "image": "registry.k8s.io/pause:3.10"}
                    ]}
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

/// A whole fake world, optionally reproducing a recorded one.
/// A `GatewaySuiteRunner` for a backend that is never asked to run one.
///
/// `run_side` fails rather than returning an empty outcome: an empty
/// `SideGatewayOutcome` would be a claim that a suite ran and observed
/// nothing, which is exactly the fabrication Global Constraint 15
/// forbids. Reaching it at all means `run_lab` ran a Gateway stage for a
/// configuration these tests never gave one, which is a bug worth
/// failing on.
struct NoGatewaySuite;

#[async_trait]
impl GatewaySuiteRunner for NoGatewaySuite {
    async fn run_side(
        &self,
        _cluster: &ClusterHandle,
        _side: Side,
        _paths: &RunPaths,
    ) -> Result<SideGatewayOutcome, GatewaySuiteError> {
        Err(GatewaySuiteError {
            contract: None,
            message: "this test's lab declares no gateway suite".to_owned(),
        })
    }
}

struct FakeBackend {
    clusters: Arc<RecordingClusterManager>,
    installed: Arc<Mutex<Vec<(String, String)>>>,
    pin: Option<ReproductionPin>,
}

impl FakeBackend {
    fn new(pin: Option<ReproductionPin>) -> Self {
        Self {
            clusters: Arc::new(RecordingClusterManager::new()),
            installed: Arc::new(Mutex::new(Vec::new())),
            pin,
        }
    }
}

#[async_trait]
impl LabBackend for FakeBackend {
    type Clusters = RecordingClusterManager;
    type Installer = RecordingInstaller;
    type Capture = FakeCapture;
    // `admissionlab reproduce` reproduces whatever the recorded run did,
    // Gateway suite included; none of these tests configures one, so
    // this is the never-constructed half of the seam.
    type Gateway = NoGatewaySuite;

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
        RecordingInstaller {
            installed: Arc::clone(&self.installed),
        }
    }

    fn gateway_suite(
        &self,
        _suite: admissionlab_spec::GatewaySuiteSpec,
        _store: ArtifactStore,
    ) -> Self::Gateway {
        NoGatewaySuite
    }

    fn fixture_capture(
        &self,
        fixtures: Vec<FixtureSource>,
        _store: ArtifactStore,
    ) -> Self::Capture {
        FakeCapture {
            fixtures,
            outcomes: Mutex::new(Vec::new()),
        }
    }

    fn component_timeout(&self) -> Duration {
        Duration::from_millis(1)
    }

    fn reproduction_pin(&self) -> Option<&ReproductionPin> {
        self.pin.as_ref()
    }
}

/// Drives one run against `backend`, discarding both streams.
fn run(backend: &FakeBackend, request: &RunRequest<'_>) -> RunDisposition {
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let mut console = Console {
        out: &mut out,
        err: &mut err,
        terminal: TerminalOptions::for_stream(false, true),
    };
    block_on(run_lab(backend, request, &mut console))
}

// ---------------------------------------------------------------------
// Step 4: never fall forward
// ---------------------------------------------------------------------

/// The claim the whole command rests on, asserted at the seam that
/// actually provisions: the reference `ClusterManager::create` receives.
///
/// `admissionlab_core::LabRunner::create_clusters` builds each
/// `ClusterSpec` from `ResolvedNodeImages`, so this is the last point
/// before `kind --image` and the first point a test can observe without a
/// Docker daemon. Asserting on the *argv* would mean asserting on
/// `admissionlab-cluster`'s own construction, which its own tests already
/// pin; the reference reaching that construction is what this task
/// changes.
#[test]
fn a_reproduction_creates_clusters_from_the_recorded_image_digest() {
    let dir = unique_dir("pinned-image");
    let config = write_lab(&dir);
    let manifest = manifest_for(&config, "1");
    let backend = FakeBackend::new(Some(ReproductionPin::from_manifest(&manifest)));
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&dir.join("artifacts")),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    let disposition = run(&backend, &request);
    assert_eq!(disposition, RunDisposition::Passed);

    let expected = format!("{NODE_IMAGE}@{NODE_DIGEST}");
    let created = backend.clusters.created();
    assert_eq!(created.len(), 2, "{created:#?}");
    for spec in &created {
        assert_eq!(
            spec.node_image, expected,
            "a reproduction must create its clusters from the recorded reference, digest and all"
        );
        assert_eq!(spec.kubernetes_version, "1.31.0");
    }
    assert_eq!(
        backend.clusters.resolutions(),
        0,
        "a reproduction must never consult node image resolution at all: consulting it is the \
         only way a newer digest could be substituted"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The control: without a pin, the same pipeline resolves as it always
/// did. Without this, the assertion above could pass because node image
/// resolution had been broken rather than bypassed.
#[test]
fn an_ordinary_run_still_resolves_its_node_images() {
    let dir = unique_dir("unpinned-image");
    let config = write_lab(&dir);
    let backend = FakeBackend::new(None);
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&dir.join("artifacts")),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    assert_eq!(run(&backend, &request), RunDisposition::Passed);
    assert_eq!(backend.clusters.resolutions(), 2);
    for spec in &backend.clusters.created() {
        assert!(
            spec.node_image.ends_with(
                "@sha256:0000000000000000000000000000000000000000000000000000000000000000"
            ),
            "{spec:#?}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// Step 2: recorded component versions reach the installer
// ---------------------------------------------------------------------

#[test]
fn a_reproduction_installs_the_recorded_component_version_not_the_configured_one() {
    let dir = unique_dir("pinned-version");
    let config = write_lab(&dir);
    // The configuration says `version: "1"`; the recorded run installed
    // `"7"`. With the configuration's digest matching this cannot arise
    // from the file itself, which is exactly why it is worth guarding:
    // it can only come from resolution behavior changing, and the
    // manifest is what wins.
    let manifest = manifest_for(&config, "7");
    let backend = FakeBackend::new(Some(ReproductionPin::from_manifest(&manifest)));
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&dir.join("artifacts")),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    assert_eq!(run(&backend, &request), RunDisposition::Passed);
    let installed = backend.installed.lock().expect("poisoned").clone();
    assert_eq!(
        installed,
        vec![
            ("setup".to_owned(), "7".to_owned()),
            ("setup".to_owned(), "7".to_owned()),
        ],
        "the recorded version must reach the installer on both sides"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_component_the_recorded_run_could_not_confirm_keeps_the_sources_own_pin() {
    let dir = unique_dir("unconfirmed-version");
    let config = write_lab(&dir);
    let manifest = manifest_for(&config, UNCONFIRMED_COMPONENT_VERSION);
    let backend = FakeBackend::new(Some(ReproductionPin::from_manifest(&manifest)));
    let request = RunRequest {
        config: &config,
        keep_clusters: false,
        report_dir: Some(&dir.join("artifacts")),
        github_summary: None,
        run_root: dir.join("runs"),
    };

    assert_eq!(run(&backend, &request), RunDisposition::Passed);
    let installed = backend.installed.lock().expect("poisoned").clone();
    assert!(
        installed.iter().all(|(_, version)| version.as_str() == "1"),
        "installing the literal {UNCONFIRMED_COMPONENT_VERSION:?} would fail for a reason that \
         has nothing to do with the lab: {installed:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// Step 1, through the binary: refusals that cost no Docker
// ---------------------------------------------------------------------

/// The compiled binary, as a user's shell reaches it.
fn admissionlab() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("admissionlab").expect("the admissionlab binary must build")
}

/// Writes `manifest` next to the source tree and returns its path.
fn write_manifest(dir: &Path, manifest: &RunManifest) -> PathBuf {
    let path = dir.join("run.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(manifest).expect("serialize manifest"),
    )
    .expect("write manifest");
    path
}

#[test]
fn a_tampered_fixture_refuses_before_anything_is_created() {
    let dir = unique_dir("tampered-fixture");
    let config = write_lab(&dir);
    let mut manifest = manifest_for(&config, "1");
    let fixture = dir.join("fixtures/pod.yaml");
    // Record the fixture as it is now, then change one byte of it.
    let discovered = admissionlab_fixtures::discover_fixtures(
        &admissionlab_spec::resolve_lab(admissionlab_spec::load_lab(&config).expect("load"))
            .expect("resolve")
            .fixtures,
    )
    .expect("discover");
    for source in &discovered {
        manifest
            .fixture_hashes
            .insert(source.id.clone(), source.sha256.clone());
    }
    let path = write_manifest(&dir, &manifest);
    let original = std::fs::read_to_string(&fixture).expect("read fixture");
    std::fs::write(&fixture, original.replace("pause:3.10", "pause:3.11"))
        .expect("tamper with fixture");

    admissionlab()
        .arg("reproduce")
        .arg(&path)
        .arg("--source-root")
        .arg(&dir)
        .assert()
        .code(2)
        .stderr(
            predicate::str::contains("no longer matches the recorded run")
                .and(predicate::str::contains("expected sha256"))
                .and(predicate::str::contains("actual   sha256")),
        );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_manifest_that_is_not_one_refuses_with_the_path_it_read() {
    let dir = unique_dir("bad-manifest");
    write_lab(&dir);
    let path = dir.join("run.json");
    std::fs::write(&path, "{\"schemaVersion\": 3}").expect("write manifest");

    admissionlab()
        .arg("reproduce")
        .arg(&path)
        .arg("--source-root")
        .arg(&dir)
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "is not a run manifest this build can reproduce",
        ));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A manifest from a schema version this build does not read is refused
/// before anything is provisioned, and the refusal names every version it
/// *does* read (ROADMAP Task 7.3).
///
/// The CLI half of `admissionlab-core`'s own version dispatch: what this
/// adds is that `admissionlab reproduce` goes through the versioned
/// reader at all, so the message a user sees is the one that names the
/// supported versions rather than a `serde` field complaint.
#[test]
fn a_manifest_from_an_unknown_schema_version_names_the_supported_versions() {
    let dir = unique_dir("unknown-schema");
    let config = write_lab(&dir);
    let mut manifest = manifest_for(&config, "1");
    manifest.schema_version = "admissionlab.io/run-manifest/v2".to_owned();
    let path = write_manifest(&dir, &manifest);

    let mut assertion = admissionlab()
        .arg("reproduce")
        .arg(&path)
        .arg("--source-root")
        .arg(&dir)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("admissionlab.io/run-manifest/v2"));
    for version in SUPPORTED_SCHEMA_VERSIONS {
        assertion = assertion.stderr(predicate::str::contains(*version));
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_lab_configuration_names_the_conventional_path_it_looked_for() {
    let dir = unique_dir("missing-config");
    let config = write_lab(&dir);
    let path = write_manifest(&dir, &manifest_for(&config, "1"));
    std::fs::remove_file(&config).expect("remove the lab configuration");

    admissionlab()
        .arg("reproduce")
        .arg(&path)
        .arg("--source-root")
        .arg(&dir)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("admissionlab.yaml"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reproduce_advertises_the_flags_it_shares_with_test() {
    admissionlab()
        .args(["reproduce", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("--source-root")
                .and(predicate::str::contains("--config"))
                .and(predicate::str::contains("--keep-clusters"))
                .and(predicate::str::contains("--report-dir")),
        );
}
