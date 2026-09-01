//! Proof that [`redact_result`] removes every secret it claims to, and
//! nothing else.
//!
//! The load-bearing test here is [`every_sentinel_is_absent_after_redaction`]:
//! it serializes the *whole* redacted result and asserts that not one of
//! the eight planted sentinel strings survives anywhere in the document.
//! Asserting on the serialized text rather than on individual fields is
//! the point -- a structural assertion only proves the fields the test
//! author thought to check, while the sentinel scan covers every field,
//! every nested payload, and any field a later task adds.
//!
//! Its companion, [`the_unredacted_result_contains_every_sentinel`], is
//! what makes the first test meaningful: it proves the sentinels really
//! are in the input, so a `redact_result` that returned an empty
//! document, or one that a refactor accidentally turned into a no-op
//! *test*, could not pass both.
//!
//! The remaining tests pin each rule individually, so a failure names
//! which rule broke rather than only that something leaked.

mod support;

use admissionlab_diff::{SemanticChange, SemanticChangeKind};
use admissionlab_policy::{ClassifiedChange, Severity};
use admissionlab_report::{
    FixtureBucket, LabResult, REDACTED, REDACTED_PRIVATE_KEY, RedactionRules, RunSummary,
    redact_result,
};
use serde_json::{Value, json};
use support::{
    ENV_LITERAL_SENTINEL, HEADER_FIELD_SENTINEL, HEADER_SENTINEL, PATCH_ENV_LITERAL_SENTINEL,
    PEM_SENTINEL, POINTER_SENTINEL, SECRET_DATA_SENTINEL, SECRET_STRING_DATA_SENTINEL,
    SENTINEL_POINTER, SENTINELS, canonical_result, sentinel_result,
};

/// The rules every whole-result test uses: the built-in ones plus the
/// single configured pointer that reaches the one sentinel no built-in
/// rule is meant to find.
fn rules() -> RedactionRules {
    RedactionRules::new().with_json_pointer(SENTINEL_POINTER)
}

/// Serializes a result to compact JSON for substring scanning.
fn serialized(result: &LabResult) -> String {
    serde_json::to_string(result).expect("a LabResult always serializes")
}

#[test]
fn the_unredacted_result_contains_every_sentinel() {
    let text = serialized(&sentinel_result());

    for sentinel in SENTINELS {
        assert!(
            text.contains(sentinel),
            "the *unredacted* fixture is missing sentinel {sentinel}; \
             the absence assertion in the companion test would then pass \
             for the wrong reason"
        );
    }
}

#[test]
fn every_sentinel_is_absent_after_redaction() {
    let text = serialized(&redact_result(&sentinel_result(), &rules()));

    for sentinel in SENTINELS {
        assert!(
            !text.contains(sentinel),
            "sentinel {sentinel} survived redaction; \
             serialized result was:\n{text}"
        );
    }
}

#[test]
fn redaction_does_not_mutate_its_input() {
    let original = sentinel_result();
    let snapshot = original.clone();

    let _ = redact_result(&original, &rules());

    assert_eq!(
        original, snapshot,
        "redact_result must leave its argument untouched"
    );
}

#[test]
fn redaction_is_idempotent() {
    let once = redact_result(&sentinel_result(), &rules());
    let twice = redact_result(&once, &rules());

    assert_eq!(
        once, twice,
        "redacting an already-redacted result must be a no-op"
    );
}

#[test]
fn redaction_preserves_structure() {
    let original = sentinel_result();
    let redacted = redact_result(&original, &rules());

    assert_eq!(redacted.fixtures.len(), original.fixtures.len());
    assert_eq!(redacted.policy.changes.len(), original.policy.changes.len());
    assert_eq!(
        redacted.policy.disposition, original.policy.disposition,
        "redaction must never change the run's verdict"
    );
    assert_eq!(
        redacted.summary, original.summary,
        "redaction must never change the bucket counts"
    );

    // The change whose payloads held a credential is still a change, and
    // still says exactly what changed and where. Global Constraint 15:
    // the existence of a difference is itself the evidence.
    let change = &redacted.fixtures[0].changes[0].change;
    assert_eq!(change.kind, SemanticChangeKind::ContainerAdded);
    assert_eq!(
        change.object_path.as_deref(),
        Some("/spec/template/spec/containers/1")
    );
    assert_eq!(change.subject.as_deref(), Some("istio-proxy"));
    assert_eq!(
        change.candidate,
        Some(json!({"name": "DB_PASSWORD", "value": REDACTED})),
        "the environment entry, its name, and the fact that it changed all survive"
    );
    assert_eq!(
        redacted.fixtures[0].changes[0].severity,
        Severity::Critical,
        "redaction must never change a grade"
    );
}

#[test]
fn secret_data_values_are_replaced_and_key_names_kept() {
    let redacted = redact_result(&sentinel_result(), &rules());
    let object = candidate_final_object(&redacted);
    let secret = &object["items"][0];

    assert_eq!(secret["kind"], json!("Secret"));
    assert_eq!(
        secret["data"],
        json!({"password": REDACTED}),
        "the value goes, the key name stays"
    );
    assert_eq!(secret["stringData"], json!({"apiToken": REDACTED}));
    assert_eq!(
        secret["metadata"]["name"],
        json!("app-credentials"),
        "a Secret's own name is not its data"
    );

    let _ = (SECRET_DATA_SENTINEL, SECRET_STRING_DATA_SENTINEL);
}

#[test]
fn a_secret_nested_anywhere_in_the_tree_is_found() {
    // The Secret above sits inside a `List`'s `items`, not at the root:
    // a rule that only inspected the top-level object would miss it.
    let redacted = redact_result(&sentinel_result(), &rules());
    let text = serialized(&redacted);

    assert!(!text.contains(SECRET_DATA_SENTINEL));
    assert_eq!(candidate_final_object(&redacted)["kind"], json!("List"));
}

#[test]
fn credential_named_env_literals_are_replaced_and_references_are_not() {
    let redacted = redact_result(&sentinel_result(), &rules());
    let containers = &candidate_final_object(&redacted)["items"][1]["spec"]["containers"];
    let env = &containers[0]["env"];

    assert_eq!(env[0], json!({"name": "DB_PASSWORD", "value": REDACTED}));
    assert_eq!(
        env[1],
        json!({
            "name": "DB_HOST",
            "valueFrom": {"secretKeyRef": {"name": "app-credentials", "key": "host"}}
        }),
        "a `valueFrom` reference names a Secret and a key; it holds neither"
    );

    let _ = ENV_LITERAL_SENTINEL;
}

#[test]
fn a_non_credential_env_literal_is_left_alone() {
    let result = result_with_change_payload(json!([
        {"name": "LOG_LEVEL", "value": "debug"},
        {"name": "API_TOKEN", "value": "t0ps3cret"}
    ]));

    let redacted = redact_result(&result, &RedactionRules::new());
    let payload = redacted.policy.changes[0]
        .change
        .candidate
        .clone()
        .expect("the payload is present");

    assert_eq!(
        payload,
        json!([
            {"name": "LOG_LEVEL", "value": "debug"},
            {"name": "API_TOKEN", "value": REDACTED}
        ])
    );
}

#[test]
fn a_credential_name_pattern_can_be_added_but_the_defaults_always_apply() {
    let result = result_with_change_payload(json!({"name": "LICENCE_BLOB", "value": "abc123"}));

    let untouched = redact_result(&result, &RedactionRules::new());
    assert_eq!(
        untouched.policy.changes[0].change.candidate,
        Some(json!({"name": "LICENCE_BLOB", "value": "abc123"})),
        "`licence` matches no default pattern"
    );

    let configured = redact_result(
        &result,
        &RedactionRules::new().with_env_name_pattern("LICENCE"),
    );
    assert_eq!(
        configured.policy.changes[0].change.candidate,
        Some(json!({"name": "LICENCE_BLOB", "value": REDACTED})),
        "an added pattern is matched case-insensitively, as a substring"
    );
}

#[test]
fn header_values_are_replaced_in_free_text() {
    let redacted = redact_result(&sentinel_result(), &rules());
    let warning = &redacted.fixtures[0]
        .admission
        .as_ref()
        .expect("the critical fixture has an admission comparison")
        .candidate
        .warnings[1];

    assert_eq!(
        warning,
        &format!("upstream replied 401; request line was\nAuthorization: {REDACTED}\nretrying"),
        "the header name and the lines around it survive; only the value goes"
    );

    let _ = HEADER_SENTINEL;
}

#[test]
fn header_named_object_keys_are_replaced() {
    let redacted = redact_result(&sentinel_result(), &rules());
    let headers = &candidate_final_object(&redacted)["status"]["observedHeaders"];

    assert_eq!(headers["authorization"], json!(REDACTED));
    assert_eq!(
        headers["content-type"],
        json!("application/json"),
        "an unrelated header is not a credential"
    );

    let _ = HEADER_FIELD_SENTINEL;
}

#[test]
fn a_header_name_only_matches_at_a_word_boundary() {
    let result = result_with_change_payload(json!(
        "x-authorization-mode: RBAC\nauthorizationMode: Node\nCookie: session=abc"
    ));

    let redacted = redact_result(&result, &RedactionRules::new());

    assert_eq!(
        redacted.policy.changes[0].change.candidate,
        Some(json!(format!(
            "x-authorization-mode: RBAC\nauthorizationMode: Node\nCookie: {REDACTED}"
        ))),
        "`x-authorization-mode` and `authorizationMode` are not Authorization headers"
    );
}

#[test]
fn private_key_blocks_are_replaced_and_certificates_are_not() {
    let redacted = redact_result(&sentinel_result(), &rules());

    assert_eq!(
        redacted.diagnostics[0].message,
        format!("webhook bootstrap wrote\n{REDACTED_PRIVATE_KEY}\nto the shared volume")
    );

    let _ = PEM_SENTINEL;

    let with_certificate = result_with_change_payload(json!(
        "-----BEGIN CERTIFICATE-----\nMIIBpublic\n-----END CERTIFICATE-----"
    ));
    let kept = redact_result(&with_certificate, &RedactionRules::new());
    assert_eq!(
        kept.policy.changes[0].change.candidate,
        Some(json!(
            "-----BEGIN CERTIFICATE-----\nMIIBpublic\n-----END CERTIFICATE-----"
        )),
        "a certificate is public material a reader may need"
    );
}

#[test]
fn a_truncated_private_key_block_is_still_redacted() {
    let result = result_with_change_payload(json!(
        "log tail: -----BEGIN EC PRIVATE KEY-----\nMHcCAQEEIclipped"
    ));

    let redacted = redact_result(&result, &RedactionRules::new());

    assert_eq!(
        redacted.policy.changes[0].change.candidate,
        Some(json!(format!("log tail: {REDACTED_PRIVATE_KEY}"))),
        "half a private key is still key material"
    );
}

#[test]
fn configured_pointers_are_resolved_against_each_payload_root() {
    let redacted = redact_result(&sentinel_result(), &rules());
    let pod = &candidate_final_object(&redacted)["items"][1];

    assert_eq!(pod["spec"]["licence"], json!(REDACTED));
    let _ = POINTER_SENTINEL;
}

#[test]
fn a_pointer_that_does_not_resolve_is_a_no_op() {
    let result = result_with_change_payload(json!({"spec": {"replicas": 3}}));

    let redacted = redact_result(
        &result,
        &RedactionRules::new().with_json_pointer("/spec/nothing/here"),
    );

    assert_eq!(
        redacted.policy.changes[0].change.candidate,
        Some(json!({"spec": {"replicas": 3}})),
        "the same rule is applied to every payload; most will not have the field"
    );
}

#[test]
fn webhook_patch_payloads_are_redacted() {
    let redacted = redact_result(&sentinel_result(), &rules());
    let patch = redacted.fixtures[0]
        .admission
        .as_ref()
        .expect("the critical fixture has an admission comparison")
        .candidate
        .trace
        .invocations[0]
        .patch
        .as_ref()
        .expect("the sentinel fixture plants a patch");

    let serialized_patch = serde_json::to_value(patch).expect("a patch always serializes");
    assert_eq!(
        serialized_patch,
        json!([{
            "op": "add",
            "path": "/spec/containers/0/env/1",
            "value": {"name": "VAULT_TOKEN", "value": REDACTED}
        }]),
        "an injected credential must be caught in the patch too, not only in the final object"
    );

    let _ = PATCH_ENV_LITERAL_SENTINEL;
}

#[test]
fn sensitive_diagnostic_context_stays_sensitive() {
    let redacted = redact_result(&canonical_result(), &RedactionRules::new());
    let context = serde_json::to_value(&redacted.diagnostics[1].context)
        .expect("a diagnostic context always serializes");

    assert_eq!(
        context,
        json!({"baseline": REDACTED, "candidate": REDACTED}),
        "`RedactedValue::Sensitive` carries no payload; there is nothing to leak or to rewrite"
    );
}

#[test]
fn bucket_counting_partitions_the_run() {
    let result = canonical_result();
    let summary = RunSummary::from_fixtures(&result.fixtures);

    assert_eq!(summary.fixtures_total, 5);
    assert_eq!(summary.critical, 1);
    assert_eq!(summary.expected, 1);
    assert_eq!(summary.inconclusive, 1);
    assert_eq!(summary.identical, 1);
    // The Gateway route contract: two unexpected `warning` changes and
    // no unexpected `critical` one.
    assert_eq!(summary.warnings, 1);
    assert_eq!(
        summary.identical
            + summary.expected
            + summary.warnings
            + summary.critical
            + summary.inconclusive,
        summary.fixtures_total,
        "every fixture lands in exactly one bucket"
    );
}

#[test]
fn each_canonical_fixture_lands_in_its_own_bucket() {
    let result = canonical_result();
    let buckets: Vec<FixtureBucket> = result
        .fixtures
        .iter()
        .map(admissionlab_report::FixtureComparison::bucket)
        .collect();

    assert_eq!(
        buckets,
        vec![
            FixtureBucket::Critical,
            FixtureBucket::Expected,
            FixtureBucket::Inconclusive,
            FixtureBucket::Identical,
            FixtureBucket::Warnings,
        ]
    );
}

#[test]
fn an_incomparable_fixture_is_inconclusive_even_with_a_critical_change() {
    // The candidate side could not be replayed at all. Every lower
    // bucket would state something about a relationship this run did not
    // establish -- including `critical`, which would present a one-sided
    // observation as a proven regression.
    let mut result = canonical_result();
    let critical_change = result.fixtures[0].changes[0].clone();
    result.fixtures[2].changes.push(critical_change);

    assert_eq!(result.fixtures[2].bucket(), FixtureBucket::Inconclusive);
}

#[test]
fn a_fixture_with_no_admission_evidence_is_inconclusive_not_identical() {
    let mut result = canonical_result();
    result.fixtures[3].admission = None;

    assert_eq!(
        result.fixtures[3].bucket(),
        FixtureBucket::Inconclusive,
        "no evidence is not the same claim as `the two sides agreed`"
    );
}

#[test]
fn an_unexpected_critical_outranks_expected_changes_on_the_same_fixture() {
    let mut result = canonical_result();
    let expected_change = result.fixtures[1].changes[0].clone();
    result.fixtures[0].changes.push(expected_change);

    assert_eq!(result.fixtures[0].bucket(), FixtureBucket::Critical);
}

#[test]
fn an_unexpected_warning_outranks_an_expected_critical() {
    let mut result = canonical_result();
    let fixture = &mut result.fixtures[1];
    let mut warning = fixture.changes[0].clone();
    warning.severity = Severity::Warning;
    warning.expected = false;
    fixture.changes.push(warning);

    assert_eq!(fixture.bucket(), FixtureBucket::Warnings);
}

#[test]
fn an_unexpected_info_change_counts_as_expected_not_identical() {
    let mut result = canonical_result();
    let mut info = result.fixtures[0].changes[0].clone();
    info.severity = Severity::Info;
    info.expected = false;
    result.fixtures[3].changes.push(info);

    assert_eq!(
        result.fixtures[3].bucket(),
        FixtureBucket::Expected,
        "a real difference the policy declined to warn on is not `identical`"
    );
}

/// The candidate side's final object from the first fixture, as JSON.
fn candidate_final_object(result: &LabResult) -> Value {
    result.fixtures[0]
        .admission
        .as_ref()
        .expect("the critical fixture has an admission comparison")
        .candidate
        .final_object
        .clone()
        .expect("the sentinel fixture plants a final object")
}

/// A minimal result whose single change carries `payload` as its
/// candidate value.
///
/// Used by the per-rule tests that need one specific payload rather than
/// the whole canonical example. Built on top of [`canonical_result`] so
/// it stays a valid `LabResult` as the model grows.
fn result_with_change_payload(payload: Value) -> LabResult {
    let mut result = canonical_result();
    let template = result.fixtures[0].changes[0].clone();
    let change = ClassifiedChange {
        change: SemanticChange {
            candidate: Some(payload),
            baseline: None,
            ..template.change
        },
        ..template
    };
    result.fixtures[0].changes = vec![change.clone()];
    result.policy.changes = vec![change];
    result
}
