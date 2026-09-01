//! The Ingress-to-Gateway migration suite's configuration surface
//! (ROADMAP Task 8.3): which `Ingress` manifests define today's
//! behavior, which Gateway API manifests are meant to replace them, and
//! what has to keep answering the same way through both.
//!
//! # Admission Lab converts nothing, and that is the design
//!
//! **v1 contains no Ingress-to-Gateway converter.** Both halves of every
//! [`MigrationCaseSpec`] are written by a person, or produced by some
//! other tool (`ingress2gateway`, a vendor's migration guide, a hand
//! edit), and this suite exists to check the conversion *that user has
//! already performed*.
//!
//! This is worth saying loudly because the opposite is the obvious first
//! guess, and it is unsound. A suite that generated its own candidate
//! manifests would compare a converter against itself: every rule the
//! converter got wrong would be applied identically on both sides, and
//! the run would report "no behavior change" for exactly the mistakes it
//! was built to catch. The pairing is explicit for the same reason
//! [`admissionlab_spec::RouteContract`] names its `Gateway` explicitly
//! -- a contract that reads its expectation out of the artifact under
//! test can never contradict that artifact.
//!
//! # Where these types are defined, and why not here
//!
//! Task 8.3's file list names this module as [`MigrationSuiteSpec`]'s
//! home, while §1.2's registry independently freezes
//! [`admissionlab_spec::ResolvedLab::migration`] as an
//! `Option<MigrationSuiteSpec>`. Exactly the situation
//! [`crate::model`] already resolved for
//! [`crate::model::GatewaySuiteSpec`], with the
//! same forced answer: this crate depends (through
//! `admissionlab-core`) on `admissionlab-spec`, so a `spec -> gateway`
//! edge would be a dependency cycle Cargo rejects. The hand-written
//! configuration types are defined in [`admissionlab_spec::v1beta1`],
//! and this module **re-exports those exact types** rather than
//! declaring parallel ones -- §1.2's "these names are canonical" rule
//! forbids a synonym.
//!
//! [`NonPortableFeatureExpectation`] is a registry-frozen name that Task
//! 8.5 will also read, and it is defined on the same side for the same
//! reason: it is nested inside a [`MigrationCaseSpec`], which is nested
//! inside the resolved lab. `MigrationBehaviorChange` -- the *observed*
//! half of the vocabulary -- is Task 8.5's, and belongs in this crate
//! when that task lands, next to everything else Admission Lab observed
//! rather than read.
//!
//! # What is validated, and where
//!
//! Every Task 8.3 rule -- a non-empty case list, unique non-empty case
//! ids, a non-empty manifest list on *both* sides of each pairing,
//! probes that are valid by the same rules a Gateway suite's probes are
//! (`admissionlab_spec::validate`'s one shared probe validator), at
//! least one probe per case, and non-portability entries with a feature
//! name, a human reason, and no duplicate feature -- is enforced by
//! [`admissionlab_spec::load_any_supported_lab`] at configuration-load
//! time, before any cluster is created. Same placement, same reasoning
//! as [`crate::model`]: the rules are about the document, the document
//! is parsed there, and a second pass here could only ever disagree with
//! the first. `tests/migration_model.rs` drives them through this
//! module's re-exports, so the surface Tasks 8.4-8.5 consume is the one
//! under test.
//!
//! # `migration:` is a v1beta1-only section
//!
//! `admissionlab.io/v1alpha1` has no `migration:` key, so an Alpha
//! document always resolves to `migration: None`. See
//! [`admissionlab_spec::V1Beta1Lab::migration`] for why that is a
//! translation rather than an invented default.
//!
//! ---
//!
//! # The comparator (ROADMAP Task 8.5)
//!
//! Everything above is configuration. The rest of this module is the
//! *observed* half: [`compare_migration_case`] takes what the legacy
//! stack did ([`IngressCaseResult`], Task 8.4) and what the Gateway
//! stack did ([`GatewayCaseResult`]) with the same case, and reports the
//! behavioral differences as [`MigrationBehaviorChange`]s.
//!
//! ## A separate vocabulary from `SemanticChangeKind`, on purpose
//!
//! [`crate::diff::diff_gateway`] compares two *Gateway* stacks and emits
//! `admissionlab_diff::SemanticChange`, the one vocabulary the whole
//! project grades, excepts and renders. This module emits
//! [`MigrationBehaviorKind`] instead, and the two never mix.
//!
//! The reason is that they answer different questions. `diff_gateway`
//! compares two implementations of *the same* configuration: the
//! objects, their conditions and their probes all line up, and a
//! difference is a regression in one implementation. A migration
//! compares two *different* configurations that are supposed to behave
//! alike -- an `Ingress` on one side and a `Gateway` + `HTTPRoute` on
//! the other. There is no `Accepted` condition to compare, no shared
//! object to name, and the interesting classification is *which routing
//! behavior* moved (host, path, TLS, backend, rewrite, redirect) rather
//! than which condition changed. Reusing `SemanticChangeKind` would have
//! meant either inventing six more variants on a vocabulary
//! `admissionlab-policy` grades with severities that mean something
//! else, or collapsing all six into `traffic_status_changed` and losing
//! exactly the classification Task 8.5 exists to produce.
//!
//! ROADMAP §1.2 freezes both names independently, which is the same
//! answer. How a [`MigrationComparison`] is presented and whether it
//! reaches a run's exit code is Task 8.8's decision, not this module's;
//! nothing here has a severity, and `admissionlab-policy` is untouched.
//!
//! ## What is compared: evidence, never manifest syntax
//!
//! Task 8.5 Step 1, in as many words: *compare observed probe
//! status/backend/path/redirect location rather than manifest syntax.*
//! [`compare_migration_traffic`] reads exactly four things off each
//! paired [`crate::probe::HttpProbeResult`] -- its status, its backend
//! identity, whether it carried a `Location`, and where that `Location`
//! pointed -- and nothing else. It never opens a manifest, never looks
//! at an `HTTPRoute`'s filters, and never asks what a user *meant*.
//!
//! The one deliberate exception is the annotation catalog
//! ([`nonportable_changes`], Step 2), which is a statement about the
//! baseline manifests rather than about traffic. It is a separate
//! function producing separately-kinded changes for that reason.
//!
//! ## What the evidence can and cannot show
//!
//! Two limits, stated here because a comparator that quietly cannot see
//! something is worse than one that says so:
//!
//! - **The echoed request path is not available.**
//!   `admissionlab_echo` answers `200` to *every* path and reports the
//!   path it received in its body, but [`crate::probe::HttpProbeResult`]
//!   keeps only the body's `backend` field and a SHA-256 of the whole
//!   body. So a rewrite that both sides deliver successfully to the same
//!   backend -- `/api/v1/x` arriving as `/x` on one side and `/api/v1/x`
//!   on the other -- is invisible to this comparator. It is detected
//!   only through a *consequence*: a different status from the same
//!   backend, or a different redirect target.
//! - **The response body hash is unusable as evidence.** It would seem
//!   to recover the point above, and it does not: the echo body includes
//!   every request header that arrived, and `ingress-nginx` injects a
//!   fresh `x-request-id` per request (its own recipe records this).
//!   Two probes of *the same* stack therefore hash differently, so a
//!   hash comparison across two stacks would report a difference on
//!   every single probe. `response_body_sha256` is never read here.
//!
//! Neither is worked around by guessing (Global Constraint 15). A later
//! task that wants rewrite behavior directly needs the echoed path to
//! reach the result type, which is a change to Task 6.8's frozen
//! evidence shape and belongs to whoever makes it.
//!
//! ## How each kind is triggered
//!
//! [`compare_migration_traffic`] runs one ordered decision per paired
//! probe. The full rule, with the reasoning for each arm, is on
//! [`MigrationBehaviorKind`]'s variants and on
//! [`compare_probe_pair`]; the short form is:
//!
//! ```text
//! backends differ (both identified)          -> backend_changed
//! then, if the Location or the status differs:
//!   both sides sent a Location:
//!     both named a scheme, and they differ   -> tls_behavior_changed
//!     else one is absolute, one relative     -> redirect_behavior_changed
//!     else the hosts differ                  -> host_behavior_changed
//!     else the paths differ                  -> rewrite_behavior_changed
//!     else (same target, different status)   -> redirect_behavior_changed
//!   exactly one side sent a Location         -> redirect_behavior_changed
//!   neither did, and the status differs:
//!     both sides reached the same backend    -> rewrite_behavior_changed
//!     else the contract's path is "/"        -> host_behavior_changed
//!     else                                   -> path_behavior_changed
//! ```
//!
//! ## An incomparable case
//!
//! [`migration_comparability`] is this module's twin of
//! [`crate::diff::gateway_comparability`], and exists for the same
//! reason: an empty change list is a positive claim, and it is only
//! honest when both sides were observed. The case Task 8.4 makes real is
//! a baseline the API server *refused*: the candidate's `HTTPRoute` may
//! be serving traffic perfectly, and there is no legacy behavior to
//! compare it against. Reporting "no behavior change" there would be a
//! claim about a comparison nobody performed, and reporting every
//! candidate probe as a change would run the claim backwards.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

pub use admissionlab_spec::{MigrationCaseSpec, MigrationSuiteSpec, NonPortableFeatureExpectation};

use crate::apply::PlannedObject;
use crate::case::GatewayCaseResult;
use crate::diff::ProbePair;
use crate::ingress::IngressCaseResult;
use crate::model::HttpProbeContract;
use crate::probe::{HttpProbeResult, describe_probe_request};

/// The set of feature names a [`MigrationCaseSpec`] declares
/// non-portable.
///
/// A free function rather than an inherent method for the reason
/// [`crate::model::contract_gateway_identity`] gives: the type is
/// defined in `admissionlab-spec` and Rust does not allow inherent impls
/// on a foreign type.
///
/// A [`BTreeSet`] of borrowed names, which is the shape the question is
/// actually asked in: Task 8.5 turns each observed behavioral difference
/// into a `MigrationBehaviorChange` and has to decide whether it was
/// *expected*, which is one membership test per difference rather than a
/// linear scan per difference. Building the set is safe because
/// `admissionlab_spec::load_any_supported_lab` has already rejected a
/// duplicated feature -- so the set's size always equals
/// [`MigrationCaseSpec::expected_nonportable`]'s length, and nothing is
/// silently collapsed here.
///
/// Deliberately does *not* return the reasons: this answers "was this
/// declared?", and a caller that needs to show a user *why* reads the
/// entry itself, where the reason sits next to the feature it explains.
#[must_use]
pub fn expected_nonportable_features(case: &MigrationCaseSpec) -> BTreeSet<&str> {
    case.expected_nonportable
        .iter()
        .map(|expectation| expectation.feature.as_str())
        .collect()
}

/// Which routing behavior moved between an `Ingress` and the Gateway API
/// objects meant to replace it.
///
/// §1.2's canonical name, frozen at these seven variants by ROADMAP Task
/// 8.5. Deliberately *not* `admissionlab_diff::SemanticChangeKind` -- see
/// this module's "A separate vocabulary from `SemanticChangeKind`".
///
/// The wire tags are `snake_case`, matching every other change
/// vocabulary in this workspace (`traffic_backend_changed`,
/// `route_attached`, ...), and they are pinned by
/// `tests/migration_diff.rs` rather than left to the derive: a rename
/// here would silently change a document a report renders.
///
/// `Serialize` but not `Deserialize`, and no `JsonSchema`: this is
/// observation travelling outward, and nothing embeds it in the frozen
/// `admissionlab.io/result/v1beta1` document yet -- Task 8.8 decides how
/// a migration comparison is presented, and adding a schema derive now
/// would freeze a shape that task has not chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationBehaviorKind {
    /// Host-based routing behaves differently.
    ///
    /// Two triggers, both evidence-first: the two sides redirected to
    /// different *hosts*, or a probe whose request path is the bare root
    /// `/` got different statuses. A root-path probe carries no
    /// path-matching information at all -- the only thing distinguishing
    /// it from any other request to that data plane is its `Host` header
    /// -- so a status difference on one is a difference in what the
    /// stacks did with that host.
    HostBehaviorChanged,
    /// Path matching behaves differently: the two sides answered a
    /// probe for a specific path differently, and nothing more specific
    /// (a redirect, a backend, a shared backend) accounts for it.
    ///
    /// This is the residual arm of the status-difference rule, and it is
    /// deliberately the residual one: a contract's `path` is the most
    /// specific thing a routing rule matches on, and an `Ingress`
    /// `pathType` and an `HTTPRoute` `path.type` are the pair most
    /// likely to have been translated wrongly by hand.
    PathBehaviorChanged,
    /// TLS behaves differently: the two sides redirected to different
    /// URI *schemes*.
    ///
    /// The classic migration bug this catches is
    /// `nginx.ingress.kubernetes.io/ssl-redirect` (on by default once an
    /// `Ingress` has any TLS block) against an `HTTPRoute` whose
    /// `RequestRedirect` filter names no `scheme`: one side sends the
    /// client to `https://`, the other to `http://`.
    ///
    /// A known limit, stated rather than hidden: probes reach a data
    /// plane through a plaintext `kubectl port-forward`, so a redirect's
    /// scheme is the *only* TLS evidence available today. When a probe
    /// can itself speak TLS, a status difference on such a probe belongs
    /// here too, and this variant is where that lands.
    TlsBehaviorChanged,
    /// A different workload answered.
    ///
    /// Claimed only when *both* sides identified their backend and the
    /// identities differ. `backend: None` means "which workload answered
    /// is unknown", never "a different one did" -- the same rule
    /// [`crate::diff::diff_gateway`] applies to
    /// `traffic_backend_changed`, and for the same reason: a fabricated
    /// value here would fabricate a regression.
    BackendChanged,
    /// The request the backend received differs, or the redirect target
    /// was rewritten differently.
    ///
    /// Two triggers. The two sides redirected to the same scheme and
    /// host but a different *path* -- which is a rewrite of the target,
    /// not a difference in whether to redirect. Or the same backend
    /// answered both sides with different statuses: an identical
    /// workload cannot answer differently unless what reached it
    /// differed, and for an echo backend the only thing that can differ
    /// is the request line the proxy forwarded.
    ///
    /// See this module's "What the evidence can and cannot show" for the
    /// rewrite this cannot see at all.
    RewriteBehaviorChanged,
    /// Whether, or how, the request is redirected differs: one side sent
    /// a `Location` and the other did not, or both did to the same place
    /// with different status codes (a `301` against a `308` is a
    /// different instruction to a client about method rewriting and
    /// cacheability).
    RedirectBehaviorChanged,
    /// The baseline `Ingress` carries an `ingress-nginx` annotation with
    /// no portable Gateway API equivalent.
    ///
    /// The one kind derived from the manifests rather than from traffic,
    /// and the only one whose `expected` flag can be `true`. See
    /// [`NONPORTABLE_INGRESS_ANNOTATIONS`] for the catalog and
    /// [`nonportable_changes`] for the rule.
    NonPortableFeature,
}

impl MigrationBehaviorKind {
    /// This kind's wire tag, exactly as [`Serialize`] emits it.
    ///
    /// Written out rather than derived so the tag is greppable and so a
    /// caller rendering a terminal line and a caller writing JSON cannot
    /// disagree -- the same reason
    /// `admissionlab_diff::SemanticChangeKind` carries its own.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HostBehaviorChanged => "host_behavior_changed",
            Self::PathBehaviorChanged => "path_behavior_changed",
            Self::TlsBehaviorChanged => "tls_behavior_changed",
            Self::BackendChanged => "backend_changed",
            Self::RewriteBehaviorChanged => "rewrite_behavior_changed",
            Self::RedirectBehaviorChanged => "redirect_behavior_changed",
            Self::NonPortableFeature => "non_portable_feature",
        }
    }
}

impl std::fmt::Display for MigrationBehaviorKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One behavioral difference between an `Ingress` and the Gateway API
/// objects meant to replace it.
///
/// §1.2's canonical three fields. `detail` is prose because the shape of
/// the evidence differs per kind (a status pair, a backend pair, two
/// redirect targets, an annotation and the objects carrying it) and a
/// structured payload general enough for all of them would be a
/// `serde_json::Value` nobody could match on either. Every `detail` this
/// module produces is deterministic and names the probe index or the
/// annotation, so two runs of the same comparison produce byte-identical
/// text (Global Constraint 7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationBehaviorChange {
    /// Which behavior moved.
    pub kind: MigrationBehaviorKind,
    /// What was observed, in full enough detail to check the claim.
    pub detail: String,
    /// Whether the case's author declared this in writing.
    ///
    /// `true` only for a [`MigrationBehaviorKind::NonPortableFeature`]
    /// whose annotation is named in
    /// [`MigrationCaseSpec::expected_nonportable`]. **Every
    /// traffic-derived change is `false`**, and that is a decision
    /// rather than an omission: `expected_nonportable` is a vocabulary
    /// about *features an Ingress declares*, not about status codes, and
    /// there is no way to attribute an observed `200 -> 404` to a
    /// declared feature without guessing which one. A team that wants to
    /// accept a traffic difference says so in the generic
    /// regression-expectation channel, which Task 8.3 Step 3 keeps
    /// deliberately separate from this one.
    ///
    /// What `expected: false` is *not* is a severity. ROADMAP Task 8.5
    /// Step 3 says an unexpected non-portable feature is "warning by
    /// default"; that default belongs to whoever grades this (Global
    /// Constraint 6), and this module records only the fact.
    pub expected: bool,
}

/// One migration case's comparison: what moved, and the probe evidence
/// it was derived from.
///
/// §1.2's canonical two fields, and the reason
/// [`unmatched_nonportable_expectations`] is a free function rather than
/// a third one: the registry freezes this type at `changes` and
/// `probes`, so a declared-but-never-observed expectation -- the
/// stale-expectation analog `admissionlab-policy` reports for its own
/// `expectations.yaml` -- is surfaced by asking, not by carrying. It is
/// also the honest placement on its own merits: a
/// [`MigrationBehaviorChange`] is something Admission Lab *observed*,
/// and an expectation nobody exercised is a statement about the
/// configuration, not about the cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationComparison {
    /// Every difference, deterministically ordered: the traffic changes
    /// first, in probe-index order and within one probe in the fixed
    /// order [`compare_probe_pair`] emits them, then the non-portable
    /// feature changes in annotation-name order.
    pub changes: Vec<MigrationBehaviorChange>,
    /// Every probe both sides answered, paired by index. Empty when
    /// [`migration_comparability`] found the two sides incomparable.
    pub probes: Vec<ProbePair>,
}

/// Whether one migration case's two sides can be compared at all.
///
/// This module's twin of [`crate::diff::GatewayComparability`]; see this
/// module's "An incomparable case" for the argument, and
/// [`MigrationComparability::reason`] for what each variant means in
/// prose.
///
/// Derives no `Default`, for the reason that type gives:
/// `Comparable` is the flattering answer, and defaulting to it would
/// turn "we could not tell" into "we looked and it was fine".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationComparability {
    /// Both sides produced traffic evidence for this case.
    Comparable,
    /// The API server refused the baseline's own objects, so the legacy
    /// stack never served this case at all. The candidate's evidence, if
    /// any, stands on its own -- there is nothing to compare it against.
    BaselineNotAdmitted,
    /// The baseline was admitted but never served the case's probes as
    /// contracted within its deadline, so it established no reference
    /// behavior. See [`crate::ingress`]'s "THE FINDING" for why that is
    /// measured with traffic rather than with a status.
    BaselineNotServing,
    /// The baseline served the case and the candidate answered no probe
    /// -- a route that never reconciled, or a suite that skipped its
    /// probes with a reason. What the candidate lost is
    /// [`crate::diff::diff_gateway`]'s claim to make about the Gateway
    /// side's own two runs; this comparator has nothing to pair.
    CandidateNotServing,
}

impl MigrationComparability {
    /// Returns `true` only for [`MigrationComparability::Comparable`].
    #[must_use]
    pub fn is_comparable(self) -> bool {
        matches!(self, Self::Comparable)
    }

    /// Why the two sides are not comparable, or why they are.
    ///
    /// Prose rather than a `Display` impl of the variant name, because
    /// this is what a report shows a user next to an empty change list;
    /// the variant name alone would leave "incomparable" unexplained.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Comparable => {
                "both sides answered the case's probes, so a difference and an absence of \
                 differences are both evidence"
            }
            Self::BaselineNotAdmitted => {
                "the API server refused the baseline Ingress, so this migration has no legacy \
                 behavior to be compared against"
            }
            Self::BaselineNotServing => {
                "the legacy Ingress stack never served this case's probes, so it established no \
                 reference behavior"
            }
            Self::CandidateNotServing => {
                "the Gateway stack answered none of this case's probes, so there is nothing to \
                 pair the baseline's answers with"
            }
        }
    }
}

/// Reports whether `baseline` and `candidate` can be compared.
///
/// The order of the checks is the order the evidence is produced in, so
/// the first failure named is the earliest one: a case that was never
/// admitted is reported as such rather than as "not serving", which is
/// true but says less.
#[must_use]
pub fn migration_comparability(
    baseline: &IngressCaseResult,
    candidate: &GatewayCaseResult,
) -> MigrationComparability {
    if !baseline.admitted {
        MigrationComparability::BaselineNotAdmitted
    } else if !baseline.ready || baseline.probes.is_empty() {
        MigrationComparability::BaselineNotServing
    } else if candidate.probes.is_empty() {
        MigrationComparability::CandidateNotServing
    } else {
        MigrationComparability::Comparable
    }
}

/// Compares one migration case's two sides.
///
/// `baseline_documents` is what the baseline side applied, as
/// [`crate::apply::plan_gateway_apply`] parses it -- the input to
/// [`nonportable_changes`], and the *only* thing here that is read from
/// a manifest rather than from a response. A caller obtains it with
/// `plan_gateway_apply(&case.baseline_ingress_manifests)`, which is
/// pure, offline and already how the objects were planned before being
/// applied. It is a parameter rather than something this function reads
/// off disk itself because a comparator that performs I/O is a
/// comparator whose output can depend on when it ran (Global Constraint
/// 7).
///
/// Deterministic: the same four inputs always produce the same
/// comparison, in the same order, with no clock, network or ambient
/// state involved.
///
/// Read [`migration_comparability`] before reporting an empty `changes`
/// as "this migration changed nothing" -- see this module's "An
/// incomparable case", which is the whole reason that function is
/// separate and public.
#[must_use]
pub fn compare_migration_case(
    case: &MigrationCaseSpec,
    baseline: &IngressCaseResult,
    candidate: &GatewayCaseResult,
    baseline_documents: &[PlannedObject],
) -> MigrationComparison {
    let comparable = migration_comparability(baseline, candidate).is_comparable();

    let mut changes = if comparable {
        compare_migration_traffic(case, baseline, candidate)
    } else {
        Vec::new()
    };
    // Emitted whether or not the traffic was comparable, because this
    // half is a statement about the manifests a user wrote rather than
    // about a response: an `Ingress` that names `canary-by-header` names
    // it whether or not a webhook let it through, and a team migrating
    // needs to know before they discover it in production.
    changes.extend(nonportable_changes(case, baseline_documents));

    MigrationComparison {
        changes,
        probes: if comparable {
            paired_probes(case, baseline, candidate)
        } else {
            Vec::new()
        },
    }
}

/// Every probe both sides answered, paired by index.
///
/// Pairing is by index for exactly the reason
/// [`crate::diff::diff_gateway`] gives for its own probes: an
/// [`HttpProbeContract`] has no id of its own, both sides ran the *same*
/// [`MigrationCaseSpec::probes`] list, and `probes[i]` is therefore the
/// answer to `case.probes[i]` on each side.
///
/// A probe only one side answered is **not** a pair: [`ProbePair`]'s two
/// results are non-optional, and a pair with a fabricated half would be
/// a claim about a request nobody answered. Unlike the Gateway
/// comparator, an unpaired probe is not reported as a change either --
/// there is no kind in [`MigrationBehaviorKind`] that could honestly
/// carry "one side answered a request the other did not", and the
/// asymmetry is already visible in [`migration_comparability`] and in
/// the probe list's own length.
fn paired_probes(
    case: &MigrationCaseSpec,
    baseline: &IngressCaseResult,
    candidate: &GatewayCaseResult,
) -> Vec<ProbePair> {
    baseline
        .probes
        .iter()
        .zip(candidate.probes.iter())
        .map(|(baseline_probe, candidate_probe)| ProbePair {
            contract_id: case.id.clone(),
            baseline: baseline_probe.clone(),
            candidate: candidate_probe.clone(),
        })
        .collect()
}

/// The traffic half of [`compare_migration_case`]: what the two sides'
/// responses say, probe by probe.
///
/// Public so the two halves can be driven and tested apart, the same way
/// [`crate::diff::gateway_comparability`] is public beside
/// [`crate::diff::diff_gateway`].
///
/// Does **not** consult [`migration_comparability`] itself: a caller that
/// wants the traffic claims for a partially-observed case can have them,
/// and [`compare_migration_case`] is where the gating decision lives so
/// there is exactly one of it.
#[must_use]
pub fn compare_migration_traffic(
    case: &MigrationCaseSpec,
    baseline: &IngressCaseResult,
    candidate: &GatewayCaseResult,
) -> Vec<MigrationBehaviorChange> {
    let mut changes = Vec::new();
    for (index, (baseline_probe, candidate_probe)) in baseline
        .probes
        .iter()
        .zip(candidate.probes.iter())
        .enumerate()
    {
        // A case whose two sides answered more probes than the case
        // declares cannot describe which request a change is about, so
        // nothing is claimed about it. This cannot arise from a runner
        // that sends `case.probes`; it is handled rather than
        // `unwrap`ped because this function is public and takes three
        // independently-constructed values.
        let Some(contract) = case.probes.get(index) else {
            break;
        };
        changes.extend(compare_probe_pair(
            index,
            contract,
            baseline_probe,
            candidate_probe,
        ));
    }
    changes
}

/// The whole classification rule for one paired probe, in the order the
/// changes are emitted.
///
/// Two independent facts can both be true of one probe, so this returns
/// a list rather than an `Option`: a request can reach a different
/// backend *and* be answered differently. `backend_changed` comes first
/// because it is the most specific claim available -- it names the
/// workload -- and because a reader scanning a report wants "a different
/// service answered" above "the status moved".
///
/// The rest is one decision, on the redirect axis first. `Location` is
/// consulted before status because a redirect *is* the answer to the
/// request rather than a failure to answer it: two stacks that both
/// redirect, to different places, differ in a way a bare status
/// comparison would call identical.
///
/// See this module's "How each kind is triggered" for the table, and
/// [`MigrationBehaviorKind`]'s variants for why each arm lands where it
/// does.
fn compare_probe_pair(
    index: usize,
    contract: &HttpProbeContract,
    baseline: &HttpProbeResult,
    candidate: &HttpProbeResult,
) -> Vec<MigrationBehaviorChange> {
    let request = describe_probe_request(contract);
    let mut changes = Vec::new();

    if let (Some(baseline_backend), Some(candidate_backend)) =
        (&baseline.backend, &candidate.backend)
        && baseline_backend != candidate_backend
    {
        changes.push(traffic_change(
            MigrationBehaviorKind::BackendChanged,
            format!(
                "probe {index} ({request}): the Ingress reached backend {baseline_backend:?} and \
                 the Gateway reached {candidate_backend:?}"
            ),
        ));
    }

    let baseline_location = redirect_target(baseline);
    let candidate_location = redirect_target(candidate);
    let status_changed = baseline.status != candidate.status;
    if baseline_location == candidate_location && !status_changed {
        return changes;
    }

    let (kind, what) = match (&baseline_location, &candidate_location) {
        (Some(from), Some(to))
            if matches!(
                (&from.scheme, &to.scheme),
                (Some(baseline_scheme), Some(candidate_scheme))
                    if baseline_scheme != candidate_scheme
            ) =>
        {
            (
                MigrationBehaviorKind::TlsBehaviorChanged,
                format!(
                    "the redirect target's scheme moved from {} to {}",
                    describe_component(from.scheme.as_deref()),
                    describe_component(to.scheme.as_deref())
                ),
            )
        }
        // One side answered with an absolute target and the other with a
        // relative one. Not a TLS difference -- a relative `Location`
        // names no scheme at all, and a client resolves it against the
        // request, which this probe reached over a port-forward whose
        // scheme says nothing about the listener. What *is* observably
        // different is the form of the instruction, so it is reported as
        // redirect behavior with both targets named.
        (Some(from), Some(to)) if from.scheme.is_some() != to.scheme.is_some() => (
            MigrationBehaviorKind::RedirectBehaviorChanged,
            format!(
                "one side's redirect target is absolute and the other's is relative: {from} \
                 against {to}"
            ),
        ),
        (Some(from), Some(to)) if from.host != to.host => (
            MigrationBehaviorKind::HostBehaviorChanged,
            format!(
                "the redirect target's host moved from {} to {}",
                describe_component(from.host.as_deref()),
                describe_component(to.host.as_deref())
            ),
        ),
        (Some(from), Some(to)) if from.path != to.path => (
            MigrationBehaviorKind::RewriteBehaviorChanged,
            format!(
                "the redirect target's path moved from {:?} to {:?}",
                from.path, to.path
            ),
        ),
        (Some(_), Some(_)) => (
            MigrationBehaviorKind::RedirectBehaviorChanged,
            "both sides redirected to the same target with different status codes".to_owned(),
        ),
        (Some(from), None) => (
            MigrationBehaviorKind::RedirectBehaviorChanged,
            format!("the Ingress redirected to {from} and the Gateway did not redirect"),
        ),
        (None, Some(to)) => (
            MigrationBehaviorKind::RedirectBehaviorChanged,
            format!("the Ingress did not redirect and the Gateway redirected to {to}"),
        ),
        (None, None) => match (&baseline.backend, &candidate.backend) {
            (Some(backend), Some(same)) if backend == same => (
                MigrationBehaviorKind::RewriteBehaviorChanged,
                format!(
                    "backend {backend:?} answered both sides differently, so what reached it \
                     differed"
                ),
            ),
            // The contract's own path is what says which behavior the
            // probe exercises -- see `HostBehaviorChanged` and
            // `PathBehaviorChanged`.
            _ if contract.path == "/" => (
                MigrationBehaviorKind::HostBehaviorChanged,
                "the probe's only routing input is its Host header".to_owned(),
            ),
            _ => (
                MigrationBehaviorKind::PathBehaviorChanged,
                format!("the probe matches on the path {:?}", contract.path),
            ),
        },
    };

    changes.push(traffic_change(
        kind,
        format!(
            "probe {index} ({request}): the Ingress answered HTTP {} and the Gateway answered \
             HTTP {}; {what}",
            baseline.status, candidate.status
        ),
    ));
    changes
}

/// One traffic-derived change. Always `expected: false` -- see
/// [`MigrationBehaviorChange::expected`].
fn traffic_change(kind: MigrationBehaviorKind, detail: String) -> MigrationBehaviorChange {
    MigrationBehaviorChange {
        kind,
        detail,
        expected: false,
    }
}

/// Renders an absent scheme or host as a word rather than as `None`.
fn describe_component(component: Option<&str>) -> String {
    component.map_or_else(
        || "(none: a relative Location)".to_owned(),
        |value| format!("{value:?}"),
    )
}

/// A response's `Location`, reduced to the three parts two
/// implementations can be expected to agree on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RedirectTarget {
    /// The URI scheme, lowercased, or `None` for a relative `Location`.
    scheme: Option<String>,
    /// The host, lowercased and **without** the port, or `None` for a
    /// relative `Location`.
    host: Option<String>,
    /// The path component, verbatim.
    path: String,
}

impl std::fmt::Display for RedirectTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.scheme, &self.host) {
            (Some(scheme), Some(host)) => write!(formatter, "{scheme}://{host}{}", self.path),
            _ => write!(formatter, "{}", self.path),
        }
    }
}

/// The normalized `Location` of a response that carried one.
///
/// **The port is dropped deliberately, and it is the only interesting
/// decision here.** A probe reaches a data plane through a `kubectl
/// port-forward`, so the request arrives at `127.0.0.1:<an ephemeral
/// local port>` while its `Host` header says something like
/// `basic.ingress.admissionlab.test`. An implementation building an
/// absolute redirect target may echo the authority it was given (no
/// port), append its listener's port, or append the port it believes the
/// client used -- all three are conforming answers to the same
/// configuration. Comparing ports would therefore compare the
/// port-forward rather than the migration, and would report a difference
/// on every run, including two runs of the same stack (the kernel picks
/// a different ephemeral port each time). The raw header is untouched in
/// [`HttpProbeResult::response_headers`], so nothing is lost.
///
/// The query and fragment are dropped for the narrower reason that
/// `admissionlab_echo::echo::EchoBody::path` already drops the query:
/// one notion of "the path" across this project, not two.
///
/// `None` covers both "no `Location` header" and "a `Location` that is
/// not a URI at all" -- never guessed, and never a fabricated target.
/// The key lookup needs no lowercasing because
/// [`HttpProbeResult::response_headers`] is already normalized to
/// lowercase names.
///
/// Private, and small enough to stay so. ROADMAP Task 8.7 needs the
/// same three parts for a *portable contract* assertion rather than for
/// a cross-stack comparison, and is landing a public equivalent in
/// [`crate::probe`] with the same port decision; whichever of the two
/// lands second should delete this function and call that one, which is
/// a change of an import and nothing else.
fn redirect_target(result: &HttpProbeResult) -> Option<RedirectTarget> {
    let value = result.response_headers.get("location")?;
    let uri = value.trim().parse::<hyper::Uri>().ok()?;
    Some(RedirectTarget {
        scheme: uri.scheme_str().map(str::to_ascii_lowercase),
        host: uri.host().map(str::to_ascii_lowercase),
        path: uri.path().to_owned(),
    })
}

/// One `ingress-nginx` annotation with no portable Gateway API
/// equivalent, and the evidence for that claim.
///
/// `documentation` is a real upstream URL, not a paraphrase: the whole
/// value of this catalog is that a team migrating can read what the
/// annotation actually did before deciding what to do instead, and a
/// claim about portability that cites nothing is a claim nobody can
/// check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonPortableAnnotation {
    /// The annotation key, exactly as an `Ingress` spells it, and
    /// exactly the string a
    /// [`NonPortableFeatureExpectation::feature`] must name to mark it
    /// expected.
    pub annotation: &'static str,
    /// What it does, and why Gateway API v1 has no faithful equivalent.
    pub reason: &'static str,
    /// Where upstream documents it.
    pub documentation: &'static str,
}

/// The reviewed catalog of `ingress-nginx` annotations with no portable
/// Gateway API equivalent (ROADMAP Task 8.5 Step 2).
///
/// # What belongs here, and what deliberately does not
///
/// An entry is a feature whose behavior **cannot be expressed in
/// portable Gateway API v1** -- not merely one that is spelled
/// differently. `nginx.ingress.kubernetes.io/ssl-redirect`,
/// `permanent-redirect`, `rewrite-target` and `server-alias` are all
/// absent for that reason: Gateway API's `RequestRedirect` filter,
/// `URLRewrite` filter and multi-hostname listeners cover them, so
/// flagging them would train a reader to ignore this list. "Portable"
/// is load-bearing in the other direction too:
/// `use-regex` is listed even though Gateway API's `PathMatchType` does
/// define `RegularExpression`, because that value's own conformance
/// level is *implementation-specific*, so a route relying on it is not
/// portable between the implementations this project certifies.
///
/// The canary family is listed by its entry points. Upstream honors
/// **no** canary annotation unless `nginx.ingress.kubernetes.io/canary`
/// is `"true"` on the same object, so no canary `Ingress` can escape
/// this catalog through an annotation it does not name.
///
/// # This is data, not severity (Global Constraint 6)
///
/// Task 8.5 Step 2's own words: "this catalog lives in migration
/// code/data, not recipe severity logic". Nothing here says how bad a
/// finding is, and nothing in a recipe may. What Step 3 calls "warning
/// by default" for an *unexpected* feature is a grading decision, and it
/// belongs to whoever grades a [`MigrationBehaviorChange`]; this module
/// records `expected: true` or `expected: false` and stops.
///
/// # Ordering
///
/// Sorted by `annotation`, and `tests/migration_diff.rs` asserts it: the
/// order is what [`nonportable_changes`] emits in, so a hand-inserted
/// entry in the wrong place would reorder a report.
pub const NONPORTABLE_INGRESS_ANNOTATIONS: [NonPortableAnnotation; 13] = [
    NonPortableAnnotation {
        annotation: "nginx.ingress.kubernetes.io/auth-type",
        reason: "HTTP basic authentication against a Secret, enforced by the data plane. Gateway \
                 API v1 defines no authentication filter at all, so the check disappears entirely \
                 on migration unless something else performs it.",
        documentation: "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#authentication",
    },
    NonPortableAnnotation {
        annotation: "nginx.ingress.kubernetes.io/auth-url",
        reason: "External (forward) authentication: the data plane sub-requests another service \
                 and gates the request on its answer. Gateway API v1 has no external-authorization \
                 filter, so this gate is not expressible in a portable HTTPRoute.",
        documentation: "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#external-authentication",
    },
    NonPortableAnnotation {
        annotation: "nginx.ingress.kubernetes.io/canary",
        reason: "Marks a second Ingress that shadows an existing one and takes a share of its \
                 traffic. Gateway API expresses a traffic split as several weighted backendRefs \
                 on ONE rule; there is no portable equivalent of a separate object silently \
                 overriding another, so the migration is a restructuring rather than a \
                 translation.",
        documentation: "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#canary",
    },
    NonPortableAnnotation {
        annotation: "nginx.ingress.kubernetes.io/canary-by-cookie",
        reason: "Routes to the canary backend based on a cookie's value. Gateway API v1 matches on \
                 headers, methods and query parameters, but has no cookie match, so this selection \
                 cannot be written portably.",
        documentation: "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#canary",
    },
    NonPortableAnnotation {
        annotation: "nginx.ingress.kubernetes.io/canary-by-header",
        reason: "Routes to the canary backend based on a request header. An HTTPRoute can match a \
                 header, but not as an override of another object's rule, so what survives \
                 migration is a different topology with different precedence.",
        documentation: "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#canary",
    },
    NonPortableAnnotation {
        annotation: "nginx.ingress.kubernetes.io/canary-weight",
        reason: "Sends a percentage of traffic to the canary backend. The closest portable \
                 equivalent -- weighted backendRefs within one HTTPRoute rule -- has partly \
                 different semantics: the weights are relative within a rule rather than a \
                 percentage stolen from another object, and no conformance level guarantees an \
                 exact split.",
        documentation: "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#canary",
    },
    NonPortableAnnotation {
        annotation: "nginx.ingress.kubernetes.io/configuration-snippet",
        reason: "Injects raw NGINX directives into the generated location block. There is no \
                 portable Gateway API counterpart to arbitrary data-plane configuration, and no \
                 way to know from the annotation alone what behavior would be lost.",
        documentation: "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#configuration-snippet",
    },
    NonPortableAnnotation {
        annotation: "nginx.ingress.kubernetes.io/limit-rps",
        reason: "Per-client request rate limiting in the data plane. Gateway API v1 has no rate \
                 limit filter, so the limit is simply absent after migration unless another \
                 component provides one.",
        documentation: "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#rate-limiting",
    },
    NonPortableAnnotation {
        annotation: "nginx.ingress.kubernetes.io/modsecurity-snippet",
        reason: "Configures the ModSecurity web application firewall in the data plane. Gateway \
                 API models no WAF, and the rules are NGINX-module-specific text.",
        documentation: "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#modsecurity",
    },
    NonPortableAnnotation {
        annotation: "nginx.ingress.kubernetes.io/server-snippet",
        reason: "Injects raw NGINX directives into the generated server block, affecting every \
                 location under that host. Same absence of a portable counterpart as \
                 configuration-snippet, with a wider blast radius.",
        documentation: "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#server-snippet",
    },
    NonPortableAnnotation {
        annotation: "nginx.ingress.kubernetes.io/stream-snippet",
        reason: "Injects raw directives into the NGINX stream (TCP/UDP) context. Gateway API's \
                 TCPRoute and UDPRoute are themselves outside the v1 standard channel, and neither \
                 accepts arbitrary configuration.",
        documentation: "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#stream-snippet",
    },
    NonPortableAnnotation {
        annotation: "nginx.ingress.kubernetes.io/use-regex",
        reason: "Treats the Ingress path as a regular expression, including capture groups a \
                 rewrite-target can reference. Gateway API v1's RegularExpression path match is \
                 explicitly implementation-specific rather than core, and no portable rewrite can \
                 reference a captured group.",
        documentation: "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#use-regex",
    },
    NonPortableAnnotation {
        annotation: "nginx.ingress.kubernetes.io/whitelist-source-range",
        reason: "Restricts access to a set of client CIDRs in the data plane. Gateway API v1 has \
                 no source-address match or filter, so the restriction does not survive migration \
                 into a portable HTTPRoute.",
        documentation: "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#whitelist-source-range",
    },
];

/// The catalog entry for `annotation`, or [`None`] if it is not
/// cataloged.
///
/// An exact, case-sensitive match on the whole key. A Kubernetes
/// annotation key is case-sensitive, and a prefix match would flag
/// `nginx.ingress.kubernetes.io/canary-weight-total` under
/// `canary-weight`'s reason -- which is *nearly* right and therefore the
/// worst kind of wrong, since the reason a reader acts on would describe
/// a different annotation.
#[must_use]
pub fn nonportable_annotation(annotation: &str) -> Option<&'static NonPortableAnnotation> {
    NONPORTABLE_INGRESS_ANNOTATIONS
        .iter()
        .find(|entry| entry.annotation == annotation)
}

/// Every cataloged annotation `documents` carry, with the objects
/// carrying each.
///
/// Keyed by the catalog's own `&'static str`, so the map's order is the
/// annotation order [`nonportable_changes`] emits in and the key cannot
/// be a string the catalog does not contain.
///
/// Every document is scanned, not only the `Ingress` ones. A
/// `nginx.ingress.kubernetes.io/*` key on a `Service` does nothing, so
/// there is no false positive to avoid -- and restricting the scan to
/// `kind: Ingress` would silently miss the same annotation on a kind a
/// future controller reads it from.
#[must_use]
pub fn observed_nonportable_annotations(
    documents: &[PlannedObject],
) -> BTreeMap<&'static str, BTreeSet<String>> {
    let mut observed: BTreeMap<&'static str, BTreeSet<String>> = BTreeMap::new();
    for document in documents {
        let Some(annotations) = document
            .object
            .get("metadata")
            .and_then(|metadata| metadata.get("annotations"))
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        for key in annotations.keys() {
            if let Some(entry) = nonportable_annotation(key) {
                observed
                    .entry(entry.annotation)
                    .or_default()
                    .insert(describe_object(document));
            }
        }
    }
    observed
}

/// One planned document, as a change's `detail` names it.
fn describe_object(document: &PlannedObject) -> String {
    match &document.namespace {
        Some(namespace) => format!("{} {}/{}", document.kind, namespace, document.name),
        None => format!("{} {}", document.kind, document.name),
    }
}

/// The non-portable-feature half of [`compare_migration_case`]: which
/// cataloged annotations the baseline manifests carry, and whether the
/// case's author declared each one (ROADMAP Task 8.5 Steps 2 and 3).
///
/// One change per *annotation*, not per object: the finding is "this
/// migration drops feature X", and an `Ingress` and its canary twin both
/// carrying `canary-by-header` is one feature to reason about. Every
/// object carrying it is named in the `detail`, so nothing is hidden.
///
/// `expected` is `true` exactly when the annotation key appears in
/// [`MigrationCaseSpec::expected_nonportable`]. The match is on the key
/// verbatim -- `nginx.ingress.kubernetes.io/canary`, not `canary` --
/// because a short name would be ambiguous across the canary family and
/// because the key is what the `detail` prints, so a user can copy the
/// exact string a declaration needs.
///
/// Emitted in [`NONPORTABLE_INGRESS_ANNOTATIONS`] order, which is
/// annotation order.
#[must_use]
pub fn nonportable_changes(
    case: &MigrationCaseSpec,
    baseline_documents: &[PlannedObject],
) -> Vec<MigrationBehaviorChange> {
    let expected = expected_nonportable_features(case);
    observed_nonportable_annotations(baseline_documents)
        .into_iter()
        .map(|(annotation, objects)| {
            let reason = nonportable_annotation(annotation).map_or("", |entry| entry.reason);
            MigrationBehaviorChange {
                kind: MigrationBehaviorKind::NonPortableFeature,
                detail: format!(
                    "{annotation} on {} has no portable Gateway API equivalent: {reason}",
                    objects.iter().cloned().collect::<Vec<_>>().join(", ")
                ),
                expected: expected.contains(annotation),
            }
        })
        .collect()
}

/// Every non-portability the case declares that its baseline manifests
/// do not actually carry.
///
/// The stale-expectation analog of what `admissionlab-policy` reports
/// for an `expectations.yaml` entry that matched nothing, and the reason
/// it is a function rather than a third field on
/// [`MigrationComparison`]: §1.2 freezes that type at two fields, and --
/// independently of the freeze -- an expectation nobody exercised is a
/// statement about the configuration rather than something Admission Lab
/// observed, so recording it as a [`MigrationBehaviorChange`] would put
/// a fabricated observation in a list of real ones (Global Constraint
/// 15).
///
/// Both causes are reported the same way, deliberately: a feature the
/// migration no longer uses, and a `feature:` string that is not a
/// cataloged annotation key at all (a typo, or a shorthand like
/// `canary`). Distinguishing them would require deciding that the
/// catalog is exhaustive, which it is not and does not claim to be --
/// see [`NONPORTABLE_INGRESS_ANNOTATIONS`].
///
/// Returned in the case's own declaration order, which is deterministic
/// and is the order a user reading their own configuration expects.
#[must_use]
pub fn unmatched_nonportable_expectations<'case>(
    case: &'case MigrationCaseSpec,
    baseline_documents: &[PlannedObject],
) -> Vec<&'case NonPortableFeatureExpectation> {
    let observed = observed_nonportable_annotations(baseline_documents);
    case.expected_nonportable
        .iter()
        .filter(|expectation| !observed.contains_key(expectation.feature.as_str()))
        .collect()
}
