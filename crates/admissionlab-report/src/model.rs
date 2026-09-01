//! The report-ready shape of one comparison run.
//!
//! [`LabResult`] is the single document every renderer in this crate
//! reads: the terminal summary, the machine-readable JSON artifact, and
//! the standalone HTML page are three views of *this* value and never of
//! the raw capture. Its field list is frozen by §1.2's cross-task type
//! registry; this module pins the wire names and defines how a run's
//! fixtures are counted into [`RunSummary`]'s five buckets.
//!
//! # What this module does not do
//!
//! It never grades anything. Severity is `admissionlab-policy`'s
//! decision and reaches here already attached to each
//! [`ClassifiedChange`] (§1.1: "do not let report rendering decide
//! severity"). Counting reads those grades; it does not second-guess
//! them. Likewise, whether a fixture's two sides were comparable at all
//! is `admissionlab-diff`'s judgement --
//! [`admissionlab_diff::decision_comparability`] -- and
//! [`FixtureComparison::bucket`] calls it rather than re-deriving the
//! rule from `AdmissionDecision` here, so the two crates can never drift
//! into disagreeing about what "incomparable" means.
//!
//! # Serialization boundary
//!
//! Every field this crate *owns* is pinned with an explicit
//! `#[serde(rename)]` in `camelCase`, so renaming a Rust field can never
//! silently change the report contract.
//!
//! Types owned by **other** crates keep their own already-pinned wire
//! shape and are not re-pinned here. That covers
//! [`admissionlab_policy::PolicyResult`] (and the
//! [`ClassifiedChange`]/[`admissionlab_policy::Severity`]/
//! [`admissionlab_policy::StaleExpectation`] values inside it),
//! [`admissionlab_diff::SemanticChange`] and
//! [`admissionlab_diff::DivergenceEvidence`],
//! [`admissionlab_admission::AdmissionOutcome`] (with its
//! `AdmissionDecision`, `AdmissionTrace`, and `WebhookInvocation`),
//! [`admissionlab_core::Diagnostic`], and
//! [`admissionlab_core::StageTimings`] -- which happens to pin
//! `camelCase` names of its own, so the timings block reads like this
//! crate's own fields without this crate having re-pinned anything.
//! Those crates pinned their tags
//! deliberately -- often `snake_case` enum tags and plain Rust field
//! names -- and a second `#[serde(rename_all)]` layer here would either
//! be ignored (it cannot reach into a foreign type) or, worse, invite
//! someone to wrap them in local mirror structs that drift. The visible
//! consequence is that a serialized `LabResult` mixes `camelCase` keys
//! (this crate's) with the foreign crates' own spellings; that is the
//! honest rendering of who owns what, and the Alpha golden file records
//! it exactly.
//!
//! [`admissionlab_core::RunId`] and [`admissionlab_core::FixtureId`]
//! implement neither `Serialize` nor `Deserialize` by design, so both are
//! written through a `serialize_with` helper that emits their bare string
//! form -- the same convention
//! [`admissionlab_admission::AdmissionOutcome`] already uses for its own
//! `fixture_id`.
//!
//! # Emit-only
//!
//! [`LabResult`] implements [`Serialize`] and never
//! `serde::Deserialize`, because several of the foreign types it carries
//! do not implement it either (`Diagnostic`, `SemanticChange`,
//! `AdmissionOutcome`, `PolicyResult`). A result is built once from a
//! real run and serialized outward; nothing in this project reads one
//! back in.

use admissionlab_admission::AdmissionOutcome;
use admissionlab_core::{Diagnostic, FixtureId, RunId, StageTimings};
use admissionlab_diff::{DivergenceEvidence, decision_comparability};
use admissionlab_policy::{PolicyResult, Severity};
use serde::{Serialize, Serializer};

/// The Alpha result schema identifier, written into
/// [`LabResult::schema_version`].
///
/// **Experimental.** Alpha makes no compatibility promise about this
/// document's shape; the schema is frozen at Beta (Global Constraint 9).
/// The version string is nonetheless explicit from the first release so
/// a consumer can tell an Alpha document from whatever follows it.
pub const SCHEMA_VERSION: &str = "admissionlab.io/result/v1alpha1";

/// One comparison run, ready to render.
///
/// Derives no `Default`: a `LabResult` exists only because a real run
/// produced one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LabResult {
    /// The result schema this document conforms to. Always
    /// [`SCHEMA_VERSION`] for documents this crate produces; kept as a
    /// `String` (rather than a unit type that can only ever be one
    /// value) because §1.2 freezes it as one and because a future
    /// version must be representable without a breaking type change.
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,
    /// The run this result describes.
    #[serde(rename = "runId", serialize_with = "serialize_run_id")]
    pub run_id: RunId,
    /// The five-bucket count over [`LabResult::fixtures`]. Build it with
    /// [`RunSummary::from_fixtures`] rather than by hand, so the counts
    /// and the per-fixture buckets can never disagree.
    #[serde(rename = "summary")]
    pub summary: RunSummary,
    /// What each side actually was, as observed.
    #[serde(rename = "environments")]
    pub environments: EnvironmentSummary,
    /// Every fixture replayed, in the order the run replayed them.
    #[serde(rename = "fixtures")]
    pub fixtures: Vec<FixtureComparison>,
    /// The run's policy verdict and every graded change, in
    /// `admissionlab-policy`'s own documented deterministic order.
    #[serde(rename = "policy")]
    pub policy: PolicyResult,
    /// Run-level diagnostics -- things that happened to the *run*, not
    /// to one fixture. Per-fixture diagnostics stay on that fixture's
    /// [`AdmissionOutcome::diagnostics`].
    #[serde(rename = "diagnostics")]
    pub diagnostics: Vec<Diagnostic>,
    /// How long each stage of the run took (ROADMAP Task 5.7), or `None`
    /// for a result whose producer did not measure.
    ///
    /// Optional for two independent reasons, and both are load-bearing.
    /// A caller that has no timings must be able to say so rather than
    /// invent zeroes (Global Constraint 15) -- that is what `None` is
    /// for, and it is why the key is *omitted* rather than written as
    /// `null` when absent. And the key being addable at all without
    /// breaking a consumer is a property of this document being
    /// emit-only: nothing in this project deserializes a `LabResult`, so
    /// no `deny_unknown_fields` reader exists to break.
    ///
    /// # What is structurally missing from a written `result.json`
    ///
    /// The snapshot is taken when this value is *assembled*, which is
    /// before the reporting stage renders and writes it, and long before
    /// cleanup deletes the clusters. So a `result.json`'s timings never
    /// carry `reportingMs` and never carry `cleanup`; those two stages
    /// are measured, and `admissionlab` prints them in its own final
    /// line, but they cannot be inside the document whose writing is one
    /// of them. `admissionlab_core::timing`'s module documentation states
    /// the same thing from the recorder's side.
    ///
    /// Placed last in declaration order deliberately: `serde` emits
    /// fields in source order, so appending here adds one block to the
    /// end of every existing document rather than shifting the whole file
    /// in the golden diff.
    #[serde(rename = "timings", skip_serializing_if = "Option::is_none")]
    pub timings: Option<StageTimings>,
}

/// How many fixtures landed in each of the five buckets.
///
/// The five counts partition the run: they always sum to
/// `fixtures_total`, because [`FixtureComparison::bucket`] assigns every
/// fixture exactly one bucket. See that method for the precedence rule.
///
/// Derives no `Default`: a summary of nothing is not a meaningful value,
/// and [`RunSummary::from_fixtures`] on an empty slice already spells
/// "an empty run" honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RunSummary {
    /// How many fixtures the run replayed. Equals the sum of the other
    /// five fields.
    #[serde(rename = "fixturesTotal")]
    pub fixtures_total: usize,
    /// Fixtures whose two sides were comparable and produced no
    /// behavior change at all.
    #[serde(rename = "identical")]
    pub identical: usize,
    /// Fixtures whose differences were all accounted for -- see
    /// [`FixtureBucket::Expected`] for exactly what "accounted for"
    /// covers.
    #[serde(rename = "expected")]
    pub expected: usize,
    /// Fixtures carrying at least one unexpected `warning` change.
    #[serde(rename = "warnings")]
    pub warnings: usize,
    /// Fixtures carrying at least one unexpected `critical` change.
    #[serde(rename = "critical")]
    pub critical: usize,
    /// Fixtures whose evidence does not support a comparison at all.
    #[serde(rename = "inconclusive")]
    pub inconclusive: usize,
}

impl RunSummary {
    /// Counts `fixtures` into the five buckets.
    ///
    /// This is the only supported way to build a `RunSummary`: it is
    /// defined entirely in terms of [`FixtureComparison::bucket`], so a
    /// summary produced here always agrees with the per-fixture bucket a
    /// renderer shows next to each row.
    #[must_use]
    pub fn from_fixtures(fixtures: &[FixtureComparison]) -> Self {
        let mut summary = Self {
            fixtures_total: fixtures.len(),
            identical: 0,
            expected: 0,
            warnings: 0,
            critical: 0,
            inconclusive: 0,
        };
        for fixture in fixtures {
            match fixture.bucket() {
                FixtureBucket::Identical => summary.identical += 1,
                FixtureBucket::Expected => summary.expected += 1,
                FixtureBucket::Warnings => summary.warnings += 1,
                FixtureBucket::Critical => summary.critical += 1,
                FixtureBucket::Inconclusive => summary.inconclusive += 1,
            }
        }
        summary
    }
}

/// Which single bucket a fixture is counted in.
///
/// Deliberately not serialized as part of [`LabResult`]: the document
/// carries the *counts* ([`RunSummary`]) and the evidence each count was
/// derived from, and a per-fixture bucket field would be a third copy of
/// the same fact that could fall out of sync with either. Renderers call
/// [`FixtureComparison::bucket`] when they need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FixtureBucket {
    /// Comparable, and no change at all.
    Identical,
    /// Changes exist, and none of them is an unexpected `warning` or
    /// `critical`.
    ///
    /// Two kinds of change land here. The first is the obvious one: a
    /// change an `expectations.yaml` entry explicitly accounted for. The
    /// second is a change policy graded `info` -- a real, visible
    /// difference that this run's policy declined to warn or fail on.
    /// Counting an `info` change as `identical` would state that nothing
    /// changed, which is false; counting it as `warnings` would
    /// contradict the grade policy gave it. `expected` is the honest
    /// third answer: the run saw differences and accounted for all of
    /// them.
    Expected,
    /// At least one unexpected `warning` change, and no unexpected
    /// `critical` one.
    Warnings,
    /// At least one unexpected `critical` change.
    Critical,
    /// The evidence does not support a comparison for this fixture.
    Inconclusive,
}

impl FixtureBucket {
    /// This bucket's stable lowercase name, matching the corresponding
    /// [`RunSummary`] field's serialized key.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identical => "identical",
            Self::Expected => "expected",
            Self::Warnings => "warnings",
            Self::Critical => "critical",
            Self::Inconclusive => "inconclusive",
        }
    }
}

/// What both sides of the comparison actually were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvironmentSummary {
    /// The baseline side.
    #[serde(rename = "baseline")]
    pub baseline: EnvironmentReport,
    /// The candidate side.
    #[serde(rename = "candidate")]
    pub candidate: EnvironmentReport,
}

/// One side's observed environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvironmentReport {
    /// The Kubernetes version this side ran, as reported by the cluster.
    #[serde(rename = "kubernetes")]
    pub kubernetes: String,
    /// The stack components installed on this side, in the order they
    /// were installed.
    #[serde(rename = "components")]
    pub components: Vec<ComponentReport>,
}

/// One installed component's identity, for provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentReport {
    /// The component's name, as the lab configuration named it.
    #[serde(rename = "name")]
    pub name: String,
    /// The version actually installed, as observed.
    #[serde(rename = "version")]
    pub version: String,
}

/// Everything the run observed about one fixture, on both sides.
///
/// Derives no `Default`: it holds evidence about a real fixture.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FixtureComparison {
    /// Which fixture this is.
    #[serde(rename = "fixtureId", serialize_with = "serialize_fixture_id")]
    pub fixture_id: FixtureId,
    /// The admission comparison, when the fixture was replayed through
    /// both sides' API servers. `None` means no admission evidence
    /// exists for this fixture at all -- which is why such a fixture is
    /// [`FixtureBucket::Inconclusive`] rather than
    /// [`FixtureBucket::Identical`].
    #[serde(rename = "admission")]
    pub admission: Option<AdmissionComparison>,
    /// The Gateway comparison, when this entry is a Gateway route
    /// contract rather than an admission fixture (ROADMAP Task 6.11).
    /// `None` for every admission fixture, and for a lab with no
    /// `gateway:` section at all.
    ///
    /// # An entry carries one or the other, never both
    ///
    /// A `FixtureComparison` is one *compared unit* of a run, and a run
    /// has two kinds: an admission fixture replayed through both API
    /// servers, and a Gateway route contract observed on both sides.
    /// Phase 6's own vocabulary already calls the latter a fixture
    /// ("Gateway fixtures are persisted in the disposable cluster"), and
    /// its [`admissionlab_gateway::model::RouteContract::id`] is unique
    /// within its suite exactly as a `FixtureId` is within a corpus --
    /// which is what lets one carry the other's identity and lets a
    /// Gateway [`admissionlab_diff::SemanticChange`] flow through the
    /// same generic, already-graded channel as every admission one.
    ///
    /// The two are never mixed on one entry, and [`Self::bucket`] is
    /// where that shows: an entry with neither is inconclusive because
    /// nothing was observed at all.
    #[serde(rename = "gateway")]
    pub gateway: Option<GatewayCaseComparison>,
    /// Every graded change attributed to this fixture, in
    /// `admissionlab-policy`'s deterministic order.
    ///
    /// The same [`ClassifiedChange`] values also appear in
    /// [`LabResult::policy`]`.changes`, which is the run-wide list.
    /// Carrying them per fixture as well is what makes a per-fixture
    /// drill-down possible without a renderer re-grouping the flat list
    /// by `fixture_id` (and re-implementing the grouping three times).
    ///
    /// [`ClassifiedChange`]: admissionlab_policy::ClassifiedChange
    #[serde(rename = "changes")]
    pub changes: Vec<admissionlab_policy::ClassifiedChange>,
}

impl FixtureComparison {
    /// The single bucket this fixture is counted in.
    ///
    /// # Precedence
    ///
    /// Checked in this order, first match wins:
    ///
    /// 1. **[`Inconclusive`](FixtureBucket::Inconclusive)** -- the
    ///    evidence does not support a comparison, as
    ///    [`Self::evidence_supports_a_comparison`] decides: nothing was
    ///    observed at all, or
    ///    [`admissionlab_diff::decision_comparability`] reports
    ///    [`DecisionComparability::Incomparable`] for the two admission
    ///    outcomes (at least one side could not be replayed as a real
    ///    admission decision), or
    ///    [`admissionlab_gateway::gateway_comparability`] reports
    ///    anything but `Comparable` for the two Gateway case results (at
    ///    least one side's route never converged).
    /// 2. **[`Critical`](FixtureBucket::Critical)** -- any change graded
    ///    `critical` that no expectation accounted for.
    /// 3. **[`Warnings`](FixtureBucket::Warnings)** -- any change graded
    ///    `warning` that no expectation accounted for.
    /// 4. **[`Expected`](FixtureBucket::Expected)** -- any change at
    ///    all, none of them an unexpected `critical`/`warning`.
    /// 5. **[`Identical`](FixtureBucket::Identical)** -- no changes.
    ///
    /// # Why *inconclusive* outranks everything
    ///
    /// Because it is the only bucket that describes this tool's own
    /// limits rather than the stack under test. A fixture whose baseline
    /// side could not be replayed has an *unknown* candidate
    /// relationship, and any of the four lower buckets would state
    /// something about that relationship that the run did not establish
    /// -- including `critical`, which would present a one-sided
    /// observation as a proven regression. Global Constraint 15 requires
    /// missing data to read as unavailable, never as a finding.
    ///
    /// Changes attributed to an inconclusive fixture are still carried
    /// in [`FixtureComparison::changes`] and still reach
    /// [`LabResult::policy`]; the bucket decides how the fixture is
    /// *counted*, never what is *shown*.
    ///
    /// # Why *critical* outranks *warning* and *expected*
    ///
    /// A bucket per fixture has to answer "how bad is the worst thing
    /// here", and the worst unexpected finding is the only honest
    /// answer. A fixture with one unexpected critical change and nine
    /// expected ones is a fixture a human must look at.
    ///
    /// [`DecisionComparability::Incomparable`]: admissionlab_diff::DecisionComparability::Incomparable
    #[must_use]
    pub fn bucket(&self) -> FixtureBucket {
        if !self.evidence_supports_a_comparison() {
            return FixtureBucket::Inconclusive;
        }

        let mut has_unexpected_warning = false;
        for classified in &self.changes {
            if classified.expected {
                continue;
            }
            match classified.severity {
                Severity::Critical => return FixtureBucket::Critical,
                Severity::Warning => has_unexpected_warning = true,
                Severity::Info => {}
            }
        }
        if has_unexpected_warning {
            FixtureBucket::Warnings
        } else if self.changes.is_empty() {
            FixtureBucket::Identical
        } else {
            FixtureBucket::Expected
        }
    }

    /// Whether this entry's evidence supports comparing its two sides at
    /// all -- [`Self::bucket`]'s first and highest-precedence check.
    ///
    /// One question, asked of whichever kind of evidence the entry
    /// carries, and in both cases answered by the crate that owns the
    /// judgement rather than re-derived here:
    ///
    /// - an admission fixture, by
    ///   [`admissionlab_diff::decision_comparability`];
    /// - a Gateway route contract, by
    ///   [`admissionlab_gateway::gateway_comparability`], through
    ///   [`GatewayCaseComparison::comparability`].
    ///
    /// The Gateway arm is the exact mirror of the admission one, and
    /// deliberately treats
    /// [`admissionlab_gateway::GatewayComparability::Partial`] as not
    /// comparable: `Partial` means exactly one side converged, so an
    /// empty change list there is "we could not tell", not "the two
    /// sides behaved the same" -- which is the whole reason
    /// `gateway_comparability` exists separately from `diff_gateway`,
    /// and the difference this bucket has to preserve. Changes claimed
    /// on a `Partial` case are still carried, still graded, and still
    /// shown; only the *counting* records that the run did not establish
    /// the relationship.
    ///
    /// An entry with neither kind of evidence is not comparable for the
    /// simplest reason: nothing was observed.
    fn evidence_supports_a_comparison(&self) -> bool {
        match (&self.admission, &self.gateway) {
            (Some(admission), _) => {
                decision_comparability(&admission.baseline, &admission.candidate).is_comparable()
            }
            (None, Some(gateway)) => gateway.comparability().is_comparable(),
            (None, None) => false,
        }
    }
}

/// Both sides' captured admission behavior for one fixture.
///
/// Carries each side's whole [`AdmissionOutcome`] rather than a
/// summarized pair: the outcomes hold the decision, the warnings, the
/// final object, the webhook trace, and each side's own diagnostics, and
/// a report that dropped any of those would be asking a reader to trust
/// a conclusion whose evidence it withheld. This is also what makes a
/// difference that produced *no* semantic change (a reworded rejection
/// message, say) still visible -- see `admissionlab_diff::admission`'s
/// own module documentation.
///
/// Derives no `Default`: it holds evidence about a real replay.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdmissionComparison {
    /// What the baseline side did.
    #[serde(rename = "baseline")]
    pub baseline: AdmissionOutcome,
    /// What the candidate side did.
    #[serde(rename = "candidate")]
    pub candidate: AdmissionOutcome,
    /// Where in the webhook chain the two sides first diverged, when
    /// that could be attributed. `None` means attribution was not
    /// attempted or not possible -- never "the sides did not diverge".
    /// A renderer must label
    /// [`DivergenceConfidence::Inferred`]/[`Unknown`] as such rather
    /// than presenting either as an observation (Global Constraint 15).
    ///
    /// [`DivergenceConfidence::Inferred`]: admissionlab_diff::DivergenceConfidence::Inferred
    /// [`Unknown`]: admissionlab_diff::DivergenceConfidence::Unknown
    #[serde(rename = "firstDivergence")]
    pub first_divergence: Option<DivergenceEvidence>,
}

/// One Gateway route contract's result on both sides.
///
/// §1.2's cross-task type registry reserved this name so
/// [`FixtureComparison::gateway`] had a stable field type from the
/// start, and through Alpha it was an empty placeholder here (Global
/// Constraint 8 kept Gateway work off the Alpha critical path). ROADMAP
/// Task 6.11 replaced the placeholder with the real type rather than
/// keeping a second one: `admissionlab_gateway::diff` owns the
/// comparator that produces it, and a mirror struct in this crate would
/// be the competing synonym §1.2 forbids -- free to drift from what the
/// Gateway engine actually observed, in a document whose whole purpose
/// is to carry that observation.
///
/// The re-export (rather than a bare `use`) keeps
/// `admissionlab_report::GatewayCaseComparison` resolving to the
/// registry name it always did, so no consumer of this crate's public
/// surface had to change.
pub use admissionlab_gateway::GatewayCaseComparison;

/// Serializes a [`RunId`] as its bare string form ([`RunId::as_str`]).
///
/// `admissionlab-core` deliberately implements neither `Serialize` nor
/// `Deserialize` for its identifier types, so every consumer states the
/// representation it wants explicitly. This one matches every other
/// place a run identifier's string form already appears (artifact
/// directory names, `kind` cluster name suffixes).
fn serialize_run_id<S: Serializer>(value: &RunId, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value.as_str())
}

/// Serializes a [`FixtureId`] as its bare string form
/// ([`FixtureId::as_str`]) -- the same convention
/// [`AdmissionOutcome`]'s own `fixture_id` uses, so the identifier reads
/// identically wherever it appears in one document.
fn serialize_fixture_id<S: Serializer>(
    value: &FixtureId,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value.as_str())
}
