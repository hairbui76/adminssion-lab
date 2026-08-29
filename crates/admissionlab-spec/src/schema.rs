//! JSON Schema generation for the `v1alpha1` [`crate::LabSpec`] model.
//!
//! # Determinism
//!
//! [`v1alpha1_json_schema`] must produce byte-for-byte identical output on
//! every run — `tests/schema.rs` compares it against the checked-in
//! `schemas/admissionlab-v1alpha1.json` and would otherwise flap. Two
//! properties of this crate's dependency configuration make that true
//! without any extra sorting step on this crate's part:
//!
//! - `schemars::Schema`'s [`serde::Serialize`] implementation does not
//!   serialize its underlying JSON value as-is: it explicitly reorders a
//!   handful of well-known keywords (`$schema`, `title`, `type`,
//!   `properties`, and so on) to the front for human readability, and —
//!   this is the load-bearing part — orders every *other* key by
//!   iterating the underlying `serde_json::Map` directly.
//! - This crate does not enable `schemars`'s or `serde_json`'s
//!   `preserve_order` feature (neither is a default feature, and neither
//!   is turned on anywhere in this workspace), so `serde_json::Map` stays
//!   backed by a `BTreeMap` rather than an insertion-order-preserving
//!   `IndexMap`. Iterating it therefore always yields keys in
//!   lexicographic order, regardless of struct field declaration order or
//!   `SchemaGenerator` internals.
//!
//! Together, that means [`serde_json::to_string_pretty`] on the value
//! this module produces is already canonical: every key at every nesting
//! level is ordered the same way on every run, on every machine. Enabling
//! `preserve_order` anywhere in this workspace in the future would break
//! that guarantee and should be treated as a determinism regression.

use schemars::Schema;

use crate::model::LabSpec;

/// Generates the JSON Schema for the `v1alpha1` `admissionlab.yaml`
/// configuration format, derived directly from [`LabSpec`] and the types
/// it references.
///
/// The returned [`Schema`] uses `camelCase` property names
/// (`apiVersion`, `expectationsFile`, `failOn`, and so on) because it is
/// generated from the same `#[derive(JsonSchema)]` and
/// `#[serde(rename_all = "camelCase")]` attributes that govern parsing in
/// [`crate::load_lab`] — the schema can never drift from the spelling
/// users actually type.
///
/// See this module's documentation for why generating this twice always
/// produces byte-for-byte identical output.
#[must_use]
pub fn v1alpha1_json_schema() -> Schema {
    schemars::schema_for!(LabSpec)
}
