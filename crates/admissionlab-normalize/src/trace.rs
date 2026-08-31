//! Canonicalizing what was observed about a fixture's webhook chain
//! (Task 4.2).
//!
//! [`normalize_trace`] is the trace-side counterpart of
//! [`crate::normalize_object`]. Where that one removes server-generated
//! noise from an object, this one removes *presentation* noise from the
//! webhook evidence captured alongside it, so that two runs which
//! observed the same behavior produce the same [`NormalizedTrace`].
//!
//! It is deliberately far less powerful than object normalization. There
//! is no profile, no rule vocabulary, and no evidence record of what it
//! did, because it does exactly one thing and that thing is not
//! configurable: it sorts the keys of JSON objects that appear inside
//! JSON Patch values. Everything else about the trace passes through
//! verbatim.
//!
//! # What canonicalization means here, precisely
//!
//! **Object key order is presentation. Array order is semantics.**
//!
//! A webhook that answers with `{"name":"app","image":"pause"}` and one
//! that answers with `{"image":"pause","name":"app"}` sent the same
//! patch; JSON object members are unordered by RFC 8259 §4, and the
//! Kubernetes API server applies both identically. So object keys are
//! sorted, recursively, at every depth inside a patch's `value`.
//!
//! An array inside a patch value is the opposite. `{"op":"add","path":
//! "/spec/initContainers","value":[{"name":"a"},{"name":"b"}]}` and the
//! same operation with those two entries swapped are two *different*
//! patches producing two different pods, and a webhook that started
//! emitting one instead of the other is exactly the regression this
//! product exists to catch. Arrays are recursed into (so an object
//! nested inside one still gets canonical keys) and never reordered.
//!
//! Three further things are explicitly *not* canonicalized:
//!
//! - **Operation order.** A JSON Patch is applied in sequence and later
//!   operations depend on what earlier ones did — `remove /spec/x` then
//!   `add /spec/x` is not the same patch as the reverse. Task 4.2 Step 2
//!   says so directly; the operations `Vec` is rebuilt in its original
//!   order.
//! - **Operation `path`/`from`.** These are RFC 6901 pointers into the
//!   target document, and they are already in whatever canonical form
//!   the webhook chose. Rewriting them would be re-encoding, not
//!   canonicalizing.
//! - **Which operations exist.** No operation is dropped, merged, or
//!   rewritten into an equivalent one, however redundant it looks. See
//!   "A patch is never removed" below.
//!
//! # Why this is written out rather than left to `serde_json`
//!
//! This workspace resolves `serde_json` without its `preserve_order`
//! feature, so a `Value::Object` is `BTreeMap`-backed and already both
//! compares and serializes in sorted key order. Today, therefore, this
//! module's canonicalization pass is a no-op on every input — and it is
//! still here on purpose.
//!
//! `preserve_order` is an *additive* Cargo feature. Any crate anywhere
//! in the workspace's resolved graph that enables it — including one
//! this crate never names, since feature unification is graph-wide —
//! silently switches every `Value` in the process to `IndexMap`. Under
//! that backing, `Value` equality happens to stay order-independent
//! (`IndexMap`'s `PartialEq` compares as a map), but **serialization
//! does not**: a patch would then render into a report, a run manifest,
//! or a hash in whatever order the webhook's response happened to be
//! parsed in, and two runs that saw identical behavior would produce
//! different bytes. Relying on a transitive feature flag to hold a
//! contract this task freezes is exactly the kind of load-bearing
//! accident that is invisible until it breaks, so the property is
//! established here explicitly instead.
//!
//! # A patch is never removed, blanked, or shortened
//!
//! Task 4.2 Step 3: a changed patch must survive normalization even when
//! the two sides' final objects happen to be identical. Two webhooks can
//! reach the same pod by different routes — one `replace`s a whole map,
//! the other `add`s two keys; one sets a field to a new value, the other
//! sets it to the value it already had — and the fact that they now
//! *differ* is a real behavioral change, whatever the final objects look
//! like.
//!
//! Structurally this is not a rule that could be violated by accident:
//! [`normalize_trace`]'s frozen signature takes only an
//! [`AdmissionTrace`], which contains no final object at all, so this
//! module cannot compare against one even if a future edit wanted to.
//! `tests/trace.rs` pins the behavior anyway, because the guarantee is
//! about the output, not about today's signature.
//!
//! An empty patch (`Some(vec![])`, a webhook that answered with a patch
//! containing no operations) is likewise kept as an empty patch and
//! never collapsed to `None`: `None` means "no patch was observed",
//! which is a different fact.
//!
//! # Nothing is filled in
//!
//! `mutated: None` and `latency: None` pass through as `None` (Global
//! Constraint 15). Neither is collapsed to a plausible `Some(false)` or
//! `Some(0)`: the first would claim a webhook was watched and found not
//! to mutate when it was not watched at all, and the second would read
//! as an instantaneous call, which Phase 4's latency comparison would
//! treat as a real and entirely fabricated improvement. Both hazards are
//! spelled out on `WebhookInvocation`'s own fields; this module's job is
//! simply not to undo them.
//!
//! [`TraceEvidence`] is copied through untouched for the same reason. It
//! is the field that says how much of the chain was actually watched,
//! and normalization learns nothing that could revise it.

use std::time::Duration;

use admissionlab_admission::trace::{
    AdmissionTrace, TraceEvidence, WebhookInvocation, WebhookOutcome,
};
use json_patch::{AddOperation, PatchOperation, ReplaceOperation, TestOperation};
use serde_json::Value;

/// One webhook's observed participation in one admission round, with its
/// patch in canonical form.
///
/// Field-for-field the same facts as
/// [`admissionlab_admission::trace::WebhookInvocation`] — the cross-task
/// type registry (§1.2) fixes exactly these eight fields — and a
/// distinct type rather than a reuse of that one, because they answer
/// different questions: `WebhookInvocation` is *what was observed*, and
/// belongs to the capture pipeline that observed it; this is *what is
/// compared*, and belongs to the comparison. Phase 4 needs both at once
/// (a report shows the raw observation and the normalized form side by
/// side), and a single type could not be both.
///
/// No `Default`, for the reason
/// [`admissionlab_admission::trace::TraceEvidence`] documents at length:
/// `outcome` and `mutated` are evidence, and a value that can appear by
/// omission is a value that can be fabricated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedWebhookInvocation {
    /// The `ValidatingWebhookConfiguration`/`MutatingWebhookConfiguration`
    /// this webhook belongs to. Verbatim.
    pub configuration: String,
    /// This webhook's own name within `configuration`. Verbatim.
    pub webhook: String,
    /// Which admission round this invocation ran in. Verbatim: the
    /// round is what makes a reinvocation legible, and renumbering it
    /// would be inventing a chain that was not observed.
    pub round: u32,
    /// This invocation's position among webhooks invoked within `round`.
    /// Verbatim, for the same reason.
    pub index: u32,
    /// Whether this webhook's response carried a patch that changed the
    /// object; `None` where the evidence could not say. Passed through
    /// exactly.
    pub mutated: Option<bool>,
    /// The JSON Patch this webhook's response carried, with every
    /// object inside every operation's `value` in canonical key order.
    /// Operation order, operation paths, and array order within a value
    /// are untouched — see this module's documentation.
    pub patch: Option<Vec<PatchOperation>>,
    /// How long the webhook call took, where it was measured. `None`
    /// passes through as `None`, never as a fabricated zero.
    pub latency: Option<Duration>,
    /// What this webhook was observed to do with the request. Verbatim,
    /// including [`WebhookOutcome::Unknown`].
    pub outcome: WebhookOutcome,
}

/// A whole fixture's webhook chain, canonicalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedTrace {
    /// How complete the chain below is believed to be. Copied from the
    /// input trace unchanged: normalization observes nothing that could
    /// revise it, and quietly upgrading it would be the single most
    /// damaging fabrication this crate could commit.
    pub evidence: TraceEvidence,
    /// Each observed invocation, in the order the capture pipeline
    /// recorded them.
    pub invocations: Vec<NormalizedWebhookInvocation>,
}

/// Canonicalizes `trace` for comparison.
///
/// Infallible by construction: every transformation here is a total
/// function on values that already exist, and nothing is parsed,
/// resolved, or looked up. See this module's documentation for exactly
/// what is and is not canonicalized.
#[must_use]
pub fn normalize_trace(trace: &AdmissionTrace) -> NormalizedTrace {
    NormalizedTrace {
        evidence: trace.evidence,
        invocations: trace.invocations.iter().map(normalize_invocation).collect(),
    }
}

/// Copies one invocation through, canonicalizing only its patch values.
fn normalize_invocation(invocation: &WebhookInvocation) -> NormalizedWebhookInvocation {
    NormalizedWebhookInvocation {
        configuration: invocation.configuration.clone(),
        webhook: invocation.webhook.clone(),
        round: invocation.round,
        index: invocation.index,
        mutated: invocation.mutated,
        // `.as_ref().map(...)` -- never `unwrap_or_default()`. An
        // unobserved patch stays `None`; an observed empty one stays an
        // observed empty one.
        patch: invocation
            .patch
            .as_ref()
            .map(|operations| operations.iter().map(canonicalize_operation).collect()),
        latency: invocation.latency,
        outcome: invocation.outcome,
    }
}

/// Canonicalizes the `value` an operation carries, if it carries one.
///
/// `Add`, `Replace`, and `Test` are the three RFC 6902 operations with a
/// `value`; `Remove`, `Move`, and `Copy` have only pointers, so they are
/// cloned untouched. Matching on the variants (rather than serializing
/// the operation to a `Value` and back) keeps every `path`/`from`
/// exactly as the webhook wrote it, and makes a future json-patch
/// release that adds a value-carrying operation a compile error here
/// rather than a silently uncanonicalized field.
fn canonicalize_operation(operation: &PatchOperation) -> PatchOperation {
    match operation {
        PatchOperation::Add(add) => PatchOperation::Add(AddOperation {
            path: add.path.clone(),
            value: canonicalize_value(&add.value),
        }),
        PatchOperation::Replace(replace) => PatchOperation::Replace(ReplaceOperation {
            path: replace.path.clone(),
            value: canonicalize_value(&replace.value),
        }),
        PatchOperation::Test(test) => PatchOperation::Test(TestOperation {
            path: test.path.clone(),
            value: canonicalize_value(&test.value),
        }),
        remove @ PatchOperation::Remove(_) => remove.clone(),
        move_op @ PatchOperation::Move(_) => move_op.clone(),
        copy @ PatchOperation::Copy(_) => copy.clone(),
    }
}

/// Rebuilds `value` with every object's keys in sorted order, at every
/// depth, leaving array order and every scalar exactly as they were.
///
/// Keys are ordered by Rust's `str` comparison — byte-wise over UTF-8:
/// total, locale-independent, and identical on every platform, matching
/// `object.rs`'s own sort for the same reason.
///
/// The sort is explicit rather than left to `serde_json::Map`'s own
/// ordering; see this module's documentation for why that distinction
/// is load-bearing rather than defensive.
fn canonicalize_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .iter()
                .map(|(key, nested)| (key.clone(), canonicalize_value(nested)))
                .collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(entries.into_iter().collect())
        }
        // Recursed into, never reordered: array order is semantics.
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_value).collect()),
        scalar => scalar.clone(),
    }
}
