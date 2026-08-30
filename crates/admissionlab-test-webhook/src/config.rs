//! Environment-variable configuration this binary reads. Deliberately
//! tiny: only the two names that genuinely vary with how this recipe's
//! manifests name their own objects live here as environment variables.
//! Everything else that could plausibly be "configuration" (the shared
//! certificate directory, the listen port, the in-cluster service
//! account namespace file) is instead a fixed implementation constant in
//! [`crate::bootstrap`]/[`crate::serve`] — nothing in this deployment
//! ever needs a *different* value for any of those, so making them
//! environment variables would only be an extra way for the manifest and
//! the binary to silently disagree with each other.
//!
//! Both variables below are read only by `bootstrap` mode
//! ([`crate::bootstrap::run`]): `serve` mode needs neither (it only reads
//! certificate files and answers `/healthz` — see [`crate::serve`]'s own
//! module documentation for why it talks to no Kubernetes API at all).
//!
//! Required, not defaulted: a missing value here is a manifest/binary
//! wiring bug, and Global Constraint 15 ("unavailable data is unknown,
//! never fabricated") means guessing a plausible-looking default (for
//! example matching this recipe's own conventional object names) would
//! silently mask exactly that bug instead of surfacing it.
//!
//! [`read_required`] reads through [`std::env::var`] directly, but does
//! so via [`read_required_with`], which takes the lookup as a parameter
//! — this crate forbids `unsafe_code`, and `std::env::set_var`/
//! `remove_var` are `unsafe fn` as of this edition, so a test cannot
//! mutate the real process environment to exercise the missing/empty
//! paths (`admissionlab-cli`'s own `doctor` module documents the same
//! "parameterize the ambient lookup" reasoning for
//! `std::env::current_dir`). [`read_required_with`]'s tests below supply
//! a fake lookup closure instead — no real environment mutation, and no
//! cross-test interference to serialize against.

use std::env::VarError;

/// The `Service` object's name this recipe's Deployment answers behind —
/// used to compute the serving certificate's Subject Alternative Names
/// (`<name>.<namespace>.svc` and `<name>.<namespace>.svc.cluster.local`;
/// see [`crate::cert::generate`]).
pub const SERVICE_NAME_ENV: &str = "ADMISSIONLAB_TEST_WEBHOOK_SERVICE_NAME";

/// The `ValidatingWebhookConfiguration` object's name this recipe
/// installs — `bootstrap` mode fetches it by this name and updates its
/// `caBundle` (see [`crate::bootstrap::patch_ca_bundle`]).
pub const WEBHOOK_CONFIGURATION_NAME_ENV: &str =
    "ADMISSIONLAB_TEST_WEBHOOK_WEBHOOK_CONFIGURATION_NAME";

/// Something went wrong reading a required environment variable.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// `name` was not set at all.
    #[error("required environment variable {name} is not set")]
    Missing {
        /// The variable that was not set.
        name: &'static str,
    },
    /// `name` was set but is empty (or all whitespace).
    #[error("required environment variable {name} is set but empty")]
    Empty {
        /// The variable that was empty.
        name: &'static str,
    },
    /// `name` was set but is not valid Unicode.
    #[error("required environment variable {name} is not valid Unicode")]
    NotUnicode {
        /// The variable that was not valid Unicode.
        name: &'static str,
    },
}

/// Reads `name` from the real process environment, trimmed. Never falls
/// back to a default — see this module's own documentation for why.
///
/// # Errors
///
/// Returns [`ConfigError`] if `name` is unset, set but empty (after
/// trimming), or set but not valid Unicode.
pub fn read_required(name: &'static str) -> Result<String, ConfigError> {
    read_required_with(name, |key| std::env::var(key))
}

/// [`read_required`]'s implementation, generic over the lookup function
/// so it can be exercised without mutating the real process environment
/// — see this module's own documentation.
fn read_required_with(
    name: &'static str,
    lookup: impl FnOnce(&str) -> Result<String, VarError>,
) -> Result<String, ConfigError> {
    let raw = match lookup(name) {
        Ok(value) => value,
        Err(VarError::NotPresent) => return Err(ConfigError::Missing { name }),
        Err(VarError::NotUnicode(_)) => return Err(ConfigError::NotUnicode { name }),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::Empty { name });
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use std::env::VarError;

    use super::{ConfigError, read_required_with};

    const TEST_VAR: &str = "ADMISSIONLAB_TEST_WEBHOOK_CONFIG_TEST_VAR";

    #[test]
    fn missing_variable_is_reported_as_missing_not_empty() {
        let error = read_required_with(TEST_VAR, |_| Err(VarError::NotPresent))
            .expect_err("unset variable must be an error");
        assert_eq!(error, ConfigError::Missing { name: TEST_VAR });
    }

    #[test]
    fn not_unicode_variable_is_reported_distinctly() {
        let error = read_required_with(TEST_VAR, |_| Err(VarError::NotUnicode("\u{fffd}".into())))
            .expect_err("non-Unicode variable must be an error");
        assert_eq!(error, ConfigError::NotUnicode { name: TEST_VAR });
    }

    #[test]
    fn empty_variable_is_reported_as_empty_not_missing() {
        let error = read_required_with(TEST_VAR, |_| Ok("   ".to_owned()))
            .expect_err("all-whitespace variable must be an error");
        assert_eq!(error, ConfigError::Empty { name: TEST_VAR });
    }

    #[test]
    fn present_variable_is_trimmed() {
        let value =
            read_required_with(TEST_VAR, |_| Ok("  admissionlab-test-webhook  ".to_owned()))
                .expect("set, non-empty variable must succeed");
        assert_eq!(value, "admissionlab-test-webhook");
    }

    #[test]
    fn read_required_reads_through_the_real_environment() {
        // The one test that exercises `read_required` itself (not
        // `read_required_with`) -- proving the two are actually wired
        // together, not merely both independently correct. `PATH` is
        // conventionally present and non-empty in any environment this
        // test suite runs in; this asserts only that *some* value comes
        // back, never a specific one.
        let result = super::read_required("PATH");
        assert!(
            result.is_ok(),
            "PATH is expected to be set in the test environment"
        );
    }
}
