//! Normalizing what a Gateway API implementation published about an
//! object (ROADMAP Task 6.3).
//!
//! Every type here answers one question honestly: *what did this cluster
//! actually say?* Nothing here decides whether the answer is good, and
//! nothing here compares two answers -- Task 6.4 waits, Task 6.9
//! compares.
//!
//! # The four states, and why `Missing` is one of them
//!
//! Kubernetes `metav1.Condition.status` has exactly three values --
//! `"True"`, `"False"`, `"Unknown"` -- and Gateway API's CRDs enforce
//! that enumeration, so those are the only three a controller can
//! publish. [`ConditionState`] adds a fourth, [`ConditionState::Missing`],
//! for the case Kubernetes represents by the condition simply not being
//! in the list: a controller that has published `Accepted` but not yet
//! `Programmed` has said *nothing* about programming.
//!
//! Those four are genuinely four different facts, and Global Constraint
//! 15 is why none of them may be folded into another:
//!
//! - `True`/`False` -- the controller reached a verdict.
//! - `Unknown` -- the controller looked and cannot yet tell (Gateway
//!   API's own initial `Pending` state uses this).
//! - `Missing` -- the controller has not spoken about this condition at
//!   all.
//!
//! Collapsing `Missing` into `False` would report "the implementation
//! rejected this" for a route the implementation has not read yet, which
//! is the most likely way this whole phase could produce a confident,
//! well-formed, entirely fictional regression.
//!
//! # Order is never assumed (Step 1)
//!
//! `status.conditions` is a list, and nothing in Kubernetes fixes its
//! order -- a controller may append, re-sort, or rewrite it wholesale.
//! [`observed_conditions`] therefore keys a [`BTreeMap`] on each entry's
//! `type`, so `Accepted`, `ResolvedRefs` and `Programmed` are found by
//! name regardless of position. `testdata/objects/gateway-status/httproute-accepted.yaml`
//! deliberately writes `ResolvedRefs` before `Accepted` so a
//! position-dependent reader fails a test rather than a run.
//!
//! # `reason` is kept; `message` is not (Step 2)
//!
//! [`ObservedCondition::reason`] is preserved. A `reason` is a
//! `CamelCase` token from a closed, API-documented set
//! (`AddressNotAssigned`, `BackendNotFound`, `NoMatchingParent`, ...) --
//! it is a machine-readable classification and a legitimate thing for a
//! report to show and for a human to compare across sides.
//!
//! `message` is **deliberately not stored anywhere in this module's
//! types**, which is a stronger position than "stored but not compared",
//! and is chosen on purpose:
//!
//! - ROADMAP Task 6.3 Step 2 says free-form message text must not become
//!   a pass/fail contract. A field that exists on an evidence type will
//!   eventually be walked by something structural -- a generic
//!   field-by-field comparator, a golden snapshot, a serialized report
//!   someone diffs -- and at that moment it *is* a contract, without
//!   anyone deciding it should be.
//! - Message text is exactly the part of a status that legitimately
//!   changes between two implementation versions with no behavioral
//!   change at all ("Resource programmed, assigned to service in the
//!   \"gateway-lab\" namespace" is one Istio release's wording), so
//!   comparing it would manufacture regressions -- the opposite of what
//!   Global Constraint 7's determinism requirement is for.
//! - It is not lost evidence. The object this was parsed from is still
//!   in the ephemeral cluster while it exists, and a caller that wants
//!   the message has the raw `serde_json::Value` it passed in.
//!
//! §1.2's registry freezes [`ObservedCondition`] at exactly four fields
//! and does permit later tasks to add more; this module is recording
//! that not adding a fifth here was a decision, not an oversight.
//!
//! # Staleness is computed, not stored (Step 3)
//!
//! `observedGeneration < metadata.generation` means the controller's
//! status describes an older version of the object's spec. That is a
//! *relationship between two numbers*, so it is exposed as
//! [`ObservedCondition::freshness`] -- a method taking the object's
//! current generation -- rather than as a fifth [`ConditionState`]
//! variant or a stored boolean.
//!
//! A `ConditionState::Stale` variant was the alternative and is worse in
//! two ways. It would destroy information: a stale-but-`True` condition
//! and a stale-but-`False` one are different observations, and one
//! variant cannot hold both. And it would make the state depend on data
//! that does not live in the condition -- the object's `generation` --
//! so the same [`ObservedCondition`] value would have to mean different
//! things depending on where it was read from. Keeping it a method makes
//! the dependency explicit at every call site.
//!
//! [`ConditionFreshness`] has three variants, not two, because "the
//! controller published no `observedGeneration`" is a real and common
//! case (the field is optional on `metav1.Condition`, and Gateway API's
//! initial `Pending` conditions omit it) and it is not evidence of
//! freshness *or* of staleness.
//!
//! # What this module does not normalize
//!
//! `Gateway.status.listeners[]` (per-listener `Accepted`, `Programmed`,
//! `ResolvedRefs`, `Conflicted`) and `Gateway.status.addresses[]` are
//! real, useful status and are deliberately left unparsed: §1.2's
//! registry fixes [`GatewayEvidence`] at identity, conditions and
//! generation, and Task 6.4's convergence rule is stated over the
//! object-level conditions only. The golden fixtures keep those fields
//! present rather than trimming them away, so whoever adds listener
//! evidence later has a realistic shape to parse and can see that
//! ignoring them was a choice.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::error::GatewayError;
use crate::model::{GatewayIdentity, RouteContract};

/// The Gateway API condition type meaning "the implementation
/// recognized this object and took responsibility for it".
///
/// Published on `GatewayClass` (`GatewayClassConditionStatusAccepted`),
/// on `Gateway` (`GatewayConditionAccepted`), and on each of a route's
/// parent statuses (`RouteConditionAccepted`) -- one name, three
/// objects, which is why it is a single constant.
pub const CONDITION_ACCEPTED: &str = "Accepted";

/// The Gateway API condition type meaning "this `Gateway` has been
/// configured into the data plane and is (as far as the implementation
/// can tell) able to serve traffic" -- upstream
/// `GatewayConditionProgrammed`.
///
/// Distinct from `Accepted` in exactly the way this phase cares about:
/// a `Gateway` can be a completely valid object (`Accepted: True`) that
/// no data plane has been told about (`Programmed: False`).
pub const CONDITION_PROGRAMMED: &str = "Programmed";

/// The Gateway API condition type meaning "every reference this object
/// makes resolved to something that exists and is permitted" -- upstream
/// `RouteConditionResolvedRefs`, published per route parent (and, for
/// completeness, per Gateway listener, which this module does not
/// currently read).
pub const CONDITION_RESOLVED_REFS: &str = "ResolvedRefs";

/// What a controller said about one condition -- including having said
/// nothing.
///
/// The three Kubernetes values keep their own wire spelling (`"True"`,
/// `"False"`, `"Unknown"`, capitalized) rather than being renamed to
/// this project's usual `snake_case`: these are the exact strings a user
/// sees in `kubectl get gateway -o yaml`, and a report that spelled them
/// differently from the cluster it read them from would be needlessly
/// harder to check by hand. `"Missing"` follows the same convention for
/// consistency even though Kubernetes has no such value. Every variant's
/// wire tag is pinned with an explicit `#[serde(rename)]` so renaming a
/// Rust variant can never silently change the report contract -- the
/// same rule `admissionlab_admission::AdmissionDecision` follows.
///
/// No `Default`: there is no defensible default observation, and a
/// derived one would make "we did not look" indistinguishable from a
/// real answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum ConditionState {
    /// The controller published this condition with `status: "True"`.
    #[serde(rename = "True")]
    True,
    /// The controller published this condition with `status: "False"`.
    #[serde(rename = "False")]
    False,
    /// The controller published this condition with `status: "Unknown"`
    /// -- it looked and cannot yet tell.
    #[serde(rename = "Unknown")]
    Unknown,
    /// The condition is not in the object's condition list at all. The
    /// controller has said nothing about it. See this module's
    /// documentation for why this is never folded into
    /// [`ConditionState::False`].
    #[serde(rename = "Missing")]
    Missing,
}

impl ConditionState {
    /// Parses a `metav1.Condition.status` value.
    ///
    /// Returns `None` for anything other than the three values the
    /// Kubernetes API defines. That is a hard rejection rather than a
    /// fallback to [`ConditionState::Unknown`], because `Unknown` is a
    /// *statement a controller made* and mapping an unparseable value
    /// onto it would put words in the controller's mouth (Global
    /// Constraint 15). Gateway API's CRDs carry
    /// `+kubebuilder:validation:Enum=True;False;Unknown` on this field,
    /// so a real API server cannot store any other value -- which makes
    /// rejecting it a check on this project's own parsing rather than a
    /// restriction on real clusters.
    #[must_use]
    pub fn parse(status: &str) -> Option<Self> {
        match status {
            "True" => Some(Self::True),
            "False" => Some(Self::False),
            "Unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// Whether this is a settled verdict (`True` or `False`) rather than
    /// an absence of one.
    ///
    /// Named for what Task 6.4's convergence rule asks for -- "present
    /// with stable True/False values" -- so the rule and the predicate
    /// that implements it use the same words. Note that a settled
    /// `False` *is* settled: a route the implementation has definitively
    /// rejected has finished reconciling, and calling that a failure is
    /// the comparator's job, not the waiter's.
    #[must_use]
    pub const fn is_settled(self) -> bool {
        matches!(self, Self::True | Self::False)
    }
}

/// Whether a condition's `observedGeneration` describes the object as it
/// is now.
///
/// See this module's "Staleness is computed, not stored" section for why
/// this is the answer to a question rather than a stored field, and why
/// there are three variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ConditionFreshness {
    /// The condition's `observedGeneration` equals the object's current
    /// `metadata.generation`: the controller has seen this spec.
    #[serde(rename = "current")]
    Current,
    /// The condition's `observedGeneration` is *older* than the object's
    /// `metadata.generation`: the status describes a spec that has since
    /// been changed.
    #[serde(rename = "stale")]
    Stale,
    /// Freshness could not be determined: the condition published no
    /// `observedGeneration` (the field is optional), or it is absent
    /// entirely, or the two generations disagree in a way that means one
    /// of them is not what it appears to be.
    #[serde(rename = "unknown")]
    Unknown,
}

impl ConditionFreshness {
    /// The more alarming of two freshness answers: `Stale` beats
    /// `Unknown` beats `Current`.
    ///
    /// Used to fold several required conditions into one answer. `Stale`
    /// wins because it is a *positive* finding -- something is
    /// definitely out of date -- while `Unknown` is only an absence of
    /// information; reporting "unknown" when one condition is provably
    /// stale would hide the stronger fact.
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Stale, _) | (_, Self::Stale) => Self::Stale,
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            (Self::Current, Self::Current) => Self::Current,
        }
    }
}

/// One condition as a controller published it, normalized.
///
/// No `Default`, and no way to build a "blank" one except
/// [`ObservedCondition::missing`], which is explicit about what it
/// means. This mirrors `admissionlab_admission::trace::TraceEvidence`'s
/// own rule: an evidence type with a `Default` is an evidence type that
/// can be fabricated by accident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedCondition {
    /// The condition's `type`, verbatim (for example `"Accepted"`).
    pub type_name: String,
    /// What the controller said, or [`ConditionState::Missing`] if it
    /// said nothing.
    pub state: ConditionState,
    /// The condition's `reason`, a `CamelCase` token from the API's own
    /// closed set. `None` when the condition is missing, or when it was
    /// published without one. See this module's documentation for why
    /// `reason` is kept and `message` is not.
    pub reason: Option<String>,
    /// The `metadata.generation` this condition was set based upon.
    /// `None` when the condition is missing, or when the controller
    /// published none -- never fabricated as the object's current
    /// generation, which would silently turn every unfresh status into a
    /// fresh-looking one.
    pub observed_generation: Option<i64>,
}

impl ObservedCondition {
    /// The observation for a condition type that is not in an object's
    /// condition list at all.
    ///
    /// Deliberately a named constructor rather than a `Default`: the
    /// caller has to say the word "missing", and the value it produces
    /// carries no reason and no generation, because there is no
    /// condition to have carried them.
    #[must_use]
    pub fn missing(type_name: &str) -> Self {
        Self {
            type_name: type_name.to_string(),
            state: ConditionState::Missing,
            reason: None,
            observed_generation: None,
        }
    }

    /// Whether this condition's status describes `object_generation`.
    ///
    /// - A published `observedGeneration` equal to `object_generation`
    ///   is [`ConditionFreshness::Current`].
    /// - A *smaller* one is [`ConditionFreshness::Stale`] -- ROADMAP
    ///   Task 6.3 Step 3's rule exactly.
    /// - No published `observedGeneration` is
    ///   [`ConditionFreshness::Unknown`], never assumed current.
    /// - A *larger* one is also [`ConditionFreshness::Unknown`]: within
    ///   one read of one object those two numbers cannot legitimately be
    ///   in that relationship, so rather than pick which of them to
    ///   believe, this reports that it cannot tell.
    ///
    /// A [`ConditionState::Missing`] condition is always
    /// [`ConditionFreshness::Unknown`] by construction, since it carries
    /// no `observedGeneration`.
    #[must_use]
    pub fn freshness(&self, object_generation: i64) -> ConditionFreshness {
        match self.observed_generation {
            Some(observed) if observed == object_generation => ConditionFreshness::Current,
            Some(observed) if observed < object_generation => ConditionFreshness::Stale,
            _ => ConditionFreshness::Unknown,
        }
    }
}

/// Which parent a route's status entry is about, as the *status* names
/// it: §1.2's canonical parent identity.
///
/// Distinct from [`GatewayIdentity`] on purpose. A `GatewayIdentity`
/// names an existing, namespaced object. A `ParentIdentity` is a
/// `ParentReference` as written, where Gateway API defines an absent
/// `namespace` to mean "the route's own namespace" -- so `None` here is
/// a real, meaningful value rather than missing data.
/// [`RouteEvidence::parent_for`] is what resolves that default, using
/// the route's namespace, rather than treating an absent namespace as a
/// wildcard that matches anything.
///
/// `group` and `kind` (also part of a `ParentReference`) are not carried:
/// §1.2 fixes this type's three fields, and a
/// [`crate::model::RouteContract`] identifies its parent as a `Gateway`
/// by construction, so there is nothing for them to disambiguate here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParentIdentity {
    /// The parent's namespace as written in `parentRef`, or `None` if
    /// the reference omitted it (meaning the route's own namespace).
    pub namespace: Option<String>,
    /// The parent's name.
    pub name: String,
    /// The listener within the parent this entry is about
    /// (`parentRef.sectionName`), or `None` if the reference named no
    /// particular listener.
    pub section_name: Option<String>,
}

/// One entry of a route's `status.parents`: what one controller said
/// about this route's attachment to one parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteParentStatus {
    /// Which parent (and which of its listeners) this entry is about.
    pub parent: ParentIdentity,
    /// The `controllerName` that wrote this entry (for example
    /// `"istio.io/gateway-controller"`), or `None` if the entry carried
    /// none.
    ///
    /// Recorded but never matched on: which controller answered is
    /// exactly the kind of thing that legitimately differs between a
    /// baseline and a candidate stack, so filtering by it would make
    /// this crate unable to observe the migration Phase 7 exists for.
    pub controller_name: Option<String>,
    /// Every condition in this entry, keyed by condition type -- so
    /// lookup never depends on list order (Step 1).
    pub conditions: BTreeMap<String, ObservedCondition>,
}

impl RouteParentStatus {
    /// This entry's condition of type `type_name`, or
    /// [`ObservedCondition::missing`] if it published none.
    ///
    /// Returns an owned value rather than an `Option<&_>` so callers
    /// cannot accidentally treat absence as a case to skip: every
    /// lookup yields an observation, and "there was no such condition"
    /// is one of the observations.
    #[must_use]
    pub fn condition(&self, type_name: &str) -> ObservedCondition {
        self.conditions
            .get(type_name)
            .cloned()
            .unwrap_or_else(|| ObservedCondition::missing(type_name))
    }
}

/// What was observed about the `GatewayClass` a `Gateway` names.
///
/// Only `Accepted` is carried, because that is the only condition
/// Gateway API's standard channel defines for a `GatewayClass` and the
/// only one §1.2's registry names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayClassEvidence {
    /// The `GatewayClass`'s name (it is cluster-scoped, so a name is a
    /// complete identity).
    pub name: String,
    /// Its `Accepted` condition, or [`ObservedCondition::missing`] if it
    /// has none yet.
    pub accepted: ObservedCondition,
}

/// What was observed about one `Gateway`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayEvidence {
    /// Which `Gateway` this is about.
    pub identity: GatewayIdentity,
    /// Every object-level condition, keyed by type (Step 1). Per-listener
    /// conditions are deliberately not included -- see this module's
    /// "What this module does not normalize" section.
    pub conditions: BTreeMap<String, ObservedCondition>,
    /// The `Gateway`'s `metadata.generation` at the moment it was read,
    /// which is what every condition's own `observedGeneration` is
    /// compared against.
    pub generation: i64,
    /// The `spec.gatewayClassName` this `Gateway` names, or `None` if it
    /// declared none.
    ///
    /// Not in §1.2's frozen field list, and added here for one concrete
    /// reason Task 6.4 states: it polls the `GatewayClass` "when the
    /// Gateway names one", and this is how it knows. Reading it from the
    /// same object read that produced the conditions is what keeps the
    /// two consistent -- a second `GET` could observe a different spec.
    pub gateway_class_name: Option<String>,
}

impl GatewayEvidence {
    /// This `Gateway`'s condition of type `type_name`, or
    /// [`ObservedCondition::missing`]. See
    /// [`RouteParentStatus::condition`] for why absence is returned as
    /// an observation rather than an `Option`.
    #[must_use]
    pub fn condition(&self, type_name: &str) -> ObservedCondition {
        self.conditions
            .get(type_name)
            .cloned()
            .unwrap_or_else(|| ObservedCondition::missing(type_name))
    }
}

/// What was observed about one `HTTPRoute`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteEvidence {
    /// The route's namespace.
    pub namespace: String,
    /// The route's name.
    pub name: String,
    /// The route's `metadata.generation` at the moment it was read.
    pub generation: i64,
    /// One entry per parent the controller processed, in the order the
    /// status listed them.
    ///
    /// A `Vec`, not a map: two entries can share a parent name and
    /// differ only by `sectionName`, and a route that has never been
    /// reconciled has no entries at all (see
    /// `testdata/objects/gateway-status/httproute-no-status.yaml`).
    /// Order is preserved rather than sorted so this stays a faithful
    /// record of what was read; nothing in this crate depends on it.
    pub parents: Vec<RouteParentStatus>,
}

/// The result of looking for the one parent status entry a contract is
/// about.
///
/// Three outcomes, not an `Option`, because "several entries match" is a
/// different fact from "none does" and must not be resolved by taking
/// the first: the two entries in
/// `testdata/objects/gateway-status/httproute-two-parents.yaml`
/// disagree, so picking one by position would flip a route between
/// converged and not depending on list order (Global Constraint 15,
/// and Global Constraint 7's determinism requirement).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParentLookup<'a> {
    /// Exactly one entry matched.
    Found(&'a RouteParentStatus),
    /// No entry matched. Normal while a route is still being
    /// reconciled, and meaningful once a deadline has passed.
    Absent,
    /// Several entries matched, and the contract does not say which one
    /// it means. The count is carried so a diagnostic can say how many.
    /// Fixed by setting [`crate::model::RouteContract::listener_name`].
    Ambiguous(usize),
}

impl RouteEvidence {
    /// Finds the single parent status entry `contract` is about.
    ///
    /// An entry matches when, all three together:
    ///
    /// - its `parentRef.name` equals
    ///   [`crate::model::RouteContract::gateway_name`];
    /// - its *effective* namespace equals
    ///   [`crate::model::RouteContract::gateway_namespace`], where
    ///   effective means `parentRef.namespace` if it was written and
    ///   this route's own namespace otherwise -- Gateway API's own
    ///   defaulting rule, resolved explicitly here rather than treating
    ///   an absent namespace as matching anything;
    /// - and, *if* the contract names a
    ///   [`crate::model::RouteContract::listener_name`], its
    ///   `parentRef.sectionName` equals it.
    ///
    /// A contract with no `listener_name` matches every entry for that
    /// Gateway, which is why a multi-listener route needs the field --
    /// see [`ParentLookup::Ambiguous`].
    #[must_use]
    pub fn parent_for(&self, contract: &RouteContract) -> ParentLookup<'_> {
        let mut matches = self.parents.iter().filter(|entry| {
            let effective_namespace = entry
                .parent
                .namespace
                .as_deref()
                .unwrap_or(self.namespace.as_str());
            entry.parent.name == contract.gateway_name
                && effective_namespace == contract.gateway_namespace
                && contract.listener_name.as_ref().is_none_or(|listener| {
                    entry.parent.section_name.as_deref() == Some(listener.as_str())
                })
        });

        match (matches.next(), matches.count()) {
            (None, _) => ParentLookup::Absent,
            (Some(entry), 0) => ParentLookup::Found(entry),
            (Some(_), remaining) => ParentLookup::Ambiguous(remaining + 1),
        }
    }
}

/// Normalizes a `Gateway` object as read from a cluster.
///
/// `object` is the whole object (a `DynamicObject`'s JSON, or a parsed
/// manifest), not just its `status`.
///
/// # Errors
///
/// Returns [`GatewayError::MalformedStatus`] if `object` is not a JSON
/// object, has no `metadata.name`/`metadata.namespace`, has no integer
/// `metadata.generation`, or has a `status.conditions` entry this module
/// cannot read (see [`observed_conditions`]). A `Gateway` with no
/// `status` at all is **not** an error -- it is a `Gateway` no controller
/// has written to yet, and it parses with an empty condition map, which
/// every lookup then reports as [`ConditionState::Missing`].
pub fn gateway_evidence(object: &serde_json::Value) -> Result<GatewayEvidence, GatewayError> {
    let namespace = required_metadata_string(object, "Gateway", "metadata.namespace")?;
    let name = required_metadata_string(object, "Gateway", "metadata.name")?;
    let described = format!("Gateway {namespace}/{name}");
    let generation = required_generation(object, "Gateway", &described)?;

    Ok(GatewayEvidence {
        identity: GatewayIdentity { namespace, name },
        conditions: observed_conditions(
            object.pointer("/status/conditions"),
            "Gateway",
            &described,
        )?,
        generation,
        gateway_class_name: object
            .pointer("/spec/gatewayClassName")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    })
}

/// Normalizes a `GatewayClass` object as read from a cluster.
///
/// # Errors
///
/// Returns [`GatewayError::MalformedStatus`] if `object` has no
/// `metadata.name` or an unreadable `status.conditions` entry. A
/// `GatewayClass` with no `Accepted` condition is not an error -- its
/// [`GatewayClassEvidence::accepted`] is
/// [`ObservedCondition::missing`].
///
/// Note that no generation is carried: §1.2 fixes
/// [`GatewayClassEvidence`] at name and `accepted`, and a `GatewayClass`
/// under test is not edited during a run, so there is no spec change for
/// its status to lag behind. A caller that needs the freshness of the
/// `Accepted` condition can still call
/// [`ObservedCondition::freshness`] with the generation it read.
pub fn gateway_class_evidence(
    object: &serde_json::Value,
) -> Result<GatewayClassEvidence, GatewayError> {
    let name = required_metadata_string(object, "GatewayClass", "metadata.name")?;
    let described = format!("GatewayClass {name}");
    let conditions = observed_conditions(
        object.pointer("/status/conditions"),
        "GatewayClass",
        &described,
    )?;

    Ok(GatewayClassEvidence {
        accepted: conditions
            .get(CONDITION_ACCEPTED)
            .cloned()
            .unwrap_or_else(|| ObservedCondition::missing(CONDITION_ACCEPTED)),
        name,
    })
}

/// Normalizes an `HTTPRoute` object as read from a cluster.
///
/// # Errors
///
/// Returns [`GatewayError::MalformedStatus`] if `object` has no
/// `metadata.namespace`/`metadata.name`, no integer
/// `metadata.generation`, a `status.parents` that is not a list, a
/// parent entry that is not an object or has no `parentRef.name`, or an
/// unreadable condition. A route with no `status` at all parses to zero
/// parents -- the state every freshly applied route is in.
pub fn route_evidence(object: &serde_json::Value) -> Result<RouteEvidence, GatewayError> {
    let namespace = required_metadata_string(object, "HTTPRoute", "metadata.namespace")?;
    let name = required_metadata_string(object, "HTTPRoute", "metadata.name")?;
    let described = format!("HTTPRoute {namespace}/{name}");
    let generation = required_generation(object, "HTTPRoute", &described)?;

    let mut parents = Vec::new();
    if let Some(value) = object.pointer("/status/parents") {
        let entries = value
            .as_array()
            .ok_or_else(|| malformed(&described, "status.parents is not a list"))?;
        for (index, entry) in entries.iter().enumerate() {
            parents.push(route_parent_status(entry, &described, index)?);
        }
    }

    Ok(RouteEvidence {
        namespace,
        name,
        generation,
        parents,
    })
}

/// Normalizes a `status.conditions` list into a type-keyed map.
///
/// `conditions` is the value at `status.conditions`, or `None` if the
/// object has no such path -- which is not an error (see
/// [`gateway_evidence`]). Every entry must be an object with a non-empty
/// string `type` and a `status` that is one of the three Kubernetes
/// values; anything else is [`GatewayError::MalformedStatus`], for the
/// reason [`ConditionState::parse`] documents.
///
/// A duplicate condition type -- which Kubernetes's own
/// `metav1.Condition` list semantics forbid (the list is keyed by type)
/// -- is rejected rather than silently resolved by last-one-wins, since
/// the two entries could disagree and choosing between them would be a
/// guess.
///
/// `kind` and `described` only appear in error messages.
///
/// # Errors
///
/// See above; every failure is [`GatewayError::MalformedStatus`].
pub fn observed_conditions(
    conditions: Option<&serde_json::Value>,
    kind: &str,
    described: &str,
) -> Result<BTreeMap<String, ObservedCondition>, GatewayError> {
    let Some(value) = conditions else {
        return Ok(BTreeMap::new());
    };
    let entries = value
        .as_array()
        .ok_or_else(|| malformed(described, "status.conditions is not a list"))?;

    let mut parsed = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let type_name = entry
            .get("type")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                malformed(
                    described,
                    &format!("condition[{index}] has no non-empty string `type`"),
                )
            })?;

        let status = entry
            .get("status")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                malformed(
                    described,
                    &format!("condition[{index}] ({type_name}) has no string `status`"),
                )
            })?;
        let state = ConditionState::parse(status).ok_or_else(|| {
            malformed(
                described,
                &format!(
                    "condition[{index}] ({type_name}) has status {status:?}, which is not one of \
                     \"True\", \"False\", \"Unknown\" -- the only values a {kind} condition can \
                     hold"
                ),
            )
        })?;

        let condition = ObservedCondition {
            type_name: type_name.to_string(),
            state,
            reason: entry
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            observed_generation: entry
                .get("observedGeneration")
                .and_then(serde_json::Value::as_i64),
        };

        if parsed.insert(type_name.to_string(), condition).is_some() {
            return Err(malformed(
                described,
                &format!(
                    "condition type {type_name:?} appears more than once; a Kubernetes condition \
                     list is keyed by type, and two entries could disagree"
                ),
            ));
        }
    }
    Ok(parsed)
}

/// Normalizes one entry of a route's `status.parents`.
fn route_parent_status(
    entry: &serde_json::Value,
    described: &str,
    index: usize,
) -> Result<RouteParentStatus, GatewayError> {
    let parent_ref = entry.get("parentRef").ok_or_else(|| {
        malformed(
            described,
            &format!("status.parents[{index}] has no parentRef"),
        )
    })?;

    let name = parent_ref
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            malformed(
                described,
                &format!("status.parents[{index}].parentRef has no non-empty string `name`"),
            )
        })?
        .to_string();

    let optional = |field: &str| {
        parent_ref
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };

    Ok(RouteParentStatus {
        parent: ParentIdentity {
            namespace: optional("namespace"),
            name,
            section_name: optional("sectionName"),
        },
        controller_name: entry
            .get("controllerName")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        conditions: observed_conditions(
            entry.get("conditions"),
            "route parent",
            &format!("{described} status.parents[{index}]"),
        )?,
    })
}

/// Reads a required `metadata.<field>` string, or reports the object as
/// malformed.
fn required_metadata_string(
    object: &serde_json::Value,
    kind: &str,
    field: &str,
) -> Result<String, GatewayError> {
    object
        .pointer(&format!("/{}", field.replace('.', "/")))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| GatewayError::MalformedStatus {
            object: format!("a {kind} object"),
            reason: format!("no non-empty string {field}"),
        })
}

/// Reads a required, integer `metadata.generation`.
///
/// Required rather than defaulted to `0`: `generation` is what every
/// `observedGeneration` is compared against, so inventing one would turn
/// the staleness check into a coin flip. The API server sets it on every
/// object with a `spec`, and Gateway API's types all have one, so its
/// absence means the value is not the object this code thinks it is.
fn required_generation(
    object: &serde_json::Value,
    kind: &str,
    described: &str,
) -> Result<i64, GatewayError> {
    object
        .pointer("/metadata/generation")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| {
            malformed(
                described,
                &format!(
                    "no integer metadata.generation; without it no condition's \
                     observedGeneration can be checked, and a {kind}'s status freshness would \
                     have to be guessed"
                ),
            )
        })
}

/// Builds a [`GatewayError::MalformedStatus`].
fn malformed(described: &str, reason: &str) -> GatewayError {
    GatewayError::MalformedStatus {
        object: described.to_string(),
        reason: reason.to_string(),
    }
}
