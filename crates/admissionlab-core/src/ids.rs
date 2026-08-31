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
//!
//! # Serialization (Task 5.1)
//!
//! Both types serialize as a **plain JSON string** and deserialize back
//! through their own [`RunId::parse`]/[`FixtureId::parse`] validation, via
//! `#[serde(into = "String", try_from = "String")]`. Two consequences are
//! deliberate:
//!
//! - A deserialized identifier is exactly as trustworthy as a parsed one.
//!   `#[serde(transparent)]` would have been shorter and would have
//!   produced the same JSON, but it would also have let a hand-edited
//!   `run.json` reintroduce `..` or a path separator into a value this
//!   crate's documentation promises is always safe to embed in a
//!   filesystem path.
//! - Serializing to a string (rather than to a one-field object) is what
//!   makes [`FixtureId`] usable as a JSON **object key**, which
//!   `RunManifest::fixture_hashes` (a `BTreeMap<FixtureId, String>`)
//!   depends on: a JSON object key can only ever be a string.
//!
//! No `JsonSchema` derive here. The run manifest's schema describes both
//! fields as plain strings at their use sites (`#[schemars(with =
//! "String")]` in [`crate::run_manifest`]) rather than pulling `schemars`
//! into this module for two newtypes, which keeps the identifier
//! vocabulary itself free of a schema-generation dependency.

use std::fmt;

use serde::{Deserialize, Serialize};
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
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

/// Deserialization's validating entry point (see this module's
/// "Serialization" section). Written as [`TryFrom`] rather than a
/// hand-rolled `Deserialize` so the check is a plain, testable
/// conversion that a non-serde caller can use too.
impl TryFrom<String> for RunId {
    type Error = IdParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_id(&value)?;
        Ok(Self(value))
    }
}

/// Serialization's exit point. Consuming rather than borrowing so the
/// already-owned `String` is moved out instead of copied again.
impl From<RunId> for String {
    fn from(id: RunId) -> Self {
        id.0
    }
}

/// Identifier for one fixture replayed through both clusters.
///
/// Follows the same syntax rules as [`RunId`] but is a distinct type: a
/// fixture identifier and a run identifier are never interchangeable even
/// though both are validated the same way. Unlike `RunId`, `FixtureId` has
/// no random generator here — a later task computes fixture identity
/// deterministically and constructs it through [`FixtureId::parse`].
///
/// [`Ord`] is derived (unlike on [`RunId`], which has no ordering) so a
/// `FixtureId` can key a [`std::collections::BTreeMap`] — which
/// `RunManifest::fixture_hashes` is. That ordering is plain lexicographic
/// order over the identifier's bytes, so a manifest's fixture hashes are
/// written in the same order on every machine regardless of the order
/// discovery happened to produce them in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
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

/// See [`TryFrom<String> for RunId`](RunId#impl-TryFrom<String>-for-RunId).
impl TryFrom<String> for FixtureId {
    type Error = IdParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_id(&value)?;
        Ok(Self(value))
    }
}

/// See [`From<RunId> for String`](RunId#impl-From<RunId>-for-String).
impl From<FixtureId> for String {
    fn from(id: FixtureId) -> Self {
        id.0
    }
}
