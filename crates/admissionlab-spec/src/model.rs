//! The **version-independent** vocabulary of the `admissionlab.yaml`
//! configuration contract: every hand-written type whose wire spelling is
//! identical in every `apiVersion` this crate reads.
//!
//! # The version split (ROADMAP Task 7.1)
//!
//! There are two supported document versions, and three modules:
//!
//! - [`crate::v1alpha1`] owns the **frozen** Public Alpha root
//!   ([`crate::v1alpha1::LabSpec`], aliased [`crate::V1Alpha1Lab`]) plus
//!   the three types whose wire spelling v1beta1 deliberately changed.
//! - [`crate::v1beta1`] owns the current root ([`crate::V1Beta1Lab`]) and
//!   that same set of three types in their frozen Beta spelling.
//! - **This module** owns everything else: the types both versions
//!   deserialize *identically*, from [`EnvironmentSpec`] and
//!   [`ComponentSpec`] down to
//!   [`HttpProbeContract`]. A type belongs here exactly while every
//!   supported version spells it the same way; the moment a new version
//!   renames a field inside one, that type is copied into the version
//!   modules that disagree and stops being shared.
//!
//! That rule is not enforced by convention alone: `schemas/admissionlab-v1alpha1.json`
//! is regenerated from [`crate::v1alpha1::LabSpec`] and compared
//! byte-for-byte by `tests/schema.rs`, so *any* change to a shared type's
//! shape — a renamed field, a new required key — fails that test with the
//! Alpha schema it would have silently broken. Sharing is safe precisely
//! because the freeze is checked, not promised.
//!
//! For back-compatibility, the current version's three split types are
//! re-exported from this module (and from the crate root) under their
//! historical bare names, so `admissionlab_spec::PolicySpec` and
//! `admissionlab_spec::model::EnvironmentSpec` keep resolving exactly as
//! they always have — to the *current* (Beta) spelling, which is what
//! [`crate::ResolvedLab`] carries.
//!
//! # Properties every type here still has
//!
//! - **Strict by construction.** Every struct carries
//!   `#[serde(deny_unknown_fields)]`, so a misspelled key (`candiate`
//!   instead of `candidate`) is a hard parse error rather than a silently
//!   ignored typo. [`crate::load_any_supported_lab`] is what turns that
//!   error into a message naming the file and the offending field.
//! - **`camelCase` on the wire, `snake_case` in Rust.** Every field carries an
//!   implicit or explicit `rename_all = "camelCase"` so users write
//!   `apiVersion`/`expectationsFile`/`failOn`, matching typical YAML/JSON
//!   convention, while Rust code reads idiomatic `snake_case`.
//!   [`crate::v1alpha1_json_schema`] and [`crate::v1beta1_json_schema`]
//!   generate their schemas from these same derives, so a schema can
//!   never drift from the camelCase spelling users actually type.
//!
//! This module defines the *raw* shape only: values exactly as written in
//! the file, with relative paths unresolved and no cross-field validation
//! applied. [`crate::resolve_lab`] (see `resolve.rs`) is what turns a
//! parsed document into a fully validated, path-resolved
//! [`crate::ResolvedLab`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;

pub use crate::v1alpha1::API_VERSION;
pub use crate::v1beta1::{GatewaySuiteSpec, LatencyPolicy, PolicySpec};

/// The only `kind` value a lab document may carry, in **every** supported
/// `apiVersion` — [`crate::v1alpha1::API_VERSION`] and
/// [`crate::v1beta1::API_VERSION`] both name their root document `Lab`.
/// A version promotion changes the group/version, never the kind.
pub const KIND: &str = "Lab";

/// The `kind` a *fixture matrix* declaration document carries, under the
/// [`API_VERSION`] group/version (that is, `admissionlab.io/v1alpha1`).
///
/// This crate does **not** parse or validate such a document — the model
/// lives in `admissionlab_fixtures::matrix`, which is where fixture
/// discovery (the only thing that ever reads one) also lives, and which
/// depends on this crate rather than the other way around. What lives
/// here is only the *name*, and keeping every kind that may legally
/// appear under `admissionlab.io/v1alpha1` in one place is what stops the
/// `Lab` check in [`crate::validate::api_version_and_kind`] and the
/// `FixtureMatrix` check in
/// `admissionlab_fixtures::matrix::classify_document` from drifting into
/// disagreement about which kinds exist at all.
///
/// **A fixture matrix is a separate document with its own version.**
/// ROADMAP Task 7.1 promotes the *lab* document to
/// `admissionlab.io/v1beta1`; it does not promote the `FixtureMatrix`
/// document, which is `admissionlab-fixtures`' contract to version (as
/// the `Expectations` document is `admissionlab-policy`'s). Both still
/// declare `admissionlab.io/v1alpha1`, and a v1beta1 lab file selects
/// v1alpha1 fixture matrices without contradiction: they are different
/// kinds of document that merely share a group.
///
/// A fixture matrix is declared as a document *inside the fixture tree*,
/// selected by the same [`FixtureSelectionSpec::include`] globs as every
/// other fixture — deliberately not as a new field in this
/// configuration model. See [`FixtureSelectionSpec::include`] for that
/// decision's reasoning in full.
pub const FIXTURE_MATRIX_KIND: &str = "FixtureMatrix";

/// Schema-only override for each version root's `kind` property: a JSON
/// Schema `const` locking it to [`KIND`], so an editor validating a YAML
/// file against the generated schema flags a wrong `kind` directly,
/// instead of only failing at load time. Does not affect deserialization
/// — the field's Rust type stays `String` and the loader still performs
/// the authoritative runtime check.
///
/// Shared by [`crate::v1alpha1::LabSpec`] and [`crate::V1Beta1Lab`]
/// because [`KIND`] itself is shared; each version supplies its own
/// `apiVersion` override, since that value is exactly what differs.
pub(crate) fn kind_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "const": KIND
    })
}

/// One side (baseline or candidate) of a comparison: a Kubernetes version
/// plus the extra components installed on top of it.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentSpec {
    /// The Kubernetes version to provision, for example `"1.30.4"`. Must
    /// not be empty — validated by [`crate::resolve_lab`].
    pub kubernetes: String,
    /// Container images already present in the operator's **local**
    /// image store that must be side-loaded into this side's ephemeral
    /// cluster before anything is installed into it.
    ///
    /// Empty by default, and empty for every lab whose workloads come
    /// from a registry: a `kind` node pulls those itself. The field
    /// exists for the case a registry cannot answer at all — a manifest
    /// referencing an image that was built locally and never pushed,
    /// which is exactly what Admission Lab's own Gateway suites do (the
    /// deterministic echo backend in `crates/admissionlab-echo` is built
    /// by `scripts/build-test-images.sh` and tagged
    /// `admissionlab-echo:dev`, and the manifests that reference it use
    /// `imagePullPolicy: IfNotPresent`). Without this list such a
    /// manifest fails inside the run with an `ErrImageNeverPull` that
    /// reads as a broken fixture rather than as a missing image.
    ///
    /// Each entry is a plain image reference (`name:tag`), passed
    /// through verbatim as one argument to the cluster backend's own
    /// side-load command — never interpolated into a shell string
    /// (Global Constraint 12). Loading happens once, immediately after
    /// the cluster is created and before the first component is
    /// installed, and a failure to load fails the cluster rather than
    /// being discovered later as a scheduling error.
    ///
    /// **This is not a way to skip a registry for convenience.** Both
    /// sides list their own images independently, so a lab that loads
    /// different images per side is expressible and is exactly as
    /// visible in the configuration as a different chart version would
    /// be.
    #[serde(default)]
    pub images: Vec<String>,
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
    ///
    /// These same patterns also select *fixture matrix* declarations: a
    /// matched document whose `apiVersion` is `admissionlab.io/v1alpha1`
    /// and whose `kind` is `FixtureMatrix` is not replayed as a
    /// Kubernetes object. It names one base document plus an explicit,
    /// hand-written list of RFC 6902 JSON Patch cases, and fixture
    /// discovery expands it into one ordinary fixture per case (see
    /// `admissionlab_fixtures::matrix`). There is deliberately no
    /// separate `matrices:` field here: a matrix lives in the fixture
    /// tree next to the fixtures it varies, so one include list stays
    /// the single answer to "which fixtures does this lab replay?", and
    /// a matrix cannot be selected by a rule the ordinary fixtures
    /// around it are not.
    ///
    /// A matrix's `base` document is *only* a template. It is replayed
    /// as a fixture in its own right if and only if it independently
    /// matches one of these patterns — this list is the whole rule, and
    /// declaring a document as a matrix base neither adds it to nor
    /// removes it from the replay set. (A matrix's own `base` path is
    /// resolved against the matrix document's directory, not against
    /// this configuration file's, so a fixture tree containing matrices
    /// stays relocatable independently of where the lab that selects it
    /// lives.)
    #[serde(default)]
    pub include: Vec<String>,
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

/// Serializes/deserializes [`Duration`] as a plain integer number of
/// milliseconds, for every scalar duration in the configuration model
/// (`policy.latency.absoluteIncreaseMillis`,
/// `gateway.reconciliationTimeoutMillis`, and their frozen v1alpha1
/// spellings).
///
/// Paired with `#[schemars(with = "u64")]` on the field so the generated
/// JSON Schema describes this same integer representation rather than
/// `Duration`'s own `{secs, nanos}` schema.
///
/// `pub(crate)` so both version modules can name it: the *encoding* is
/// version-independent even where the field's wire name is not.
pub(crate) mod duration_millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<S: Serializer>(
        value: &Duration,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        // `as_millis()` returns `u128`; saturate rather than `as`-cast so
        // a (practically impossible, multi-million-year) overflow clamps
        // to `u64::MAX` instead of silently wrapping.
        let millis = u64::try_from(value.as_millis()).unwrap_or(u64::MAX);
        millis.serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Duration, D::Error> {
        let millis = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(millis))
    }
}

/// The hand-written form of [`crate::GatewayEndpointStrategy`]: how to
/// find the `Service` fronting a Gateway's data plane, exactly as it is
/// written in YAML.
///
/// ```yaml
/// gatewayEndpoint:
///   type: serviceBySelector
///   namespace: "{gatewayNamespace}"
///   selector:
///     gateway.networking.k8s.io/gateway-name: "{gatewayName}"
///   portName: http
/// ```
///
/// # One raw type, two documents
///
/// This is the raw shape for **both** `admissionlab.yaml`'s
/// [`GatewaySuiteSpec::gateway_endpoint`] and a recipe's own
/// `gatewayEndpoint:` block: `admissionlab_recipes::model` names this
/// type rather than declaring a second one. Two deserializable shapes
/// for one resolved [`crate::GatewayEndpointStrategy`] is precisely the
/// synonym §1.2 forbids — they would be free to drift a field apart,
/// and the recipe half and the lab half would then mean subtly
/// different things while looking identical in YAML. Defining it here
/// is what makes that impossible, and it is the same direction
/// [`crate::GatewayEndpointStrategy`] itself already resolved: the leaf
/// crate both producers and both consumers can name.
///
/// Internally tagged (a `type:` discriminant beside the variant's own
/// fields), matching [`InstallMethodSpec`] and [`ReadinessCheckSpec`] —
/// see the first for why `serde_norway` cannot represent a natural
/// `type:` key with external tagging. `deny_unknown_fields` at this
/// level too, so an invented key is a parse error here exactly as it is
/// everywhere else in this schema.
///
/// Both ports are optional and neither is defaulted at parse time: what
/// they mean is a question about a `Service` that does not exist yet
/// when a document is read, so it is answered where the `Service` is
/// actually read (`admissionlab_gateway::endpoint`). Validating them
/// here would mean guessing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum GatewayEndpointSpec {
    /// Find the data-plane `Service` by label selector — the form this
    /// should normally take (see [`crate::GatewayEndpointStrategy`]'s
    /// "Prefer the selector to the name").
    ServiceBySelector {
        /// The namespace to search, usually `"{gatewayNamespace}"`.
        namespace: String,
        /// Label requirements every candidate `Service` must satisfy.
        /// Keys are literal; only values are substituted.
        selector: BTreeMap<String, String>,
        /// Which of the matched `Service`'s ports to use, by name.
        #[serde(default)]
        port_name: Option<String>,
        /// Which of the matched `Service`'s ports to use, by number.
        #[serde(default)]
        port: Option<u16>,
    },
    /// Find the data-plane `Service` by an exact name.
    ServiceByName {
        /// The `Service`'s namespace.
        namespace: String,
        /// The `Service`'s name.
        name: String,
        /// See [`GatewayEndpointSpec::ServiceBySelector::port_name`].
        #[serde(default)]
        port_name: Option<String>,
        /// See [`GatewayEndpointSpec::ServiceBySelector::port`].
        #[serde(default)]
        port: Option<u16>,
    },
}

/// The default [`GatewaySuiteSpec::reconciliation_timeout`]: two
/// minutes.
///
/// Chosen to be comfortably longer than a healthy Istio Gateway
/// reconciliation (`Accepted` and `Programmed` normally settle in
/// seconds once the control plane is running) while still bounded, so a
/// stuck controller fails the run in bounded time instead of hanging it.
/// A timeout is never itself a regression verdict — see
/// `admissionlab_gateway::reconcile`, which returns explicit
/// `converged: false` evidence for the comparator to interpret.
pub const DEFAULT_RECONCILIATION_TIMEOUT: Duration = Duration::from_secs(120);

/// [`GatewaySuiteSpec::reconciliation_timeout`]'s `serde` default. A
/// function rather than `#[serde(default)]` because [`Duration`]'s own
/// `Default` is zero, which this field explicitly rejects.
pub(crate) const fn default_reconciliation_timeout() -> Duration {
    DEFAULT_RECONCILIATION_TIMEOUT
}

/// One route's reconciliation and traffic contract: which `HTTPRoute`,
/// attached to which `Gateway`, and what requests through it are
/// expected to do.
///
/// # Gateway identity is always explicit
///
/// `gateway_namespace`/`gateway_name` are required fields with no
/// default and no inference (ROADMAP Task 6.1 Step 2). Admission Lab
/// never guesses the target `Gateway` from "the first `Gateway` in the
/// manifest directory", from the route's own `parentRefs`, or from a
/// single-Gateway fixture happening to be unambiguous. Two reasons, both
/// load-bearing: a fixture that installs two Gateways (the realistic
/// migration case Phase 7 exists for) has no defensible "first"; and a
/// contract that reads its target out of the fixture it is testing can
/// never detect the fixture pointing at the wrong Gateway, because it
/// would follow the fixture there. The contract states the expectation;
/// the cluster supplies the observation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteContract {
    /// A stable identifier for this contract, unique within the suite.
    /// Used to correlate the same contract's baseline and candidate
    /// results (see `GatewayCaseComparison` in §1.2's registry), so it
    /// plays the role [`ComponentSpec::name`] plays for components and
    /// must not be empty.
    pub id: String,
    /// The namespace of the `Gateway` this route attaches to. Explicit
    /// and required — see this type's documentation.
    pub gateway_namespace: String,
    /// The name of the `Gateway` this route attaches to. Explicit and
    /// required — see this type's documentation.
    pub gateway_name: String,
    /// The namespace of the `HTTPRoute` under contract.
    pub route_namespace: String,
    /// The name of the `HTTPRoute` under contract.
    pub route_name: String,
    /// Which of the `Gateway`'s listeners this contract is about, by the
    /// listener's `name` — Gateway API's `parentRef.sectionName`.
    ///
    /// `None` means "whichever listener the route attached to", which is
    /// unambiguous only while the route reports exactly one parent
    /// status entry for this Gateway. `admissionlab_gateway::conditions`
    /// reports an ambiguous match as ambiguous rather than picking one
    /// (Global Constraint 15), so a multi-listener Gateway needs this
    /// field set.
    #[serde(default)]
    pub listener_name: Option<String>,
    /// The HTTP probes to send through this route once it reconciles.
    /// May be empty: a contract that only asserts the route reconciles
    /// (its `Accepted`/`ResolvedRefs` conditions) is a meaningful,
    /// self-contained test, unlike an empty `manifests` list which
    /// installs nothing at all.
    #[serde(default)]
    pub probes: Vec<HttpProbeContract>,
}

/// One HTTP request to send through a reconciled route, and what the
/// response is expected to look like.
///
/// See [`GatewaySuiteSpec`]'s "Traffic expectations are not regression
/// policy" section for why nothing here grades a mismatch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpProbeContract {
    /// The `Host` header to send, which is what a Gateway API listener's
    /// `hostname` and an `HTTPRoute`'s `hostnames` are matched against.
    /// Required and non-empty: a Gateway that routes purely on path
    /// still needs *some* authority in the request, and leaving it
    /// implicit would make the probe's meaning depend on whatever
    /// address the data plane happened to be reached at.
    pub host: String,
    /// The request path, which must begin with `/` — the same shape an
    /// `HTTPRoute`'s `matches[].path.value` takes. Validated by
    /// [`crate::resolve_lab`].
    pub path: String,
    /// The HTTP method, uppercase, from [`ALLOWED_HTTP_METHODS`].
    /// Validated by [`crate::resolve_lab`]; see that constant for why
    /// the list is closed and why case matters.
    pub method: String,
    /// Extra request headers to send, beyond `Host` (which is `host`
    /// above). A [`BTreeMap`] rather than a `Vec` of pairs so the
    /// serialized/schema order is deterministic and a repeated header
    /// name cannot silently mean two different things. Empty by
    /// default.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// The HTTP status code the response is expected to carry. Must be a
    /// real HTTP status code (`100..=599`) — validated by
    /// [`crate::resolve_lab`]; see [`is_valid_http_status`].
    pub expected_status: u16,
    /// Which backend the request is expected to reach, as that backend
    /// reports itself (ROADMAP Task 6.5's deterministic echo server
    /// answers with its own identity). `None` means the contract does
    /// not constrain *which* backend answered, only the status — a real
    /// and sometimes correct answer for a probe asserting a 404 or a
    /// 403, where no backend was reached at all.
    #[serde(default)]
    pub expected_backend: Option<String>,
}

/// The closed set of HTTP methods [`HttpProbeContract::method`] accepts,
/// uppercase.
///
/// **Provenance, not invention:** this is exactly the Gateway API v1
/// `HTTPMethod` enumeration (`GET`, `HEAD`, `POST`, `PUT`, `DELETE`,
/// `CONNECT`, `OPTIONS`, `TRACE`, `PATCH`) — the same nine values an
/// `HTTPRoute`'s own `matches[].method` field accepts, which is in turn
/// the eight methods RFC 9110 defines plus RFC 5789's `PATCH`. Keeping
/// the probe vocabulary identical to the routing vocabulary means a
/// probe can always be written for any method a route can match on, and
/// never for one it cannot.
///
/// **An allow-list, not a deny-list**, for the reason
/// [`crate::validate::require_pinned_helm_version`] gives for its own
/// grammar: a typo (`GTE`) and a method this project cannot route on
/// (`LINK`) are both rejected by the same rule, without anyone having to
/// enumerate every wrong answer.
///
/// **Case-sensitive.** RFC 9110 §9.1 defines the method token as
/// case-sensitive and the registered methods as uppercase, and Gateway
/// API's own `HTTPMethod` enum lists only the uppercase spellings — so
/// `get` is not a lowercase `GET`, it is a method no conformant Gateway
/// will ever match. Accepting it here by up-casing would make a
/// configuration that silently probes something other than what it says.
pub const ALLOWED_HTTP_METHODS: [&str; 9] = [
    "CONNECT", "DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT", "TRACE",
];

/// Whether `status` is a syntactically real HTTP status code.
///
/// RFC 9110 §15 fixes the three-digit range at `100..=599` and reserves
/// each hundred for a class (`1xx` informational through `5xx` server
/// error); no code outside that range can ever appear on the wire, so a
/// contract naming one could never be satisfied by any response. This
/// deliberately does **not** check the code against a registry of
/// *assigned* codes: `599` is unassigned but perfectly reachable from a
/// misbehaving proxy, and a lab whose whole purpose is observing what a
/// stack really returns must be able to write that expectation down.
///
/// `pub` because `admissionlab_gateway` re-exports it alongside the
/// contract types: the code that later compares an *observed* status
/// against a contract should test membership with the same predicate
/// that admitted the contract in the first place.
#[must_use]
pub const fn is_valid_http_status(status: u16) -> bool {
    // `matches!` with a range pattern rather than `(100..=599).contains(&status)`:
    // `RangeInclusive::contains` is not a `const fn`, and this predicate is
    // wanted in const context.
    matches!(status, 100..=599)
}

/// Placeholder for the migration test suite configuration.
///
/// See [`GatewaySuiteSpec`]: same reservation, same owner (Phase 6), same
/// reason for staying empty.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationSuiteSpec {}
