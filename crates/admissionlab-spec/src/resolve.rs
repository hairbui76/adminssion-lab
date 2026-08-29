//! Turning a freshly loaded [`crate::LabSpec`] into a validated,
//! path-resolved [`ResolvedLab`].
//!
//! # Path resolution: always relative to the configuration file, never the
//! current working directory
//!
//! Every relative path this task resolves (`expectationsFile`, fixture
//! include patterns' root) is resolved against **the directory containing
//! the configuration file**, not against the process's current working
//! directory. Concretely: `resolve_lab` computes `config_dir` once, from
//! [`LoadedLab::source_path`]'s parent, and joins every relative path onto
//! that — it never calls [`std::env::current_dir`]. That is what makes
//! `admissionlab test ../lab/admissionlab.yaml`, run from some unrelated
//! directory, still find `../lab`'s own fixtures: `source_path` is used
//! exactly as given (never canonicalized), so if it is itself relative to
//! the current directory, `config_dir` — and everything resolved against
//! it — stays correctly relative to that same directory too, without ever
//! having to ask the process what that directory is.
//!
//! This does **not** yet reach every path in the document: a component's
//! `install` block (Helm `valuesFiles`, manifest `paths`) is carried
//! through unresolved inside [`ResolvedEnvironment::components`], because
//! [`ResolvedComponent`] is a deliberately minimal placeholder for Task
//! 2.1's full resolved component model (see that type's documentation).
//! Resolving those paths is part of the resolution that model performs.
//!
//! [`LoadedLab::raw`] keeps every path exactly as written (the *original*
//! form); [`ResolvedLab`]'s fields hold the joined, resolved form for the
//! paths this task does resolve. Keeping both stages as distinct values
//! (rather than resolving in place) is what lets a caller retain the
//! as-written original for diagnostics alongside the resolved form it
//! actually acts on.

use std::path::{Path, PathBuf};

use globset::Glob;

use crate::error::SpecError;
use crate::model::{
    ComponentSpec, EnvironmentSpec, FixtureSelectionSpec, GatewaySuiteSpec, LabSpec,
    MigrationSuiteSpec, PolicySpec,
};
use crate::validate;

/// A [`LabSpec`] freshly parsed from a configuration file, paired with the
/// path it was loaded from.
///
/// `raw` is exactly what [`crate::load_lab`] parsed: relative paths inside
/// it have **not** been resolved against the configuration file's
/// directory yet. [`resolve_lab`] is what performs that resolution (see
/// this module's documentation) and validation, producing a
/// [`ResolvedLab`].
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedLab {
    /// The path [`crate::load_lab`] was called with, exactly as given —
    /// not canonicalized. Relative paths found inside `raw` are resolved
    /// against this path's parent directory, whatever form it takes.
    pub source_path: PathBuf,
    /// The parsed configuration, with every path exactly as the user
    /// wrote it.
    pub raw: LabSpec,
}

/// A [`LabSpec`] that has been validated and had every relative path
/// resolved against its configuration file's own directory.
///
/// `policy` is carried through unchanged from [`LoadedLab::raw`]: none of
/// [`crate::PolicySpec`]'s fields are filesystem paths (its `path` field
/// on [`crate::PolicyOverrideSpec`] names a location *within* a compared
/// object, not on disk), so there is nothing for this step to resolve.
///
/// `gateway` and `migration` are always `None` today: [`LabSpec`] has no
/// YAML section feeding them yet. See [`crate::GatewaySuiteSpec`] and
/// [`crate::MigrationSuiteSpec`] for why the fields exist regardless.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLab {
    /// The path this configuration was loaded from, exactly as
    /// [`crate::load_lab`] was called with — carried through from
    /// [`LoadedLab::source_path`].
    pub source_path: PathBuf,
    /// The resolved baseline environment.
    pub baseline: ResolvedEnvironment,
    /// The resolved candidate environment.
    pub candidate: ResolvedEnvironment,
    /// The resolved fixture selection.
    pub fixtures: ResolvedFixtureSelection,
    /// The regression policy, unchanged from [`LoadedLab::raw`].
    pub policy: PolicySpec,
    /// The expectations file path, resolved against the configuration
    /// file's directory if it was written as a relative path.
    pub expectations_file: Option<PathBuf>,
    /// Reserved for Phase 6's gateway conformance suite; always `None`
    /// until that phase adds a YAML section to populate it.
    pub gateway: Option<GatewaySuiteSpec>,
    /// Reserved for Phase 6's migration test suite; always `None` until
    /// that phase adds a YAML section to populate it.
    pub migration: Option<MigrationSuiteSpec>,
}

/// One resolved side (baseline or candidate) of a comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEnvironment {
    /// The Kubernetes version to provision. Never empty, and trimmed of
    /// surrounding whitespace — validated by [`resolve_lab`].
    pub kubernetes: String,
    /// The environment's resolved components. Names are unique within
    /// this list — validated by [`resolve_lab`].
    pub components: Vec<ResolvedComponent>,
}

/// One resolved component within a [`ResolvedEnvironment`].
///
/// Deliberately minimal: only the resolved `name`, which is all this task
/// needs to validate uniqueness. Task 2.1 owns the full resolved
/// component model (recipe resolution, the resolved install method, and
/// so on) and is expected to grow this type rather than introduce a
/// competing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedComponent {
    /// The component's name, unique within its environment.
    pub name: String,
}

/// A resolved fixture selection: compiled glob patterns plus the
/// directory they are matched relative to.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedFixtureSelection {
    /// The compiled include patterns, in the order they were written.
    /// Never empty — validated by [`resolve_lab`].
    pub include: Vec<Glob>,
    /// The directory `include` patterns are matched against: the
    /// configuration file's own directory. Fixture discovery (a later
    /// task) is what actually walks this directory; this step only
    /// records where that walk should start.
    pub root: PathBuf,
}

/// Validates `loaded` and resolves every relative path it contains against
/// its configuration file's own directory (see this module's
/// documentation for why the current working directory is never used).
///
/// Consumes `loaded` by value. A caller that wants both the as-written
/// original *and* the resolved form (see this module's documentation on
/// "preserve both the original and resolved paths for diagnostics")
/// should `loaded.clone()` before calling this function, since
/// [`LoadedLab`] implements [`Clone`].
///
/// # Errors
///
/// Returns [`SpecError::Validation`] if the baseline or candidate
/// Kubernetes version is empty, if a component is missing a name, if two
/// components in the same environment share a name, or if the fixture
/// include list is empty. Returns [`SpecError::InvalidGlob`] if a fixture
/// include pattern is not a syntactically valid glob.
pub fn resolve_lab(loaded: LoadedLab) -> Result<ResolvedLab, SpecError> {
    let LoadedLab { source_path, raw } = loaded;
    let config_dir = config_directory(&source_path);

    let baseline = resolve_environment("baseline", &raw.baseline, &source_path)?;
    let candidate = resolve_environment("candidate", &raw.candidate, &source_path)?;
    let fixtures = resolve_fixtures(&raw.fixtures, &config_dir, &source_path)?;
    let expectations_file = raw
        .expectations_file
        .map(|path| resolve_relative(&config_dir, path));

    Ok(ResolvedLab {
        source_path,
        baseline,
        candidate,
        fixtures,
        policy: raw.policy,
        expectations_file,
        gateway: None,
        migration: None,
    })
}

/// The directory every relative path originating in `source_path`'s
/// configuration must be joined against.
///
/// **Invariant every caller in this crate must preserve:** any path that
/// came from the configuration file — whether resolved today (fixtures
/// root, `expectationsFile`) or still pending (a component's `install`
/// block: `HelmInstallSpec::values_files`, `ManifestsInstallSpec::paths`,
/// left unresolved by this task, see [`ResolvedComponent`]'s
/// documentation) — must be joined against *this* directory and never
/// against [`std::env::current_dir`]. `pub(crate)` rather than private so
/// Task 2.1's resolved component model, the next consumer of this
/// invariant, reuses this function instead of rediscovering (or
/// mis-deriving) it when it starts opening `install`-block paths for
/// `helm install -f`.
///
/// Deliberately never touches [`std::env::current_dir`] itself: this is
/// the one place that property is enforced, so every caller (all pure
/// path joins) automatically inherits it.
pub(crate) fn config_directory(source_path: &Path) -> PathBuf {
    match source_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        // `source_path` was a bare filename ("admissionlab.yaml") or had
        // no parent component at all: both cases mean "resolve relative
        // paths the same way the bare filename itself would be" — i.e.
        // relative to the current directory, expressed here as `.` rather
        // than by calling `current_dir()` so this function stays pure.
        _ => PathBuf::from("."),
    }
}

/// Joins `path` onto `config_dir` if `path` is relative; returns `path`
/// unchanged if it is already absolute.
fn resolve_relative(config_dir: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        config_dir.join(path)
    }
}

fn resolve_environment(
    field: &str,
    raw: &EnvironmentSpec,
    source_path: &Path,
) -> Result<ResolvedEnvironment, SpecError> {
    let kubernetes = validate::kubernetes_version(field, &raw.kubernetes, source_path)?;

    let components = raw
        .components
        .iter()
        .enumerate()
        .map(|(index, component)| resolve_component(field, index, component, source_path))
        .collect::<Result<Vec<_>, _>>()?;
    validate::unique_component_names(field, &components, source_path)?;

    Ok(ResolvedEnvironment {
        kubernetes,
        components,
    })
}

fn resolve_component(
    field: &str,
    index: usize,
    raw: &ComponentSpec,
    source_path: &Path,
) -> Result<ResolvedComponent, SpecError> {
    let name = validate::require_component_name(field, index, raw.name.as_deref(), source_path)?;
    Ok(ResolvedComponent { name })
}

fn resolve_fixtures(
    raw: &FixtureSelectionSpec,
    config_dir: &Path,
    source_path: &Path,
) -> Result<ResolvedFixtureSelection, SpecError> {
    validate::fixture_include_nonempty(&raw.include, source_path)?;

    let include = raw
        .include
        .iter()
        .map(|pattern| {
            Glob::new(pattern).map_err(|source| SpecError::InvalidGlob {
                path: source_path.to_path_buf(),
                pattern: pattern.clone(),
                source,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ResolvedFixtureSelection {
        include,
        root: config_dir.to_path_buf(),
    })
}
