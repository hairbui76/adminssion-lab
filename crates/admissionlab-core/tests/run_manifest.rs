//! Behavioral tests for the versioned run manifest (Task 5.1).
//!
//! Four groups, in the order the module's own documentation makes its
//! promises:
//!
//! - **Credential safety** (Global Constraint 14). The manifest is built
//!   from inputs that sit right next to real kubeconfig paths and
//!   certificate material, and the serialized document is searched for
//!   every one of them. A second test freezes the top-level key set, so a
//!   future field that *could* carry a path fails a test rather than
//!   quietly shipping.
//! - **Digests.** Known SHA-256 vectors (so "digest-shaped" cannot pass
//!   for "is SHA-256"), plus the two properties the canonical encoding
//!   exists to provide: object-key order does not depend on Rust field
//!   declaration order, and array order *does* matter.
//! - **The pinned wire form.** RFC 3339 with exactly nine fractional
//!   digits, `null` for an unfinished run, string map keys in identifier
//!   order, and a full round-trip through `serde` that also proves
//!   identifier validation survives deserialization.
//! - **The checked-in artifacts.** `schemas/run-manifest-v1alpha1.json`
//!   and `testdata/golden/run-manifest-alpha.json` are regenerated and
//!   compared byte-for-byte, with an `#[ignore]`d regenerator alongside
//!   each — the pattern `admissionlab-spec`'s `tests/schema.rs`
//!   established, so the generator and the checker can never drift from
//!   each other by construction.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use admissionlab_core::run_manifest::{
    ComponentProvenance, EffectiveNormalization, EnvironmentProvenance, HostProvenance,
    NormalizationRuleRecord, RunManifest, SCHEMA_VERSION, ToolProvenance, canonical_sha256,
    normalization_sha256, policy_sha256, run_manifest_v1alpha1_json_schema, sha256_hex,
    split_node_image_reference,
};
use admissionlab_core::{DoctorReport, FixtureId, RunId, ToolName, ToolStatus};

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

/// A fixed instant with a non-zero nanosecond part, so the pinned
/// nine-digit fractional encoding is actually exercised rather than
/// coincidentally satisfied by a whole second.
fn started() -> SystemTime {
    // 2026-09-01T12:00:00.123456789Z
    SystemTime::UNIX_EPOCH + Duration::new(1_788_264_000, 123_456_789)
}

/// The same run, seventeen and a half minutes later.
fn completed() -> SystemTime {
    started() + Duration::from_secs(1_050)
}

/// A realistic, fully-populated manifest — the value both the golden file
/// and the credential-safety test are built from.
///
/// Every string here is one a real run would produce: node images from
/// `compatibility/kubernetes.yaml`, tool versions in the exact verbatim
/// form each probe prints, and a candidate stack one chart version ahead
/// of the baseline (which is the whole point of a comparison run).
fn realistic_manifest() -> RunManifest {
    let mut fixture_hashes = BTreeMap::new();
    // Inserted out of order deliberately: the serialized document must
    // come out in identifier order regardless.
    fixture_hashes.insert(
        FixtureId::parse("pods-privileged-0").expect("valid fixture id"),
        sha256_hex(b"apiVersion: v1\nkind: Pod\nmetadata:\n  name: privileged\n"),
    );
    fixture_hashes.insert(
        FixtureId::parse("deployments-web-0").expect("valid fixture id"),
        sha256_hex(b"apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n"),
    );

    RunManifest {
        schema_version: SCHEMA_VERSION.to_owned(),
        run_id: RunId::parse("6f1a7c2e-9b34-4d5a-8f0e-11c2b3a4d5e6").expect("valid run id"),
        admissionlab_version: "0.1.0".to_owned(),
        host: HostProvenance {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
        },
        tools: ToolProvenance {
            kind: Some("v0.33.0".to_owned()),
            kubectl: Some("v1.36.4".to_owned()),
            helm: Some("v3.16.2".to_owned()),
            docker: Some("27.5.0".to_owned()),
        },
        baseline: EnvironmentProvenance {
            kubernetes_version: "1.36.4".to_owned(),
            node_image: "kindest/node:v1.36.4".to_owned(),
            node_image_digest: Some(
                "sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed"
                    .to_owned(),
            ),
            components: vec![ComponentProvenance {
                name: "kyverno".to_owned(),
                version: "3.2.6".to_owned(),
                source_sha256: None,
            }],
        },
        candidate: EnvironmentProvenance {
            kubernetes_version: "1.36.4".to_owned(),
            node_image: "kindest/node:v1.36.4".to_owned(),
            node_image_digest: Some(
                "sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed"
                    .to_owned(),
            ),
            components: vec![ComponentProvenance {
                name: "kyverno".to_owned(),
                version: "3.3.0".to_owned(),
                source_sha256: None,
            }],
        },
        config_sha256: sha256_hex(b"apiVersion: admissionlab.io/v1alpha1\nkind: Lab\n"),
        fixture_hashes,
        expectations_sha256: Some(sha256_hex(b"expectations: []\n")),
        normalization_sha256: normalization_sha256(&sample_normalization()),
        policy_sha256: policy_sha256(&admissionlab_spec::PolicySpec::default()),
        started_at: started(),
        completed_at: Some(completed()),
    }
}

/// A small effective normalization profile with rules in every tier.
fn sample_normalization() -> EffectiveNormalization {
    EffectiveNormalization {
        built_in: vec![
            NormalizationRuleRecord::RemovePointer {
                pointer: "/metadata/uid".to_owned(),
            },
            NormalizationRuleRecord::RemoveAnnotation {
                annotation: "kubectl.kubernetes.io/last-applied-configuration".to_owned(),
            },
        ],
        recipe: vec![NormalizationRuleRecord::SortNamedArray {
            pointer: "/spec/containers".to_owned(),
            key: "name".to_owned(),
        }],
        user: Vec::new(),
    }
}

/// Renders a manifest exactly as it is written to `run.json` and checked
/// in as a golden file: pretty-printed JSON with one trailing newline.
fn render_manifest(manifest: &RunManifest) -> String {
    let mut text = serde_json::to_string_pretty(manifest).expect("a manifest always serializes");
    text.push('\n');
    text
}

/// Path to a workspace-root-relative file (this crate lives two levels
/// below the workspace root).
fn workspace_file(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

// ---------------------------------------------------------------------
// Global Constraint 14: no secrets, no kubeconfigs
// ---------------------------------------------------------------------

/// The whole of Global Constraint 14 for this document, stated as a
/// search over the bytes actually written.
///
/// The needles are not arbitrary: they are the exact strings a real run
/// has sitting one struct away from the values this manifest is built
/// from. `ClusterHandle` carries a kubeconfig path and an audit-log path;
/// a kubeconfig's own contents carry `certificate-authority-data`,
/// `client-key-data`, and a bearer `token`; a private key is delimited by
/// `-----BEGIN`. If any of them ever reaches `run.json`, this fails.
///
/// Mutation check: this test would catch a manifest that gained a
/// `kubeconfig`, `run_root`, or `audit_log` field, which is the realistic
/// way this constraint gets broken — not by someone writing a secret into
/// a version string.
#[test]
fn manifest_from_realistic_inputs_leaks_no_credential_material() {
    let rendered = render_manifest(&realistic_manifest());

    for needle in [
        "kubeconfig",
        "certificate-authority-data",
        "client-certificate-data",
        "client-key-data",
        "-----BEGIN",
        "BEARER",
        "Authorization",
        "audit.log",
        "/home/",
        "/tmp/",
    ] {
        assert!(
            !rendered.contains(needle),
            "run manifest must never contain {needle:?}; it appeared in:\n{rendered}"
        );
    }
}

/// Freezes the manifest's top-level key set.
///
/// Paired with the needle search above rather than redundant with it: the
/// search proves today's fields carry nothing sensitive, and this proves
/// a *new* field cannot appear without someone deliberately updating this
/// list — at which point the module documentation's "no path, no
/// environment, no captured output" rule is in front of them.
#[test]
fn top_level_keys_are_exactly_the_frozen_set() {
    let value: serde_json::Value =
        serde_json::to_value(realistic_manifest()).expect("a manifest always serializes");
    let object = value.as_object().expect("a manifest is a JSON object");
    let keys: Vec<&str> = object.keys().map(String::as_str).collect();

    assert_eq!(
        keys,
        [
            "admissionlabVersion",
            "baseline",
            "candidate",
            "completedAt",
            "configSha256",
            "expectationsSha256",
            "fixtureHashes",
            "host",
            "normalizationSha256",
            "policySha256",
            "runId",
            "schemaVersion",
            "startedAt",
            "tools",
        ],
        "the run manifest's top-level keys changed; re-read `run_manifest.rs`'s \
         \"What this document may never contain\" section before updating this list"
    );
}

/// No field anywhere in the document (not only at the top level) is a
/// filesystem path.
///
/// Walks every leaf string in the serialized manifest and rejects
/// anything that looks like an absolute path. Broader than the needle
/// list above, and the reason both exist: the needle list catches known
/// secrets, this catches an unknown one that arrives *as* a path.
#[test]
fn no_leaf_value_anywhere_is_an_absolute_path() {
    let value: serde_json::Value =
        serde_json::to_value(realistic_manifest()).expect("a manifest always serializes");

    let mut stack = vec![value];
    while let Some(current) = stack.pop() {
        match current {
            serde_json::Value::String(text) => assert!(
                !text.starts_with('/') && !text.contains(":\\"),
                "run manifest leaf value {text:?} looks like a filesystem path"
            ),
            serde_json::Value::Array(items) => stack.extend(items),
            serde_json::Value::Object(fields) => stack.extend(fields.into_values()),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------
// Digests
// ---------------------------------------------------------------------

/// Pins two published SHA-256 vectors, so an implementation that returned
/// something merely digest-shaped (a fixed 64-character string, or a
/// different hash function) fails here rather than passing every other
/// test in this file.
#[test]
fn sha256_hex_matches_known_vectors() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// The canonical encoding's central promise: object-key order is
/// lexicographic, so two types that describe the same document with their
/// fields declared in *opposite* order hash identically.
///
/// This is what makes a digest survive a future field reordering in
/// `admissionlab-spec` or in this crate. Serializing a struct directly
/// (rather than through `serde_json::Value`) would fail this test.
#[test]
fn canonical_digest_does_not_depend_on_field_declaration_order() {
    #[derive(serde::Serialize)]
    struct Forward {
        alpha: u32,
        beta: &'static str,
    }
    #[derive(serde::Serialize)]
    struct Reversed {
        beta: &'static str,
        alpha: u32,
    }

    let forward = canonical_sha256(&Forward {
        alpha: 7,
        beta: "seven",
    })
    .expect("plain data serializes");
    let reversed = canonical_sha256(&Reversed {
        beta: "seven",
        alpha: 7,
    })
    .expect("plain data serializes");

    assert_eq!(forward, reversed);
}

/// The complementary promise: array order is *preserved*, because a
/// normalization profile's rules apply in order and reordering them can
/// change what normalization produces.
#[test]
fn normalization_digest_changes_when_rule_order_changes() {
    let profile = sample_normalization();
    let mut reordered = profile.clone();
    reordered.built_in.reverse();

    assert_ne!(
        normalization_sha256(&profile),
        normalization_sha256(&reordered),
        "reordering rules within a tier must change the digest: order is meaning"
    );
    assert_eq!(
        normalization_sha256(&profile),
        normalization_sha256(&profile.clone()),
        "the same profile must hash identically every time"
    );
}

/// A rule moved between tiers is a different profile, even though the
/// same rules are present — the tiers apply in a fixed order, so which
/// tier a rule sits in changes the result.
#[test]
fn normalization_digest_distinguishes_tiers() {
    let mut recipe_tier = EffectiveNormalization {
        built_in: Vec::new(),
        recipe: vec![NormalizationRuleRecord::RemovePointer {
            pointer: "/status".to_owned(),
        }],
        user: Vec::new(),
    };
    let user_tier = EffectiveNormalization {
        built_in: Vec::new(),
        recipe: Vec::new(),
        user: recipe_tier.recipe.clone(),
    };
    assert_ne!(
        normalization_sha256(&recipe_tier),
        normalization_sha256(&user_tier)
    );

    // And an empty profile is not the same as one with a rule.
    recipe_tier.recipe.clear();
    assert_ne!(
        normalization_sha256(&recipe_tier),
        normalization_sha256(&user_tier)
    );
}

/// The policy digest tracks the *effective* policy: two policies that
/// differ only in a latency threshold hash differently, and the same
/// policy hashes identically every time.
#[test]
fn policy_digest_tracks_effective_policy() {
    let defaults = admissionlab_spec::PolicySpec::default();
    assert_eq!(policy_sha256(&defaults), policy_sha256(&defaults));

    let mut stricter = admissionlab_spec::PolicySpec::default();
    stricter.latency.absolute_increase = std::time::Duration::from_millis(25);
    assert_ne!(policy_sha256(&defaults), policy_sha256(&stricter));

    let mut fails_on_more = admissionlab_spec::PolicySpec::default();
    fails_on_more.fail_on.insert("newly_denied".to_owned());
    assert_ne!(policy_sha256(&defaults), policy_sha256(&fails_on_more));
}

/// `fail_on` is a set, so the order the same names were inserted in must
/// not reach the digest.
#[test]
fn policy_digest_ignores_fail_on_insertion_order() {
    let mut one = admissionlab_spec::PolicySpec::default();
    one.fail_on.insert("newly_denied".to_owned());
    one.fail_on.insert("container_removed".to_owned());

    let mut other = admissionlab_spec::PolicySpec::default();
    other.fail_on.insert("container_removed".to_owned());
    other.fail_on.insert("newly_denied".to_owned());

    assert_eq!(policy_sha256(&one), policy_sha256(&other));
}

// ---------------------------------------------------------------------
// Node image references
// ---------------------------------------------------------------------

/// Splitting a digest-pinned reference, and the honest `None` for one
/// that carries no digest (Global Constraint 15 — never a fabricated or
/// empty digest).
#[test]
fn node_image_reference_splits_into_image_and_optional_digest() {
    assert_eq!(
        split_node_image_reference("kindest/node:v1.36.4@sha256:099e0493"),
        (
            "kindest/node:v1.36.4".to_owned(),
            Some("sha256:099e0493".to_owned())
        )
    );
    assert_eq!(
        split_node_image_reference("kindest/node:v1.36.4"),
        ("kindest/node:v1.36.4".to_owned(), None)
    );
    // A trailing `@` with nothing after it is not a digest.
    assert_eq!(
        split_node_image_reference("kindest/node:v1.36.4@"),
        ("kindest/node:v1.36.4".to_owned(), None)
    );
}

// ---------------------------------------------------------------------
// Tool provenance
// ---------------------------------------------------------------------

/// A tool that was found but whose version could not be read is recorded
/// as `null`, never as an empty or invented string (Global Constraint
/// 15).
#[test]
fn tool_provenance_records_unreadable_versions_as_none() {
    let report = DoctorReport {
        tools: vec![
            ToolStatus {
                name: ToolName::Kind,
                found: true,
                version: Some("v0.33.0".to_owned()),
                diagnostic: None,
            },
            ToolStatus {
                name: ToolName::Kubectl,
                found: true,
                version: None,
                diagnostic: Some("could not parse output".to_owned()),
            },
            // `helm` is absent from the report entirely.
            ToolStatus {
                name: ToolName::Docker,
                found: false,
                version: None,
                diagnostic: Some("not on PATH".to_owned()),
            },
        ],
        docker_reachable: false,
        disk_warning: None,
    };

    let tools = ToolProvenance::from_doctor_report(&report);
    assert_eq!(tools.kind.as_deref(), Some("v0.33.0"));
    assert_eq!(tools.kubectl, None);
    assert_eq!(tools.helm, None);
    assert_eq!(tools.docker, None);
}

// ---------------------------------------------------------------------
// The pinned wire form
// ---------------------------------------------------------------------

/// Timestamps are RFC 3339 in UTC with exactly nine fractional digits,
/// and an unfinished run's `completedAt` is JSON `null`.
///
/// The fixed precision is the part worth pinning: jiff's own `Display`
/// trims trailing zeroes, so without it a manifest recorded on a whole
/// second would render `...T12:00:00Z` and one recorded a nanosecond
/// later would render `...T12:00:00.000000001Z` — a golden file that only
/// sometimes matches.
#[test]
fn timestamps_are_rfc3339_with_fixed_nanosecond_precision() {
    let mut manifest = realistic_manifest();
    let value: serde_json::Value =
        serde_json::to_value(&manifest).expect("a manifest always serializes");
    assert_eq!(value["startedAt"], "2026-09-01T12:00:00.123456789Z");
    assert_eq!(value["completedAt"], "2026-09-01T12:17:30.123456789Z");

    // A whole second still renders nine digits, not zero. Built by
    // subtracting the fractional part from `started()` rather than as a
    // fresh `Duration::from_secs`, so the two assertions above and below
    // are visibly the same instant to the nanosecond.
    manifest.started_at = started() - Duration::from_nanos(123_456_789);
    manifest.completed_at = None;
    let value: serde_json::Value =
        serde_json::to_value(&manifest).expect("a manifest always serializes");
    assert_eq!(value["startedAt"], "2026-09-01T12:00:00.000000000Z");
    assert_eq!(value["completedAt"], serde_json::Value::Null);
}

/// Fixture hashes are a JSON object with plain string keys, written in
/// identifier order rather than the order they were inserted.
#[test]
fn fixture_hashes_are_string_keyed_and_identifier_ordered() {
    let value: serde_json::Value =
        serde_json::to_value(realistic_manifest()).expect("a manifest always serializes");
    let hashes = value["fixtureHashes"]
        .as_object()
        .expect("fixtureHashes is a JSON object");

    let keys: Vec<&str> = hashes.keys().map(String::as_str).collect();
    assert_eq!(keys, ["deployments-web-0", "pods-privileged-0"]);
    for hash in hashes.values() {
        let hash = hash.as_str().expect("each hash is a string");
        assert_eq!(hash.len(), 64, "{hash:?} is not a SHA-256 hex digest");
    }
}

/// A manifest survives a full serialize/deserialize round trip
/// unchanged — the property Task 5.3's `reproduce` depends on, since it
/// reads back exactly what a run wrote.
#[test]
fn manifest_round_trips_through_json() {
    let manifest = realistic_manifest();
    let text = serde_json::to_string(&manifest).expect("a manifest always serializes");
    let parsed: RunManifest = serde_json::from_str(&text).expect("a manifest round-trips");
    assert_eq!(parsed, manifest);
}

/// Deserialization runs the identifier's own validation: a hand-edited
/// `run.json` cannot smuggle a path separator into a `runId` that this
/// crate promises is always safe to use as a path segment.
#[test]
fn deserializing_rejects_an_unsafe_run_id() {
    let manifest = realistic_manifest();
    let text = serde_json::to_string(&manifest)
        .expect("a manifest always serializes")
        .replace("6f1a7c2e-9b34-4d5a-8f0e-11c2b3a4d5e6", "../../etc/passwd-0");

    let error = serde_json::from_str::<RunManifest>(&text)
        .expect_err("a run id containing path separators must be rejected");
    assert!(
        error.to_string().contains('/'),
        "the rejection should name the offending character, got: {error}"
    );
}

/// An unknown field is rejected rather than silently ignored: a manifest
/// written by a newer Admission Lab is not something an older one may
/// pretend to understand.
#[test]
fn deserializing_rejects_unknown_fields() {
    let text = serde_json::to_string(&realistic_manifest())
        .expect("a manifest always serializes")
        .replace(
            "{\"schemaVersion\"",
            "{\"somethingNew\":1,\"schemaVersion\"",
        );

    let error =
        serde_json::from_str::<RunManifest>(&text).expect_err("an unknown field must be rejected");
    assert!(
        error.to_string().contains("somethingNew"),
        "the rejection should name the unknown field, got: {error}"
    );
}

// ---------------------------------------------------------------------
// Checked-in artifacts
// ---------------------------------------------------------------------

/// Renders the schema in the exact byte-for-byte form checked in at
/// `schemas/run-manifest-v1alpha1.json`. Shared by the checker and the
/// regenerator so the two can never disagree with each other.
fn render_schema() -> String {
    let schema = run_manifest_v1alpha1_json_schema();
    let mut text =
        serde_json::to_string_pretty(&schema).expect("a schemars::Schema always serializes");
    text.push('\n');
    text
}

#[test]
fn schema_matches_checked_in_file() {
    let path = workspace_file("schemas/run-manifest-v1alpha1.json");
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "checked-in schema missing at {path:?} ({error}); generate it with \
             `cargo test -p admissionlab-core --test run_manifest -- --ignored regenerate_schema_file`"
        )
    });
    assert_eq!(
        render_schema(),
        expected,
        "generated schema no longer matches schemas/run-manifest-v1alpha1.json; regenerate it \
         with `cargo test -p admissionlab-core --test run_manifest -- --ignored regenerate_schema_file`"
    );
}

#[test]
fn schema_generation_is_deterministic_across_runs() {
    assert_eq!(render_schema(), render_schema());
}

#[test]
#[ignore = "run explicitly to (re)write schemas/run-manifest-v1alpha1.json after a deliberate model change"]
fn regenerate_schema_file() {
    std::fs::write(
        workspace_file("schemas/run-manifest-v1alpha1.json"),
        render_schema(),
    )
    .expect("write schema file");
}

/// The golden example is a *documentation* artifact as much as a test
/// one: it is the concrete manifest a reader looks at to understand the
/// shape, so it must always be exactly what this crate currently writes.
#[test]
fn golden_manifest_matches_checked_in_file() {
    let path = workspace_file("testdata/golden/run-manifest-alpha.json");
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "checked-in golden manifest missing at {path:?} ({error}); generate it with \
             `cargo test -p admissionlab-core --test run_manifest -- --ignored regenerate_golden_manifest`"
        )
    });
    assert_eq!(
        render_manifest(&realistic_manifest()),
        expected,
        "the run manifest's rendered form changed; regenerate the golden example with \
         `cargo test -p admissionlab-core --test run_manifest -- --ignored regenerate_golden_manifest`"
    );
}

#[test]
#[ignore = "run explicitly to (re)write testdata/golden/run-manifest-alpha.json after a deliberate model change"]
fn regenerate_golden_manifest() {
    std::fs::write(
        workspace_file("testdata/golden/run-manifest-alpha.json"),
        render_manifest(&realistic_manifest()),
    )
    .expect("write golden manifest");
}
