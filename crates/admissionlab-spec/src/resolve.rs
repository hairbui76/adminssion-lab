//! Turning a freshly loaded lab document into a validated, path-resolved
//! [`ResolvedLab`].
//!
//! # One resolver, every version
//!
//! [`resolve_v1_lab`] is the only implementation. An `admissionlab.io/v1`
//! document reaches it directly from [`crate::load_any_supported_lab`]; a
//! `v1beta1` document reaches it through
//! [`crate::migrate_v1beta1_to_v1`] and a `v1alpha1` document through
//! both migrations in turn, whether they arrived via that same loader or
//! via [`resolve_lab`]'s [`LoadedLab`]. Nothing here branches on a
//! version, and [`ResolvedLab`] records none — see its own
//! "Version-independent by construction".
//!
//! # Path resolution: always relative to the configuration file, never the
//! current working directory
//!
//! Every relative path in the document (`expectationsFile`, fixture
//! include patterns' root, and — since Task 2.1 — a component's `install`
//! block: [`crate::component::HelmInstallSpec::values_files`],
//! [`crate::component::ManifestInstallSpec::paths`]) is resolved against
//! **the directory containing the configuration file**, not against the
//! process's current working directory. Concretely: `resolve_lab`
//! computes `config_dir` once, from [`LoadedLab::source_path`]'s parent,
//! and joins every relative path onto that (via [`resolve_relative`]) —
//! it never calls [`std::env::current_dir`]. That is what makes
//! `admissionlab test ../lab/admissionlab.yaml`, run from some unrelated
//! directory, still find `../lab`'s own fixtures: `source_path` is used
//! exactly as given (never canonicalized), so if it is itself relative to
//! the current directory, `config_dir` — and everything resolved against
//! it — stays correctly relative to that same directory too, without ever
//! having to ask the process what that directory is.
//!
//! [`LoadedLab::raw`] keeps every path exactly as written (the *original*
//! form); [`ResolvedLab`]'s fields hold the joined, resolved form for
//! every path. Keeping both stages as distinct values (rather than
//! resolving in place) is what lets a caller retain the as-written
//! original for diagnostics alongside the resolved form it actually acts
//! on.

use std::path::{Path, PathBuf};

use globset::Glob;

use crate::component::{self, ResolvedComponent};
use crate::error::SpecError;
use crate::migrate::{migrate_v1alpha1_to_v1beta1, migrate_v1beta1_to_v1};
use crate::model::{
    EnvironmentSpec, FixtureSelectionSpec, GatewaySuiteSpec, MigrationCaseSpec, MigrationSuiteSpec,
    PolicySpec,
};
use crate::v1::V1Lab;
use crate::v1alpha1::V1Alpha1Lab;
use crate::validate;

/// A [`V1Alpha1Lab`] freshly parsed from a configuration file, paired
/// with the path it was loaded from.
///
/// `raw` is exactly what [`crate::load_lab`] parsed: relative paths inside
/// it have **not** been resolved against the configuration file's
/// directory yet. [`resolve_lab`] is what performs that resolution (see
/// this module's documentation) and validation, producing a
/// [`ResolvedLab`].
///
/// This is the *Alpha* document, because [`crate::load_lab`] is the Alpha
/// loader — there is deliberately no `LoadedLab` for a `v1beta1`
/// document, since [`crate::load_any_supported_lab`] resolves in one
/// step and nothing needs an unresolved Beta document to hold on to.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedLab {
    /// The path [`crate::load_lab`] was called with, exactly as given —
    /// not canonicalized. Relative paths found inside `raw` are resolved
    /// against this path's parent directory, whatever form it takes.
    pub source_path: PathBuf,
    /// The parsed configuration, with every path exactly as the user
    /// wrote it.
    pub raw: V1Alpha1Lab,
}

/// A lab configuration that has been validated and had every relative
/// path resolved against its configuration file's own directory.
///
/// # Version-independent by construction
///
/// This type carries no `apiVersion` and no version marker of any kind,
/// and that is a design commitment rather than an omission. Every
/// supported document version is migrated to [`V1Lab`] *before*
/// resolution (see [`crate::load_any_supported_lab`] and `migrate.rs`),
/// so there is exactly one resolver, one resolved shape, and one thing
/// for the whole workspace above this crate to consume. Adding a version
/// adds a parse arm and a migration; it does not add a branch to this
/// module, a variant to this struct, or a single `match` anywhere in
/// `admissionlab-core`, `-cli`, or the report. The fields typed
/// [`PolicySpec`] and [`GatewaySuiteSpec`] are the *current* version's
/// types for exactly that reason: "current" is the only version that
/// reaches this far.
///
/// `policy` is carried through unchanged from [`LoadedLab::raw`]: none of
/// [`crate::PolicySpec`]'s fields are filesystem paths (its `path` field
/// on [`crate::PolicyOverrideSpec`] names a location *within* a compared
/// object, not on disk), so there is nothing for this step to resolve.
///
/// `gateway` is real as of ROADMAP Task 6.1 and `migration` as of Task
/// 8.3; each is `Some` exactly when the document has the corresponding
/// top-level section. A `v1alpha1` document always resolves to
/// `migration: None`, because that version has no `migration:` key at
/// all — see [`crate::V1Lab::migration`].
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
    /// The Gateway behavior suite, if the document declared one, with
    /// every [`GatewaySuiteSpec::manifests`] path resolved against the
    /// configuration file's own directory. `None` for an admission-only
    /// lab. See [`GatewaySuiteSpec`] for why the raw and resolved
    /// stages share one type.
    pub gateway: Option<GatewaySuiteSpec>,
    /// The Ingress-to-Gateway migration suite, if the document declared
    /// one, with every case's baseline and candidate manifest path
    /// resolved against the configuration file's own directory. `None`
    /// for a lab that is not migrating off `Ingress` — and always `None`
    /// for a `v1alpha1` document, which has no such section. See
    /// [`MigrationSuiteSpec`] for why the raw and resolved stages share
    /// one type.
    pub migration: Option<MigrationSuiteSpec>,
}

/// One resolved side (baseline or candidate) of a comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEnvironment {
    /// The Kubernetes version to provision. Never empty, and trimmed of
    /// surrounding whitespace — validated by [`resolve_lab`].
    pub kubernetes: String,
    /// Local container images to side-load into this side's cluster
    /// before anything is installed. Each entry is non-empty and
    /// trimmed; the list is empty for a lab that names none. See
    /// [`crate::EnvironmentSpec::images`].
    pub images: Vec<String>,
    /// The environment's resolved components. Names are unique within
    /// this list — validated by [`resolve_lab`].
    pub components: Vec<ResolvedComponent>,
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
/// components in the same environment share a name, if the fixture
/// include list is empty, if a component has no `install` method, if a
/// Helm install has no repository or no exact pinned chart version (see
/// [`crate::validate::require_pinned_helm_version`]), or if a component
/// has no way to resolve a version (no explicit top-level version, and an
/// install method — a manifests install — that provides no implicit
/// one). Returns [`SpecError::InvalidGlob`] if a fixture include pattern
/// is not a syntactically valid glob.
pub fn resolve_lab(loaded: LoadedLab) -> Result<ResolvedLab, SpecError> {
    let LoadedLab { source_path, raw } = loaded;
    // One resolver, not one per version: the Alpha document is promoted
    // to the current model first and then follows the identical path a
    // natively-Beta document does. See `ResolvedLab`'s
    // "Version-independent by construction".
    let beta = migrate_v1alpha1_to_v1beta1(raw)
        .map_err(|error| SpecError::validation(&source_path, "apiVersion", error))?;
    let stable = migrate_v1beta1_to_v1(beta)
        .map_err(|error| SpecError::validation(&source_path, "apiVersion", error))?;
    resolve_v1_lab(source_path, stable)
}

/// Validates and resolves a current-version document — the single
/// implementation both [`resolve_lab`] and
/// [`crate::load_any_supported_lab`] funnel into.
///
/// `pub(crate)` rather than public: a caller outside this crate should
/// not have to know which version it holds, and
/// [`crate::load_any_supported_lab`] exists precisely so it never does.
///
/// # Errors
///
/// See [`resolve_lab`], whose documented failures are all raised here.
pub(crate) fn resolve_v1_lab(source_path: PathBuf, raw: V1Lab) -> Result<ResolvedLab, SpecError> {
    let config_dir = config_directory(&source_path);

    let baseline = resolve_environment("baseline", &raw.baseline, &config_dir, &source_path)?;
    let candidate = resolve_environment("candidate", &raw.candidate, &config_dir, &source_path)?;
    let fixtures = resolve_fixtures(&raw.fixtures, &config_dir, &source_path)?;
    let expectations_file = raw
        .expectations_file
        .map(|path| resolve_relative(&config_dir, path));
    let gateway = raw
        .gateway
        .map(|gateway| resolve_gateway(gateway, &config_dir, &source_path))
        .transpose()?;
    let migration = raw
        .migration
        .map(|migration| resolve_migration(migration, &config_dir, &source_path))
        .transpose()?;

    Ok(ResolvedLab {
        source_path,
        baseline,
        candidate,
        fixtures,
        policy: raw.policy,
        expectations_file,
        gateway,
        migration,
    })
}

/// The directory every relative path originating in `source_path`'s
/// configuration must be joined against.
///
/// **Invariant every caller in this crate must preserve:** any path that
/// came from the configuration file — including a component's `install`
/// block ([`crate::component::HelmInstallSpec::values_files`],
/// [`crate::component::ManifestInstallSpec::paths`], resolved by
/// [`crate::component::resolve_component`] via [`resolve_relative`]) —
/// must be joined against *this* directory and never against
/// [`std::env::current_dir`]. `pub(crate)` rather than private so every
/// module in this crate that resolves a path reuses this function
/// instead of rediscovering (or mis-deriving) the invariant.
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
///
/// `pub(crate)` so every module in this crate that resolves a
/// configuration-file-relative path (currently this module and
/// [`crate::component`]) shares one implementation rather than
/// duplicating it.
pub(crate) fn resolve_relative(config_dir: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        config_dir.join(path)
    }
}

fn resolve_environment(
    field: &str,
    raw: &EnvironmentSpec,
    config_dir: &Path,
    source_path: &Path,
) -> Result<ResolvedEnvironment, SpecError> {
    let kubernetes = validate::kubernetes_version(field, &raw.kubernetes, source_path)?;
    let images = validate::environment_images(field, &raw.images, source_path)?;

    let components = raw
        .components
        .iter()
        .enumerate()
        .map(|(index, comp)| {
            component::resolve_component(field, index, comp, config_dir, source_path)
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate::unique_component_names(field, &components, source_path)?;

    Ok(ResolvedEnvironment {
        kubernetes,
        images,
        components,
    })
}

/// Validates a [`GatewaySuiteSpec`] and resolves its manifest paths
/// against the configuration file's own directory (ROADMAP Task 6.1).
///
/// Consumes `raw` and returns the same type with `manifests` rewritten —
/// see [`GatewaySuiteSpec`]'s "One type for both the raw and the
/// resolved stage" section for why there is no separate resolved twin.
/// Validation runs *before* path resolution so an error message quotes
/// the manifest list the user actually wrote rather than a joined
/// absolute path they never typed.
fn resolve_gateway(
    raw: GatewaySuiteSpec,
    config_dir: &Path,
    source_path: &Path,
) -> Result<GatewaySuiteSpec, SpecError> {
    validate::gateway_suite(&raw, source_path)?;

    let GatewaySuiteSpec {
        manifests,
        routes,
        reconciliation_timeout,
        gateway_endpoint,
        readiness,
    } = raw;
    Ok(GatewaySuiteSpec {
        manifests: manifests
            .into_iter()
            .map(|path| resolve_relative(config_dir, path))
            .collect(),
        routes,
        reconciliation_timeout,
        gateway_endpoint,
        readiness,
    })
}

/// Validates a [`MigrationSuiteSpec`] and resolves both of every case's
/// manifest path lists against the configuration file's own directory
/// (ROADMAP Task 8.3).
///
/// Structurally the twin of [`resolve_gateway`], deliberately: the two
/// sections carry manifest paths written the same way, so they are
/// resolved by the same rule ([`resolve_relative`], never
/// [`std::env::current_dir`]) in the same order — validate first, so a
/// message quotes the list the user wrote rather than a joined absolute
/// path they never typed, then join.
///
/// Both lists in a case are resolved, and nothing else in a case is
/// touched: [`crate::HttpProbeContract`] and
/// [`crate::NonPortableFeatureExpectation`] contain no filesystem paths
/// at all.
fn resolve_migration(
    raw: MigrationSuiteSpec,
    config_dir: &Path,
    source_path: &Path,
) -> Result<MigrationSuiteSpec, SpecError> {
    validate::migration_suite(&raw, source_path)?;

    let MigrationSuiteSpec {
        cases,
        baseline,
        candidate,
    } = raw;
    let cases = cases
        .into_iter()
        .map(|case| {
            // Exhaustive destructure, as everywhere else in this module:
            // a field added to `MigrationCaseSpec` that also needs
            // resolving must be a compile error here rather than a value
            // silently carried through unresolved.
            let MigrationCaseSpec {
                id,
                baseline_ingress_manifests,
                candidate_gateway_manifests,
                probes,
                expected_nonportable,
            } = case;
            MigrationCaseSpec {
                id,
                baseline_ingress_manifests: baseline_ingress_manifests
                    .into_iter()
                    .map(|path| resolve_relative(config_dir, path))
                    .collect(),
                candidate_gateway_manifests: candidate_gateway_manifests
                    .into_iter()
                    .map(|path| resolve_relative(config_dir, path))
                    .collect(),
                probes,
                expected_nonportable,
            }
        })
        .collect();

    // The two per-side blocks carry no filesystem path -- a
    // `GatewayEndpointSpec` is a namespace, a name or a selector, and a
    // port -- so they are carried through unchanged, exactly as
    // `resolve_gateway` carries the Gateway suite's own
    // `gateway_endpoint` through. Their *content* is validated where
    // every other endpoint block's is, by
    // `crate::resolve_gateway_endpoint`, at the moment a runner turns
    // one into a strategy.
    Ok(MigrationSuiteSpec {
        cases,
        baseline,
        candidate,
    })
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
