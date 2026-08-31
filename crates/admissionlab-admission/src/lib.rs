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
//! - [`audit_reader`] implements Task 3.5:
//!   [`audit_reader::AuditLogReader`] reads the window of a real
//!   kube-apiserver audit log that belongs to one fixture request,
//!   between an offset [`audit_reader::AuditCheckpoint`] taken before
//!   the request and the request's own `ResponseComplete` event. Its
//!   module documentation covers the four things it refuses to guess --
//!   an unfinished trailing line is waited on rather than called
//!   corruption, an unparsable complete line becomes an
//!   [`admissionlab_core::Diagnostic`] rather than a fixture failure, a
//!   rotated/truncated file is an explicit
//!   [`audit_reader::AuditError::Truncated`] rather than a silent reread
//!   from `0`, and an expired deadline is a partial `Ok` rather than an
//!   error (Global Constraint 15 in all four cases). The raw
//!   `annotations` map it preserves untouched is Task 3.6's input.
//! - [`correlate`] implements Task 3.6:
//!   [`correlate::reconstruct_mutating_trace`] turns the mutating-webhook
//!   annotations on one fixture's `ResponseComplete` audit event back
//!   into a [`trace::AdmissionTrace`]. Its module documentation records
//!   the upstream kube-apiserver source every claim about those
//!   annotations was read from, and -- the reason it is worth reading
//!   before using the result -- exactly how little each annotation
//!   proves: `mutated: false` is emitted from a `defer` on every exit
//!   path including timeouts and denials, so it yields
//!   [`trace::WebhookOutcome::Unknown`], while `mutated: true`, a patch
//!   annotation, and a `failed-open.` annotation each prove something
//!   specific. The same module implements Task 3.7:
//!   [`correlate::select_fixture_event`] picks that one event out of a
//!   window full of controller traffic, matching stage, verb,
//!   `objectRef`, a parsed `dryRun=All` query parameter and the caller's
//!   own request window -- and returning a
//!   [`correlate::CorrelationError`] carrying candidate `auditID`s
//!   rather than breaking a tie by nearest timestamp.
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
//! - [`metrics`] implements Task 3.8: the *optional* per-webhook latency
//!   and rejection signal, read from kube-apiserver `/metrics` deltas
//!   around one serial fixture request. Global Constraint 19 makes it
//!   optional by construction, so every absence there is representable:
//!   [`metrics::MetricsUnavailable`] is a recoverable "no metric
//!   evidence", never a run failure, and
//!   [`metrics::WebhookMetricDelta::attributable_latency`] yields `Some`
//!   only under the exactly-one-invocation rule that makes attributing a
//!   duration to a single fixture sound at all.
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

pub mod audit_reader;
pub mod correlate;
pub mod execute;
pub mod metrics;
pub mod outcome;
pub mod trace;

pub use audit_reader::{
    AuditCheckpoint, AuditError, AuditEvent, AuditLogReader, AuditObjectRef, AuditResponseStatus,
    DEFAULT_POLL_INTERVAL, FileAuditLogReader, STAGE_RESPONSE_COMPLETE,
};
pub use correlate::{
    AUDIT_TIMESTAMP_RESOLUTION, CorrelationError, DRY_RUN_ALL, DRY_RUN_QUERY_PARAM,
    FAILED_OPEN_MUTATION_ANNOTATION_PREFIX, MUTATION_ANNOTATION_PREFIX, NearMiss, NearMissReason,
    ObjectKey, PATCH_ANNOTATION_PREFIX, TraceError, VALIDATION_ANNOTATION_PREFIX, VERB_CREATE,
    reconstruct_mutating_trace, select_fixture_event,
};
pub use execute::{
    AdmissionExecutor, FixtureExecutionError, KubeAdmissionExecutor, RawAdmissionResponse,
    execute_create_with_client,
};
pub use metrics::{
    AdmissionMetricSnapshot, AdmissionMetricsSource, DeltaEvidence, DurationSample,
    KubeMetricsSource, MetricsUnavailable, RejectionEvidence, RejectionKey, WebhookMetricDelta,
    WebhookMetricKey, diff_metrics, parse_snapshot, scrape_metrics_text,
    scrape_metrics_text_with_client,
};
pub use outcome::{AdmissionDecision, AdmissionOutcome};
pub use trace::{AdmissionTrace, TraceEvidence, WebhookInvocation, WebhookOutcome};
