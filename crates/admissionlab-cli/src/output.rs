//! Terminal output for the Admission Lab CLI: `tracing` subscriber
//! initialization.
//!
//! This module owns *how* the CLI configures its logging output, not
//! *whether* a given invocation should be verbose or which subcommand is
//! running — argument parsing (Clap, `--verbose`, subcommands, exit
//! codes) belongs to a later task. [`init_tracing`] therefore takes an
//! already-decided verbosity flag rather than reading `std::env::args()`
//! itself. As later tasks give crates something to say through
//! `admissionlab-core`'s `Diagnostic` type, the code that renders one to
//! the terminal belongs here too, alongside the subscriber it is
//! rendered through.

use tracing_subscriber::EnvFilter;

/// `tracing` filter applied when `RUST_LOG` is unset and `verbose` is
/// `false`: `warn` for dependencies, `info` and above for Admission Lab's
/// own crates (all named with the shared `admissionlab` prefix). This is
/// deliberately below `debug`, so a plain invocation never prints
/// debug-level raw Kubernetes object dumps.
const DEFAULT_FILTER: &str = "warn,admissionlab=info";

/// `tracing` filter applied when `RUST_LOG` is unset and `verbose` is
/// `true`: dependencies stay capped at `warn` so third-party HTTP/client
/// crates don't flood the terminal, while Admission Lab's own crates are
/// raised to `debug`.
const VERBOSE_FILTER: &str = "warn,admissionlab=debug";

/// Initializes the process-global `tracing` subscriber.
///
/// `RUST_LOG` always takes precedence over `verbose` when set, so an
/// operator can request any filter — including `trace` — explicitly.
/// When `RUST_LOG` is unset, `verbose` selects between [`DEFAULT_FILTER`]
/// and [`VERBOSE_FILTER`]; neither reaches `debug` for anything outside
/// Admission Lab's own crates, and only an explicit `RUST_LOG` override
/// reaches `trace`.
///
/// Call this once, near the start of `main`. This module does not parse
/// arguments: pass in the already-decided `verbose` flag rather than
/// reading `std::env::args()` here.
///
/// # Panics
///
/// Panics if a global `tracing` subscriber has already been installed for
/// this process.
pub fn init_tracing(verbose: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(if verbose {
            VERBOSE_FILTER
        } else {
            DEFAULT_FILTER
        })
    });

    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::EnvFilter;

    use super::{DEFAULT_FILTER, VERBOSE_FILTER};

    /// Splits a rendered `EnvFilter`'s `Display` form (comma-separated
    /// directives, for example `"admissionlab=info,warn"`) into its
    /// individual directive tokens, so tests can check for an exact
    /// directive rather than a substring that a different, unintended
    /// directive could also satisfy.
    fn directives(filter: &EnvFilter) -> Vec<String> {
        filter.to_string().split(',').map(str::to_owned).collect()
    }

    #[test]
    fn default_filter_does_not_enable_debug_anywhere() {
        let rendered = EnvFilter::new(DEFAULT_FILTER).to_string();
        assert!(
            !rendered.contains("debug"),
            "default filter must stay below debug level, got {rendered:?}"
        );
    }

    #[test]
    fn default_filter_shows_info_for_admissionlab_crates() {
        let tokens = directives(&EnvFilter::new(DEFAULT_FILTER));
        assert!(
            tokens.iter().any(|d| d == "admissionlab=info"),
            "expected an admissionlab=info directive, got {tokens:?}"
        );
    }

    #[test]
    fn verbose_filter_raises_admissionlab_crates_to_debug() {
        let tokens = directives(&EnvFilter::new(VERBOSE_FILTER));
        assert!(
            tokens.iter().any(|d| d == "admissionlab=debug"),
            "expected an admissionlab=debug directive, got {tokens:?}"
        );
    }

    #[test]
    fn verbose_filter_still_caps_dependencies_at_warn() {
        // Verbose mode must not become a firehose of third-party HTTP or
        // Kubernetes client noise: dependencies (anything outside the
        // `admissionlab=...` directive) stay at the global `warn` level,
        // never an unscoped `debug`.
        let tokens = directives(&EnvFilter::new(VERBOSE_FILTER));
        assert!(
            tokens.iter().any(|d| d == "warn"),
            "expected a global warn directive, got {tokens:?}"
        );
        assert!(
            !tokens.iter().any(|d| d == "debug"),
            "verbose mode must not set an unscoped debug level, got {tokens:?}"
        );
    }
}
