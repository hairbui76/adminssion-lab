//! The whole `admissionlab test` pipeline: configuration in, reports and
//! an exit disposition out.
//!
//! [`run_lab`] is the one function that owns the order of a lab run. It
//! is generic over [`LabBackend`], so the same code path that drives real
//! `kind` clusters, real `helm`/`kubectl` installs, and real server-side
//! dry-run capture is the one `tests/test_command.rs` drives against
//! fakes — the decisions under test (which failure maps to which exit
//! code, when reports are written, when cleanup runs) are made here, not
//! in the backends.
//!
//! # Why this half of the product lives above `admissionlab-core`
//!
//! `admissionlab_core::LabRunner` takes a run as far as clusters,
//! installs, and captured evidence, and stops. Everything after that —
//! normalize, semantic diff, first divergence, policy, expectations,
//! redaction, rendering — needs crates that are all *downstream* of
//! `admissionlab-core`: `admissionlab-diff` depends on
//! `admissionlab-admission` → `admissionlab-fixtures` →
//! `admissionlab-core`, `admissionlab-policy` depends on
//! `admissionlab-diff`, `admissionlab-report` on all three. A
//! `core -> policy` edge would close a cycle Cargo rejects outright.
//!
//! So the assembly lands here, in the one crate that depends on
//! everything and that nothing depends on. `admissionlab_core::run`'s own
//! module documentation states the same conclusion from the other side.
//!
//! # Stage order
//!
//! ```text
//! load + resolve configuration          |  no cluster exists yet:
//! validate the policy section           |  everything here is the
//! load expectations                     |  user's own input, and
//! discover fixtures                     |  every failure is exit 2
//! -------------------------------------------------------------------
//! check host prerequisites (doctor)     |  exit 2 (see `crate::exit`)
//! -------------------------------------------------------------------
//! create the run workspace + clusters   |  exit 3 (or 2 for a version
//!                                       |  Admission Lab cannot pin)
//! install both stacks                   |  exit 4
//! capture both sides' fixtures          |  exit 5
//! normalize -> diff -> first divergence |
//! evaluate policy + expectations        |
//! redact -> terminal + JSON + HTML      |
//! cleanup (always, unless --keep-clusters)
//! ```
//!
//! ## Every input check happens before any cluster is created
//!
//! ROADMAP Task 4.14 step 1 lists fixture discovery after stack
//! installation. This module deliberately hoists it — along with policy
//! validation and expectations loading — above cluster creation instead,
//! and the reason is what those failures *are*: ROADMAP §0.4 classes an
//! "invalid user configuration / fixture definition" as exit 2, a
//! category that exists precisely because it is not the lab's
//! infrastructure failing. Discovering a fixture with no
//! `metadata.name` only after provisioning two `kind` clusters and
//! installing two Helm stacks would charge a user several minutes to
//! learn something knowable in milliseconds, and nothing in discovery
//! depends on a cluster existing (`discover_fixtures` walks the
//! filesystem; resolving a fixture's `apiVersion`/`kind` against a live
//! cluster happens later, inside capture, and is exit 5 when it fails).
//!
//! `admissionlab_policy::validate_policy_spec`'s own documentation is
//! explicit that its call site must be "immediately after
//! `admissionlab_spec::resolve_lab` and before any cluster is created";
//! this is that call site, and the same reasoning covers its neighbors.
//!
//! ## Everything after cluster creation funnels through cleanup
//!
//! [`run_lab`] creates the clusters, then hands off to a single inner
//! function for every later stage, then always calls [`finish`]. That
//! shape is what makes ROADMAP step 4 ("always attempt cleanup unless
//! `--keep-clusters`") structural rather than a rule every early return
//! has to remember: there is exactly one path from "the clusters exist"
//! to "this function returns", and cleanup is on it.
//!
//! ## A later-stage failure still writes what it knows
//!
//! ROADMAP step 3. A failure at install, capture, comparison, or
//! rendering writes a `diagnostics.json`
//! ([`report::FailureArtifact`]) into the report directory *before*
//! cleanup runs, carrying the failure and every diagnostic collected up
//! to that point. What it does **not** write is a `result.json`: a
//! `LabResult` requires a policy verdict, and a run that never compared
//! both sides has not earned one. Manufacturing a `pass` there — or a
//! `fail` — would be exactly the fabrication Global Constraint 15
//! forbids, and it is the reason the failure path has its own artifact
//! shape rather than a half-filled result.
//!
//! ## Cancellation stops the next stage, never the current one
//!
//! ROADMAP Task 9.6. `crate::cancel` watches `SIGINT`/`SIGTERM` and sets
//! the [`Cancellation`] this run's backend handed over
//! ([`LabBackend::cancellation`]); this module reads it at each stage
//! boundary — before install, before capture, before the behavior
//! suites, and before the comparison — through [`stop_if_canceled`], and
//! answers by declining to start the next stage. Nothing is aborted.
//! Work already running finishes or hits its own bound (Global
//! Constraint 13 requires every external interaction to have one), which
//! is the only way a `kind create` in flight can be prevented from
//! leaking the cluster it is halfway through building.
//!
//! Teardown from there is the ordinary path: the boundary returns into
//! [`run_lab`]'s single route to [`finish`], long-lived children (a
//! `kubectl port-forward`) have already died with the stage that owned
//! them, the `diagnostics.json` is written before cleanup exactly as it
//! is for a failure, and both clusters are deleted unless
//! `--keep-clusters` asked for them. A canceled run therefore never
//! states a verdict, and never gets a second cleanup implementation to
//! keep in sync with the one every other path uses.
//!
//! The boundary is a *stage*, not a fixture: the per-fixture loop lives
//! inside a `FixtureCapture` implementation in another crate
//! (`admissionlab-admission`), and the handle is not threaded through
//! that trait. What bounds an already-started capture is what has always
//! bounded it — each fixture's own timeout — so "in flight" stays
//! finite; it is not instant, which is what the second interrupt
//! (`crate::cancel`) is for.
//!
//! ## The run manifest is written before anything is provisioned
//!
//! ROADMAP Task 5.2. `run.json` is created after every input has been
//! validated and hashed, the host has been probed, and both sides' node
//! images have been resolved — and *before* the first cluster is created
//! — then atomically rewritten after each stage. A run that dies at
//! install therefore leaves a valid manifest with `completedAt: null` and
//! the failed stage recorded, which is the difference between "rerun the
//! twenty-minute lab to find out what it was running" and reading a file.
//! See `admissionlab_core::run_manifest`'s own "Incremental writes"
//! section for the contract, and [`provenance`] for how the values are
//! assembled.
//!
//! ## The job summary is written whatever happens
//!
//! ROADMAP Task 5.4. `--github-summary` names one file the GitHub Action
//! appends to `$GITHUB_STEP_SUMMARY`, and the reason it is written by the
//! CLI rather than assembled in YAML is the same reason `result.json` is:
//! the action is a wrapper, and a summary composed in shell would be a
//! second renderer that can disagree with the first.
//!
//! Every path out of this module that has *anything* to say writes it —
//! a verdict summary alongside the reports when the run reached one
//! ([`report::write_reports`]), and [`report::render_no_verdict_summary`]
//! naming the failed stage when it did not, from the configuration stage
//! (before any cluster exists) through reporting. That is what lets the
//! action's summary step be an unconditional `cat` of one file rather
//! than a shell script deciding what a run meant.
//!
//! Writing it is always best-effort on the failure paths and never
//! changes an exit code: a run that already failed must not be re-failed,
//! or have its failure replaced, because a Markdown file could not be
//! written.
//!
//! ## A reproduction is this same pipeline, with its environment pinned
//!
//! ROADMAP Task 5.3. `admissionlab reproduce` does not have a second
//! engine: it verifies a source tree against a recorded manifest, builds
//! an `admissionlab_core::ReproductionPin` from that manifest, and then
//! runs *this* function. [`LabBackend::reproduction_pin`] is the whole of
//! the difference, and it changes exactly two things — the resolved lab's
//! versions, and where both sides' node images come from. Every other
//! stage, every failure mapping, and every artifact is identical, which
//! is the only way a reproduction and the run it reproduces can be
//! compared at all.
//!
//! Only the *first* manifest write is fatal: at that moment nothing has
//! been provisioned, and a run workspace that cannot be written to is one
//! whose later evidence would be untrustworthy anyway. Every later
//! rewrite failure is reported and the run continues — a manifest that
//! stopped being updated is a smaller loss than abandoning a comparison
//! whose clusters are already up.
//!
//! ## Every stage is timed, at the boundaries that already exist
//!
//! ROADMAP Task 5.7. One `admissionlab_core::TimingRecorder` is started
//! before the first file is read and threaded through every stage below
//! (inside [`Run`]), and each stage's timer is a scope opened at exactly
//! the boundary the run manifest already records — so "the run failed at
//! `fixture_capture`" and "`fixtureCapture` took 130s" name the same
//! stretch of the run rather than two overlapping ones. There are no
//! parallel stage boundaries invented for measurement.
//!
//! Per-side numbers cannot be taken here, because `LabRunner` runs both
//! sides at once: a timer around `create_clusters` measures the pair.
//! They come from `admissionlab_core`'s three transparent decorators,
//! wrapped around the backend's own cluster manager, installer and
//! capture at their construction sites below, where the trait call names
//! a side.
//!
//! The result is that a `result.json` carries the run's own cost
//! (`admissionlab_report::LabResult::timings`) and the run's last console
//! line carries the two stages the document cannot contain — reporting,
//! which writes it, and cleanup, which follows it.

pub mod capture;
pub mod certification;
pub mod compare;
pub mod gateway;
pub mod install;
pub mod migration;
pub mod provenance;
pub mod report;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use admissionlab_admission::AdmissionOutcome;
use admissionlab_core::{
    ArtifactStore, Cancellation, ClusterManager, Diagnostic, DoctorReport, InstalledLab, LabRunner,
    PreparedLab, RedactedValue, ReproductionPin, ResolvedNodeImages, RunDisposition, RunError,
    RunManifestWriter, RunOptions, RunPaths, RunStage, Side, StackInstaller, TimedClusterManager,
    TimedFixtureCapture, TimedSideStage, TimedStackInstaller, TimedStage, TimingRecorder,
    manual_cluster_deletion_commands, preserved_cluster_report,
};
use admissionlab_fixtures::{FixtureSource, discover_fixtures};
use admissionlab_gateway::GatewayCaseResult;
use admissionlab_policy::{
    ResolvedExpectations, ResolvedPolicy, evaluate_with_expectations, load_expectations,
    resolve_policy, validate_policy_spec,
};
use admissionlab_report::{MigrationCaseComparison, TerminalOptions};
use admissionlab_spec::{
    GatewaySuiteSpec, MigrationSuiteSpec, ResolvedLab, declared_api_version, load_any_supported_lab,
};
use async_trait::async_trait;

pub use capture::OutcomeCapture;
pub use gateway::{GatewaySuiteError, GatewaySuiteRunner, SideGatewayOutcome};
pub use migration::{MigrationRunOutcome, MigrationSuiteError, MigrationSuiteRunner};

/// How long each component gets to install *and* become ready before its
/// side's stack is failed.
///
/// Ten minutes, per component. Sized for the real Alpha targets on a
/// cold machine rather than for the happy path: an Istio or Kyverno
/// chart on a fresh `kind` node pulls several container images before
/// anything can report ready, and a CI runner with no warm image cache
/// routinely spends minutes there. The bound exists because Global
/// Constraint 13 requires every external interaction to have one, not
/// because a shorter value would be more useful — a timeout that fires
/// on a slow image pull turns a working stack into a spurious exit 4.
/// `admissionlab_installer::stack::install_stack`'s own documentation
/// describes how a component spends this budget across its install and
/// its readiness checks.
pub const DEFAULT_COMPONENT_TIMEOUT: Duration = Duration::from_secs(600);

/// One `admissionlab test` invocation's inputs.
#[derive(Debug, Clone)]
pub struct RunRequest<'a> {
    /// The lab configuration to run.
    pub config: &'a Path,
    /// Leave both clusters running instead of deleting them.
    pub keep_clusters: bool,
    /// Where to write `result.json`/`report.html`. `None` writes them
    /// into this run's own `reports/` directory under [`Self::run_root`].
    pub report_dir: Option<&'a Path>,
    /// Where to write the GitHub Actions job summary, or `None` to write
    /// none. Any path the caller chose — it is not required to be inside
    /// [`Self::report_dir`], and its parent directory is created if it
    /// does not exist. See this module's "The job summary is written
    /// whatever happens" section.
    pub github_summary: Option<&'a Path>,
    /// The directory this run's on-disk workspace is created under. Must
    /// be absolute (`LabRunner::prepare_clusters` rejects a relative one
    /// before creating anything).
    pub run_root: PathBuf,
}

/// Where a run's human-readable output goes.
///
/// Taken as a parameter rather than written with `println!`/`eprintln!`
/// so a test can read exactly what a run reported without capturing
/// process-wide stdout, and so the terminal report's color decision is
/// made once, by the caller that actually observed the stream, rather
/// than probed for deep inside a renderer (`TerminalOptions::for_stream`
/// is explicit about that being the caller's job).
pub struct Console<'a> {
    /// Ordinary progress and the rendered terminal report.
    pub out: &'a mut dyn io::Write,
    /// Failures, cleanup problems, and manual-recovery commands.
    pub err: &'a mut dyn io::Write,
    /// How to render the terminal report.
    pub terminal: TerminalOptions,
}

impl Console<'_> {
    /// Prefix every progress and problem line carries, so a line from
    /// Admission Lab is recognizable in a CI log interleaved with other
    /// tools' output.
    ///
    /// The command name is deliberately *not* part of it. It was
    /// `"admissionlab test:"` until Task 5.3, when a second command
    /// started driving this same pipeline: `admissionlab reproduce`
    /// printing "admissionlab test:" on every line would be a small,
    /// constant lie about which command the user ran. Naming the tool
    /// rather than the subcommand is what makes one prefix correct for
    /// both — and for whatever third command drives [`run_lab`] next.
    const PREFIX: &'static str = "admissionlab:";

    /// Reports progress on the output stream.
    fn say(&mut self, message: &str) {
        // Write failures are ignored throughout: a closed stdout is not
        // a reason to abandon a lab run (and not a reason to fail one
        // that already succeeded), and there is nowhere left to report
        // the failure to anyway.
        let _ = writeln!(self.out, "{} {message}", Self::PREFIX);
    }

    /// Reports a problem on the error stream.
    fn problem(&mut self, message: &str) {
        let _ = writeln!(self.err, "{} {message}", Self::PREFIX);
    }

    /// Writes an already-rendered block (the terminal report) verbatim,
    /// with no prefix.
    fn block(&mut self, text: &str) {
        let _ = write!(self.out, "{text}");
    }
}

/// The parts of a lab run this module drives through an abstraction
/// rather than constructing itself.
///
/// Three of the four are the traits `admissionlab-core` already declares
/// for exactly this reason ([`ClusterManager`], [`StackInstaller`],
/// `FixtureCapture` — see `admissionlab_core::run`'s documentation for
/// why they live there); the fourth is the host-prerequisite probe,
/// which is a plain function over a `ProcessRunner` in production. They
/// are gathered behind one trait so a caller substitutes a whole
/// consistent world at once, and so [`run_lab`]'s signature stays one
/// type parameter rather than four.
///
/// The two factory methods take what they need at the moment they are
/// called: an installer needs this run's [`RunPaths`] (that is what
/// isolates `helm`'s and `kubectl`'s client-side state away from the
/// operator's own), and a capture needs the discovered fixtures, neither
/// of which exists when the backend itself is constructed.
#[async_trait]
pub trait LabBackend: Send + Sync {
    /// The cluster backend both clusters are created and deleted
    /// through.
    type Clusters: ClusterManager;
    /// The stack installer both sides' components are installed with.
    type Installer: StackInstaller;
    /// The fixture capture both sides are replayed through.
    type Capture: OutcomeCapture;
    /// The Gateway suite runner both sides' route contracts are observed
    /// through. Never constructed at all for a lab with no `gateway:`
    /// section.
    type Gateway: GatewaySuiteRunner;
    /// The Ingress-to-Gateway migration suite runner (ROADMAP Task 8.8).
    /// Never constructed at all for a lab with no `migration:` section.
    type Migration: MigrationSuiteRunner;

    /// Probes host prerequisites, as `admissionlab doctor` does.
    async fn doctor_report(&self) -> DoctorReport;

    /// The cluster backend, shared across both sides' concurrent
    /// create/delete calls.
    fn cluster_manager(&self) -> Arc<Self::Clusters>;

    /// Builds the installer for a run whose workspace is at `paths`.
    fn stack_installer(&self, paths: &RunPaths) -> Self::Installer;

    /// Builds the capture for `fixtures`, writing evidence bundles
    /// through `store`. The same capture is used for both sides — which
    /// is what makes their results comparable at all.
    fn fixture_capture(&self, fixtures: Vec<FixtureSource>, store: ArtifactStore) -> Self::Capture;

    /// Builds the Gateway suite runner for `suite`, writing evidence
    /// bundles through `store`.
    ///
    /// Called only when the resolved configuration declares a `gateway:`
    /// section, and — like [`Self::fixture_capture`] — the one value it
    /// returns runs *both* sides, which is what makes their results
    /// comparable at all.
    ///
    /// It takes the suite rather than reading it off a `ResolvedLab`
    /// for the same reason the capture takes its fixtures: what a
    /// backend needs to construct an implementation is not available
    /// when the backend itself is constructed.
    fn gateway_suite(&self, suite: GatewaySuiteSpec, store: ArtifactStore) -> Self::Gateway;

    /// Builds the migration suite runner for `suite`, writing evidence
    /// bundles through `store`.
    ///
    /// Called only when the resolved configuration declares a
    /// `migration:` section. Unlike [`Self::gateway_suite`]'s runner,
    /// the one value returned here drives **both** clusters in a single
    /// call rather than being called once per side -- see
    /// [`migration`]'s "One call, two clusters" for why a migration
    /// suite has no per-side shape to share.
    fn migration_suite(&self, suite: MigrationSuiteSpec, store: ArtifactStore) -> Self::Migration;

    /// How long each component gets to install and become ready.
    /// Defaults to [`DEFAULT_COMPONENT_TIMEOUT`]; a test overrides it so
    /// its fakes never wait on a real clock.
    fn component_timeout(&self) -> Duration {
        DEFAULT_COMPONENT_TIMEOUT
    }

    /// The recorded environment this run is reproducing (ROADMAP Task
    /// 5.3), or `None` — the default — for an ordinary
    /// `admissionlab test`.
    ///
    /// This belongs on the backend rather than on [`RunRequest`] because
    /// it *is* a statement about the world the run executes against: a
    /// reproduction's node images come from a recorded manifest instead
    /// of from the compatibility matrix, and its component versions from
    /// what a previous run installed instead of from what the
    /// configuration resolves to today. A backend is already the seam
    /// through which "which world does this run happen in" is answered,
    /// and defaulting to `None` means every existing backend — the
    /// production one and every fake — keeps its current behavior
    /// untouched.
    ///
    /// [`run_lab`] does two things with it and nothing else: it applies
    /// the pin to the resolved lab (before any cluster exists, after the
    /// input digests have been computed from the source as written), and
    /// it uses [`ReproductionPin::node_images`] *instead of*
    /// `LabRunner::resolve_node_images`. That second half is what makes
    /// ROADMAP Task 5.3 step 4 structural: with a pin present the
    /// compatibility matrix is never consulted, so there is no code path
    /// by which a newer node image digest could be substituted.
    fn reproduction_pin(&self) -> Option<&ReproductionPin> {
        None
    }

    /// The handle this run watches for a cancellation request (ROADMAP
    /// Task 9.6), and registers its own manual-recovery commands with.
    ///
    /// On the backend rather than on [`RunRequest`] for the same reason
    /// [`Self::reproduction_pin`] is: it describes the *world* the run
    /// executes in — one where a process can be interrupted — rather
    /// than something the user asked for on the command line. It also
    /// keeps every existing construction site of [`RunRequest`]
    /// untouched, which matters more here than elsewhere: the request is
    /// built by two commands and half a dozen tests, and a run nobody
    /// interrupts must behave exactly as it did before.
    ///
    /// The default is a handle nothing ever requests, so a backend that
    /// does not opt in (every fake that does not care, and any future
    /// caller embedding the pipeline) runs to completion exactly as
    /// before. `commands::test` and `commands::reproduce` return the
    /// same handle `cancel::install` watches signals into, which is the
    /// whole of the wiring.
    ///
    /// Returned by value: [`Cancellation`] is an `Arc` inside, so a
    /// clone is a refcount bump, and returning one rather than a
    /// borrow lets a backend keep it wherever it likes.
    fn cancellation(&self) -> Cancellation {
        Cancellation::new()
    }
}

/// Everything the run validated out of the user's own input, before any
/// cluster existed.
struct Inputs {
    /// The resolved configuration.
    lab: ResolvedLab,
    /// The `apiVersion` the configuration document declared, read from
    /// the document itself: a [`ResolvedLab`] is version-independent and
    /// carries no `apiVersion`, and the run manifest records which
    /// configuration schema drove the run (Task 7.3 — see
    /// `admissionlab_core::RunManifest::config_api_version`).
    config_api_version: String,
    /// The compiled `policy` section.
    policy: ResolvedPolicy,
    /// The loaded `expectationsFile`, or none.
    expectations: ResolvedExpectations,
    /// Every discovered fixture, in discovery order.
    fixtures: Vec<FixtureSource>,
    /// Every content digest the run manifest records for these inputs
    /// (Task 5.1). Computed here, alongside the loading and validation
    /// that already read the same files, rather than later — by the time
    /// a cluster exists the manifest already needs them.
    digests: provenance::InputDigests,
    /// Advisory warnings about recipe/Kubernetes combinations Admission
    /// Lab has not certified (Task 7.4 step 3). Never a reason to stop:
    /// see [`certification`]'s own documentation. Left empty by
    /// [`prepare_inputs`] and filled in by [`run_lab`] once any
    /// reproduction pin has been applied — a pin can change the very
    /// Kubernetes version this asks about, so computing it any earlier
    /// would answer the wrong question, and it still happens before
    /// anything is provisioned.
    certification: Vec<Diagnostic>,
}

/// One invocation, and the stopwatch measuring it.
///
/// The two values every stage below needs and none of them owns: the
/// request the user made, and the recorder each stage reports its own
/// duration to (Task 5.7). They travel together because they have the
/// same lifetime and the same reach — from the first file read to the
/// last cluster deleted — and because threading the recorder as a ninth
/// independent parameter through functions that already take seven would
/// make every signature in this module harder to read for one borrowed
/// handle.
struct Run<'a> {
    /// What the user asked for.
    request: &'a RunRequest<'a>,
    /// Where every stage's measured duration lands.
    timings: &'a TimingRecorder,
    /// Whether somebody has asked this run to stop, and the list of
    /// manual recovery commands a forced exit would print (Task 9.6).
    /// Read at every stage boundary below; written only by
    /// [`crate::cancel`] and by [`provision`]/[`finish`], which register
    /// and retire the cluster deletions.
    cancel: &'a Cancellation,
}

/// Where a post-cluster stage puts what it produced.
///
/// Bundled rather than threaded as three more parameters: every stage
/// from install onward writes to all of them, and each stage function
/// already carries enough arguments that adding three independent ones to
/// each would make their signatures the least readable thing in the
/// module.
struct Outputs<'a> {
    /// The directory `result.json`/`report.html`/`diagnostics.json` land
    /// in.
    report_dir: PathBuf,
    /// Where the GitHub job summary goes, when one was asked for. Its
    /// parent directory has already been created by the time this exists.
    github_summary: Option<&'a Path>,
    /// This run's manifest, advanced (or failed) at every stage boundary.
    manifest: &'a mut RunManifestWriter,
    /// Where the comparison and reporting stages record how long they
    /// took. A duration is as much a thing a stage produced as a file is.
    timings: &'a TimingRecorder,
}

/// Runs one lab end to end and reports how it ended.
///
/// Never panics and never leaves a created cluster behind on any
/// ordinary failure path: see this module's documentation for the stage
/// order, for why every input check precedes cluster creation, and for
/// how cleanup is made structural rather than remembered.
///
/// The returned [`RunDisposition`] is what `crate::exit` turns into the
/// process's exit code; nothing here decides that mapping.
pub async fn run_lab<B: LabBackend>(
    backend: &B,
    request: &RunRequest<'_>,
    console: &mut Console<'_>,
) -> RunDisposition {
    let started_at = SystemTime::now();
    // Started before the first file is read, so `elapsedMs` covers the
    // whole invocation rather than only the provisioned part of it — the
    // difference between it and the stages' sum is what configuration
    // loading, hashing, and the doctor probe cost.
    let timings = TimingRecorder::start();
    let cancel = backend.cancellation();
    let run = Run {
        request,
        timings: &timings,
        cancel: &cancel,
    };
    let mut inputs = match prepare_inputs(request, console) {
        Ok(inputs) => inputs,
        Err(disposition) => {
            return no_verdict(
                request,
                console,
                None,
                "configuration",
                "The lab configuration, its policy section, its expectations, or its fixtures \
                 could not be loaded. Nothing was provisioned.",
                disposition,
            )
            .await;
        }
    };
    if let Err(disposition) = apply_reproduction_pin(backend, &mut inputs, request, console).await {
        return disposition;
    }

    // Task 7.4 step 3. Still part of validating the user's input —
    // nothing has been provisioned yet — but deliberately after the
    // reproduction pin, which can replace the very Kubernetes version
    // this asks about. Advisory in both directions: every warning is
    // printed and carried into `LabResult::diagnostics`, and none of
    // them can end the run. See `certification`'s own documentation for
    // why refusing here would break Global Constraint 6.
    inputs.certification = certification::uncertified_combinations(&inputs.lab);
    for warning in &inputs.certification {
        console.problem(&warning.message);
    }

    let doctor = match check_prerequisites(backend, console).await {
        Ok(report) => report,
        Err(disposition) => {
            return no_verdict(
                request,
                console,
                None,
                "prerequisites",
                "This host does not meet the prerequisites `admissionlab test` needs. Run \
                 `admissionlab doctor` for the full report.",
                disposition,
            )
            .await;
        }
    };

    // The last boundary before anything is provisioned. A run canceled
    // here has created nothing, so there is nothing to delete and
    // nothing to confess: it says what stopped it and ends.
    if cancel.is_requested() {
        return canceled_before_provisioning(request, console, &cancel).await;
    }

    // The backend's own cluster manager, wrapped so that each side's
    // `create` and `delete` is measured separately. Both are driven
    // concurrently by `LabRunner`, so this decorator is the only seam at
    // which "how long did the *baseline* cluster take" can be observed at
    // all — see `admissionlab_core::timing`'s own documentation.
    let runner = LabRunner {
        cluster_manager: Arc::new(TimedClusterManager::new(
            backend.cluster_manager(),
            timings.clone(),
        )),
        artifact_store: ArtifactStore::new(&request.run_root),
    };
    let pinned_images = backend.reproduction_pin().map(ReproductionPin::node_images);
    let (prepared, mut manifest) = match provision(
        &run,
        console,
        &runner,
        &inputs,
        &doctor,
        started_at,
        pinned_images,
    )
    .await
    {
        Ok(provisioned) => provisioned,
        Err(disposition) => return disposition,
    };

    // Exactly one path from here to this function's return, and
    // `finish` is on it — see this module's documentation.
    let outcome = run_with_clusters(
        backend,
        &run,
        console,
        &inputs,
        &runner,
        &prepared,
        &mut manifest,
    )
    .await;
    // Captured before `finish`, which may downgrade the disposition
    // because *cleanup* failed: a run whose comparison produced a verdict
    // did complete, and a cluster that would not delete afterwards does
    // not retroactively unmake that.
    let reached_verdict = matches!(
        outcome,
        RunDisposition::Passed | RunDisposition::PolicyFailed
    );

    let disposition = finish(&runner, &prepared, &run, console, outcome).await;
    if reached_verdict && let Err(error) = manifest.complete(SystemTime::now()).await {
        console.problem(&format!("could not record this run's completion: {error}"));
    }
    // The run's last word about itself, and the only place the reporting
    // and cleanup stages are ever visible: `result.json` was written
    // during the first and before the second, so its own `timings` block
    // cannot contain either (see `admissionlab_report::LabResult::timings`).
    // Same renderer as the terminal report's "Stage timings" block, a
    // later snapshot of the same recorder.
    console.say(&format!(
        "stage timings: {}",
        timings.snapshot().summary_line()
    ));
    disposition
}

/// Applies the recorded environment a reproduction is pinned to, if this
/// run is one.
///
/// Called *after* `prepare_inputs`, deliberately: the digests it computed
/// describe the source tree as written, which is the thing a reproduction
/// verified against the manifest and the thing this run's own manifest
/// should record. Pinning first would make those digests describe a lab
/// no file on disk contains.
///
/// Returns `Err(disposition)` when the pin cannot be applied — an
/// ordinary `admissionlab test` (no pin) and a pin that applied cleanly
/// both return `Ok(())`. Notes the pin raises are reported and the run
/// continues; they describe what the pin changed, not a problem with it.
async fn apply_reproduction_pin<B: LabBackend>(
    backend: &B,
    inputs: &mut Inputs,
    request: &RunRequest<'_>,
    console: &mut Console<'_>,
) -> Result<(), RunDisposition> {
    let Some(pin) = backend.reproduction_pin() else {
        return Ok(());
    };
    match pin.apply(&mut inputs.lab) {
        Ok(notes) => {
            for note in &notes {
                console.problem(&note.message);
            }
            Ok(())
        }
        Err(error) => {
            console.problem(&format!("cannot reproduce the recorded run: {error}"));
            let failure = error.to_string();
            Err(no_verdict(
                request,
                console,
                None,
                "reproduction",
                &failure,
                RunDisposition::InvalidInput,
            )
            .await)
        }
    }
}

/// Everything between "the inputs are good" and "both clusters exist":
/// the run workspace, both sides' node images, the run manifest, and the
/// clusters themselves.
///
/// Split out of [`run_lab`] because it is the one stretch of the run
/// where every step can fail in the same way — nothing is provisioned
/// yet or the leak has already been rolled back, the failure is reported,
/// a no-verdict summary is written, and the run is over — and because
/// `prepare_clusters`'s three parts have to be driven separately (Task
/// 5.2): the manifest has to exist before a cluster does, cannot exist
/// before the workspace that holds it, and cannot be complete before both
/// sides' node images are known.
///
/// Returns the two values the rest of the run needs: the prepared
/// clusters and the manifest writer, already advanced past cluster
/// creation.
///
/// `pinned_images` is `Some` only for a reproduction (ROADMAP Task 5.3),
/// and when it is, [`LabRunner::resolve_node_images`] is not called at
/// all — see [`LabBackend::reproduction_pin`] for why that bypass is the
/// mechanism rather than a shortcut.
async fn provision<C: ClusterManager>(
    run: &Run<'_>,
    console: &mut Console<'_>,
    runner: &LabRunner<C>,
    inputs: &Inputs,
    doctor: &DoctorReport,
    started_at: SystemTime,
    pinned_images: Option<ResolvedNodeImages>,
) -> Result<(PreparedLab, RunManifestWriter), RunDisposition> {
    let request = run.request;
    let options = RunOptions {
        keep_clusters: request.keep_clusters,
        run_root: request.run_root.clone(),
    };
    let (run_id, paths) = match runner.create_workspace(&options).await {
        Ok(workspace) => workspace,
        Err(error) => {
            let disposition = report_cluster_failure(&error, console);
            let failure = error.to_string();
            return Err(
                no_verdict(request, console, None, "workspace", &failure, disposition).await,
            );
        }
    };
    // The identifier every no-verdict summary below names. Not `run`,
    // which is this stage's context parameter.
    let started = Some(run_id.as_str());
    let images = match pinned_images {
        // A reproduction runs the images the recorded manifest named,
        // and never asks the compatibility matrix what it thinks today.
        Some(images) => images,
        None => match runner.resolve_node_images(&inputs.lab).await {
            Ok(images) => images,
            Err(error) => {
                let disposition = report_cluster_failure(&error, console);
                let failure = error.to_string();
                return Err(no_verdict(
                    request,
                    console,
                    started,
                    "node-image",
                    &failure,
                    disposition,
                )
                .await);
            }
        },
    };

    let manifest = provenance::initial_manifest(
        &run_id,
        doctor,
        &inputs.lab,
        &inputs.config_api_version,
        &images,
        inputs.digests.clone(),
        started_at,
    );
    let mut manifest =
        match RunManifestWriter::create(ArtifactStore::new(&request.run_root), &paths, manifest)
            .await
        {
            Ok(writer) => writer,
            Err(error) => {
                // Fatal, and cheap to be fatal about: nothing is provisioned
                // yet — see this module's "The run manifest is written before
                // anything is provisioned" section.
                console.problem(&format!("failed to write this run's manifest: {error}"));
                let failure = error.to_string();
                let disposition = RunDisposition::InfrastructureFailed;
                return Err(no_verdict(
                    request,
                    console,
                    started,
                    "manifest",
                    &failure,
                    disposition,
                )
                .await);
            }
        };
    console.say(&format!("wrote {}.", manifest.path().display()));

    // The pair's wall-clock; each side's own is measured one level down,
    // by `TimedClusterManager`.
    let created = {
        let _stage = run.timings.stage(TimedStage::ClusterCreation);
        runner
            .create_clusters(&inputs.lab, &run_id, &paths, &images)
            .await
    };
    let prepared = match created {
        Ok(prepared) => prepared,
        Err(error) => {
            record_stage_failure(&mut manifest, RunStage::ClusterCreation, console).await;
            let disposition = report_cluster_failure(&error, console);
            let failure = error.to_string();
            let stage = "cluster-creation";
            return Err(no_verdict(request, console, started, stage, &failure, disposition).await);
        }
    };
    // Registered the instant both clusters exist, and retired by
    // `finish` once they are gone: from here until then, a *second*
    // interrupt exits without unwinding, and this list is the only thing
    // that can tell the operator what to delete by hand (Task 9.6 step
    // 4). Registering after the fact rather than before is deliberate —
    // a `create` that failed has already rolled its own side back, and
    // printing a delete command for a cluster that never existed sends
    // somebody looking for a leak that is not there.
    for command in manual_cluster_deletion_commands(&prepared) {
        run.cancel.register_cleanup_command(command);
    }
    console.say(&format!(
        "created baseline cluster {:?} and candidate cluster {:?}.",
        prepared.baseline.spec.name, prepared.candidate.spec.name
    ));
    record_stage(&mut manifest, RunStage::ClusterCreation, |_| {}, console).await;
    Ok((prepared, manifest))
}

/// The `stage` label every canceled run's `diagnostics.json` and job
/// summary carry.
///
/// One label for every boundary rather than the name of whichever stage
/// was next, because the stage is not what went wrong: nothing failed,
/// somebody stopped the run. *Which* boundary it stopped at is in the
/// failure sentence beside it, where a reader looking for "how far did
/// this get" will actually read it.
const CANCELED_STAGE: &str = "canceled";

/// Ends a run that was asked to stop at the boundary before `next`.
///
/// Called only when `run.cancel.is_requested()` already said so — the
/// check is at the call site so that a run nobody interrupted pays one
/// atomic load per stage and nothing else.
///
/// Everything a canceled run owes anybody happens here: it says what
/// stopped it, writes the `diagnostics.json` naming the interruption and
/// the boundary it stopped at, and hands back the canceled disposition.
/// It writes no `result.json` and touches no verdict, because a run that
/// stopped before its comparison has none; and it does not mark a
/// manifest stage *failed*, because none did. `run.json` is left exactly
/// as it is, which already says what happened: its last recorded stage
/// is the last one that completed, and its status stays `in_progress`
/// because nothing completed this run.
///
/// Cleanup is deliberately not done here. The caller returns straight
/// into [`run_lab`]'s single path to [`finish`], which deletes both
/// clusters (or prints them for `--keep-clusters`) exactly as it does on
/// every other path — see this module's "Cancellation" section for why a
/// separate teardown for this case would be a second implementation to
/// keep in sync with the one that matters.
async fn canceled(
    run: &Run<'_>,
    console: &mut Console<'_>,
    prepared: &PreparedLab,
    outputs: &Outputs<'_>,
    next: &str,
    diagnostics: &[Diagnostic],
) -> RunDisposition {
    let cause = cancellation_cause(run.cancel);
    console.problem(&format!(
        "canceled by {cause}: not starting the {next} stage. Tearing down."
    ));
    let failure = format!(
        "interrupted by {cause} before the {next} stage; this run stopped without comparing \
         baseline and candidate, so it states no verdict"
    );
    write_failure(
        outputs,
        prepared,
        CANCELED_STAGE,
        &failure,
        diagnostics.to_vec(),
        console,
    )
    .await;
    crate::exit::CANCELED_DISPOSITION
}

/// How a message should name whatever asked this run to stop.
///
/// The signal is `Some` for every caller here: a boundary only asks
/// after `Cancellation::is_requested` said yes, and a request records
/// its signal before it becomes visible as one. The fallback wording
/// exists so that stays true without an `expect` — a panic on the
/// teardown path would cost the operator the very cleanup these
/// functions exist to reach, in exchange for a message.
fn cancellation_cause(cancel: &Cancellation) -> String {
    cancel.signal().map_or_else(
        || "a cancellation request".to_owned(),
        |signal| signal.to_string(),
    )
}

/// Ends a run canceled before anything was provisioned.
///
/// Separate from [`canceled`] because it has strictly less to say and
/// nowhere to say it: with no cluster there is no run workspace, so
/// there is no report directory to write a `diagnostics.json` into and
/// nothing to tear down. What it can still write is the job summary,
/// which is written on every path out of this module (see "The job
/// summary is written whatever happens").
async fn canceled_before_provisioning(
    request: &RunRequest<'_>,
    console: &mut Console<'_>,
    cancel: &Cancellation,
) -> RunDisposition {
    let cause = cancellation_cause(cancel);
    console.problem(&format!(
        "canceled by {cause} before any cluster was created; nothing was provisioned."
    ));
    let failure = format!("interrupted by {cause} before any cluster was created");
    no_verdict(
        request,
        console,
        None,
        CANCELED_STAGE,
        &failure,
        crate::exit::CANCELED_DISPOSITION,
    )
    .await
}

/// Writes the no-verdict job summary for a run that is ending without
/// one, and returns `disposition` unchanged.
///
/// Returning the caller's own disposition is the whole shape of this
/// helper: every call site reads `return no_verdict(..., disposition)`,
/// so writing the summary cannot accidentally become the thing that
/// decides how the run ended. A write failure is reported and dropped for
/// the same reason (see this module's "The job summary is written
/// whatever happens" section), and no summary is written at all when the
/// caller asked for none.
async fn no_verdict(
    request: &RunRequest<'_>,
    console: &mut Console<'_>,
    run_id: Option<&str>,
    stage: &str,
    failure: &str,
    disposition: RunDisposition,
) -> RunDisposition {
    write_no_verdict_summary(request.github_summary, console, run_id, stage, failure, 0).await;
    disposition
}

/// Writes the no-verdict job summary to `path`, if there is one to write
/// to.
///
/// Separate from [`no_verdict`] because the post-cluster failure path
/// ([`write_failure`]) has diagnostics to count and a disposition it
/// decides elsewhere, so it needs the write without the return-value
/// plumbing.
async fn write_no_verdict_summary(
    path: Option<&Path>,
    console: &mut Console<'_>,
    run_id: Option<&str>,
    stage: &str,
    failure: &str,
    diagnostics: usize,
) {
    let Some(path) = path else {
        return;
    };
    match report::write_no_verdict_summary(path, run_id, stage, failure, diagnostics).await {
        Ok(()) => console.say(&format!("wrote {}.", path.display())),
        Err(error) => console.problem(&format!(
            "could not write this run's job summary to {}: {error}",
            path.display()
        )),
    }
}

/// Advances the manifest to `stage`, applying `update` first, and reports
/// (without propagating) a write failure.
///
/// Non-fatal by design: see this module's "The run manifest is written
/// before anything is provisioned" section.
async fn record_stage<F>(
    manifest: &mut RunManifestWriter,
    stage: RunStage,
    update: F,
    console: &mut Console<'_>,
) where
    F: FnOnce(&mut admissionlab_core::RunManifest),
{
    if let Err(error) = manifest.record(stage, update).await {
        console.problem(&format!(
            "could not record the {stage} stage in this run's manifest: {error}"
        ));
    }
}

/// Records that `stage` failed, leaving `completedAt` null.
///
/// Reported but never propagated: a manifest that could not be updated
/// must not replace, or obscure, the failure it was trying to describe.
async fn record_stage_failure(
    manifest: &mut RunManifestWriter,
    stage: RunStage,
    console: &mut Console<'_>,
) {
    if let Err(error) = manifest.fail(stage).await {
        console.problem(&format!(
            "could not record the {stage} failure in this run's manifest: {error}"
        ));
    }
}

/// Loads and checks everything that comes out of the user's own files.
///
/// Every failure here is [`RunDisposition::InvalidInput`] except a
/// fixture problem that only a live cluster could produce, which
/// `discover_fixtures` cannot reach — see
/// [`crate::exit::disposition_for_fixture_error`].
fn prepare_inputs(
    request: &RunRequest<'_>,
    console: &mut Console<'_>,
) -> Result<Inputs, RunDisposition> {
    // Read separately from the load: `ResolvedLab` is version-independent
    // by construction (every supported document is migrated to the current
    // model before resolution), so it carries no `apiVersion` for the
    // manifest to record which configuration schema drove this run.
    let config_api_version = declared_api_version(request.config).map_err(|error| {
        console.problem(&format!("failed to load lab configuration: {error}"));
        crate::exit::disposition_for_spec_error(&error)
    })?;
    // Accepts every `apiVersion` this build still reads — a Public Alpha
    // `admissionlab.io/v1alpha1` or Public Beta `admissionlab.io/v1beta1`
    // file as well as today's `admissionlab.io/v1` — migrating the older
    // ones forward before resolving them (ROADMAP Task 7.1 Step 2, Task
    // 9.1 Step 3).
    let lab = load_any_supported_lab(request.config).map_err(|error| {
        console.problem(&format!("failed to load lab configuration: {error}"));
        crate::exit::disposition_for_spec_error(&error)
    })?;

    // `admissionlab_policy::validate_policy_spec` is the documented
    // load-time seam for names `admissionlab-spec` cannot check itself
    // (a semantic-change kind belongs to `admissionlab-diff`, which
    // §1.1 places above `spec`). Called here, right after `resolve_lab`
    // and before any cluster exists, exactly as that function's own
    // documentation requires.
    let problems = validate_policy_spec(&lab.policy);
    if !problems.is_empty() {
        console.problem("invalid regression policy:");
        for problem in &problems {
            console.problem(&format!("  {}: {}", problem.locator, problem.message));
        }
        return Err(crate::exit::disposition_for_policy_spec());
    }
    // Compiling cannot fail once validation passed (both are the same
    // check — `validate_policy_spec` is defined as `resolve_policy`'s
    // error half). Handled rather than unwrapped anyway: this module
    // should not encode an assumption about another crate's internals
    // that a future refactor there could quietly break.
    let policy = resolve_policy(&lab.policy).map_err(|errors| {
        console.problem("invalid regression policy:");
        for problem in errors.as_slice() {
            console.problem(&format!("  {}: {}", problem.locator, problem.message));
        }
        crate::exit::disposition_for_policy_spec()
    })?;

    let expectations = match &lab.expectations_file {
        Some(path) => load_expectations(path).map_err(|error| {
            console.problem(&format!("invalid expectations: {error}"));
            crate::exit::disposition_for_expectations()
        })?,
        None => ResolvedExpectations::none(),
    };

    let fixtures = discover_fixtures(&lab.fixtures).map_err(|error| {
        console.problem(&format!("failed to discover fixtures: {error}"));
        crate::exit::disposition_for_fixture_error(&error)
    })?;
    if fixtures.is_empty() {
        // A lab with nothing to replay is a configuration mistake, not a
        // trivially passing run. `fixtures.include` is already validated
        // as non-empty by `resolve_lab`, so reaching here means every
        // pattern matched no file — and a run that compared nothing must
        // never exit 0, which is exactly the claim "the policy passed"
        // would make. Reported with the patterns and the directory they
        // were matched against, because a glob that matches nothing is
        // almost always a path problem the message can point straight at.
        console.problem(&format!(
            "no fixtures matched {} under {}; there is nothing to replay, so this run cannot \
             produce a comparison",
            lab.fixtures
                .include
                .iter()
                .map(|pattern| format!("{:?}", pattern.glob()))
                .collect::<Vec<_>>()
                .join(", "),
            lab.fixtures.root.display(),
        ));
        return Err(RunDisposition::InvalidInput);
    }
    validate_gateway_ids(&lab, &fixtures, console)?;
    validate_migration_suite(&lab, console)?;
    console.say(&format!(
        "loaded {}; {} fixture(s), {} expectation(s).",
        request.config.display(),
        fixtures.len(),
        expectations.len()
    ));

    // Hashed here, with the same files still on disk and already known
    // to parse, so the manifest can be complete the moment the run
    // workspace exists. A read failure is the user's own input problem
    // (the file vanished or became unreadable between parse and hash),
    // which is what exit 2 means — see `provenance`'s own documentation
    // for why this is not degraded to an absent digest.
    let digests = provenance::input_digests(request.config, &lab, &fixtures).map_err(|error| {
        console.problem(&format!(
            "failed to hash this run's inputs for its manifest: {error}"
        ));
        RunDisposition::InvalidInput
    })?;

    Ok(Inputs {
        lab,
        config_api_version,
        policy,
        expectations,
        fixtures,
        digests,
        certification: Vec::new(),
    })
}

/// Refuses a configuration whose Gateway route contract ids cannot be
/// used as report identities.
///
/// A route contract is reported as a [`FixtureComparison`] like every
/// admission fixture, under the id the user wrote (see
/// [`compare::gateway_fixture_id`] for why the id is used verbatim
/// rather than derived). Two properties are needed for that, and neither
/// can be checked by `admissionlab-spec`, which is a leaf crate and
/// cannot name `admissionlab_core::FixtureId`:
///
/// - the id must parse as a `FixtureId` — lowercase ASCII, digits, `-`;
/// - it must not collide with a discovered fixture's id, since two
///   entries under one identifier would make a per-fixture drill-down
///   ambiguous and would attribute one's changes to the other.
///
/// Both are checked here, at exit 2, before any cluster is created —
/// the same placement, and for the same reason, as every other input
/// check in [`prepare_inputs`].
///
/// [`FixtureComparison`]: admissionlab_report::FixtureComparison
fn validate_gateway_ids(
    lab: &ResolvedLab,
    fixtures: &[FixtureSource],
    console: &mut Console<'_>,
) -> Result<(), RunDisposition> {
    let Some(suite) = &lab.gateway else {
        return Ok(());
    };
    for contract in &suite.routes {
        if compare::gateway_fixture_id(&contract.id).is_none() {
            console.problem(&format!(
                "invalid lab configuration: gateway route contract id {:?} cannot be reported: a \
                 contract is reported under its own id, which must contain only lowercase \
                 letters, digits, and `-`",
                contract.id
            ));
            return Err(RunDisposition::InvalidInput);
        }
        if let Some(clash) = fixtures
            .iter()
            .find(|fixture| fixture.id.as_str() == contract.id)
        {
            console.problem(&format!(
                "invalid lab configuration: gateway route contract id {:?} is also the fixture id \
                 of {}; a run reports both under one identifier, so the two would be \
                 indistinguishable",
                contract.id,
                clash.path.display(),
            ));
            return Err(RunDisposition::InvalidInput);
        }
    }
    Ok(())
}

/// Refuses a `migration:` suite that could not produce evidence
/// (ROADMAP Task 8.8).
///
/// Two checks, both at exit 2 and both before any cluster is created —
/// the same placement, and for the same reason, as
/// [`validate_gateway_ids`]:
///
/// - **Every case id must parse as an
///   [`admissionlab_core::FixtureId`].** A case's evidence is written to
///   `raw/<side>/migration/<case-id>/`, so the id is a path segment;
///   that grammar (lowercase ASCII, digits, `-`) is what makes it one
///   safely. This does *not* also check for a collision with a fixture
///   id, and that is the one place it differs from the Gateway check:
///   a migration case is reported in its own top-level `migration` array
///   rather than as a `FixtureComparison`, so two entries under one name
///   are in two different lists and nothing is made ambiguous.
/// - **Both per-side `gatewayEndpoint` blocks must be present and
///   resolve.** `admissionlab-spec` cannot require them — they are an
///   *optional* addition to an existing `admissionlab.io/v1beta1`
///   section, and `docs/schema-migrations.md`'s first obligation is that
///   an addition leaves older documents valid (see
///   [`admissionlab_spec::MigrationSideSpec`]). But a suite with nowhere
///   to send its probes would install two stacks, compare nothing, and
///   report success, so it is refused here instead — which gives a
///   load-time-quality error without a schema-level required field.
fn validate_migration_suite(
    lab: &ResolvedLab,
    console: &mut Console<'_>,
) -> Result<(), RunDisposition> {
    let Some(suite) = &lab.migration else {
        return Ok(());
    };
    for case in &suite.cases {
        if admissionlab_core::FixtureId::parse(&case.id).is_err() {
            console.problem(&format!(
                "invalid lab configuration: migration case id {:?} cannot be reported: a case's \
                 evidence is written under its own id, which must contain only lowercase letters, \
                 digits, and `-`",
                case.id
            ));
            return Err(RunDisposition::InvalidInput);
        }
    }
    for (name, side) in [
        ("baseline", suite.baseline.as_ref()),
        ("candidate", suite.candidate.as_ref()),
    ] {
        let Some(side) = side else {
            console.problem(&format!(
                "invalid lab configuration: this lab declares migration cases but no \
                 migration.{name}.gatewayEndpoint, so the {name} side's probes have no data-plane \
                 Service to go through -- and a migration case's probes are the only thing its \
                 two sides can be compared on"
            ));
            return Err(RunDisposition::InvalidInput);
        };
        if let Err((locator, message)) =
            admissionlab_spec::resolve_gateway_endpoint(&side.gateway_endpoint)
        {
            console.problem(&format!(
                "invalid lab configuration: migration.{name}.gatewayEndpoint.{locator} is \
                 invalid: {message}"
            ));
            return Err(RunDisposition::InvalidInput);
        }
    }
    Ok(())
}

/// Refuses to start a lab on a host that cannot run one.
///
/// Returns `Err(disposition)` when the run must stop, and otherwise the
/// report itself — which the run manifest records as its
/// `ToolProvenance` (Task 5.1), so the versions written down are the same
/// ones the run was gated on rather than a second, later probe's. The
/// report is printed in full when something is wrong, so a user learns
/// every missing prerequisite at once rather than one rerun at a time —
/// and `admissionlab doctor` is named explicitly, because it is the
/// command that explains this in detail.
async fn check_prerequisites<B: LabBackend>(
    backend: &B,
    console: &mut Console<'_>,
) -> Result<DoctorReport, RunDisposition> {
    let report = backend.doctor_report().await;
    if let Some(warning) = &report.disk_warning {
        // Advisory only, exactly as `meets_prerequisites` treats it
        // (PRODUCT.md §34 calls it a warning threshold, not a
        // requirement) — surfaced rather than swallowed, since a run
        // that later dies pulling images should not be the first hint.
        console.problem(&format!("disk space: {warning}"));
    }
    let Some(disposition) = crate::exit::disposition_for_prerequisites(&report) else {
        return Ok(report);
    };

    console.problem("host prerequisites are not met:");
    for tool in &report.tools {
        if !tool.found {
            let detail = tool.diagnostic.as_deref().unwrap_or("not found");
            console.problem(&format!("  {}: {detail}", tool.name));
        }
    }
    if !report.docker_reachable {
        console.problem("  docker daemon: unreachable");
    }
    console.problem("run `admissionlab doctor` for the full report.");
    Err(disposition)
}

/// Reports a cluster-stage failure and maps it.
fn report_cluster_failure(error: &RunError, console: &mut Console<'_>) -> RunDisposition {
    console.problem(&format!("failed to prepare lab clusters: {error}"));
    if let RunError::ClusterCreationFailed { rollback, .. } = error {
        // `prepare_clusters` already deleted whichever side came up;
        // these say what that attempt found, including the manual
        // recovery command when it did not succeed.
        for diagnostic in rollback {
            console.problem(&diagnostic.message);
        }
    }
    crate::exit::disposition_for_run_error(error)
}

/// Every stage that runs while the two clusters exist.
///
/// Split out from [`run_lab`] purely so cleanup cannot be skipped by an
/// early return: this function may return from anywhere, and its caller
/// always runs [`finish`] afterwards.
/// Resolves and creates the directory this run's reports go in, or
/// reports the failure and ends the run.
///
/// Called before anything that can fail, so every later stage has
/// somewhere to write its evidence — and split out of
/// [`run_with_clusters`] for the same reason each stage below it is: it
/// is one step with its own failure handling, and inline it pushed the
/// function that lists the stages past the point where it reads as a
/// list of them.
///
/// Recorded as a `reporting` failure when it fails: preparing where the
/// reports go is the reporting stage's first act, even though it happens
/// this early.
async fn report_directory(
    request: &RunRequest<'_>,
    console: &mut Console<'_>,
    prepared: &PreparedLab,
    manifest: &mut RunManifestWriter,
) -> Result<PathBuf, RunDisposition> {
    match resolve_report_dir(request, prepared).await {
        Ok(directory) => Ok(directory),
        Err(error) => {
            console.problem(&format!("failed to prepare the report directory: {error}"));
            record_stage_failure(manifest, RunStage::Reporting, console).await;
            let started = Some(prepared.run_id.as_str());
            let failure = error.to_string();
            let disposition = RunDisposition::InfrastructureFailed;
            Err(no_verdict(
                request,
                console,
                started,
                "reporting",
                &failure,
                disposition,
            )
            .await)
        }
    }
}

async fn run_with_clusters<B: LabBackend, C: ClusterManager>(
    backend: &B,
    run: &Run<'_>,
    console: &mut Console<'_>,
    inputs: &Inputs,
    runner: &LabRunner<C>,
    prepared: &PreparedLab,
    manifest: &mut RunManifestWriter,
) -> RunDisposition {
    let request = run.request;
    let report_dir = match report_directory(request, console, prepared, manifest).await {
        Ok(directory) => directory,
        Err(disposition) => return disposition,
    };
    let mut outputs = Outputs {
        report_dir,
        github_summary: request.github_summary,
        manifest,
        timings: run.timings,
    };

    // Nothing is installed yet, so the certification warnings are the
    // only thing a run canceled here has observed.
    let observed = &inputs.certification;
    if run.cancel.is_requested() {
        return canceled(run, console, prepared, &outputs, "install", observed).await;
    }

    let stacks = match install_both_stacks(
        backend,
        run,
        console,
        inputs,
        runner,
        prepared,
        &mut outputs,
    )
    .await
    {
        Ok(stacks) => stacks,
        Err(disposition) => return disposition,
    };

    // Wrapped so each side's replay is measured separately, and so the
    // corpus size reaches the recorder from the side that counted it.
    let capture = TimedFixtureCapture::new(
        backend.fixture_capture(
            inputs.fixtures.clone(),
            ArtifactStore::new(&request.run_root),
        ),
        run.timings.clone(),
    );
    // The certification warnings lead, because they describe the stack
    // the rest of these diagnostics were produced by: they were raised
    // before this run had a cluster at all (Task 7.4 step 3), and they
    // are run-level in exactly the sense `install_diagnostics` documents
    // — about the environment the comparison ran in, not about any one
    // fixture.
    let mut diagnostics = inputs.certification.clone();
    diagnostics.extend(install_diagnostics(&stacks));
    if run.cancel.is_requested() {
        return canceled(run, console, prepared, &outputs, "capture", &diagnostics).await;
    }
    let captured = {
        let _stage = run.timings.stage(TimedStage::FixtureCapture);
        runner.capture_fixtures(prepared, &capture).await
    };
    if let Err(failure) = captured {
        console.problem(&format!("{failure}"));
        record_stage_failure(outputs.manifest, RunStage::FixtureCapture, console).await;
        // Whatever was captured before the failure is still a real
        // observation; its diagnostics go into the artifact rather than
        // being discarded with the run (ROADMAP step 3).
        diagnostics.extend(outcome_diagnostics(&capture.captured_outcomes()));
        write_failure(
            &outputs,
            prepared,
            "capture",
            &failure.to_string(),
            diagnostics,
            console,
        )
        .await;
        return crate::exit::disposition_for_capture_failure();
    }
    console.say(&format!(
        "captured {} fixture(s) per side; raw evidence in {}.",
        inputs.fixtures.len(),
        prepared.paths.raw().display()
    ));
    record_stage(outputs.manifest, RunStage::FixtureCapture, |_| {}, console).await;

    if run.cancel.is_requested() {
        return canceled(run, console, prepared, &outputs, "behavior", &diagnostics).await;
    }

    let (gateway, migration) =
        match run_behavior_suites(backend, run, console, inputs, prepared, &mut outputs).await {
            Ok(observed) => observed,
            Err(disposition) => return disposition,
        };
    diagnostics.extend(gateway.diagnostics.iter().cloned());
    diagnostics.extend(migration.diagnostics.iter().cloned());

    // The last boundary, and the one that matters most: everything above
    // is evidence-gathering, and everything below states a verdict. A
    // run interrupted after its evidence exists still must not grade it
    // -- an interrupted corpus is a partial one, and a comparison over
    // part of a corpus is not the comparison the user asked for.
    if run.cancel.is_requested() {
        return canceled(run, console, prepared, &outputs, "comparison", &diagnostics).await;
    }

    compare_and_report(
        console,
        inputs,
        prepared,
        &stacks,
        Observations {
            outcomes: &capture.captured_outcomes(),
            gateway: &gateway,
            migration: &migration,
        },
        &mut outputs,
        diagnostics,
    )
    .await
}

/// Everything the two clusters produced, ready to be compared.
///
/// The two halves travel together because they are one subject -- what
/// this run observed -- and because passing them separately pushed
/// [`compare_and_report`] past the argument count that keeps a signature
/// readable. Borrowed: the comparison stage only reads them, and both
/// are still owned by the stage that produced them.
struct Observations<'a> {
    /// Every admission fixture outcome, both sides interleaved.
    outcomes: &'a [AdmissionOutcome],
    /// Both sides' Gateway case results, empty for an admission-only
    /// lab.
    gateway: &'a GatewayRun,
    /// Every Ingress-to-Gateway migration case the run compared, and the
    /// run-level diagnostics the suite produced. Empty for a lab with no
    /// `migration:` section.
    migration: &'a MigrationRun,
}

/// What the migration suite produced, plus whether one ran at all.
///
/// The `declared` flag is what keeps "this lab is not migrating off
/// `Ingress`" distinguishable from "this lab is migrating and compared
/// nothing", which
/// [`admissionlab_report::LabResult::migration`] represents as `None`
/// against `Some(vec![])`. Deriving it from an empty case list instead
/// would collapse the two, and a `migration:` section whose cases all
/// failed to compare is exactly the case a reader must be able to tell
/// apart from an admission-only run.
#[derive(Debug, Default)]
struct MigrationRun {
    /// Whether the lab declared a `migration:` section at all.
    declared: bool,
    /// One comparison per case, in suite order.
    cases: Vec<MigrationCaseComparison>,
    /// Run-level findings, already tagged with their case and side.
    diagnostics: Vec<Diagnostic>,
}

/// Both sides' Gateway case results, and the run-level diagnostics the
/// suite produced.
///
/// Empty on every axis for a lab with no `gateway:` section, which is
/// what makes "suite absent ⇒ zero Gateway output" structural rather
/// than a rule each renderer has to remember: nothing downstream can
/// fabricate a section out of two empty slices.
#[derive(Debug, Default)]
struct GatewayRun {
    /// What the baseline side produced, in suite order.
    baseline: Vec<GatewayCaseResult>,
    /// What the candidate side produced, in suite order.
    candidate: Vec<GatewayCaseResult>,
    /// Every skipped-probe diagnostic, already tagged with its side and
    /// contract.
    diagnostics: Vec<Diagnostic>,
}

/// Runs the configured Gateway suite on both sides, or reports the
/// failure, records it, and ends the run.
///
/// # Why after fixture capture rather than before
///
/// ROADMAP Task 6.11 step 1 requires the Gateway apply to run separately
/// from the admission dry-run fixtures, and after the stacks are
/// installed. Both orders satisfy that; this one is chosen because
/// Gateway fixtures are **persisted** (Phase 6's own execution
/// distinction), so running them first would mean every admission
/// fixture is replayed against a cluster that now contains a
/// `GatewayClass`, a `Gateway`, an `HTTPRoute`, two namespaces and a
/// backend `Deployment` that the admission corpus never asked for. That
/// would be equal on both sides and so would not *manufacture* a
/// difference — but it would silently change what the admission half of
/// every lab observes the moment a `gateway:` section is added, which is
/// a coupling between two independent halves of the product that nothing
/// requires.
///
/// Running it after also means an admission-only failure never pays for
/// a Gateway suite, and a Gateway failure leaves the admission evidence
/// already written.
///
/// # Both sides at once
///
/// `tokio::join!`, never `try_join!` — the same discipline, and the same
/// argument, as `LabRunner::install_stacks` and
/// `LabRunner::capture_fixtures`: abandoning the other side's in-flight
/// future the instant one fails would drop a suite midway through
/// writing its evidence, and — uniquely here — could drop it while a
/// `kubectl port-forward` child is still running. Both sides always
/// reach their own natural conclusion.
async fn run_gateway_suite<B: LabBackend>(
    backend: &B,
    run: &Run<'_>,
    console: &mut Console<'_>,
    inputs: &Inputs,
    prepared: &PreparedLab,
    outputs: &mut Outputs<'_>,
) -> Result<GatewayRun, RunDisposition> {
    let Some(suite) = inputs.lab.gateway.clone() else {
        return Ok(GatewayRun::default());
    };
    let routes = suite.routes.len();
    let runner = backend.gateway_suite(suite, ArtifactStore::new(&run.request.run_root));

    let (baseline, candidate) = {
        let _stage = run.timings.stage(TimedStage::GatewaySuite);
        tokio::join!(
            timed_gateway_side(
                &runner,
                &prepared.baseline,
                Side::Baseline,
                &prepared.paths,
                run.timings
            ),
            timed_gateway_side(
                &runner,
                &prepared.candidate,
                Side::Candidate,
                &prepared.paths,
                run.timings
            ),
        )
    };

    let (baseline, candidate) = match (baseline, candidate) {
        (Ok(baseline), Ok(candidate)) => (baseline, candidate),
        (baseline, candidate) => {
            // Both sides' failures are reported, not just the first:
            // a suite that broke on both sides usually broke for one
            // reason, and seeing only half of it costs a rerun.
            let failure = [
                baseline.err().map(|error| format!("baseline: {error}")),
                candidate.err().map(|error| format!("candidate: {error}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ");
            console.problem(&format!("the Gateway suite failed: {failure}"));
            record_stage_failure(outputs.manifest, RunStage::GatewaySuite, console).await;
            write_failure(
                &*outputs,
                prepared,
                "gateway",
                &failure,
                Vec::new(),
                console,
            )
            .await;
            return Err(crate::exit::disposition_for_gateway_failure());
        }
    };

    console.say(&format!(
        "observed {routes} Gateway route contract(s) per side; raw evidence in {}.",
        prepared.paths.raw().display()
    ));
    let mut diagnostics = baseline.diagnostics.clone();
    diagnostics.extend(candidate.diagnostics.iter().cloned());
    for diagnostic in &diagnostics {
        // A skipped probe is evidence a reader must see while the run is
        // still on screen, not only in `result.json` (ROADMAP Task 6.11
        // step 3).
        console.problem(&diagnostic.message);
    }
    record_stage(outputs.manifest, RunStage::GatewaySuite, |_| {}, console).await;

    Ok(GatewayRun {
        baseline: baseline.cases,
        candidate: candidate.cases,
        diagnostics,
    })
}

/// Runs both behavior suites, in order, or ends the run.
///
/// The two are driven together because they are one phase of a run --
/// "what did these two stacks *do*", as opposed to "what did their API
/// servers *decide*" -- and because a lab may declare either, both, or
/// neither. Either one being absent is a zero-cost early return inside
/// its own function, so this composition costs nothing for the
/// admission-only lab that is still the common case.
async fn run_behavior_suites<B: LabBackend>(
    backend: &B,
    run: &Run<'_>,
    console: &mut Console<'_>,
    inputs: &Inputs,
    prepared: &PreparedLab,
    outputs: &mut Outputs<'_>,
) -> Result<(GatewayRun, MigrationRun), RunDisposition> {
    let gateway = run_gateway_suite(backend, run, console, inputs, prepared, outputs).await?;
    let migration = run_migration_suite(backend, run, console, inputs, prepared, outputs).await?;
    Ok((gateway, migration))
}

/// Runs the configured Ingress-to-Gateway migration suite across both
/// clusters, or reports the failure, records it, and ends the run
/// (ROADMAP Task 8.8).
///
/// # Why after the Gateway suite
///
/// Both are persisted-object phases, and both leave their objects
/// behind, so the ordering question is only "which of the two changes
/// what the other observes". Running the migration suite *second* is the
/// answer that keeps an existing lab unchanged: a `gateway:` suite that
/// already ran a route contract in an empty cluster keeps doing so, and
/// the new phase inherits whatever the old one left rather than the
/// reverse. Neither suite's objects overlap in practice -- a migration
/// case brings its own namespaces -- but "which phase is allowed to be
/// affected by the other" should be a decision rather than an accident.
///
/// # Why this stage is neither timed nor recorded in the run manifest
///
/// See [`migration`]'s own "No stage timing, and no run-manifest stage":
/// `RunStage` and `TimedStage` are the run manifest's frozen vocabulary,
/// and adding a variant to them is a change to a different frozen
/// document than the one this task owns.
async fn run_migration_suite<B: LabBackend>(
    backend: &B,
    run: &Run<'_>,
    console: &mut Console<'_>,
    inputs: &Inputs,
    prepared: &PreparedLab,
    outputs: &mut Outputs<'_>,
) -> Result<MigrationRun, RunDisposition> {
    let Some(suite) = inputs.lab.migration.clone() else {
        return Ok(MigrationRun::default());
    };
    let declared = suite.cases.len();
    let runner = backend.migration_suite(suite, ArtifactStore::new(&run.request.run_root));

    let started = std::time::Instant::now();
    let outcome = runner
        .run(&prepared.baseline, &prepared.candidate, &prepared.paths)
        .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(failure) => {
            console.problem(&format!("the migration suite failed: {failure}"));
            write_failure(
                &*outputs,
                prepared,
                "migration",
                &failure.to_string(),
                Vec::new(),
                console,
            )
            .await;
            // The same disposition a Gateway suite failure gets: both
            // are "the run could not obtain the evidence it was asked
            // for", which is what exit 5 means.
            return Err(crate::exit::disposition_for_gateway_failure());
        }
    };

    console.say(&format!(
        "compared {declared} Ingress-to-Gateway migration case(s) in {:.1}s; raw evidence in {}.",
        started.elapsed().as_secs_f64(),
        prepared.paths.raw().display()
    ));
    for diagnostic in &outcome.diagnostics {
        // A denied baseline, a legacy stack that never served, or a
        // candidate route that carried no traffic is evidence a reader
        // must see while the run is still on screen -- the same rule the
        // Gateway suite's skipped probes get.
        console.problem(&diagnostic.message);
    }

    Ok(MigrationRun {
        declared: true,
        cases: outcome.cases,
        diagnostics: outcome.diagnostics,
    })
}

/// One side's Gateway suite run, measured.
///
/// The per-side timer lives here rather than in a decorator: the three
/// `admissionlab_core` timing decorators exist because `LabRunner` drives
/// those traits and the pipeline never sees a single side's call, while
/// this port is driven from this module directly — so the boundary that
/// names a side is already in hand and a fourth decorator would add a
/// type to wrap it in and nothing else.
async fn timed_gateway_side<G: GatewaySuiteRunner + ?Sized>(
    runner: &G,
    cluster: &admissionlab_core::ClusterHandle,
    side: Side,
    paths: &RunPaths,
    timings: &TimingRecorder,
) -> Result<SideGatewayOutcome, GatewaySuiteError> {
    let _stage = timings.side(TimedSideStage::GatewaySuite, side);
    runner.run_side(cluster, side, paths).await
}

/// Installs both sides' component stacks, or reports the failure,
/// records it, and ends the run.
///
/// Split from [`run_with_clusters`] because it is one self-contained
/// stage with its own failure handling, and because leaving it inline
/// made that function long enough to stop reading as the ordered list of
/// stages it exists to be.
async fn install_both_stacks<B: LabBackend, C: ClusterManager>(
    backend: &B,
    run: &Run<'_>,
    console: &mut Console<'_>,
    inputs: &Inputs,
    runner: &LabRunner<C>,
    prepared: &PreparedLab,
    outputs: &mut Outputs<'_>,
) -> Result<InstalledLab, RunDisposition> {
    // Wrapped so each side's install — and each component's own
    // already-measured `elapsed` inside it — reaches the recorder.
    let installer = TimedStackInstaller::new(
        backend.stack_installer(&prepared.paths),
        run.timings.clone(),
    );
    let outcome = {
        let _stage = run.timings.stage(TimedStage::Installation);
        runner
            .install_stacks(
                &inputs.lab,
                prepared,
                &installer,
                backend.component_timeout(),
            )
            .await
    };
    let stacks = match outcome {
        Ok(stacks) => stacks,
        Err(failure) => {
            console.problem(&format!("{failure}"));
            record_stage_failure(outputs.manifest, RunStage::Installation, console).await;
            write_failure(
                &*outputs,
                prepared,
                "install",
                &failure.to_string(),
                Vec::new(),
                console,
            )
            .await;
            return Err(crate::exit::disposition_for_install_failure(&failure));
        }
    };
    console.say(&format!(
        "installed {} baseline and {} candidate component(s).",
        stacks.baseline.components.len(),
        stacks.candidate.components.len()
    ));
    // Each side's components are re-recorded from what was actually
    // installed, replacing the configured versions the first write used
    // — see `provenance`'s "Components are recorded twice" section.
    let baseline_components = provenance::installed_components(&stacks.baseline);
    let candidate_components = provenance::installed_components(&stacks.candidate);
    record_stage(
        outputs.manifest,
        RunStage::Installation,
        |manifest| {
            manifest.baseline.components = baseline_components;
            manifest.candidate.components = candidate_components;
        },
        console,
    )
    .await;
    Ok(stacks)
}

/// Compares what both sides did, grades it, and writes the run's
/// reports.
///
/// Split from [`run_with_clusters`] because it is a different subject:
/// everything above it is about driving external systems, everything
/// here is a pure computation over what those systems produced, plus the
/// writes at the end. `diagnostics` arrives carrying whatever the
/// install stage observed, and leaves inside the result.
async fn compare_and_report(
    console: &mut Console<'_>,
    inputs: &Inputs,
    prepared: &PreparedLab,
    stacks: &InstalledLab,
    observed: Observations<'_>,
    outputs: &mut Outputs<'_>,
    mut diagnostics: Vec<Diagnostic>,
) -> RunDisposition {
    let timings = outputs.timings;
    // Held across normalization, the semantic diff, first-divergence
    // attribution and policy grading — the whole of what
    // `RunStage::Comparison` means, and the one stage PRODUCT.md §33
    // gives a sub-second target for. Dropped before the result is
    // assembled, so the snapshot that goes *into* the result already
    // contains this stage's own number.
    let comparison_stage = timings.stage(TimedStage::Comparison);
    let gateway_results = compare::GatewayResults {
        baseline: &observed.gateway.baseline,
        candidate: &observed.gateway.candidate,
    };
    let comparison = match compare::compare(
        &inputs.lab,
        &inputs.fixtures,
        observed.outcomes,
        Some(&gateway_results),
    ) {
        Ok(comparison) => comparison,
        Err(error) => {
            console.problem(&format!("failed to normalize captured objects: {error}"));
            record_stage_failure(outputs.manifest, RunStage::Comparison, console).await;
            write_failure(
                &*outputs,
                prepared,
                "normalize",
                &error.to_string(),
                diagnostics,
                console,
            )
            .await;
            return crate::exit::disposition_for_normalize(&error);
        }
    };
    diagnostics.extend(comparison.diagnostics.iter().cloned());

    // The only place in the pipeline where severity is decided, and it
    // is `admissionlab-policy` deciding it (§1.1: report rendering never
    // grades). Expectations are applied here too, so an accounted-for
    // change stays visible and simply stops driving the verdict.
    let mut policy =
        evaluate_with_expectations(&inputs.policy, &inputs.expectations, &comparison.changes());

    // The migration suite's contribution to the run's verdict (ROADMAP
    // Task 8.8). `PolicyResult::disposition` is *the run's verdict* --
    // it is what `crate::exit::disposition_for_policy` turns into an
    // exit code and what `result.json`'s reader branches on -- so a
    // migration regression that did not reach it would produce a run
    // that exits 0 while its own report says a route now answers from a
    // different backend.
    //
    // The join is a `max` over the same three-word scale, never a
    // rewrite of what policy concluded: `policy.changes` is untouched,
    // every admission and Gateway grade is exactly what
    // `admissionlab-policy` assigned, and `migration_disposition`'s own
    // reasoning is in `migration::grade` and `migration::case_disposition`.
    // What a reader sees when the two disagree is a `fail` disposition
    // whose explanation is in the document's `migration` array rather
    // than in `policy.changes` -- which is why the terminal and HTML
    // reports both render that array in full, unconditionally.
    let migration_verdict = migration::migration_disposition(&observed.migration.cases);
    policy.disposition = policy.disposition.max(migration_verdict);
    drop(comparison_stage);
    let result = report::build_result(
        &prepared.run_id,
        report::environment_summary(prepared, stacks),
        &comparison,
        policy,
        diagnostics,
        observed
            .migration
            .declared
            .then(|| observed.migration.cases.clone()),
        Some(timings.snapshot()),
    );

    record_stage(outputs.manifest, RunStage::Comparison, |_| {}, console).await;

    let reporting_stage = timings.stage(TimedStage::Reporting);
    let rendered = report::write_reports(
        &outputs.report_dir,
        outputs.github_summary,
        &result,
        console.terminal,
    );
    drop(reporting_stage);
    let written = match rendered {
        Ok(written) => written,
        Err(error) => {
            console.problem(&format!("failed to write the run's reports: {error}"));
            record_stage_failure(outputs.manifest, RunStage::Reporting, console).await;
            // The verdict exists but nobody can read it, so the summary
            // says exactly that rather than nothing: this is the one
            // no-verdict summary written for a run that did reach one,
            // and it still states no verdict, because the value it would
            // have stated is the value that could not be written.
            write_no_verdict_summary(
                outputs.github_summary,
                console,
                Some(prepared.run_id.as_str()),
                "reporting",
                &error.to_string(),
                0,
            )
            .await;
            return crate::exit::disposition_for_report_error(&error);
        }
    };
    console.block(&written.terminal);
    console.say(&format!(
        "wrote {} and {}.",
        written.result_json.display(),
        written.report_html.display()
    ));
    if let Some(summary) = &written.github_summary {
        console.say(&format!("wrote {}.", summary.display()));
    }
    record_stage(outputs.manifest, RunStage::Reporting, |_| {}, console).await;

    crate::exit::disposition_for_policy(result.policy.disposition)
}

/// Deletes both clusters, or preserves them, and folds the result into
/// `disposition`.
///
/// Always reached once the clusters exist. A failed deletion never
/// silently leaves a run reporting success — see
/// [`crate::exit::after_failed_cleanup`] for the precedence rule and the
/// argument for it.
async fn finish<C: ClusterManager>(
    runner: &LabRunner<C>,
    prepared: &PreparedLab,
    run: &Run<'_>,
    console: &mut Console<'_>,
    disposition: RunDisposition,
) -> RunDisposition {
    if run.request.keep_clusters {
        // Printed rather than deleted (PRODUCT.md §10.4): both cluster
        // names, both kubeconfig paths, and the exact
        // `kind delete cluster --name <name>` for each.
        console.block(&preserved_cluster_report(prepared));
        return disposition;
    }

    // Only the deleting path is timed. A `--keep-clusters` run returns
    // above without deleting anything, and reports no `cleanup` timing at
    // all rather than a zero — there was no cleanup to measure.
    let diagnostics = {
        let _stage = run.timings.stage(TimedStage::Cleanup);
        runner.cleanup(prepared).await
    };
    if diagnostics.is_empty() {
        // Both clusters are gone, so the delete commands `provision`
        // registered describe nothing: a forced exit from here on must
        // not print them, which would read as a leak that is not there
        // (Task 9.6). A *failed* cleanup below keeps them registered,
        // for the opposite and equally literal reason.
        run.cancel.clear_cleanup_commands();
        console.say("baseline and candidate clusters deleted.");
        return disposition;
    }
    for diagnostic in &diagnostics {
        console.problem(&diagnostic.message);
    }
    console.problem(
        "cleanup did not fully succeed; a cluster may still exist - see the delete command(s) \
         above.",
    );
    crate::exit::after_failed_cleanup(disposition)
}

/// The directory `result.json`/`report.html`/`diagnostics.json` are
/// written into: `--report-dir` when given, otherwise this run's own
/// `reports/` directory.
///
/// A `--report-dir` that does not exist yet is created (including
/// parents): a user pointing at `./artifacts` on a fresh checkout means
/// "put them there", not "fail because I did not `mkdir` first". The
/// default needs no creation — `ArtifactStore::create_run` already made
/// it.
///
/// `--github-summary`'s parent directory is created here too, for the
/// same reason and at the same moment, so that
/// [`report::write_reports`] can require it to exist exactly as it
/// requires the report directory to.
async fn resolve_report_dir(
    request: &RunRequest<'_>,
    prepared: &PreparedLab,
) -> io::Result<PathBuf> {
    if let Some(parent) = request
        .github_summary
        .and_then(Path::parent)
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    match request.report_dir {
        Some(directory) => {
            tokio::fs::create_dir_all(directory).await?;
            Ok(directory.to_path_buf())
        }
        None => Ok(prepared.paths.reports().to_path_buf()),
    }
}

/// Writes the failure-path `diagnostics.json` and the matching
/// no-verdict job summary, reporting where each landed (or why it could
/// not be written — which must never itself replace the failure being
/// reported).
///
/// The two are written together because they are the same statement to
/// two readers: `diagnostics.json` is what a machine (or a human with the
/// artifact downloaded) reads, and the summary is what the same person
/// sees in the pull request without downloading anything. Neither carries
/// a verdict.
async fn write_failure(
    outputs: &Outputs<'_>,
    prepared: &PreparedLab,
    stage: &'static str,
    failure: &str,
    diagnostics: Vec<Diagnostic>,
    console: &mut Console<'_>,
) {
    let collected = diagnostics.len();
    let artifact = report::FailureArtifact {
        run_id: prepared.run_id.as_str().to_owned(),
        stage,
        failure: failure.to_owned(),
        diagnostics,
    };
    match report::write_failure_artifact(&outputs.report_dir, &artifact).await {
        Ok(path) => console.say(&format!("wrote {}.", path.display())),
        Err(error) => console.problem(&format!(
            "could not write this run's diagnostics artifact: {error}"
        )),
    }
    write_no_verdict_summary(
        outputs.github_summary,
        console,
        Some(prepared.run_id.as_str()),
        stage,
        failure,
        collected,
    )
    .await;
}

/// Run-level diagnostics from installing both stacks.
///
/// These are genuinely run-level — they describe the *environment* the
/// comparison ran in, not any one fixture — so unlike per-fixture
/// capture diagnostics they belong directly in
/// `LabResult::diagnostics`. Each is tagged with the side and component
/// it came from, which the installer's own diagnostic did not know it
/// would need.
fn install_diagnostics(installed: &InstalledLab) -> Vec<Diagnostic> {
    let mut collected = Vec::new();
    for side in [&installed.baseline, &installed.candidate] {
        for component in &side.components {
            for diagnostic in &component.diagnostics {
                let mut tagged = diagnostic.clone();
                tagged.context.insert(
                    "side".to_owned(),
                    RedactedValue::Public(side.side.to_string()),
                );
                tagged.context.insert(
                    "component".to_owned(),
                    RedactedValue::Public(component.name.clone()),
                );
                collected.push(tagged);
            }
        }
    }
    collected
}

/// Every diagnostic on every captured outcome, tagged with the fixture
/// and side it came from.
///
/// Used **only** on the failure path, where the destination is a
/// diagnostics dump rather than a `LabResult` — a successful run leaves
/// these on their own outcomes, where `LabResult::diagnostics`'s frozen
/// contract puts them, and surfaces a counted summary instead (see
/// [`compare`]'s module documentation).
fn outcome_diagnostics(outcomes: &[AdmissionOutcome]) -> Vec<Diagnostic> {
    let mut collected = Vec::new();
    for outcome in outcomes {
        for diagnostic in &outcome.diagnostics {
            let mut tagged = diagnostic.clone();
            tagged.context.insert(
                "fixture".to_owned(),
                RedactedValue::Public(outcome.fixture_id.as_str().to_owned()),
            );
            tagged.context.insert(
                "side".to_owned(),
                RedactedValue::Public(outcome.side.to_string()),
            );
            collected.push(tagged);
        }
    }
    collected
}
