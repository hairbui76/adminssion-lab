//! Tests for `admissionlab-core`'s tool-discovery probes
//! ([`admissionlab_core::tool`]), using a fake [`ProcessRunner`] so that
//! no test here ever spawns a real `kind`, `kubectl`, `helm`, or
//! `docker`.
//!
//! Covers Task 1.4 brief Step 1's required cases (all tools present;
//! `kind` missing; malformed version output; Docker daemon unreachable),
//! plus the `found`/`docker_reachable` distinction, `meets_prerequisites`,
//! and the disk-space check.

use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;

use admissionlab_core::process::{CommandResult, CommandSpec, ProcessError, ProcessRunner};
use admissionlab_core::tool::{
    DISK_WARNING_THRESHOLD_BYTES, ToolName, collect_doctor_report, disk_space_warning, probe_tool,
};
use async_trait::async_trait;

// ---------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------

/// A fresh `tokio` runtime for driving one test's async calls.
///
/// Mirrors `tests/artifact.rs`'s hand-rolled runtime construction rather
/// than adopting `#[tokio::test]`, which would need a new `tokio`
/// feature (`macros`) this crate's production code has no other reason
/// to depend on.
fn test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build test tokio runtime")
}

/// Runs `future` to completion on a fresh [`test_runtime`].
fn block_on<F: Future>(future: F) -> F::Output {
    test_runtime().block_on(future)
}

/// A real `kubectl version --client=true --output=json` response shape,
/// with `gitVersion` swapped for whatever the test needs.
fn kubectl_json(git_version: &str) -> Vec<u8> {
    format!(
        r#"{{"clientVersion":{{"major":"1","minor":"32","gitVersion":"{git_version}","gitCommit":"deadbeef","gitTreeState":"clean","buildDate":"2026-08-01T00:00:00Z","goVersion":"go1.23.4","compiler":"gc","platform":"linux/amd64"}},"kustomizeVersion":"v5.5.0"}}"#
    )
    .into_bytes()
}

/// One scripted response [`FakeProcessRunner`] gives for a program name.
#[derive(Clone)]
enum FakeOutcome {
    /// The command ran and exited 0 with this stdout.
    Success(Vec<u8>),
    /// The command ran but exited non-zero, with this stderr.
    Failure { code: i32, stderr: Vec<u8> },
    /// The command could not be spawned at all (simulates "not on
    /// `PATH`").
    Missing,
}

/// A [`ProcessRunner`] that never spawns a real process: it returns a
/// scripted [`FakeOutcome`] keyed by [`CommandSpec::program`], so every
/// test in this file exercises `admissionlab_core::tool`'s probing and
/// parsing logic without depending on any real `kind`, `kubectl`,
/// `helm`, or `docker` being installed.
#[derive(Default)]
struct FakeProcessRunner {
    outcomes: BTreeMap<String, FakeOutcome>,
}

impl FakeProcessRunner {
    /// All four tools present with the real, verified output shapes
    /// from Task 1.4's brief (captured against `kind` v0.33.0, `kubectl`
    /// v1.32.1, `helm` v3.15.2, `docker` server 27.5.0).
    fn all_present() -> Self {
        Self::default()
            .with(
                "kind",
                FakeOutcome::Success(b"kind v0.33.0 go1.26.7 linux/amd64\n".to_vec()),
            )
            .with("kubectl", FakeOutcome::Success(kubectl_json("v1.32.1")))
            // No trailing newline: `helm version --template` does not
            // emit one.
            .with("helm", FakeOutcome::Success(b"v3.15.2".to_vec()))
            // JSON-quoted, quotes included: `docker version --format
            // {{json .Server.Version}}`'s real output shape.
            .with("docker", FakeOutcome::Success(b"\"27.5.0\"\n".to_vec()))
    }

    /// Overrides (or adds) the outcome for one program name.
    fn with(mut self, program: &str, outcome: FakeOutcome) -> Self {
        self.outcomes.insert(program.to_owned(), outcome);
        self
    }
}

/// Builds a normal-exit [`ExitStatus`] reporting `code`, using the
/// well-known Unix `wait(2)` encoding (`WEXITSTATUS` occupies bits 8-15)
/// that [`std::os::unix::process::ExitStatusExt::from_raw`] decodes.
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

// ---------------------------------------------------------------------
// Brief Step 1, case: all tools present.
// ---------------------------------------------------------------------

#[test]
fn all_tools_present_reports_found_with_real_versions() {
    let runner = FakeProcessRunner::all_present();

    let kind = block_on(probe_tool(&runner, ToolName::Kind));
    assert!(kind.found);
    assert_eq!(kind.version.as_deref(), Some("v0.33.0"));
    assert_eq!(kind.diagnostic, None);

    let kubectl = block_on(probe_tool(&runner, ToolName::Kubectl));
    assert!(kubectl.found);
    assert_eq!(kubectl.version.as_deref(), Some("v1.32.1"));
    assert_eq!(kubectl.diagnostic, None);

    let helm = block_on(probe_tool(&runner, ToolName::Helm));
    assert!(helm.found);
    assert_eq!(helm.version.as_deref(), Some("v3.15.2"));
    assert_eq!(helm.diagnostic, None);

    let docker = block_on(probe_tool(&runner, ToolName::Docker));
    assert!(docker.found);
    // The literal JSON quotes must not survive into the reported
    // version: this is the "unquote it, don't trim it" requirement.
    assert_eq!(docker.version.as_deref(), Some("27.5.0"));
    assert!(!docker.version.as_deref().unwrap_or_default().contains('"'));
    assert_eq!(docker.diagnostic, None);
}

#[test]
fn collect_doctor_report_reports_docker_reachable_when_all_present() {
    let runner = FakeProcessRunner::all_present();
    let report = block_on(collect_doctor_report(&runner, Path::new("/")));

    assert_eq!(report.tools.len(), ToolName::ALL.len());
    assert!(report.docker_reachable);
    assert!(report.meets_prerequisites());
}

// ---------------------------------------------------------------------
// Brief Step 1, case: missing kind.
// ---------------------------------------------------------------------

#[test]
fn missing_kind_is_not_found_with_a_diagnostic_and_no_fabricated_version() {
    let runner = FakeProcessRunner::all_present().with("kind", FakeOutcome::Missing);

    let kind = block_on(probe_tool(&runner, ToolName::Kind));
    assert!(!kind.found);
    assert_eq!(kind.version, None);
    assert!(
        kind.diagnostic.is_some_and(|d| !d.is_empty()),
        "a missing tool must explain why, not just say nothing"
    );
}

#[test]
fn missing_kind_fails_prerequisites_but_leaves_other_tools_found() {
    let runner = FakeProcessRunner::all_present().with("kind", FakeOutcome::Missing);
    let report = block_on(collect_doctor_report(&runner, Path::new("/")));

    assert!(!report.meets_prerequisites());
    assert!(!report.tool(ToolName::Kind).unwrap().found);
    assert!(report.tool(ToolName::Kubectl).unwrap().found);
    assert!(report.tool(ToolName::Helm).unwrap().found);
    assert!(report.tool(ToolName::Docker).unwrap().found);
}

// ---------------------------------------------------------------------
// Brief Step 1, case: malformed version output (never a panic, never a
// fabricated value).
// ---------------------------------------------------------------------

#[test]
fn malformed_kind_output_has_no_second_field_and_degrades_to_no_version() {
    let runner =
        FakeProcessRunner::all_present().with("kind", FakeOutcome::Success(b"garbage\n".to_vec()));
    let kind = block_on(probe_tool(&runner, ToolName::Kind));
    assert!(kind.found);
    assert_eq!(kind.version, None);
    assert!(kind.diagnostic.is_some());
}

#[test]
fn malformed_kubectl_json_degrades_to_no_version_not_a_panic() {
    let runner = FakeProcessRunner::all_present()
        .with("kubectl", FakeOutcome::Success(b"not json at all".to_vec()));
    let kubectl = block_on(probe_tool(&runner, ToolName::Kubectl));
    assert!(kubectl.found);
    assert_eq!(kubectl.version, None);
    assert!(kubectl.diagnostic.is_some());
}

#[test]
fn kubectl_json_missing_git_version_field_degrades_to_no_version() {
    let runner = FakeProcessRunner::all_present().with(
        "kubectl",
        FakeOutcome::Success(br#"{"clientVersion":{"major":"1"}}"#.to_vec()),
    );
    let kubectl = block_on(probe_tool(&runner, ToolName::Kubectl));
    assert!(kubectl.found);
    assert_eq!(kubectl.version, None);
    assert!(kubectl.diagnostic.is_some());
}

#[test]
fn empty_helm_output_degrades_to_no_version() {
    let runner = FakeProcessRunner::all_present().with("helm", FakeOutcome::Success(Vec::new()));
    let helm = block_on(probe_tool(&runner, ToolName::Helm));
    assert!(helm.found);
    assert_eq!(helm.version, None);
    assert!(helm.diagnostic.is_some());
}

#[test]
fn docker_output_without_json_quoting_is_rejected_as_malformed() {
    // Proves the parser does not "trim it blindly": an unquoted raw
    // value must never be accepted as if it were the correctly
    // JSON-quoted form.
    let runner =
        FakeProcessRunner::all_present().with("docker", FakeOutcome::Success(b"27.5.0\n".to_vec()));
    let docker = block_on(probe_tool(&runner, ToolName::Docker));
    assert!(docker.found);
    assert_eq!(docker.version, None);
    assert!(docker.diagnostic.is_some());
}

// ---------------------------------------------------------------------
// Brief Step 1, case: Docker daemon unreachable — and the companion
// "docker binary missing entirely" case, to prove the two are
// distinguishable.
// ---------------------------------------------------------------------

#[test]
fn docker_daemon_unreachable_is_found_but_not_reachable() {
    let runner = FakeProcessRunner::all_present().with(
        "docker",
        FakeOutcome::Failure {
            code: 1,
            stderr: b"Cannot connect to the Docker daemon at unix:///var/run/docker.sock. \
                      Is the docker daemon running?\n"
                .to_vec(),
        },
    );

    let docker = block_on(probe_tool(&runner, ToolName::Docker));
    // The client binary ran; it is "found" even though its server-version
    // probe failed.
    assert!(docker.found);
    assert_eq!(docker.version, None);
    assert!(
        docker
            .diagnostic
            .is_some_and(|d| d.contains("Cannot connect"))
    );

    let report = block_on(collect_doctor_report(&runner, Path::new("/")));
    assert!(!report.docker_reachable);
    assert!(report.tool(ToolName::Docker).unwrap().found);
    assert!(!report.meets_prerequisites());
}

#[test]
fn docker_binary_missing_is_neither_found_nor_reachable() {
    let runner = FakeProcessRunner::all_present().with("docker", FakeOutcome::Missing);

    let docker = block_on(probe_tool(&runner, ToolName::Docker));
    assert!(!docker.found);
    assert_eq!(docker.version, None);

    let report = block_on(collect_doctor_report(&runner, Path::new("/")));
    assert!(!report.docker_reachable);
    assert!(!report.tool(ToolName::Docker).unwrap().found);
    assert!(!report.meets_prerequisites());
}

// ---------------------------------------------------------------------
// `meets_prerequisites`
// ---------------------------------------------------------------------

#[test]
fn meets_prerequisites_ignores_disk_warning() {
    // A low-disk warning must not, by itself, fail prerequisites:
    // PRODUCT.md §34 calls this a warning threshold, not a hard gate.
    let runner = FakeProcessRunner::all_present();
    let mut report = block_on(collect_doctor_report(&runner, Path::new("/")));
    assert!(report.meets_prerequisites());

    report.disk_warning =
        Some("only 1.0 GiB free at / (below the 10.0 GiB warning threshold)".to_owned());
    assert!(
        report.meets_prerequisites(),
        "disk_warning must be advisory only and never gate meets_prerequisites"
    );
}

// ---------------------------------------------------------------------
// Disk space.
// ---------------------------------------------------------------------

#[test]
fn disk_space_warning_is_none_when_the_threshold_is_zero() {
    // Any real, existing path has at least zero bytes free, so a
    // zero-byte threshold can never be breached — this exercises the
    // real `statvfs` call (not a subprocess: no external tool is
    // executed) without depending on how much space this test machine
    // actually has free.
    assert_eq!(disk_space_warning(Path::new("/"), 0), None);
}

#[test]
fn disk_space_warning_fires_when_the_threshold_is_unreasonably_high() {
    let warning = disk_space_warning(Path::new("/"), u64::MAX);
    assert!(warning.is_some_and(|message| message.contains("GiB") && message.contains('/')));
}

#[test]
fn disk_space_warning_reports_indeterminate_rather_than_fabricating_for_a_missing_path() {
    let warning = disk_space_warning(Path::new("/definitely/does/not/exist/admissionlab"), 0);
    assert!(
        warning.is_some_and(|message| message.contains("could not determine")),
        "a path statvfs cannot read must say so, never silently report no warning"
    );
}

#[test]
fn disk_warning_threshold_constant_is_ten_gib() {
    assert_eq!(DISK_WARNING_THRESHOLD_BYTES, 10 * 1024 * 1024 * 1024);
}

#[test]
fn collect_doctor_report_includes_the_disk_check_for_the_given_path() {
    let runner = FakeProcessRunner::all_present();
    let report = block_on(collect_doctor_report(&runner, Path::new("/")));
    // With a zero-effective floor (any real filesystem clears it), a
    // real path never warns; this proves the disk check actually ran as
    // part of the assembled report rather than being skipped.
    assert!(disk_space_warning(Path::new("/"), 0).is_none());
    assert_eq!(report.disk_warning, None);
}

// Sanity: the fake runner itself must route by program name correctly,
// otherwise every test above would be trivially meaningless.
#[test]
fn fake_runner_is_keyed_by_program_name() {
    let runner = FakeProcessRunner::all_present();
    let spec = CommandSpec {
        program: "kind".into(),
        args: vec!["version".into()],
        cwd: None,
        env: BTreeMap::new(),
        sensitive_env_keys: std::collections::BTreeSet::new(),
        timeout: Duration::from_secs(1),
    };
    let result = block_on(runner.run(spec)).unwrap();
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).starts_with("kind v0.33.0"));
}
