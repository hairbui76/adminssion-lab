//! The run manifest's *versions* (ROADMAP Tasks 7.3 and 9.1).
//!
//! `tests/run_manifest.rs` covers the document — what it may contain, how
//! it hashes, what it looks like on the wire. This file covers the one
//! thing a version bump adds: that there are now three identifiers in the
//! world and this build has to be right about which one it is holding.
//!
//! Four groups:
//!
//! - **The document this build writes.** Round-trips through the
//!   versioned reader, and still refuses an unknown field — promoting a
//!   schema must not quietly relax the strictness the older documents
//!   already had.
//! - **The older documents, still readable.** The checked-in
//!   `testdata/golden/run-manifest-alpha.json` and
//!   `testdata/golden/run-manifest-beta.json` are read back through the
//!   versioned reader with no editing at all, and a hand-stripped alpha
//!   manifest of a real source tree still plans a reproduction. This is
//!   the promise Task 7.3 made and Task 9.1 renews: a run recorded before
//!   a promotion is exactly as reproducible as one recorded after it.
//! - **Refusals.** An unknown `schemaVersion` names every version this
//!   build reads; a v1alpha1 document carrying a v1beta1 field is
//!   refused rather than half-believed.
//! - **The compatibility rule as a test.** The generated v1 schema is
//!   compared against *both* frozen schema files and must be a
//!   backward-compatible superset of each — which is
//!   `docs/schema-migrations.md`'s rule, checked rather than promised.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use admissionlab_core::reproduce::{DEFAULT_LAB_FILE_NAME, plan_reproduction};
use admissionlab_core::run_manifest::{
    ComponentProvenance, EnvironmentProvenance, GatewayProvenance, HostProvenance,
    ManifestReadError, RunManifest, RunStage, RunStatus, SCHEMA_VERSION, SCHEMA_VERSION_V1ALPHA1,
    SCHEMA_VERSION_V1BETA1, SUPPORTED_SCHEMA_VERSIONS, ToolProvenance, file_sha256,
    read_run_manifest, run_manifest_v1_json_schema, sha256_hex,
};
use admissionlab_core::{FixtureId, RunId};

// ---------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------

/// The digest-pinned node image both sides of every manifest here ran.
const NODE_IMAGE: &str = "kindest/node:v1.36.4";
/// That image's recorded content digest.
const NODE_DIGEST: &str = "sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed";

/// Every field name v1beta1 added, in its wire spelling.
///
/// The list a v1alpha1 document must not contain, and the list the
/// superset test expects to find as *optional* additions in the generated
/// schema. Written once so the two cannot disagree about what "what
/// v1beta1 added" means.
const V1BETA1_ADDITIONS: [&str; 2] = ["configApiVersion", "gateway"];

/// A fresh, guaranteed-unique scratch directory, removed by each test
/// that makes one. Mirrors this crate's own `tests/reproduce.rs`.
fn unique_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-core-manifest-beta-{label}-{}",
        RunId::generate().as_str()
    ));
    std::fs::create_dir_all(&dir).expect("failed to create scratch dir");
    dir
}

/// Path to a workspace-root-relative file (this crate lives two levels
/// below the workspace root).
fn workspace_file(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

/// A manifest in the version this build writes, with every field
/// v1beta1 added actually populated.
fn current_manifest() -> RunManifest {
    let environment = || EnvironmentProvenance {
        kubernetes_version: "1.36.4".to_owned(),
        node_image: NODE_IMAGE.to_owned(),
        node_image_digest: Some(NODE_DIGEST.to_owned()),
        images: Some(vec!["admissionlab-echo:dev".to_owned()]),
        components: vec![ComponentProvenance {
            name: "kyverno".to_owned(),
            version: "3.2.6".to_owned(),
            source_sha256: None,
        }],
    };
    let mut fixture_hashes = BTreeMap::new();
    fixture_hashes.insert(
        FixtureId::parse("pods-privileged-0").expect("valid fixture id"),
        sha256_hex(b"apiVersion: v1\nkind: Pod\n"),
    );

    RunManifest {
        schema_version: SCHEMA_VERSION.to_owned(),
        run_id: RunId::generate(),
        admissionlab_version: "0.1.0".to_owned(),
        status: RunStatus::Completed,
        stage: RunStage::Completed,
        host: HostProvenance::detect(),
        tools: ToolProvenance {
            kind: Some("v0.33.0".to_owned()),
            kubectl: Some("v1.36.4".to_owned()),
            helm: Some("v3.16.2".to_owned()),
            docker: Some("27.5.0".to_owned()),
        },
        baseline: environment(),
        candidate: environment(),
        config_api_version: Some("admissionlab.io/v1alpha1".to_owned()),
        config_sha256: sha256_hex(b"config"),
        fixture_hashes,
        expectations_sha256: None,
        normalization_sha256: sha256_hex(b"normalization"),
        policy_sha256: sha256_hex(b"policy"),
        gateway: Some(GatewayProvenance {
            routes: vec!["echo-a-root".to_owned()],
            reconciliation_timeout_millis: 90_000,
            endpoint_strategy: Some("serviceBySelector".to_owned()),
        }),
        started_at: SystemTime::UNIX_EPOCH,
        completed_at: Some(SystemTime::UNIX_EPOCH),
    }
}

/// Renders `manifest` as the v1alpha1 document a pre-Task-7.3 build would
/// have written for the same run: the `schemaVersion` rewritten, and
/// every v1beta1 addition **removed** rather than left as `null`.
///
/// Removal is the point. A v1alpha1 writer had no such fields at all, so
/// a document that merely nulls them is not the historical artifact this
/// file claims to be reading — and `null` would exercise a different
/// serde path (present-but-`None`) from the one a real old manifest takes
/// (absent).
fn as_v1alpha1_document(manifest: &RunManifest) -> String {
    let mut value = serde_json::to_value(manifest).expect("a manifest always serializes");
    let object = value.as_object_mut().expect("a manifest is a JSON object");
    object.insert(
        "schemaVersion".to_owned(),
        serde_json::Value::String(SCHEMA_VERSION_V1ALPHA1.to_owned()),
    );
    for field in V1BETA1_ADDITIONS {
        object.remove(field);
    }
    for side in ["baseline", "candidate"] {
        object[side]
            .as_object_mut()
            .expect("each side is a JSON object")
            .remove("images");
    }
    serde_json::to_string(&value).expect("a JSON value re-encodes")
}

/// Writes a minimal but real lab configuration and the one fixture it
/// selects, and returns the source root.
///
/// Real YAML, for the reason `tests/reproduce.rs` gives at length: a
/// reproduction's subject is the bytes on disk, and a `ResolvedLab`
/// built in memory has none.
fn write_source(dir: &Path) -> PathBuf {
    std::fs::write(
        dir.join(DEFAULT_LAB_FILE_NAME),
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: Lab\n\
         baseline:\n\
         \x20\x20kubernetes: \"1.36.4\"\n\
         \x20\x20components:\n\
         \x20\x20\x20\x20- name: kyverno\n\
         \x20\x20\x20\x20\x20\x20install:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20type: helm\n\
         \x20\x20\x20\x20\x20\x20\x20\x20chart: kyverno/kyverno\n\
         \x20\x20\x20\x20\x20\x20\x20\x20repo: https://kyverno.github.io/kyverno/\n\
         \x20\x20\x20\x20\x20\x20\x20\x20version: \"3.2.6\"\n\
         candidate:\n\
         \x20\x20kubernetes: \"1.36.4\"\n\
         \x20\x20components:\n\
         \x20\x20\x20\x20- name: kyverno\n\
         \x20\x20\x20\x20\x20\x20install:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20type: helm\n\
         \x20\x20\x20\x20\x20\x20\x20\x20chart: kyverno/kyverno\n\
         \x20\x20\x20\x20\x20\x20\x20\x20repo: https://kyverno.github.io/kyverno/\n\
         \x20\x20\x20\x20\x20\x20\x20\x20version: \"3.2.6\"\n\
         fixtures:\n\
         \x20\x20include:\n\
         \x20\x20\x20\x20- \"fixtures/**/*.yaml\"\n",
    )
    .expect("write lab configuration");
    let fixtures = dir.join("fixtures");
    std::fs::create_dir_all(&fixtures).expect("create fixtures dir");
    std::fs::write(
        fixtures.join("pod.yaml"),
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: probe\n",
    )
    .expect("write fixture");
    dir.to_path_buf()
}

// ---------------------------------------------------------------------
// The document this build writes
// ---------------------------------------------------------------------

/// The version this build writes is the stable `run/v1`, and nothing
/// else.
///
/// Pinned as literals rather than compared against other constants: these
/// strings are wire values that consumers match on, so a typo in the
/// promotion must fail here rather than propagate into every manifest
/// written from now on. Note the stable identifier drops the `-manifest`
/// infix the two pre-stable ones carry — that is the deliberate freeze
/// decision `run_manifest.rs` explains, not a typo for this test to
/// "fix".
#[test]
fn the_written_schema_version_is_the_stable_v1() {
    assert_eq!(SCHEMA_VERSION, "admissionlab.io/run/v1");
    assert_eq!(
        SCHEMA_VERSION_V1BETA1,
        "admissionlab.io/run-manifest/v1beta1"
    );
    assert_eq!(
        SCHEMA_VERSION_V1ALPHA1,
        "admissionlab.io/run-manifest/v1alpha1"
    );
    assert_eq!(
        SUPPORTED_SCHEMA_VERSIONS,
        [
            SCHEMA_VERSION,
            SCHEMA_VERSION_V1BETA1,
            SCHEMA_VERSION_V1ALPHA1
        ],
        "all three versions are read; only the newest is written"
    );
}

/// A manifest this build wrote survives serialization and the versioned
/// reader unchanged, every field and all.
#[test]
fn a_v1_manifest_round_trips_through_the_versioned_reader() {
    let manifest = current_manifest();
    let text = serde_json::to_string(&manifest).expect("a manifest always serializes");

    let parsed = read_run_manifest(&text).expect("a v1 manifest reads");
    assert_eq!(parsed, manifest);
    assert_eq!(parsed.schema_version, SCHEMA_VERSION);
}

/// Promotion does not relax strictness: an unknown field is still
/// refused, and the rejection still names it.
#[test]
fn a_v1_manifest_still_rejects_unknown_fields() {
    let text = serde_json::to_string(&current_manifest())
        .expect("a manifest always serializes")
        .replace(
            "{\"schemaVersion\"",
            "{\"somethingNew\":1,\"schemaVersion\"",
        );

    let error = read_run_manifest(&text).expect_err("an unknown field must be rejected");
    assert!(
        matches!(&error, ManifestReadError::Malformed { schema_version, .. }
            if schema_version == SCHEMA_VERSION),
        "{error:?}"
    );
    assert!(
        error.to_string().contains("somethingNew"),
        "the rejection should name the unknown field, got: {error}"
    );
}

// ---------------------------------------------------------------------
// The older documents, still readable
// ---------------------------------------------------------------------

/// A v1beta1 manifest reads in full, with its own version preserved.
///
/// The stable freeze added no field and removed none, so a Beta document
/// *is* a v1 document with a different identifier on it — which is
/// exactly why the identifier is not rewritten on read. A consumer that
/// wants to know which build recorded a run has this field and nothing
/// else to go on.
#[test]
fn a_v1beta1_manifest_still_reads_in_full() {
    let mut manifest = current_manifest();
    manifest.schema_version = SCHEMA_VERSION_V1BETA1.to_owned();
    let text = serde_json::to_string(&manifest).expect("a manifest always serializes");

    let parsed = read_run_manifest(&text).expect("a v1beta1 manifest still reads");
    assert_eq!(parsed, manifest);
    assert_eq!(
        parsed.schema_version, SCHEMA_VERSION_V1BETA1,
        "the document's own version is preserved, never normalized to the one this build writes"
    );
    // Everything v1beta1 added is still read as the value it carries,
    // rather than being blanked because the identifier is older.
    assert!(parsed.gateway.is_some());
    assert!(parsed.config_api_version.is_some());
    assert!(parsed.baseline.images.is_some());
}

/// The checked-in v1beta1 golden manifest — a document this build no
/// longer writes — still reads, byte for byte as it sits in the
/// repository.
///
/// Read from the file rather than from a value built here, for the reason
/// the v1alpha1 test below gives: the claim is about *historical
/// artifacts*, and that file is the closest thing this repository has to a
/// manifest a user actually has in an artifact directory.
#[test]
fn the_checked_in_v1beta1_golden_manifest_still_reads() {
    let path = workspace_file("testdata/golden/run-manifest-beta.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("the frozen v1beta1 golden manifest is missing: {error}"));

    let manifest = read_run_manifest(&text).expect("a v1beta1 manifest still reads");

    assert_eq!(manifest.schema_version, SCHEMA_VERSION_V1BETA1);
    assert_eq!(manifest.status, RunStatus::Completed);
    assert!(
        manifest.config_api_version.is_some(),
        "a v1beta1 manifest records the configuration version it was driven by"
    );
}

/// The checked-in v1alpha1 golden manifest — a document this build no
/// longer writes — is still read, with its own version preserved and
/// every v1beta1 addition honestly absent.
///
/// Read from the file rather than from a value built here, because the
/// claim under test is about *historical artifacts*: that file was
/// generated by the alpha model and has not been touched since, so it is
/// the closest thing this repository has to a manifest a user actually
/// has sitting in an artifact directory.
#[test]
fn the_checked_in_v1alpha1_golden_manifest_still_reads() {
    let path = workspace_file("testdata/golden/run-manifest-alpha.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("the frozen v1alpha1 golden manifest is missing: {error}"));

    let manifest = read_run_manifest(&text).expect("a v1alpha1 manifest still reads");

    // Preserved, never normalized to the version this build writes: it is
    // the only evidence of which of the two meanings the `None`s below
    // carry (`run_manifest`'s "Two schema versions, one Rust type").
    assert_eq!(manifest.schema_version, SCHEMA_VERSION_V1ALPHA1);

    // Honest absence, not an invented empty value (Global Constraint 15).
    assert_eq!(manifest.config_api_version, None);
    assert_eq!(manifest.gateway, None);
    assert_eq!(manifest.baseline.images, None);
    assert_eq!(manifest.candidate.images, None);

    // And everything v1alpha1 *did* record survived the promotion
    // unchanged — the half of the compatibility rule that says existing
    // fields never change meaning.
    assert_eq!(manifest.status, RunStatus::Completed);
    assert_eq!(manifest.baseline.kubernetes_version, "1.36.4");
    assert_eq!(manifest.candidate.components[0].version, "3.3.0");
    assert_eq!(manifest.fixture_hashes.len(), 2);
}

/// `None` for a side's images means "not recorded", and an empty list
/// means "recorded, and there were none" — the two are different claims
/// and the reader keeps them apart.
///
/// Stated as its own test because this is the single most confusable
/// pair in the promotion: both render as "no images" in any summary that
/// does not look closely.
#[test]
fn an_absent_image_list_is_not_an_empty_one() {
    let mut manifest = current_manifest();
    manifest.baseline.images = Some(Vec::new());
    let recorded_none =
        read_run_manifest(&serde_json::to_string(&manifest).expect("a manifest always serializes"))
            .expect("a v1beta1 manifest reads");
    assert_eq!(recorded_none.baseline.images, Some(Vec::new()));

    let unrecorded = read_run_manifest(&as_v1alpha1_document(&manifest))
        .expect("a v1alpha1 manifest still reads");
    assert_eq!(unrecorded.baseline.images, None);
    assert_ne!(unrecorded.baseline.images, recorded_none.baseline.images);
}

/// A v1alpha1 manifest of a real source tree still plans a reproduction.
///
/// The end-to-end version of the promise: read an old document through
/// the versioned reader, hand it to the unchanged `plan_reproduction`,
/// and get a plan with no digest mismatches — no editing, no migration
/// step the user has to run first.
#[test]
fn a_v1alpha1_manifest_still_plans_a_reproduction() {
    let dir = unique_dir("plan");
    let source = write_source(&dir);

    let mut recorded = current_manifest();
    recorded.config_sha256 =
        file_sha256(&source.join(DEFAULT_LAB_FILE_NAME)).expect("hash the configuration");
    recorded.expectations_sha256 = None;
    let document = as_v1alpha1_document(&recorded);

    let manifest = read_run_manifest(&document).expect("a v1alpha1 manifest still reads");
    let plan = plan_reproduction(&manifest, &source).expect("an alpha manifest plans");

    assert_eq!(
        plan.mismatches().count(),
        0,
        "the source is byte-identical to the recorded run, so nothing may mismatch"
    );
    assert_eq!(plan.resolved_lab.baseline.components.len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------

/// An unknown `schemaVersion` is refused, and the message names every
/// version this build does read.
#[test]
fn an_unknown_schema_version_names_the_supported_versions() {
    let text = serde_json::to_string(&current_manifest())
        .expect("a manifest always serializes")
        .replace(SCHEMA_VERSION, "admissionlab.io/run/v2");

    let error = read_run_manifest(&text).expect_err("an unknown schema version is refused");
    assert!(
        matches!(&error, ManifestReadError::UnsupportedSchemaVersion { found, supported }
            if found == "admissionlab.io/run/v2"
                && *supported == SUPPORTED_SCHEMA_VERSIONS),
        "{error:?}"
    );

    let message = error.to_string();
    for version in SUPPORTED_SCHEMA_VERSIONS {
        assert!(
            message.contains(version),
            "the rejection must name {version:?}, got: {message}"
        );
    }
}

/// A document with no `schemaVersion` at all is refused before anything
/// else is judged about it.
#[test]
fn a_document_without_a_schema_version_is_not_a_manifest() {
    let error = read_run_manifest("{\"runId\":\"6f1a7c2e\"}")
        .expect_err("a document with no schemaVersion is refused");
    assert!(
        matches!(&error, ManifestReadError::NotAManifest { .. }),
        "{error:?}"
    );
}

/// A v1alpha1 document carrying a v1beta1-only field is refused, by name.
///
/// No version of Admission Lab ever wrote such a document, so there is no
/// reading of it that is not a guess: believing the field contradicts the
/// version it is filed under, and ignoring it discards a value someone
/// put there deliberately.
#[test]
fn a_v1alpha1_document_carrying_a_v1beta1_field_is_refused() {
    for field in V1BETA1_ADDITIONS {
        let manifest = current_manifest();
        let mut value: serde_json::Value = serde_json::from_str(&as_v1alpha1_document(&manifest))
            .expect("the stripped document is JSON");
        let full = serde_json::to_value(&manifest).expect("a manifest always serializes");
        value[field] = full[field].clone();
        let text = serde_json::to_string(&value).expect("a JSON value re-encodes");

        let error = match read_run_manifest(&text) {
            Ok(accepted) => panic!("{field:?} under v1alpha1 must be refused, got {accepted:?}"),
            Err(error) => error,
        };
        assert!(
            matches!(&error, ManifestReadError::FieldFromNewerVersion { field: named, schema_version, beta }
                if *named == field
                    && schema_version == SCHEMA_VERSION_V1ALPHA1
                    && *beta == SCHEMA_VERSION_V1BETA1),
            "{error:?}"
        );
        assert!(
            error.to_string().contains(field),
            "the rejection must name {field:?}, got: {error}"
        );
    }
}

/// The same refusal for a field nested inside a side's environment, which
/// the top-level check would miss.
#[test]
fn a_v1alpha1_document_carrying_a_v1beta1_side_field_is_refused() {
    let manifest = current_manifest();
    let mut value: serde_json::Value =
        serde_json::from_str(&as_v1alpha1_document(&manifest)).expect("the stripped document");
    value["candidate"]["images"] = serde_json::json!(["admissionlab-echo:dev"]);
    let text = serde_json::to_string(&value).expect("a JSON value re-encodes");

    let error = read_run_manifest(&text).expect_err("images under v1alpha1 must be refused");
    assert!(
        matches!(&error, ManifestReadError::FieldFromNewerVersion { field, schema_version, .. }
            if *field == "images" && schema_version == SCHEMA_VERSION_V1ALPHA1),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------
// The compatibility rule, as a test
// ---------------------------------------------------------------------

/// `docs/schema-migrations.md`'s compatibility rule, checked against the
/// artifacts it governs.
///
/// The frozen `schemas/run-manifest-v1alpha1.json` and
/// `schemas/run-manifest-v1beta1.json` are the previous versions'
/// published contracts; the generated v1 schema is this build's. The rule
/// says a promotion may **add optional fields** and nothing else, so, for
/// each frozen file:
///
/// - every property it declares still exists in v1 (no removal, no
///   rename — either would need a migration note, and there is none);
/// - every requirement it declares is still required (a requirement
///   dropped is a semantics change, even though it looks like a
///   relaxation);
/// - every v1 addition is **optional** (a new required field would make
///   every existing manifest invalid against the new schema).
///
/// This is why a superseded schema file stays checked in with no
/// generator behind it: it is the reference the next promotion is
/// measured against.
#[test]
fn the_v1_schema_is_a_backward_compatible_superset_of_every_frozen_version() {
    for frozen in [
        "schemas/run-manifest-v1alpha1.json",
        "schemas/run-manifest-v1beta1.json",
    ] {
        assert_superset_of_frozen(frozen);
    }
}

/// One frozen schema file's worth of [the rule
/// above](the_v1_schema_is_a_backward_compatible_superset_of_every_frozen_version).
fn assert_superset_of_frozen(frozen: &str) {
    let alpha: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(workspace_file(frozen))
            .unwrap_or_else(|error| panic!("the frozen schema {frozen} is checked in: {error}")),
    )
    .expect("the frozen schema is JSON");
    let beta = serde_json::to_value(run_manifest_v1_json_schema())
        .expect("a schemars::Schema always serializes");

    let properties = |schema: &serde_json::Value| -> Vec<String> {
        schema["properties"]
            .as_object()
            .expect("a schema has properties")
            .keys()
            .cloned()
            .collect()
    };
    let required = |schema: &serde_json::Value| -> Vec<String> {
        schema["required"]
            .as_array()
            .expect("a schema has a required list")
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .expect("a required entry is a string")
                    .to_owned()
            })
            .collect()
    };

    let beta_properties = properties(&beta);
    for property in properties(&alpha) {
        assert!(
            beta_properties.contains(&property),
            "v1 dropped or renamed {property:?}, which {frozen} publishes; that needs a v2 and a \
             migration note in docs/schema-migrations.md"
        );
    }

    let beta_required = required(&beta);
    for property in required(&alpha) {
        assert!(
            beta_required.contains(&property),
            "v1 stopped requiring {property:?}, which {frozen} requires; a requirement dropped \
             is a semantics change, not an addition"
        );
    }

    let alpha_properties = properties(&alpha);
    for property in &beta_properties {
        if alpha_properties.contains(property) {
            continue;
        }
        assert!(
            !beta_required.contains(property),
            "v1 added {property:?} as a *required* field, which invalidates every manifest \
             written against {frozen}; additions must be optional"
        );
        assert!(
            V1BETA1_ADDITIONS.contains(&property.as_str()),
            "the top-level property {property:?} is not in {frozen} and this test does not know \
             about it; add it to V1BETA1_ADDITIONS and re-read run_manifest.rs's \"What this \
             document may never contain\" section first"
        );
    }
}

/// The same rule one level down, for the side environment that also
/// gained a field.
#[test]
fn the_v1_environment_schema_is_a_backward_compatible_superset() {
    let alpha: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(workspace_file("schemas/run-manifest-v1alpha1.json"))
            .expect("the frozen v1alpha1 schema is checked in"),
    )
    .expect("the frozen schema is JSON");
    let beta = serde_json::to_value(run_manifest_v1_json_schema())
        .expect("a schemars::Schema always serializes");

    let alpha_environment = &alpha["$defs"]["EnvironmentProvenance"];
    let beta_environment = &beta["$defs"]["EnvironmentProvenance"];

    for (name, _) in alpha_environment["properties"]
        .as_object()
        .expect("the alpha environment has properties")
    {
        assert!(
            beta_environment["properties"].get(name).is_some(),
            "v1 dropped or renamed the v1alpha1 environment property {name:?}"
        );
    }
    assert!(
        beta_environment["properties"].get("images").is_some(),
        "v1 keeps the images v1beta1 added to each side's environment"
    );
    let beta_required: Vec<&str> = beta_environment["required"]
        .as_array()
        .expect("the beta environment has a required list")
        .iter()
        .map(|value| value.as_str().expect("a required entry is a string"))
        .collect();
    assert!(
        !beta_required.contains(&"images"),
        "images must be optional: every v1alpha1 manifest lacks it"
    );
}
