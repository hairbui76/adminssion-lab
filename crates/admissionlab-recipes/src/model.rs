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
use std::path::{Component, Path, PathBuf};

use admissionlab_spec::component::HelmInstallSpec;
use admissionlab_spec::{
    Capability, GatewayEndpointStrategy, InstallMethod, ManifestInstallSpec, ReadinessCheck,
    RecipeNormalizeRule,
};
use serde::Deserialize;
use thiserror::Error;

use crate::capability::{capability_spelling, parse_capability, resolve_gateway_endpoint};

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
    /// How to find the `Service` fronting this recipe's data plane, when
    /// its component provides one (ROADMAP Task 6.6).
    ///
    /// `None` for every recipe that declares no *traffic-serving*
    /// capability — and [`resolve_recipe`] rejects the two inconsistent
    /// combinations outright rather than letting either pass silently: a
    /// `gatewayEndpoint:` without such a capability (metadata for a data
    /// plane the recipe does not claim to provide), and such a
    /// capability without a `gatewayEndpoint:` (a recipe whose objects
    /// Admission Lab could observe but never send a single request
    /// through). The second is the load-bearing one: silently accepting
    /// it would turn a missing recipe field into "this stack serves no
    /// traffic", which is a fabricated observation rather than a
    /// configuration error (Global Constraint 15).
    ///
    /// # Which capabilities are "traffic-serving"
    ///
    /// Exactly the set this module's own `TRAFFIC_SERVING_CAPABILITIES`
    /// constant names, and that constant is the whole definition — there
    /// is no second list anywhere. Task 6.6 wrote this rule over
    /// [`Capability::GatewayApi`] alone, which was exactly right while
    /// that was the only capability whose recipes were probed over HTTP.
    /// Task 8.2 added [`Capability::LegacyIngress`]
    /// (`recipes/ingress-nginx-legacy/`), which is the same *kind* of
    /// thing — one `Service` fronting a data plane that fixtures send
    /// real requests through — arrived at by a different API. Widening
    /// the rule to a named set is the smaller change of the two
    /// available: the alternative, letting a `legacyIngress` recipe
    /// carry an endpoint by exempting it from the pairing check, would
    /// have made the *absent* endpoint legal too, which is the
    /// fabricated-observation case above.
    ///
    /// The field keeps the name `gatewayEndpoint` even though an
    /// `Ingress` controller is not a Gateway: the same YAML shape is
    /// `admissionlab.yaml`'s own `gateway.gatewayEndpoint:` block
    /// (Task 6.11) and is resolved by one shared validator, so renaming
    /// it here is a schema break in two files for a cosmetic gain.
    ///
    /// Install metadata, not classification — see
    /// [`GatewayEndpointStrategy`]'s own documentation, which is where
    /// that argument and the upstream provenance for the well-known
    /// gateway-name label both live.
    pub gateway_endpoint: Option<GatewayEndpointStrategy>,
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
    #[serde(default)]
    pub gateway_endpoint: Option<RawGatewayEndpoint>,
}

/// The raw shape of [`RawRecipe::gateway_endpoint`] (ROADMAP Task 6.6),
/// exactly as a recipe author writes it:
///
/// ```yaml
/// capabilities:
///   - gatewayApi
/// gatewayEndpoint:
///   type: serviceBySelector
///   namespace: "{gatewayNamespace}"
///   selector:
///     gateway.networking.k8s.io/gateway-name: "{gatewayName}"
///   portName: http
/// ```
///
/// **This is `admissionlab_spec::GatewayEndpointSpec`, not a type of
/// this crate's own.** It was declared here until ROADMAP Task 6.11 gave
/// `admissionlab.yaml` a `gateway.gatewayEndpoint:` block that parses
/// the identical YAML shape into the identical resolved
/// [`GatewayEndpointStrategy`]. Two `Deserialize` shapes for one
/// resolved type is the synonym §1.2 forbids -- they would be free to
/// drift a field apart while looking identical in YAML -- so the
/// canonical raw shape moved to `admissionlab-spec` (the leaf crate both
/// readers already depend on, and the same crate that owns the resolved
/// strategy for the same reason) and this alias keeps the recipe
/// loader's own vocabulary reading as it did.
///
/// Unlike [`RawInstallMethod`], where a narrower recipe-specific shape
/// is deliberate because the lab file's `install:` block carries fields
/// this crate cannot resolve, there is nothing here for a recipe to omit
/// or a lab file to add: the two documents want exactly the same four
/// fields, and a `gatewayEndpoint` means the same thing in both.
pub(crate) use admissionlab_spec::GatewayEndpointSpec as RawGatewayEndpoint;

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
/// An absolute path is used exactly as written. A relative path is
/// resolved against the recipe's own directory — see
/// [`resolve_manifests`]'s documentation for exactly how, for why a
/// built-in (embedded, no filesystem location) recipe cannot use one at
/// all, and for the traversal restriction a relative path is held to.
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

/// The capabilities whose recipes front a data plane Admission Lab sends
/// real HTTP requests through, and which therefore must declare a
/// `gatewayEndpoint:` (see [`Recipe::gateway_endpoint`] for the full
/// argument, including why a *missing* endpoint is the load-bearing half
/// of the rule).
///
/// Deliberately an explicit, short list rather than "every capability
/// except [`Capability::Admission`]". A capability added later is not
/// automatically traffic-serving, and the failure mode of guessing wrong
/// in that direction is silent: a new capability would inherit a
/// requirement its recipes cannot satisfy, or — worse, if the default
/// went the other way — would quietly stop requiring an endpoint it
/// needs. Adding a variant to [`Capability`] should make an author come
/// here and decide.
const TRAFFIC_SERVING_CAPABILITIES: &[Capability] =
    &[Capability::GatewayApi, Capability::LegacyIngress];

/// [`TRAFFIC_SERVING_CAPABILITIES`] rendered as the quoted, comma-joined
/// wire spellings a recipe author actually writes (`"gatewayApi",
/// "legacyIngress"`), for the two validation messages that must name
/// them.
///
/// Built from [`crate::capability::capability_spelling`] rather than
/// from string literals repeated here: the spelling of a capability has
/// exactly one home (`capability.rs`'s `KNOWN` table), and an error
/// message that told an author to write a spelling the parser does not
/// accept would be worse than no message at all.
fn traffic_serving_spellings() -> String {
    TRAFFIC_SERVING_CAPABILITIES
        .iter()
        .map(|capability| format!("{:?}", capability_spelling(*capability)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Validates `raw` and resolves it into a [`Recipe`].
///
/// `source_label` identifies which document `raw` came from for every
/// error message this produces — see [`RecipeError::Parse::source_label`].
/// `base_dir` is the directory a relative `install.paths` entry
/// resolves against: `Some` for an on-disk override recipe (the
/// directory [`crate::load::load_recipe_overrides`] found the recipe
/// file in), `None` for a built-in, embedded recipe, which has no
/// filesystem location at all. See [`resolve_manifests`]'s
/// documentation for exactly how a relative path is resolved and
/// confined once `base_dir` is `Some`.
///
/// # Errors
///
/// Returns [`RecipeError::Validation`] if `name` or `version` is empty
/// (or all whitespace), if a Helm install's `chart`/`repo` is empty or
/// whose `version` is not an exact pin, if a manifests install's `paths`
/// is empty, contains a relative path while `base_dir` is `None`, or
/// contains a relative path that resolves outside `base_dir`'s own
/// directory tree, if any readiness check or normalize rule field is
/// empty, if a `capabilities` entry is not a recognized spelling (see
/// [`crate::capability::parse_capability`]), if a `gatewayEndpoint`
/// block fails [`crate::capability::resolve_gateway_endpoint`]'s
/// validation, or if `gatewayEndpoint` and a traffic-serving capability
/// ([`TRAFFIC_SERVING_CAPABILITIES`]) are not both present or both
/// absent (see [`Recipe::gateway_endpoint`]).
pub(crate) fn resolve_recipe(
    source_label: &str,
    raw: RawRecipe,
    base_dir: Option<&Path>,
) -> Result<Recipe, RecipeError> {
    let name = require_nonempty(source_label, "name", &raw.name)?;
    let version = require_nonempty(source_label, "version", &raw.version)?;
    let install = resolve_install(source_label, &name, raw.install, base_dir)?;

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

    let gateway_endpoint = raw
        .gateway_endpoint
        .map(|raw_endpoint| {
            resolve_gateway_endpoint(&raw_endpoint).map_err(|(locator, message)| {
                RecipeError::validation(
                    source_label,
                    format_args!("gatewayEndpoint.{locator}"),
                    message,
                )
            })
        })
        .transpose()?;

    // The two halves must agree -- see `Recipe::gateway_endpoint` for
    // why neither mismatch is allowed to pass quietly, and
    // `TRAFFIC_SERVING_CAPABILITIES` for which capabilities this rule is
    // stated over.
    let serves_traffic = TRAFFIC_SERVING_CAPABILITIES
        .iter()
        .any(|capability| capabilities.contains(capability));
    match (serves_traffic, gateway_endpoint.is_some()) {
        (true, false) => {
            return Err(RecipeError::validation(
                source_label,
                "gatewayEndpoint",
                format_args!(
                    "is required by a recipe declaring any of {} -- without it Admission Lab can \
                     observe an object's status but has no way to find the Service to send a \
                     request through",
                    traffic_serving_spellings()
                ),
            ));
        }
        (false, true) => {
            return Err(RecipeError::validation(
                source_label,
                "gatewayEndpoint",
                format_args!(
                    "is only meaningful for a recipe declaring one of {}; add one of them to \
                     capabilities, or remove this block",
                    traffic_serving_spellings()
                ),
            ));
        }
        (true, true) | (false, false) => {}
    }

    Ok(Recipe {
        name,
        version,
        install,
        readiness,
        normalize_rules,
        capabilities,
        gateway_endpoint,
    })
}

fn resolve_install(
    source_label: &str,
    recipe_name: &str,
    raw: RawInstallMethod,
    base_dir: Option<&Path>,
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
            base_dir,
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
/// An absolute path is used exactly as written, unconditionally — this
/// project places no restriction on what an absolute path may name, for
/// a built-in or an override recipe alike, both before this function
/// existed and after.
///
/// A relative path is resolved against `base_dir`, mirroring
/// `admissionlab_spec::resolve_lab`'s own established pattern for a lab
/// file's relative paths (`admissionlab_spec::resolve::resolve_relative`)
/// — except a relative path here is additionally confined to
/// `base_dir`'s own subtree (see [`join_confined`]'s documentation for
/// exactly what that confinement does and does not stop), because
/// PRODUCT.md §29.1 treats everything a recipe causes to be installed as
/// an untrusted test workload, unlike a lab file a user hand-writes and
/// runs against their own machine directly.
///
/// `base_dir` is `None` for a built-in, embedded recipe — its text is
/// `include_str!`-embedded into the compiled binary (`crate::load`'s
/// module documentation) and has no filesystem location at all to
/// resolve a relative path against — so a relative path always fails
/// for one, with an error explaining why. It is `Some` for an on-disk
/// override recipe: [`crate::load::load_recipe_overrides`] passes the
/// directory the recipe file itself was found in.
fn resolve_manifests(
    source_label: &str,
    raw: RawManifestsInstall,
    base_dir: Option<&Path>,
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
        .map(|path| resolve_manifest_path(source_label, base_dir, path))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ManifestInstallSpec { paths })
}

/// Resolves one `install.paths` entry.
///
/// An absolute `path` passes through unchanged. A relative `path` with
/// `base_dir` `None` (a built-in recipe) always fails. A relative `path`
/// with `base_dir` `Some` is joined onto it and confined to it via
/// [`join_confined`]; failing that confinement is reported the same way
/// as any other validation failure (a [`RecipeError::Validation`]
/// naming `install.paths`), not a distinct error variant — from a
/// recipe author's point of view, a `..` that escapes `base_dir` is
/// exactly as invalid a value for this field as a relative path with no
/// `base_dir` to resolve against at all.
fn resolve_manifest_path(
    source_label: &str,
    base_dir: Option<&Path>,
    path: PathBuf,
) -> Result<PathBuf, RecipeError> {
    if path.is_absolute() {
        return Ok(path);
    }
    let Some(base_dir) = base_dir else {
        return Err(RecipeError::validation(
            source_label,
            "install.paths",
            format_args!(
                "{} is relative, but this recipe has no directory to resolve it against -- a \
                 built-in recipe's text is embedded into the compiled binary and has no \
                 filesystem location at all (see crate::load's module documentation); use an \
                 absolute path, or load this recipe from an on-disk override directory instead",
                path.display()
            ),
        ));
    };
    join_confined(base_dir, &path).ok_or_else(|| {
        RecipeError::validation(
            source_label,
            "install.paths",
            format_args!(
                "{} would resolve outside this recipe's own directory ({}) -- a relative \
                 install.paths entry may only reach a file inside the recipe's own directory \
                 tree",
                path.display(),
                base_dir.display()
            ),
        )
    })
}

/// Joins `relative` onto `base_dir`, but only when that can be proven,
/// from `relative`'s own text alone, to stay inside `base_dir`'s
/// subtree; returns `None` otherwise.
///
/// # What this stops
///
/// Rejects `relative` if any of its own components — read purely as
/// text, independent of whatever `base_dir` happens to contain on disk
/// — would step above the point `relative` itself started resolving
/// from. A leading `..` (`"../x"`), or enough of them to outrun the
/// preceding components available to cancel each one
/// (`"a/../../x"`, `"../../../etc/passwd"`), is rejected regardless of
/// how deep `base_dir` itself is, and regardless of whether anything
/// involved exists on disk yet: this never touches the filesystem. A
/// `..` that *is* cancelled by an earlier component of the same
/// `relative` value is allowed, since the final destination never
/// leaves `base_dir` (`"a/../b"` resolves exactly as `"b"` alone
/// would).
///
/// This deliberately does not join first and test
/// `joined.starts_with(base_dir)` afterwards: [`PathBuf::join`] never
/// rewrites or removes the components it started from, so that test is
/// vacuously true for *any* relative second argument — `..`-laden or
/// not — and rejects nothing at all. This function instead walks
/// `relative`'s own components and tracks the running depth below
/// `base_dir` directly, rejecting the moment a `ParentDir` component
/// would take that depth negative, before it is ever allowed to touch
/// `base_dir`'s own components.
///
/// # What this does not stop
///
/// A symlink. If some path inside `base_dir`'s own subtree is a symlink
/// pointing outside it, a `relative` value that reaches its target
/// through that symlink — no `..` involved at all, for example
/// `"linked/x.yaml"` where `base_dir/linked` is a symlink to `/etc` — is
/// still accepted by this function: lexically, `"linked/x.yaml"` never
/// leaves `base_dir`. Telling the two cases apart requires resolving
/// the real filesystem path (`Path::canonicalize`), which requires the
/// target to already exist; that existence requirement is a behavior
/// change this function does not make on its own — see
/// [`resolve_manifest_path`]'s callers, none of which requires an
/// `install.paths` entry to exist at recipe-resolution time today.
///
/// # This is input validation, not a security boundary
///
/// The symlink gap above is a documented limitation rather than a hole
/// to plug, because confinement here is not what stands between a
/// hostile recipe and the filesystem — nothing does. An **absolute**
/// `install.paths` entry passes through [`resolve_manifest_path`]
/// entirely unchecked, so a recipe author who wants to read `/etc` can
/// simply write `/etc`, with no symlink and no `..` needed. Hardening
/// this function against symlinks while that remains true would buy no
/// safety and cost a real behavior change.
///
/// What this function is actually for: catching an **accidental** `..`
/// in a checked-in, reviewed recipe — a copied path that silently
/// escapes its directory and reads something unintended. Treat a
/// recipe as trusted input, because it is; confine relative paths
/// because a mistake there is easy to make and easy to miss in review.
fn join_confined(base_dir: &Path, relative: &Path) -> Option<PathBuf> {
    let mut resolved = base_dir.to_path_buf();
    let mut depth: usize = 0;
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                resolved.push(part);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                depth = depth.checked_sub(1)?;
                resolved.pop();
            }
            // `relative` is only ever passed here already known
            // non-absolute (both callers of this function branch on
            // `path.is_absolute()` first), and on this project's sole
            // build target (`x86_64-unknown-linux-gnu`) a non-absolute
            // path's own `components()` never yields `RootDir`/
            // `Prefix`. Handled as an escape rather than reached by a
            // panic, in case that ever stops holding.
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(resolved)
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

// ---------------------------------------------------------------------
// Tests: base-directory resolution and traversal confinement (Task
// 2.10). Only reachable here, as unit tests inside this crate -- every
// function under test (`join_confined`, `resolve_manifest_path`,
// `resolve_manifests`, `resolve_recipe`) is private or `pub(crate)`, so
// none of it is visible to an integration test under `tests/`, which
// links against this crate's *public* API only. The public-surface
// proof (through `load_recipe_overrides`) lives in `tests/load.rs`.
//
// None of `join_confined`/`resolve_manifest_path`/`resolve_manifests`
// ever touches the filesystem (see `join_confined`'s own documentation)
// -- every path below, including every `base_dir`, is a synthetic,
// nonexistent `PathBuf`, on purpose: a test that only passed because it
// happened to point at real files on the machine running it would not
// actually be proving the no-filesystem-access claim these doc comments
// make.
#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    // -------------------------------------------------------------
    // `join_confined`: the pure lexical safety join.
    // -------------------------------------------------------------

    #[test]
    fn join_confined_resolves_a_plain_relative_path() {
        let base = Path::new("/recipes/test-webhook");
        let resolved = join_confined(base, Path::new("manifests/x.yaml"));
        assert_eq!(
            resolved,
            Some(base.join("manifests/x.yaml")),
            "a relative path with no `..` at all must resolve, unmodified, under base_dir"
        );
    }

    #[test]
    fn join_confined_ignores_current_dir_components() {
        let base = Path::new("/recipes/test-webhook");
        let resolved = join_confined(base, Path::new("./manifests/./x.yaml"));
        assert_eq!(
            resolved,
            Some(base.join("manifests/x.yaml")),
            "a `.` component must be a no-op, not literally appended or treated as an escape"
        );
    }

    #[test]
    fn join_confined_allows_a_dotdot_that_is_cancelled_within_the_same_path() {
        // A `..` that is cancelled by an earlier component of the SAME
        // relative value must be allowed: the final destination never
        // leaves base_dir. An implementation that rejects any `..`
        // whatsoever (an overly strict, but still "safe", mutation)
        // would fail this test -- proving the confinement is exactly
        // "never ends up above base_dir", not "never uses `..`".
        let base = Path::new("/recipes/test-webhook");
        let resolved = join_confined(base, Path::new("sibling/../manifests/x.yaml"));
        assert_eq!(
            resolved,
            Some(base.join("manifests/x.yaml")),
            "a `..` cancelled by a preceding component must resolve exactly as if neither had \
             been written"
        );
    }

    #[test]
    fn join_confined_rejects_a_single_leading_dotdot() {
        let base = Path::new("/recipes/test-webhook");
        let resolved = join_confined(base, Path::new("../escape.yaml"));
        assert_eq!(
            resolved, None,
            "a `..` with nothing preceding it to cancel must be rejected, not silently resolved \
             to base_dir's own parent"
        );
    }

    #[test]
    fn join_confined_rejects_the_brief_s_own_traversal_example() {
        // `.superpowers/sdd/ROADMAP/task-2.10-brief.md`'s own example of
        // what must not be reachable.
        let base = Path::new("/recipes/test-webhook");
        let resolved = join_confined(base, Path::new("../../../etc/passwd"));
        assert_eq!(resolved, None);
    }

    #[test]
    fn join_confined_rejects_a_dotdot_that_outruns_its_own_cancellation() {
        // The mutation this test exists to kill: an implementation that
        // lets a `ParentDir` component unconditionally pop from the
        // accumulated result (rather than refusing once there is
        // nothing of `relative`'s own to pop) would, after "a" cancels
        // the first `..`, let the SECOND `..` walk directly into
        // base_dir's own parent and beyond -- silently, with no error.
        // One Normal component is not enough cover for two ParentDir
        // components.
        let base = Path::new("/recipes/test-webhook");
        let resolved = join_confined(base, Path::new("a/../../etc/passwd"));
        assert_eq!(resolved, None);
    }

    #[test]
    fn join_confined_rejects_a_bare_dotdot() {
        let base = Path::new("/recipes/test-webhook");
        let resolved = join_confined(base, Path::new(".."));
        assert_eq!(
            resolved, None,
            "base_dir's own parent directory is definitionally outside base_dir's tree"
        );
    }

    #[test]
    fn join_confined_naive_starts_with_check_would_have_wrongly_accepted_this() {
        // Documents, with a runnable assertion, exactly the trap
        // `join_confined`'s own doc comment describes: `PathBuf::join`
        // never removes or rewrites the components it started from, so
        // `base.join(relative).starts_with(base)` is true for EVERY
        // relative `relative`, including this one. If `join_confined`
        // were reimplemented as that naive join-then-`starts_with`
        // check, this assertion would fail (the naive check returns
        // `Some`, never `None`, for any relative path at all) even
        // though the two prior traversal tests above might still
        // happen to look reasonable in isolation.
        let base = Path::new("/recipes/test-webhook");
        let relative = Path::new("../../../etc/passwd");
        assert!(
            base.join(relative).starts_with(base),
            "sanity check on the trap itself: join-then-starts_with must be vacuously true here"
        );
        assert_eq!(
            join_confined(base, relative),
            None,
            "join_confined must reject this despite the naive check above being satisfied"
        );
    }

    #[test]
    fn join_confined_rejects_a_component_that_looks_absolute() {
        // Defense in depth: both real callers of `join_confined`
        // (`resolve_manifest_path`) already branch on `path.is_absolute()`
        // before ever calling this function, so this input never
        // actually reaches it in production. This pins the fallback
        // behavior directly against the pure function anyway, in case
        // that calling discipline is ever violated.
        let base = Path::new("/recipes/test-webhook");
        let resolved = join_confined(base, Path::new("/etc/passwd"));
        assert_eq!(resolved, None);
    }

    /// Empirically pins the gap [`join_confined`]'s own doc comment
    /// describes under "What this does not stop", rather than leaving
    /// it as an unverified claim: a real symlink inside `base_dir`'s
    /// subtree pointing outside it, named by a `relative` value with no
    /// `..` in it at all. Unlike every test above, this one needs real
    /// files on disk -- a symlink is a filesystem fact, not something a
    /// `PathBuf`'s text alone can express -- so, uniquely in this
    /// module, it creates some.
    ///
    /// If this test ever starts failing because `join_confined` began
    /// rejecting the escape, that is a real behavior change (canonicalize-based
    /// checking was added) and this test's own assertions -- not only
    /// its doc comment -- need updating to match, which is exactly the
    /// point of pinning the gap with a runnable test rather than prose
    /// alone.
    #[test]
    fn join_confined_does_not_detect_a_symlink_that_points_outside_base_dir() {
        let root = unique_symlink_test_dir("join-confined-symlink-gap");
        let base_dir = root.join("recipe");
        let outside_dir = root.join("outside");
        std::fs::create_dir_all(&base_dir).expect("create synthetic recipe directory");
        std::fs::create_dir_all(&outside_dir).expect("create synthetic outside directory");

        // A marker file entirely outside base_dir's own subtree -- the
        // thing a confinement check exists to keep a relative
        // install.paths entry from ever reaching.
        let marker_contents = "TASK-2.10-SYMLINK-ESCAPE-MARKER";
        std::fs::write(outside_dir.join("secret.yaml"), marker_contents)
            .expect("write marker file outside base_dir");

        // base_dir/linked -> outside_dir. A relative path through it
        // carries no `..` component anywhere in its text.
        std::os::unix::fs::symlink(&outside_dir, base_dir.join("linked"))
            .expect("create the symlink this test exists to exercise");

        let relative = Path::new("linked/secret.yaml");
        let resolved = join_confined(&base_dir, relative);

        assert_eq!(
            resolved,
            Some(base_dir.join("linked/secret.yaml")),
            "join_confined accepts this today -- lexically, \"linked/secret.yaml\" never leaves \
             base_dir; that IS the gap this test pins"
        );
        let resolved = resolved.expect("just asserted Some above");

        // Prove this is a real escape, not merely an unresolved `..`
        // sitting harmlessly in the returned PathBuf: canonicalizing
        // the path join_confined accepted lands outside base_dir's own
        // canonical form, and actually reading it returns the marker
        // content that lives only in outside_dir.
        let canonical_resolved = resolved
            .canonicalize()
            .expect("the symlinked-through path must exist for real");
        let canonical_base = base_dir
            .canonicalize()
            .expect("base_dir must exist for real");
        assert!(
            !canonical_resolved.starts_with(&canonical_base),
            "the path join_confined accepted must genuinely resolve outside base_dir once \
             symlinks are followed for real, or this test is not demonstrating the gap it \
             claims to"
        );
        let read_back =
            std::fs::read_to_string(&resolved).expect("read through the symlink for real");
        assert_eq!(
            read_back, marker_contents,
            "the content actually reachable through the accepted path must be outside_dir's \
             own marker, not something inside base_dir"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A fresh, guaranteed-unique directory under the system temp
    /// directory, for the one test above that (uniquely in this module)
    /// needs real files and a real symlink on disk. Mirrors
    /// `tests/load.rs`'s own `unique_temp_dir` helper shape.
    fn unique_symlink_test_dir(label: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "admissionlab-recipes-model-test-{}-{label}-{n}",
            std::process::id()
        ))
    }

    // -------------------------------------------------------------
    // `resolve_manifest_path` / `resolve_manifests`: wiring
    // `join_confined` (and the built-in `None` case) into a
    // `RecipeError`.
    // -------------------------------------------------------------

    #[test]
    fn resolve_manifest_path_leaves_an_absolute_path_unchanged_even_with_a_base_dir() {
        let base = Path::new("/recipes/test-webhook");
        let absolute = PathBuf::from("/opt/somewhere-else/x.yaml");
        let resolved = resolve_manifest_path("label", Some(base), absolute.clone())
            .expect("an absolute path must never be rejected");
        assert_eq!(
            resolved, absolute,
            "an absolute install.paths entry must be used exactly as written, never joined onto \
             base_dir"
        );
    }

    #[test]
    fn resolve_manifest_path_rejects_relative_path_with_no_base_dir() {
        let error = resolve_manifest_path("label", None, PathBuf::from("manifests/x.yaml"))
            .expect_err("a relative path with no base_dir (a built-in recipe) must be rejected");
        let message = error.to_string();
        assert!(
            message.contains("install.paths"),
            "error must name the offending field, got: {message}"
        );
        assert!(
            message.contains("built-in") && message.contains("embedded"),
            "error must explain that a built-in recipe has no filesystem location, got: \
             {message}"
        );
    }

    #[test]
    fn resolve_manifest_path_resolves_relative_path_inside_base_dir() {
        let base = Path::new("/recipes/test-webhook");
        let resolved =
            resolve_manifest_path("label", Some(base), PathBuf::from("manifests/x.yaml"))
                .expect("a relative path that stays inside base_dir must resolve");
        assert_eq!(resolved, base.join("manifests/x.yaml"));
    }

    #[test]
    fn resolve_manifest_path_rejects_traversal_outside_base_dir() {
        let base = Path::new("/recipes/test-webhook");
        let error =
            resolve_manifest_path("label", Some(base), PathBuf::from("../../../etc/passwd"))
                .expect_err("a relative path escaping base_dir must be rejected");
        let message = error.to_string();
        assert!(
            message.contains("install.paths"),
            "error must name the offending field, got: {message}"
        );
        assert!(
            message.contains("outside"),
            "error must explain the path would resolve outside the recipe's own directory, got: \
             {message}"
        );
    }

    #[test]
    fn resolve_manifests_still_rejects_an_empty_paths_list() {
        let base = Path::new("/recipes/test-webhook");
        let error = resolve_manifests(
            "label",
            RawManifestsInstall { paths: Vec::new() },
            Some(base),
        )
        .expect_err("an empty install.paths list must still be rejected");
        assert!(error.to_string().contains("install.paths"));
    }

    // -------------------------------------------------------------
    // `resolve_recipe`: base_dir actually threads all the way from the
    // public(crate) entry point down to `join_confined`, in both
    // directions (a resolvable relative path, and the built-in `None`
    // rejection) -- not merely at `resolve_manifests`'s own level.
    // -------------------------------------------------------------

    fn manifests_recipe(paths: Vec<PathBuf>) -> RawRecipe {
        RawRecipe {
            name: "test-webhook".to_owned(),
            version: "0.1.0".to_owned(),
            install: RawInstallMethod::Manifests(RawManifestsInstall { paths }),
            readiness: Vec::new(),
            normalize_rules: Vec::new(),
            capabilities: Vec::new(),
            gateway_endpoint: None,
        }
    }

    #[test]
    fn resolve_recipe_resolves_a_relative_manifests_path_against_base_dir() {
        let base = Path::new("/recipes/test-webhook");
        let raw = manifests_recipe(vec![PathBuf::from("manifests/x.yaml")]);

        let recipe =
            resolve_recipe("label", raw, Some(base)).expect("must resolve with a base_dir set");

        let InstallMethod::Manifests(manifests) = &recipe.install else {
            panic!(
                "expected a Manifests install method, got {:?}",
                recipe.install
            );
        };
        assert_eq!(manifests.paths, vec![base.join("manifests/x.yaml")]);
    }

    #[test]
    fn resolve_recipe_rejects_a_relative_manifests_path_with_no_base_dir() {
        // The acceptance criterion this task names explicitly: "a
        // built-in recipe with a relative path still fails, with an
        // explanatory error". `load_builtin_recipes` always calls
        // `resolve_recipe` with `base_dir: None` -- this is that exact
        // call shape, without needing a real manifests-based entry in
        // `BUILTIN_RECIPES` (there is none; both built-ins today are
        // Helm-only) to exercise it.
        let raw = manifests_recipe(vec![PathBuf::from("manifests/x.yaml")]);

        let error = resolve_recipe("label", raw, None)
            .expect_err("a relative path with no base_dir must fail");
        let message = error.to_string();
        assert!(message.contains("install.paths"));
        assert!(message.contains("built-in") && message.contains("embedded"));
    }

    #[test]
    fn resolve_recipe_rejects_traversal_outside_base_dir() {
        let base = Path::new("/recipes/test-webhook");
        let raw = manifests_recipe(vec![PathBuf::from("../../../etc/passwd")]);

        let error = resolve_recipe("label", raw, Some(base))
            .expect_err("a traversing relative path must fail even once base_dir is set");
        let message = error.to_string();
        assert!(message.contains("install.paths"));
        assert!(message.contains("outside"));
    }
}
