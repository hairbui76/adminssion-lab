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

pub mod capture;
pub mod compare;
pub mod install;
pub mod provenance;
pub mod report;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use admissionlab_admission::AdmissionOutcome;
use admissionlab_core::{
    ArtifactStore, ClusterManager, Diagnostic, DoctorReport, InstalledLab, LabRunner, PreparedLab,
    RedactedValue, ReproductionPin, ResolvedNodeImages, RunDisposition, RunError,
    RunManifestWriter, RunOptions, RunPaths, RunStage, StackInstaller, preserved_cluster_report,
};
use admissionlab_fixtures::{FixtureSource, discover_fixtures};
use admissionlab_policy::{
    ResolvedExpectations, ResolvedPolicy, evaluate_with_expectations, load_expectations,
    resolve_policy, validate_policy_spec,
};
use admissionlab_report::TerminalOptions;
use admissionlab_spec::{ResolvedLab, load_lab, resolve_lab};
use async_trait::async_trait;

pub use capture::OutcomeCapture;

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
}

/// Everything the run validated out of the user's own input, before any
/// cluster existed.
struct Inputs {
    /// The resolved configuration.
    lab: ResolvedLab,
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
}

/// Where a post-cluster stage puts what it produced.
///
/// Bundled rather than threaded as two more parameters: every stage from
/// install onward writes to both, and each stage function already carries
/// enough arguments that adding two independent ones to each would make
/// their signatures the least readable thing in the module.
struct Outputs<'a> {
    /// The directory `result.json`/`report.html`/`diagnostics.json` land
    /// in.
    report_dir: PathBuf,
    /// Where the GitHub job summary goes, when one was asked for. Its
    /// parent directory has already been created by the time this exists.
    github_summary: Option<&'a Path>,
    /// This run's manifest, advanced (or failed) at every stage boundary.
    manifest: &'a mut RunManifestWriter,
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
    // Applied *after* `prepare_inputs`, deliberately: the digests it
    // computed describe the source tree as written, which is the thing a
    // reproduction verified against the manifest and the thing this
    // run's own manifest should record. Pinning first would make those
    // digests describe a lab no file on disk contains.
    if let Some(pin) = backend.reproduction_pin() {
        match pin.apply(&mut inputs.lab) {
            Ok(notes) => {
                for note in &notes {
                    console.problem(&note.message);
                }
            }
            Err(error) => {
                console.problem(&format!("cannot reproduce the recorded run: {error}"));
                let failure = error.to_string();
                return no_verdict(
                    request,
                    console,
                    None,
                    "reproduction",
                    &failure,
                    RunDisposition::InvalidInput,
                )
                .await;
            }
        }
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

    let runner = LabRunner {
        cluster_manager: backend.cluster_manager(),
        artifact_store: ArtifactStore::new(&request.run_root),
    };
    let pinned_images = backend.reproduction_pin().map(ReproductionPin::node_images);
    let (prepared, mut manifest) = match provision(
        request,
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
        request,
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

    let disposition = finish(&runner, &prepared, request, console, outcome).await;
    if reached_verdict && let Err(error) = manifest.complete(SystemTime::now()).await {
        console.problem(&format!("could not record this run's completion: {error}"));
    }
    disposition
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
    request: &RunRequest<'_>,
    console: &mut Console<'_>,
    runner: &LabRunner<C>,
    inputs: &Inputs,
    doctor: &DoctorReport,
    started_at: SystemTime,
    pinned_images: Option<ResolvedNodeImages>,
) -> Result<(PreparedLab, RunManifestWriter), RunDisposition> {
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
    let run = Some(run_id.as_str());
    let images = match pinned_images {
        // A reproduction runs the images the recorded manifest named,
        // and never asks the compatibility matrix what it thinks today.
        Some(images) => images,
        None => match runner.resolve_node_images(&inputs.lab).await {
            Ok(images) => images,
            Err(error) => {
                let disposition = report_cluster_failure(&error, console);
                let failure = error.to_string();
                return Err(
                    no_verdict(request, console, run, "node-image", &failure, disposition).await,
                );
            }
        },
    };

    let manifest = provenance::initial_manifest(
        &run_id,
        doctor,
        &inputs.lab,
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
                return Err(
                    no_verdict(request, console, run, "manifest", &failure, disposition).await,
                );
            }
        };
    console.say(&format!("wrote {}.", manifest.path().display()));

    let prepared = match runner
        .create_clusters(&inputs.lab, &run_id, &paths, &images)
        .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            record_stage_failure(&mut manifest, RunStage::ClusterCreation, console).await;
            let disposition = report_cluster_failure(&error, console);
            let failure = error.to_string();
            let stage = "cluster-creation";
            return Err(no_verdict(request, console, run, stage, &failure, disposition).await);
        }
    };
    console.say(&format!(
        "created baseline cluster {:?} and candidate cluster {:?}.",
        prepared.baseline.spec.name, prepared.candidate.spec.name
    ));
    record_stage(&mut manifest, RunStage::ClusterCreation, |_| {}, console).await;
    Ok((prepared, manifest))
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
    let loaded = load_lab(request.config).map_err(|error| {
        console.problem(&format!("failed to load lab configuration: {error}"));
        crate::exit::disposition_for_spec_error(&error)
    })?;
    let lab = resolve_lab(loaded).map_err(|error| {
        console.problem(&format!("invalid lab configuration: {error}"));
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
        policy,
        expectations,
        fixtures,
        digests,
    })
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
async fn run_with_clusters<B: LabBackend>(
    backend: &B,
    request: &RunRequest<'_>,
    console: &mut Console<'_>,
    inputs: &Inputs,
    runner: &LabRunner<B::Clusters>,
    prepared: &PreparedLab,
    manifest: &mut RunManifestWriter,
) -> RunDisposition {
    // Resolved before anything can fail, so every later stage has
    // somewhere to write its evidence. Recorded as a `reporting` failure
    // when it fails: preparing where the reports go is the reporting
    // stage's first act, even though it happens early for the reason
    // above.
    let report_dir = match resolve_report_dir(request, prepared).await {
        Ok(directory) => directory,
        Err(error) => {
            console.problem(&format!("failed to prepare the report directory: {error}"));
            record_stage_failure(manifest, RunStage::Reporting, console).await;
            let run = Some(prepared.run_id.as_str());
            let failure = error.to_string();
            let disposition = RunDisposition::InfrastructureFailed;
            return no_verdict(request, console, run, "reporting", &failure, disposition).await;
        }
    };
    let mut outputs = Outputs {
        report_dir,
        github_summary: request.github_summary,
        manifest,
    };

    let installer = backend.stack_installer(&prepared.paths);
    let stacks = match runner
        .install_stacks(
            &inputs.lab,
            prepared,
            &installer,
            backend.component_timeout(),
        )
        .await
    {
        Ok(stacks) => stacks,
        Err(failure) => {
            console.problem(&format!("{failure}"));
            record_stage_failure(outputs.manifest, RunStage::Installation, console).await;
            write_failure(
                &outputs,
                prepared,
                "install",
                &failure.to_string(),
                Vec::new(),
                console,
            )
            .await;
            return crate::exit::disposition_for_install_failure(&failure);
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

    let capture = backend.fixture_capture(
        inputs.fixtures.clone(),
        ArtifactStore::new(&request.run_root),
    );
    let mut diagnostics = install_diagnostics(&stacks);
    if let Err(failure) = runner.capture_fixtures(prepared, &capture).await {
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

    compare_and_report(
        console,
        inputs,
        prepared,
        &stacks,
        &capture.captured_outcomes(),
        &mut outputs,
        diagnostics,
    )
    .await
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
    outcomes: &[AdmissionOutcome],
    outputs: &mut Outputs<'_>,
    mut diagnostics: Vec<Diagnostic>,
) -> RunDisposition {
    let comparison = match compare::compare(&inputs.lab, &inputs.fixtures, outcomes) {
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
    let policy =
        evaluate_with_expectations(&inputs.policy, &inputs.expectations, &comparison.changes());
    let result = report::build_result(
        &prepared.run_id,
        report::environment_summary(prepared, stacks),
        &comparison,
        policy,
        diagnostics,
    );

    record_stage(outputs.manifest, RunStage::Comparison, |_| {}, console).await;

    let written = match report::write_reports(
        &outputs.report_dir,
        outputs.github_summary,
        &result,
        console.terminal,
    ) {
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
    request: &RunRequest<'_>,
    console: &mut Console<'_>,
    disposition: RunDisposition,
) -> RunDisposition {
    if request.keep_clusters {
        // Printed rather than deleted (PRODUCT.md §10.4): both cluster
        // names, both kubeconfig paths, and the exact
        // `kind delete cluster --name <name>` for each.
        console.block(&preserved_cluster_report(prepared));
        return disposition;
    }

    let diagnostics = runner.cleanup(prepared).await;
    if diagnostics.is_empty() {
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
