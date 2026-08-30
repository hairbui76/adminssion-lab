//! What was observed about the chain of webhooks a fixture passed
//! through.
//!
//! Kubernetes runs `ValidatingWebhookConfiguration`/
//! `MutatingWebhookConfiguration` webhooks in rounds, and every webhook
//! in a round can see the object mutated by an earlier round. Nothing in
//! the API server itself hands this project a first-class trace of that
//! chain -- it has to be reconstructed from audit-log evidence gathered
//! elsewhere (a later task's job), and that reconstruction can be
//! incomplete: an audit backend can be missing, rate-limited, or simply
//! not configured to log a particular stage. [`AdmissionTrace`] and
//! [`TraceEvidence`] exist to carry that incompleteness forward
//! explicitly rather than let a caller of this model assume a trace it
//! got back is a complete one (Global Constraint 15).

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How completely [`AdmissionTrace::invocations`] reflects what actually
/// happened.
///
/// Every value carries **no** `Default`, this type derives no `Default`,
/// and no field holding it may carry `#[serde(default)]`: the one
/// concrete way that "unknown" can quietly become a fabricated fact here
/// is a caller (or an evolving JSON payload) omitting this field and
/// something silently filling in a plausible-looking value. Because
/// `Observed` is (deliberately) the variant a developer would write
/// first, an accidental `Default` derivation would default to it -- the
/// single most dangerous collapse this type exists to prevent, since it
/// would claim the admission chain was watched when it was not
/// (controller supplement §1, Task 3.3). Deserializing any document that
/// omits this field must fail; see `tests/model.rs`'s
/// `deserializing_admission_trace_without_evidence_fails` for the test
/// that pins this, and its own doc comment for how it was mutation-
/// tested.
///
/// Each variant's wire tag is pinned with an explicit `#[serde(rename)]`
/// rather than left to derive from the Rust identifier (controller
/// supplement §5): Phase 4 and the JSON report contract depend on these
/// exact strings never drifting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TraceEvidence {
    /// The full webhook chain was watched; `invocations` is believed
    /// complete.
    #[serde(rename = "observed")]
    Observed,
    /// Some, but not all, of the webhook chain was watched; `invocations`
    /// may be missing entries.
    #[serde(rename = "partial")]
    Partial,
    /// No usable evidence of the webhook chain was available;
    /// `invocations` should be treated as empty regardless of its actual
    /// length.
    #[serde(rename = "unavailable")]
    Unavailable,
}

/// What one webhook, in one round, was observed to do with the request.
///
/// This is the *webhook's* own result, distinct from
/// [`crate::outcome::AdmissionDecision`], which is the API server's
/// final verdict after every webhook in every round has run. Carries no
/// `Default` and no `#[serde(default)]`-eligible field for the same
/// reason [`TraceEvidence`] does not: a missing `outcome` must never be
/// read as a webhook having quietly succeeded (controller supplement §2,
/// Task 3.3 -- "do not let a missing outcome default to success").
///
/// Four variants, not three: `Allowed`/`Denied` are the two decisions a
/// webhook that actually ran and responded can reach; `Errored` is a
/// webhook call that failed outright (timeout, connection refused, a
/// malformed response) -- which is not the same fact as `Denied` and
/// must stay distinguishable from it, since `failurePolicy: Ignore`
/// treats an error as "allow" while `Fail` treats it as "deny", and later
/// tasks (3.6, 3.7) need to tell that apart from the underlying evidence,
/// not this type. `Unknown` is the explicit "the evidence could not tell
/// us" case Global Constraint 15 requires: it is a fourth, distinct
/// value, never a fallback encoded by omission or by collapsing onto one
/// of the other three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebhookOutcome {
    /// The webhook ran, responded, and allowed the request to proceed
    /// (`response.allowed: true`).
    #[serde(rename = "allowed")]
    Allowed,
    /// The webhook ran, responded, and denied the request
    /// (`response.allowed: false`).
    #[serde(rename = "denied")]
    Denied,
    /// The webhook call itself failed (timeout, connection error, or an
    /// otherwise unusable response) rather than reaching a normal
    /// allow/deny verdict.
    #[serde(rename = "errored")]
    Errored,
    /// The evidence available could not establish which of the above
    /// happened.
    #[serde(rename = "unknown")]
    Unknown,
}

/// One webhook's observed participation in one admission round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookInvocation {
    /// The name of the `ValidatingWebhookConfiguration` or
    /// `MutatingWebhookConfiguration` this webhook belongs to.
    pub configuration: String,
    /// This webhook's own name within `configuration`.
    pub webhook: String,
    /// Which admission round this invocation ran in (zero-based:
    /// mutating webhooks can run in more than one round as each mutates
    /// the object further).
    pub round: u32,
    /// This invocation's position among webhooks invoked within `round`
    /// (zero-based).
    pub index: u32,
    /// Whether this webhook's response carried a JSON Patch that changed
    /// the object. `None` means the evidence could not establish
    /// whether it mutated -- this is never collapsed to `Some(false)`:
    /// doing so would fabricate "it did not mutate" for a webhook this
    /// project could not actually observe, which is precisely the
    /// behavioural gap this product exists to detect (controller
    /// supplement §1, Task 3.3).
    pub mutated: Option<bool>,
    /// The JSON Patch this webhook's response carried, when one was
    /// observed. `None` covers both "this webhook did not mutate" and
    /// "whether it mutated is unknown" -- a caller must consult
    /// `mutated` to tell those apart; `patch` alone never distinguishes
    /// them.
    pub patch: Option<Vec<json_patch::PatchOperation>>,
    /// How long this webhook call took to respond, when it was measured.
    /// `None` -- never a fabricated `0` -- means the duration was not
    /// measured or could not be attributed to this specific invocation;
    /// a zero here would read as "instantaneous," which Phase 4's
    /// latency comparison would then treat as a real (and false)
    /// improvement (controller supplement §1, Task 3.3).
    #[serde(with = "duration_millis_option")]
    pub latency: Option<Duration>,
    /// What this webhook was observed to do with the request.
    pub outcome: WebhookOutcome,
}

/// What was observed about the full webhook chain for one fixture, on
/// one side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionTrace {
    /// How complete `invocations` is believed to be. Required: no
    /// default, so a document that omits it fails to deserialize rather
    /// than silently reading as [`TraceEvidence::Observed`] -- see that
    /// type's own documentation.
    pub evidence: TraceEvidence,
    /// Each webhook invocation observed, in the order they were
    /// observed to run. May be incomplete or empty; `evidence` is the
    /// only field that says how much to trust that.
    pub invocations: Vec<WebhookInvocation>,
}

/// Serializes/deserializes `Option<Duration>` as `null` or a plain
/// integer number of milliseconds.
///
/// Distinct from `admissionlab_spec::model`'s `duration_millis` (which
/// handles a bare, always-known `Duration`): here the absent case is a
/// first-class possibility that must reach the wire as literal JSON
/// `null`, never as `0` -- see [`WebhookInvocation::latency`]'s own
/// documentation for why that distinction is load-bearing.
mod duration_millis_option {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    // `serde(with = "...")` always calls `serialize` with a reference to
    // the field itself -- here `&Option<Duration>` -- regardless of
    // clippy's usual `Option<&T>` preference; the signature is fixed by
    // the derive macro's generated call site, not a style choice.
    #[allow(clippy::ref_option)]
    pub(super) fn serialize<S: Serializer>(
        value: &Option<Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            // `as_millis()` returns `u128`; saturate rather than
            // `as`-cast, matching `outcome::serialize_duration_millis`.
            Some(duration) => {
                let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
                serializer.serialize_some(&millis)
            }
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        let millis = Option::<u64>::deserialize(deserializer)?;
        Ok(millis.map(Duration::from_millis))
    }
}
