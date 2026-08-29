//! `admissionlab doctor` argument parsing and entry point.
//!
//! Checking host prerequisites (`kind`, `kubectl`, `helm`, `docker`) is
//! Task 1.4's job; the real `--deep` cluster-probe behavior is Task 1.9's.
//! This module only declares the command surface those tasks fill in.

use std::process::ExitCode;

use clap::Args;

use super::not_implemented;

/// Arguments for `admissionlab doctor`.
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Additionally probe by creating and deleting a real ephemeral
    /// cluster.
    ///
    /// Declared now to lock in the documented command surface. Task 1.4
    /// hides this flag from release builds until Task 1.9 implements the
    /// real probe; in this phase it has no effect at all — `doctor`
    /// performs no checks of any kind yet, deep or otherwise, and never
    /// inspects this value.
    #[arg(long)]
    pub deep: bool,
}

/// Runs `admissionlab doctor`.
///
/// This phase implements no prerequisite checks: it does not shell out to
/// `kind`, `kubectl`, `helm`, or `docker`. It always reports that
/// `doctor` is not implemented yet, regardless of `args`.
pub fn run(_args: &DoctorArgs) -> ExitCode {
    not_implemented("doctor")
}
