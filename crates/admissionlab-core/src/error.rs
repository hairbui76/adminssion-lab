//! Core error types shared across `admissionlab-core`.
//!
//! This module currently holds the failure mode for parsing the crate's
//! identifier types ([`crate::RunId`] and [`crate::FixtureId`]). Modules
//! added by later tasks (external process execution, artifact storage, and
//! so on) define their own error types alongside the code that produces
//! them rather than growing this module into an unrelated grab bag.

use thiserror::Error;

/// A [`crate::RunId`] or [`crate::FixtureId`] failed to parse.
///
/// Returned by `RunId::parse` and `FixtureId::parse` when the input is not
/// a syntactically valid identifier: empty, or containing any character
/// outside ASCII lowercase letters, digits, and `-`. Because only that
/// fixed character set is ever accepted, this rejects path separators
/// (`/`, `\`), parent-directory segments (`..`, since `.` alone is never a
/// valid character), and whitespace as a side effect of the allow-list
/// rather than as special-cased patterns. That is what keeps every
/// successfully parsed identifier safe to use directly as a filesystem
/// path segment or `kind` cluster name suffix.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdParseError {
    /// The identifier was an empty string.
    #[error("id must not be empty")]
    Empty,
    /// The identifier contained a character outside `[a-z0-9-]`.
    #[error(
        "id {value:?} contains invalid character {invalid:?}; only ASCII \
         lowercase letters, digits, and '-' are allowed"
    )]
    InvalidCharacter {
        /// The full input string that failed to parse.
        value: String,
        /// The first disallowed character encountered, in byte order.
        invalid: char,
    },
}
