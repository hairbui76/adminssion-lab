//! The stable `admissionlab.io/v1` configuration contract (ROADMAP Task
//! 9.1).
//!
//! `tests/schema.rs` owns the two frozen pre-stable schemas;
//! `tests/migrate_alpha_beta.rs` owns the Alpha -> Beta boundary. This
//! file owns the freeze itself, in four groups:
//!
//! - **The published artifact.** `schemas/admissionlab-v1.json` is
//!   byte-for-byte what [`admissionlab_spec::v1_json_schema`] generates,
//!   with an `#[ignore]`d regenerator beside the checker so the two
//!   cannot drift.
//! - **The stable-schema rule, as a test.** The generated `v1` schema is
//!   a backward-compatible superset of the frozen
//!   `schemas/admissionlab-v1beta1.json`: no property dropped, no
//!   requirement dropped, every addition optional. That is
//!   `docs/schema-migrations.md`'s rule for `v1.x`, checked rather than
//!   promised.
//! - **Zero wire changes.** Stronger than the superset rule and the
//!   reason `v1` may share every nested type with `v1beta1`: with prose
//!   stripped and the two `apiVersion` constants neutralized, the two
//!   schemas are *equal*. A field added to one model and not the other
//!   fails here.
//! - **The reader matrix.** `v1alpha1`, `v1beta1` and `v1` documents all
//!   load, and a lab written in any two of them resolves to the same
//!   [`admissionlab_spec::ResolvedLab`].
//!
//! Two clauses of the rule are pinned elsewhere, deliberately rather than
//! by omission: the semantic-change wire strings a result document
//! serializes are pinned by `admissionlab-diff`'s own exhaustive
//! `SemanticChangeKind` tests and by `admissionlab-report`'s
//! `tests/stable_schema.rs`, and the exit-code contract is ROADMAP Task
//! 9.2's, pinned by `admissionlab-cli`'s exit-code tests.

use std::path::PathBuf;

use admissionlab_spec::{
    MigrationError, ResolvedLab, SUPPORTED_API_VERSIONS, SpecError, V1Beta1Lab,
    load_any_supported_lab, migrate_v1beta1_to_v1, v1, v1alpha1, v1beta1,
};
use serde_json::Value;

// ---------------------------------------------------------------------
// Test support
// ---------------------------------------------------------------------

/// The workspace root, two levels above this crate's own
/// `CARGO_MANIFEST_DIR`.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Path to a file under the workspace-root `schemas/` directory.
fn schema_file_path(name: &str) -> PathBuf {
    workspace_root().join("schemas").join(name)
}

/// Path to one of the checked-in fixtures under `testdata/configs/`.
fn testdata_config(name: &str) -> PathBuf {
    workspace_root().join("testdata/configs").join(name)
}

/// Renders `schema` as canonical, pretty-printed JSON text with a single
/// trailing newline — the exact byte-for-byte form checked in under
/// `schemas/`. See `src/schema.rs` for why this is deterministic.
fn render(schema: &schemars::Schema) -> String {
    let mut text =
        serde_json::to_string_pretty(schema).expect("a schemars::Schema always serializes");
    text.push('\n');
    text
}

fn render_v1_schema() -> String {
    render(&admissionlab_spec::v1_json_schema())
}

/// The generated stable schema, parsed.
fn v1_schema_value() -> Value {
    serde_json::from_str(&render_v1_schema()).expect("the generated schema is valid JSON")
}

/// The frozen `schemas/admissionlab-v1beta1.json`, parsed.
fn beta_schema_value() -> Value {
    let text = std::fs::read_to_string(schema_file_path("admissionlab-v1beta1.json"))
        .expect("the v1beta1 schema is checked in");
    serde_json::from_str(&text).expect("the checked-in schema is valid JSON")
}

/// A [`ResolvedLab`] with `source_path` replaced, so two labs loaded from
/// *different files in the same directory* compare equal on everything
/// else. Every other path in a resolved lab is joined onto the
/// configuration's own directory, which the twins share.
fn without_source_path(mut lab: ResolvedLab) -> ResolvedLab {
    lab.source_path = PathBuf::new();
    lab
}

/// A temporary directory that deletes itself when the test's binding goes
/// out of scope — including on a failed assertion, because [`Drop`] runs
/// while a panic unwinds. Mirrors `tests/migrate_alpha_beta.rs`.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "admissionlab-spec-stable-{}-{label}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn write(&self, name: &str, yaml: &str) -> PathBuf {
        let path = self.path.join(name);
        std::fs::write(&path, yaml).expect("write temp config");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Every property name a JSON Schema object node declares.
fn properties(schema: &Value) -> Vec<String> {
    schema["properties"]
        .as_object()
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

/// Every property name a JSON Schema object node requires.
fn required(schema: &Value) -> Vec<String> {
    schema["required"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Removes every documentation-only key from `value`, recursively.
///
/// `title` and `description` are generated from Rust type and field
/// documentation. They are what a schema *says about itself*, not part of
/// the contract a document has to satisfy, so two schemas that differ
/// only in prose describe the same documents — which is exactly the claim
/// [`the_stable_schema_has_no_wire_change_from_v1beta1`] needs to make.
fn strip_prose(value: &mut Value) {
    match value {
        Value::Object(members) => {
            members.remove("title");
            members.remove("description");
            for member in members.values_mut() {
                strip_prose(member);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_prose(item);
            }
        }
        _ => {}
    }
}

/// Replaces the root `apiVersion` property's `const` with a placeholder,
/// so two versions' schemas can be compared on everything *except* the
/// one value they are required to disagree about.
fn neutralize_api_version(schema: &mut Value) {
    let target = schema
        .get_mut("properties")
        .and_then(|properties| properties.get_mut("apiVersion"))
        .and_then(|api_version| api_version.get_mut("const"))
        .expect("every lab schema const-locks its own apiVersion");
    *target = Value::String("<the document's own version>".to_owned());
}

// ---------------------------------------------------------------------
// The published artifact
// ---------------------------------------------------------------------

#[test]
fn the_stable_schema_matches_the_checked_in_file() {
    let path = schema_file_path("admissionlab-v1.json");
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "checked-in schema missing at {} ({error}); generate it with `cargo test -p \
             admissionlab-spec --test stable_schema -- --ignored regenerate_stable_schema_file`",
            path.display()
        )
    });

    assert_eq!(
        render_v1_schema(),
        expected,
        "the generated schema no longer matches schemas/admissionlab-v1.json; the v1 \
         configuration schema is stable — an addition must be optional and regenerated with \
         `cargo test -p admissionlab-spec --test stable_schema -- --ignored \
         regenerate_stable_schema_file`, and a removal or rename needs a v2 and a migration note \
         in docs/schema-migrations.md"
    );
}

#[test]
fn stable_schema_generation_is_deterministic_across_runs() {
    assert_eq!(render_v1_schema(), render_v1_schema());
}

#[test]
fn the_stable_schema_locks_its_own_api_version() {
    let schema = v1_schema_value();
    assert_eq!(
        schema["properties"]["apiVersion"]["const"],
        Value::from("admissionlab.io/v1"),
        "an editor validating a lab file must be told the exact apiVersion this schema describes"
    );
    // Pinned as a literal rather than compared to the constant alone: the
    // string is a wire value users type and consumers match on, so a typo
    // in the promotion has to fail here rather than propagate.
    assert_eq!(v1::API_VERSION, "admissionlab.io/v1");
    assert_eq!(
        schema["properties"]["apiVersion"]["const"],
        Value::from(v1::API_VERSION)
    );
}

// ---------------------------------------------------------------------
// The stable-schema rule, as a test
// ---------------------------------------------------------------------

/// `docs/schema-migrations.md`'s rule for `v1.x`, checked against the two
/// artifacts it governs: the frozen `v1beta1` schema is the previous
/// published contract, and the generated `v1` schema is this build's.
///
/// - every `v1beta1` property still exists in `v1` (a removal or a rename
///   needs a `v2` and a migration note, and there is none);
/// - every `v1beta1` requirement is still required (a requirement dropped
///   is a semantics change even though it looks like a relaxation);
/// - every `v1` addition is optional (a new required field would
///   invalidate every document written before it).
///
/// Applied to the root document *and* to every definition both schemas
/// share, because a field can be dropped just as quietly three levels
/// down as at the top.
#[test]
fn the_stable_schema_is_a_backward_compatible_superset_of_v1beta1() {
    let v1 = v1_schema_value();
    let beta = beta_schema_value();

    let check = |node: &str, old: &Value, new: &Value| {
        let new_properties = properties(new);
        for property in properties(old) {
            assert!(
                new_properties.contains(&property),
                "v1 dropped or renamed the v1beta1 property {property:?} on {node}; that needs a \
                 v2 and a migration note in docs/schema-migrations.md"
            );
        }
        let new_required = required(new);
        for property in required(old) {
            assert!(
                new_required.contains(&property),
                "v1 stopped requiring {property:?} on {node}, which v1beta1 required; a \
                 requirement dropped is a semantics change, not an addition"
            );
        }
        let old_properties = properties(old);
        for property in &new_properties {
            assert!(
                old_properties.contains(property) || !new_required.contains(property),
                "v1 added {property:?} to {node} as a *required* field, which invalidates every \
                 v1beta1 document; additions must be optional"
            );
        }
    };

    check("the root document", &beta, &v1);

    let beta_defs = beta["$defs"]
        .as_object()
        .expect("the beta schema has $defs");
    let v1_defs = v1["$defs"].as_object().expect("the v1 schema has $defs");
    for (name, beta_def) in beta_defs {
        let v1_def = v1_defs.get(name).unwrap_or_else(|| {
            panic!(
                "v1 no longer defines {name:?}, which v1beta1 published; a definition that \
                 disappears takes every property in it with it"
            )
        });
        check(name, beta_def, v1_def);
    }
}

/// The claim `crates/admissionlab-spec/src/v1.rs` makes in prose, as an
/// assertion: the stable freeze changed **nothing** on the wire.
///
/// This is what licenses [`admissionlab_spec::V1Lab`] to share every
/// nested type with [`V1Beta1Lab`] instead of declaring copies. Strip the
/// documentation (which is prose about the contract, not the contract)
/// and neutralize the one value the two versions must disagree about, and
/// the two published schemas are equal — so a field added to one model
/// and forgotten in the other, or a default that drifted, fails here
/// rather than shipping.
#[test]
fn the_stable_schema_has_no_wire_change_from_v1beta1() {
    let mut v1 = v1_schema_value();
    let mut beta = beta_schema_value();
    for schema in [&mut v1, &mut beta] {
        strip_prose(schema);
        neutralize_api_version(schema);
    }

    assert_eq!(
        v1, beta,
        "v1 and v1beta1 no longer describe the same document. If that is deliberate it is a wire \
         change at a boundary that renamed nothing: give v1 its own copies of the types it no \
         longer shares, write the migration in migrate.rs and the note in \
         docs/schema-migrations.md, and make migrate_v1beta1_to_v1 do the translation"
    );
}

// ---------------------------------------------------------------------
// The reader matrix
// ---------------------------------------------------------------------

#[test]
fn every_supported_version_is_accepted_current_first() {
    assert_eq!(
        SUPPORTED_API_VERSIONS,
        [
            "admissionlab.io/v1",
            "admissionlab.io/v1beta1",
            "admissionlab.io/v1alpha1"
        ],
        "all three versions are read; only the newest is what a user should write today"
    );
    assert_eq!(
        SUPPORTED_API_VERSIONS,
        [v1::API_VERSION, v1beta1::API_VERSION, v1alpha1::API_VERSION]
    );
}

#[test]
fn one_lab_written_in_all_three_versions_resolves_to_one_value() {
    for (alpha, beta, stable) in [
        (
            "minimal-valid.yaml",
            "minimal-valid-v1beta1.yaml",
            "minimal-valid-v1.yaml",
        ),
        (
            "renamed-fields-v1alpha1.yaml",
            "renamed-fields-v1beta1.yaml",
            "renamed-fields-v1.yaml",
        ),
    ] {
        let load = |name: &str| {
            without_source_path(
                load_any_supported_lab(&testdata_config(name))
                    .unwrap_or_else(|error| panic!("{name} must load and resolve: {error}")),
            )
        };
        assert_eq!(
            load(stable),
            load(beta),
            "{stable} and {beta} describe the same lab; a v1beta1 document must resolve to \
             exactly what its v1 twin does"
        );
        assert_eq!(
            load(stable),
            load(alpha),
            "{stable} and {alpha} describe the same lab; a v1alpha1 document must resolve to \
             exactly what its v1 twin does, after both migrations"
        );
    }
}

/// The same property over the fixtures that have no hand-written twin,
/// generated instead of copied: rewrite only the `apiVersion` line of a
/// checked-in `v1beta1` document and the resolved lab must not move.
///
/// `migration-valid.yaml` is the reason this exists. Its `migration:`
/// section is the newest part of the configuration surface — it landed
/// inside `v1beta1` as an additive change, *after* that version's freeze
/// — so it is the section with the least history of being carried across
/// a boundary, and the one a stable freeze most needs to prove it
/// carries. Both copies are written to one directory so every resolved
/// path is joined against the same configuration directory and the two
/// values are comparable.
#[test]
fn every_checked_in_beta_document_resolves_identically_when_written_as_v1() {
    for name in [
        "minimal-valid-v1beta1.yaml",
        "renamed-fields-v1beta1.yaml",
        "migration-valid.yaml",
    ] {
        let original = std::fs::read_to_string(testdata_config(name))
            .unwrap_or_else(|error| panic!("{name} must be readable: {error}"));
        assert!(
            original.contains(v1beta1::API_VERSION),
            "{name} is no longer a v1beta1 document; the Beta read-support proof needs a \
             replacement fixture"
        );
        let promoted = original.replace(v1beta1::API_VERSION, v1::API_VERSION);

        let dir = TempDir::new("promote");
        let beta_path = dir.write("beta.yaml", &original);
        let v1_path = dir.write("v1.yaml", &promoted);

        let beta = load_any_supported_lab(&beta_path)
            .unwrap_or_else(|error| panic!("{name} must load as v1beta1: {error}"));
        let stable = load_any_supported_lab(&v1_path)
            .unwrap_or_else(|error| panic!("{name} must load once promoted to v1: {error}"));

        assert_eq!(
            without_source_path(stable),
            without_source_path(beta),
            "{name} resolved differently after nothing but its apiVersion line changed"
        );
    }
}

#[test]
fn an_unsupported_api_version_is_refused_and_names_all_three_versions() {
    let dir = TempDir::new("unsupported");
    let path = dir.write(
        "admissionlab.yaml",
        "apiVersion: admissionlab.io/v2\n\
         kind: Lab\n\
         baseline:\n  kubernetes: \"1.29.4\"\n\
         candidate:\n  kubernetes: \"1.29.4\"\n\
         fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n",
    );

    let error = load_any_supported_lab(&path).expect_err("v2 is not a supported version");
    let message = error.to_string();
    assert!(matches!(error, SpecError::Validation { .. }), "{message}");
    for version in SUPPORTED_API_VERSIONS {
        assert!(
            message.contains(version),
            "the error must name {version}, got: {message}"
        );
    }
    assert!(
        message.contains("admissionlab.io/v2\""),
        "the error must quote what was actually found, got: {message}"
    );
}

#[test]
fn a_v1_document_still_rejects_an_unknown_field() {
    // Promoting a schema must not relax the strictness the two frozen
    // versions already had: a misspelled key is a named parse error, not
    // a silently ignored one.
    let dir = TempDir::new("unknown-field");
    let path = dir.write(
        "admissionlab.yaml",
        "apiVersion: admissionlab.io/v1\n\
         kind: Lab\n\
         baseline:\n  kubernetes: \"1.29.4\"\n\
         candiate:\n  kubernetes: \"1.29.4\"\n\
         fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n",
    );

    let error = load_any_supported_lab(&path).expect_err("a misspelled key is refused");
    assert!(
        matches!(error, SpecError::Parse { .. }) && error.to_string().contains("candiate"),
        "expected an unknown-field parse error naming the typo, got: {error}"
    );
}

#[test]
fn a_v1_document_still_rejects_the_alpha_spelling_of_a_renamed_key() {
    // The Beta renames stayed renamed. `deny_unknown_fields` is what
    // makes that a fact about the parser rather than a claim in a
    // changelog, and the stable version inherits it by sharing the very
    // type that enforces it.
    let dir = TempDir::new("alpha-key");
    let path = dir.write(
        "admissionlab.yaml",
        "apiVersion: admissionlab.io/v1\n\
         kind: Lab\n\
         baseline:\n  kubernetes: \"1.29.4\"\n\
         candidate:\n  kubernetes: \"1.29.4\"\n\
         fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n\
         policy:\n  latency:\n    absoluteIncrease: 50\n",
    );

    let error = load_any_supported_lab(&path).expect_err("a v1 document must reject the alpha key");
    assert!(
        matches!(error, SpecError::Parse { .. }) && error.to_string().contains("absoluteIncrease"),
        "expected an unknown-field parse error naming absoluteIncrease, got: {error}"
    );
}

// ---------------------------------------------------------------------
// The Beta -> stable migration itself
// ---------------------------------------------------------------------

#[test]
fn migration_refuses_a_document_that_is_not_a_v1beta1_lab() {
    // The one precondition, and the only reachable `MigrationError` — a
    // hand-built value whose header says it is something else. See
    // `migrate.rs`'s "Then why does this return a `Result`?".
    let beta = V1Beta1Lab {
        api_version: v1::API_VERSION.to_owned(),
        kind: "Lab".to_owned(),
        baseline: sample_environment(),
        candidate: sample_environment(),
        fixtures: sample_fixtures(),
        policy: v1beta1::PolicySpec::default(),
        expectations_file: None,
        gateway: None,
        migration: None,
    };

    assert_eq!(
        migrate_v1beta1_to_v1(beta.clone()),
        Err(MigrationError::UnexpectedApiVersion {
            found: v1::API_VERSION.to_owned(),
            expected: v1beta1::API_VERSION,
            target: v1::API_VERSION,
        })
    );

    let wrong_kind = V1Beta1Lab {
        api_version: v1beta1::API_VERSION.to_owned(),
        kind: "FixtureMatrix".to_owned(),
        ..beta
    };
    assert_eq!(
        migrate_v1beta1_to_v1(wrong_kind),
        Err(MigrationError::UnexpectedKind {
            found: "FixtureMatrix".to_owned(),
            expected: "Lab",
        })
    );
}

/// The smallest environment the loader accepts, built by hand so the
/// migration's precondition can be tested without a file.
fn sample_environment() -> admissionlab_spec::EnvironmentSpec {
    admissionlab_spec::EnvironmentSpec {
        kubernetes: "1.29.4".to_owned(),
        images: Vec::new(),
        components: Vec::new(),
    }
}

fn sample_fixtures() -> admissionlab_spec::FixtureSelectionSpec {
    admissionlab_spec::FixtureSelectionSpec {
        include: vec!["fixtures/**/*.yaml".to_owned()],
    }
}

#[test]
#[ignore = "run explicitly to (re)write schemas/admissionlab-v1.json after a deliberate, additive model change"]
fn regenerate_stable_schema_file() {
    std::fs::write(schema_file_path("admissionlab-v1.json"), render_v1_schema())
        .expect("write schema file");
}
