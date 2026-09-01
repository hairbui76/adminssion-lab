#![forbid(unsafe_code)]
//! `admissionlab-echo`'s command line front end: argument parsing,
//! logging setup, configuration, and an exit code -- nothing else.
//!
//! Every behavior this binary has lives in the library crate beside it
//! (`src/lib.rs`), which documents the component as a whole and the
//! frozen response contract it serves. This file deliberately holds no
//! logic a test would want to reach.
//!
//! # Why parse a command line with no arguments in it
//!
//! This binary takes no subcommands and no flags of its own: its two
//! inputs are environment variables (`admissionlab_echo::config`), the
//! way a container's configuration usually arrives. `clap` still runs,
//! for two reasons that cost one struct: `--version` reports the build
//! an image actually contains, and a mistyped `args:` entry in a
//! fixture manifest is rejected loudly at startup instead of being
//! silently ignored by a binary that never looked at its own argv.

use std::process::ExitCode;

use admissionlab_echo::config::EchoConfig;
use admissionlab_echo::serve;
use clap::Parser;

/// Admission Lab's deterministic HTTP echo backend.
///
/// Serves `GET /healthz` and echoes every other request as JSON naming
/// the backend that answered it. Configured entirely through
/// `ADMISSIONLAB_BACKEND_ID` (required) and `ADMISSIONLAB_ECHO_DELAY_MS`
/// (optional).
#[derive(Debug, Parser)]
#[command(name = "admissionlab-echo", version, about)]
struct Cli {}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let Cli {} = Cli::parse();

    // Read before binding anything: a backend that does not know which
    // backend it is must never serve a request, because a response
    // carrying the wrong (or a defaulted) identity is worse than no
    // response at all -- it reads as "no routing change" to Task 6.9's
    // comparator. See `admissionlab_echo::config`'s own documentation.
    let config = match EchoConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            tracing::error!("{error}");
            return ExitCode::FAILURE;
        }
    };

    match serve::run(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}
