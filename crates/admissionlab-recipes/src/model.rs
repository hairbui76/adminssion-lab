//! The recipe document schema: the raw, as-written YAML shape a recipe
//! author writes, the strict allow-list that shape enforces, and
//! [`resolve_recipe`], which turns a parsed, raw document into a fully
//! validated [`Recipe`].
//!
//! # The allow-list is the enforcement mechanism, not a keyword blocklist
//!
//! PRODUCT.md §14 / Global Constraint 6: **a recipe must not contain
//! regression-classification business logic** — deciding *which*
//! behavioral differences count as a regression is the core engine's job
//! (`admissionlab-diff`, `admissionlab-policy`), never a recipe's. This
//! module enforces that by construction, not by scanning for specific
//! forbidden names:
//!
//! - Every raw struct below carries `#[serde(deny_unknown_fields)]`. A
//!   recipe document may contain *only* the fields this module declares
//!   — nothing else parses, at any nesting level. `failOn` and
//!   `severity` (the two examples PRODUCT.md §14 and this task's brief
//!   name explicitly) are rejected exactly the same way an invented
//!   name never mentioned anywhere — `classification`, `riskLevel`,
//!   `ignoreRegression`, `blocking` — would be: none of them is a field
//!   this schema defines, so all of them are an "unknown field" parse
//!   error. This is deliberately an allow-list, not a denylist: a
//!   denylist of specific forbidden names is only ever as complete as
//!   the list of names someone thought to write down, and a vendor
//!   determined to smuggle in a classification signal would simply pick
//!   a name not on it. An allow-list has no such gap — *only* the fields
//!   named below ever reach a [`Recipe`], regardless of what a document
//!   calls anything else.
//! - [`RawReadinessCheck`] and [`RawNormalizeRule`] are internally
//!   tagged enums whose variant sets are closed and mirror
//!   [`admissionlab_spec::ReadinessCheck`] /
//!   [`admissionlab_spec::RecipeNormalizeRule`] exactly — both of which
//!   are themselves closed enums *owned by `admissionlab-spec`*, not
//!   this crate (Controller Ruling R30). A recipe cannot invent a new
//!   normalize-rule "type" that smuggles in a semantic judgment call
//!   disguised as normalization (for example a hypothetical
//!   `treatAsEquivalent`/`ignoreDifference` rule that decides a
//!   difference is expected rather than mechanically transforming it) —
//!   every existing variant is a pure, mechanical operation (remove a
//!   value, remove an annotation, sort an array) with no notion of
//!   "regression" attached, and an invented variant name is simply an
//!   unrecognized enum tag, rejected the same way an unknown `failOn`
//!   field is.
//! - [`crate::capability::parse_capability`] is the same allow-list
//!   principle applied to `capabilities:`'s string *values* rather than
//!   struct field *names*: only the three spellings
//!   [`admissionlab_spec::Capability`] actually has variants for are
//!   accepted, so a capability string cannot be repurposed as a
//!   classification flag either.
//!
//! What is *not* attempted here: a secondary keyword-blocklist scan
//! layered on top of the allow-list above. Every field in this schema is
//! a closed type (a required/optional `String`, a `PathBuf`, or a closed
//! enum) — there is no free-form map anywhere a key could hide from
//! `deny_unknown_fields` the way it could inside, say, a hypothetical
//! opaque `diagnosticHints: BTreeMap<String, String>` field. PRODUCT.md
//! §14 lists "diagnostic hints" among what a recipe may contain, but
//! this task does not implement that field (YAGNI — nothing in this
//! task's brief interface needs it, and [`Recipe`] below matches that
//! interface exactly); whichever later task adds it must give it the
//! same scrutiny this comment describes, since a free-form map is
//! exactly the shape that could hide a `failOn` key from
//! `deny_unknown_fields`.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;

use admissionlab_spec::component::HelmInstallSpec;
use admissionlab_spec::{
    Capability, InstallMethod, ManifestInstallSpec, ReadinessCheck, RecipeNormalizeRule,
};
use serde::Deserialize;
use thiserror::Error;

use crate::capability::parse_capability;

// ---------------------------------------------------------------------
// Resolved shape: what every loader in this crate produces
// ---------------------------------------------------------------------

/// A fully validated, ready-to-use recipe: curated convenience metadata
/// for installing a known admission-stack component (see this crate's
/// own `lib.rs` module documentation and PRODUCT.md §14).
///
/// Every field here is exactly what a caller needs and nothing more —
/// see this module's documentation for why a recipe cannot contain
/// anything beyond these fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    /// The recipe's name — for example `"kyverno"`. Unique within
    /// whatever set of recipes a caller has loaded (built-in plus
    /// override; see [`crate::load::load_recipes`]'s documentation for
    /// how a name collision between the two is handled).
    pub name: String,
    /// The recipe's version — for example a pinned Helm chart version
    /// such as `"3.9.0"`. Distinct from
    /// [`admissionlab_spec::ComponentSpec::version`]: this is the
    /// *recipe's own* version, not necessarily the version a lab file
    /// that references this recipe ends up installing.
    pub version: String,
    /// How this recipe installs its component.
    pub install: InstallMethod,
    /// Readiness checks to wait on after installing.
    pub readiness: Vec<ReadinessCheck>,
    /// Known harmless response normalization rules this recipe
    /// contributes.
    pub normalize_rules: Vec<RecipeNormalizeRule>,
    /// Which admission-related capabilities this recipe's component
    /// provides.
    pub capabilities: BTreeSet<Capability>,
}

// ---------------------------------------------------------------------
// Failure modes
// ---------------------------------------------------------------------

/// Something went wrong loading, parsing, or validating a [`Recipe`].
#[derive(Debug, Error)]
pub enum RecipeError {
    /// A recipe file could not be read from disk.
    ///
    /// Only ever returned by [`crate::load::load_recipe_overrides`] (and
    /// transitively, [`crate::load::load_recipes`]) — a built-in
    /// recipe's text is embedded into the compiled binary at build time
    /// ([`crate::load::load_builtin_recipes`]), so reading it can never
    /// fail at runtime. This also covers an override directory that does
    /// not exist at all: a caller that explicitly names a directory and
    /// gets this back has a real, actionable problem (a typo'd path, a
    /// directory that was never created), which is why a missing
    /// directory is a loud error here rather than treated the same as
    /// "no override requested" — that distinction is the caller's to
    /// make (by choosing whether to call this function at all), not this
    /// function's to paper over.
    #[error("failed to read recipe file at {}: {source}", path.display())]
    Io {
        /// The path that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
    },

    /// A recipe document's contents are not valid YAML matching the
    /// recipe schema — including, deliberately, any key this schema does
    /// not define. See this module's own documentation for why that is
    /// the actual enforcement mechanism behind PRODUCT.md §14's "no
    /// regression-classification logic" rule, not a separate check.
    #[error("failed to parse recipe {source_label}: {source}")]
    Parse {
        /// Identifies which recipe document failed: a built-in's
        /// embedded label (its checked-in repository-relative path, used
        /// purely as a diagnostic name, e.g.
        /// `"recipes/kyverno/recipe.yaml"`) or an override recipe's real
        /// file path, displayed. A plain
        /// `String` rather than a `PathBuf` because a built-in recipe
        /// has no real filesystem location at all — see
        /// [`crate::load`]'s module documentation.
        source_label: String,
        /// The underlying parse failure, including YAML location and
        /// (where applicable) the serde path to the offending field.
        #[source]
        source: serde_norway::Error,
    },

    /// A recipe document parsed successfully but fails a semantic
    /// validation rule (for example an empty name, a non-pinned Helm
    /// chart version, or an unrecognized capability string).
    #[error("{source_label}: {message}")]
    Validation {
        /// See [`RecipeError::Parse::source_label`].
        source_label: String,
        /// A message starting with a dotted locator into the document,
        /// for example `install.version: ...`.
        message: String,
    },

    /// Two recipe files in the same override directory both declare the
    /// same [`Recipe::name`].
    ///
    /// Unlike a built-in/override name collision (an override recipe
    /// replacing a built-in one — an intentional, well-defined act; see
    /// [`crate::load::load_recipes`]'s documentation), nothing orders
    /// two files within the *same* directory the caller controls
    /// directly, so this is rejected outright rather than silently
    /// picking a winner by file name or load order.
    #[error(
        "duplicate recipe name {name:?} in override directory: both {} and {} declare it",
        .first.display(), .second.display()
    )]
    DuplicateOverrideName {
        /// The recipe name both files declare.
        name: String,
        /// The first file (in sorted order) declaring `name`.
        first: PathBuf,
        /// The second file (in sorted order) declaring `name`.
        second: PathBuf,
    },
}

impl RecipeError {
    /// Builds a [`RecipeError::Validation`] whose message is prefixed
    /// with `locator` (for example `"install.version"`), matching the
    /// dotted-path convention [`RecipeError::Parse`] gets for free from
    /// `serde_norway` — the same convention
    /// `admissionlab_spec::error::SpecError::validation` follows for the
    /// same reason.
    fn validation(
        source_label: &str,
        locator: impl std::fmt::Display,
        message: impl std::fmt::Display,
    ) -> Self {
        Self::Validation {
            source_label: source_label.to_owned(),
            message: format!("{locator}: {message}"),
        }
    }
}

// ---------------------------------------------------------------------
// Raw shape: exactly what a recipe YAML document may contain
// ---------------------------------------------------------------------

/// The raw recipe document shape, exactly as written in YAML. See this
/// module's documentation for why every field here — and no other — is
/// what a recipe document may contain.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RawRecipe {
    pub name: String,
    pub version: String,
    pub install: RawInstallMethod,
    #[serde(default)]
    pub readiness: Vec<RawReadinessCheck>,
    #[serde(default)]
    pub normalize_rules: Vec<RawNormalizeRule>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// The raw shape of [`RawRecipe::install`].
///
/// Represented as an internally tagged enum (a `type` discriminant field
/// alongside the variant's own fields), the same representation and for
/// the same reason `admissionlab_spec::model::InstallMethodSpec`
/// documents: `serde_norway` only supports externally tagged enums via
/// an explicit YAML type tag, not a wrapping single-key mapping, so
/// external tagging cannot represent a natural `type: helm` YAML shape.
///
/// Deliberately **not** a reuse of
/// `admissionlab_spec::model::InstallMethodSpec` (the raw shape a lab
/// file's own `install:` block parses into), even though the two look
/// superficially similar: that type's `HelmInstallSpec` carries
/// `valuesFiles`/`setValues` fields this crate does not yet resolve (see
/// [`RawHelmInstall`]'s documentation) — reusing it here would let a
/// recipe author write `valuesFiles:` and have it silently parse but
/// never take effect, exactly the kind of silent, surprising behavior
/// this project's `deny_unknown_fields` convention exists to prevent
/// elsewhere. A narrower, recipe-specific raw type keeps that field
/// absent from the schema entirely, so writing it is a loud parse error
/// instead.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum RawInstallMethod {
    /// Install via a Helm chart.
    Helm(RawHelmInstall),
    /// Install a fixed set of raw Kubernetes manifests.
    Manifests(RawManifestsInstall),
}

/// Install method: a Helm chart, exactly as a recipe author writes it.
///
/// `chart`/`repo`/`version` are all required (not `Option`, unlike
/// `admissionlab_spec::model::HelmInstallSpec`'s `repo`/`version`): a
/// recipe's entire purpose is supplying a pinned installation default
/// (PRODUCT.md §14's "installation defaults" and "supported
/// versions/ranges"), so there is no legitimate "recipe with an
/// unstated chart version" the way a lab file's
/// `admissionlab_spec::ComponentSpec::version` can legitimately fall
/// back to whatever its install method implies. `version` is further
/// validated to be an exact pin (see [`crate::model::require_pinned_version`]) —
/// the same reproducibility rule
/// `admissionlab_spec::validate::require_pinned_helm_version` enforces
/// for a lab file's own Helm installs, reimplemented here rather than
/// reused because that function is private to `admissionlab-spec` (not
/// part of its public API).
///
/// No `valuesFiles`/`setValues`: a values file's relative path would
/// need a directory to resolve against, and an embedded built-in recipe
/// has no real filesystem location to resolve one against (an on-disk
/// override recipe does, but this crate does not special-case built-in
/// vs. override resolution for one field only the override side could
/// ever support). Deferred to whichever later task first implements a
/// recipe that actually needs a values override, with a concrete answer
/// to that question in hand rather than a speculative one built now.
/// `setValues` (literal `--set-string` overrides) has no such ambiguity
/// — it is plain string data — but is left out for the same YAGNI
/// reason: nothing in this task's scope needs it yet, and
/// [`admissionlab_spec::component::HelmInstallSpec::set_values`] is
/// always constructed empty here regardless.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RawHelmInstall {
    pub chart: String,
    pub repo: String,
    pub version: String,
    #[serde(default)]
    pub repo_name: Option<String>,
    #[serde(default)]
    pub release_name: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
}

/// Install method: a fixed set of raw Kubernetes manifests, exactly as a
/// recipe author writes it.
///
/// Every path must be absolute — see [`resolve_manifests`]'s
/// documentation for why a relative path is rejected outright rather
/// than resolved against some base directory.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RawManifestsInstall {
    pub paths: Vec<PathBuf>,
}

/// The raw shape of one [`RawRecipe::readiness`] entry. Mirrors
/// [`admissionlab_spec::ReadinessCheck`]'s variant set exactly (see this
/// module's documentation for why that closed set matters).
///
/// `rename_all` alone only renames the `type` tag values (the variant
/// names); it does not cascade into a struct-like variant's own field
/// names the way it would for a plain struct. `rename_all_fields` is the
/// separate attribute that does that (verified directly against
/// `serde_derive-1.0.229`'s own `internals/attr.rs`, which parses it as
/// `#[serde(rename_all_fields = "...")]`, valid only on enums) — both are
/// needed here so `CustomResourceCondition`'s `api_version`/
/// `condition_type` are written `apiVersion`/`conditionType` on the
/// wire, consistent with every other multi-word key in this project's
/// YAML surface.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum RawReadinessCheck {
    /// A `Deployment`'s `Available` condition must be `True`.
    DeploymentAvailable { namespace: String, name: String },
    /// A `DaemonSet` must have every desired pod scheduled and ready.
    DaemonSetReady { namespace: String, name: String },
    /// A `Job` must have completed successfully.
    JobComplete { namespace: String, name: String },
    /// A `ValidatingWebhookConfiguration`/`MutatingWebhookConfiguration`
    /// with this name must exist.
    WebhookConfigurationPresent { name: String },
    /// A custom resource's named condition must equal a given status.
    CustomResourceCondition {
        api_version: String,
        kind: String,
        #[serde(default)]
        namespace: Option<String>,
        name: String,
        condition_type: String,
        status: String,
    },
}

/// The raw shape of one [`RawRecipe::normalize_rules`] entry. Mirrors
/// [`admissionlab_spec::RecipeNormalizeRule`]'s variant set exactly (see
/// this module's documentation for why that closed set matters — this is
/// specifically the set PRODUCT.md §14 calls "known harmless
/// normalization rules").
///
/// `rename_all_fields` alongside `rename_all`: see
/// [`RawReadinessCheck`]'s documentation for why both are needed for a
/// struct-like variant's own fields to be renamed, not only the `type`
/// tag. Every field here happens to already be a single lowercase word
/// today, so this has no visible effect yet, but is included for the
/// same correctness reason regardless — a future field addition should
/// not have to remember to add it retroactively.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum RawNormalizeRule {
    /// Remove the value at a JSON Pointer before comparison.
    RemovePointer { pointer: String },
    /// Remove a specific annotation before comparison.
    RemoveAnnotation { annotation: String },
    /// Sort an array of objects at a JSON Pointer by a named key before
    /// comparison.
    SortNamedArray { pointer: String, key: String },
}

// ---------------------------------------------------------------------
// Resolution: RawRecipe -> Recipe
// ---------------------------------------------------------------------

/// Validates `raw` and resolves it into a [`Recipe`].
///
/// `source_label` identifies which document `raw` came from for every
/// error message this produces — see [`RecipeError::Parse::source_label`].
///
/// # Errors
///
/// Returns [`RecipeError::Validation`] if `name` or `version` is empty
/// (or all whitespace), if a Helm install's `chart`/`repo` is empty or
/// whose `version` is not an exact pin, if a manifests install's `paths`
/// is empty or contains a relative path, if any readiness check or
/// normalize rule field is empty, or if a `capabilities` entry is not a
/// recognized spelling (see [`crate::capability::parse_capability`]).
pub(crate) fn resolve_recipe(source_label: &str, raw: RawRecipe) -> Result<Recipe, RecipeError> {
    let name = require_nonempty(source_label, "name", &raw.name)?;
    let version = require_nonempty(source_label, "version", &raw.version)?;
    let install = resolve_install(source_label, &name, raw.install)?;

    let readiness = raw
        .readiness
        .into_iter()
        .enumerate()
        .map(|(index, check)| resolve_readiness(source_label, index, check))
        .collect::<Result<Vec<_>, _>>()?;

    let normalize_rules = raw
        .normalize_rules
        .into_iter()
        .enumerate()
        .map(|(index, rule)| resolve_normalize_rule(source_label, index, rule))
        .collect::<Result<Vec<_>, _>>()?;

    let capabilities = raw
        .capabilities
        .iter()
        .enumerate()
        .map(|(index, raw_capability)| {
            parse_capability(raw_capability).map_err(|message| {
                RecipeError::validation(
                    source_label,
                    format_args!("capabilities[{index}]"),
                    message,
                )
            })
        })
        .collect::<Result<BTreeSet<Capability>, _>>()?;

    Ok(Recipe {
        name,
        version,
        install,
        readiness,
        normalize_rules,
        capabilities,
    })
}

fn resolve_install(
    source_label: &str,
    recipe_name: &str,
    raw: RawInstallMethod,
) -> Result<InstallMethod, RecipeError> {
    match raw {
        RawInstallMethod::Helm(helm) => Ok(InstallMethod::Helm(resolve_helm(
            source_label,
            recipe_name,
            &helm,
        )?)),
        RawInstallMethod::Manifests(manifests) => Ok(InstallMethod::Manifests(resolve_manifests(
            source_label,
            manifests,
        )?)),
    }
}

/// Resolves a Helm install method. `repo_name`/`release_name`/`namespace`
/// each come from `raw`'s matching optional field when set, and
/// otherwise default to `recipe_name` — the same defaulting shape
/// `admissionlab_spec::component::resolve_helm` uses for a lab file's
/// own Helm installs, reimplemented here for the same reason
/// [`RawHelmInstall`]'s documentation gives for not reusing that type.
fn resolve_helm(
    source_label: &str,
    recipe_name: &str,
    raw: &RawHelmInstall,
) -> Result<HelmInstallSpec, RecipeError> {
    let chart = require_nonempty(source_label, "install.chart", &raw.chart)?;
    let repo_url = require_nonempty(source_label, "install.repo", &raw.repo)?;
    let version = require_pinned_version(source_label, "install.version", &raw.version)?;

    Ok(HelmInstallSpec {
        repo_name: nonempty_or(raw.repo_name.as_deref(), recipe_name),
        repo_url,
        chart,
        version,
        release_name: nonempty_or(raw.release_name.as_deref(), recipe_name),
        namespace: nonempty_or(raw.namespace.as_deref(), recipe_name),
        values_files: Vec::new(),
        set_values: BTreeMap::new(),
    })
}

/// Resolves a manifests install method.
///
/// Every path must already be absolute. A relative path is rejected
/// rather than resolved against some base directory, because a recipe
/// has no single, well-defined directory to resolve it against: an
/// on-disk override recipe has a real parent directory, but an embedded
/// built-in recipe's text has no filesystem location at all (see
/// `crate::load`'s module documentation) — inventing one for embedded
/// content would fabricate a location that does not exist, and silently
/// treating built-in and override recipes differently for this one field
/// would be its own surprise. Revisit once a real manifests-based recipe
/// needs relative paths and can settle this with a concrete use case.
fn resolve_manifests(
    source_label: &str,
    raw: RawManifestsInstall,
) -> Result<ManifestInstallSpec, RecipeError> {
    if raw.paths.is_empty() {
        return Err(RecipeError::validation(
            source_label,
            "install.paths",
            "must not be empty",
        ));
    }
    let paths = raw
        .paths
        .into_iter()
        .map(|path| {
            if path.is_absolute() {
                Ok(path)
            } else {
                Err(RecipeError::validation(
                    source_label,
                    "install.paths",
                    format_args!(
                        "{} is relative, but a recipe has no directory to resolve it against \
                         yet (an embedded built-in recipe has no filesystem location at all) -- \
                         use an absolute path",
                        path.display()
                    ),
                ))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ManifestInstallSpec { paths })
}

fn resolve_readiness(
    source_label: &str,
    index: usize,
    raw: RawReadinessCheck,
) -> Result<ReadinessCheck, RecipeError> {
    let locator = format!("readiness[{index}]");
    match raw {
        RawReadinessCheck::DeploymentAvailable { namespace, name } => {
            Ok(ReadinessCheck::DeploymentAvailable {
                namespace: require_nonempty(
                    source_label,
                    &format!("{locator}.namespace"),
                    &namespace,
                )?,
                name: require_nonempty(source_label, &format!("{locator}.name"), &name)?,
            })
        }
        RawReadinessCheck::DaemonSetReady { namespace, name } => {
            Ok(ReadinessCheck::DaemonSetReady {
                namespace: require_nonempty(
                    source_label,
                    &format!("{locator}.namespace"),
                    &namespace,
                )?,
                name: require_nonempty(source_label, &format!("{locator}.name"), &name)?,
            })
        }
        RawReadinessCheck::JobComplete { namespace, name } => Ok(ReadinessCheck::JobComplete {
            namespace: require_nonempty(source_label, &format!("{locator}.namespace"), &namespace)?,
            name: require_nonempty(source_label, &format!("{locator}.name"), &name)?,
        }),
        RawReadinessCheck::WebhookConfigurationPresent { name } => {
            Ok(ReadinessCheck::WebhookConfigurationPresent {
                name: require_nonempty(source_label, &format!("{locator}.name"), &name)?,
            })
        }
        RawReadinessCheck::CustomResourceCondition {
            api_version,
            kind,
            namespace,
            name,
            condition_type,
            status,
        } => Ok(ReadinessCheck::CustomResourceCondition {
            api_version: require_nonempty(
                source_label,
                &format!("{locator}.apiVersion"),
                &api_version,
            )?,
            kind: require_nonempty(source_label, &format!("{locator}.kind"), &kind)?,
            namespace: namespace
                .map(|value| {
                    require_nonempty(source_label, &format!("{locator}.namespace"), &value)
                })
                .transpose()?,
            name: require_nonempty(source_label, &format!("{locator}.name"), &name)?,
            condition_type: require_nonempty(
                source_label,
                &format!("{locator}.conditionType"),
                &condition_type,
            )?,
            status: require_nonempty(source_label, &format!("{locator}.status"), &status)?,
        }),
    }
}

fn resolve_normalize_rule(
    source_label: &str,
    index: usize,
    raw: RawNormalizeRule,
) -> Result<RecipeNormalizeRule, RecipeError> {
    let locator = format!("normalizeRules[{index}]");
    match raw {
        RawNormalizeRule::RemovePointer { pointer } => Ok(RecipeNormalizeRule::RemovePointer(
            require_nonempty(source_label, &format!("{locator}.pointer"), &pointer)?,
        )),
        RawNormalizeRule::RemoveAnnotation { annotation } => {
            Ok(RecipeNormalizeRule::RemoveAnnotation(require_nonempty(
                source_label,
                &format!("{locator}.annotation"),
                &annotation,
            )?))
        }
        RawNormalizeRule::SortNamedArray { pointer, key } => {
            Ok(RecipeNormalizeRule::SortNamedArray {
                pointer: require_nonempty(source_label, &format!("{locator}.pointer"), &pointer)?,
                key: require_nonempty(source_label, &format!("{locator}.key"), &key)?,
            })
        }
    }
}

// ---------------------------------------------------------------------
// Small validation helpers, mirroring admissionlab_spec::validate's
// shape (that module is private to admissionlab-spec, so these are
// independent, recipe-scoped reimplementations, not shared code).
// ---------------------------------------------------------------------

/// Rejects an empty (or all-whitespace) string; otherwise returns it
/// trimmed.
fn require_nonempty(source_label: &str, locator: &str, value: &str) -> Result<String, RecipeError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(RecipeError::validation(
            source_label,
            locator,
            "must not be empty",
        ));
    }
    Ok(trimmed.to_owned())
}

/// Returns `value` trimmed, if present and non-empty; otherwise returns
/// `default` as an owned `String`. Mirrors
/// `admissionlab_spec::component::nonempty_or`'s exact shape.
fn nonempty_or(value: Option<&str>, default: &str) -> String {
    match value.map(str::trim) {
        Some(v) if !v.is_empty() => v.to_owned(),
        _ => default.to_owned(),
    }
}

/// Requires `version` to be an exact pin, returning it trimmed. See
/// [`is_pinned_semver`] for the exact grammar.
fn require_pinned_version(
    source_label: &str,
    locator: &str,
    version: &str,
) -> Result<String, RecipeError> {
    let trimmed = require_nonempty(source_label, locator, version)?;
    if !is_pinned_semver(&trimmed) {
        return Err(RecipeError::validation(
            source_label,
            locator,
            format_args!(
                "{trimmed:?} is not an exact pinned version -- ranges, \"latest\", wildcards, \
                 and partial versions (a bare major or major.minor) are not reproducible; use a \
                 full major.minor.patch such as \"3.9.0\""
            ),
        ));
    }
    Ok(trimmed)
}

/// An optional `v`/`V` prefix, then exactly three dot-separated, purely
/// numeric `MAJOR.MINOR.PATCH` segments, then an optional `-prerelease`
/// and/or `+build` suffix. Mirrors
/// `admissionlab_spec::validate::is_pinned_semver`'s grammar exactly
/// (that function is private to `admissionlab-spec`, so this is a
/// deliberate, independent reimplementation of the same rule for the
/// same reproducibility reason a recipe's pinned version exists at all —
/// not a divergence).
fn is_pinned_semver(version: &str) -> bool {
    let core = version.strip_prefix(['v', 'V']).unwrap_or(version);

    let (core, build) = match core.split_once('+') {
        Some((core, build)) => (core, Some(build)),
        None => (core, None),
    };
    if build.is_some_and(str::is_empty) {
        return false;
    }

    let (release, prerelease) = match core.split_once('-') {
        Some((release, prerelease)) => (release, Some(prerelease)),
        None => (core, None),
    };
    if prerelease.is_some_and(str::is_empty) {
        return false;
    }

    let segments: Vec<&str> = release.split('.').collect();
    segments.len() == 3
        && segments
            .iter()
            .all(|segment| !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit()))
}
