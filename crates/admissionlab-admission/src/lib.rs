#![forbid(unsafe_code)]
//! The observed admission-outcome domain model for Admission Lab (Task
//! 3.3).
//!
//! This crate defines what "the decision an API server reached on a
//! fixture" means as a Rust type -- not how that decision is captured
//! (a later task's job) or compared across baseline/candidate (Phase
//! 4's job). Every type here exists to answer one question honestly:
//! *what did this project actually observe*, keeping "we don't know"
//! representable and never collapsible into a plausible-looking real
//! value (Global Constraint 15). Three fields carry that risk directly
//! and are documented at the point they matter most:
//!
//! - [`trace::WebhookInvocation::latency`] -- an unmeasured duration is
//!   `None`, never a fabricated `0`.
//! - [`trace::WebhookInvocation::mutated`] -- "could not tell whether it
//!   mutated" is `None`, never a fabricated `Some(false)`.
//! - [`trace::TraceEvidence`] -- has no `Default`; a document that omits
//!   it fails to deserialize rather than silently reading as
//!   [`trace::TraceEvidence::Observed`].
//!
//! - [`outcome`] defines [`outcome::AdmissionOutcome`] (the top-level
//!   observation) and [`outcome::AdmissionDecision`] (the API server's
//!   final verdict).
//! - [`trace`] defines [`trace::AdmissionTrace`],
//!   [`trace::TraceEvidence`], [`trace::WebhookInvocation`], and
//!   [`trace::WebhookOutcome`] -- what was observed about the chain of
//!   webhooks a fixture passed through.
//!
//! # Dependency direction (controller supplement §3, Task 3.3)
//!
//! This crate depends on `admissionlab-core` for
//! [`admissionlab_core::FixtureId`], [`admissionlab_core::Side`], and
//! [`admissionlab_core::Diagnostic`] -- the first edge out of this
//! crate, and a safe one on its own. What must never happen is the
//! reverse: a `core -> admission` edge. Task 3.10 integrates this
//! crate's capture pipeline into `admissionlab-core`'s own `run.rs`
//! through a trait declared in `core` and implemented here -- the same
//! shape `admissionlab_core::ClusterManager` and
//! `admissionlab_core::StackInstaller` already use -- not a new
//! dependency edge, which would close a cycle Cargo would reject
//! outright once `admission -> fixtures -> core` also exists (by Task
//! 3.10's time).

pub mod outcome;
pub mod trace;

pub use outcome::{AdmissionDecision, AdmissionOutcome};
pub use trace::{AdmissionTrace, TraceEvidence, WebhookInvocation, WebhookOutcome};
