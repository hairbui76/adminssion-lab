//! Core run and fixture identifiers.
//!
//! [`RunId`] and [`FixtureId`] share identical syntax rules — enforced by
//! one shared validation routine — but are kept as distinct Rust types so
//! they can never be accidentally interchanged: a run identifier names one
//! baseline/candidate comparison, a fixture identifier names one input
//! replayed through both clusters. `RunId` additionally supports random
//! generation, because a run has no natural deterministic identity beyond
//! "this particular invocation." `FixtureId` intentionally does not: a
//! later task derives fixture identity deterministically from the
//! fixture's normalized path, document index, and object identity, so it
//! must stay stable across machines rather than being randomly assigned.

use std::fmt;

use uuid::Uuid;

use crate::error::IdParseError;

/// Validates that `value` is a syntactically valid core identifier.
///
/// Shared by [`RunId::parse`] and [`FixtureId::parse`] so both types
/// enforce identical rules: non-empty, and only ASCII lowercase letters,
/// digits, and `-`. Restricting parsing to this fixed allow-list — rather
/// than enumerating forbidden patterns — is what keeps a successfully
/// parsed identifier safe to embed directly in filesystem paths and
/// ephemeral cluster name suffixes.
fn validate_id(value: &str) -> Result<(), IdParseError> {
    if value.is_empty() {
        return Err(IdParseError::Empty);
    }
    if let Some(invalid) = value
        .chars()
        .find(|&c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
    {
        return Err(IdParseError::InvalidCharacter {
            value: value.to_owned(),
            invalid,
        });
    }
    Ok(())
}

/// Identifier for one baseline/candidate comparison run.
///
/// Generated as a lowercase random UUID by default (see
/// [`RunId::generate`]). Every successfully parsed `RunId` is safe to use
/// directly as a filesystem path segment or `kind` cluster name suffix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunId(String);

impl RunId {
    /// Generates a new random run identifier.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Parses `value` as a run identifier.
    ///
    /// # Errors
    ///
    /// Returns [`IdParseError`] if `value` is empty or contains any
    /// character other than an ASCII lowercase letter, digit, or `-`.
    pub fn parse(value: &str) -> Result<Self, IdParseError> {
        validate_id(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Returns this identifier as a plain string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier for one fixture replayed through both clusters.
///
/// Follows the same syntax rules as [`RunId`] but is a distinct type: a
/// fixture identifier and a run identifier are never interchangeable even
/// though both are validated the same way. Unlike `RunId`, `FixtureId` has
/// no random generator here — a later task computes fixture identity
/// deterministically and constructs it through [`FixtureId::parse`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FixtureId(String);

impl FixtureId {
    /// Parses `value` as a fixture identifier.
    ///
    /// # Errors
    ///
    /// Returns [`IdParseError`] if `value` is empty or contains any
    /// character other than an ASCII lowercase letter, digit, or `-`.
    pub fn parse(value: &str) -> Result<Self, IdParseError> {
        validate_id(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Returns this identifier as a plain string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FixtureId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
