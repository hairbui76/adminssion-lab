//! JSON Schema generation for the lab configuration model, one function
//! per supported `apiVersion`.
//!
//! All three schemas are checked in and all three are
//! regenerate-and-compare verified:
//!
//! | function | checked-in file | test |
//! |---|---|---|
//! | [`v1_json_schema`] | `schemas/admissionlab-v1.json` | `tests/stable_schema.rs` |
//! | [`v1beta1_json_schema`] | `schemas/admissionlab-v1beta1.json` | `tests/schema.rs` |
//! | [`v1alpha1_json_schema`] | `schemas/admissionlab-v1alpha1.json` | `tests/schema.rs` |
//!
//! The older schemas stay checked in for as long as their models are read
//! (ROADMAP Task 7.1 Step 2, Task 9.1 Step 3), and their comparison tests
//! are what make [`crate::v1alpha1`]'s and [`crate::v1beta1`]'s freezes
//! enforceable rather than merely stated: any change to either model — or
//! to a shared type in [`crate::model`] that it references — fails one of
//! those tests.
//!
//! Every one of the three has a generator behind it, unlike the
//! superseded *result* and *run manifest* schemas, and that is not an
//! inconsistency: a configuration model is still parsed at every version
//! this build reads, so a generator here describes a type that genuinely
//! still exists. A frozen-file-with-no-generator is the right answer only
//! where the old shape has no Rust type left (see
//! `docs/schema-migrations.md`, "How the rule is enforced").
//!
//! # Determinism
//!
//! Each generator must produce byte-for-byte identical output on every
//! run — `tests/schema.rs` compares them against the checked-in files and
//! would otherwise flap. Two properties of this crate's dependency
//! configuration make that true without any extra sorting step on this
//! crate's part:
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
//! these functions produce is already canonical: every key at every
//! nesting level is ordered the same way on every run, on every machine.
//! Enabling `preserve_order` anywhere in this workspace in the future
//! would break that guarantee and should be treated as a determinism
//! regression.

use schemars::Schema;

use crate::v1::V1Lab;
use crate::v1alpha1::LabSpec;
use crate::v1beta1::V1Beta1Lab;

/// Generates the JSON Schema for the current, stable `admissionlab.io/v1`
/// `admissionlab.yaml` configuration format, derived directly from
/// [`V1Lab`] and the types it references.
///
/// This is the schema an editor should be pointed at for a lab file
/// written today. It differs from [`v1beta1_json_schema`]'s output in
/// exactly two places, and both are consequences of the version rather
/// than of the shape: the `apiVersion` `const` and the root schema's
/// `title`. Everything under `$defs` is generated from the very same Rust
/// types, because [`V1Lab`] shares them with [`V1Beta1Lab`] rather than
/// declaring copies (see [`crate::v1`]).
///
/// See this module's documentation for why generating this twice always
/// produces byte-for-byte identical output.
#[must_use]
pub fn v1_json_schema() -> Schema {
    schemars::schema_for!(V1Lab)
}

/// Generates the JSON Schema for the frozen `admissionlab.io/v1beta1`
/// `admissionlab.yaml` configuration format, derived directly from
/// [`V1Beta1Lab`] and the types it references.
///
/// Still generated, still checked in, and still compared byte-for-byte,
/// for the reason [`v1alpha1_json_schema`] is: Public Beta configurations
/// remain readable, so an editor validating one needs its schema, and the
/// comparison is what keeps the Beta model frozen.
///
/// It is generated from the same `#[derive(JsonSchema)]`
/// and `#[serde(rename_all = "camelCase")]`/`#[serde(rename = "...")]`
/// attributes that govern parsing, so it always shows the exact keys the
/// loader accepts — including the two Beta renames
/// (`absoluteIncreaseMillis`, `reconciliationTimeoutMillis`), which are
/// therefore impossible to document wrongly here.
///
/// See this module's documentation for why generating this twice always
/// produces byte-for-byte identical output.
#[must_use]
pub fn v1beta1_json_schema() -> Schema {
    schemars::schema_for!(V1Beta1Lab)
}

/// Generates the JSON Schema for the frozen `admissionlab.io/v1alpha1`
/// `admissionlab.yaml` configuration format, derived directly from
/// [`LabSpec`] and the types it references.
///
/// Still generated, still checked in, and still compared byte-for-byte:
/// Public Alpha configurations remain readable through at least v1.0, so
/// an editor validating one needs its schema — and, more importantly,
/// that comparison is the mechanism that keeps the Alpha model frozen
/// (see this module's documentation).
///
/// The returned [`Schema`] uses `camelCase` property names
/// (`apiVersion`, `expectationsFile`, `failOn`, and so on) because it is
/// generated from the same `#[derive(JsonSchema)]` and
/// `#[serde(rename_all = "camelCase")]` attributes that governed parsing
/// in [`crate::load_lab`] — the schema can never drift from the spelling
/// users actually typed.
#[must_use]
pub fn v1alpha1_json_schema() -> Schema {
    schemars::schema_for!(LabSpec)
}
