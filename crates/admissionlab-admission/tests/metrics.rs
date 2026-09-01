//! Task 3.8's own suite: reading kube-apiserver `/metrics` pages,
//! diffing two of them, and the exactly-one rule that decides whether a
//! delta may be attributed to a single fixture request.
//!
//! Everything here runs offline. The two checked-in pages
//! (`testdata/metrics/{before,after}.prom`, at the workspace root) stand
//! in for a real API server's output; each of their five webhook label
//! sets exercises a different delta case, and those files' own headers
//! say which. What is *not* covered without a live cluster --
//! `admissionlab_admission::metrics::scrape_metrics_text`'s kubeconfig
//! handling, and whether a real kube-apiserver serves these families in
//! this shape -- is scoped in `src/metrics.rs`'s own closing note, the
//! same way `admissionlab-fixtures`'s `resources.rs` scopes its
//! equivalent gap.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::pin::pin;
use std::time::Duration;

use admissionlab_admission::metrics::{
    AdmissionMetricSnapshot, DeltaEvidence, DurationSample, MetricsUnavailable, RejectionEvidence,
    RejectionKey, WebhookMetricDelta, WebhookMetricKey, diff_metrics, parse_snapshot,
    scrape_metrics_text, scrape_metrics_text_with_client,
};
use admissionlab_core::{ClusterHandle, ClusterSpec, RunId, Side};
use http::{Request, Response};
use kube::client::Body;
use tower_test::mock;

/// The workspace's checked-in `testdata/metrics/`, two levels above this
/// crate. Mirrors `admissionlab-spec`'s own `testdata_config` helper.
fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/metrics")
        .join(name)
}

fn load(name: &str) -> AdmissionMetricSnapshot {
    let path = testdata(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    parse_snapshot(&text)
}

/// A duration key with no labels beyond the four well-known ones -- the
/// shape every label set in the checked-in pages has.
fn key(name: &str, operation: &str, rejected: &str, webhook_type: &str) -> WebhookMetricKey {
    WebhookMetricKey {
        name: name.to_string(),
        operation: operation.to_string(),
        rejected: rejected.to_string(),
        webhook_type: webhook_type.to_string(),
        other_labels: BTreeMap::new(),
    }
}

/// The one delta for `webhook`, or a panic naming what was actually
/// emitted. Deliberately asserts uniqueness: a diff that emitted the same
/// webhook twice would otherwise pass every assertion below on whichever
/// copy came first.
fn delta_for<'a>(deltas: &'a [WebhookMetricDelta], webhook: &str) -> &'a WebhookMetricDelta {
    let matching: Vec<&WebhookMetricDelta> =
        deltas.iter().filter(|d| d.webhook == webhook).collect();
    assert_eq!(
        matching.len(),
        1,
        "expected exactly one delta for {webhook}, got {matching:#?} out of {:?}",
        deltas.iter().map(|d| &d.webhook).collect::<Vec<_>>()
    );
    matching[0]
}

/// `assert_eq!` for a float, with a tolerance far tighter than any of the
/// differences these tests distinguish but loose enough to survive the
/// last-bit error of subtracting two decimal literals in binary floating
/// point (`0.0568 - 0.0411` is not bit-identical to `0.0157`).
#[track_caller]
fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

// =========================================================================
// Step 1/2: parsing
// =========================================================================

#[test]
fn parsing_reads_the_exact_sum_and_count_of_every_webhook_label_set() {
    // Fails if any label set were mis-keyed, if the neighbouring
    // `apiserver_admission_step_admission_duration_seconds` family were
    // picked up (it has no `name` label, so it would land as a skipped
    // line rather than a wrong number -- caught by the diagnostics
    // assertion below), or if `_bucket` counts were mistaken for `_count`
    // (the `mutate` label set's `le="0.005"` bucket is 5, its total 7,
    // so a bucket-reading parser reports a visibly different number).
    let before = load("before.prom");

    let durations = before
        .durations
        .as_ref()
        .expect("the duration family is present on the page");
    assert_eq!(
        durations.len(),
        4,
        "before.prom carries exactly four webhook label sets: {:#?}",
        durations.keys().collect::<Vec<_>>()
    );

    let mutate = durations
        .get(&key(
            "mutate.test.admissionlab.io",
            "CREATE",
            "false",
            "admit",
        ))
        .expect("the mutate label set is on the page");
    assert_eq!(mutate.count, Some(7));
    assert_close(mutate.sum.expect("a sum was parsed"), 0.0411);

    let deny = durations
        .get(&key(
            "deny.test.admissionlab.io",
            "CREATE",
            "true",
            "validate",
        ))
        .expect("the rejected label set is on the page");
    assert_eq!(deny.count, Some(5));
    assert_close(deny.sum.expect("a sum was parsed"), 0.5123);

    assert!(
        before.diagnostics.is_empty(),
        "a realistic page must parse cleanly: {:#?}",
        before.diagnostics
    );
}

#[test]
fn parsing_reads_a_sum_written_in_scientific_notation() {
    // `after.prom` writes the `inject.istio.io` sum as `4.2657e-03`.
    // Fails if the value parser only accepted plain decimals -- which
    // would show up as a skipped line and a silently missing webhook,
    // not as an error.
    let after = load("after.prom");
    let sample = after
        .durations
        .as_ref()
        .expect("the duration family is present")
        .get(&key("inject.istio.io", "CREATE", "false", "admit"))
        .expect("the istio label set is on the page");
    assert_eq!(sample.count, Some(1));
    assert_close(sample.sum.expect("a sum was parsed"), 0.004_265_7);
}

#[test]
fn parsing_reads_every_rejection_series_including_its_error_type_split() {
    // Fails if the two `error_type` children were collapsed at parse
    // time: the snapshot must keep them apart (aggregation is
    // `diff_metrics`'s job, and it aggregates *increases*, not totals).
    let before = load("before.prom");
    let rejections = before
        .rejections
        .as_ref()
        .expect("the rejection family is present on the page");
    assert_eq!(rejections.len(), 2, "{rejections:#?}");

    let no_error = RejectionKey {
        name: "deny.test.admissionlab.io".to_string(),
        operation: "CREATE".to_string(),
        webhook_type: "validate".to_string(),
        other_labels: BTreeMap::from([
            ("error_type".to_string(), "no_error".to_string()),
            ("rejection_code".to_string(), "400".to_string()),
        ]),
    };
    assert_eq!(rejections.get(&no_error), Some(&5));
}

#[test]
fn parsing_ignores_unrelated_families_without_complaint() {
    // The `etcd_request_duration_seconds` line in both pages carries a
    // label value containing `"`, `,` and `}`; `go_goroutines` carries no
    // label block at all. Neither may produce a diagnostic, and neither
    // may end up in either family's map.
    let before = load("before.prom");
    assert!(before.diagnostics.is_empty(), "{:#?}", before.diagnostics);
    assert!(
        before
            .durations
            .as_ref()
            .expect("the duration family is present")
            .keys()
            .all(|k| k.name.contains('.')),
        "only webhook label sets may be stored"
    );
}

#[test]
fn an_absent_family_is_none_not_an_empty_map() {
    // The distinction Global Constraint 15 turns on: a page that never
    // mentioned the family carries *no evidence*, which must not read as
    // "the family was there and had nothing in it".
    let snapshot = parse_snapshot("# HELP go_goroutines Number of goroutines.\ngo_goroutines 3\n");
    assert!(snapshot.durations.is_none());
    assert!(snapshot.rejections.is_none());
}

#[test]
fn a_family_declared_but_carrying_no_series_is_present_and_empty() {
    // Fails if family presence were inferred from sample lines alone: a
    // `# TYPE` line with no children is a real page state, and it means
    // "exported, nothing recorded yet" -- an observation, not an absence.
    let snapshot = parse_snapshot(
        "# TYPE apiserver_admission_webhook_rejection_count counter\n\
         # TYPE apiserver_admission_webhook_admission_duration_seconds histogram\n",
    );
    assert_eq!(snapshot.durations.map(|d| d.len()), Some(0));
    assert_eq!(snapshot.rejections.map(|r| r.len()), Some(0));
}

#[test]
fn label_values_are_unescaped_per_the_exposition_format() {
    // Escapes never appear in a real webhook name, but the exposition
    // format permits them anywhere, so the parser must not mangle one:
    // `\"` and `\\` must survive as a single character each, and the
    // resulting key must be the *unescaped* text (otherwise two pages
    // written with different escaping would key to different label sets).
    let text = concat!(
        "# TYPE apiserver_admission_webhook_admission_duration_seconds histogram\n",
        r#"apiserver_admission_webhook_admission_duration_seconds_sum{name="odd\\one\"two",operation="CREATE",rejected="false",type="admit"} 0.5"#,
        "\n",
        r#"apiserver_admission_webhook_admission_duration_seconds_count{name="odd\\one\"two",operation="CREATE",rejected="false",type="admit"} 2"#,
        "\n",
    );
    let snapshot = parse_snapshot(text);
    assert!(
        snapshot.diagnostics.is_empty(),
        "{:#?}",
        snapshot.diagnostics
    );
    let sample = snapshot
        .durations
        .as_ref()
        .expect("the duration family is present")
        .get(&key("odd\\one\"two", "CREATE", "false", "admit"))
        .expect("the escaped name unescapes to exactly this key");
    assert_eq!(sample.count, Some(2));
}

#[test]
fn a_malformed_line_is_skipped_with_a_diagnostic_never_an_error() {
    // Three ways one line can be unusable, one intact line to prove the
    // parser keeps going. Global Constraint 19 is why none of these is an
    // `Err`; Global Constraint 15 is why each still leaves a trace, so an
    // empty result is never mistaken for an observed zero.
    let text = concat!(
        "# TYPE apiserver_admission_webhook_admission_duration_seconds histogram\n",
        // Unterminated label block.
        "apiserver_admission_webhook_admission_duration_seconds_sum{name=\"broken\n",
        // Missing the `rejected` label, so it cannot be keyed.
        "apiserver_admission_webhook_admission_duration_seconds_count{name=\"a\",operation=\"CREATE\",type=\"admit\"} 1\n",
        // A value that is not a number.
        "apiserver_admission_webhook_admission_duration_seconds_sum{name=\"a\",operation=\"CREATE\",rejected=\"false\",type=\"admit\"} not-a-number\n",
        // Intact.
        "apiserver_admission_webhook_admission_duration_seconds_count{name=\"a\",operation=\"CREATE\",rejected=\"false\",type=\"admit\"} 4\n",
    );
    let snapshot = parse_snapshot(text);

    assert_eq!(
        snapshot.diagnostics.len(),
        3,
        "one diagnostic per unusable line: {:#?}",
        snapshot.diagnostics
    );
    assert!(
        snapshot
            .diagnostics
            .iter()
            .all(|d| d.code == "metrics.malformed_line"),
        "{:#?}",
        snapshot.diagnostics
    );

    let durations = snapshot
        .durations
        .as_ref()
        .expect("the family was declared");
    let sample = durations
        .get(&key("a", "CREATE", "false", "admit"))
        .expect("the intact line was still read");
    // The intact `_count` survives; the `_sum` that failed to parse stays
    // `None` rather than defaulting to `0.0`, which is what makes this
    // label set's delta `Unavailable` downstream instead of a fabricated
    // zero-second increase.
    assert_eq!(sample.count, Some(4));
    assert_eq!(sample.sum, None);
}

#[test]
fn a_repeated_series_keeps_the_first_value_and_reports_the_repeat() {
    let text = concat!(
        "# TYPE apiserver_admission_webhook_admission_duration_seconds histogram\n",
        "apiserver_admission_webhook_admission_duration_seconds_count{name=\"a\",operation=\"CREATE\",rejected=\"false\",type=\"admit\"} 4\n",
        "apiserver_admission_webhook_admission_duration_seconds_count{name=\"a\",operation=\"CREATE\",rejected=\"false\",type=\"admit\"} 9\n",
    );
    let snapshot = parse_snapshot(text);
    assert_eq!(
        snapshot
            .durations
            .as_ref()
            .and_then(|d| d.get(&key("a", "CREATE", "false", "admit")))
            .and_then(|s| s.count),
        Some(4)
    );
    assert_eq!(
        snapshot
            .diagnostics
            .iter()
            .filter(|d| d.code == "metrics.duplicate_sample")
            .count(),
        1,
        "{:#?}",
        snapshot.diagnostics
    );
}

// =========================================================================
// Step 3: diffing, and the exactly-one attribution rule
// =========================================================================

#[test]
fn diffing_the_two_pages_covers_every_delta_case() {
    let deltas = diff_metrics(&load("before.prom"), &load("after.prom"));

    // Exactly one invocation in the window: attributable.
    let mutate = delta_for(&deltas, "mutate.test.admissionlab.io");
    assert_eq!(mutate.request_count_delta, 1);
    assert_close(mutate.duration_sum_delta, 0.0157);
    assert_eq!(mutate.duration_evidence, DeltaEvidence::Observed);
    assert_close(
        mutate
            .attributable_latency()
            .expect("a count delta of exactly one is attributable")
            .as_secs_f64(),
        0.0157,
    );

    // Untouched between the two pages: a real, observed zero.
    let quiet = delta_for(&deltas, "validate.test.admissionlab.io");
    assert_eq!(quiet.request_count_delta, 0);
    assert_close(quiet.duration_sum_delta, 0.0);
    assert_eq!(quiet.duration_evidence, DeltaEvidence::Observed);
    assert_eq!(quiet.attributable_latency(), None);

    // Three invocations shared the window: aggregate evidence survives,
    // per-fixture attribution does not.
    let busy = delta_for(&deltas, "policy.kyverno.svc");
    assert_eq!(busy.request_count_delta, 3);
    assert_close(busy.duration_sum_delta, 0.0312);
    assert_eq!(busy.duration_evidence, DeltaEvidence::Observed);
    assert_eq!(
        busy.attributable_latency(),
        None,
        "a count delta above one must never be divided into a per-fixture latency"
    );

    // A webhook whose first invocation ever happened inside the window:
    // absent from `before.prom` entirely, and still attributable, because
    // an absent child of a family that *was* exported is a genuine zero.
    let newcomer = delta_for(&deltas, "inject.istio.io");
    assert_eq!(newcomer.request_count_delta, 1);
    assert_eq!(newcomer.duration_evidence, DeltaEvidence::Observed);
    assert_close(
        newcomer
            .attributable_latency()
            .expect("a first-ever invocation is still exactly one")
            .as_secs_f64(),
        0.004_265_7,
    );
}

#[test]
fn a_rejection_increment_is_attributed_once_to_the_rejected_true_label_set() {
    // The `no_error` child moves 5 -> 6 while the `calling_webhook_error`
    // child stays at 2. Fails if the two children's *totals* were summed
    // instead of their increases (which would report 8), if the increase
    // were attributed to a `rejected="false"` row, or if it were
    // duplicated across both `rejected` values.
    let deltas = diff_metrics(&load("before.prom"), &load("after.prom"));

    let deny = delta_for(&deltas, "deny.test.admissionlab.io");
    assert_eq!(deny.rejected.as_deref(), Some("true"));
    assert_eq!(deny.rejection_delta, 1);
    assert_eq!(deny.rejection_evidence, RejectionEvidence::Observed);
    assert_eq!(deny.observed_rejection_delta(), Some(1));

    // Every other label set is `rejected="false"`, where the rejection
    // counter structurally never attributes anything.
    for delta in &deltas {
        if delta.webhook == "deny.test.admissionlab.io" {
            continue;
        }
        assert_eq!(
            delta.rejection_evidence,
            RejectionEvidence::NotCounted,
            "{delta:#?}"
        );
        assert_eq!(delta.rejection_delta, 0);
    }
}

#[test]
fn deltas_are_emitted_in_a_deterministic_order() {
    // Global Constraint 7: the same two pages must produce byte-identical
    // output every time, so nothing downstream can be order-dependent.
    let deltas = diff_metrics(&load("before.prom"), &load("after.prom"));
    let names: Vec<&str> = deltas.iter().map(|d| d.webhook.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "deny.test.admissionlab.io",
            "inject.istio.io",
            "mutate.test.admissionlab.io",
            "policy.kyverno.svc",
            "validate.test.admissionlab.io",
        ]
    );
    assert_eq!(
        deltas,
        diff_metrics(&load("before.prom"), &load("after.prom"))
    );
}

/// A snapshot carrying exactly one duration label set with the given
/// totals, and no rejection family at all -- the shape of a cluster where
/// no webhook has ever rejected anything.
fn snapshot_with(sum: f64, count: u64) -> AdmissionMetricSnapshot {
    AdmissionMetricSnapshot {
        durations: Some(BTreeMap::from([(
            key("w", "CREATE", "false", "admit"),
            DurationSample {
                sum: Some(sum),
                count: Some(count),
            },
        )])),
        rejections: None,
        diagnostics: Vec::new(),
    }
}

#[test]
fn the_exactly_one_rule_is_the_only_path_to_a_per_fixture_latency() {
    // The rule at its three boundaries, driven directly rather than
    // through the fixture pages so the count deltas are unambiguous.
    let before = snapshot_with(1.0, 10);

    let zero = diff_metrics(&before, &snapshot_with(1.0, 10));
    assert_eq!(zero[0].request_count_delta, 0);
    assert_eq!(
        zero[0].attributable_latency(),
        None,
        "a webhook that did not run has no latency to report -- not a zero one"
    );

    let one = diff_metrics(&before, &snapshot_with(1.25, 11));
    assert_eq!(one[0].request_count_delta, 1);
    assert_eq!(
        one[0].attributable_latency(),
        Some(Duration::from_millis(250))
    );

    let two = diff_metrics(&before, &snapshot_with(1.5, 12));
    assert_eq!(two[0].request_count_delta, 2);
    assert_close(two[0].duration_sum_delta, 0.5);
    assert_eq!(
        two[0].attributable_latency(),
        None,
        "two invocations must not be averaged into one plausible-looking latency"
    );
}

#[test]
fn an_absent_rejection_family_yields_no_rejection_claims() {
    // Global Constraint 15, on the one field Task 3.8 froze as a bare
    // `u64`: an API server that has never rejected anything through a
    // webhook does not export the counter at all, so the `0` in
    // `rejection_delta` must never be readable as an observation.
    // A `rejected="true"` label set is the only place a rejection could
    // ever be attributed, so it is the only row that can distinguish
    // "the counter was there and said zero" from "there was no counter".
    let mut before = snapshot_with(1.0, 10);
    let mut after = snapshot_with(1.25, 11);
    for snapshot in [&mut before, &mut after] {
        let durations = snapshot.durations.as_mut().expect("built with a family");
        let sample = durations
            .remove(&key("w", "CREATE", "false", "admit"))
            .expect("the only label set");
        durations.insert(key("w", "CREATE", "true", "admit"), sample);
    }
    let deltas = diff_metrics(&before, &after);
    assert_eq!(deltas[0].rejection_evidence, RejectionEvidence::Unavailable);
    assert_eq!(
        deltas[0].observed_rejection_delta(),
        None,
        "an unexported counter is not evidence of zero rejections"
    );
    assert_eq!(deltas[0].rejection_delta, 0, "the frozen field stays a u64");
}

#[test]
fn an_absent_duration_family_yields_no_deltas_at_all() {
    let empty = AdmissionMetricSnapshot {
        durations: None,
        rejections: None,
        diagnostics: Vec::new(),
    };
    assert!(diff_metrics(&empty, &snapshot_with(1.0, 1)).is_empty());
    assert!(diff_metrics(&snapshot_with(1.0, 1), &empty).is_empty());
}

#[test]
fn a_counter_that_went_backwards_is_unavailable_not_a_saturated_zero() {
    // An API server restarted between the two scrapes. Saturating the
    // subtraction would report "nothing happened", which is a claim this
    // run cannot make.
    let deltas = diff_metrics(&snapshot_with(5.0, 50), &snapshot_with(0.25, 2));
    assert_eq!(deltas[0].duration_evidence, DeltaEvidence::Unavailable);
    assert_eq!(deltas[0].observed_request_count_delta(), None);
    assert_eq!(deltas[0].attributable_latency(), None);
}

#[test]
fn a_label_set_missing_half_its_histogram_is_unavailable() {
    // A `_count` with no `_sum` (a truncated page, or a `_sum` line this
    // parser had to skip) cannot produce a duration delta -- and must not
    // be completed with a fabricated `0.0` sum.
    let after = AdmissionMetricSnapshot {
        durations: Some(BTreeMap::from([(
            key("w", "CREATE", "false", "admit"),
            DurationSample {
                sum: None,
                count: Some(11),
            },
        )])),
        rejections: None,
        diagnostics: Vec::new(),
    };
    let deltas = diff_metrics(&snapshot_with(1.0, 10), &after);
    assert_eq!(deltas[0].duration_evidence, DeltaEvidence::Unavailable);
    assert_eq!(deltas[0].attributable_latency(), None);
}

#[test]
fn a_rejection_increase_with_no_matching_duration_series_is_still_reported() {
    // Rejection evidence is never silently dropped just because no
    // `rejected="true"` duration label set was on the page. Such a row
    // reports `rejected: None` and `Unavailable` duration evidence,
    // because no duration observation for it was actually seen.
    let rejection_key = RejectionKey {
        name: "orphan.test.admissionlab.io".to_string(),
        operation: "CREATE".to_string(),
        webhook_type: "validate".to_string(),
        other_labels: BTreeMap::from([("error_type".to_string(), "no_error".to_string())]),
    };
    let mut before = snapshot_with(1.0, 10);
    before.rejections = Some(BTreeMap::from([(rejection_key.clone(), 4)]));
    let mut after = snapshot_with(1.25, 11);
    after.rejections = Some(BTreeMap::from([(rejection_key, 6)]));

    let deltas = diff_metrics(&before, &after);
    let orphan = delta_for(&deltas, "orphan.test.admissionlab.io");
    assert_eq!(orphan.rejected, None);
    assert_eq!(orphan.rejection_delta, 2);
    assert_eq!(orphan.rejection_evidence, RejectionEvidence::Observed);
    assert_eq!(
        orphan.duration_evidence,
        DeltaEvidence::Unavailable,
        "no duration series was observed for this webhook, so its zeroes are placeholders"
    );
    assert_eq!(orphan.attributable_latency(), None);
}

// =========================================================================
// Step 4: the scrape
// =========================================================================

/// A minimal, otherwise-valid [`ClusterHandle`] pointing at a path that
/// does not exist. Mirrors `admissionlab-fixtures`'s own identical helper.
fn cluster_handle_with_missing_kubeconfig() -> ClusterHandle {
    let unique = RunId::generate();
    ClusterHandle {
        spec: ClusterSpec {
            side: Side::Baseline,
            name: "metrics-test-cluster".to_string(),
            kubernetes_version: "1.36.0".to_string(),
            node_image: "kindest/node:v1.36.0".to_string(),
            images: Vec::new(),
        },
        kubeconfig: std::env::temp_dir().join(format!(
            "admissionlab-metrics-test-{}.yaml",
            unique.as_str()
        )),
        audit_log: std::env::temp_dir().join("admissionlab-metrics-test-audit.log"),
    }
}

#[tokio::test]
async fn the_scrape_requests_the_apiservers_metrics_path_and_returns_the_page() {
    // Pins the one thing about this request that has to be right and
    // cannot be checked without a server: it is a plain `GET /metrics`,
    // with no resource path, query, or body of its own.
    let (mock_service, handle) = mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(mock_service, "default");

    let responder = tokio::spawn(async move {
        let mut handle = pin!(handle);
        let (request, send) = handle.next_request().await.expect("GET /metrics");
        assert_eq!(request.method(), http::Method::GET);
        assert_eq!(request.uri().path(), "/metrics");
        assert_eq!(request.uri().query(), None);
        send.send_response(Response::new(Body::from(
            "# TYPE apiserver_admission_webhook_rejection_count counter\n"
                .as_bytes()
                .to_vec(),
        )));
    });

    let text = scrape_metrics_text_with_client(client, "cluster", Duration::from_secs(5))
        .await
        .expect("the mocked apiserver answered");
    responder.await.expect("mock responder task must not panic");

    assert!(text.contains("apiserver_admission_webhook_rejection_count"));
    assert_eq!(parse_snapshot(&text).rejections.map(|r| r.len()), Some(0));
}

#[tokio::test]
async fn a_scrape_that_never_answers_times_out_rather_than_hanging() {
    // Global Constraint 13. The mock is deliberately never answered, so a
    // missing timeout would hang this test rather than fail it -- which
    // is exactly the failure mode a bounded scrape exists to prevent.
    let (mock_service, _handle) = mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(mock_service, "default");

    let error = scrape_metrics_text_with_client(client, "cluster", Duration::from_millis(50))
        .await
        .expect_err("an unanswered request must not succeed");

    match error {
        MetricsUnavailable::Timeout { cluster, timeout } => {
            assert_eq!(cluster, "cluster");
            assert_eq!(timeout, Duration::from_millis(50));
        }
        other => panic!("expected Timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn a_non_2xx_metrics_response_is_unavailable_not_a_page() {
    // A cluster whose credentials cannot read `/metrics` (RBAC on the
    // `/metrics` non-resource URL) must degrade to "no metric evidence",
    // never to an empty page that would read as "nothing was recorded".
    let (mock_service, handle) = mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(mock_service, "default");

    let responder = tokio::spawn(async move {
        let mut handle = pin!(handle);
        let (_request, send) = handle.next_request().await.expect("GET /metrics");
        send.send_response(
            Response::builder()
                .status(http::StatusCode::FORBIDDEN)
                .body(Body::from(br#"{"kind":"Status","code":403}"#.to_vec()))
                .expect("a well-formed response"),
        );
    });

    let error = scrape_metrics_text_with_client(client, "cluster", Duration::from_secs(5))
        .await
        .expect_err("a 403 must not be reported as a page");
    responder.await.expect("mock responder task must not panic");

    assert!(
        matches!(error, MetricsUnavailable::Request { .. }),
        "expected Request, got {error:?}"
    );
}

#[tokio::test]
async fn a_missing_kubeconfig_is_reported_as_metrics_unavailable() {
    // The only part of `scrape_metrics_text` reachable offline: its
    // client-construction failure path. Fails if a kubeconfig problem
    // were surfaced as anything other than a recoverable absence.
    let cluster = cluster_handle_with_missing_kubeconfig();
    let error = scrape_metrics_text(&cluster, Duration::from_secs(1))
        .await
        .expect_err("a nonexistent kubeconfig must not succeed");
    match error {
        MetricsUnavailable::Client { cluster, .. } => {
            assert_eq!(cluster, "metrics-test-cluster");
        }
        other => panic!("expected Client, got {other:?}"),
    }
}
