//! What the Admission Lab process returns, and how every pipeline
//! failure gets there.
//!
//! Two mappings live here, and both are deliberately in one file:
//!
//! 1. **Every typed failure the `admissionlab test` pipeline can produce
//!    → an [`RunDisposition`]** (the `disposition_for_*` functions
//!    below). Each one matches its error type *exhaustively*, with no
//!    `_` arm, so adding a variant anywhere in the workspace is a
//!    compile error here rather than a silent fall into whichever bucket
//!    happened to be last.
//! 2. **[`RunDisposition`] → the process's [`ExitCode`]**
//!    ([`code_for_disposition`]), reusing the enum's own discriminant so
//!    the frozen 0–6 numbering (ROADMAP §0.4) is never re-derived.
//!
//! A third mapping sits deliberately *outside* both:
//! [`code_for_cancellation`] answers `130`/`143` for a run an operator
//! interrupted, because such a run reached none of the seven meanings
//! the frozen table assigns. Its own documentation argues the case.
//!
//! This is the one module in the workspace that can see all of those
//! error types at once — `admissionlab-core` cannot name
//! `admissionlab_policy::PolicySpecErrors` or
//! `admissionlab_report::ReportError` without the dependency cycle
//! `admissionlab_core::run`'s module documentation describes — which is
//! why the table lives here rather than beside each error.
//!
//! # The mapping, and the judgement calls in it
//!
//! ROADMAP §0.4 fixes the meaning of each code: `0` completed and
//! passed, `1` completed and the regression policy failed, `2` invalid
//! user configuration or fixture definition, `3` lab infrastructure
//! failure, `4` installation/readiness failure, `5` fixture
//! execution/capture failure, `6` internal Admission Lab error. Most
//! failures land on exactly one of those. Four do not, and each is
//! resolved here explicitly:
//!
//! - **A missing host prerequisite is `2`, not `3`.**
//!   [`disposition_for_prerequisites`]. `admissionlab doctor` already
//!   answers this question — `commands::doctor::run_with` returns
//!   [`RunDisposition::InvalidInput`] when
//!   `DoctorReport::meets_prerequisites` is false (Task 1.4) — and the
//!   two commands must not disagree about what "you have not installed
//!   `kind`" means. It is also the honest reading: no lab infrastructure
//!   failed, because none was ever attempted; the *host* does not yet
//!   satisfy what the tool documents it needs, which is something the
//!   operator fixes, exactly like a malformed `admissionlab.yaml`.
//! - **A policy `warn` is still exit `0`.**
//!   [`disposition_for_policy`]. §0.4 has no code for "completed with
//!   warnings", and inventing one would break the frozen table; folding
//!   it into `1` would make every warning fail a CI job, which is
//!   precisely the distinction `admissionlab_policy::PolicyDisposition`
//!   exists to draw ("unexpected differences a human should look at,
//!   none critical"). The warnings are not hidden: they are in the
//!   terminal summary, in `result.json`, and in the HTML report, all of
//!   which are written before this code is chosen.
//! - **A cleanup failure downgrades a passing run to `3`, and never
//!   masks a non-zero verdict.** [`after_failed_cleanup`]. See its own
//!   documentation for the argument.
//! - **A built-in normalization rule that cannot be parsed is `6`, a
//!   recipe or user rule is `2`.** [`disposition_for_normalize`]. The
//!   built-in rules are Admission Lab's own constant; if one of those is
//!   unusable, the user has nothing to fix.

use std::process::ExitCode;

use admissionlab_core::{
    ArtifactError, CancelSignal, DoctorReport, ReproduceError, RunDisposition, RunError,
    StackInstallFailure,
};
use admissionlab_fixtures::FixtureError;
use admissionlab_normalize::{NormalizeError, RuleTier};
use admissionlab_policy::PolicyDisposition;
use admissionlab_report::ReportError;
use admissionlab_spec::SpecError;

/// The process exit code Admission Lab commits to for `disposition`.
///
/// Reuses `RunDisposition`'s own discriminant (`disposition as u8`)
/// rather than a second hand-written match arm, so this can never drift
/// from the canonical 0-6 ordering documented on [`RunDisposition`]
/// itself: [`RunDisposition::Passed`] is the only disposition that maps
/// to [`ExitCode::SUCCESS`]; every other variant maps to a distinct
/// non-zero code.
#[must_use]
pub fn code_for_disposition(disposition: RunDisposition) -> ExitCode {
    ExitCode::from(disposition as u8)
}

/// The process exit code a run canceled by `signal` returns: `130` for
/// `SIGINT`, `143` for `SIGTERM` (ROADMAP Task 9.6).
///
/// # Why this is not one of the frozen seven
///
/// ROADMAP §0.4 fixed `0`-`6`, and Task 9.2 froze them: each states
/// something definite about a run that reached its own conclusion —
/// it passed, its policy failed, its input was invalid, its
/// infrastructure or its install or its capture failed, or Admission Lab
/// itself is broken. A run somebody interrupted established none of
/// those. Answering `3` would report an infrastructure failure that did
/// not happen, `6` an Admission Lab bug that does not exist, and `0` a
/// pass that was never computed. Adding a seventh meaning to the frozen
/// table was the other option, and it is worse: the table is a published
/// contract with a promise that its meanings will not be reassigned.
///
/// `128 + signal number` is what every Unix shell already reports for a
/// process that *died* of a signal, so a script that treats `130` as
/// "the operator stopped this" is reading a convention it already knows
/// rather than an Admission Lab invention — and a gate written the
/// ordinary way (`exit code != 0` fails) keeps working untouched. This
/// is additive and frozen in exactly the same sense the seven are: `130`
/// and `143` now mean "canceled, no verdict" and will not be reassigned.
/// [`admissionlab_core::CancelSignal::exit_code`] owns the numbers, so
/// they are stated once, and `docs/troubleshooting.md` publishes them
/// beside the table.
///
/// A canceled run reaches this *instead of*
/// [`code_for_disposition`] — never in addition to it. The disposition
/// such a run returns from `pipeline::run_lab` is
/// [`CANCELED_DISPOSITION`], which exists only so a caller that ignores
/// cancellation entirely still cannot read a canceled run as a pass.
#[must_use]
pub fn code_for_cancellation(signal: CancelSignal) -> ExitCode {
    ExitCode::from(signal.exit_code())
}

/// The code the process returns for a run that ended with `disposition`
/// while `canceled_by` says whether anything asked it to stop.
///
/// One function because the interaction between the two is a rule, not a
/// preference, and it has exactly one place to live:
///
/// - **A run that reached a verdict reports the verdict**, even if a
///   signal arrived afterwards. `pipeline::run_lab` refuses to grade a
///   run canceled before its comparison, so a [`RunDisposition::Passed`]
///   or [`RunDisposition::PolicyFailed`] here means the comparison
///   happened, the reports are on disk, and cleanup finished — the
///   signal landed in the seconds after all of that. Answering `130`
///   there would tell a CI gate to discard a comparison that really was
///   performed and really is readable.
/// - **Otherwise a canceled run reports the cancellation** — `130`/`143`
///   (see [`code_for_cancellation`]) — rather than the fallback
///   disposition the pipeline returned.
/// - **Otherwise the disposition decides**, exactly as it always has.
#[must_use]
pub fn code_for_run(disposition: RunDisposition, canceled_by: Option<CancelSignal>) -> ExitCode {
    let reached_verdict = matches!(
        disposition,
        RunDisposition::Passed | RunDisposition::PolicyFailed
    );
    match canceled_by {
        Some(signal) if !reached_verdict => code_for_cancellation(signal),
        _ => code_for_disposition(disposition),
    }
}

/// What `pipeline::run_lab` returns for a run that stopped because it was
/// canceled.
///
/// Not a verdict, and not the code the process actually answers with:
/// `commands::test` and `commands::reproduce` both consult the
/// [`Cancellation`] handle first and return
/// [`code_for_cancellation`]'s `130`/`143`. This value is the fallback
/// for a caller that does not — a future command, or a test driving
/// `run_lab` directly — and it is
/// [`RunDisposition::InfrastructureFailed`] because that is the most
/// conservative "this run produced no verdict" answer inside the frozen
/// table: whatever else is true of an interrupted run, it did not pass
/// and its policy did not fail, so the two codes a CI gate reads as
/// *results* are exactly the two it must never be.
///
/// [`Cancellation`]: admissionlab_core::Cancellation
pub const CANCELED_DISPOSITION: RunDisposition = RunDisposition::InfrastructureFailed;

/// A configuration that failed to load, parse, or resolve.
///
/// Every [`SpecError`] is the user's own `admissionlab.yaml` at fault —
/// a missing file, a malformed document, a misspelled key, an unpinned
/// Helm version, an uncompilable fixture glob — so all of them are
/// [`RunDisposition::InvalidInput`]. Matched exhaustively anyway: a
/// future variant describing something that is *not* the user's input
/// (an unreadable recipe shipped with the tool, say) must be a compile
/// error here rather than silently inheriting this answer.
#[must_use]
pub fn disposition_for_spec_error(error: &SpecError) -> RunDisposition {
    match error {
        SpecError::Io { .. }
        | SpecError::Parse { .. }
        | SpecError::Validation { .. }
        | SpecError::InvalidGlob { .. } => RunDisposition::InvalidInput,
    }
}

/// A `policy` section that names something that does not exist.
///
/// `admissionlab_policy::validate_policy_spec` reports these before any
/// cluster is created (see its own documentation for why it, rather than
/// `admissionlab-spec`, owns the check): an unknown semantic-change
/// kind, an unknown severity, an uncompilable fixture glob, or an
/// impossible empty selector. All of them are the user's configuration.
#[must_use]
pub const fn disposition_for_policy_spec() -> RunDisposition {
    RunDisposition::InvalidInput
}

/// An `expectations.yaml` that could not be read, parsed, or checked.
///
/// `Io` is included deliberately: the path came from the user's own
/// `expectationsFile`, so a missing file is a configuration mistake, not
/// an infrastructure failure — the same reading
/// [`disposition_for_spec_error`] gives a missing `admissionlab.yaml`.
#[must_use]
pub const fn disposition_for_expectations() -> RunDisposition {
    RunDisposition::InvalidInput
}

/// A reproduction that could not be planned (ROADMAP Task 5.3).
///
/// Every variant is `2`, and matched exhaustively rather than collapsed
/// to a constant so a future variant describing something that is *not*
/// the user's input has to be classified here deliberately.
///
/// The reading is the same one [`disposition_for_spec_error`] gives a
/// malformed `admissionlab.yaml`: the command was handed a manifest and a
/// source tree that do not go together — a manifest from another schema,
/// a configuration that has been edited since, a corpus that no longer
/// matches — and that pair is invalid *input*. No lab infrastructure
/// failed, because none was attempted: every one of these is detected
/// from files on disk before a container exists (see
/// `admissionlab_core::reproduce`'s "Plan-time refusal versus run-time
/// unavailability" section for the failures that are *not* detectable
/// there, and which therefore surface as `3`/`4` from the run itself).
///
/// [`ReproduceError::Unreadable`] is `2` for the same reason
/// [`disposition_for_expectations`] treats a missing expectations file as
/// one: the path came from the user's own `--source-root`/`--config`.
#[must_use]
pub fn disposition_for_reproduce_error(error: &ReproduceError) -> RunDisposition {
    match error {
        ReproduceError::UnsupportedSchema { .. }
        | ReproduceError::Unreadable { .. }
        | ReproduceError::Config { .. }
        | ReproduceError::ExpectationsPresenceChanged { .. }
        | ReproduceError::ComponentSetChanged { .. } => RunDisposition::InvalidInput,
    }
}

/// Host prerequisites, as `admissionlab doctor` probes them.
///
/// See this module's documentation for why a missing tool or an
/// unreachable Docker daemon is [`RunDisposition::InvalidInput`] (`2`)
/// rather than [`RunDisposition::InfrastructureFailed`] (`3`), and why
/// that has to match `doctor`'s own answer.
///
/// `DoctorReport::disk_warning` never enters into it — that is
/// `meets_prerequisites`'s own documented rule (PRODUCT.md §34 calls it
/// a warning threshold, not a requirement), and this function does not
/// second-guess it.
#[must_use]
pub fn disposition_for_prerequisites(report: &DoctorReport) -> Option<RunDisposition> {
    if report.meets_prerequisites() {
        None
    } else {
        Some(RunDisposition::InvalidInput)
    }
}

/// Fixture discovery, or a fixture that could not be replayed.
///
/// Split down the middle, and the line is who can fix it:
///
/// - Everything `admissionlab_fixtures::discover_fixtures` can return is
///   the user's own fixture corpus — an unreadable directory, a
///   malformed YAML document, a document with no `metadata.name`, two
///   documents colliding on one identifier, a fixture matrix (Task
///   5.10) whose declaration or patches do not hold up — so those are
///   [`RunDisposition::InvalidInput`] (§0.4's "invalid ... fixture
///   definition"), and they are all discovered before any cluster
///   exists.
/// - The three variants produced *against a live cluster*
///   (`ResourceDiscoveryUnavailable`, `UnsupportedResource`,
///   `ReplayUnavailable`) belong to fixture execution and are
///   [`RunDisposition::FixtureFailed`] (`5`).
#[must_use]
pub fn disposition_for_fixture_error(error: &FixtureError) -> RunDisposition {
    match error {
        FixtureError::WalkDirectory { .. }
        | FixtureError::NonUtf8Path { .. }
        | FixtureError::ReadFile { .. }
        | FixtureError::Parse { .. }
        | FixtureError::NotAnObject { .. }
        | FixtureError::MissingField { .. }
        | FixtureError::GenerateNameUnsupported { .. }
        | FixtureError::Matrix(_)
        | FixtureError::DuplicateFixtureId { .. } => RunDisposition::InvalidInput,
        FixtureError::ResourceDiscoveryUnavailable { .. }
        | FixtureError::UnsupportedResource { .. }
        | FixtureError::ReplayUnavailable { .. } => RunDisposition::FixtureFailed,
    }
}

/// A failure bringing this run's workspace or clusters up.
///
/// - `NodeImageResolutionFailed` is the user asking for a Kubernetes
///   version Admission Lab cannot provision (Controller Ruling R25):
///   their configuration, caught before any cluster is created, so `2`.
///   This is the mapping `commands::test` has made since Task 1.10 and
///   `tests/cli.rs` pins.
/// - `Workspace` is a filesystem failure creating the run's own artifact
///   tree: lab infrastructure, so `3`.
/// - `ClusterCreationFailed` is `kind`/Docker failing: `3`.
/// - `NonAbsoluteRunRoot` can only happen if *this crate* hands
///   `LabRunner` a relative run root, which `commands::test` never does
///   (it builds one under `std::env::temp_dir()`). It is unreachable by
///   any user input, so it is [`RunDisposition::InternalError`] (`6`) —
///   the code §0.4 reserves for "an internal Admission Lab error" —
///   rather than blamed on infrastructure that never failed.
#[must_use]
pub fn disposition_for_run_error(error: &RunError) -> RunDisposition {
    match error {
        RunError::NodeImageResolutionFailed { .. } => RunDisposition::InvalidInput,
        RunError::Workspace(_) | RunError::ClusterCreationFailed { .. } => {
            RunDisposition::InfrastructureFailed
        }
        RunError::NonAbsoluteRunRoot(_) => RunDisposition::InternalError,
    }
}

/// A stack that failed to install or never became ready.
///
/// Always `4`, on either or both sides: §0.4 gives
/// installation/readiness its own code precisely so a chart that will
/// not come up is distinguishable from a cluster that will not start
/// (`3`) and from a fixture that could not be replayed (`5`). Takes the
/// failure by reference rather than being a bare constant so the call
/// site reads as a mapping and a future variant needing a different
/// answer has somewhere to go.
#[must_use]
pub const fn disposition_for_install_failure(_failure: &StackInstallFailure) -> RunDisposition {
    RunDisposition::InstallationFailed
}

/// A fixture that could not be replayed or whose evidence could not be
/// written. Always `5` (§0.4's "fixture execution/capture failure"), on
/// either or both sides.
///
/// A fixture the API server *rejected* never reaches here: that is an
/// ordinary captured outcome and one of the two things the whole
/// pipeline exists to observe (see
/// `admissionlab_core::FixtureCapture::capture_side`).
#[must_use]
pub const fn disposition_for_capture_failure() -> RunDisposition {
    RunDisposition::FixtureFailed
}

/// A Gateway route contract whose behavior could not be observed, or a
/// Gateway suite whose manifests would not apply. Always `5`, the same
/// answer [`disposition_for_capture_failure`] gives, and for the same
/// reason: §0.4's category is "fixture execution/capture failure", and
/// ROADMAP Phase 6's own execution note calls a persisted Gateway
/// manifest set a fixture. What the two have in common is the thing the
/// exit code names -- the run could not obtain the evidence it exists to
/// compare.
///
/// A route that reconciled to a `False` condition, or a probe that was
/// skipped because it did, never reaches here: both are ordinary
/// observations, and they are what this phase exists to see.
#[must_use]
pub const fn disposition_for_gateway_failure() -> RunDisposition {
    RunDisposition::FixtureFailed
}

/// A normalization rule that cannot be applied to any document.
///
/// See this module's documentation: a broken *built-in* rule is
/// Admission Lab's own constant being wrong, which the user cannot fix,
/// so it is `6`; a broken recipe or user rule is configuration, so `2`.
#[must_use]
pub fn disposition_for_normalize(error: &NormalizeError) -> RunDisposition {
    let tier = match error {
        NormalizeError::InvalidPointer { tier, .. }
        | NormalizeError::RemovesDocumentRoot { tier } => *tier,
    };
    match tier {
        RuleTier::BuiltIn => RunDisposition::InternalError,
        RuleTier::Recipe | RuleTier::User => RunDisposition::InvalidInput,
    }
}

/// A completed run's own verdict.
///
/// [`PolicyDisposition::Warn`] maps to [`RunDisposition::Passed`] — see
/// this module's documentation for why, and for where the warnings do
/// get reported.
#[must_use]
pub const fn disposition_for_policy(disposition: PolicyDisposition) -> RunDisposition {
    match disposition {
        PolicyDisposition::Pass | PolicyDisposition::Warn => RunDisposition::Passed,
        PolicyDisposition::Fail => RunDisposition::PolicyFailed,
    }
}

/// A report artifact that could not be produced.
///
/// - `Serialize` means Admission Lab's *own* result model failed to
///   encode, which no user input can cause and no user can fix: `6`.
/// - `Io` means the report directory could not be written to — a full
///   disk, a read-only `--report-dir`: `3`.
#[must_use]
pub fn disposition_for_report_error(error: &ReportError) -> RunDisposition {
    match error {
        ReportError::Serialize(_) => RunDisposition::InternalError,
        ReportError::Io { .. } => RunDisposition::InfrastructureFailed,
    }
}

/// An artifact-store write that failed.
///
/// `PathEscapesRoot` is unreachable from user input (every path this
/// crate writes is built from the run's own [`RunPaths`]), so it is a
/// bug in Admission Lab: `6`. `Serialize` likewise. An `Io` failure is a
/// real filesystem problem: `3`.
///
/// [`RunPaths`]: admissionlab_core::RunPaths
#[must_use]
pub fn disposition_for_artifact_error(error: &ArtifactError) -> RunDisposition {
    match error {
        ArtifactError::PathEscapesRoot { .. } | ArtifactError::Serialize(_) => {
            RunDisposition::InternalError
        }
        ArtifactError::Io { .. } => RunDisposition::InfrastructureFailed,
    }
}

/// Folds a failed cleanup into the disposition a run had otherwise
/// reached.
///
/// **A run whose clusters could not be deleted never exits `0`.** That
/// is the whole rule, and it is one-directional: a
/// [`RunDisposition::Passed`] becomes
/// [`RunDisposition::InfrastructureFailed`], and every other disposition
/// is returned unchanged.
///
/// # Why not simply "cleanup failure always wins"
///
/// Because that would throw away the more actionable of two true facts.
/// A run that found a critical regression *and* failed to delete a
/// cluster has two problems, and §0.4 has one code to say it with. The
/// regression is the finding the tool exists to deliver and the one a
/// reviewer must act on; the leaked cluster is reported loudly on
/// stderr, with the exact `kind delete cluster --name <name>` command
/// for each, by `LabRunner::cleanup`'s own diagnostics — so nothing
/// about it is hidden by keeping the more specific code. Both values are
/// non-zero, so any `if !ok` CI gate behaves identically either way;
/// only the reason differs.
///
/// # Why a passing run is downgraded at all
///
/// Because `0` is not merely "less severe", it is a positive claim that
/// the run completed cleanly, and a machine left with two `kind`
/// clusters running has not. This mirrors what `commands::test` has done
/// since Task 1.10, when a failed cleanup was already the one thing that
/// could turn a successful two-cluster lifecycle into a non-zero exit.
#[must_use]
pub const fn after_failed_cleanup(disposition: RunDisposition) -> RunDisposition {
    match disposition {
        RunDisposition::Passed => RunDisposition::InfrastructureFailed,
        RunDisposition::PolicyFailed
        | RunDisposition::InvalidInput
        | RunDisposition::InfrastructureFailed
        | RunDisposition::InstallationFailed
        | RunDisposition::FixtureFailed
        | RunDisposition::InternalError => disposition,
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;
    use std::process::ExitCode;

    use admissionlab_core::{
        ArtifactError, DoctorReport, RunDisposition, RunError, StackInstallError,
        StackInstallFailure, ToolName, ToolStatus,
    };
    use admissionlab_fixtures::FixtureError;
    use admissionlab_normalize::{NormalizeError, RuleTier};
    use admissionlab_policy::PolicyDisposition;
    use admissionlab_report::ReportError;
    use admissionlab_spec::SpecError;

    use super::*;

    /// Every disposition, so a table test can assert over the whole set
    /// rather than a sample of it. Kept in declaration order, which is
    /// also exit-code order.
    const ALL_DISPOSITIONS: [RunDisposition; 7] = [
        RunDisposition::Passed,
        RunDisposition::PolicyFailed,
        RunDisposition::InvalidInput,
        RunDisposition::InfrastructureFailed,
        RunDisposition::InstallationFailed,
        RunDisposition::FixtureFailed,
        RunDisposition::InternalError,
    ];

    // -------------------------------------------------------------
    // disposition -> exit code
    // -------------------------------------------------------------

    #[test]
    fn passed_maps_to_process_success() {
        assert_eq!(
            code_for_disposition(RunDisposition::Passed),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn every_disposition_maps_to_its_roadmap_code() {
        // ROADMAP §0.4's frozen table, written out by hand here rather
        // than derived from the enum: this test's whole job is to catch
        // the discriminant order changing underneath
        // `code_for_disposition`, which reading it back from the same
        // enum could not do.
        for (disposition, expected) in [
            (RunDisposition::Passed, 0_u8),
            (RunDisposition::PolicyFailed, 1),
            (RunDisposition::InvalidInput, 2),
            (RunDisposition::InfrastructureFailed, 3),
            (RunDisposition::InstallationFailed, 4),
            (RunDisposition::FixtureFailed, 5),
            (RunDisposition::InternalError, 6),
        ] {
            assert_eq!(
                code_for_disposition(disposition),
                ExitCode::from(expected),
                "{disposition:?} must map to exit code {expected}"
            );
        }
    }

    #[test]
    fn every_other_disposition_maps_to_a_nonzero_code() {
        for disposition in ALL_DISPOSITIONS {
            if disposition == RunDisposition::Passed {
                continue;
            }
            assert_ne!(
                code_for_disposition(disposition),
                ExitCode::SUCCESS,
                "{disposition:?} must not map to a process success code"
            );
        }
    }

    #[test]
    fn distinct_dispositions_map_to_distinct_codes() {
        assert_ne!(
            code_for_disposition(RunDisposition::PolicyFailed),
            code_for_disposition(RunDisposition::InternalError)
        );
    }

    // -------------------------------------------------------------
    // error -> disposition
    // -------------------------------------------------------------

    fn spec_errors() -> Vec<SpecError> {
        vec![
            SpecError::Io {
                path: PathBuf::from("/nope/admissionlab.yaml"),
                source: io::Error::new(io::ErrorKind::NotFound, "no such file"),
            },
            SpecError::Validation {
                path: PathBuf::from("/lab.yaml"),
                message: "baseline.kubernetes must not be empty".to_owned(),
            },
        ]
    }

    #[test]
    fn every_spec_error_is_invalid_input() {
        for error in spec_errors() {
            assert_eq!(
                disposition_for_spec_error(&error),
                RunDisposition::InvalidInput,
                "{error} must be invalid input"
            );
        }
    }

    #[test]
    fn discovery_fixture_errors_are_invalid_input_and_live_ones_are_fixture_failures() {
        let discovery = [
            FixtureError::NonUtf8Path {
                path: PathBuf::from("/fixtures/bad"),
            },
            FixtureError::Parse {
                path: PathBuf::from("/fixtures/pod.yaml"),
                document_index: 0,
                reason: "unexpected token".to_owned(),
            },
            FixtureError::NotAnObject {
                path: PathBuf::from("/fixtures/pod.yaml"),
                document_index: 0,
                found: "an array",
            },
            FixtureError::MissingField {
                path: PathBuf::from("/fixtures/pod.yaml"),
                document_index: 0,
                field: "metadata.name",
            },
            FixtureError::GenerateNameUnsupported {
                path: PathBuf::from("/fixtures/pod.yaml"),
                document_index: 0,
            },
            FixtureError::DuplicateFixtureId {
                id: "pod-0".to_owned(),
                first_path: PathBuf::from("/fixtures/a.yaml"),
                first_document_index: 0,
                second_path: PathBuf::from("/fixtures/b.yaml"),
                second_document_index: 0,
            },
        ];
        for error in discovery {
            assert_eq!(
                disposition_for_fixture_error(&error),
                RunDisposition::InvalidInput,
                "{error} is a fixture definition problem, so it must be invalid input"
            );
        }

        let live = [
            FixtureError::ResourceDiscoveryUnavailable {
                cluster: "adlab-baseline-x".to_owned(),
                reason: "connection refused".to_owned(),
            },
            FixtureError::UnsupportedResource {
                cluster: "adlab-baseline-x".to_owned(),
                api_version: "kyverno.io/v1".to_owned(),
                kind: "ClusterPolicy".to_owned(),
            },
            FixtureError::ReplayUnavailable {
                cluster: "adlab-baseline-x".to_owned(),
                reason: "connection refused".to_owned(),
            },
        ];
        for error in live {
            assert_eq!(
                disposition_for_fixture_error(&error),
                RunDisposition::FixtureFailed,
                "{error} happens against a live cluster, so it must be a fixture failure"
            );
        }
    }

    #[test]
    fn run_errors_split_between_the_users_version_and_the_labs_infrastructure() {
        assert_eq!(
            disposition_for_run_error(&RunError::NonAbsoluteRunRoot(PathBuf::from("relative"))),
            RunDisposition::InternalError
        );
        assert_eq!(
            disposition_for_run_error(&RunError::Workspace(ArtifactError::Io {
                operation: "create directory",
                path: PathBuf::from("/runs"),
                source: io::Error::other("disk full"),
            })),
            RunDisposition::InfrastructureFailed
        );
    }

    #[test]
    fn install_failures_are_installation_failures() {
        let failure = StackInstallFailure::Both {
            baseline: StackInstallError {
                component: Some("kyverno".to_owned()),
                message: "helm exited 1".to_owned(),
            },
            candidate: StackInstallError {
                component: None,
                message: "helm exited 1".to_owned(),
            },
        };
        assert_eq!(
            disposition_for_install_failure(&failure),
            RunDisposition::InstallationFailed
        );
    }

    #[test]
    fn capture_failures_are_fixture_failures() {
        assert_eq!(
            disposition_for_capture_failure(),
            RunDisposition::FixtureFailed
        );
    }

    #[test]
    fn normalize_errors_blame_the_tier_the_rule_came_from() {
        assert_eq!(
            disposition_for_normalize(&NormalizeError::RemovesDocumentRoot {
                tier: RuleTier::BuiltIn
            }),
            RunDisposition::InternalError
        );
        for tier in [RuleTier::Recipe, RuleTier::User] {
            assert_eq!(
                disposition_for_normalize(&NormalizeError::RemovesDocumentRoot { tier }),
                RunDisposition::InvalidInput,
                "a {tier} rule is configuration the user can fix"
            );
        }
    }

    #[test]
    fn a_policy_warning_still_passes_and_only_a_failure_is_exit_one() {
        assert_eq!(
            disposition_for_policy(PolicyDisposition::Pass),
            RunDisposition::Passed
        );
        assert_eq!(
            disposition_for_policy(PolicyDisposition::Warn),
            RunDisposition::Passed,
            "§0.4 has no warn code; pass-with-warnings is still a completed, passing run"
        );
        assert_eq!(
            code_for_disposition(disposition_for_policy(PolicyDisposition::Warn)),
            ExitCode::SUCCESS
        );
        assert_eq!(
            disposition_for_policy(PolicyDisposition::Fail),
            RunDisposition::PolicyFailed
        );
    }

    #[test]
    fn report_and_artifact_errors_split_between_our_bug_and_the_filesystem() {
        assert_eq!(
            disposition_for_report_error(&ReportError::Io {
                operation: "create temporary file",
                path: PathBuf::from("/artifacts/result.json"),
                source: io::Error::other("read-only file system"),
            }),
            RunDisposition::InfrastructureFailed
        );
        assert_eq!(
            disposition_for_artifact_error(&ArtifactError::PathEscapesRoot {
                path: PathBuf::from("/etc/passwd"),
                root: PathBuf::from("/runs"),
            }),
            RunDisposition::InternalError
        );
        assert_eq!(
            disposition_for_artifact_error(&ArtifactError::Io {
                operation: "write temporary file",
                path: PathBuf::from("/runs/x"),
                source: io::Error::other("disk full"),
            }),
            RunDisposition::InfrastructureFailed
        );
    }

    #[test]
    fn missing_host_prerequisites_are_invalid_input_and_a_good_host_is_no_failure() {
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
            // Present on purpose: a disk warning must not fail the gate.
            disk_warning: Some("only 1.0 GiB free".to_owned()),
        };
        assert_eq!(disposition_for_prerequisites(&good), None);

        let mut missing_kind = good.clone();
        missing_kind.tools[0].found = false;
        assert_eq!(
            disposition_for_prerequisites(&missing_kind),
            Some(RunDisposition::InvalidInput),
            "a missing host tool must agree with `admissionlab doctor`'s own exit code"
        );

        let mut docker_down = good;
        docker_down.docker_reachable = false;
        assert_eq!(
            disposition_for_prerequisites(&docker_down),
            Some(RunDisposition::InvalidInput)
        );
    }

    // -------------------------------------------------------------
    // Cleanup precedence
    // -------------------------------------------------------------

    #[test]
    fn a_failed_cleanup_never_leaves_a_run_reporting_success() {
        assert_eq!(
            after_failed_cleanup(RunDisposition::Passed),
            RunDisposition::InfrastructureFailed
        );
        assert_ne!(
            code_for_disposition(after_failed_cleanup(RunDisposition::Passed)),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn a_failed_cleanup_never_masks_an_already_failing_run() {
        for disposition in ALL_DISPOSITIONS {
            if disposition == RunDisposition::Passed {
                continue;
            }
            assert_eq!(
                after_failed_cleanup(disposition),
                disposition,
                "{disposition:?} is already non-zero and more specific than a cleanup failure"
            );
        }
    }
}
