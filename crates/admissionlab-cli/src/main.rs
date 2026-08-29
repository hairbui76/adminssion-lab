#![forbid(unsafe_code)]

mod commands;
mod exit;
mod output;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use commands::doctor::DoctorArgs;
use commands::test::TestArgs;

/// Admission Lab: creates baseline and candidate ephemeral Kubernetes
/// clusters, installs admission stacks into both, replays fixtures
/// through each, and reports semantic behavioral regressions between
/// them.
#[derive(Debug, Parser)]
#[command(name = "admissionlab", version)]
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    output::init_tracing(cli.verbose);
    tracing::info!("admissionlab starting");

    match cli.command {
        Commands::Doctor(args) => commands::doctor::run(&args),
        Commands::Test(args) => commands::test::run(&args),
    }
}
