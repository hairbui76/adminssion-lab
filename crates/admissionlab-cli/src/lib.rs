#![forbid(unsafe_code)]
//! The Admission Lab command-line tool, as a library.
//!
//! `admissionlab` (the binary in `src/main.rs`) is nothing but argument
//! parsing plus a dispatch into [`commands`]; everything it actually does
//! lives here. The split exists for one reason, stated plainly because it
//! is the only justification for a bin-only tool growing a library
//! target: `pipeline::run_lab` — the whole `admissionlab test` pipeline —
//! is driven in tests against fake cluster/install/capture backends, and
//! an integration test can only name those seams if they are part of a
//! library. `tests/cli.rs` still exercises the compiled binary as a user's
//! shell would; `tests/test_command.rs` exercises the pipeline's
//! decisions without needing Docker.
//!
//! - [`commands`] holds one module per subcommand (`doctor`, `test`,
//!   `reproduce`): argument parsing, the production backends, and
//!   terminal wording.
//! - [`pipeline`] holds the `admissionlab test` assembly itself — config
//!   through report — and its module documentation is the authoritative
//!   description of the stage order and of why this half of the product
//!   lives above `admissionlab-core` rather than inside it.
//! - [`exit`] holds the two mappings that decide what the process returns:
//!   every pipeline error to an `admissionlab_core::RunDisposition`, and
//!   every disposition to a process exit code.
//! - [`output`] holds `tracing` subscriber initialization.

pub mod commands;
pub mod exit;
pub mod output;
pub mod pipeline;
