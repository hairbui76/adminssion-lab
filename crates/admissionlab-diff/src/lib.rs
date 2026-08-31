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
//! [`admissionlab_core::FixtureId`]. Later tasks add
//! `admissionlab-admission` (Task 4.4 compares its `AdmissionOutcome`s)
//! and `admissionlab-normalize` (Tasks 4.5/4.6 compare its normalized
//! objects and traces). Nothing in this crate may ever be depended on by
//! `admissionlab-core`.

pub mod raw;
pub mod types;

pub use raw::{RawChange, RawChangeOp, raw_object_diff};
pub use types::{DivergenceConfidence, DivergenceEvidence, SemanticChange, SemanticChangeKind};
