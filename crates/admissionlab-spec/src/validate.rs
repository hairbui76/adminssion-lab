//! Semantic validation rules applied while resolving a [`crate::LabSpec`].
//!
//! Every function here is a small, independently testable predicate over
//! already-parsed data; [`crate::resolve_lab`] (in `resolve.rs`) is what
//! sequences them while building a [`crate::ResolvedLab`]. Kept separate
//! from `resolve.rs` so each rule reads as a single, named check rather
//! than being folded into the resolution walk.
//!
//! Every rejection uses [`crate::error::SpecError::validation`], which
//! prefixes the message with a dotted locator into the document (for
//! example `baseline.kubernetes: ...`) — the same convention
//! `serde_norway`'s own parse errors follow, so a validation failure and a
//! parse failure read consistently.

use std::path::Path;

use crate::error::SpecError;
use crate::resolve::ResolvedComponent;

/// Rejects an empty (or all-whitespace) Kubernetes version; otherwise
/// returns it trimmed of surrounding whitespace, mirroring
/// [`require_component_name`]'s trim-and-return shape so a padded
/// Kubernetes version (`" 1.29.4 "`) doesn't keep its padding in
/// [`crate::ResolvedEnvironment::kubernetes`] the way a padded component
/// name wouldn't either.
///
/// `field` is the dotted locator of the enclosing environment (`baseline`
/// or `candidate`), used to build the error's locator prefix.
pub(crate) fn kubernetes_version(
    field: &str,
    version: &str,
    path: &Path,
) -> Result<String, SpecError> {
    let trimmed = version.trim();
    if trimmed.is_empty() {
        return Err(SpecError::validation(
            path,
            format_args!("{field}.kubernetes"),
            "must not be empty",
        ));
    }
    Ok(trimmed.to_owned())
}

/// Requires a component to have a non-empty resolved name, returning it.
///
/// A component's `name` field is optional in YAML (a later task's recipe
/// system may eventually derive one), but resolution has no recipe system
/// to fall back on yet, so a name is required for now — without one,
/// [`unique_component_names`] would have nothing meaningful to compare.
pub(crate) fn require_component_name(
    field: &str,
    index: usize,
    name: Option<&str>,
    path: &Path,
) -> Result<String, SpecError> {
    match name.map(str::trim) {
        Some(name) if !name.is_empty() => Ok(name.to_owned()),
        _ => Err(SpecError::validation(
            path,
            format_args!("{field}.components[{index}]"),
            "name must not be empty (recipe-derived naming is not yet implemented)",
        )),
    }
}

/// Rejects a duplicate component name within one environment's resolved
/// component list.
///
/// Scoped to a single environment deliberately: baseline and candidate
/// are expected to install components under the *same* name (that is how
/// they are paired for comparison), so checking across both environments
/// would reject the tool's own normal use case.
pub(crate) fn unique_component_names(
    field: &str,
    components: &[ResolvedComponent],
    path: &Path,
) -> Result<(), SpecError> {
    let mut seen = std::collections::BTreeSet::new();
    for component in components {
        if !seen.insert(component.name.as_str()) {
            return Err(SpecError::validation(
                path,
                format_args!("{field}.components"),
                format_args!("duplicate component name {:?}", component.name),
            ));
        }
    }
    Ok(())
}

/// Rejects an empty fixture include list.
pub(crate) fn fixture_include_nonempty(include: &[String], path: &Path) -> Result<(), SpecError> {
    if include.is_empty() {
        return Err(SpecError::validation(
            path,
            "fixtures.include",
            "must not be empty",
        ));
    }
    Ok(())
}

/// Rejects a [`crate::LabSpec`] whose `apiVersion`/`kind` do not match the
/// only values [`crate::load_lab`] accepts.
pub(crate) fn api_version_and_kind(
    api_version: &str,
    kind: &str,
    path: &Path,
) -> Result<(), SpecError> {
    if api_version != crate::model::API_VERSION {
        return Err(SpecError::validation(
            path,
            "apiVersion",
            format_args!(
                "must be {:?}, found {api_version:?}",
                crate::model::API_VERSION
            ),
        ));
    }
    if kind != crate::model::KIND {
        return Err(SpecError::validation(
            path,
            "kind",
            format_args!("must be {:?}, found {kind:?}", crate::model::KIND),
        ));
    }
    Ok(())
}
