//! Optional per-webhook latency and rejection evidence, read from
//! kube-apiserver's own `/metrics` page around one serial fixture
//! request (Task 3.8).
//!
//! Global Constraint 19 fixes this module's status precisely:
//!
//! > Per-webhook latency is treated as an optional observed signal. When
//! > collected, it comes from isolated kube-apiserver admission webhook
//! > metric deltas around serial fixture requests; absent or ambiguous
//! > metrics never fail a run by themselves.
//!
//! So every failure mode here is a *recoverable absence*, never a run
//! failure: a scrape that cannot be performed is
//! [`MetricsUnavailable`] (a value Task 3.10 turns into "no latency
//! evidence", not an aborted fixture), a malformed sample line is
//! skipped with a [`Diagnostic`] rather than an error, and an ambiguous
//! delta produces `None` from [`WebhookMetricDelta::attributable_latency`]
//! rather than a plausible-looking number. Global Constraint 15 is the
//! reason each of those is an explicit representation instead of a
//! silent fallback.
//!
//! # The three questions this module answers, and the honest answer to each
//!
//! **1. What did the API server report?** [`parse_snapshot`] reads the
//! Prometheus text-format page into an [`AdmissionMetricSnapshot`]: the
//! `_sum`/`_count` of
//! `apiserver_admission_webhook_admission_duration_seconds`, and the
//! `apiserver_admission_webhook_rejection_count` counter, each keyed on
//! the label set the API server actually wrote. Both families are
//! [`Option`]-typed on the snapshot, because "the family was not exported
//! at all" and "the family was exported and this label set was not in it"
//! are different facts (see [`AdmissionMetricSnapshot::durations`]).
//!
//! **2. What changed across the request?** [`diff_metrics`] subtracts one
//! snapshot from another into [`WebhookMetricDelta`]s. The counters it
//! reads are monotonic, so an *absent child of a present family* on the
//! earlier side is a genuine zero (a Prometheus vector only materialises
//! a child the first time it is incremented) -- while an absent *family*
//! is no evidence at all and yields no deltas whatsoever. A counter that
//! went **backwards** (an API-server restart between the two scrapes) is
//! neither: it is [`DeltaEvidence::Unavailable`].
//!
//! **3. Can this delta be attributed to the one fixture request?** Only
//! under the exactly-one rule (Task 3.8 Step 3):
//! [`WebhookMetricDelta::attributable_latency`] returns `Some` if and only
//! if the label set's `_count` rose by exactly `1`. A delta of `0` means
//! the webhook did not run for this fixture; a delta above `1` means
//! background traffic (controllers, kubelet, other admission activity)
//! shared the window and the `_sum` increase cannot be split. Both are
//! `None` -- and in both cases the aggregate [`WebhookMetricDelta`] is
//! still returned, so the evidence is preserved even when the attribution
//! is not available.
//!
//! # `WebhookMetricDelta::rejection_delta` is a `u64`; absence is not zero
//!
//! Task 3.8 froze `rejection_delta: u64`, and `0` is exactly the value a
//! naive implementation would report for "the rejection family was not
//! there". That is the fabrication Global Constraint 15 forbids: a
//! kube-apiserver that has never rejected anything through a webhook does
//! not export `apiserver_admission_webhook_rejection_count` at all, so its
//! absence is indistinguishable, from the counter alone, from a scrape
//! that failed to include it. The frozen field is kept, and every reading
//! of it is qualified by [`WebhookMetricDelta::rejection_evidence`]:
//!
//! - [`RejectionEvidence::Observed`] -- the family was present in **both**
//!   snapshots and this label set's increase was computed from it;
//!   `rejection_delta` is real.
//! - [`RejectionEvidence::Unavailable`] -- the family was missing from at
//!   least one snapshot, or its counter went backwards. `rejection_delta`
//!   is a structural `0` and is **not** a claim that nothing was rejected.
//! - [`RejectionEvidence::NotCounted`] -- this delta's label set has
//!   `rejected` other than `"true"`, and
//!   `apiserver_admission_webhook_rejection_count` carries no `rejected`
//!   label, so nothing is ever attributed to it. `0` here is a fact about
//!   where rejections are counted, not an observation that could have been
//!   non-zero.
//!
//! [`WebhookMetricDelta::observed_rejection_delta`] applies that rule for
//! callers who would rather not match on the variant themselves.
//!
//! # Why the rejection counter is attributed to the `rejected="true"` rows
//!
//! The two families are keyed differently:
//! `apiserver_admission_webhook_admission_duration_seconds` carries
//! `name`/`operation`/`rejected`/`type`, while
//! `apiserver_admission_webhook_rejection_count` carries
//! `name`/`operation`/`type` plus `error_type`/`rejection_code`. A
//! rejection is, by definition, a rejected admission, so its increase is
//! attributed to the `rejected="true"` label set of the same
//! `(name, operation, type)` -- once, not spread across every `rejected`
//! variant, which would double-count it. Increases across `error_type`
//! and `rejection_code` values are summed into that one figure (they
//! partition the same webhook's rejections). A rejection increase with no
//! matching `rejected="true"` duration label set is not dropped: it is
//! emitted as its own delta with `rejected: None` and
//! [`DeltaEvidence::Unavailable`] duration fields, since no duration
//! observation for it was seen.
//!
//! # Parser scope: hand-rolled, deliberately narrow
//!
//! No Prometheus-parser dependency is added (the roadmap's tech-stack
//! line names "Prometheus text parsing", and this workspace's Cargo
//! files justify every entry individually). A real kube-apiserver
//! `/metrics` page is hundreds of families and tens of thousands of
//! lines; this parser reads the metric name first and skips the line
//! outright unless it is one of the two families above, so the label
//! parser only ever runs on lines this project cares about. It handles
//! what that subset requires and no more: `#`-prefixed `HELP`/`TYPE`/
//! comment lines (from which family *presence* is also learned, so a
//! family exported with no children is still known to have been
//! exported), quoted label values with the exposition format's `\\`,
//! `\"`, and `\n` escapes, values in decimal or scientific notation,
//! `+Inf`/`NaN`, an optional trailing timestamp, and `_bucket` lines
//! (skipped -- only `_sum`/`_count` carry what this module needs). It is
//! not a general-purpose exposition-format parser and is not offered as
//! one.
//!
//! # Malformed lines are skipped with a diagnostic, never an error
//!
//! [`parse_snapshot`] is infallible by design. Returning `Err` for one
//! unparseable line would let a single oddity anywhere on a page this
//! project does not control discard an entire optional signal -- and,
//! worse, fail a run, which Global Constraint 19 forbids. But a silently
//! dropped line is exactly how "unavailable" becomes an unearned "zero",
//! so every skip is recorded in
//! [`AdmissionMetricSnapshot::diagnostics`] with the family name, the
//! line number, and the reason. The *content* of the offending line is
//! deliberately not copied into the diagnostic: label values are
//! attacker- and workload-controlled text this project would then carry
//! into a report (Global Constraint 14), and the family plus line number
//! is enough to find it in the raw `.prom` artifact Task 3.10 writes.
//!
//! # Scraping: bounded, authenticated, and never fatal
//!
//! [`KubeMetricsSource`] is the production [`AdmissionMetricsSource`]. It
//! builds a `kube::Client` from the cluster's own isolated kubeconfig
//! (never the operator's ambient `~/.kube/config`, mirroring
//! `admissionlab_fixtures::resources`'s own `client_for`) and issues one
//! authenticated `GET /metrics` through
//! [`kube::Client::request_text`], wrapped in a
//! [`tokio::time::timeout`] so the request is bounded like every other
//! external interaction in this project (Global Constraint 13). Every way
//! that can fail becomes a [`MetricsUnavailable`], whose own
//! documentation states the contract Task 3.10 relies on: it is an
//! absence of an optional signal, not a fixture failure.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use admissionlab_core::{ClusterHandle, Diagnostic, RedactedValue};
use async_trait::async_trait;
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use thiserror::Error;

/// The histogram family carrying per-webhook admission latency. Only its
/// `_sum` and `_count` series are read; `_bucket` series are skipped.
const DURATION_FAMILY: &str = "apiserver_admission_webhook_admission_duration_seconds";

/// The counter family carrying per-webhook rejection counts. Absent
/// entirely from an API server that has never recorded a webhook
/// rejection -- see this module's documentation for why that absence is
/// never read as zero.
const REJECTION_FAMILY: &str = "apiserver_admission_webhook_rejection_count";

/// The `rejected` label value that marks a duration label set as the one
/// a rejection is counted against.
const REJECTED_TRUE: &str = "true";

/// The non-resource path a kube-apiserver serves its Prometheus page on.
const METRICS_PATH: &str = "/metrics";

/// The default bound on one `/metrics` scrape (Global Constraint 13).
///
/// Generous on purpose: a kube-apiserver `/metrics` page is large and is
/// rendered on demand, so a too-tight bound would turn a slow-but-healthy
/// API server into a permanent "metrics unavailable". Also bounded on
/// purpose: this scrape brackets every single fixture request, so an
/// unbounded one would stall a whole run on one wedged connection.
pub const DEFAULT_SCRAPE_TIMEOUT: Duration = Duration::from_secs(15);

/// The largest counter value an `f64` sample can carry without losing
/// integer precision (2^53). A `_count` or rejection counter at or above
/// this is rejected as unrepresentable rather than rounded to a
/// neighbouring integer -- unreachable in practice for an admission
/// counter, and cheaper to refuse than to explain.
const MAX_EXACT_COUNTER: f64 = 9_007_199_254_740_992.0;

/// The label set one
/// `apiserver_admission_webhook_admission_duration_seconds` series was
/// recorded under.
///
/// The four fields named by Task 3.8 Step 1 are pulled out by name;
/// anything else the API server wrote stays verbatim in
/// [`WebhookMetricKey::other_labels`] rather than being discarded, so two
/// series that differ only in a label this project does not know about
/// can never collapse into one and silently sum together.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WebhookMetricKey {
    /// The `name` label: the webhook's own name, for example
    /// `mutate.test.admissionlab.io`.
    pub name: String,
    /// The `operation` label, for example `CREATE`.
    pub operation: String,
    /// The `rejected` label, verbatim (`"true"`/`"false"` in practice).
    /// Kept as the raw string rather than parsed into a `bool`: a value
    /// this project did not anticipate must stay distinguishable rather
    /// than fall into one of the two branches (Global Constraint 15).
    pub rejected: String,
    /// The `type` label (`"admit"`/`"validate"` in practice). Named
    /// `webhook_type` because `type` is a Rust keyword.
    pub webhook_type: String,
    /// Every other label on the series, verbatim. Empty for the label
    /// sets a current kube-apiserver writes.
    pub other_labels: BTreeMap<String, String>,
}

/// The label set one `apiserver_admission_webhook_rejection_count` series
/// was recorded under. `error_type` and `rejection_code` land in
/// [`RejectionKey::other_labels`]; [`diff_metrics`] sums their increases
/// per `(name, operation, type)` -- see this module's documentation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RejectionKey {
    /// The `name` label: the webhook's own name.
    pub name: String,
    /// The `operation` label.
    pub operation: String,
    /// The `type` label. Named `webhook_type` because `type` is a Rust
    /// keyword.
    pub webhook_type: String,
    /// Every other label on the series, verbatim -- `error_type` and
    /// `rejection_code` for a current kube-apiserver.
    pub other_labels: BTreeMap<String, String>,
}

/// The `_sum` and `_count` observed for one duration label set.
///
/// Both are [`Option`] because a page can carry one without the other (a
/// truncated scrape, or a `_sum` line this parser had to skip). Neither
/// is defaulted to `0`: a missing half makes the whole label set's delta
/// [`DeltaEvidence::Unavailable`], never a delta computed against a
/// fabricated zero.
#[derive(Debug, Clone, PartialEq)]
pub struct DurationSample {
    /// The `..._sum` value in seconds, if a `_sum` line was read for this
    /// label set.
    pub sum: Option<f64>,
    /// The `..._count` value, if a `_count` line was read for this label
    /// set.
    pub count: Option<u64>,
}

/// One kube-apiserver `/metrics` page, reduced to the two admission
/// families this project reads.
///
/// Carries no `Default`, matching this crate's rule for every
/// evidence-bearing type (see [`crate::trace::TraceEvidence`]): a
/// snapshot must come from a real scrape ([`parse_snapshot`]) or from an
/// explicit construction that states, field by field, what was and was
/// not present. A derived `Default` would produce "both families absent,
/// no diagnostics", which reads as a successful scrape of an API server
/// exporting nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct AdmissionMetricSnapshot {
    /// Every `apiserver_admission_webhook_admission_duration_seconds`
    /// series read from the page.
    ///
    /// `None` and `Some(empty map)` are deliberately different facts:
    /// `None` means the family was not on the page at all (no `HELP`,
    /// no `TYPE`, no sample), so this snapshot carries *no evidence*
    /// about webhook durations and [`diff_metrics`] will make no claims
    /// from it. `Some` means the family was exported, so a label set
    /// missing from the map genuinely had no observations yet -- which
    /// [`diff_metrics`] does treat as zero.
    pub durations: Option<BTreeMap<WebhookMetricKey, DurationSample>>,
    /// Every `apiserver_admission_webhook_rejection_count` series read
    /// from the page, with the same `None`-versus-empty distinction as
    /// [`AdmissionMetricSnapshot::durations`]. `None` is the common case
    /// on a cluster where no webhook has ever rejected anything.
    pub rejections: Option<BTreeMap<RejectionKey, u64>>,
    /// One entry per line that named one of the two families above but
    /// could not be read (see this module's documentation). A non-empty
    /// vector means this snapshot is known to be incomplete; an empty one
    /// means every line of both families parsed.
    pub diagnostics: Vec<Diagnostic>,
}

/// Whether a [`WebhookMetricDelta`]'s duration fields are a real measured
/// increase.
///
/// Two variants, not three: everything that is not a real increase is
/// [`DeltaEvidence::Unavailable`], because the callers this serves need
/// exactly one bit ("may I use these numbers?"), and a finer taxonomy of
/// *why* would invite a caller to treat some of the failure modes as
/// usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaEvidence {
    /// Both snapshots carried usable totals for this label set (or the
    /// earlier one legitimately had no child of a family it *was*
    /// exporting), and the counters did not go backwards.
    /// [`WebhookMetricDelta::request_count_delta`] and
    /// [`WebhookMetricDelta::duration_sum_delta`] are real.
    Observed,
    /// The increase could not be computed: the later snapshot reported
    /// lower totals than the earlier one (an API-server restart or metric
    /// reset between the two scrapes), one of the `_sum`/`_count` halves
    /// was missing, or the label set vanished between scrapes. The
    /// numeric delta fields are structural `0`/`0.0` placeholders and are
    /// **not** evidence that nothing happened (Global Constraint 15).
    Unavailable,
}

/// Whether a [`WebhookMetricDelta`]'s [`WebhookMetricDelta::rejection_delta`]
/// is a real measured increase. See this module's documentation for the
/// full rule and for why the frozen `u64` field needs this qualifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionEvidence {
    /// `apiserver_admission_webhook_rejection_count` was present in both
    /// snapshots and this label set's increase was computed from it.
    Observed,
    /// The rejection family was absent from at least one snapshot, or its
    /// counter went backwards. `rejection_delta`'s `0` is a placeholder,
    /// never a claim that nothing was rejected.
    Unavailable,
    /// This delta's `rejected` label is not `"true"`, and the rejection
    /// counter carries no `rejected` label, so no rejection is ever
    /// attributed here. `rejection_delta` is structurally `0`.
    NotCounted,
}

/// What changed, for one webhook label set, between two
/// [`AdmissionMetricSnapshot`]s.
///
/// `webhook`, `request_count_delta`, `duration_sum_delta`, and
/// `rejection_delta` are the field names Task 3.8 froze; the rest were
/// added by this task and may be extended, but none of the four may be
/// renamed. The two `*_evidence` fields are not decoration: the four
/// frozen fields are plain numbers with no room for "unknown", so
/// reading any of them without first checking the matching evidence
/// variant will eventually read a placeholder `0` as an observation
/// (Global Constraint 15). [`WebhookMetricDelta::attributable_latency`],
/// [`WebhookMetricDelta::observed_request_count_delta`], and
/// [`WebhookMetricDelta::observed_rejection_delta`] do that check for
/// callers who want it done once, correctly.
///
/// Carries no `Default` for the same reason
/// [`AdmissionMetricSnapshot`] does not.
#[derive(Debug, Clone, PartialEq)]
pub struct WebhookMetricDelta {
    /// The webhook's `name` label.
    pub webhook: String,
    /// The `operation` label this delta is for.
    pub operation: String,
    /// The `type` label this delta is for (`"admit"`/`"validate"`).
    pub webhook_type: String,
    /// The `rejected` label this delta is for, verbatim. `None` for a
    /// delta built from a rejection-counter increase that had no matching
    /// duration label set at all -- see this module's documentation.
    pub rejected: Option<String>,
    /// Any labels beyond the well-known ones, carried through from the
    /// key this delta was computed for.
    pub other_labels: BTreeMap<String, String>,
    /// How many admission-webhook calls this label set recorded between
    /// the two snapshots. Meaningful only when
    /// [`WebhookMetricDelta::duration_evidence`] is
    /// [`DeltaEvidence::Observed`].
    pub request_count_delta: u64,
    /// How much total webhook time, in seconds, this label set
    /// accumulated between the two snapshots. Meaningful only when
    /// [`WebhookMetricDelta::duration_evidence`] is
    /// [`DeltaEvidence::Observed`]; and attributable to a single fixture
    /// only under the exactly-one rule -- use
    /// [`WebhookMetricDelta::attributable_latency`] rather than dividing
    /// this by anything.
    pub duration_sum_delta: f64,
    /// Whether the two fields above are a real measured increase.
    pub duration_evidence: DeltaEvidence,
    /// How many rejections `apiserver_admission_webhook_rejection_count`
    /// recorded for this webhook/operation/type between the two
    /// snapshots. Meaningful only when
    /// [`WebhookMetricDelta::rejection_evidence`] says so.
    pub rejection_delta: u64,
    /// Whether [`WebhookMetricDelta::rejection_delta`] is a real measured
    /// increase, a placeholder for missing evidence, or a structural zero.
    pub rejection_evidence: RejectionEvidence,
}

impl WebhookMetricDelta {
    /// This webhook's latency for the one fixture request the two
    /// snapshots bracket -- **only** when that attribution is sound.
    ///
    /// Returns `Some` if and only if all of the following hold:
    ///
    /// 1. [`WebhookMetricDelta::duration_evidence`] is
    ///    [`DeltaEvidence::Observed`], so the numbers are real at all;
    /// 2. [`WebhookMetricDelta::request_count_delta`] is exactly `1`
    ///    (Task 3.8 Step 3) -- the window contains exactly one invocation
    ///    of this webhook for this label set, so the whole `_sum`
    ///    increase belongs to it;
    /// 3. the `_sum` increase converts to a [`Duration`] at all
    ///    (non-negative, finite, in range).
    ///
    /// `None` in every other case, including a count delta of `0` (the
    /// webhook did not run for this fixture) and a count delta above `1`
    /// (background traffic shared the window, so the increase cannot be
    /// split). Task 3.10 maps this straight onto
    /// [`crate::trace::WebhookInvocation::latency`], whose own
    /// documentation explains why an unmeasured latency must stay `None`
    /// rather than become a fabricated `0`.
    #[must_use]
    pub fn attributable_latency(&self) -> Option<Duration> {
        if self.duration_evidence != DeltaEvidence::Observed || self.request_count_delta != 1 {
            return None;
        }
        // `Duration::from_secs_f64` panics on a negative, NaN, or
        // out-of-range value; the fallible form returns `Err` instead, so
        // a nonsensical `_sum` increase reads as "unknown" rather than
        // taking the process down.
        Duration::try_from_secs_f64(self.duration_sum_delta).ok()
    }

    /// [`WebhookMetricDelta::request_count_delta`] when it is a real
    /// measured increase, `None` when
    /// [`WebhookMetricDelta::duration_evidence`] says it is a placeholder.
    #[must_use]
    pub fn observed_request_count_delta(&self) -> Option<u64> {
        match self.duration_evidence {
            DeltaEvidence::Observed => Some(self.request_count_delta),
            DeltaEvidence::Unavailable => None,
        }
    }

    /// [`WebhookMetricDelta::rejection_delta`] when this project can
    /// honestly state a number, `None` when it cannot.
    ///
    /// `Some` for both [`RejectionEvidence::Observed`] (a measured
    /// increase) and [`RejectionEvidence::NotCounted`] (a label set the
    /// rejection counter structurally never attributes to, so `0` is a
    /// fact rather than a guess). `None` only for
    /// [`RejectionEvidence::Unavailable`], where the `0` in the field is
    /// a placeholder for evidence this run does not have.
    #[must_use]
    pub fn observed_rejection_delta(&self) -> Option<u64> {
        match self.rejection_evidence {
            RejectionEvidence::Observed | RejectionEvidence::NotCounted => {
                Some(self.rejection_delta)
            }
            RejectionEvidence::Unavailable => None,
        }
    }
}

/// Reads one kube-apiserver `/metrics` page into an
/// [`AdmissionMetricSnapshot`].
///
/// Infallible: unrelated metric families are skipped silently, and a line
/// belonging to one of the two admission families that cannot be read is
/// skipped with an entry in [`AdmissionMetricSnapshot::diagnostics`]. See
/// this module's documentation for the parser's exact scope and for why
/// this never returns an error.
#[must_use]
pub fn parse_snapshot(text: &str) -> AdmissionMetricSnapshot {
    let mut snapshot = AdmissionMetricSnapshot {
        durations: None,
        rejections: None,
        diagnostics: Vec::new(),
    };
    for (index, raw_line) in text.lines().enumerate() {
        parse_line(&mut snapshot, index + 1, raw_line.trim());
    }
    snapshot
}

/// Computes what changed between `before` and `after`, one
/// [`WebhookMetricDelta`] per duration label set present in either
/// snapshot, plus one per rejection increase that had no matching
/// `rejected="true"` duration label set.
///
/// Returns an empty vector -- making no claims at all -- when either
/// snapshot lacks the duration family entirely, since a delta needs two
/// observations and one side has none (Global Constraint 15).
///
/// Deterministic: label sets are compared and emitted in
/// [`WebhookMetricKey`] order, with any rejection-only deltas appended in
/// their own key order, so two runs over the same pages produce
/// byte-identical output (Global Constraint 7).
#[must_use]
pub fn diff_metrics(
    before: &AdmissionMetricSnapshot,
    after: &AdmissionMetricSnapshot,
) -> Vec<WebhookMetricDelta> {
    let (Some(before_durations), Some(after_durations)) =
        (before.durations.as_ref(), after.durations.as_ref())
    else {
        return Vec::new();
    };
    let rejections = rejection_deltas(before, after);

    let mut deltas = Vec::new();
    let mut attributed: BTreeSet<RejectionTriple> = BTreeSet::new();
    let keys: BTreeSet<&WebhookMetricKey> = before_durations
        .keys()
        .chain(after_durations.keys())
        .collect();
    for key in keys {
        let (request_count_delta, duration_sum_delta, duration_evidence) =
            duration_delta(before_durations.get(key), after_durations.get(key));
        let triple = key.rejection_triple();
        let (rejection_delta, rejection_evidence) = if key.rejected == REJECTED_TRUE {
            attributed.insert(triple.clone());
            rejection_for(rejections.as_ref(), &triple)
        } else {
            (0, RejectionEvidence::NotCounted)
        };
        deltas.push(WebhookMetricDelta {
            webhook: key.name.clone(),
            operation: key.operation.clone(),
            webhook_type: key.webhook_type.clone(),
            rejected: Some(key.rejected.clone()),
            other_labels: key.other_labels.clone(),
            request_count_delta,
            duration_sum_delta,
            duration_evidence,
            rejection_delta,
            rejection_evidence,
        });
    }
    deltas.extend(orphan_rejection_deltas(rejections.as_ref(), &attributed));
    deltas
}

/// The contract Task 3.10 calls to bracket one fixture request with two
/// `/metrics` scrapes. See [`KubeMetricsSource`] for the one production
/// implementation.
///
/// `Send + Sync` for the same reason every other async trait in this
/// workspace is (see `admissionlab_core::ClusterManager`'s
/// documentation): baseline and candidate clusters are driven
/// concurrently, so a source shared across both must be usable from more
/// than one task at once. Within a single cluster the two scrapes stay
/// serial, because the whole exactly-one attribution rule depends on the
/// window between them containing one fixture request (Global Constraint
/// 17).
#[async_trait]
pub trait AdmissionMetricsSource: Send + Sync {
    /// Scrapes `cluster`'s API server and returns what it reported.
    ///
    /// # Errors
    ///
    /// Returns [`MetricsUnavailable`] if no page could be obtained at
    /// all. Per Global Constraint 19 a caller must treat that as an
    /// absent optional signal -- capture the fixture with no latency
    /// evidence -- never as a fixture or run failure. See
    /// [`MetricsUnavailable`]'s own documentation.
    async fn snapshot(
        &self,
        cluster: &ClusterHandle,
    ) -> Result<AdmissionMetricSnapshot, MetricsUnavailable>;
}

/// No `/metrics` page could be obtained from an API server.
///
/// **This is never a run failure.** Global Constraint 19 makes
/// per-webhook latency an optional observed signal, so every variant here
/// means "this run has no metric evidence for this fixture", which Task
/// 3.10 records as an unknown latency
/// ([`crate::trace::WebhookInvocation::latency`] stays `None`) and moves
/// on. A caller that propagates one of these as a fixture failure has
/// turned an optional signal into a required one.
///
/// Distinct from a *parsed but unusable* page: a page that was retrieved
/// and simply carried no admission families is an ordinary
/// [`AdmissionMetricSnapshot`] with `None` families, not an error here.
#[derive(Debug, Error)]
pub enum MetricsUnavailable {
    /// `cluster`'s kubeconfig could not be turned into a usable
    /// `kube::Client` (missing, unreadable, or malformed).
    #[error("cannot scrape metrics from cluster {cluster}: building a client failed: {reason}")]
    Client {
        /// The cluster whose metrics could not be scraped.
        cluster: String,
        /// The underlying `kube` error, rendered.
        reason: String,
    },
    /// The `GET /metrics` request could not be built, did not reach the
    /// API server, or came back as a non-2xx response.
    #[error("cannot scrape metrics from cluster {cluster}: the /metrics request failed: {reason}")]
    Request {
        /// The cluster whose metrics could not be scraped.
        cluster: String,
        /// The underlying error, rendered.
        reason: String,
    },
    /// The request did not finish within its bound (Global Constraint
    /// 13). Reported separately from
    /// [`MetricsUnavailable::Request`] because a timeout says something
    /// different about the cluster -- reachable but slow -- and because
    /// a run that hits this repeatedly should raise the bound rather
    /// than chase a transport error that is not there.
    #[error(
        "cannot scrape metrics from cluster {cluster}: /metrics did not respond within {timeout:?}"
    )]
    Timeout {
        /// The cluster whose metrics could not be scraped.
        cluster: String,
        /// The bound that elapsed.
        timeout: Duration,
    },
}

/// The one production [`AdmissionMetricsSource`]: an authenticated
/// `GET /metrics` against the cluster's own API server, bounded by a
/// timeout. See this module's documentation ("Scraping").
#[derive(Debug, Clone, Copy)]
pub struct KubeMetricsSource {
    /// The bound on each scrape.
    timeout: Duration,
}

impl KubeMetricsSource {
    /// Creates a source using [`DEFAULT_SCRAPE_TIMEOUT`]. Carries no
    /// state beyond that bound -- every scrape resolves a fresh
    /// `kube::Client` from the cluster's own kubeconfig, exactly as
    /// [`crate::execute::KubeAdmissionExecutor`] does.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            timeout: DEFAULT_SCRAPE_TIMEOUT,
        }
    }

    /// Creates a source bounding each scrape by `timeout` instead of
    /// [`DEFAULT_SCRAPE_TIMEOUT`].
    #[must_use]
    pub const fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for KubeMetricsSource {
    /// Hand-written rather than derived: a derived `Default` would give
    /// `timeout: Duration::ZERO`, which fails every scrape instantly.
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AdmissionMetricsSource for KubeMetricsSource {
    async fn snapshot(
        &self,
        cluster: &ClusterHandle,
    ) -> Result<AdmissionMetricSnapshot, MetricsUnavailable> {
        let text = scrape_metrics_text(cluster, self.timeout).await?;
        Ok(parse_snapshot(&text))
    }
}

/// Fetches `cluster`'s raw `/metrics` page, bounded by `timeout`.
///
/// Public alongside [`AdmissionMetricsSource::snapshot`] because Task
/// 3.10's artifact bundle writes the page verbatim
/// (`raw/<side>/<fixture-id>/metrics-before.prom`) *and* parses it; this
/// lets it do both from one scrape rather than making the parsed
/// snapshot carry a copy of a page that is routinely hundreds of
/// kilobytes.
///
/// # Errors
///
/// Returns [`MetricsUnavailable::Client`] if `cluster`'s kubeconfig could
/// not be turned into a usable client, or any of
/// [`scrape_metrics_text_with_client`]'s own error cases. Never a run
/// failure -- see [`MetricsUnavailable`].
pub async fn scrape_metrics_text(
    cluster: &ClusterHandle,
    timeout: Duration,
) -> Result<String, MetricsUnavailable> {
    let client = client_for(cluster)
        .await
        .map_err(|source| MetricsUnavailable::Client {
            cluster: cluster.spec.name.clone(),
            reason: source.to_string(),
        })?;
    scrape_metrics_text_with_client(client, &cluster.spec.name, timeout).await
}

/// [`scrape_metrics_text`]'s offline-testable core: given an
/// already-built `client`, issues the bounded `GET /metrics` and returns
/// the page. `cluster_name` only labels a [`MetricsUnavailable`] if this
/// fails; it is never used to build or look up `client`, so this function
/// never touches a kubeconfig or the filesystem -- the same seam
/// `admissionlab_fixtures::execute::dry_run_create_with_client` and
/// [`crate::execute::execute_create_with_client`] expose, and what
/// `tests/metrics.rs` drives against a `tower_test::mock`-backed
/// `Client`.
///
/// # Errors
///
/// Returns [`MetricsUnavailable::Timeout`] if the request did not finish
/// within `timeout`, and [`MetricsUnavailable::Request`] if the request
/// could not be built, the exchange failed at the transport level, the
/// API server answered non-2xx (`kube::Client::request_text` turns that
/// into an error), or the body was not UTF-8.
pub async fn scrape_metrics_text_with_client(
    client: Client,
    cluster_name: &str,
    timeout: Duration,
) -> Result<String, MetricsUnavailable> {
    let request = http::Request::builder()
        .method(http::Method::GET)
        .uri(METRICS_PATH)
        // kube-apiserver content-negotiates its metrics page; asking for
        // the text exposition format explicitly keeps this parser's input
        // stable rather than depending on what the server would pick.
        .header(http::header::ACCEPT, "text/plain")
        .body(Vec::new())
        .map_err(|source| MetricsUnavailable::Request {
            cluster: cluster_name.to_string(),
            reason: source.to_string(),
        })?;

    match tokio::time::timeout(timeout, client.request_text(request)).await {
        Err(_elapsed) => Err(MetricsUnavailable::Timeout {
            cluster: cluster_name.to_string(),
            timeout,
        }),
        Ok(Err(source)) => Err(MetricsUnavailable::Request {
            cluster: cluster_name.to_string(),
            reason: source.to_string(),
        }),
        Ok(Ok(text)) => Ok(text),
    }
}

/// Builds a `kube::Client` for `cluster` from its own isolated
/// kubeconfig -- never the operator's ambient `~/.kube/config`.
///
/// Four lines duplicated from `admissionlab_fixtures::resources::client_for`
/// rather than reused: that function is `pub(crate)` to its own crate, so
/// it is not reachable from here at all, and widening its visibility to
/// export a four-line kubeconfig read would grow this workspace's public
/// surface for no benefit. `admissionlab_installer::readiness::client_for`
/// carries the same four lines for the same reason, and documents the
/// same trade-off.
async fn client_for(cluster: &ClusterHandle) -> Result<Client, kube::Error> {
    let kubeconfig = Kubeconfig::read_from(&cluster.kubeconfig)?;
    let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default()).await?;
    Client::try_from(config)
}

// =========================================================================
// Parsing internals
// =========================================================================

/// Which series a matched sample line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SampleField {
    /// `apiserver_admission_webhook_admission_duration_seconds_sum`.
    DurationSum,
    /// `apiserver_admission_webhook_admission_duration_seconds_count`.
    DurationCount,
    /// `apiserver_admission_webhook_rejection_count`.
    Rejection,
}

/// Reads one already-trimmed line into `snapshot`.
fn parse_line(snapshot: &mut AdmissionMetricSnapshot, line_number: usize, line: &str) {
    if line.is_empty() {
        return;
    }
    if let Some(comment) = line.strip_prefix('#') {
        note_declared_family(snapshot, comment);
        return;
    }

    let mut cursor = Cursor { rest: line };
    let metric = cursor.take_while(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':');
    let Some(field) = classify_metric(metric) else {
        return;
    };
    let diagnostic = match parse_sample(&mut cursor) {
        Err(reason) => Some(malformed(line_number, metric, reason)),
        Ok((labels, value)) => record(snapshot, line_number, metric, field, labels, value),
    };
    if let Some(diagnostic) = diagnostic {
        snapshot.diagnostics.push(diagnostic);
    }
}

/// Notes that a `# HELP <family>` or `# TYPE <family>` line named one of
/// the two families this module reads, so a family that is exported but
/// currently has no children is still recorded as *present* rather than
/// absent -- the distinction
/// [`AdmissionMetricSnapshot::durations`] documents.
fn note_declared_family(snapshot: &mut AdmissionMetricSnapshot, comment: &str) {
    let mut tokens = comment.split_ascii_whitespace();
    let Some(keyword) = tokens.next() else {
        return;
    };
    if keyword != "HELP" && keyword != "TYPE" {
        return;
    }
    match tokens.next() {
        Some(DURATION_FAMILY) => {
            snapshot.durations.get_or_insert_with(BTreeMap::new);
        }
        Some(REJECTION_FAMILY) => {
            snapshot.rejections.get_or_insert_with(BTreeMap::new);
        }
        _ => {}
    }
}

/// Maps a metric name to the series it is, or `None` for everything this
/// module ignores -- including this family's own `_bucket` series, which
/// carry per-bucket counts (and an `le` label) that the sum/count
/// attribution rule has no use for.
fn classify_metric(metric: &str) -> Option<SampleField> {
    if metric == REJECTION_FAMILY {
        return Some(SampleField::Rejection);
    }
    let suffix = metric.strip_prefix(DURATION_FAMILY)?;
    match suffix {
        "_sum" => Some(SampleField::DurationSum),
        "_count" => Some(SampleField::DurationCount),
        _ => None,
    }
}

/// Stores one parsed sample, returning a [`Diagnostic`] instead if it
/// could not be keyed or its value could not be represented.
fn record(
    snapshot: &mut AdmissionMetricSnapshot,
    line_number: usize,
    metric: &str,
    field: SampleField,
    labels: BTreeMap<String, String>,
    value: f64,
) -> Option<Diagnostic> {
    match field {
        SampleField::Rejection => {
            let Some(key) = rejection_key(labels) else {
                return Some(malformed(line_number, metric, MISSING_LABELS));
            };
            let Some(count) = counter_value(value) else {
                return Some(malformed(line_number, metric, UNREPRESENTABLE_COUNTER));
            };
            // First writer wins, matching `record_duration` below, so a
            // page that repeats a series is reported rather than having
            // one of its two values quietly chosen.
            match snapshot
                .rejections
                .get_or_insert_with(BTreeMap::new)
                .entry(key)
            {
                Entry::Vacant(slot) => {
                    slot.insert(count);
                    None
                }
                Entry::Occupied(_) => Some(duplicate(line_number, metric)),
            }
        }
        SampleField::DurationSum | SampleField::DurationCount => {
            record_duration(snapshot, line_number, metric, field, labels, value)
        }
    }
}

/// [`record`]'s duration half, split out so neither function runs past
/// the length this workspace's clippy configuration allows.
fn record_duration(
    snapshot: &mut AdmissionMetricSnapshot,
    line_number: usize,
    metric: &str,
    field: SampleField,
    labels: BTreeMap<String, String>,
    value: f64,
) -> Option<Diagnostic> {
    let Some(key) = webhook_key(labels) else {
        return Some(malformed(line_number, metric, MISSING_LABELS));
    };
    // Both halves are validated *before* the entry is created, so a
    // rejected value never leaves a half-populated label set behind.
    let count = if field == SampleField::DurationCount {
        match counter_value(value) {
            None => return Some(malformed(line_number, metric, UNREPRESENTABLE_COUNTER)),
            Some(count) => Some(count),
        }
    } else {
        if !value.is_finite() {
            return Some(malformed(line_number, metric, NON_FINITE_SUM));
        }
        None
    };

    let sample = snapshot
        .durations
        .get_or_insert_with(BTreeMap::new)
        .entry(key)
        .or_insert(DurationSample {
            sum: None,
            count: None,
        });
    // First writer wins, so a duplicated line is reported rather than
    // silently overwriting an already-observed value with a second one.
    match count {
        Some(count) if sample.count.is_none() => sample.count = Some(count),
        None if sample.sum.is_none() => sample.sum = Some(value),
        _ => return Some(duplicate(line_number, metric)),
    }
    None
}

/// The reason text for a sample line missing one of the labels its family
/// is keyed on.
const MISSING_LABELS: &str = "the sample is missing one of the labels this family is keyed on";
/// The reason text for a counter value no `u64` can carry exactly.
const UNREPRESENTABLE_COUNTER: &str =
    "the counter value is negative, non-finite, or too large to represent exactly";
/// The reason text for a histogram `_sum` that is not a finite number.
const NON_FINITE_SUM: &str = "the histogram sum is not a finite number";

/// Builds the "skipped a line" diagnostic. See this module's
/// documentation for why the line's own text is deliberately not
/// included.
fn malformed(line_number: usize, metric: &str, reason: &str) -> Diagnostic {
    Diagnostic {
        code: "metrics.malformed_line".to_string(),
        message: format!("skipped `{metric}` sample on /metrics line {line_number}: {reason}"),
        context: BTreeMap::from([
            (
                "metric".to_string(),
                RedactedValue::Public(metric.to_string()),
            ),
            (
                "line".to_string(),
                RedactedValue::Public(line_number.to_string()),
            ),
            (
                "reason".to_string(),
                RedactedValue::Public(reason.to_string()),
            ),
        ]),
    }
}

/// Builds the "this label set was already seen" diagnostic. A
/// well-formed exposition page never repeats a series, so this records a
/// page that is not what it claims to be rather than quietly keeping one
/// of the two values.
fn duplicate(line_number: usize, metric: &str) -> Diagnostic {
    Diagnostic {
        code: "metrics.duplicate_sample".to_string(),
        message: format!(
            "ignored a repeated `{metric}` sample for an already-observed label set on /metrics \
             line {line_number}; the first value was kept"
        ),
        context: BTreeMap::from([
            (
                "metric".to_string(),
                RedactedValue::Public(metric.to_string()),
            ),
            (
                "line".to_string(),
                RedactedValue::Public(line_number.to_string()),
            ),
        ]),
    }
}

/// Pulls the four well-known duration labels out of a parsed label set,
/// leaving the rest in [`WebhookMetricKey::other_labels`]. `None` if any
/// of the four is missing -- the sample cannot be keyed at all, so it is
/// skipped rather than filed under a fabricated label value.
fn webhook_key(mut labels: BTreeMap<String, String>) -> Option<WebhookMetricKey> {
    let name = labels.remove("name")?;
    let operation = labels.remove("operation")?;
    let rejected = labels.remove("rejected")?;
    let webhook_type = labels.remove("type")?;
    Some(WebhookMetricKey {
        name,
        operation,
        rejected,
        webhook_type,
        other_labels: labels,
    })
}

/// [`webhook_key`]'s counterpart for the rejection family, which carries
/// no `rejected` label.
fn rejection_key(mut labels: BTreeMap<String, String>) -> Option<RejectionKey> {
    let name = labels.remove("name")?;
    let operation = labels.remove("operation")?;
    let webhook_type = labels.remove("type")?;
    Some(RejectionKey {
        name,
        operation,
        webhook_type,
        other_labels: labels,
    })
}

/// Converts a Prometheus sample value into a counter, or `None` if it is
/// not one this project can carry exactly.
// The guard immediately above the cast rules out every case the two lints
// warn about: `raw` is finite, non-negative, and below 2^53 (itself far
// below `u64::MAX`), so the conversion is exact and sign-preserving.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn counter_value(raw: f64) -> Option<u64> {
    if !raw.is_finite() || raw < 0.0 || raw >= MAX_EXACT_COUNTER {
        return None;
    }
    Some(raw.round() as u64)
}

/// A borrow-and-advance cursor over the remainder of one sample line.
/// Only ever splits at ASCII delimiters, so the `&str` slicing below can
/// never land inside a multi-byte character in a label value.
struct Cursor<'a> {
    /// The unread remainder of the line.
    rest: &'a str,
}

impl<'a> Cursor<'a> {
    /// The next character, without consuming it.
    fn peek(&self) -> Option<char> {
        self.rest.chars().next()
    }

    /// Consumes and returns the next character.
    fn bump(&mut self) -> Option<char> {
        let mut chars = self.rest.chars();
        let next = chars.next()?;
        self.rest = chars.as_str();
        Some(next)
    }

    /// Consumes `expected` if it is next, reporting whether it was.
    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Consumes and returns the longest prefix every character of which
    /// satisfies `predicate`.
    fn take_while(&mut self, predicate: impl Fn(char) -> bool) -> &'a str {
        let end = self.rest.find(|c| !predicate(c)).unwrap_or(self.rest.len());
        let (taken, rest) = self.rest.split_at(end);
        self.rest = rest;
        taken
    }

    /// Consumes any run of spaces and tabs.
    fn skip_spaces(&mut self) {
        self.rest = self.rest.trim_start_matches([' ', '\t']);
    }
}

/// Reads a sample line's optional label block and its value, starting
/// immediately after the metric name.
///
/// `Err` carries a short, fixed reason string for the diagnostic; it is
/// never derived from the line's own content (see this module's
/// documentation).
fn parse_sample(cursor: &mut Cursor<'_>) -> Result<(BTreeMap<String, String>, f64), &'static str> {
    let labels = if cursor.peek() == Some('{') {
        parse_labels(cursor)?
    } else {
        BTreeMap::new()
    };

    let mut fields = cursor.rest.split_ascii_whitespace();
    let value_token = fields.next().ok_or("the sample carries no value")?;
    let value: f64 = value_token
        .parse()
        .map_err(|_| "the sample value is not a number")?;
    // An optional trailing timestamp is part of the exposition format;
    // kube-apiserver does not write one, but a page that does must not be
    // read as malformed. The value is parsed only to confirm it is one,
    // then discarded: nothing here needs the server's own clock.
    if let Some(timestamp) = fields.next() {
        timestamp
            .parse::<f64>()
            .map_err(|_| "the sample timestamp is not a number")?;
    }
    if fields.next().is_some() {
        return Err("the sample carries unexpected trailing text");
    }
    Ok((labels, value))
}

/// Reads a `{name="value",...}` block, leaving `cursor` just past the
/// closing brace.
fn parse_labels(cursor: &mut Cursor<'_>) -> Result<BTreeMap<String, String>, &'static str> {
    if !cursor.eat('{') {
        return Err("the label block does not start with `{`");
    }
    let mut labels = BTreeMap::new();
    loop {
        cursor.skip_spaces();
        if cursor.eat('}') {
            return Ok(labels);
        }
        let name = cursor.take_while(|c| c.is_ascii_alphanumeric() || c == '_');
        if name.is_empty() {
            return Err("a label name is missing or contains unexpected characters");
        }
        cursor.skip_spaces();
        if !cursor.eat('=') {
            return Err("a label name is not followed by `=`");
        }
        cursor.skip_spaces();
        if !cursor.eat('"') {
            return Err("a label value is not quoted");
        }
        let value = parse_label_value(cursor)?;
        if labels.insert(name.to_string(), value).is_some() {
            return Err("the label block repeats a label name");
        }
        cursor.skip_spaces();
        if cursor.eat(',') {
            continue;
        }
        if cursor.eat('}') {
            return Ok(labels);
        }
        return Err("a label value is not followed by `,` or `}`");
    }
}

/// Reads a quoted label value, the opening quote already consumed.
///
/// The exposition format defines exactly three escapes inside a label
/// value -- `\\`, `\"`, and `\n`. Anything else after a backslash is kept
/// as the backslash plus that character, verbatim: inventing an
/// interpretation for an escape the format does not define would change
/// the label value this project keys on, and dropping the backslash would
/// silently merge two distinct label sets.
fn parse_label_value(cursor: &mut Cursor<'_>) -> Result<String, &'static str> {
    let mut value = String::new();
    loop {
        match cursor.bump() {
            None => return Err("a label value is not terminated"),
            Some('"') => return Ok(value),
            Some('\\') => match cursor.bump() {
                None => return Err("a label value ends in an unfinished escape"),
                Some('n') => value.push('\n'),
                Some('\\') => value.push('\\'),
                Some('"') => value.push('"'),
                Some(other) => {
                    value.push('\\');
                    value.push(other);
                }
            },
            Some(character) => value.push(character),
        }
    }
}

// =========================================================================
// Diff internals
// =========================================================================

/// The `(name, operation, type)` triple both families share, and the only
/// key a rejection counter can be matched to a duration label set on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RejectionTriple {
    /// The webhook's `name` label.
    name: String,
    /// The `operation` label.
    operation: String,
    /// The `type` label.
    webhook_type: String,
}

impl WebhookMetricKey {
    /// The triple this duration label set would be matched to a rejection
    /// counter on.
    fn rejection_triple(&self) -> RejectionTriple {
        RejectionTriple {
            name: self.name.clone(),
            operation: self.operation.clone(),
            webhook_type: self.webhook_type.clone(),
        }
    }
}

impl RejectionKey {
    /// The triple this rejection series aggregates into, dropping
    /// `error_type`/`rejection_code` (see this module's documentation).
    fn rejection_triple(&self) -> RejectionTriple {
        RejectionTriple {
            name: self.name.clone(),
            operation: self.operation.clone(),
            webhook_type: self.webhook_type.clone(),
        }
    }
}

/// The `(count, sum, evidence)` triple for one duration label set.
///
/// `before`/`after` of `None` are not symmetric, and deliberately so: a
/// label set absent from the *earlier* page but present in the later one
/// was genuinely at zero (a Prometheus vector materialises a child only
/// when it is first incremented, and this function is only reached when
/// both pages carried the family), while a label set present earlier and
/// gone later can only mean the counter was reset.
fn duration_delta(
    before: Option<&DurationSample>,
    after: Option<&DurationSample>,
) -> (u64, f64, DeltaEvidence) {
    let unavailable = (0, 0.0, DeltaEvidence::Unavailable);
    let (before_sum, before_count) = match before {
        None => (0.0, 0),
        Some(sample) => match (sample.sum, sample.count) {
            (Some(sum), Some(count)) => (sum, count),
            _ => return unavailable,
        },
    };
    let Some(after) = after else {
        return unavailable;
    };
    let (Some(after_sum), Some(after_count)) = (after.sum, after.count) else {
        return unavailable;
    };
    if !after_sum.is_finite()
        || !before_sum.is_finite()
        || after_count < before_count
        || after_sum < before_sum
    {
        return unavailable;
    }
    (
        after_count - before_count,
        after_sum - before_sum,
        DeltaEvidence::Observed,
    )
}

/// Per-triple rejection increases, or `None` when the family was absent
/// from at least one snapshot (no evidence at all). An inner `None` marks
/// a triple whose counter went backwards or lost a child, so its increase
/// is unknown even though the family was present.
fn rejection_deltas(
    before: &AdmissionMetricSnapshot,
    after: &AdmissionMetricSnapshot,
) -> Option<BTreeMap<RejectionTriple, Option<u64>>> {
    let before_map = before.rejections.as_ref()?;
    let after_map = after.rejections.as_ref()?;

    let mut totals: BTreeMap<RejectionTriple, Option<u64>> = BTreeMap::new();
    let keys: BTreeSet<&RejectionKey> = before_map.keys().chain(after_map.keys()).collect();
    for key in keys {
        let child = match (before_map.get(key), after_map.get(key)) {
            // Absent from the earlier page but present in the later one:
            // a counter that had not been incremented yet.
            (None, later) => later.copied(),
            // Present earlier, gone later: only a reset explains that.
            (Some(_), None) => None,
            (Some(earlier), Some(later)) => later.checked_sub(*earlier),
        };
        let total = totals.entry(key.rejection_triple()).or_insert(Some(0));
        *total = match (*total, child) {
            (Some(running), Some(child)) => Some(running.saturating_add(child)),
            // One unusable child makes the whole triple unusable: a
            // partial sum would understate the real rejection count.
            _ => None,
        };
    }
    Some(totals)
}

/// The `(delta, evidence)` pair for one `rejected="true"` label set.
fn rejection_for(
    rejections: Option<&BTreeMap<RejectionTriple, Option<u64>>>,
    triple: &RejectionTriple,
) -> (u64, RejectionEvidence) {
    let unavailable = (0, RejectionEvidence::Unavailable);
    // The family was missing from at least one page: no evidence at all.
    let Some(totals) = rejections else {
        return unavailable;
    };
    match totals.get(triple) {
        // The family was present on both pages and this triple was never
        // in it: nothing was ever rejected here, which is an observation.
        None => (0, RejectionEvidence::Observed),
        // Present, but its counter went backwards or lost a child.
        Some(None) => unavailable,
        Some(Some(delta)) => (*delta, RejectionEvidence::Observed),
    }
}

/// Deltas for rejection increases with no matching `rejected="true"`
/// duration label set, so observed rejection evidence is never silently
/// dropped. Triples whose increase is a plain, observed `0` are left out:
/// they carry nothing a caller could act on, and emitting one per
/// never-rejecting webhook would bury the rows that matter.
fn orphan_rejection_deltas(
    rejections: Option<&BTreeMap<RejectionTriple, Option<u64>>>,
    attributed: &BTreeSet<RejectionTriple>,
) -> Vec<WebhookMetricDelta> {
    let Some(totals) = rejections else {
        return Vec::new();
    };
    totals
        .iter()
        .filter(|(triple, delta)| !attributed.contains(*triple) && **delta != Some(0))
        .map(|(triple, delta)| {
            let (rejection_delta, rejection_evidence) = match delta {
                Some(delta) => (*delta, RejectionEvidence::Observed),
                None => (0, RejectionEvidence::Unavailable),
            };
            WebhookMetricDelta {
                webhook: triple.name.clone(),
                operation: triple.operation.clone(),
                webhook_type: triple.webhook_type.clone(),
                // No duration label set matched, so there is no observed
                // `rejected` label value to report -- and none is invented.
                rejected: None,
                other_labels: BTreeMap::new(),
                request_count_delta: 0,
                duration_sum_delta: 0.0,
                // No duration observation was seen for this webhook at
                // all, so the zeroes above are placeholders, not a
                // measured absence of activity.
                duration_evidence: DeltaEvidence::Unavailable,
                rejection_delta,
                rejection_evidence,
            }
        })
        .collect()
}

// =========================================================================
// What is, and is not, covered here
//
// Covered in `tests/metrics.rs`, offline: the whole parser against the
// checked-in `testdata/metrics/{before,after}.prom` pages (exact sums and
// counts per label set), every `diff_metrics` case, the exactly-one
// attribution rule at count deltas of 0/1/2, absent-family honesty, and
// malformed-line handling. The scrape path downstream of an already-built
// `Client` -- request shape, timeout, error mapping -- is covered there
// too, against a `tower_test::mock`-backed `Client`, the same technique
// `admissionlab-installer/src/readiness.rs` and
// `admissionlab-fixtures/src/resources.rs` already use.
//
// NOT covered without a live cluster, and left to a live exit gate for
// the same reason those two modules document for their own `client_for`:
// whether `client_for` here genuinely connects using a real
// `kind`-produced kubeconfig, and whether a real kube-apiserver's
// `/metrics` page is reachable with that kubeconfig's credentials and
// carries these two families in the shape `testdata/metrics/*.prom`
// models.
// =========================================================================
#[cfg(test)]
mod tests {
    use super::{Cursor, SampleField, classify_metric, counter_value, parse_labels};

    #[test]
    fn the_duration_familys_bucket_series_is_ignored() {
        // The mutation this kills: a `starts_with(DURATION_FAMILY)` match
        // that swept `_bucket` lines in, whose `le`-labelled counts would
        // then be filed as if they were the label set's total count.
        assert_eq!(
            classify_metric("apiserver_admission_webhook_admission_duration_seconds_bucket"),
            None
        );
        assert_eq!(
            classify_metric("apiserver_admission_webhook_admission_duration_seconds_sum"),
            Some(SampleField::DurationSum)
        );
    }

    #[test]
    fn a_different_family_with_the_same_suffix_is_not_matched() {
        // `apiserver_admission_step_admission_duration_seconds_sum` is a
        // real, different family (built-in admission plugins, not
        // webhooks). Matching it would attribute plugin latency to a
        // webhook -- and it carries no `name` label, so the bug would
        // surface only as silently missing evidence.
        assert_eq!(
            classify_metric("apiserver_admission_step_admission_duration_seconds_sum"),
            None
        );
    }

    #[test]
    fn a_counter_beyond_exact_f64_integer_range_is_refused() {
        // Fails if `counter_value` cast without a guard: 2^53 + 1 is not
        // representable, so an `as` cast would report a neighbouring
        // integer as though it had been observed.
        assert_eq!(counter_value(7.0), Some(7));
        assert_eq!(counter_value(-1.0), None);
        assert_eq!(counter_value(f64::INFINITY), None);
        assert_eq!(counter_value(9_007_199_254_740_993.0), None);
    }

    #[test]
    fn a_label_value_may_contain_a_quoted_delimiter() {
        // The mutation this kills: splitting the label block on `,` or
        // scanning for the first `}`, either of which mis-reads a value
        // that legitimately contains one.
        let mut cursor = Cursor {
            rest: r#"{name="a\"b, c}d",operation="CREATE"} 3"#,
        };
        let labels = parse_labels(&mut cursor).expect("the label block is well formed");
        assert_eq!(labels.get("name").map(String::as_str), Some("a\"b, c}d"));
        assert_eq!(labels.get("operation").map(String::as_str), Some("CREATE"));
        assert_eq!(cursor.rest, " 3");
    }

    #[test]
    fn an_unterminated_label_block_is_an_error_not_a_partial_read() {
        // Fails if the parser returned whatever labels it managed to read
        // before running out of input -- a partially-keyed sample filed
        // under a label set that was never actually observed.
        let mut cursor = Cursor {
            rest: r#"{name="unfinished"#,
        };
        assert!(parse_labels(&mut cursor).is_err());
    }
}
