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
//!
//! `reproduce` (Task 5.3) is that same pipeline run against a recorded
//! run manifest: it verifies the source tree against the digests the
//! manifest recorded, pins both sides' node images and component
//! versions to what that run actually used, and then calls
//! [`crate::pipeline::run_lab`] through `test`'s own production backend.
//! See its module documentation for the plan-time/run-time split.

pub mod doctor;
pub mod reproduce;
pub mod test;
