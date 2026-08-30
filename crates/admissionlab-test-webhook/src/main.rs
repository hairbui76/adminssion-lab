#![forbid(unsafe_code)]
//! `admissionlab-test-webhook`: Admission Lab's own deterministic
//! dogfood admission webhook (PRODUCT.md §30, Task 2.7).
//!
//! Admission Lab compares baseline and candidate admission stacks; to
//! test *itself* it needs a component whose admission behavior is known
//! and deterministic, so a change in a vendor's release (Kyverno,
//! Istio, ...) never breaks Admission Lab's own test suite for reasons
//! unrelated to Admission Lab (PRODUCT.md §30: "This prevents core
//! tests from depending entirely on external vendor behavior").
//!
//! # Scope of this task
//!
//! Task 2.7's brief is explicit: "Modify `crates/admissionlab-test-webhook/*`
//! only enough to build a health endpoint; mutation behavior lands
//! Phase 3." This binary therefore implements exactly two subcommands,
//! run as this recipe's Deployment's init and main containers
//! respectively (`recipes/test-webhook/manifests/30-deployment.yaml`):
//!
//! - [`Command::Bootstrap`] ([`bootstrap::run`]): generates a fresh,
//!   test-only, per-cluster CA and serving certificate, writes the
//!   serving certificate/key where `serve` reads them from, and updates
//!   this cluster's `ValidatingWebhookConfiguration` so its `caBundle`
//!   validates that certificate. See [`bootstrap`]'s own module
//!   documentation for the full design, and this task's own report for
//!   why this approach was chosen over the alternatives Task 2.7's brief
//!   raised.
//! - [`Command::Serve`] ([`serve::run`]): a minimal HTTPS server
//!   answering `GET /healthz` — nothing else. PRODUCT.md §30's
//!   controlled admission behaviors (allow, deny, add label, add/remove
//!   container, controlled delay, controlled failure, and so on) are
//!   Task 3.9's job; there is no admission-review request/response
//!   handling anywhere in this crate.
//!
//! Both modes log via `tracing` (`RUST_LOG` controls verbosity; `info`
//! by default) rather than printing directly, so pod logs stay
//! structured and filterable the same way every other binary in this
//! workspace already logs.

mod bootstrap;
mod cert;
mod config;
mod serve;

use std::process::ExitCode;

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
    /// Generate this cluster's CA/serving certificate and update the
    /// installed `ValidatingWebhookConfiguration`'s `caBundle`. Run once,
    /// as this recipe's Deployment's init container.
    Bootstrap,
    /// Serve `GET /healthz` over HTTPS. Run as this recipe's
    /// Deployment's main container, after `bootstrap` has already
    /// written a serving certificate for it to read.
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
