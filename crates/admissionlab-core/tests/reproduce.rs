//! `admissionlab_core::reproduce`: planning a reproduction from a run
//! manifest, and refusing to fall forward (ROADMAP Task 5.3).
//!
//! Everything here is filesystem-and-strings: a real lab configuration
//! and a real expectations file are written to a scratch directory and
//! loaded, parsed, and hashed for real, because the whole subject of this
//! module is whether the bytes on disk are the bytes the recorded run
//! used. Only the manifest is hand-built — that is the *input* under
//! test, and constructing one literally is how a test states "this is
//! what was recorded" without a run having happened.
//!
//! The fixture-hash and effective-digest comparisons are pure functions
//! over values the caller computed, so they are exercised directly; see
//! `admissionlab_core::reproduce`'s "Where verification is split" section
//! for why this crate deliberately cannot compute either itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use admissionlab_core::reproduce::{
    DEFAULT_LAB_FILE_NAME, DiscoveredFixture, ReproduceError, ReproductionPin,
    UNCONFIRMED_COMPONENT_VERSION, incomplete_run_warning, plan_reproduction,
    plan_reproduction_from_config, verify_effective_digests, verify_fixtures,
};
use admissionlab_core::run_manifest::SCHEMA_VERSION;
use admissionlab_core::{
    ComponentProvenance, EnvironmentProvenance, FixtureId, HostProvenance, RunId, RunManifest,
    RunStage, RunStatus, ToolProvenance, file_sha256, sha256_hex,
};
use admissionlab_spec::InstallMethod;

// ---------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------

/// A fresh, guaranteed-unique scratch directory. Mirrors this crate's own
/// `tests/run_lifecycle.rs`.
fn unique_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-core-reproduce-{label}-{}",
        RunId::generate().as_str()
    ));
    std::fs::create_dir_all(&dir).expect("failed to create scratch dir");
    dir
}

/// The digest-pinned node image both sides of every manifest here ran.
const NODE_IMAGE: &str = "kindest/node:v1.36.4";
/// That image's recorded content digest.
const NODE_DIGEST: &str = "sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed";

/// Writes a lab configuration with one Helm component pinned at
/// `chart_version`, plus the expectations file it declares and one
/// fixture, and returns the source root.
///
/// Real YAML rather than a hand-built `ResolvedLab`: the point of every
/// test below is what happens to the bytes on disk, and a resolved value
/// constructed in memory has no bytes.
fn write_source(dir: &Path, chart_version: &str) -> PathBuf {
    std::fs::write(
        dir.join(DEFAULT_LAB_FILE_NAME),
        format!(
            "apiVersion: admissionlab.io/v1alpha1\n\
             kind: Lab\n\
             expectationsFile: expectations.yaml\n\
             baseline:\n\
             \x20\x20kubernetes: \"1.36.4\"\n\
             \x20\x20components:\n\
             \x20\x20\x20\x20- name: kyverno\n\
             \x20\x20\x20\x20\x20\x20install:\n\
             \x20\x20\x20\x20\x20\x20\x20\x20type: helm\n\
             \x20\x20\x20\x20\x20\x20\x20\x20chart: kyverno/kyverno\n\
             \x20\x20\x20\x20\x20\x20\x20\x20repo: https://kyverno.github.io/kyverno/\n\
             \x20\x20\x20\x20\x20\x20\x20\x20version: \"{chart_version}\"\n\
             candidate:\n\
             \x20\x20kubernetes: \"1.36.4\"\n\
             \x20\x20components:\n\
             \x20\x20\x20\x20- name: kyverno\n\
             \x20\x20\x20\x20\x20\x20install:\n\
             \x20\x20\x20\x20\x20\x20\x20\x20type: helm\n\
             \x20\x20\x20\x20\x20\x20\x20\x20chart: kyverno/kyverno\n\
             \x20\x20\x20\x20\x20\x20\x20\x20repo: https://kyverno.github.io/kyverno/\n\
             \x20\x20\x20\x20\x20\x20\x20\x20version: \"{chart_version}\"\n\
             fixtures:\n\
             \x20\x20include:\n\
             \x20\x20\x20\x20- \"fixtures/**/*.yaml\"\n"
        ),
    )
    .expect("write lab configuration");
    std::fs::write(dir.join("expectations.yaml"), "expected: []\n").expect("write expectations");
    let fixtures = dir.join("fixtures");
    std::fs::create_dir_all(&fixtures).expect("create fixtures dir");
    std::fs::write(
        fixtures.join("pod.yaml"),
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: probe\n",
    )
    .expect("write fixture");
    dir.to_path_buf()
}

/// The manifest a run over [`write_source`]'s tree would have written,
/// with both file digests taken from the files themselves.
fn manifest_for(source: &Path, chart_version: &str) -> RunManifest {
    RunManifest {
        config_sha256: file_sha256(&source.join(DEFAULT_LAB_FILE_NAME)).expect("hash config"),
        expectations_sha256: Some(
            file_sha256(&source.join("expectations.yaml")).expect("hash expectations"),
        ),
        ..bare_manifest(chart_version)
    }
}

/// The same manifest with placeholder file digests, for the tests whose
/// subject is a pure comparison and which therefore have no source tree.
fn bare_manifest(chart_version: &str) -> RunManifest {
    let environment = |version: &str| EnvironmentProvenance {
        kubernetes_version: "1.36.4".to_owned(),
        node_image: NODE_IMAGE.to_owned(),
        node_image_digest: Some(NODE_DIGEST.to_owned()),
        components: vec![ComponentProvenance {
            name: "kyverno".to_owned(),
            version: version.to_owned(),
            source_sha256: None,
        }],
    };
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
        baseline: environment(chart_version),
        candidate: environment(chart_version),
        config_sha256: sha256_hex(b"config"),
        fixture_hashes: BTreeMap::new(),
        expectations_sha256: Some(sha256_hex(b"expectations")),
        normalization_sha256: sha256_hex(b"normalization"),
        policy_sha256: sha256_hex(b"policy"),
        started_at: std::time::SystemTime::UNIX_EPOCH,
        completed_at: Some(std::time::SystemTime::UNIX_EPOCH),
    }
}

/// A fixture identifier, spelled once.
fn fixture_id(id: &str) -> FixtureId {
    FixtureId::parse(id).expect("valid fixture id")
}

// ---------------------------------------------------------------------
// Planning against an unchanged source
// ---------------------------------------------------------------------

#[test]
fn an_unchanged_source_verifies_every_file_backed_input() {
    let dir = unique_dir("unchanged");
    let source = write_source(&dir, "3.9.0");
    let manifest = manifest_for(&source, "3.9.0");

    let plan = plan_reproduction(&manifest, &source).expect("an unchanged source must plan");

    assert_eq!(
        plan.verified_inputs.len(),
        2,
        "the configuration and the expectations file are both checked: {:#?}",
        plan.verified_inputs
    );
    assert_eq!(plan.mismatches().count(), 0, "{:#?}", plan.verified_inputs);
    assert_eq!(
        plan.verified_inputs[0].path,
        source.join(DEFAULT_LAB_FILE_NAME)
    );
    assert_eq!(
        plan.verified_inputs[1].path,
        source.join("expectations.yaml")
    );
    // The lab really was re-resolved, not merely located.
    assert_eq!(plan.resolved_lab.baseline.components.len(), 1);
    assert_eq!(plan.resolved_lab.baseline.components[0].version, "3.9.0");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_lab_file_under_another_name_is_reached_through_the_explicit_config_entry_point() {
    let dir = unique_dir("named");
    let source = write_source(&dir, "3.9.0");
    let manifest = manifest_for(&source, "3.9.0");
    let renamed = source.join("lab.yaml");
    std::fs::rename(source.join(DEFAULT_LAB_FILE_NAME), &renamed).expect("rename lab file");

    // The frozen entry point looks for the conventional name and finds
    // nothing, which is a read failure naming the path it looked at.
    let error = plan_reproduction(&manifest, &source)
        .expect_err("the conventional name no longer exists here");
    assert!(
        matches!(&error, ReproduceError::Unreadable { path, .. }
            if path == &source.join(DEFAULT_LAB_FILE_NAME)),
        "{error:?}"
    );

    let plan = plan_reproduction_from_config(&manifest, &renamed).expect("named lab file plans");
    assert_eq!(plan.mismatches().count(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// Step 1: a changed input is reported, with both digests
// ---------------------------------------------------------------------

#[test]
fn an_edited_configuration_is_reported_as_a_mismatch_rather_than_raised() {
    let dir = unique_dir("edited-config");
    let source = write_source(&dir, "3.9.0");
    let manifest = manifest_for(&source, "3.9.0");
    // A comment: parses identically, hashes differently. This is exactly
    // the case `config_sha256` exists to catch and `policy_sha256` does
    // not (see `run_manifest`'s "Canonical serialization" section).
    let config = source.join(DEFAULT_LAB_FILE_NAME);
    let text = std::fs::read_to_string(&config).expect("read config");
    std::fs::write(&config, format!("# edited\n{text}")).expect("rewrite config");

    let plan = plan_reproduction(&manifest, &source)
        .expect("a still-valid configuration plans, mismatch and all");

    let mismatches: Vec<_> = plan.mismatches().collect();
    assert_eq!(mismatches.len(), 1, "{:#?}", plan.verified_inputs);
    assert_eq!(mismatches[0].path, config);
    assert_eq!(mismatches[0].expected_sha256, manifest.config_sha256);
    assert_eq!(
        mismatches[0].actual_sha256,
        file_sha256(&config).expect("hash config")
    );
    assert_ne!(mismatches[0].expected_sha256, mismatches[0].actual_sha256);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_edited_expectations_file_is_reported_as_a_mismatch() {
    let dir = unique_dir("edited-expectations");
    let source = write_source(&dir, "3.9.0");
    let manifest = manifest_for(&source, "3.9.0");
    std::fs::write(source.join("expectations.yaml"), "expected: []\n# edited\n")
        .expect("rewrite expectations");

    let plan = plan_reproduction(&manifest, &source).expect("plans");
    let mismatches: Vec<_> = plan.mismatches().collect();
    assert_eq!(mismatches.len(), 1);
    assert_eq!(mismatches[0].path, source.join("expectations.yaml"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_configuration_that_no_longer_parses_reports_the_digest_mismatch_as_the_likely_cause() {
    let dir = unique_dir("unparseable");
    let source = write_source(&dir, "3.9.0");
    let manifest = manifest_for(&source, "3.9.0");
    std::fs::write(
        source.join(DEFAULT_LAB_FILE_NAME),
        "this: is: not: a: lab\n",
    )
    .expect("rewrite config");

    let error = plan_reproduction(&manifest, &source).expect_err("an unparseable lab cannot plan");
    let ReproduceError::Config { config, .. } = &error else {
        panic!("expected a configuration failure, got {error:?}");
    };
    assert!(!config.matches());
    let rendered = error.to_string();
    assert!(
        rendered.contains("no longer matches the recorded run"),
        "a parse failure on a changed file must say the file changed: {rendered}"
    );
    assert!(
        rendered.contains(&manifest.config_sha256),
        "the message must carry the recorded digest: {rendered}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// Refusals that precede any file check
// ---------------------------------------------------------------------

#[test]
fn a_manifest_from_another_schema_is_refused_rather_than_read_hopefully() {
    let dir = unique_dir("schema");
    let source = write_source(&dir, "3.9.0");
    let mut manifest = manifest_for(&source, "3.9.0");
    manifest.schema_version = "admissionlab.io/run-manifest/v2".to_owned();

    let error = plan_reproduction(&manifest, &source).expect_err("an unknown schema is refused");
    assert!(
        matches!(&error, ReproduceError::UnsupportedSchema { found, expected }
            if found == "admissionlab.io/run-manifest/v2" && *expected == SCHEMA_VERSION),
        "{error:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_manifest_recording_no_expectations_against_a_source_that_declares_one_is_refused() {
    let dir = unique_dir("expectations-presence");
    let source = write_source(&dir, "3.9.0");
    let mut manifest = manifest_for(&source, "3.9.0");
    manifest.expectations_sha256 = None;

    let error = plan_reproduction(&manifest, &source).expect_err("the two disagree");
    assert!(
        matches!(
            &error,
            ReproduceError::ExpectationsPresenceChanged {
                recorded: false,
                source_file: Some(_)
            }
        ),
        "{error:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// Fixture and effective-digest comparisons
// ---------------------------------------------------------------------

#[test]
fn fixture_verification_separates_edited_content_from_a_changed_corpus() {
    let mut fixture_hashes = BTreeMap::new();
    fixture_hashes.insert(fixture_id("pods-probe-0"), sha256_hex(b"probe"));
    fixture_hashes.insert(fixture_id("pods-gone-0"), sha256_hex(b"gone"));

    let mut manifest = bare_manifest("3.9.0");
    manifest.fixture_hashes = fixture_hashes;

    let discovered = vec![
        DiscoveredFixture {
            id: fixture_id("pods-probe-0"),
            path: PathBuf::from("fixtures/pod.yaml"),
            // Edited since the recorded run.
            sha256: sha256_hex(b"probe-edited"),
        },
        DiscoveredFixture {
            id: fixture_id("pods-new-0"),
            path: PathBuf::from("fixtures/new.yaml"),
            sha256: sha256_hex(b"new"),
        },
    ];

    let verification = verify_fixtures(&manifest, &discovered);

    assert!(!verification.is_faithful());
    assert_eq!(verification.verified.len(), 1);
    assert_eq!(
        verification.verified[0].path,
        PathBuf::from("fixtures/pod.yaml")
    );
    assert_eq!(
        verification.verified[0].expected_sha256,
        sha256_hex(b"probe")
    );
    assert_eq!(
        verification.verified[0].actual_sha256,
        sha256_hex(b"probe-edited")
    );
    assert_eq!(verification.missing, vec![fixture_id("pods-gone-0")]);
    assert_eq!(verification.unexpected, vec![fixture_id("pods-new-0")]);
}

#[test]
fn an_identical_corpus_is_faithful() {
    let mut manifest = bare_manifest("3.9.0");
    manifest
        .fixture_hashes
        .insert(fixture_id("pods-probe-0"), sha256_hex(b"probe"));

    let verification = verify_fixtures(
        &manifest,
        &[DiscoveredFixture {
            id: fixture_id("pods-probe-0"),
            path: PathBuf::from("fixtures/pod.yaml"),
            sha256: sha256_hex(b"probe"),
        }],
    );

    assert!(verification.is_faithful(), "{verification:#?}");
}

#[test]
fn effective_digest_verification_names_only_the_value_that_moved() {
    let manifest = bare_manifest("3.9.0");

    assert!(
        verify_effective_digests(
            &manifest,
            &manifest.normalization_sha256,
            &manifest.policy_sha256
        )
        .is_empty()
    );

    let mismatches = verify_effective_digests(
        &manifest,
        &sha256_hex(b"a different profile"),
        &manifest.policy_sha256,
    );
    assert_eq!(mismatches.len(), 1);
    assert_eq!(mismatches[0].what, "normalization");
    assert_eq!(mismatches[0].expected_sha256, manifest.normalization_sha256);
}

// ---------------------------------------------------------------------
// Step 2 / step 4: recorded versions win, and nothing falls forward
// ---------------------------------------------------------------------

#[test]
fn the_pin_reassembles_the_recorded_node_image_reference_verbatim() {
    let manifest = bare_manifest("3.9.0");
    let pin = ReproductionPin::from_manifest(&manifest);
    let images = pin.node_images();

    let expected = format!("{NODE_IMAGE}@{NODE_DIGEST}");
    assert_eq!(images.baseline, expected);
    assert_eq!(images.candidate, expected);
    assert!(pin.baseline.node_image_digest_pinned);
    assert!(
        pin.pinned_summary().contains(&expected),
        "the pre-run summary must name the exact reference: {}",
        pin.pinned_summary()
    );
}

#[test]
fn a_recorded_image_with_no_digest_pins_the_tag_and_says_so() {
    let dir = unique_dir("no-digest");
    let source = write_source(&dir, "3.9.0");
    let mut manifest = manifest_for(&source, "3.9.0");
    manifest.baseline.node_image_digest = None;

    let pin = ReproductionPin::from_manifest(&manifest);
    assert_eq!(pin.node_images().baseline, NODE_IMAGE);
    assert!(!pin.baseline.node_image_digest_pinned);

    let mut lab = plan_reproduction(&manifest, &source)
        .expect("plans")
        .resolved_lab;
    let notes = pin.apply(&mut lab).expect("pins");
    assert!(
        notes
            .iter()
            .any(|note| note.code == "reproduce.node_image_not_digest_pinned"),
        "an unpinnable image must be reported, never assumed stable: {notes:#?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_component_version_the_source_moved_on_from_is_pinned_back_to_the_recorded_one() {
    let dir = unique_dir("pin-version");
    // The recorded run installed 3.9.0; the source now resolves 3.10.0.
    // With the configuration's own digest matching this cannot arise from
    // the file, but it is exactly the fall-forward this must never do.
    let source = write_source(&dir, "3.10.0");
    let manifest = manifest_for(&source, "3.9.0");

    let mut lab = plan_reproduction(&manifest, &source)
        .expect("plans")
        .resolved_lab;
    let notes = ReproductionPin::from_manifest(&manifest)
        .apply(&mut lab)
        .expect("pins");

    for side in [&lab.baseline, &lab.candidate] {
        assert_eq!(side.components[0].version, "3.9.0");
        let InstallMethod::Helm(helm) = &side.components[0].install else {
            panic!("the fixture configuration installs Kyverno through Helm");
        };
        assert_eq!(
            helm.version, "3.9.0",
            "pinning the display version alone would leave `helm install --version` free to \
             resolve the newer chart, which is the whole failure this guards"
        );
    }
    let pinned: Vec<&str> = notes
        .iter()
        .filter(|note| note.code == "reproduce.component_version_pinned")
        .map(|note| note.message.as_str())
        .collect();
    assert_eq!(pinned.len(), 2, "one per side: {notes:#?}");
    assert!(
        pinned[0].contains("3.10.0") && pinned[0].contains("3.9.0"),
        "the substitution must name both versions: {}",
        pinned[0]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_kubernetes_version_the_source_moved_on_from_is_pinned_back_to_the_recorded_one() {
    let dir = unique_dir("pin-kubernetes");
    let source = write_source(&dir, "3.9.0");
    let mut manifest = manifest_for(&source, "3.9.0");
    manifest.baseline.kubernetes_version = "1.35.8".to_owned();

    let mut lab = plan_reproduction(&manifest, &source)
        .expect("plans")
        .resolved_lab;
    let notes = ReproductionPin::from_manifest(&manifest)
        .apply(&mut lab)
        .expect("pins");

    assert_eq!(lab.baseline.kubernetes, "1.35.8");
    assert_eq!(lab.candidate.kubernetes, "1.36.4");
    assert!(
        notes
            .iter()
            .any(|note| note.code == "reproduce.kubernetes_version_pinned"),
        "{notes:#?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unconfirmed_recorded_version_is_never_installed_as_a_version() {
    let dir = unique_dir("unconfirmed");
    let source = write_source(&dir, "3.9.0");
    let manifest = manifest_for(&source, UNCONFIRMED_COMPONENT_VERSION);

    let mut lab = plan_reproduction(&manifest, &source)
        .expect("plans")
        .resolved_lab;
    let notes = ReproductionPin::from_manifest(&manifest)
        .apply(&mut lab)
        .expect("pins");

    let InstallMethod::Helm(helm) = &lab.baseline.components[0].install else {
        panic!("the fixture configuration installs Kyverno through Helm");
    };
    assert_eq!(
        helm.version, "3.9.0",
        "`helm install --version unknown` would fail for a reason unrelated to the lab"
    );
    assert!(
        notes
            .iter()
            .any(|note| note.code == "reproduce.component_version_unconfirmed"),
        "the degradation must be reported: {notes:#?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unchanged_source_pins_silently() {
    let dir = unique_dir("silent");
    let source = write_source(&dir, "3.9.0");
    let manifest = manifest_for(&source, "3.9.0");

    let mut lab = plan_reproduction(&manifest, &source)
        .expect("plans")
        .resolved_lab;
    let notes = ReproductionPin::from_manifest(&manifest)
        .apply(&mut lab)
        .expect("pins");

    assert!(
        notes.is_empty(),
        "a reproduction that substituted nothing must say nothing: {notes:#?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_side_whose_component_set_changed_is_refused_outright() {
    let dir = unique_dir("component-set");
    let source = write_source(&dir, "3.9.0");
    let mut manifest = manifest_for(&source, "3.9.0");
    manifest.candidate.components.push(ComponentProvenance {
        name: "istiod".to_owned(),
        version: "1.30.4".to_owned(),
        source_sha256: None,
    });

    let mut lab = plan_reproduction(&manifest, &source)
        .expect("plans")
        .resolved_lab;
    let error = ReproductionPin::from_manifest(&manifest)
        .apply(&mut lab)
        .expect_err("there is nothing to install the recorded component from");
    assert!(
        matches!(&error, ReproduceError::ComponentSetChanged { side, recorded, .. }
            if side.as_str() == "candidate" && recorded.contains(&"istiod".to_owned())),
        "{error:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// Reproducing a run that never finished
// ---------------------------------------------------------------------

#[test]
fn an_unfinished_run_warns_and_is_still_reproducible() {
    let dir = unique_dir("unfinished");
    let source = write_source(&dir, "3.9.0");
    let mut manifest = manifest_for(&source, "3.9.0");
    manifest.status = RunStatus::Failed;
    manifest.stage = RunStage::Installation;
    manifest.completed_at = None;

    let warning = incomplete_run_warning(&manifest).expect("a failed run is worth a warning");
    assert!(warning.contains("installation"), "{warning}");
    plan_reproduction(&manifest, &source).expect("a failed run's manifest still reproduces");

    let completed = manifest_for(&source, "3.9.0");
    assert_eq!(incomplete_run_warning(&completed), None);

    let _ = std::fs::remove_dir_all(&dir);
}
