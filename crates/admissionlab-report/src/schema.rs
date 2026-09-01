//! JSON Schema generation for the stable `v1` result document.
//!
//! ROADMAP Task 9.1 publishes `schemas/result-v1.json` next to the
//! two schemas `admissionlab-spec` and `admissionlab-core` already
//! publish, and for the same reason: a consumer that wants to validate a
//! `result.json` should not have to read Rust to find out what is in
//! one.
//!
//! The schema is generated from [`crate::wire::ResultDocument`] -- the
//! *same* types that serialize a run's `result.json` -- so it describes
//! the document that is actually written rather than a hand-maintained
//! description of it. `tests/result_schema.rs` regenerates it and
//! compares against the checked-in file, which is what keeps the two in
//! step.
//!
//! `schemas/result-v1beta1.json` stays checked in beside it and has no
//! generator behind it any more: it is the contract the Beta documents
//! already in users' artifact directories were written against, and it is
//! the reference `tests/stable_schema.rs` measures the stable schema
//! against. A generator can only describe the type that exists now, so a
//! "v1beta1 generator" would silently start describing v1 the moment a
//! field was added -- which is how a frozen schema stops being frozen.
//!
//! # Determinism
//!
//! [`result_v1_json_schema`] produces byte-for-byte identical
//! output on every run, for exactly the two reasons
//! `admissionlab_spec::schema` documents at length: `schemars::Schema`'s
//! [`serde::Serialize`] hoists a handful of well-known keywords and then
//! iterates the underlying `serde_json::Map` directly, and this
//! workspace enables neither `schemars`'s nor `serde_json`'s
//! `preserve_order` feature, so that map is `BTreeMap`-backed and
//! iterates lexicographically. Turning `preserve_order` on anywhere in
//! this workspace would break both generated schemas at once and should
//! be treated as a determinism regression.
//!
//! # Fidelity across crate boundaries
//!
//! A result document embeds evidence types owned by
//! `admissionlab-admission`, `admissionlab-diff`, `admissionlab-policy`,
//! `admissionlab-gateway` and `admissionlab-core`. Those types carry
//! their own `#[derive(schemars::JsonSchema)]` (added mechanically by
//! Task 7.2, alongside the `Serialize` derive they already had), rather
//! than being described here by hand: a hand-written description of a
//! foreign type is a second source of truth that nothing forces to stay
//! correct, which is the specific failure a generated schema exists to
//! prevent.

use schemars::Schema;

use crate::wire::ResultDocument;

/// Generates the JSON Schema for the stable
/// `admissionlab.io/result/v1` result document.
///
/// See this module's documentation for why generating this twice always
/// produces byte-for-byte identical output, and where the checked-in
/// copy is compared against it.
#[must_use]
pub fn result_v1_json_schema() -> Schema {
    schemars::schema_for!(ResultDocument<'static>)
}
