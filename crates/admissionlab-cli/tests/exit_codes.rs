//! The frozen v1 CLI contract (ROADMAP Task 9.2): what `admissionlab`
//! accepts, and what it returns.
//!
//! Three separate promises are pinned here, and each is a promise to
//! somebody's shell script rather than to a reader of the source:
//!
//! 1. **Every typed failure maps to its exact exit code.** The table in
//!    the first section drives each error family through the *real*
//!    `admissionlab_cli::exit` mapping functions — the ones
//!    `pipeline::run_lab` itself calls — and asserts the `ExitCode` the
//!    process would return, not the intermediate `RunDisposition`. Each
//!    family also has a `..._variant` function whose `match` is
//!    exhaustive with no `_` arm, so a new variant added anywhere in the
//!    workspace fails *this file* to compile until somebody decides
//!    which frozen code it belongs to.
//! 2. **`--help` and `--version` always exit `0`**, on the root and on
//!    every subcommand, and a bare `admissionlab` exits `2`.
//! 3. **The command surface itself does not drift.** The last section
//!    parses `--help` down to just the command names, positional value
//!    names, and option spellings — a golden trimmed to exactly what a
//!    script can depend on — and compares it against the frozen list in
//!    `main.rs`'s module documentation. Rewording a description is free;
//!    adding, renaming, or dropping a flag is not.
//!
//! And `--keep-clusters` gets its own section, because it is the one
//! flag that could plausibly be mistaken for changing a verdict: it is
//! driven through `pipeline::run_lab` against fakes on a passing run, a
//! policy-failing run, and a failing run, with and without the flag,
//! asserting the same code both ways.
//!
//! # Why this file duplicates a little of `tests/exit_codes` in
//! `src/exit.rs`
//!
//! `src/exit.rs`'s own unit tests check that module's internal
//! consistency (the discriminant ordering, the cleanup precedence rule).
//! This file is the *contract*: it states the ROADMAP §0.4 numbers as
//! literals next to each typed failure, so changing the mapping requires
//! editing a table that reads like the published documentation rather
//! than a `match` arm. The overlap is the point — two independent
//! statements of one frozen fact.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use admissionlab_admission::{AdmissionDecision, AdmissionOutcome, AdmissionTrace, TraceEvidence};
use admissionlab_cli::exit::{
    after_failed_cleanup, code_for_disposition, disposition_for_artifact_error,
    disposition_for_capture_failure, disposition_for_expectations, disposition_for_fixture_error,
    disposition_for_gateway_failure, disposition_for_install_failure, disposition_for_normalize,
    disposition_for_policy, disposition_for_policy_spec, disposition_for_prerequisites,
    disposition_for_report_error, disposition_for_reproduce_error, disposition_for_run_error,
    disposition_for_spec_error,
};
use admissionlab_cli::pipeline::{
    Console, GatewaySuiteError, GatewaySuiteRunner, LabBackend, MigrationRunOutcome,
    MigrationSuiteError, MigrationSuiteRunner, OutcomeCapture, RunRequest, SideGatewayOutcome,
    run_lab,
};
use admissionlab_core::{
    ArtifactError, ArtifactStore, CapturedFixture, ClusterCreationFailure, ClusterDiagnostics,
    ClusterError, ClusterHandle, ClusterManager, ClusterSpec, DoctorReport, FixtureCapture,
    FixtureCaptureError, InstalledComponent, ReproduceError, RunDisposition, RunError, RunId,
    RunPaths, Side, SideCapture, SideInstall, StackInstallError, StackInstallFailure,
    StackInstaller, ToolName, ToolStatus, VerifiedInput,
};
use admissionlab_fixtures::{FixtureError, FixtureSource, MatrixError};
use admissionlab_normalize::{NormalizeError, PointerError, RuleTier};
use admissionlab_policy::PolicyDisposition;
use admissionlab_report::{ReportError, TerminalOptions};
use admissionlab_spec::{GatewaySuiteSpec, MigrationSuiteSpec, ResolvedComponent, SpecError};
use async_trait::async_trait;

// =====================================================================
// The frozen table: typed failure -> exit code
// =====================================================================

/// One row of the frozen contract.
///
/// `code` is the *process* exit code, spelled as the literal ROADMAP
/// §0.4 number rather than derived from a `RunDisposition`, so this
/// table can be read against the published exit-code documentation
/// line by line.
struct Row<E> {
    /// The variant this row stands for, as
    /// [`spec_variant`]-style naming returns it. Asserted against the
    /// exhaustive `match`, so a row can never silently describe a
    /// different variant than the one it constructs.
    variant: &'static str,
    /// The failure itself, constructed exactly as the code that raises
    /// it would.
    error: E,
    /// The frozen exit code.
    code: u8,
}

/// Drives every row of one family through the real mapping and checks
/// that the family is covered variant for variant.
///
/// `variant_of` is the exhaustive-`match` naming function; `variants` is
/// the family's full variant list, written out by hand. A *new* variant
/// breaks `variant_of`'s compilation; a variant that exists but has no
/// row fails the coverage assertion at the end.
fn check_family<E>(
    family: &str,
    rows: &[Row<E>],
    disposition_of: impl Fn(&E) -> RunDisposition,
    variant_of: impl Fn(&E) -> &'static str,
    variants: &[&'static str],
) {
    let mut covered: BTreeSet<&'static str> = BTreeSet::new();
    for row in rows {
        assert_eq!(
            variant_of(&row.error),
            row.variant,
            "{family}: a row labelled {:?} constructs a different variant",
            row.variant
        );
        let disposition = disposition_of(&row.error);
        assert_eq!(
            code_for_disposition(disposition),
            ExitCode::from(row.code),
            "{family}::{} is frozen at exit code {} (got {disposition:?})",
            row.variant,
            row.code
        );
        covered.insert(row.variant);
    }
    let expected: BTreeSet<&'static str> = variants.iter().copied().collect();
    assert_eq!(
        covered, expected,
        "{family}: every variant needs a row in the frozen table"
    );
}

/// A `serde_json::Error`, for the two `Serialize` variants that carry
/// one. Produced by an actual failed parse rather than mocked: the type
/// has no public constructor, and none is needed.
fn json_error() -> serde_json::Error {
    serde_json::from_str::<serde_json::Value>("{").expect_err("truncated JSON must not parse")
}

// ---------------------------------------------------------------------
// Exit 2: invalid configuration
// ---------------------------------------------------------------------

fn spec_variant(error: &SpecError) -> &'static str {
    match error {
        SpecError::Io { .. } => "Io",
        SpecError::Parse { .. } => "Parse",
        SpecError::InvalidGlob { .. } => "InvalidGlob",
        SpecError::Validation { .. } => "Validation",
    }
}

#[test]
fn every_spec_error_exits_two() {
    let rows = [
        Row {
            variant: "Io",
            error: SpecError::Io {
                path: PathBuf::from("/nope/admissionlab.yaml"),
                source: io::Error::new(io::ErrorKind::NotFound, "no such file"),
            },
            code: 2,
        },
        Row {
            variant: "Parse",
            error: SpecError::Parse {
                path: PathBuf::from("/lab.yaml"),
                source: serde_norway::from_str::<serde_norway::Value>("\tnot: yaml")
                    .expect_err("a tab-indented document must not parse"),
            },
            code: 2,
        },
        Row {
            variant: "InvalidGlob",
            error: SpecError::InvalidGlob {
                path: PathBuf::from("/lab.yaml"),
                pattern: "fixtures/[".to_owned(),
                source: globset::Glob::new("fixtures/[").expect_err("an unclosed class is invalid"),
            },
            code: 2,
        },
        Row {
            variant: "Validation",
            error: SpecError::Validation {
                path: PathBuf::from("/lab.yaml"),
                message: "baseline.kubernetes must not be empty".to_owned(),
            },
            code: 2,
        },
    ];
    check_family(
        "SpecError",
        &rows,
        disposition_for_spec_error,
        spec_variant,
        &["Io", "Parse", "InvalidGlob", "Validation"],
    );
}

fn reproduce_variant(error: &ReproduceError) -> &'static str {
    match error {
        ReproduceError::UnsupportedSchema { .. } => "UnsupportedSchema",
        ReproduceError::Unreadable { .. } => "Unreadable",
        ReproduceError::Config { .. } => "Config",
        ReproduceError::ExpectationsPresenceChanged { .. } => "ExpectationsPresenceChanged",
        ReproduceError::ComponentSetChanged { .. } => "ComponentSetChanged",
    }
}

#[test]
fn every_reproduce_error_exits_two() {
    let rows = [
        Row {
            variant: "UnsupportedSchema",
            error: ReproduceError::UnsupportedSchema {
                found: "admissionlab.io/v9".to_owned(),
                supported: &["admissionlab.io/v1alpha1"],
            },
            code: 2,
        },
        Row {
            variant: "Unreadable",
            error: ReproduceError::Unreadable {
                path: PathBuf::from("/artifacts/run.json"),
                source: io::Error::new(io::ErrorKind::NotFound, "no such file"),
            },
            code: 2,
        },
        Row {
            variant: "Config",
            error: ReproduceError::Config {
                source: Box::new(SpecError::Validation {
                    path: PathBuf::from("/lab.yaml"),
                    message: "baseline.kubernetes must not be empty".to_owned(),
                }),
                config: Box::new(VerifiedInput {
                    path: PathBuf::from("/lab.yaml"),
                    expected_sha256: "a".repeat(64),
                    actual_sha256: "b".repeat(64),
                }),
            },
            code: 2,
        },
        Row {
            variant: "ExpectationsPresenceChanged",
            error: ReproduceError::ExpectationsPresenceChanged {
                recorded: true,
                source_file: None,
            },
            code: 2,
        },
        Row {
            variant: "ComponentSetChanged",
            error: ReproduceError::ComponentSetChanged {
                side: Side::Candidate,
                recorded: vec!["kyverno".to_owned()],
                current: vec!["gatekeeper".to_owned()],
            },
            code: 2,
        },
    ];
    check_family(
        "ReproduceError",
        &rows,
        disposition_for_reproduce_error,
        reproduce_variant,
        &[
            "UnsupportedSchema",
            "Unreadable",
            "Config",
            "ExpectationsPresenceChanged",
            "ComponentSetChanged",
        ],
    );
}

#[test]
fn the_constant_invalid_input_families_exit_two() {
    // Neither of these takes an error value: `validate_policy_spec`
    // reports its own list, and an expectations failure is always the
    // user's `expectationsFile`. They are still part of the frozen
    // table, so they are stated here rather than left implicit.
    for (label, disposition) in [
        ("policy spec validation", disposition_for_policy_spec()),
        ("expectations loading", disposition_for_expectations()),
    ] {
        assert_eq!(
            code_for_disposition(disposition),
            ExitCode::from(2),
            "{label} is frozen at exit code 2"
        );
    }
}

#[test]
fn a_missing_host_prerequisite_exits_two_and_a_good_host_is_not_a_failure() {
    let good = DoctorReport {
        tools: ToolName::ALL
            .iter()
            .map(|name| ToolStatus {
                name: *name,
                found: true,
                version: Some("1.0.0".to_owned()),
                diagnostic: None,
            })
            .collect(),
        docker_reachable: true,
        // Present deliberately: a disk warning is a warning, and must
        // never become an exit code.
        disk_warning: Some("only 1.0 GiB free".to_owned()),
    };
    assert_eq!(disposition_for_prerequisites(&good), None);

    let mut missing_tool = good.clone();
    missing_tool.tools[0].found = false;
    let mut docker_down = good;
    docker_down.docker_reachable = false;
    for (label, report) in [
        ("a missing host tool", missing_tool),
        ("an unreachable Docker daemon", docker_down),
    ] {
        let disposition =
            disposition_for_prerequisites(&report).expect("an unmet prerequisite must fail");
        assert_eq!(
            code_for_disposition(disposition),
            ExitCode::from(2),
            "{label} is frozen at exit code 2, in `test` exactly as in `doctor`"
        );
    }
}

// ---------------------------------------------------------------------
// Exit 2 or 5: fixtures, split by who can fix them
// ---------------------------------------------------------------------

fn fixture_variant(error: &FixtureError) -> &'static str {
    match error {
        FixtureError::WalkDirectory { .. } => "WalkDirectory",
        FixtureError::NonUtf8Path { .. } => "NonUtf8Path",
        FixtureError::ReadFile { .. } => "ReadFile",
        FixtureError::Parse { .. } => "Parse",
        FixtureError::NotAnObject { .. } => "NotAnObject",
        FixtureError::MissingField { .. } => "MissingField",
        FixtureError::GenerateNameUnsupported { .. } => "GenerateNameUnsupported",
        FixtureError::DuplicateFixtureId { .. } => "DuplicateFixtureId",
        FixtureError::Matrix(_) => "Matrix",
        FixtureError::ResourceDiscoveryUnavailable { .. } => "ResourceDiscoveryUnavailable",
        FixtureError::UnsupportedResource { .. } => "UnsupportedResource",
        FixtureError::ReplayUnavailable { .. } => "ReplayUnavailable",
    }
}

/// Everything `discover_fixtures` can produce from files on disk.
///
/// All of it is the user's own fixture corpus, so all of it exits `2`,
/// and all of it is found before any cluster exists.
fn fixture_definition_rows() -> Vec<Row<FixtureError>> {
    let path = PathBuf::from("/fixtures/pod.yaml");
    vec![
        Row {
            variant: "WalkDirectory",
            error: FixtureError::WalkDirectory {
                path: path.clone(),
                source: io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
            },
            code: 2,
        },
        Row {
            variant: "NonUtf8Path",
            error: FixtureError::NonUtf8Path { path: path.clone() },
            code: 2,
        },
        Row {
            variant: "ReadFile",
            error: FixtureError::ReadFile {
                path: path.clone(),
                source: io::Error::new(io::ErrorKind::NotFound, "no such file"),
            },
            code: 2,
        },
        Row {
            variant: "Parse",
            error: FixtureError::Parse {
                path: path.clone(),
                document_index: 0,
                reason: "unexpected token".to_owned(),
            },
            code: 2,
        },
        Row {
            variant: "NotAnObject",
            error: FixtureError::NotAnObject {
                path: path.clone(),
                document_index: 0,
                found: "an array",
            },
            code: 2,
        },
        Row {
            variant: "MissingField",
            error: FixtureError::MissingField {
                path: path.clone(),
                document_index: 0,
                field: "metadata.name",
            },
            code: 2,
        },
        Row {
            variant: "GenerateNameUnsupported",
            error: FixtureError::GenerateNameUnsupported {
                path: path.clone(),
                document_index: 0,
            },
            code: 2,
        },
        Row {
            variant: "DuplicateFixtureId",
            error: FixtureError::DuplicateFixtureId {
                id: "pod-0".to_owned(),
                first_path: path.clone(),
                first_document_index: 0,
                second_path: PathBuf::from("/fixtures/other.yaml"),
                second_document_index: 0,
            },
            code: 2,
        },
        Row {
            variant: "Matrix",
            error: FixtureError::Matrix(MatrixError::UnknownAdmissionlabKind {
                path,
                document_index: 0,
                kind: "NotAMatrix".to_owned(),
            }),
            code: 2,
        },
    ]
}

/// The three failures produced only *against a live cluster*: fixture
/// execution rather than fixture definition, so exit `5`.
fn fixture_live_rows() -> Vec<Row<FixtureError>> {
    vec![
        Row {
            variant: "ResourceDiscoveryUnavailable",
            error: FixtureError::ResourceDiscoveryUnavailable {
                cluster: "adlab-baseline-x".to_owned(),
                reason: "connection refused".to_owned(),
            },
            code: 5,
        },
        Row {
            variant: "UnsupportedResource",
            error: FixtureError::UnsupportedResource {
                cluster: "adlab-baseline-x".to_owned(),
                api_version: "kyverno.io/v1".to_owned(),
                kind: "ClusterPolicy".to_owned(),
            },
            code: 5,
        },
        Row {
            variant: "ReplayUnavailable",
            error: FixtureError::ReplayUnavailable {
                cluster: "adlab-baseline-x".to_owned(),
                reason: "connection refused".to_owned(),
            },
            code: 5,
        },
    ]
}

#[test]
fn fixture_definition_errors_exit_two_and_live_cluster_ones_exit_five() {
    let mut rows = fixture_definition_rows();
    rows.extend(fixture_live_rows());
    check_family(
        "FixtureError",
        &rows,
        disposition_for_fixture_error,
        fixture_variant,
        &[
            "WalkDirectory",
            "NonUtf8Path",
            "ReadFile",
            "Parse",
            "NotAnObject",
            "MissingField",
            "GenerateNameUnsupported",
            "DuplicateFixtureId",
            "Matrix",
            "ResourceDiscoveryUnavailable",
            "UnsupportedResource",
            "ReplayUnavailable",
        ],
    );
}

#[test]
fn a_capture_or_gateway_suite_failure_exits_five() {
    for (label, disposition) in [
        (
            "a fixture capture failure",
            disposition_for_capture_failure(),
        ),
        (
            "a Gateway or migration suite failure",
            disposition_for_gateway_failure(),
        ),
    ] {
        assert_eq!(
            code_for_disposition(disposition),
            ExitCode::from(5),
            "{label} is frozen at exit code 5"
        );
    }
}

// ---------------------------------------------------------------------
// Exit 2, 3, or 6: bringing the run up
// ---------------------------------------------------------------------

fn run_variant(error: &RunError) -> &'static str {
    match error {
        RunError::NonAbsoluteRunRoot(_) => "NonAbsoluteRunRoot",
        RunError::Workspace(_) => "Workspace",
        RunError::NodeImageResolutionFailed { .. } => "NodeImageResolutionFailed",
        RunError::ClusterCreationFailed { .. } => "ClusterCreationFailed",
    }
}

fn cluster_error() -> ClusterError {
    ClusterError::InvalidName {
        name: "Not A Label".to_owned(),
        reason: "not a DNS-1123 label".to_owned(),
    }
}

#[test]
fn run_errors_split_between_the_users_input_the_lab_and_our_own_bug() {
    let rows = [
        Row {
            variant: "NodeImageResolutionFailed",
            error: RunError::NodeImageResolutionFailed {
                side: Side::Baseline,
                source: Box::new(cluster_error()),
            },
            // The user asked for a Kubernetes version the lab cannot
            // provision, and nothing was created: their configuration.
            code: 2,
        },
        Row {
            variant: "Workspace",
            error: RunError::Workspace(ArtifactError::Io {
                operation: "create directory",
                path: PathBuf::from("/runs"),
                source: io::Error::other("disk full"),
            }),
            code: 3,
        },
        Row {
            variant: "ClusterCreationFailed",
            error: RunError::ClusterCreationFailed {
                failure: ClusterCreationFailure::Both {
                    baseline: Box::new(cluster_error()),
                    candidate: Box::new(cluster_error()),
                },
                rollback: Vec::new(),
            },
            code: 3,
        },
        Row {
            variant: "NonAbsoluteRunRoot",
            // Unreachable from any user input: this crate always builds
            // an absolute run root, so reaching it is our bug.
            error: RunError::NonAbsoluteRunRoot(PathBuf::from("relative/runs")),
            code: 6,
        },
    ];
    check_family(
        "RunError",
        &rows,
        disposition_for_run_error,
        run_variant,
        &[
            "NonAbsoluteRunRoot",
            "Workspace",
            "NodeImageResolutionFailed",
            "ClusterCreationFailed",
        ],
    );
}

// ---------------------------------------------------------------------
// Exit 4: installation and readiness
// ---------------------------------------------------------------------

fn install_variant(failure: &StackInstallFailure) -> &'static str {
    match failure {
        StackInstallFailure::Baseline { .. } => "Baseline",
        StackInstallFailure::Candidate { .. } => "Candidate",
        StackInstallFailure::Both { .. } => "Both",
    }
}

fn install_error() -> StackInstallError {
    StackInstallError {
        component: Some("kyverno".to_owned()),
        message: "helm exited 1".to_owned(),
    }
}

fn side_install(side: Side) -> SideInstall {
    SideInstall {
        side,
        components: Vec::new(),
    }
}

#[test]
fn every_install_failure_exits_four() {
    let rows = [
        Row {
            variant: "Baseline",
            error: StackInstallFailure::Baseline {
                error: install_error(),
                candidate: side_install(Side::Candidate),
            },
            code: 4,
        },
        Row {
            variant: "Candidate",
            error: StackInstallFailure::Candidate {
                baseline: side_install(Side::Baseline),
                error: install_error(),
            },
            code: 4,
        },
        Row {
            variant: "Both",
            error: StackInstallFailure::Both {
                baseline: install_error(),
                candidate: install_error(),
            },
            code: 4,
        },
    ];
    check_family(
        "StackInstallFailure",
        &rows,
        disposition_for_install_failure,
        install_variant,
        &["Baseline", "Candidate", "Both"],
    );
}

// ---------------------------------------------------------------------
// Exit 2 or 6: normalization, split by which tier owns the rule
// ---------------------------------------------------------------------

/// A normalization failure, named by variant *and* by the tier the rule
/// came from — the tier is what decides the code, so a row that ignored
/// it would not describe the mapping.
fn normalize_variant(error: &NormalizeError) -> &'static str {
    match error {
        NormalizeError::InvalidPointer { tier, .. } => match tier {
            RuleTier::BuiltIn => "InvalidPointer/BuiltIn",
            RuleTier::Recipe => "InvalidPointer/Recipe",
            RuleTier::User => "InvalidPointer/User",
        },
        NormalizeError::RemovesDocumentRoot { tier } => match tier {
            RuleTier::BuiltIn => "RemovesDocumentRoot/BuiltIn",
            RuleTier::Recipe => "RemovesDocumentRoot/Recipe",
            RuleTier::User => "RemovesDocumentRoot/User",
        },
    }
}

#[test]
fn a_broken_built_in_rule_exits_six_and_a_broken_user_rule_exits_two() {
    let bad_pointer = || PointerError::MissingLeadingSlash {
        pointer: "metadata/uid".to_owned(),
    };
    let rows = [
        Row {
            variant: "InvalidPointer/BuiltIn",
            error: NormalizeError::InvalidPointer {
                tier: RuleTier::BuiltIn,
                source: bad_pointer(),
            },
            // Admission Lab's own constant is wrong: nothing the user
            // can fix, so it is an internal error.
            code: 6,
        },
        Row {
            variant: "InvalidPointer/Recipe",
            error: NormalizeError::InvalidPointer {
                tier: RuleTier::Recipe,
                source: bad_pointer(),
            },
            code: 2,
        },
        Row {
            variant: "InvalidPointer/User",
            error: NormalizeError::InvalidPointer {
                tier: RuleTier::User,
                source: bad_pointer(),
            },
            code: 2,
        },
        Row {
            variant: "RemovesDocumentRoot/BuiltIn",
            error: NormalizeError::RemovesDocumentRoot {
                tier: RuleTier::BuiltIn,
            },
            code: 6,
        },
        Row {
            variant: "RemovesDocumentRoot/Recipe",
            error: NormalizeError::RemovesDocumentRoot {
                tier: RuleTier::Recipe,
            },
            code: 2,
        },
        Row {
            variant: "RemovesDocumentRoot/User",
            error: NormalizeError::RemovesDocumentRoot {
                tier: RuleTier::User,
            },
            code: 2,
        },
    ];
    check_family(
        "NormalizeError",
        &rows,
        disposition_for_normalize,
        normalize_variant,
        &[
            "InvalidPointer/BuiltIn",
            "InvalidPointer/Recipe",
            "InvalidPointer/User",
            "RemovesDocumentRoot/BuiltIn",
            "RemovesDocumentRoot/Recipe",
            "RemovesDocumentRoot/User",
        ],
    );
}

// ---------------------------------------------------------------------
// Exit 3 or 6: writing the evidence out
// ---------------------------------------------------------------------

fn artifact_variant(error: &ArtifactError) -> &'static str {
    match error {
        ArtifactError::PathEscapesRoot { .. } => "PathEscapesRoot",
        ArtifactError::Serialize(_) => "Serialize",
        ArtifactError::Io { .. } => "Io",
    }
}

fn report_variant(error: &ReportError) -> &'static str {
    match error {
        ReportError::Serialize(_) => "Serialize",
        ReportError::Io { .. } => "Io",
    }
}

#[test]
fn artifact_and_report_failures_split_between_our_bug_and_the_filesystem() {
    let artifacts = [
        Row {
            variant: "PathEscapesRoot",
            error: ArtifactError::PathEscapesRoot {
                path: PathBuf::from("/etc/passwd"),
                root: PathBuf::from("/runs"),
            },
            code: 6,
        },
        Row {
            variant: "Serialize",
            error: ArtifactError::Serialize(json_error()),
            code: 6,
        },
        Row {
            variant: "Io",
            error: ArtifactError::Io {
                operation: "write temporary file",
                path: PathBuf::from("/runs/x"),
                source: io::Error::other("disk full"),
            },
            code: 3,
        },
    ];
    check_family(
        "ArtifactError",
        &artifacts,
        disposition_for_artifact_error,
        artifact_variant,
        &["PathEscapesRoot", "Serialize", "Io"],
    );

    let reports = [
        Row {
            variant: "Serialize",
            error: ReportError::Serialize(json_error()),
            code: 6,
        },
        Row {
            variant: "Io",
            error: ReportError::Io {
                operation: "create temporary file",
                path: PathBuf::from("/artifacts/result.json"),
                source: io::Error::other("read-only file system"),
            },
            code: 3,
        },
    ];
    check_family(
        "ReportError",
        &reports,
        disposition_for_report_error,
        report_variant,
        &["Serialize", "Io"],
    );
}

// ---------------------------------------------------------------------
// Exit 0 or 1: the verdict itself
// ---------------------------------------------------------------------

fn policy_variant(disposition: PolicyDisposition) -> &'static str {
    match disposition {
        PolicyDisposition::Pass => "Pass",
        PolicyDisposition::Warn => "Warn",
        PolicyDisposition::Fail => "Fail",
    }
}

#[test]
fn a_warning_verdict_exits_zero_and_only_a_failing_one_exits_one() {
    let rows = [
        Row {
            variant: "Pass",
            error: PolicyDisposition::Pass,
            code: 0,
        },
        Row {
            // §0.4 has no "completed with warnings" code; the warnings
            // are reported in all three renderers instead.
            variant: "Warn",
            error: PolicyDisposition::Warn,
            code: 0,
        },
        Row {
            variant: "Fail",
            error: PolicyDisposition::Fail,
            code: 1,
        },
    ];
    check_family(
        "PolicyDisposition",
        &rows,
        |disposition| disposition_for_policy(*disposition),
        |disposition| policy_variant(*disposition),
        &["Pass", "Warn", "Fail"],
    );
}

// ---------------------------------------------------------------------
// The disposition table itself, and the cleanup override
// ---------------------------------------------------------------------

/// ROADMAP §0.4's table, written out as literals.
const FROZEN_CODES: [(RunDisposition, u8); 7] = [
    (RunDisposition::Passed, 0),
    (RunDisposition::PolicyFailed, 1),
    (RunDisposition::InvalidInput, 2),
    (RunDisposition::InfrastructureFailed, 3),
    (RunDisposition::InstallationFailed, 4),
    (RunDisposition::FixtureFailed, 5),
    (RunDisposition::InternalError, 6),
];

#[test]
fn the_disposition_table_is_the_roadmap_table() {
    for (disposition, code) in FROZEN_CODES {
        assert_eq!(
            code_for_disposition(disposition),
            ExitCode::from(code),
            "{disposition:?} is frozen at exit code {code}"
        );
    }
}

#[test]
fn a_failed_cleanup_downgrades_a_pass_and_never_masks_a_failure() {
    assert_eq!(
        code_for_disposition(after_failed_cleanup(RunDisposition::Passed)),
        ExitCode::from(3),
        "a run that leaked clusters must never claim to have completed cleanly"
    );
    for (disposition, code) in FROZEN_CODES {
        if disposition == RunDisposition::Passed {
            continue;
        }
        assert_eq!(
            code_for_disposition(after_failed_cleanup(disposition)),
            ExitCode::from(code),
            "{disposition:?} is already non-zero and more specific than a cleanup failure"
        );
    }
}

#[test]
fn every_frozen_code_is_reachable_from_some_typed_failure() {
    // Assembled from the same mapping functions the rows above use, so
    // this cannot pass by listing dispositions that nothing produces.
    let reachable: BTreeSet<u8> = [
        disposition_for_policy(PolicyDisposition::Pass),
        disposition_for_policy(PolicyDisposition::Fail),
        disposition_for_spec_error(&SpecError::Validation {
            path: PathBuf::from("/lab.yaml"),
            message: "empty".to_owned(),
        }),
        disposition_for_artifact_error(&ArtifactError::Io {
            operation: "write",
            path: PathBuf::from("/runs/x"),
            source: io::Error::other("disk full"),
        }),
        disposition_for_install_failure(&StackInstallFailure::Both {
            baseline: install_error(),
            candidate: install_error(),
        }),
        disposition_for_capture_failure(),
        disposition_for_run_error(&RunError::NonAbsoluteRunRoot(PathBuf::from("relative"))),
    ]
    .into_iter()
    .map(|disposition| disposition as u8)
    .collect();
    let frozen: BTreeSet<u8> = FROZEN_CODES.iter().map(|(_, code)| *code).collect();
    assert_eq!(
        reachable, frozen,
        "every code in the frozen table must be produced by a real failure, and no other"
    );
}

// =====================================================================
// `--help` and `--version` always exit 0; no arguments exits 2
// =====================================================================

/// Every level of the CLI that answers `--help` and `--version`: the
/// root (no subcommand) and each frozen subcommand.
const HELP_LEVELS: [&[&str]; 4] = [&[], &["doctor"], &["test"], &["reproduce"]];

fn admissionlab() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("admissionlab").expect("the binary must be built")
}

#[test]
fn help_and_version_always_exit_zero_at_every_level() {
    for level in HELP_LEVELS {
        for flag in ["--help", "-h", "--version", "-V"] {
            let assert = admissionlab().args(level).arg(flag).assert().success();
            let output = assert.get_output();
            assert!(
                !output.stdout.is_empty(),
                "`admissionlab {} {flag}` must answer on stdout",
                level.join(" ")
            );
        }
    }
}

#[test]
fn the_help_subcommand_exits_zero_for_every_frozen_command() {
    admissionlab().arg("help").assert().success();
    for command in ["doctor", "test", "reproduce"] {
        admissionlab().args(["help", command]).assert().success();
    }
}

#[test]
fn no_arguments_at_all_exits_two_with_usage_on_stderr() {
    // Frozen deliberately: a bare invocation is a usage mistake, `2` is
    // already this tool's invalid-input code, and a script that forgot
    // its subcommand must fail rather than look like a passing run.
    let assert = admissionlab().assert().code(2);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("Usage: admissionlab"),
        "a bare invocation must print usage on stderr, got:\n{stderr}"
    );
    assert!(
        assert.get_output().stdout.is_empty(),
        "usage for a missing subcommand belongs on stderr, never stdout"
    );
}

#[test]
fn an_unknown_flag_exits_two_rather_than_pretending_to_run() {
    admissionlab()
        .args(["test", "config.yaml", "--not-a-flag"])
        .assert()
        .code(2);
}

// =====================================================================
// The frozen command surface
// =====================================================================

/// The surface of one `--help` page, trimmed to what a script can
/// depend on: subcommand names, positional value names, and option
/// spellings. Descriptions, defaults, and wording are deliberately
/// discarded — rewording them is free, and only this is frozen.
#[derive(Debug, Default, PartialEq, Eq)]
struct Surface {
    commands: BTreeSet<String>,
    positionals: BTreeSet<String>,
    options: BTreeSet<String>,
}

impl Surface {
    /// Parses `--help` output.
    ///
    /// Entries are recognized by their section (`Commands:`,
    /// `Arguments:`, `Options:`) and by their indentation: Clap indents
    /// an entry by at most 6 columns and any wrapped description by 10,
    /// so an 8-column cutoff separates the two in both the short and the
    /// long help layouts without depending on either's exact spacing.
    fn parse(help: &str) -> Self {
        const ENTRY_INDENT_LIMIT: usize = 8;
        let mut surface = Self::default();
        let mut section = "";
        for line in help.lines() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() {
                continue;
            }
            if !line.starts_with(' ') {
                if let Some(header) = trimmed.strip_suffix(':') {
                    section = match header {
                        "Commands" => "commands",
                        "Arguments" => "positionals",
                        "Options" => "options",
                        _ => "",
                    };
                }
                continue;
            }
            if line.len() - trimmed.len() > ENTRY_INDENT_LIMIT {
                continue;
            }
            let mut tokens = trimmed.split_whitespace();
            match section {
                "commands" => {
                    if let Some(name) = tokens
                        .next()
                        .filter(|name| name.chars().next().is_some_and(char::is_alphanumeric))
                    {
                        surface.commands.insert(name.to_owned());
                    }
                }
                "positionals" => {
                    if let Some(name) = tokens.next().filter(|name| name.starts_with('<')) {
                        surface.positionals.insert(name.to_owned());
                    }
                }
                "options" => {
                    for token in tokens {
                        let token = token.trim_end_matches(',');
                        if !token.starts_with('-') {
                            break;
                        }
                        surface.options.insert(token.to_owned());
                    }
                }
                _ => {}
            }
        }
        surface
    }

    /// The frozen surface, spelled the way `main.rs`'s module
    /// documentation spells it.
    ///
    /// `options` names only what is *specific* to this page:
    /// [`UNIVERSAL_OPTIONS`] — the global `-v`/`--verbose` plus the two
    /// meta flags every level answers — is added to every page here
    /// rather than repeated at four call sites.
    fn frozen(commands: &[&str], positionals: &[&str], options: &[&str]) -> Self {
        Self {
            commands: commands.iter().map(|name| (*name).to_owned()).collect(),
            positionals: positionals.iter().map(|name| (*name).to_owned()).collect(),
            options: options
                .iter()
                .chain(UNIVERSAL_OPTIONS.iter())
                .map(|name| (*name).to_owned())
                .collect(),
        }
    }
}

/// `--help` for one level of the CLI, as text.
fn help_text(level: &[&str]) -> String {
    let assert = admissionlab().args(level).arg("--help").assert().success();
    String::from_utf8(assert.get_output().stdout.clone()).expect("help must be UTF-8")
}

/// `-v`/`--verbose` is `global = true`, and `--help`/`--version` are
/// answered at every level, so all six spellings appear on every page.
const UNIVERSAL_OPTIONS: [&str; 6] = ["-v", "--verbose", "-h", "--help", "-V", "--version"];

#[test]
fn the_root_command_surface_is_frozen() {
    assert_eq!(
        Surface::parse(&help_text(&[])),
        Surface::frozen(&["doctor", "test", "reproduce", "help"], &[], &[]),
        "the frozen v1 root surface changed; see `main.rs`'s module documentation"
    );
}

#[test]
fn the_doctor_command_surface_is_frozen() {
    assert_eq!(
        Surface::parse(&help_text(&["doctor"])),
        Surface::frozen(&[], &[], &["--deep"]),
        "the frozen v1 `doctor` surface changed; see `main.rs`'s module documentation"
    );
}

#[test]
fn the_test_command_surface_is_frozen() {
    assert_eq!(
        Surface::parse(&help_text(&["test"])),
        Surface::frozen(
            &[],
            &["<CONFIG>"],
            &["--keep-clusters", "--report-dir", "--github-summary"],
        ),
        "the frozen v1 `test` surface changed; see `main.rs`'s module documentation"
    );
}

#[test]
fn the_reproduce_command_surface_is_frozen() {
    assert_eq!(
        Surface::parse(&help_text(&["reproduce"])),
        Surface::frozen(
            &[],
            &["<MANIFEST>"],
            &[
                "--source-root",
                "--config",
                "--keep-clusters",
                "--report-dir",
            ],
        ),
        "the frozen v1 `reproduce` surface changed; see `main.rs`'s module documentation"
    );
}

#[test]
fn the_global_verbose_flag_is_accepted_on_both_sides_of_the_subcommand() {
    // `global = true` is part of the surface: a script may write either
    // spelling, and both must keep parsing.
    for args in [
        vec!["--verbose", "test", "/nope/admissionlab.yaml"],
        vec!["test", "/nope/admissionlab.yaml", "--verbose"],
        vec!["-v", "test", "/nope/admissionlab.yaml"],
    ] {
        admissionlab()
            .args(&args)
            .assert()
            // Reaching our own configuration-load failure (exit 2, our
            // message) rather than a Clap usage error proves it parsed.
            .code(2)
            .stderr(predicates::str::contains(
                "failed to load lab configuration",
            ));
    }
}

// =====================================================================
// `--keep-clusters` never changes what an exit code means
// =====================================================================

/// A fresh, guaranteed-unique scratch directory, removed by the test
/// that made it.
fn unique_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-cli-exit-codes-{label}-{}",
        RunId::generate().as_str()
    ));
    std::fs::create_dir_all(&dir).expect("failed to create scratch dir");
    dir
}

/// Writes a minimal real lab configuration and one real fixture, so the
/// pipeline loads, resolves, and discovers for real; only the three
/// external systems below are faked.
fn write_lab(dir: &Path) -> PathBuf {
    let config = dir.join("admissionlab.yaml");
    std::fs::write(
        &config,
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: Lab\n\
         baseline:\n  kubernetes: \"1.36.4\"\n\
         candidate:\n  kubernetes: \"1.36.4\"\n\
         fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n",
    )
    .expect("failed to write lab configuration");
    let fixtures = dir.join("fixtures");
    std::fs::create_dir_all(&fixtures).expect("failed to create fixtures dir");
    std::fs::write(
        fixtures.join("pod.yaml"),
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: probe\nspec:\n  containers:\n\
         \x20\x20\x20\x20- name: app\n      image: registry.k8s.io/pause:3.10\n",
    )
    .expect("failed to write fixture");
    config
}

/// What the fake API servers pretend to have done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behavior {
    /// Both sides admit the fixture unchanged: a passing run.
    Identical,
    /// The candidate rejects what the baseline admitted: a
    /// `newly_denied`, graded critical, so a policy failure.
    CandidateDenies,
    /// The candidate's capture fails outright: a fixture failure.
    CandidateFails,
}

struct FakeClusters {
    created: Mutex<Vec<Side>>,
    deleted: Mutex<Vec<Side>>,
}

#[async_trait]
impl ClusterManager for FakeClusters {
    async fn resolve_node_image(&self, version: &str) -> Result<String, ClusterError> {
        Ok(format!("kindest/node:v{version}"))
    }

    async fn create(
        &self,
        spec: &ClusterSpec,
        paths: &RunPaths,
    ) -> Result<ClusterHandle, ClusterError> {
        self.created.lock().expect("poisoned").push(spec.side);
        Ok(ClusterHandle {
            spec: spec.clone(),
            kubeconfig: paths
                .kubeconfigs()
                .join(format!("{}.yaml", spec.side.as_str())),
            audit_log: paths.logs().join(spec.side.as_str()).join("audit.log"),
        })
    }

    async fn delete(&self, handle: &ClusterHandle) -> Result<(), ClusterError> {
        self.deleted
            .lock()
            .expect("poisoned")
            .push(handle.spec.side);
        Ok(())
    }

    async fn diagnostics(&self, handle: &ClusterHandle) -> ClusterDiagnostics {
        ClusterDiagnostics {
            cluster_name: handle.spec.name.clone(),
            cluster_exists: Some(false),
            kubeconfig_present: false,
            audit_log_present: false,
            notes: Vec::new(),
        }
    }
}

struct FakeInstaller;

#[async_trait]
impl StackInstaller for FakeInstaller {
    async fn install_stack(
        &self,
        cluster: &ClusterHandle,
        components: &[ResolvedComponent],
        _component_timeout: Duration,
    ) -> Result<SideInstall, StackInstallError> {
        Ok(SideInstall {
            side: cluster.spec.side,
            components: components
                .iter()
                .map(|component| InstalledComponent {
                    name: component.name.clone(),
                    method: "fake".to_owned(),
                    resolved_version: component.version.clone(),
                    started_at: std::time::SystemTime::UNIX_EPOCH,
                    elapsed: Duration::from_millis(1),
                    diagnostics: Vec::new(),
                })
                .collect(),
        })
    }
}

struct FakeCapture {
    fixtures: Vec<FixtureSource>,
    behavior: Behavior,
    outcomes: Mutex<Vec<AdmissionOutcome>>,
}

#[async_trait]
impl FixtureCapture for FakeCapture {
    async fn capture_side(
        &self,
        _cluster: &ClusterHandle,
        side: Side,
        paths: &RunPaths,
    ) -> Result<SideCapture, FixtureCaptureError> {
        if self.behavior == Behavior::CandidateFails && side == Side::Candidate {
            return Err(FixtureCaptureError {
                fixture: self
                    .fixtures
                    .first()
                    .map(|fixture| fixture.id.as_str().to_owned()),
                message: "fake capture failure".to_owned(),
            });
        }
        let denied = self.behavior == Behavior::CandidateDenies && side == Side::Candidate;
        let mut captured = Vec::new();
        for fixture in &self.fixtures {
            let outcome = AdmissionOutcome {
                fixture_id: fixture.id.clone(),
                side,
                decision: if denied {
                    AdmissionDecision::Rejected {
                        code: Some(403),
                        message: "denied by the candidate policy".to_owned(),
                    }
                } else {
                    AdmissionDecision::Accepted
                },
                warnings: Vec::new(),
                total_latency: Duration::from_millis(7),
                final_object: if denied {
                    None
                } else {
                    Some(serde_json::json!({
                        "apiVersion": "v1",
                        "kind": "Pod",
                        "metadata": {"name": "probe"},
                        "spec": {"containers": [{
                            "name": "app",
                            "image": "registry.k8s.io/pause:3.10"
                        }]}
                    }))
                },
                trace: AdmissionTrace {
                    evidence: TraceEvidence::Observed,
                    invocations: Vec::new(),
                },
                diagnostics: Vec::new(),
            };
            let artifact_dir = paths.raw().join(side.as_str()).join(fixture.id.as_str());
            self.outcomes
                .lock()
                .expect("poisoned")
                .push(outcome.clone());
            captured.push(CapturedFixture {
                fixture_id: fixture.id.clone(),
                side,
                outcome_path: artifact_dir.join("outcome.json"),
                artifact_dir,
                diagnostics: outcome.diagnostics,
            });
        }
        Ok(SideCapture {
            side,
            fixtures: captured,
        })
    }
}

impl OutcomeCapture for FakeCapture {
    fn captured_outcomes(&self) -> Vec<AdmissionOutcome> {
        self.outcomes.lock().expect("poisoned").clone()
    }
}

/// The two suite runners this file never reaches: [`write_lab`] declares
/// no `gateway:` and no `migration:` section, and `run_lab` constructs a
/// runner only for a lab that does. They exist to satisfy
/// [`LabBackend`]'s associated types.
struct UnusedSuite;

#[async_trait]
impl GatewaySuiteRunner for UnusedSuite {
    async fn run_side(
        &self,
        _cluster: &ClusterHandle,
        _side: Side,
        _paths: &RunPaths,
    ) -> Result<SideGatewayOutcome, GatewaySuiteError> {
        unreachable!("this file's lab configuration declares no Gateway suite")
    }
}

#[async_trait]
impl MigrationSuiteRunner for UnusedSuite {
    async fn run(
        &self,
        _baseline: &ClusterHandle,
        _candidate: &ClusterHandle,
        _paths: &RunPaths,
    ) -> Result<MigrationRunOutcome, MigrationSuiteError> {
        unreachable!("this file's lab configuration declares no migration suite")
    }
}

struct FakeBackend {
    clusters: Arc<FakeClusters>,
    behavior: Behavior,
}

impl FakeBackend {
    fn new(behavior: Behavior) -> Self {
        Self {
            clusters: Arc::new(FakeClusters {
                created: Mutex::new(Vec::new()),
                deleted: Mutex::new(Vec::new()),
            }),
            behavior,
        }
    }
}

#[async_trait]
impl LabBackend for FakeBackend {
    type Clusters = FakeClusters;
    type Installer = FakeInstaller;
    type Capture = FakeCapture;
    type Gateway = UnusedSuite;
    type Migration = UnusedSuite;

    async fn doctor_report(&self) -> DoctorReport {
        DoctorReport {
            tools: ToolName::ALL
                .iter()
                .map(|name| ToolStatus {
                    name: *name,
                    found: true,
                    version: Some("1.0.0".to_owned()),
                    diagnostic: None,
                })
                .collect(),
            docker_reachable: true,
            disk_warning: None,
        }
    }

    fn cluster_manager(&self) -> Arc<Self::Clusters> {
        Arc::clone(&self.clusters)
    }

    fn stack_installer(&self, _paths: &RunPaths) -> Self::Installer {
        FakeInstaller
    }

    fn gateway_suite(&self, _suite: GatewaySuiteSpec, _store: ArtifactStore) -> Self::Gateway {
        UnusedSuite
    }

    fn migration_suite(
        &self,
        _suite: MigrationSuiteSpec,
        _store: ArtifactStore,
    ) -> Self::Migration {
        UnusedSuite
    }

    fn fixture_capture(
        &self,
        fixtures: Vec<FixtureSource>,
        _store: ArtifactStore,
    ) -> Self::Capture {
        FakeCapture {
            fixtures,
            behavior: self.behavior,
            outcomes: Mutex::new(Vec::new()),
        }
    }

    fn component_timeout(&self) -> Duration {
        Duration::from_millis(1)
    }
}

/// Runs one whole lab against fakes and returns the code the process
/// would have exited with, plus how many clusters were deleted.
fn run_and_exit_code(behavior: Behavior, keep_clusters: bool, label: &str) -> (ExitCode, usize) {
    let dir = unique_dir(label);
    let config = write_lab(&dir);
    let backend = FakeBackend::new(behavior);
    let request = RunRequest {
        config: &config,
        keep_clusters,
        report_dir: None,
        github_summary: None,
        run_root: dir.join("runs"),
    };
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let disposition = {
        let mut console = Console {
            out: &mut out,
            err: &mut err,
            terminal: TerminalOptions::for_stream(false, true),
        };
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build test tokio runtime")
            .block_on(run_lab(&backend, &request, &mut console))
    };
    let deleted = backend.clusters.deleted.lock().expect("poisoned").len();
    let _ = std::fs::remove_dir_all(&dir);
    (code_for_disposition(disposition), deleted)
}

#[test]
fn keep_clusters_never_changes_the_exit_code_of_any_run() {
    // The flag changes what is left running afterwards and nothing
    // else. A CI job that adds it to collect a failing cluster for
    // debugging must still gate on the same code it gated on before.
    for (behavior, expected, label) in [
        (Behavior::Identical, 0_u8, "pass"),
        (Behavior::CandidateDenies, 1, "policy-fail"),
        (Behavior::CandidateFails, 5, "capture-fail"),
    ] {
        let (deleting, deleted) = run_and_exit_code(behavior, false, label);
        let (keeping, kept) = run_and_exit_code(behavior, true, &format!("{label}-keep"));
        assert_eq!(
            deleting,
            ExitCode::from(expected),
            "a {label} run without --keep-clusters is frozen at exit {expected}"
        );
        assert_eq!(
            keeping, deleting,
            "--keep-clusters must not change a {label} run's exit code"
        );
        assert_eq!(deleted, 2, "a {label} run must delete both clusters");
        assert_eq!(
            kept, 0,
            "--keep-clusters must delete nothing, even on a {label} run"
        );
    }
}
