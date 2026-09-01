//! The run manifest's incremental-write contract (Task 5.2), driven
//! against real filesystem writes and this file's own `ClusterManager`/
//! `StackInstaller` test doubles.
//!
//! The property under test is the one a user actually depends on: **a run
//! that dies partway through still leaves a valid, parseable `run.json`
//! that says where it stopped.** `tests/run_manifest.rs` covers the
//! document's shape; this file covers its life cycle, and does so through
//! a real [`ArtifactStore`] on a real temporary directory rather than an
//! in-memory stub, because the guarantee being tested — atomic rewrite
//! after every stage — is a filesystem guarantee.
//!
//! # Why the sequence here is hand-driven rather than a single call
//!
//! `admissionlab-core` cannot run the pipeline: normalization, diffing,
//! policy, and reporting all live in crates *above* it (see
//! `admissionlab_core::run`'s own module documentation), so no function
//! in this crate takes a run from start to finish. What this crate does
//! own is every piece the pipeline drives — [`LabRunner::create_workspace`],
//! [`LabRunner::resolve_node_images`], [`LabRunner::create_clusters`],
//! [`LabRunner::install_stacks`], and [`RunManifestWriter`] — so these
//! tests drive exactly that sequence, in exactly the order
//! `admissionlab-cli`'s `pipeline::run_lab` drives it. The end-to-end
//! proof that the real pipeline drives it that way lives alongside the
//! pipeline, in `admissionlab-cli`'s `tests/test_command.rs`
//! (`a_failed_candidate_install_leaves_a_manifest_naming_the_stage`);
//! the two are complementary rather than redundant, the same way
//! `tests/cli.rs` and `tests/test_command.rs` already are.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use admissionlab_core::run_manifest::SCHEMA_VERSION;
use admissionlab_core::{
    ArtifactStore, ClusterDiagnostics, ClusterError, ClusterHandle, ClusterManager, ClusterSpec,
    ComponentProvenance, EnvironmentProvenance, HostProvenance, InstalledComponent, LabRunner,
    RunId, RunManifest, RunManifestWriter, RunOptions, RunPaths, RunStage, RunStatus, Side,
    SideInstall, StackInstallError, StackInstallFailure, StackInstaller, ToolProvenance,
    normalization_sha256, policy_sha256, sha256_hex, split_node_image_reference,
};
use admissionlab_spec::{ResolvedLab, load_lab, resolve_lab};
use async_trait::async_trait;

// ---------------------------------------------------------------------
// Scaffolding (mirrors `tests/run_lifecycle.rs`)
// ---------------------------------------------------------------------

fn test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build test tokio runtime")
}

fn unique_run_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "admissionlab-core-run-manifest-{label}-{}",
        RunId::generate().as_str()
    ))
}

/// The workspace's checked-in minimal valid lab configuration, loaded and
/// resolved for real (`ResolvedFixtureSelection` holds compiled globs
/// that only `admissionlab-spec` can build).
fn minimal_resolved_lab() -> ResolvedLab {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/configs/minimal-valid.yaml");
    let loaded = load_lab(&path).expect("minimal-valid.yaml must load");
    resolve_lab(loaded).expect("minimal-valid.yaml must resolve")
}

fn fake_error(reason: &str) -> ClusterError {
    ClusterError::InvalidKubeconfig {
        path: PathBuf::from("/fake/kubeconfig"),
        reason: reason.to_owned(),
    }
}

/// A `ClusterManager` that provisions nothing and records what it was
/// asked to do — enough to prove the manifest already existed before the
/// first `create` call.
struct FakeClusterManager {
    created: Mutex<Vec<Side>>,
    fail_delete: HashSet<Side>,
}

impl FakeClusterManager {
    fn new() -> Self {
        Self {
            created: Mutex::new(Vec::new()),
            fail_delete: HashSet::new(),
        }
    }

    fn create_count(&self) -> usize {
        self.created.lock().expect("mutex poisoned").len()
    }
}

#[async_trait]
impl ClusterManager for FakeClusterManager {
    async fn resolve_node_image(&self, kubernetes_version: &str) -> Result<String, ClusterError> {
        // Digest-pinned, like the real `kind` backend resolving against
        // `compatibility/kubernetes.yaml` — so the manifest this test
        // reads back actually exercises the image/digest split.
        Ok(format!(
            "kindest/node:v{kubernetes_version}@sha256:\
             099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed"
        ))
    }

    async fn create(
        &self,
        spec: &ClusterSpec,
        paths: &RunPaths,
    ) -> Result<ClusterHandle, ClusterError> {
        self.created.lock().expect("mutex poisoned").push(spec.side);
        Ok(ClusterHandle {
            spec: spec.clone(),
            kubeconfig: paths
                .kubeconfigs()
                .join(format!("{}.yaml", spec.side.as_str())),
            audit_log: paths.logs().join(spec.side.as_str()).join("audit.log"),
        })
    }

    async fn delete(&self, handle: &ClusterHandle) -> Result<(), ClusterError> {
        if self.fail_delete.contains(&handle.spec.side) {
            Err(fake_error("fake delete failure"))
        } else {
            Ok(())
        }
    }

    async fn diagnostics(&self, handle: &ClusterHandle) -> ClusterDiagnostics {
        ClusterDiagnostics {
            cluster_name: handle.spec.name.clone(),
            cluster_exists: Some(true),
            kubeconfig_present: false,
            audit_log_present: false,
            notes: Vec::new(),
        }
    }
}

/// A `StackInstaller` that succeeds on one side and fails on the other —
/// the asymmetry Task 5.2 Step 1 asks for ("a candidate install
/// failure").
struct FakeStackInstaller {
    failing_side: Option<Side>,
}

#[async_trait]
impl StackInstaller for FakeStackInstaller {
    async fn install_stack(
        &self,
        cluster: &ClusterHandle,
        components: &[admissionlab_spec::ResolvedComponent],
        _component_timeout: Duration,
    ) -> Result<SideInstall, StackInstallError> {
        if self.failing_side == Some(cluster.spec.side) {
            return Err(StackInstallError {
                component: components.first().map(|component| component.name.clone()),
                message: "fake install failure".to_owned(),
            });
        }
        Ok(SideInstall {
            side: cluster.spec.side,
            components: components
                .iter()
                .map(|component| InstalledComponent {
                    name: component.name.clone(),
                    method: "fake".to_owned(),
                    // Deliberately *not* `component.version`: the whole
                    // point of re-recording components after install is
                    // that the resolved version can differ from the
                    // configured one.
                    resolved_version: format!("{}+resolved", component.version),
                    started_at: SystemTime::UNIX_EPOCH,
                    elapsed: Duration::from_millis(1),
                    diagnostics: Vec::new(),
                })
                .collect(),
        })
    }
}

/// The pre-cluster manifest, built exactly as `admissionlab-cli`'s
/// `pipeline::provenance::initial_manifest` builds it: complete except
/// for the component versions installation will confirm.
fn initial_manifest(
    run_id: &RunId,
    lab: &ResolvedLab,
    baseline_image: &str,
    candidate_image: &str,
) -> RunManifest {
    let environment = |kubernetes: &str, image: &str| {
        let (node_image, node_image_digest) = split_node_image_reference(image);
        EnvironmentProvenance {
            kubernetes_version: kubernetes.to_owned(),
            node_image,
            node_image_digest,
            images: Some(Vec::new()),
            components: vec![ComponentProvenance {
                name: "configured".to_owned(),
                version: "1.0.0".to_owned(),
                source_sha256: None,
            }],
        }
    };

    RunManifest {
        schema_version: SCHEMA_VERSION.to_owned(),
        run_id: run_id.clone(),
        admissionlab_version: "0.1.0".to_owned(),
        status: RunStatus::InProgress,
        stage: RunStage::Started,
        host: HostProvenance::detect(),
        tools: ToolProvenance {
            kind: Some("v0.33.0".to_owned()),
            kubectl: Some("v1.36.4".to_owned()),
            helm: Some("v3.16.2".to_owned()),
            docker: Some("27.5.0".to_owned()),
        },
        baseline: environment(&lab.baseline.kubernetes, baseline_image),
        candidate: environment(&lab.candidate.kubernetes, candidate_image),
        config_api_version: Some("admissionlab.io/v1alpha1".to_owned()),
        config_sha256: sha256_hex(b"apiVersion: admissionlab.io/v1alpha1\n"),
        fixture_hashes: BTreeMap::new(),
        expectations_sha256: None,
        normalization_sha256: normalization_sha256(&admissionlab_core::EffectiveNormalization {
            built_in: Vec::new(),
            recipe: Vec::new(),
            user: Vec::new(),
        }),
        policy_sha256: policy_sha256(&lab.policy),
        gateway: None,
        started_at: SystemTime::UNIX_EPOCH + Duration::new(1_788_264_000, 0),
        completed_at: None,
    }
}

/// Reads `run.json` back as both a typed manifest and raw JSON.
///
/// Both, deliberately: the typed read proves the document still
/// round-trips (a partial manifest that no longer parses would be worse
/// than none), and the raw read is what lets a test assert the literal
/// `"completedAt": null` the roadmap's contract is written in terms of,
/// which a typed `Option<SystemTime>` cannot distinguish from an absent
/// key.
fn read_manifest(paths: &RunPaths) -> (RunManifest, serde_json::Value) {
    let text = std::fs::read_to_string(paths.run_json()).expect("run.json must exist");
    (
        serde_json::from_str(&text).expect("run.json must be a parseable RunManifest"),
        serde_json::from_str(&text).expect("run.json must be valid JSON"),
    )
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

/// Task 5.2's headline contract, end to end at the level this crate owns:
/// a candidate install failure leaves a valid manifest with
/// `completedAt: null` and the failed stage recorded — and with every
/// pre-cluster fact it had already learned still intact.
#[test]
fn a_failed_candidate_install_leaves_a_valid_manifest_naming_the_stage() {
    let root = unique_run_root("candidate-install-fails");
    let manager = Arc::new(FakeClusterManager::new());
    let runner = LabRunner {
        cluster_manager: Arc::clone(&manager),
        artifact_store: ArtifactStore::new(&root),
    };
    let options = RunOptions {
        keep_clusters: false,
        run_root: root.clone(),
    };
    let lab = minimal_resolved_lab();

    let paths = test_runtime().block_on(async {
        let (run_id, paths) = runner
            .create_workspace(&options)
            .await
            .expect("workspace creation must succeed");
        let images = runner
            .resolve_node_images(&lab)
            .await
            .expect("node image resolution must succeed");

        let mut manifest = RunManifestWriter::create(
            ArtifactStore::new(&root),
            &paths,
            initial_manifest(&run_id, &lab, &images.baseline, &images.candidate),
        )
        .await
        .expect("the initial manifest must be writable");

        // The contract's first half: the manifest exists *before* any
        // cluster does. Asserted against the backend's own call counter,
        // not against ordering in this test's source.
        assert!(paths.run_json().is_file());
        assert_eq!(
            manager.create_count(),
            0,
            "the run manifest must be written before the first cluster is created"
        );

        let prepared = runner
            .create_clusters(&lab, &run_id, &paths, &images)
            .await
            .expect("cluster creation must succeed");
        manifest
            .record(RunStage::ClusterCreation, |_| {})
            .await
            .expect("recording cluster creation must succeed");

        let installer = FakeStackInstaller {
            failing_side: Some(Side::Candidate),
        };
        let failure = runner
            .install_stacks(&lab, &prepared, &installer, Duration::from_millis(1))
            .await
            .expect_err("the candidate install was configured to fail");
        assert!(
            matches!(failure, StackInstallFailure::Candidate { .. }),
            "expected a candidate-side failure, got {failure:?}"
        );
        manifest
            .fail(RunStage::Installation)
            .await
            .expect("recording the failure must succeed");

        paths
    });

    let (manifest, raw) = read_manifest(&paths);

    assert_eq!(manifest.status, RunStatus::Failed);
    assert_eq!(manifest.stage, RunStage::Installation);
    assert_eq!(manifest.completed_at, None);
    assert_eq!(raw["completedAt"], serde_json::Value::Null);
    assert_eq!(raw["status"], "failed");
    assert_eq!(raw["stage"], "installation");

    // The provenance the run *did* gather is all still there. This is the
    // point of writing early: a failed install is exactly when a user
    // needs to know which node image and which chart version were in
    // play.
    assert_eq!(
        manifest.baseline.node_image,
        format!("kindest/node:v{}", lab.baseline.kubernetes)
    );
    assert_eq!(
        manifest.baseline.kubernetes_version,
        lab.baseline.kubernetes
    );
    assert_eq!(
        manifest.baseline.node_image_digest.as_deref(),
        Some("sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed")
    );
    assert_eq!(manifest.tools.kind.as_deref(), Some("v0.33.0"));
    assert_eq!(manifest.candidate.components.len(), 1);

    let _ = std::fs::remove_dir_all(&root);
}

/// Every intermediate write leaves a document that parses, so a crash
/// between any two stages is recoverable rather than a truncated file.
///
/// Also pins the in-progress semantics: `status` stays `in_progress` and
/// `completedAt` stays null no matter how many stages have completed —
/// only `complete` may change either.
#[test]
fn every_stage_write_leaves_a_parseable_in_progress_manifest() {
    let root = unique_run_root("stage-writes");
    let runner = LabRunner {
        cluster_manager: Arc::new(FakeClusterManager::new()),
        artifact_store: ArtifactStore::new(&root),
    };
    let options = RunOptions {
        keep_clusters: false,
        run_root: root.clone(),
    };
    let lab = minimal_resolved_lab();

    let paths = test_runtime().block_on(async {
        let (run_id, paths) = runner
            .create_workspace(&options)
            .await
            .expect("workspace creation must succeed");
        let images = runner
            .resolve_node_images(&lab)
            .await
            .expect("node image resolution must succeed");
        let mut manifest = RunManifestWriter::create(
            ArtifactStore::new(&root),
            &paths,
            initial_manifest(&run_id, &lab, &images.baseline, &images.candidate),
        )
        .await
        .expect("the initial manifest must be writable");

        for stage in [
            RunStage::Started,
            RunStage::ClusterCreation,
            RunStage::Installation,
            RunStage::FixtureCapture,
            RunStage::Comparison,
            RunStage::Reporting,
        ] {
            manifest
                .record(stage, |_| {})
                .await
                .expect("each stage write must succeed");

            let (written, raw) = read_manifest(&paths);
            assert_eq!(written.stage, stage);
            assert_eq!(written.status, RunStatus::InProgress);
            assert_eq!(raw["completedAt"], serde_json::Value::Null);
            // The file and the writer's own copy never disagree.
            assert_eq!(&written, manifest.manifest());
        }

        paths
    });

    assert!(paths.run_json().is_file());
    let _ = std::fs::remove_dir_all(&root);
}

/// `record` may revise the manifest's contents, not only its stage — the
/// mechanism the pipeline uses to replace each side's configured
/// component versions with the versions actually installed.
#[test]
fn recording_a_stage_can_revise_the_manifest_contents() {
    let root = unique_run_root("revise");
    let runner = LabRunner {
        cluster_manager: Arc::new(FakeClusterManager::new()),
        artifact_store: ArtifactStore::new(&root),
    };
    let options = RunOptions {
        keep_clusters: false,
        run_root: root.clone(),
    };
    let lab = minimal_resolved_lab();

    let paths = test_runtime().block_on(async {
        let (run_id, paths) = runner
            .create_workspace(&options)
            .await
            .expect("workspace creation must succeed");
        let images = runner
            .resolve_node_images(&lab)
            .await
            .expect("node image resolution must succeed");
        let mut manifest = RunManifestWriter::create(
            ArtifactStore::new(&root),
            &paths,
            initial_manifest(&run_id, &lab, &images.baseline, &images.candidate),
        )
        .await
        .expect("the initial manifest must be writable");

        assert_eq!(manifest.manifest().candidate.components[0].version, "1.0.0");

        manifest
            .record(RunStage::Installation, |manifest| {
                manifest.candidate.components = vec![ComponentProvenance {
                    name: "configured".to_owned(),
                    version: "1.0.0+resolved".to_owned(),
                    source_sha256: None,
                }];
            })
            .await
            .expect("recording the install stage must succeed");

        paths
    });

    let (manifest, _) = read_manifest(&paths);
    assert_eq!(
        manifest.candidate.components[0].version, "1.0.0+resolved",
        "the installed version must replace the configured one on disk, not only in memory"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Completion is the only thing that sets `completedAt` — and a failure
/// recorded afterwards takes it away again, so a manifest can never claim
/// both a completion time and a failure (Global Constraint 15).
#[test]
fn completion_sets_the_timestamp_and_a_later_failure_clears_it() {
    let root = unique_run_root("complete-then-fail");
    let runner = LabRunner {
        cluster_manager: Arc::new(FakeClusterManager::new()),
        artifact_store: ArtifactStore::new(&root),
    };
    let options = RunOptions {
        keep_clusters: false,
        run_root: root.clone(),
    };
    let lab = minimal_resolved_lab();
    let finished = SystemTime::UNIX_EPOCH + Duration::new(1_788_265_050, 0);

    let paths = test_runtime().block_on(async {
        let (run_id, paths) = runner
            .create_workspace(&options)
            .await
            .expect("workspace creation must succeed");
        let images = runner
            .resolve_node_images(&lab)
            .await
            .expect("node image resolution must succeed");
        let mut manifest = RunManifestWriter::create(
            ArtifactStore::new(&root),
            &paths,
            initial_manifest(&run_id, &lab, &images.baseline, &images.candidate),
        )
        .await
        .expect("the initial manifest must be writable");

        manifest
            .complete(finished)
            .await
            .expect("completing must succeed");
        let (written, raw) = read_manifest(&paths);
        assert_eq!(written.status, RunStatus::Completed);
        assert_eq!(written.stage, RunStage::Completed);
        assert_eq!(written.completed_at, Some(finished));
        assert_eq!(raw["completedAt"], "2026-09-01T12:17:30.000000000Z");

        manifest
            .fail(RunStage::Reporting)
            .await
            .expect("failing must succeed");

        paths
    });

    let (manifest, raw) = read_manifest(&paths);
    assert_eq!(manifest.status, RunStatus::Failed);
    assert_eq!(manifest.stage, RunStage::Reporting);
    assert_eq!(
        manifest.completed_at, None,
        "a failed run must never carry a completion time"
    );
    assert_eq!(raw["completedAt"], serde_json::Value::Null);
    let _ = std::fs::remove_dir_all(&root);
}

/// `create_workspace`'s own contract, which the manifest now depends on:
/// a relative run root is rejected before anything is created, so a
/// caller cannot end up with a manifest path it cannot write to.
#[test]
fn create_workspace_rejects_a_relative_run_root_before_creating_anything() {
    let runner = LabRunner {
        cluster_manager: Arc::new(FakeClusterManager::new()),
        artifact_store: ArtifactStore::new(std::path::Path::new("relative/runs")),
    };
    let options = RunOptions {
        keep_clusters: false,
        run_root: PathBuf::from("relative/runs"),
    };

    let error = test_runtime()
        .block_on(runner.create_workspace(&options))
        .expect_err("a relative run root must be rejected");
    assert!(
        matches!(error, admissionlab_core::RunError::NonAbsoluteRunRoot(_)),
        "expected NonAbsoluteRunRoot, got {error:?}"
    );
    assert!(
        !std::path::Path::new("relative").exists(),
        "nothing must be created for a rejected run root"
    );
}
