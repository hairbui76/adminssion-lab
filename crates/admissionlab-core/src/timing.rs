//! How long each stage of a run actually took (ROADMAP Task 5.7).
//!
//! [`StageTimings`] is a plain, serializable record of one run's stage
//! durations; [`TimingRecorder`] is the monotonic recorder that produces
//! one. Together they answer the question PRODUCT.md §33 sets targets
//! for — "typical `kind` cluster creation under approximately 90 seconds
//! per cluster", "semantic comparison of 100 ordinary fixtures in under
//! one second", "100-fixture admission suite ... within approximately
//! five minutes excluding component installation" — with measurements
//! rather than with an impression, and they are the evidence ROADMAP Task
//! 5.8's serial-versus-parallel decision is made from.
//!
//! # Monotonic, always
//!
//! Every duration here comes from [`Instant`], never from
//! [`std::time::SystemTime`]. A lab run creates two clusters, installs
//! two stacks, and replays a corpus; on a CI runner that is minutes of
//! wall-clock during which NTP can step the system clock backwards, and a
//! stage that reported a negative or wildly inflated duration because of
//! that would be worse than one that reported nothing. `Instant` cannot
//! go backwards, which is the only property a duration measurement
//! actually needs.
//!
//! The one place a `SystemTime` still appears in a run's own record is
//! [`crate::run_manifest::RunManifest`]'s `startedAt`/`completedAt`,
//! which answer "when", not "how long" — a different question, correctly
//! answered by a wall clock.
//!
//! # Absent is absent, never zero (Global Constraint 15)
//!
//! Every stage is an `Option` and every absent one is *omitted from the
//! serialized document*, not written as `0`. A run that failed during
//! installation never captured a fixture, and a `fixtureCapture` of zero
//! milliseconds would state that capturing a hundred fixtures was
//! instantaneous. The same rule applies one level down: a per-side
//! duration is present only for a side that actually ran, so a run whose
//! candidate cluster never came up reports the baseline's real number and
//! no candidate key at all.
//!
//! # Stage boundaries are [`crate::run_manifest::RunStage`]'s, not new ones
//!
//! [`TimedStage`] deliberately mirrors the manifest's own stage vocabulary
//! (Task 5.2) rather than inventing a parallel one, so "the run failed at
//! `fixture_capture`" in `run.json` and "`fixtureCapture` took 130s" in
//! `result.json` name the same stretch of the run. There is exactly one
//! addition, [`TimedStage::Cleanup`], because cleanup happens *after* the
//! last stage the manifest records and therefore has no `RunStage` of its
//! own — it is still a real, measurable, sometimes-slow part of a run, and
//! omitting it would leave a visible gap between the stages' sum and the
//! elapsed total.
//!
//! # Two ways a duration gets here, and only two
//!
//! - **A scope.** [`TimingRecorder::stage`] and [`TimingRecorder::side`]
//!   return a [`StageScope`] that records its own elapsed time when it is
//!   dropped. That is how the whole-stage and per-side numbers are taken.
//! - **An observation somebody else already made.** Per-component install
//!   durations are *not* measured again here: they are
//!   [`crate::run::InstalledComponent::elapsed`], which the installer
//!   measured around its own `helm`/`kubectl` invocation and readiness
//!   wait. Measuring a second time around the same call would produce a
//!   slightly different number for the same thing, and then a reader
//!   would have to decide which one to believe.
//!
//! # Per-side numbers need a decorator, and here is why
//!
//! [`crate::run::LabRunner`] drives both sides concurrently
//! (`tokio::join!` — see its own module documentation for why never
//! `try_join!`), so a timer wrapped around `create_clusters`,
//! `install_stacks`, `capture_fixtures` or `cleanup` measures the *pair*
//! and can say nothing about either side alone. PRODUCT.md §33's cluster
//! target is stated per cluster, so the per-side number is the one that
//! matters most.
//!
//! [`TimedClusterManager`], [`TimedStackInstaller`] and
//! [`TimedFixtureCapture`] are that measurement, placed at the only seam
//! where one side is distinguishable from the other: the trait call that
//! takes a side. Each is a transparent decorator — it forwards every
//! method, changes no behavior, and returns exactly what it was given —
//! so wrapping a backend cannot alter what a run does, only what is known
//! about how long it took.
//!
//! # What a [`StageTimings`] inside a `result.json` cannot contain
//!
//! `admissionlab_report::LabResult` carries a [`StageTimings`] snapshot,
//! and that snapshot is necessarily taken *before* the document is
//! rendered and written, because the document contains it. So a
//! `result.json`'s timings never carry `reportingMs`, and never carry
//! `cleanup`: at the instant the value was frozen, neither stage had
//! happened. That is an absence, and it reads as one — there is no `0`
//! anywhere claiming those stages were free.
//!
//! Both are still recorded, and both are still reported: `admissionlab`
//! prints [`StageTimings::summary_line`] of a *final* snapshot, taken
//! after cleanup, as the last line of a run. One renderer, two snapshots
//! of the same recorder, no second format to drift.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Serialize, Serializer};

use crate::artifact::RunPaths;
use crate::cluster::{
    ClusterDiagnostics, ClusterError, ClusterHandle, ClusterManager, ClusterSpec,
};
use crate::run::{
    FixtureCapture, FixtureCaptureError, InstalledComponent, SideCapture, SideInstall,
    StackInstallError, StackInstaller,
};
use crate::side::Side;
use admissionlab_spec::ResolvedComponent;

// =========================================================================
// The record
// =========================================================================

/// How long each stage of one run took.
///
/// Produced by [`TimingRecorder::snapshot`]. Every stage is optional and
/// an absent one is omitted entirely — see this module's documentation
/// for why that is a correctness requirement rather than a formatting
/// preference.
///
/// Wire names are pinned literals in `camelCase`, and every duration is
/// serialized as whole milliseconds under a key that says so
/// (`...Ms`). Milliseconds because that is the resolution at which every
/// number here is meaningful: the fastest stage a run has (semantic
/// comparison) is budgeted in hundreds of milliseconds, and the slowest
/// (cluster creation) in tens of seconds, so sub-millisecond precision
/// would be noise printed to four extra digits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StageTimings {
    /// Creating both ephemeral clusters.
    #[serde(rename = "clusterCreation", skip_serializing_if = "Option::is_none")]
    pub cluster_creation: Option<SideStage>,
    /// Installing both sides' component stacks, per side and per
    /// component.
    #[serde(rename = "installation", skip_serializing_if = "Option::is_none")]
    pub installation: Option<InstallStage>,
    /// Replaying the fixture corpus through both sides.
    #[serde(rename = "fixtureCapture", skip_serializing_if = "Option::is_none")]
    pub fixture_capture: Option<CaptureStage>,
    /// Applying the Gateway suite to both sides, observing every route's
    /// reconciliation, and sending or skipping every traffic probe
    /// (ROADMAP Task 6.11).
    ///
    /// Absent — the key omitted entirely — for a lab with no `gateway:`
    /// section, which never runs the stage. That is the same rule every
    /// other field here follows and the reason it is optional: an
    /// admission-only run has no Gateway stage that took zero time, it
    /// has none at all.
    #[serde(rename = "gatewaySuite", skip_serializing_if = "Option::is_none")]
    pub gateway_suite: Option<SideStage>,
    /// Normalizing both sides' evidence, diffing it, attributing the
    /// first divergence, grading it against the policy and the
    /// expectations, and assembling the result — exactly the stretch
    /// [`crate::run_manifest::RunStage::Comparison`] covers.
    #[serde(
        rename = "comparisonMs",
        serialize_with = "serialize_optional_millis",
        skip_serializing_if = "Option::is_none"
    )]
    pub comparison: Option<Duration>,
    /// Redacting the result once and rendering and writing every view of
    /// it.
    ///
    /// Structurally absent from a `result.json`, which is one of the
    /// files this stage writes — see this module's documentation.
    #[serde(
        rename = "reportingMs",
        serialize_with = "serialize_optional_millis",
        skip_serializing_if = "Option::is_none"
    )]
    pub reporting: Option<Duration>,
    /// Deleting both clusters. Absent for a `--keep-clusters` run, which
    /// deletes nothing, and absent from a `result.json` for the same
    /// reason `reportingMs` is.
    #[serde(rename = "cleanup", skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<SideStage>,
    /// Wall-clock from the moment this run started being timed to the
    /// moment this snapshot was taken.
    ///
    /// Always present, and never the sum of the stages above: it also
    /// covers loading and validating the configuration, hashing the
    /// inputs, probing the host, and every gap between stages. The
    /// difference between this and the stages' sum is itself the useful
    /// number when it grows.
    #[serde(rename = "elapsedMs", serialize_with = "serialize_millis")]
    pub elapsed: Duration,
}

/// A stage both sides run concurrently: the pair's wall-clock, plus each
/// side's own.
///
/// The wall-clock is not the sum and not the maximum of the two sides —
/// it is what a caller waiting on both actually waited. It is usually a
/// little above the slower side (the join's own overhead) and is the
/// number that belongs in "the run spent this long here".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SideStage {
    /// How long the caller waited for both sides together.
    #[serde(rename = "wallMs", serialize_with = "serialize_millis")]
    pub wall: Duration,
    /// The baseline side's own duration, when that side ran.
    #[serde(
        rename = "baselineMs",
        serialize_with = "serialize_optional_millis",
        skip_serializing_if = "Option::is_none"
    )]
    pub baseline: Option<Duration>,
    /// The candidate side's own duration, when that side ran.
    #[serde(
        rename = "candidateMs",
        serialize_with = "serialize_optional_millis",
        skip_serializing_if = "Option::is_none"
    )]
    pub candidate: Option<Duration>,
}

/// The installation stage: both sides' wall-clock, and each side's own
/// per-component breakdown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct InstallStage {
    /// How long the caller waited for both sides' stacks together.
    #[serde(rename = "wallMs", serialize_with = "serialize_millis")]
    pub wall: Duration,
    /// The baseline side's stack, when that side installed one.
    #[serde(rename = "baseline", skip_serializing_if = "Option::is_none")]
    pub baseline: Option<SideInstallTiming>,
    /// The candidate side's stack, when that side installed one.
    #[serde(rename = "candidate", skip_serializing_if = "Option::is_none")]
    pub candidate: Option<SideInstallTiming>,
}

/// One side's stack installation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SideInstallTiming {
    /// How long this side's whole stack took to install and become
    /// ready.
    #[serde(rename = "elapsedMs", serialize_with = "serialize_millis")]
    pub elapsed: Duration,
    /// One entry per component, in install order.
    ///
    /// Three distinguishable states, deliberately: absent means this side
    /// never reported a breakdown (its install failed before it could),
    /// `[]` means it installed successfully with no components at all —
    /// a bare cluster is a legitimate side — and a non-empty list is the
    /// breakdown. Collapsing the first two into `[]` would state that a
    /// failed install installed nothing, which is usually false.
    #[serde(rename = "components", skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<ComponentTiming>>,
}

/// One component's install-and-readiness duration.
///
/// Not measured by this module: it is
/// [`crate::run::InstalledComponent::elapsed`] verbatim, the installer's
/// own measurement around its own work. See this module's documentation
/// ("Two ways a duration gets here").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentTiming {
    /// The component's name, as the lab configuration named it.
    #[serde(rename = "name")]
    pub name: String,
    /// How long it took to install and become ready.
    #[serde(rename = "elapsedMs", serialize_with = "serialize_millis")]
    pub elapsed: Duration,
}

impl From<&InstalledComponent> for ComponentTiming {
    fn from(component: &InstalledComponent) -> Self {
        Self {
            name: component.name.clone(),
            elapsed: component.elapsed,
        }
    }
}

/// The fixture-capture stage: both sides' wall-clock, each side's own,
/// and how many fixtures each side replayed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CaptureStage {
    /// How long the caller waited for both sides' capture together.
    #[serde(rename = "wallMs", serialize_with = "serialize_millis")]
    pub wall: Duration,
    /// The baseline side's own capture duration, when that side ran.
    #[serde(
        rename = "baselineMs",
        serialize_with = "serialize_optional_millis",
        skip_serializing_if = "Option::is_none"
    )]
    pub baseline: Option<Duration>,
    /// The candidate side's own capture duration, when that side ran.
    #[serde(
        rename = "candidateMs",
        serialize_with = "serialize_optional_millis",
        skip_serializing_if = "Option::is_none"
    )]
    pub candidate: Option<Duration>,
    /// How many fixtures each side replayed.
    ///
    /// Per side, not summed across both: both sides replay the same
    /// corpus (that is what makes their results comparable at all), so
    /// this is the corpus size, and dividing a side's own duration by it
    /// gives a per-fixture cost. Absent when no side completed its
    /// capture, because then nobody counted.
    #[serde(rename = "fixtures", skip_serializing_if = "Option::is_none")]
    pub fixtures: Option<usize>,
}

impl StageTimings {
    /// One compact line naming every stage that was measured.
    ///
    /// The single rendering of this type anywhere in the project: the
    /// terminal report prints it under a "Stage timings" heading, and
    /// `admissionlab` prints it again after cleanup with the two stages a
    /// `result.json` cannot contain. Two call sites, one format, nothing
    /// to drift.
    ///
    /// Stages that were not measured are omitted rather than rendered as
    /// zero, exactly as they are omitted from the serialized document.
    #[must_use]
    pub fn summary_line(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(clusters) = &self.cluster_creation {
            parts.push(format!("clusters {}", clusters.render()));
        }
        if let Some(install) = &self.installation {
            parts.push(format!("install {}", seconds(install.wall)));
        }
        if let Some(capture) = &self.fixture_capture {
            let mut rendered = format!(
                "capture {}",
                SideStage {
                    wall: capture.wall,
                    baseline: capture.baseline,
                    candidate: capture.candidate,
                }
                .render()
            );
            if let Some(fixtures) = capture.fixtures {
                let _: Result<(), std::fmt::Error> =
                    write!(rendered, " [{fixtures} fixture(s)/side]");
            }
            parts.push(rendered);
        }
        if let Some(gateway) = &self.gateway_suite {
            parts.push(format!("gateway {}", gateway.render()));
        }
        if let Some(comparison) = self.comparison {
            parts.push(format!("compare {}", seconds(comparison)));
        }
        if let Some(reporting) = self.reporting {
            parts.push(format!("report {}", seconds(reporting)));
        }
        if let Some(cleanup) = &self.cleanup {
            parts.push(format!("cleanup {}", cleanup.render()));
        }
        parts.push(format!("elapsed {}", seconds(self.elapsed)));
        parts.join(", ")
    }
}

impl SideStage {
    /// `12.34s (baseline 11.90s, candidate 12.10s)`, dropping whichever
    /// side did not run.
    fn render(&self) -> String {
        let mut sides: Vec<String> = Vec::new();
        if let Some(baseline) = self.baseline {
            sides.push(format!("baseline {}", seconds(baseline)));
        }
        if let Some(candidate) = self.candidate {
            sides.push(format!("candidate {}", seconds(candidate)));
        }
        if sides.is_empty() {
            seconds(self.wall)
        } else {
            format!("{} ({})", seconds(self.wall), sides.join(", "))
        }
    }
}

/// A duration as seconds with two decimals — the resolution at which
/// every stage in a lab run is worth reading.
fn seconds(duration: Duration) -> String {
    format!("{:.2}s", duration.as_secs_f64())
}

/// Serializes a [`Duration`] as whole milliseconds.
///
/// Saturating at [`u64::MAX`] rather than wrapping: a run long enough to
/// overflow would have to last half a billion years, and a wrapped tiny
/// number would be a lie where a saturated enormous one is merely
/// useless.
fn serialize_millis<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_u64(u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
}

/// [`serialize_millis`] for an `Option`. Only ever reached for `Some`:
/// every field using it also carries `skip_serializing_if`, so an absent
/// stage is omitted rather than written as `null` — see this module's
/// documentation.
///
/// `clippy::ref_option` would prefer `Option<&Duration>`, and `serde`
/// cannot supply one: `serialize_with` is called with a reference to the
/// field itself, and the field is an `Option<Duration>`.
#[allow(clippy::ref_option)]
fn serialize_optional_millis<S: Serializer>(
    value: &Option<Duration>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(duration) => serialize_millis(duration, serializer),
        None => serializer.serialize_none(),
    }
}

// =========================================================================
// The recorder
// =========================================================================

/// A stage that is measured as a whole.
///
/// Mirrors [`crate::run_manifest::RunStage`] one for one, plus
/// [`TimedStage::Cleanup`] — see this module's documentation for why
/// cleanup is the one addition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimedStage {
    /// Creating both ephemeral clusters
    /// ([`crate::run_manifest::RunStage::ClusterCreation`]).
    ClusterCreation,
    /// Installing both sides' stacks
    /// ([`crate::run_manifest::RunStage::Installation`]).
    Installation,
    /// Replaying the corpus through both sides
    /// ([`crate::run_manifest::RunStage::FixtureCapture`]).
    FixtureCapture,
    /// Running the Gateway suite on both sides
    /// ([`crate::run_manifest::RunStage::GatewaySuite`]).
    GatewaySuite,
    /// Normalizing, diffing, and grading
    /// ([`crate::run_manifest::RunStage::Comparison`]).
    Comparison,
    /// Redacting and rendering every report
    /// ([`crate::run_manifest::RunStage::Reporting`]).
    Reporting,
    /// Deleting both clusters. The one stage with no `RunStage`: it runs
    /// after the manifest's last recorded stage.
    Cleanup,
}

/// A stage that additionally has a per-side measurement.
///
/// A separate enum rather than a `side: Option<Side>` parameter on
/// [`TimedStage`], so that "the comparison stage on the candidate side"
/// — which is not a thing; comparison is one pure computation over both
/// sides' evidence — cannot be expressed at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimedSideStage {
    /// One side's cluster creation.
    ClusterCreation,
    /// One side's stack installation.
    Installation,
    /// One side's fixture capture.
    FixtureCapture,
    /// One side's Gateway suite run.
    GatewaySuite,
    /// One side's cluster deletion.
    Cleanup,
}

impl TimedSideStage {
    /// The whole-stage this side-scoped measurement belongs to.
    #[must_use]
    pub const fn stage(self) -> TimedStage {
        match self {
            Self::ClusterCreation => TimedStage::ClusterCreation,
            Self::Installation => TimedStage::Installation,
            Self::FixtureCapture => TimedStage::FixtureCapture,
            Self::GatewaySuite => TimedStage::GatewaySuite,
            Self::Cleanup => TimedStage::Cleanup,
        }
    }
}

/// Everything recorded so far, behind the recorder's lock.
#[derive(Debug, Default)]
struct Recorded {
    cluster_creation: SideRecord,
    installation: SideRecord,
    install_components: SideComponents,
    fixture_capture: SideRecord,
    fixtures: Option<usize>,
    gateway_suite: SideRecord,
    comparison: Option<Duration>,
    reporting: Option<Duration>,
    cleanup: SideRecord,
}

/// One stage's whole-stage and per-side durations.
#[derive(Debug, Default)]
struct SideRecord {
    wall: Option<Duration>,
    baseline: Option<Duration>,
    candidate: Option<Duration>,
}

impl SideRecord {
    /// Records `elapsed` for `side`, or for the stage as a whole when
    /// `side` is `None`.
    fn set(&mut self, side: Option<Side>, elapsed: Duration) {
        match side {
            None => self.wall = Some(elapsed),
            Some(Side::Baseline) => self.baseline = Some(elapsed),
            Some(Side::Candidate) => self.candidate = Some(elapsed),
        }
    }

    /// This stage as it is published, or `None` when nothing about it was
    /// measured at all.
    ///
    /// A stage with per-side numbers but no whole-stage one publishes a
    /// wall of the slower side rather than nothing: that situation only
    /// arises if a caller times the sides without timing the pair, and a
    /// missing `wallMs` in a `SideStage` that exists would be a hole in
    /// the document's own shape.
    fn published(&self) -> Option<SideStage> {
        let wall = self
            .wall
            .or_else(|| match (self.baseline, self.candidate) {
                (Some(baseline), Some(candidate)) => Some(baseline.max(candidate)),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            })?;
        Some(SideStage {
            wall,
            baseline: self.baseline,
            candidate: self.candidate,
        })
    }
}

/// Each side's per-component install records.
#[derive(Debug, Default)]
struct SideComponents {
    baseline: Option<Vec<ComponentTiming>>,
    candidate: Option<Vec<ComponentTiming>>,
}

impl SideComponents {
    /// The list recorded for `side`, if that side reported one.
    fn get(&self, side: Side) -> Option<&Vec<ComponentTiming>> {
        match side {
            Side::Baseline => self.baseline.as_ref(),
            Side::Candidate => self.candidate.as_ref(),
        }
    }

    /// Records `components` as `side`'s install breakdown.
    fn set(&mut self, side: Side, components: Vec<ComponentTiming>) {
        match side {
            Side::Baseline => self.baseline = Some(components),
            Side::Candidate => self.candidate = Some(components),
        }
    }
}

/// The monotonic recorder one run's [`StageTimings`] is taken from.
///
/// Cheaply cloneable and safe to share across the concurrent per-side
/// futures a run drives: every clone writes into the same record. Cloning
/// does **not** restart the elapsed clock — every clone reports the same
/// [`StageTimings::elapsed`], because they are all views of one run.
///
/// A poisoned lock is recovered from rather than propagated
/// ([`PoisonError::into_inner`]): a panic somewhere else in the process
/// must not turn "this run cannot be timed" into "this run cannot
/// finish". The worst case is one stage's duration being missing, which
/// this type's whole contract already treats as an ordinary, honestly
/// representable outcome.
#[derive(Debug, Clone)]
pub struct TimingRecorder {
    /// When this run started being timed.
    started: Instant,
    /// The shared record every clone and every scope writes into.
    recorded: Arc<Mutex<Recorded>>,
}

impl Default for TimingRecorder {
    fn default() -> Self {
        Self::start()
    }
}

impl TimingRecorder {
    /// Starts timing a run, now.
    #[must_use]
    pub fn start() -> Self {
        Self {
            started: Instant::now(),
            recorded: Arc::new(Mutex::new(Recorded::default())),
        }
    }

    /// Times `stage` as a whole, until the returned scope is dropped.
    pub fn stage(&self, stage: TimedStage) -> StageScope {
        StageScope {
            recorder: self.clone(),
            stage,
            side: None,
            started: Instant::now(),
        }
    }

    /// Times `side`'s half of `stage`, until the returned scope is
    /// dropped.
    pub fn side(&self, stage: TimedSideStage, side: Side) -> StageScope {
        StageScope {
            recorder: self.clone(),
            stage: stage.stage(),
            side: Some(side),
            started: Instant::now(),
        }
    }

    /// Records `side`'s per-component install durations, as the installer
    /// itself measured them.
    pub fn record_components(&self, side: Side, components: Vec<ComponentTiming>) {
        self.with(|recorded| recorded.install_components.set(side, components));
    }

    /// Records how many fixtures one side replayed.
    ///
    /// Both sides replay the same corpus, so this is called once per side
    /// with the same value; the last writer wins and they agree.
    pub fn record_fixture_count(&self, fixtures: usize) {
        self.with(|recorded| recorded.fixtures = Some(fixtures));
    }

    /// Everything measured so far, as the published record.
    ///
    /// Takeable at any point in a run, and meant to be: a `result.json`
    /// carries a snapshot from before its own reporting stage, and the
    /// final console line carries one from after cleanup.
    #[must_use]
    pub fn snapshot(&self) -> StageTimings {
        let elapsed = self.started.elapsed();
        let recorded = self.recorded.lock().unwrap_or_else(PoisonError::into_inner);
        let installation = recorded.installation.published().map(|stage| InstallStage {
            wall: stage.wall,
            baseline: side_install(&recorded.install_components, Side::Baseline, stage.baseline),
            candidate: side_install(
                &recorded.install_components,
                Side::Candidate,
                stage.candidate,
            ),
        });
        let fixture_capture = recorded
            .fixture_capture
            .published()
            .map(|stage| CaptureStage {
                wall: stage.wall,
                baseline: stage.baseline,
                candidate: stage.candidate,
                fixtures: recorded.fixtures,
            });
        StageTimings {
            cluster_creation: recorded.cluster_creation.published(),
            installation,
            fixture_capture,
            gateway_suite: recorded.gateway_suite.published(),
            comparison: recorded.comparison,
            reporting: recorded.reporting,
            cleanup: recorded.cleanup.published(),
            elapsed,
        }
    }

    /// Records one measured duration.
    fn record(&self, stage: TimedStage, side: Option<Side>, elapsed: Duration) {
        self.with(|recorded| match stage {
            TimedStage::ClusterCreation => recorded.cluster_creation.set(side, elapsed),
            TimedStage::Installation => recorded.installation.set(side, elapsed),
            TimedStage::FixtureCapture => recorded.fixture_capture.set(side, elapsed),
            TimedStage::GatewaySuite => recorded.gateway_suite.set(side, elapsed),
            TimedStage::Comparison => recorded.comparison = Some(elapsed),
            TimedStage::Reporting => recorded.reporting = Some(elapsed),
            TimedStage::Cleanup => recorded.cleanup.set(side, elapsed),
        });
    }

    /// Runs `update` against the shared record, recovering a poisoned
    /// lock rather than panicking (see this type's own documentation).
    fn with<F: FnOnce(&mut Recorded)>(&self, update: F) {
        let mut recorded = self.recorded.lock().unwrap_or_else(PoisonError::into_inner);
        update(&mut recorded);
    }
}

/// One side's published install timing: its own measured duration plus
/// whatever component breakdown that side reported.
///
/// Keyed on the *duration*: a side that was never installed has nothing
/// to publish, and a side that was has a real number even when its
/// breakdown is missing (see [`SideInstallTiming::components`] for what
/// that combination means).
fn side_install(
    components: &SideComponents,
    side: Side,
    elapsed: Option<Duration>,
) -> Option<SideInstallTiming> {
    Some(SideInstallTiming {
        elapsed: elapsed?,
        components: components.get(side).cloned(),
    })
}

/// A running measurement that records itself when it is dropped.
///
/// Dropping is the recording, which is what makes a stage's timer
/// impossible to forget on an early return: every `?`, every `return`,
/// and every error path runs it. The cost of that choice is stated
/// plainly: a scope dropped because its future was *cancelled* records
/// the time up to the cancellation, which is a true measurement of a
/// stage that did not finish. Nothing in this project cancels a stage
/// future (`LabRunner` uses `tokio::join!`, never `try_join!`, precisely
/// so that no in-flight side is ever abandoned), so that case does not
/// arise today.
#[must_use = "a stage scope records nothing until it is dropped; bind it to a name"]
#[derive(Debug)]
pub struct StageScope {
    /// Where the measurement lands.
    recorder: TimingRecorder,
    /// Which stage is being measured.
    stage: TimedStage,
    /// Which side, or the stage as a whole.
    side: Option<Side>,
    /// When this scope began.
    started: Instant,
}

impl Drop for StageScope {
    fn drop(&mut self) {
        self.recorder
            .record(self.stage, self.side, self.started.elapsed());
    }
}

// =========================================================================
// Decorators: the only place a per-side duration can be observed
// =========================================================================

/// A [`ClusterManager`] that times each side's create and delete.
///
/// Transparent: every method forwards to the wrapped manager and returns
/// its result unchanged. `resolve_node_image` and `diagnostics` are
/// forwarded untimed — neither is a stage, and `resolve_node_image` runs
/// before the run has provisioned anything at all.
#[derive(Debug)]
pub struct TimedClusterManager<C> {
    /// The manager doing the actual work.
    inner: Arc<C>,
    /// Where the per-side durations land.
    timings: TimingRecorder,
}

impl<C> TimedClusterManager<C> {
    /// Wraps `inner`, recording into `timings`.
    #[must_use]
    pub fn new(inner: Arc<C>, timings: TimingRecorder) -> Self {
        Self { inner, timings }
    }
}

#[async_trait]
impl<C: ClusterManager> ClusterManager for TimedClusterManager<C> {
    async fn resolve_node_image(&self, kubernetes_version: &str) -> Result<String, ClusterError> {
        self.inner.resolve_node_image(kubernetes_version).await
    }

    async fn create(
        &self,
        spec: &ClusterSpec,
        paths: &RunPaths,
    ) -> Result<ClusterHandle, ClusterError> {
        let _scope = self
            .timings
            .side(TimedSideStage::ClusterCreation, spec.side);
        self.inner.create(spec, paths).await
    }

    async fn delete(&self, handle: &ClusterHandle) -> Result<(), ClusterError> {
        let _scope = self.timings.side(TimedSideStage::Cleanup, handle.spec.side);
        self.inner.delete(handle).await
    }

    async fn diagnostics(&self, handle: &ClusterHandle) -> ClusterDiagnostics {
        self.inner.diagnostics(handle).await
    }
}

/// A [`StackInstaller`] that times each side's stack and copies out the
/// installer's own per-component durations.
///
/// The per-component numbers are *not* re-measured here — see this
/// module's documentation.
#[derive(Debug)]
pub struct TimedStackInstaller<I> {
    /// The installer doing the actual work.
    inner: I,
    /// Where the per-side and per-component durations land.
    timings: TimingRecorder,
}

impl<I> TimedStackInstaller<I> {
    /// Wraps `inner`, recording into `timings`.
    #[must_use]
    pub fn new(inner: I, timings: TimingRecorder) -> Self {
        Self { inner, timings }
    }
}

#[async_trait]
impl<I: StackInstaller> StackInstaller for TimedStackInstaller<I> {
    async fn install_stack(
        &self,
        cluster: &ClusterHandle,
        components: &[ResolvedComponent],
        component_timeout: Duration,
    ) -> Result<SideInstall, StackInstallError> {
        let installed = {
            let _scope = self
                .timings
                .side(TimedSideStage::Installation, cluster.spec.side);
            self.inner
                .install_stack(cluster, components, component_timeout)
                .await
        };
        if let Ok(side) = &installed {
            self.timings.record_components(
                side.side,
                side.components.iter().map(ComponentTiming::from).collect(),
            );
        }
        installed
    }
}

/// A [`FixtureCapture`] that times each side's capture and records how
/// many fixtures it replayed.
///
/// A failed capture records its own duration (the scope drops either way)
/// but no fixture count: a side that gave up part way through did not
/// replay the corpus, and reporting the corpus size as if it had would
/// make the per-fixture cost derived from it wrong.
#[derive(Debug)]
pub struct TimedFixtureCapture<F> {
    /// The capture doing the actual work.
    inner: F,
    /// Where the per-side durations and the fixture count land.
    timings: TimingRecorder,
}

impl<F> TimedFixtureCapture<F> {
    /// Wraps `inner`, recording into `timings`.
    #[must_use]
    pub fn new(inner: F, timings: TimingRecorder) -> Self {
        Self { inner, timings }
    }

    /// The wrapped capture.
    ///
    /// Exposed because a caller often needs something from the concrete
    /// implementation that [`FixtureCapture`] itself cannot express —
    /// `admissionlab-cli`'s `OutcomeCapture` is exactly that case, and
    /// implements itself for this type by forwarding through here.
    #[must_use]
    pub fn inner(&self) -> &F {
        &self.inner
    }
}

#[async_trait]
impl<F: FixtureCapture> FixtureCapture for TimedFixtureCapture<F> {
    async fn capture_side(
        &self,
        cluster: &ClusterHandle,
        side: Side,
        paths: &RunPaths,
    ) -> Result<SideCapture, FixtureCaptureError> {
        let captured = {
            let _scope = self.timings.side(TimedSideStage::FixtureCapture, side);
            self.inner.capture_side(cluster, side, paths).await
        };
        if let Ok(capture) = &captured {
            self.timings.record_fixture_count(capture.fixtures.len());
        }
        captured
    }
}
