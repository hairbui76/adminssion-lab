//! The frozen `admissionlab.io/result/v1beta1` contract.
//!
//! Three things are asserted here, and together they are the freeze:
//!
//! 1. `schemas/result-v1beta1.json` is exactly what
//!    [`admissionlab_report::result_v1beta1_json_schema`] currently
//!    generates from the Rust result model, byte for byte.
//! 2. `testdata/golden/result-v1beta1.json` is exactly what the shared
//!    canonical example serializes to -- that assertion lives in
//!    `tests/json.rs`, which owns the golden -- and *validates against
//!    the generated schema*, which is this file's job.
//! 3. The freeze's three roadmap requirements hold on a real document:
//!    explicit evidence/confidence fields, three separate evidence
//!    sections, and semantic change identifiers that are stable within a
//!    run.
//!
//! Structured after `admissionlab-spec`'s `tests/schema.rs`, including
//! the `#[ignore]`d regeneration tests: a developer who deliberately
//! changes the model runs those and reviews the resulting diff, which is
//! the review of a schema change.
//!
//! # Why the validation is hand-written
//!
//! Validating the golden against the generated schema needs a JSON
//! Schema validator, and this workspace has none: the `jsonschema` crate
//! is not in `Cargo.lock`, and pulling a validator (and its regex engine
//! and its own transitive tree) into the dependency graph to check one
//! file in one test is a poor trade. [`validate`] below is therefore a
//! deliberately small structural validator covering exactly the
//! constructs `schemars` emits for this model -- `$ref`/`$defs`,
//! `oneOf`/`anyOf`/`allOf`, `type` (including type arrays), `enum`,
//! `const`, `properties`/`required`/`additionalProperties`,
//! `items`/`prefixItems`, and boolean schemas. It is not a conformant
//! JSON Schema implementation and does not try to be; what it does
//! catch is the failure that actually matters here -- a key in the
//! document that the published schema does not describe, or a value of
//! the wrong shape -- and it catches it for *every* key at *every*
//! depth, which is the property the freeze needs.

mod support;

use std::collections::BTreeSet;
use std::path::PathBuf;

use admissionlab_report::{render_json, result_v1beta1_json_schema, semantic_change_id};
use serde_json::Value;
use support::canonical_result;

/// The checked-in golden document, embedded at compile time.
const GOLDEN: &str = include_str!("../../../testdata/golden/result-v1beta1.json");

/// Path to `schemas/result-v1beta1.json`, at the workspace root (two
/// levels above this crate's `CARGO_MANIFEST_DIR`).
fn schema_file_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/result-v1beta1.json")
}

/// Path to `testdata/golden/result-v1beta1.json`.
fn golden_file_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/golden/result-v1beta1.json")
}

/// The current schema as canonical, pretty-printed JSON text with a
/// single trailing newline -- the exact form checked in at
/// [`schema_file_path`].
fn render_schema() -> String {
    let schema = result_v1beta1_json_schema();
    let mut text =
        serde_json::to_string_pretty(&schema).expect("a schemars::Schema always serializes");
    text.push('\n');
    text
}

/// The generated schema, parsed.
fn schema_value() -> Value {
    serde_json::from_str(&render_schema()).expect("the generated schema is valid JSON")
}

/// The golden document, parsed.
fn golden_value() -> Value {
    serde_json::from_str(GOLDEN).expect("the golden file is valid JSON")
}

#[test]
fn schema_matches_checked_in_file() {
    let expected = std::fs::read_to_string(schema_file_path()).unwrap_or_else(|error| {
        panic!(
            "checked-in schema missing at {:?} ({error}); generate it with \
             `cargo test -p admissionlab-report --test result_schema -- --ignored regenerate_schema_file`",
            schema_file_path()
        )
    });

    assert_eq!(
        render_schema(),
        expected,
        "the generated schema no longer matches schemas/result-v1beta1.json; the result schema is \
         frozen -- if this change is deliberate and additive, regenerate with `cargo test -p \
         admissionlab-report --test result_schema -- --ignored regenerate_schema_file`, and if it \
         removes or renames a field it needs a new schema version and a migration note instead"
    );
}

#[test]
fn schema_generation_is_deterministic_across_runs() {
    assert_eq!(render_schema(), render_schema());
}

#[test]
fn the_golden_validates_against_the_generated_schema() {
    let schema = schema_value();
    let golden = golden_value();
    let mut errors = Vec::new();

    validate(&golden, &schema, &schema, "$", &mut errors);

    assert!(
        errors.is_empty(),
        "the golden document does not validate against the generated schema:\n  {}",
        errors.join("\n  ")
    );
}

#[test]
fn the_schema_describes_the_frozen_version() {
    let golden = golden_value();

    assert_eq!(
        golden["schemaVersion"],
        Value::String("admissionlab.io/result/v1beta1".to_owned())
    );
    assert_eq!(
        golden["schemaVersion"],
        Value::String(admissionlab_report::SCHEMA_VERSION.to_owned())
    );
}

#[test]
fn every_fixture_writes_all_three_evidence_sections() {
    // Global Constraint 15: a consumer never infers "there was no
    // Gateway evidence" from a missing key. Every entry carries all
    // three section keys; the ones that do not apply are `null`.
    let golden = golden_value();
    let fixtures = golden["fixtures"]
        .as_array()
        .expect("`fixtures` is an array");
    assert!(!fixtures.is_empty());

    for fixture in fixtures {
        for section in ["admission", "gatewayReconciliation", "traffic"] {
            assert!(
                fixture.get(section).is_some(),
                "fixture {} is missing the `{section}` key entirely",
                fixture["fixtureId"]
            );
        }
    }
}

#[test]
fn admission_evidence_states_its_own_availability() {
    let golden = golden_value();
    let confidences: BTreeSet<String> = golden["fixtures"]
        .as_array()
        .expect("`fixtures` is an array")
        .iter()
        .filter_map(|fixture| fixture["admission"].as_object())
        .map(|admission| {
            let comparability = admission
                .get("comparability")
                .expect("every admission section states its comparability");
            assert!(!comparability.is_null());

            let trace = admission
                .get("traceEvidence")
                .expect("every admission section states each side's trace evidence");
            assert!(trace.get("baseline").is_some_and(Value::is_string));
            assert!(trace.get("candidate").is_some_and(Value::is_string));

            admission["divergenceConfidence"]
                .as_str()
                .expect("`divergenceConfidence` is always a written word")
                .to_owned()
        })
        .collect();

    // The canonical example covers the observed case, the deliberately
    // labelled `unknown` case, and the case where no attribution was
    // produced at all -- which is spelled, not left as a `null`
    // `firstDivergence` for a reader to interpret.
    assert!(confidences.contains("observed"));
    assert!(confidences.contains("unknown"));
    assert!(confidences.contains("unattributed"));

    for fixture in golden["fixtures"].as_array().expect("an array") {
        let Some(admission) = fixture["admission"].as_object() else {
            continue;
        };
        let unattributed =
            admission["divergenceConfidence"] == Value::String("unattributed".into());
        assert_eq!(
            unattributed,
            admission["firstDivergence"].is_null(),
            "`unattributed` and a null `firstDivergence` must always agree"
        );
    }
}

#[test]
fn gateway_evidence_is_split_into_reconciliation_and_traffic() {
    let golden = golden_value();
    let gateway = golden["fixtures"]
        .as_array()
        .expect("an array")
        .iter()
        .find(|fixture| !fixture["gatewayReconciliation"].is_null())
        .expect("the canonical example carries a Gateway route contract");

    let reconciliation = &gateway["gatewayReconciliation"];
    assert!(reconciliation.get("contractId").is_some());
    assert_eq!(reconciliation["comparability"], Value::from("comparable"));
    assert!(reconciliation["evidenceLevel"].get("baseline").is_some());
    assert!(reconciliation["evidenceLevel"].get("candidate").is_some());
    // The reconciliation section carries reconciliation evidence and
    // nothing else: the probes are the traffic section's.
    assert!(reconciliation["baseline"].get("converged").is_some());
    assert!(reconciliation["baseline"].get("probes").is_none());

    let traffic = &gateway["traffic"];
    assert_eq!(traffic["contractId"], reconciliation["contractId"]);
    assert_eq!(traffic["evidence"], Value::from("observed"));
    assert_eq!(
        traffic["pairs"].as_array().expect("an array").len(),
        1,
        "one probe was answered by both sides"
    );
    assert_eq!(traffic["pairs"][0]["index"], Value::from(0));
    assert_eq!(
        traffic["unpairedBaseline"]
            .as_array()
            .expect("an array")
            .len(),
        1,
        "one probe was answered only by the baseline"
    );
    assert!(
        traffic["unpairedCandidate"]
            .as_array()
            .expect("an array")
            .is_empty()
    );

    // A fixture with no Gateway evidence gets `null` sections rather
    // than empty ones -- "not a route contract" is not "a route
    // contract with nothing in it".
    let admission_only = golden["fixtures"]
        .as_array()
        .expect("an array")
        .iter()
        .find(|fixture| !fixture["admission"].is_null())
        .expect("the canonical example carries admission fixtures too");
    assert!(admission_only["gatewayReconciliation"].is_null());
    assert!(admission_only["traffic"].is_null());
}

#[test]
fn change_identifiers_are_stable_across_two_serializations() {
    let result = canonical_result();
    let first: Value = serde_json::from_str(&render_json(&result).expect("serializes"))
        .expect("the rendered document is valid JSON");
    let second: Value = serde_json::from_str(&render_json(&result).expect("serializes"))
        .expect("the rendered document is valid JSON");

    let ids = |document: &Value| -> Vec<String> {
        document["policy"]["changes"]
            .as_array()
            .expect("an array")
            .iter()
            .map(|change| change["id"].as_str().expect("a string").to_owned())
            .collect()
    };

    let first_ids = ids(&first);
    assert!(!first_ids.is_empty(), "the example has graded changes");
    assert_eq!(first_ids, ids(&second));
    assert!(first_ids.iter().all(|id| id.starts_with("sc-")));
}

#[test]
fn a_change_carries_one_identifier_in_both_lists() {
    // The per-fixture list and the run-wide policy list are two views of
    // the same claims; a content-derived identifier is what lets a
    // consumer join them without comparing whole payloads.
    let golden = golden_value();

    let mut per_fixture: Vec<String> = Vec::new();
    for fixture in golden["fixtures"].as_array().expect("an array") {
        for change in fixture["changes"].as_array().expect("an array") {
            per_fixture.push(change["id"].as_str().expect("a string").to_owned());
        }
    }
    let run_wide: Vec<String> = golden["policy"]["changes"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|change| change["id"].as_str().expect("a string").to_owned())
        .collect();

    assert!(!per_fixture.is_empty());
    assert_eq!(per_fixture, run_wide);
}

#[test]
fn a_change_identifier_ignores_its_grade_and_its_attribution() {
    // Re-grading a change (a new expectation, a different `failOn`) or
    // attributing its origin more precisely must not renumber it: the
    // identifier names the claim, not the run's opinion of it.
    let result = canonical_result();
    let classified = &result.fixtures[0].changes[0];
    let baseline_id = semantic_change_id(&classified.change);

    let mut regraded = classified.change.clone();
    regraded.origin = None;
    assert_eq!(semantic_change_id(&regraded), baseline_id);

    // ...but a different claim gets a different identifier.
    let mut different = classified.change.clone();
    different.subject = Some("some-other-container".to_owned());
    assert_ne!(semantic_change_id(&different), baseline_id);
}

#[test]
#[ignore = "run explicitly to (re)write schemas/result-v1beta1.json after a deliberate model change"]
fn regenerate_schema_file() {
    std::fs::write(schema_file_path(), render_schema()).expect("write schema file");
}

#[test]
#[ignore = "run explicitly to (re)write testdata/golden/result-v1beta1.json after a deliberate model change"]
fn regenerate_golden_file() {
    let rendered = render_json(&canonical_result()).expect("the canonical result serializes");
    std::fs::write(golden_file_path(), rendered).expect("write golden file");
}

/// Checks `value` against `schema`, appending one message per failure.
///
/// `root` is the whole schema document, used to resolve `$ref`; `path`
/// is the JSON path walked so far, so a failure names the offending key
/// rather than only its shape. See this file's module documentation for
/// what this covers and what it deliberately does not.
fn validate(value: &Value, schema: &Value, root: &Value, path: &str, errors: &mut Vec<String>) {
    // A boolean schema: `true` accepts anything (what `schemars` emits
    // for a `serde_json::Value` field), `false` accepts nothing.
    match schema {
        Value::Bool(true) => return,
        Value::Bool(false) => {
            errors.push(format!("{path}: schema accepts no value here"));
            return;
        }
        _ => {}
    }
    let Some(object) = schema.as_object() else {
        errors.push(format!(
            "{path}: schema node is neither an object nor a bool"
        ));
        return;
    };

    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        match resolve(reference, root) {
            Some(target) => validate(value, target, root, path, errors),
            None => errors.push(format!("{path}: unresolvable $ref `{reference}`")),
        }
        return;
    }

    if let Some(allowed) = object.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        errors.push(format!("{path}: {value} is not one of {allowed:?}"));
    }
    if let Some(expected) = object.get("const")
        && expected != value
    {
        errors.push(format!("{path}: {value} is not the constant {expected}"));
    }

    // `oneOf`/`anyOf` are how `schemars` writes an externally tagged
    // enum and an `Option`; one branch accepting is enough.
    for keyword in ["oneOf", "anyOf"] {
        if let Some(branches) = object.get(keyword).and_then(Value::as_array) {
            let accepted = branches.iter().any(|branch| {
                let mut branch_errors = Vec::new();
                validate(value, branch, root, path, &mut branch_errors);
                branch_errors.is_empty()
            });
            if !accepted {
                errors.push(format!("{path}: no `{keyword}` branch accepts {value}"));
            }
        }
    }
    if let Some(branches) = object.get("allOf").and_then(Value::as_array) {
        for branch in branches {
            validate(value, branch, root, path, errors);
        }
    }

    if let Some(declared) = object.get("type")
        && !type_matches(declared, value)
    {
        errors.push(format!("{path}: {value} does not have type {declared}"));
        return;
    }

    if let Some(members) = value.as_object() {
        let properties = object.get("properties").and_then(Value::as_object);
        for (key, member) in members {
            let child = format!("{path}.{key}");
            match properties.and_then(|properties| properties.get(key)) {
                Some(property) => validate(member, property, root, &child, errors),
                None => match object.get("additionalProperties") {
                    Some(Value::Bool(false)) => {
                        errors.push(format!("{child}: not described by the schema"));
                    }
                    Some(additional) => validate(member, additional, root, &child, errors),
                    // No `additionalProperties` keyword only means
                    // "unconstrained" when this node describes an object
                    // at all; a node with `properties` and none of the
                    // document's keys in it is the drift worth naming.
                    None => {
                        if properties.is_some() {
                            errors.push(format!("{child}: not described by the schema"));
                        }
                    }
                },
            }
        }
        if let Some(required) = object.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !members.contains_key(key) {
                    errors.push(format!("{path}.{key}: required by the schema but absent"));
                }
            }
        }
    }

    if let Some(items) = value.as_array() {
        if let Some(prefix) = object.get("prefixItems").and_then(Value::as_array) {
            for (index, (item, item_schema)) in items.iter().zip(prefix).enumerate() {
                validate(item, item_schema, root, &format!("{path}[{index}]"), errors);
            }
        } else if let Some(item_schema) = object.get("items") {
            for (index, item) in items.iter().enumerate() {
                validate(item, item_schema, root, &format!("{path}[{index}]"), errors);
            }
        }
    }
}

/// Resolves a local `#/$defs/Name` reference against the schema root.
fn resolve<'a>(reference: &str, root: &'a Value) -> Option<&'a Value> {
    let name = reference.strip_prefix("#/$defs/")?;
    root.get("$defs")?.get(name)
}

/// Whether `value` has the JSON Schema `type` (or one of the types)
/// `declared` names.
fn type_matches(declared: &Value, value: &Value) -> bool {
    match declared {
        Value::String(name) => matches_type_name(name, value),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .any(|name| matches_type_name(name, value)),
        _ => false,
    }
}

/// Whether `value` is an instance of the single JSON Schema type `name`.
fn matches_type_name(name: &str, value: &Value) -> bool {
    match name {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "string" => value.is_string(),
        _ => false,
    }
}
