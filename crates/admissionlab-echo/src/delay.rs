//! The optional response delay: how long this backend waits before
//! answering an echo request, and where that number comes from.
//!
//! Task 6.5 Step 3 asks for "an optional response delay endpoint/config
//! for later timeout tests, default 0ms". Those later tests need a
//! Gateway's own request timeout to fire, deterministically, without
//! depending on a real slow network -- so a backend that can be told
//! "take 2 seconds" is the whole mechanism.
//!
//! # Two inputs, one rule
//!
//! | Input | Set by | Scope |
//! | --- | --- | --- |
//! | [`crate::config::DELAY_MS_ENV`] | the Deployment's `env:` | every request this pod answers |
//! | [`DELAY_HEADER`] | the probe's own request headers | that one request |
//!
//! The header wins whenever it is present; otherwise the environment
//! default applies. Both are whole milliseconds, both are capped at
//! [`MAX_DELAY_MS`], and neither is ever silently ignored.
//!
//! Both exist because they answer different questions. The environment
//! variable is the honest model of "this backend is slow" -- it is a
//! property of the deployed workload, it survives a Gateway that
//! rewrites or strips request headers, and it is what a fixture
//! declares in YAML. The header is the model of "this *request* should
//! be slow", and it is what makes a single deployed backend able to
//! serve a fast probe and a timeout probe in the same run, with no
//! second Deployment and no rollout in between -- which matters because
//! a rollout mid-run would change which pod answers and muddy exactly
//! the backend-identity signal Phase 6 reads. A per-request *path*
//! (`/delay/2000`) was the alternative considered and rejected: the
//! path is routing input, so encoding delay in it would make the delay
//! and the route match impossible to vary independently, and Task 6.5's
//! own frozen response body echoes the path back as evidence of what
//! the Gateway matched.
//!
//! # Everything unusable is a 400, never a silent zero
//!
//! A [`DELAY_HEADER`] that is not a whole number of milliseconds, or
//! that exceeds [`MAX_DELAY_MS`], is answered `400 Bad Request` with a
//! plain-text explanation ([`crate::serve`]), not treated as "no
//! delay" and not clamped. A timeout fixture whose delay quietly became
//! zero would pass while asserting nothing at all, and it would pass in
//! the direction that reads as "no regression" -- the same failure
//! shape `admissionlab-test-webhook`'s own `behavior` module refuses
//! for an unusable annotation.
//!
//! # The cap
//!
//! [`MAX_DELAY_MS`] is one minute: long enough to exceed any Gateway
//! request timeout a fixture would plausibly assert against (Istio's
//! and NGINX Gateway Fabric's defaults are both far below it), short
//! enough that a typo'd `x-admissionlab-delay-ms: 60000000` cannot hang
//! a probe -- or a whole suite -- for a day. It matches
//! `admissionlab-test-webhook`'s own `behavior::MAX_DELAY_MS` on
//! purpose: one number for "the longest an Admission Lab test component
//! will ever stall on request", so the two halves of the project cannot
//! drift into different notions of "too long".
//!
//! # Why `tokio::time::sleep`
//!
//! [`apply`] sleeps on Tokio's timer rather than blocking the thread,
//! so a caller using a paused clock (`tokio::time::pause`, or
//! `#[tokio::test(start_paused = true)]`) observes the full delay with
//! no wall-clock cost -- which is how this module's own tests, and
//! [`crate::serve`]'s, cover a full-minute delay without a
//! minute-long test suite.

use std::time::Duration;

use hyper::HeaderMap;

use crate::config::{ConfigError, EchoConfig};

/// The per-request delay override. Lowercase because HTTP/1.1 header
/// names are case-insensitive and `hyper` normalizes them to lowercase
/// on the way in; this constant is compared against an already
/// normalized name.
///
/// Prefixed `x-admissionlab-` rather than reusing
/// `admissionlab-test-webhook`'s `test.admissionlab.io/*` annotation
/// vocabulary: that vocabulary lives on Kubernetes objects, this one
/// lives on the HTTP wire, and the two are read by different components
/// at different times.
pub const DELAY_HEADER: &str = "x-admissionlab-delay-ms";

/// The longest delay this backend accepts, in milliseconds -- see this
/// module's own documentation for how the number was chosen.
pub const MAX_DELAY_MS: u64 = 60_000;

/// A [`DELAY_HEADER`] value that cannot be used.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DelayError {
    /// The value is not a whole, non-negative number of milliseconds.
    #[error("{DELAY_HEADER} is not a whole number of milliseconds: {value:?}")]
    NotMilliseconds {
        /// What the request asked for.
        value: String,
    },
    /// The value parsed but exceeds [`MAX_DELAY_MS`].
    #[error("{DELAY_HEADER} exceeds the maximum delay of {MAX_DELAY_MS}ms: {value}")]
    TooLarge {
        /// The requested delay, in milliseconds.
        value: u64,
    },
    /// The value is not valid ASCII/UTF-8, so there is nothing to
    /// parse. Distinguished from [`DelayError::NotMilliseconds`]
    /// because the two have different causes: a malformed number is a
    /// fixture typo, non-text bytes are a client or proxy corrupting
    /// the header.
    #[error("{DELAY_HEADER} is not valid text")]
    NotText,
}

impl DelayError {
    /// Re-reports a parse failure as the equivalent
    /// [`ConfigError`] for `name`, so
    /// [`crate::config`] and this module cannot disagree about what a
    /// well-formed millisecond value is. `raw` is the value as written,
    /// for the error message.
    #[must_use]
    pub fn into_config_error(self, name: &'static str, raw: &str) -> ConfigError {
        match self {
            Self::TooLarge { value } => ConfigError::DelayTooLarge { name, value },
            Self::NotMilliseconds { .. } | Self::NotText => ConfigError::NotMilliseconds {
                name,
                value: raw.to_owned(),
            },
        }
    }
}

/// Parses a whole, non-negative, in-range number of milliseconds.
///
/// The single definition of "a usable delay value", shared by the
/// environment variable and the per-request header so the two can never
/// accept different things.
///
/// # Errors
///
/// Returns [`DelayError::NotMilliseconds`] if `raw` is not a base-10
/// unsigned integer (which is also what rejects `-1`, `1.5` and
/// `250ms`), or [`DelayError::TooLarge`] if it exceeds
/// [`MAX_DELAY_MS`].
pub fn parse_millis(raw: &str) -> Result<u64, DelayError> {
    let millis = raw
        .parse::<u64>()
        .map_err(|_| DelayError::NotMilliseconds {
            value: raw.to_owned(),
        })?;
    if millis > MAX_DELAY_MS {
        return Err(DelayError::TooLarge { value: millis });
    }
    Ok(millis)
}

/// Decides how long this request waits: the [`DELAY_HEADER`] if the
/// request carries one, otherwise `config`'s environment default.
///
/// # Errors
///
/// Returns [`DelayError`] if the request carries a [`DELAY_HEADER`]
/// that is not usable -- see this module's own documentation for why
/// that is an error rather than a fallback to the default.
pub fn resolve(headers: &HeaderMap, config: &EchoConfig) -> Result<Duration, DelayError> {
    let Some(value) = headers.get(DELAY_HEADER) else {
        return Ok(config.default_delay);
    };
    let text = value.to_str().map_err(|_| DelayError::NotText)?;
    parse_millis(text.trim()).map(Duration::from_millis)
}

/// Waits `delay`, unless it is zero.
///
/// The zero case skips the sleep entirely rather than awaiting a
/// zero-length timer, so the overwhelmingly common "no delay
/// configured" request is not made to yield to the runtime at all --
/// and so a test can assert that an unconfigured backend really does no
/// waiting rather than a very short one.
pub async fn apply(delay: Duration) {
    if delay.is_zero() {
        return;
    }
    tracing::debug!(?delay, "delaying this echo response");
    tokio::time::sleep(delay).await;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hyper::HeaderMap;
    use hyper::header::{HeaderName, HeaderValue};
    use tokio::time::Instant;

    use super::{DELAY_HEADER, DelayError, MAX_DELAY_MS, apply, parse_millis, resolve};
    use crate::config::EchoConfig;

    fn config(default_delay_ms: u64) -> EchoConfig {
        EchoConfig {
            backend_id: "echo-a".to_owned(),
            default_delay: Duration::from_millis(default_delay_ms),
        }
    }

    fn headers(pairs: &[(&str, &[u8])]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).expect("test header names are well-formed"),
                HeaderValue::from_bytes(value).expect("test header values are well-formed"),
            );
        }
        map
    }

    #[test]
    fn no_header_uses_the_environment_default() {
        let delay = resolve(&HeaderMap::new(), &config(250)).expect("no header is not an error");
        assert_eq!(delay, Duration::from_millis(250));
    }

    #[test]
    fn the_header_overrides_the_environment_default() {
        let delay = resolve(&headers(&[(DELAY_HEADER, b"10")]), &config(250))
            .expect("a well-formed header must be accepted");
        assert_eq!(
            delay,
            Duration::from_millis(10),
            "the per-request value wins, including when it is shorter"
        );
    }

    /// A request may ask for *no* delay on a backend deployed slow --
    /// the header wins in both directions, or it would not be an
    /// override at all.
    #[test]
    fn the_header_can_ask_for_no_delay_at_all() {
        let delay = resolve(&headers(&[(DELAY_HEADER, b"0")]), &config(250))
            .expect("zero is a well-formed delay");
        assert_eq!(delay, Duration::ZERO);
    }

    #[test]
    fn a_malformed_header_is_an_error_not_the_default() {
        let error = resolve(&headers(&[(DELAY_HEADER, b"soon")]), &config(250))
            .expect_err("an unusable delay header must be refused");
        assert_eq!(
            error,
            DelayError::NotMilliseconds {
                value: "soon".to_owned()
            }
        );
    }

    #[test]
    fn a_header_beyond_the_cap_is_refused_not_clamped() {
        let over = MAX_DELAY_MS + 1;
        let error = resolve(
            &headers(&[(DELAY_HEADER, over.to_string().as_bytes())]),
            &config(0),
        )
        .expect_err("a delay beyond the cap must be refused");
        assert_eq!(error, DelayError::TooLarge { value: over });
    }

    #[test]
    fn a_non_text_header_is_distinguished_from_a_malformed_number() {
        let error = resolve(&headers(&[(DELAY_HEADER, &[0xff, 0xfe])]), &config(0))
            .expect_err("a non-text delay header must be refused");
        assert_eq!(error, DelayError::NotText);
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        let delay = resolve(&headers(&[(DELAY_HEADER, b"  25  ")]), &config(0))
            .expect("a padded value is still a number");
        assert_eq!(delay, Duration::from_millis(25));
    }

    #[test]
    fn a_negative_delay_is_not_a_number() {
        assert_eq!(
            parse_millis("-1"),
            Err(DelayError::NotMilliseconds {
                value: "-1".to_owned()
            })
        );
    }

    #[test]
    fn the_cap_itself_is_legal() {
        assert_eq!(parse_millis(&MAX_DELAY_MS.to_string()), Ok(MAX_DELAY_MS));
    }

    /// Tokio's paused clock: the sleep is observed in full (the elapsed
    /// `tokio::time::Instant` really does advance by the requested
    /// delay) while the test itself costs no wall-clock time, so this
    /// covers the largest delay this backend accepts without a
    /// minute-long test.
    #[tokio::test(start_paused = true)]
    async fn the_longest_legal_delay_is_actually_waited() {
        let started = Instant::now();
        apply(Duration::from_millis(MAX_DELAY_MS)).await;
        assert_eq!(started.elapsed(), Duration::from_millis(MAX_DELAY_MS));
    }

    #[tokio::test(start_paused = true)]
    async fn a_zero_delay_waits_for_nothing() {
        let started = Instant::now();
        apply(Duration::ZERO).await;
        assert_eq!(
            started.elapsed(),
            Duration::ZERO,
            "an absent delay must not become a zero-length sleep that still yields"
        );
    }
}
