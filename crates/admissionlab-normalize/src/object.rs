//! Deterministic normalization of one Kubernetes object (Task 4.1).
//!
//! [`normalize_object`] takes the object a fixture produced — for Alpha,
//! the `final_object` of a server-side dry-run `CREATE` (Global
//! Constraint 16) — applies a [`NormalizationProfile`]'s rules to a copy
//! of it, and hands back the result together with a
//! [`NormalizationEvidence`] record of what it actually did.
//!
//! # Determinism is the whole point
//!
//! The same input value and the same profile always produce a
//! byte-identical [`NormalizedObject`]. Three things make that true, and
//! all three are load-bearing rather than incidental:
//!
//! 1. **Rule order is a `Vec` order, everywhere.** Tiers run
//!    `built_in` → `recipe` → `user`, and rules within a tier run in the
//!    order they were written. No set, map, or hash iteration influences
//!    the result.
//! 2. **`serde_json::Map` is ordered.** This workspace resolves
//!    `serde_json` without its `preserve_order` feature (verified
//!    against `Cargo.lock`: `serde_json`'s dependency list has no
//!    `indexmap`), so an object's keys are stored and serialized in
//!    sorted order regardless of the order they were parsed in. Nothing
//!    in this module reorders object keys itself; `trace.rs` (Task 4.2)
//!    does canonicalize explicitly, and its own documentation explains
//!    why it does not rely on this.
//! 3. **Sorting is stable and total.** See [`sort_named_array`].
//!
//! # `applied_rules` means "matched *and* changed something"
//!
//! Not "was configured". A rule appears in
//! [`NormalizationEvidence::applied_rules`] only if applying it left the
//! object different from how it found it. Two reasons, both about what
//! Phase 4 uses this record for:
//!
//! - The consumer is a **diff explanation**: "these two objects compare
//!   equal, and here is what normalization removed on the way". A rule
//!   that matched nothing removed nothing and explains nothing, so
//!   listing it adds a line a reader has to rule out by hand. Under the
//!   "configured" reading every normalized object in a run would carry
//!   the entire profile verbatim, and the handful of entries that
//!   actually suppressed something would be indistinguishable from the
//!   rest.
//! - The record is **already lossless the other way round**. The profile
//!   itself says what was configured, and the caller has it. Only "what
//!   actually happened to *this* object" is information the caller
//!   cannot reconstruct — so that is what evidence carries.
//!
//! A no-op is therefore silent, including for a rule that changed
//! nothing because the field was already in normal form (a
//! `SortNamedArray` over an array that was already sorted).
//!
//! # A configured-but-never-matching rule gets no warning
//!
//! Deliberately, and this is the awkward one. A user rule that matches
//! nothing *could* be a typo worth reporting. It is not reported here,
//! because [`normalize_object`] is the wrong altitude to report it from:
//!
//! - It sees **one object**. A perfectly correct rule such as
//!   `/spec/nodeName` matches pods and misses every `ConfigMap`,
//!   `Service`, and namespace in the same corpus. Warning per object
//!   would emit the warning overwhelmingly on rules that are working as
//!   intended, and `warnings` feeds user-facing report text where that
//!   volume destroys the signal the broad-parent warnings below carry.
//! - The genuinely useful question — "did this rule match *anything*, in
//!   the whole run?" — is a whole-run question. It needs the union of
//!   every object's `applied_rules`, which a caller already has, and
//!   which is exactly where a profile linter (an `admissionlab doctor`
//!   check, or the run-summary step) belongs. That is the seam; this
//!   function's silence is what keeps it available rather than
//!   pre-empted by noise.
//!
//! Absence from `applied_rules` already states the fact losslessly. What
//! *is* warned about is different in kind: a rule that matched and
//! removed a whole subtree, which is a suppression the user cannot see
//! by reading the diff, because the diff no longer contains it.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::pointer::{self, JsonPointer, PointerError};
use crate::rules::{NormalizationProfile, NormalizeRule, RuleTier};

/// The reference-token path to an object's own annotations map. Used
/// both to build a [`NormalizeRule::RemoveAnnotation`] pointer and to
/// find the map again afterwards.
const ANNOTATIONS_PATH: [&str; 2] = ["metadata", "annotations"];

/// A rule could not be applied at all, so no [`NormalizedObject`] was
/// produced.
///
/// Both variants are *configuration* errors — a rule as written cannot
/// mean anything against any document. A well-formed rule that simply
/// does not match the object at hand is never an error; it is a silent
/// no-op (see this module's documentation).
///
/// [`normalize_object`] checks every rule in the profile before it
/// changes anything, so a profile containing one bad rule never yields a
/// half-normalized object.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NormalizeError {
    /// A rule's pointer is not valid RFC 6901 syntax.
    #[error("{tier} normalization rule has an unusable JSON Pointer: {source}")]
    InvalidPointer {
        /// Which profile layer the rule came from.
        tier: RuleTier,
        /// What is wrong with the pointer.
        #[source]
        source: PointerError,
    },
    /// A removal rule's pointer is the empty pointer, which addresses the
    /// whole document.
    ///
    /// Rejected rather than honored. There is no value a document can
    /// normalize to once it has removed itself: the only candidates are
    /// JSON `null` or an empty object, and either one would make *every*
    /// baseline-versus-candidate comparison of that object trivially
    /// equal. That is a silent, total suppression of the product's own
    /// output — precisely the failure a warning is too quiet for, so it
    /// is an error a user has to fix instead.
    ///
    /// Removing a whole top-level section (`/spec`, `/status`, …) is
    /// still allowed, and is what the broad-parent warnings cover.
    #[error(
        "{tier} normalization rule `remove-pointer \"\"` addresses the whole document; \
         removing it would leave nothing to compare and make every comparison of this \
         object pass. Remove a specific pointer instead."
    )]
    RemovesDocumentRoot {
        /// Which profile layer the rule came from.
        tier: RuleTier,
    },
}

/// What normalization did to one object.
///
/// Carries no `Default` and no `#[serde(default)]`-eligible field, for
/// the reason `admissionlab_admission::trace::TraceEvidence` documents
/// at length: this is evidence, and a value that can appear by omission
/// is a value that can be fabricated. An object that reached a report
/// with empty evidence must have genuinely had nothing applied to it,
/// not merely have been deserialized from a document that forgot to say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizationEvidence {
    /// The rules that actually changed this object, in application
    /// order, each rendered as a stable text form
    /// (`"<tier>: <rule-kind> <target>"`, for example
    /// `"built_in: remove-pointer /metadata/uid"`). "Actually changed"
    /// is the operative phrase — see this module's documentation.
    pub applied_rules: Vec<String>,
    /// Human-readable notes about suppressions worth a second look, in
    /// the order they arose. Never a substitute for a
    /// [`NormalizeError`]: a warning describes something that *was*
    /// done.
    pub warnings: Vec<String>,
}

/// One object after normalization, with the record of how it got there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedObject {
    /// The normalized object. A copy: [`normalize_object`] never mutates
    /// its input, so the raw captured object stays available for a
    /// report that wants to show what was actually observed.
    pub value: Value,
    /// What was applied to produce `value`.
    pub evidence: NormalizationEvidence,
}

/// One rule with its pointer already parsed and validated.
enum PlannedRule<'profile> {
    Remove(JsonPointer),
    Sort {
        pointer: JsonPointer,
        key: &'profile str,
    },
    RemoveAnnotation {
        key: &'profile str,
        pointer: JsonPointer,
    },
}

/// Applies `profile` to a copy of `value`.
///
/// Tiers run `built_in` → `recipe` → `user`; within a tier, rules run in
/// order. See this module's documentation for what determinism this
/// guarantees, what `applied_rules` means, and what is and is not warned
/// about.
///
/// # Errors
///
/// [`NormalizeError::InvalidPointer`] if any rule's pointer is not valid
/// RFC 6901, and [`NormalizeError::RemovesDocumentRoot`] if a
/// [`NormalizeRule::RemovePointer`] addresses the whole document. Both
/// are detected across the entire profile before anything is modified,
/// so an error means the input was not touched.
pub fn normalize_object(
    value: &Value,
    profile: &NormalizationProfile,
) -> Result<NormalizedObject, NormalizeError> {
    let tiers = [
        (RuleTier::BuiltIn, &profile.built_in),
        (RuleTier::Recipe, &profile.recipe),
        (RuleTier::User, &profile.user),
    ];

    // Plan every rule first. A profile with one broken rule must not
    // produce a partially normalized object that looks plausible.
    let mut plan: Vec<(RuleTier, PlannedRule<'_>)> = Vec::new();
    for (tier, rules) in tiers {
        for rule in rules {
            plan.push((tier, plan_rule(tier, rule)?));
        }
    }

    let mut normalized = value.clone();
    let mut evidence = NormalizationEvidence {
        applied_rules: Vec::new(),
        warnings: Vec::new(),
    };
    for (tier, planned) in &plan {
        apply_rule(&mut normalized, *tier, planned, &mut evidence);
    }

    Ok(NormalizedObject {
        value: normalized,
        evidence,
    })
}

/// Parses and validates one rule's pointer.
fn plan_rule(tier: RuleTier, rule: &NormalizeRule) -> Result<PlannedRule<'_>, NormalizeError> {
    match rule {
        NormalizeRule::RemovePointer(raw) => {
            let pointer = parse_pointer(tier, raw)?;
            if pointer.is_document_root() {
                return Err(NormalizeError::RemovesDocumentRoot { tier });
            }
            Ok(PlannedRule::Remove(pointer))
        }
        NormalizeRule::SortNamedArray { pointer, key } => Ok(PlannedRule::Sort {
            pointer: parse_pointer(tier, pointer)?,
            key: key.as_str(),
        }),
        // The key is a literal annotation key, not a pointer: it is
        // escaped into one here rather than parsed, so a key containing
        // `/` or `~` needs no hand-escaping in a profile and cannot fail
        // to parse.
        NormalizeRule::RemoveAnnotation(key) => Ok(PlannedRule::RemoveAnnotation {
            key: key.as_str(),
            pointer: JsonPointer::from_tokens(
                ANNOTATIONS_PATH
                    .iter()
                    .copied()
                    .chain(std::iter::once(key.as_str())),
            ),
        }),
    }
}

fn parse_pointer(tier: RuleTier, raw: &str) -> Result<JsonPointer, NormalizeError> {
    JsonPointer::parse(raw).map_err(|source| NormalizeError::InvalidPointer { tier, source })
}

/// Applies one already-planned rule, recording evidence for whatever it
/// actually did.
fn apply_rule(
    value: &mut Value,
    tier: RuleTier,
    planned: &PlannedRule<'_>,
    evidence: &mut NormalizationEvidence,
) {
    match planned {
        PlannedRule::Remove(target) => {
            if pointer::remove(value, target).is_some() {
                evidence
                    .applied_rules
                    .push(format!("{tier}: remove-pointer {}", target.as_str()));
                if let Some(warning) = broad_parent_warning(tier, target) {
                    evidence.warnings.push(warning);
                }
            }
        }
        PlannedRule::RemoveAnnotation {
            key,
            pointer: target,
        } => {
            if pointer::remove(value, target).is_some() {
                evidence
                    .applied_rules
                    .push(format!("{tier}: remove-annotation {key}"));
                prune_empty_annotations(value);
            }
        }
        PlannedRule::Sort {
            pointer: target,
            key,
        } => {
            apply_sort(value, tier, target, key, evidence);
        }
    }
}

/// Sorts the array a [`NormalizeRule::SortNamedArray`] addresses, if it
/// is one.
fn apply_sort(
    value: &mut Value,
    tier: RuleTier,
    target: &JsonPointer,
    key: &str,
    evidence: &mut NormalizationEvidence,
) {
    let Some(found) = pointer::resolve_mut(value, target) else {
        // Pointer matches nothing: an ordinary no-op.
        return;
    };
    let Value::Array(items) = found else {
        // Pointer matches something that is not an array. Unlike a
        // missing pointer this is genuinely surprising -- the rule
        // asserts a shape the object does not have -- so it is worth a
        // note even though nothing was changed.
        evidence.warnings.push(format!(
            "{tier} rule sort-named-array {} by {key} was skipped: the value at that \
             pointer is not an array",
            target.as_str()
        ));
        return;
    };
    if sort_named_array(items, key) {
        evidence.applied_rules.push(format!(
            "{tier}: sort-named-array {} by {key}",
            target.as_str()
        ));
    }
}

/// Stably sorts `items` by each element's string value at `key`,
/// returning whether the order actually changed.
///
/// # How elements without a usable key are handled
///
/// An element is *keyed* if it is a JSON object with a **string** value
/// at `key`. Everything else — an element missing the key, an element
/// that is not an object at all, and an element whose value at `key` is
/// a number, bool, null, array, or object — is *unkeyed*.
///
/// Keyed elements come first, sorted by their key. Unkeyed elements
/// follow, in their original relative order. Nothing is ever dropped,
/// and the array's length is unchanged.
///
/// That layout is chosen over the two alternatives on purpose:
///
/// - **Dropping unkeyed elements** would delete data the object actually
///   contains, which normalization must never do.
/// - **Sorting "around" unkeyed elements** — leaving each one pinned at
///   its original index and sorting the rest into the gaps — is not a
///   canonical form at all: the result depends on where the unkeyed
///   elements happened to sit, so two objects with the same set of
///   containers in different orders would still normalize differently.
///   The whole purpose of this rule is to make those two compare equal.
///
/// Keys are compared by Rust's `str` ordering, which is byte-wise over
/// UTF-8 — total, locale-independent, and identical on every platform.
/// Locale-aware collation would be neither. Ties (two elements with the
/// same key, which Kubernetes forbids for the lists in the built-in
/// profile but nothing here relies on) keep their original relative
/// order, because `slice::sort_by` is a stable sort.
fn sort_named_array(items: &mut Vec<Value>, key: &str) -> bool {
    let original = items.clone();
    let mut keyed: Vec<(&str, usize)> = Vec::new();
    let mut unkeyed: Vec<usize> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item.get(key).and_then(Value::as_str) {
            Some(name) => keyed.push((name, index)),
            None => unkeyed.push(index),
        }
    }
    keyed.sort_by(|left, right| left.0.cmp(right.0));

    let order: Vec<usize> = keyed
        .into_iter()
        .map(|(_, index)| index)
        .chain(unkeyed)
        .collect();
    *items = order
        .into_iter()
        .map(|index| original[index].clone())
        .collect();
    *items != original
}

/// Removes an annotations map that a [`NormalizeRule::RemoveAnnotation`]
/// has just emptied.
///
/// Without this, an object whose only annotation was noise normalizes to
/// `metadata.annotations: {}` while an object that never had annotations
/// at all normalizes to no `annotations` key — a difference introduced
/// *by* normalization, reported against two objects that are in every
/// meaningful sense the same.
///
/// This is tied to [`NormalizeRule::RemoveAnnotation`] specifically, and
/// [`NormalizeRule::RemovePointer`] gets no equivalent treatment even
/// when it removes the same key by pointer. The asymmetry is the point:
/// `RemoveAnnotation` is the annotation-aware vocabulary and owns the
/// map it operates on, while `RemovePointer` is the literal one whose
/// contract is that it removes exactly what it names and nothing else. A
/// caller who wants the map gone can name the map.
///
/// It only ever removes a map this crate emptied: it runs solely after a
/// successful `RemoveAnnotation`, so an `annotations: {}` that was
/// already empty in the input is left exactly as the object had it.
fn prune_empty_annotations(value: &mut Value) {
    let annotations = JsonPointer::from_tokens(ANNOTATIONS_PATH);
    let is_empty = pointer::resolve(value, &annotations)
        .and_then(Value::as_object)
        .is_some_and(serde_json::Map::is_empty);
    if is_empty {
        pointer::remove(value, &annotations);
    }
}

/// A warning for a removal that took out a whole subtree, or `None`.
///
/// # What counts as broad
///
/// Structural, not a list of blessed field names:
///
/// - a **single-token** pointer — `/spec`, `/status`, `/metadata`,
///   `/data`, `/rules`, `/webhooks`, or anything else — removes an
///   entire top-level section of the object;
/// - `/metadata/annotations` or `/metadata/labels` removes every
///   annotation or every label at once, which Task 4.1 Step 4 names
///   explicitly.
///
/// A name list would only ever cover the resource kinds someone thought
/// of; `/spec` on a `CustomResourceDefinition` and `/data` on a
/// `ConfigMap` are the same act, and the token-count test catches both.
///
/// # Which tiers are warned about, and when
///
/// Task 4.1 Step 4 asks for user rules. Recipe rules are included too:
/// a recipe is vendor-supplied data (Global Constraint 6), and a recipe
/// that removes `/spec` blinds a comparison exactly as thoroughly as a
/// user rule that does. The tier is named in the warning text, so a
/// report can still say whose rule it was. `built_in` is excluded
/// because it contains no broad rule by construction — see
/// `crate::rules::built_in_rules` — and a warning that fires on every
/// object for Admission Lab's own defaults would train users to ignore
/// the ones that matter.
///
/// The warning fires only when the removal actually removed something.
/// A broad rule that matched nothing suppressed nothing, and there is
/// nothing for a reader to go looking for.
fn broad_parent_warning(tier: RuleTier, target: &JsonPointer) -> Option<String> {
    if tier == RuleTier::BuiltIn {
        return None;
    }
    let tokens = target.tokens();
    let broad = match tokens {
        [_] => true,
        [first, second] => first == "metadata" && (second == "annotations" || second == "labels"),
        _ => false,
    };
    broad.then(|| {
        format!(
            "{tier} rule removed {}, a broad parent: no difference beneath it can be \
             observed in this comparison",
            target.as_str()
        )
    })
}
