//! The whole per-fixture capture pipeline (Task 3.10): one fixture, one
//! side, one real API server, one evidence bundle on disk.
//!
//! Everything this crate built separately meets here.
//! [`capture_fixture`] takes a checkpoint of the cluster's audit log and
//! (optionally) its `/metrics` page, replays the fixture as exactly one
//! server-side dry-run CREATE ([`crate::execute`]), waits for that
//! request's own `ResponseComplete` audit event
//! ([`crate::audit_reader`] plus [`crate::correlate::select_fixture_event`]),
//! reconstructs the mutating-webhook chain from it
//! ([`crate::correlate::reconstruct_mutating_trace`]), merges whatever
//! per-webhook latency the metric deltas can *soundly* be attributed to
//! this one request ([`crate::metrics`]), and reports the result as an
//! [`AdmissionOutcome`].
//!
//! # Why this is its own module rather than more of `execute.rs`
//!
//! The roadmap's file list names `execute.rs`. That module has one
//! subject and states it plainly: turning the API server's raw answer to
//! one dry-run CREATE into an [`crate::outcome::AdmissionDecision`], and
//! the Global Constraint 16 rules around that classification. This
//! module is a different subject -- orchestration across five
//! collaborators, three of them injected as `dyn` traits, plus the
//! artifact bundle -- and it is where the pipeline's own honesty rules
//! (below) belong, next to the code that applies them rather than
//! appended to a module about classification. `execute.rs` is still
//! modified by this task, for the one thing that genuinely belongs
//! there: [`FixtureExecutionError`] gains the pipeline's failure modes,
//! because Task 3.10's frozen signature returns that type.
//!
//! # The line between "this fixture failed" and "this evidence is absent"
//!
//! This is the module's most consequential decision, so it is stated
//! once, here, in full. Global Constraint 15 says missing data must be
//! represented as unknown and never fabricated. It does *not* say a
//! fixture must fail whenever something is missing -- and choosing wrong
//! in either direction is a real defect: failing too eagerly throws away
//! a correct, comparable admission decision, while degrading too eagerly
//! turns broken infrastructure into a run full of results Phase 4 cannot
//! distinguish from a stack that stopped running webhooks.
//!
//! **Fails the fixture** (a [`FixtureExecutionError`]):
//!
//! - the dry-run CREATE could not be issued at all, or the fixture's
//!   `apiVersion`/`kind` does not resolve on this cluster
//!   ([`FixtureExecutionError::Replay`]) -- there is no observation to
//!   report;
//! - the audit log could not be read, or was rotated out from under the
//!   checkpoint ([`FixtureExecutionError::Audit`]) -- Admission Lab
//!   writes that policy and mounts that file itself (Global Constraint
//!   18), so this is its own infrastructure being broken, identically for
//!   every remaining fixture;
//! - the fixture's *own* event was found but its mutating annotations
//!   are malformed ([`FixtureExecutionError::Trace`]) -- fatal by
//!   [`crate::correlate`]'s own documented design, because silently
//!   dropping an unreadable invocation would make Phase 4 report a
//!   chain-length difference that never happened;
//! - the evidence bundle could not be written
//!   ([`FixtureExecutionError::Artifact`],
//!   [`FixtureExecutionError::ArtifactDirectory`]) -- the bundle *is* the
//!   deliverable.
//!
//! **Does not fail the fixture** (recorded as absence, plus a
//! [`Diagnostic`]):
//!
//! - **no audit event could be correlated**, or more than one matched
//!   equally well ([`crate::correlate::CorrelationError`]). The outcome
//!   is returned with [`TraceEvidence::Unavailable`] and no invocations.
//!   This is exactly the case [`crate::correlate`]'s own documentation
//!   reserves that value for ("a caller that could not obtain an audit
//!   event at all"), and it is not the same fact as an empty *observed*
//!   chain: [`TraceEvidence::Observed`] with zero invocations is the
//!   positive claim that no mutating webhook ran, which is the single
//!   regression this product exists to catch and must never be
//!   manufactured out of a lookup failure. Everything else about the
//!   fixture -- the decision, the response object, the warnings, the
//!   total latency -- was really observed and stays fully comparable, so
//!   discarding it would lose more than it protects. The raw
//!   `audit.json` artifact is written either way, carrying every event
//!   the window did contain plus the correlation failure's own message,
//!   so a human can see precisely what was there.
//! - **the `/metrics` page could not be scraped**, or its deltas cannot
//!   be attributed to this one request
//!   ([`crate::metrics::MetricsUnavailable`], and the exactly-one rule).
//!   Global Constraint 19 makes this signal optional by construction;
//!   [`crate::trace::WebhookInvocation::latency`] stays `None`, never a
//!   fabricated `0`.
//!
//! # The correlation deadline (Global Constraint 13)
//!
//! [`DEFAULT_AUDIT_CORRELATION_TIMEOUT`] bounds step 3 as a whole.
//! [`crate::audit_reader::AuditLogReader::events_since`] deliberately
//! returns a partial `Ok` when its own deadline expires, and it stops
//! waiting at the *first* `ResponseComplete` it sees -- which on a live
//! `kind` cluster is frequently a lease renewal or a kubelet status
//! patch rather than this fixture's own request. So this module re-reads
//! from the same checkpoint (never from an advanced cursor: re-reading
//! the whole window is what keeps one event from being counted twice and
//! turning into a spurious
//! [`crate::correlate::CorrelationError::Ambiguous`]) and re-selects,
//! sleeping [`CORRELATION_RETRY_INTERVAL`] between passes so a
//! never-matching window cannot spin. An
//! [`crate::correlate::CorrelationError::Ambiguous`] is *not* retried:
//! more events can only add candidates, never remove one, so waiting
//! longer cannot resolve a tie -- and this project does not break ties
//! (see [`crate::correlate`]'s "Why no tie is ever broken").
//!
//! # Ordering, and why the metric scrapes bracket exactly one request
//!
//! The steps are: metrics-before, audit checkpoint, **the one request**,
//! metrics-after, then the audit wait. The `after` scrape happens before
//! the (potentially second-long) audit wait so the window the deltas
//! describe is as close as possible to the request alone -- the whole
//! exactly-one-invocation attribution rule
//! ([`crate::metrics::WebhookMetricDelta::attributable_latency`]) rests
//! on background traffic not sharing that window. The checkpoint is
//! taken after the `before` scrape and immediately before the request,
//! for the mirror-image reason: every audit event the scrape itself
//! produced falls before the checkpoint rather than into the correlation
//! window.
//!
//! # The bundle
//!
//! [`write_evidence`] writes, per side and fixture:
//!
//! ```text
//! raw/<side>/<fixture-id>/request.json          the object exactly as submitted
//! raw/<side>/<fixture-id>/response.json         what the API server answered
//! raw/<side>/<fixture-id>/audit.json            the correlation window and its result
//! raw/<side>/<fixture-id>/metrics-before.prom   verbatim page, if metrics were enabled
//! raw/<side>/<fixture-id>/metrics-after.prom    verbatim page, if metrics were enabled
//! raw/<side>/<fixture-id>/outcome.json          the AdmissionOutcome
//! ```
//!
//! Every file goes through [`ArtifactStore`]'s atomic writers, so a
//! reader never sees a half-written artifact. The two `.prom` pages are
//! the *same bytes that were parsed* -- one scrape each, via
//! [`crate::metrics::AdmissionMetricsSource::snapshot_with_text`] --
//! because a page re-fetched for the artifact would describe a different
//! instant than the delta computed beside it.
//!
//! # Serial within a side (Global Constraint 17)
//!
//! [`KubeFixtureCapture`] is this module's implementation of
//! `admissionlab_core::FixtureCapture`, and it replays its fixtures in a
//! plain `for` loop -- one request in flight at a time, per cluster. That
//! is not a performance choice: offset-plus-object correlation is only
//! sound while one request is being served, so parallelising within a
//! side would break correctness rather than merely determinism. Baseline
//! and candidate are separate clusters with separate audit logs, and
//! `admissionlab_core::LabRunner::capture_fixtures` does run those two
//! concurrently.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use admissionlab_core::{
    ArtifactStore, CapturedFixture, ClusterHandle, Diagnostic, FixtureCapture, FixtureCaptureError,
    RedactedValue, RunPaths, Side, SideCapture,
};
use admissionlab_fixtures::execute::namespace_of;
use admissionlab_fixtures::{FixtureSource, ResolvedResource, ResourceResolver};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

use crate::audit_reader::{
    AuditCheckpoint, AuditError, AuditEvent, AuditLogReader, DEFAULT_POLL_INTERVAL,
    FileAuditLogReader,
};
use crate::correlate::{
    CorrelationError, ObjectKey, reconstruct_mutating_trace, select_fixture_event,
};
use crate::execute::{AdmissionExecutor, FixtureExecutionError, KubeAdmissionExecutor};
use crate::metrics::{
    AdmissionMetricSnapshot, AdmissionMetricsSource, WebhookMetricDelta, diff_metrics,
};
use crate::outcome::{AdmissionDecision, AdmissionOutcome};
use crate::trace::{AdmissionTrace, TraceEvidence};

/// How long [`capture_fixture`] waits, in total, for a fixture request's
/// own `ResponseComplete` audit event to appear and be selectable.
///
/// Ten seconds, and both halves of that are deliberate. It is *bounded*
/// because Global Constraint 13 requires every external interaction to
/// be, and because every fixture pays this serially (Global Constraint
/// 17), so an unbounded wait on one wedged event would stall a whole
/// run. It is *generous* because the alternative failure is far worse
/// than a slow fixture: a deadline that expires while the API server
/// simply had not flushed yet produces a
/// [`TraceEvidence::Unavailable`] outcome, which reads as "we could not
/// look" for a fixture that was in fact fine. kube-apiserver's log
/// audit backend writes each event as the request completes, so the
/// event is normally present within milliseconds of the response; ten
/// seconds is roughly three orders of magnitude of headroom for a
/// loaded CI runner.
pub const DEFAULT_AUDIT_CORRELATION_TIMEOUT: Duration = Duration::from_secs(10);

/// How long [`capture_fixture`] sleeps between correlation attempts.
///
/// Reuses [`DEFAULT_POLL_INTERVAL`], the same interval
/// [`FileAuditLogReader`] polls the log with, rather than introducing a
/// second tuning knob for the same underlying wait. Non-zero because
/// `events_since` returns immediately whenever *any* `ResponseComplete`
/// is already in the window -- without a sleep here, a window full of
/// unrelated cluster traffic would spin this loop at full speed against
/// the file the API server is writing.
pub const CORRELATION_RETRY_INTERVAL: Duration = DEFAULT_POLL_INTERVAL;

/// File name of the submitted object inside a fixture's bundle.
pub const REQUEST_ARTIFACT: &str = "request.json";
/// File name of the API server's answer inside a fixture's bundle.
pub const RESPONSE_ARTIFACT: &str = "response.json";
/// File name of the audit correlation window inside a fixture's bundle.
pub const AUDIT_ARTIFACT: &str = "audit.json";
/// File name of the pre-request `/metrics` page inside a fixture's
/// bundle. Absent when metric collection was not enabled.
pub const METRICS_BEFORE_ARTIFACT: &str = "metrics-before.prom";
/// File name of the post-request `/metrics` page inside a fixture's
/// bundle. Absent when metric collection was not enabled.
pub const METRICS_AFTER_ARTIFACT: &str = "metrics-after.prom";
/// File name of the serialized [`AdmissionOutcome`] inside a fixture's
/// bundle -- Phase 4's actual input.
pub const OUTCOME_ARTIFACT: &str = "outcome.json";

/// Everything one [`capture_fixture`] call observed, including the raw
/// material [`AdmissionOutcome`] itself does not carry.
///
/// [`capture_fixture`] returns only the outcome, because that is Task
/// 3.10's frozen signature. This type is what
/// [`capture_fixture_evidence`] returns and [`write_evidence`] consumes:
/// the same capture, plus the submitted object, the API server's raw
/// answer, the audit window, and the two `/metrics` pages -- none of
/// which belongs on `AdmissionOutcome` (a report model, deliberately
/// free of hundreds of kilobytes of Prometheus text) but all of which
/// belongs in the on-disk bundle.
#[derive(Debug, Clone)]
pub struct CapturedEvidence {
    /// What was observed, as this project's report model.
    pub outcome: AdmissionOutcome,
    /// The fixture object exactly as submitted -- byte-for-byte the
    /// document discovery parsed, with nothing added (see
    /// `admissionlab_fixtures::execute`'s "The fixture object is sent
    /// byte-for-byte, never annotated").
    pub request: Value,
    /// What the API server answered.
    pub response: ResponseArtifact,
    /// The audit events the correlation window held, and what selecting
    /// this fixture's own event out of them produced.
    pub audit: AuditArtifact,
    /// The `/metrics` page scraped immediately before the request,
    /// verbatim. `None` when metric collection was not enabled, or the
    /// scrape failed (Global Constraint 19: never a fixture failure).
    pub metrics_before: Option<String>,
    /// The `/metrics` page scraped immediately after the response, with
    /// the same `None` meaning.
    pub metrics_after: Option<String>,
}

/// `response.json`: the API server's own answer to the one dry-run
/// CREATE, at the HTTP level.
///
/// [`ResponseArtifact::object`] is `null` for every rejection, and is
/// never filled in with the fixture's *input* object: the input is
/// already `request.json`, and presenting it as a response would claim
/// the API server returned something it did not (Global Constraint 15).
#[derive(Debug, Clone, Serialize)]
pub struct ResponseArtifact {
    /// The classified decision, in the same wire vocabulary
    /// `outcome.json` uses.
    pub decision: AdmissionDecision,
    /// The object the API server reports it would have persisted, when
    /// the request was admitted.
    pub object: Option<Value>,
    /// `Warning` response headers, verbatim and in the order sent.
    pub warnings: Vec<String>,
    /// Wall-clock duration of the request, in milliseconds.
    #[serde(rename = "elapsedMillis")]
    pub elapsed_millis: u64,
    /// RFC 3339 instant just before the request was sent. `None` only if
    /// the recorded [`SystemTime`] is outside the range a timestamp can
    /// represent at all -- never a substituted "now".
    #[serde(rename = "requestStartedAt")]
    pub request_started_at: Option<String>,
    /// RFC 3339 instant just after the response finished arriving, with
    /// the same `None` meaning.
    #[serde(rename = "requestFinishedAt")]
    pub request_finished_at: Option<String>,
}

/// `audit.json`: the correlation window, and what selecting this
/// fixture's own event out of it produced.
///
/// Written whether or not correlation succeeded. A failed correlation is
/// exactly when a human most needs to see what *was* in the window, and
/// the [`AuditEvent`] subset this crate parses carries no request or
/// response bodies at all (see [`crate::audit_reader::AuditEvent`]'s own
/// documentation), so preserving the window costs nothing under Global
/// Constraint 14.
#[derive(Debug, Clone, Serialize)]
pub struct AuditArtifact {
    /// The byte offset the window started at, taken immediately before
    /// the request was issued.
    pub checkpoint: AuditCheckpoint,
    /// This fixture's own `ResponseComplete` event, when exactly one
    /// event in the window matched it.
    pub selected: Option<AuditEvent>,
    /// Why no single event could be selected, rendered. `None` when
    /// [`AuditArtifact::selected`] is `Some` -- the two are never both
    /// populated and never both absent.
    #[serde(rename = "correlationError")]
    pub correlation_error: Option<String>,
    /// Every complete event read from the checkpoint, in file order.
    pub events: Vec<AuditEvent>,
}

/// Captures one fixture on one side and reports what the API server
/// decided.
///
/// Task 3.10's frozen entry point. It returns only the
/// [`AdmissionOutcome`]; a caller that also needs the raw material for
/// the on-disk bundle calls [`capture_fixture_evidence`] (which this
/// delegates to) and then [`write_evidence`].
///
/// Read this module's documentation before relying on the result -- in
/// particular "The line between 'this fixture failed' and 'this evidence
/// is absent'", which is what decides whether a missing audit event
/// returns `Err` or an `Ok` outcome whose trace is
/// [`TraceEvidence::Unavailable`].
///
/// # Errors
///
/// Returns [`FixtureExecutionError`] when the fixture could not be
/// captured at all -- see that type's own documentation, and this
/// module's, for exactly which absences are errors and which are
/// recorded as unknown.
pub async fn capture_fixture(
    cluster: &ClusterHandle,
    side: Side,
    fixture: &FixtureSource,
    resolver: &dyn ResourceResolver,
    executor: &dyn AdmissionExecutor,
    audit: &dyn AuditLogReader,
    metrics: Option<&dyn AdmissionMetricsSource>,
) -> Result<AdmissionOutcome, FixtureExecutionError> {
    let evidence =
        capture_fixture_evidence(cluster, side, fixture, resolver, executor, audit, metrics)
            .await?;
    Ok(evidence.outcome)
}

/// [`capture_fixture`] plus everything the on-disk bundle needs. See
/// this module's documentation for the pipeline's steps, their ordering,
/// and its honesty rules.
///
/// # Errors
///
/// Identical to [`capture_fixture`]'s.
pub async fn capture_fixture_evidence(
    cluster: &ClusterHandle,
    side: Side,
    fixture: &FixtureSource,
    resolver: &dyn ResourceResolver,
    executor: &dyn AdmissionExecutor,
    audit: &dyn AuditLogReader,
    metrics: Option<&dyn AdmissionMetricsSource>,
) -> Result<CapturedEvidence, FixtureExecutionError> {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    let (api_version, kind) = object_type(&fixture.object);
    let resource = resolver.resolve(cluster, api_version, kind).await?;

    // Step 1: metrics snapshot, then the audit offset -- in that order,
    // so the scrape's own audit events land before the checkpoint.
    let before = scrape(metrics, cluster, "before", &mut diagnostics).await;
    let checkpoint = audit.checkpoint()?;

    // Step 2: exactly one dry-run CREATE.
    let raw = executor.execute_create(cluster, fixture, &resource).await?;

    // Step 4 (first half): the closing scrape, taken before the audit
    // wait so the metric window brackets the request and little else.
    let after = scrape(metrics, cluster, "after", &mut diagnostics).await;

    // Step 3: this request's own ResponseComplete event.
    let correlation = correlate(
        audit,
        &checkpoint,
        fixture,
        &resource,
        raw.response_object.as_ref(),
        raw.request_started_at,
        raw.request_finished_at,
    )
    .await?;
    diagnostics.extend(audit.drain_diagnostics());

    // Step 4 (second half): the trace, and any latency that can be
    // soundly attributed to this one request.
    let mut trace = if let Some(event) = &correlation.selected {
        reconstruct_mutating_trace(event)?
    } else {
        diagnostics.push(correlation_diagnostic(correlation.error.as_deref()));
        AdmissionTrace {
            // Never `Observed` with an empty `invocations`: that is the
            // positive claim that no mutating webhook ran. See this
            // module's documentation.
            evidence: TraceEvidence::Unavailable,
            invocations: Vec::new(),
        }
    };

    let deltas = match (&before, &after) {
        (Some((_, before_snapshot)), Some((_, after_snapshot))) => {
            diagnostics.extend(before_snapshot.diagnostics.iter().cloned());
            diagnostics.extend(after_snapshot.diagnostics.iter().cloned());
            diff_metrics(before_snapshot, after_snapshot)
        }
        _ => Vec::new(),
    };
    merge_metric_latency(&mut trace, &deltas);
    diagnostics.extend(rejection_diagnostics(&deltas));

    let outcome = AdmissionOutcome {
        fixture_id: fixture.id.clone(),
        side,
        decision: raw.decision.clone(),
        warnings: raw.warnings.clone(),
        total_latency: raw.elapsed,
        final_object: raw.response_object.clone(),
        trace,
        diagnostics,
    };

    Ok(CapturedEvidence {
        outcome,
        request: fixture.object.clone(),
        response: ResponseArtifact {
            decision: raw.decision,
            object: raw.response_object,
            warnings: raw.warnings,
            elapsed_millis: u64::try_from(raw.elapsed.as_millis()).unwrap_or(u64::MAX),
            request_started_at: rfc3339(raw.request_started_at),
            request_finished_at: rfc3339(raw.request_finished_at),
        },
        audit: AuditArtifact {
            checkpoint,
            selected: correlation.selected,
            correlation_error: correlation.error,
            events: correlation.events,
        },
        metrics_before: before.and_then(|(text, _)| text),
        metrics_after: after.and_then(|(text, _)| text),
    })
}

/// Writes one fixture's whole evidence bundle under `paths`'
/// [`RunPaths::raw`] root and returns the directory it wrote into.
///
/// The side and fixture id come from `evidence.outcome`, not from
/// separate parameters, so the directory a bundle lands in can never
/// disagree with the `outcome.json` inside it.
///
/// # Errors
///
/// Returns [`FixtureExecutionError::ArtifactDirectory`] if the bundle's
/// directory could not be created, and [`FixtureExecutionError::Artifact`]
/// if any file could not be written -- both fatal for the fixture, since
/// the bundle is the deliverable.
pub async fn write_evidence(
    store: &ArtifactStore,
    paths: &RunPaths,
    evidence: &CapturedEvidence,
) -> Result<PathBuf, FixtureExecutionError> {
    let directory = paths
        .raw()
        .join(evidence.outcome.side.as_str())
        .join(evidence.outcome.fixture_id.as_str());
    // `ArtifactStore` deliberately never creates directories (see its own
    // documentation), so this one call is the only filesystem work here
    // that does not go through its atomic writers.
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|source| FixtureExecutionError::ArtifactDirectory {
            path: directory.clone(),
            source,
        })?;

    store
        .write_json_atomic(&directory.join(REQUEST_ARTIFACT), &evidence.request)
        .await?;
    store
        .write_json_atomic(&directory.join(RESPONSE_ARTIFACT), &evidence.response)
        .await?;
    store
        .write_json_atomic(&directory.join(AUDIT_ARTIFACT), &evidence.audit)
        .await?;
    if let Some(text) = &evidence.metrics_before {
        store
            .write_bytes_atomic(&directory.join(METRICS_BEFORE_ARTIFACT), text.as_bytes())
            .await?;
    }
    if let Some(text) = &evidence.metrics_after {
        store
            .write_bytes_atomic(&directory.join(METRICS_AFTER_ARTIFACT), text.as_bytes())
            .await?;
    }
    store
        .write_json_atomic(&directory.join(OUTCOME_ARTIFACT), &evidence.outcome)
        .await?;

    Ok(directory)
}

/// What selecting one fixture's own audit event out of its correlation
/// window produced.
struct Correlated {
    /// The selected event, when exactly one matched.
    selected: Option<AuditEvent>,
    /// Why none was selected, rendered.
    error: Option<String>,
    /// Every event the window held on the final pass.
    events: Vec<AuditEvent>,
}

/// Waits for, and selects, the audit event this fixture's own request
/// produced. See this module's documentation ("The correlation
/// deadline").
async fn correlate(
    audit: &dyn AuditLogReader,
    checkpoint: &AuditCheckpoint,
    fixture: &FixtureSource,
    resource: &ResolvedResource,
    response_object: Option<&Value>,
    started: SystemTime,
    finished: SystemTime,
) -> Result<Correlated, AuditError> {
    let Some(key) = object_key(fixture, resource, response_object) else {
        // Unreachable for a discovered fixture: `admissionlab-fixtures`
        // rejects a document with no `metadata.name` outright (and
        // `generateName` with it), so there is always a name to match on.
        // Reported rather than asserted, because the alternative -- an
        // empty name in the key -- would silently match nothing and read
        // as an ordinary correlation failure.
        return Ok(Correlated {
            selected: None,
            error: Some(
                "the fixture object carries no string `metadata.name`, so no audit objectRef \
                 could be matched against it"
                    .to_string(),
            ),
            events: Vec::new(),
        });
    };

    let deadline = Instant::now() + DEFAULT_AUDIT_CORRELATION_TIMEOUT;
    loop {
        let events = audit.events_since(checkpoint, deadline).await?;
        // Cloned out immediately so `events` is no longer borrowed and
        // can be moved into the result below.
        let selected = select_fixture_event(&events, &key, started, finished).cloned();
        match selected {
            Ok(event) => {
                return Ok(Correlated {
                    selected: Some(event),
                    error: None,
                    events,
                });
            }
            // A tie is never broken, and never waited out: further
            // events can only add candidates.
            Err(error @ CorrelationError::Ambiguous { .. }) => {
                return Ok(Correlated {
                    selected: None,
                    error: Some(error.to_string()),
                    events,
                });
            }
            Err(error) => {
                if Instant::now() >= deadline {
                    return Ok(Correlated {
                        selected: None,
                        error: Some(error.to_string()),
                        events,
                    });
                }
            }
        }
        tokio::time::sleep(CORRELATION_RETRY_INTERVAL).await;
    }
}

/// The `apiVersion`/`kind` a fixture document declares, for
/// [`ResourceResolver::resolve`].
///
/// Empty strings when a field is absent or not a string. Discovery
/// already rejects such a document ([`admissionlab_fixtures::FixtureError::MissingField`]),
/// so this is a total function over a value that cannot occur rather
/// than a fallback with meaning of its own -- and an empty
/// `apiVersion`/`kind` resolves to nothing, which fails the fixture
/// explicitly.
fn object_type(object: &Value) -> (&str, &str) {
    let field = |name: &str| object.get(name).and_then(Value::as_str).unwrap_or("");
    (field("apiVersion"), field("kind"))
}

/// The audit `objectRef` key this fixture's request targeted.
///
/// The name is taken from the API server's own response object when the
/// request was admitted, falling back to the fixture's own
/// `metadata.name` for a rejection (where there is no response object).
/// Both agree for every fixture Alpha accepts -- `generateName` is
/// rejected at discovery -- but preferring the observed value is what
/// keeps this correct if a later task ever relaxes that.
///
/// The namespace comes from [`namespace_of`], the same function that
/// built the request URL, so the `"default"` fallback cannot drift apart
/// from what the API server actually recorded.
fn object_key(
    fixture: &FixtureSource,
    resource: &ResolvedResource,
    response_object: Option<&Value>,
) -> Option<ObjectKey> {
    let name = response_object
        .and_then(|object| object.pointer("/metadata/name"))
        .or_else(|| fixture.object.pointer("/metadata/name"))
        .and_then(Value::as_str)?;
    Some(ObjectKey {
        group: resource.api_resource.group.clone(),
        version: resource.api_resource.version.clone(),
        resource: resource.api_resource.plural.clone(),
        namespace: resource.namespaced.then(|| namespace_of(fixture)),
        name: name.to_string(),
    })
}

/// Scrapes one `/metrics` page, or records why it could not be scraped.
///
/// Returns `None` both when metric collection is disabled and when the
/// scrape failed -- Global Constraint 19 makes those the same thing for
/// this pipeline (no metric evidence), and neither is ever a fixture
/// failure. The failure case is distinguishable in the result by the
/// [`Diagnostic`] it leaves behind.
async fn scrape(
    metrics: Option<&dyn AdmissionMetricsSource>,
    cluster: &ClusterHandle,
    phase: &'static str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(Option<String>, AdmissionMetricSnapshot)> {
    let source = metrics?;
    match source.snapshot_with_text(cluster).await {
        Ok(scraped) => Some(scraped),
        Err(error) => {
            let mut context = BTreeMap::new();
            context.insert(
                "phase".to_string(),
                RedactedValue::Public(phase.to_string()),
            );
            context.insert(
                "cluster".to_string(),
                RedactedValue::Public(cluster.spec.name.clone()),
            );
            diagnostics.push(Diagnostic {
                code: "admission.metrics_unavailable".to_string(),
                message: format!(
                    "no {phase}-request /metrics page could be scraped, so this fixture has no \
                     per-webhook latency evidence: {error}"
                ),
                context,
            });
            None
        }
    }
}

/// Sets each invocation's latency from the metric deltas, when -- and
/// only when -- that attribution is sound.
///
/// Every invocation's latency is assigned here, including back to `None`:
/// a trace reconstructed from audit annotations always arrives with
/// `latency: None` (those annotations carry no timing at all), so this
/// only ever adds evidence.
fn merge_metric_latency(trace: &mut AdmissionTrace, deltas: &[WebhookMetricDelta]) {
    for invocation in &mut trace.invocations {
        invocation.latency = attributable_latency(&invocation.webhook, deltas);
    }
}

/// The latency one webhook's metric deltas attribute to this single
/// fixture request, or `None` when no such attribution is sound.
///
/// The exactly-one rule
/// ([`crate::metrics::WebhookMetricDelta::attributable_latency`]) applies
/// per *label set*, and one webhook name can carry several -- the
/// `rejected="true"`/`rejected="false"` pair, at least. So this requires
/// exactly one of that webhook's label sets to have recorded exactly one
/// call, and every other one of its label sets to have recorded zero
/// with real evidence behind that zero. A label set whose evidence is
/// [`crate::metrics::DeltaEvidence::Unavailable`] yields `None` for the
/// whole webhook: "some of this webhook's calls in the window are
/// unaccounted for" cannot support attributing the rest to one fixture.
fn attributable_latency(webhook: &str, deltas: &[WebhookMetricDelta]) -> Option<Duration> {
    let mut attributed: Option<&WebhookMetricDelta> = None;
    for delta in deltas.iter().filter(|delta| delta.webhook == webhook) {
        match delta.observed_request_count_delta() {
            Some(0) => {}
            Some(1) if attributed.is_none() => attributed = Some(delta),
            // Two label sets each recording one call, a label set
            // recording several, or a label set with no usable evidence:
            // the `_sum` increase cannot be split (see this function's
            // own documentation).
            _ => return None,
        }
    }
    attributed?.attributable_latency()
}

/// One [`Diagnostic`] per webhook whose rejection counter really did
/// rise across this fixture's request.
///
/// Deliberately a diagnostic and not a [`crate::trace::WebhookInvocation`]:
/// a metric increase proves a rejection was counted, not that a webhook
/// this project can name ran at a particular round and index. Inventing
/// an invocation from it would be exactly the fabricated
/// validating-webhook chain the Phase 3 Exit Gate forbids.
fn rejection_diagnostics(deltas: &[WebhookMetricDelta]) -> Vec<Diagnostic> {
    deltas
        .iter()
        .filter_map(|delta| {
            let count = delta.observed_rejection_delta()?;
            if count == 0 {
                return None;
            }
            let mut context = BTreeMap::new();
            context.insert(
                "webhook".to_string(),
                RedactedValue::Public(delta.webhook.clone()),
            );
            context.insert(
                "operation".to_string(),
                RedactedValue::Public(delta.operation.clone()),
            );
            context.insert(
                "type".to_string(),
                RedactedValue::Public(delta.webhook_type.clone()),
            );
            context.insert(
                "rejection_delta".to_string(),
                RedactedValue::Public(count.to_string()),
            );
            Some(Diagnostic {
                code: "admission.webhook_rejection_metric".to_string(),
                message: format!(
                    "kube-apiserver's rejection counter for webhook {} rose by {count} across \
                     this fixture's request",
                    delta.webhook
                ),
                context,
            })
        })
        .collect()
}

/// The [`Diagnostic`] recorded when no audit event could be correlated.
///
/// Its message states the consequence, not only the cause: the trace is
/// reported as unavailable, which is not the same claim as an observed
/// chain that happened to be empty.
fn correlation_diagnostic(error: Option<&str>) -> Diagnostic {
    let mut context = BTreeMap::new();
    if let Some(error) = error {
        context.insert(
            "correlation_error".to_string(),
            RedactedValue::Public(error.to_string()),
        );
    }
    Diagnostic {
        code: "admission.audit_correlation_failed".to_string(),
        message: format!(
            "no single audit event could be matched to this fixture's own dry-run create, so its \
             webhook trace is reported as unavailable rather than as an observed empty chain: {}",
            error.unwrap_or("no further detail is available")
        ),
        context,
    }
}

/// An RFC 3339 rendering of `time`, or `None` if it is outside the range
/// a timestamp can represent -- never a substituted value.
fn rfc3339(time: SystemTime) -> Option<String> {
    jiff::Timestamp::try_from(time)
        .ok()
        .map(|ts| ts.to_string())
}

/// The production `admissionlab_core::FixtureCapture`: replays a
/// discovered fixture set through one cluster, serially, writing each
/// fixture's evidence bundle as it goes.
///
/// Holds the fixture set because `admissionlab-core` cannot name
/// `admissionlab_fixtures::FixtureSource` (see
/// `admissionlab_core::run`'s own module documentation for the
/// dependency direction this whole trait exists to respect), so
/// discovery happens in the caller and its result is handed to this type
/// once, for both sides.
///
/// # Why it also retains every [`AdmissionOutcome`] it captured
///
/// [`KubeFixtureCapture::captured_outcomes`] hands back, in capture
/// order, the very outcomes this capture observed. That is not a
/// convenience: it is the only way a caller can obtain them.
///
/// - `admissionlab_core::CapturedFixture` — what
///   [`FixtureCapture::capture_side`]'s core-visible return type carries
///   — deliberately holds *paths* and nothing else, because
///   `admissionlab-core` cannot name [`AdmissionOutcome`] without the
///   `core -> admission` edge that would close a dependency cycle (see
///   `admissionlab_core::run`'s own module documentation).
/// - `outcome.json` cannot be read back to recover it either:
///   [`AdmissionOutcome`] implements `Serialize` and deliberately not
///   `Deserialize` (its `diagnostics` are `admissionlab_core::Diagnostic`
///   values, a one-way, emit-only vocabulary whose `[REDACTED]` rendering
///   has no faithful inverse — see that type's own documentation).
///
/// So the comparison stage — Phase 4's normalize/diff/report pipeline,
/// wired in `admissionlab-cli` by Task 4.14 — receives the outcomes from
/// the capture implementation it constructed, rather than by re-reading
/// an artifact that was never designed to round-trip.
pub struct KubeFixtureCapture {
    /// The fixtures to replay, in discovery order.
    fixtures: Vec<FixtureSource>,
    /// Where evidence bundles are written. Must be rooted at the same
    /// directory as the [`RunPaths`] passed to
    /// [`FixtureCapture::capture_side`]; [`ArtifactStore`] itself
    /// rejects any write that resolves outside its root.
    store: ArtifactStore,
    /// Resolves each fixture's `apiVersion`/`kind` against a cluster.
    /// Shared across both sides on purpose: it caches one discovery
    /// snapshot per cluster, keyed by kubeconfig path.
    resolver: Arc<dyn ResourceResolver>,
    /// Issues the dry-run CREATE.
    executor: Arc<dyn AdmissionExecutor>,
    /// The optional `/metrics` source (Global Constraint 19). `None`
    /// disables metric collection entirely: no `.prom` artifacts, and
    /// every observed latency stays `None`.
    metrics: Option<Arc<dyn AdmissionMetricsSource>>,
    /// Every outcome captured so far, in capture order — see this type's
    /// documentation for why they are retained. Behind a [`Mutex`]
    /// because [`FixtureCapture::capture_side`] takes `&self` and is
    /// driven concurrently for both sides by
    /// `admissionlab_core::LabRunner::capture_fixtures`; the lock is only
    /// ever held for one `push`, never across an `await`.
    outcomes: Mutex<Vec<AdmissionOutcome>>,
}

impl std::fmt::Debug for KubeFixtureCapture {
    /// Hand-written because none of the three injected backends is
    /// `Debug` (they are `dyn` trait objects). Reports what is
    /// inspectable and states plainly that metric collection is on or
    /// off, which is the one field a reader of a log line would want.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KubeFixtureCapture")
            .field("fixtures", &self.fixtures.len())
            .field("metrics_enabled", &self.metrics.is_some())
            .finish_non_exhaustive()
    }
}

impl KubeFixtureCapture {
    /// Creates a capture over `fixtures`, writing bundles through
    /// `store`, using this crate's production backends
    /// ([`admissionlab_fixtures::KubeResourceResolver`],
    /// [`KubeAdmissionExecutor`]) and **no** metric collection.
    ///
    /// Metrics are off by default because they are optional by
    /// construction (Global Constraint 19) and cost one full
    /// kube-apiserver `/metrics` render per fixture request, serially --
    /// a caller that wants per-webhook latency asks for it explicitly
    /// with [`KubeFixtureCapture::with_metrics`].
    #[must_use]
    pub fn new(fixtures: Vec<FixtureSource>, store: ArtifactStore) -> Self {
        Self {
            fixtures,
            store,
            resolver: Arc::new(admissionlab_fixtures::KubeResourceResolver::new()),
            executor: Arc::new(KubeAdmissionExecutor::new()),
            metrics: None,
            outcomes: Mutex::new(Vec::new()),
        }
    }

    /// Enables per-webhook latency and rejection evidence, scraped from
    /// `metrics` around every fixture request.
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<dyn AdmissionMetricsSource>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Replaces the resolver and executor -- the seam a test drives
    /// against fakes, mirroring
    /// [`crate::execute::execute_create_with_client`]'s own "given an
    /// already-built client" split.
    #[must_use]
    pub fn with_backends(
        mut self,
        resolver: Arc<dyn ResourceResolver>,
        executor: Arc<dyn AdmissionExecutor>,
    ) -> Self {
        self.resolver = resolver;
        self.executor = executor;
        self
    }

    /// The fixtures this capture replays, in discovery order.
    #[must_use]
    pub fn fixtures(&self) -> &[FixtureSource] {
        &self.fixtures
    }

    /// Every [`AdmissionOutcome`] this capture has produced so far, in
    /// the order it produced them (both sides interleaved — each
    /// outcome names its own [`AdmissionOutcome::side`] and
    /// `fixture_id`, so a caller pairs them by those rather than by
    /// position).
    ///
    /// See this type's documentation for why the outcomes are retained
    /// here at all rather than being recovered from
    /// `admissionlab_core::CapturedFixture` or from `outcome.json`.
    ///
    /// Also populated on a *failed* `capture_side`: every fixture
    /// captured before the failing one is still a real observation, and
    /// a caller writing whatever evidence exists before it gives up
    /// (Task 4.14 step 3) needs exactly those.
    #[must_use]
    pub fn captured_outcomes(&self) -> Vec<AdmissionOutcome> {
        // Never panics on a poisoned lock: a panic in some other
        // fixture's capture must not also destroy the evidence this one
        // already collected, which is precisely what this accessor
        // exists to hand back on a failure path.
        self.outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl FixtureCapture for KubeFixtureCapture {
    async fn capture_side(
        &self,
        cluster: &ClusterHandle,
        side: Side,
        paths: &RunPaths,
    ) -> Result<SideCapture, FixtureCaptureError> {
        let audit = FileAuditLogReader::for_cluster(cluster);
        let mut captured = Vec::with_capacity(self.fixtures.len());

        // A plain sequential loop, and that is the point: Global
        // Constraint 17 makes at most one fixture request per cluster in
        // flight at a time a correctness requirement for audit
        // correlation, not a throughput choice.
        for fixture in &self.fixtures {
            let evidence = capture_fixture_evidence(
                cluster,
                side,
                fixture,
                self.resolver.as_ref(),
                self.executor.as_ref(),
                &audit,
                self.metrics.as_deref(),
            )
            .await
            .map_err(|error| capture_error(fixture, &error))?;

            let artifact_dir = write_evidence(&self.store, paths, &evidence)
                .await
                .map_err(|error| capture_error(fixture, &error))?;

            // Recorded after the bundle is written, so an outcome this
            // accessor hands back is always one whose evidence is also
            // on disk. The guard is dropped before the next `await`.
            self.outcomes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(evidence.outcome.clone());

            captured.push(CapturedFixture {
                fixture_id: fixture.id.clone(),
                side,
                outcome_path: artifact_dir.join(OUTCOME_ARTIFACT),
                artifact_dir,
                diagnostics: evidence.outcome.diagnostics,
            });
        }

        Ok(SideCapture {
            side,
            fixtures: captured,
        })
    }
}

/// Renders one fixture's [`FixtureExecutionError`] into the core-visible
/// [`FixtureCaptureError`], naming the fixture it belongs to.
///
/// The whole error chain is rendered (`{error:#}`-style, by walking
/// `source`), not just its outermost message: the outer variants here
/// are `#[error(transparent)]` wrappers, so the outermost `Display`
/// alone would be the underlying message anyway -- but a future variant
/// with its own message must not swallow its source.
fn capture_error(fixture: &FixtureSource, error: &FixtureExecutionError) -> FixtureCaptureError {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        let rendered = cause.to_string();
        if !message.contains(&rendered) {
            message.push_str(": ");
            message.push_str(&rendered);
        }
        source = cause.source();
    }
    FixtureCaptureError {
        fixture: Some(fixture.id.as_str().to_string()),
        message,
    }
}
