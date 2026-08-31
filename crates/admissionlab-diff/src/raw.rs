//! The raw, diagnostics-only object diff.
//!
//! This module answers one narrow question -- *what literally differs
//! between these two JSON documents* -- and nothing else. Its output is
//! evidence for a human (and for the report's "show me the actual
//! difference" panel), not a claim about admission behavior. Claims live
//! in [`crate::types::SemanticChange`], are produced by the
//! `diff_*` functions in this crate, and are the only thing policy grades
//! or a run's exit status depends on.
//!
//! Keeping the two vocabularies apart is load-bearing, not stylistic. A
//! raw difference existing does not license emitting a semantic change:
//! Task 4.4's rejected-to-rejected case has a perfectly real raw
//! difference (the rejection message changed) and must produce an *empty*
//! semantic list, because "the object is still denied" is not a behavior
//! regression. Conversely a semantic change may exist where the raw diff
//! is silent (a webhook latency change touches no field of the object at
//! all). Nothing in this module may therefore be reached for as a
//! shortcut to classification.
//!
//! # Determinism
//!
//! Global Constraint 7 requires classification to be deterministic, and
//! this diff feeds the diagnostics a report renders, so its ordering must
//! be stable across processes and machines. It is, and for reasons worth
//! writing down rather than assuming:
//!
//! - [`raw_object_diff`] wraps [`json_patch::diff`], whose traversal is
//!   itself deterministic given a deterministic map iteration order: it
//!   walks the candidate object's keys, then the baseline-only keys, then
//!   arrays by ascending index (verified by reading `json-patch-4.2.0`'s
//!   own `diff.rs`, not assumed from its documentation).
//! - `serde_json::Value::Object` is a `BTreeMap` in this workspace's
//!   dependency graph, so key iteration is sorted, and two `Value`s
//!   parsed from JSON texts that list the same keys in different orders
//!   produce byte-identical diffs. That holds only while `serde_json`'s
//!   `preserve_order` feature stays off (it swaps the backing map for an
//!   insertion-ordered `IndexMap`). It is off: nothing in this workspace
//!   requests it, and `json-patch` asks for it only in its own
//!   `[dev-dependencies]`, which never reach this graph -- confirmed by
//!   reading that crate's manifest and by `Cargo.lock` resolving
//!   `serde_json` without `indexmap`. `tests/types.rs`'s
//!   `raw_object_diff_ignores_source_key_order` is the regression test
//!   that fails loudly if a future dependency turns the feature on.
//!
//! # Why a wrapper type rather than `json_patch::PatchOperation`
//!
//! [`RawChange`] is this crate's own type so the diagnostic vocabulary a
//! report renders is owned here and cannot be changed out from under the
//! report by a dependency bump. The mapping is lossless in both
//! directions: every one of RFC 6902's six operations is representable,
//! including the `from` pointer that only `move`/`copy` carry, so
//! converting a `PatchOperation` never has to drop or invent a field.
//! `json_patch::diff` itself only ever emits `add`/`remove`/`replace`
//! (again, read from its source), but a total mapping means that fact is
//! an observation about the current producer rather than an assumption
//! this type would break on.

use json_patch::PatchOperation;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which RFC 6902 operation a [`RawChange`] describes.
///
/// Wire tags are pinned explicitly to RFC 6902's own lowercase operation
/// names, so a serialized [`RawChange`] list is a valid JSON Patch
/// document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RawChangeOp {
    /// RFC 6902 `add`: `path` exists only in the candidate.
    #[serde(rename = "add")]
    Add,
    /// RFC 6902 `remove`: `path` exists only in the baseline.
    #[serde(rename = "remove")]
    Remove,
    /// RFC 6902 `replace`: `path` exists on both sides with different
    /// values.
    #[serde(rename = "replace")]
    Replace,
    /// RFC 6902 `move`.
    #[serde(rename = "move")]
    Move,
    /// RFC 6902 `copy`.
    #[serde(rename = "copy")]
    Copy,
    /// RFC 6902 `test`.
    #[serde(rename = "test")]
    Test,
}

/// One RFC 6902 patch operation turning the baseline document into the
/// candidate document.
///
/// Serializes to exactly the RFC 6902 object shape -- `op` and `path`
/// always, `value` only for operations that carry one, `from` only for
/// `move`/`copy` -- so a `Vec<RawChange>` serializes to a valid JSON
/// Patch document that external tooling can apply directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawChange {
    /// The operation.
    pub op: RawChangeOp,
    /// RFC 6901 JSON pointer to the location the operation applies to.
    pub path: String,
    /// The operation's value, for `add`, `replace`, and `test`. `None`
    /// for the operations RFC 6902 defines without one; a JSON `null`
    /// value is `Some(Value::Null)` and stays distinguishable from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// The source pointer, for `move` and `copy` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// Computes the RFC 6902 patch that turns `baseline` into `candidate`.
///
/// Diagnostics only -- see this module's documentation for why the result
/// must never be used to decide whether a semantic change occurred. The
/// returned order is deterministic; see the same documentation for what
/// that rests on.
///
/// An empty result means the two documents are equal.
#[must_use]
pub fn raw_object_diff(baseline: &Value, candidate: &Value) -> Vec<RawChange> {
    json_patch::diff(baseline, candidate)
        .0
        .iter()
        .map(RawChange::from_operation)
        .collect()
}

impl RawChange {
    /// Converts one [`json_patch::PatchOperation`] into this crate's own
    /// representation.
    ///
    /// Total and lossless: every RFC 6902 operation maps, and each
    /// operation's optional `value`/`from` is carried across exactly when
    /// that operation defines it.
    #[must_use]
    pub fn from_operation(operation: &PatchOperation) -> Self {
        match operation {
            PatchOperation::Add(op) => Self {
                op: RawChangeOp::Add,
                path: op.path.to_string(),
                value: Some(op.value.clone()),
                from: None,
            },
            PatchOperation::Remove(op) => Self {
                op: RawChangeOp::Remove,
                path: op.path.to_string(),
                value: None,
                from: None,
            },
            PatchOperation::Replace(op) => Self {
                op: RawChangeOp::Replace,
                path: op.path.to_string(),
                value: Some(op.value.clone()),
                from: None,
            },
            PatchOperation::Move(op) => Self {
                op: RawChangeOp::Move,
                path: op.path.to_string(),
                value: None,
                from: Some(op.from.to_string()),
            },
            PatchOperation::Copy(op) => Self {
                op: RawChangeOp::Copy,
                path: op.path.to_string(),
                value: None,
                from: Some(op.from.to_string()),
            },
            PatchOperation::Test(op) => Self {
                op: RawChangeOp::Test,
                path: op.path.to_string(),
                value: Some(op.value.clone()),
                from: None,
            },
        }
    }
}
