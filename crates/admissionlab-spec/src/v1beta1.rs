//! The **frozen** `admissionlab.io/v1beta1` lab document: the Public Beta
//! configuration contract ROADMAP Task 7.1 froze, still read (and still
//! the source of every type the stable [`crate::v1`] shares) after Task
//! 9.1 promoted the current version past it.
//!
//! # What "frozen" commits this project to
//!
//! Every field name below was reviewed once, deliberately, for the long
//! haul — see `migrate.rs` for the field-by-field record, including the
//! two names that were changed on the way here and the ones that were
//! examined and deliberately kept. From this point on the Beta contract
//! grows only by *addition* (a new optional field with a default that
//! preserves existing behavior); a rename or a removal needs a new
//! `apiVersion` and a new migration, exactly as v1alpha1 -> v1beta1 did.
//!
//! # What is here, and what is not
//!
//! Only the root document ([`V1Beta1Lab`]) and the types v1alpha1 does
//! not have *in this spelling*, which is two groups:
//!
//! - the types whose wire spelling differs from [`crate::v1alpha1`]'s:
//!   [`LatencyPolicy`] (which is what changed), [`PolicySpec`] (which
//!   carries it), and [`GatewaySuiteSpec`];
//! - the types v1alpha1 does not have **at all**, because they were
//!   added after the Alpha freeze: [`MigrationSuiteSpec`],
//!   [`MigrationCaseSpec`], and [`NonPortableFeatureExpectation`]
//!   (ROADMAP Task 8.3). An Alpha document maps to `migration: None`;
//!   see [`V1Beta1Lab::migration`] for why that is a translation rather
//!   than an invented default.
//!
//! Everything else — [`crate::EnvironmentSpec`],
//! [`crate::ComponentSpec`], [`crate::InstallMethodSpec`],
//! [`crate::ReadinessCheckSpec`], [`crate::FixtureSelectionSpec`],
//! [`crate::PolicyOverrideSpec`], [`crate::RouteContract`],
//! [`crate::HttpProbeContract`], [`crate::GatewayEndpointSpec`] — is
//! spelled identically in both versions and is defined once, in
//! [`crate::model`].
//!
//! # These types are still the ones the rest of the workspace sees
//!
//! [`crate::ResolvedLab`] is version-independent by construction: every
//! supported document is migrated forward to [`crate::v1::V1Lab`] before
//! [`crate::resolve_lab`] runs, so there is exactly one resolver and one
//! resolved shape, and no crate above this one ever names an
//! `apiVersion`.
//!
//! The stable version renamed nothing, so [`crate::v1`] re-exports every
//! type below rather than declaring copies of them — which is why they
//! are *also* still re-exported from [`crate::model`] and from the crate
//! root under their historical bare names ([`crate::PolicySpec`],
//! [`crate::GatewaySuiteSpec`], ...). "The current version's spelling" is
//! what those names have always meant, and neither promotion changed
//! which type the resolved model carries. The day a `v2` disagrees about
//! one of them, that type is copied into `v2` and these names follow the
//! new current version, exactly as they followed [`crate::v1alpha1`] to
//! here.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::model::{
    EnvironmentSpec, FixtureSelectionSpec, GatewayEndpointSpec, HttpProbeContract,
    PolicyOverrideSpec, ReadinessCheckSpec, RouteContract, default_reconciliation_timeout,
    duration_millis, kind_schema,
};

/// The `apiVersion` of a Public Beta lab document, and the only value
/// [`crate::load_any_supported_lab`] accepts without migrating first.
pub const API_VERSION: &str = "admissionlab.io/v1beta1";

/// Schema-only override for [`V1Beta1Lab::api_version`]: a JSON Schema
/// `const` locking the property to [`API_VERSION`], so an editor
/// validating a YAML file against the generated schema flags a wrong
/// `apiVersion` (for example `admissionlab.io/v1beta9`) directly, instead
/// of only failing at load time. Does not affect deserialization — the
/// field's Rust type stays `String` and
/// [`crate::load_any_supported_lab`] still performs the authoritative
/// runtime check.
fn api_version_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "const": API_VERSION
    })
}

/// The root of an `admissionlab.yaml` configuration file.
///
/// `api_version` and `kind` are plain `String`s in Rust — parsing accepts
/// any value at the type level — because rejecting the wrong value is a
/// semantic check, not a syntactic one;
/// [`crate::load_any_supported_lab`] validates them against
/// [`API_VERSION`] and [`crate::model::KIND`] immediately after
/// deserializing.
/// The generated JSON Schema additionally `const`-locks both properties
/// (see [`api_version_schema`]/[`kind_schema`]), so an editor validating
/// against the schema also catches a wrong value, without changing what
/// this Rust type accepts.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct V1Beta1Lab {
    /// Must equal [`API_VERSION`]; checked by
    /// [`crate::load_any_supported_lab`] and `const`-locked in the
    /// generated schema (see [`api_version_schema`]).
    #[schemars(schema_with = "api_version_schema")]
    pub api_version: String,
    /// Must equal [`crate::model::KIND`]; checked by
    /// [`crate::load_any_supported_lab`] and `const`-locked in the
    /// generated schema (see [`crate::model::kind_schema`]).
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
    /// The Ingress-to-Gateway migration suite (ROADMAP Task 8.3): pairs
    /// of hand-written baseline `Ingress` manifests and candidate
    /// Gateway API manifests that are meant to behave the same, plus the
    /// probes that check whether they do.
    ///
    /// Omit the section entirely — as every lab that is not migrating
    /// off `Ingress` does — and [`crate::ResolvedLab::migration`] is
    /// `None`.
    ///
    /// # This section exists only in `v1beta1`
    ///
    /// `admissionlab.io/v1alpha1` has no `migration:` key and never
    /// will: it is frozen (see `migrate.rs`), and Phase 8 is v1.0 work
    /// that postdates the freeze. That is not a gap in the migration —
    /// it is the *addition-only* rule the Beta freeze committed to,
    /// working as intended: an Alpha document maps to `migration: None`,
    /// which is exactly what an Alpha document meant, and no value has
    /// to be invented for a field its author could not have written.
    #[serde(default)]
    pub migration: Option<MigrationSuiteSpec>,
}

/// The regression policy: which categories of behavioral difference fail
/// the run, targeted overrides, and latency-regression thresholds.
///
/// Every field defaults independently, so a user who wants the tool's
/// defaults everywhere may omit `policy` from their configuration
/// entirely (see [`V1Beta1Lab::policy`]).
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
/// A candidate observation only counts as a latency regression once it
/// exceeds *both* thresholds: `baseline + absolute_increase` and
/// `baseline * relative_multiplier`. Exact evaluation semantics are Task
/// 4.8's responsibility; this type only carries the configured values.
///
/// `absolute_increase` is written in YAML as a plain integer number of
/// milliseconds (`absoluteIncreaseMillis: 50`), not `serde`'s default
/// `{secs, nanos}` object representation for [`Duration`] — that default
/// is correct for machine-to-machine formats but not for a configuration
/// file a person hand-writes.
///
/// # Why the wire name carries the unit and the Rust name does not
///
/// The YAML value is a bare integer, so `absoluteIncrease: 50` said
/// nothing about whether 50 meant milliseconds or seconds, and the two
/// readings differ by 1000x — an off-by-a-factor bug that fails silently
/// as "the threshold is never (or always) crossed" rather than loudly as
/// a parse error. Kubernetes' own API settles this the same way
/// (`timeoutSeconds`, `periodSeconds`, `initialDelaySeconds`), so
/// `admissionlab.io/v1beta1` spells the field `absoluteIncreaseMillis`.
/// The *Rust* field stays `absolute_increase`: its type is [`Duration`],
/// which already carries the unit, and suffixing a `Duration` with
/// `_millis` would state a precision the type does not have. That is the
/// whole of the divergence between the two spellings, and it is the
/// reason for it.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct LatencyPolicy {
    /// The absolute latency increase, in milliseconds, tolerated before a
    /// candidate observation counts as a regression. Defaults to 100
    /// milliseconds when `policy.latency` is omitted (the Alpha default
    /// ROADMAP Task 4.6 Step 3 fixes: a latency increase is reported only
    /// when the candidate is at least 100ms slower *and* at least 2x the
    /// baseline for the same webhook).
    #[serde(rename = "absoluteIncreaseMillis", with = "duration_millis")]
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
/// - in [`V1Beta1Lab::gateway`], every [`GatewaySuiteSpec::manifests`]
///   path is exactly as the user wrote it (possibly relative);
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
/// expectations file, [`V1Beta1Lab::expectations_file`]). A severity field
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
    /// (`reconciliationTimeoutMillis: 120000`), matching
    /// [`LatencyPolicy::absolute_increase`]'s established style — unit in
    /// the wire name, [`Duration`] in Rust; see that field for why the
    /// two spellings differ — rather than serde's default `{secs, nanos}`
    /// object shape. Defaults to
    /// [`crate::DEFAULT_RECONCILIATION_TIMEOUT`] when omitted, and must be
    /// non-zero — a zero timeout could never observe the two-poll
    /// stability window `admissionlab_gateway::reconcile` requires, so
    /// it would report every route as unconverged without ever having
    /// looked.
    #[serde(
        rename = "reconciliationTimeoutMillis",
        with = "duration_millis",
        default = "default_reconciliation_timeout"
    )]
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

/// The Ingress-to-Gateway migration suite (ROADMAP Task 8.3): a list of
/// cases, each pairing the `Ingress` manifests a team runs today against
/// the Gateway API manifests they intend to replace them with, plus the
/// probes that decide whether the two really behave the same.
///
/// # Admission Lab does not convert anything
///
/// **Read this before writing a case.** v1 contains no
/// Ingress-to-Gateway converter, and adding one is explicitly out of
/// scope. Both halves of every case are written by a human or produced
/// by some *other* tool (`ingress2gateway`, a vendor migration guide, a
/// hand edit), and this suite's entire job is to check that the
/// conversion **that user already performed** preserves behavior.
///
/// That is not a temporary limitation waiting on a converter; it is the
/// only arrangement under which the answer means anything. A suite that
/// generated the candidate manifests itself would be comparing its own
/// converter against its own converter: every rule the converter got
/// wrong would be applied identically on both sides of the comparison,
/// so the run would report "no behavior change" precisely for the
/// mistakes it was supposed to catch. The pairing is required, and it is
/// required *explicitly*, for the same reason
/// [`RouteContract`]'s Gateway identity is: a contract that reads its
/// expectation out of the artifact under test can never contradict that
/// artifact.
///
/// # Where this type lives, and why not in `admissionlab-gateway`
///
/// ROADMAP Task 8.3 lists `crates/admissionlab-gateway/src/migration.rs`
/// as this type's file, and §1.2's registry *also* freezes
/// [`crate::ResolvedLab::migration`] as an `Option<MigrationSuiteSpec>`.
/// Exactly the situation [`GatewaySuiteSpec`] already resolved, with the
/// same forced answer: `admissionlab-gateway` depends (transitively) on
/// this crate, so a `spec -> gateway` edge would be a dependency cycle.
/// The hand-written configuration surface is defined here;
/// `admissionlab_gateway::migration` **re-exports these exact types**
/// rather than declaring twins, and owns what a user never writes.
///
/// # One type for both the raw and the resolved stage
///
/// As with [`GatewaySuiteSpec`]: [`crate::resolve_lab`] rewrites
/// [`MigrationCaseSpec::baseline_ingress_manifests`] and
/// [`MigrationCaseSpec::candidate_gateway_manifests`] onto the
/// configuration file's own directory and validates everything else in
/// place, so the raw and the resolved value differ in those two fields
/// and nowhere else.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationSuiteSpec {
    /// The migration cases. Must not be empty, and every
    /// [`MigrationCaseSpec::id`] must be unique within the suite — both
    /// validated by [`crate::resolve_lab`]. A `migration:` section
    /// declaring no cases is the same quiet no-op an empty
    /// [`GatewaySuiteSpec::manifests`] would be: it would install
    /// nothing, probe nothing, and report success.
    pub cases: Vec<MigrationCaseSpec>,
    /// Where the **legacy** side's data plane is: the one shared
    /// `Service` an `Ingress` controller serves every `Ingress` on the
    /// cluster through.
    ///
    /// See [`MigrationSideSpec`] for why this is per side, why it is
    /// `Option` in the type but required in practice, and where the
    /// requirement is enforced.
    #[serde(default)]
    pub baseline: Option<MigrationSideSpec>,
    /// Where the **Gateway** side's data plane is: the `Service` the
    /// implementation provisions for the case's own `Gateway`, usually
    /// selected by Gateway API's `gateway.networking.k8s.io/gateway-name`
    /// label with `"{gatewayName}"` substituted.
    #[serde(default)]
    pub candidate: Option<MigrationSideSpec>,
}

/// Where one side of a migration suite's data plane is (ROADMAP Task
/// 8.8).
///
/// # Why a migration suite needs two of these and a Gateway suite needs
/// # one
///
/// [`GatewaySuiteSpec::gatewayEndpoint`](GatewaySuiteSpec::gateway_endpoint)
/// is a single block because a Gateway suite applies the *same*
/// manifests to two clusters running the same implementation: one
/// strategy locates both sides' data planes. A migration suite is the
/// opposite by construction — its baseline is an `Ingress` controller
/// and its candidate is a Gateway API implementation — and the two
/// locate their data planes in structurally different ways.
/// `recipes/ingress-nginx-legacy/recipe.yaml` says so in as many words:
/// an `Ingress` controller is *one shared* `Service` in the controller's
/// own namespace with no per-object substitution possible, while
/// `recipes/nginx-gateway-fabric/recipe.yaml` must template
/// `{gatewayNamespace}`/`{gatewayName}` because a Gateway API
/// implementation provisions one `Service` per `Gateway`. A single
/// strategy cannot be both, so there are two.
///
/// # Optional in the type, required to run
///
/// Both fields on [`MigrationSuiteSpec`] are `Option` with
/// `#[serde(default)]`, so every document written before Task 8.8
/// existed still parses — `docs/schema-migrations.md`'s first obligation
/// ("additions are optional") applied literally, rather than a new
/// required key inside an existing `admissionlab.io/v1beta1` section.
///
/// A suite that omits one is nonetheless unrunnable: with no strategy
/// there is no `Service` to port-forward to and therefore no probe, and
/// a migration case's probes are the *only* thing its two sides can be
/// compared on ([`MigrationCaseSpec::probes`]). That is refused by
/// `admissionlab_cli::pipeline`'s pre-flight validation, **before any
/// cluster is created**, in exactly the place and for exactly the reason
/// a Gateway route contract id that cannot be reported is refused there.
/// The result is a load-time-quality error without a schema-level
/// required field.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationSideSpec {
    /// How to find this side's data-plane `Service`. The same block, in
    /// the same shape, validated by the same
    /// [`crate::resolve_gateway_endpoint`], that a recipe's own
    /// `gatewayEndpoint:` and a lab's `gateway.gatewayEndpoint:` carry —
    /// one vocabulary for "where does a request go in", not three.
    pub gateway_endpoint: GatewayEndpointSpec,
}

/// One migration case: the `Ingress` manifests that define today's
/// behavior, the Gateway API manifests intended to replace them, the
/// probes that must answer identically through both, and the differences
/// the author already knows about and has written down.
///
/// # The two manifest lists are peers, not a source and a target
///
/// Neither list is derived from the other (see [`MigrationSuiteSpec`]'s
/// "Admission Lab does not convert anything"). They are applied to the
/// *baseline* and the *candidate* side respectively, and both are
/// resolved against the configuration file's own directory exactly as
/// [`GatewaySuiteSpec::manifests`] is. Both must be non-empty: a case
/// with an empty side has nothing to install there, so every probe
/// against that side would measure the absence of a fixture rather than
/// a migration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationCaseSpec {
    /// A stable identifier for this case, unique within the suite and
    /// non-empty — the handle that correlates the baseline and candidate
    /// observations of one migration, exactly as [`RouteContract::id`]
    /// does for a route contract.
    pub id: String,
    /// The `Ingress` manifests (plus whatever namespaces, backends and
    /// `IngressClass` they need) that define the behavior being
    /// migrated *away from*, applied to the baseline side. Resolved
    /// against the configuration file's own directory. Must not be
    /// empty.
    pub baseline_ingress_manifests: Vec<PathBuf>,
    /// The Gateway API manifests (`GatewayClass`, `Gateway`,
    /// `HTTPRoute`, backends) intended to reproduce that behavior,
    /// applied to the candidate side. **Written by the user, never
    /// generated from the list above.** Resolved against the
    /// configuration file's own directory. Must not be empty.
    pub candidate_gateway_manifests: Vec<PathBuf>,
    /// The requests replayed through *both* sides. The same
    /// [`HttpProbeContract`] the Gateway suite uses — one vocabulary for
    /// "send this request and expect this answer", because a migration
    /// case asks the identical question a route contract does, only of
    /// two differently-shaped stacks instead of two versions of one.
    ///
    /// Must not be empty, which is where this deliberately differs from
    /// [`RouteContract::probes`]. A route contract with no probes still
    /// asserts something real (that the route reconciles, which
    /// `admissionlab_gateway::reconcile` observes independently of any
    /// traffic). A migration case with no probes asserts nothing at all:
    /// an `Ingress` and an `HTTPRoute` have no comparable status
    /// vocabulary, so traffic behavior is the *only* thing the two sides
    /// can be compared on.
    pub probes: Vec<HttpProbeContract>,
    /// Behavioral differences the author already knows the migration
    /// introduces, each with a written justification. Empty by default:
    /// a migration nobody expects to change anything declares nothing
    /// here, and any difference the run observes is then unexplained.
    ///
    /// See [`NonPortableFeatureExpectation`] for why this is its own
    /// vocabulary rather than a reuse of `expectations.yaml`.
    #[serde(default)]
    pub expected_nonportable: Vec<NonPortableFeatureExpectation>,
}

/// One `Ingress` feature the author knows has no faithful Gateway API
/// equivalent, and the human reason it is accepted.
///
/// The realistic migration is not lossless. `ingress-nginx`'s
/// annotations reach well past what Gateway API v1 models —
/// `nginx.ingress.kubernetes.io/configuration-snippet` has no portable
/// counterpart at all; `auth-url`, `server-snippet`, and the
/// `canary-*` family have partial ones with different semantics. A team
/// migrating declares those here, once, with a sentence about what they
/// decided to do instead, and Task 8.5 marks the corresponding observed
/// difference as expected rather than reporting it as a surprise.
///
/// # Two required fields, both human
///
/// `feature` must be non-empty and unique within its case; `reason` must
/// be non-empty. The same rule, for the same reason,
/// `admissionlab_policy::ExpectedChange` applies to its own `id`/`reason`
/// pair: the name is the only handle anything downstream has back to
/// this entry, duplicates would make that handle ambiguous, and an entry
/// that suppresses a behavioral difference with no written
/// justification is indistinguishable from someone quietly silencing a
/// real regression. Neither field is decoration, and neither is
/// defaulted.
///
/// # Why this is not `expectations.yaml`
///
/// A migration expectation and a regression expectation look similar for
/// about one sentence and then diverge in all three ways that matter:
///
/// - **Different vocabulary.** `admissionlab_policy::ExpectedChange`
///   selects on a `SemanticChangeKind` (`container_added`,
///   `newly_denied`, ...) — the closed set of *admission* differences
///   `admissionlab-diff` computes. `feature` here names an **input**
///   feature of the stack being migrated away from (an annotation, a
///   snippet, an auth mode), which is not in that set and could not
///   sensibly be added to it: one is a fact about a diff, the other a
///   fact about an `Ingress`.
/// - **Different lifecycle.** A regression expectation is transient —
///   `admissionlab-policy` reports one that stops matching as a *stale
///   expectation*, so it gets deleted on the next upgrade. A
///   non-portability statement is *permanent* for as long as the
///   migration exists: `configuration-snippet` does not become portable
///   later, and reporting it as stale every run would train reviewers to
///   ignore staleness reports.
/// - **Different lifetime in the file.** Migration cases are deleted
///   wholesale the day the migration lands; an `expectations.yaml`
///   outlives every individual upgrade it was written for. Sharing one
///   file would mean deleting a migration would either strand or delete
///   entries that had nothing to do with it.
///
/// So this is a separate type, in a separate section, and
/// [`V1Beta1Lab::expectations_file`] is untouched by it. Neither
/// mechanism can grade the other's subject matter, which is exactly the
/// property that keeps "this difference is accepted" answerable in one
/// place per question rather than two places per run.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NonPortableFeatureExpectation {
    /// The non-portable feature, named as the source stack names it —
    /// for example `nginx.ingress.kubernetes.io/configuration-snippet`.
    /// A free-form `String` rather than an enumeration: the set of
    /// annotations an Ingress controller understands is that
    /// controller's, not this project's, and Global Constraint 6 keeps
    /// vendor vocabulary out of the classification engine. Must be
    /// non-empty and unique within its case.
    pub feature: String,
    /// Why this is accepted: what the feature did, and what the
    /// migration does instead. Must be non-empty — see this type's "Two
    /// required fields, both human".
    pub reason: String,
}
