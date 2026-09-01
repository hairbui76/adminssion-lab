//! The **frozen** `admissionlab.io/v1alpha1` lab document: the Public
//! Alpha wire model, kept readable through at least v1.0 (ROADMAP Task
//! 7.1 Step 2).
//!
//! # Frozen means frozen
//!
//! Nothing in this module may change shape again. Not a renamed field,
//! not a new required key, not a relaxed default — an `admissionlab.yaml`
//! that loaded under Public Alpha must keep loading, unchanged, for as
//! long as this module exists. New configuration surface goes to
//! [`crate::v1beta1`] and reaches Alpha documents only through
//! [`crate::migrate_v1alpha1_to_v1beta1`], never by editing anything
//! here.
//!
//! The freeze is *checked*, not merely asserted: `tests/schema.rs`
//! regenerates [`crate::v1alpha1_json_schema`] from [`LabSpec`] and
//! compares it byte-for-byte against the checked-in
//! `schemas/admissionlab-v1alpha1.json`, so any change to this module —
//! or to a shared type in [`crate::model`] that this module references —
//! fails that test.
//!
//! # What is here, and what is not
//!
//! Only the root document and the three types whose **wire spelling**
//! v1beta1 deliberately changed:
//!
//! | v1alpha1 | v1beta1 | renamed |
//! |---|---|---|
//! | [`LatencyPolicy`] | [`crate::v1beta1::LatencyPolicy`] | `absoluteIncrease` -> `absoluteIncreaseMillis` |
//! | [`PolicySpec`] | [`crate::v1beta1::PolicySpec`] | (unchanged itself; carries the [`LatencyPolicy`] above) |
//! | [`GatewaySuiteSpec`] | [`crate::v1beta1::GatewaySuiteSpec`] | `reconciliationTimeout` -> `reconciliationTimeoutMillis` |
//!
//! Every other hand-written type — [`crate::EnvironmentSpec`],
//! [`crate::ComponentSpec`],
//! [`crate::InstallMethodSpec`], [`crate::ReadinessCheckSpec`],
//! [`crate::FixtureSelectionSpec`], [`crate::PolicyOverrideSpec`],
//! [`crate::RouteContract`], [`crate::HttpProbeContract`],
//! [`crate::GatewayEndpointSpec`] — is spelled identically in both
//! versions and is therefore defined once, in [`crate::model`]. See
//! `migrate.rs` for the per-field record of what was reviewed for this
//! freeze and why each name was kept or changed.
//!
//! # Why the root struct is still called `LabSpec`
//!
//! [`V1Alpha1Lab`] is the name ROADMAP Task 7.1 froze in its interface
//! list, and it is the name to use. The *struct* keeps its original
//! identifier because `schemars` derives the checked-in schema's `title`
//! and `$defs` keys from Rust type names: renaming the struct would
//! rewrite `schemas/admissionlab-v1alpha1.json`'s keys, which is exactly
//! the published-artifact churn this freeze exists to prevent. So the
//! struct is `LabSpec` and [`V1Alpha1Lab`] is an alias for it; the two
//! are the same type and either may be used to construct or match.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::model::{
    EnvironmentSpec, FixtureSelectionSpec, GatewayEndpointSpec, PolicyOverrideSpec,
    ReadinessCheckSpec, RouteContract, default_reconciliation_timeout, duration_millis,
    kind_schema,
};

/// The frozen `apiVersion` of a Public Alpha lab document.
///
/// [`crate::load_lab`] accepts only this value;
/// [`crate::load_any_supported_lab`] accepts it *and*
/// [`crate::v1beta1::API_VERSION`], migrating a document carrying this one
/// through [`crate::migrate_v1alpha1_to_v1beta1`] before resolving it.
pub const API_VERSION: &str = "admissionlab.io/v1alpha1";

/// [`LabSpec`] under the name ROADMAP Task 7.1's interface list froze.
///
/// An alias rather than the struct itself — see this module's "Why the
/// root struct is still called `LabSpec`".
pub type V1Alpha1Lab = LabSpec;

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

/// The root of an `admissionlab.yaml` configuration file (frozen
/// `admissionlab.io/v1alpha1` spelling).
///
/// `api_version` and `kind` are plain `String`s in Rust — parsing accepts
/// any value at the type level — because rejecting the wrong value is a
/// semantic check, not a syntactic one; [`crate::load_lab`] validates them
/// against [`API_VERSION`] and [`crate::model::KIND`] immediately after deserializing.
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
    /// Must equal [`crate::model::KIND`]; checked by [`crate::load_lab`] and
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
    /// The Gateway behavior suite: which Gateway API fixtures to persist
    /// in each side's cluster, and what each route is contracted to do.
    /// Omit the section entirely for an admission-only lab (Global
    /// Constraint 8: Public Alpha is admission regression only, so the
    /// overwhelmingly common configuration has no `gateway` section at
    /// all) — [`crate::ResolvedLab::gateway`] is then `None`.
    #[serde(default)]
    pub gateway: Option<GatewaySuiteSpec>,
}

/// The regression policy: which categories of behavioral difference fail
/// the run, targeted overrides, and latency-regression thresholds (frozen
/// `admissionlab.io/v1alpha1` spelling; identical to
/// [`crate::v1beta1::PolicySpec`] except for the type of its `latency`
/// field).
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

/// Thresholds below which a latency increase from baseline to candidate is
/// not itself treated as a regression.
///
/// **Frozen `admissionlab.io/v1alpha1` spelling.** `absoluteIncrease` is
/// spelled `absoluteIncreaseMillis` in `admissionlab.io/v1beta1` — see
/// [`crate::v1beta1::LatencyPolicy`], and `migrate.rs` for the reason.
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

/// The Gateway behavior suite (ROADMAP Task 6.1): the Gateway API
/// fixtures a lab persists in each side's cluster, plus the traffic
/// contract each route is expected to satisfy.
///
/// **Frozen `admissionlab.io/v1alpha1` spelling.** `reconciliationTimeout`
/// is spelled `reconciliationTimeoutMillis` in
/// `admissionlab.io/v1beta1` — see [`crate::v1beta1::GatewaySuiteSpec`],
/// which is also the type [`crate::ResolvedLab::gateway`] carries and the
/// one whose documentation the sections below describe; `migrate.rs`
/// carries the reason for the rename.
///
/// # Why this type lives in `admissionlab-spec` and not `admissionlab-gateway`
///
/// ROADMAP Task 6.1 lists `crates/admissionlab-gateway/src/model.rs` as
/// this type's file, but §1.2's cross-task type registry *also* freezes
/// [`crate::ResolvedLab::gateway`] as an `Option<GatewaySuiteSpec>`.
/// Those two statements can only both hold if exactly one crate defines
/// the type and the other names it — and the direction is forced:
/// `admissionlab-gateway` depends on `admissionlab-core`, which depends
/// on this crate, so a `spec -> gateway` edge would be a dependency
/// cycle Cargo rejects outright. The canonical definition therefore
/// lives here, next to every other type a user hand-writes in
/// `admissionlab.yaml`, and `admissionlab_gateway::model` **re-exports
/// this exact type** rather than declaring a second one — §1.2's "these
/// names are canonical" rule forbids a synonym, and a parallel
/// `GatewayRouteContract`-style twin is exactly the drift that rule
/// exists to prevent. Everything Phase 6 adds that a *user never writes*
/// (observed conditions, reconciliation evidence, probe results) is
/// declared in `admissionlab-gateway` itself; only the hand-written
/// configuration surface is here. That is the same split this workspace
/// already uses between [`crate::ComponentSpec`] (hand-written) and
/// [`crate::ResolvedComponent`] (runtime).
///
/// # One type for both the raw and the resolved stage
///
/// Unlike [`EnvironmentSpec`]/[`crate::ResolvedEnvironment`], there is
/// no separate resolved twin, because §1.2 freezes the single name
/// `GatewaySuiteSpec` on [`crate::ResolvedLab`]. The two stages are
/// distinguished by *where the value is read from*, and the difference
/// is confined to one field:
///
/// - in [`LabSpec::gateway`], every [`GatewaySuiteSpec::manifests`] path
///   is exactly as the user wrote it (possibly relative);
/// - in [`crate::ResolvedLab::gateway`], every path has been joined onto
///   the configuration file's own directory by [`crate::resolve_lab`].
///
/// Nothing else changes between the stages: [`RouteContract`] and
/// [`crate::HttpProbeContract`] contain no filesystem paths at all, and
/// [`crate::resolve_lab`] validates them in place rather than rewriting
/// them.
///
/// # Traffic expectations are not regression policy
///
/// [`crate::HttpProbeContract::expected_status`] and
/// [`crate::HttpProbeContract::expected_backend`] say what a route *should do*.
/// They deliberately carry no severity, no `failOn` category, and no
/// "expected regression" marker: Global Constraint 6 keeps
/// classification out of vendor-supplied metadata, and this project
/// already has exactly one place where a behavioral difference is graded
/// ([`PolicySpec`], evaluated by `admissionlab-policy`) and exactly one
/// place where a known-and-accepted difference is recorded (the
/// expectations file, [`LabSpec::expectations_file`]). A severity field
/// here would be a second, competing grader whose answer could disagree
/// with the first for the same run — see [`PolicyOverrideSpec`] for the
/// vocabulary that already exists for "this difference is acceptable".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewaySuiteSpec {
    /// Kubernetes manifest files defining the Gateway API fixture:
    /// namespaces, backends, `GatewayClass`, `Gateway`, `HTTPRoute`, and
    /// anything else the suite needs. Resolved against the configuration
    /// file's own directory by [`crate::resolve_lab`], the same as every
    /// other path in the document (see [`crate::resolve::config_directory`]).
    /// Must not be empty — validated by [`crate::resolve_lab`], for the
    /// same reason [`crate::ManifestsInstallSpec::paths`] must not be: a suite
    /// that installs nothing and calls that success is a quiet no-op.
    ///
    /// **These manifests are persisted, not dry-run.** Global Constraint
    /// 16 makes server-side dry-run the authoritative *admission*
    /// fixture mode; ROADMAP Phase 6's own execution note carves out
    /// Gateway fixtures explicitly, because controller reconciliation
    /// and data-plane programming require durable resources. The
    /// isolation that makes that safe is the disposable cluster itself —
    /// see `admissionlab_gateway::apply`'s module documentation, which
    /// is where that statement is enforced rather than merely repeated.
    pub manifests: Vec<PathBuf>,
    /// The routes this suite observes and probes. Must not be empty, and
    /// every [`RouteContract::id`] must be unique within the suite —
    /// both validated by [`crate::resolve_lab`].
    pub routes: Vec<RouteContract>,
    /// How long to wait for a route to reach a stable, current status on
    /// each side before recording it as unconverged.
    ///
    /// Written in YAML as a plain integer number of milliseconds
    /// (`reconciliationTimeout: 120000`), matching
    /// [`LatencyPolicy::absolute_increase`]'s established style rather
    /// than serde's default `{secs, nanos}` object shape. Defaults to
    /// [`crate::DEFAULT_RECONCILIATION_TIMEOUT`] when omitted, and must be
    /// non-zero — a zero timeout could never observe the two-poll
    /// stability window `admissionlab_gateway::reconcile` requires, so
    /// it would report every route as unconverged without ever having
    /// looked.
    #[serde(with = "duration_millis", default = "default_reconciliation_timeout")]
    #[schemars(with = "u64")]
    pub reconciliation_timeout: Duration,
    /// How to find the `Service` that fronts each Gateway's data plane,
    /// so a probe can be sent through it.
    ///
    /// `None` — the default — means **no traffic probe is ever sent**.
    /// Every [`RouteContract`] still has its reconciliation observed and
    /// compared; a contract that also declares [`RouteContract::probes`]
    /// has each of them recorded as an explicit skip rather than
    /// silently dropped, because a run that cannot reach a data plane
    /// has not observed that route's traffic behavior and must not
    /// imply it did (Global Constraint 15).
    ///
    /// # Why this is written here and not inherited from a recipe
    ///
    /// [`crate::GatewayEndpointStrategy`]'s own documentation calls this
    /// *install metadata*, and a certified recipe declares it
    /// (`recipes/istio-gateway/recipe.yaml`'s `gatewayEndpoint:` block
    /// parses into the very same [`GatewayEndpointSpec`] this field
    /// takes). But `admissionlab.yaml` has no recipe resolution:
    /// [`crate::ComponentSpec::recipe`] is carried through unresolved, and
    /// every component in a lab file spells out its own
    /// `install`/`readiness` by hand. A lab that hand-writes how Istio
    /// is installed must equally hand-write where Istio's data plane
    /// is; inferring it from a recipe the run never loaded would be a
    /// guess wearing a citation. When recipe-driven components land,
    /// this field becomes the override rather than the only source, and
    /// the type is already the one a recipe produces.
    #[serde(default)]
    pub gateway_endpoint: Option<GatewayEndpointSpec>,
    /// Conditions the suite's own manifests must satisfy after they are
    /// applied and before any route's behavior is observed.
    ///
    /// Empty by default, and the same vocabulary [`crate::ComponentSpec::readiness`]
    /// uses — for the same reason, restated one layer down: applying a
    /// manifest returns when its objects *exist*, never when the
    /// workloads they describe are running. A backend `Deployment` that
    /// is not yet `Available` has no endpoints, so a request routed to
    /// it is answered by the data plane's own `503` — a statement about
    /// this run's timing rather than about the route, and one that would
    /// differ between the two sides on every run.
    ///
    /// # Gate on what the fixture owns
    ///
    /// The right entries here are the suite's *own* objects: the echo
    /// backends, a `Job` that seeds them. Gating on something the
    /// implementation under test provisions (Istio's generated
    /// data-plane `Deployment`, say) is a real trade and is sometimes
    /// still correct — but it exchanges "reported as a behavior
    /// difference" for "fails the run before a comparison happens", so
    /// it belongs only where that object's absence is *not* the thing
    /// being compared. Nothing here is inferred; a suite that declares
    /// no readiness waits for nothing.
    #[serde(default)]
    pub readiness: Vec<ReadinessCheckSpec>,
}
