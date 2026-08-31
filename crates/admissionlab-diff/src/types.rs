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
//! `tests/types.rs` snapshot-asserts all seventeen strings against an
//! exhaustive match (adding a variant without extending that table is a
//! compile error, not a silently unpinned name).
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

use admissionlab_core::FixtureId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What kind of behavior difference a [`SemanticChange`] claims.
///
/// The seventeen variants are the complete Alpha set. Their wire strings
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
        }
    }
}

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SemanticChange {
    /// What kind of difference this is.
    pub kind: SemanticChangeKind,
    /// Which fixture the difference was observed for. Both compared
    /// sides always describe the same fixture; the caller pairs them.
    #[serde(serialize_with = "serialize_fixture_id")]
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
