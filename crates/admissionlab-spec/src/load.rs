//! Strict YAML loading of an `admissionlab.yaml` configuration file.

use std::fs;
use std::path::Path;

use crate::error::SpecError;
use crate::model::LabSpec;
use crate::resolve::LoadedLab;
use crate::validate;

/// Reads and strictly parses the `admissionlab.yaml` configuration file at
/// `path`.
///
/// "Strict" means: an unknown or misspelled key (`candiate` instead of
/// `candidate`) is rejected rather than silently ignored — every
/// user-facing struct in [`crate::model`] carries
/// `#[serde(deny_unknown_fields)]` — and `apiVersion`/`kind` are checked
/// against the only values this loader accepts
/// ([`crate::model::API_VERSION`], [`crate::model::KIND`]) immediately
/// after parsing.
///
/// This function only reads and parses; it does not resolve relative
/// paths or apply the semantic validation rules (empty Kubernetes
/// version, duplicate component names, and so on) — those are
/// [`crate::resolve_lab`]'s job, run against the [`LoadedLab`] this
/// returns.
///
/// # Errors
///
/// Returns [`SpecError::Io`] if `path` cannot be read, [`SpecError::Parse`]
/// if its contents are not a syntactically valid `LabSpec` document (the
/// error includes the serde path to the offending field — see
/// [`SpecError::Parse`]'s documentation), or [`SpecError::Validation`] if
/// `apiVersion` or `kind` do not match the expected values.
pub fn load_lab(path: &Path) -> Result<LoadedLab, SpecError> {
    let text = fs::read_to_string(path).map_err(|source| SpecError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let raw: LabSpec = serde_norway::from_str(&text).map_err(|source| SpecError::Parse {
        path: path.to_path_buf(),
        source,
    })?;

    validate::api_version_and_kind(&raw.api_version, &raw.kind, path)?;

    Ok(LoadedLab {
        source_path: path.to_path_buf(),
        raw,
    })
}
