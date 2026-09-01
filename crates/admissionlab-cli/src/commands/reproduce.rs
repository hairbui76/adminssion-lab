//! `admissionlab reproduce`: re-running a recorded run from its manifest
//! (ROADMAP Task 5.3; PRODUCT.md §28).
//!
//! ```text
//! admissionlab reproduce ./artifacts/run.json --source-root .
//! ```
//!
//! # There is no second engine
//!
//! This command reuses [`crate::pipeline::run_lab`] — the same function
//! `admissionlab test` runs, with the same stage order, the same failure
//! mappings, and the same artifacts. A reproduction that ran through
//! different code could not be compared against the run it reproduces,
//! because any difference in the report would have two possible causes.
//! The only thing that differs is the *world* the run happens in, and
//! that is expressed as [`LabBackend::reproduction_pin`]: recorded node
//! images instead of freshly resolved ones, recorded component versions
//! instead of whatever the source resolves to today.
//!
//! One consequence is worth naming rather than hiding: the lab
//! configuration is loaded, resolved, and hashed **twice** — once here,
//! to verify it against the manifest before anything is provisioned, and
//! once inside `run_lab`, which loads its own inputs exactly as it does
//! for `admissionlab test`. That is a few milliseconds of duplicated
//! parsing, and the alternative is a second entry point into the
//! pipeline that takes a pre-resolved lab — which would mean two ways
//! for a run to be set up and one of them exercised only by
//! reproduction. The duplicated read is also not purely wasted: it is
//! what makes the refusal above happen before `run_lab` is entered at
//! all, so a mismatched source tree never reaches the host-prerequisite
//! probe, let alone Docker.
//!
//! # What happens before anything is created
//!
//! Every check below runs against files on disk, in milliseconds, before
//! a single container exists (ROADMAP Task 5.3 step 1):
//!
//! 1. The manifest parses as a `v1alpha1` run manifest.
//! 2. The lab configuration and, when the lab declares one, the
//!    expectations file still hash to what the manifest recorded
//!    (`admissionlab_core::plan_reproduction`).
//! 3. Every fixture the recorded run replayed is still discovered, with
//!    the content it had (`admissionlab_core::verify_fixtures`).
//! 4. The effective normalization profile and regression policy still
//!    hash to what the manifest recorded
//!    (`admissionlab_core::verify_effective_digests`).
//!
//! Every failure among these is reported **together**, not one per run:
//! a user who edited three fixtures learns about three fixtures. All of
//! them exit `2` — the invalid-input class. A manifest and a source tree
//! that disagree are not a valid input pair for reproduction, in exactly
//! the way a malformed `admissionlab.yaml` is not a valid input for
//! `admissionlab test`; nothing failed, and nothing was attempted.
//!
//! # Where the source is, and why the command has to be told
//!
//! A run manifest **cannot** record where its configuration lived.
//! `admissionlab_core::run_manifest`'s module documentation makes it
//! structural that no type in that document holds a `PathBuf`, so a
//! manifest attached to a public bug report cannot leak a filesystem
//! layout. That is worth more than the convenience it costs here, so
//! reproduction is told instead: `--source-root` names the directory
//! holding the lab configuration, and defaults to `.`.
//!
//! `.` is the only defensible default, and it is worth saying why the
//! two tempting alternatives are not. The manifest's own directory is
//! wrong: `./artifacts/run.json` sits wherever `--report-dir` pointed,
//! which is typically a CI artifact directory containing no source at
//! all. And there is nothing recorded to fall back to. So the default is
//! the working directory — which is also exactly what ROADMAP Task 5.3's
//! own example spells out — and `--config` covers a lab file that is not
//! named `admissionlab.yaml`.
//!
//! # Plan-time refusal versus run-time unavailability
//!
//! ROADMAP Task 5.3 step 3 requires a clear failure listing what is
//! unavailable, and the two halves of that are genuinely different
//! problems.
//!
//! Everything knowable from disk is checked above and refuses at plan
//! time. What is **not** knowable without a network — whether the
//! recorded node image digest still pulls, whether the recorded chart
//! version is still published — cannot be detected here at all, and this
//! command does not pretend otherwise by probing a registry it would then
//! have to trust. Instead it prints
//! [`ReproductionPin::pinned_summary`] before provisioning anything, so
//! every recorded artifact the run is about to demand is on screen
//! *above* whatever `kind` or `helm` says when one of them is gone; and
//! it repeats that list when the run fails at cluster creation (exit `3`)
//! or installation (exit `4`), which are the two dispositions a
//! no-longer-available pinned artifact produces.

use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use admissionlab_core::{
    ArtifactStore, DiscoveredFixture, DoctorReport, ReproduceError, ReproducePlan, ReproductionPin,
    RunDisposition, RunManifest, RunPaths, plan_reproduction, plan_reproduction_from_config,
    read_run_manifest, verify_effective_digests, verify_fixtures,
};
use admissionlab_fixtures::{FixtureSource, discover_fixtures};
use admissionlab_report::TerminalOptions;
use async_trait::async_trait;
use clap::Args;

use crate::commands::test::{KindBackend, default_run_root};
use crate::exit;
use crate::pipeline::{Console, LabBackend, RunRequest, provenance, run_lab};

/// Prefix every line this command prints carries.
///
/// The same literal `pipeline::Console::PREFIX` uses, deliberately: a
/// reproduction's pre-cluster refusals and the run's own progress lines
/// are one stream to whoever is reading the log, and two prefixes would
/// suggest two tools.
const PREFIX: &str = "admissionlab:";

/// Arguments for `admissionlab reproduce`.
#[derive(Debug, Args)]
pub struct ReproduceArgs {
    /// Path to the run manifest (`run.json`) of the run to reproduce.
    #[arg(value_name = "MANIFEST")]
    pub manifest: PathBuf,

    /// Directory holding the lab configuration the recorded run used.
    /// See this module's documentation for why this must be given rather
    /// than read from the manifest, and why the default is the working
    /// directory.
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub source_root: PathBuf,

    /// The lab configuration file, when it is not
    /// `<SOURCE_ROOT>/admissionlab.yaml`.
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Preserve baseline/candidate clusters after the run instead of
    /// deleting them. Identical to `admissionlab test --keep-clusters`.
    #[arg(long)]
    pub keep_clusters: bool,

    /// Write `result.json` and `report.html` into this directory instead
    /// of the run's own `reports/` directory. Identical to
    /// `admissionlab test --report-dir`.
    #[arg(long, value_name = "DIR")]
    pub report_dir: Option<PathBuf>,
}

/// The production [`LabBackend`] with a recorded environment pinned onto
/// it.
///
/// Delegates every method to the same [`KindBackend`]
/// `admissionlab test` uses — real `kind`, real `helm`/`kubectl`, real
/// server-side dry-run capture — and overrides exactly one thing. That
/// is the entire mechanical difference between a test and a
/// reproduction.
struct ReproduceBackend {
    /// The production world.
    inner: KindBackend,
    /// The recorded environment this run must reproduce.
    pin: ReproductionPin,
}

#[async_trait]
impl LabBackend for ReproduceBackend {
    type Clusters = <KindBackend as LabBackend>::Clusters;
    type Installer = <KindBackend as LabBackend>::Installer;
    type Capture = <KindBackend as LabBackend>::Capture;
    type Gateway = <KindBackend as LabBackend>::Gateway;

    async fn doctor_report(&self) -> DoctorReport {
        self.inner.doctor_report().await
    }

    fn cluster_manager(&self) -> Arc<Self::Clusters> {
        self.inner.cluster_manager()
    }

    fn stack_installer(&self, paths: &RunPaths) -> Self::Installer {
        self.inner.stack_installer(paths)
    }

    fn gateway_suite(
        &self,
        suite: admissionlab_spec::GatewaySuiteSpec,
        store: ArtifactStore,
    ) -> Self::Gateway {
        self.inner.gateway_suite(suite, store)
    }

    fn fixture_capture(&self, fixtures: Vec<FixtureSource>, store: ArtifactStore) -> Self::Capture {
        self.inner.fixture_capture(fixtures, store)
    }

    fn component_timeout(&self) -> Duration {
        self.inner.component_timeout()
    }

    fn reproduction_pin(&self) -> Option<&ReproductionPin> {
        Some(&self.pin)
    }
}

/// Everything the pre-cluster verification established.
struct Verified {
    /// Where the lab configuration was found.
    config: PathBuf,
    /// The pins the run will impose.
    pin: ReproductionPin,
}

/// Runs `admissionlab reproduce`.
///
/// # Panics
///
/// Panics only if this process's own `tokio` runtime cannot be built at
/// all — an environmental failure with no sensible fallback, matching
/// `commands::test::run`'s identical convention.
#[must_use]
pub fn run(args: &ReproduceArgs) -> ExitCode {
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();

    let verified = match verify(args, &mut err) {
        Ok(verified) => verified,
        Err(disposition) => return exit::code_for_disposition(disposition),
    };

    let _ = write!(out, "{}", verified.pin.pinned_summary());
    let backend = ReproduceBackend {
        inner: KindBackend::new(&default_run_root()),
        pin: verified.pin,
    };
    let request = RunRequest {
        config: &verified.config,
        keep_clusters: args.keep_clusters,
        report_dir: args.report_dir.as_deref(),
        // `admissionlab reproduce` has no `--github-summary`: a job
        // summary is a report *of a comparison*, and the flag exists on
        // the command CI actually runs. Nothing stops a later task from
        // adding it here; nothing needs it today.
        github_summary: None,
        run_root: default_run_root(),
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build the reproduce command's tokio runtime");
    let disposition = {
        let mut console = Console {
            terminal: TerminalOptions::for_stream(
                out.is_terminal(),
                std::env::var_os("NO_COLOR").is_none(),
            ),
            out: &mut out,
            err: &mut err,
        };
        runtime.block_on(run_lab(&backend, &request, &mut console))
    };

    report_possible_unavailability(&backend.pin, disposition, &mut err);
    exit::code_for_disposition(disposition)
}

/// Everything ROADMAP Task 5.3 step 1 requires, before any cluster.
///
/// Returns [`RunDisposition::InvalidInput`] for every failure here: see
/// this module's documentation for why a manifest and a source tree that
/// disagree are an input problem rather than an infrastructure one.
fn verify(args: &ReproduceArgs, err: &mut dyn std::io::Write) -> Result<Verified, RunDisposition> {
    let manifest = read_manifest(&args.manifest, err)?;
    if let Some(warning) = admissionlab_core::incomplete_run_warning(&manifest) {
        let _ = writeln!(err, "{PREFIX} warning: {warning}");
    }

    // The same file [`plan`] is about to load, by construction: both
    // reach it by joining the one shared `DEFAULT_LAB_FILE_NAME` onto
    // `--source-root`, or by taking `--config` verbatim. It is needed
    // here as well because `input_digests` hashes the configuration by
    // path, and hashing a *different* file than the lab was resolved
    // from would make every digest below meaningless.
    let config = args.config.clone().unwrap_or_else(|| {
        args.source_root
            .join(admissionlab_core::DEFAULT_LAB_FILE_NAME)
    });
    let mut plan = plan(&manifest, args).map_err(|error| {
        let _ = writeln!(err, "{PREFIX} cannot reproduce this run: {error}");
        exit::disposition_for_reproduce_error(&error)
    })?;

    // Fixture discovery and the effective digests, which
    // `admissionlab-core` cannot reach — see its `reproduce` module's
    // "Where verification is split" section. `input_digests` is the one
    // function in the product that computes a run's input digests, so
    // this compares against its output rather than deriving a second.
    let fixtures = discover_fixtures(&plan.resolved_lab.fixtures).map_err(|error| {
        let _ = writeln!(err, "{PREFIX} failed to discover fixtures: {error}");
        exit::disposition_for_fixture_error(&error)
    })?;
    let digests =
        provenance::input_digests(&config, &plan.resolved_lab, &fixtures).map_err(|error| {
            let _ = writeln!(err, "{PREFIX} failed to hash this lab's inputs: {error}");
            RunDisposition::InvalidInput
        })?;

    let discovered: Vec<DiscoveredFixture> = fixtures
        .iter()
        .map(|fixture| DiscoveredFixture {
            id: fixture.id.clone(),
            path: fixture.path.clone(),
            // Copied, never recomputed: discovery's own digest is the one
            // the manifest recorded and the one this run would use.
            sha256: fixture.sha256.clone(),
        })
        .collect();
    let corpus = verify_fixtures(&manifest, &discovered);
    plan.verified_inputs.extend(corpus.verified.iter().cloned());
    let effective = verify_effective_digests(
        &manifest,
        &digests.normalization_sha256,
        &digests.policy_sha256,
    );

    let mismatched: Vec<_> = plan.mismatches().collect();
    if !mismatched.is_empty()
        || !corpus.missing.is_empty()
        || !corpus.unexpected.is_empty()
        || !effective.is_empty()
    {
        let _ = writeln!(
            err,
            "{PREFIX} the source tree no longer matches the recorded run, so this would not be a \
             reproduction:"
        );
        for input in mismatched {
            let _ = writeln!(
                err,
                "  changed  {}\n    expected sha256 {}\n    actual   sha256 {}",
                input.path.display(),
                input.expected_sha256,
                input.actual_sha256,
            );
        }
        for id in &corpus.missing {
            let _ = writeln!(
                err,
                "  missing  fixture {} (the recorded run replayed it; this source does not \
                 discover it)",
                id.as_str()
            );
        }
        for id in &corpus.unexpected {
            let _ = writeln!(
                err,
                "  added    fixture {} (this source discovers it; the recorded run never replayed \
                 it)",
                id.as_str()
            );
        }
        for mismatch in &effective {
            let _ = writeln!(
                err,
                "  changed  effective {} configuration\n    expected sha256 {}\n    actual   \
                 sha256 {}",
                mismatch.what, mismatch.expected_sha256, mismatch.actual_sha256,
            );
        }
        let _ = writeln!(
            err,
            "{PREFIX} restore the recorded revision of this lab (for example, check out the commit \
             the run was made from) and try again."
        );
        return Err(RunDisposition::InvalidInput);
    }

    let pin = ReproductionPin::from_manifest(&manifest);
    Ok(Verified { config, pin })
}

/// Plans the reproduction, through the frozen entry point when the lab
/// file is conventionally named and the explicit one otherwise.
fn plan(manifest: &RunManifest, args: &ReproduceArgs) -> Result<ReproducePlan, ReproduceError> {
    match &args.config {
        Some(config) => plan_reproduction_from_config(manifest, config),
        None => plan_reproduction(manifest, &args.source_root),
    }
}

/// Reads and parses the run manifest, at any schema version this build
/// supports.
///
/// Goes through [`read_run_manifest`] rather than `serde_json` directly,
/// which is what makes `admissionlab reproduce` work on a manifest older
/// than the running build (ROADMAP Task 7.3): that function dispatches on
/// the document's own `schemaVersion` and leaves the fields a v1alpha1
/// writer never recorded honestly absent. Its errors already name the
/// problem precisely — including every schema version this build reads,
/// when the document's is not one of them — so this only adds which file
/// they are about.
///
/// `RunManifest` is `deny_unknown_fields`, so a document carrying a field
/// this build does not know still fails here rather than being silently
/// read with that field ignored — which for a provenance document would
/// mean reproducing from a record this build only partly understood.
fn read_manifest(path: &Path, err: &mut dyn std::io::Write) -> Result<RunManifest, RunDisposition> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        let _ = writeln!(
            err,
            "{PREFIX} failed to read the run manifest {}: {error}",
            path.display()
        );
        RunDisposition::InvalidInput
    })?;
    read_run_manifest(&text).map_err(|error| {
        let _ = writeln!(
            err,
            "{PREFIX} {} is not a run manifest this build can reproduce: {error}",
            path.display()
        );
        RunDisposition::InvalidInput
    })
}

/// Repeats what was pinned when the run died at a stage that a
/// no-longer-available recorded artifact would kill it at.
///
/// See this module's "Plan-time refusal versus run-time unavailability"
/// section. This is not a diagnosis — nothing here knows whether the
/// registry moved or the machine ran out of disk — it is the list of
/// recorded artifacts the run demanded, printed next to the failure so a
/// user can check them without re-reading the manifest.
fn report_possible_unavailability(
    pin: &ReproductionPin,
    disposition: RunDisposition,
    err: &mut dyn std::io::Write,
) {
    let stage = match disposition {
        RunDisposition::InfrastructureFailed => "creating the recorded clusters",
        RunDisposition::InstallationFailed => "installing the recorded components",
        _ => return,
    };
    let _ = writeln!(
        err,
        "{PREFIX} this reproduction failed while {stage}. It demanded exactly these recorded \
         artifacts, one of which may no longer be available:"
    );
    let _ = write!(err, "{}", pin.pinned_summary());
}
