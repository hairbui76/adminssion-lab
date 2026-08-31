//! Golden and behavioral tests for the Alpha JSON result artifact.
//!
//! The golden lives in `testdata/golden/result-alpha.json` rather than
//! inline, because unlike the terminal report this document is a
//! contract other tools read: a reviewer changing the model should have
//! to look at a real, complete, pretty-printed example of what consumers
//! will receive, and a `git diff` of that file is the review.
//!
//! The comparison is byte for byte. Parsing both sides and comparing
//! `Value`s would pass for a document with different key order, different
//! indentation, or a missing trailing newline -- all three of which are
//! part of what `write_json_report` promises.

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use admissionlab_report::{ReportError, SCHEMA_VERSION, render_json, write_json_report};
use serde_json::Value;
use support::canonical_result;

/// The committed Alpha example, embedded at compile time.
const GOLDEN: &str = include_str!("../../../testdata/golden/result-alpha.json");

/// A fresh, guaranteed-unique directory under the system temp directory,
/// following the same convention as the other crates' filesystem tests.
fn unique_temp_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-report-json-test-{}-{label}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create unique temp dir");
    dir
}

/// The golden document, parsed. Used only by the tests that assert on
/// *structure*; the contract test compares bytes.
fn golden_value() -> Value {
    serde_json::from_str(GOLDEN).expect("the golden file is valid JSON")
}

#[test]
fn the_canonical_result_matches_the_golden_byte_for_byte() {
    let rendered = render_json(&canonical_result()).expect("the canonical result serializes");

    assert_eq!(
        rendered, GOLDEN,
        "the Alpha result shape changed; review `testdata/golden/result-alpha.json` \
         and regenerate it deliberately"
    );
}

#[test]
fn writing_produces_exactly_the_rendered_bytes() {
    let result = canonical_result();
    let path = unique_temp_dir("write").join("result.json");

    write_json_report(&path, &result).expect("writing the report succeeds");

    let written = std::fs::read_to_string(&path).expect("the report was written");
    assert_eq!(written, GOLDEN);
}

#[test]
fn the_document_ends_with_exactly_one_newline() {
    let rendered = render_json(&canonical_result()).expect("serializes");

    assert!(rendered.ends_with("}\n"));
    assert!(!rendered.ends_with("\n\n"));
}

#[test]
fn serialization_is_deterministic() {
    let result = canonical_result();

    assert_eq!(
        render_json(&result).expect("serializes"),
        render_json(&result).expect("serializes"),
    );
}

#[test]
fn the_schema_version_is_the_pinned_alpha_identifier() {
    assert_eq!(SCHEMA_VERSION, "admissionlab.io/result/v1alpha1");
    assert_eq!(
        golden_value()["schemaVersion"],
        Value::String(SCHEMA_VERSION.to_owned()),
        "the serialized key is `schemaVersion`, not `schema_version`"
    );
}

#[test]
fn report_owned_fields_are_camel_case() {
    let golden = golden_value();

    for key in [
        "schemaVersion",
        "runId",
        "summary",
        "environments",
        "fixtures",
        "policy",
        "diagnostics",
    ] {
        assert!(
            golden.get(key).is_some(),
            "top-level key `{key}` is missing from the golden"
        );
    }
    assert!(golden["summary"].get("fixturesTotal").is_some());
    assert!(golden["fixtures"][0].get("fixtureId").is_some());
    assert!(
        golden["fixtures"][0]["admission"]
            .get("firstDivergence")
            .is_some()
    );
}

#[test]
fn foreign_types_keep_their_own_pinned_wire_names() {
    // These keys are `admissionlab-admission`'s, `admissionlab-diff`'s,
    // and `admissionlab-policy`'s, not this crate's. The report does not
    // re-pin them: each owning crate already froze its own tags, and a
    // second rename layer here could not reach into a foreign type
    // anyway. The mixed casing in one document is the honest rendering
    // of who owns what.
    let golden = golden_value();
    let outcome = &golden["fixtures"][0]["admission"]["baseline"];

    assert!(outcome.get("fixture_id").is_some());
    assert!(outcome.get("total_latency").is_some());
    assert!(outcome.get("final_object").is_some());
    assert!(golden["policy"].get("stale_expectations").is_some());
    assert!(
        golden["policy"]["changes"][0]["change"]
            .get("object_path")
            .is_some()
    );
}

#[test]
fn semantic_kind_and_severity_strings_are_the_owning_crates_wire_tags() {
    let golden = golden_value();
    let critical = &golden["policy"]["changes"][0];

    assert_eq!(
        critical["change"]["kind"],
        Value::String("container_added".to_owned())
    );
    assert_eq!(critical["severity"], Value::String("critical".to_owned()));
    assert_eq!(critical["expected"], Value::Bool(false));
    assert_eq!(
        golden["policy"]["changes"][1]["change"]["kind"],
        Value::String("image_changed".to_owned())
    );
    assert_eq!(
        golden["policy"]["disposition"],
        Value::String("fail".to_owned())
    );
}

#[test]
fn the_golden_carries_a_critical_change_with_its_divergence() {
    let golden = golden_value();
    let divergence = &golden["fixtures"][0]["admission"]["firstDivergence"];

    assert_eq!(
        divergence["confidence"],
        Value::String("observed".to_owned())
    );
    assert!(
        divergence["explanation"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "an attribution with no explanation is not usable evidence"
    );
    assert_eq!(
        golden["policy"]["changes"][0]["change"]["origin"]["candidate_position"],
        serde_json::json!([0, 0])
    );
}

#[test]
fn the_golden_carries_an_expected_change_and_an_unknown_confidence_attribution() {
    let golden = golden_value();

    assert_eq!(
        golden["policy"]["changes"][1]["expected"],
        Value::Bool(true)
    );
    assert_eq!(
        golden["fixtures"][1]["admission"]["firstDivergence"]["confidence"],
        Value::String("unknown".to_owned()),
        "an unlocated divergence must serialize as `unknown`, never as an observation"
    );
}

#[test]
fn the_golden_carries_an_inconclusive_fixture() {
    let golden = golden_value();
    let decision = &golden["fixtures"][2]["admission"]["candidate"]["decision"];

    assert!(
        decision.get("unsupported_dry_run").is_some(),
        "the inconclusive case must be visible in the document, not only in the counts"
    );
    assert_eq!(golden["summary"]["inconclusive"], serde_json::json!(1));
}

#[test]
fn the_golden_carries_a_stale_expectation_and_diagnostics() {
    let golden = golden_value();

    assert_eq!(
        golden["policy"]["stale_expectations"][0]["id"],
        Value::String("sidecar-injection-rollout".to_owned())
    );
    assert_eq!(golden["diagnostics"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        golden["diagnostics"][1]["context"]["baseline"],
        Value::String("[REDACTED]".to_owned()),
        "a `Sensitive` context entry serializes as the withheld marker"
    );
}

#[test]
fn the_timings_block_pins_its_wire_names() {
    let golden = golden_value();
    let timings = &golden["timings"];

    assert_eq!(
        timings["clusterCreation"]["wallMs"],
        serde_json::json!(43_512)
    );
    assert_eq!(
        timings["clusterCreation"]["baselineMs"],
        serde_json::json!(41_204)
    );
    assert_eq!(
        timings["installation"]["baseline"]["components"][0]["name"],
        Value::String("sidecar-injector".to_owned()),
        "the install breakdown names the same component `environments` does"
    );
    assert_eq!(
        timings["installation"]["baseline"]["components"][0]["elapsedMs"],
        serde_json::json!(92_310)
    );
    assert_eq!(
        timings["fixtureCapture"]["fixtures"],
        serde_json::json!(4),
        "the fixture count is per side, and this run replayed four fixtures"
    );
    assert_eq!(timings["comparisonMs"], serde_json::json!(212));
    assert_eq!(timings["elapsedMs"], serde_json::json!(149_006));
}

#[test]
fn an_unmeasured_stage_is_an_absent_key_and_never_a_zero() {
    let golden = golden_value();
    let timings = golden["timings"].as_object().expect("timings is an object");

    // A `result.json` is written *during* the reporting stage and
    // *before* cleanup, so a document that reported a duration for either
    // would be reporting a stage that had not happened when it was
    // serialized. Global Constraint 15: the honest rendering of that is
    // no key at all.
    assert!(
        !timings.contains_key("reportingMs"),
        "a result written by the reporting stage cannot carry that stage's duration"
    );
    assert!(
        !timings.contains_key("cleanup"),
        "a result written before cleanup cannot carry cleanup's duration"
    );
}

#[test]
fn the_reserved_gateway_field_is_present_as_null() {
    let golden = golden_value();

    for fixture in golden["fixtures"].as_array().expect("fixtures is an array") {
        assert_eq!(
            fixture["gateway"],
            Value::Null,
            "the key is always present, so `no Gateway evidence` is distinguishable \
             from `this producer predates the field`"
        );
    }
}

#[test]
fn the_summary_counts_partition_the_fixtures() {
    let golden = golden_value();
    let summary = &golden["summary"];
    let total: u64 = [
        "identical",
        "expected",
        "warnings",
        "critical",
        "inconclusive",
    ]
    .into_iter()
    .map(|key| summary[key].as_u64().expect("a count is a number"))
    .sum();

    assert_eq!(total, summary["fixturesTotal"].as_u64().expect("a count"));
    assert_eq!(
        total,
        golden["fixtures"].as_array().expect("an array").len() as u64
    );
}

#[test]
fn writing_replaces_existing_content_completely() {
    let path = unique_temp_dir("replace").join("result.json");
    std::fs::write(&path, "x".repeat(100_000)).expect("seed the destination");

    write_json_report(&path, &canonical_result()).expect("writing succeeds");

    let written = std::fs::read_to_string(&path).expect("the report was written");
    assert_eq!(
        written, GOLDEN,
        "a rename-into-place must leave no trace of the previous, longer content"
    );
}

#[test]
fn writing_leaves_no_temporary_file_behind() {
    let dir = unique_temp_dir("no-temp");
    write_json_report(&dir.join("result.json"), &canonical_result()).expect("writing succeeds");

    let entries: Vec<String> = std::fs::read_dir(&dir)
        .expect("read the temp dir")
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    assert_eq!(entries, vec!["result.json".to_owned()]);
}

#[test]
fn a_missing_parent_directory_is_a_reported_error() {
    let path = unique_temp_dir("missing-parent")
        .join("does-not-exist")
        .join("result.json");

    let error = write_json_report(&path, &canonical_result())
        .expect_err("writing into a missing directory must fail");

    assert!(
        matches!(error, ReportError::Io { .. }),
        "expected an I/O error, got {error:?}"
    );
    assert!(
        error.to_string().contains("create temporary file"),
        "the message must name what was attempted; got `{error}`"
    );
}
