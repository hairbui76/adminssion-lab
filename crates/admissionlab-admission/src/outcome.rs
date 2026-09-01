//! What the API server decided about one fixture on one side.
//!
//! [`AdmissionOutcome`] is the top-level observation Phase 4 compares:
//! one fixture, replayed through one side (baseline or candidate), and
//! everything this project managed to observe about how the API server
//! and its admission chain handled it. [`crate::trace`] defines the
//! nested webhook-chain detail ([`crate::trace::AdmissionTrace`]).

use std::time::Duration;

use admissionlab_core::{Diagnostic, FixtureId, Side};
// ROADMAP Task 7.2 (frozen `admissionlab.io/result/v1` result
// schema): every type this file defines is embedded verbatim in that
// document, so the schema generated from the result model has to
// describe it. Derives and `#[schemars(with = ...)]` restatements of
// what the existing `serialize_with` helpers already emit -- no field,
// name, or semantic change.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use crate::trace::AdmissionTrace;

/// The API server's final verdict on one fixture, on one side.
///
/// This is the *server's* decision about whether the object was
/// admitted -- not a per-webhook opinion (see
/// [`crate::trace::WebhookOutcome`] for that). Represented as an
/// externally tagged enum: `Accepted` serializes as the bare JSON string
/// `"accepted"`; the other two variants serialize as a single-key object
/// (for example `{"rejected":{"code":403,"message":"..."}}`). Each
/// variant's wire tag is pinned with an explicit `#[serde(rename)]`
/// rather than left to derive from the Rust identifier, so renaming a
/// variant in Rust can never silently change the JSON report contract
/// (controller supplement §5, Task 3.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AdmissionDecision {
    /// The object was admitted.
    #[serde(rename = "accepted")]
    Accepted,
    /// The object was rejected. `code` is the HTTP-style status code the
    /// API server (or a rejecting webhook) reported, when one was
    /// present in the observed response; `None` means the rejection was
    /// observed but no code was -- this is never fabricated as a
    /// plausible default such as `403`, so a rejection with a known code
    /// stays distinguishable from one without (Global Constraint 15).
    #[serde(rename = "rejected")]
    Rejected {
        /// The rejecting response's status code, if the audit evidence
        /// carried one.
        code: Option<u16>,
        /// A human-readable rejection message, taken from the observed
        /// response.
        message: String,
    },
    /// The fixture could not be replayed as a real admission decision
    /// because the side does not support (or this run did not attempt)
    /// the dry-run mechanism replay depends on. `message` explains why.
    #[serde(rename = "unsupported_dry_run")]
    UnsupportedDryRun {
        /// Why dry-run replay was not possible for this fixture on this
        /// side.
        message: String,
    },
}

/// Everything observed about one fixture's admission on one side.
///
/// Implements [`Serialize`] only, never [`serde::Deserialize`]:
/// `diagnostics` holds [`Diagnostic`], which
/// `admissionlab-core` itself implements `Serialize` for but
/// deliberately not `Deserialize` (see that type's own module
/// documentation -- it is a one-way, emit-only report vocabulary), and
/// this task must not modify `admissionlab-core` to add that impl
/// (controller supplement §3). `AdmissionOutcome` is captured once from a
/// live cluster and only ever serialized *outward* into the run's JSON
/// report; nothing in this project reads one back in from JSON, so the
/// asymmetry costs this crate nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AdmissionOutcome {
    /// Which fixture this outcome observed.
    #[serde(serialize_with = "serialize_fixture_id")]
    #[schemars(with = "String")]
    pub fixture_id: FixtureId,
    /// Which side (baseline or candidate) produced this outcome.
    #[serde(serialize_with = "serialize_side")]
    #[schemars(with = "String")]
    pub side: Side,
    /// The API server's final verdict.
    pub decision: AdmissionDecision,
    /// Warnings the API server attached to its response, verbatim and in
    /// the order observed.
    pub warnings: Vec<String>,
    /// Wall-clock time from issuing the replay request to receiving the
    /// API server's response. Unlike a per-webhook
    /// [`crate::trace::WebhookInvocation::latency`], this is always
    /// measured directly by the code that issues the request, so it is a
    /// bare [`Duration`], never an [`Option`].
    #[serde(serialize_with = "serialize_duration_millis")]
    #[schemars(with = "u64")]
    pub total_latency: Duration,
    /// The object as the API server persisted it, if the decision
    /// admitted it and the persisted form was observed. `None` covers
    /// both "the object was rejected" and "it was accepted but its final
    /// form could not be captured" -- this type cannot itself distinguish
    /// those two; a caller needing to must consult `decision`.
    pub final_object: Option<Value>,
    /// What was observed about the webhook chain this fixture passed
    /// through.
    pub trace: AdmissionTrace,
    /// Diagnostics collected while capturing this outcome.
    pub diagnostics: Vec<Diagnostic>,
}

/// Serializes a [`FixtureId`] as its bare string form
/// ([`FixtureId::as_str`]), matching every other place this identifier's
/// string form already appears (filesystem paths, cluster name
/// suffixes) rather than exposing whatever internal shape
/// `admissionlab_core::FixtureId` happens to use.
fn serialize_fixture_id<S: Serializer>(
    value: &FixtureId,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value.as_str())
}

/// Serializes a [`Side`] as its stable lowercase name
/// ([`Side::as_str`]): `"baseline"` or `"candidate"` -- the same strings
/// `admissionlab_core::Side` already documents as a stable serialization
/// value.
// `serde(serialize_with = "...")` always calls this function with a
// reference to the field, regardless of whether the field type is
// `Copy` -- confirmed empirically: changing this to take `Side` by
// value fails to compile against the derive's generated call site.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn serialize_side<S: Serializer>(value: &Side, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value.as_str())
}

/// Serializes a [`Duration`] as a plain integer number of milliseconds.
///
/// Mirrors `admissionlab_spec::model`'s own `duration_millis` module
/// (same representation, same saturating-rather-than-panicking overflow
/// handling) rather than serde's default `{secs, nanos}` object shape,
/// which would be needlessly verbose for a value nothing here re-parses.
fn serialize_duration_millis<S: Serializer>(
    value: &Duration,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // `as_millis()` returns `u128`; saturate rather than `as`-cast so a
    // (practically impossible) multi-million-year value clamps to
    // `u64::MAX` instead of silently wrapping.
    let millis = u64::try_from(value.as_millis()).unwrap_or(u64::MAX);
    serializer.serialize_u64(millis)
}
