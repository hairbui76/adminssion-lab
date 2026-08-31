//! Turning one fixture's audit event back into the chain of mutating
//! webhooks that ran on it (Task 3.6).
//!
//! [`crate::audit_reader`] (Task 3.5) hands over an [`AuditEvent`] with
//! its `annotations` map verbatim and uninterpreted. This module is the
//! one place in the codebase that decides what the mutating-webhook keys
//! in that map mean, and turns them into a
//! [`crate::trace::AdmissionTrace`]. Task 3.10's `capture_fixture` is the
//! consumer.
//!
//! # Provenance: where the annotation contract comes from
//!
//! Every claim this module makes about the annotations was read out of
//! kube-apiserver's own mutating-webhook dispatcher --
//! `staging/src/k8s.io/apiserver/pkg/admission/plugin/webhook/mutating/dispatcher.go`
//! in `kubernetes/kubernetes` (read at `release-1.32`; unchanged in shape
//! across the three minor versions Global Constraint 10 commits this
//! project to). It is not inferred from example logs, because an example
//! log can only show the cases that happened to occur, and the decisions
//! below turn on the cases that *did not*.
//!
//! Four constants and two payload shapes come straight from that file:
//!
//! ```text
//! MutationAuditAnnotationPrefix              = "mutation.webhook.admission.k8s.io/"
//! PatchAuditAnnotationPrefix                 = "patch.webhook.admission.k8s.io/"
//! MutationAuditAnnotationFailedOpenKeyPrefix = "failed-open." + MutationAuditAnnotationPrefix
//! key suffix                                 = fmt.Sprintf("round_%d_index_%d", round, idx)
//!
//! type MutationAuditAnnotation struct {
//!     Configuration string `json:"configuration"`
//!     Webhook       string `json:"webhook"`
//!     Mutated       bool   `json:"mutated"`
//! }
//! type PatchAuditAnnotation struct {
//!     Configuration string      `json:"configuration"`
//!     Webhook       string      `json:"webhook"`
//!     Patch         interface{} `json:"patch,omitempty"`
//!     PatchType     string      `json:"patchType,omitempty"`
//! }
//! ```
//!
//! The failed-open annotation is the odd one out: its value is the bare
//! webhook name, not JSON.
//!
//! # What each annotation actually proves
//!
//! This is the whole reason the module exists, and it is subtler than the
//! key names suggest. In `callAttrMutatingHook`, the mutation annotation
//! is written from a **deferred** call registered as the very first
//! statement of the function:
//!
//! ```text
//! changed := false
//! defer func() { annotator.addMutationAnnotation(changed) }()
//! ```
//!
//! so it is emitted on *every* exit path -- a dry-run/`sideEffects`
//! rejection, a REST-client failure, a connection timeout, an unusable
//! response body, an explicit `allowed: false` denial, an empty patch, a
//! patch that failed to decode or apply, and the ordinary success path
//! alike. `changed` is only ever set to `true` at one place, after the
//! `if !result.Allowed` early return has been passed and after the
//! decoded JSON Patch has actually been applied and the result compared
//! with `apiequality.Semantic.DeepEqual`. Therefore:
//!
//! - **`mutated: true` proves a lot.** The webhook was invoked, returned
//!   a well-formed `AdmissionReview` with `allowed: true`, and its JSON
//!   Patch changed the object. That is exactly
//!   [`WebhookOutcome::Allowed`].
//! - **`mutated: false` proves almost nothing.** It proves only that the
//!   webhook was *invoked*. It is equally what a denial, a timeout, a
//!   connection refusal and a genuinely no-op webhook produce. Reading it
//!   as "the webhook ran and allowed the request" would be a fabricated
//!   fact of exactly the kind Global Constraint 15 forbids, so it becomes
//!   [`WebhookOutcome::Unknown`] unless something else in the same event
//!   narrows it down.
//! - **A patch annotation proves the allow.** `addPatchAnnotation` is
//!   reached only after the `if !result.Allowed` early return and after
//!   the patch applied cleanly, so its mere presence upgrades a
//!   `mutated: false` invocation to [`WebhookOutcome::Allowed`]. (That
//!   combination is reachable and legitimate: a webhook can return a
//!   patch that sets a field to the value it already had, which applies
//!   fine and leaves `DeepEqual` true.)
//! - **A `failed-open.` annotation proves the error.** It is written only
//!   where the dispatcher logs `Failed calling webhook, failing open`,
//!   which is an `ErrCallingWebhook` under `failurePolicy: Ignore` --
//!   precisely [`WebhookOutcome::Errored`], the variant
//!   [`crate::trace::WebhookOutcome`]'s own documentation asks Task 3.6
//!   to keep distinguishable from `Denied`.
//!
//! Reading the failed-open key also has one trap worth naming, because
//! it is invisible until it bites: its prefix *ends with* the mutation
//! prefix. A `key.contains(MUTATION_ANNOTATION_PREFIX)` test would
//! classify every failed-open annotation as a mutation annotation and
//! then fail to parse its bare-string value as JSON. This module only
//! ever uses `strip_prefix`, and checks the failed-open prefix first.
//!
//! # Why `latency` is always `None`
//!
//! None of these annotations carries a duration; kube-apiserver reports
//! per-webhook timing through the `apiserver_admission_webhook_duration_seconds`
//! metric instead, which is Task 3.8's job (and Global Constraint 19's
//! "optional observed signal"). A trace reconstructed here therefore sets
//! [`crate::trace::WebhookInvocation::latency`] to `None` on every
//! invocation -- never a `0`, which Phase 4 would read as a real and
//! impossibly fast measurement.
//!
//! # Why validating webhooks are never invented, only counted
//!
//! An event that ran validating webhooks carries
//! `validation.webhook.admission.k8s.io/round_<r>_index_<i>` annotations
//! too. This module never builds a [`WebhookInvocation`] from one (Task
//! 3.6 Step 5): a mutating annotation says nothing about a validating
//! webhook, and a validating annotation is not what this function was
//! asked to reconstruct.
//!
//! It does, however, *notice that they exist*, and that is not the same
//! thing. [`crate::trace::TraceEvidence::Observed`] means "the full
//! webhook chain was watched; `invocations` is believed complete". If the
//! event proves a validating chain ran that this trace does not describe,
//! then `invocations` is demonstrably not the complete chain, and the
//! honest answer is [`crate::trace::TraceEvidence::Partial`]. Only the
//! key prefix is looked at; not one field of a validating payload is
//! parsed, and no invocation is created from it.
//!
//! # Which `TraceEvidence` an empty trace deserves
//!
//! An event with no mutating annotations at all returns an
//! `Observed` trace with zero invocations -- a positive claim that no
//! mutating webhook ran. Three things justify it:
//!
//! 1. The mutation annotation is unconditional (the `defer` above), so
//!    every invoked mutating webhook leaves one. There is no "invoked but
//!    silent" case to hide behind an empty map -- not even a fail-open
//!    one.
//! 2. It is written with `auditinternal.LevelMetadata`, and Global
//!    Constraint 18 configures Admission Lab's own clusters at `Request`
//!    level, which is strictly higher. (The patch annotation needs
//!    `Request`; that difference is exactly why GC18 says `Request` and
//!    not `Metadata`.)
//! 3. The alternative is worse. [`crate::trace::TraceEvidence::Unavailable`]
//!    tells a reader to treat `invocations` as empty *regardless of its
//!    actual length* -- so returning it here would tell Phase 4 to
//!    disregard the trace, and "the candidate stack's mutating webhook
//!    stopped running" would become indistinguishable from "we did not
//!    look". That is the single regression this product exists to catch,
//!    so it must not be encoded as missing data.
//!
//! This function never returns `Unavailable`. That value belongs to a
//! caller that could not obtain an audit event at all, which is a
//! situation this function -- which is *handed* an event -- cannot
//! observe.
//!
//! Point 2 does carry a precondition this module cannot verify on its
//! own: [`AuditEvent`] deliberately does not parse the audit `level`
//! field (Task 3.5 keeps a narrow subset), so an event captured under a
//! policy below `Metadata` would yield a confidently empty trace.
//! Admission Lab writes the audit policy for the ephemeral clusters it
//! creates, so the precondition holds by construction for every event
//! Task 3.10 passes in; it is stated here rather than silently assumed.
//! What this function *can* check, and does, is that the event is the
//! `ResponseComplete` one: admission annotations are attached while the
//! request is being served, so a `RequestReceived` event never has any,
//! and reconstructing from one would manufacture a confident empty chain
//! out of an event that was written before admission had run at all.
//! That is [`TraceError::NotResponseComplete`].
//!
//! # Why a malformed annotation is fatal here, and not in the reader
//!
//! [`crate::audit_reader`] turns an unparsable audit-log *line* into a
//! [`admissionlab_core::Diagnostic`] and keeps reading, because that line
//! belongs to some unknown component of the cluster and says nothing
//! about this fixture. The opposite is true here: this function is handed
//! the fixture's *own* target event, so every
//! `mutation`/`patch`/`failed-open` annotation on it is this fixture's
//! own admission evidence, written by the API server in a format this
//! module pins to upstream source.
//!
//! So a malformed key or payload is [`TraceError`], not a quietly dropped
//! entry. [`crate::trace::AdmissionTrace`] has exactly one field for
//! trustworthiness -- a tri-state `evidence` -- and no channel for "the
//! invocation at round 0 index 2 was discarded". Dropping it and
//! returning `Partial` would leave Phase 4 comparing a three-webhook
//! baseline against a two-webhook candidate and reporting a behavioural
//! difference that never happened. A fixture whose own evidence cannot be
//! read must fail loudly instead, which is what the roadmap's "fatal if
//! the target event cannot be reconstructed" asks for.
//!
//! Like the reader's diagnostics, and for the same Global Constraint 14
//! reason, a [`TraceError`] never quotes the value it failed on: a patch
//! annotation embeds fragments of the request object, so the error
//! carries the annotation *key* (a fixed kube-apiserver-generated string)
//! plus a value-free JSON error classification, and nothing else. In
//! particular it never uses `serde_json::Error`'s own `Display`, which
//! embeds the offending value on a type mismatch.
//!
//! # What an index gap does not mean
//!
//! Indices come from `for i, hook := range hooks`, and the dispatcher
//! `continue`s past a hook whose match conditions no longer hold --
//! without consuming a smaller index. So `round_0_index_0` followed by
//! `round_0_index_10` is an ordinary event, not evidence of nine hidden
//! invocations, and this module infers nothing from the gap. It does sort
//! numerically rather than lexically, because `round_0_index_10` sorts
//! *before* `round_0_index_2` as text.
//!
//! Rounds behave similarly: the dispatcher only ever emits round `0` or
//! round `1` (`round := 0; if reinvokeCtx.IsReinvoke() { round = 1 }`),
//! but this module parses any `u32` rather than hard-coding that, since
//! nothing here needs the assumption and a future reinvocation policy
//! could widen it.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::audit_reader::{AuditEvent, json_error_category};
use crate::trace::{AdmissionTrace, TraceEvidence, WebhookInvocation, WebhookOutcome};

/// kube-apiserver's `MutationAuditAnnotationPrefix`: the key prefix of
/// the annotation recording that one mutating webhook was invoked.
///
/// Match it with `strip_prefix`, never `contains` -- see this module's
/// documentation for the failed-open key that would otherwise be
/// misclassified.
pub const MUTATION_ANNOTATION_PREFIX: &str = "mutation.webhook.admission.k8s.io/";

/// kube-apiserver's `PatchAuditAnnotationPrefix`: the key prefix of the
/// annotation recording the JSON Patch one mutating webhook applied.
///
/// Emitted at `Request` audit level only, which is why Global Constraint
/// 18 configures `Request` rather than `Metadata`.
pub const PATCH_ANNOTATION_PREFIX: &str = "patch.webhook.admission.k8s.io/";

/// kube-apiserver's `MutationAuditAnnotationFailedOpenKeyPrefix`: the key
/// prefix of the annotation recording that a mutating webhook call failed
/// and was failed open under `failurePolicy: Ignore`.
///
/// Its value is the bare webhook name, not a JSON payload. Note that this
/// prefix *ends with* [`MUTATION_ANNOTATION_PREFIX`].
pub const FAILED_OPEN_MUTATION_ANNOTATION_PREFIX: &str =
    "failed-open.mutation.webhook.admission.k8s.io/";

/// The key prefix of a *validating* webhook's audit annotation.
///
/// Recognized only to the extent of noticing that such a key exists --
/// see this module's documentation ("Why validating webhooks are never
/// invented, only counted"). No payload behind this prefix is ever
/// parsed, and no [`WebhookInvocation`] is ever built from one.
pub const VALIDATION_ANNOTATION_PREFIX: &str = "validation.webhook.admission.k8s.io/";

/// The only `patchType` a mutating webhook annotation can carry: upstream
/// `addPatchAnnotation` refuses to write an annotation for any other
/// type.
const JSON_PATCH_TYPE: &str = "JSONPatch";

/// The literal prefix of an annotation key's `round_<r>_index_<i>`
/// suffix.
const ROUND_PREFIX: &str = "round_";

/// The literal separator between the round and index digits of an
/// annotation key's suffix.
const INDEX_SEPARATOR: &str = "_index_";

/// One fixture's own admission evidence could not be reconstructed, so no
/// trace at all is reported for it.
///
/// Deliberately fatal rather than partial: see this module's
/// documentation ("Why a malformed annotation is fatal here, and not in
/// the reader"). Every variant names the annotation key it failed on and
/// never the value behind it (Global Constraint 14) -- the key is a
/// kube-apiserver-generated constant plus decimal digits, the value is
/// request-object content.
#[derive(Debug, Error)]
pub enum TraceError {
    /// Reconstruction was attempted from an event that is not the
    /// request's final `ResponseComplete` one.
    ///
    /// Admission annotations are attached while the request is served, so
    /// an earlier stage carries none -- and an empty annotation map from
    /// such an event would otherwise be read as the positive claim that
    /// no mutating webhook ran.
    #[error(
        "audit event {audit_id} is at stage {stage}, not ResponseComplete: admission annotations \
         are only present on a request's final event, so no mutating trace can be reconstructed \
         from this one"
    )]
    NotResponseComplete {
        /// The `auditID` of the event reconstruction was attempted from.
        audit_id: String,
        /// The stage that event was actually at.
        stage: String,
    },
    /// An annotation key carried a recognized mutating-webhook prefix but
    /// not the `round_<round>_index_<index>` suffix kube-apiserver
    /// generates, so the invocation it describes cannot be placed in the
    /// chain.
    #[error(
        "mutating webhook audit annotation key {key:?} does not end in the \
         round_<round>_index_<index> form kube-apiserver writes, so the invocation it records \
         cannot be placed in the admission chain"
    )]
    AnnotationKey {
        /// The full annotation key, verbatim.
        key: String,
    },
    /// An annotation's JSON payload is not the shape kube-apiserver
    /// writes.
    ///
    /// Carries a value-free classification of the JSON failure (shared
    /// with [`crate::audit_reader`]'s own unparsable-line diagnostic) and
    /// the parser's column, never the payload text -- a patch payload
    /// embeds request-object content.
    #[error(
        "mutating webhook audit annotation {key:?} is not a parsable {expected} payload \
         ({category} error at column {column}); the annotation's value is withheld"
    )]
    AnnotationPayload {
        /// The full annotation key, verbatim.
        key: String,
        /// Which payload shape was expected: `"mutation"` or `"patch"`.
        expected: &'static str,
        /// A stable, value-free name for the kind of JSON failure.
        category: &'static str,
        /// The column the JSON parser failed at.
        column: usize,
    },
    /// A patch annotation declared a `patchType` other than `JSONPatch`.
    ///
    /// Unreachable against current kube-apiserver, whose
    /// `addPatchAnnotation` writes no annotation at all for any other
    /// type. Reported rather than ignored because the alternative would
    /// be to present some future patch dialect as though it were a JSON
    /// Patch.
    #[error(
        "mutating webhook audit annotation {key:?} declares patchType {patch_type:?}, but only \
         {JSON_PATCH_TYPE:?} can be reported as a JSON Patch"
    )]
    UnsupportedPatchType {
        /// The full annotation key, verbatim.
        key: String,
        /// The `patchType` the payload declared.
        patch_type: String,
    },
}

/// kube-apiserver's `MutationAuditAnnotation`: what a
/// [`MUTATION_ANNOTATION_PREFIX`] annotation's value deserializes into.
///
/// No `deny_unknown_fields`, for the same reason [`AuditEvent`] has none:
/// this project supports three Kubernetes minor versions at a time
/// (Global Constraint 10), and a field added upstream must not turn every
/// trace on a newer cluster into a [`TraceError`]. Every field here is
/// required, because every one is non-`omitempty` upstream.
#[derive(Debug, Deserialize)]
struct MutationAnnotation {
    /// The `MutatingWebhookConfiguration` the webhook belongs to.
    configuration: String,
    /// The webhook's own name within that configuration.
    webhook: String,
    /// Whether applying the webhook's patch actually changed the object.
    /// See this module's documentation for how little `false` proves.
    mutated: bool,
}

/// kube-apiserver's `PatchAuditAnnotation`: what a
/// [`PATCH_ANNOTATION_PREFIX`] annotation's value deserializes into.
///
/// `patch` and `patchType` are `omitempty` upstream, yet both are
/// required here. That is not an oversight: `addPatchAnnotation` is only
/// ever called after the dispatcher's own `if len(patchObj) == 0` early
/// return, and only with `PatchTypeJSONPatch`, so neither field can
/// actually be omitted. An annotation that omits one is not a patch
/// annotation this project can report, and saying so is more honest than
/// emitting a patch-less patch.
#[derive(Debug, Deserialize)]
struct PatchAnnotation {
    /// The `MutatingWebhookConfiguration` the webhook belongs to.
    configuration: String,
    /// The webhook's own name within that configuration.
    webhook: String,
    /// The JSON Patch the webhook returned, as typed operations rather
    /// than a raw `serde_json::Value`, matching
    /// [`WebhookInvocation::patch`].
    patch: Vec<json_patch::PatchOperation>,
    /// The declared patch dialect; see [`JSON_PATCH_TYPE`].
    #[serde(rename = "patchType")]
    patch_type: String,
}

/// The merge key of Task 3.6 Step 3: `(round, index, configuration,
/// webhook)`.
///
/// Ordered on exactly that tuple, so a [`BTreeMap`] keyed by it yields
/// invocations in `(round, index)` order for free -- numerically, which
/// is the point, since `round_0_index_10` sorts before `round_0_index_2`
/// as text. `configuration` and `webhook` participate in the ordering
/// only as a deterministic tie-break; kube-apiserver never emits two
/// different webhooks at one `(round, index)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct InvocationKey {
    /// The admission round the invocation ran in.
    round: u32,
    /// The invocation's index within that round.
    index: u32,
    /// The `MutatingWebhookConfiguration` name.
    configuration: String,
    /// The webhook's own name.
    webhook: String,
}

/// The evidence accumulated for one [`InvocationKey`] while the
/// annotation map is scanned.
#[derive(Debug, Default)]
struct InvocationEvidence {
    /// `Some` exactly when a mutation annotation was seen for this key.
    mutated: Option<bool>,
    /// `Some` exactly when a patch annotation was seen for this key.
    patch: Option<Vec<json_patch::PatchOperation>>,
    /// Whether a failed-open annotation named this webhook at this
    /// `(round, index)`.
    failed_open: bool,
}

/// Reconstructs the chain of mutating webhooks that ran on one fixture
/// request, from that request's own `ResponseComplete` audit event.
///
/// The returned [`AdmissionTrace`] describes *only* what the mutating
/// annotations prove: invocations ordered by `(round, index)`, each with
/// the [`WebhookOutcome`] its evidence supports and
/// [`WebhookInvocation::latency`] left `None`. Read this module's
/// documentation before relying on any of those values -- in particular
/// for why `mutated: false` yields [`WebhookOutcome::Unknown`], why a
/// trace with no invocations is [`TraceEvidence::Observed`] rather than
/// [`TraceEvidence::Unavailable`], and why the presence of validating
/// annotations downgrades the trace to [`TraceEvidence::Partial`] without
/// adding anything to it.
///
/// The trace is [`TraceEvidence::Partial`] when, and only when, the event
/// proves the chain is not fully described here:
///
/// 1. an invocation reports `mutated: true` but carries no patch
///    annotation -- what a `Metadata`-level audit policy produces, and
///    Task 3.6 Step 4's case. The invocation keeps `mutated: Some(true)`
///    with `patch: None`; no patch content is ever invented for it;
/// 2. a patch annotation has no matching mutation annotation, which
///    upstream's unconditional `defer` cannot produce, so some evidence
///    was lost in transit;
/// 3. a failed-open annotation names a `(round, index, webhook)` no
///    mutation annotation covers, leaving that invocation's
///    `configuration` unrecoverable and the invocation unreportable;
/// 4. the event carries validating-webhook annotations, describing part
///    of the chain this reconstruction deliberately does not.
///
/// # Errors
///
/// Returns [`TraceError`] when this fixture's own evidence cannot be
/// read: the event is not the request's `ResponseComplete` one
/// ([`TraceError::NotResponseComplete`]), an annotation key does not
/// carry the `round_<round>_index_<index>` suffix
/// ([`TraceError::AnnotationKey`]), an annotation payload is not the JSON
/// shape kube-apiserver writes ([`TraceError::AnnotationPayload`]), or a
/// patch annotation declares a dialect other than `JSONPatch`
/// ([`TraceError::UnsupportedPatchType`]). See this module's
/// documentation for why these are fatal rather than silently dropped.
pub fn reconstruct_mutating_trace(event: &AuditEvent) -> Result<AdmissionTrace, TraceError> {
    if !event.is_response_complete() {
        return Err(TraceError::NotResponseComplete {
            audit_id: event.audit_id.clone(),
            stage: event.stage.clone(),
        });
    }

    let mut evidence: BTreeMap<InvocationKey, InvocationEvidence> = BTreeMap::new();
    // `(round, index, webhook name)` of every failed-open annotation.
    // Collected rather than applied immediately because the annotation
    // map is walked in key order, which puts `failed-open.` before
    // `mutation.`, and the failed-open value carries no `configuration`
    // to build an `InvocationKey` from on its own.
    let mut failed_open: Vec<(u32, u32, &str)> = Vec::new();
    let mut saw_validation_annotation = false;

    for (key, value) in &event.annotations {
        // The failed-open prefix is tested first because it *contains*
        // the mutation prefix as a suffix; testing it later would be
        // correct only by the accident of `strip_prefix` being anchored.
        if let Some(suffix) = key.strip_prefix(FAILED_OPEN_MUTATION_ANNOTATION_PREFIX) {
            let (round, index) = parse_round_index(key, suffix)?;
            failed_open.push((round, index, value.as_str()));
        } else if let Some(suffix) = key.strip_prefix(MUTATION_ANNOTATION_PREFIX) {
            let (round, index) = parse_round_index(key, suffix)?;
            let payload: MutationAnnotation = parse_payload(key, value, "mutation")?;
            evidence
                .entry(InvocationKey {
                    round,
                    index,
                    configuration: payload.configuration,
                    webhook: payload.webhook,
                })
                .or_default()
                .mutated = Some(payload.mutated);
        } else if let Some(suffix) = key.strip_prefix(PATCH_ANNOTATION_PREFIX) {
            let (round, index) = parse_round_index(key, suffix)?;
            let payload: PatchAnnotation = parse_payload(key, value, "patch")?;
            if payload.patch_type != JSON_PATCH_TYPE {
                return Err(TraceError::UnsupportedPatchType {
                    key: key.clone(),
                    patch_type: payload.patch_type,
                });
            }
            evidence
                .entry(InvocationKey {
                    round,
                    index,
                    configuration: payload.configuration,
                    webhook: payload.webhook,
                })
                .or_default()
                .patch = Some(payload.patch);
        } else if key.starts_with(VALIDATION_ANNOTATION_PREFIX) {
            saw_validation_annotation = true;
        }
    }

    let mut partial = saw_validation_annotation;
    for (round, index, webhook) in failed_open {
        let mut matched = false;
        for (key, entry) in &mut evidence {
            if key.round == round && key.index == index && key.webhook == webhook {
                entry.failed_open = true;
                matched = true;
            }
        }
        // A failed-open annotation with no mutation annotation beside it:
        // its value is only the webhook name, so the invocation's
        // `configuration` cannot be recovered and the invocation cannot
        // be reported at all. Never guessed from another entry's
        // configuration.
        partial |= !matched;
    }

    let mut invocations = Vec::with_capacity(evidence.len());
    for (key, entry) in evidence {
        let InvocationEvidence {
            mutated,
            patch,
            failed_open,
        } = entry;
        // Reason 1: the object was changed but the change itself was not
        // recorded. Reason 2: a patch with no invocation record beside
        // it.
        partial |= (mutated == Some(true) && patch.is_none()) || mutated.is_none();
        let outcome = if failed_open {
            // The call itself failed; the deferred `mutated: false`
            // beside it is a by-product of that failure, not a verdict.
            WebhookOutcome::Errored
        } else if mutated == Some(true) || patch.is_some() {
            // Either fact places the webhook past kube-apiserver's own
            // `if !result.Allowed` early return.
            WebhookOutcome::Allowed
        } else {
            // `mutated: false` alone. Invoked -- nothing more.
            WebhookOutcome::Unknown
        };
        invocations.push(WebhookInvocation {
            configuration: key.configuration,
            webhook: key.webhook,
            round: key.round,
            index: key.index,
            mutated,
            patch,
            // Global Constraint 15: these annotations carry no timing at
            // all, and Task 3.8 supplies latency from metrics separately.
            latency: None,
            outcome,
        });
    }

    Ok(AdmissionTrace {
        evidence: if partial {
            TraceEvidence::Partial
        } else {
            TraceEvidence::Observed
        },
        invocations,
    })
}

/// Parses the `round_<round>_index_<index>` suffix of an annotation key.
///
/// `key` is carried only so a failure can name the whole key rather than
/// the fragment.
fn parse_round_index(key: &str, suffix: &str) -> Result<(u32, u32), TraceError> {
    round_index_of(suffix).ok_or_else(|| TraceError::AnnotationKey {
        key: key.to_string(),
    })
}

/// The `(round, index)` a key suffix encodes, or `None` if it is not the
/// exact form `fmt.Sprintf("round_%d_index_%d", ...)` produces.
fn round_index_of(suffix: &str) -> Option<(u32, u32)> {
    let rest = suffix.strip_prefix(ROUND_PREFIX)?;
    let (round, index) = rest.split_once(INDEX_SEPARATOR)?;
    Some((decimal(round)?, decimal(index)?))
}

/// A plain decimal `u32`.
///
/// Stricter than [`str::parse`] on purpose: `parse` accepts a leading
/// `+`, so `round_+0_index_0` and `round_0_index_0` would name the same
/// invocation through two different keys and merge into one entry with
/// silently doubled evidence. `%d` never emits a sign, so requiring
/// ASCII digits costs nothing and removes the aliasing.
fn decimal(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Deserializes one annotation payload, converting a parse failure into a
/// [`TraceError::AnnotationPayload`] that carries no payload text.
///
/// `expected` names the shape for the error message. See this module's
/// documentation for why `serde_json::Error`'s own `Display` is never
/// used here (Global Constraint 14).
fn parse_payload<T: DeserializeOwned>(
    key: &str,
    value: &str,
    expected: &'static str,
) -> Result<T, TraceError> {
    serde_json::from_str(value).map_err(|error| TraceError::AnnotationPayload {
        key: key.to_string(),
        expected,
        category: json_error_category(&error),
        column: error.column(),
    })
}

#[cfg(test)]
mod tests {
    use super::{FAILED_OPEN_MUTATION_ANNOTATION_PREFIX, MUTATION_ANNOTATION_PREFIX, decimal};

    /// The mutation this test exists to kill: classifying annotation keys
    /// with `contains` instead of `strip_prefix`, which would read every
    /// failed-open annotation as a mutation annotation and then fail to
    /// parse its bare webhook name as JSON.
    #[test]
    fn the_failed_open_prefix_contains_the_mutation_prefix_but_never_starts_with_it() {
        assert!(FAILED_OPEN_MUTATION_ANNOTATION_PREFIX.contains(MUTATION_ANNOTATION_PREFIX));
        assert!(!FAILED_OPEN_MUTATION_ANNOTATION_PREFIX.starts_with(MUTATION_ANNOTATION_PREFIX));
    }

    /// The mutation this test exists to kill: reaching for `str::parse`
    /// directly, which accepts `+0` and would let two spellings of the
    /// same key merge into one doubly-attested invocation.
    #[test]
    fn a_round_or_index_is_plain_ascii_digits_only() {
        assert_eq!(decimal("0"), Some(0));
        assert_eq!(decimal("10"), Some(10));
        assert_eq!(decimal("+0"), None);
        assert_eq!(decimal("-1"), None);
        assert_eq!(decimal(" 1"), None);
        assert_eq!(decimal(""), None);
        assert_eq!(decimal("4294967296"), None);
    }
}
