//! Comparing what two stacks did with the same Gateway route contract
//! (ROADMAP Task 6.9).
//!
//! Everything else in this crate observes one side. This module is the
//! only place that looks at two and *claims* something, in the one
//! vocabulary the whole project grades and renders:
//! [`SemanticChange`]. `admissionlab-policy` then decides what each
//! claim is worth (Global Constraint 6 -- classification lives in
//! exactly one place, and severity in exactly one other).
//!
//! # Conditions, not `converged`
//!
//! [`crate::reconcile::ReconciliationEvidence::converged`] is *not* a
//! pass. `reconcile.rs` says so in as many words: a route the
//! implementation settled on **rejecting** converges with
//! `Accepted: False`. So nothing here reads that boolean as a verdict.
//! It is used for exactly one thing -- deciding whether a side is a
//! stable reference at all ([`GatewayEvidenceLevel`]) -- and every claim
//! this module makes comes from the conditions and the probe results
//! themselves.
//!
//! # What is compared, and how the two sides are paired
//!
//! A [`GatewayCaseResult`] is one route contract's result on one side,
//! and pairing two of them is the caller's job (`case.rs` explains why
//! the pairing key is `contract_id` rather than position). Given a pair,
//! five stages run in this fixed order, and each iterates a sorted key
//! set, so the output is deterministic without a final sort:
//!
//! 1. the `GatewayClass`'s `Accepted` condition;
//! 2. the `Gateway`'s `Accepted` and `Programmed` conditions;
//! 3. route parent *membership* -- which `Gateway`s, and which of their
//!    listeners, the route's status says it is attached to;
//! 4. the conditions of every parent both sides published;
//! 5. the probes, paired by index.
//!
//! **Parent pairing.** `diff_gateway`'s signature is frozen at two
//! results and is handed no [`crate::model::RouteContract`], so
//! [`crate::conditions::RouteEvidence::parent_for`] -- which resolves
//! *the contract's* parent -- cannot be used here. Parents are paired
//! instead by their full [`ParentIdentity`] with the namespace
//! **defaulted the way Gateway API defaults it** (an absent
//! `parentRef.namespace` means the route's own namespace), so a status
//! that spells the namespace out and one that omits it pair as the same
//! parent rather than as a detach plus an attach. That is the
//! contract-free equivalent of [`crate::conditions::ParentLookup`], and
//! it also covers the parents a contract does not name.
//!
//! An identity appearing *twice* on one side is that lookup's
//! `Ambiguous` case. Its conditions are not compared, for the reason
//! `parent_for` gives for refusing to take the first match: two entries
//! for one identity can disagree, and choosing between them by position
//! would make the verdict depend on list order.
//!
//! **Probe pairing** is by index within the case's `probes`, because
//! that is the probe's identity: `GatewayCaseResult::probes` holds one
//! result per [`crate::model::HttpProbeContract`] "in the order the
//! contract declares them", both sides ran the *same* resolved lab
//! configuration, and an [`crate::model::HttpProbeContract`] has no id
//! of its own to key on. [`ProbePair`] is that pairing, made public for
//! Task 6.11's renderer so the rule is written once.
//!
//! # Evidence completeness: what silence means
//!
//! [`gateway_comparability`] is this module's twin of
//! `admissionlab_diff::trace_comparability` and
//! `admissionlab_diff::decision_comparability`, and exists for the same
//! reason: an empty change list is a positive claim ("these two sides
//! behaved the same"), and that is only honest when both sides were
//! actually observed.
//!
//! Task 6.9 step 5 is the rule it encodes: *baseline converged and
//! candidate timeout/inconclusive becomes critical only when candidate
//! lacks a previously stable required condition or traffic contract;
//! otherwise surface inconclusive lab evidence.* Concretely:
//!
//! - **Neither side converged** -- [`GatewayComparability::Incomparable`],
//!   and nothing at all is emitted. Neither side established a
//!   reference behavior, and `reconcile.rs` already names this case:
//!   "one that times out on *both* is a broken fixture or a slow
//!   machine, not a behavior change".
//! - **One side converged** -- [`GatewayComparability::Partial`].
//!   Condition and traffic claims are still made, and that is exactly
//!   step 5's "critical when the candidate lacks a previously stable
//!   required condition or traffic contract": a candidate that times out
//!   having published `Accepted: False`, or having answered no probe the
//!   baseline answered, has lost something the baseline had, and this
//!   module says so. When it has lost nothing, no change is emitted and
//!   the `Partial` answer is the inconclusive lab evidence the caller
//!   surfaces.
//! - **A stale side** -- [`GatewayEvidenceLevel::Stale`], and
//!   [`GatewayComparability::absence_is_evidence`] turns `false`.
//!
//! # Stale evidence weakens absence claims (step 8)
//!
//! A condition's [`ConditionFreshness`] is a *relationship*: it is
//! `Stale` when the controller's `observedGeneration` is behind the
//! object's `metadata.generation`, and `Unknown` when it cannot be
//! determined at all. Either way the published status describes a spec
//! that is not the one under test, or describes nothing checkable -- so
//! what that status does **not** say proves nothing. A condition absent
//! from a stale status is not evidence of removal, and a parent absent
//! from a stale status is not evidence of detachment; both would
//! otherwise be reported as a regression built entirely out of a
//! controller that had not caught up yet. This is `conditions.rs`'s own
//! rule that `Missing` is never folded into `False`, carried across to a
//! comparison.
//!
//! So every claim that rests on *absence* -- `route_attached`,
//! `route_detached`, `listener_binding_changed`, a condition that is
//! [`ConditionState::Missing`] on one side, a probe present on only one
//! side -- is made only when
//! [`GatewayComparability::absence_is_evidence`] holds. Claims where
//! both sides published a value (`True -> False`, `200 -> 503`) are
//! unaffected: a settled value is a statement, and this module compares
//! statements.
//!
//! Probes are the one exception, because they are gated by
//! *convergence* rather than by freshness: an [`HttpProbeResult`] has no
//! `observedGeneration` for staleness to be measured against, and a
//! probe is only ever sent once a route has settled. A probe the
//! **baseline** answered and the candidate did not is claimed whenever
//! the baseline converged -- that is step 5's "candidate lacks a ...
//! previously passing traffic contract" in its literal form, and it is
//! the case that must survive a candidate timeout. The mirror case -- a
//! probe only the *candidate* answered -- is claimed only when **both**
//! sides converged: a baseline that never reconciled had no data plane
//! to probe, Task 6.11 records that as an explicit skip, and reporting
//! the candidate's extra answer as a traffic regression would run the
//! claim backwards.
//!
//! # A reason string is never a change
//!
//! Two sides can publish the same condition state with different
//! `reason`/`message` text -- implementations word things differently,
//! and the same implementation rewords them between versions. Only a
//! **state** difference produces a change here; the reasons ride along
//! in the payloads as evidence. This is Task 4.4's rejected-to-rejected
//! message-drift rule applied to conditions, and it is also why step 6
//! insists the comparator "encode direction rather than relying on
//! free-form reason strings".
//!
//! # Direction, and who grades it
//!
//! Every condition change carries an
//! [`admissionlab_diff::ChangeDirection`] in its candidate payload when
//! the transition determines one: `Improvement` when the candidate
//! reached `True` from something that was not `True`, `Regression` when
//! it left a `True` the baseline published, and *nothing* when neither
//! side is `True` (a `False -> Unknown` move is not an improvement, and
//! claiming it were would be a guess). `admissionlab_policy`'s
//! `default_change_severity` is what turns that fact into the step 6
//! downgrade -- severity is decided there, never here.
//!
//! Traffic changes carry no direction on purpose: whether `503 -> 200`
//! is an improvement depends on the probe's
//! [`crate::model::HttpProbeContract::expected_status`], and this
//! function is not handed the contract. Inventing a direction from the
//! status code alone would be exactly the fabrication Global Constraint
//! 15 forbids.
//!
//! # Fixture attribution
//!
//! Every change is stamped with
//! [`admissionlab_diff::unattributed_fixture_id`] and the caller that
//! knows which fixture it paired replaces it with
//! [`SemanticChange::attributed_to`] -- the same seam, for the same
//! reason, as `admissionlab_diff`'s object and trace comparisons: a
//! [`GatewayCaseResult`] carries a `contract_id`, which is not a
//! `FixtureId`, and this task's signature is frozen at two parameters.
//!
//! `object_path` is [`None`] on every change. It is documented as a
//! pointer "into the compared object", and a Gateway case compares
//! *three* objects (a `GatewayClass`, a `Gateway`, an `HTTPRoute`) plus
//! a set of HTTP responses; a pointer with no stated document would be
//! worse than none. Which object a change is about is named in its
//! payload's `object` key, and its bare name is the change's `subject`.

use std::collections::{BTreeMap, BTreeSet};

use admissionlab_diff::{
    ChangeDirection, DIRECTION_KEY, SemanticChange, SemanticChangeKind, unattributed_fixture_id,
};
// ROADMAP Task 7.2 (frozen `admissionlab.io/result/v1` result
// schema): every type this file defines that reaches a run's result
// document is embedded verbatim in it, so the schema generated from the
// result model has to describe it. Derives and `#[schemars(with = ...)]`
// restatements of what the existing `serialize_with` helpers already
// emit -- no field, name, or semantic change.
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use crate::case::GatewayCaseResult;
use crate::conditions::{
    CONDITION_ACCEPTED, CONDITION_PROGRAMMED, CONDITION_RESOLVED_REFS, ConditionFreshness,
    ConditionState, ObservedCondition, ParentIdentity, RouteEvidence, RouteParentStatus,
};
use crate::probe::HttpProbeResult;
use crate::reconcile::{
    REQUIRED_GATEWAY_CONDITIONS, REQUIRED_ROUTE_PARENT_CONDITIONS, ReconciliationEvidence,
};

/// The `object` key's value for each kind of object a change can be
/// about, matching the Kubernetes kind exactly so a payload and a
/// `kubectl get` name the same thing.
const OBJECT_GATEWAY_CLASS: &str = "GatewayClass";
const OBJECT_GATEWAY: &str = "Gateway";
const OBJECT_ROUTE: &str = "HTTPRoute";

/// How good one side's evidence is, as a reference to compare against.
///
/// The Gateway counterpart of `admissionlab_admission::trace::TraceEvidence`,
/// and three values for the same reason that one has three: the middle
/// one changes *which* claims are honest rather than whether any is.
///
/// Derives no `Default`. [`GatewayEvidenceLevel::Converged`] is the
/// convenient answer, and defaulting to it would silently upgrade "the
/// wait timed out" into "the route settled".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub enum GatewayEvidenceLevel {
    /// The route reached a stable, current status within the
    /// reconciliation deadline. Its conditions and probes are a
    /// reference behavior.
    #[serde(rename = "converged")]
    Converged,
    /// The wait ended without convergence, but what the side *did*
    /// publish describes the spec under test (every compared condition
    /// it published is [`ConditionFreshness::Current`]).
    ///
    /// This is still real evidence: the waiter did not take a snapshot,
    /// it waited out the whole deadline, so a condition this side never
    /// published is a condition it did not publish *in time*. It is not
    /// a stable reference, which is what makes the comparison
    /// [`GatewayComparability::Partial`].
    #[serde(rename = "unconverged")]
    Unconverged,
    /// The wait ended without convergence *and* what the side published
    /// is stale or of undeterminable currency -- it describes a spec
    /// that has since changed, or carries no `observedGeneration` to
    /// check, or is empty. Absences on this side prove nothing; see this
    /// module's "Stale evidence weakens absence claims".
    #[serde(rename = "stale")]
    Stale,
}

/// How comparable two sides' Gateway evidence is, and therefore which
/// claims about them are honest.
///
/// Mirrors `admissionlab_diff::TraceComparability` in shape and in
/// intent; see this module's "Evidence completeness" section for the
/// rule each variant encodes. Serializes with pinned wire tags, since it
/// reaches the same reports a [`SemanticChange`] does.
///
/// Derives no `Default`, for the reason
/// `admissionlab_diff::TraceComparability` gives: `Comparable` is the
/// flattering answer, and defaulting to it would turn "we could not
/// tell" into "we looked and it was fine".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub enum GatewayComparability {
    /// Both sides converged. Differences *and* absences are evidence,
    /// and an empty change list means the two sides genuinely behaved
    /// the same.
    #[serde(rename = "comparable")]
    Comparable,
    /// Exactly one side converged. Differences between values both sides
    /// published are real -- including a required condition the
    /// converged side published and the other did not -- but no claim
    /// rests on the unconverged side's silence unless that side's status
    /// is current.
    #[serde(rename = "partial")]
    Partial {
        /// The baseline side's own evidence level.
        baseline: GatewayEvidenceLevel,
        /// The candidate side's own evidence level.
        candidate: GatewayEvidenceLevel,
    },
    /// Neither side converged. Nothing was compared: with no stable
    /// reference on either side, every difference between two in-flight
    /// statuses would be noise reported as a regression.
    #[serde(rename = "incomparable")]
    Incomparable {
        /// The baseline side's own evidence level.
        baseline: GatewayEvidenceLevel,
        /// The candidate side's own evidence level.
        candidate: GatewayEvidenceLevel,
    },
}

impl GatewayComparability {
    /// Returns `true` only for [`GatewayComparability::Comparable`].
    #[must_use]
    pub fn is_comparable(self) -> bool {
        matches!(self, Self::Comparable)
    }

    /// Whether something *absent* from one side may be claimed as a
    /// difference.
    ///
    /// False when either side is [`GatewayEvidenceLevel::Stale`] and
    /// when nothing was compared at all. See this module's "Stale
    /// evidence weakens absence claims" for the argument, and for the
    /// one documented exception this predicate does not gate.
    #[must_use]
    pub fn absence_is_evidence(self) -> bool {
        match self {
            Self::Comparable => true,
            Self::Partial {
                baseline,
                candidate,
            } => {
                baseline != GatewayEvidenceLevel::Stale && candidate != GatewayEvidenceLevel::Stale
            }
            Self::Incomparable { .. } => false,
        }
    }
}

/// How good one side's evidence is on its own.
///
/// `converged` wins outright: the waiter's convergence rule already
/// required every one of that side's compared conditions to be settled
/// *and* current, and it is the authority on what it observed over the
/// whole wait rather than at the last poll.
#[must_use]
pub fn gateway_evidence_level(result: &GatewayCaseResult) -> GatewayEvidenceLevel {
    if result.reconciliation.converged {
        GatewayEvidenceLevel::Converged
    } else if compared_condition_freshness(&result.reconciliation) == ConditionFreshness::Current {
        GatewayEvidenceLevel::Unconverged
    } else {
        GatewayEvidenceLevel::Stale
    }
}

/// Reports how comparable the two sides' evidence is.
///
/// [`GatewayComparability::Comparable`] when both converged,
/// [`GatewayComparability::Incomparable`] when neither did, and
/// [`GatewayComparability::Partial`] when exactly one did -- carrying
/// both sides' own [`GatewayEvidenceLevel`] in the two cases where they
/// are not both `Converged`.
#[must_use]
pub fn gateway_comparability(
    baseline: &GatewayCaseResult,
    candidate: &GatewayCaseResult,
) -> GatewayComparability {
    let levels = (
        gateway_evidence_level(baseline),
        gateway_evidence_level(candidate),
    );
    match levels {
        (GatewayEvidenceLevel::Converged, GatewayEvidenceLevel::Converged) => {
            GatewayComparability::Comparable
        }
        (GatewayEvidenceLevel::Converged, _) | (_, GatewayEvidenceLevel::Converged) => {
            GatewayComparability::Partial {
                baseline: levels.0,
                candidate: levels.1,
            }
        }
        _ => GatewayComparability::Incomparable {
            baseline: levels.0,
            candidate: levels.1,
        },
    }
}

/// The freshness of every condition this module would compare on one
/// side, folded into one answer by [`ConditionFreshness::merge`].
///
/// Only conditions the side actually *published* are folded in.
/// Including looked-up-but-missing ones would fold in the
/// [`ConditionFreshness::Unknown`] that
/// [`ObservedCondition::missing`] necessarily has, and so would make
/// every absence weaken its own evidence in a circle. A side that
/// published none of them at all folds to `Unknown`: there is then
/// nothing at all to say the status describes the spec under test.
fn compared_condition_freshness(evidence: &ReconciliationEvidence) -> ConditionFreshness {
    let mut answers = Vec::new();
    for type_name in REQUIRED_GATEWAY_CONDITIONS {
        if let Some(condition) = evidence.gateway.conditions.get(type_name) {
            answers.push(condition.freshness(evidence.gateway.generation));
        }
    }
    for parent in &evidence.route.parents {
        for type_name in REQUIRED_ROUTE_PARENT_CONDITIONS {
            if let Some(condition) = parent.conditions.get(type_name) {
                answers.push(condition.freshness(evidence.route.generation));
            }
        }
    }

    answers
        .into_iter()
        .reduce(ConditionFreshness::merge)
        .unwrap_or(ConditionFreshness::Unknown)
}

/// One route contract's result on both sides.
///
/// §1.2's canonical name for the pair, and the unit Task 6.11 renders.
/// Pairing is the caller's job -- see [`GatewayCaseComparison::is_paired`]
/// for what this type does and does not check about that.
///
/// # Handoff to Task 6.11
///
/// `admissionlab_report::model::GatewayCaseComparison` is a **separate,
/// empty placeholder**, reserved by Task 4.10 so
/// `FixtureComparison::gateway` had a stable name to carry through
/// Alpha (Global Constraint 8 kept Gateway work off the Alpha critical
/// path). *This* is the real type. They are not yet the same type
/// because making them so means `admissionlab-report` depending on
/// `admissionlab-gateway`, which is a report-shaped decision Task 6.11
/// owns: that task wires Gateway results into a run's report and can
/// weigh the edge against the alternatives with the renderer in front of
/// it. Until then the placeholder stays empty and that field stays
/// `None`; nothing constructs a fake one in the meantime. The
/// placeholder's own documentation points back here.
///
/// `Serialize` but not `Deserialize`, like every evidence type in this
/// crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayCaseComparison {
    /// What the baseline stack did with this route contract.
    pub baseline: GatewayCaseResult,
    /// What the candidate stack did with the same route contract.
    pub candidate: GatewayCaseResult,
}

impl GatewayCaseComparison {
    /// The contract both sides are about, taken from the baseline.
    ///
    /// Reading one side rather than storing a third copy of the string:
    /// §1.2 freezes this type's two fields, and a stored `contract_id`
    /// could disagree with the results it labels. [`Self::is_paired`] is
    /// how a caller checks the two sides agree.
    #[must_use]
    pub fn contract_id(&self) -> &str {
        self.baseline.contract_id.as_str()
    }

    /// Whether both sides describe the same route contract.
    ///
    /// Never checked implicitly: a mispaired comparison is a *caller*
    /// bug (the run loop pairs by `contract_id`), and silently returning
    /// no changes for one would hide it. Every other method here assumes
    /// the pairing is right, exactly as `admissionlab-diff`'s
    /// comparisons assume their two sides describe one fixture.
    #[must_use]
    pub fn is_paired(&self) -> bool {
        self.baseline.contract_id == self.candidate.contract_id
    }

    /// How comparable the two sides' evidence is -- see
    /// [`gateway_comparability`].
    #[must_use]
    pub fn comparability(&self) -> GatewayComparability {
        gateway_comparability(&self.baseline, &self.candidate)
    }

    /// The behavior differences between the two sides -- see
    /// [`diff_gateway`], including who is expected to stamp the returned
    /// changes with a real fixture identity.
    #[must_use]
    pub fn changes(&self) -> Vec<SemanticChange> {
        diff_gateway(&self.baseline, &self.candidate)
    }

    /// Every probe both sides answered, paired by index.
    ///
    /// A probe answered by only one side is **not** in this list:
    /// §1.2 gives [`ProbePair`] two non-optional results, and a pair with
    /// a fabricated half would be a claim about a request that was never
    /// answered. [`diff_gateway`] reports those one-sided probes as
    /// changes instead. See this module's "Probe pairing" for why the key
    /// is the index.
    #[must_use]
    pub fn probe_pairs(&self) -> Vec<ProbePair> {
        paired_probes(&self.baseline, &self.candidate)
            .map(|(_, baseline, candidate)| ProbePair {
                contract_id: self.contract_id().to_owned(),
                baseline: baseline.clone(),
                candidate: candidate.clone(),
            })
            .collect()
    }
}

/// One probe's result on both sides.
///
/// §1.2's canonical name. Carries its own `contract_id` (rather than
/// borrowing the enclosing comparison's) because a report renders probe
/// pairs in a flat list across contracts, where each row has to be able
/// to say which route it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbePair {
    /// The [`crate::model::RouteContract::id`] both results belong to.
    pub contract_id: String,
    /// What the baseline stack's data plane returned.
    pub baseline: HttpProbeResult,
    /// What the candidate stack's data plane returned for the same
    /// probe.
    pub candidate: HttpProbeResult,
}

/// Classifies the behavior differences between two sides' results for
/// one Gateway route contract.
///
/// Read this module's documentation before relying on what an empty
/// result means: it can mean the two sides behaved identically, and it
/// can mean [`gateway_comparability`] found nothing worth comparing.
/// Callers that report an empty list as "no Gateway regressions" must
/// consult that function too -- which is the whole reason it is separate
/// and public.
///
/// Deterministic: the same two results always produce the same changes
/// in the same order, with no clock, network, or ambient state involved
/// (Global Constraint 7).
///
/// Every change carries
/// [`admissionlab_diff::unattributed_fixture_id`] and [`None`] for both
/// `object_path` and `origin`; see this module's "Fixture attribution".
#[must_use]
pub fn diff_gateway(
    baseline: &GatewayCaseResult,
    candidate: &GatewayCaseResult,
) -> Vec<SemanticChange> {
    let comparability = gateway_comparability(baseline, candidate);
    if matches!(comparability, GatewayComparability::Incomparable { .. }) {
        return Vec::new();
    }
    let absence_is_evidence = comparability.absence_is_evidence();

    let mut changes = Vec::new();
    diff_gateway_class(baseline, candidate, absence_is_evidence, &mut changes);
    diff_gateway_object(baseline, candidate, absence_is_evidence, &mut changes);
    diff_route_parents(baseline, candidate, absence_is_evidence, &mut changes);
    diff_probes(baseline, candidate, &mut changes);
    changes
}

/// Stage 1: the `GatewayClass`'s `Accepted` condition, and whether the
/// class was observed at all.
fn diff_gateway_class(
    baseline: &GatewayCaseResult,
    candidate: &GatewayCaseResult,
    absence_is_evidence: bool,
    changes: &mut Vec<SemanticChange>,
) {
    let sides = (
        baseline.reconciliation.gateway_class.as_ref(),
        candidate.reconciliation.gateway_class.as_ref(),
    );
    // `ReconciliationEvidence::gateway_class` is `None` both when the
    // `Gateway` named no class and when it named one that is not on the
    // cluster, so a one-sided `None` is an absence claim like any other.
    match sides {
        (Some(baseline_class), Some(candidate_class)) => {
            let pair = ConditionPair {
                object: OBJECT_GATEWAY_CLASS,
                name: baseline_class.name.clone(),
                namespace: None,
                parent: None,
                baseline: SideCondition::classless(baseline_class.accepted.clone()),
                candidate: SideCondition::classless(candidate_class.accepted.clone()),
            };
            pair.push_if_claimable(
                SemanticChangeKind::AcceptedConditionChanged,
                absence_is_evidence,
                changes,
            );
        }
        (Some(baseline_class), None) if absence_is_evidence => changes.push(change(
            SemanticChangeKind::AcceptedConditionChanged,
            Some(baseline_class.name.clone()),
            Some(condition_payload(
                OBJECT_GATEWAY_CLASS,
                &baseline_class.name,
                None,
                None,
                &SideCondition::classless(baseline_class.accepted.clone()),
                None,
            )),
            None,
        )),
        (None, Some(candidate_class)) if absence_is_evidence => changes.push(change(
            SemanticChangeKind::AcceptedConditionChanged,
            Some(candidate_class.name.clone()),
            None,
            Some(condition_payload(
                OBJECT_GATEWAY_CLASS,
                &candidate_class.name,
                None,
                None,
                &SideCondition::classless(candidate_class.accepted.clone()),
                None,
            )),
        )),
        _ => {}
    }
}

/// Stage 2: the `Gateway`'s own `Accepted` and `Programmed` conditions,
/// in [`REQUIRED_GATEWAY_CONDITIONS`] order.
fn diff_gateway_object(
    baseline: &GatewayCaseResult,
    candidate: &GatewayCaseResult,
    absence_is_evidence: bool,
    changes: &mut Vec<SemanticChange>,
) {
    let baseline_gateway = &baseline.reconciliation.gateway;
    let candidate_gateway = &candidate.reconciliation.gateway;
    for (type_name, kind) in [
        (
            CONDITION_ACCEPTED,
            SemanticChangeKind::AcceptedConditionChanged,
        ),
        (
            CONDITION_PROGRAMMED,
            SemanticChangeKind::ProgrammedConditionChanged,
        ),
    ] {
        let pair = ConditionPair {
            object: OBJECT_GATEWAY,
            name: baseline_gateway.identity.name.clone(),
            namespace: Some(baseline_gateway.identity.namespace.clone()),
            parent: None,
            baseline: SideCondition::new(
                baseline_gateway.condition(type_name),
                baseline_gateway.generation,
            ),
            candidate: SideCondition::new(
                candidate_gateway.condition(type_name),
                candidate_gateway.generation,
            ),
        };
        pair.push_if_claimable(kind, absence_is_evidence, changes);
    }
}

/// Stages 3 and 4: which parents the route is attached to, and what each
/// parent both sides published says.
fn diff_route_parents(
    baseline: &GatewayCaseResult,
    candidate: &GatewayCaseResult,
    absence_is_evidence: bool,
    changes: &mut Vec<SemanticChange>,
) {
    let baseline_route = &baseline.reconciliation.route;
    let candidate_route = &candidate.reconciliation.route;
    let baseline_parents = index_parents(baseline_route);
    let candidate_parents = index_parents(candidate_route);

    // Stage 3, part one: whole parents, keyed by the Gateway they name.
    let baseline_gateways = group_by_gateway(&baseline_parents);
    let candidate_gateways = group_by_gateway(&candidate_parents);
    let gateway_keys: BTreeSet<&(String, String)> = baseline_gateways
        .keys()
        .chain(candidate_gateways.keys())
        .collect();
    for key in gateway_keys {
        match (baseline_gateways.get(key), candidate_gateways.get(key)) {
            (Some(listeners), None) if absence_is_evidence => changes.push(change(
                SemanticChangeKind::RouteDetached,
                Some(baseline_route.name.clone()),
                Some(attachment_payload(baseline_route, key, listeners)),
                None,
            )),
            (None, Some(listeners)) if absence_is_evidence => changes.push(change(
                SemanticChangeKind::RouteAttached,
                Some(candidate_route.name.clone()),
                None,
                Some(attachment_payload(candidate_route, key, listeners)),
            )),
            // Stage 3, part two: the same Gateway, a different set of
            // its listeners. One change per listener bound on exactly
            // one side, so each change states one fact.
            (Some(baseline_listeners), Some(candidate_listeners)) if absence_is_evidence => {
                let listeners: BTreeSet<&Option<String>> = baseline_listeners
                    .iter()
                    .chain(candidate_listeners.iter())
                    .collect();
                for listener in listeners {
                    let in_baseline = baseline_listeners.contains(listener);
                    let in_candidate = candidate_listeners.contains(listener);
                    if in_baseline == in_candidate {
                        continue;
                    }
                    let payload = listener_payload(key, listener.as_deref());
                    changes.push(change(
                        SemanticChangeKind::ListenerBindingChanged,
                        // `None` for a `parentRef` that named no
                        // listener: there is no listener name to put
                        // here, and the Gateway is in the payload.
                        listener.clone(),
                        in_baseline.then(|| payload.clone()),
                        in_candidate.then_some(payload),
                    ));
                }
            }
            _ => {}
        }
    }

    // Stage 4: the conditions of every parent identity both sides
    // published exactly once.
    for (identity, baseline_entries) in &baseline_parents {
        let Some(candidate_entries) = candidate_parents.get(identity) else {
            continue;
        };
        let ([baseline_entry], [candidate_entry]) =
            (baseline_entries.as_slice(), candidate_entries.as_slice())
        else {
            // The `ParentLookup::Ambiguous` case -- see this module's
            // "Parent pairing".
            continue;
        };
        diff_parent_conditions(
            baseline_route,
            candidate_route,
            identity,
            baseline_entry,
            candidate_entry,
            absence_is_evidence,
            changes,
        );
    }
}

/// One parent's `Accepted` and `ResolvedRefs` conditions.
///
/// `ResolvedRefs` produces up to two changes, in this order:
/// `backend_resolution_changed` first (the product-level claim: whether
/// this route's backends resolve at all changed) and
/// `resolved_refs_condition_changed` second (the condition evidence for
/// it). That is Task 6.9 step 2's "backend-resolution change **plus**
/// condition evidence" read as two changes rather than one, because they
/// are two different claims that a policy must be able to grade and
/// except separately -- a team can accept that an implementation words
/// its `ResolvedRefs` differently without accepting that its backends
/// stopped resolving. The condition change is emitted for any state
/// difference; the backend-resolution change only when *exactly one*
/// side published `ResolvedRefs: True`, which is precisely when whether
/// the backends resolve has changed rather than merely how the
/// implementation described not resolving them.
fn diff_parent_conditions(
    baseline_route: &RouteEvidence,
    candidate_route: &RouteEvidence,
    identity: &ParentIdentity,
    baseline_entry: &RouteParentStatus,
    candidate_entry: &RouteParentStatus,
    absence_is_evidence: bool,
    changes: &mut Vec<SemanticChange>,
) {
    let pair = |type_name: &'static str| ConditionPair {
        object: OBJECT_ROUTE,
        name: baseline_route.name.clone(),
        namespace: Some(baseline_route.namespace.clone()),
        parent: Some(identity.clone()),
        baseline: SideCondition::new(
            baseline_entry.condition(type_name),
            baseline_route.generation,
        ),
        candidate: SideCondition::new(
            candidate_entry.condition(type_name),
            candidate_route.generation,
        ),
    };

    pair(CONDITION_ACCEPTED).push_if_claimable(
        SemanticChangeKind::AcceptedConditionChanged,
        absence_is_evidence,
        changes,
    );

    let resolved_refs = pair(CONDITION_RESOLVED_REFS);
    if !resolved_refs.is_claimable(absence_is_evidence) {
        return;
    }
    let resolution_changed = (resolved_refs.baseline.condition.state == ConditionState::True)
        != (resolved_refs.candidate.condition.state == ConditionState::True);
    if resolution_changed {
        resolved_refs.push(SemanticChangeKind::BackendResolutionChanged, changes);
    }
    resolved_refs.push(SemanticChangeKind::ResolvedRefsConditionChanged, changes);
}

/// Stage 5: the probes, paired by index.
fn diff_probes(
    baseline: &GatewayCaseResult,
    candidate: &GatewayCaseResult,
    changes: &mut Vec<SemanticChange>,
) {
    let route_name = baseline.reconciliation.route.name.clone();
    for (index, baseline_probe, candidate_probe) in paired_probes(baseline, candidate) {
        if baseline_probe.status != candidate_probe.status {
            changes.push(change(
                SemanticChangeKind::TrafficStatusChanged,
                Some(route_name.clone()),
                Some(probe_payload(index, baseline_probe)),
                Some(probe_payload(index, candidate_probe)),
            ));
        }
        // Only when *both* sides identified their backend. `None` means
        // the response did not identify itself, never "a different
        // backend answered" -- see `probe.rs`.
        if let (Some(baseline_backend), Some(candidate_backend)) =
            (&baseline_probe.backend, &candidate_probe.backend)
            && baseline_backend != candidate_backend
        {
            changes.push(change(
                SemanticChangeKind::TrafficBackendChanged,
                Some(baseline_backend.clone()),
                Some(probe_payload(index, baseline_probe)),
                Some(probe_payload(index, candidate_probe)),
            ));
        }
    }

    // A probe only one side answered. The two directions are gated
    // differently and deliberately -- see this module's "Stale evidence
    // weakens absence claims" for the argument.
    let paired = baseline.probes.len().min(candidate.probes.len());
    let baseline_converged = gateway_evidence_level(baseline) == GatewayEvidenceLevel::Converged;
    let candidate_converged = gateway_evidence_level(candidate) == GatewayEvidenceLevel::Converged;
    if baseline_converged {
        for (offset, probe) in baseline.probes.iter().skip(paired).enumerate() {
            changes.push(change(
                SemanticChangeKind::TrafficStatusChanged,
                Some(route_name.clone()),
                Some(probe_payload(paired + offset, probe)),
                None,
            ));
        }
    }
    if baseline_converged && candidate_converged {
        for (offset, probe) in candidate.probes.iter().skip(paired).enumerate() {
            changes.push(change(
                SemanticChangeKind::TrafficStatusChanged,
                Some(route_name.clone()),
                None,
                Some(probe_payload(paired + offset, probe)),
            ));
        }
    }
}

/// Every probe index both sides answered, with both results.
///
/// The one place the index-pairing rule is written; both
/// [`GatewayCaseComparison::probe_pairs`] and [`diff_probes`] use it.
fn paired_probes<'a>(
    baseline: &'a GatewayCaseResult,
    candidate: &'a GatewayCaseResult,
) -> impl Iterator<Item = (usize, &'a HttpProbeResult, &'a HttpProbeResult)> {
    baseline
        .probes
        .iter()
        .zip(candidate.probes.iter())
        .enumerate()
        .map(|(index, (baseline_probe, candidate_probe))| (index, baseline_probe, candidate_probe))
}

/// One side's view of one condition, with the generation its freshness
/// is measured against.
struct SideCondition {
    condition: ObservedCondition,
    /// The `metadata.generation` of the object that published it, or
    /// [`None`] for a `GatewayClass` -- §1.2 gives
    /// [`crate::conditions::GatewayClassEvidence`] no generation, so its
    /// freshness is genuinely undeterminable rather than merely
    /// unmeasured (`reconcile.rs` records the same absence in its own
    /// convergence rule).
    generation: Option<i64>,
}

impl SideCondition {
    fn new(condition: ObservedCondition, generation: i64) -> Self {
        Self {
            condition,
            generation: Some(generation),
        }
    }

    fn classless(condition: ObservedCondition) -> Self {
        Self {
            condition,
            generation: None,
        }
    }

    fn freshness(&self) -> Option<ConditionFreshness> {
        self.generation
            .map(|generation| self.condition.freshness(generation))
    }
}

/// One condition as both sides published it, and everything needed to
/// describe it in a change.
struct ConditionPair {
    object: &'static str,
    name: String,
    namespace: Option<String>,
    parent: Option<ParentIdentity>,
    /// Which condition this is is carried by the two
    /// [`ObservedCondition`]s themselves -- including on a side that
    /// published none, since [`ObservedCondition::missing`] records the
    /// type it was looked up under -- so there is no third copy here to
    /// disagree with them.
    baseline: SideCondition,
    candidate: SideCondition,
}

impl ConditionPair {
    /// Whether this pair may be claimed as a change at all: the two
    /// states differ, and if either side's state is
    /// [`ConditionState::Missing`] then absences are evidence on this
    /// comparison.
    fn is_claimable(&self, absence_is_evidence: bool) -> bool {
        if self.baseline.condition.state == self.candidate.condition.state {
            return false;
        }
        let involves_absence = self.baseline.condition.state == ConditionState::Missing
            || self.candidate.condition.state == ConditionState::Missing;
        absence_is_evidence || !involves_absence
    }

    /// Which way this transition moved, when it determines one. See this
    /// module's "Direction, and who grades it".
    fn direction(&self) -> Option<ChangeDirection> {
        let baseline_true = self.baseline.condition.state == ConditionState::True;
        let candidate_true = self.candidate.condition.state == ConditionState::True;
        match (baseline_true, candidate_true) {
            (false, true) => Some(ChangeDirection::Improvement),
            (true, false) => Some(ChangeDirection::Regression),
            _ => None,
        }
    }

    fn push(&self, kind: SemanticChangeKind, changes: &mut Vec<SemanticChange>) {
        changes.push(change(
            kind,
            Some(self.name.clone()),
            Some(condition_payload(
                self.object,
                &self.name,
                self.namespace.as_deref(),
                self.parent.as_ref(),
                &self.baseline,
                None,
            )),
            Some(condition_payload(
                self.object,
                &self.name,
                self.namespace.as_deref(),
                self.parent.as_ref(),
                &self.candidate,
                self.direction(),
            )),
        ));
    }

    fn push_if_claimable(
        &self,
        kind: SemanticChangeKind,
        absence_is_evidence: bool,
        changes: &mut Vec<SemanticChange>,
    ) {
        if self.is_claimable(absence_is_evidence) {
            self.push(kind, changes);
        }
    }
}

/// One side's condition, as a change payload.
///
/// `direction` is stamped on the candidate side only, under
/// `admissionlab_diff`'s own [`DIRECTION_KEY`], and only when the
/// transition determined one. Every other key is an observation:
/// `reason`, `observedGeneration` and `freshness` are [`Value::Null`]
/// where the cluster (or, for a `GatewayClass`, §1.2's type) supplied
/// nothing, never a stand-in value.
fn condition_payload(
    object: &str,
    name: &str,
    namespace: Option<&str>,
    parent: Option<&ParentIdentity>,
    side: &SideCondition,
    direction: Option<ChangeDirection>,
) -> Value {
    let mut payload = json!({
        "object": object,
        "name": name,
        "namespace": namespace,
        "condition": side.condition.type_name,
        "state": side.condition.state,
        "reason": side.condition.reason,
        "generation": side.generation,
        "observedGeneration": side.condition.observed_generation,
        "freshness": side.freshness(),
    });
    if let Some(parent) = parent {
        payload["parent"] = json!(parent);
    }
    if let Some(direction) = direction {
        payload[DIRECTION_KEY] = json!(direction.as_str());
    }
    payload
}

/// A whole parent attachment, as a change payload: which `Gateway`, and
/// which of its listeners this route's status bound to.
fn attachment_payload(
    route: &RouteEvidence,
    gateway: &(String, String),
    listeners: &BTreeSet<Option<String>>,
) -> Value {
    json!({
        "object": OBJECT_ROUTE,
        "name": route.name,
        "namespace": route.namespace,
        "gateway": {"namespace": gateway.0, "name": gateway.1},
        "listeners": listeners.iter().collect::<Vec<_>>(),
    })
}

/// One listener binding, as a change payload.
fn listener_payload(gateway: &(String, String), listener: Option<&str>) -> Value {
    json!({
        "object": OBJECT_GATEWAY,
        "namespace": gateway.0,
        "name": gateway.1,
        "listener": listener,
    })
}

/// One probe result, as a change payload.
///
/// The three facts a traffic claim is about -- which probe, what came
/// back, and who answered. The rest of the [`HttpProbeResult`] (headers,
/// body hash, timing, attempt count) travels in the report as evidence
/// through [`GatewayCaseComparison`]; repeating it inside every change
/// would not make the claim any more checkable.
fn probe_payload(index: usize, probe: &HttpProbeResult) -> Value {
    json!({
        "probeIndex": index,
        "status": probe.status,
        "backend": probe.backend,
    })
}

/// Every parent status entry, keyed by its identity with the namespace
/// resolved.
///
/// A [`Vec`] per key rather than one entry, so the ambiguous case (two
/// entries claiming one identity) is visible to the caller instead of
/// being silently resolved by position; see this module's "Parent
/// pairing".
fn index_parents(route: &RouteEvidence) -> BTreeMap<ParentIdentity, Vec<&RouteParentStatus>> {
    let mut index: BTreeMap<ParentIdentity, Vec<&RouteParentStatus>> = BTreeMap::new();
    for entry in &route.parents {
        let identity = ParentIdentity {
            namespace: Some(
                entry
                    .parent
                    .namespace
                    .clone()
                    .unwrap_or_else(|| route.namespace.clone()),
            ),
            name: entry.parent.name.clone(),
            section_name: entry.parent.section_name.clone(),
        };
        index.entry(identity).or_default().push(entry);
    }
    index
}

/// The listeners bound per parent `Gateway`, from an [`index_parents`]
/// index.
///
/// The key is `(namespace, name)`: attachment is to a `Gateway`, and
/// which of its listeners were bound is the finer question
/// `listener_binding_changed` answers.
fn group_by_gateway(
    parents: &BTreeMap<ParentIdentity, Vec<&RouteParentStatus>>,
) -> BTreeMap<(String, String), BTreeSet<Option<String>>> {
    let mut grouped: BTreeMap<(String, String), BTreeSet<Option<String>>> = BTreeMap::new();
    for identity in parents.keys() {
        let namespace = identity.namespace.clone().unwrap_or_default();
        grouped
            .entry((namespace, identity.name.clone()))
            .or_default()
            .insert(identity.section_name.clone());
    }
    grouped
}

/// Builds one [`SemanticChange`] with this module's fixed conventions:
/// the unattributed fixture sentinel, no `object_path`, no `origin`.
fn change(
    kind: SemanticChangeKind,
    subject: Option<String>,
    baseline: Option<Value>,
    candidate: Option<Value>,
) -> SemanticChange {
    SemanticChange {
        kind,
        fixture_id: unattributed_fixture_id(),
        object_path: None,
        subject,
        baseline,
        candidate,
        origin: None,
    }
}
