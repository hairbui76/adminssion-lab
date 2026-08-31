//! The `v1alpha1` user-facing `admissionlab.yaml` configuration model.
//!
//! Every type in this module is deserialized directly from the
//! configuration file a user hand-writes, so two properties hold
//! throughout:
//!
//! - **Strict by construction.** Every struct carries
//!   `#[serde(deny_unknown_fields)]`, so a misspelled key (`candiate`
//!   instead of `candidate`) is a hard parse error rather than a silently
//!   ignored typo. [`crate::load_lab`] is what turns that error into a
//!   message naming the file and the offending field.
//! - **`camelCase` on the wire, `snake_case` in Rust.** Every field carries an
//!   implicit or explicit `rename_all = "camelCase"` so users write
//!   `apiVersion`/`expectationsFile`/`failOn`, matching typical YAML/JSON
//!   convention, while Rust code reads idiomatic `snake_case`. [`crate::v1alpha1_json_schema`]
//!   generates its schema from these same derives, so the schema always
//!   reflects the camelCase spelling users actually type.
//!
//! This module defines the *raw* shape only: values exactly as written in
//! the file, with relative paths unresolved and no cross-field validation
//! applied. [`crate::resolve_lab`] (see `resolve.rs`) is what turns a
//! parsed [`LabSpec`] into a fully validated, path-resolved [`crate::ResolvedLab`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;

/// The only `apiVersion` value [`crate::load_lab`] accepts.
pub const API_VERSION: &str = "admissionlab.io/v1alpha1";

/// The only `kind` value [`crate::load_lab`] accepts.
pub const KIND: &str = "Lab";

/// Schema-only override for [`LabSpec::api_version`]: a JSON Schema
/// `const` locking the property to [`API_VERSION`], so an editor
/// validating a YAML file against the generated schema flags a wrong
/// `apiVersion` (for example `admissionlab.io/v1beta9`) directly, instead
/// of only failing at load time. Does not affect deserialization — the
/// field's Rust type stays `String` and [`crate::load_lab`] still performs
/// the authoritative runtime check.
fn api_version_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "const": API_VERSION
    })
}

/// Schema-only override for [`LabSpec::kind`]: a JSON Schema `const`
/// locking the property to [`KIND`]. See [`api_version_schema`] — same
/// reasoning, same non-effect on deserialization.
fn kind_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "const": KIND
    })
}

/// The root of an `admissionlab.yaml` configuration file.
///
/// `api_version` and `kind` are plain `String`s in Rust — parsing accepts
/// any value at the type level — because rejecting the wrong value is a
/// semantic check, not a syntactic one; [`crate::load_lab`] validates them
/// against [`API_VERSION`] and [`KIND`] immediately after deserializing.
/// The generated JSON Schema additionally `const`-locks both properties
/// (see [`api_version_schema`]/[`kind_schema`]), so an editor validating
/// against the schema also catches a wrong value, without changing what
/// this Rust type accepts.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LabSpec {
    /// Must equal [`API_VERSION`]; checked by [`crate::load_lab`] and
    /// `const`-locked in the generated schema (see [`api_version_schema`]).
    #[schemars(schema_with = "api_version_schema")]
    pub api_version: String,
    /// Must equal [`KIND`]; checked by [`crate::load_lab`] and
    /// `const`-locked in the generated schema (see [`kind_schema`]).
    #[schemars(schema_with = "kind_schema")]
    pub kind: String,
    /// The unmodified stack being compared against.
    pub baseline: EnvironmentSpec,
    /// The stack under test for regressions.
    pub candidate: EnvironmentSpec,
    /// Which fixtures to replay through both environments.
    pub fixtures: FixtureSelectionSpec,
    /// Regression policy. Omit entirely to accept every field's default
    /// (see [`PolicySpec`]'s `Default` impl).
    #[serde(default)]
    pub policy: PolicySpec,
    /// Path to an expectations file, resolved by [`crate::resolve_lab`]
    /// against this configuration file's own directory — see that
    /// function's documentation for why the working directory is never
    /// used for this.
    #[serde(default)]
    pub expectations_file: Option<PathBuf>,
}

/// One side (baseline or candidate) of a comparison: a Kubernetes version
/// plus the extra components installed on top of it.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentSpec {
    /// The Kubernetes version to provision, for example `"1.30.4"`. Must
    /// not be empty — validated by [`crate::resolve_lab`].
    pub kubernetes: String,
    /// Components to install on top of the base cluster, in installation
    /// order. Empty by default: a bare Kubernetes cluster with no extra
    /// components is a valid environment.
    #[serde(default)]
    pub components: Vec<ComponentSpec>,
}

/// One admission-stack component to install into an [`EnvironmentSpec`].
///
/// This is the **user-facing YAML form**. [`crate::component`] defines
/// the separate resolved component model [`crate::resolve_lab`] converts
/// this into ([`crate::ResolvedComponent`]); this type only carries what
/// a user writes by hand.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentSpec {
    /// The component's name, used to correlate it with its counterpart in
    /// the other environment and to detect duplicates. Required by
    /// [`crate::resolve_lab`] until recipe-derived naming exists (Task
    /// 2.5) — currently the only source of a resolved name.
    #[serde(default)]
    pub name: Option<String>,
    /// A named recipe to install this component from. Recipe resolution
    /// is Task 2.5's responsibility; this field is carried through
    /// unresolved and does not currently affect resolution at all — an
    /// explicit `install` block (below) is required regardless of
    /// whether `recipe` is also set.
    #[serde(default)]
    pub recipe: Option<String>,
    /// The component's version, in whatever form its install method
    /// understands (a Helm chart version, an image tag, and so on).
    /// [`crate::resolve_lab`] requires a non-empty value here *unless*
    /// the install method itself carries an unambiguous version (a
    /// pinned Helm chart version), in which case an omitted value here
    /// defaults to that.
    #[serde(default)]
    pub version: Option<String>,
    /// How to install this component. Required by [`crate::resolve_lab`]
    /// — recipe-driven installation (Task 2.5) does not exist yet, so
    /// this is currently the only source of a resolved
    /// [`crate::InstallMethod`]. Any relative paths inside it
    /// (`HelmInstallSpec::values_files`, `ManifestsInstallSpec::paths`)
    /// are resolved against the configuration file's own directory by
    /// [`crate::resolve_lab`], the same as every other path in the
    /// document — see [`crate::resolve::config_directory`].
    #[serde(default)]
    pub install: Option<InstallMethodSpec>,
    /// Conditions this component must satisfy before the next component
    /// on the same side is installed, and before any fixture is
    /// replayed.
    ///
    /// Empty by default, which is a real and sometimes correct answer (a
    /// component that only applies cluster-scoped configuration has
    /// nothing to wait on) — but for anything that serves admission it is
    /// almost never the right one. `helm upgrade --install` returns as
    /// soon as a release's manifests are applied, and `kubectl apply` as
    /// soon as its objects exist; neither waits for a controller to be
    /// running, for a `caBundle` to be filled in, or for a webhook
    /// configuration that a controller creates at *runtime* to appear at
    /// all. A lab that replays fixtures inside that window observes a
    /// stack that is not yet the stack under test, and does so at a
    /// different moment on each side — which is exactly the
    /// nondeterminism Global Constraint 7 rules out.
    ///
    /// The variant set mirrors `recipes/*/recipe.yaml`'s own `readiness`
    /// section one for one — deliberately, so a certified recipe's
    /// checks can be transcribed into a lab file unchanged (see
    /// [`ReadinessCheckSpec`]).
    #[serde(default)]
    pub readiness: Vec<ReadinessCheckSpec>,
}

/// One condition a [`ComponentSpec`] must satisfy before it counts as
/// installed.
///
/// The user-facing YAML form of [`crate::ReadinessCheck`], which is what
/// [`crate::resolve_lab`] converts this into and what
/// `admissionlab_installer::readiness` actually probes. The variant set
/// and the wire spelling of every field match `admissionlab_recipes`'s
/// own recipe-file readiness section exactly, so the two surfaces cannot
/// drift into two different vocabularies for one closed set of checks.
///
/// Internally tagged on `type`, with each variant's payload a named
/// struct rather than inline fields — the same representation
/// [`InstallMethodSpec`] uses, and for the same two reasons: it is the
/// only enum shape `serde_norway` can express as a plain YAML mapping,
/// and putting each payload in its own `rename_all = "camelCase"` struct
/// is what gives multi-word fields (`apiVersion`, `conditionType`) their
/// camelCase spelling in the parser *and* in the generated JSON Schema,
/// without depending on `rename_all_fields` cascading through `schemars`
/// as well as `serde`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum ReadinessCheckSpec {
    /// A `Deployment`'s `Available` condition must be `True`.
    DeploymentAvailable(NamespacedObjectSpec),
    /// A `DaemonSet` must have every desired pod scheduled and ready.
    DaemonSetReady(NamespacedObjectSpec),
    /// A `Job` must have completed successfully.
    JobComplete(NamespacedObjectSpec),
    /// A `ValidatingWebhookConfiguration`/`MutatingWebhookConfiguration`
    /// with this name must exist.
    WebhookConfigurationPresent(NamedObjectSpec),
    /// A custom resource's named condition must equal a given status.
    CustomResourceCondition(CustomResourceConditionSpec),
}

/// A namespaced object named by a [`ReadinessCheckSpec`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamespacedObjectSpec {
    /// The object's namespace.
    pub namespace: String,
    /// The object's name.
    pub name: String,
}

/// A cluster-scoped object named by a [`ReadinessCheckSpec`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamedObjectSpec {
    /// The object's name.
    pub name: String,
}

/// A custom resource whose named condition must reach a given status.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomResourceConditionSpec {
    /// The custom resource's `apiVersion`, for example `kyverno.io/v1`.
    pub api_version: String,
    /// The custom resource's `kind`, for example `ClusterPolicy`.
    pub kind: String,
    /// The custom resource's namespace. Omitted for a cluster-scoped
    /// resource.
    #[serde(default)]
    pub namespace: Option<String>,
    /// The custom resource's name.
    pub name: String,
    /// The condition's `type`, for example `Ready`.
    pub condition_type: String,
    /// The condition's required `status`, typically `"True"`.
    pub status: String,
}

/// The user-facing YAML form of a component's install method.
///
/// This is intentionally minimal: enough to express the two Alpha
/// installers — a Helm chart, or a fixed set of raw manifests — exactly
/// as a user would write them by hand. [`crate::InstallMethod`] is the
/// separate *resolved* form the installer actually consumes; the two are
/// distinct types by design and must not be merged or aliased.
///
/// Represented as an internally tagged enum (a `type` discriminant field
/// alongside the variant's own fields), not the more common
/// externally-tagged `{helm: {...}}` shape: `serde_yaml`/`serde_norway`
/// only support externally tagged enums via an explicit YAML type tag
/// (`!Helm ...`), not a wrapping single-key mapping, so external tagging
/// cannot represent this the way a hand-written YAML config wants to.
/// Internally tagged enums use a format-agnostic buffering strategy that
/// works with a plain mapping, giving both a natural YAML shape:
///
/// ```yaml
/// install:
///   type: helm
///   chart: cert-manager
///   repo: https://charts.jetstack.io
/// ```
///
/// and a strict one: an unrecognized `type` or a misspelled field inside
/// the variant is still a loud, named error.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum InstallMethodSpec {
    /// Install via a Helm chart.
    Helm(HelmInstallSpec),
    /// Install a fixed set of raw Kubernetes manifests.
    Manifests(ManifestsInstallSpec),
}

/// Install method: a Helm chart.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelmInstallSpec {
    /// The chart reference passed to `helm install` (a repo-relative
    /// chart name, a local path, or an `oci://` reference). Only the
    /// repo-relative form is resolvable today — see `repo`.
    pub chart: String,
    /// The Helm repository URL to add/use, if `chart` is a bare chart
    /// name rather than a path or `oci://` reference. Required by
    /// [`crate::resolve_lab`]: local path and `oci://` chart references
    /// remain syntactically valid in `chart` above, but resolution has no
    /// way to act on them without a registered repository, so `repo`
    /// must be set for every Helm install today.
    #[serde(default)]
    pub repo: Option<String>,
    /// The local name to register `repo` under (`helm repo add
    /// <repo_name> <repo_url>`). Defaults to the component's own
    /// resolved name when omitted — see
    /// [`crate::component::resolve_component`]'s documentation. Purely a
    /// local bookkeeping label with no cluster-visible effect, so the
    /// default is safe to leave unset in the common case.
    #[serde(default)]
    pub repo_name: Option<String>,
    /// The chart version passed to `helm install --version`. Required by
    /// [`crate::resolve_lab`] to be an exact pin, never a floating range
    /// — see [`crate::validate::require_pinned_helm_version`]'s
    /// documentation for exactly what counts as floating and why.
    #[serde(default)]
    pub version: Option<String>,
    /// The Helm release name (`helm upgrade --install <release_name>
    /// ...`). Defaults to the component's own resolved name when
    /// omitted.
    #[serde(default)]
    pub release_name: Option<String>,
    /// The namespace to install into. Defaults to the component's own
    /// resolved name when omitted — **that default is not always
    /// correct** and must be overridden whenever a chart's own
    /// convention differs from the component's name: for example
    /// `istio/istiod` and `istio/base` both conventionally install into
    /// `istio-system`, not `istiod`/`base`.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Values override files, resolved against the configuration file's
    /// own directory by [`crate::resolve_lab`] — see
    /// [`ComponentSpec::install`] and [`crate::resolve::config_directory`].
    #[serde(default)]
    pub values_files: Vec<PathBuf>,
    /// Literal `--set-string` key/value overrides passed to `helm
    /// upgrade --install`. Empty by default.
    #[serde(default)]
    pub set_values: BTreeMap<String, String>,
}

/// Install method: a fixed set of raw Kubernetes manifests.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestsInstallSpec {
    /// Manifest file or directory paths, resolved against the
    /// configuration file's own directory by [`crate::resolve_lab`] — see
    /// [`HelmInstallSpec::values_files`]'s documentation; the same
    /// applies here. Must not be empty — validated by
    /// [`crate::resolve_lab`].
    pub paths: Vec<PathBuf>,
}

/// Which fixtures to replay through both environments.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureSelectionSpec {
    /// Glob patterns selecting fixture files, resolved by
    /// [`crate::resolve_lab`] against this configuration file's own
    /// directory. Must not be empty — validated by [`crate::resolve_lab`].
    #[serde(default)]
    pub include: Vec<String>,
}

/// The regression policy: which categories of behavioral difference fail
/// the run, targeted overrides, and latency-regression thresholds.
///
/// Every field defaults independently, so a user who wants the tool's
/// defaults everywhere may omit `policy` from their configuration
/// entirely (see [`LabSpec::policy`]).
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct PolicySpec {
    /// Regression categories that fail the run when observed, named by
    /// their semantic-change wire name (`container_removed`,
    /// `newly_denied`, ...). A [`BTreeSet`] rather than a `Vec` so a
    /// duplicated entry collapses silently and the serialized/schema
    /// representation has a deterministic order.
    ///
    /// A `String` rather than a typed enum, and unvalidated by this
    /// crate: the set of meaningful names belongs to
    /// `admissionlab-diff`, which §1.1 places *above* this crate, so
    /// checking a name here would mean either an inverted dependency or
    /// a second copy of the list that could drift from the first.
    /// `admissionlab_policy::validate_policy_spec` performs that check
    /// instead, and the orchestrator calls it right after
    /// [`crate::resolve_lab`] — before any cluster is created — so an
    /// unknown name still fails at load time rather than silently
    /// matching nothing.
    pub fail_on: BTreeSet<String>,
    /// Targeted exceptions to the blanket `fail_on` policy.
    pub overrides: Vec<PolicyOverrideSpec>,
    /// Thresholds below which a latency increase is not itself treated as
    /// a regression.
    pub latency: LatencyPolicy,
}

/// One targeted exception to the blanket [`PolicySpec::fail_on`] policy.
///
/// `fixtures`, `subject`, and `path` narrow which regressions this
/// override applies to; omitting one leaves that dimension unrestricted.
/// `path` names a location *within* the compared object (for example a
/// JSON-pointer-like field path), not a filesystem path, so it is never
/// resolved against the configuration file's directory.
///
/// `kind` and `severity` are `String`s that this crate does not
/// validate, for the reason [`PolicySpec::fail_on`] gives;
/// `admissionlab_policy::validate_policy_spec` checks both names, checks
/// that `fixtures` is a compilable glob, and rejects an
/// empty-but-present `fixtures`/`subject`/`path` (a restriction nothing
/// could ever satisfy).
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyOverrideSpec {
    /// The regression category this override applies to (matches a
    /// [`PolicySpec::fail_on`] entry).
    pub kind: String,
    /// Restrict this override to fixtures matching this pattern.
    #[serde(default)]
    pub fixtures: Option<String>,
    /// Restrict this override to a specific admission subject (webhook,
    /// controller, or similar).
    #[serde(default)]
    pub subject: Option<String>,
    /// Restrict this override to a specific field path within the
    /// compared object.
    #[serde(default)]
    pub path: Option<String>,
    /// The severity to report this regression as when the override
    /// applies, instead of failing the run outright.
    pub severity: String,
}

/// Thresholds below which a latency increase from baseline to candidate is
/// not itself treated as a regression.
///
/// A candidate observation only counts as a latency regression once it
/// exceeds *both* thresholds: `baseline + absolute_increase` and
/// `baseline * relative_multiplier`. Exact evaluation semantics are Task
/// 4.8's responsibility; this type only carries the configured values.
///
/// `absolute_increase` is written in YAML as a plain integer number of
/// milliseconds (for example `absoluteIncrease: 50`), not `serde`'s
/// default `{secs, nanos}` object representation for [`Duration`] — that
/// default is correct for machine-to-machine formats but not for a
/// configuration file a person hand-writes.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct LatencyPolicy {
    /// The absolute latency increase, in milliseconds, tolerated before a
    /// candidate observation counts as a regression. Defaults to 100
    /// milliseconds when `policy.latency` is omitted (the Alpha default
    /// ROADMAP Task 4.6 Step 3 fixes: a latency increase is reported only
    /// when the candidate is at least 100ms slower *and* at least 2x the
    /// baseline for the same webhook).
    #[serde(with = "duration_millis")]
    #[schemars(with = "u64")]
    pub absolute_increase: Duration,
    /// The multiplier on baseline latency tolerated before a candidate
    /// observation counts as a regression. Defaults to `2.0` when
    /// `policy.latency` is omitted -- see
    /// [`LatencyPolicy::absolute_increase`] for the paired Alpha default
    /// and its source.
    pub relative_multiplier: f64,
}

impl Default for LatencyPolicy {
    fn default() -> Self {
        // ROADMAP Task 4.6 Step 3's Alpha default: report a latency
        // regression only when the candidate is at least 100ms slower
        // *and* at least 2x baseline. A zero/1.0x default would make the
        // conjunctive rule flag every webhook whose latency merely failed
        // to improve, drowning real regressions in noise.
        Self {
            absolute_increase: Duration::from_millis(100),
            relative_multiplier: 2.0,
        }
    }
}

/// Serializes/deserializes [`Duration`] as a plain integer number of
/// milliseconds, for [`LatencyPolicy::absolute_increase`].
///
/// Paired with `#[schemars(with = "u64")]` on the field so the generated
/// JSON Schema describes this same integer representation rather than
/// `Duration`'s own `{secs, nanos}` schema.
mod duration_millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(
        value: &Duration,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        // `as_millis()` returns `u128`; saturate rather than `as`-cast so
        // a (practically impossible, multi-million-year) overflow clamps
        // to `u64::MAX` instead of silently wrapping.
        let millis = u64::try_from(value.as_millis()).unwrap_or(u64::MAX);
        millis.serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Duration, D::Error> {
        let millis = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(millis))
    }
}

/// Placeholder for the gateway conformance suite configuration.
///
/// Reserved by the project's cross-task type registry so
/// [`crate::ResolvedLab::gateway`] has a stable name to carry from the
/// start; [`LabSpec`] has no `gateway` section yet and
/// [`crate::resolve_lab`] always produces `None`. Phase 6 defines this
/// type's real fields, the YAML section that populates it, and the
/// resolution logic that fills it in. Left deliberately empty rather than
/// guessing at fields no task has asked for yet.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GatewaySuiteSpec {}

/// Placeholder for the migration test suite configuration.
///
/// See [`GatewaySuiteSpec`]: same reservation, same owner (Phase 6), same
/// reason for staying empty.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationSuiteSpec {}
