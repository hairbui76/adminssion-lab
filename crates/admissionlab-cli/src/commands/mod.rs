//! Subcommand entry points for the Admission Lab CLI.
//!
//! Each submodule owns one subcommand's Clap argument struct and the
//! function `main` dispatches into for it. `doctor` implements both its
//! shallow host-prerequisite checks (`kind`/`kubectl`/`helm`/`docker`
//! discovery, disk space — Task 1.4) and its real `--deep` cluster
//! create/verify/delete probe (Task 1.9); see `doctor`'s own module
//! documentation for both. As of Task 4.14, `test` runs the whole lab:
//! configuration, host prerequisites, clusters, stacks, fixtures,
//! comparison, policy, reports, and cleanup. That assembly itself lives
//! in [`crate::pipeline`]; this module owns only the command's argument
//! surface, its production backends, and its exit code.

pub mod doctor;
pub mod test;
