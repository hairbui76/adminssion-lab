//! Why a policy section (or, from Task 4.9, an expectations file) was
//! rejected.
//!
//! Every message here starts with a dotted locator into the document it
//! came from (`policy.overrides[1].kind: ...`), the same convention
//! `admissionlab_spec::SpecError::Validation` follows and
//! `serde_norway`'s own parse errors already produce -- so a policy
//! rejection and a configuration-parse rejection read the same way to
//! the person fixing the file, even though they are produced by two
//! different crates.

use std::fmt;

use thiserror::Error;

/// One rejected value in a `policy` section.
///
/// Deliberately *not* an enum over the individual rules (unknown kind,
/// unknown severity, bad glob, empty selector field): every one of them
/// is a "this string cannot mean anything" rejection, callers act on all
/// of them identically (print it, refuse to start), and the useful
/// distinguishing information is the locator, which is data rather than
/// a variant. `admissionlab_spec::SpecError::Validation` collapses its
/// own dozen rules the same way and for the same reason.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{locator}: {message}")]
pub struct PolicyValidationError {
    /// A dotted locator into the configuration document, for example
    /// `policy.overrides[1].kind`.
    pub locator: String,
    /// What is wrong with the value at `locator`, including the valid
    /// alternatives where there is a closed set of them.
    pub message: String,
}

impl PolicyValidationError {
    /// Builds a validation error at `locator`.
    pub(crate) fn new(locator: impl fmt::Display, message: impl fmt::Display) -> Self {
        Self {
            locator: locator.to_string(),
            message: message.to_string(),
        }
    }
}

/// Every problem found in one `policy` section, never just the first.
///
/// [`crate::validate_policy_spec`] reports all of them at once on
/// purpose: a user who typo'd three regression-kind names should learn
/// about three typos from one run, not discover them one failed startup
/// at a time. Guaranteed non-empty -- [`crate::resolve_policy`] returns
/// `Ok` rather than an empty error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid policy: {}", render_all(&self.0))]
pub struct PolicySpecErrors(pub Vec<PolicyValidationError>);

impl PolicySpecErrors {
    /// The individual problems, in document order.
    #[must_use]
    pub fn as_slice(&self) -> &[PolicyValidationError] {
        &self.0
    }

    /// Consumes this error, yielding the individual problems.
    #[must_use]
    pub fn into_vec(self) -> Vec<PolicyValidationError> {
        self.0
    }
}

/// Renders every problem on one line, separated by `; `.
///
/// A single line rather than a bulleted block because this is a
/// [`std::error::Error`] `Display` implementation: the caller decides
/// how to present it (the CLI may well print one per line), and an
/// error whose own `Display` spans lines composes badly with every
/// wrapper that prints `{err}` inline.
fn render_all(errors: &[PolicyValidationError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}
