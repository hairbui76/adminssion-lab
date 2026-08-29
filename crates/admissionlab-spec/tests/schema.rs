//! Verifies that [`admissionlab_spec::v1alpha1_json_schema`] is
//! deterministic and that the checked-in
//! `schemas/admissionlab-v1alpha1.json` is exactly what it currently
//! produces.
//!
//! `schema_matches_checked_in_file` is the test CI runs. It regenerates
//! the schema and compares it, byte-for-byte, against the checked-in
//! file. `regenerate_schema_file` is this task's "small generator
//! command" (brief Step 3): an `#[ignore]`d test a developer runs
//! explicitly to update the checked-in file after a deliberate model
//! change. Both share `render_schema`, so the two can never drift apart
//! from each other by construction — only from the checked-in file, which
//! is exactly what the main test catches.

use std::path::PathBuf;

/// Path to `schemas/admissionlab-v1alpha1.json`, which lives at the
/// workspace root (two levels above this crate's own
/// `CARGO_MANIFEST_DIR`).
fn schema_file_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/admissionlab-v1alpha1.json")
}

/// Renders the current schema as canonical, pretty-printed JSON text with
/// a single trailing newline — the exact byte-for-byte form checked in at
/// [`schema_file_path`].
///
/// See `src/schema.rs`'s module documentation for why this is
/// deterministic across runs: `schemars::Schema`'s `Serialize`
/// implementation orders JSON Schema keywords itself, and — because
/// neither `schemars`'s nor `serde_json`'s `preserve_order` feature is
/// enabled anywhere in this workspace — every other key falls back to
/// `serde_json::Map`'s `BTreeMap`-backed lexicographic order.
fn render_schema() -> String {
    let schema = admissionlab_spec::v1alpha1_json_schema();
    let mut text =
        serde_json::to_string_pretty(&schema).expect("a schemars::Schema always serializes");
    text.push('\n');
    text
}

#[test]
fn schema_matches_checked_in_file() {
    let expected = std::fs::read_to_string(schema_file_path()).unwrap_or_else(|e| {
        panic!(
            "checked-in schema missing at {:?} ({e}); generate it with \
             `cargo test -p admissionlab-spec --test schema -- --ignored regenerate_schema_file`",
            schema_file_path()
        )
    });
    let actual = render_schema();

    assert_eq!(
        actual, expected,
        "generated schema no longer matches schemas/admissionlab-v1alpha1.json; \
         regenerate it with `cargo test -p admissionlab-spec --test schema -- --ignored regenerate_schema_file`"
    );
}

#[test]
fn schema_generation_is_deterministic_across_runs() {
    // Independent of the checked-in file: two generations in the same
    // process must agree byte-for-byte with each other.
    assert_eq!(render_schema(), render_schema());
}

#[test]
#[ignore = "run explicitly to (re)write schemas/admissionlab-v1alpha1.json after a deliberate model change"]
fn regenerate_schema_file() {
    std::fs::write(schema_file_path(), render_schema()).expect("write schema file");
}
