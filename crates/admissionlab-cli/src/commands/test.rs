//! `admissionlab test` argument parsing and entry point.
//!
//! As of Task 1.10, this loads and validates a lab configuration and
//! creates/destroys its baseline and candidate clusters through
//! [`admissionlab_core::LabRunner`]. Stack installation, fixture
//! discovery/replay, semantic diff, and reporting are still Task 4.14's
//! job — this module never implements, wires, or claims to have run any
//! of them.
//!
//! # Honesty constraint
//!
//! `admissionlab test` must never exit `0` (as [`RunDisposition::Passed`]
//! does) and must never print anything implying a lab actually ran to
//! completion: no `PASS`/`FAIL`, no regression verdict, nothing
//! suggesting a fixture was replayed or baseline/candidate behavior was
//! compared. What it *can* now truthfully report is exactly what it did:
//! whether the configuration loaded, and whether the two clusters were
//! created and then deleted (or preserved). [`run_async`] prints those
//! facts plainly, then states just as plainly, every time it gets that
//! far, that fixture execution and comparison are not implemented yet —
//! so a user reading either stream comes away with an accurate picture
//! of how far the tool got, never a false impression that more happened.
//!
//! # Exit codes
//!
//! - [`RunDisposition::InvalidInput`] (2): `args.config` failed to load
//!   or resolve ([`load_lab`]/[`resolve_lab`]), *or* a configured
//!   Kubernetes version could not be resolved to a node image
//!   ([`RunError::NodeImageResolutionFailed`] — Controller Ruling R25).
//!   The latter is still the user's configuration at fault (they asked
//!   for a version Admission Lab does not know how to provision), not
//!   the lab's infrastructure, and it is always discovered before any
//!   cluster is created — the same reason an empty or malformed
//!   `kubernetes:` field is `InvalidInput` rather than
//!   `InfrastructureFailed`.
//! - [`RunDisposition::InfrastructureFailed`] (3): cluster creation
//!   itself failed, or cleanup could not fully delete both clusters. A
//!   cluster that might still be running is a more urgent, more specific
//!   problem than "the rest of the pipeline isn't implemented," so it
//!   earns its own distinct code rather than being folded into the
//!   generic not-yet-implemented outcome below.
//! - [`RunDisposition::InternalError`] (6): everything this phase
//!   implements succeeded (configuration loaded, both clusters created,
//!   then deleted or preserved as requested) but fixture execution and
//!   comparison — required for any real pass/fail verdict — are not
//!   implemented yet. This is deliberately *not* `0`: nothing this run
//!   did amounts to a completed lab, so there is no result to call a
//!   pass, even though both clusters genuinely existed for a moment.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use admissionlab_cluster::KindClusterManager;
use admissionlab_core::{
    ArtifactStore, LabRunner, ProcessRunner, RunDisposition, RunError, RunOptions,
    TokioProcessRunner, preserved_cluster_report,
};
use admissionlab_spec::{load_lab, resolve_lab};
use clap::Args;

use crate::exit;

/// Arguments for `admissionlab test`.
#[derive(Debug, Args)]
pub struct TestArgs {
    /// Path to the lab configuration file.
    #[arg(value_name = "CONFIG")]
    pub config: PathBuf,

    /// Preserve baseline/candidate clusters after the run instead of
    /// deleting them. On success, prints each cluster's name, its
    /// kubeconfig path, and the exact `kind delete cluster` command to
    /// remove it by hand (PRODUCT.md §10.4).
    #[arg(long)]
    pub keep_clusters: bool,
}

/// The directory this run's on-disk workspace is created under.
///
/// PRODUCT.md §10.3 describes this as "the configured cache/run root",
/// but no task before this one has added a way to configure it (no
/// `--run-root` flag, no config file setting), so this is a fixed
/// default under the OS temp directory — consistent with this
/// codebase's existing ephemeral-workspace precedents
/// (`admissionlab-cluster`'s `tests/kind_smoke.rs`,
/// `admissionlab-cli`'s own `doctor --deep` probe). Making this
/// configurable is left to whichever later task actually needs runs to
/// persist somewhere the user chose.
fn default_run_root() -> PathBuf {
    std::env::temp_dir().join("admissionlab-runs")
}

/// Runs `admissionlab test`. See this module's documentation for the
/// exact honesty constraint and exit-code mapping this implements.
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
    runtime.block_on(run_async(args))
}

/// [`run`]'s async core.
async fn run_async(args: &TestArgs) -> ExitCode {
    let loaded = match load_lab(&args.config) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("admissionlab test: failed to load lab configuration: {error}");
            return exit::code_for_disposition(RunDisposition::InvalidInput);
        }
    };
    let lab = match resolve_lab(loaded) {
        Ok(lab) => lab,
        Err(error) => {
            eprintln!("admissionlab test: invalid lab configuration: {error}");
            return exit::code_for_disposition(RunDisposition::InvalidInput);
        }
    };

    let run_root = default_run_root();
    let process_runner: Arc<dyn ProcessRunner> = Arc::new(TokioProcessRunner::new());
    let runner = LabRunner {
        cluster_manager: Arc::new(KindClusterManager::new(process_runner)),
        artifact_store: ArtifactStore::new(&run_root),
    };
    let options = RunOptions {
        keep_clusters: args.keep_clusters,
        run_root,
    };

    let prepared = match runner.prepare_clusters(&lab, &options).await {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("admissionlab test: failed to prepare lab clusters: {error}");
            if let RunError::ClusterCreationFailed { rollback, .. } = &error {
                for diagnostic in rollback {
                    eprintln!("admissionlab test: {}", diagnostic.message);
                }
            }
            // An unresolvable Kubernetes version is the user's
            // configuration at fault (see this module's documentation)
            // — every other `prepare_clusters` failure is a genuine
            // infrastructure problem. Matched exhaustively (no `_` arm)
            // so a future `RunError` variant forces a deliberate choice
            // here rather than silently falling into one bucket.
            let disposition = match &error {
                RunError::NodeImageResolutionFailed { .. } => RunDisposition::InvalidInput,
                RunError::NonAbsoluteRunRoot(_)
                | RunError::Workspace(_)
                | RunError::ClusterCreationFailed { .. } => RunDisposition::InfrastructureFailed,
            };
            return exit::code_for_disposition(disposition);
        }
    };

    println!(
        "admissionlab test: created baseline cluster {:?} and candidate cluster {:?}.",
        prepared.baseline.spec.name, prepared.candidate.spec.name
    );

    // What became of the two clusters, folded into the disclaimer below
    // so it reads accurately — and completely, on its own, without
    // requiring the earlier stdout lines — regardless of which mode
    // this run took.
    let cluster_outcome = if options.keep_clusters {
        println!("{}", preserved_cluster_report(&prepared));
        "left running, as requested by --keep-clusters"
    } else {
        let cleanup_diagnostics = runner.cleanup(&prepared).await;
        if cleanup_diagnostics.is_empty() {
            println!("admissionlab test: baseline and candidate clusters deleted.");
            "destroyed"
        } else {
            for diagnostic in &cleanup_diagnostics {
                eprintln!("admissionlab test: {}", diagnostic.message);
            }
            eprintln!(
                "admissionlab test: cleanup did not fully succeed; a cluster may still exist — \
                 see the delete command(s) above."
            );
            return exit::code_for_disposition(RunDisposition::InfrastructureFailed);
        }
    };

    // Reachable only once both clusters genuinely existed — never on a
    // configuration or infrastructure failure above, both of which
    // already returned their own, more specific exit code. Stated
    // plainly and unconditionally regardless: creating and destroying
    // (or preserving) two clusters is real infrastructure work, but it
    // is not a lab result, so this can never read as, or be mistaken
    // for, a pass.
    eprintln!(
        "admissionlab test: fixture execution and comparison are not implemented in this phase \
         of Admission Lab. Both clusters were created and {cluster_outcome}, but no fixtures \
         were replayed against them and no baseline/candidate behavior was compared — this is \
         not a pass or a fail."
    );
    exit::code_for_disposition(RunDisposition::InternalError)
}
