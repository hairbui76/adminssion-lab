//! The Alpha-stable vocabulary for one behavior difference.
//!
//! [`SemanticChange`] is what every comparison in Phase 4 produces and
//! what policy, expectations, and every report renderer consume. Its
//! [`SemanticChangeKind`] names are a **public contract**: they appear in
//! JSON reports, in lab-file `policy.failOn` lists, and in
//! `expectations.yaml` files that users write by hand and keep in their
//! own repositories. Renaming one silently breaks every such file, so
//! each variant's wire tag is pinned with an explicit `#[serde(rename)]`
//! rather than left to derive from the Rust identifier, and
//! `tests/types.rs` snapshot-asserts all twenty-six strings against an
//! exhaustive match (adding a variant without extending that table is a
//! compile error, not a silently unpinned name).
//!
//! # Two vocabularies, one enum
//!
//! Seventeen of the kinds are *admission* claims, produced by this
//! crate's own comparisons. The other nine are *Gateway* claims (ROADMAP
//! Task 6.9), produced by `admissionlab_gateway::diff::diff_gateway`.
//! They share one enum because they share one downstream: a
//! `SemanticChange` is a `SemanticChange` wherever it came from, and
//! `admissionlab-policy` grades, `expectations.yaml` names, and every
//! renderer prints them through exactly one vocabulary. A second enum
//! would have forced a second severity table, a second `failOn` list and
//! a second `expectations.yaml` dialect for no gain to the person
//! reading the report.
//!
//! The Gateway *comparator* still lives in `admissionlab-gateway`, not
//! here: it compares Gateway evidence types this crate cannot see (§1.1
//! draws no `diff -> gateway` edge, and `gateway -> diff` is the acyclic
//! direction), so this module contributes the shared vocabulary and
//! nothing else.
//!
//! # Semantic changes are not raw changes
//!
//! Everything in this module is *semantic* vocabulary: a claim, in
//! product terms, that admission behavior changed in a way a human is
//! meant to reason about and a policy is meant to grade. [`crate::raw`]
//! holds the separate, deliberately dumber *diagnostic* vocabulary
//! (RFC 6902 patch operations over two JSON documents). The two must not
//! be confused: a raw change is evidence, a semantic change is a claim,
//! and a raw difference existing does not by itself justify emitting a
//! semantic change (Task 4.4's rejected-to-rejected message drift is the
//! canonical case where a real raw difference must produce *no* semantic
//! change).
//!
//! # Absence is representable everywhere it matters
//!
//! [`SemanticChange::object_path`], `subject`, `baseline`, `candidate`,
//! and `origin` are all [`Option`], and each `None` means "this comparison
//! genuinely had nothing to put here", never a fabricated placeholder
//! (Global Constraint 15). In particular `origin: None` means
//! first-divergence attribution was *not attempted or not possible* for
//! this change -- it never means "there was no divergence". Attribution
//! itself is Task 4.7's [`DivergenceEvidence`]-producing function; the
//! evidence *types* are declared here because `SemanticChange` carries
//! one and this crate must compile before that function exists.
//!
//! # Fixture attribution
//!
//! [`SemanticChange::fixture_id`] is *not* optional, because every
//! change a report renders belongs to exactly one fixture. Not every
//! comparison is handed that identity, though: the normalized object and
//! trace types Tasks 4.5 and 4.6 compare carry none. Those comparisons
//! emit changes stamped with [`unattributed_fixture_id`], and the caller
//! that paired the two sides finishes the job with
//! [`SemanticChange::attributed_to`]. See that method's documentation for
//! the full argument, including why the sentinel is a loud one.

use admissionlab_core::FixtureId;
// ROADMAP Task 7.2 (frozen `admissionlab.io/result/v1beta1` result
// schema): every type this file defines is embedded verbatim in that
// document, so the schema generated from the result model has to
// describe it. Derives and `#[schemars(with = ...)]` restatements of
// what the existing `serialize_with` helpers already emit -- no field,
// name, or semantic change.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What kind of behavior difference a [`SemanticChange`] claims.
///
/// Seventeen variants are the complete Alpha (admission) set and nine
/// more are Phase 6's Gateway set. Their wire strings
/// are human-oriented `snake_case` and are **not** mechanically derived
/// from the Rust identifiers -- four of them differ deliberately
/// (`ObjectNewlyDenied` serializes as `newly_denied`, `ObjectNewlyAllowed`
/// as `newly_allowed`), which is exactly why every variant carries an
/// explicit `#[serde(rename)]`: a reader of this enum must never have to
/// work out which names are derived and which are pinned.
///
/// Derives no `Default`. There is no such thing as a default kind of
/// behavior change; a value of this type only ever exists because a
/// comparison concluded something specific.
///
/// [`Deserialize`] is derived (unlike [`SemanticChange`] itself, which is
/// emit-only) because Task 4.9's `ExpectedChange::kind` is read back out
/// of a user-authored `expectations.yaml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum SemanticChangeKind {
    /// The baseline admitted the object and the candidate rejected it.
    #[serde(rename = "newly_denied")]
    ObjectNewlyDenied,
    /// The baseline rejected the object and the candidate admitted it.
    #[serde(rename = "newly_allowed")]
    ObjectNewlyAllowed,
    /// A container present only in the candidate's admitted object.
    #[serde(rename = "container_added")]
    ContainerAdded,
    /// A container present only in the baseline's admitted object.
    #[serde(rename = "container_removed")]
    ContainerRemoved,
    /// An init container present only in the candidate's admitted object.
    #[serde(rename = "init_container_added")]
    InitContainerAdded,
    /// An init container present only in the baseline's admitted object.
    #[serde(rename = "init_container_removed")]
    InitContainerRemoved,
    /// A volume present only in the candidate's admitted object.
    #[serde(rename = "volume_added")]
    VolumeAdded,
    /// A volume present only in the baseline's admitted object.
    #[serde(rename = "volume_removed")]
    VolumeRemoved,
    /// A container's volume mounts differ between the two sides.
    #[serde(rename = "volume_mount_changed")]
    VolumeMountChanged,
    /// A container's environment differs between the two sides.
    #[serde(rename = "environment_changed")]
    EnvironmentChanged,
    /// A container image differs between the two sides.
    #[serde(rename = "image_changed")]
    ImageChanged,
    /// The admitted object's service account differs between the two
    /// sides.
    #[serde(rename = "service_account_changed")]
    ServiceAccountChanged,
    /// A pod- or container-level security context differs between the
    /// two sides.
    #[serde(rename = "security_context_changed")]
    SecurityContextChanged,
    /// A container's resource requests or limits differ between the two
    /// sides.
    #[serde(rename = "resource_requirement_changed")]
    ResourceRequirementChanged,
    /// A webhook that was observed to fail on one side and not the
    /// other.
    #[serde(rename = "webhook_failed")]
    WebhookFailed,
    /// The observed set or ordering of webhook invocations differs
    /// between the two sides.
    #[serde(rename = "webhook_invocation_changed")]
    WebhookInvocationChanged,
    /// An observed per-webhook latency differs between the two sides by
    /// more than the configured latency policy allows.
    #[serde(rename = "webhook_latency_changed")]
    WebhookLatencyChanged,
    /// An `HTTPRoute` is attached to a parent `Gateway` on the candidate
    /// side that it was not attached to on the baseline side -- the
    /// parent appears in the candidate's `status.parents` and not in the
    /// baseline's.
    #[serde(rename = "route_attached")]
    RouteAttached,
    /// An `HTTPRoute` is no longer attached to a parent `Gateway` it was
    /// attached to on the baseline side.
    #[serde(rename = "route_detached")]
    RouteDetached,
    /// Whether a route's backend references resolve changed between the
    /// two sides: exactly one side published `ResolvedRefs: True`.
    #[serde(rename = "backend_resolution_changed")]
    BackendResolutionChanged,
    /// A route is attached to the same parent `Gateway` through a
    /// different set of listeners (`parentRef.sectionName`).
    #[serde(rename = "listener_binding_changed")]
    ListenerBindingChanged,
    /// An `Accepted` condition's state differs between the two sides, on
    /// a `GatewayClass`, a `Gateway`, or one of a route's parent status
    /// entries.
    #[serde(rename = "accepted_condition_changed")]
    AcceptedConditionChanged,
    /// A route parent's `ResolvedRefs` condition state differs between
    /// the two sides.
    #[serde(rename = "resolved_refs_condition_changed")]
    ResolvedRefsConditionChanged,
    /// A `Gateway`'s `Programmed` condition state differs between the
    /// two sides.
    #[serde(rename = "programmed_condition_changed")]
    ProgrammedConditionChanged,
    /// The HTTP status one probe received through the data plane differs
    /// between the two sides, or the candidate returned no answer at all
    /// for a probe the baseline answered.
    #[serde(rename = "traffic_status_changed")]
    TrafficStatusChanged,
    /// The same probe reached a different backend workload on the two
    /// sides, as each backend identified itself.
    #[serde(rename = "traffic_backend_changed")]
    TrafficBackendChanged,
}

impl SemanticChangeKind {
    /// Returns this kind's stable wire name.
    ///
    /// Exactly the string `serde` serializes this variant as -- pinned
    /// by `tests/types.rs`, which asserts the two agree for every
    /// variant rather than trusting that the `#[serde(rename)]`
    /// attributes and this match were kept in sync by hand.
    ///
    /// Exists because the string form is what user-facing configuration
    /// speaks: `PolicySpec::fail_on` is a `BTreeSet<String>` of these
    /// names, so grading a change means comparing against this value,
    /// not re-serializing through `serde_json`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ObjectNewlyDenied => "newly_denied",
            Self::ObjectNewlyAllowed => "newly_allowed",
            Self::ContainerAdded => "container_added",
            Self::ContainerRemoved => "container_removed",
            Self::InitContainerAdded => "init_container_added",
            Self::InitContainerRemoved => "init_container_removed",
            Self::VolumeAdded => "volume_added",
            Self::VolumeRemoved => "volume_removed",
            Self::VolumeMountChanged => "volume_mount_changed",
            Self::EnvironmentChanged => "environment_changed",
            Self::ImageChanged => "image_changed",
            Self::ServiceAccountChanged => "service_account_changed",
            Self::SecurityContextChanged => "security_context_changed",
            Self::ResourceRequirementChanged => "resource_requirement_changed",
            Self::WebhookFailed => "webhook_failed",
            Self::WebhookInvocationChanged => "webhook_invocation_changed",
            Self::WebhookLatencyChanged => "webhook_latency_changed",
            Self::RouteAttached => "route_attached",
            Self::RouteDetached => "route_detached",
            Self::BackendResolutionChanged => "backend_resolution_changed",
            Self::ListenerBindingChanged => "listener_binding_changed",
            Self::AcceptedConditionChanged => "accepted_condition_changed",
            Self::ResolvedRefsConditionChanged => "resolved_refs_condition_changed",
            Self::ProgrammedConditionChanged => "programmed_condition_changed",
            Self::TrafficStatusChanged => "traffic_status_changed",
            Self::TrafficBackendChanged => "traffic_backend_changed",
        }
    }
}

/// Which way a behavior change moved: toward the good state, or away
/// from it.
///
/// ROADMAP Task 6.9 step 6: *"A condition change that moves from
/// False/Unknown to True may be downgraded to Info by the comparator
/// because it is an improvement; True to False is Critical. The
/// comparator must encode direction rather than relying on free-form
/// reason strings."*
///
/// This type is that encoding. It is declared here, in the shared
/// vocabulary crate, rather than in `admissionlab-gateway`, because two
/// crates must agree on it and neither may depend on the other: the
/// *comparator* that knows the direction is
/// `admissionlab_gateway::diff`, and the *grader* that acts on it is
/// `admissionlab_policy::severity`. Both already depend on this crate,
/// and `policy -> gateway` would drag a Phase 6 crate into Phase 4's
/// grading path for one enum.
///
/// # Where it is carried
///
/// In the change's own [`SemanticChange::candidate`] payload, under the
/// [`DIRECTION_KEY`] key, read back by [`SemanticChange::direction`].
/// [`SemanticChange`]'s field list is a §1.2-frozen Alpha contract and
/// direction is meaningful for only a handful of kinds, so it rides in
/// the payload rather than becoming a twenty-eighth `Option` field that
/// is `None` for every admission change ever emitted. The candidate side
/// carries it because that is the side the transition moved *to*, and a
/// change with no candidate payload has no transition to describe.
///
/// A kind whose payloads carry no direction is not "undirected by
/// omission" -- see [`SemanticChange::direction`] for the two distinct
/// reasons a direction can be absent.
///
/// Derives no `Default`: a direction exists only because a comparison
/// determined one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ChangeDirection {
    /// The candidate moved toward the good state -- for a Gateway
    /// condition, to `True` from something that was not `True`.
    #[serde(rename = "improvement")]
    Improvement,
    /// The candidate moved away from the good state -- for a Gateway
    /// condition, away from a `True` the baseline published.
    #[serde(rename = "regression")]
    Regression,
}

impl ChangeDirection {
    /// Returns this direction's stable wire name -- exactly the string
    /// `serde` serializes it as (asserted in `tests/types.rs`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Improvement => "improvement",
            Self::Regression => "regression",
        }
    }

    /// Parses a direction from its wire name, exactly.
    ///
    /// Returns [`None`] for anything else, including a near miss: an
    /// unrecognized direction must read as "no direction was recorded"
    /// rather than as a guess, since the value it feeds is a severity.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "improvement" => Some(Self::Improvement),
            "regression" => Some(Self::Regression),
            _ => None,
        }
    }
}

/// The key a [`ChangeDirection`] is carried under inside a
/// [`SemanticChange::candidate`] payload object.
///
/// Public because two crates write and read it -- the Gateway
/// comparator stamps it, `admissionlab-policy` reads it -- and a string
/// literal duplicated across a crate boundary is exactly how the two
/// would drift.
pub const DIRECTION_KEY: &str = "direction";

/// How well supported a [`DivergenceEvidence`] claim is.
///
/// Task 4.7 computes first-divergence attribution from two normalized
/// webhook traces, and that computation is only ever as good as the
/// evidence it was handed: an audit backend can be missing or partial
/// (see `admissionlab_admission::trace::TraceEvidence`), and two traces
/// can be byte-identical while the final objects still differ. This enum
/// is how such an attribution states its own strength instead of
/// presenting every conclusion as equally proven (Global Constraint 15,
/// and Global Constraint 7: attribution is deterministic, never guessed
/// by a model).
///
/// Derives no `Default`, for the same reason
/// `admissionlab_admission::trace::TraceEvidence` does not: `Observed` is
/// the variant a developer writes first, so an accidental `Default`
/// would silently upgrade an unproven claim to a proven one.
///
/// Each variant's wire tag is pinned explicitly -- this type serializes
/// into the same reports [`SemanticChangeKind`] does, so its names are
/// Alpha-stable too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum DivergenceConfidence {
    /// The divergence itself was directly observed in the captured
    /// evidence on both sides.
    #[serde(rename = "observed")]
    Observed,
    /// The divergence was deduced from evidence that was incomplete on
    /// at least one side. The conclusion is deterministic but the
    /// underlying observation was not complete.
    #[serde(rename = "inferred")]
    Inferred,
    /// A difference exists but the captured evidence does not locate it.
    /// The canonical case is two identical traces with different final
    /// objects: something diverged outside the captured mutating-webhook
    /// evidence, or that evidence is incomplete.
    #[serde(rename = "unknown")]
    Unknown,
}

/// Where a difference first became visible in the webhook chain.
///
/// Produced by Task 4.7's `first_divergence` and carried by
/// [`SemanticChange::origin`]. This task declares the type (a
/// `SemanticChange` has to name it) and pins its wire shape; the
/// function that fills one in lands with Task 4.7.
///
/// Positions are `(round, index)` pairs matching
/// `admissionlab_admission::trace::WebhookInvocation`'s own `round`/
/// `index` fields, and serialize as a two-element JSON array
/// `[round, index]`. A side's position or webhook name is [`None`] when
/// that side has no invocation at the point of divergence at all -- the
/// added/removed-invocation case -- never as a stand-in for "not
/// captured".
///
/// `explanation` is required and non-optional on purpose: an attribution
/// with no human-readable account of *why* is not usable evidence in a
/// report, and a [`DivergenceConfidence::Unknown`] value especially must
/// carry the sentence saying what could not be determined.
///
/// Derives no `Default`: like the confidence it carries, evidence exists
/// only because something concluded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DivergenceEvidence {
    /// How well supported this attribution is.
    pub confidence: DivergenceConfidence,
    /// The baseline trace position where the divergence was located, as
    /// `(round, index)`.
    pub baseline_position: Option<(u32, u32)>,
    /// The candidate trace position where the divergence was located, as
    /// `(round, index)`.
    pub candidate_position: Option<(u32, u32)>,
    /// The baseline webhook at `baseline_position`, by name.
    pub baseline_webhook: Option<String>,
    /// The candidate webhook at `candidate_position`, by name.
    pub candidate_webhook: Option<String>,
    /// A human-readable account of what diverged and how it was
    /// determined.
    pub explanation: String,
}

/// One claimed behavior difference between the baseline and candidate
/// stacks for one fixture.
///
/// Implements [`Serialize`] only, never [`Deserialize`] -- the same
/// asymmetry, for the same reason, as
/// `admissionlab_admission::outcome::AdmissionOutcome`: `fixture_id` is
/// an `admissionlab_core::FixtureId`, which that crate deliberately
/// implements neither trait for, and a `SemanticChange` is only ever
/// produced by a comparison in this crate and serialized *outward* into a
/// report. Nothing reads one back in. (Its `kind` *is* readable, because
/// user-authored expectation files name kinds; see
/// [`SemanticChangeKind`].)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SemanticChange {
    /// What kind of difference this is.
    pub kind: SemanticChangeKind,
    /// Which fixture the difference was observed for. Both compared
    /// sides always describe the same fixture; the caller pairs them.
    #[serde(serialize_with = "serialize_fixture_id")]
    #[schemars(with = "String")]
    pub fixture_id: FixtureId,
    /// An RFC 6901 JSON pointer into the compared object, when the
    /// difference has one specific location (for example
    /// `/spec/containers/0/image`). `None` for differences that are not
    /// about a field of the object at all, such as a whole-request
    /// decision flip.
    pub object_path: Option<String>,
    /// The named thing the difference is about, when there is one -- a
    /// container name, a webhook name. `None` when the difference is not
    /// scoped to a named subject. This is the value
    /// `admissionlab_policy`'s `ChangeSelector::subject` matches
    /// against.
    pub subject: Option<String>,
    /// The baseline side's value, as JSON. `None` means the baseline had
    /// no value here (for example an added container), never that the
    /// value was not captured.
    pub baseline: Option<Value>,
    /// The candidate side's value, as JSON. `None` means the candidate
    /// had no value here (for example a removed container), never that
    /// the value was not captured.
    pub candidate: Option<Value>,
    /// Where in the webhook chain this difference first became visible,
    /// when that could be attributed. `None` means attribution was not
    /// attempted or not possible for this change -- it never means "no
    /// divergence occurred". Filled in by Task 4.7.
    pub origin: Option<DivergenceEvidence>,
}

impl SemanticChange {
    /// Returns this change with its [`SemanticChange::fixture_id`]
    /// replaced by `fixture_id`.
    ///
    /// This is the seam every comparison whose *inputs carry no fixture
    /// identity* uses. [`crate::admission::diff_admission_decision`]
    /// needs nothing of the sort: an
    /// `admissionlab_admission::AdmissionOutcome` has a `fixture_id`
    /// field, so that function reads the identity straight off its own
    /// input and its changes are attributed the moment they exist. The
    /// two normalized types Tasks 4.5 and 4.6 compare --
    /// `admissionlab_normalize::NormalizedObject` and
    /// `NormalizedTrace` -- deliberately carry no identifier (§1.2 fixes
    /// their fields, and normalization is a transformation of one
    /// document, not a record of which fixture produced it), and both
    /// tasks' signatures are frozen at two parameters. So those
    /// functions emit changes stamped with
    /// [`unattributed_fixture_id`], and the caller that *does* know
    /// which fixture it paired -- the run loop, and ultimately
    /// `admissionlab_report`'s `FixtureComparison`, which is keyed by
    /// `fixture_id` -- stamps them here.
    ///
    /// Consuming `self` rather than mutating in place makes the stamping
    /// visible at the call site (`changes.into_iter().map(|change|
    /// change.attributed_to(&id))`), which is where a reviewer needs to
    /// see it.
    #[must_use]
    pub fn attributed_to(mut self, fixture_id: &FixtureId) -> Self {
        self.fixture_id = fixture_id.clone();
        self
    }

    /// The [`ChangeDirection`] the comparator recorded for this change,
    /// if it recorded one.
    ///
    /// Reads [`DIRECTION_KEY`] out of the [`SemanticChange::candidate`]
    /// payload, which must be a JSON object with a string value there;
    /// anything else is [`None`]. This is the only reader -- nothing
    /// else in the workspace may poke at that key by hand.
    ///
    /// # [`None`] means one of two things, and never a third
    ///
    /// Either the change's *kind* has no notion of direction (every
    /// admission kind; a Gateway traffic change, whose "better" depends
    /// on a probe contract the comparator is not handed), or the kind
    /// has one and this particular transition did not determine it (a
    /// Gateway condition moving `False -> Unknown` is neither toward
    /// `True` nor away from it). It never means "this was an
    /// improvement but nobody said so": a caller that reads [`None`]
    /// must grade the change at its kind's default severity, which is
    /// exactly what `admissionlab_policy::default_change_severity` does.
    #[must_use]
    pub fn direction(&self) -> Option<ChangeDirection> {
        self.candidate
            .as_ref()?
            .get(DIRECTION_KEY)?
            .as_str()
            .and_then(ChangeDirection::from_name)
    }
}

/// The [`FixtureId`] string a [`SemanticChange`] carries before a caller
/// has attributed it to the fixture it was computed for.
///
/// A sentinel, and a deliberately recognizable one. It cannot collide
/// with a real fixture identifier: `admissionlab_fixtures`' own
/// `compute_fixture_id` always appends the document index, so every
/// identifier it produces ends in a decimal digit, and this one does
/// not. A report that renders this string is a report whose caller
/// forgot to call [`SemanticChange::attributed_to`] -- which is exactly
/// what a loud sentinel is for, as against a plausible-looking
/// placeholder that would silently mislabel a change as belonging to
/// some real fixture.
pub const UNATTRIBUTED_FIXTURE: &str = "unattributed";

/// Returns the [`UNATTRIBUTED_FIXTURE`] sentinel as a [`FixtureId`].
///
/// See [`SemanticChange::attributed_to`] for why comparisons over
/// normalized objects and traces produce changes carrying this, and who
/// is expected to replace it.
///
/// # Panics
///
/// Never. [`UNATTRIBUTED_FIXTURE`] is a non-empty string of ASCII
/// lowercase letters, which is precisely what `FixtureId::parse`
/// accepts.
#[must_use]
pub fn unattributed_fixture_id() -> FixtureId {
    FixtureId::parse(UNATTRIBUTED_FIXTURE)
        .expect("UNATTRIBUTED_FIXTURE is non-empty and ASCII lowercase, which FixtureId accepts")
}

/// Serializes a [`FixtureId`] as its bare string form
/// ([`FixtureId::as_str`]).
///
/// Mirrors `admissionlab_admission::outcome`'s own helper of the same
/// name, so this identifier reaches a report as the same plain string
/// there and here rather than as whatever internal shape
/// `admissionlab_core::FixtureId` happens to use.
fn serialize_fixture_id<S: serde::Serializer>(
    value: &FixtureId,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value.as_str())
}
