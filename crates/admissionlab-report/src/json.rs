//! The machine-readable result artifact.
//!
//! [`write_json_report`] serializes a [`LabResult`] to a file. This is
//! the artifact other tools read -- a CI job deciding whether to block a
//! merge, a script diffing two runs, a future `admissionlab` subcommand
//! replaying an old result -- so its shape matters more than its looks.
//!
//! # The schema is frozen
//!
//! [`crate::model::SCHEMA_VERSION`] is `admissionlab.io/result/v1` and
//! this document is **frozen** (ROADMAP Task 9.1, Global Constraint 9).
//! Within `v1.x` a reader may be given additional optional fields; no
//! existing field's meaning changes silently, no semantic-change wire
//! string is renamed, and removing or renaming a field requires a new
//! result schema version and a migration note. This writer emits that one
//! version and no other.
//!
//! The shape itself is [`crate::wire::ResultDocument`]'s, not
//! [`LabResult`]'s field list -- read that module before changing
//! anything a consumer can see. The published schema is
//! `schemas/result-v1.json`, generated from those same types by
//! [`crate::schema`].
//!
//! # Determinism
//!
//! The same [`LabResult`] always serializes to byte-identical output,
//! which is what makes the golden test in `tests/json.rs` a contract
//! rather than a snapshot that has to be regenerated on every run. Three
//! things make that true, and all three are load-bearing:
//!
//! 1. **Struct fields serialize in declaration order.** `serde`'s derive
//!    emits fields in the order they are written in the Rust source, so
//!    the key order of every object this crate owns is fixed by
//!    `wire.rs`'s own layout.
//! 2. **`serde_json::Value` maps are sorted.** With `serde_json`'s
//!    `preserve_order` feature *off* -- and it is off in this workspace,
//!    which `admissionlab-diff`'s `raw` module already documents a
//!    determinism argument on top of -- a `Value::Object` is backed by a
//!    `BTreeMap` and serializes in sorted key order regardless of how
//!    the object was captured. Every embedded Kubernetes object in the
//!    report goes through that.
//! 3. **Nothing here iterates a `HashMap`.** A `Diagnostic`'s `context`
//!    is a `BTreeMap` for exactly this reason.
//!
//! # Formatting
//!
//! Pretty-printed with `serde_json`'s default two-space indent, plus a
//! trailing newline. The newline is added deliberately: the artifact is
//! a text file that people `cat`, `diff`, and commit to golden
//! directories, and a file without a final newline breaks all three in
//! small, annoying ways.
//!
//! # Why not `ArtifactStore`
//!
//! `admissionlab_core::ArtifactStore` already does atomic JSON writes,
//! and `report -> core` is a sanctioned edge (§1.1), so reusing it was
//! the first thing considered. Its API does not fit this function's
//! frozen signature in three separate ways: it is `async` (this one is
//! not, and adding a runtime requirement to a report writer to reach a
//! `rename` would be backwards), it is constructed around a *root*
//! directory and rejects any path that does not canonicalize inside it
//! (this one takes a bare caller-chosen path with no root to check
//! against), and its error type carries a `PathEscapesRoot` variant that
//! would be unreachable here.
//!
//! So the atomic-write mechanism is mirrored rather than reused, with
//! the same guarantee and the same steps: serialize into memory first
//! (so a serialization failure never touches the filesystem), write to a
//! uniquely named temporary file beside the destination, `fsync` it,
//! then `rename` it into place. A reader of the destination path sees
//! either the previous content or the complete new content, never a
//! partial write. On any failure the temporary file is removed. Same
//! `uuid` v4 suffix core uses, so two concurrent writers of the same
//! path cannot collide on the temporary name.
//!
//! `path`'s parent directory must already exist; like `ArtifactStore`'s
//! own writer, this creates no directories.
//!
//! # Redact first
//!
//! This writer serializes whatever it is given. Hand it a redacted
//! result -- see [`crate::redact::redact_result`] and the "redact once,
//! render many" note on the crate root.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use uuid::Uuid;

use crate::error::ReportError;
use crate::model::LabResult;

/// Serializes `result` as pretty-printed JSON and writes it to `path`
/// atomically.
///
/// See this module's documentation for the schema's stability status,
/// the determinism guarantee, the formatting choices, and exactly what
/// "atomically" means here.
///
/// # Errors
///
/// Returns [`ReportError::Serialize`] if `result` fails to serialize, in
/// which case the filesystem was never touched. Returns
/// [`ReportError::Io`] if creating, writing, syncing, or renaming the
/// temporary file fails -- including when `path` has no parent directory,
/// or when that directory does not exist.
pub fn write_json_report(path: &Path, result: &LabResult) -> Result<(), ReportError> {
    let text = render_json(result)?;
    write_atomic(path, text.as_bytes())
}

/// Serializes `result` exactly as [`write_json_report`] writes it:
/// pretty-printed, with a trailing newline.
///
/// Public because the golden test asserts on the bytes rather than on a
/// file's contents -- a test that had to write and re-read a temporary
/// file to check the formatting would be proving something about the
/// filesystem instead. [`write_json_report`] is defined in terms of this
/// function, so the two can never disagree about what a report looks
/// like.
///
/// # Errors
///
/// Returns [`ReportError::Serialize`] if `result` fails to serialize.
pub fn render_json(result: &LabResult) -> Result<String, ReportError> {
    let mut text = serde_json::to_string_pretty(result).map_err(ReportError::Serialize)?;
    text.push('\n');
    Ok(text)
}

/// Writes `bytes` to `path` through a temporary file and a rename.
///
/// See this module's "Why not `ArtifactStore`" section for what this
/// mirrors and why it is not that type.
///
/// Shared with [`crate::html`] rather than duplicated there: both
/// artifacts are written to a caller-chosen path with the same
/// guarantee, and two copies of a temp-write-fsync-rename dance are two
/// chances to get the cleanup path wrong.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ReportError> {
    let parent = path.parent().ok_or_else(|| {
        ReportError::io(
            "determine the parent directory of",
            path,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path has no parent directory",
            ),
        )
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        ReportError::io(
            "determine the file name of",
            path,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name"),
        )
    })?;

    let temp_path = parent.join(format!(
        ".{}.tmp-{}",
        file_name.to_string_lossy(),
        Uuid::new_v4()
    ));

    if let Err(error) = write_and_sync(&temp_path, bytes) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    if let Err(source) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(ReportError::io(
            "rename temporary file into place at",
            path,
            source,
        ));
    }

    Ok(())
}

/// Creates `temp_path`, writes `bytes` to it in full, and `fsync`s it.
///
/// The `fsync` is what makes the subsequent rename meaningful across a
/// crash: without it the rename can be durable while the content behind
/// it is not, which is the one failure mode this whole dance exists to
/// prevent.
fn write_and_sync(temp_path: &Path, bytes: &[u8]) -> Result<(), ReportError> {
    let mut file = fs::File::create(temp_path)
        .map_err(|source| ReportError::io("create temporary file", temp_path, source))?;
    file.write_all(bytes)
        .map_err(|source| ReportError::io("write temporary file", temp_path, source))?;
    file.sync_all()
        .map_err(|source| ReportError::io("sync temporary file", temp_path, source))?;
    Ok(())
}
