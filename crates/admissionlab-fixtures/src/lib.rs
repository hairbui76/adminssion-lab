#![forbid(unsafe_code)]
//! Fixture discovery, identity, and hashing for Admission Lab (Phase 3).
//!
//! This crate finds the Kubernetes manifest documents an
//! `admissionlab.yaml` configuration's `fixtures.include` globs select,
//! gives each one a [`admissionlab_core::FixtureId`] that is stable
//! across machines, and hashes its source content for provenance. It
//! does not itself replay a fixture through a cluster or resolve which
//! Kubernetes API resource it targets -- those are later tasks' jobs
//! (Task 3.2 and onward).
//!
//! - [`discover`] implements Task 3.1: [`discover::discover_fixtures`]
//!   walks a [`admissionlab_spec::ResolvedFixtureSelection`]'s root,
//!   matches files against its compiled glob patterns, parses each
//!   match as a (possibly multi-document) YAML stream, and returns one
//!   [`discover::FixtureSource`] per valid document, in a fully
//!   deterministic order. See that module's documentation for the
//!   ordering, validation, and hashing rules in full.
//! - [`identity`] extracts a document's Kubernetes identity
//!   (`apiVersion`/`kind`/`metadata.name`, enforcing Alpha's
//!   deterministic-name requirement) and folds it, together with a
//!   document's path and position, into a [`admissionlab_core::FixtureId`].
//! - [`hash`] computes the SHA-256 content hash [`discover::FixtureSource::sha256`]
//!   carries.
//!
//! # Dependency direction (controller supplement §2, Task 3.1)
//!
//! This crate depends on `admissionlab-spec` (a leaf crate) for
//! [`admissionlab_spec::ResolvedFixtureSelection`], and on
//! `admissionlab-core` for [`admissionlab_core::FixtureId`]. Neither
//! edge can ever become a cycle on its own. What this crate's own code
//! must never do -- enforced by review, not by the compiler -- is
//! introduce a dependency the *other* way, from `admissionlab-core` onto
//! this crate: Phase 3 eventually integrates fixture capture into
//! `admissionlab-core`'s own `run.rs`, and the shape that avoids a cycle
//! there is a trait declared in `core` and implemented elsewhere (the
//! same shape `admissionlab_core::ClusterManager` and
//! `admissionlab_core::StackInstaller` already use), not a new
//! `core -> fixtures` edge.

pub mod discover;
pub mod hash;
mod identity;

pub use discover::{FixtureSource, discover_fixtures};

use std::path::PathBuf;

use thiserror::Error;

/// Failure modes of [`discover::discover_fixtures`].
///
/// Every variant that names a specific fixture document carries both
/// `path` (the file it came from) and `document_index` (its position --
/// zero-based, counting every `---`-separated YAML document in file
/// order, including one later skipped for being empty; see
/// [`discover`]'s module documentation) so a user can find the exact
/// offending document without re-deriving position from a renumbered
/// list.
#[derive(Debug, Error)]
pub enum FixtureError {
    /// Walking the fixture selection's root directory (or a directory
    /// beneath it) failed: `path` could not be listed, or one of its
    /// entries' file type could not be read. This is also what a
    /// missing/inaccessible root itself surfaces as, since the very
    /// first call in the walk is `read_dir` on the root.
    #[error("failed to walk fixture directory {}: {source}", .path.display())]
    WalkDirectory {
        /// The directory (or, for a `read_dir` entry-iteration failure,
        /// its parent) that could not be walked.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// A discovered file's path, relative to the selection's root, is
    /// not valid UTF-8. [`discover::discover_fixtures`] requires this
    /// because a [`admissionlab_core::FixtureId`] and a matched glob
    /// pattern are both computed from that relative path as text; there
    /// is no lossless, deterministic way to fold arbitrary non-UTF-8
    /// bytes into either without risking two different byte sequences
    /// silently mapping to the same result (Global Constraint 15: this
    /// crate does not guess).
    #[error("fixture path {} is not valid UTF-8", .path.display())]
    NonUtf8Path {
        /// The absolute (or selection-root-relative-prefixed) path that
        /// could not be represented as UTF-8.
        path: PathBuf,
    },
    /// A matched fixture file could not be read from disk.
    #[error("failed to read fixture file {}: {source}", .path.display())]
    ReadFile {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// One document inside a matched fixture file is not syntactically
    /// valid YAML (JSON is valid YAML, so a `.json` fixture file is
    /// covered by this same path -- see [`discover`]'s module
    /// documentation).
    #[error(
        "fixture file {} document {}: not valid YAML: {reason}",
        .path.display(), .document_index + 1
    )]
    Parse {
        /// The fixture file containing the malformed document.
        path: PathBuf,
        /// Zero-based position of the malformed document within `path`.
        document_index: usize,
        /// A human-readable explanation from the underlying parser.
        reason: String,
    },
    /// A non-empty document parsed to something other than a JSON/YAML
    /// mapping (a scalar or an array), so it has no `apiVersion`/`kind`/
    /// `metadata` to even look for.
    #[error(
        "fixture file {} document {}: expected a Kubernetes object (a YAML/JSON mapping), \
         found {found}",
        .path.display(), .document_index + 1
    )]
    NotAnObject {
        /// The fixture file containing the malformed document.
        path: PathBuf,
        /// Zero-based position of the malformed document within `path`.
        document_index: usize,
        /// What the document actually parsed to, for the message (for
        /// example `"an array"`).
        found: &'static str,
    },
    /// A document is missing a field Alpha requires for a deterministic
    /// identity (`apiVersion`, `kind`, or `metadata.name`), or has that
    /// field present but not as a non-empty string.
    #[error(
        "fixture file {} document {}: missing required field {field:?}",
        .path.display(), .document_index + 1
    )]
    MissingField {
        /// The fixture file containing the incomplete document.
        path: PathBuf,
        /// Zero-based position of the incomplete document within `path`.
        document_index: usize,
        /// The dotted field path that was missing (for example
        /// `"metadata.name"`).
        field: &'static str,
    },
    /// A document has `metadata.generateName` but no `metadata.name`.
    /// Rejected, not merely unsupported: Kubernetes assigns a
    /// `generateName` object's real name at admission time, which is not
    /// reproducible across runs, and no name-rewrite contract exists yet
    /// to make it deterministic (controller supplement §4, Task 3.1).
    #[error(
        "fixture file {} document {}: `metadata.generateName` is not supported -- Alpha \
         requires every fixture to have a deterministic `metadata.name`, and no name-rewrite \
         contract exists yet to make a generated name reproducible across runs",
        .path.display(), .document_index + 1
    )]
    GenerateNameUnsupported {
        /// The fixture file containing the `generateName`-only document.
        path: PathBuf,
        /// Zero-based position of that document within `path`.
        document_index: usize,
    },
    /// Two distinct fixture documents computed the same
    /// [`admissionlab_core::FixtureId`]. Always reported relative to the
    /// *first* occurrence in discovery order (itself deterministic --
    /// see [`discover`]'s module documentation), so which pair is named
    /// never depends on hash-map iteration order or any other
    /// unspecified ordering.
    #[error(
        "fixture id {id:?} is not unique: produced by both {} document {} and {} document {}",
        .first_path.display(), .first_document_index + 1,
        .second_path.display(), .second_document_index + 1
    )]
    DuplicateFixtureId {
        /// The [`admissionlab_core::FixtureId`] two documents share.
        id: String,
        /// The file containing the first (in discovery order) document
        /// that produced `id`.
        first_path: PathBuf,
        /// That document's zero-based position within `first_path`.
        first_document_index: usize,
        /// The file containing the later document that produced the
        /// same `id`.
        second_path: PathBuf,
        /// That document's zero-based position within `second_path`.
        second_document_index: usize,
    },
}
