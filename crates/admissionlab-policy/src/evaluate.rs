//! Turning a lab's `policy` section plus a run's semantic changes into a
//! single deterministic pass/warn/fail decision.
//!
//! Two functions matter here. [`resolve_policy`] checks a
//! `PolicySpec` and compiles it into a [`ResolvedPolicy`] -- this is
//! where every unknown name is rejected. [`evaluate`] then grades a slice
//! of [`SemanticChange`]s against that resolved policy, producing a
//! [`PolicyResult`]. Both are pure: no clock, no network, no model, no
//! ambient state (Global Constraint 7). The same inputs always produce
//! byte-identical output, including ordering.
//!
//! # How a change's severity is decided
//!
//! Three layers, each overriding the one before it:
//!
//! 1. [`crate::severity::default_change_severity`] -- the frozen table
//!    ([`crate::severity::default_severity`]) plus the one documented
//!    Gateway direction exception, which that module states in full.
//! 2. `policy.failOn` -- naming a kind there escalates it to
//!    [`Severity::Critical`]. `failOn` is *additive only*: it can raise
//!    a kind's severity and can never lower one. A user asking for
//!    "these must fail" is stating a floor, and reading an entry as
//!    permission to *downgrade* the other sixteen kinds would silently
//!    disarm the tool for anyone who listed a single kind.
//! 3. `policy.overrides` -- a matching entry replaces the severity
//!    outright, up or down. This is the layer that can downgrade,
//!    which is exactly what `PolicyOverrideSpec`'s own documentation
//!    calls it: a *targeted exception to the blanket `failOn` policy*.
//!    An override therefore beats `failOn` on the same kind, because
//!    the narrower statement is the more specific expression of intent.
//!
//! # When several overrides match one change
//!
//! **Most specific wins; ties go to the last one declared.**
//!
//! "Most specific" is the count of restricted dimensions in the
//! override's selector ([`crate::CompiledSelector::specificity`],
//! `0..=3`). An override naming a kind *and* a fixture glob *and* a
//! subject is a strictly narrower statement than one naming only the
//! kind, and letting the broader one win would make the narrower one
//! unreachable -- dead configuration that a reader would rightly assume
//! was in effect. Ranking by specificity means an override is never
//! shadowed by something more general than itself.
//!
//! Specificity is a *count*, deliberately not a lattice over which
//! dimensions are set: two overrides restricting `fixtures` and
//! `subject` respectively are equally specific here, and neither is
//! "more targeted" in any way this crate could justify. Such ties -- and
//! ties between two genuinely identical selectors -- go to the entry
//! declared **last** in `policy.overrides`. Configuration files are read
//! top to bottom as successive refinements, so the later line is the one
//! a reader expects to win, and declaration order is something the
//! author controls exactly. Both rules together are total and
//! deterministic over any override list, which is what Global Constraint
//! 7 requires.
//!
//! # Ordering of the result
//!
//! [`PolicyResult::changes`] is sorted by, in order: fixture identifier,
//! semantic kind *wire name*, object path, subject; ties keep input
//! order (the sort is stable). Two consequences are deliberate. Sorting
//! on the wire name rather than the enum's declaration order means
//! reordering [`crate::severity::ALL_KINDS`], or adding a variant in the
//! middle of the diff crate's enum, cannot reorder anybody's report.
//! And [`None`] sorts before [`Some`] for the two optional keys (Rust's
//! own [`Ord`] for [`Option`]), so a whole-request decision flip with no
//! path lands above the field-level changes for the same fixture, which
//! is also the order a human wants to read them in.
//!
//! Severity is deliberately *not* a sort key: a report renderer that
//! wants worst-first can sort by it, but the stored order stays stable
//! when a policy override changes a single change's grade.

use std::collections::BTreeSet;

use admissionlab_diff::{SemanticChange, SemanticChangeKind};
use admissionlab_spec::PolicySpec;
// ROADMAP Task 7.2 (frozen `admissionlab.io/result/v1` result
// schema): every type this file defines is embedded verbatim in that
// document, so the schema generated from the result model has to
// describe it. Derives and `#[schemars(with = ...)]` restatements of
// what the existing `serialize_with` helpers already emit -- no field,
// name, or semantic change.
use schemars::JsonSchema;
use serde::Serialize;

use crate::error::{PolicySpecErrors, PolicyValidationError};
use crate::expectation::{ResolvedExpectations, match_expectations};
use crate::selector::{ChangeSelector, CompiledSelector};
use crate::severity::severity_name_list;
use crate::severity::{Severity, default_change_severity, kind_from_name, kind_name_list};

/// One graded behavior change.
///
/// Carries the change itself unmodified: policy grades claims, it never
/// edits or drops them. In particular an *expected* change is still
/// present, with its severity intact -- see [`ClassifiedChange::expected`].
///
/// [`Serialize`] only, never [`serde::Deserialize`], because
/// [`SemanticChange`] is itself emit-only (its `fixture_id` is an
/// `admissionlab_core::FixtureId`, which that crate deliberately does
/// not implement `Deserialize` for). A classified change is produced
/// here and serialized outward into a report; nothing reads one back.
///
/// Derives no `Default`: it holds evidence about a real run.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ClassifiedChange {
    /// The change exactly as `admissionlab-diff` claimed it.
    pub change: SemanticChange,
    /// The severity policy resolved for it -- see this module's
    /// documentation for the three layers.
    pub severity: Severity,
    /// Whether an explicit expectation accounted for this change.
    ///
    /// An expected change keeps its real severity and stays visible in
    /// the report; it simply does not contribute to
    /// [`PolicyResult::disposition`]. Expectations decide which changes
    /// are *counted*, never how any change is *graded* -- which is why
    /// this is a separate flag rather than a severity downgrade.
    ///
    /// Always `false` when reached through [`evaluate`], which evaluates
    /// against no expectations; [`evaluate_with_expectations`] is what
    /// sets it. See [`crate::expectation`].
    pub expected: bool,
}

/// An expectation that matched nothing in this run.
///
/// Surfaced so a stale entry in an `expectations.yaml` is visible rather
/// than silently harmless: an expectation written for a regression that
/// has since been fixed keeps suppressing nothing, and the only way a
/// user finds out is if the tool says so.
///
/// Produced by [`crate::match_expectations`]; declared here rather than
/// in [`crate::expectation`] because it is a *result* type,
/// [`PolicyResult`]'s frozen shape carries it, and Task 4.8 needed it
/// before expectations existed.
///
/// Derives no `Default`, and its `reason` is required: a stale
/// expectation with no account of what did not happen is not usable
/// evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct StaleExpectation {
    /// The `id` of the expectation that matched nothing.
    pub id: String,
    /// Why it is stale -- what did not happen, in the run's own terms.
    ///
    /// **Not** the expectation's own `reason` field (the human
    /// justification for why the change was expected). That one explains
    /// why the author wrote the entry; this one explains why it did not
    /// apply. `id` is how a reader gets back to the original.
    pub reason: String,
}

/// What a run's policy evaluation concluded overall.
///
/// Derives no `Default`: a `PolicyResult` only ever exists because
/// [`evaluate`] produced one from real changes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PolicyResult {
    /// The run's overall verdict.
    pub disposition: PolicyDisposition,
    /// Every change, graded, in the deterministic order this module's
    /// documentation describes. Includes expected changes.
    pub changes: Vec<ClassifiedChange>,
    /// Expectations that matched nothing. Does not affect
    /// `disposition` -- see [`PolicyDisposition`].
    pub stale_expectations: Vec<StaleExpectation>,
}

/// A run's overall verdict.
///
/// Decided by the *unexpected* changes alone:
///
/// - any unexpected [`Severity::Critical`] change => [`Fail`](Self::Fail);
/// - otherwise any unexpected [`Severity::Warning`] change =>
///   [`Warn`](Self::Warn);
/// - otherwise [`Pass`](Self::Pass) -- including a run whose only
///   changes are [`Severity::Info`], and a run whose only critical
///   changes were explicitly expected.
///
/// Stale expectations deliberately do not enter into it. A stale entry
/// is a statement about the *configuration*, not about the compared
/// stacks, and folding it in would make `Warn` mean two unrelated things
/// at once -- a user could not tell "the candidate changed behavior"
/// from "somebody's expectations file needs tidying". It is reported
/// alongside the disposition ([`PolicyResult::stale_expectations`]) so a
/// renderer or a CI job can still act on it.
///
/// Each wire tag is pinned with an explicit `#[serde(rename)]`: these
/// strings go into JSON reports and are what CI jobs will branch on.
/// [`Serialize`] only, for the same reason as [`ClassifiedChange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub enum PolicyDisposition {
    /// Nothing unexpected worth acting on.
    #[serde(rename = "pass")]
    Pass,
    /// Unexpected differences a human should look at, none critical.
    #[serde(rename = "warn")]
    Warn,
    /// At least one unexpected critical difference.
    #[serde(rename = "fail")]
    Fail,
}

impl PolicyDisposition {
    /// Returns this disposition's stable wire name -- exactly the string
    /// `serde` serializes it as (asserted in `tests/evaluate.rs`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// One `policy.overrides` entry, checked and compiled.
#[derive(Debug, Clone)]
struct ResolvedOverride {
    /// The semantic kind this override applies to.
    kind: SemanticChangeKind,
    /// Which changes of that kind it applies to.
    selector: CompiledSelector,
    /// The severity to use instead of the layered default.
    severity: Severity,
}

/// A `PolicySpec` whose every name is known-good and whose every glob is
/// compiled.
///
/// Producing one is the only way to reach [`evaluate`], which is the
/// point: a policy that names a semantic kind that does not exist can
/// never reach the stage where it would silently match nothing.
///
/// Derives no `Default`: use [`ResolvedPolicy::permissive`] and say so.
#[derive(Debug, Clone)]
pub struct ResolvedPolicy {
    /// Kinds escalated to critical by `policy.failOn`.
    fail_on: BTreeSet<&'static str>,
    /// `policy.overrides`, in declaration order -- which is load-bearing
    /// for tie-breaking (see this module's documentation).
    overrides: Vec<ResolvedOverride>,
}

impl ResolvedPolicy {
    /// The policy of a lab with no `policy` section at all: default
    /// severities everywhere, no escalations, no overrides.
    ///
    /// Equivalent to resolving `PolicySpec::default()`, but infallible,
    /// so tests and callers that genuinely have no policy do not have to
    /// handle an error that cannot happen.
    #[must_use]
    pub fn permissive() -> Self {
        Self {
            fail_on: BTreeSet::new(),
            overrides: Vec::new(),
        }
    }

    /// The severity for `change` under this policy, ignoring
    /// expectations (which affect whether a change *counts*, never how
    /// it is graded).
    ///
    /// See this module's documentation for the layering and for the
    /// most-specific-then-last-declared rule applied when several
    /// overrides match.
    #[must_use]
    pub fn severity_for(&self, change: &SemanticChange) -> Severity {
        let mut severity = default_change_severity(change);
        if self.fail_on.contains(change.kind.as_str()) {
            severity = Severity::Critical;
        }

        // `max_by_key` returns the *last* maximum when several compare
        // equal (its own documented behavior), which is precisely the
        // "ties go to the last declared" rule -- so declaration order
        // needs no separate index key here. Verified against the
        // standard library's documentation rather than assumed, and
        // pinned by `overrides_tie_breaks_on_declaration_order` in
        // `tests/evaluate.rs`.
        let winner = self
            .overrides
            .iter()
            .filter(|candidate| candidate.kind == change.kind && candidate.selector.matches(change))
            .max_by_key(|candidate| candidate.selector.specificity());
        if let Some(winner) = winner {
            severity = winner.severity;
        }
        severity
    }
}

/// Checks a `policy` section and compiles it for evaluation.
///
/// # Errors
///
/// Returns [`PolicySpecErrors`] listing **every** problem found, in
/// document order: an unknown semantic-kind name in `failOn` or in an
/// override, an unknown severity name, an unparsable fixture glob, or a
/// present-but-empty selector dimension. See
/// [`crate::CompiledSelector::compile`] for why the last of those is a
/// rejection rather than a no-op.
pub fn resolve_policy(spec: &PolicySpec) -> Result<ResolvedPolicy, PolicySpecErrors> {
    let mut errors = Vec::new();
    let mut fail_on = BTreeSet::new();

    // `PolicySpec::fail_on` is a `BTreeSet<String>`, so iteration order
    // is the names' own lexicographic order and the locator below is an
    // index into *that* order, not into the file's line order. Naming
    // the offending value in the message is what actually locates it for
    // the reader; the index only disambiguates two errors in one list.
    for (index, name) in spec.fail_on.iter().enumerate() {
        match kind_from_name(name) {
            Some(kind) => {
                fail_on.insert(kind.as_str());
            }
            None => errors.push(unknown_kind_error(&format!("policy.failOn[{index}]"), name)),
        }
    }

    let mut overrides = Vec::with_capacity(spec.overrides.len());
    for (index, entry) in spec.overrides.iter().enumerate() {
        let locator = format!("policy.overrides[{index}]");

        let kind = kind_from_name(&entry.kind);
        if kind.is_none() {
            errors.push(unknown_kind_error(&format!("{locator}.kind"), &entry.kind));
        }

        let severity = Severity::from_name(&entry.severity);
        if severity.is_none() {
            errors.push(PolicyValidationError::new(
                format_args!("{locator}.severity"),
                format_args!(
                    "unknown severity {:?}; expected one of: {}",
                    entry.severity.trim(),
                    severity_name_list()
                ),
            ));
        }

        let selector = CompiledSelector::compile(&ChangeSelector::from_override(entry), &locator);
        match (kind, severity, selector) {
            (Some(kind), Some(severity), Ok(selector)) => overrides.push(ResolvedOverride {
                kind,
                selector,
                severity,
            }),
            (_, _, selector) => {
                if let Err(selector_errors) = selector {
                    errors.extend(selector_errors);
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(ResolvedPolicy { fail_on, overrides })
    } else {
        Err(PolicySpecErrors(errors))
    }
}

/// Reports every problem in a `policy` section without compiling it.
///
/// This is the **load-time validation seam** Task 4.8 step 3 asks for,
/// and it lives here rather than in `admissionlab_spec::validate`
/// deliberately. Validating `failOn`/`overrides[].kind` means knowing
/// the seventeen [`SemanticChangeKind`] names, which
/// `admissionlab-diff` owns; §1.1 keeps `admissionlab-spec` beneath
/// everything, so a `spec -> diff` (or `spec -> policy`) edge is not
/// available, and duplicating the seventeen names inside `spec` would
/// create a second source of truth that could drift from the first
/// without anything noticing. The orchestrator (`admissionlab-core`,
/// which §1.1 already has depending on both `spec` and `policy`) is the
/// one component that can see both, so it calls
/// `admissionlab_spec::load_lab`/`resolve_lab` and then this function
/// at startup, and refuses to create a cluster if either objects.
///
/// Nothing in this crate touches a cluster, a kubeconfig, or a
/// subprocess, so "before any cluster is created" is a property of the
/// call site, not a promise this function could break: it is a pure
/// function of parsed configuration. `tests/evaluate.rs` exercises it
/// against a parsed YAML `policy` section for exactly that reason.
///
/// Returns an empty vector when the policy is valid.
#[must_use]
pub fn validate_policy_spec(spec: &PolicySpec) -> Vec<PolicyValidationError> {
    resolve_policy(spec)
        .err()
        .map_or_else(Vec::new, PolicySpecErrors::into_vec)
}

/// Grades `changes` against `policy`.
///
/// Returns every change, graded and deterministically ordered, plus the
/// run's overall disposition. Nothing is dropped or edited: a change
/// downgraded to [`Severity::Info`] by an override is still reported, it
/// just stops driving the verdict.
///
/// Equivalent to [`evaluate_with_expectations`] with no expectations, so
/// every [`ClassifiedChange::expected`] is `false` and
/// [`PolicyResult::stale_expectations`] is empty.
#[must_use]
pub fn evaluate(policy: &ResolvedPolicy, changes: &[SemanticChange]) -> PolicyResult {
    evaluate_with_expectations(policy, &ResolvedExpectations::none(), changes)
}

/// Grades `changes` against `policy`, marking those an explicit
/// expectation accounts for.
///
/// An expected change keeps its severity and its place in
/// [`PolicyResult::changes`]; it simply stops driving
/// [`PolicyResult::disposition`] (Task 4.9 step 4). A run whose only
/// critical change was expected therefore passes *with that critical
/// change still listed* -- which is the point: the tool never hides a
/// difference, it only stops treating an accounted-for one as a reason
/// to fail.
///
/// Expectations are matched against the changes **after** grading and
/// sorting, so [`crate::ExpectationMatch::change_index`] indexes the
/// same list a report renders. See [`crate::expectation`] for the exact
/// matching rule and the contested-change tiebreaker.
#[must_use]
pub fn evaluate_with_expectations(
    policy: &ResolvedPolicy,
    expectations: &ResolvedExpectations,
    changes: &[SemanticChange],
) -> PolicyResult {
    let mut classified = classify_changes(policy, changes);
    let matching = match_expectations(expectations, &classified);
    for matched in &matching.matches {
        classified[matched.change_index].expected = true;
    }
    let disposition = disposition_of(&classified);
    PolicyResult {
        disposition,
        changes: classified,
        stale_expectations: matching.stale,
    }
}

/// Grades and orders `changes`, with every `expected` flag left `false`.
///
/// Split out because [`evaluate_with_expectations`] needs the graded,
/// *ordered* list before it can decide which entries an expectation
/// accounts for: matching is defined against the same indices a report
/// shows, so it has to run after this sort, not before.
pub(crate) fn classify_changes(
    policy: &ResolvedPolicy,
    changes: &[SemanticChange],
) -> Vec<ClassifiedChange> {
    let mut classified: Vec<ClassifiedChange> = changes
        .iter()
        .map(|change| ClassifiedChange {
            severity: policy.severity_for(change),
            change: change.clone(),
            expected: false,
        })
        .collect();

    // Stable sort: entries with an identical key keep the order the
    // caller supplied them in, which is the final tiebreaker this
    // module's documentation names.
    classified.sort_by(|left, right| sort_key(&left.change).cmp(&sort_key(&right.change)));
    classified
}

/// The documented ordering key for one change.
///
/// Borrowed rather than owned so sorting allocates nothing per
/// comparison.
fn sort_key(change: &SemanticChange) -> (&str, &str, Option<&str>, Option<&str>) {
    (
        change.fixture_id.as_str(),
        change.kind.as_str(),
        change.object_path.as_deref(),
        change.subject.as_deref(),
    )
}

/// Applies the disposition rule to already-graded changes.
///
/// Expected changes are skipped entirely -- see [`PolicyDisposition`].
pub(crate) fn disposition_of(changes: &[ClassifiedChange]) -> PolicyDisposition {
    let worst = changes
        .iter()
        .filter(|classified| !classified.expected)
        .map(|classified| classified.severity)
        .max();
    match worst {
        Some(Severity::Critical) => PolicyDisposition::Fail,
        Some(Severity::Warning) => PolicyDisposition::Warn,
        Some(Severity::Info) | None => PolicyDisposition::Pass,
    }
}

/// Builds the "that name is not a semantic change kind" rejection,
/// listing every name that would have been accepted.
///
/// Listing all seventeen is verbose but is the difference between a user
/// fixing a typo in one edit and going to read the source: the names are
/// a public contract precisely so people write them by hand.
fn unknown_kind_error(locator: &str, name: &str) -> PolicyValidationError {
    PolicyValidationError::new(
        locator,
        format_args!(
            "unknown semantic change kind {:?}; expected one of: {}",
            name.trim(),
            kind_name_list()
        ),
    )
}
