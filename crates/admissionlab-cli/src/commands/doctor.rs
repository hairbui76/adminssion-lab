//! `admissionlab doctor` argument parsing and entry point.
//!
//! This is Task 1.4's shallow host-prerequisite check: probing `kind`,
//! `kubectl`, `helm`, and `docker` via [`admissionlab_core::collect_doctor_report`]
//! (the actual probing/parsing logic lives in `admissionlab-core::tool`,
//! not here — see that module's documentation), then rendering a summary
//! and choosing an exit code. The real `--deep` cluster-create/delete
//! probe is Task 1.9's; see [`DoctorArgs::deep`] and
//! [`deep_probe_notice`] for how this phase keeps that flag honest
//! without implementing it.
//!
//! # One check, two callers
//!
//! [`run_with`] calls `admissionlab_core::collect_doctor_report` and
//! [`DoctorReport::meets_prerequisites`] — the exact same functions a
//! future task wiring a prerequisite gate into `test` will call — rather
//! than re-implementing any probing or pass/fail logic here. This
//! module's own job is strictly the `doctor`-specific parts: argument
//! parsing, rendering, and choosing *this command's* exit code.
//!
//! # Honesty constraint on `--deep`
//!
//! [`DoctorArgs::deep`] exists on the parsed arguments at all only when
//! built with the `unstable-doctor-deep` Cargo feature (off by default,
//! so it is absent from every normal build including a release build —
//! see that feature's own documentation in `Cargo.toml`). [`run_with`]
//! never lets requesting it look like it succeeded: when
//! [`deep_probe_notice`] has something to say, that takes over the exit
//! code (mapped to [`RunDisposition::InternalError`], mirroring
//! `commands::not_implemented`'s existing convention for "Admission Lab
//! itself is not ready for this yet") even if every shallow check above
//! it passed, precisely so a caller checking only the exit code can
//! never mistake "shallow checks passed" for "the deep probe you asked
//! for actually ran."

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use admissionlab_core::{
    DoctorReport, ProcessRunner, RunDisposition, TokioProcessRunner, ToolName, ToolStatus,
    collect_doctor_report,
};
use clap::Args;

use crate::exit;

/// Arguments for `admissionlab doctor`.
#[derive(Debug, Default, Args)]
pub struct DoctorArgs {
    /// Additionally probe by creating and deleting a real ephemeral
    /// cluster.
    ///
    /// Only present at all when built with the (default-off)
    /// `unstable-doctor-deep` Cargo feature — see that feature's
    /// documentation in `Cargo.toml` for why. Task 1.9 implements the
    /// real probe and removes this feature gate; until then, passing
    /// this (in a build where it is reachable at all) never performs a
    /// deep probe and never lets the command look like it succeeded at
    /// one — see [`deep_probe_notice`].
    #[cfg(feature = "unstable-doctor-deep")]
    #[arg(long)]
    pub deep: bool,
}

/// Runs `admissionlab doctor`: probes host prerequisites via a real
/// [`TokioProcessRunner`], prints a summary to stdout, and returns the
/// process exit code for what was found.
///
/// Diagnostic only, per PRODUCT.md §34: this never writes to a
/// kubeconfig, switches a `kubectl` context, creates anything, or
/// otherwise mutates user or cluster state.
///
/// # Panics
///
/// Panics only if this process's own `tokio` runtime cannot be built at
/// all (for example, the OS refuses to create the underlying I/O
/// driver) — an environmental failure with no sensible fallback, not a
/// condition any host-prerequisite state can trigger.
#[must_use]
pub fn run(args: &DoctorArgs) -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build the doctor command's tokio runtime");
    let runner = TokioProcessRunner::new();
    let disk_check_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    runtime.block_on(run_with(&runner, &disk_check_path, args))
}

/// [`run`]'s testable core: takes an injected [`ProcessRunner`] and an
/// explicit disk-check path rather than constructing a
/// [`TokioProcessRunner`] or reading `std::env::current_dir()` itself, so
/// tests can exercise every outcome without spawning a real `kind`,
/// `kubectl`, `helm`, or `docker` and without depending on how much disk
/// space the test machine actually has free.
async fn run_with(
    runner: &dyn ProcessRunner,
    disk_check_path: &Path,
    args: &DoctorArgs,
) -> ExitCode {
    let mut report = collect_doctor_report(runner, disk_check_path).await;
    apply_kubectl_skew_warning(&mut report);
    let platform_diagnostic = platform_support_diagnostic(std::env::consts::OS);

    println!(
        "{}",
        render_summary(&report, platform_diagnostic.as_deref())
    );

    if let Some(notice) = deep_probe_notice(args) {
        eprintln!("{notice}");
        // Takes precedence over the shallow result below — see this
        // module's "Honesty constraint on `--deep`" documentation.
        return exit::code_for_disposition(RunDisposition::InternalError);
    }

    if report.meets_prerequisites() && platform_diagnostic.is_none() {
        exit::code_for_disposition(RunDisposition::Passed)
    } else {
        exit::code_for_disposition(RunDisposition::InvalidInput)
    }
}

// ---------------------------------------------------------------------
// `--deep`: honest even where it is reachable at all.
// ---------------------------------------------------------------------

/// Returns a stderr notice if `args` requested the deep cluster probe,
/// making explicit that Admission Lab does not perform it yet rather
/// than silently ignoring the request or letting the shallow checks
/// above look like they covered it. `None` whenever there is nothing to
/// report.
#[cfg(feature = "unstable-doctor-deep")]
fn deep_probe_notice(args: &DoctorArgs) -> Option<&'static str> {
    args.deep.then_some(
        "admissionlab doctor --deep: the deep cluster create/delete probe is not \
         implemented in this phase of Admission Lab (Task 1.9); only the checks above \
         were performed.",
    )
}

/// `--deep` does not exist on [`DoctorArgs`] at all without the
/// `unstable-doctor-deep` feature (see that field's `cfg`), so there is
/// never anything to report in a normal build. This fallback exists only
/// so [`run_with`] can call [`deep_probe_notice`] unconditionally
/// regardless of which build this is.
#[cfg(not(feature = "unstable-doctor-deep"))]
fn deep_probe_notice(_args: &DoctorArgs) -> Option<&'static str> {
    None
}

// ---------------------------------------------------------------------
// kubectl client/server minor-skew warning.
// ---------------------------------------------------------------------

/// Kubernetes tolerates only ±1 minor of client/server skew; see
/// PRODUCT.md §32 ("Kubernetes Compatibility Policy").
const MAX_SUPPORTED_SKEW: u32 = 1;

/// If `report` has a `kubectl` entry with a parsed version, appends an
/// advisory warning to its diagnostic when that version is more than
/// [`MAX_SUPPORTED_SKEW`] Kubernetes minors away from every
/// currently-supported entry in the checked-in compatibility matrix.
///
/// This compares against `admissionlab_cluster`'s compiled-in
/// `compatibility/kubernetes.yaml` matrix, not a user's lab
/// configuration: `doctor` may run with no config file at all (loading
/// one is a later task's job, out of scope here), and the matrix is
/// always available regardless. Silently does nothing if `kubectl` was
/// not probed, has no version, or the matrix fails to load — this is an
/// advisory nicety layered on a version `tool.rs` already reported
/// verbatim, not a second source of truth, so degrading gracefully here
/// never hides or duplicates that module's own "malformed version"
/// diagnostic.
fn apply_kubectl_skew_warning(report: &mut DoctorReport) {
    let Some(kubectl) = report
        .tools
        .iter_mut()
        .find(|tool| tool.name == ToolName::Kubectl)
    else {
        return;
    };
    let Some(version) = kubectl.version.clone() else {
        return;
    };
    let Ok(matrix) = admissionlab_cluster::load_matrix() else {
        return;
    };
    let Some(warning) = kubectl_skew_warning(&version, &matrix) else {
        return;
    };
    kubectl.diagnostic = Some(match kubectl.diagnostic.take() {
        Some(existing) => format!("{existing}; {warning}"),
        None => warning,
    });
}

/// Builds a warning message if `client_version` (kubectl's reported
/// `gitVersion`, for example `"v1.32.1"`) is more than
/// [`MAX_SUPPORTED_SKEW`] Kubernetes minors away from every
/// `supported: true` entry in `matrix`. Returns `None` both when the
/// skew is within tolerance and when `client_version` cannot be parsed —
/// never a fabricated distance for a version this cannot make sense of.
fn kubectl_skew_warning(
    client_version: &str,
    matrix: &admissionlab_cluster::KubernetesImageMatrix,
) -> Option<String> {
    let client = major_minor(client_version)?;
    let nearest = matrix
        .releases
        .iter()
        .filter(|release| release.supported)
        .filter_map(|release| major_minor(&release.minor))
        .map(|supported| minor_distance(client, supported))
        .min()?;
    if nearest <= MAX_SUPPORTED_SKEW {
        return None;
    }

    let supported_minors: Vec<&str> = matrix
        .releases
        .iter()
        .filter(|release| release.supported)
        .map(|release| release.minor.as_str())
        .collect();
    Some(format!(
        "kubectl client {client_version} is {nearest} Kubernetes minor versions away from \
         Admission Lab's supported range ({}); Kubernetes tolerates only \u{b1}{MAX_SUPPORTED_SKEW} \
         minor of client/server skew, which can produce confusing failures against a \
         provisioned cluster",
        supported_minors.join(", "),
    ))
}

/// Parses a Kubernetes version string (`"v1.32.1"`, or a bare minor like
/// `"1.36"`) into `(major, minor)`. Returns `None` for anything that
/// does not fit that shape, so a caller can degrade to "skip this check"
/// rather than fabricate a version.
fn major_minor(version: &str) -> Option<(u32, u32)> {
    let stripped = version.strip_prefix('v').unwrap_or(version);
    let mut parts = stripped.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// How many Kubernetes minor versions apart `client` is from `other`
/// (both `(major, minor)`). A major-version mismatch is reported as
/// [`u32::MAX`]: the compatibility matrix only ever lists Kubernetes 1.x
/// releases, so a major mismatch is always far outside any supported
/// skew, and this sentinel means every call site can treat "distance" as
/// one plain number rather than special-casing a major mismatch
/// separately.
fn minor_distance(client: (u32, u32), other: (u32, u32)) -> u32 {
    if client.0 != other.0 {
        return u32::MAX;
    }
    client.1.abs_diff(other.1)
}

// ---------------------------------------------------------------------
// Host platform support.
// ---------------------------------------------------------------------

/// Platforms Admission Lab is built and tested for — this repository's
/// own cross-compilation targets (see `rust-toolchain.toml`'s installed
/// targets: `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`,
/// `x86_64-pc-windows-msvc`) map to these three `std::env::consts::OS`
/// values.
///
/// `DoctorReport` has no field for this (Task 1.4's interface is fixed
/// to `tools`/`docker_reachable`/`disk_warning`), so the check lives
/// here rather than there; PRODUCT.md §34 asks `doctor` to *inspect*
/// host platform support, not that `DoctorReport` itself carry it.
const SUPPORTED_PLATFORMS: &[&str] = &["linux", "macos", "windows"];

/// Returns a diagnostic if `os` (in production, `std::env::consts::OS`)
/// is not one of [`SUPPORTED_PLATFORMS`]. Takes `os` as a parameter
/// rather than reading the constant itself so this is testable with any
/// value — `std::env::consts::OS` is fixed at compile time, so a test
/// running on Linux could otherwise never exercise the "unsupported"
/// branch at all.
fn platform_support_diagnostic(os: &str) -> Option<String> {
    if SUPPORTED_PLATFORMS.contains(&os) {
        None
    } else {
        Some(format!(
            "host platform {os:?} is not one Admission Lab supports (supported: {}); \
             kind/Docker-based clusters are unlikely to work here",
            SUPPORTED_PLATFORMS.join(", "),
        ))
    }
}

// ---------------------------------------------------------------------
// Rendering.
// ---------------------------------------------------------------------

/// Renders `report` (plus `platform_diagnostic`, if any) as the summary
/// `run_with` prints to stdout.
///
/// Returns a `String` rather than printing directly, so its exact
/// content is unit-testable without capturing process-wide stdout.
fn render_summary(report: &DoctorReport, platform_diagnostic: Option<&str>) -> String {
    let mut lines = vec!["Admission Lab doctor".to_owned()];

    lines.push(match platform_diagnostic {
        Some(diagnostic) => format!("  platform: {diagnostic}"),
        None => format!("  platform: {} (supported)", std::env::consts::OS),
    });

    lines.extend(
        report
            .tools
            .iter()
            .map(|tool| format!("  {}", render_tool_line(tool))),
    );

    lines.push(format!(
        "  docker daemon: {}",
        if report.docker_reachable {
            "reachable"
        } else {
            "unreachable"
        }
    ));

    if let Some(warning) = &report.disk_warning {
        lines.push(format!("  disk space: {warning}"));
    }

    lines.push(String::new());
    lines.push(
        if report.meets_prerequisites() && platform_diagnostic.is_none() {
            "All required prerequisites are met.".to_owned()
        } else {
            "Some required prerequisites are missing; see above.".to_owned()
        },
    );

    lines.join("\n")
}

/// Renders one [`ToolStatus`] as a single summary line.
fn render_tool_line(tool: &ToolStatus) -> String {
    let status = if tool.found { "found" } else { "NOT FOUND" };
    let version = tool.version.as_deref().unwrap_or("unknown");
    match &tool.diagnostic {
        Some(diagnostic) => format!("{}: {status} ({version}) - {diagnostic}", tool.name),
        None => format!("{}: {status} ({version})", tool.name),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::io;
    use std::os::unix::process::ExitStatusExt as _;
    use std::path::Path;
    use std::process::ExitStatus;
    use std::time::Duration;

    use admissionlab_cluster::{KubernetesImage, KubernetesImageMatrix};
    use admissionlab_core::{
        CommandResult, CommandSpec, ProcessError, ProcessRunner, RunDisposition,
    };
    use async_trait::async_trait;

    use super::*;
    use crate::exit;

    // -------------------------------------------------------------
    // Test scaffolding: mirrors `admissionlab-core`'s own
    // `tests/tool.rs` (hand-rolled runtime instead of `#[tokio::test]`,
    // a `ProcessRunner` fake keyed by program name).
    // -------------------------------------------------------------

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build test tokio runtime")
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        test_runtime().block_on(future)
    }

    #[derive(Clone)]
    enum FakeOutcome {
        Success(Vec<u8>),
        Failure { code: i32, stderr: Vec<u8> },
        Missing,
    }

    #[derive(Default)]
    struct FakeProcessRunner {
        outcomes: BTreeMap<String, FakeOutcome>,
    }

    impl FakeProcessRunner {
        /// All four tools present. Deliberately uses an in-range kubectl
        /// version (Tier 1's primary, per `compatibility/kubernetes.yaml`)
        /// so tests using this baseline are not also, incidentally,
        /// exercising the skew warning unless they override it.
        fn all_present() -> Self {
            Self::default()
                .with(
                    "kind",
                    FakeOutcome::Success(b"kind v0.33.0 go1.26.7 linux/amd64\n".to_vec()),
                )
                .with(
                    "kubectl",
                    FakeOutcome::Success(br#"{"clientVersion":{"gitVersion":"v1.36.4"}}"#.to_vec()),
                )
                .with("helm", FakeOutcome::Success(b"v3.15.2".to_vec()))
                .with("docker", FakeOutcome::Success(b"\"27.5.0\"\n".to_vec()))
        }

        fn with(mut self, program: &str, outcome: FakeOutcome) -> Self {
            self.outcomes.insert(program.to_owned(), outcome);
            self
        }
    }

    fn exit_status(code: i32) -> ExitStatus {
        ExitStatus::from_raw(code << 8)
    }

    #[async_trait]
    impl ProcessRunner for FakeProcessRunner {
        async fn run(&self, spec: CommandSpec) -> Result<CommandResult, ProcessError> {
            let program = spec.program.to_string_lossy().into_owned();
            match self.outcomes.get(&program) {
                Some(FakeOutcome::Success(stdout)) => Ok(CommandResult {
                    status: exit_status(0),
                    stdout: stdout.clone(),
                    stderr: Vec::new(),
                    elapsed: Duration::from_millis(1),
                }),
                Some(FakeOutcome::Failure { code, stderr }) => Ok(CommandResult {
                    status: exit_status(*code),
                    stdout: Vec::new(),
                    stderr: stderr.clone(),
                    elapsed: Duration::from_millis(1),
                }),
                Some(FakeOutcome::Missing) | None => Err(ProcessError::Spawn {
                    context: Box::new(spec.context()),
                    source: io::Error::new(io::ErrorKind::NotFound, "No such file or directory"),
                }),
            }
        }
    }

    fn sample_tool(
        name: ToolName,
        found: bool,
        version: Option<&str>,
        diagnostic: Option<&str>,
    ) -> ToolStatus {
        ToolStatus {
            name,
            found,
            version: version.map(str::to_owned),
            diagnostic: diagnostic.map(str::to_owned),
        }
    }

    fn fixture_matrix() -> KubernetesImageMatrix {
        KubernetesImageMatrix {
            releases: vec![
                KubernetesImage {
                    minor: "1.37".to_owned(),
                    version: "1.37.0".to_owned(),
                    image: "kindest/node:v1.37.0".to_owned(),
                    digest: "sha256:aaaa".to_owned(),
                    supported: true,
                },
                KubernetesImage {
                    minor: "1.36".to_owned(),
                    version: "1.36.4".to_owned(),
                    image: "kindest/node:v1.36.4".to_owned(),
                    digest: "sha256:bbbb".to_owned(),
                    supported: true,
                },
                KubernetesImage {
                    minor: "1.35".to_owned(),
                    version: "1.35.8".to_owned(),
                    image: "kindest/node:v1.35.8".to_owned(),
                    digest: "sha256:cccc".to_owned(),
                    supported: true,
                },
                KubernetesImage {
                    minor: "1.34".to_owned(),
                    version: "1.34.11".to_owned(),
                    image: "kindest/node:v1.34.11".to_owned(),
                    digest: "sha256:dddd".to_owned(),
                    supported: false,
                },
            ],
        }
    }

    // -------------------------------------------------------------
    // `run_with`: end-to-end through the real `admissionlab-core`
    // probing, with a fake `ProcessRunner`.
    // -------------------------------------------------------------

    #[test]
    fn run_with_all_present_exits_passed() {
        let runner = FakeProcessRunner::all_present();
        let code = block_on(run_with(&runner, Path::new("/"), &DoctorArgs::default()));
        assert_eq!(code, exit::code_for_disposition(RunDisposition::Passed));
    }

    #[test]
    fn run_with_missing_tool_exits_invalid_input() {
        let runner = FakeProcessRunner::all_present().with("kind", FakeOutcome::Missing);
        let code = block_on(run_with(&runner, Path::new("/"), &DoctorArgs::default()));
        assert_eq!(
            code,
            exit::code_for_disposition(RunDisposition::InvalidInput)
        );
    }

    #[test]
    fn run_with_docker_daemon_unreachable_exits_invalid_input() {
        let runner = FakeProcessRunner::all_present().with(
            "docker",
            FakeOutcome::Failure {
                code: 1,
                stderr: b"Cannot connect to the Docker daemon".to_vec(),
            },
        );
        let code = block_on(run_with(&runner, Path::new("/"), &DoctorArgs::default()));
        assert_eq!(
            code,
            exit::code_for_disposition(RunDisposition::InvalidInput)
        );
    }

    #[test]
    fn run_with_applies_the_kubectl_skew_warning_but_it_stays_advisory() {
        let runner = FakeProcessRunner::all_present().with(
            "kubectl",
            FakeOutcome::Success(br#"{"clientVersion":{"gitVersion":"v1.32.1"}}"#.to_vec()),
        );

        let mut report = block_on(collect_doctor_report(&runner, Path::new("/")));
        apply_kubectl_skew_warning(&mut report);
        let kubectl = report.tool(ToolName::Kubectl).unwrap();
        assert!(
            kubectl
                .diagnostic
                .as_deref()
                .is_some_and(|d| d.contains("minor"))
        );

        // Advisory only: the same scenario still exits `Passed` end to
        // end, proving a kubectl skew note never gates the exit code.
        let code = block_on(run_with(&runner, Path::new("/"), &DoctorArgs::default()));
        assert_eq!(code, exit::code_for_disposition(RunDisposition::Passed));
    }

    // -------------------------------------------------------------
    // `--deep`
    // -------------------------------------------------------------

    #[test]
    fn deep_probe_notice_is_none_by_default() {
        assert_eq!(deep_probe_notice(&DoctorArgs::default()), None);
    }

    #[cfg(feature = "unstable-doctor-deep")]
    #[test]
    fn deep_probe_notice_is_honest_when_requested() {
        let args = DoctorArgs { deep: true };
        let notice = deep_probe_notice(&args).expect("--deep must never be silently ignored");
        assert!(notice.contains("not implemented"));
        assert!(!notice.to_lowercase().contains("success"));
    }

    #[cfg(feature = "unstable-doctor-deep")]
    #[test]
    fn deep_flag_forces_internal_error_even_when_shallow_checks_pass() {
        let runner = FakeProcessRunner::all_present();
        let args = DoctorArgs { deep: true };
        let code = block_on(run_with(&runner, Path::new("/"), &args));
        assert_eq!(
            code,
            exit::code_for_disposition(RunDisposition::InternalError)
        );
    }

    // -------------------------------------------------------------
    // kubectl skew helpers.
    // -------------------------------------------------------------

    #[test]
    fn major_minor_parses_v_prefixed_and_bare_versions() {
        assert_eq!(major_minor("v1.32.1"), Some((1, 32)));
        assert_eq!(major_minor("1.36"), Some((1, 36)));
        assert_eq!(major_minor("1.36.4"), Some((1, 36)));
    }

    #[test]
    fn major_minor_rejects_unparseable_strings() {
        assert_eq!(major_minor(""), None);
        assert_eq!(major_minor("v1"), None);
        assert_eq!(major_minor("vX.Y"), None);
    }

    #[test]
    fn minor_distance_is_zero_for_identical_minors() {
        assert_eq!(minor_distance((1, 36), (1, 36)), 0);
    }

    #[test]
    fn minor_distance_counts_minors_within_a_major() {
        assert_eq!(minor_distance((1, 32), (1, 36)), 4);
        assert_eq!(minor_distance((1, 36), (1, 32)), 4);
    }

    #[test]
    fn minor_distance_is_max_for_a_major_mismatch() {
        assert_eq!(minor_distance((2, 0), (1, 36)), u32::MAX);
    }

    #[test]
    fn kubectl_skew_warning_fires_far_outside_the_supported_range() {
        let warning = kubectl_skew_warning("v1.32.1", &fixture_matrix());
        assert!(warning.is_some_and(|message| message.contains("minor")));
    }

    #[test]
    fn kubectl_skew_warning_is_none_within_one_minor_of_support() {
        assert_eq!(kubectl_skew_warning("v1.36.4", &fixture_matrix()), None);
        assert_eq!(kubectl_skew_warning("v1.37.0", &fixture_matrix()), None);
        assert_eq!(kubectl_skew_warning("v1.35.0", &fixture_matrix()), None);
    }

    #[test]
    fn kubectl_skew_warning_measures_against_supported_minors_only() {
        // 1.34 is listed but `supported: false`; 1.34.11 is exactly one
        // minor from 1.35 (supported), so this must not warn even though
        // 1.34 itself is retired.
        assert_eq!(kubectl_skew_warning("v1.34.11", &fixture_matrix()), None);
    }

    #[test]
    fn kubectl_skew_warning_is_none_for_an_unparseable_client_version() {
        assert_eq!(
            kubectl_skew_warning("not-a-version", &fixture_matrix()),
            None
        );
    }

    // -------------------------------------------------------------
    // Host platform support.
    // -------------------------------------------------------------

    #[test]
    fn platform_support_diagnostic_is_none_for_every_supported_platform() {
        for os in SUPPORTED_PLATFORMS {
            assert_eq!(platform_support_diagnostic(os), None);
        }
    }

    #[test]
    fn platform_support_diagnostic_flags_an_unsupported_platform() {
        let diagnostic = platform_support_diagnostic("plan9");
        assert!(diagnostic.is_some_and(|message| message.contains("plan9")));
    }

    // -------------------------------------------------------------
    // Rendering.
    // -------------------------------------------------------------

    #[test]
    fn render_summary_lists_every_tool_and_notes_docker_reachability() {
        let report = DoctorReport {
            tools: vec![
                sample_tool(ToolName::Kind, true, Some("v0.33.0"), None),
                sample_tool(ToolName::Kubectl, true, Some("v1.36.4"), None),
                sample_tool(ToolName::Helm, true, Some("v3.15.2"), None),
                sample_tool(ToolName::Docker, true, Some("27.5.0"), None),
            ],
            docker_reachable: true,
            disk_warning: None,
        };

        let summary = render_summary(&report, None);
        for expected in [
            "kind",
            "v0.33.0",
            "kubectl",
            "v1.36.4",
            "helm",
            "v3.15.2",
            "docker",
            "27.5.0",
            "reachable",
        ] {
            assert!(
                summary.contains(expected),
                "summary missing {expected:?}:\n{summary}"
            );
        }
        assert!(summary.contains("All required prerequisites are met"));
    }

    #[test]
    fn render_summary_reports_a_missing_tool_and_the_platform_diagnostic() {
        let report = DoctorReport {
            tools: vec![sample_tool(
                ToolName::Kind,
                false,
                None,
                Some("kind was not usable: not found"),
            )],
            docker_reachable: false,
            disk_warning: Some(
                "only 1.0 GiB free at / (below the 10.0 GiB warning threshold)".to_owned(),
            ),
        };

        let summary = render_summary(&report, Some("host platform \"plan9\" is not supported"));
        assert!(summary.contains("NOT FOUND"));
        assert!(summary.contains("plan9"));
        assert!(summary.contains("1.0 GiB"));
        assert!(summary.contains("Some required prerequisites are missing"));
    }
}
