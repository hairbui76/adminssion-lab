#![forbid(unsafe_code)]
//! The `admissionlab` binary: argument parsing and dispatch, and nothing
//! else. Every behavior it invokes lives in this crate's library half
//! (see `lib.rs`'s module documentation for why that split exists).
//!
//! # The frozen v1 command surface (ROADMAP Task 9.2)
//!
//! Three commands, and exactly these flags:
//!
//! ```text
//! admissionlab [-v|--verbose] doctor    [--deep]
//! admissionlab [-v|--verbose] test      <CONFIG>
//!                                       [--keep-clusters]
//!                                       [--report-dir <DIR>]
//!                                       [--github-summary <FILE>]
//! admissionlab [-v|--verbose] reproduce <MANIFEST>
//!                                       [--source-root <DIR>]
//!                                       [--config <FILE>]
//!                                       [--keep-clusters]
//!                                       [--report-dir <DIR>]
//! ```
//!
//! Everything above is a **public contract** as of v1: no command,
//! positional argument, or long flag in that list may be renamed or
//! removed, and none may change from taking a value to not taking one
//! (or the reverse), because a user's shell script and a CI workflow
//! step are both written against exactly these spellings. Adding a new
//! optional flag with a backwards-compatible default is the only change
//! that stays inside the contract.
//!
//! `tests/exit_codes.rs` pins the list mechanically: it parses the
//! `--help` output of the root and of every subcommand down to just the
//! command names, positional value names, and option spellings, and
//! compares that against the table above. A flag added, renamed, or
//! dropped fails that test loudly rather than reaching a release, while
//! rewording any *description* is deliberately free — the golden is
//! trimmed to the surface a script can depend on, and nothing else.
//!
//! `-v`/`--verbose` is `global = true` and therefore accepted both
//! before and after the subcommand; the two spellings are equivalent
//! everywhere.
//!
//! # `--help`, `--version`, and no arguments at all
//!
//! `--help`/`-h` and `--version`/`-V` **always exit `0`**, on the root
//! and on every subcommand — `propagate_version` is set below precisely
//! so `admissionlab test --version` answers rather than erroring, which
//! makes "ask any level of this CLI what it is" a uniform rule instead
//! of a root-only special case.
//!
//! `admissionlab` with no arguments at all prints the root help to
//! **stderr** and exits `2`. That is Clap's default for a missing
//! required subcommand, and it is frozen deliberately rather than
//! softened to `0`: a bare invocation is a usage mistake, exit `2` is
//! already this tool's "invalid user input" code (ROADMAP §0.4), and a
//! script that forgot to pass its subcommand must fail rather than look
//! like a passing run.
//!
//! # Exit codes
//!
//! The 0-6 contract every command answers with is owned, documented,
//! and frozen in [`admissionlab_cli::exit`], and reproduced for users in
//! `docs/troubleshooting.md`.
//!
//! A run an operator *interrupts* answers `130` (`SIGINT`) or `143`
//! (`SIGTERM`) instead — deliberately outside that table, because such a
//! run reached none of the seven conclusions it assigns. That decision,
//! and why `128 + signal` rather than an eighth meaning, is argued in
//! [`admissionlab_cli::exit::code_for_cancellation`]; what the process
//! does between the signal and the exit is
//! [`admissionlab_cli::cancel`].

use std::process::ExitCode;

use admissionlab_cli::commands;
use admissionlab_cli::output;
use clap::{Parser, Subcommand};

use commands::doctor::DoctorArgs;
use commands::reproduce::ReproduceArgs;
use commands::test::TestArgs;

/// Admission Lab: creates baseline and candidate ephemeral Kubernetes
/// clusters, installs admission stacks into both, replays fixtures
/// through each, and reports semantic behavioral regressions between
/// them.
///
/// `propagate_version` is what makes `--version` answer on every
/// subcommand and not only on the root; see this module's documentation
/// for why that is part of the frozen contract.
#[derive(Debug, Parser)]
#[command(name = "admissionlab", version, propagate_version = true)]
struct Cli {
    /// Raise Admission Lab's own crates to `debug`-level logging.
    /// Dependencies stay capped at `warn`. Ignored whenever `RUST_LOG` is
    /// set — `RUST_LOG` always wins.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

/// Top-level Admission Lab subcommands.
#[derive(Debug, Subcommand)]
enum Commands {
    /// Check host prerequisites for running Admission Lab.
    Doctor(DoctorArgs),
    /// Create baseline/candidate clusters, replay fixtures, and report
    /// behavioral regressions for one lab configuration.
    Test(TestArgs),
    /// Re-run a recorded run from its run manifest, against the same
    /// source files, Kubernetes versions, node images, and component
    /// versions it used.
    Reproduce(ReproduceArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    output::init_tracing(cli.verbose);
    tracing::info!("admissionlab starting");

    match cli.command {
        Commands::Doctor(args) => commands::doctor::run(&args),
        Commands::Test(args) => commands::test::run(&args),
        Commands::Reproduce(args) => commands::reproduce::run(&args),
    }
}
