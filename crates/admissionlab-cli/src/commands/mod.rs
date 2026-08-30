//! Subcommand entry points for the Admission Lab CLI.
//!
//! Each submodule owns one subcommand's Clap argument struct and the
//! function `main` dispatches into for it. `doctor` implements both its
//! shallow host-prerequisite checks (`kind`/`kubectl`/`helm`/`docker`
//! discovery, disk space — Task 1.4) and its real `--deep` cluster
//! create/verify/delete probe (Task 1.9); see `doctor`'s own module
//! documentation for both. `test`'s full pipeline still lands in Task
//! 1.10 (cluster lifecycle) and Task 4.14 (config through report); until
//! then it reports [`not_implemented`] rather than doing — or claiming
//! to do — anything.

use std::process::ExitCode;

use admissionlab_core::RunDisposition;

use crate::exit;

pub mod doctor;
pub mod test;

/// Reports that `command` has no implementation in the current phase of
/// Admission Lab, and returns the exit code for that outcome.
///
/// This prints exactly one line, unconditionally, to **stderr** — never
/// to stdout, and never through `tracing` (whose output is subject to the
/// `RUST_LOG`/`--verbose` filter and so could otherwise be configured
/// into silence). The message states plainly that nothing was attempted —
/// no cluster was created, nothing was installed, no fixture was
/// replayed, no prerequisite was checked — so a caller can never mistake
/// "not implemented yet" for "ran and passed."
///
/// Maps to [`RunDisposition::InternalError`]: a command having no
/// implementation yet is not the user's input, the lab infrastructure,
/// component installation, or fixture execution failing — it is
/// Admission Lab itself not being ready, which is exactly what
/// `InternalError` describes.
fn not_implemented(command: &str) -> ExitCode {
    eprintln!(
        "admissionlab {command}: not implemented in this phase of Admission \
         Lab. Nothing was created, installed, checked, or compared."
    );
    exit::code_for_disposition(RunDisposition::InternalError)
}
