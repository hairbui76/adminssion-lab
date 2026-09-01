//! The run manifest carries **hashes and metadata, never inputs**
//! (ROADMAP Task 9.3 step 4; Global Constraint 14).
//!
//! `tests/run_manifest.rs` already searches a rendered manifest for known
//! credential needles and freezes its top-level key set. This file is the
//! part that neither of those covers, and it is written from the opposite
//! direction: instead of asserting that a hand-built manifest happens to
//! be clean, it builds one out of inputs that **do** carry secrets — a
//! configuration file holding a password, a fixture that is a Secret, a
//! host probe whose diagnostics quote paths under the operator's home —
//! writes it through the real [`RunManifestWriter`] into a real
//! workspace, and searches the bytes that landed on disk.
//!
//! Three claims, one test each:
//!
//! - [`the_manifest_on_disk_contains_no_input_only_its_digests`]: none of
//!   the planted secrets reach `run.json`, and — the other half, which
//!   makes the first meaningful rather than merely true of an empty file
//!   — the digest of every one of those inputs *is* there. A manifest
//!   that dropped `configSha256` would be as broken as one that inlined
//!   the config; provenance is the point of the document.
//! - [`no_field_anywhere_in_the_schema_is_shaped_like_a_path_or_a_secret`]:
//!   a walk over the generated JSON Schema, so a field added to a
//!   *nested* type (`GatewayProvenance`, `ComponentProvenance`) is covered
//!   too. The frozen-key test in `run_manifest.rs` covers the top level
//!   only.
//! - [`the_workspace_path_the_manifest_was_written_into_is_not_in_it`]:
//!   the run root here is a directory whose path looks like a home
//!   directory, so a writer that recorded where it wrote fails.
//!
//! # Why a search at all, when the type system already forbids it
//!
//! `run_manifest.rs`'s "What this document may never contain" section
//! makes the guarantee structural: **no type in that module holds a
//! `PathBuf`, an environment map, a captured stdout/stderr, or any
//! cluster-connection material.** Every field is a version string, an
//! image reference, an identifier, a SHA-256 digest, or a timestamp. That
//! is the real defence, and it is stronger than any search.
//!
//! The searches exist for the two things a type cannot say: that a
//! *`String`* field was not handed a path by its caller (nothing stops
//! `ComponentProvenance::version` from being given one), and that the
//! rule survives the next field. The schema walk is the one that catches
//! tomorrow's `PathBuf` — it fails on the field's *name* before anyone
//! notices the type.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use admissionlab_core::run_manifest::{
    ComponentProvenance, EnvironmentProvenance, GatewayProvenance, HostProvenance, RunManifest,
    RunManifestWriter, RunStage, RunStatus, SCHEMA_VERSION, ToolProvenance,
    run_manifest_v1_json_schema, sha256_hex,
};
use admissionlab_core::{ArtifactStore, DoctorReport, FixtureId, RunId, ToolName, ToolStatus};

// ---------------------------------------------------------------------
// Inputs that carry secrets
// ---------------------------------------------------------------------

/// The operator's home directory, as it appears in every path below.
const HOME_SENTINEL: &str = "/home/sentinel-operator-x1";

/// An `admissionlab.yaml` that holds a credential and a kubeconfig path.
/// Realistic: a values file inlined into a config is exactly how a
/// password ends up next to a run's inputs.
fn config_text() -> String {
    format!(
        "apiVersion: admissionlab.io/v1alpha1\nkind: Lab\n\
         # kubeconfig: {HOME_SENTINEL}/.kube/config\n\
         baseline:\n  components:\n    - name: registry\n      values:\n        \
         password: sentinel-config-password-x2\n"
    )
}

/// A fixture that *is* a Secret — the strongest case for "the manifest
/// records a hash of this, never its bytes".
fn fixture_text() -> &'static str {
    "apiVersion: v1\nkind: Secret\nmetadata:\n  name: registry-pull\n\
     stringData:\n  token: sentinel-fixture-token-x3\n"
}

/// A PEM private key of the shape a Gateway suite generates and writes
/// into the run workspace next to `run.json`.
fn private_key_pem() -> &'static str {
    "-----BEGIN PRIVATE KEY-----\nsentinel-manifest-private-key-x4\n-----END PRIVATE KEY-----\n"
}

/// A host probe whose free-text diagnostics quote absolute paths.
///
/// [`ToolStatus::diagnostic`] is unconstrained text a probe writes, and
/// [`ToolProvenance::from_doctor_report`] must take the *versions* out of
/// this and nothing else. A future refactor that carried the diagnostic
/// along "for context" would put a home-directory path into every
/// manifest, and that is what this input is here to catch.
fn doctor_report() -> DoctorReport {
    let status = |name: ToolName, version: &str| ToolStatus {
        name,
        found: true,
        version: Some(version.to_owned()),
        diagnostic: Some(format!(
            "resolved from {HOME_SENTINEL}/.local/bin, kubeconfig at \
             {HOME_SENTINEL}/.kube/config"
        )),
    };
    DoctorReport {
        tools: vec![
            status(ToolName::Kind, "v0.33.0"),
            status(ToolName::Kubectl, "v1.36.4"),
            status(ToolName::Helm, "v3.16.2"),
            status(ToolName::Docker, "27.5.0"),
        ],
        docker_reachable: true,
        disk_warning: Some(format!("only 4 GiB free on {HOME_SENTINEL}")),
    }
}

/// Every string that must never appear in a manifest, planted in the
/// inputs above.
fn needles() -> Vec<String> {
    vec![
        HOME_SENTINEL.to_owned(),
        "sentinel-config-password-x2".to_owned(),
        "sentinel-fixture-token-x3".to_owned(),
        "sentinel-manifest-private-key-x4".to_owned(),
        "-----BEGIN".to_owned(),
        "client-key-data".to_owned(),
        "certificate-authority-data".to_owned(),
        ".kube/config".to_owned(),
        "kubeconfig".to_owned(),
        "Authorization".to_owned(),
    ]
}

/// A manifest built from exactly those inputs, the way a run builds one.
///
/// The only channel any of the inputs has into this document is a digest,
/// which is the claim under test.
fn manifest_from_secret_bearing_inputs() -> RunManifest {
    let mut fixture_hashes = BTreeMap::new();
    fixture_hashes.insert(
        FixtureId::parse("secrets-registry-pull-0").expect("valid fixture id"),
        sha256_hex(fixture_text().as_bytes()),
    );

    let side = |chart: &str| EnvironmentProvenance {
        kubernetes_version: "1.36.4".to_owned(),
        node_image: "kindest/node:v1.36.4".to_owned(),
        node_image_digest: Some(
            "sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed".to_owned(),
        ),
        images: Some(vec!["admissionlab-echo:dev".to_owned()]),
        components: vec![ComponentProvenance {
            name: "kyverno".to_owned(),
            version: chart.to_owned(),
            // The private key generated for this run's TLS listener is
            // hashed, exactly like every other input: a digest is
            // provenance, the key is not.
            source_sha256: Some(sha256_hex(private_key_pem().as_bytes())),
        }],
    };

    RunManifest {
        schema_version: SCHEMA_VERSION.to_owned(),
        run_id: RunId::parse("2b8f6c1d-4e5a-4b7c-9d0e-3f1a2b3c4d5e").expect("valid run id"),
        admissionlab_version: "1.0.0".to_owned(),
        status: RunStatus::InProgress,
        stage: RunStage::Started,
        host: HostProvenance::detect(),
        tools: ToolProvenance::from_doctor_report(&doctor_report()),
        baseline: side("3.2.6"),
        candidate: side("3.3.0"),
        config_api_version: Some("admissionlab.io/v1alpha1".to_owned()),
        config_sha256: sha256_hex(config_text().as_bytes()),
        fixture_hashes,
        expectations_sha256: Some(sha256_hex(b"expectations: []\n")),
        normalization_sha256: sha256_hex(b"{}"),
        policy_sha256: sha256_hex(b"{}"),
        gateway: Some(GatewayProvenance {
            routes: vec!["echo-a-root".to_owned()],
            reconciliation_timeout_millis: 90_000,
            endpoint_strategy: Some("serviceBySelector".to_owned()),
        }),
        started_at: SystemTime::UNIX_EPOCH + Duration::new(1_788_264_000, 123_456_789),
        completed_at: None,
    }
}

// ---------------------------------------------------------------------
// Writing it the way a run writes it
// ---------------------------------------------------------------------

/// A hand-rolled current-thread runtime, mirroring this crate's other
/// filesystem tests rather than adding a dependency for one call.
fn test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build test tokio runtime")
}

/// A scratch run root whose path is shaped like a home directory, so a
/// manifest that recorded where it was written is caught by the same
/// search as one that recorded a kubeconfig.
fn home_shaped_run_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "admissionlab-manifest-security-{}-{label}{HOME_SENTINEL}/.cache/admissionlab",
        std::process::id()
    ))
}

/// Writes `manifest` through the real writer and returns the bytes that
/// reached disk, cleaning the workspace up afterwards.
fn written_manifest_bytes(manifest: RunManifest, label: &str) -> String {
    let root = home_shaped_run_root(label);
    let store = ArtifactStore::new(&root);
    let run_id = manifest.run_id.clone();

    let text = test_runtime().block_on(async {
        let paths = store
            .create_run(&run_id)
            .await
            .expect("create run workspace");
        let mut writer = RunManifestWriter::create(store.clone(), &paths, manifest)
            .await
            .expect("write run.json");
        // Two more writes, because a run rewrites the manifest at every
        // stage and this test should cover the file as it finally stands.
        writer
            .record(RunStage::Installation, |_| {})
            .await
            .expect("record a stage");
        writer
            .complete(SystemTime::UNIX_EPOCH + Duration::new(1_788_265_050, 0))
            .await
            .expect("complete the run");
        tokio::fs::read_to_string(writer.path())
            .await
            .expect("read run.json back")
    });

    // Self-cleaning: the assertions run on the text, not the workspace.
    let _ = std::fs::remove_dir_all(first_component_of(&root));
    text
}

/// The topmost directory this test created under the system temp dir, so
/// cleanup removes the whole tree rather than only its deepest leaf.
fn first_component_of(root: &Path) -> PathBuf {
    let temp = std::env::temp_dir();
    root.strip_prefix(&temp)
        .ok()
        .and_then(|relative| relative.components().next())
        .map_or_else(|| root.to_path_buf(), |first| temp.join(first))
}

// ---------------------------------------------------------------------
// The proof
// ---------------------------------------------------------------------

#[test]
fn the_manifest_on_disk_contains_no_input_only_its_digests() {
    let manifest = manifest_from_secret_bearing_inputs();
    let expected_digests = [
        sha256_hex(config_text().as_bytes()),
        sha256_hex(fixture_text().as_bytes()),
        sha256_hex(private_key_pem().as_bytes()),
    ];
    let written = written_manifest_bytes(manifest, "digests");

    for needle in needles() {
        assert!(
            !written.contains(&needle),
            "run.json contains {needle:?}, which came from a run input. The manifest records \
             digests and metadata only — see `run_manifest.rs`'s \"What this document may never \
             contain\". Written document:\n{written}"
        );
    }

    for digest in expected_digests {
        assert!(
            written.contains(&digest),
            "run.json is missing the digest {digest}. Absence of the input is only half the \
             claim: a manifest that recorded nothing about an input could not reproduce the run, \
             and this test would otherwise pass for that document too"
        );
    }
}

#[test]
fn the_workspace_path_the_manifest_was_written_into_is_not_in_it() {
    let root = home_shaped_run_root("path");
    let written = written_manifest_bytes(manifest_from_secret_bearing_inputs(), "path");

    assert!(
        !written.contains(&root.display().to_string()),
        "run.json records the workspace it was written into; a manifest is attached to bug \
         reports and a run root is a path under someone's home directory"
    );
    assert!(
        !written.contains("/tmp/") && !written.contains(HOME_SENTINEL),
        "no absolute path of any kind belongs in a manifest:\n{written}"
    );
}

#[test]
fn no_field_anywhere_in_the_schema_is_shaped_like_a_path_or_a_secret() {
    // Covers *nested* types and, more usefully, future fields: the
    // frozen-key test in `tests/run_manifest.rs` covers the top level
    // only, so a `logPath` added to `ComponentProvenance` would pass
    // there and fail here.
    //
    // The check is on names rather than values because a name is what a
    // reviewer would have to notice: a field called `kubeconfigPath`
    // announces the mistake before anything is ever written into it.
    const FORBIDDEN: &[&str] = &[
        "path",
        "dir",
        "file",
        "kubeconfig",
        "token",
        "password",
        "secret",
        "credential",
        "private",
        "env",
        "stdout",
        "stderr",
        "output",
        "home",
        "url",
    ];

    let schema = serde_json::to_value(run_manifest_v1_json_schema())
        .expect("the generated schema always serializes");
    let mut names = Vec::new();
    collect_property_names(&schema, &mut names);
    assert!(
        names.len() > 10,
        "the schema walk found only {} property names, which means it is not walking the \
         document it thinks it is",
        names.len()
    );

    for name in names {
        let lowered = name.to_lowercase();
        for forbidden in FORBIDDEN {
            assert!(
                !lowered.contains(forbidden),
                "the run manifest has a field named {name:?}, which contains {forbidden:?}. \
                 Re-read `run_manifest.rs`'s \"What this document may never contain\": every \
                 field is a version string, an image reference, an identifier, a SHA-256 \
                 digest, or a timestamp. If this field is genuinely none of those things, it \
                 does not belong in a document people attach to issues"
            );
        }
    }
}

/// Collects every `properties` key anywhere in a JSON Schema document.
fn collect_property_names(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(fields) => {
            if let Some(serde_json::Value::Object(properties)) = fields.get("properties") {
                out.extend(properties.keys().cloned());
            }
            for nested in fields.values() {
                collect_property_names(nested, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_property_names(item, out);
            }
        }
        _ => {}
    }
}
