//! Comparing what the two *webhook chains* did.
//!
//! [`diff_admission_trace`] takes the two sides' normalized traces and
//! classifies the three trace-shaped kinds in the frozen vocabulary: a
//! webhook whose invocation changed
//! ([`WebhookInvocationChanged`](SemanticChangeKind::WebhookInvocationChanged)),
//! a webhook that is now failing
//! ([`WebhookFailed`](SemanticChangeKind::WebhookFailed)), and a webhook
//! that got materially slower
//! ([`WebhookLatencyChanged`](SemanticChangeKind::WebhookLatencyChanged)).
//! What the API server finally decided is [`crate::admission`]'s
//! question, what the admitted object ended up looking like is
//! [`crate::workload`]'s, and *where in the chain* a difference first
//! appeared is Task 4.7's.
//!
//! # Invocations are matched by webhook and round, not by position
//!
//! The match key is `(configuration, webhook, round)` -- "this webhook's
//! participation in this admission round" -- and never `(round, index)`.
//!
//! Position keying looks natural and is a trap: inserting one webhook at
//! the front of a round shifts every later index, so a single added
//! webhook would report a difference at *every* subsequent position, and
//! each of those reports would compare two entirely different webhooks
//! against each other. Keying by identity means an added webhook is one
//! claim about that webhook, and every other webhook is compared against
//! itself.
//!
//! Ordering is not thereby ignored. `index` is one of the compared
//! fields, so a webhook that now runs earlier or later within its round
//! *is* reported -- ordering is behavior here, since a mutating webhook
//! sees whatever the webhooks before it left behind. The consequence is
//! worth stating plainly: inserting a webhook at the front of a round
//! produces one change for the new webhook and one for each webhook it
//! displaced. Those displacements are real (each of those webhooks now
//! runs after a new mutator), so they are reported rather than
//! suppressed.
//!
//! A repeated key would mean the same webhook was recorded twice in one
//! round, which Kubernetes does not do and Task 3.3's reconstruction does
//! not produce; the first occurrence wins and the rest are ignored rather
//! than being silently merged.
//!
//! # A dimension is compared only where both sides observed it
//!
//! `mutated`, `patch`, and `outcome` can all say "the evidence could not
//! tell" -- as [`None`], [`None`], and
//! [`WebhookOutcome::Unknown`] respectively, each of which
//! `admissionlab-admission` documents at length as a value that must
//! never be collapsed into a plausible one. So each of those dimensions
//! participates in the comparison **only when both sides observed it**.
//! A baseline that recorded a patch against a candidate that recorded
//! none is an *evidence* difference, not a behavior difference, and
//! reporting it as `webhook_invocation_changed` would tell a user their
//! stack changed when only this lab's visibility into it did (Global
//! Constraint 15).
//!
//! `configuration`, `webhook`, `round`, and `index` are not optional and
//! are always compared.
//!
//! # At most one invocation-level claim per webhook and round
//!
//! A webhook whose patch *and* mutated flag *and* index all differ is one
//! change, not three: the claim is "this invocation changed", and both
//! sides' full summaries are in the payload for a reader to compare
//! field by field. That keeps a report's change count meaningful -- it
//! counts webhooks whose behavior moved, not fields.
//!
//! The one substitution: when the difference is that the *candidate*
//! errored, the claim is `webhook_failed` instead, because that is the
//! more specific and more actionable statement about the same fact. The
//! payloads are unchanged, so nothing is lost.
//!
//! # What `webhook_failed` requires, and what it deliberately excludes
//!
//! It is emitted only when the candidate side observed
//! [`WebhookOutcome::Errored`] for a webhook whose baseline side was
//! observed to *not* error (`Allowed` or `Denied`). Three exclusions
//! follow, each on purpose:
//!
//! - **Baseline `Unknown`.** A candidate error against a baseline whose
//!   outcome was never established cannot support "newly failing"; the
//!   error itself is still in the trace a report renders.
//! - **Both sides errored.** Nothing changed.
//! - **The improvement direction.** A baseline that errored against a
//!   candidate that now answers cleanly is not a `webhook_failed` --
//!   nothing failed in the candidate. It is reported as
//!   `webhook_invocation_changed`, because that webhook's observed
//!   behavior genuinely did change, and whether an improvement matters is
//!   `admissionlab-policy`'s decision, not this crate's.
//!
//! Rejection-*metric* evidence is a second, independent source for this
//! kind (`apiserver_admission_webhook_rejection_count`, which
//! `admissionlab_admission::metrics` already parses). It cannot be
//! consulted here: this function's frozen signature sees two traces and
//! nothing else. Metric-sourced webhook failures therefore belong to the
//! integration altitude that holds both the traces and the metric deltas
//! for a fixture -- Task 4.10's result assembly -- and this module claims
//! only what a trace can support.
//!
//! # Latency, and the zero that is never substituted
//!
//! A latency regression needs **both** sides to carry an observed
//! duration for the same webhook and round. A missing latency is not a
//! zero, not a "no change", and not a reason to compare against
//! anything: it is simply not comparable, and the pair is skipped
//! (Task 4.6 Step 4, Global Constraint 15). Turning a `None` into
//! `Duration::ZERO` would manufacture an enormous fabricated *improvement*
//! on the missing side, which is the exact hazard
//! `WebhookInvocation::latency`'s own documentation warns about.
//!
//! Given two observed durations, all three of these must hold:
//!
//! 1. the candidate is strictly slower than the baseline;
//! 2. `candidate >= baseline + absolute_increase`;
//! 3. `candidate >= baseline * relative_multiplier`.
//!
//! Conditions 2 and 3 are the roadmap's conjunctive thresholds, and
//! [`LatencyPolicy`]'s own documentation states the same pair. Condition
//! 1 is this module's addition and it is load-bearing: a user may
//! legitimately configure `absoluteIncrease: 0` / `relativeMultiplier:
//! 1.0` (zero tolerance), under which conditions 2 and 3 are satisfied
//! by an *identical* latency, and every webhook in a run would be
//! reported as a regression. A regression is an increase; an unchanged
//! or improved latency is never one, whatever the thresholds say.
//! (`LatencyPolicy::default()` itself is the Alpha 100ms + 2x pair, but
//! this guard must not depend on that.)
//!
//! Two arithmetic notes. The absolute threshold uses
//! [`Duration::checked_add`], so an absurd configured tolerance saturates
//! into "unreachable, therefore not a regression" instead of panicking.
//! The relative threshold is evaluated in `f64` seconds; a non-finite or
//! negative `relative_multiplier` makes the comparison false or leaves
//! the absolute threshold as the only gate, and never panics the way
//! [`Duration::mul_f64`] would.
//!
//! Improvements are not reported. The kind exists to flag a candidate
//! that got slower, and a report shows both observed durations anyway.
//!
//! # Evidence completeness: what silence means
//!
//! [`TraceEvidence`] says how much of a chain was actually watched, and
//! it governs which claims are honest at all. [`trace_comparability`] is
//! the seam that reports it, exactly as
//! [`crate::admission::decision_comparability`] reports the decision-side
//! equivalent, and for the same reason: an empty change list is a
//! positive claim ("these two chains agree") that is only true when both
//! chains were observed.
//!
//! - **Either side [`TraceEvidence::Unavailable`]**: nothing is emitted
//!   at all. That variant's own contract is that `invocations` "should be
//!   treated as empty regardless of its actual length", so comparing
//!   against it would report every invocation on the other side as added
//!   or removed -- a fabricated chain-wide regression built out of a
//!   missing audit backend.
//! - **Either side [`TraceEvidence::Partial`]**: comparisons between
//!   invocations *both* sides observed are still perfectly valid and are
//!   emitted. Claims that rest on **absence** are not: an invocation
//!   present in a complete baseline and missing from a partial candidate
//!   is not proof of removal, because "missing" is precisely what partial
//!   evidence is allowed to be. So membership changes -- in both
//!   directions, since either side's partiality can hide an invocation --
//!   are suppressed, and [`trace_comparability`] is how a caller learns
//!   that the run's silence about them is qualified.

use std::time::Duration;

use admissionlab_admission::trace::{TraceEvidence, WebhookOutcome};
use admissionlab_normalize::{NormalizedTrace, NormalizedWebhookInvocation};
use admissionlab_spec::LatencyPolicy;
use serde::Serialize;
use serde_json::{Value, json};

use crate::types::{SemanticChange, SemanticChangeKind, unattributed_fixture_id};

/// How much of two webhook chains was actually observed, and therefore
/// which claims about them are honest.
///
/// The trace-side counterpart of
/// [`crate::admission::DecisionComparability`], and a three-state answer
/// rather than a two-state one because [`TraceEvidence`] itself has three
/// states and the middle one changes what may be concluded rather than
/// whether anything may be.
///
/// Serializes with pinned wire tags, since it reaches the same reports
/// [`SemanticChange`] does. Derives no `Default`, for the reason
/// [`TraceEvidence`] documents: `Comparable` is the convenient answer,
/// and defaulting to it would silently upgrade "we could not watch" into
/// "we watched and it was fine".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TraceComparability {
    /// Both chains were fully observed. Differences *and* absences are
    /// both evidence, and an empty change list means the two chains
    /// genuinely agree.
    #[serde(rename = "comparable")]
    Comparable,
    /// At least one chain was only partially observed. Differences
    /// between invocations both sides recorded are still real; an
    /// invocation missing from one side proves nothing, and no
    /// added/removed claim was made about it.
    #[serde(rename = "partial")]
    Partial {
        /// The baseline side's own evidence level.
        baseline: TraceEvidence,
        /// The candidate side's own evidence level.
        candidate: TraceEvidence,
    },
    /// At least one chain has no usable evidence. Nothing was compared.
    #[serde(rename = "incomparable")]
    Incomparable {
        /// The baseline side's own evidence level.
        baseline: TraceEvidence,
        /// The candidate side's own evidence level.
        candidate: TraceEvidence,
    },
}

impl TraceComparability {
    /// Returns `true` only for [`TraceComparability::Comparable`].
    #[must_use]
    pub fn is_comparable(&self) -> bool {
        matches!(self, Self::Comparable)
    }

    /// Whether an invocation missing from one side may be reported as
    /// added or removed.
    ///
    /// True only when both chains were fully observed. See this module's
    /// documentation for why partial evidence makes an absence
    /// uninformative while leaving every both-sides comparison intact.
    #[must_use]
    pub fn absence_is_evidence(&self) -> bool {
        matches!(self, Self::Comparable)
    }
}

/// Reports how completely the two chains were observed.
///
/// [`TraceComparability::Incomparable`] whenever either side is
/// [`TraceEvidence::Unavailable`], [`TraceComparability::Partial`]
/// whenever either side is [`TraceEvidence::Partial`] (and neither is
/// unavailable), and [`TraceComparability::Comparable`] only when both
/// sides are [`TraceEvidence::Observed`]. The worse of the two sides
/// always decides: a comparison is no better than its weaker half.
#[must_use]
pub fn trace_comparability(
    baseline: &NormalizedTrace,
    candidate: &NormalizedTrace,
) -> TraceComparability {
    let sides = (baseline.evidence, candidate.evidence);
    match sides {
        (TraceEvidence::Unavailable, _) | (_, TraceEvidence::Unavailable) => {
            TraceComparability::Incomparable {
                baseline: sides.0,
                candidate: sides.1,
            }
        }
        (TraceEvidence::Partial, _) | (_, TraceEvidence::Partial) => TraceComparability::Partial {
            baseline: sides.0,
            candidate: sides.1,
        },
        (TraceEvidence::Observed, TraceEvidence::Observed) => TraceComparability::Comparable,
    }
}

/// Classifies the differences between two sides' webhook chains.
///
/// Emits at most one invocation-level change (either
/// `webhook_invocation_changed` or `webhook_failed`) and at most one
/// `webhook_latency_changed` per `(configuration, webhook, round)`, in
/// sorted key order. See this module's documentation for the matching
/// rule, for why a dimension is compared only where both sides observed
/// it, and for what an empty result does and does not mean --
/// [`trace_comparability`] is what separates "the chains agree" from
/// "the chains could not be compared".
///
/// `latency_policy` supplies the two configured thresholds and nothing
/// else; this function decides nothing about where they came from.
///
/// Every returned change carries
/// [`crate::types::unattributed_fixture_id`]: a [`NormalizedTrace`] does
/// not say which fixture it came from, and this task's signature is
/// frozen at three parameters. The caller stamps the real identity with
/// [`SemanticChange::attributed_to`]. `object_path` is [`None`] -- a
/// webhook invocation is not a field of the compared object -- and
/// `origin` is [`None`], since first-divergence attribution is Task
/// 4.7's separate function.
#[must_use]
pub fn diff_admission_trace(
    baseline: &NormalizedTrace,
    candidate: &NormalizedTrace,
    latency_policy: &LatencyPolicy,
) -> Vec<SemanticChange> {
    let comparability = trace_comparability(baseline, candidate);
    if !matches!(
        comparability,
        TraceComparability::Comparable | TraceComparability::Partial { .. }
    ) {
        return Vec::new();
    }
    let absence_is_evidence = comparability.absence_is_evidence();

    let baseline_invocations = index_invocations(baseline);
    let candidate_invocations = index_invocations(candidate);
    let keys: std::collections::BTreeSet<InvocationKey<'_>> = baseline_invocations
        .keys()
        .chain(candidate_invocations.keys())
        .copied()
        .collect();

    let mut changes = Vec::new();
    for key in keys {
        match (
            baseline_invocations.get(&key),
            candidate_invocations.get(&key),
        ) {
            (Some(in_baseline), Some(in_candidate)) => {
                diff_invocation(in_baseline, in_candidate, &mut changes);
                diff_latency(in_baseline, in_candidate, latency_policy, &mut changes);
            }
            (Some(only_baseline), None) if absence_is_evidence => changes.push(change(
                SemanticChangeKind::WebhookInvocationChanged,
                only_baseline.webhook.as_str(),
                Some(invocation_value(only_baseline)),
                None,
            )),
            (None, Some(only_candidate)) if absence_is_evidence => changes.push(change(
                SemanticChangeKind::WebhookInvocationChanged,
                only_candidate.webhook.as_str(),
                None,
                Some(invocation_value(only_candidate)),
            )),
            // Partial evidence: an absence proves nothing. See this
            // module's documentation. (`(None, None)` cannot occur --
            // every key came from one of the two maps.)
            _ => {}
        }
    }
    changes
}

/// A webhook's participation in one admission round: the match key.
type InvocationKey<'trace> = (&'trace str, &'trace str, u32);

/// Indexes a trace's invocations by `(configuration, webhook, round)`,
/// keeping the first occurrence of a repeated key.
fn index_invocations(
    trace: &NormalizedTrace,
) -> std::collections::BTreeMap<InvocationKey<'_>, &NormalizedWebhookInvocation> {
    let mut indexed = std::collections::BTreeMap::new();
    for invocation in &trace.invocations {
        indexed
            .entry((
                invocation.configuration.as_str(),
                invocation.webhook.as_str(),
                invocation.round,
            ))
            .or_insert(invocation);
    }
    indexed
}

/// Emits the single invocation-level claim for one webhook and round, if
/// there is one.
fn diff_invocation(
    baseline: &NormalizedWebhookInvocation,
    candidate: &NormalizedWebhookInvocation,
    changes: &mut Vec<SemanticChange>,
) {
    // The candidate started failing a call the baseline was observed to
    // complete. The more specific claim wins; see this module's
    // documentation for the three cases this deliberately excludes.
    let newly_failing = candidate.outcome == WebhookOutcome::Errored
        && matches!(
            baseline.outcome,
            WebhookOutcome::Allowed | WebhookOutcome::Denied
        );

    let outcome_changed = observed_outcomes_differ(baseline.outcome, candidate.outcome);
    let invocation_differs = baseline.index != candidate.index
        || both_observed_and_differ(baseline.mutated, candidate.mutated)
        || both_observed_and_differ(baseline.patch.as_ref(), candidate.patch.as_ref())
        || outcome_changed;

    if !invocation_differs {
        return;
    }
    changes.push(change(
        if newly_failing {
            SemanticChangeKind::WebhookFailed
        } else {
            SemanticChangeKind::WebhookInvocationChanged
        },
        candidate.webhook.as_str(),
        Some(invocation_value(baseline)),
        Some(invocation_value(candidate)),
    ));
}

/// Emits a latency regression for one webhook and round, if both sides
/// observed a duration and the candidate's exceeds both thresholds.
fn diff_latency(
    baseline: &NormalizedWebhookInvocation,
    candidate: &NormalizedWebhookInvocation,
    policy: &LatencyPolicy,
    changes: &mut Vec<SemanticChange>,
) {
    // Never `unwrap_or_default()`: an unobserved latency is not a zero.
    let (Some(before), Some(after)) = (baseline.latency, candidate.latency) else {
        return;
    };
    if !is_latency_regression(before, after, policy) {
        return;
    }
    changes.push(change(
        SemanticChangeKind::WebhookLatencyChanged,
        candidate.webhook.as_str(),
        Some(latency_value(before)),
        Some(latency_value(after)),
    ));
}

/// Whether `after` counts as a latency regression against `before` under
/// `policy`.
///
/// All three conditions this module's documentation lists must hold. The
/// arithmetic is written to be total: no [`Duration`] overflow panic, and
/// no `f64` conversion that can panic on a hostile configured
/// multiplier.
fn is_latency_regression(before: Duration, after: Duration, policy: &LatencyPolicy) -> bool {
    if after <= before {
        return false;
    }
    let Some(absolute_threshold) = before.checked_add(policy.absolute_increase) else {
        // An absolute tolerance this large is unreachable by any real
        // observation, so nothing clears it.
        return false;
    };
    if after < absolute_threshold {
        return false;
    }
    // `f64` seconds rather than `Duration::mul_f64`, which panics on a
    // negative, NaN, or overflowing multiplier. A NaN here simply makes
    // the comparison false.
    after.as_secs_f64() >= before.as_secs_f64() * policy.relative_multiplier
}

/// Whether two optional observations both exist and disagree.
///
/// [`None`] on either side means that side did not observe this
/// dimension, which is never itself a difference; see this module's
/// documentation.
fn both_observed_and_differ<T: PartialEq>(baseline: Option<T>, candidate: Option<T>) -> bool {
    match (baseline, candidate) {
        (Some(left), Some(right)) => left != right,
        _ => false,
    }
}

/// Whether two outcomes were both established and disagree.
///
/// [`WebhookOutcome::Unknown`] is this dimension's "not observed" value,
/// and is excluded for exactly the reason a [`None`] is.
fn observed_outcomes_differ(baseline: WebhookOutcome, candidate: WebhookOutcome) -> bool {
    baseline != WebhookOutcome::Unknown
        && candidate != WebhookOutcome::Unknown
        && baseline != candidate
}

/// Renders one invocation as a report-ready payload.
///
/// Unobserved values stay JSON `null` -- `mutated` and `patch` are
/// carried as the [`Option`]s they are, never flattened into a
/// plausible `false` or `[]`.
///
/// # Panics
///
/// Never in practice. The only fallible step is serializing a
/// `Vec<json_patch::PatchOperation>`, which is a plain structure of
/// pointers and `serde_json::Value`s with no non-string map key and no
/// custom `Serialize` impl that can error.
fn invocation_value(invocation: &NormalizedWebhookInvocation) -> Value {
    json!({
        "configuration": invocation.configuration,
        "webhook": invocation.webhook,
        "round": invocation.round,
        "index": invocation.index,
        "mutated": invocation.mutated,
        "patch": invocation.patch,
        "outcome": invocation.outcome,
    })
}

/// Renders one observed duration as a report-ready payload.
///
/// Milliseconds, matching every other duration this project renders
/// (`AdmissionOutcome::total_latency`, `ResponseArtifact::elapsed_millis`
/// and `WebhookInvocation::latency` all serialize this way). The
/// *comparison* above uses the full observed [`Duration`]; only the
/// rendering is rounded, and a duration too large for a `u64` of
/// milliseconds saturates rather than wrapping.
fn latency_value(latency: Duration) -> Value {
    json!({ "latency_millis": u64::try_from(latency.as_millis()).unwrap_or(u64::MAX) })
}

/// Builds one change, filling in the fields every change from this
/// module shares.
fn change(
    kind: SemanticChangeKind,
    webhook: &str,
    baseline: Option<Value>,
    candidate: Option<Value>,
) -> SemanticChange {
    SemanticChange {
        kind,
        fixture_id: unattributed_fixture_id(),
        // A webhook invocation is not a field of the compared object.
        object_path: None,
        // The bare webhook name, matching `crate::workload`'s convention
        // and what `admissionlab_policy`'s `ChangeSelector::subject`
        // compares against.
        subject: Some(webhook.to_owned()),
        baseline,
        candidate,
        origin: None,
    }
}
