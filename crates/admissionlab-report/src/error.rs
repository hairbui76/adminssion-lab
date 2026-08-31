//! What can go wrong while writing a report artifact to disk.
//!
//! One error type for every renderer that touches the filesystem
//! ([`crate::json::write_json_report`], and Task 4.13's HTML writer), so
//! a caller writing both artifacts in sequence handles one vocabulary
//! rather than two nearly identical ones.
//!
//! Modelled on `admissionlab_core::ArtifactError`, whose
//! `operation`/`path`/`source` triple this reuses verbatim: a failure to
//! write a report should read the same as a failure to write any other
//! run artifact. It is deliberately *not* that type. `ArtifactError`
//! carries a `PathEscapesRoot` variant that only means something inside
//! an artifact store's root, and these writers take a bare path with no
//! root to check against -- see [`crate::json`] for why they cannot go
//! through `ArtifactStore` at all.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// A report artifact could not be written.
#[derive(Debug, Error)]
pub enum ReportError {
    /// Serializing the result as JSON failed.
    ///
    /// Because the writers serialize into an in-memory buffer before
    /// touching the filesystem at all, this variant is only ever
    /// produced before any I/O has happened: the destination file is
    /// untouched, or unchanged if it already existed, and no temporary
    /// file was ever created.
    #[error("failed to serialize the result as JSON: {0}")]
    Serialize(#[source] serde_json::Error),

    /// A filesystem operation failed.
    #[error("failed to {operation} `{}`: {source}", .path.display())]
    Io {
        /// A short description of what was being attempted, for example
        /// `"create temporary file"` or `"rename temporary file into
        /// place at"`.
        operation: &'static str,
        /// The path the operation was acting on.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: io::Error,
    },
}

impl ReportError {
    /// Builds a [`ReportError::Io`] describing a failure while
    /// performing `operation` on `path`.
    pub(crate) fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}
