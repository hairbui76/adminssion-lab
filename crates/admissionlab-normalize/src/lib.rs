//! Normalization: turning what was *observed* into what can be
//! *compared*.
//!
//! Baseline and candidate run on two separate ephemeral clusters (Global
//! Constraint 4), at two different moments, and every object a real API
//! server hands back carries the marks of that: a fresh `uid`, a
//! cluster-local `resourceVersion`, a wall-clock `creationTimestamp`,
//! server-side-apply bookkeeping. None of those differences is a
//! behavior change, and all of them would show up in a naive
//! comparison. This crate removes exactly that class of noise, and
//! nothing else.
//!
//! # Two entry points
//!
//! - [`normalize_object`] (Task 4.1) normalizes one Kubernetes object —
//!   for Alpha, the final admitted/mutated object of a server-side
//!   dry-run `CREATE` — under a [`NormalizationProfile`], returning the
//!   transformed value plus a [`NormalizationEvidence`] record of what
//!   was done to it.
//! - [`normalize_trace`] (Task 4.2) canonicalizes the webhook-invocation
//!   evidence captured alongside that object: JSON Patch *values* get
//!   canonical object-key order, and nothing else about the trace
//!   changes.
//!
//! Neither ever mutates its input.
//!
//! The asymmetry between them is deliberate. Object normalization is
//! rule-driven and configurable because what counts as noise in an
//! object depends on the resource and the stack under test; trace
//! canonicalization is fixed and unconfigurable because a webhook chain
//! has exactly one presentation-only degree of freedom, and every other
//! part of it is behavior.
//!
//! # What this crate must never do
//!
//! Normalization is **mechanical**. Every rule here removes, reorders,
//! or re-serializes; none of them decides that a difference is expected,
//! acceptable, or a regression. That judgment is
//! `admissionlab-diff`/`admissionlab-policy`'s (Global Constraint 6 and
//! 7, PRODUCT.md §14), and keeping it out of here is what lets recipes
//! contribute normalization rules at all without contributing
//! classification logic. [`crate::rules`]'s own module documentation
//! covers how the closed rule vocabulary enforces that by construction.
//!
//! The second rule follows from Global Constraint 15: normalization may
//! never invent a value. It removes fields and reorders arrays; it does
//! not fill in a missing `latency` with a plausible number, collapse an
//! unknown `mutated` to `false`, or blank a patch it believes to be
//! redundant. [`crate::trace`] documents each of those cases, since it
//! is where they would be tempting.

#![forbid(unsafe_code)]

pub mod object;
pub mod pointer;
pub mod rules;
pub mod trace;

pub use object::{NormalizationEvidence, NormalizeError, NormalizedObject, normalize_object};
pub use pointer::{JsonPointer, PointerError};
pub use rules::{NormalizationProfile, NormalizeRule, RuleTier, built_in_rules};
pub use trace::{NormalizedTrace, NormalizedWebhookInvocation, normalize_trace};
