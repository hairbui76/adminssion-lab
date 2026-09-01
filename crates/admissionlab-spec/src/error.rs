//! Failure modes for loading, parsing, migrating, resolving, and
//! validating a lab configuration, in any supported `apiVersion`.
//!
//! Every variant that originates from a specific file carries that file's
//! path, so a message never leaves the reader guessing which
//! `admissionlab.yaml` is at fault — important once fixtures or CI wire
//! multiple configurations together. [`SpecError::Parse`] additionally
//! carries `serde_norway`'s own error, which — for a value nested inside
//! the document — already includes the dotted path to the offending
//! field (for example: `baseline: unknown field "kuberentes", expected
//! "kubernetes" or "components"`) alongside the line and column; see that
//! variant's documentation.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Something went wrong loading, parsing, migrating, resolving, or
/// validating a lab configuration.
///
/// A [`crate::MigrationError`] — the one failure
/// [`crate::migrate_v1alpha1_to_v1beta1`] can produce — arrives here as
/// [`SpecError::Validation`] rather than as a variant of its own,
/// because that is exactly what it is: a document whose `apiVersion` or
/// `kind` does not match the model being applied to it, reported with
/// the same dotted locator every other document-level rejection uses.
#[derive(Debug, Error)]
pub enum SpecError {
    /// The configuration file could not be read.
    #[error("failed to read lab configuration at {}: {source}", path.display())]
    Io {
        /// The path [`crate::load_lab`] was asked to read.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
    },

    /// The configuration file's contents are not a valid lab document
    /// in the version it declares.
    ///
    /// `source`'s own [`Display`](std::fmt::Display) implementation
    /// already includes the line, column, and — for a value nested inside
    /// the document — a dotted path to the offending field (for example:
    /// `baseline: unknown field "kuberentes", expected "kubernetes" or
    /// "components" at line 4 column 3`), so this variant does not
    /// duplicate that information; it only adds *which file* failed.
    #[error("failed to parse lab configuration at {}: {source}", path.display())]
    Parse {
        /// The path [`crate::load_lab`] was asked to read.
        path: PathBuf,
        /// The underlying parse failure, including YAML location and
        /// (where applicable) the serde path to the offending field.
        #[source]
        source: serde_norway::Error,
    },

    /// A fixture include pattern is not a syntactically valid glob.
    #[error(
        "{}: fixtures.include: invalid glob pattern {pattern:?}: {source}",
        path.display()
    )]
    InvalidGlob {
        /// The configuration file the invalid pattern came from.
        path: PathBuf,
        /// The pattern, exactly as written in `fixtures.include`.
        pattern: String,
        /// Why `globset` rejected the pattern.
        #[source]
        source: globset::Error,
    },

    /// The configuration parsed successfully but fails a semantic
    /// validation rule (for example an empty Kubernetes version, a
    /// duplicate component name, or an empty fixture include list).
    ///
    /// `message` is prefixed with a dotted locator (for example
    /// `baseline.kubernetes: ...`) for the same reason
    /// [`SpecError::Parse`]'s underlying error is: naming exactly which
    /// part of the document is at fault.
    #[error("{}: {message}", path.display())]
    Validation {
        /// The configuration file that failed validation.
        path: PathBuf,
        /// A message starting with a dotted locator into the document,
        /// for example `baseline.kubernetes: must not be empty`.
        message: String,
    },
}

impl SpecError {
    /// Builds a [`SpecError::Validation`] whose message is prefixed with
    /// `locator` (for example `"baseline.kubernetes"`), matching the
    /// dotted-path convention [`SpecError::Parse`] gets for free from
    /// `serde_norway`.
    pub(crate) fn validation(
        path: &Path,
        locator: impl std::fmt::Display,
        message: impl std::fmt::Display,
    ) -> Self {
        Self::Validation {
            path: path.to_path_buf(),
            message: format!("{locator}: {message}"),
        }
    }
}
