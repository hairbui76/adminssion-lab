//! Behavioral tests for the core domain vocabulary: [`Side`], [`RunId`],
//! [`FixtureId`], [`RunDisposition`], and [`RunPaths`].

use std::path::PathBuf;

use admissionlab_core::{FixtureId, IdParseError, RunDisposition, RunId, RunPaths, Side};

// ---------------------------------------------------------------------
// Side
// ---------------------------------------------------------------------

#[test]
fn side_names_are_stable() {
    assert_eq!(Side::Baseline.as_str(), "baseline");
    assert_eq!(Side::Candidate.as_str(), "candidate");
}

// ---------------------------------------------------------------------
// RunId / FixtureId — rejected character classes
// ---------------------------------------------------------------------

#[test]
fn run_id_rejects_path_separators() {
    assert!(RunId::parse("abc/def").is_err());
}

#[test]
fn run_id_rejects_backslash() {
    assert!(RunId::parse("abc\\def").is_err());
}

#[test]
fn run_id_rejects_parent_dir_segment() {
    assert!(RunId::parse("..").is_err());
}

#[test]
fn run_id_rejects_whitespace() {
    for candidate in ["abc def", "abc\tdef", "abc\ndef", " abc"] {
        assert!(
            RunId::parse(candidate).is_err(),
            "expected {candidate:?} to be rejected"
        );
    }
}

#[test]
fn run_id_rejects_uppercase() {
    assert!(RunId::parse("Abc123").is_err());
}

#[test]
fn run_id_parse_rejects_empty_string() {
    assert_eq!(RunId::parse("").unwrap_err(), IdParseError::Empty);
}

#[test]
fn run_id_parse_error_reports_offending_character() {
    let err = RunId::parse("abc/def").unwrap_err();
    assert_eq!(
        err,
        IdParseError::InvalidCharacter {
            value: "abc/def".to_string(),
            invalid: '/',
        }
    );
}

#[test]
fn run_id_accepts_lowercase_alphanumeric_and_hyphen() {
    assert!(RunId::parse("run-2026-08-29-abc123").is_ok());
}

#[test]
fn fixture_id_rejects_path_separators() {
    assert!(FixtureId::parse("abc/def").is_err());
}

#[test]
fn fixture_id_accepts_lowercase_alphanumeric_and_hyphen() {
    assert!(FixtureId::parse("configmap-basic-mutation-0").is_ok());
}

#[test]
fn fixture_id_and_run_id_share_the_same_validation_rules() {
    // RunId and FixtureId are distinct types, but both must be validated by
    // the same shared routine: an invalid input produces byte-for-byte the
    // same `IdParseError` from either type.
    let run_err = RunId::parse("abc/def").unwrap_err();
    let fixture_err = FixtureId::parse("abc/def").unwrap_err();
    assert_eq!(run_err, fixture_err);
}

// ---------------------------------------------------------------------
// RunId / FixtureId — generation and round-trip behavior
// ---------------------------------------------------------------------

#[test]
fn run_id_generate_produces_only_allowed_characters() {
    let id = RunId::generate();
    assert!(
        id.as_str()
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "generated id {:?} contains a character outside [a-z0-9-]",
        id.as_str()
    );
}

#[test]
fn run_id_generate_round_trips_through_parse() {
    let generated = RunId::generate();
    let reparsed = RunId::parse(generated.as_str()).expect("generated id must parse");
    assert_eq!(generated, reparsed);
}

#[test]
fn run_id_generate_produces_distinct_ids() {
    assert_ne!(RunId::generate(), RunId::generate());
}

#[test]
fn fixture_id_round_trips_through_parse() {
    let original = FixtureId::parse("configmap-basic-mutation-0").unwrap();
    let reparsed = FixtureId::parse(original.as_str()).unwrap();
    assert_eq!(original, reparsed);
}

// ---------------------------------------------------------------------
// RunPaths
// ---------------------------------------------------------------------

#[test]
fn run_paths_children_are_nested_under_run_root() {
    let root = PathBuf::from("/var/lib/admissionlab/runs");
    let run_id = RunId::parse("run-abc123").unwrap();
    let paths = RunPaths::new(&root, &run_id);

    let run_root = root.join("run-abc123");
    assert_eq!(paths.root(), run_root.as_path());
    assert_eq!(paths.raw(), run_root.join("raw").as_path());
    assert_eq!(paths.normalized(), run_root.join("normalized").as_path());
    assert_eq!(paths.reports(), run_root.join("reports").as_path());
    assert_eq!(paths.logs(), run_root.join("logs").as_path());
    assert_eq!(paths.kubeconfigs(), run_root.join("kubeconfigs").as_path());
    assert_eq!(paths.run_json(), run_root.join("run.json").as_path());
}

#[test]
fn run_paths_new_does_not_touch_filesystem() {
    // A fresh, guaranteed-unique path that has certainly never been created
    // on this machine: if any of these exist afterward, `RunPaths::new`
    // must have created them, which it must never do.
    let run_id = RunId::generate();
    let root =
        std::env::temp_dir().join(format!("admissionlab-core-domain-test-{}", run_id.as_str()));
    assert!(!root.exists(), "test precondition: root must not exist yet");

    let paths = RunPaths::new(&root, &run_id);

    assert!(!paths.root().exists());
    assert!(!paths.raw().exists());
    assert!(!paths.normalized().exists());
    assert!(!paths.reports().exists());
    assert!(!paths.logs().exists());
    assert!(!paths.kubeconfigs().exists());
    assert!(!paths.run_json().exists());
    assert!(!root.exists());
}

// ---------------------------------------------------------------------
// RunDisposition
// ---------------------------------------------------------------------

#[test]
fn run_disposition_has_seven_variants() {
    // Exhaustive match: fails to compile if a variant is added, removed,
    // or renamed without updating this test.
    let variants = [
        RunDisposition::Passed,
        RunDisposition::PolicyFailed,
        RunDisposition::InvalidInput,
        RunDisposition::InfrastructureFailed,
        RunDisposition::InstallationFailed,
        RunDisposition::FixtureFailed,
        RunDisposition::InternalError,
    ];
    assert_eq!(variants.len(), 7);
    for variant in variants {
        match variant {
            RunDisposition::Passed
            | RunDisposition::PolicyFailed
            | RunDisposition::InvalidInput
            | RunDisposition::InfrastructureFailed
            | RunDisposition::InstallationFailed
            | RunDisposition::FixtureFailed
            | RunDisposition::InternalError => {}
        }
    }
}

#[test]
fn run_disposition_variants_are_declared_in_exit_code_order() {
    // A later task maps these 1:1 to CLI exit codes 0-6 (see ROADMAP.md
    // section 0.4); this locks the declaration order those codes will
    // depend on.
    assert_eq!(RunDisposition::Passed as u8, 0);
    assert_eq!(RunDisposition::PolicyFailed as u8, 1);
    assert_eq!(RunDisposition::InvalidInput as u8, 2);
    assert_eq!(RunDisposition::InfrastructureFailed as u8, 3);
    assert_eq!(RunDisposition::InstallationFailed as u8, 4);
    assert_eq!(RunDisposition::FixtureFailed as u8, 5);
    assert_eq!(RunDisposition::InternalError as u8, 6);
}
