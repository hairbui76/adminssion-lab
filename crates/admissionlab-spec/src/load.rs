//! Strict YAML loading of an `admissionlab.yaml` configuration file, in
//! any `apiVersion` this crate still reads.
//!
//! Two entry points, one of which is the one to use:
//!
//! - [`load_any_supported_lab`] is the loader (ROADMAP Task 7.1 Step 2).
//!   It dispatches on the document's own `apiVersion`, parses it with
//!   that version's model, migrates it to the current model if it is not
//!   already, and returns a fully resolved [`ResolvedLab`].
//! - [`load_lab`] is the original `v1alpha1`-only loader, kept because it
//!   is the one entry point that hands back the *unresolved*
//!   [`LoadedLab`] — the as-written document, paths and all — which is
//!   what makes it useful in tests and diagnostics. It accepts Alpha
//!   documents and nothing else, by design: a function whose name and
//!   return type promise "the v1alpha1 model" must not quietly return
//!   something else.
//!
//! # Reading a document is not the same as hashing one
//!
//! `admissionlab reproduce` verifies a run by hashing the configuration
//! **file**, byte for byte, exactly as it sits on disk
//! (`admissionlab_core::run_manifest`'s `config_sha256`, computed by
//! `file_sha256`). Migration happens strictly *after* those bytes have
//! been read and never rewrites the file, so promoting the config
//! contract does not move a single hash: a manifest recorded from a
//! `v1alpha1` file still verifies against that same `v1alpha1` file, and
//! would (correctly) fail against a hand-migrated `v1beta1` rewrite of
//! it, because that is a different input to the run. Reproducibility is
//! a claim about the bytes a user ran, not about the model this crate
//! parsed them into.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::error::SpecError;
use crate::migrate::migrate_v1alpha1_to_v1beta1;
use crate::resolve::{LoadedLab, ResolvedLab};
use crate::v1alpha1::{self, V1Alpha1Lab};
use crate::v1beta1::{self, V1Beta1Lab};
use crate::{resolve, validate};

/// Every `apiVersion` [`load_any_supported_lab`] accepts, current first.
///
/// Ordered deliberately: an error message built from this list names the
/// version a user should be writing today before the one that is merely
/// still read.
pub const SUPPORTED_API_VERSIONS: [&str; 2] = [v1beta1::API_VERSION, v1alpha1::API_VERSION];

/// Just enough of a lab document to decide which model to parse it with.
///
/// Deliberately **not** `deny_unknown_fields`: this is a peek, and every
/// other key in the document belongs to a model that has not been chosen
/// yet. The strict parse happens immediately afterwards, against the
/// version-specific model, so a misspelled key is still a loud error —
/// just one reported by the right model.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentVersion {
    #[serde(default)]
    api_version: Option<String>,
}

/// The `apiVersion` the lab document at `path` declares, exactly as
/// written, once it has been confirmed to be one this crate reads.
///
/// This exists for **provenance, not behavior**. A run manifest records
/// which configuration contract a run was driven by
/// (`admissionlab_core::RunManifest::config_api_version`), and
/// [`ResolvedLab`] deliberately carries no version marker — see its own
/// "Version-independent by construction". Rather than weaken that by
/// threading a version through the resolved model, the one caller that
/// needs the string asks for it directly.
///
/// Reading the document twice (once here, once in
/// [`load_any_supported_lab`]) is deliberate and cheap: a configuration
/// file is small, and the alternative — a second loader entry point
/// returning a pair — would put a version back into the type every other
/// caller passes around. The run's own `config_sha256` already reads the
/// file separately for the same reason.
///
/// # Errors
///
/// Returns [`SpecError::Io`] if `path` cannot be read,
/// [`SpecError::Parse`] if it is not a YAML mapping at all, and
/// [`SpecError::Validation`] naming every supported version if the
/// document declares no `apiVersion` or an unsupported one — the same
/// error [`load_any_supported_lab`] would give it.
pub fn declared_api_version(path: &Path) -> Result<String, SpecError> {
    let text = read_document(path)?;
    let peeked: DocumentVersion =
        serde_norway::from_str(&text).map_err(|source| SpecError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    match peeked.api_version {
        Some(version) if SUPPORTED_API_VERSIONS.contains(&version.as_str()) => Ok(version),
        Some(version) => Err(unsupported_api_version(path, Some(&version))),
        None => Err(unsupported_api_version(path, None)),
    }
}

/// Reads, parses, migrates if necessary, and resolves the
/// `admissionlab.yaml` configuration file at `path`, whatever supported
/// `apiVersion` it declares.
///
/// This is the entry point the CLI uses, and the only one that
/// implements ROADMAP Task 7.1 Step 2's promise that Public Alpha
/// configurations keep working:
///
/// - `admissionlab.io/v1beta1` documents are parsed with the current
///   model ([`V1Beta1Lab`]) and resolved directly.
/// - `admissionlab.io/v1alpha1` documents are parsed with the frozen
///   Alpha model ([`V1Alpha1Lab`]), migrated by
///   [`migrate_v1alpha1_to_v1beta1`], and then resolved by the very same
///   code — there is one resolver, not one per version, which is what
///   makes "an Alpha config behaves identically" a structural property
///   rather than a pair of implementations to keep in sync.
/// - Anything else is refused with an error naming every version that
///   *is* supported.
///
/// The returned [`ResolvedLab`] carries no trace of which version it came
/// from: by the time it exists, the document has been normalized to one
/// model. No crate above this one needs to know two versions exist.
///
/// # Errors
///
/// Returns [`SpecError::Io`] if `path` cannot be read,
/// [`SpecError::Parse`] if its contents are not a syntactically valid
/// document in the version it declares (the error includes the serde
/// path to the offending field), [`SpecError::Validation`] if
/// `apiVersion` is missing, is not a supported version, or if `kind` is
/// wrong, and [`SpecError::Validation`] or [`SpecError::InvalidGlob`]
/// for any of the semantic failures [`crate::resolve_lab`] documents.
pub fn load_any_supported_lab(path: &Path) -> Result<ResolvedLab, SpecError> {
    let text = read_document(path)?;

    let peeked: DocumentVersion =
        serde_norway::from_str(&text).map_err(|source| SpecError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    let Some(api_version) = peeked.api_version else {
        return Err(unsupported_api_version(path, None));
    };

    let beta = if api_version == v1beta1::API_VERSION {
        let raw: V1Beta1Lab = parse_document(&text, path)?;
        validate::document_header(v1beta1::API_VERSION, &raw.api_version, &raw.kind, path)?;
        raw
    } else if api_version == v1alpha1::API_VERSION {
        let raw: V1Alpha1Lab = parse_document(&text, path)?;
        validate::api_version_and_kind(&raw.api_version, &raw.kind, path)?;
        // Unreachable in practice — the check above already proved this
        // document's `apiVersion` and `kind`, which is the only
        // precondition migration has (see `migrate.rs`). Mapped rather
        // than unwrapped anyway: an unreachable branch that panics is
        // still a panic if the reasoning behind it ever stops holding.
        migrate_v1alpha1_to_v1beta1(raw)
            .map_err(|error| SpecError::validation(path, "apiVersion", error))?
    } else {
        return Err(unsupported_api_version(path, Some(&api_version)));
    };

    resolve::resolve_beta_lab(path.to_path_buf(), beta)
}

/// Reads and strictly parses the `admissionlab.yaml` configuration file at
/// `path` as an `admissionlab.io/v1alpha1` document.
///
/// "Strict" means: an unknown or misspelled key (`candiate` instead of
/// `candidate`) is rejected rather than silently ignored — every
/// user-facing struct in [`crate::model`] and [`crate::v1alpha1`] carries
/// `#[serde(deny_unknown_fields)]` — and `apiVersion`/`kind` are checked
/// against the only values this loader accepts
/// ([`v1alpha1::API_VERSION`], [`crate::model::KIND`]) immediately after
/// parsing.
///
/// This function only reads and parses; it does not resolve relative
/// paths or apply the semantic validation rules (empty Kubernetes
/// version, duplicate component names, and so on) — those are
/// [`crate::resolve_lab`]'s job, run against the [`LoadedLab`] this
/// returns.
///
/// **This loader is version-specific on purpose.** It returns the
/// as-written Alpha document, which is precisely why it cannot accept a
/// `v1beta1` file: there would be nothing coherent to put in
/// [`LoadedLab::raw`]. Use [`load_any_supported_lab`] to load whatever a
/// user actually wrote.
///
/// # Errors
///
/// Returns [`SpecError::Io`] if `path` cannot be read, [`SpecError::Parse`]
/// if its contents are not a syntactically valid `v1alpha1` document (the
/// error includes the serde path to the offending field — see
/// [`SpecError::Parse`]'s documentation), or [`SpecError::Validation`] if
/// `apiVersion` or `kind` do not match the expected values.
pub fn load_lab(path: &Path) -> Result<LoadedLab, SpecError> {
    let text = read_document(path)?;
    let raw: V1Alpha1Lab = parse_document(&text, path)?;

    validate::api_version_and_kind(&raw.api_version, &raw.kind, path)?;

    Ok(LoadedLab {
        source_path: path.to_path_buf(),
        raw,
    })
}

/// Reads `path`, tagging an I/O failure with the file that caused it.
fn read_document(path: &Path) -> Result<String, SpecError> {
    fs::read_to_string(path).map_err(|source| SpecError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Parses `text` as `T`, tagging a parse failure with the file that
/// caused it. `serde_norway`'s own error already carries the line,
/// column, and dotted path to the offending field.
fn parse_document<T: serde::de::DeserializeOwned>(text: &str, path: &Path) -> Result<T, SpecError> {
    serde_norway::from_str(text).map_err(|source| SpecError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// The error for a document this crate cannot read: no `apiVersion` at
/// all, or one naming a version outside [`SUPPORTED_API_VERSIONS`].
///
/// Lists every supported version rather than only the current one — a
/// user on an unsupported version needs to know both which one to write
/// now and that the older one would also have been accepted, since
/// "upgrade your config" and "you typo'd the version" are very different
/// problems with the same symptom.
fn unsupported_api_version(path: &Path, found: Option<&str>) -> SpecError {
    let supported = SUPPORTED_API_VERSIONS
        .iter()
        .map(|version| format!("{version:?}"))
        .collect::<Vec<_>>()
        .join(" or ");
    match found {
        Some(found) => SpecError::validation(
            path,
            "apiVersion",
            format_args!("must be {supported}, found {found:?}"),
        ),
        None => SpecError::validation(path, "apiVersion", format_args!("must be {supported}")),
    }
}
