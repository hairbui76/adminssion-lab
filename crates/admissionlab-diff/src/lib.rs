#![forbid(unsafe_code)]
//! Deterministic comparison of two observed admission stacks.
//!
//! This crate turns *observations* (what the baseline API server did,
//! what the candidate API server did) into *claims* (what changed, in
//! product terms). It decides nothing about severity: grading a claim
//! against a lab's policy and expectations is `admissionlab-policy`'s
//! job, and rendering it is `admissionlab-report`'s. Everything here is
//! deterministic and pure -- no clock, no network, no model (Global
//! Constraint 7).
//!
//! # What is compared
//!
//! - [`admission`] compares two sides' *decisions*
//!   ([`admission::diff_admission_decision`]): did the admit/deny verdict
//!   flip? Its module documentation covers the two ways a result can be
//!   empty -- the verdict held, or the two sides were never comparable --
//!   and why [`admission::DecisionComparability`] exists so a caller can
//!   tell those apart.
//!
//! # Two vocabularies, deliberately separate
//!
//! - [`types`] holds the **semantic** vocabulary: [`SemanticChange`] and
//!   the seventeen [`SemanticChangeKind`] names that are a public,
//!   Alpha-stable contract (they appear in JSON reports and in
//!   user-authored `expectations.yaml` files). It also declares
//!   [`DivergenceEvidence`]/[`DivergenceConfidence`], the shape Task
//!   4.7's first-divergence attribution produces and
//!   [`SemanticChange::origin`] carries.
//! - [`raw`] holds the **diagnostic** vocabulary: [`RawChange`], an
//!   RFC 6902 patch operation over two JSON documents. It is evidence a
//!   human reads, never grounds for emitting a semantic change on its
//!   own. That module's own documentation explains why the separation
//!   has to be maintained by discipline rather than by types.
//!
//! # Dependency direction
//!
//! This crate depends on `admissionlab-core` for
//! [`admissionlab_core::FixtureId`] and (as of Task 4.4) on
//! `admissionlab-admission` for the
//! [`admissionlab_admission::AdmissionOutcome`] pair
//! [`admission::diff_admission_decision`]'s frozen signature takes.
//! Tasks 4.5/4.6 add `admissionlab-normalize`. All of these edges point
//! away from this crate; nothing in it may ever be depended on by
//! `admissionlab-core` or `admissionlab-admission`, which would close a
//! cycle Cargo rejects outright.

pub mod admission;
pub mod raw;
pub mod types;

pub use admission::{
    DecisionComparability, decision_comparability, diff_admission_decision, raw_decision_diff,
};
pub use raw::{RawChange, RawChangeOp, raw_object_diff};
pub use types::{DivergenceConfidence, DivergenceEvidence, SemanticChange, SemanticChangeKind};
