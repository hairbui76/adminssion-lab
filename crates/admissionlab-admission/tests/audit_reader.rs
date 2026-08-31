//! Task 3.5's behavioural suite for
//! [`admissionlab_admission::audit_reader`].
//!
//! Every test here drives the real [`FileAuditLogReader`] against a real
//! file on disk -- there is no fake filesystem and no injected clock,
//! because the behaviours under test *are* filesystem races: a line
//! half-written by another process, a file rotated out from under a
//! checkpoint, a `ResponseComplete` event that arrives before a deadline
//! that would otherwise have been waited out.
//!
//! Two kinds of input are used, deliberately:
//!
//! - `testdata/audit/basic.jsonl`, a checked-in file of realistic
//!   `audit.k8s.io/v1` events, for the parsing and checkpoint-arithmetic
//!   tests. Checked in rather than generated, so the exact JSON shape
//!   this project claims to parse is reviewable in the diff and cannot
//!   drift to match a bug in the parser.
//! - Per-test temporary files assembled *from* those same lines, for the
//!   timing behaviours. Reusing the real lines keeps a wait/rotation
//!   test from accidentally passing on JSON that a real API server would
//!   never write.
//!
//! Sleeps are kept in the tens of milliseconds and every reader is built
//! with [`FileAuditLogReader::with_poll_interval`] at a few milliseconds,
//! so the whole suite runs in well under a second while still exercising
//! more than one poll pass.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use admissionlab_admission::{
    AuditCheckpoint, AuditError, AuditLogReader, AuditObjectRef, AuditResponseStatus,
    FileAuditLogReader,
};
use admissionlab_core::RedactedValue;
use tokio::io::AsyncWriteExt;

// ---------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------

/// A poll interval short enough that a test's own tens-of-milliseconds
/// waits cover several passes, without being a busy loop.
const TEST_POLL_INTERVAL: Duration = Duration::from_millis(3);

/// A deadline far enough out that any test using it fails by hanging
/// visibly rather than by silently returning a partial batch -- used
/// wherever the *expected* behaviour is to return before the deadline.
const GENEROUS_DEADLINE: Duration = Duration::from_secs(10);

/// Path to `testdata/audit/basic.jsonl`, which lives at the workspace
/// root rather than inside this crate, mirroring
/// `admissionlab-fixtures/tests/discover.rs`'s own `testdata_dir`
/// helper.
fn basic_jsonl_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/audit/basic.jsonl")
}

/// The lines of `testdata/audit/basic.jsonl`, each still carrying its
/// own trailing newline so byte offsets computed from them match the
/// file exactly.
fn basic_lines() -> Vec<String> {
    let text =
        std::fs::read_to_string(basic_jsonl_path()).expect("read testdata/audit/basic.jsonl");
    text.split_inclusive('\n').map(str::to_string).collect()
}

/// A fresh, guaranteed-unique directory under the system temp directory,
/// mirroring `admissionlab-installer/tests/manifests_unit.rs`'s own
/// `unique_temp_dir` helper.
fn unique_temp_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-admission-audit-test-{}-{label}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create unique temp dir");
    dir
}

/// Writes `contents` as a temp audit log named after `label` and returns
/// its path.
fn write_audit_log(label: &str, contents: &str) -> PathBuf {
    let path = unique_temp_dir(label).join("audit.log");
    std::fs::write(&path, contents).expect("write temp audit log");
    path
}

/// A reader over `path` polling at [`TEST_POLL_INTERVAL`].
fn reader_at(path: impl Into<PathBuf>) -> FileAuditLogReader {
    FileAuditLogReader::with_poll_interval(path, TEST_POLL_INTERVAL)
}

/// A deadline `after` from now.
fn deadline_in(after: Duration) -> Instant {
    Instant::now() + after
}

/// The RFC 3339 microsecond timestamp `text`, parsed the way an
/// `AuditEvent`'s own timestamp fields are.
fn timestamp(text: &str) -> jiff::Timestamp {
    text.parse().expect("parse RFC 3339 timestamp")
}

// ---------------------------------------------------------------------
// Step 2: the typed subset, and the raw annotation map
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: any parse that drops, renames,
/// or reorders one of the fields Task 3.10 correlates on, or that
/// "helpfully" interprets the annotation map instead of handing it over
/// untouched (which is Task 3.6's job, not this module's).
///
/// Also pins that unknown fields are tolerated: every line in
/// `basic.jsonl` carries `kind`, `apiVersion`, `level`, `user`, and
/// `sourceIPs`, none of which this crate's `AuditEvent` declares. A
/// `deny_unknown_fields` regression would turn all five into
/// diagnostics.
#[tokio::test]
async fn parses_the_typed_subset_and_keeps_the_annotation_map_raw() {
    let reader = reader_at(basic_jsonl_path());
    let events = reader
        .events_since(
            &AuditCheckpoint { byte_offset: 0 },
            deadline_in(GENEROUS_DEADLINE),
        )
        .await
        .expect("read testdata/audit/basic.jsonl");

    assert_eq!(events.len(), 5, "every line in basic.jsonl is an event");
    assert!(
        reader.drain_diagnostics().is_empty(),
        "no line in basic.jsonl is malformed"
    );

    let created = &events[1];
    assert_eq!(created.audit_id, "5b2f1c9e-1f4a-4a4e-9d2f-7c1a0f9b3e21");
    assert_eq!(created.stage, "ResponseComplete");
    assert!(created.is_response_complete());
    assert_eq!(created.verb, "create");
    assert_eq!(
        created.request_uri,
        "/api/v1/namespaces/admissionlab/pods?dryRun=All&fieldManager=admissionlab",
        "the ?dryRun=All query string is how Task 3.10 recognises its own request"
    );
    assert_eq!(
        created.user_agent.as_deref(),
        Some("admissionlab/0.1.0 (linux/amd64) kube/4.2.0")
    );
    assert_eq!(
        created.object_ref,
        Some(AuditObjectRef {
            api_group: None,
            api_version: Some("v1".to_string()),
            resource: Some("pods".to_string()),
            subresource: None,
            namespace: Some("admissionlab".to_string()),
            name: Some("fixture-nginx".to_string()),
        })
    );
    assert_eq!(
        created.response_status,
        Some(AuditResponseStatus {
            code: Some(201),
            message: None,
            reason: None,
            status: None,
        }),
        "responseStatus carries a nested `metadata` object this subset must ignore, \
         not choke on"
    );
    assert_eq!(
        created.request_received_timestamp,
        Some(timestamp("2026-09-01T09:14:22.481930Z"))
    );
    assert_eq!(
        created.stage_timestamp,
        Some(timestamp("2026-09-01T09:14:22.507113Z"))
    );

    // Task 3.7 compares these against `RawAdmissionResponse`'s own
    // `SystemTime` request window; pin that the conversion exists and is
    // infallible, so that task never has to re-parse the string form.
    let stage_time = SystemTime::from(created.stage_timestamp.expect("stageTimestamp"));
    let received_time = SystemTime::from(
        created
            .request_received_timestamp
            .expect("received timestamp"),
    );
    assert!(stage_time > received_time);

    // The annotation map, byte-for-byte as the API server wrote it.
    // Nothing here is parsed, split, or renamed -- Task 3.6 owns that.
    assert_eq!(created.annotations.len(), 4);
    assert_eq!(
        created
            .annotations
            .get("mutation.webhook.admission.k8s.io/round_0_index_0")
            .map(String::as_str),
        Some(
            r#"{"configuration":"admissionlab-test-webhook","webhook":"mutate.test-webhook.admissionlab.dev","mutated":true}"#
        )
    );
    assert_eq!(
        created
            .annotations
            .get("patch.webhook.admission.k8s.io/round_0_index_0")
            .map(String::as_str),
        Some(
            r#"[{"op":"add","path":"/metadata/labels/admissionlab.dev~1mutated","value":"true"}]"#
        )
    );
    assert_eq!(
        created
            .annotations
            .get("authorization.k8s.io/decision")
            .map(String::as_str),
        Some("allow")
    );
    assert!(
        created
            .annotations
            .contains_key("authorization.k8s.io/reason"),
        "unrelated annotations are preserved too, not filtered to webhook keys"
    );
}

/// The mutation this test exists to kill: filling an absent optional
/// field in with a plausible-looking value -- an empty-string
/// `userAgent`, a `code: 0` `responseStatus`, a `namespace: "default"`
/// -- which is exactly the fabrication Global Constraint 15 forbids.
#[tokio::test]
async fn absent_optional_evidence_is_none_never_a_fabricated_value() {
    let reader = reader_at(basic_jsonl_path());
    let events = reader
        .events_since(
            &AuditCheckpoint { byte_offset: 0 },
            deadline_in(GENEROUS_DEADLINE),
        )
        .await
        .expect("read testdata/audit/basic.jsonl");

    // A `RequestReceived` event is written before any response exists.
    let received = &events[0];
    assert_eq!(received.stage, "RequestReceived");
    assert!(!received.is_response_complete());
    assert_eq!(
        received.response_status, None,
        "no responseStatus was logged, so none is reported"
    );
    assert!(
        received.annotations.is_empty(),
        "an event with no annotations key yields an empty map, not a guessed one"
    );

    // An internal component's request with no User-Agent header at all.
    let no_user_agent = &events[3];
    assert_eq!(no_user_agent.verb, "patch");
    assert_eq!(no_user_agent.user_agent, None);
    assert_eq!(no_user_agent.response_status, None);
    let object_ref = no_user_agent
        .object_ref
        .as_ref()
        .expect("objectRef is present on this event");
    assert_eq!(object_ref.subresource.as_deref(), Some("status"));
    assert_eq!(
        object_ref.api_group, None,
        "the core group is encoded as an absent apiGroup; None means core, not unknown"
    );

    // A non-core group, so `apiGroup` is genuinely present.
    let lease = &events[2];
    assert_eq!(lease.verb, "get");
    assert_eq!(
        lease
            .object_ref
            .as_ref()
            .and_then(|r| r.api_group.as_deref()),
        Some("coordination.k8s.io")
    );

    // A rejection: every `metav1.Status` field this subset reads is
    // populated here, so a dropped field would show up as a `None`.
    let denied = &events[4];
    assert_eq!(
        denied.response_status,
        Some(AuditResponseStatus {
            code: Some(403),
            message: Some(
                "admission webhook \"validate.test-webhook.admissionlab.dev\" denied the \
                 request: fixture is denied by policy"
                    .to_string()
            ),
            reason: Some("Forbidden".to_string()),
            status: Some("Failure".to_string()),
        })
    );
}

// ---------------------------------------------------------------------
// Checkpoints
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: a `checkpoint()` that reports
/// anything other than the current end of the file -- `0`, a line count,
/// or the offset of the last newline -- any of which would make
/// `events_since` replay events written *before* the caller's own
/// request.
#[tokio::test]
async fn checkpoint_is_the_current_end_of_file_and_later_events_start_there() {
    let lines = basic_lines();
    let path = write_audit_log("checkpoint-eof", &lines.concat());
    let reader = reader_at(&path);

    let checkpoint = reader
        .checkpoint()
        .expect("checkpoint an existing audit log");
    let length = std::fs::metadata(&path).expect("stat temp audit log").len();
    assert_eq!(checkpoint.byte_offset, length);

    // Nothing has been appended yet, so a short deadline must come back
    // empty rather than replaying the file.
    let before_append = reader
        .events_since(&checkpoint, deadline_in(Duration::from_millis(30)))
        .await
        .expect("an empty window is not an error");
    assert!(before_append.is_empty());

    // Now append one more `ResponseComplete` event and read again from
    // the same checkpoint.
    std::fs::write(&path, format!("{}{}", lines.concat(), lines[4]))
        .expect("append one more event");
    let after_append = reader
        .events_since(&checkpoint, deadline_in(GENEROUS_DEADLINE))
        .await
        .expect("read the appended event");
    assert_eq!(after_append.len(), 1);
    assert_eq!(
        after_append[0].audit_id,
        "a1b2c3d4-5e6f-4071-8293-a4b5c6d7e8f9"
    );
    assert!(reader.drain_diagnostics().is_empty());
}

/// The mutation this test exists to kill: an `events_since` that ignores
/// `byte_offset` and always reads the whole file, which would hand Task
/// 3.10 events belonging to a *previous* fixture and let it correlate
/// the wrong one.
#[tokio::test]
async fn events_since_a_mid_file_checkpoint_returns_only_later_events() {
    let lines = basic_lines();
    let byte_offset = (lines[0].len() + lines[1].len()) as u64;

    let reader = reader_at(basic_jsonl_path());
    let events = reader
        .events_since(
            &AuditCheckpoint { byte_offset },
            deadline_in(GENEROUS_DEADLINE),
        )
        .await
        .expect("read from a mid-file checkpoint");

    let audit_ids: Vec<&str> = events.iter().map(|event| event.audit_id.as_str()).collect();
    assert_eq!(
        audit_ids,
        vec![
            "c0d3f5a1-8b77-4c1e-b0aa-2f6d9e4c7b10",
            "7e41a2b8-3c5d-4e6f-9a01-b2c3d4e5f607",
            "a1b2c3d4-5e6f-4071-8293-a4b5c6d7e8f9",
        ],
        "the two events before the checkpoint must not reappear, and every event after \
         it must -- including the two written after the first ResponseComplete of the batch"
    );
}

/// The mutation this test exists to kill: a `checkpoint()` that
/// swallowed a missing/unreadable audit log and returned `byte_offset:
/// 0`, which would then read the whole of whatever file later appeared
/// at that path.
#[test]
fn checkpointing_a_missing_audit_log_is_an_error_not_offset_zero() {
    let path = unique_temp_dir("missing-log").join("audit.log");
    let reader = reader_at(&path);
    match reader.checkpoint() {
        Err(AuditError::Metadata { path: reported, .. }) => assert_eq!(reported, path),
        Err(other) => panic!("expected a Metadata error, got {other:?}"),
        Ok(checkpoint) => panic!("expected an error, got {checkpoint:?}"),
    }
}

// ---------------------------------------------------------------------
// Step 1: an unfinished trailing line is waited on, never called corrupt
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: parsing the trailing bytes
/// before their newline arrives -- which would turn kube-apiserver's
/// ordinary non-atomic append into either an `AuditError` or a spurious
/// diagnostic, and would lose the event entirely.
#[tokio::test]
async fn an_unfinished_trailing_line_is_waited_for_not_treated_as_corruption() {
    let lines = basic_lines();
    let (head, tail) = lines[1].split_at(200);
    let path = write_audit_log("partial-completed", &format!("{}{head}", lines[0]));

    let completion_path = path.clone();
    let tail = tail.to_string();
    let completer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&completion_path)
            .await
            .expect("reopen the temp audit log for append");
        file.write_all(tail.as_bytes())
            .await
            .expect("finish writing the partial line");
        file.flush().await.expect("flush the completed line");
    });

    let reader = reader_at(&path);
    let started = Instant::now();
    let events = reader
        .events_since(
            &AuditCheckpoint { byte_offset: 0 },
            deadline_in(GENEROUS_DEADLINE),
        )
        .await
        .expect("a half-written line is not an error");
    let elapsed = started.elapsed();
    completer.await.expect("the completing task must not panic");

    assert_eq!(events.len(), 2, "both the complete and the completed line");
    assert!(events[1].is_response_complete());
    assert_eq!(events[1].audit_id, "5b2f1c9e-1f4a-4a4e-9d2f-7c1a0f9b3e21");
    assert!(
        reader.drain_diagnostics().is_empty(),
        "an unfinished line is not a malformed one and must produce no diagnostic"
    );
    assert!(
        elapsed >= Duration::from_millis(30),
        "the reader must actually have waited for the line to complete, took {elapsed:?}"
    );
}

/// The mutation this test exists to kill: turning deadline expiry into
/// an `Err`, which would make "the API server has not flushed the event
/// yet" indistinguishable from "the audit log could not be read at all",
/// and would throw away the events that *were* read.
#[tokio::test]
async fn a_deadline_that_expires_on_a_never_completed_line_returns_the_complete_events() {
    let lines = basic_lines();
    let (head, _) = lines[1].split_at(200);
    let path = write_audit_log("partial-forever", &format!("{}{head}", lines[0]));

    let reader = reader_at(&path);
    let started = Instant::now();
    let events = reader
        .events_since(
            &AuditCheckpoint { byte_offset: 0 },
            deadline_in(Duration::from_millis(60)),
        )
        .await
        .expect("deadline expiry is Ok, not Err");
    let elapsed = started.elapsed();

    assert_eq!(
        events.len(),
        1,
        "only the one line that was actually terminated by a newline"
    );
    assert_eq!(events[0].stage, "RequestReceived");
    assert!(
        reader.drain_diagnostics().is_empty(),
        "the still-unfinished line must not be reported as malformed on the way out"
    );
    assert!(
        elapsed >= Duration::from_millis(50),
        "the reader must have waited out the deadline, took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------
// Step 3: ResponseComplete ends the wait
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: always polling until the
/// deadline. Global Constraint 17 makes every fixture pay this wait
/// serially, so a reader that ignores `ResponseComplete` would add the
/// full deadline to every fixture in a run.
#[tokio::test]
async fn a_response_complete_event_returns_immediately_rather_than_waiting_out_the_deadline() {
    let lines = basic_lines();
    let path = write_audit_log("early-stop", &format!("{}{}", lines[0], lines[1]));

    let reader = reader_at(&path);
    let started = Instant::now();
    let events = reader
        .events_since(
            &AuditCheckpoint { byte_offset: 0 },
            deadline_in(GENEROUS_DEADLINE),
        )
        .await
        .expect("read a complete request window");
    let elapsed = started.elapsed();

    assert_eq!(events.len(), 2);
    assert!(events[1].is_response_complete());
    assert!(
        elapsed < Duration::from_secs(1),
        "returning took {elapsed:?}, which means the 10s deadline was being waited out"
    );
}

// ---------------------------------------------------------------------
// Step 4: unparsable complete lines are diagnostics, not failures
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: failing the whole read (or
/// silently dropping the batch) because one unrelated line in a
/// cluster-wide audit log did not parse. An audit log carries every
/// request the cluster serves; one bad line says nothing about whether
/// this fixture's own event is present.
#[tokio::test]
async fn an_unparsable_complete_line_becomes_a_diagnostic_and_the_other_events_still_return() {
    let lines = basic_lines();
    let contents = format!(
        "{}{}{}{}",
        lines[0],
        "{ this is not JSON at all\n",
        "{\"kind\":\"Event\",\"level\":\"Request\"}\n",
        lines[1],
    );
    let path = write_audit_log("malformed", &contents);

    let reader = reader_at(&path);
    let events = reader
        .events_since(
            &AuditCheckpoint { byte_offset: 0 },
            deadline_in(GENEROUS_DEADLINE),
        )
        .await
        .expect("one malformed line must not fail the read");

    assert_eq!(
        events.len(),
        2,
        "both well-formed events are still returned"
    );
    assert!(events[1].is_response_complete());

    let diagnostics = reader.drain_diagnostics();
    assert_eq!(
        diagnostics.len(),
        2,
        "a syntactically invalid line and a JSON object that is not an audit event both count"
    );
    for diagnostic in &diagnostics {
        assert_eq!(diagnostic.code, "audit.line_unparsable");
        assert_eq!(
            diagnostic.context.get("path"),
            Some(&RedactedValue::Public(path.display().to_string()))
        );
        assert_eq!(
            diagnostic.context.get("line"),
            Some(&RedactedValue::Sensitive),
            "a Request-level audit line can contain Secret request bodies (GC14), so its \
             contents are withheld rather than attached"
        );
        assert!(diagnostic.context.contains_key("byte_offset"));
    }
    // The first bad line starts immediately after line 0.
    assert_eq!(
        diagnostics[0].context.get("byte_offset"),
        Some(&RedactedValue::Public(lines[0].len().to_string()))
    );

    assert!(
        reader.drain_diagnostics().is_empty(),
        "draining takes the diagnostics, so a second drain reports nothing twice"
    );
}

/// The mutation this test exists to kill: reporting a blank line as
/// unparsable, which would fill a run's report with diagnostics for
/// something that carries no claim at all.
#[tokio::test]
async fn blank_lines_between_events_are_skipped_without_a_diagnostic() {
    let lines = basic_lines();
    let path = write_audit_log("blank-lines", &format!("{}\n   \n{}", lines[0], lines[1]));

    let reader = reader_at(&path);
    let events = reader
        .events_since(
            &AuditCheckpoint { byte_offset: 0 },
            deadline_in(GENEROUS_DEADLINE),
        )
        .await
        .expect("blank lines are not an error");

    assert_eq!(events.len(), 2);
    assert!(reader.drain_diagnostics().is_empty());
}

// ---------------------------------------------------------------------
// Step 5: rotation/truncation is explicit, never a silent reread
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: clamping a now-too-large
/// checkpoint back to `0` (or to the new length) and rereading. That
/// would return events written *before* the caller's own request while
/// looking exactly like a normal, successful read -- the single most
/// dangerous silent failure in this module, because Task 3.10 would then
/// correlate another request's evidence to its fixture.
#[tokio::test]
async fn a_rotated_or_truncated_audit_log_is_an_explicit_error() {
    let lines = basic_lines();
    let path = write_audit_log("truncated", &lines.concat());
    let reader = reader_at(&path);
    let checkpoint = reader.checkpoint().expect("checkpoint the full file");

    // Rotation: the path now holds a much shorter, freshly started log.
    std::fs::write(&path, &lines[0]).expect("rotate the temp audit log");

    match reader
        .events_since(&checkpoint, deadline_in(GENEROUS_DEADLINE))
        .await
    {
        Err(AuditError::Truncated {
            path: reported,
            read_offset,
            current_length,
        }) => {
            assert_eq!(reported, path);
            assert_eq!(read_offset, checkpoint.byte_offset);
            assert_eq!(current_length, lines[0].len() as u64);
        }
        Err(other) => panic!("expected a Truncated error, got {other:?}"),
        Ok(events) => panic!(
            "expected a Truncated error, but {} events were returned from the rotated file",
            events.len()
        ),
    }
}
