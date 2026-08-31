#![forbid(unsafe_code)]
//! Deterministic grading of admission behavior changes against a lab's
//! explicit policy.
//!
//! `admissionlab-diff` produces *claims* (this container was removed,
//! this request is newly denied). This crate decides *how much each
//! claim matters* and whether the run as a whole passes, warns, or
//! fails. It is the only place in the project allowed to make that
//! decision: recipes carry install/readiness/normalization metadata and
//! never classification logic (Global Constraint 6), and report
//! rendering never decides severity (§1.1). Everything here is pure and
//! deterministic -- no clock, no network, no model, no ambient state
//! (Global Constraint 7).
//!
//! # The pieces
//!
//! - [`severity`] holds [`Severity`] and the frozen seventeen-row Alpha
//!   default table ([`default_severity`]), plus the name lookups
//!   (`kind_from_name`, `Severity::from_name`) that turn hand-written
//!   configuration strings into typed values or an error.
//! - [`selector`] holds [`ChangeSelector`] -- the §1.2 registry's
//!   canonical narrowing vocabulary (fixture glob, subject, object
//!   path) -- and its compiled, validated form.
//! - [`evaluate`] holds [`resolve_policy`] (which rejects every unknown
//!   or impossible name at load time) and [`evaluate`](evaluate())
//!   (which grades a run and produces a [`PolicyResult`]).
//! - [`error`] holds the rejection vocabulary, which follows
//!   `admissionlab_spec::SpecError`'s dotted-locator convention so a
//!   policy complaint and a configuration complaint read alike.
//!
//! # Where validation happens, and why here
//!
//! Task 4.8 asks for unknown semantic-kind names to be rejected "at
//! config load time". They are -- by [`validate_policy_spec`], which the
//! orchestrator calls immediately after `admissionlab_spec::resolve_lab`
//! and before any cluster is created. It could not live in
//! `admissionlab-spec` itself: the valid names belong to
//! `admissionlab-diff`, and §1.1 keeps `spec` beneath every crate that
//! could supply them. See that function's own documentation for the full
//! argument.

pub mod error;
pub mod evaluate;
pub mod selector;
pub mod severity;

pub use error::{PolicySpecErrors, PolicyValidationError};
pub use evaluate::{
    ClassifiedChange, PolicyDisposition, PolicyResult, ResolvedPolicy, StaleExpectation, evaluate,
    resolve_policy, validate_policy_spec,
};
pub use selector::{ChangeSelector, CompiledSelector};
pub use severity::{ALL_KINDS, Severity, default_severity, kind_from_name, kind_index};
