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
//! # Entry point
//!
//! [`normalize_object`] (Task 4.1) normalizes one Kubernetes object —
//! for Alpha, the final admitted/mutated object of a server-side dry-run
//! `CREATE` — under a [`NormalizationProfile`]. It returns the
//! transformed value together with a [`NormalizationEvidence`] record of
//! what was done to it, and never mutates its input.
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
//! not fill in a value that was not observed, and it does not delete one
//! that was.

#![forbid(unsafe_code)]

pub mod object;
pub mod pointer;
pub mod rules;

pub use object::{NormalizationEvidence, NormalizeError, NormalizedObject, normalize_object};
pub use pointer::{JsonPointer, PointerError};
pub use rules::{NormalizationProfile, NormalizeRule, RuleTier, built_in_rules};
