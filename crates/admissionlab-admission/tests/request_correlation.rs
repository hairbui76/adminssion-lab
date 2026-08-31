//! Task 3.7's behavioural suite for
//! [`admissionlab_admission::correlate::select_fixture_event`].
//!
//! `testdata/audit/background-noise.jsonl` is the whole point of this
//! file. Correlating a fixture request against an audit log that contains
//! only that request would prove nothing: the interesting failures are
//! all of the form "something else in the window looked close enough".
//! So the fixture surrounds one dry-run CREATE with the traffic a real
//! `kind` cluster produces around it -- a leader-election lease renewal,
//! the garbage collector reading the object that was just created, a
//! kubelet node-status patch -- plus five deliberate lookalikes:
//!
//! - the target request's own `RequestReceived` event, identical in every
//!   field this function looks at except the stage;
//! - a persisted (non-dry-run) CREATE of the same object;
//! - a byte-identical dry-run CREATE four seconds before the window, as a
//!   previous run of the same fixture would leave behind;
//! - a `?dryRun=AllOfThem` request, so the literal text `dryRun=All`
//!   appears in a URI that is not a dry run;
//! - a dry-run CREATE against the object's `binding` subresource.
//!
//! Cases that are about *this function's* logic rather than about what a
//! cluster writes -- an ambiguous pair, a missing timestamp, the
//! microsecond boundary of the window -- are built by editing a parsed
//! fixture event, for the same reason `tests/correlate.rs` builds its
//! impossible annotation shapes in code.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use admissionlab_admission::AuditEvent;
use admissionlab_admission::correlate::{
    CorrelationError, NearMissReason, ObjectKey, select_fixture_event,
};

// ---------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------

/// The `auditID` shared by the target request's `RequestReceived` and
/// `ResponseComplete` events.
const TARGET_AUDIT_ID: &str = "8a4e1b60-0f31-4d2a-9c77-5e6f708192a3";

/// The instant just before the fixture request was sent, as
/// `crate::execute::RawAdmissionResponse::request_started_at` would have
/// recorded it: 15 microseconds before the API server logged receiving
/// it.
const WINDOW_STARTED: &str = "2026-09-01T11:47:12.480900Z";

/// The instant just after the response finished arriving.
const WINDOW_FINISHED: &str = "2026-09-01T11:47:12.515002Z";

/// The `requestReceivedTimestamp` the target event carries.
const TARGET_RECEIVED: &str = "2026-09-01T11:47:12.480915Z";

/// Path to `testdata/audit/background-noise.jsonl`, which lives at the
/// workspace root rather than inside this crate, mirroring
/// `tests/audit_reader.rs`'s own `basic_jsonl_path` helper.
fn background_noise_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/audit/background-noise.jsonl")
}

/// Every event in `testdata/audit/background-noise.jsonl`, in file order.
fn noise() -> Vec<AuditEvent> {
    let text = std::fs::read_to_string(background_noise_path())
        .expect("read testdata/audit/background-noise.jsonl");
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("parse a background-noise.jsonl line"))
        .collect()
}

/// The object the fixture request targeted.
fn fixture_key() -> ObjectKey {
    ObjectKey {
        group: String::new(),
        version: "v1".to_string(),
        resource: "pods".to_string(),
        namespace: Some("admissionlab".to_string()),
        name: "fixture-nginx".to_string(),
    }
}

/// The RFC 3339 timestamp `text` as a [`SystemTime`], parsed exactly the
/// way an [`AuditEvent`]'s own timestamp fields are.
fn instant(text: &str) -> SystemTime {
    SystemTime::from(text.parse::<jiff::Timestamp>().expect("parse RFC 3339"))
}

/// The fixture's request window.
fn window() -> (SystemTime, SystemTime) {
    (instant(WINDOW_STARTED), instant(WINDOW_FINISHED))
}

/// Selects against the checked-in noise, with the fixture's own key and
/// window.
fn select(events: &[AuditEvent]) -> Result<&AuditEvent, CorrelationError> {
    let (started, finished) = window();
    select_fixture_event(events, &fixture_key(), started, finished)
}

/// The target `ResponseComplete` event, on its own.
fn target() -> AuditEvent {
    noise()
        .into_iter()
        .find(|event| event.audit_id == TARGET_AUDIT_ID && event.is_response_complete())
        .expect("background-noise.jsonl has the target ResponseComplete event")
}

/// The near misses a [`CorrelationError::NoMatch`] reported, as
/// `(auditID, reason)` pairs.
fn near_misses(error: &CorrelationError) -> Vec<(String, NearMissReason)> {
    let CorrelationError::NoMatch { near_misses, .. } = error else {
        panic!("expected NoMatch, got {error:?}");
    };
    near_misses
        .iter()
        .map(|near_miss| (near_miss.audit_id.clone(), near_miss.reason))
        .collect()
}

// ---------------------------------------------------------------------
// Steps 1 and 2: the target, found among realistic noise
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: dropping any one of the five
/// criteria. Each of the fixture's lookalikes fails exactly one of them,
/// so relaxing any single check turns this unique match into an
/// [`CorrelationError::Ambiguous`] and this test into a failure.
#[test]
fn the_fixture_request_is_found_among_realistic_cluster_noise() {
    let events = noise();
    assert!(
        events.len() >= 13,
        "precondition: the fixture is a crowded window, not a single event"
    );

    let selected = select(&events).expect("the fixture's own event is selectable");

    assert_eq!(selected.audit_id, TARGET_AUDIT_ID);
    assert!(selected.is_response_complete());
    assert_eq!(
        selected.request_received_timestamp.map(SystemTime::from),
        Some(instant(TARGET_RECEIVED))
    );
    assert!(
        selected
            .annotations
            .contains_key("mutation.webhook.admission.k8s.io/round_0_index_0"),
        "the selected event is the one carrying admission annotations, which is what Task 3.6 \
         reconstructs from"
    );
}

/// The mutation this test exists to kill: any relaxation of the criteria,
/// stated as the individual reason each lookalike was rejected for.
///
/// Removing the target from the log is what makes those reasons
/// observable: with the target present the function returns it and says
/// nothing about the rest.
#[test]
fn every_lookalike_is_rejected_for_its_own_specific_reason() {
    let events: Vec<AuditEvent> = noise()
        .into_iter()
        .filter(|event| !(event.audit_id == TARGET_AUDIT_ID && event.is_response_complete()))
        .collect();

    let error = select(&events).expect_err("without the target, nothing may be selected");

    assert_eq!(
        near_misses(&error),
        vec![
            // The target's own RequestReceived half.
            (TARGET_AUDIT_ID.to_string(), NearMissReason::Stage),
            // The garbage collector reading the object.
            (
                "a0c63d82-2153-4f4c-be99-70819203ccc5".to_string(),
                NearMissReason::Verb
            ),
            // A real, persisted CREATE of the same object.
            (
                "b1d74e93-3264-405d-8faa-819203ddd6d7".to_string(),
                NearMissReason::DryRun
            ),
            // The same fixture's dry run from four seconds earlier.
            (
                "c2e85fa4-4375-416e-90bb-9203444e07e8".to_string(),
                NearMissReason::OutsideWindow
            ),
            // `?dryRun=AllOfThem`: the literal marker, not the parameter.
            (
                "d3f960b5-5486-427f-a1cc-a3145f5f18f9".to_string(),
                NearMissReason::DryRun
            ),
            // A dry-run UPDATE of the same object.
            (
                "062c93e8-87b9-45a2-94ff-d6478c8c4b1c".to_string(),
                NearMissReason::Verb
            ),
        ],
    );
}

/// The mutation this test exists to kill: listing every event in the
/// window as a near miss, which would bury the handful that actually
/// referred to the fixture's object under lease renewals and node-status
/// patches -- and, in the subresource case, would suggest a
/// `pods/binding` create was nearly the fixture's request when it is a
/// different request entirely.
#[test]
fn unrelated_objects_are_not_reported_as_near_misses() {
    let events: Vec<AuditEvent> = noise()
        .into_iter()
        .filter(|event| !(event.audit_id == TARGET_AUDIT_ID && event.is_response_complete()))
        .collect();

    let error = select(&events).expect_err("without the target, nothing may be selected");
    let reported: Vec<String> = near_misses(&error)
        .into_iter()
        .map(|(audit_id, _)| audit_id)
        .collect();

    for unrelated in [
        // A lease renewal.
        "9b5f2c71-1042-4e3b-ad88-6f7081920bb4",
        // A dry-run create of a different pod.
        "e40a71c6-6597-4380-b2dd-b4256a6a29fa",
        // The same pod name in another namespace.
        "f51b82d7-76a8-4491-83ee-c5367b7b3a0b",
        // A dry-run create against the object's `binding` subresource.
        "173da4f9-98ca-46b3-a500-e7589d9d5c2d",
        // A kubelet node-status patch.
        "284eb50a-a9db-47c4-b611-f869aeae6d3e",
        // A dry-run create of `apps/v1 deployments` with the same
        // namespace and name.
        "395fc61b-baec-48d5-8722-097abfbf7e4f",
    ] {
        assert!(
            !reported.contains(&unrelated.to_string()),
            "{unrelated} does not refer to the fixture's object and must not be listed"
        );
    }
}

/// The mutation this test exists to kill: comparing the event's
/// `stageTimestamp` instead of its `requestReceivedTimestamp`. On a
/// `ResponseComplete` event the stage timestamp is written after the
/// response was flushed, so it can fall outside a window the request
/// genuinely belongs to.
#[test]
fn the_window_is_compared_against_the_request_received_timestamp() {
    let mut event = target();
    event.stage_timestamp = Some(
        "2031-01-01T00:00:00.000000Z"
            .parse()
            .expect("parse RFC 3339"),
    );

    let selected = select(std::slice::from_ref(&event)).expect("selectable on receipt time alone");

    assert_eq!(selected.audit_id, TARGET_AUDIT_ID);
}

/// The mutation this test exists to kill: widening the window by an
/// invented clock-skew tolerance, or dropping the one microsecond that is
/// not one.
///
/// Audit timestamps are `metav1.MicroTime` and Go's `.000000` layout
/// truncates, so a request received up to a microsecond *after* the
/// client's `started` can be logged as having arrived just before it.
/// Truncation only moves a value earlier, so the upper bound gets no
/// matching slack -- a symmetric tolerance would be a fabricated
/// allowance.
#[test]
fn the_window_tolerates_exactly_one_microsecond_of_timestamp_truncation() {
    let event = target();
    let received = instant(TARGET_RECEIVED);
    let finished = instant(WINDOW_FINISHED);
    let events = std::slice::from_ref(&event);
    let key = fixture_key();

    let at_the_edge = received + Duration::from_micros(1);
    assert!(
        select_fixture_event(events, &key, at_the_edge, finished).is_ok(),
        "a logged timestamp exactly one microsecond of truncation early still matches"
    );

    let past_the_edge = received + Duration::from_nanos(1_001);
    let error = select_fixture_event(events, &key, past_the_edge, finished)
        .expect_err("beyond the truncation window, nothing matches");
    assert_eq!(
        near_misses(&error),
        vec![(TARGET_AUDIT_ID.to_string(), NearMissReason::OutsideWindow)]
    );

    let started = instant(WINDOW_STARTED);
    let error = select_fixture_event(events, &key, started, received - Duration::from_nanos(1))
        .expect_err("the upper bound carries no tolerance at all");
    assert_eq!(
        near_misses(&error),
        vec![(TARGET_AUDIT_ID.to_string(), NearMissReason::OutsideWindow)]
    );
}

/// The mutation this test exists to kill: treating an event with no
/// `requestReceivedTimestamp` as though it were inside the window,
/// which would match any object-shaped event at all.
#[test]
fn an_event_without_a_receipt_timestamp_cannot_be_placed_in_the_window() {
    let mut event = target();
    event.request_received_timestamp = None;

    let error = select(std::slice::from_ref(&event)).expect_err("unplaceable is not a match");

    assert_eq!(
        near_misses(&error),
        vec![(
            TARGET_AUDIT_ID.to_string(),
            NearMissReason::MissingTimestamp
        )]
    );
}

// ---------------------------------------------------------------------
// Steps 3 and 4: failures are reported, ties are never broken
// ---------------------------------------------------------------------

/// The mutation this test exists to kill: resolving a tie -- by nearest
/// timestamp, by first match, by last match, by anything. Two events that
/// satisfy every criterion are equally likely to be the fixture's, and
/// choosing one would attach a real webhook chain to the wrong fixture.
///
/// Built here rather than checked in: two indistinguishable dry-run
/// CREATEs of the same object inside one serial fixture's window (Global
/// Constraint 17) is a broken cluster or a broken caller, not something a
/// realistic audit log should be presented as containing.
#[test]
fn two_equally_valid_candidates_are_ambiguous_and_never_tie_broken() {
    let first = target();
    let mut second = first.clone();
    second.audit_id = "5c1d0e2f-3a4b-4c5d-9e6f-708192a3b4c5".to_string();
    second.request_received_timestamp = Some(
        "2026-09-01T11:47:12.500000Z"
            .parse()
            .expect("parse RFC 3339"),
    );

    let error = select(&[first, second]).expect_err("a tie is a failure, not a choice");

    let CorrelationError::Ambiguous { audit_ids, .. } = &error else {
        panic!("expected Ambiguous, got {error:?}");
    };
    assert_eq!(
        audit_ids,
        &[
            TARGET_AUDIT_ID.to_string(),
            "5c1d0e2f-3a4b-4c5d-9e6f-708192a3b4c5".to_string(),
        ],
        "both candidates are reported, so Task 3.10 can say which events collided"
    );
    let rendered = error.to_string();
    assert!(rendered.contains(TARGET_AUDIT_ID));
    assert!(rendered.contains("5c1d0e2f-3a4b-4c5d-9e6f-708192a3b4c5"));
    assert!(
        rendered.contains("nearest timestamp"),
        "the message says outright that the tie was not broken by proximity"
    );
}

/// The mutation this test exists to kill: reporting an empty near-miss
/// list the same way as a populated one. "Nothing in the window mentioned
/// that object" points at a lost or truncated audit window; "three events
/// mentioned it and each missed differently" points at the criteria. A
/// caller reading only the message must be able to tell those apart.
#[test]
fn a_window_that_never_mentions_the_object_says_so() {
    let events: Vec<AuditEvent> = noise()
        .into_iter()
        .filter(|event| {
            event
                .object_ref
                .as_ref()
                .and_then(|object_ref| object_ref.name.as_deref())
                != Some("fixture-nginx")
        })
        .collect();

    let error = select(&events).expect_err("the object is not in this window at all");

    assert_eq!(near_misses(&error), vec![]);
    let rendered = error.to_string();
    assert!(
        rendered.contains("v1 pods admissionlab/fixture-nginx"),
        "the message names the object that was looked for: {rendered}"
    );
    assert!(rendered.contains("referred to that object at all"));
}

/// The mutation this test exists to kill: matching a name-generated
/// fixture by its `generateName` prefix. The audit event carries the name
/// the API server invented; [`ObjectKey::name`] is the resolved name Task
/// 3.4's executor read back from the dry-run response, and matching is
/// exact against that -- a prefix match would match every sibling the
/// same fixture ever generated.
#[test]
fn a_generate_name_prefix_is_not_a_match_for_the_resolved_name() {
    let events = noise();
    let mut key = fixture_key();
    key.name = "fixture-".to_string();
    let (started, finished) = window();

    let error = select_fixture_event(&events, &key, started, finished)
        .expect_err("a generateName prefix names no object");

    assert_eq!(near_misses(&error), vec![]);
}

/// The mutation this test exists to kill: comparing an audit
/// `objectRef`'s absent `apiGroup` against anything but the core group.
/// Kubernetes encodes the core group by *omitting* the field, so an
/// [`ObjectKey`] with an empty `group` must match an event that carries
/// no `apiGroup` -- and must not match `apps`.
#[test]
fn the_core_group_is_the_absence_of_an_api_group_not_an_unknown_one() {
    let events = noise();
    let (started, finished) = window();

    let mut wrong_group = fixture_key();
    wrong_group.group = "apps".to_string();
    let error = select_fixture_event(&events, &wrong_group, started, finished)
        .expect_err("an explicit group does not match a core-group objectRef");
    assert_eq!(near_misses(&error), vec![]);

    let selected = select(&events).expect("the empty group matches the absent apiGroup");
    assert_eq!(selected.audit_id, TARGET_AUDIT_ID);
}
