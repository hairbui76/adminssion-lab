//! `admissionlab test`: arguments, the production backends, and the exit
//! code.
//!
//! The run itself — configuration, prerequisites, clusters, stacks,
//! fixtures, comparison, policy, reports, cleanup — is
//! [`crate::pipeline::run_lab`], and that module's documentation is the
//! authoritative description of the stage order and why it is that
//! order. This module contributes three things and nothing else: the
//! command's argument surface, the concrete backends
//! [`crate::pipeline::LabBackend`] is satisfied with in production, and
//! the translation from the run's [`RunDisposition`] to the process's
//! [`ExitCode`] (via [`crate::exit`], which owns the mapping).
//!
//! # What changed with Task 4.14
//!
//! Until this task, this command deliberately refused to pretend: it
//! created and destroyed two clusters and then said plainly that fixture
//! execution and comparison were not implemented, exiting `6` so nothing
//! could mistake it for a pass. That constraint has been *satisfied*,
//! not relaxed — the pipeline now genuinely replays fixtures through two
//! real API servers and compares what they did, so exit `0` finally
//! means what it says. The honesty rule underneath it is unchanged and
//! still load-bearing everywhere in the pipeline: a run that could not
//! observe something reports it as unavailable
//! (`FixtureBucket::Inconclusive`, `TraceEvidence::Unavailable`,
//! `DivergenceConfidence::Unknown`) rather than as agreement.
//!
//! # Per-webhook metrics are collected
//!
//! [`KubeFixtureCapture`] takes no `/metrics` samples by default,
//! because they are optional by construction (Global Constraint 19) and
//! cost one full kube-apiserver `/metrics` render per fixture request,
//! serially. This command turns them **on**, and the reason is that
//! `policy.latency` is a real, documented, user-configurable part of the
//! product (`admissionlab_spec::LatencyPolicy`, with an Alpha default of
//! "100ms slower *and* 2x baseline"): without metric samples every
//! observed latency is `None`, `webhook_latency_changed` can never be
//! emitted, and that configuration silently does nothing. Paying two
//! scrapes per fixture to make a configured policy mean something is the
//! right trade, and it stays non-fatal in every failure mode — a scrape
//! that fails leaves the latency `None`, exactly as if metrics were off,
//! and never fails a fixture or a run.

use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use admissionlab_admission::{KubeFixtureCapture, KubeMetricsSource};
use admissionlab_cluster::KindClusterManager;
use admissionlab_core::{
    ArtifactStore, DoctorReport, ProcessRunner, ProcessSpawner, RunPaths, TokioProcessRunner,
    collect_doctor_report,
};
use admissionlab_fixtures::FixtureSource;
use admissionlab_report::TerminalOptions;
use admissionlab_spec::GatewaySuiteSpec;
use async_trait::async_trait;
use clap::Args;

use crate::exit;
use crate::pipeline::gateway::KubeGatewaySuite;
use crate::pipeline::install::KubeStackInstaller;
use crate::pipeline::migration::KubeMigrationSuite;
use crate::pipeline::{Console, LabBackend, RunRequest, run_lab};

/// Arguments for `admissionlab test`.
#[derive(Debug, Args)]
pub struct TestArgs {
    /// Path to the lab configuration file.
    #[arg(value_name = "CONFIG")]
    pub config: PathBuf,

    /// Preserve baseline/candidate clusters after the run instead of
    /// deleting them. Prints each cluster's name, its kubeconfig path,
    /// and the exact `kind delete cluster` command to remove it by hand
    /// (PRODUCT.md §10.4).
    #[arg(long)]
    pub keep_clusters: bool,

    /// Write `result.json` and `report.html` into this directory instead
    /// of the run's own `reports/` directory. Created if it does not
    /// exist. The raw per-fixture evidence bundles always stay in the
    /// run workspace, which is where every path inside the reports
    /// points.
    #[arg(long, value_name = "DIR")]
    pub report_dir: Option<PathBuf>,

    /// Write a GitHub Actions job summary to this file (Markdown).
    ///
    /// Intended for the composite action in
    /// `.github/actions/admissionlab`, which appends the file to
    /// `$GITHUB_STEP_SUMMARY`: this command deliberately does not read
    /// that variable itself, so the same flag is useful outside GitHub
    /// and so nothing here has to guess what CI system it is running
    /// under. Parent directories are created.
    ///
    /// The file is written whatever happens — the run's verdict when it
    /// reached one, and otherwise a summary naming the stage that failed.
    /// It never states a verdict the run did not reach.
    #[arg(long, value_name = "FILE")]
    pub github_summary: Option<PathBuf>,
}

/// The directory this run's on-disk workspace is created under.
///
/// PRODUCT.md §10.3 describes this as "the configured cache/run root",
/// but nothing has added a way to configure it yet (no `--run-root`
/// flag, no config file setting), so this is a fixed default under the
/// OS temp directory — consistent with this codebase's existing
/// ephemeral-workspace precedents (`admissionlab-cluster`'s
/// `tests/kind_smoke.rs`, `admissionlab-cli`'s own `doctor --deep`
/// probe). `--report-dir` is the escape hatch that matters in practice:
/// it puts the artifacts a user actually reads wherever they want them,
/// without moving the workspace `kind` bind-mounts into.
#[must_use]
pub fn default_run_root() -> PathBuf {
    std::env::temp_dir().join("admissionlab-runs")
}

/// The production [`LabBackend`]: real `kind` clusters, real
/// `helm`/`kubectl` installs, real server-side dry-run capture against
/// real API servers (Global Constraints 2, 3, and 16).
///
/// `pub` because `commands::reproduce` runs the *same* world with one
/// thing added — the recorded environment pin (ROADMAP Task 5.3) — and a
/// second, separately-constructed production backend would be a second
/// place for "which cluster/install/capture implementations are real" to
/// be answered, and eventually to be answered differently.
pub struct KindBackend {
    /// Every external command — `kind`, `helm`, `kubectl`, `docker` —
    /// goes through this one runner, so every invocation inherits its
    /// timeout, separate stdout/stderr capture, and structured error
    /// context (Global Constraint 13).
    process_runner: Arc<dyn ProcessRunner>,
    /// The `kind` backend, built once from `process_runner`.
    cluster_manager: Arc<KindClusterManager>,
    /// The same runner again, behind the trait a long-lived child needs.
    ///
    /// `TokioProcessRunner` implements both [`ProcessRunner`] (a command
    /// run to completion) and [`ProcessSpawner`] (a child whose stdout is
    /// read while it keeps running), and Rust cannot upcast one `Arc<dyn
    /// Trait>` to a sibling trait object -- so the one value is held
    /// under both views rather than constructed twice, which is what
    /// keeps `kubectl port-forward` inheriting the same argv-only, no-
    /// shell, redaction-aware discipline every other command does.
    port_forward_spawner: Arc<dyn ProcessSpawner>,
    /// Which filesystem the free-space check reports on. The run
    /// workspace and every `kind` node's storage live under the OS temp
    /// directory, so that is the filesystem whose free space actually
    /// decides whether this run can finish.
    disk_check_path: PathBuf,
}

impl KindBackend {
    /// Builds the production backend for a run rooted at `run_root`.
    #[must_use]
    pub fn new(run_root: &std::path::Path) -> Self {
        let tokio_runner = Arc::new(TokioProcessRunner::new());
        let process_runner: Arc<dyn ProcessRunner> = tokio_runner.clone();
        Self {
            cluster_manager: Arc::new(KindClusterManager::new(Arc::clone(&process_runner))),
            port_forward_spawner: tokio_runner,
            process_runner,
            // The root itself may not exist yet on a first run, and the
            // free-space check needs an existing path; its parent (the
            // OS temp directory) is on the same filesystem and always
            // exists.
            disk_check_path: run_root
                .parent()
                .map_or_else(std::env::temp_dir, std::path::Path::to_path_buf),
        }
    }
}

#[async_trait]
impl LabBackend for KindBackend {
    type Clusters = KindClusterManager;
    type Installer = KubeStackInstaller;
    type Capture = KubeFixtureCapture;
    type Gateway = KubeGatewaySuite;
    type Migration = KubeMigrationSuite;

    async fn doctor_report(&self) -> DoctorReport {
        // The same function `admissionlab doctor` calls, rather than a
        // second prerequisite check that could disagree with it (see
        // `commands::doctor`'s "One check, two callers").
        collect_doctor_report(self.process_runner.as_ref(), &self.disk_check_path).await
    }

    fn cluster_manager(&self) -> Arc<Self::Clusters> {
        Arc::clone(&self.cluster_manager)
    }

    fn stack_installer(&self, paths: &RunPaths) -> Self::Installer {
        KubeStackInstaller::new(Arc::clone(&self.process_runner), paths)
    }

    fn gateway_suite(&self, suite: GatewaySuiteSpec, store: ArtifactStore) -> Self::Gateway {
        // The same `ProcessRunner` every other external command in this
        // run goes through, handed over as the `ProcessSpawner` a
        // long-lived `kubectl port-forward` needs -- `TokioProcessRunner`
        // implements both, and which one an API takes is the whole of
        // the distinction (see `admissionlab_core::process`).
        KubeGatewaySuite::new(suite, store, Arc::clone(&self.port_forward_spawner))
    }

    fn migration_suite(
        &self,
        suite: admissionlab_spec::MigrationSuiteSpec,
        store: ArtifactStore,
    ) -> Self::Migration {
        // The same spawner, for the same reason: a migration case opens
        // a `kubectl port-forward` into each side's data plane.
        KubeMigrationSuite::new(suite, store, Arc::clone(&self.port_forward_spawner))
    }

    fn fixture_capture(&self, fixtures: Vec<FixtureSource>, store: ArtifactStore) -> Self::Capture {
        // Metrics on: see this module's documentation for why the cost
        // is worth it and why it can never fail a run.
        KubeFixtureCapture::new(fixtures, store).with_metrics(Arc::new(KubeMetricsSource::new()))
    }
}

/// Runs `admissionlab test`.
///
/// # Panics
///
/// Panics only if this process's own `tokio` runtime cannot be built at
/// all (for example, the OS refuses to create the underlying I/O
/// driver) — an environmental failure with no sensible fallback, not a
/// condition any configuration or cluster state can trigger. Mirrors
/// `commands::doctor::run`'s identical convention.
#[must_use]
pub fn run(args: &TestArgs) -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build the test command's tokio runtime");

    let run_root = default_run_root();
    let backend = KindBackend::new(&run_root);
    let request = RunRequest {
        config: &args.config,
        keep_clusters: args.keep_clusters,
        report_dir: args.report_dir.as_deref(),
        github_summary: args.github_summary.as_deref(),
        run_root,
    };

    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    let mut console = Console {
        // The two observations `TerminalOptions::for_stream` documents
        // as the caller's to make, made here where the streams actually
        // are: color only for a real terminal, and never when `NO_COLOR`
        // is set to anything at all (its *presence* is the signal).
        terminal: TerminalOptions::for_stream(
            out.is_terminal(),
            std::env::var_os("NO_COLOR").is_none(),
        ),
        out: &mut out,
        err: &mut err,
    };

    let disposition = runtime.block_on(run_lab(&backend, &request, &mut console));
    exit::code_for_disposition(disposition)
}
