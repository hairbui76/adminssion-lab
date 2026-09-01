//! Verifies that both generated configuration schemas are deterministic
//! and that the checked-in files under `schemas/` are exactly what the
//! current models produce.
//!
//! `*_schema_matches_checked_in_file` are the tests CI runs. Each
//! regenerates its schema and compares it, byte-for-byte, against the
//! checked-in file. The `#[ignore]`d `regenerate_*_schema_file` tests are
//! this task's "small generator command": a developer runs one explicitly
//! to update a checked-in file after a deliberate model change. Each pair
//! shares one `render_*` helper, so the checker and the generator can
//! never drift apart from each other by construction — only from the
//! checked-in file, which is exactly what the main test catches.
//!
//! # The v1alpha1 pair is also the freeze
//!
//! `admissionlab.io/v1alpha1` is frozen (ROADMAP Task 7.1 Step 2 keeps it
//! readable through at least v1.0). `alpha_schema_matches_checked_in_file`
//! is what makes that enforceable rather than merely stated: it fails on
//! *any* change to `crate::v1alpha1` — including one made indirectly, by
//! editing a type in `crate::model` that both versions share. That is
//! precisely why sharing those types between the two versions is safe.

use std::path::PathBuf;

/// Path to a file under the workspace-root `schemas/` directory, which
/// lives two levels above this crate's own `CARGO_MANIFEST_DIR`.
fn schema_file_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas")
        .join(name)
}

/// Renders `schema` as canonical, pretty-printed JSON text with a single
/// trailing newline — the exact byte-for-byte form checked in under
/// `schemas/`.
///
/// See `src/schema.rs`'s module documentation for why this is
/// deterministic across runs: `schemars::Schema`'s `Serialize`
/// implementation orders JSON Schema keywords itself, and — because
/// neither `schemars`'s nor `serde_json`'s `preserve_order` feature is
/// enabled anywhere in this workspace — every other key falls back to
/// `serde_json::Map`'s `BTreeMap`-backed lexicographic order.
fn render(schema: &schemars::Schema) -> String {
    let mut text =
        serde_json::to_string_pretty(schema).expect("a schemars::Schema always serializes");
    text.push('\n');
    text
}

fn render_beta_schema() -> String {
    render(&admissionlab_spec::v1beta1_json_schema())
}

fn render_alpha_schema() -> String {
    render(&admissionlab_spec::v1alpha1_json_schema())
}

/// Asserts the checked-in `schemas/<name>` is byte-for-byte what
/// `actual` currently generates, naming the exact regeneration command in
/// the failure message.
fn assert_matches_checked_in(name: &str, test_name: &str, actual: &str) {
    let path = schema_file_path(name);
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "checked-in schema missing at {} ({e}); generate it with \
             `cargo test -p admissionlab-spec --test schema -- --ignored {test_name}`",
            path.display()
        )
    });

    assert_eq!(
        actual, expected,
        "generated schema no longer matches schemas/{name}; regenerate it with \
         `cargo test -p admissionlab-spec --test schema -- --ignored {test_name}`"
    );
}

#[test]
fn beta_schema_matches_checked_in_file() {
    assert_matches_checked_in(
        "admissionlab-v1beta1.json",
        "regenerate_beta_schema_file",
        &render_beta_schema(),
    );
}

#[test]
fn alpha_schema_matches_checked_in_file() {
    assert_matches_checked_in(
        "admissionlab-v1alpha1.json",
        "regenerate_alpha_schema_file",
        &render_alpha_schema(),
    );
}

#[test]
fn schema_generation_is_deterministic_across_runs() {
    // Independent of the checked-in files: two generations in the same
    // process must agree byte-for-byte with each other.
    assert_eq!(render_beta_schema(), render_beta_schema());
    assert_eq!(render_alpha_schema(), render_alpha_schema());
}

/// The two schemas describe two different documents, and the property
/// worth asserting is not that they differ somewhere but that they differ
/// *exactly where the freeze review said they would*: the `apiVersion`
/// constant, and the two renamed duration keys.
#[test]
fn the_two_schemas_lock_their_own_api_versions_and_duration_keys() {
    let beta: serde_json::Value =
        serde_json::from_str(&render_beta_schema()).expect("beta schema is JSON");
    let alpha: serde_json::Value =
        serde_json::from_str(&render_alpha_schema()).expect("alpha schema is JSON");

    assert_eq!(
        beta["properties"]["apiVersion"]["const"],
        serde_json::json!("admissionlab.io/v1beta1")
    );
    assert_eq!(
        alpha["properties"]["apiVersion"]["const"],
        serde_json::json!("admissionlab.io/v1alpha1")
    );

    let latency = |schema: &serde_json::Value| {
        schema["$defs"]["LatencyPolicy"]["properties"]
            .as_object()
            .expect("LatencyPolicy has properties")
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        latency(&beta),
        vec!["absoluteIncreaseMillis", "relativeMultiplier"]
    );
    assert_eq!(
        latency(&alpha),
        vec!["absoluteIncrease", "relativeMultiplier"]
    );

    let gateway_keys = |schema: &serde_json::Value| {
        schema["$defs"]["GatewaySuiteSpec"]["properties"]
            .as_object()
            .expect("GatewaySuiteSpec has properties")
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert!(
        gateway_keys(&beta).contains(&"reconciliationTimeoutMillis".to_owned()),
        "beta gateway keys: {:?}",
        gateway_keys(&beta)
    );
    assert!(
        gateway_keys(&alpha).contains(&"reconciliationTimeout".to_owned()),
        "alpha gateway keys: {:?}",
        gateway_keys(&alpha)
    );
}

#[test]
#[ignore = "run explicitly to (re)write schemas/admissionlab-v1beta1.json after a deliberate model change"]
fn regenerate_beta_schema_file() {
    std::fs::write(
        schema_file_path("admissionlab-v1beta1.json"),
        render_beta_schema(),
    )
    .expect("write schema file");
}

#[test]
#[ignore = "run explicitly to (re)write schemas/admissionlab-v1alpha1.json — the v1alpha1 model is frozen, so needing this is itself a review signal"]
fn regenerate_alpha_schema_file() {
    std::fs::write(
        schema_file_path("admissionlab-v1alpha1.json"),
        render_alpha_schema(),
    )
    .expect("write schema file");
}
