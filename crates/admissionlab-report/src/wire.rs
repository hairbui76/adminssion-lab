//! The frozen `admissionlab.io/result/v1` result document.
//!
//! ROADMAP Task 7.2 froze the machine-readable result and Task 9.1
//! promotes that same shape to the stable identifier. This module is
//! that freeze: [`ResultDocument`] *is* the wire format, it is the only
//! thing a [`LabResult`] ever serializes as (see that type's
//! hand-written [`Serialize`]), and `schemas/result-v1.json` is
//! generated from these very types by [`crate::schema`]. One definition
//! serves the artifact, the published schema, and the golden, so none of
//! the three can drift from the other two.
//!
//! # Projection, not a second model
//!
//! Every type here **borrows** from a [`LabResult`]; none of them owns a
//! copy of any evidence. The four computed fields the freeze adds
//! ([`FixtureDocument::bucket`], [`AdmissionSection::comparability`],
//! [`ReconciliationSection::evidence_level`],
//! [`TrafficSection::evidence`], and the change identifiers) are
//! *derived at serialization time* by calling the crate that owns each
//! judgement -- `admissionlab_diff::decision_comparability`,
//! `admissionlab_gateway::gateway_evidence_level`,
//! [`FixtureComparison::bucket`] -- so the document can never disagree
//! with the model it was projected from, the way a stored duplicate
//! could.
//!
//! ## Why a projection rather than a reshaped model
//!
//! The roadmap's frozen shape wants three sibling evidence sections
//! (`admission`, `gatewayReconciliation`, `traffic`) where the model
//! carries two fields ([`FixtureComparison::admission`] and
//! [`FixtureComparison::gateway`]). Splitting the *Rust* model to match
//! would take an `admissionlab_gateway::GatewayCaseComparison` -- one
//! value, produced by the Gateway engine, and the receiver of
//! `comparability()`, `changes()` and `probe_pairs()` -- and store it as
//! two halves that [`FixtureComparison::bucket`] would then have to
//! reassemble to ask its one question. The pair is the unit the engine
//! observed; the two sections are how a *reader* wants it presented.
//! Presentation is this crate's job, and doing it here keeps the
//! Gateway crate's evidence types (which are `Serialize`-only and owned
//! elsewhere) untouched.
//!
//! # Casing
//!
//! Reviewed and deliberately kept at the freeze: **every key this crate
//! owns is `camelCase`**, and a value embedded verbatim from another
//! crate keeps that crate's own already-pinned tags (`fixture_id`,
//! `total_latency`, `object_path`, `newly_denied`, ...). Re-spelling a
//! foreign type's keys here would mean wrapping it in a local mirror
//! struct -- the competing synonym §1.2 forbids, free to stop matching
//! what was actually observed. The casing boundary in a
//! `result.json` is therefore an ownership boundary, and it is
//! load-bearing information rather than an oversight.
//!
//! # Global Constraint 15: nothing is inferred from an absent key
//!
//! The three evidence sections are always *written*, as `null` when the
//! fixture carries no evidence of that kind, so a consumer never has to
//! read "the key is missing" as a claim. Each section then states its
//! own availability explicitly rather than leaving it to be inferred
//! from an empty collection or a `null` sibling:
//!
//! - [`AdmissionSection::comparability`] -- whether the two sides'
//!   decisions could be compared at all, with each side's own reason
//!   when they could not.
//! - [`AdmissionSection::trace_evidence`] -- each side's
//!   [`TraceEvidence`], surfaced at the section's top level rather than
//!   only nested inside `baseline.trace`/`candidate.trace`.
//! - [`AdmissionSection::divergence_confidence`] -- a four-valued
//!   answer, so "attribution was never possible" is a written word
//!   (`unattributed`) instead of a `null` `firstDivergence`.
//! - [`ReconciliationSection::comparability`] and
//!   [`ReconciliationSection::evidence_level`] -- both sides'
//!   convergence, stated.
//! - [`TrafficSection::evidence`] -- whether probes were answered by
//!   both sides, one, or neither, so an empty `pairs` list is never
//!   read as "the two data planes agreed".
//!
//! # Change identifiers
//!
//! [`ChangeDocument::id`] is **content-derived**: the first 16 hex
//! characters of the SHA-256 of a canonical encoding of the claim
//! itself, prefixed `sc-`. See [`semantic_change_id`] for exactly what
//! is hashed and why an index-based identifier was rejected.

use admissionlab_admission::{AdmissionOutcome, TraceEvidence};
use admissionlab_core::{Diagnostic, StageTimings};
use admissionlab_diff::{
    DecisionComparability, DivergenceConfidence, DivergenceEvidence, SemanticChange,
    SemanticChangeKind, decision_comparability,
};
use admissionlab_gateway::{
    GatewayCaseComparison, GatewayComparability, GatewayEvidenceLevel, HttpProbeResult,
    MigrationBehaviorChange, MigrationBehaviorKind, MigrationComparability,
    NonPortableFeatureExpectation, ReconciliationEvidence, gateway_evidence_level,
};
use admissionlab_policy::{
    ClassifiedChange, PolicyDisposition, PolicyResult, Severity, StaleExpectation,
};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::model::{
    AdmissionComparison, EnvironmentSummary, FixtureComparison, GradedMigrationChange, LabResult,
    MigrationCaseComparison, RunSummary,
};

/// One comparison run, as `result.json` writes it.
///
/// Borrowed from a [`LabResult`] through [`From`]; see this module's
/// documentation for the whole contract.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ResultDocument<'a> {
    /// The frozen schema identifier -- always
    /// [`crate::model::SCHEMA_VERSION`] for a document this crate wrote.
    #[serde(rename = "schemaVersion")]
    pub schema_version: &'a str,
    /// The run this result describes.
    #[serde(rename = "runId")]
    pub run_id: &'a str,
    /// The five-bucket count over [`Self::fixtures`].
    #[serde(rename = "summary")]
    pub summary: &'a RunSummary,
    /// What each side actually was, as observed.
    #[serde(rename = "environments")]
    pub environments: &'a EnvironmentSummary,
    /// Every compared unit, in the order the run compared them.
    #[serde(rename = "fixtures")]
    pub fixtures: Vec<FixtureDocument<'a>>,
    /// The run's verdict and every graded change.
    #[serde(rename = "policy")]
    pub policy: PolicyDocument<'a>,
    /// Run-level diagnostics. Per-fixture ones stay on the outcome that
    /// produced them.
    #[serde(rename = "diagnostics")]
    pub diagnostics: &'a [Diagnostic],
    /// Every Ingress-to-Gateway migration case the run compared
    /// (ROADMAP Task 8.8), or **omitted entirely** for a lab with no
    /// `migration:` section.
    ///
    /// # Why this key is omitted rather than written as `null`
    ///
    /// The three *evidence sections* inside a fixture are always
    /// written, because each one's absence would otherwise have to be
    /// interpreted (see this module's Global Constraint 15 section).
    /// This key is different in kind: it is an **optional addition to a
    /// frozen document**, and `docs/schema-migrations.md`'s first
    /// obligation is that an addition must leave every document written
    /// before it existed still valid. Omitting it means a `result.json`
    /// from an admission-only or Gateway-only lab is byte-identical to
    /// what this crate wrote before Task 8.8 — which is exactly the
    /// property "additive" is supposed to name. The same reasoning, and
    /// the same treatment, [`Self::timings`] already carries.
    ///
    /// Where a claim *could* be misread the section states it inside
    /// itself instead: [`MigrationSection::comparability`] says whether
    /// an empty `changes` list is agreement or ignorance, and
    /// [`MigrationSection::comparabilityReason`](MigrationSection::comparability_reason)
    /// says it in prose.
    #[serde(rename = "migration", skip_serializing_if = "Option::is_none")]
    pub migration: Option<Vec<MigrationSection<'a>>>,
    /// How long each stage took, when the producer measured.
    ///
    /// The one key in this document that is *omitted* rather than
    /// written as `null`, and deliberately: absent timings are the
    /// absence of a measurement, and the block's own optional stages
    /// already spell absence the same way (see
    /// [`LabResult::timings`] for what a written `result.json`
    /// structurally cannot contain).
    #[serde(rename = "timings", skip_serializing_if = "Option::is_none")]
    pub timings: Option<&'a StageTimings>,
}

impl<'a> From<&'a LabResult> for ResultDocument<'a> {
    fn from(result: &'a LabResult) -> Self {
        // Exhaustive destructuring, here and in every other projection
        // in this module: a field added to a model type is then a
        // compile error in the code that writes the document, rather
        // than evidence that silently stops being published.
        let LabResult {
            schema_version,
            run_id,
            summary,
            environments,
            fixtures,
            policy,
            diagnostics,
            migration,
            timings,
        } = result;

        Self {
            schema_version: schema_version.as_str(),
            run_id: run_id.as_str(),
            summary,
            environments,
            fixtures: fixtures.iter().map(FixtureDocument::from).collect(),
            policy: PolicyDocument::from(policy),
            diagnostics,
            migration: migration
                .as_ref()
                .map(|cases| cases.iter().map(MigrationSection::from).collect()),
            timings: timings.as_ref(),
        }
    }
}

/// One compared unit: an admission fixture, a Gateway route contract, or
/// (when nothing at all was observed) neither.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct FixtureDocument<'a> {
    /// Which fixture or route contract this is.
    #[serde(rename = "fixtureId")]
    pub fixture_id: &'a str,
    /// The single bucket this entry was counted in, as
    /// [`FixtureComparison::bucket`] decides it.
    ///
    /// Written even though [`ResultDocument::summary`] already carries
    /// the counts: without it a consumer that wants to know *why* a
    /// fixture is inconclusive rather than identical has to
    /// re-implement the precedence rule, and a re-implementation is
    /// exactly what would drift. It is derived at serialization time
    /// from the same method the terminal and HTML renderers call, so it
    /// is not a second stored copy that can disagree.
    #[serde(rename = "bucket")]
    pub bucket: &'static str,
    /// The admission evidence, or `null` for an entry that was never
    /// replayed through two API servers.
    #[serde(rename = "admission")]
    pub admission: Option<AdmissionSection<'a>>,
    /// The Gateway *reconciliation* evidence, or `null` for an entry
    /// that is not a Gateway route contract.
    #[serde(rename = "gatewayReconciliation")]
    pub gateway_reconciliation: Option<ReconciliationSection<'a>>,
    /// The Gateway *traffic* evidence, or `null` for an entry that is
    /// not a Gateway route contract.
    ///
    /// A route contract whose probes never ran still gets a section,
    /// with [`TrafficEvidence::Unavailable`]: "we did not probe" and
    /// "this is not a route contract" are different facts and the
    /// document states both.
    #[serde(rename = "traffic")]
    pub traffic: Option<TrafficSection<'a>>,
    /// Every graded change attributed to this entry, in
    /// `admissionlab-policy`'s deterministic order. The same entries
    /// appear, with the same [`ChangeDocument::id`], in
    /// [`PolicyDocument::changes`].
    #[serde(rename = "changes")]
    pub changes: Vec<ChangeDocument<'a>>,
}

impl<'a> From<&'a FixtureComparison> for FixtureDocument<'a> {
    fn from(fixture: &'a FixtureComparison) -> Self {
        let FixtureComparison {
            fixture_id,
            admission,
            gateway,
            changes,
        } = fixture;

        Self {
            fixture_id: fixture_id.as_str(),
            bucket: fixture.bucket().as_str(),
            admission: admission.as_ref().map(AdmissionSection::from),
            gateway_reconciliation: gateway.as_ref().map(ReconciliationSection::from),
            traffic: gateway.as_ref().map(TrafficSection::from),
            changes: changes.iter().map(ChangeDocument::from).collect(),
        }
    }
}

/// What both sides' API servers did with one fixture.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct AdmissionSection<'a> {
    /// Whether the two decisions mean the same thing and can be
    /// compared at all -- `admissionlab_diff::decision_comparability`'s
    /// answer, carrying each side's own verbatim reason when they
    /// cannot.
    #[serde(rename = "comparability")]
    pub comparability: DecisionComparability,
    /// What the baseline side did, verbatim.
    #[serde(rename = "baseline")]
    pub baseline: &'a AdmissionOutcome,
    /// What the candidate side did, verbatim.
    #[serde(rename = "candidate")]
    pub candidate: &'a AdmissionOutcome,
    /// How complete each side's webhook-chain trace is, surfaced here so
    /// a consumer reading the divergence fields below never has to guess
    /// whether an empty `invocations` list means "no webhook ran" or
    /// "the trace could not be captured".
    #[serde(rename = "traceEvidence")]
    pub trace_evidence: SideTraceEvidence,
    /// How the first divergence was established, as one written word --
    /// including [`DivergenceAttribution::Unattributed`] for the case
    /// where [`Self::first_divergence`] is `null`.
    #[serde(rename = "divergenceConfidence")]
    pub divergence_confidence: DivergenceAttribution,
    /// Where in the webhook chain the two sides first diverged, when
    /// that could be attributed at all.
    #[serde(rename = "firstDivergence")]
    pub first_divergence: Option<&'a DivergenceEvidence>,
}

impl<'a> From<&'a AdmissionComparison> for AdmissionSection<'a> {
    fn from(admission: &'a AdmissionComparison) -> Self {
        let AdmissionComparison {
            baseline,
            candidate,
            first_divergence,
        } = admission;

        Self {
            comparability: decision_comparability(baseline, candidate),
            baseline,
            candidate,
            trace_evidence: SideTraceEvidence {
                baseline: baseline.trace.evidence,
                candidate: candidate.trace.evidence,
            },
            divergence_confidence: DivergenceAttribution::of(first_divergence.as_ref()),
            first_divergence: first_divergence.as_ref(),
        }
    }
}

/// Each side's webhook-chain trace completeness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SideTraceEvidence {
    /// The baseline side's own trace evidence level.
    #[serde(rename = "baseline")]
    pub baseline: TraceEvidence,
    /// The candidate side's own trace evidence level.
    #[serde(rename = "candidate")]
    pub candidate: TraceEvidence,
}

/// How a first divergence was established, with no `null` to interpret.
///
/// Three of the four values are
/// `admissionlab_diff::DivergenceConfidence`'s own, tag for tag. The
/// fourth exists because the model's `first_divergence` is an
/// [`Option`], and Global Constraint 15 does not accept an absent value
/// as a statement: a consumer must be able to read "no attribution was
/// produced" without inferring it from a missing object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub enum DivergenceAttribution {
    /// The captured evidence locates the divergence directly.
    #[serde(rename = "observed")]
    Observed,
    /// The divergence was narrowed down from indirect evidence. A
    /// renderer must label it rather than present it as an observation.
    #[serde(rename = "inferred")]
    Inferred,
    /// Attribution was attempted and the evidence does not locate the
    /// divergence.
    #[serde(rename = "unknown")]
    Unknown,
    /// No attribution was produced at all -- the case where
    /// [`AdmissionSection::first_divergence`] is `null`. Never "the two
    /// sides did not diverge".
    #[serde(rename = "unattributed")]
    Unattributed,
}

impl DivergenceAttribution {
    /// The written answer for an optional [`DivergenceEvidence`].
    #[must_use]
    fn of(evidence: Option<&DivergenceEvidence>) -> Self {
        match evidence.map(|evidence| evidence.confidence) {
            Some(DivergenceConfidence::Observed) => Self::Observed,
            Some(DivergenceConfidence::Inferred) => Self::Inferred,
            Some(DivergenceConfidence::Unknown) => Self::Unknown,
            None => Self::Unattributed,
        }
    }
}

/// What both sides' controllers did with one Gateway route contract.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ReconciliationSection<'a> {
    /// The route contract both sides are about.
    #[serde(rename = "contractId")]
    pub contract_id: &'a str,
    /// How comparable the two sides' reconciliation evidence is --
    /// `admissionlab_gateway::gateway_comparability`'s answer.
    #[serde(rename = "comparability")]
    pub comparability: GatewayComparability,
    /// Each side's own evidence level, stated separately from
    /// [`Self::comparability`] because `comparable` carries neither
    /// side's level in its own payload.
    #[serde(rename = "evidenceLevel")]
    pub evidence_level: SideEvidenceLevel,
    /// The baseline side's observed conditions, verbatim.
    #[serde(rename = "baseline")]
    pub baseline: &'a ReconciliationEvidence,
    /// The candidate side's observed conditions, verbatim.
    #[serde(rename = "candidate")]
    pub candidate: &'a ReconciliationEvidence,
}

impl<'a> From<&'a GatewayCaseComparison> for ReconciliationSection<'a> {
    fn from(gateway: &'a GatewayCaseComparison) -> Self {
        Self {
            contract_id: gateway.contract_id(),
            comparability: gateway.comparability(),
            evidence_level: SideEvidenceLevel {
                baseline: gateway_evidence_level(&gateway.baseline),
                candidate: gateway_evidence_level(&gateway.candidate),
            },
            baseline: &gateway.baseline.reconciliation,
            candidate: &gateway.candidate.reconciliation,
        }
    }
}

/// Each side's own Gateway evidence level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SideEvidenceLevel {
    /// The baseline side's own evidence level.
    #[serde(rename = "baseline")]
    pub baseline: GatewayEvidenceLevel,
    /// The candidate side's own evidence level.
    #[serde(rename = "candidate")]
    pub candidate: GatewayEvidenceLevel,
}

/// What both sides' data planes returned for one route contract's
/// probes.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TrafficSection<'a> {
    /// The route contract these probes belong to.
    #[serde(rename = "contractId")]
    pub contract_id: &'a str,
    /// Whether probe evidence exists on both sides, one, or neither.
    #[serde(rename = "evidence")]
    pub evidence: TrafficEvidence,
    /// Every probe both sides answered, paired by index -- the pairing
    /// `admissionlab_gateway::diff` itself uses, because a suite sends
    /// its probes in declaration order on both sides.
    #[serde(rename = "pairs")]
    pub pairs: Vec<ProbeExchange<'a>>,
    /// Baseline probes with no candidate counterpart.
    ///
    /// Carried rather than dropped, and carried *here* rather than
    /// folded into [`Self::pairs`] with a fabricated other half: a
    /// one-sided probe is real evidence about one side and no evidence
    /// at all about the other.
    #[serde(rename = "unpairedBaseline")]
    pub unpaired_baseline: &'a [HttpProbeResult],
    /// Candidate probes with no baseline counterpart.
    #[serde(rename = "unpairedCandidate")]
    pub unpaired_candidate: &'a [HttpProbeResult],
}

impl<'a> From<&'a GatewayCaseComparison> for TrafficSection<'a> {
    fn from(gateway: &'a GatewayCaseComparison) -> Self {
        let baseline = &gateway.baseline.probes;
        let candidate = &gateway.candidate.probes;
        let paired = baseline.len().min(candidate.len());

        Self {
            contract_id: gateway.contract_id(),
            evidence: TrafficEvidence::of(baseline.len(), candidate.len()),
            pairs: (0..paired)
                .map(|index| ProbeExchange {
                    index,
                    baseline: &baseline[index],
                    candidate: &candidate[index],
                })
                .collect(),
            unpaired_baseline: &baseline[paired..],
            unpaired_candidate: &candidate[paired..],
        }
    }
}

/// Whether a route contract's traffic evidence supports a comparison.
///
/// The Gateway data plane's counterpart of
/// `admissionlab_admission::TraceEvidence`, and named with the same
/// three words for the same reason: an empty [`TrafficSection::pairs`]
/// has two completely different meanings and the document must say
/// which one this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub enum TrafficEvidence {
    /// Both sides answered at least one probe.
    #[serde(rename = "observed")]
    Observed,
    /// Exactly one side answered any probe. Its results are real
    /// evidence about that side; nothing here compares the two.
    #[serde(rename = "partial")]
    Partial,
    /// Neither side answered a probe -- the route contract declared
    /// none, or the probes never ran. Never "both data planes behaved
    /// the same".
    #[serde(rename = "unavailable")]
    Unavailable,
}

impl TrafficEvidence {
    /// The answer for the two sides' probe counts.
    #[must_use]
    fn of(baseline: usize, candidate: usize) -> Self {
        match (baseline > 0, candidate > 0) {
            (true, true) => Self::Observed,
            (true, false) | (false, true) => Self::Partial,
            (false, false) => Self::Unavailable,
        }
    }
}

/// One probe, answered by both sides.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ProbeExchange<'a> {
    /// The probe's position in the route contract's declared probe
    /// list, which is what pairs the two sides.
    #[serde(rename = "index")]
    pub index: usize,
    /// What the baseline stack's data plane returned.
    #[serde(rename = "baseline")]
    pub baseline: &'a HttpProbeResult,
    /// What the candidate stack's data plane returned for the same
    /// probe.
    #[serde(rename = "candidate")]
    pub candidate: &'a HttpProbeResult,
}

/// What one Ingress-to-Gateway migration case did on both sides
/// (ROADMAP Task 8.8).
///
/// A sibling of [`FixtureDocument`] rather than a section inside one —
/// see [`crate::model::LabResult::migration`] for the argument, which is
/// about vocabularies rather than about layout.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct MigrationSection<'a> {
    /// The migration case both sides are about.
    #[serde(rename = "caseId")]
    pub case_id: &'a str,
    /// Whether the two sides were comparable at all.
    #[serde(rename = "comparability")]
    pub comparability: MigrationComparability,
    /// The same answer in prose —
    /// `admissionlab_gateway::MigrationComparability::reason`'s own
    /// words, carried because "the variant name alone would leave
    /// 'incomparable' unexplained" and because a `result.json` is read
    /// by people as well as by programs.
    #[serde(rename = "comparabilityReason")]
    pub comparability_reason: &'static str,
    /// Every observed difference, in the Gateway engine's own
    /// deterministic order, each with the severity the run graded it.
    #[serde(rename = "changes")]
    pub changes: Vec<MigrationChangeDocument<'a>>,
    /// Every probe both sides answered, paired by index — the same
    /// pairing, and the same [`ProbeExchange`] shape, the Gateway
    /// traffic section uses, because it is the same question asked of a
    /// differently-shaped pair of stacks.
    #[serde(rename = "probes")]
    pub probes: Vec<ProbeExchange<'a>>,
    /// Non-portable features declared for this case that its baseline
    /// manifests do not carry. Reported, never graded — see
    /// [`crate::model::MigrationCaseComparison::unmatched_expectations`].
    #[serde(rename = "unmatchedExpectations")]
    pub unmatched_expectations: Vec<NonPortableExpectationDocument<'a>>,
}

impl<'a> From<&'a MigrationCaseComparison> for MigrationSection<'a> {
    fn from(case: &'a MigrationCaseComparison) -> Self {
        let MigrationCaseComparison {
            case_id,
            comparability,
            changes,
            probes,
            unmatched_expectations,
        } = case;

        Self {
            case_id: case_id.as_str(),
            comparability: *comparability,
            comparability_reason: comparability.reason(),
            changes: changes.iter().map(MigrationChangeDocument::from).collect(),
            probes: probes
                .iter()
                .enumerate()
                .map(|(index, pair)| ProbeExchange {
                    index,
                    baseline: &pair.baseline,
                    candidate: &pair.candidate,
                })
                .collect(),
            unmatched_expectations: unmatched_expectations
                .iter()
                .map(NonPortableExpectationDocument::from)
                .collect(),
        }
    }
}

/// One migration behavior change, graded.
///
/// Carries no `id`: [`semantic_change_id`] exists because a
/// `SemanticChange` appears *twice* in this document (per fixture and
/// run-wide) and the two lists need a join key. A migration change
/// appears exactly once, under the case that observed it, so an
/// identifier here would be a key with nothing to join to.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct MigrationChangeDocument<'a> {
    /// Which routing behavior moved, in
    /// `admissionlab_gateway::MigrationBehaviorKind`'s own vocabulary —
    /// deliberately *not* a `SemanticChangeKind`.
    #[serde(rename = "kind")]
    pub kind: MigrationBehaviorKind,
    /// What was observed, in full enough detail to check the claim.
    /// Written by the Gateway engine and deterministic.
    #[serde(rename = "detail")]
    pub detail: &'a str,
    /// Whether the case's author declared this difference in writing.
    /// Only a `non_portable_feature` change can be `true`.
    #[serde(rename = "expected")]
    pub expected: bool,
    /// How much the run says it matters. Decided by
    /// `admissionlab_cli::pipeline::migration`, never here.
    #[serde(rename = "severity")]
    pub severity: Severity,
}

impl<'a> From<&'a GradedMigrationChange> for MigrationChangeDocument<'a> {
    fn from(graded: &'a GradedMigrationChange) -> Self {
        let GradedMigrationChange { change, severity } = graded;
        let MigrationBehaviorChange {
            kind,
            detail,
            expected,
        } = change;

        Self {
            kind: *kind,
            detail: detail.as_str(),
            expected: *expected,
            severity: *severity,
        }
    }
}

/// One declared non-portability that the baseline manifests do not
/// actually carry.
///
/// A projection rather than the configuration type itself:
/// `admissionlab_spec::NonPortableFeatureExpectation` derives
/// `Deserialize` (a user writes it) and not `Serialize`, and this
/// document owns its own `camelCase` keys — see this module's "Casing".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct NonPortableExpectationDocument<'a> {
    /// The feature the case declared, verbatim.
    #[serde(rename = "feature")]
    pub feature: &'a str,
    /// The author's written justification, verbatim.
    #[serde(rename = "reason")]
    pub reason: &'a str,
}

impl<'a> From<&'a NonPortableFeatureExpectation> for NonPortableExpectationDocument<'a> {
    fn from(expectation: &'a NonPortableFeatureExpectation) -> Self {
        Self {
            feature: expectation.feature.as_str(),
            reason: expectation.reason.as_str(),
        }
    }
}

/// The run's verdict and every graded change.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct PolicyDocument<'a> {
    /// Pass, warn, or fail.
    #[serde(rename = "disposition")]
    pub disposition: PolicyDisposition,
    /// Every graded change in the run, in `admissionlab-policy`'s
    /// deterministic order.
    #[serde(rename = "changes")]
    pub changes: Vec<ChangeDocument<'a>>,
    /// Expectations that matched nothing this run.
    ///
    /// Spelled `staleExpectations` rather than `stale_expectations`:
    /// this document is this crate's, and the freeze made its own keys
    /// uniformly `camelCase` (see this module's "Casing"). The change
    /// is safe precisely because it happens *at* the version bump.
    #[serde(rename = "staleExpectations")]
    pub stale_expectations: &'a [StaleExpectation],
}

impl<'a> From<&'a PolicyResult> for PolicyDocument<'a> {
    fn from(policy: &'a PolicyResult) -> Self {
        let PolicyResult {
            disposition,
            changes,
            stale_expectations,
        } = policy;

        Self {
            disposition: *disposition,
            changes: changes.iter().map(ChangeDocument::from).collect(),
            stale_expectations,
        }
    }
}

/// One graded change, with the identifier that ties its two appearances
/// (per fixture, and run-wide) together.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ChangeDocument<'a> {
    /// This change's stable identifier -- see [`semantic_change_id`].
    #[serde(rename = "id")]
    pub id: String,
    /// What differed, verbatim, in `admissionlab-diff`'s own wire
    /// shape.
    #[serde(rename = "change")]
    pub change: &'a SemanticChange,
    /// How much `admissionlab-policy` says it matters. Never decided
    /// here.
    #[serde(rename = "severity")]
    pub severity: Severity,
    /// Whether an `expectations.yaml` entry accounted for it.
    #[serde(rename = "expected")]
    pub expected: bool,
}

impl<'a> From<&'a ClassifiedChange> for ChangeDocument<'a> {
    fn from(classified: &'a ClassifiedChange) -> Self {
        let ClassifiedChange {
            change,
            severity,
            expected,
        } = classified;

        Self {
            id: semantic_change_id(change),
            change,
            severity: *severity,
            expected: *expected,
        }
    }
}

/// The `sc-` prefix every change identifier carries, so one is
/// recognizable on sight in a log line or a CI comment.
const CHANGE_ID_PREFIX: &str = "sc-";

/// How many hex characters of the digest a change identifier keeps.
///
/// Sixteen: 64 bits, which is far more than a run's few hundred changes
/// need to stay distinct, and short enough to read in a terminal
/// column. The identifier is a join key within one document, not a
/// cryptographic commitment, and truncation is stated here rather than
/// left for a reader to discover from the string length.
const CHANGE_ID_HEX_CHARS: usize = 16;

/// This change's identifier: `sc-` followed by the first
/// sixteen hex characters of the SHA-256 of a canonical encoding of the
/// claim.
///
/// # Content-derived, not index-based
///
/// Every change appears **twice** in a result document: once under the
/// fixture it was attributed to, and once in the run-wide
/// [`PolicyDocument::changes`] list. An index-based identifier would
/// give the same claim two different numbers in those two lists, so the
/// join a consumer actually wants ("show me this fixture's critical
/// change in the run-wide ordering") would have to be re-derived by
/// comparing whole payloads. Deriving the identifier from the claim
/// makes the two lists agree by construction, and makes the identifier
/// survive any reordering of either list.
///
/// # What is hashed
///
/// The change's *identity*: its kind, the fixture it belongs to, the
/// object path and subject it is about, and the two values it compares.
/// Deliberately **not** hashed:
///
/// - `origin` -- where the divergence was attributed is evidence
///   *about* the claim, not part of what is being claimed. A run that
///   attributes a change more precisely is still making the same
///   claim.
/// - `severity` and `expected` -- those are `admissionlab-policy`'s
///   grades, and re-grading a change (a new `expectations.yaml` entry,
///   a different `failOn`) must not renumber it.
///
/// # Stability and uniqueness
///
/// Stable *within* a run, as the roadmap requires, and in fact stable
/// across runs that observe the same difference -- which is a useful
/// property this scheme gets for free rather than a promise the schema
/// makes. Two changes that are identical in every hashed field get the
/// same identifier, because they are the same claim stated twice; the
/// identifier names a claim and is not a uniqueness key over list
/// positions.
///
/// # Panics
///
/// Unreachable in practice. The canonical encoding is a struct of
/// strings and already-parsed [`serde_json::Value`]s, none of which can
/// fail to serialize; the only `serde_json` failure modes for a
/// `to_vec` are a `Serialize` implementation that returns an error and a
/// map with non-string keys, and neither exists here. It panics rather
/// than falling back to a placeholder identifier because a *wrong*
/// identifier would silently break the join between the per-fixture and
/// run-wide change lists.
#[must_use]
pub fn semantic_change_id(change: &SemanticChange) -> String {
    // Exhaustive, so that a new `SemanticChange` field is a compile
    // error here and someone has to decide whether it is part of the
    // claim's identity.
    let SemanticChange {
        kind,
        fixture_id,
        object_path,
        subject,
        baseline,
        candidate,
        origin: _,
    } = change;

    let identity = ChangeIdentity {
        kind: *kind,
        fixture_id: fixture_id.as_str(),
        object_path: object_path.as_deref(),
        subject: subject.as_deref(),
        baseline: baseline.as_ref(),
        candidate: candidate.as_ref(),
    };

    // `serde_json::Value`'s maps are `BTreeMap`-backed workspace-wide
    // (`preserve_order` is off everywhere -- `crate::json` documents the
    // same dependency on it), so two captures of one object hash
    // identically regardless of the key order they arrived in.
    let canonical = serde_json::to_vec(&identity)
        .expect("a change identity is JSON values and strings, which always serialize");

    let digest = Sha256::digest(&canonical);
    let mut id = String::with_capacity(CHANGE_ID_PREFIX.len() + CHANGE_ID_HEX_CHARS);
    id.push_str(CHANGE_ID_PREFIX);
    for byte in digest.iter().take(CHANGE_ID_HEX_CHARS / 2) {
        use std::fmt::Write as _;
        write!(id, "{byte:02x}").expect("writing to a String never fails");
    }
    id
}

/// Exactly the fields [`semantic_change_id`] hashes, in a fixed order.
///
/// A named struct rather than an ad-hoc tuple or a concatenated string:
/// the encoding is part of the frozen document's contract (two
/// implementations of this project must agree on it), and a struct with
/// pinned key names is the version of that encoding a reader can check.
#[derive(Serialize)]
struct ChangeIdentity<'a> {
    kind: SemanticChangeKind,
    #[serde(rename = "fixtureId")]
    fixture_id: &'a str,
    #[serde(rename = "objectPath")]
    object_path: Option<&'a str>,
    subject: Option<&'a str>,
    baseline: Option<&'a Value>,
    candidate: Option<&'a Value>,
}
