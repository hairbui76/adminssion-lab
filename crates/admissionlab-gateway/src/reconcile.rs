//! Waiting for a route to reach a stable, current status (ROADMAP Task
//! 6.4).
//!
//! [`wait_for_route_reconciliation`] polls the `HTTPRoute`, its
//! `Gateway`, and (when the `Gateway` names one) its `GatewayClass`
//! until they *settle*, and reports what it saw. It does not decide
//! whether what it saw is acceptable.
//!
//! # The convergence rule, verbatim
//!
//! ROADMAP Task 6.4 Step 3 states it as:
//!
//! > For the target parent, a route is converged when status has current
//! > `observedGeneration` and the required positive conditions are
//! > present with stable `True`/`False` values for two consecutive polls
//! > at least 250ms apart. This dampens one transient status update
//! > without imposing long sleeps.
//!
//! Each clause maps onto one piece of this module, and the mapping is
//! worth stating because every clause is a place a plausible-looking
//! simplification would be wrong:
//!
//! - **"For the target parent"** -- [`crate::conditions::RouteEvidence::parent_for`]
//!   finds *the one* `status.parents` entry the contract is about. Zero
//!   matches and several matches are both "not converged", never a
//!   guess: a route attached to two listeners can have them disagree, so
//!   taking the first would make convergence depend on list order.
//! - **"current `observedGeneration`"** -- every required condition's
//!   [`crate::conditions::ObservedCondition::freshness`] must be
//!   [`crate::conditions::ConditionFreshness::Current`]. Note that
//!   [`crate::conditions::ConditionFreshness::Unknown`] (the controller
//!   published no `observedGeneration`) is **not** current: "the status
//!   might describe this spec" is not "the status describes this spec".
//! - **"the required positive conditions are present"** --
//!   [`REQUIRED_GATEWAY_CONDITIONS`],
//!   [`REQUIRED_ROUTE_PARENT_CONDITIONS`] and
//!   [`REQUIRED_GATEWAY_CLASS_CONDITIONS`] name them.
//!   [`crate::conditions::ConditionState::Missing`] fails this clause,
//!   which is exactly why `Missing` is a distinct state.
//! - **"with stable `True`/`False` values"** --
//!   [`crate::conditions::ConditionState::is_settled`]. A settled
//!   `False` converges. That is not a bug: a route the implementation
//!   has definitively rejected has *finished reconciling*, and the
//!   evidence says so with `converged: true` and a `False` condition.
//!   Whether that is a regression depends on what the other side did,
//!   which only Task 6.9's comparator knows.
//! - **"for two consecutive polls at least 250ms apart"** -- the
//!   [`ConvergenceSnapshot`] observed by one poll must equal the one
//!   observed by the *immediately previous* poll, and the two polls must
//!   be at least [`STABILITY_INTERVAL`] apart. Implemented strictly:
//!   the recorded timestamp is always the previous poll's, never the
//!   earliest of a longer stable run, so "consecutive" and "250ms apart"
//!   describe the same pair of polls the rule does. The waiter
//!   guarantees the pair can satisfy it by sleeping at least
//!   [`STABILITY_INTERVAL`] whenever it is holding a candidate snapshot.
//!
//! What the snapshot compares is deliberately narrow: the required
//! conditions' `True`/`False` values, plus the `Gateway` and `HTTPRoute`
//! generations. Not `reason` -- a controller may refine a reason while
//! the verdict stands, and treating that as instability would spin until
//! the deadline. The generations *are* included, so a spec change
//! between two polls resets stability rather than letting a snapshot
//! from before the change confirm one from after it.
//!
//! # A timeout is evidence, not a verdict (Step 4)
//!
//! Reaching the deadline unconverged returns `Ok`, with
//! [`ReconciliationEvidence::converged`] `false`, the last observation
//! intact, and a diagnostic saying what was still missing. It is never
//! an `Err`, and it is never called a regression here.
//!
//! That restraint is the whole point. A route that times out on the
//! candidate and converged on the baseline is a regression; one that
//! times out on *both* is a broken fixture or a slow machine, not a
//! behavior change; and one that times out on the baseline and converged
//! on the candidate is an improvement. This function cannot tell those
//! apart, because it only ever sees one side. Task 6.9's comparator sees
//! both, and it is the only thing that may classify.
//!
//! # What is an `Err`
//!
//! Only two things: the cluster could not be queried at all
//! ([`GatewayError::ObservationUnavailable`]), or the `Gateway`/
//! `HTTPRoute` under contract did not exist at any point up to the
//! deadline ([`GatewayError::ObjectAbsent`]).
//!
//! The second is a consequence of §1.2's frozen
//! [`ReconciliationEvidence`], whose `gateway` and `route` are not
//! optional: an object that never existed has no evidence, and
//! manufacturing an empty [`crate::conditions::GatewayEvidence`] with a
//! made-up generation to fill the field would be precisely the
//! fabrication Global Constraint 15 forbids. A *transient* 404 during
//! polling is not this case -- it is simply retried, since the object
//! may not have been observable yet on the API server this poll reached.
//!
//! # Polling, not watching
//!
//! A `watch` would deliver every intermediate status; this polls. The
//! convergence rule is stated in terms of polls, and more importantly a
//! watch stream gives no way to say "the status has not changed for
//! 250ms" without adding a timer anyway. Backoff runs from
//! [`INITIAL_POLL_INTERVAL`] to [`MAX_POLL_INTERVAL`], doubling, so a
//! Gateway that programs in 300ms is not made to wait two seconds and a
//! Gateway that will never program does not generate a request storm.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use admissionlab_core::{ClusterHandle, Diagnostic, RedactedValue};
use async_trait::async_trait;
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::core::{ApiResource, DynamicObject};
use kube::{Api, Client, Config};
// ROADMAP Task 7.2 (frozen `admissionlab.io/result/v1beta1` result
// schema): every type this file defines that reaches a run's result
// document is embedded verbatim in it, so the schema generated from the
// result model has to describe it. Derives and `#[schemars(with = ...)]`
// restatements of what the existing `serialize_with` helpers already
// emit -- no field, name, or semantic change.
use schemars::JsonSchema;
use serde::Serialize;

use crate::conditions::{
    CONDITION_ACCEPTED, CONDITION_PROGRAMMED, CONDITION_RESOLVED_REFS, ConditionFreshness,
    ConditionState, GatewayClassEvidence, GatewayEvidence, ParentLookup, RouteEvidence,
    gateway_class_evidence, gateway_evidence, route_evidence,
};
use crate::error::GatewayError;
use crate::model::RouteContract;

/// The API group every object this module reads lives in.
pub const GATEWAY_API_GROUP: &str = "gateway.networking.k8s.io";

/// The Gateway API version this module reads.
///
/// `v1` is the standard-channel GA version carrying `GatewayClass`,
/// `Gateway` and `HTTPRoute`. Pinned rather than discovered: reading a
/// route's status from `v1beta1` on one side and `v1` on the other would
/// compare two different serializations of the same object and call the
/// difference a behavior change.
pub const GATEWAY_API_VERSION: &str = "v1";

/// The conditions a `Gateway` must publish, settled and current, before
/// its route can be called converged.
///
/// `Accepted` and `Programmed` are Gateway API v1's two standard-channel
/// `GatewayConditionType` values. `Ready` is deliberately absent: it is
/// an *experimental*-channel condition, so requiring it would make
/// convergence depend on which channel's CRDs a stack happened to
/// install.
pub const REQUIRED_GATEWAY_CONDITIONS: [&str; 2] = [CONDITION_ACCEPTED, CONDITION_PROGRAMMED];

/// The conditions a route's target parent status entry must publish,
/// settled and current, before the route can be called converged.
///
/// `Accepted` and `ResolvedRefs` are two of Gateway API v1's three
/// `RouteConditionType` values. The third, `PartiallyInvalid`, is
/// omitted on purpose: it is only published when something *is* partly
/// invalid, so requiring its presence would mean a perfectly healthy
/// route could never converge.
pub const REQUIRED_ROUTE_PARENT_CONDITIONS: [&str; 2] =
    [CONDITION_ACCEPTED, CONDITION_RESOLVED_REFS];

/// The conditions a `GatewayClass` must publish, settled, before the
/// route naming it can be called converged.
///
/// `Accepted` is the only condition Gateway API's standard channel
/// defines for a `GatewayClass`.
pub const REQUIRED_GATEWAY_CLASS_CONDITIONS: [&str; 1] = [CONDITION_ACCEPTED];

/// The minimum time between the two consecutive polls whose agreement
/// declares convergence -- the roadmap's own "at least 250ms apart".
pub const STABILITY_INTERVAL: Duration = Duration::from_millis(250);

/// The first gap between polls.
///
/// Deliberately shorter than [`STABILITY_INTERVAL`]: the *first* time a
/// converged-looking shape appears, this module wants to have seen it as
/// early as possible; only the confirming poll is required to be 250ms
/// later. Starting at 250ms would add a quarter second to every route
/// that reconciles instantly, for nothing.
pub const INITIAL_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The longest gap backoff will grow to.
///
/// Bounded so a Gateway that will never program still gets a handful of
/// looks per second rather than a request storm, and so evidence at the
/// deadline is never more than two seconds stale.
pub const MAX_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Diagnostic code: the deadline passed before the route settled.
pub const DIAGNOSTIC_TIMEOUT: &str = "gateway.reconciliation.timeout";

/// Diagnostic code: no `status.parents` entry matches the contract's
/// Gateway (and listener, if it names one).
pub const DIAGNOSTIC_PARENT_ABSENT: &str = "gateway.reconciliation.parent_absent";

/// Diagnostic code: several `status.parents` entries match the
/// contract, so which one it means is ambiguous. Fixed by setting
/// [`RouteContract::listener_name`].
pub const DIAGNOSTIC_PARENT_AMBIGUOUS: &str = "gateway.reconciliation.parent_ambiguous";

/// Diagnostic code: a required condition's `observedGeneration` is
/// older than its object's `metadata.generation`, so the status
/// describes a spec that has since changed.
pub const DIAGNOSTIC_STALE_STATUS: &str = "gateway.reconciliation.stale_status";

/// Diagnostic code: the `Gateway` names a `spec.gatewayClassName` that
/// does not exist on the cluster.
pub const DIAGNOSTIC_GATEWAY_CLASS_ABSENT: &str = "gateway.reconciliation.gateway_class_absent";

/// Everything observed while waiting for one route to reconcile.
///
/// `Serialize` only, never `Deserialize`, for the reason
/// `admissionlab_admission::AdmissionOutcome` documents: `diagnostics`
/// holds [`Diagnostic`], which `admissionlab-core` deliberately
/// implements `Serialize` but not `Deserialize` for, and this evidence
/// is captured once from a live cluster and only ever serialized
/// outward.
///
/// No `Default`: an evidence type with one can be fabricated by
/// accident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationEvidence {
    /// The `GatewayClass` the `Gateway` named, if it named one and that
    /// class exists. `None` covers both "the Gateway declared no
    /// `spec.gatewayClassName`" and "it named one that is not on the
    /// cluster" -- the second also raises a
    /// [`DIAGNOSTIC_GATEWAY_CLASS_ABSENT`] diagnostic, which is how the
    /// two stay distinguishable.
    pub gateway_class: Option<GatewayClassEvidence>,
    /// The `Gateway` under contract, as last observed.
    pub gateway: GatewayEvidence,
    /// The `HTTPRoute` under contract, as last observed.
    pub route: RouteEvidence,
    /// Wall-clock time from the first poll to the last, measured with
    /// [`Instant`] so it is monotonic.
    #[serde(serialize_with = "serialize_duration_millis")]
    #[schemars(with = "u64")]
    pub elapsed: Duration,
    /// Whether the convergence rule was satisfied. `false` at a timeout
    /// -- and `true` for a route the implementation settled on
    /// *rejecting*. See this module's documentation.
    pub converged: bool,
    /// What was noteworthy about this wait: a timeout, an absent or
    /// ambiguous parent, a stale status, a missing `GatewayClass`.
    /// Empty when the route converged cleanly.
    pub diagnostics: Vec<Diagnostic>,
}

/// Where [`wait_for_route_reconciliation_with_source`] reads objects
/// from.
///
/// A trait rather than a bare `kube::Client` parameter because the
/// interesting behavior of this module is a *sequence* of observations
/// over time -- transient flips, staleness that clears, a status that
/// never settles -- and scripting those against a real HTTP mock would
/// bury the rule under request bookkeeping.
/// `tests/reconcile_unit.rs` drives the rule through a scripted
/// implementation of this trait, *and* separately drives the real
/// [`KubeGatewayStatusSource`] through a `tower_test::mock`-backed
/// client, so neither the logic nor the wire format is only asserted by
/// the other's stand-in.
///
/// Every method returns `Ok(None)` for "the object is not there",
/// distinct from an `Err` for "the cluster could not be asked".
#[async_trait]
pub trait GatewayStatusSource: Send + Sync {
    /// The cluster's own name, used only to label errors and
    /// diagnostics.
    fn cluster_name(&self) -> &str;

    /// Reads one `Gateway`, or `Ok(None)` if it does not exist.
    ///
    /// # Errors
    ///
    /// [`GatewayError::ObservationUnavailable`] if the cluster could not
    /// be queried at all.
    async fn get_gateway(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError>;

    /// Reads one (cluster-scoped) `GatewayClass`, or `Ok(None)`.
    ///
    /// # Errors
    ///
    /// See [`GatewayStatusSource::get_gateway`].
    async fn get_gateway_class(
        &self,
        name: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError>;

    /// Reads one `HTTPRoute`, or `Ok(None)`.
    ///
    /// # Errors
    ///
    /// See [`GatewayStatusSource::get_gateway`].
    async fn get_route(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError>;
}

/// Polls `cluster` until `contract`'s route reconciles, or until
/// `deadline`.
///
/// See this module's documentation for the convergence rule and for why
/// a timeout is `Ok` with `converged: false` rather than an error.
///
/// # Errors
///
/// Returns [`GatewayError::ObservationUnavailable`] if `cluster`'s
/// kubeconfig could not be turned into a usable client or the cluster
/// could not be queried, and [`GatewayError::ObjectAbsent`] if the
/// contract's `Gateway` or `HTTPRoute` was never observed to exist.
pub async fn wait_for_route_reconciliation(
    cluster: &ClusterHandle,
    contract: &RouteContract,
    deadline: Instant,
) -> Result<ReconciliationEvidence, GatewayError> {
    let client =
        client_for(cluster)
            .await
            .map_err(|source| GatewayError::ObservationUnavailable {
                cluster: cluster.spec.name.clone(),
                object: format!(
                    "HTTPRoute {}/{}",
                    contract.route_namespace, contract.route_name
                ),
                reason: source.to_string(),
            })?;
    wait_for_route_reconciliation_with_client(client, &cluster.spec.name, contract, deadline).await
}

/// [`wait_for_route_reconciliation`]'s offline-testable seam: the same
/// wait, driven by an already-built `client`.
///
/// The same split -- and for the same reason -- as
/// `admissionlab_fixtures::execute::dry_run_create_with_client` and
/// [`crate::apply::apply_gateway_plan_with_client`]: building a client
/// from an on-disk kubeconfig has nowhere to insert a fake, everything
/// after it does.
///
/// # Errors
///
/// See [`wait_for_route_reconciliation`].
pub async fn wait_for_route_reconciliation_with_client(
    client: Client,
    cluster_name: &str,
    contract: &RouteContract,
    deadline: Instant,
) -> Result<ReconciliationEvidence, GatewayError> {
    let source = KubeGatewayStatusSource::new(client, cluster_name);
    wait_for_route_reconciliation_with_source(&source, contract, deadline).await
}

/// The convergence rule itself, over an arbitrary
/// [`GatewayStatusSource`].
///
/// This is where the polling loop, the backoff, and the two-poll
/// stability window live; the two wrappers above only decide where
/// objects are read from.
///
/// # Errors
///
/// See [`wait_for_route_reconciliation`].
pub async fn wait_for_route_reconciliation_with_source(
    source: &dyn GatewayStatusSource,
    contract: &RouteContract,
    deadline: Instant,
) -> Result<ReconciliationEvidence, GatewayError> {
    let started = Instant::now();
    let mut interval = INITIAL_POLL_INTERVAL;
    // The immediately previous poll's snapshot and the instant it was
    // taken. Replaced (never merged) on every poll, so "consecutive"
    // and "at least 250ms apart" always describe the same pair of polls
    // the roadmap's rule does.
    let mut previous: Option<(ConvergenceSnapshot, Instant)> = None;

    let (observation, converged) = loop {
        let observation = observe(source, contract).await?;
        let now = Instant::now();
        let snapshot = convergence_snapshot(contract, &observation);

        let converged = match (&snapshot, &previous) {
            (Some(current), Some((earlier, at))) => {
                current == earlier && now.duration_since(*at) >= STABILITY_INTERVAL
            }
            _ => false,
        };
        previous = snapshot.map(|snapshot| (snapshot, now));

        if converged || now >= deadline {
            break (observation, converged);
        }

        // Hold a candidate snapshot? Then the next poll is the
        // confirming one, and it must be at least STABILITY_INTERVAL
        // later -- otherwise the pair can never satisfy the rule and the
        // loop would spin re-observing the same thing.
        let mut sleep_for = if previous.is_some() {
            interval.max(STABILITY_INTERVAL)
        } else {
            interval
        };
        // Never sleep past the deadline: one final poll happens right at
        // it. A truncated sleep can never *cause* a false convergence,
        // because the 250ms check above is what grants it.
        sleep_for = sleep_for.min(deadline.saturating_duration_since(now));
        if sleep_for.is_zero() {
            break (observation, converged);
        }
        tokio::time::sleep(sleep_for).await;
        interval = (interval * 2).min(MAX_POLL_INTERVAL);
    };

    finish(source, contract, observation, started.elapsed(), converged)
}

/// What one poll saw. `gateway`/`route` are `Option` because an object
/// may legitimately not exist yet mid-poll.
struct Observation {
    gateway_class: Option<GatewayClassEvidence>,
    gateway_class_named: Option<String>,
    gateway: Option<GatewayEvidence>,
    route: Option<RouteEvidence>,
}

/// Reads and normalizes the route, its Gateway, and (only when the
/// Gateway names one) its `GatewayClass`.
///
/// The `GatewayClass` is read from the same poll's `Gateway`, never from
/// the contract or from a cached earlier read, so the class observed is
/// always the one the Gateway currently names.
async fn observe(
    source: &dyn GatewayStatusSource,
    contract: &RouteContract,
) -> Result<Observation, GatewayError> {
    let route = source
        .get_route(&contract.route_namespace, &contract.route_name)
        .await?
        .map(|object| route_evidence(&object))
        .transpose()?;
    let gateway = source
        .get_gateway(&contract.gateway_namespace, &contract.gateway_name)
        .await?
        .map(|object| gateway_evidence(&object))
        .transpose()?;

    let gateway_class_named = gateway
        .as_ref()
        .and_then(|gateway| gateway.gateway_class_name.clone());
    let gateway_class = match &gateway_class_named {
        Some(name) => source
            .get_gateway_class(name)
            .await?
            .map(|object| gateway_class_evidence(&object))
            .transpose()?,
        None => None,
    };

    Ok(Observation {
        gateway_class,
        gateway_class_named,
        gateway,
        route,
    })
}

/// The narrow projection of an observation that two consecutive polls
/// must agree on.
///
/// Built only when the whole convergence rule's *content* clauses hold
/// (required conditions present, settled, and current); `None` means
/// "not in a converged shape right now", which resets stability. See
/// this module's documentation for what is and is not compared, and why
/// `reason` is not.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConvergenceSnapshot {
    gateway_class: Option<(String, ConditionState)>,
    gateway_generation: i64,
    gateway: BTreeMap<String, ConditionState>,
    route_generation: i64,
    route_parent: BTreeMap<String, ConditionState>,
}

/// The snapshot for `observation`, or `None` if it is not currently in a
/// converged shape.
fn convergence_snapshot(
    contract: &RouteContract,
    observation: &Observation,
) -> Option<ConvergenceSnapshot> {
    let gateway = observation.gateway.as_ref()?;
    let route = observation.route.as_ref()?;
    let ParentLookup::Found(parent) = route.parent_for(contract) else {
        // Zero matches or several: both mean this module cannot say
        // which entry the contract is about, so it says nothing.
        return None;
    };

    let gateway_conditions = settled_current_conditions(
        &REQUIRED_GATEWAY_CONDITIONS,
        gateway.generation,
        |type_name| gateway.condition(type_name),
    )?;
    let route_conditions = settled_current_conditions(
        &REQUIRED_ROUTE_PARENT_CONDITIONS,
        route.generation,
        |type_name| parent.condition(type_name),
    )?;

    // A Gateway that names a class it does not have is not converged:
    // the class is a prerequisite the Gateway itself declared, and
    // ignoring its absence would let a route "converge" against a
    // Gateway nothing will ever program.
    let gateway_class = match (&observation.gateway_class_named, &observation.gateway_class) {
        (None, _) => None,
        (Some(_), None) => return None,
        (Some(_), Some(class)) => {
            if !REQUIRED_GATEWAY_CLASS_CONDITIONS.contains(&class.accepted.type_name.as_str())
                || !class.accepted.state.is_settled()
            {
                return None;
            }
            // A `GatewayClass` under test is never edited during a run,
            // and §1.2 gives `GatewayClassEvidence` no generation to
            // compare against, so its freshness is deliberately not part
            // of this clause -- only that it has settled.
            Some((class.name.clone(), class.accepted.state))
        }
    };

    Some(ConvergenceSnapshot {
        gateway_class,
        gateway_generation: gateway.generation,
        gateway: gateway_conditions,
        route_generation: route.generation,
        route_parent: route_conditions,
    })
}

/// The `True`/`False` value of every condition in `required`, or `None`
/// if any of them is missing, unsettled, or not current for
/// `generation`.
fn settled_current_conditions(
    required: &[&str],
    generation: i64,
    lookup: impl Fn(&str) -> crate::conditions::ObservedCondition,
) -> Option<BTreeMap<String, ConditionState>> {
    let mut states = BTreeMap::new();
    for type_name in required {
        let condition = lookup(type_name);
        if !condition.state.is_settled()
            || condition.freshness(generation) != ConditionFreshness::Current
        {
            return None;
        }
        states.insert((*type_name).to_string(), condition.state);
    }
    Some(states)
}

/// Turns the last observation into [`ReconciliationEvidence`], raising
/// [`GatewayError::ObjectAbsent`] if the frozen non-optional fields
/// cannot be filled honestly.
fn finish(
    source: &dyn GatewayStatusSource,
    contract: &RouteContract,
    observation: Observation,
    elapsed: Duration,
    converged: bool,
) -> Result<ReconciliationEvidence, GatewayError> {
    let absent = |object: String| GatewayError::ObjectAbsent {
        cluster: source.cluster_name().to_string(),
        object,
    };
    let gateway = observation.gateway.ok_or_else(|| {
        absent(format!(
            "Gateway {}/{}",
            contract.gateway_namespace, contract.gateway_name
        ))
    })?;
    let route = observation.route.ok_or_else(|| {
        absent(format!(
            "HTTPRoute {}/{}",
            contract.route_namespace, contract.route_name
        ))
    })?;

    let diagnostics = reconciliation_diagnostics(
        contract,
        &gateway,
        &route,
        observation.gateway_class_named.as_deref(),
        observation.gateway_class.is_some(),
        elapsed,
        converged,
    );

    Ok(ReconciliationEvidence {
        gateway_class: observation.gateway_class,
        gateway,
        route,
        elapsed,
        converged,
        diagnostics,
    })
}

/// Everything noteworthy about how this wait ended.
///
/// Computed once, from the *final* observation, rather than accumulated
/// per poll: a route that flickered through three transient states on
/// its way to converging has nothing wrong with it, and emitting a
/// diagnostic for each intermediate poll would bury a real finding under
/// normal reconciliation noise.
///
/// Split out of [`finish`] so each diagnostic's condition reads as one
/// named case rather than a branch inside a long constructor.
fn reconciliation_diagnostics(
    contract: &RouteContract,
    gateway: &GatewayEvidence,
    route: &RouteEvidence,
    gateway_class_named: Option<&str>,
    gateway_class_found: bool,
    elapsed: Duration,
    converged: bool,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let public = |value: &str| RedactedValue::Public(value.to_string());

    if let (Some(name), false) = (gateway_class_named, gateway_class_found) {
        diagnostics.push(Diagnostic {
            code: DIAGNOSTIC_GATEWAY_CLASS_ABSENT.to_string(),
            message: format!(
                "Gateway {} names spec.gatewayClassName {name:?}, but no GatewayClass with that \
                 name exists on this cluster",
                gateway.identity
            ),
            context: BTreeMap::from([
                ("contract".to_string(), public(&contract.id)),
                ("gateway".to_string(), public(&gateway.identity.to_string())),
                ("gatewayClassName".to_string(), public(name)),
            ]),
        });
    }

    if let Some(diagnostic) = parent_diagnostic(contract, gateway, route) {
        diagnostics.push(diagnostic);
    }

    if !converged {
        diagnostics.push(Diagnostic {
            code: DIAGNOSTIC_TIMEOUT.to_string(),
            message: format!(
                "route contract {:?} did not reach a stable, current status within {}ms; this is \
                 recorded as evidence, not as a regression -- only a baseline/candidate \
                 comparison can decide what it means",
                contract.id,
                elapsed.as_millis()
            ),
            context: BTreeMap::from([
                ("contract".to_string(), public(&contract.id)),
                (
                    "elapsedMillis".to_string(),
                    public(&elapsed.as_millis().to_string()),
                ),
            ]),
        });
    }

    diagnostics
}

/// The one diagnostic (if any) about how the contract's target parent
/// status entry was found: absent, ambiguous, or found but stale.
///
/// At most one, and in that priority order, because they are three
/// answers to the same question -- "which entry is this contract about,
/// and is what it says current?" -- and only the first that applies
/// tells the reader anything they can act on.
fn parent_diagnostic(
    contract: &RouteContract,
    gateway: &GatewayEvidence,
    route: &RouteEvidence,
) -> Option<Diagnostic> {
    let public = |value: &str| RedactedValue::Public(value.to_string());

    match route.parent_for(contract) {
        ParentLookup::Found(parent) => {
            let stale = REQUIRED_GATEWAY_CONDITIONS
                .iter()
                .map(|type_name| gateway.condition(type_name).freshness(gateway.generation))
                .chain(
                    REQUIRED_ROUTE_PARENT_CONDITIONS
                        .iter()
                        .map(|type_name| parent.condition(type_name).freshness(route.generation)),
                )
                .fold(ConditionFreshness::Current, ConditionFreshness::merge);
            if stale == ConditionFreshness::Stale {
                return Some(Diagnostic {
                    code: DIAGNOSTIC_STALE_STATUS.to_string(),
                    message: format!(
                        "a required condition's observedGeneration is older than its object's \
                         metadata.generation (Gateway {} at generation {}, HTTPRoute {}/{} at \
                         generation {}), so the published status describes a spec that has since \
                         changed",
                        gateway.identity,
                        gateway.generation,
                        route.namespace,
                        route.name,
                        route.generation
                    ),
                    context: BTreeMap::from([
                        ("contract".to_string(), public(&contract.id)),
                        ("gateway".to_string(), public(&gateway.identity.to_string())),
                    ]),
                });
            }
            None
        }
        ParentLookup::Absent => Some(Diagnostic {
            code: DIAGNOSTIC_PARENT_ABSENT.to_string(),
            message: format!(
                "HTTPRoute {}/{} has no status.parents entry for Gateway {}/{}{}",
                route.namespace,
                route.name,
                contract.gateway_namespace,
                contract.gateway_name,
                contract
                    .listener_name
                    .as_ref()
                    .map_or_else(String::new, |listener| format!(" listener {listener:?}"))
            ),
            context: BTreeMap::from([
                ("contract".to_string(), public(&contract.id)),
                (
                    "observedParents".to_string(),
                    public(&route.parents.len().to_string()),
                ),
            ]),
        }),
        ParentLookup::Ambiguous(count) => Some(Diagnostic {
            code: DIAGNOSTIC_PARENT_AMBIGUOUS.to_string(),
            message: format!(
                "HTTPRoute {}/{} has {count} status.parents entries matching Gateway {}/{}, and \
                 this contract does not say which listener it means; set listenerName",
                route.namespace, route.name, contract.gateway_namespace, contract.gateway_name
            ),
            context: BTreeMap::from([
                ("contract".to_string(), public(&contract.id)),
                ("matchingParents".to_string(), public(&count.to_string())),
            ]),
        }),
    }
}

/// The production [`GatewayStatusSource`]: reads Gateway API v1 objects
/// through a `kube::Client`'s dynamic API.
pub struct KubeGatewayStatusSource {
    client: Client,
    cluster_name: String,
}

impl KubeGatewayStatusSource {
    /// Builds a source reading through `client`. `cluster_name` only
    /// ever labels errors and diagnostics.
    #[must_use]
    pub fn new(client: Client, cluster_name: &str) -> Self {
        Self {
            client,
            cluster_name: cluster_name.to_string(),
        }
    }

    /// Reads one object, mapping a 404 to `Ok(None)`.
    async fn get(
        &self,
        api: Api<DynamicObject>,
        described: &str,
        name: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError> {
        let unavailable = |reason: String| GatewayError::ObservationUnavailable {
            cluster: self.cluster_name.clone(),
            object: described.to_string(),
            reason,
        };
        // `get_opt` is `kube`'s own "404 is not an error" read, so a
        // not-yet-created object never has to be recognized by parsing an
        // error message.
        let object = api
            .get_opt(name)
            .await
            .map_err(|source| unavailable(source.to_string()))?;
        object
            .map(|object| {
                serde_json::to_value(object).map_err(|source| unavailable(source.to_string()))
            })
            .transpose()
    }
}

/// The `ApiResource` for one Gateway API v1 kind.
///
/// The plurals are **not guessed**: `gateways`, `httproutes` and
/// `gatewayclasses` are the `spec.names.plural` values in Gateway API's
/// own published CRD manifests. That distinction matters because
/// `admissionlab_fixtures::resources` exists precisely to avoid
/// `ApiResource::from_gvk`'s plural heuristic, and Global Constraint 15
/// forbids guessing -- but the heuristic's failure mode is *unknown*
/// CRDs, and these three are a fixed, upstream-specified set this crate
/// names by hand rather than infers. `crate::apply` still resolves
/// through real discovery, because there the documents are arbitrary
/// user manifests of kinds this crate has never heard of.
///
/// Reading through a fixed descriptor also avoids running discovery
/// (four extra HTTP requests) on every poll of every route.
fn gateway_api_resource(kind: &str, plural: &str) -> ApiResource {
    ApiResource {
        group: GATEWAY_API_GROUP.to_string(),
        version: GATEWAY_API_VERSION.to_string(),
        api_version: format!("{GATEWAY_API_GROUP}/{GATEWAY_API_VERSION}"),
        kind: kind.to_string(),
        plural: plural.to_string(),
    }
}

#[async_trait]
impl GatewayStatusSource for KubeGatewayStatusSource {
    fn cluster_name(&self) -> &str {
        &self.cluster_name
    }

    async fn get_gateway(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError> {
        let resource = gateway_api_resource("Gateway", "gateways");
        self.get(
            Api::namespaced_with(self.client.clone(), namespace, &resource),
            &format!("Gateway {namespace}/{name}"),
            name,
        )
        .await
    }

    async fn get_gateway_class(
        &self,
        name: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError> {
        let resource = gateway_api_resource("GatewayClass", "gatewayclasses");
        self.get(
            Api::all_with(self.client.clone(), &resource),
            &format!("GatewayClass {name}"),
            name,
        )
        .await
    }

    async fn get_route(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError> {
        let resource = gateway_api_resource("HTTPRoute", "httproutes");
        self.get(
            Api::namespaced_with(self.client.clone(), namespace, &resource),
            &format!("HTTPRoute {namespace}/{name}"),
            name,
        )
        .await
    }
}

/// Serializes a [`Duration`] as a plain integer number of milliseconds,
/// mirroring `admissionlab_admission::outcome`'s own helper and
/// `admissionlab_spec::model`'s `duration_millis`, including the
/// saturating overflow handling.
fn serialize_duration_millis<S: serde::Serializer>(
    value: &Duration,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let millis = u64::try_from(value.as_millis()).unwrap_or(u64::MAX);
    serializer.serialize_u64(millis)
}

/// Builds a `kube::Client` for `cluster` from its own isolated
/// kubeconfig. See [`crate::apply`]'s own copy for why this function is
/// duplicated rather than shared across crates.
async fn client_for(cluster: &ClusterHandle) -> Result<Client, kube::Error> {
    let kubeconfig = Kubeconfig::read_from(&cluster.kubeconfig)?;
    let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default()).await?;
    Client::try_from(config)
}
