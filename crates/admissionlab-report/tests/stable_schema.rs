//! The stable `admissionlab.io/result/v1` freeze (ROADMAP Task 9.1).
//!
//! `tests/result_schema.rs` owns the published artifact — it regenerates
//! `schemas/result-v1.json`, validates the golden against it, and asserts
//! the evidence-section requirements. This file owns the *rule* that
//! artifact is now frozen under, and it has three clauses to pin:
//!
//! - **No property or requirement dropped versus `v1beta1`, and every
//!   addition optional.** Measured against the frozen
//!   `schemas/result-v1beta1.json`, which is why that file stays checked
//!   in with no generator behind it.
//! - **Zero wire change at the freeze.** Stronger than the clause above,
//!   and the whole reason the stable promotion is a one-line change: with
//!   prose stripped, the two published schemas are *equal*.
//! - **Semantic-change serialization strings cannot be renamed.** The
//!   closed set of `snake_case` wire strings a consumer matches on is
//!   listed here as literals. `admissionlab-diff`'s own tests prove the
//!   Rust enum is exhaustive and that each variant serializes to its
//!   pinned string; what this file adds is that the *set* is part of the
//!   result schema's frozen surface, so extending it is a deliberate,
//!   reviewed act and renaming one is a new result schema version.
//!
//! Two more clauses of `docs/schema-migrations.md`'s stable rule are
//! elsewhere by design: the configuration family's is
//! `admissionlab-spec`'s `tests/stable_schema.rs`, and the exit-code
//! contract is ROADMAP Task 9.2's, pinned by `admissionlab-cli`'s
//! exit-code tests.
//!
//! # There is no reader to test
//!
//! A result is emit-only: [`admissionlab_report::LabResult`] implements
//! `Serialize` and never `Deserialize`, so unlike the configuration and
//! the run manifest there is no "older version still loads" property
//! here. What replaces it is that a `v1beta1` result document is still
//! *valid against its own published schema*, which the frozen file makes
//! true by construction, and that this build emits exactly one version,
//! which `tests/json.rs` pins.

use std::path::PathBuf;

use admissionlab_report::{SCHEMA_VERSION, result_v1_json_schema};
use serde_json::Value;

/// Every `SemanticChangeKind` wire string, in the order the schema lists
/// them.
///
/// A closed set a consumer's `match` is written against: renaming one
/// silently re-classifies a finding for every reader that does not know,
/// which is why `docs/schema-migrations.md` makes it a new result schema
/// version rather than an ordinary change. Adding one is allowed within
/// `v1.x` — a new *case* gets a new literal, and a reader that does not
/// know it must tolerate it — and shows up here as a deliberate edit.
const SEMANTIC_CHANGE_KINDS: [&str; 26] = [
    "newly_denied",
    "newly_allowed",
    "container_added",
    "container_removed",
    "init_container_added",
    "init_container_removed",
    "volume_added",
    "volume_removed",
    "volume_mount_changed",
    "environment_changed",
    "image_changed",
    "service_account_changed",
    "security_context_changed",
    "resource_requirement_changed",
    "webhook_failed",
    "webhook_invocation_changed",
    "webhook_latency_changed",
    "route_attached",
    "route_detached",
    "backend_resolution_changed",
    "listener_binding_changed",
    "accepted_condition_changed",
    "resolved_refs_condition_changed",
    "programmed_condition_changed",
    "traffic_status_changed",
    "traffic_backend_changed",
];

/// Every `MigrationBehaviorKind` wire string, frozen for the same reason
/// and under the same rule as [`SEMANTIC_CHANGE_KINDS`].
const MIGRATION_BEHAVIOR_KINDS: [&str; 7] = [
    "host_behavior_changed",
    "path_behavior_changed",
    "tls_behavior_changed",
    "backend_changed",
    "rewrite_behavior_changed",
    "redirect_behavior_changed",
    "non_portable_feature",
];

/// Path to a file under the workspace-root `schemas/` directory.
fn schema_file_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas")
        .join(name)
}

/// The generated stable schema, parsed.
fn v1_schema() -> Value {
    serde_json::to_value(result_v1_json_schema()).expect("a schemars::Schema always serializes")
}

/// The frozen `schemas/result-v1beta1.json`, parsed.
fn frozen_beta_schema() -> Value {
    let text = std::fs::read_to_string(schema_file_path("result-v1beta1.json"))
        .expect("the frozen v1beta1 result schema is checked in");
    serde_json::from_str(&text).expect("the frozen schema is valid JSON")
}

fn properties(schema: &Value) -> Vec<String> {
    schema["properties"]
        .as_object()
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

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
/// `title` and `description` are generated from Rust documentation: what
/// a schema says *about* itself, not the contract a document must
/// satisfy. Stripping them is what lets two versions be compared on shape
/// alone.
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

/// Every `const` string a `oneOf` list of string constants offers — the
/// shape `schemars` emits for a documented, `#[serde(rename)]`d fieldless
/// enum.
fn enum_wire_strings(schema: &Value, name: &str) -> Vec<String> {
    schema["$defs"][name]["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("the schema defines {name} as a oneOf of string constants"))
        .iter()
        .map(|branch| {
            branch["const"]
                .as_str()
                .unwrap_or_else(|| panic!("every {name} branch pins a const wire string"))
                .to_owned()
        })
        .collect()
}

#[test]
fn the_schema_version_is_the_stable_identifier() {
    // Pinned as a literal: the string is a wire value consumers match on,
    // so a typo in the promotion must fail here rather than propagate
    // into every result written from now on.
    assert_eq!(SCHEMA_VERSION, "admissionlab.io/result/v1");
}

/// `docs/schema-migrations.md`'s stable rule, checked against the two
/// artifacts it governs: the frozen `v1beta1` schema is the contract
/// already-written documents were produced under, and the generated `v1`
/// schema is this build's.
///
/// Applied to the root document *and* to every definition both schemas
/// share, because a field can be dropped just as quietly three levels
/// down as at the top.
#[test]
fn the_stable_schema_is_a_backward_compatible_superset_of_v1beta1() {
    let v1 = v1_schema();
    let beta = frozen_beta_schema();

    let check = |node: &str, old: &Value, new: &Value| {
        let new_properties = properties(new);
        for property in properties(old) {
            assert!(
                new_properties.contains(&property),
                "result/v1 dropped or renamed the result/v1beta1 property {property:?} on \
                 {node}; that needs a new result schema version and a migration note in \
                 docs/schema-migrations.md"
            );
        }
        let new_required = required(new);
        for property in required(old) {
            assert!(
                new_required.contains(&property),
                "result/v1 stopped requiring {property:?} on {node}, which result/v1beta1 \
                 required; a requirement dropped is a semantics change, not an addition"
            );
        }
        let old_properties = properties(old);
        for property in &new_properties {
            assert!(
                old_properties.contains(property) || !new_required.contains(property),
                "result/v1 added {property:?} to {node} as a *required* field, which invalidates \
                 every result/v1beta1 document; additions must be optional"
            );
        }
    };

    check("the root document", &beta, &v1);

    let beta_defs = beta["$defs"]
        .as_object()
        .expect("the frozen schema has $defs");
    let v1_defs = v1["$defs"].as_object().expect("the v1 schema has $defs");
    for (name, beta_def) in beta_defs {
        let v1_def = v1_defs.get(name).unwrap_or_else(|| {
            panic!(
                "result/v1 no longer defines {name:?}, which result/v1beta1 published; a \
                 definition that disappears takes every property in it with it"
            )
        });
        check(name, beta_def, v1_def);
    }
}

/// The freeze's actual content: the stable promotion changed the
/// identifier and nothing else.
///
/// This is why there is no result migration to write and nothing for a
/// consumer to do — a `v1beta1` result and a `v1` result are the same
/// document under two names — and it is the assertion that would fail if
/// a field were quietly added on the way to the stable version instead of
/// after it, where the additive rule and a regenerated
/// `schemas/result-v1.json` would put it in front of a reviewer.
#[test]
fn the_stable_schema_has_no_wire_change_from_v1beta1() {
    let mut v1 = v1_schema();
    let mut beta = frozen_beta_schema();
    for schema in [&mut v1, &mut beta] {
        strip_prose(schema);
    }

    assert_eq!(
        v1, beta,
        "result/v1 no longer describes the same document as result/v1beta1. An addition is \
         allowed within v1.x — regenerate schemas/result-v1.json and the golden, and note it in \
         docs/schema-migrations.md — but it must be optional, and this assertion has to be \
         retired deliberately rather than by editing the model"
    );
}

/// The semantic-change vocabulary is part of the frozen surface.
///
/// `admissionlab_diff`'s tests already prove the Rust enum is exhaustive
/// and that each variant serializes to its pinned string. What is frozen
/// *here* is the set those strings form: a consumer's `match` and a
/// `policy.failOn` entry in a user's `admissionlab.yaml` are both written
/// against these literals, so renaming one is a new result schema version
/// and quietly dropping one is a breaking change no schema diff would
/// describe as such.
#[test]
fn the_semantic_change_wire_strings_are_frozen() {
    let schema = v1_schema();

    assert_eq!(
        enum_wire_strings(&schema, "SemanticChangeKind"),
        SEMANTIC_CHANGE_KINDS,
        "a semantic-change wire string changed. Adding a *new* case is allowed within v1.x (new \
         case, new literal); renaming or removing one is not, and needs a new result schema \
         version with a migration note"
    );
    assert_eq!(
        enum_wire_strings(&schema, "MigrationBehaviorKind"),
        MIGRATION_BEHAVIOR_KINDS,
        "a migration-behavior wire string changed; the same rule applies to it"
    );
}

/// Every pinned string really is `snake_case`, which is the convention a
/// `policy.failOn` entry is written in.
///
/// Cheap, and it catches the one mistake the list above cannot: a new
/// entry added here in the Rust identifier's casing, which would then be
/// "frozen" in a spelling no other change kind uses.
#[test]
fn every_change_wire_string_is_snake_case() {
    for kind in SEMANTIC_CHANGE_KINDS
        .iter()
        .chain(&MIGRATION_BEHAVIOR_KINDS)
    {
        assert!(
            kind.chars()
                .all(|character| character.is_ascii_lowercase() || character == '_'),
            "{kind:?} is not snake_case; every change kind on the wire is"
        );
    }
}
