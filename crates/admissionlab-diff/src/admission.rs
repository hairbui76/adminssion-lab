//! Comparing what the two API servers *decided* about one fixture.
//!
//! This module answers exactly one question -- did the admit/deny verdict
//! flip? -- and deliberately refuses to answer any other. What the
//! admitted object looked like is Task 4.5's job; what the webhook chain
//! did is Task 4.6's; where a difference first appeared is Task 4.7's.
//! Keeping this narrow is what makes
//! [`ObjectNewlyDenied`](SemanticChangeKind::ObjectNewlyDenied) mean the
//! one thing every user reads it as: *this used to be allowed and now it
//! is not*.
//!
//! # Three outcomes, not two
//!
//! A pair of [`AdmissionOutcome`]s can be in one of three states, and
//! collapsing the third into either of the first two is exactly the
//! fabrication Global Constraint 15 forbids:
//!
//! 1. **The verdict flipped.** One semantic change, either
//!    `ObjectNewlyDenied` or `ObjectNewlyAllowed`.
//! 2. **The verdict held.** No semantic change -- including when both
//!    sides rejected with *different* messages or codes (see below).
//! 3. **The two sides were never comparable.** At least one side is
//!    [`AdmissionDecision::UnsupportedDryRun`], which is a statement
//!    about this lab's replay capability, not about the stack under test.
//!    No semantic change, because there is no observation to compare;
//!    [`decision_comparability`] is how a caller tells this apart from
//!    state 2.
//!
//! States 2 and 3 both yield an empty `Vec<SemanticChange>`, and that is
//! precisely why [`decision_comparability`] exists as a separate,
//! public function rather than as a comment. "No change" is a positive
//! claim -- it is what makes a run pass -- and it is only honest when
//! both sides were actually observed. Phase 4's summary counts state 3
//! as `inconclusive` (`admissionlab_report`'s `RunSummary` already has
//! that field) rather than as `identical`, and this function is the seam
//! it reads to do so. Nothing richer is built here: no `SemanticChange`
//! kind for incomparability (the seventeen Alpha kinds are frozen and
//! none of them means this), and no severity opinion (that is
//! `admissionlab-policy`'s).
//!
//! # Rejected on both sides, with a different message
//!
//! A candidate that denies the same object with a reworded message or a
//! different status code has *not* regressed: the object is still
//! denied, which is the behavior a user's policy actually cares about.
//! Emitting `ObjectNewlyDenied` there would be a false positive, and
//! emitting some other kind would overstate a cosmetic difference. So
//! this module emits nothing, and the difference stays visible through
//! the diagnostic channel instead: [`raw_decision_diff`] returns the
//! RFC 6902 operations between the two serialized decisions (for the
//! canonical case, a single `replace` at `/rejected/message`).
//! Independently of that helper, Task 4.10's report model carries both
//! sides' full `AdmissionOutcome` under `AdmissionComparison`, so the
//! two messages reach a reader verbatim regardless. What must never
//! happen is the reverse -- a raw difference being promoted into a
//! semantic claim just because it is non-empty.

use admissionlab_admission::{AdmissionDecision, AdmissionOutcome};
use serde::Serialize;
use serde_json::Value;

use crate::raw::{RawChange, raw_object_diff};
use crate::types::{SemanticChange, SemanticChangeKind};

/// Whether two [`AdmissionOutcome`]s can be compared as decisions at all.
///
/// `Incomparable` is a statement about *this lab's* ability to replay the
/// fixture on a side, never about the stack being tested. It exists so a
/// caller can distinguish an empty semantic-change list that means "both
/// sides were observed and agreed" from one that means "there was
/// nothing to compare" -- see this module's documentation for why that
/// distinction is load-bearing and how Phase 4's summary consumes it.
///
/// Serializes with pinned wire tags, since it reaches the same reports
/// [`SemanticChange`] does. Derives no `Default`: `Comparable` is the
/// convenient answer, and defaulting to it would silently upgrade "we
/// could not look" into "we looked and it was fine".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum DecisionComparability {
    /// Both sides produced a real admit/deny verdict from a real API
    /// server, so their decisions mean the same thing and can be
    /// compared.
    #[serde(rename = "comparable")]
    Comparable,
    /// At least one side could not be replayed as a real admission
    /// decision. Each field holds that side's own explanation verbatim,
    /// or [`None`] when that side *was* replayed normally.
    #[serde(rename = "incomparable")]
    Incomparable {
        /// Why the baseline side produced no comparable decision.
        baseline: Option<String>,
        /// Why the candidate side produced no comparable decision.
        candidate: Option<String>,
    },
}

impl DecisionComparability {
    /// Returns `true` only for [`DecisionComparability::Comparable`].
    #[must_use]
    pub fn is_comparable(&self) -> bool {
        matches!(self, Self::Comparable)
    }
}

/// Reports whether `baseline` and `candidate` can be compared as
/// decisions.
///
/// Returns [`DecisionComparability::Incomparable`] when either side's
/// decision is [`AdmissionDecision::UnsupportedDryRun`], carrying that
/// side's own message rather than a message invented here. Note that
/// `UnsupportedDryRun` is a variant `admissionlab-admission`'s current
/// classification never produces (its `execute` module documents the
/// live investigation behind that, on the Kubernetes versions this
/// project targets); it is handled here regardless, because the variant
/// is part of the frozen input type and a later task may start producing
/// it the moment a genuine trigger is confirmed.
#[must_use]
pub fn decision_comparability(
    baseline: &AdmissionOutcome,
    candidate: &AdmissionOutcome,
) -> DecisionComparability {
    let baseline_reason = unsupported_reason(&baseline.decision);
    let candidate_reason = unsupported_reason(&candidate.decision);

    if baseline_reason.is_none() && candidate_reason.is_none() {
        DecisionComparability::Comparable
    } else {
        DecisionComparability::Incomparable {
            baseline: baseline_reason,
            candidate: candidate_reason,
        }
    }
}

/// Classifies the difference between two sides' admission decisions.
///
/// Returns at most one [`SemanticChange`]: `ObjectNewlyDenied` when the
/// baseline admitted the object and the candidate rejected it,
/// `ObjectNewlyAllowed` for the reverse, and an empty vector in every
/// other case -- both sides agreeing (whatever their rejection messages
/// say), or the two sides not being comparable at all. Read this
/// module's documentation before treating an empty result as "nothing
/// changed"; [`decision_comparability`] is what separates the two
/// reasons a result can be empty.
///
/// The change's `baseline` and `candidate` payloads are the two
/// [`AdmissionDecision`] values serialized verbatim -- `"accepted"` for
/// an admission, `{"rejected":{"code":403,"message":"..."}}` for a
/// rejection. Nothing is synthesized: a rejection observed without a
/// status code stays `"code":null` rather than acquiring a
/// plausible-looking `403`, exactly as `AdmissionDecision` itself
/// records it. `object_path` and `subject` are [`None`] because a
/// verdict flip is a property of the whole request, not of any one field
/// or named subject. `origin` is [`None`]: first-divergence attribution
/// needs normalized traces and is Task 4.7's function, and a `None`
/// there means "not attributed", never "no divergence".
///
/// `fixture_id` is taken from `baseline`. Both outcomes always describe
/// the same fixture -- the caller pairs them, and
/// `admissionlab_report`'s `AdmissionComparison` groups both sides under
/// one `fixture_id` -- so the two are interchangeable here.
///
/// # Panics
///
/// Never in practice. Panics only if serializing an
/// [`AdmissionDecision`] fails, which cannot happen for that concrete
/// type: it is a plain enum of `String`, `Option<u16>`, and a unit
/// variant, with no map keyed by a non-string and no custom
/// `Serialize` impl that can error.
#[must_use]
pub fn diff_admission_decision(
    baseline: &AdmissionOutcome,
    candidate: &AdmissionOutcome,
) -> Vec<SemanticChange> {
    if !decision_comparability(baseline, candidate).is_comparable() {
        return Vec::new();
    }

    let kind = match (&baseline.decision, &candidate.decision) {
        (AdmissionDecision::Accepted, AdmissionDecision::Rejected { .. }) => {
            SemanticChangeKind::ObjectNewlyDenied
        }
        (AdmissionDecision::Rejected { .. }, AdmissionDecision::Accepted) => {
            SemanticChangeKind::ObjectNewlyAllowed
        }
        // Both admitted, or both rejected (however differently worded):
        // the verdict held, so there is no semantic change to claim.
        (AdmissionDecision::Accepted, AdmissionDecision::Accepted)
        | (AdmissionDecision::Rejected { .. }, AdmissionDecision::Rejected { .. }) => {
            return Vec::new();
        }
        // Already handled by the comparability guard above; restated
        // rather than reached for with `unreachable!`, so this function
        // cannot panic if that guard is ever changed.
        (AdmissionDecision::UnsupportedDryRun { .. }, _)
        | (_, AdmissionDecision::UnsupportedDryRun { .. }) => return Vec::new(),
    };

    vec![SemanticChange {
        kind,
        fixture_id: baseline.fixture_id.clone(),
        object_path: None,
        subject: None,
        baseline: Some(decision_value(&baseline.decision)),
        candidate: Some(decision_value(&candidate.decision)),
        origin: None,
    }]
}

/// Returns the raw, diagnostics-only difference between the two sides'
/// serialized admission decisions.
///
/// This is the channel through which a difference that is deliberately
/// *not* a semantic change stays visible -- most importantly two
/// rejections whose message or status code differ, which yields a single
/// `replace` operation at `/rejected/message` (or `/rejected/code`) and
/// no semantic change at all. Its output is evidence for a reader, never
/// grounds for classification; see [`crate::raw`]'s module documentation.
///
/// Returns an empty vector when both sides recorded the identical
/// decision.
///
/// # Panics
///
/// Never in practice, for the same reason
/// [`diff_admission_decision`] does not.
#[must_use]
pub fn raw_decision_diff(
    baseline: &AdmissionOutcome,
    candidate: &AdmissionOutcome,
) -> Vec<RawChange> {
    raw_object_diff(
        &decision_value(&baseline.decision),
        &decision_value(&candidate.decision),
    )
}

/// Returns an [`AdmissionDecision::UnsupportedDryRun`]'s message, or
/// [`None`] for a decision that was a real verdict.
fn unsupported_reason(decision: &AdmissionDecision) -> Option<String> {
    match decision {
        AdmissionDecision::UnsupportedDryRun { message } => Some(message.clone()),
        AdmissionDecision::Accepted | AdmissionDecision::Rejected { .. } => None,
    }
}

/// Serializes one [`AdmissionDecision`] into the JSON a
/// [`SemanticChange`] payload (or a [`RawChange`] diff) carries.
///
/// Deliberately reuses `AdmissionDecision`'s own pinned wire shape rather
/// than rebuilding a payload by hand here: one source of truth for what
/// a decision looks like in a report, and no chance of this crate and
/// `admissionlab-admission` drifting into two different renderings of
/// the same observation.
///
/// # Panics
///
/// Only if serializing `AdmissionDecision` fails, which that type's
/// shape makes impossible -- see [`diff_admission_decision`]'s own
/// `# Panics` section.
fn decision_value(decision: &AdmissionDecision) -> Value {
    serde_json::to_value(decision).expect(
        "AdmissionDecision is a plain enum of String/Option<u16>; serialization cannot fail",
    )
}
