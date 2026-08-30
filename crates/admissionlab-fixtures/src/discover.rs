//! Finding fixture documents on disk and turning each into a
//! [`FixtureSource`]: [`discover_fixtures`] is this crate's entry point
//! for Task 3.1.
//!
//! # Pipeline
//!
//! 1. Recursively walk [`admissionlab_spec::ResolvedFixtureSelection::root`]
//!    (see "Symlinks" below for what is deliberately excluded).
//! 2. Keep every discovered file whose path, made relative to `root`,
//!    matches at least one of [`admissionlab_spec::ResolvedFixtureSelection::include`]'s
//!    compiled glob patterns (a logical OR across patterns -- there is
//!    no exclude list in Alpha).
//! 3. **Sort the matched files by that relative path, explicitly**
//!    (`str`'s own byte-lexicographic [`Ord`], never `Path`'s
//!    component-wise one -- the two disagree on some inputs, for
//!    example `"a-b"` sorts before `"a/b"` under `str::cmp` but after it
//!    under `Path::cmp`, so which one is used is a real, observable
//!    choice, not a formality). This is what makes the returned order
//!    independent of `read_dir`'s own unspecified traversal order —
//!    load-bearing for determinism, not cosmetic; see
//!    `tests/discover.rs`'s ordering test for a proof that does not rely
//!    on the filesystem happening to already return sorted entries.
//! 4. For each matched file, in that sorted order: read its bytes once,
//!    hash them (see "Hashing" below), and parse them as a (possibly
//!    multi-document) YAML stream. JSON is valid YAML, so a `.json`
//!    fixture file needs no separate code path -- this crate does not
//!    branch on file extension at all.
//! 5. For each parsed document, in file order: skip it if it is empty
//!    (see "Empty documents" below); otherwise validate it and extract
//!    its Kubernetes identity ([`crate::identity::extract_object_identity`]),
//!    compute its [`admissionlab_core::FixtureId`]
//!    ([`crate::identity::compute_fixture_id`]), and emit one
//!    [`FixtureSource`].
//! 6. Once every file has been processed with no error, check the
//!    *complete* result for a repeated [`admissionlab_core::FixtureId`]
//!    (see "Fixture ID collisions" below) before returning it.
//!
//! Any failure in steps 1, 4, or 5 returns immediately
//! ([`crate::FixtureError`]), without processing any file later in the
//! sorted order — mirroring `admissionlab_installer::manifests`'s own
//! "parse everything locally before doing anything else" discipline.
//!
//! # Symlinks
//!
//! A symlink — whether it names a file or a directory — is never
//! followed: [`walk_files`] checks
//! [`std::fs::DirEntry::file_type`] (which, unlike
//! [`std::fs::metadata`], does not follow a symlink) and skips any entry
//! reporting `is_symlink()`. This is a deliberate scope boundary, not an
//! oversight: recursing through a symlinked directory risks an infinite
//! loop (a cycle back to an ancestor), and following a symlinked file
//! would let content physically outside `root` be silently discovered as
//! though it were inside it — the exact class of gap
//! `admissionlab_recipes::model`'s own `join_confined` documents for a
//! different feature (`install.paths` confinement). This module avoids
//! that class of bug structurally, by never resolving a symlink at all,
//! rather than by joining a path and checking containment after the
//! fact.
//!
//! # Path normalization (controller supplement §3, Task 3.1)
//!
//! A discovered file's path relative to `root` is required to be valid
//! UTF-8 ([`crate::FixtureError::NonUtf8Path`] otherwise) and is used
//! exactly as [`std::path::Path::to_str`] renders it — no case-folding,
//! no Unicode normalization, no separator rewriting. This project builds
//! only for `x86_64-unknown-linux-gnu`, where a real filesystem walk's
//! path components are always `/`-separated already (never `\`, never a
//! drive letter or UNC prefix), so there is no cross-platform separator
//! problem to normalize away; implementing speculative handling for a
//! target this project does not build for would be unverifiable dead
//! code. The file extension (`.yaml`, `.json`, or anything else) is
//! **not** stripped before this path text is folded into a
//! [`admissionlab_core::FixtureId`] (see
//! [`crate::identity::compute_fixture_id`]) — keeping the relative path
//! textually intact, beyond the character-set sanitization
//! [`admissionlab_core::FixtureId`] itself requires, is a simpler and
//! more literal reading of "derive from the normalized relative path"
//! than inventing an additional stripping rule the brief does not ask
//! for.
//!
//! # Empty documents
//!
//! A YAML document that is only comments and/or a bare `---` separator
//! parses to `Value::Null`. This module **skips** such a document —
//! it is not turned into a [`FixtureSource`], and is not an error —
//! mirroring `admissionlab_installer::manifests`'s established handling
//! of the identical situation for raw manifests ("a trailing empty
//! document ... is silently dropped rather than surfacing as a spurious
//! null entry"). This is a deliberate choice between two options the
//! controller supplement left open (skip or reject); a document that
//! *is* present but structurally incomplete (missing `apiVersion`,
//! `kind`, or a deterministic name) is **not** treated the same way — it
//! is rejected as an error, because unlike a stray formatting `---` it
//! looks like an attempt at a real fixture that failed, not incidental
//! YAML syntax.
//!
//! **`document_index` is never renumbered around a skipped document.**
//! It is the document's zero-based position in `serde_norway`'s own
//! document stream — computed once via `Iterator::enumerate` before any
//! filtering — so a file whose first document is empty has its first
//! *real* fixture at `document_index == 1`, not `0`; adding or removing
//! a leading empty document therefore never silently shifts a later
//! fixture's identity. See `tests/discover.rs`'s
//! `document_index_survives_a_skipped_empty_document_without_renumbering`
//! for a test that would fail if indices were instead assigned by
//! position among survivors.
//!
//! # Hashing
//!
//! [`FixtureSource::sha256`] is the SHA-256 (lowercase hex) of the
//! **whole matched file's** raw, on-disk, un-re-serialized bytes —
//! computed once per file and shared identically by every
//! [`FixtureSource`] produced from that file's documents. This mirrors
//! `admissionlab_installer::manifests::ManifestBundle::source_hash`'s
//! established "canonical source bytes, never a re-serialization"
//! convention in this same codebase. It is deliberately **not** a
//! per-document hash: `serde_norway`'s multi-document API hands back
//! parsed values, not each document's own exact byte span, and
//! reconstructing that span by re-splitting on `---` would have to
//! reimplement YAML's own (non-trivial — a literal `---`-prefixed line
//! can legally appear inside a block scalar without ending a document)
//! boundary detection. A file-level hash still gives genuine
//! provenance value — it changes if and only if the file's bytes
//! change — at the cost of two sibling documents from the same file
//! being unable to distinguish "my content changed" from "my sibling's
//! did," which downstream provenance use (PRODUCT.md §28 records a hash
//! per [`admissionlab_core::FixtureId`], not a claim that two different
//! IDs from the same file must hash differently) does not require. See
//! this hash's own documentation ([`crate::hash::sha256_hex`]) for why
//! it is provenance-only, never a security/tamper-authentication
//! mechanism.
//!
//! # Fixture ID collisions
//!
//! [`crate::identity::compute_fixture_id`]'s own documentation explains
//! why its slug construction is lossy and therefore not provably
//! injective. [`discover_fixtures`] does not trust it blindly: after
//! every file has been processed, [`reject_duplicate_ids`] walks the
//! complete, already-deterministically-ordered result and returns
//! [`crate::FixtureError::DuplicateFixtureId`] the moment two documents
//! share one [`admissionlab_core::FixtureId`], naming the *first* (in
//! discovery order) occurrence — so which pair gets named in the error
//! is itself deterministic, never dependent on hash-map iteration order.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use admissionlab_core::FixtureId;
use admissionlab_spec::ResolvedFixtureSelection;
use globset::GlobMatcher;
use serde::Deserialize as _;

use crate::FixtureError;
use crate::hash::sha256_hex;
use crate::identity::{compute_fixture_id, extract_object_identity};

/// One discovered fixture document: a single Kubernetes object parsed
/// from one position in one file, with a stable identity and a
/// provenance hash. See [`discover_fixtures`] and this module's
/// documentation for exactly how each field is computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureSource {
    /// This document's stable, cross-machine-deterministic identifier.
    pub id: FixtureId,
    /// The file this document was parsed from,
    /// [`admissionlab_spec::ResolvedFixtureSelection::root`] joined with
    /// the matched relative path — never canonicalized, matching how
    /// every other path in this workspace is handled (see
    /// `admissionlab_spec::resolve`'s own module documentation).
    pub path: PathBuf,
    /// This document's zero-based position within `path`'s YAML document
    /// stream. See this module's documentation ("Empty documents") for
    /// why this can be greater than a naive count of *valid* documents
    /// seen so far.
    pub document_index: usize,
    /// SHA-256 (lowercase hex) of `path`'s whole raw file content. Every
    /// [`FixtureSource`] sharing the same `path` also shares this value
    /// — see this module's documentation ("Hashing").
    pub sha256: String,
    /// This document's own parsed content, exactly as YAML/JSON
    /// deserialized it — never re-serialized or mutated.
    pub object: serde_json::Value,
}

/// Discovers every fixture document [`ResolvedFixtureSelection`] selects
/// and returns one [`FixtureSource`] per valid document, in a fully
/// deterministic order (sorted by each source file's path relative to
/// `selection.root`, then by each file's own document order). See this
/// module's documentation for the full pipeline.
///
/// # Errors
///
/// Returns [`FixtureError::WalkDirectory`] if `selection.root` (or a
/// directory beneath it) cannot be listed. Returns
/// [`FixtureError::NonUtf8Path`] if a matched file's path relative to
/// `selection.root` is not valid UTF-8. Returns
/// [`FixtureError::ReadFile`] if a matched file cannot be read.
/// Returns [`FixtureError::Parse`] if a matched file is not valid YAML.
/// Returns [`FixtureError::NotAnObject`], [`FixtureError::MissingField`],
/// or [`FixtureError::GenerateNameUnsupported`] if a non-empty document
/// does not have the deterministic Kubernetes identity Alpha requires
/// (see [`crate::identity`]'s module documentation). Returns
/// [`FixtureError::DuplicateFixtureId`] if two documents land on the
/// same [`FixtureId`]. Every error case stops discovery immediately,
/// without processing any file later in the deterministic order.
pub fn discover_fixtures(
    selection: &ResolvedFixtureSelection,
) -> Result<Vec<FixtureSource>, FixtureError> {
    let matchers: Vec<GlobMatcher> = selection
        .include
        .iter()
        .map(globset::Glob::compile_matcher)
        .collect();

    let mut candidates = matched_relative_paths(&selection.root, &matchers)?;
    // Load-bearing: see this module's documentation, "Pipeline" step 3,
    // for why this sorts the `String` relative-path key rather than the
    // `PathBuf` full path.
    candidates.sort();

    let mut sources = Vec::new();
    for (relative, full_path) in candidates {
        let bytes = fs::read(&full_path).map_err(|source| FixtureError::ReadFile {
            path: full_path.clone(),
            source,
        })?;
        let sha256 = sha256_hex(&bytes);

        for (document_index, document) in serde_norway::Deserializer::from_slice(&bytes).enumerate()
        {
            let value: serde_json::Value =
                serde_json::Value::deserialize(document).map_err(|error| FixtureError::Parse {
                    path: full_path.clone(),
                    document_index,
                    reason: error.to_string(),
                })?;
            if value.is_null() {
                // Comment-only or bare `---` document -- see this
                // module's documentation, "Empty documents". Not an
                // error, and `document_index` is not reused or
                // decremented: the next real document keeps its true
                // position in `full_path`'s own stream.
                continue;
            }

            let identity = extract_object_identity(&value, &full_path, document_index)?;
            let id = compute_fixture_id(&relative, document_index, &identity);

            sources.push(FixtureSource {
                id,
                path: full_path.clone(),
                document_index,
                sha256: sha256.clone(),
                object: value,
            });
        }
    }

    reject_duplicate_ids(&sources)?;

    Ok(sources)
}

/// Walks every file reachable under `root` (see this module's
/// documentation, "Symlinks", for what is deliberately excluded) and
/// returns `(relative_path, full_path)` for each one whose
/// `relative_path` (its path relative to `root`, rendered as UTF-8 text)
/// matches at least one of `matchers`. Order is **not** meaningful here
/// — [`discover_fixtures`] sorts the result itself.
///
/// # Errors
///
/// Returns [`FixtureError::WalkDirectory`] if `root` (or a directory
/// beneath it) cannot be listed. Returns [`FixtureError::NonUtf8Path`]
/// if a file that matches at least one pattern has a relative path that
/// is not valid UTF-8. Matching is checked first, directly against the
/// `Path` (`GlobMatcher::is_match` needs no `&str`), and the UTF-8
/// conversion only afterward, on the reduced candidate set — so an
/// unrelated, non-UTF-8-named file elsewhere under `root` that nothing
/// would ever have selected can never break discovery.
fn matched_relative_paths(
    root: &Path,
    matchers: &[GlobMatcher],
) -> Result<Vec<(String, PathBuf)>, FixtureError> {
    let mut out = Vec::new();
    for full_path in walk_files(root)? {
        let relative = full_path
            .strip_prefix(root)
            .expect("walk_files only ever yields paths nested under the root it was given");
        if !matchers.iter().any(|matcher| matcher.is_match(relative)) {
            continue;
        }
        let relative_str = relative.to_str().ok_or_else(|| FixtureError::NonUtf8Path {
            path: full_path.clone(),
        })?;
        out.push((relative_str.to_owned(), full_path));
    }
    Ok(out)
}

/// Recursively collects every plain file reachable under `root`,
/// skipping symlinks entirely (see this module's documentation,
/// "Symlinks"). Returned order is unspecified — an artifact of whatever
/// order the OS's `read_dir` happens to yield, deliberately never relied
/// upon by any caller.
///
/// # Errors
///
/// Returns [`FixtureError::WalkDirectory`] if `root` itself, or any
/// directory reached while recursing into it, cannot be listed, or if
/// any entry's file type cannot be read.
fn walk_files(root: &Path) -> Result<Vec<PathBuf>, FixtureError> {
    let mut out = Vec::new();
    walk_into(root, &mut out)?;
    Ok(out)
}

fn walk_into(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), FixtureError> {
    let entries = fs::read_dir(dir).map_err(|source| FixtureError::WalkDirectory {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| FixtureError::WalkDirectory {
            path: dir.to_path_buf(),
            source,
        })?;
        let file_type = entry
            .file_type()
            .map_err(|source| FixtureError::WalkDirectory {
                path: entry.path(),
                source,
            })?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            walk_into(&entry.path(), out)?;
        } else if file_type.is_file() {
            out.push(entry.path());
        }
        // Anything else (a socket, FIFO, device node, ...) is not a
        // candidate fixture file and is silently skipped.
    }
    Ok(())
}

/// Returns [`FixtureError::DuplicateFixtureId`] if any two entries of
/// `sources` share an [`FixtureId`]; `Ok(())` otherwise. `sources` is
/// walked in its given order (assumed already deterministic — see this
/// module's documentation, "Fixture ID collisions"), and a
/// [`BTreeMap`] (never a `HashMap`) tracks each id's first occurrence so
/// the reported pair never depends on any unspecified iteration order.
fn reject_duplicate_ids(sources: &[FixtureSource]) -> Result<(), FixtureError> {
    let mut seen: BTreeMap<&str, (&Path, usize)> = BTreeMap::new();
    for source in sources {
        let id = source.id.as_str();
        if let Some(&(first_path, first_document_index)) = seen.get(id) {
            return Err(FixtureError::DuplicateFixtureId {
                id: id.to_owned(),
                first_path: first_path.to_path_buf(),
                first_document_index,
                second_path: source.path.clone(),
                second_document_index: source.document_index,
            });
        }
        seen.insert(id, (source.path.as_path(), source.document_index));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::{FixtureSource, reject_duplicate_ids};
    use crate::FixtureError;
    use admissionlab_core::FixtureId;

    fn source(id: &str, path: &str, document_index: usize) -> FixtureSource {
        FixtureSource {
            id: FixtureId::parse(id).unwrap(),
            path: PathBuf::from(path),
            document_index,
            sha256: "0".repeat(64),
            object: json!({"kind": "ConfigMap"}),
        }
    }

    #[test]
    fn no_duplicates_among_distinct_ids_is_accepted() {
        let sources = vec![source("a", "a.yaml", 0), source("b", "b.yaml", 0)];
        assert!(reject_duplicate_ids(&sources).is_ok());
    }

    #[test]
    fn an_empty_list_is_accepted() {
        assert!(reject_duplicate_ids(&[]).is_ok());
    }

    #[test]
    fn a_single_source_is_accepted() {
        assert!(reject_duplicate_ids(&[source("a", "a.yaml", 0)]).is_ok());
    }

    #[test]
    fn a_repeated_id_is_rejected_naming_the_first_occurrence() {
        // A three-element list, so this also proves the check does not
        // merely compare each element to its immediate predecessor: `a`
        // (index 0) and `a` (index 2) collide across a distinct `b` in
        // between.
        let sources = vec![
            source("a", "first.yaml", 0),
            source("b", "second.yaml", 0),
            source("a", "third.yaml", 0),
        ];
        let err = reject_duplicate_ids(&sources).unwrap_err();
        match err {
            FixtureError::DuplicateFixtureId {
                id,
                first_path,
                first_document_index,
                second_path,
                second_document_index,
            } => {
                assert_eq!(id, "a");
                assert_eq!(first_path, PathBuf::from("first.yaml"));
                assert_eq!(first_document_index, 0);
                assert_eq!(second_path, PathBuf::from("third.yaml"));
                assert_eq!(second_document_index, 0);
            }
            other => panic!("expected DuplicateFixtureId, got {other:?}"),
        }
    }

    /// The mutation this test exists to kill: an implementation that
    /// never called `reject_duplicate_ids` at all (or called it but
    /// ignored the result) would let `discover_fixtures` silently return
    /// two [`FixtureSource`]s with the same id. This test does not
    /// exercise `discover_fixtures` itself (that is `tests/discover.rs`'s
    /// job, end to end) -- it pins the pure check in isolation, so a
    /// failure here can only mean this function's own logic broke, not
    /// something upstream of it in the pipeline.
    #[test]
    fn duplicate_check_is_not_vacuously_true_for_every_input() {
        let colliding = vec![source("same", "x.yaml", 0), source("same", "y.yaml", 0)];
        assert!(
            reject_duplicate_ids(&colliding).is_err(),
            "sanity check on the check itself: two equal ids must be rejected"
        );
    }
}
