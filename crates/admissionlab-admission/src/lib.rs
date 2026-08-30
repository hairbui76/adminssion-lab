#![forbid(unsafe_code)]
//! The observed admission-outcome domain model for Admission Lab (Task
//! 3.3), and (Task 3.4) the pipeline that actually captures it from a
//! real API server.
//!
//! This crate defines what "the decision an API server reached on a
//! fixture" means as a Rust type -- not how that decision is compared
//! across baseline/candidate (Phase 4's job). Every type here exists to
//! answer one question honestly: *what did this project actually
//! observe*, keeping "we don't know" representable and never
//! collapsible into a plausible-looking real value (Global Constraint
//! 15). Three fields carry that risk directly and are documented at the
//! point they matter most:
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
//! - [`execute`] implements Task 3.4:
//!   [`execute::AdmissionExecutor::execute_create`] replays a fixture
//!   through a real API server as a server-side dry-run CREATE (via
//!   `admissionlab_fixtures::execute::dry_run_create`) and classifies
//!   what came back into [`outcome::AdmissionDecision`]. See that
//!   module's documentation for Global Constraint 16 (dry-run is the
//!   only Alpha replay mode; a fixture that cannot be safely evaluated
//!   with it fails explicitly, never silently as a persisted CREATE)
//!   and for the live investigation behind why `UnsupportedDryRun` is
//!   not yet asserted by that classification step.
//!
//! # Dependency direction (controller supplement §3, Task 3.3; §2, Task 3.4)
//!
//! This crate depends on `admissionlab-core` for
//! [`admissionlab_core::FixtureId`], [`admissionlab_core::Side`], and
//! [`admissionlab_core::Diagnostic`], and (as of Task 3.4) on
//! `admissionlab-fixtures` for
//! [`admissionlab_fixtures::FixtureSource`],
//! [`admissionlab_fixtures::ResolvedResource`], and
//! [`admissionlab_fixtures::execute::dry_run_create`] itself -- giving
//! `admission -> fixtures -> core` (since `admissionlab-fixtures`
//! already depends on `admissionlab-core`). Both edges are safe on their
//! own. What must never happen is the reverse: a `core -> admission`
//! edge, which would close that chain into a cycle Cargo rejects
//! outright. Task 3.10 integrates this crate's capture pipeline into
//! `admissionlab-core`'s own `run.rs` through a *separate*, coarser
//! trait declared in `core` (using only core-visible types) and
//! implemented here -- the same shape `admissionlab_core::ClusterManager`
//! and `admissionlab_core::StackInstaller` already use -- never by
//! naming [`execute::AdmissionExecutor`] itself from `core`.

pub mod execute;
pub mod outcome;
pub mod trace;

pub use execute::{
    AdmissionExecutor, FixtureExecutionError, KubeAdmissionExecutor, RawAdmissionResponse,
    execute_create_with_client,
};
pub use outcome::{AdmissionDecision, AdmissionOutcome};
pub use trace::{AdmissionTrace, TraceEvidence, WebhookInvocation, WebhookOutcome};
