//! `admissionlab test` argument parsing and entry point.
//!
//! The real pipeline — config loading, cluster orchestration, stack
//! installation, fixture replay, semantic diff, and reporting — is wired
//! up by Task 1.10 (cluster lifecycle) and Task 4.14 (config through
//! report). This module only declares the command surface those tasks
//! fill in.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;

use super::not_implemented;

/// Arguments for `admissionlab test`.
#[derive(Debug, Args)]
pub struct TestArgs {
    /// Path to the lab configuration file.
    #[arg(value_name = "CONFIG")]
    pub config: PathBuf,

    /// Preserve baseline/candidate clusters after the run instead of
    /// deleting them.
    ///
    /// Declared now to lock in the documented command surface; Task 1.10
    /// implements cluster orchestration and gives this flag its real
    /// preserve-and-print-cleanup-commands behavior. In this phase no
    /// cluster is ever created, so there is nothing for this flag to
    /// preserve, and it is never inspected.
    #[arg(long)]
    pub keep_clusters: bool,
}

/// Runs `admissionlab test`.
///
/// # Honesty constraint
///
/// This phase implements none of the lab pipeline: no configuration is
/// loaded from `args.config`, no cluster is created, nothing is
/// installed, and no fixture is replayed. This function must never be
/// changed to exit successfully or to print output that could be
/// mistaken for a completed — let alone passing — lab run; it must only
/// ever report, honestly and unconditionally, that `test` is not
/// implemented yet.
pub fn run(_args: &TestArgs) -> ExitCode {
    not_implemented("test")
}
