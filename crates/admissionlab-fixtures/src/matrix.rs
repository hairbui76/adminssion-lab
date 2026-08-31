//! Explicit, parameterized fixture matrices (Task 5.10): one base
//! document plus a hand-written list of RFC 6902 JSON Patch cases,
//! expanded at discovery time into ordinary [`FixtureSource`]s.
//!
//! # What this is, and what it deliberately is not
//!
//! Global Constraint 11 puts *generated* fuzz fixtures out of scope for
//! v1. Nothing in this module generates anything: every case, and every
//! patch operation inside it, is written out by hand in a checked-in
//! YAML document and read back verbatim. There is no combinatorial
//! product, no range expansion, no "for each of these values" construct,
//! and no place a future one could be slipped in without changing
//! [`FixtureMatrixSpec`]'s own frozen shape. A matrix is a way to stop
//! copying the same 30-line Pod six times with one field changed — it is
//! not a fixture generator.
//!
//! Parameterization is **patch-based, never textual**. The base document
//! is parsed into a [`serde_json::Value`] first, and each case applies
//! [`json_patch::patch`] to a clone of that parsed value. No case ever
//! sees, splices, interpolates, or re-serializes the base's YAML *text*.
//! That rules out the entire class of bug where a substituted value
//! changes the surrounding document's structure (an unquoted `yes`
//! becoming a boolean, a multi-line value breaking indentation, a `$`
//! sequence being re-expanded), and it means an ill-formed case fails
//! loudly in [`json_patch::patch`] rather than quietly producing a
//! different-but-valid document.
//!
//! # How a matrix is declared (the configuration surface)
//!
//! A matrix is a document **in the fixture tree**, selected by the same
//! `fixtures.include` globs as every other fixture:
//!
//! ```yaml
//! apiVersion: admissionlab.io/v1alpha1
//! kind: FixtureMatrix
//! spec:
//!   id: pod-variants
//!   base: pod-base.yaml
//!   cases:
//!     - id: host-network
//!       patches:
//!         - op: replace
//!           path: /metadata/name
//!           value: pod-variants-host-network
//!         - op: add
//!           path: /spec/hostNetwork
//!           value: true
//! ```
//!
//! **There is no `matrices:` field in `admissionlab.yaml`.** That was a
//! real fork in the road, and this is why it went this way:
//!
//! - Discovery already has exactly one input for "what does this lab
//!   replay?" — [`admissionlab_spec::ResolvedFixtureSelection`]. A
//!   second, parallel list in the lab configuration would make that
//!   question have two answers that could disagree (a matrix selected
//!   but its base excluded, a fixture tree that means different things
//!   depending on which config points at it).
//! - A matrix is *about* the fixtures around it. Keeping it in the
//!   fixture tree keeps it reviewable next to its base, movable with it,
//!   and shareable between labs the same way the base already is.
//! - Determinism costs nothing this way: matrix documents are found by
//!   the same byte-lexicographic walk as static ones (see
//!   [`crate::discover`]), so a matrix's cases always land at the same
//!   position in the discovered order.
//!
//! `base` is resolved **relative to the matrix declaration's own
//! directory**, and confined to it: absolute paths, `..`, and `.` are
//! rejected outright rather than normalized
//! ([`MatrixError::BaseOutsideRoot`]). That makes a matrix and the base
//! it varies a self-contained, relocatable unit — a fixture tree can be
//! moved, vendored, or shared between labs whose `admissionlab.yaml`
//! files live in completely different directories, and every matrix in
//! it keeps working. The alternative considered was resolving `base`
//! against the fixture *selection* root, which would have matched how
//! `fixtures.include` patterns are written; it was rejected precisely
//! because it ties a checked-in fixture tree to where some config file
//! happens to sit, which is the coupling a shared corpus most needs to
//! avoid. The cost is real and worth naming: a matrix cannot reach a
//! base in a sibling directory (`../bases/pod.yaml`), so a base shared
//! by several matrices must sit at or below their common directory.
//!
//! This is also why [`expand_matrix`]'s `root` parameter is that
//! directory rather than [`admissionlab_spec::ResolvedFixtureSelection::root`]:
//! [`crate::discover`] passes the parent of the file the matrix was
//! found in. Confinement to the selection root still holds transitively
//! — the matrix document was itself discovered inside it, and the base
//! can only be at or below the matrix document.
//!
//! The `apiVersion`/`kind` pair, not a filename convention, is what
//! makes a document a matrix. [`crate::discover`] documents that it
//! "does not branch on file extension at all", and this module keeps
//! that property: `*.matrix.yaml` is a naming convention used by the
//! checked-in examples for human legibility only, and renaming such a
//! file changes nothing. Recognition by content is also what makes the
//! interception safe to do at all: `admissionlab.io/v1alpha1` is not a
//! group a real Kubernetes API server serves, so a matrix document could
//! never have been a replayable fixture in the first place.
//!
//! # Is the base document itself replayed?
//!
//! **Only if it independently matches an include glob.** Naming a
//! document as a matrix `base` neither adds it to the replay set nor
//! removes it from one. This is the honest default for two reasons:
//!
//! - `fixtures.include` is documented (in
//!   [`admissionlab_spec::FixtureSelectionSpec::include`]) as *the* rule
//!   for what gets replayed. A matrix that could silently add its base,
//!   or silently suppress a file the user's own glob selected, would
//!   make that rule conditional on the content of some other file.
//! - Both behaviors are genuinely wanted by real corpora. A base that
//!   is itself a meaningful "no variation applied" case should be
//!   replayed; a base that is a bare skeleton no one wants results for
//!   should not. Rather than inventing a `replayBase: true` knob, the
//!   author expresses the choice with the tool that already exists:
//!   where the base file sits relative to the include globs. The
//!   checked-in `fixtures/core/matrix/` example takes the first branch
//!   (its base is a valid, interesting Pod and is matched by the same
//!   `*.yaml` pattern), and says so in its own comments.
//!
//! A replayed base and its cases never collide on identity: a static
//! fixture's [`admissionlab_core::FixtureId`] is path-derived
//! ([`crate::identity::compute_fixture_id`]), a case's is
//! `<matrix-id>-<case-id>`, and [`crate::discover`] checks the complete
//! merged set for duplicates regardless.
//!
//! # Fixture identity: `<matrix-id>-<case-id>`, not `<matrix-id>/<case-id>`
//!
//! The roadmap's stated intent is a stable identity of the form
//! "matrix, then case". It spells that `<matrix-id>/<case-id>`, which
//! [`admissionlab_core::FixtureId`] cannot represent: that type's parser
//! accepts only non-empty strings of ASCII lowercase letters, digits and
//! `-`, precisely so that "every successfully parsed identifier is safe
//! to use directly as a filesystem path segment" (its own module
//! documentation). A `/` is exactly what that rule exists to keep out,
//! and weakening it to satisfy a spelling would put a path separator
//! into a value the artifact store embeds in real paths.
//!
//! So the separator is `-`, and the *intent* — a stable, two-part,
//! matrix-scoped identity — is preserved exactly. The one cost is that
//! `-` is also legal inside each part, so `(matrix "a-b", case "c")` and
//! `(matrix "a", case "b-c")` both render `a-b-c`. That is not silently
//! tolerated: [`crate::discover::discover_fixtures`] runs its existing
//! whole-corpus duplicate-id check over matrix-expanded sources too, so
//! such a pair is a loud [`crate::FixtureError::DuplicateFixtureId`],
//! not two fixtures fighting over one identity. An escaping scheme (say,
//! `--` for a literal `-`) was considered and rejected: it would make
//! every id less legible to defend against a collision that is already
//! detected, for a corpus that is hand-written and small.
//!
//! Both parts are validated by handing them to
//! [`admissionlab_core::FixtureId::parse`] itself, rather than by a
//! second copy of its character rules here — so a matrix id and case id
//! are held to exactly the identifier contract the joined result must
//! satisfy, and the join can then never fail. Note the consequence:
//! unlike a fixture's Kubernetes `metadata.name`, a matrix/case id is
//! **not** slugified. It is an author-chosen identifier, not a value
//! salvaged from arbitrary user data, so a typo (`Host-Network`) is
//! reported rather than quietly rewritten — which also means matrix ids
//! carry none of the lossiness
//! [`crate::identity::compute_fixture_id`]'s own documentation warns
//! about.
//!
//! # Source hash
//!
//! [`FixtureSource::sha256`] for an expanded case is **not** the hash of
//! any file's bytes, and cannot be: no such file exists. It is
//!
//! ```text
//! sha256_hex("admissionlab-fixture-matrix\nv1\n" + <base file sha256, lowercase hex> + "\n"
//!            + <canonical patch JSON> + "\n")
//! ```
//!
//! where `<canonical patch JSON>` is [`serde_json::to_string`] of the
//! case's `Vec<PatchOperation>` (compact, no trailing newline of its
//! own). Point by point:
//!
//! - **Why the base's file hash rather than its parsed content.** It is
//!   the same value [`crate::discover`] would give the base if the base
//!   were replayed directly, computed by the same
//!   [`crate::hash::sha256_hex`] over the same raw on-disk bytes. Two
//!   runs agree on a case's hash if and only if they agree on the base
//!   file's bytes and on the patch list — which is exactly the
//!   provenance claim PRODUCT.md §28 records.
//! - **Why not hash the expanded object.** This crate has a standing
//!   rule (see [`crate::discover`]'s "Hashing") that it never hashes a
//!   re-serialization of a parsed document, because that would make the
//!   hash depend on a canonicalization this crate does not own. Hashing
//!   the expanded object would break that rule for matrices only.
//! - **Why the patch list serializes deterministically.** Every field is
//!   emitted in the order `json_patch`'s own `#[derive(Serialize)]`
//!   declares (with `op` first, from its `#[serde(tag = "op")]`), and
//!   every `value` payload is a [`serde_json::Value`] whose object keys
//!   come out in `BTreeMap` order — this workspace enables
//!   `serde_json`'s `preserve_order` feature nowhere, the same property
//!   `admissionlab_spec::schema`'s module documentation already depends
//!   on and already names as a determinism regression if it changes.
//! - **Why the domain-separating prefix and version.** Without it, a
//!   64-character hex string alone is ambiguous between "a file's hash"
//!   and "a matrix case's hash". `v1` makes a future change to this
//!   recipe an explicit, greppable version bump rather than a silent
//!   hash churn nobody can attribute.
//! - **Why the matrix/case ids are *not* in the hash.** The hash answers
//!   "what content was replayed"; the id answers "which fixture is
//!   this". Two cases that genuinely apply the same patches to the same
//!   base *are* the same content and should hash alike; they still have
//!   distinct ids, and [`crate::discover`] still rejects a duplicate id.
//!
//! # Errors are load-time errors, never skips
//!
//! Every failure in this module surfaces as
//! [`crate::FixtureError::Matrix`], which
//! `admissionlab_cli::exit::disposition_for_fixture_error` classifies
//! with the rest of discovery as invalid input (exit code `2`). A matrix
//! that cannot be expanded stops discovery outright — it is never
//! skipped, never expanded partially, and never downgraded to a warning.
//! A corpus that silently replayed four of five declared cases would
//! report a clean run over a set nobody chose.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use admissionlab_core::{FixtureId, IdParseError};
use json_patch::PatchOperation;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::FixtureError;
use crate::discover::FixtureSource;
use crate::hash::sha256_hex;
use crate::identity::extract_object_identity;

/// Domain-separating prefix and recipe version for
/// [`FixtureSource::sha256`] of a matrix-expanded fixture. See this
/// module's documentation ("Source hash") for why both halves exist.
const HASH_DOMAIN: &str = "admissionlab-fixture-matrix\nv1\n";

/// One declared fixture matrix: a base document plus the explicit,
/// hand-written cases that vary it.
///
/// This is the `spec` block of a `FixtureMatrix` document (see this
/// module's documentation for the full YAML shape). It is strict in the
/// same way every user-authored model in this workspace is: unknown keys
/// are a hard error, not a silently ignored typo.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureMatrixSpec {
    /// This matrix's own identifier, and the first half of every case's
    /// [`admissionlab_core::FixtureId`]. Must itself parse as a
    /// [`admissionlab_core::FixtureId`] — see this module's
    /// documentation for why it is validated rather than slugified.
    /// Must be unique across the whole discovered corpus (checked by
    /// [`crate::discover::discover_fixtures`], not here: one matrix
    /// cannot see another).
    pub id: String,
    /// The base document every case patches, as a path **relative to the
    /// directory this matrix declaration itself lives in**. Absolute
    /// paths, `..`, and `.` are rejected
    /// ([`MatrixError::BaseOutsideRoot`]): a matrix may only name a
    /// document at or below its own directory, which is what keeps a
    /// matrix and its base a relocatable unit. See this module's
    /// documentation for that decision and its cost.
    ///
    /// The file must hold exactly one non-empty YAML document. Naming it
    /// here does not cause it to be replayed as a fixture — see this
    /// module's documentation ("Is the base document itself
    /// replayed?").
    pub base: PathBuf,
    /// The cases, in the order the author wrote them — which is the
    /// order they are discovered in. Must not be empty
    /// ([`MatrixError::NoCases`]).
    pub cases: Vec<FixtureMatrixCase>,
}

/// One case of a [`FixtureMatrixSpec`]: a name, and the RFC 6902
/// operations that turn the base document into this case's object.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureMatrixCase {
    /// This case's identifier, and the second half of its
    /// [`admissionlab_core::FixtureId`]. Must itself parse as a
    /// [`admissionlab_core::FixtureId`], and must be unique within its
    /// own matrix ([`MatrixError::DuplicateCaseId`]).
    pub id: String,
    /// The RFC 6902 JSON Patch operations applied, in order, to a clone
    /// of the parsed base document. Applied by [`json_patch::patch`],
    /// which is atomic: if any operation fails, the whole case fails
    /// ([`MatrixError::PatchFailed`]) and nothing half-patched is ever
    /// observed.
    ///
    /// May legally be empty — a case that applies no patches is exactly
    /// the base document under a matrix-scoped id, which is a
    /// reasonable "control" case to declare next to the variants. It is
    /// still revalidated like any other case.
    pub patches: Vec<PatchOperation>,
}

/// The whole `FixtureMatrix` document, `apiVersion`/`kind` envelope
/// included.
///
/// Nested under a `spec:` key rather than flattened, so
/// [`FixtureMatrixSpec`] can derive [`Deserialize`] with
/// `deny_unknown_fields` directly (serde's `flatten` is incompatible
/// with `deny_unknown_fields`, and duplicating the field list in a
/// second struct would let the two drift). It also gives the document
/// the ordinary Kubernetes `apiVersion`/`kind`/`spec` shape, which is
/// the shape every other document in a fixture tree already has.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureMatrixDocument {
    api_version: String,
    kind: String,
    spec: FixtureMatrixSpec,
}

/// What [`classify_document`] decided a discovered document is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocumentKind {
    /// An ordinary Kubernetes object to validate and replay.
    Static,
    /// A `FixtureMatrix` declaration to parse and expand.
    Matrix,
}

/// Decides whether `document` is an ordinary fixture or a fixture matrix
/// declaration, by its `apiVersion`/`kind` alone.
///
/// A document is a matrix if and only if its `apiVersion` is
/// [`admissionlab_spec::model::API_VERSION`] and its `kind` is
/// [`admissionlab_spec::model::FIXTURE_MATRIX_KIND`]. Both constants come
/// from `admissionlab-spec`, which owns the `admissionlab.io/v1alpha1`
/// vocabulary, so this check can never drift from the `Lab` check
/// `load_lab` performs against the same group/version.
///
/// # Errors
///
/// Returns [`MatrixError::UnknownAdmissionlabKind`] for a document that
/// claims the `admissionlab.io/v1alpha1` group/version but some *other*
/// kind. Such a document is never a replayable Kubernetes object (no API
/// server serves that group), so the alternative to failing here is
/// failing later with a much worse message: a misspelled
/// `kind: Fixturematrix` would fall through to static validation and be
/// reported as a fixture missing `metadata.name`.
pub(crate) fn classify_document(
    document: &Value,
    path: &Path,
    document_index: usize,
) -> Result<DocumentKind, MatrixError> {
    let field = |name: &str| {
        document
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
    };

    if field("apiVersion") != admissionlab_spec::model::API_VERSION {
        return Ok(DocumentKind::Static);
    }
    let kind = field("kind");
    if kind == admissionlab_spec::model::FIXTURE_MATRIX_KIND {
        Ok(DocumentKind::Matrix)
    } else {
        Err(MatrixError::UnknownAdmissionlabKind {
            path: path.to_path_buf(),
            document_index,
            kind: kind.to_owned(),
        })
    }
}

/// Parses an already-classified matrix document into its
/// [`FixtureMatrixSpec`].
///
/// Goes through [`serde_json::from_value`] on the
/// [`serde_json::Value`] [`crate::discover`] already parsed, rather than
/// deserializing from the YAML stream a second time: the patch
/// operations are an internally tagged (`op`) enum whose values are
/// arbitrary JSON, and routing them through `serde_json` is both the
/// representation `json_patch` is written against and a guarantee that
/// what is validated here is byte-identical to what
/// [`case_source_hash`] later serializes.
///
/// # Errors
///
/// Returns [`MatrixError::Document`] if the document does not match the
/// `apiVersion`/`kind`/`spec` shape — an unknown key, a missing `spec`,
/// a patch operation with no `op`, and so on. `serde_json`'s own message
/// names the offending field, so this variant only adds which document
/// it was in.
pub(crate) fn parse_matrix_document(
    document: &Value,
    path: &Path,
    document_index: usize,
) -> Result<FixtureMatrixSpec, MatrixError> {
    let parsed: FixtureMatrixDocument =
        serde_json::from_value(document.clone()).map_err(|error| MatrixError::Document {
            path: path.to_path_buf(),
            document_index,
            reason: error.to_string(),
        })?;
    // `classify_document` already established both; re-deserializing
    // them keeps `deny_unknown_fields` able to see them as known keys.
    debug_assert_eq!(parsed.api_version, admissionlab_spec::model::API_VERSION);
    debug_assert_eq!(parsed.kind, admissionlab_spec::model::FIXTURE_MATRIX_KIND);
    Ok(parsed.spec)
}

/// Expands one declared matrix into one [`FixtureSource`] per case, in
/// the order the cases are written.
///
/// `root` is the directory `spec.base` is resolved against and confined
/// to: the directory the matrix declaration itself lives in, which is
/// what [`crate::discover`] passes. It is deliberately *not*
/// [`admissionlab_spec::ResolvedFixtureSelection::root`] — see this
/// module's documentation for why, and for the identity, hashing, and
/// base-replay rules each returned source follows.
///
/// Every expanded object is put through
/// [`crate::identity::extract_object_identity`] — the *same* function,
/// not a copy of its rules, that every static fixture goes through — so
/// a patch that deletes `kind`, empties `metadata.name`, introduces a
/// `generateName`-only identity, or replaces the whole document with a
/// scalar fails here exactly as it would have failed on disk.
///
/// # Errors
///
/// Returns [`FixtureError::Matrix`] wrapping: [`MatrixError::InvalidMatrixId`]
/// or [`MatrixError::InvalidCaseId`] if an id is not a valid
/// [`FixtureId`]; [`MatrixError::NoCases`] for an empty case list;
/// [`MatrixError::DuplicateCaseId`] if two cases share an id;
/// [`MatrixError::BaseOutsideRoot`] if `spec.base` is absolute or
/// escapes `root`; [`MatrixError::BaseUnreadable`],
/// [`MatrixError::BaseParse`], or [`MatrixError::BaseNotSingleDocument`]
/// if the base file cannot be read as exactly one YAML document;
/// [`MatrixError::PatchFailed`] if [`json_patch::patch`] rejects a
/// case's operations; and [`MatrixError::CaseInvalid`] if the patched
/// result is not a valid fixture object.
pub fn expand_matrix(
    spec: &FixtureMatrixSpec,
    root: &Path,
) -> Result<Vec<FixtureSource>, FixtureError> {
    Ok(expand(spec, root)?)
}

/// [`expand_matrix`]'s body, returning the precise error type so every
/// `?` inside stays typed; the public wrapper widens it exactly once.
fn expand(spec: &FixtureMatrixSpec, root: &Path) -> Result<Vec<FixtureSource>, MatrixError> {
    let matrix_id = FixtureId::parse(&spec.id).map_err(|source| MatrixError::InvalidMatrixId {
        value: spec.id.clone(),
        source,
    })?;

    if spec.cases.is_empty() {
        return Err(MatrixError::NoCases {
            matrix_id: spec.id.clone(),
        });
    }
    reject_duplicate_case_ids(spec)?;

    let base_path = resolve_base_path(root, &spec.base, &spec.id)?;
    let base = load_base_document(&base_path, &spec.id)?;

    let mut sources = Vec::with_capacity(spec.cases.len());
    for case in &spec.cases {
        let case_id = FixtureId::parse(&case.id).map_err(|source| MatrixError::InvalidCaseId {
            matrix_id: spec.id.clone(),
            value: case.id.clone(),
            source,
        })?;

        let mut object = base.object.clone();
        json_patch::patch(&mut object, &case.patches).map_err(|error| {
            MatrixError::PatchFailed {
                matrix_id: spec.id.clone(),
                case_id: case.id.clone(),
                reason: error.to_string(),
            }
        })?;

        // The one validation gate every static fixture passes, reused
        // rather than restated. Its own errors are phrased in terms of a
        // file and a document index, which for an expanded case are the
        // base's -- true, but not the whole story -- so they are wrapped
        // with the matrix/case that produced the object rather than
        // surfaced bare.
        extract_object_identity(&object, &base_path, base.document_index).map_err(|source| {
            MatrixError::CaseInvalid {
                matrix_id: spec.id.clone(),
                case_id: case.id.clone(),
                source: Box::new(source),
            }
        })?;

        sources.push(FixtureSource {
            // Valid by construction: both halves already parsed as a
            // `FixtureId`, so each is non-empty and contains only
            // `[a-z0-9-]`; joining them with `-` (itself allowed) can
            // introduce neither an illegal character nor emptiness.
            // This mirrors `compute_fixture_id`'s own argument for the
            // same `expect`.
            id: FixtureId::parse(&format!("{matrix_id}-{case_id}"))
                .expect("two parsed FixtureIds joined with `-` always re-parse as a FixtureId"),
            path: base_path.clone(),
            document_index: base.document_index,
            sha256: case_source_hash(&base.sha256, &case.patches),
            object,
        });
    }

    Ok(sources)
}

/// Returns [`MatrixError::DuplicateCaseId`] if two cases of `spec` share
/// an id, naming the first (in declaration order) occurrence — so which
/// pair is reported never depends on iteration order of anything
/// unspecified.
fn reject_duplicate_case_ids(spec: &FixtureMatrixSpec) -> Result<(), MatrixError> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, case) in spec.cases.iter().enumerate() {
        if let Some(&first_index) = seen.get(case.id.as_str()) {
            return Err(MatrixError::DuplicateCaseId {
                matrix_id: spec.id.clone(),
                case_id: case.id.clone(),
                first_index,
                second_index: index,
            });
        }
        seen.insert(case.id.as_str(), index);
    }
    Ok(())
}

/// Joins `base` onto `root` (the matrix declaration's own directory),
/// refusing anything that could name a file outside it.
///
/// Only ordinary path components are accepted: an absolute path, a
/// `..`, a bare `.`, and a Windows prefix are all rejected outright
/// rather than normalized. This is the same structural approach
/// [`crate::discover`] takes to symlinks (never resolve the dangerous
/// thing at all, rather than resolve it and check afterwards), and it is
/// decided purely from the written path — never from the filesystem — so
/// it cannot be defeated by a symlink or a race.
fn resolve_base_path(root: &Path, base: &Path, matrix_id: &str) -> Result<PathBuf, MatrixError> {
    let all_normal = base
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    if base.as_os_str().is_empty() || !all_normal {
        return Err(MatrixError::BaseOutsideRoot {
            matrix_id: matrix_id.to_owned(),
            base: base.to_path_buf(),
        });
    }
    Ok(root.join(base))
}

/// A matrix's base document: the single Kubernetes object it holds, that
/// object's position in the file's own YAML stream, and the file's
/// content hash.
struct BaseDocument {
    object: Value,
    document_index: usize,
    sha256: String,
}

/// Reads and parses the base file at `path`.
///
/// Empty documents (comment-only or a bare `---`) are skipped exactly as
/// [`crate::discover`] skips them, and exactly one *non-empty* document
/// must remain: a matrix names a file, not a file-plus-index, so a
/// multi-document base would leave "which one?" unanswered. The
/// surviving document keeps its true index in the file's stream, which
/// is what every expanded [`FixtureSource::document_index`] reports.
///
/// The base is deliberately **not** validated as a fixture object here.
/// Only the patched result is ([`expand`]), because a base whose
/// `metadata.name` every case replaces is a legitimate template, and
/// requiring it to be independently valid would forbid that for no gain
/// — a base that is broken in a way the patches do not fix still fails,
/// once per case, with the case named.
fn load_base_document(path: &Path, matrix_id: &str) -> Result<BaseDocument, MatrixError> {
    let bytes = fs::read(path).map_err(|source| MatrixError::BaseUnreadable {
        matrix_id: matrix_id.to_owned(),
        path: path.to_path_buf(),
        source,
    })?;
    let sha256 = sha256_hex(&bytes);

    let mut found: Option<(usize, Value)> = None;
    let mut count = 0usize;
    for (document_index, document) in serde_norway::Deserializer::from_slice(&bytes).enumerate() {
        let value = Value::deserialize(document).map_err(|error| MatrixError::BaseParse {
            matrix_id: matrix_id.to_owned(),
            path: path.to_path_buf(),
            document_index,
            reason: error.to_string(),
        })?;
        if value.is_null() {
            continue;
        }
        count += 1;
        if found.is_none() {
            found = Some((document_index, value));
        }
    }

    match (found, count) {
        (Some((document_index, object)), 1) => Ok(BaseDocument {
            object,
            document_index,
            sha256,
        }),
        (_, documents) => Err(MatrixError::BaseNotSingleDocument {
            matrix_id: matrix_id.to_owned(),
            path: path.to_path_buf(),
            documents,
        }),
    }
}

/// Computes [`FixtureSource::sha256`] for one expanded case. See this
/// module's documentation ("Source hash") for the recipe and for why
/// each ingredient is the one it is.
fn case_source_hash(base_sha256: &str, patches: &[PatchOperation]) -> String {
    let canonical_patches = serde_json::to_string(patches)
        .expect("a Vec<PatchOperation> is built from serde_json::Values and always serializes");
    let mut preimage =
        String::with_capacity(HASH_DOMAIN.len() + base_sha256.len() + canonical_patches.len() + 2);
    preimage.push_str(HASH_DOMAIN);
    preimage.push_str(base_sha256);
    preimage.push('\n');
    preimage.push_str(&canonical_patches);
    preimage.push('\n');
    sha256_hex(preimage.as_bytes())
}

/// Tracks which matrix ids have been seen across a whole discovery run,
/// so a repeated one is rejected naming its first occurrence.
///
/// Lives here rather than in [`crate::discover`] because the rule is
/// this module's ("a matrix id is unique corpus-wide"), but it is driven
/// from there because only discovery sees more than one matrix. A
/// [`BTreeMap`] (never a `HashMap`) for the same reason
/// [`crate::discover`]'s own duplicate-id check uses one: the reported
/// pair must never depend on an unspecified iteration order.
#[derive(Debug, Default)]
pub(crate) struct MatrixIdRegistry {
    seen: BTreeMap<String, (PathBuf, usize)>,
}

impl MatrixIdRegistry {
    /// Records that `id` was declared by `path`'s `document_index`-th
    /// document.
    ///
    /// # Errors
    ///
    /// Returns [`MatrixError::DuplicateMatrixId`] if `id` was already
    /// recorded, naming both declarations.
    pub(crate) fn record(
        &mut self,
        id: &str,
        path: &Path,
        document_index: usize,
    ) -> Result<(), MatrixError> {
        if let Some((first_path, first_document_index)) = self.seen.get(id) {
            return Err(MatrixError::DuplicateMatrixId {
                id: id.to_owned(),
                first_path: first_path.clone(),
                first_document_index: *first_document_index,
                second_path: path.to_path_buf(),
                second_document_index: document_index,
            });
        }
        self.seen
            .insert(id.to_owned(), (path.to_path_buf(), document_index));
        Ok(())
    }
}

/// Everything that can go wrong declaring or expanding a fixture matrix.
///
/// Reached through [`crate::FixtureError::Matrix`], which is what makes
/// every variant here a load-time, invalid-input failure (exit code
/// `2`) rather than something discovery could skip past — see this
/// module's documentation ("Errors are load-time errors, never skips").
#[derive(Debug, Error)]
pub enum MatrixError {
    /// A document claiming the `admissionlab.io/v1alpha1` group/version
    /// carries a `kind` this crate has no meaning for. Never a
    /// replayable Kubernetes object either, since no API server serves
    /// that group — see [`classify_document`] for why this is caught
    /// here rather than left to fail later and worse.
    #[error(
        "fixture file {} document {}: kind {kind:?} is not a fixture matrix -- a document with \
         apiVersion {:?} must have kind {:?}",
        .path.display(), .document_index + 1,
        admissionlab_spec::model::API_VERSION,
        admissionlab_spec::model::FIXTURE_MATRIX_KIND
    )]
    UnknownAdmissionlabKind {
        /// The file containing the document.
        path: PathBuf,
        /// That document's zero-based position within `path`.
        document_index: usize,
        /// The `kind` the document actually carried (empty if absent or
        /// not a string).
        kind: String,
    },

    /// A `FixtureMatrix` document does not have the declared shape: a
    /// missing or misspelled key, a `cases` entry that is not an
    /// object, a patch operation with an unrecognized `op`, and so on.
    #[error(
        "fixture file {} document {}: not a valid fixture matrix declaration: {reason}",
        .path.display(), .document_index + 1
    )]
    Document {
        /// The file containing the malformed declaration.
        path: PathBuf,
        /// That document's zero-based position within `path`.
        document_index: usize,
        /// `serde_json`'s own explanation, which already names the
        /// offending field.
        reason: String,
    },

    /// `spec.id` is not a usable identifier. Deliberately not slugified
    /// — see this module's documentation ("Fixture identity").
    #[error(
        "fixture matrix id {value:?} is not a valid fixture identifier ({source}); a matrix id \
         is written by hand and is used verbatim, so it must already be non-empty and contain \
         only ASCII lowercase letters, digits, and `-`"
    )]
    InvalidMatrixId {
        /// The id exactly as written.
        value: String,
        /// Why [`FixtureId::parse`] rejected it.
        #[source]
        source: IdParseError,
    },

    /// A `spec.cases[].id` is not a usable identifier, for the same
    /// reasons as [`MatrixError::InvalidMatrixId`].
    #[error(
        "fixture matrix {matrix_id:?}: case id {value:?} is not a valid fixture identifier \
         ({source}); a case id is written by hand and is used verbatim, so it must already be \
         non-empty and contain only ASCII lowercase letters, digits, and `-`"
    )]
    InvalidCaseId {
        /// The matrix the case belongs to.
        matrix_id: String,
        /// The case id exactly as written.
        value: String,
        /// Why [`FixtureId::parse`] rejected it.
        #[source]
        source: IdParseError,
    },

    /// A matrix declares no cases, so it would contribute nothing.
    /// Rejected rather than accepted as a no-op: a matrix that expands
    /// to zero fixtures is a corpus silently smaller than its author
    /// believes it to be.
    #[error(
        "fixture matrix {matrix_id:?} declares no cases; a matrix with an empty `cases` list \
         would expand to no fixtures at all"
    )]
    NoCases {
        /// The matrix with no cases.
        matrix_id: String,
    },

    /// Two cases within one matrix share an id, which would make their
    /// expanded fixtures share an identity. Reported against the first
    /// occurrence in declaration order.
    #[error(
        "fixture matrix {matrix_id:?}: case id {case_id:?} is declared twice (cases {} and {})",
        .first_index + 1, .second_index + 1
    )]
    DuplicateCaseId {
        /// The matrix containing both cases.
        matrix_id: String,
        /// The repeated case id.
        case_id: String,
        /// Zero-based position of the first case with this id.
        first_index: usize,
        /// Zero-based position of the later case with the same id.
        second_index: usize,
    },

    /// Two matrix declarations, anywhere in the discovered corpus, share
    /// an `spec.id`. Reported against the first occurrence in discovery
    /// order (itself deterministic — see [`crate::discover`]).
    #[error(
        "fixture matrix id {id:?} is not unique: declared by both {} document {} and {} \
         document {}",
        .first_path.display(), .first_document_index + 1,
        .second_path.display(), .second_document_index + 1
    )]
    DuplicateMatrixId {
        /// The repeated matrix id.
        id: String,
        /// The file containing the first (in discovery order)
        /// declaration.
        first_path: PathBuf,
        /// That declaration's zero-based position within `first_path`.
        first_document_index: usize,
        /// The file containing the later declaration.
        second_path: PathBuf,
        /// That declaration's zero-based position within `second_path`.
        second_document_index: usize,
    },

    /// `spec.base` is absolute, or contains a component (`..`, `.`, a
    /// filesystem prefix) that could name a file outside the fixture
    /// tree.
    #[error(
        "fixture matrix {matrix_id:?}: base {} must be a relative path made only of ordinary \
         path components, naming a document at or below the directory this matrix is declared in",
        .base.display()
    )]
    BaseOutsideRoot {
        /// The matrix with the unusable base path.
        matrix_id: String,
        /// The base path exactly as written.
        base: PathBuf,
    },

    /// The base file could not be read.
    #[error("fixture matrix {matrix_id:?}: failed to read base {}: {source}", .path.display())]
    BaseUnreadable {
        /// The matrix whose base could not be read.
        matrix_id: String,
        /// The resolved base path.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// A document in the base file is not valid YAML.
    #[error(
        "fixture matrix {matrix_id:?}: base {} document {}: not valid YAML: {reason}",
        .path.display(), .document_index + 1
    )]
    BaseParse {
        /// The matrix whose base is malformed.
        matrix_id: String,
        /// The resolved base path.
        path: PathBuf,
        /// Zero-based position of the malformed document within `path`.
        document_index: usize,
        /// A human-readable explanation from the underlying parser.
        reason: String,
    },

    /// The base file does not hold exactly one non-empty document. A
    /// matrix names a file, not a file and a document index, so zero or
    /// several documents leave "which one is the base?" unanswered.
    #[error(
        "fixture matrix {matrix_id:?}: base {} must contain exactly one document, found {documents}",
        .path.display()
    )]
    BaseNotSingleDocument {
        /// The matrix whose base is ambiguous or empty.
        matrix_id: String,
        /// The resolved base path.
        path: PathBuf,
        /// How many non-empty documents the file actually held.
        documents: usize,
    },

    /// [`json_patch::patch`] rejected a case's operations against the
    /// base document (a pointer naming a location that does not exist, a
    /// failed `test`, and so on). The patch is atomic, so nothing
    /// half-applied is ever observed.
    #[error("fixture matrix {matrix_id:?} case {case_id:?}: patch failed: {reason}")]
    PatchFailed {
        /// The matrix containing the case.
        matrix_id: String,
        /// The case whose patches could not be applied.
        case_id: String,
        /// `json_patch`'s own explanation, which names the failing
        /// operation's index and pointer.
        reason: String,
    },

    /// The patched result is not a valid fixture object: it lost
    /// `apiVersion`/`kind`/`metadata.name`, gained a `generateName`-only
    /// identity, or stopped being an object at all.
    ///
    /// `source` is the *same* [`crate::FixtureError`] a static fixture
    /// with that defect would have produced — this crate validates
    /// expanded objects by reusing
    /// [`crate::identity::extract_object_identity`], never by restating
    /// its rules. It names the base file and document index, since that
    /// is where the object's structure came from; this variant adds
    /// which case's patches left it that way.
    #[error(
        "fixture matrix {matrix_id:?} case {case_id:?}: patched object is not a valid fixture: {source}"
    )]
    CaseInvalid {
        /// The matrix containing the case.
        matrix_id: String,
        /// The case whose patched object is invalid.
        case_id: String,
        /// The underlying static-fixture validation failure. Boxed
        /// because [`crate::FixtureError`] itself contains this type.
        #[source]
        source: Box<FixtureError>,
    },
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use json_patch::PatchOperation;
    use serde_json::json;

    use super::{
        DocumentKind, FixtureMatrixCase, FixtureMatrixSpec, MatrixError, MatrixIdRegistry,
        case_source_hash, classify_document, parse_matrix_document, reject_duplicate_case_ids,
        resolve_base_path,
    };

    fn patches(value: serde_json::Value) -> Vec<PatchOperation> {
        serde_json::from_value(value).expect("valid patch operations")
    }

    fn spec(id: &str, case_ids: &[&str]) -> FixtureMatrixSpec {
        FixtureMatrixSpec {
            id: id.to_owned(),
            base: PathBuf::from("base.yaml"),
            cases: case_ids
                .iter()
                .map(|case_id| FixtureMatrixCase {
                    id: (*case_id).to_owned(),
                    patches: Vec::new(),
                })
                .collect(),
        }
    }

    // -------------------------------------------------------------
    // classify_document
    // -------------------------------------------------------------

    #[test]
    fn an_ordinary_kubernetes_object_classifies_as_static() {
        let doc = json!({"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "p"}});
        assert_eq!(
            classify_document(&doc, Path::new("a.yaml"), 0).unwrap(),
            DocumentKind::Static
        );
    }

    #[test]
    fn a_fixture_matrix_document_classifies_as_matrix() {
        let doc = json!({
            "apiVersion": "admissionlab.io/v1alpha1",
            "kind": "FixtureMatrix",
        });
        assert_eq!(
            classify_document(&doc, Path::new("a.yaml"), 0).unwrap(),
            DocumentKind::Matrix
        );
    }

    /// A near-miss `kind` under this project's own group must be a loud
    /// error, not a fixture that fails much later with an unrelated
    /// message. The mutation this kills: classifying by `kind` alone, or
    /// falling through to `Static` for any unrecognized kind.
    #[test]
    fn a_misspelled_kind_under_the_admissionlab_group_is_rejected() {
        let doc = json!({
            "apiVersion": "admissionlab.io/v1alpha1",
            "kind": "Fixturematrix",
        });
        let err = classify_document(&doc, Path::new("a.yaml"), 2).unwrap_err();
        match err {
            MatrixError::UnknownAdmissionlabKind {
                kind,
                document_index,
                ..
            } => {
                assert_eq!(kind, "Fixturematrix");
                assert_eq!(document_index, 2);
            }
            other => panic!("expected UnknownAdmissionlabKind, got {other:?}"),
        }
    }

    /// A `kind` that is not a string at all must not be silently read as
    /// a match; it falls into the same "unknown kind" error with an
    /// empty name rather than panicking or being treated as static.
    #[test]
    fn a_non_string_kind_under_the_admissionlab_group_is_rejected() {
        let doc = json!({"apiVersion": "admissionlab.io/v1alpha1", "kind": 7});
        assert!(matches!(
            classify_document(&doc, Path::new("a.yaml"), 0).unwrap_err(),
            MatrixError::UnknownAdmissionlabKind { .. }
        ));
    }

    /// `kind: FixtureMatrix` under some *other* group is somebody else's
    /// CRD, not ours -- it must stay a static fixture.
    #[test]
    fn fixture_matrix_kind_under_another_group_is_still_static() {
        let doc = json!({
            "apiVersion": "example.com/v1",
            "kind": "FixtureMatrix",
            "metadata": {"name": "x"},
        });
        assert_eq!(
            classify_document(&doc, Path::new("a.yaml"), 0).unwrap(),
            DocumentKind::Static
        );
    }

    // -------------------------------------------------------------
    // parse_matrix_document
    // -------------------------------------------------------------

    #[test]
    fn parses_a_well_formed_declaration() {
        let doc = json!({
            "apiVersion": "admissionlab.io/v1alpha1",
            "kind": "FixtureMatrix",
            "spec": {
                "id": "m",
                "base": "base.yaml",
                "cases": [
                    {"id": "c", "patches": [{"op": "add", "path": "/a", "value": 1}]},
                ],
            },
        });
        let parsed = parse_matrix_document(&doc, Path::new("a.yaml"), 0).unwrap();
        assert_eq!(parsed.id, "m");
        assert_eq!(parsed.base, PathBuf::from("base.yaml"));
        assert_eq!(parsed.cases.len(), 1);
        assert_eq!(
            parsed.cases[0].patches,
            patches(json!([{"op": "add", "path": "/a", "value": 1}]))
        );
    }

    /// `deny_unknown_fields` is load-bearing, not decorative: a typo in
    /// a matrix key must be reported, never ignored.
    #[test]
    fn an_unknown_key_in_the_spec_is_rejected() {
        let doc = json!({
            "apiVersion": "admissionlab.io/v1alpha1",
            "kind": "FixtureMatrix",
            "spec": {"id": "m", "base": "b.yaml", "cases": [], "caes": []},
        });
        assert!(matches!(
            parse_matrix_document(&doc, Path::new("a.yaml"), 0).unwrap_err(),
            MatrixError::Document { .. }
        ));
    }

    #[test]
    fn an_unrecognized_patch_op_is_rejected() {
        let doc = json!({
            "apiVersion": "admissionlab.io/v1alpha1",
            "kind": "FixtureMatrix",
            "spec": {
                "id": "m",
                "base": "b.yaml",
                "cases": [{"id": "c", "patches": [{"op": "upsert", "path": "/a", "value": 1}]}],
            },
        });
        assert!(matches!(
            parse_matrix_document(&doc, Path::new("a.yaml"), 0).unwrap_err(),
            MatrixError::Document { .. }
        ));
    }

    // -------------------------------------------------------------
    // reject_duplicate_case_ids
    // -------------------------------------------------------------

    #[test]
    fn distinct_case_ids_are_accepted() {
        assert!(reject_duplicate_case_ids(&spec("m", &["a", "b", "c"])).is_ok());
    }

    #[test]
    fn a_repeated_case_id_is_rejected_naming_the_first_occurrence() {
        // Three cases with the collision spanning a distinct middle
        // entry, so an implementation that only compared neighbours
        // would pass every other assertion but fail this one.
        let err = reject_duplicate_case_ids(&spec("m", &["a", "b", "a"])).unwrap_err();
        match err {
            MatrixError::DuplicateCaseId {
                matrix_id,
                case_id,
                first_index,
                second_index,
            } => {
                assert_eq!(matrix_id, "m");
                assert_eq!(case_id, "a");
                assert_eq!(first_index, 0);
                assert_eq!(second_index, 2);
            }
            other => panic!("expected DuplicateCaseId, got {other:?}"),
        }
    }

    // -------------------------------------------------------------
    // resolve_base_path
    // -------------------------------------------------------------

    #[test]
    fn a_nested_relative_base_resolves_under_the_root() {
        let resolved = resolve_base_path(Path::new("/root"), Path::new("a/b.yaml"), "m").unwrap();
        assert_eq!(resolved, PathBuf::from("/root/a/b.yaml"));
    }

    #[test]
    fn an_absolute_base_is_rejected() {
        assert!(matches!(
            resolve_base_path(Path::new("/root"), Path::new("/etc/passwd"), "m").unwrap_err(),
            MatrixError::BaseOutsideRoot { .. }
        ));
    }

    #[test]
    fn a_parent_traversal_base_is_rejected() {
        assert!(matches!(
            resolve_base_path(Path::new("/root"), Path::new("../outside.yaml"), "m").unwrap_err(),
            MatrixError::BaseOutsideRoot { .. }
        ));
    }

    /// A traversal buried mid-path still escapes (`a/../../x`), so the
    /// check must look at every component, not only the first.
    #[test]
    fn a_mid_path_parent_traversal_is_rejected() {
        assert!(matches!(
            resolve_base_path(Path::new("/root"), Path::new("a/../../x.yaml"), "m").unwrap_err(),
            MatrixError::BaseOutsideRoot { .. }
        ));
    }

    #[test]
    fn an_empty_base_is_rejected() {
        assert!(matches!(
            resolve_base_path(Path::new("/root"), Path::new(""), "m").unwrap_err(),
            MatrixError::BaseOutsideRoot { .. }
        ));
    }

    // -------------------------------------------------------------
    // case_source_hash
    // -------------------------------------------------------------

    #[test]
    fn the_case_hash_is_stable_across_calls() {
        let ops = patches(json!([{"op": "add", "path": "/spec/hostNetwork", "value": true}]));
        assert_eq!(case_source_hash("abc", &ops), case_source_hash("abc", &ops));
    }

    #[test]
    fn the_case_hash_changes_when_the_base_hash_changes() {
        let ops = patches(json!([{"op": "add", "path": "/a", "value": 1}]));
        assert_ne!(case_source_hash("abc", &ops), case_source_hash("abd", &ops));
    }

    #[test]
    fn the_case_hash_changes_when_the_patches_change() {
        let a = patches(json!([{"op": "add", "path": "/a", "value": 1}]));
        let b = patches(json!([{"op": "add", "path": "/a", "value": 2}]));
        assert_ne!(case_source_hash("abc", &a), case_source_hash("abc", &b));
    }

    /// Patch *order* is semantically meaningful (RFC 6902 applies
    /// operations in sequence), so a reordering must change the hash.
    /// The mutation this kills: canonicalizing by sorting the operation
    /// list before serializing it.
    #[test]
    fn the_case_hash_changes_when_the_patches_are_reordered() {
        let a = patches(json!([
            {"op": "add", "path": "/a", "value": 1},
            {"op": "add", "path": "/b", "value": 2},
        ]));
        let b = patches(json!([
            {"op": "add", "path": "/b", "value": 2},
            {"op": "add", "path": "/a", "value": 1},
        ]));
        assert_ne!(case_source_hash("abc", &a), case_source_hash("abc", &b));
    }

    /// Two objects whose keys were *written* in different orders are the
    /// same JSON value, so they must hash the same -- this is the
    /// `BTreeMap`-backed `serde_json::Map` property the module
    /// documentation names. The mutation this kills: enabling
    /// `preserve_order` anywhere in the workspace.
    #[test]
    fn the_case_hash_ignores_the_written_key_order_of_a_patch_value() {
        let a = patches(json!([{"op": "add", "path": "/m", "value": {"x": 1, "y": 2}}]));
        let b = patches(json!([{"op": "add", "path": "/m", "value": {"y": 2, "x": 1}}]));
        assert_eq!(case_source_hash("abc", &a), case_source_hash("abc", &b));
    }

    /// The hash must not merely be the base file's hash passed through
    /// -- otherwise every case of a matrix would carry the base's own
    /// provenance and the patch list would be invisible.
    #[test]
    fn the_case_hash_is_not_the_bare_base_hash() {
        let base = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_ne!(case_source_hash(base, &[]), base);
    }

    // -------------------------------------------------------------
    // MatrixIdRegistry
    // -------------------------------------------------------------

    #[test]
    fn distinct_matrix_ids_are_accepted() {
        let mut registry = MatrixIdRegistry::default();
        registry.record("a", Path::new("a.yaml"), 0).unwrap();
        registry.record("b", Path::new("b.yaml"), 0).unwrap();
    }

    #[test]
    fn a_repeated_matrix_id_is_rejected_naming_both_declarations() {
        let mut registry = MatrixIdRegistry::default();
        registry.record("a", Path::new("first.yaml"), 0).unwrap();
        registry.record("b", Path::new("second.yaml"), 1).unwrap();
        let err = registry
            .record("a", Path::new("third.yaml"), 2)
            .unwrap_err();
        match err {
            MatrixError::DuplicateMatrixId {
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
                assert_eq!(second_document_index, 2);
            }
            other => panic!("expected DuplicateMatrixId, got {other:?}"),
        }
    }
}
