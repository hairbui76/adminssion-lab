//! Subcommand entry points for the Admission Lab CLI.
//!
//! Each submodule owns one subcommand's Clap argument struct and the
//! function `main` dispatches into for it. `doctor` implements both its
//! shallow host-prerequisite checks (`kind`/`kubectl`/`helm`/`docker`
//! discovery, disk space — Task 1.4) and its real `--deep` cluster
//! create/verify/delete probe (Task 1.9); see `doctor`'s own module
//! documentation for both. As of Task 1.10, `test` loads and validates a
//! lab configuration and creates/destroys its baseline and candidate
//! clusters; it does not yet install components, discover or replay
//! fixtures, or compare results — Task 4.14 wires that remainder. See
//! `test`'s own module documentation for exactly what it reports about
//! that gap and why.

pub mod doctor;
pub mod test;
