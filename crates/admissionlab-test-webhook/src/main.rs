#![forbid(unsafe_code)]
//! `admissionlab-test-webhook`'s command line front end: argument
//! parsing, logging setup, and an exit code — nothing else.
//!
//! Every behavior this binary has lives in the library crate beside it
//! (`src/lib.rs`), which documents the component as a whole and explains
//! why it is a library at all. This file deliberately holds no logic
//! that a test would want to reach.

use std::process::ExitCode;

use admissionlab_test_webhook::{bootstrap, serve};
use clap::{Parser, Subcommand};

/// Admission Lab's deterministic dogfood admission webhook.
#[derive(Debug, Parser)]
#[command(name = "admissionlab-test-webhook", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate this cluster's CA/serving certificate and update every
    /// installed webhook configuration's `caBundle`. Run once, as this
    /// recipe's Deployment's init container.
    Bootstrap,
    /// Serve `GET /healthz` and the admission-review routes over HTTPS.
    /// Run as this recipe's Deployment's main container, after
    /// `bootstrap` has already written a serving certificate for it to
    /// read.
    Serve,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let result = match cli.command {
        Command::Bootstrap => bootstrap::run().await.map_err(|error| error.to_string()),
        Command::Serve => serve::run().await.map_err(|error| error.to_string()),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            tracing::error!("{message}");
            ExitCode::FAILURE
        }
    }
}
