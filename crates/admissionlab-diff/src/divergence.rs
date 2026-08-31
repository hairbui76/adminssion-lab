//! Where two admission chains *first* stopped agreeing.
//!
//! [`first_divergence`] answers the question a user asks after a
//! regression is reported: not "what is different" (that is
//! [`crate::workload`] and [`crate::trace`]) but "which webhook did it,
//! and when". It returns a [`DivergenceEvidence`] naming the earliest
//! position in the chain where the two sides' observations disagree,
//! with an explanation a human can read and a
//! [`DivergenceConfidence`] saying how well supported the claim is.
//!
//! Its entire design is about *not overclaiming*. Attribution is the
//! most tempting place in this product to turn a correlation into a
//! cause, and Global Constraint 15 forbids exactly that; Global
//! Constraint 7 adds that whatever is claimed must be deterministic
//! rather than guessed.
//!
//! # Position order, not identity order
//!
//! Unlike [`crate::trace`], which matches invocations by
//! `(configuration, webhook, round)` because it is answering "did this
//! webhook's behavior change", this module walks both chains by
//! `(round, index)` position. "First" is inherently positional: the
//! question is where in the sequence the two runs stopped agreeing, and
//! an invocation that exists on one side only *is* a divergence at the
//! position it occupies rather than a fact about a webhook.
//!
//! At each position, in this order:
//!
//! 1. **Identity.** Different `configuration`/`webhook` at the same
//!    position: the chains took different shapes here.
//! 2. **Mutated flag**, where both sides observed one.
//! 3. **Patch**, where both sides observed one.
//!
//! A dimension only one side observed is skipped, for the reason
//! [`crate::trace`] documents at length: an evidence gap is not a
//! behavior difference. `outcome` is deliberately not consulted -- a
//! webhook flipping from allow to deny is a decision-level fact
//! [`crate::admission`] and [`crate::trace`] already report, and Task
//! 4.7 Step 1 names identity, mutated flag, and patch.
//!
//! # Confidence is decided by the evidence, not by the finding
//!
//! - Both chains [`TraceEvidence::Observed`]: any divergence found is
//!   [`DivergenceConfidence::Observed`].
//! - Either chain [`TraceEvidence::Partial`]: at most
//!   [`DivergenceConfidence::Inferred`], because index alignment itself
//!   becomes uncertain -- an invocation the evidence simply failed to
//!   record shifts every position after it, so "different webhooks at
//!   position (0, 2)" may be an artifact of the gap rather than a real
//!   difference in the chain.
//!
//!   The one exception is Task 4.7 Step 4's own: a **patch difference on
//!   the same webhook**, where both sides recorded a patch, is
//!   [`DivergenceConfidence::Observed`] even under partial evidence.
//!   That claim does not rest on alignment at all -- the same named
//!   webhook responded with two different patches, and both were seen.
//!
//!   The exception is deliberately not extended to a same-webhook
//!   `mutated` disagreement, which stays `Inferred`. That flag is a
//!   summary the audit reconstruction derived, while a patch is the
//!   observed response content itself, and Step 4 names the patch.
//! - Either chain [`TraceEvidence::Unavailable`]:
//!   [`DivergenceConfidence::Unknown`], with no position at all. That
//!   variant's contract is that its `invocations` mean nothing
//!   regardless of length, so walking it would attribute a divergence at
//!   every position of the other side -- a fabricated finding built out
//!   of a missing audit backend.
//!
//! Note the contrast with [`crate::trace`], which *suppresses*
//! absence-based claims under partial evidence rather than weakening
//! them. The difference is that a [`SemanticChange`](crate::SemanticChange)
//! has no way to say "probably": it is a claim or it is nothing.
//! [`DivergenceEvidence`] carries a confidence, so the honest move here
//! is to state the finding and grade it.
//!
//! # Identical chains, different objects
//!
//! Two byte-identical chains with two different final objects is a real
//! and important case: something mutated the object outside the captured
//! mutating-webhook evidence, or that evidence is incomplete. It is also
//! not something this function can detect, because its frozen signature
//! sees two traces and no objects at all.
//!
//! Rather than widen the signature or have this module reach for objects
//! it was not given, the seam is a second, explicit function:
//! [`first_divergence_with_objects`], which takes the one extra fact
//! ("do the two compared objects differ?") that the caller already knows
//! because it just ran [`crate::workload::diff_workload_objects`] or
//! [`crate::raw::raw_object_diff`]. A `bool` rather than the two objects
//! keeps the composition explicit and leaves the decision of *what
//! counts as differing* with the caller that has the normalization
//! evidence in hand.

use admissionlab_admission::trace::TraceEvidence;
use admissionlab_normalize::{NormalizedTrace, NormalizedWebhookInvocation};

use crate::trace::{TraceComparability, trace_comparability};
use crate::types::{DivergenceConfidence, DivergenceEvidence};

/// Locates the first position at which two observed webhook chains
/// disagree.
///
/// Returns [`None`] when the two chains agree at every position that was
/// observed. That is **not** a claim that nothing diverged: two identical
/// chains can still leave different objects behind (see
/// [`first_divergence_with_objects`]), and under partial evidence the
/// positions that were *not* observed were not compared. Use
/// [`crate::trace::trace_comparability`] to tell a qualified silence from
/// an unqualified one.
///
/// Returns [`DivergenceConfidence::Unknown`] evidence, with no position,
/// when either side's chain is [`TraceEvidence::Unavailable`]: nothing
/// could be compared, and saying so is more useful -- and more honest --
/// than a [`None`] a caller would read as agreement.
///
/// See this module's documentation for the comparison order, and for how
/// confidence is decided.
#[must_use]
pub fn first_divergence(
    baseline: &NormalizedTrace,
    candidate: &NormalizedTrace,
) -> Option<DivergenceEvidence> {
    let comparability = trace_comparability(baseline, candidate);
    if let TraceComparability::Incomparable {
        baseline: baseline_evidence,
        candidate: candidate_evidence,
    } = comparability
    {
        return Some(unavailable_evidence(baseline_evidence, candidate_evidence));
    }
    let partial = !comparability.is_comparable();

    let baseline_positions = index_positions(baseline);
    let candidate_positions = index_positions(candidate);
    let positions: std::collections::BTreeSet<(u32, u32)> = baseline_positions
        .keys()
        .chain(candidate_positions.keys())
        .copied()
        .collect();

    positions.into_iter().find_map(|position| {
        let divergence = match (
            baseline_positions.get(&position),
            candidate_positions.get(&position),
        ) {
            (Some(in_baseline), Some(in_candidate)) => compare_position(in_baseline, in_candidate),
            (Some(only_baseline), None) => Some(Divergence::BaselineOnly(only_baseline)),
            (None, Some(only_candidate)) => Some(Divergence::CandidateOnly(only_candidate)),
            // Unreachable: every position came from one of the two maps.
            (None, None) => None,
        }?;
        Some(divergence.into_evidence(partial))
    })
}

/// Locates the first divergence, falling back to an explicit
/// [`DivergenceConfidence::Unknown`] when the chains agree but the
/// objects do not.
///
/// `objects_differ` is the caller's answer to "are the two compared
/// objects different at all?" -- normally
/// `!baseline.value.eq(&candidate.value)` over the two
/// `admissionlab_normalize::NormalizedObject`s, or equivalently a
/// non-empty [`crate::raw::raw_object_diff`]. Passing `false` when they
/// do differ loses this attribution; it cannot produce a wrong one.
///
/// See this module's documentation for why this is a separate function
/// rather than a wider signature.
#[must_use]
pub fn first_divergence_with_objects(
    baseline: &NormalizedTrace,
    candidate: &NormalizedTrace,
    objects_differ: bool,
) -> Option<DivergenceEvidence> {
    if let Some(evidence) = first_divergence(baseline, candidate) {
        return Some(evidence);
    }
    if !objects_differ {
        return None;
    }
    Some(DivergenceEvidence {
        confidence: DivergenceConfidence::Unknown,
        baseline_position: None,
        candidate_position: None,
        baseline_webhook: None,
        candidate_webhook: None,
        explanation: "The two observed webhook chains are identical, but the two compared \
                      objects are not. The difference occurred outside captured \
                      mutating-webhook evidence, or that evidence is incomplete."
            .to_owned(),
    })
}

/// What was found at the first differing position.
enum Divergence<'trace> {
    /// Two different webhooks occupy the same position.
    Identity {
        baseline: &'trace NormalizedWebhookInvocation,
        candidate: &'trace NormalizedWebhookInvocation,
    },
    /// The same webhook, observed to mutate on one side and not the
    /// other.
    Mutated {
        baseline: &'trace NormalizedWebhookInvocation,
        candidate: &'trace NormalizedWebhookInvocation,
    },
    /// The same webhook, with two different observed patches.
    Patch {
        baseline: &'trace NormalizedWebhookInvocation,
        candidate: &'trace NormalizedWebhookInvocation,
    },
    /// Only the baseline has an invocation at this position.
    BaselineOnly(&'trace NormalizedWebhookInvocation),
    /// Only the candidate has an invocation at this position.
    CandidateOnly(&'trace NormalizedWebhookInvocation),
}

/// Compares two invocations that occupy the same position, in the
/// documented order.
fn compare_position<'trace>(
    baseline: &'trace NormalizedWebhookInvocation,
    candidate: &'trace NormalizedWebhookInvocation,
) -> Option<Divergence<'trace>> {
    if baseline.configuration != candidate.configuration || baseline.webhook != candidate.webhook {
        return Some(Divergence::Identity {
            baseline,
            candidate,
        });
    }
    if both_observed_and_differ(baseline.mutated, candidate.mutated) {
        return Some(Divergence::Mutated {
            baseline,
            candidate,
        });
    }
    if both_observed_and_differ(baseline.patch.as_ref(), candidate.patch.as_ref()) {
        return Some(Divergence::Patch {
            baseline,
            candidate,
        });
    }
    None
}

/// Whether two optional observations both exist and disagree.
///
/// The same rule [`crate::trace`] applies, for the same reason: a
/// dimension only one side observed is an evidence gap, not a
/// difference.
fn both_observed_and_differ<T: PartialEq>(baseline: Option<T>, candidate: Option<T>) -> bool {
    match (baseline, candidate) {
        (Some(left), Some(right)) => left != right,
        _ => false,
    }
}

impl Divergence<'_> {
    /// Renders this finding as evidence, grading it against how complete
    /// the underlying observation was.
    fn into_evidence(self, partial: bool) -> DivergenceEvidence {
        let confidence = if !partial || self.survives_partial_evidence() {
            DivergenceConfidence::Observed
        } else {
            DivergenceConfidence::Inferred
        };
        let mut explanation = self.explain();
        if confidence == DivergenceConfidence::Inferred {
            explanation.push_str(
                " At least one side's webhook evidence is partial, so an unrecorded \
                 invocation could have shifted the positions being compared; this \
                 attribution is inferred rather than directly observed.",
            );
        }
        let (baseline, candidate) = self.sides();
        DivergenceEvidence {
            confidence,
            baseline_position: baseline.map(position_of),
            candidate_position: candidate.map(position_of),
            baseline_webhook: baseline.map(|invocation| invocation.webhook.clone()),
            candidate_webhook: candidate.map(|invocation| invocation.webhook.clone()),
            explanation,
        }
    }

    /// Whether this finding stays [`DivergenceConfidence::Observed`]
    /// even when a chain was only partially observed.
    ///
    /// Only a patch difference does. See this module's documentation for
    /// why alignment-dependent findings do not, and why the `mutated`
    /// flag is deliberately not included.
    fn survives_partial_evidence(&self) -> bool {
        matches!(self, Self::Patch { .. })
    }

    /// The two sides' invocations, either of which is [`None`] when that
    /// side has no invocation at the diverging position.
    fn sides(
        &self,
    ) -> (
        Option<&NormalizedWebhookInvocation>,
        Option<&NormalizedWebhookInvocation>,
    ) {
        match self {
            Self::Identity {
                baseline,
                candidate,
            }
            | Self::Mutated {
                baseline,
                candidate,
            }
            | Self::Patch {
                baseline,
                candidate,
            } => (Some(baseline), Some(candidate)),
            Self::BaselineOnly(invocation) => (Some(invocation), None),
            Self::CandidateOnly(invocation) => (None, Some(invocation)),
        }
    }

    /// A complete-sentence account of what diverged, naming the round,
    /// the position, and the webhooks involved -- and claiming nothing
    /// the comparison did not establish.
    fn explain(&self) -> String {
        match self {
            Self::Identity {
                baseline,
                candidate,
            } => format!(
                "In round {} at index {}, the baseline invoked webhook `{}` (configuration \
                 `{}`) while the candidate invoked webhook `{}` (configuration `{}`). This is \
                 the first position at which the two observed chains differ.",
                baseline.round,
                baseline.index,
                baseline.webhook,
                baseline.configuration,
                candidate.webhook,
                candidate.configuration,
            ),
            Self::Mutated {
                baseline,
                candidate: _,
            } => format!(
                "Webhook `{}` (configuration `{}`) ran in round {} at index {} on both sides, \
                 and was observed to mutate the object on the {} side but not on the {} side. \
                 This is the first position at which the two observed chains differ.",
                baseline.webhook,
                baseline.configuration,
                baseline.round,
                baseline.index,
                if baseline.mutated == Some(true) {
                    "baseline"
                } else {
                    "candidate"
                },
                if baseline.mutated == Some(true) {
                    "candidate"
                } else {
                    "baseline"
                },
            ),
            Self::Patch {
                baseline,
                candidate: _,
            } => format!(
                "Webhook `{}` (configuration `{}`) ran in round {} at index {} on both sides \
                 and responded with a different patch: both patches were observed, and they \
                 are not equal. This is the first position at which the two observed chains \
                 differ.",
                baseline.webhook, baseline.configuration, baseline.round, baseline.index,
            ),
            Self::BaselineOnly(invocation) => format!(
                "The baseline invoked webhook `{}` (configuration `{}`) in round {} at index \
                 {}, and the candidate's observed chain has no invocation at that position. \
                 This is the first position at which the two observed chains differ.",
                invocation.webhook, invocation.configuration, invocation.round, invocation.index,
            ),
            Self::CandidateOnly(invocation) => format!(
                "The candidate invoked webhook `{}` (configuration `{}`) in round {} at index \
                 {}, and the baseline's observed chain has no invocation at that position. \
                 This is the first position at which the two observed chains differ.",
                invocation.webhook, invocation.configuration, invocation.round, invocation.index,
            ),
        }
    }
}

/// The evidence returned when a side's chain could not be read at all.
fn unavailable_evidence(baseline: TraceEvidence, candidate: TraceEvidence) -> DivergenceEvidence {
    let unreadable = match (baseline, candidate) {
        (TraceEvidence::Unavailable, TraceEvidence::Unavailable) => "both sides'",
        (TraceEvidence::Unavailable, _) => "the baseline side's",
        _ => "the candidate side's",
    };
    DivergenceEvidence {
        confidence: DivergenceConfidence::Unknown,
        baseline_position: None,
        candidate_position: None,
        baseline_webhook: None,
        candidate_webhook: None,
        explanation: format!(
            "No divergence could be located: {unreadable} webhook evidence is unavailable, so \
             the two chains were not compared position by position. Any difference between \
             the two runs occurred outside captured mutating-webhook evidence, or that \
             evidence is incomplete."
        ),
    }
}

/// Indexes a trace's invocations by `(round, index)`, keeping the first
/// occurrence of a repeated position.
///
/// A repeated position would mean two webhooks were recorded as the same
/// step of the same round, which Task 3.3's reconstruction does not
/// produce; the first wins rather than the two being merged into
/// something neither side observed.
fn index_positions(
    trace: &NormalizedTrace,
) -> std::collections::BTreeMap<(u32, u32), &NormalizedWebhookInvocation> {
    let mut indexed = std::collections::BTreeMap::new();
    for invocation in &trace.invocations {
        indexed
            .entry((invocation.round, invocation.index))
            .or_insert(invocation);
    }
    indexed
}

/// One invocation's `(round, index)` position.
fn position_of(invocation: &NormalizedWebhookInvocation) -> (u32, u32) {
    (invocation.round, invocation.index)
}
