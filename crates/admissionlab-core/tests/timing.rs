//! What the stage-timing recorder promises (ROADMAP Task 5.7).
//!
//! Three properties are worth a test each, and they are the three a
//! reader of a `result.json` relies on:
//!
//! 1. **A stage nobody measured is absent, not zero** (Global Constraint
//!    15). Asserted on the serialized document, because that is where a
//!    consumer sees it.
//! 2. **Per-side numbers survive concurrency.** The decorators are the
//!    only place a side is distinguishable, and both sides are driven at
//!    once, so a recorder that lost or overwrote one side's value would
//!    be silently wrong in exactly the case it exists for.
//! 3. **Per-component durations are the installer's own.** Not measured
//!    a second time here — a second measurement of the same work is a
//!    second answer nobody can adjudicate.
//!
//! The decorators are driven against hand-written fakes rather than real
//! `kind`/`helm` backends: the property under test is that a decorator
//! forwards faithfully and records the elapsed time of whatever it
//! wrapped, and a fake that sleeps a known amount proves that far more
//! precisely than a real cluster ever could.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use admissionlab_core::{
    ArtifactStore, CapturedFixture, ClusterDiagnostics, ClusterError, ClusterHandle,
    ClusterManager, ClusterSpec, ComponentTiming, FixtureCapture, FixtureCaptureError, FixtureId,
    InstalledComponent, RunId, RunPaths, Side, SideCapture, SideInstall, StackInstallError,
    StackInstaller, StageTimings, TimedClusterManager, TimedFixtureCapture, TimedSideStage,
    TimedStackInstaller, TimedStage, TimingRecorder,
};
use admissionlab_spec::ResolvedComponent;
use async_trait::async_trait;
use serde_json::Value;

/// How long every fake below takes, so an assertion can name a floor the
/// real elapsed time must clear. Small enough that the whole file runs in
/// well under a second, large enough that a timer resolution problem
/// would show up as a failure rather than as flakiness.
const FAKE_WORK: Duration = Duration::from_millis(30);

// ---------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------

/// A cluster backend that sleeps, then hands back a handle for whichever
/// side it was asked about.
struct SleepyClusters;

#[async_trait]
impl ClusterManager for SleepyClusters {
    async fn resolve_node_image(&self, kubernetes_version: &str) -> Result<String, ClusterError> {
        Ok(format!("kindest/node:v{kubernetes_version}"))
    }

    async fn create(
        &self,
        spec: &ClusterSpec,
        _paths: &RunPaths,
    ) -> Result<ClusterHandle, ClusterError> {
        tokio::time::sleep(FAKE_WORK).await;
        Ok(handle(spec.clone()))
    }

    async fn delete(&self, _handle: &ClusterHandle) -> Result<(), ClusterError> {
        tokio::time::sleep(FAKE_WORK).await;
        Ok(())
    }

    async fn diagnostics(&self, handle: &ClusterHandle) -> ClusterDiagnostics {
        ClusterDiagnostics {
            cluster_name: handle.spec.name.clone(),
            cluster_exists: None,
            kubeconfig_present: false,
            audit_log_present: false,
            notes: Vec::new(),
        }
    }
}

/// An installer that reports one component with a fixed, recognizable
/// `elapsed` that no timer in this process could produce by accident.
struct FixedInstaller;

/// The `elapsed` [`FixedInstaller`] reports. Deliberately far larger than
/// anything this test could actually spend, so a per-component duration
/// that equals it proves it was copied rather than re-measured.
const COMPONENT_ELAPSED: Duration = Duration::from_secs(600);

#[async_trait]
impl StackInstaller for FixedInstaller {
    async fn install_stack(
        &self,
        cluster: &ClusterHandle,
        _components: &[ResolvedComponent],
        _component_timeout: Duration,
    ) -> Result<SideInstall, StackInstallError> {
        tokio::time::sleep(FAKE_WORK).await;
        Ok(SideInstall {
            side: cluster.spec.side,
            components: vec![InstalledComponent {
                name: "test-webhook".to_owned(),
                method: "manifests".to_owned(),
                resolved_version: "1".to_owned(),
                started_at: std::time::SystemTime::UNIX_EPOCH,
                elapsed: COMPONENT_ELAPSED,
                diagnostics: Vec::new(),
            }],
        })
    }
}

/// A capture that reports a fixed number of replayed fixtures.
struct CountingCapture {
    /// How many fixtures each side "replayed".
    fixtures: usize,
}

#[async_trait]
impl FixtureCapture for CountingCapture {
    async fn capture_side(
        &self,
        _cluster: &ClusterHandle,
        side: Side,
        _paths: &RunPaths,
    ) -> Result<SideCapture, FixtureCaptureError> {
        tokio::time::sleep(FAKE_WORK).await;
        Ok(SideCapture {
            side,
            fixtures: (0..self.fixtures)
                .map(|index| CapturedFixture {
                    fixture_id: FixtureId::parse(&format!("pod-{index}"))
                        .expect("a generated fixture id is well formed"),
                    side,
                    artifact_dir: PathBuf::from("/raw"),
                    outcome_path: PathBuf::from("/raw/outcome.json"),
                    diagnostics: Vec::new(),
                })
                .collect(),
        })
    }
}

/// A cluster handle for `spec`, with paths nothing in this file reads.
fn handle(spec: ClusterSpec) -> ClusterHandle {
    ClusterHandle {
        spec,
        kubeconfig: PathBuf::from("/kubeconfig"),
        audit_log: PathBuf::from("/audit.log"),
    }
}

/// A spec for `side`.
fn spec(side: Side) -> ClusterSpec {
    ClusterSpec {
        side,
        name: format!("adlab-{side}"),
        kubernetes_version: "1.36.4".to_owned(),
        node_image: "kindest/node:v1.36.4".to_owned(),
    }
}

/// A run workspace under `root`, which every fake ignores but the trait
/// signatures require.
async fn paths(root: &Path) -> RunPaths {
    ArtifactStore::new(root)
        .create_run(&RunId::generate())
        .await
        .expect("a run workspace is created")
}

/// A fresh, guaranteed-unique directory under the system temp directory.
fn unique_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-core-timing-{}-{label}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create unique temp dir");
    dir
}

/// `timings` serialized, so the absence assertions read the same document
/// a consumer would.
fn serialized(timings: &StageTimings) -> Value {
    serde_json::to_value(timings).expect("stage timings serialize")
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[test]
fn a_recorder_that_measured_nothing_publishes_only_its_elapsed_time() {
    let document = serialized(&TimingRecorder::start().snapshot());
    let object = document.as_object().expect("an object");

    assert_eq!(
        object.keys().collect::<Vec<_>>(),
        vec!["elapsedMs"],
        "an unmeasured stage must be an absent key, never a zero: {document}"
    );
}

#[test]
fn every_stage_is_absent_until_its_scope_drops() {
    let recorder = TimingRecorder::start();
    let scope = recorder.stage(TimedStage::Comparison);

    assert!(
        recorder.snapshot().comparison.is_none(),
        "a stage still running has no duration yet, and must not report one"
    );

    drop(scope);
    assert!(
        recorder.snapshot().comparison.is_some(),
        "a dropped scope records"
    );
}

#[tokio::test]
async fn both_sides_of_a_concurrent_stage_keep_their_own_duration() {
    let recorder = TimingRecorder::start();
    let root = unique_temp_dir("clusters");
    let paths = paths(&root).await;
    let clusters = TimedClusterManager::new(Arc::new(SleepyClusters), recorder.clone());

    let baseline = spec(Side::Baseline);
    let candidate = spec(Side::Candidate);
    {
        let _stage = recorder.stage(TimedStage::ClusterCreation);
        let _ = tokio::join!(
            clusters.create(&baseline, &paths),
            clusters.create(&candidate, &paths),
        );
    }

    let created = recorder
        .snapshot()
        .cluster_creation
        .expect("both sides were created");
    assert!(
        created.baseline.is_some_and(|elapsed| elapsed >= FAKE_WORK),
        "the baseline side's own duration must survive the concurrent candidate one"
    );
    assert!(
        created
            .candidate
            .is_some_and(|elapsed| elapsed >= FAKE_WORK),
        "and the candidate's must survive the baseline's"
    );
    assert!(
        created.wall >= FAKE_WORK,
        "the pair's wall-clock is what the caller waited"
    );
}

#[tokio::test]
async fn a_component_duration_is_the_installers_own_measurement() {
    let recorder = TimingRecorder::start();
    let installer = TimedStackInstaller::new(FixedInstaller, recorder.clone());

    let reported = {
        let _stage = recorder.stage(TimedStage::Installation);
        installer
            .install_stack(&handle(spec(Side::Baseline)), &[], Duration::from_secs(1))
            .await
            .expect("the fake installer succeeds")
    };
    assert_eq!(reported.components.len(), 1, "the decorator is transparent");

    let stage = recorder
        .snapshot()
        .installation
        .expect("the baseline side installed");
    let baseline = stage.baseline.expect("the baseline side has a breakdown");
    assert_eq!(
        baseline.components,
        Some(vec![ComponentTiming {
            name: "test-webhook".to_owned(),
            elapsed: COMPONENT_ELAPSED,
        }]),
        "the component's duration is copied from `InstalledComponent::elapsed`, not re-measured"
    );
    assert!(
        baseline.elapsed < COMPONENT_ELAPSED,
        "the side's own duration is measured here and is nothing like the fake component's"
    );
}

#[tokio::test]
async fn a_capture_records_the_corpus_size_it_replayed() {
    let recorder = TimingRecorder::start();
    let root = unique_temp_dir("capture");
    let paths = paths(&root).await;
    let capture = TimedFixtureCapture::new(CountingCapture { fixtures: 100 }, recorder.clone());
    let baseline = handle(spec(Side::Baseline));
    let candidate = handle(spec(Side::Candidate));

    {
        let _stage = recorder.stage(TimedStage::FixtureCapture);
        let _ = tokio::join!(
            capture.capture_side(&baseline, Side::Baseline, &paths),
            capture.capture_side(&candidate, Side::Candidate, &paths),
        );
    }

    let captured = recorder
        .snapshot()
        .fixture_capture
        .expect("both sides captured");
    assert_eq!(
        captured.fixtures,
        Some(100),
        "the count is per side, not the sum of both"
    );
    assert!(captured.baseline.is_some() && captured.candidate.is_some());
}

#[test]
fn the_summary_line_names_only_the_stages_that_were_measured() {
    let recorder = TimingRecorder::start();
    drop(recorder.stage(TimedStage::Comparison));
    drop(recorder.side(TimedSideStage::Cleanup, Side::Baseline));

    let line = recorder.snapshot().summary_line();

    assert!(line.contains("compare "), "{line}");
    assert!(line.contains("cleanup "), "{line}");
    assert!(line.contains("baseline "), "{line}");
    assert!(
        !line.contains("clusters ") && !line.contains("install ") && !line.contains("capture "),
        "a stage that never ran must not appear at all: {line}"
    );
    assert!(
        line.contains("elapsed "),
        "the elapsed total is always present: {line}"
    );
}
