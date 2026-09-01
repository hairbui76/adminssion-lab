//! Environment-variable configuration this binary reads. Deliberately
//! two variables and nothing else: everything else that could
//! plausibly be "configuration" (the listen port, the health path, the
//! header names) is a fixed implementation constant in [`crate::serve`]
//! and [`crate::delay`], because nothing in this deployment ever needs
//! a *different* value for any of them and making them environment
//! variables would only add a way for a fixture manifest and this
//! binary to silently disagree -- the same reasoning
//! `admissionlab-test-webhook`'s own `config` module records.
//!
//! # Why the backend id is required and never defaulted
//!
//! [`BACKEND_ID_ENV`] has no default, and a missing or empty value is a
//! fatal startup error rather than something this process papers over.
//! The whole point of this component is that two backends have two
//! *distinguishable* identities: Task 6.9's comparator reports
//! `traffic_backend_changed` when the same probe reaches `echo-a` on
//! the baseline and `echo-b` on the candidate. A defaulted id (say
//! `"echo"`, or the pod's hostname, or an empty string) would let both
//! Deployments claim the same identity, at which point that comparison
//! silently succeeds for every routing change -- hiding exactly the
//! regression this backend exists to catch, and hiding it in the
//! direction that reads as "no change". Global Constraint 15
//! ("unavailable data is unknown, never fabricated") says the same
//! thing more generally: a manifest that forgot to set the id must
//! produce a `CrashLoopBackOff` an operator can see, not a green run.
//!
//! # Why the delay default is a variable at all
//!
//! [`DELAY_MS_ENV`] *is* optional (absent means no delay), because a
//! zero delay is the honest, non-fabricated meaning of "this backend
//! was not asked to be slow" -- there is no identity to confuse and no
//! comparison to corrupt. See [`crate::delay`] for the delay design as
//! a whole, including the per-request override that makes one deployed
//! backend able to serve both fast and slow probes.
//!
//! # Reading the environment without mutating it
//!
//! [`EchoConfig::from_env`] reads through [`std::env::var`], but does so
//! via [`EchoConfig::from_lookup`], which takes the lookup as a
//! parameter: this crate forbids `unsafe_code` and
//! `std::env::set_var`/`remove_var` are `unsafe fn` as of this edition,
//! so a test cannot mutate the real process environment to exercise the
//! missing/empty/malformed paths. The tests below supply a fake lookup
//! closure instead -- no real environment mutation, and no cross-test
//! interference to serialize against. (`crates/admissionlab-echo/tests/http.rs`
//! covers the *process-level* consequence of a missing variable by
//! running the real binary with `assert_cmd`, which is the one thing a
//! fake lookup cannot prove.)

use std::env::VarError;
use std::time::Duration;

use crate::delay::MAX_DELAY_MS;

/// This backend's identity, echoed verbatim as the frozen response
/// body's `backend` field ([`crate::echo::EchoBody::backend`]) and
/// matched by `fixtures/gateway/backends/echo-a.yaml` /
/// `echo-b.yaml`'s own `env:` entries. Required -- see this module's
/// own documentation for why it is never defaulted.
pub const BACKEND_ID_ENV: &str = "ADMISSIONLAB_BACKEND_ID";

/// The delay, in whole milliseconds, applied to every echoed response
/// unless a request overrides it ([`crate::delay`]). Optional; absent
/// means no delay at all.
pub const DELAY_MS_ENV: &str = "ADMISSIONLAB_ECHO_DELAY_MS";

/// Something went wrong reading this binary's configuration. Every
/// variant is fatal at startup: this process serves nothing until it
/// knows who it is.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// A required variable was not set at all.
    #[error("required environment variable {name} is not set")]
    Missing {
        /// The variable that was not set.
        name: &'static str,
    },
    /// A required variable was set but is empty (or all whitespace).
    #[error("required environment variable {name} is set but empty")]
    Empty {
        /// The variable that was empty.
        name: &'static str,
    },
    /// A variable was set but is not valid Unicode.
    #[error("environment variable {name} is not valid Unicode")]
    NotUnicode {
        /// The variable that was not valid Unicode.
        name: &'static str,
    },
    /// [`DELAY_MS_ENV`] was set to something that is not a whole,
    /// non-negative number of milliseconds. Rejected rather than
    /// treated as zero: a typo'd delay that silently became "no delay"
    /// would make a timeout fixture quietly assert nothing.
    #[error("environment variable {name} is not a whole number of milliseconds: {value:?}")]
    NotMilliseconds {
        /// The variable that could not be parsed.
        name: &'static str,
        /// What it was set to.
        value: String,
    },
    /// [`DELAY_MS_ENV`] parsed but exceeds [`MAX_DELAY_MS`]. Rejected
    /// rather than clamped, for the same reason
    /// [`ConfigError::NotMilliseconds`] is: a clamp turns a manifest
    /// typo into a silently different test.
    #[error("environment variable {name} exceeds the maximum delay of {MAX_DELAY_MS}ms: {value}")]
    DelayTooLarge {
        /// The variable that was out of range.
        name: &'static str,
        /// The requested delay, in milliseconds.
        value: u64,
    },
}

/// Everything this process needs to know to answer a request.
///
/// Cloned into every connection task through an [`std::sync::Arc`] (see
/// [`crate::serve::serve_on`]), never re-read from the environment
/// per request: a backend whose identity could change mid-run would
/// make a data-plane comparison meaningless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoConfig {
    /// This backend's identity, trimmed. See [`BACKEND_ID_ENV`].
    pub backend_id: String,
    /// The delay applied to an echoed response when the request does
    /// not ask for a different one. [`Duration::ZERO`] when
    /// [`DELAY_MS_ENV`] is unset -- see [`crate::delay::resolve`].
    pub default_delay: Duration,
}

impl EchoConfig {
    /// Reads this process's configuration from the real environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] if [`BACKEND_ID_ENV`] is unset, empty or
    /// not valid Unicode, or if [`DELAY_MS_ENV`] is set to something
    /// that is not a whole number of milliseconds within
    /// [`MAX_DELAY_MS`].
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key))
    }

    /// [`EchoConfig::from_env`]'s implementation, generic over the
    /// environment lookup so it can be exercised without mutating the
    /// real process environment -- see this module's own documentation.
    ///
    /// # Errors
    ///
    /// As [`EchoConfig::from_env`].
    pub fn from_lookup(
        mut lookup: impl FnMut(&str) -> Result<String, VarError>,
    ) -> Result<Self, ConfigError> {
        let backend_id = read_required(BACKEND_ID_ENV, &mut lookup)?;
        let default_delay = read_delay(DELAY_MS_ENV, &mut lookup)?;
        Ok(Self {
            backend_id,
            default_delay,
        })
    }
}

/// Reads `name`, trimmed, refusing both "unset" and "set to nothing".
fn read_required(
    name: &'static str,
    lookup: &mut impl FnMut(&str) -> Result<String, VarError>,
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

/// Reads an optional whole-millisecond duration. Unset means
/// [`Duration::ZERO`]; set-but-unusable is an error, never a silent
/// zero -- see [`ConfigError::NotMilliseconds`].
fn read_delay(
    name: &'static str,
    lookup: &mut impl FnMut(&str) -> Result<String, VarError>,
) -> Result<Duration, ConfigError> {
    let raw = match lookup(name) {
        Ok(value) => value,
        Err(VarError::NotPresent) => return Ok(Duration::ZERO),
        Err(VarError::NotUnicode(_)) => return Err(ConfigError::NotUnicode { name }),
    };
    let trimmed = raw.trim();
    // An explicitly empty value is the shape a manifest produces when
    // it writes `value: ""`, which reads as "no delay" to anyone
    // looking at the YAML. Honouring that reading is the one place a
    // missing value is not an error -- and it is unambiguous, unlike an
    // empty backend id.
    if trimmed.is_empty() {
        return Ok(Duration::ZERO);
    }
    crate::delay::parse_millis(trimmed)
        .map_err(|error| error.into_config_error(name, trimmed))
        .map(Duration::from_millis)
}

#[cfg(test)]
mod tests {
    use std::env::VarError;
    use std::time::Duration;

    use super::{BACKEND_ID_ENV, ConfigError, DELAY_MS_ENV, EchoConfig};
    use crate::delay::MAX_DELAY_MS;

    /// A lookup that answers from an explicit table, so every test says
    /// exactly which variables exist.
    fn lookup(pairs: &[(&str, &str)]) -> impl FnMut(&str) -> Result<String, VarError> {
        move |key: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
                .ok_or(VarError::NotPresent)
        }
    }

    #[test]
    fn a_backend_id_is_required() {
        let error = EchoConfig::from_lookup(lookup(&[]))
            .expect_err("a backend with no id must refuse to start");
        assert_eq!(
            error,
            ConfigError::Missing {
                name: BACKEND_ID_ENV
            }
        );
    }

    #[test]
    fn an_empty_backend_id_is_rejected_not_defaulted() {
        let error = EchoConfig::from_lookup(lookup(&[(BACKEND_ID_ENV, "   ")]))
            .expect_err("an empty backend id must refuse to start");
        assert_eq!(
            error,
            ConfigError::Empty {
                name: BACKEND_ID_ENV
            }
        );
    }

    #[test]
    fn a_backend_id_is_trimmed() {
        let config = EchoConfig::from_lookup(lookup(&[(BACKEND_ID_ENV, "  echo-a  ")]))
            .expect("a set, non-empty backend id must be accepted");
        assert_eq!(config.backend_id, "echo-a");
        assert_eq!(
            config.default_delay,
            Duration::ZERO,
            "an unset delay is no delay"
        );
    }

    #[test]
    fn a_delay_is_read_in_whole_milliseconds() {
        let config =
            EchoConfig::from_lookup(lookup(&[(BACKEND_ID_ENV, "echo-a"), (DELAY_MS_ENV, "250")]))
                .expect("a well-formed delay must be accepted");
        assert_eq!(config.default_delay, Duration::from_millis(250));
    }

    /// `value: ""` in a manifest reads as "no delay" -- see
    /// [`super::read_delay`]'s own comment for why this is the one
    /// place an empty value is not an error.
    #[test]
    fn an_empty_delay_means_no_delay() {
        let config =
            EchoConfig::from_lookup(lookup(&[(BACKEND_ID_ENV, "echo-a"), (DELAY_MS_ENV, "  ")]))
                .expect("an empty delay must be accepted as no delay");
        assert_eq!(config.default_delay, Duration::ZERO);
    }

    #[test]
    fn a_malformed_delay_is_rejected_not_treated_as_zero() {
        let error = EchoConfig::from_lookup(lookup(&[
            (BACKEND_ID_ENV, "echo-a"),
            (DELAY_MS_ENV, "250ms"),
        ]))
        .expect_err("a delay that is not a number must refuse to start");
        assert_eq!(
            error,
            ConfigError::NotMilliseconds {
                name: DELAY_MS_ENV,
                value: "250ms".to_owned()
            }
        );
    }

    #[test]
    fn a_negative_delay_is_rejected() {
        let error =
            EchoConfig::from_lookup(lookup(&[(BACKEND_ID_ENV, "echo-a"), (DELAY_MS_ENV, "-1")]))
                .expect_err("a negative delay must refuse to start");
        assert_eq!(
            error,
            ConfigError::NotMilliseconds {
                name: DELAY_MS_ENV,
                value: "-1".to_owned()
            }
        );
    }

    #[test]
    fn a_delay_beyond_the_cap_is_rejected_not_clamped() {
        let over = MAX_DELAY_MS + 1;
        let error = EchoConfig::from_lookup(lookup(&[
            (BACKEND_ID_ENV, "echo-a"),
            (DELAY_MS_ENV, &over.to_string()),
        ]))
        .expect_err("a delay beyond the cap must refuse to start");
        assert_eq!(
            error,
            ConfigError::DelayTooLarge {
                name: DELAY_MS_ENV,
                value: over
            }
        );
    }

    #[test]
    fn a_delay_exactly_at_the_cap_is_accepted() {
        let config = EchoConfig::from_lookup(lookup(&[
            (BACKEND_ID_ENV, "echo-a"),
            (DELAY_MS_ENV, &MAX_DELAY_MS.to_string()),
        ]))
        .expect("the cap itself is a legal delay");
        assert_eq!(config.default_delay, Duration::from_millis(MAX_DELAY_MS));
    }

    #[test]
    fn a_non_unicode_backend_id_is_reported_distinctly() {
        let error = EchoConfig::from_lookup(|_| Err(VarError::NotUnicode("\u{fffd}".into())))
            .expect_err("a non-Unicode backend id must refuse to start");
        assert_eq!(
            error,
            ConfigError::NotUnicode {
                name: BACKEND_ID_ENV
            }
        );
    }

    // There is deliberately no unit test here for
    // `EchoConfig::from_env` reading the *real* environment: this
    // process cannot set or unset an environment variable without
    // `unsafe`, and a test that merely accepts "either the variable is
    // missing or it is not" would assert nothing. The wiring between
    // `from_env` and the process environment is proved instead by
    // `crates/admissionlab-echo/tests/http.rs`'s `assert_cmd` tests,
    // which run the real binary with the variable removed and with it
    // set to whitespace, and require a non-zero exit both times.
}
