//! `admissionlab doctor` argument parsing and entry point.
//!
//! Two layers: Task 1.4's shallow host-prerequisite check (probing
//! `kind`, `kubectl`, `helm`, and `docker` via
//! [`admissionlab_core::collect_doctor_report`] — the actual
//! probing/parsing logic lives in `admissionlab-core::tool`, not here —
//! then rendering a summary and choosing an exit code), and Task 1.9's
//! real `--deep` probe: create one temporary cluster through
//! `admissionlab_cluster::KindClusterManager`, verify its API health and
//! that its audit log exists, then delete it — see [`run_deep_probe`]
//! and [`DeepProbeGuard`].
//!
//! # One check, two callers
//!
//! [`run_with`] calls `admissionlab_core::collect_doctor_report` and
//! [`DoctorReport::meets_prerequisites`] — the exact same functions
//! `admissionlab test`'s own prerequisite gate calls (Task 4.14; see
//! `crate::pipeline`'s stage order and `crate::exit`'s note on why a
//! missing host tool is exit `2` in *both* commands) — rather than
//! re-implementing any probing or pass/fail logic here. This module's
//! own job is strictly the `doctor`-specific parts: argument parsing,
//! rendering, choosing *this command's* exit code, and (only when
//! `--deep` is passed) the temporary-cluster probe itself.
//!
//! # `--deep`: only when asked, and never touching user state
//!
//! [`DoctorArgs::deep`] defaults to `false`, so a plain `admissionlab
//! doctor` never creates anything. When passed, [`run_deep_probe`]
//! builds its own fresh, isolated temporary workspace under the OS temp
//! directory ([`fresh_deep_probe_workspace`]) — never the location a lab
//! configuration would use, and never anywhere near `~/.kube/config` or
//! the user's current `kubectl` context (PRODUCT.md §34: diagnostic
//! only, must not mutate production contexts). A `--deep` failure always
//! maps to [`RunDisposition::InternalError`], regardless of whether the
//! shallow checks above it passed, so a caller checking only the exit
//! code can never mistake "shallow checks passed" for "the deep probe
//! you asked for actually succeeded" — the same honesty discipline
//! `commands::test` applies to its own, larger surface (see that
//! module's documentation).
//!
//! # Cleanup: explicit, with `Drop` only as a last-resort warning
//!
//! [`DeepProbeGuard`] mirrors the exact discipline
//! `admissionlab-cluster`'s `tests/kind_smoke.rs` integration test uses
//! for its own two-cluster guard (see that file's module documentation
//! for the full reasoning): dropping a guard cannot reliably perform
//! async cleanup (`Drop` cannot be `async`, and blocking a runtime from
//! inside `drop` risks a panic or a deadlock), so [`run_deep_probe`]
//! always calls [`DeepProbeGuard::cleanup`] explicitly — after the
//! health/audit-log checks, regardless of what they found — and `Drop`
//! only emits a warning containing the exact `kind delete cluster --name
//! <name>` command if a handle still survives.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use admissionlab_cluster::{KindClusterManager, cluster_name, load_matrix, resolve_node_image};
use admissionlab_core::{
    ArtifactStore, ClusterError, ClusterHandle, ClusterManager, ClusterSpec, CommandSpec,
    DoctorReport, ProcessRunner, RunDisposition, RunId, RunPaths, Side, TokioProcessRunner,
    ToolName, ToolStatus, collect_doctor_report,
};
use clap::Args;

use crate::exit;

/// Arguments for `admissionlab doctor`.
#[derive(Debug, Default, Args)]
pub struct DoctorArgs {
    /// Additionally probe by creating a real ephemeral cluster,
    /// verifying its API health and that its audit log exists, then
    /// deleting it (Task 1.9). Off by default: a plain `admissionlab
    /// doctor` never creates anything, and this is only performed when
    /// the flag is passed explicitly.
    #[arg(long)]
    pub deep: bool,
}

/// Runs `admissionlab doctor`: probes host prerequisites via a real
/// [`TokioProcessRunner`], prints a summary to stdout, and returns the
/// process exit code for what was found.
///
/// Diagnostic only, per PRODUCT.md §34: this never writes to a
/// kubeconfig, switches a `kubectl` context, or otherwise mutates user
/// state. When `--deep` is passed it does create and delete one
/// temporary cluster (see [`run_deep_probe`]) — that is the one
/// documented exception PRODUCT.md §34 itself allows ("ability to create
/// a temporary test cluster in an optional deep-check mode"), and it is
/// always cleaned up before this returns.
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
    let runner: Arc<dyn ProcessRunner> = Arc::new(TokioProcessRunner::new());
    let disk_check_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    runtime.block_on(run_with(runner, &disk_check_path, args))
}

/// [`run`]'s testable core: takes an injected [`ProcessRunner`] (owned,
/// as an `Arc`, so it can also be handed to the `--deep` probe's own
/// `KindClusterManager` without a second construction) and an explicit
/// disk-check path rather than constructing a [`TokioProcessRunner`] or
/// reading `std::env::current_dir()` itself, so tests can exercise every
/// outcome without spawning a real `kind`, `kubectl`, `helm`, or `docker`
/// and without depending on how much disk space the test machine
/// actually has free.
async fn run_with(
    runner: Arc<dyn ProcessRunner>,
    disk_check_path: &Path,
    args: &DoctorArgs,
) -> ExitCode {
    let mut report = collect_doctor_report(runner.as_ref(), disk_check_path).await;
    apply_kubectl_skew_warning(&mut report);
    let platform_diagnostic = platform_support_diagnostic(std::env::consts::OS);
    // Computed exactly once and threaded into `render_summary` below,
    // rather than each recomputing `meets_prerequisites() &&
    // platform_diagnostic.is_none()` independently: two copies of the
    // same condition can drift if one is edited and not the other,
    // which would let the printed summary and the process exit code
    // disagree with each other.
    let prerequisites_met = report.meets_prerequisites() && platform_diagnostic.is_none();

    println!(
        "{}",
        render_summary(&report, platform_diagnostic.as_deref(), prerequisites_met)
    );

    if !args.deep {
        return if prerequisites_met {
            exit::code_for_disposition(RunDisposition::Passed)
        } else {
            exit::code_for_disposition(RunDisposition::InvalidInput)
        };
    }

    let deep_result = match fresh_deep_probe_workspace().await {
        Ok((run_id, paths)) => run_deep_probe(runner, &run_id, &paths).await,
        Err(reason) => Err(reason),
    };
    println!(
        "{}",
        render_deep_probe_summary(deep_result.as_ref().err().map(String::as_str))
    );

    match deep_result {
        // A `--deep` success still respects the shallow prerequisites
        // (for example an unsupported host platform): succeeding at
        // creating one temporary cluster today does not retroactively
        // make an otherwise-flagged host fully supported.
        Ok(()) if prerequisites_met => exit::code_for_disposition(RunDisposition::Passed),
        Ok(()) => exit::code_for_disposition(RunDisposition::InvalidInput),
        Err(_) => exit::code_for_disposition(RunDisposition::InternalError),
    }
}

// ---------------------------------------------------------------------
// `--deep`: a real, temporary create/verify/delete cluster probe.
// ---------------------------------------------------------------------

/// The Kubernetes version `--deep` provisions its temporary cluster at:
/// Tier 1's primary supported version per `compatibility/kubernetes.yaml`
/// (see that file's own comments). Resolved through
/// [`resolve_node_image`] rather than a second hardcoded pinned image, so
/// this stays in sync with the checked-in matrix instead of silently
/// drifting from whatever it actually resolves to.
const PRIMARY_KUBERNETES_VERSION: &str = "1.36.4";

/// A narrow, `doctor`-local mirror of the same "explicit cleanup is
/// primary, `Drop` only warns" discipline
/// `admissionlab-cluster`'s `tests/kind_smoke.rs` integration test uses
/// for its own two-cluster guard (see that file's module documentation
/// for the full reasoning) — applied here to the single temporary
/// cluster `--deep` creates. Not shared code across the two crates on
/// purpose: each guard is small and crate-local, and a new public type in
/// `admissionlab-cluster` reachable from both would be a larger surface
/// than this narrowly-scoped safety net needs.
struct DeepProbeGuard {
    handle: Option<ClusterHandle>,
}

impl DeepProbeGuard {
    fn new(handle: ClusterHandle) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    /// Deletes the guarded cluster through `manager`. On success, clears
    /// the tracked handle so `Drop` stays silent; on failure, leaves it
    /// in place so `Drop`'s warning still fires with the exact manual
    /// recovery command — a failed cleanup attempt must never look like a
    /// successful one.
    async fn cleanup(&mut self, manager: &KindClusterManager) -> Result<(), ClusterError> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        match manager.delete(&handle).await {
            Ok(()) => Ok(()),
            Err(error) => {
                self.handle = Some(handle);
                Err(error)
            }
        }
    }
}

impl Drop for DeepProbeGuard {
    /// Never performs async cleanup — see this module's "Cleanup:
    /// explicit, with `Drop` only as a last-resort warning" documentation.
    /// Only a synchronous, best-effort warning naming the exact command a
    /// user can paste to delete a cluster this guard never confirmed was
    /// deleted.
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            let name = &handle.spec.name;
            eprintln!(
                "warning: doctor --deep's temporary cluster {name:?} was not confirmed \
                 deleted; if it still exists, delete it manually with: \
                 kind delete cluster --name {name}"
            );
        }
    }
}

/// Builds a fresh, isolated temporary workspace for one `--deep`
/// invocation, under the OS temp directory — never the user's own
/// configured run/cache root (`doctor` may run with no
/// `admissionlab.yaml` at all) and never anywhere near
/// `~/.kube/config` (PRODUCT.md §34: diagnostic only).
async fn fresh_deep_probe_workspace() -> Result<(RunId, RunPaths), String> {
    let run_id = RunId::generate();
    let root = std::env::temp_dir().join("admissionlab-doctor-deep");
    let store = ArtifactStore::new(&root);
    let paths = store.create_run(&run_id).await.map_err(|error| {
        format!("failed to prepare a temporary workspace for the deep check: {error}")
    })?;
    Ok((run_id, paths))
}

/// Creates one temporary cluster, verifies API health and audit-log
/// existence, then deletes it (Task 1.9 brief Step 3; PRODUCT.md §34's
/// "ability to create a temporary test cluster in an optional deep-check
/// mode"). Every path after a successful create — whichever checks pass
/// or fail — funnels through [`DeepProbeGuard::cleanup`] before
/// returning, so no ordinary failure here leaves the temporary cluster
/// behind (PRODUCT.md §33).
///
/// Takes `process_runner`, `run_id`, and `paths` as parameters (rather
/// than constructing them itself) so tests can exercise every outcome
/// with a fake [`ProcessRunner`] and a controlled temporary workspace,
/// mirroring [`run_with`]'s own injected `disk_check_path`.
async fn run_deep_probe(
    process_runner: Arc<dyn ProcessRunner>,
    run_id: &RunId,
    paths: &RunPaths,
) -> Result<(), String> {
    let manager = KindClusterManager::new(Arc::clone(&process_runner));

    let matrix = load_matrix()
        .map_err(|error| format!("failed to load the Kubernetes compatibility matrix: {error}"))?;
    let resolved = resolve_node_image(PRIMARY_KUBERNETES_VERSION, &matrix)
        .map_err(|error| format!("failed to resolve the deep-check node image: {error}"))?;
    // `Side::Baseline` is an arbitrary but harmless choice: this cluster
    // is not part of a baseline/candidate comparison at all, and
    // `ClusterSpec` has no third "diagnostic" side to name instead. It
    // exists for seconds and is always deleted before `--deep` returns.
    let name = cluster_name(Side::Baseline, run_id)
        .map_err(|error| format!("failed to build a temporary cluster name: {error}"))?;
    let spec = ClusterSpec {
        side: Side::Baseline,
        name,
        kubernetes_version: resolved.version,
        node_image: resolved.pinned_image,
        // Nothing to side-load: this cluster runs no workload at all,
        // it only proves one can be created and reached.
        images: Vec::new(),
    };

    let handle = manager
        .create(&spec, paths)
        .await
        .map_err(|error| format!("failed to create a temporary cluster: {error}"))?;
    let mut guard = DeepProbeGuard::new(handle.clone());

    let mut problems = Vec::new();
    if let Err(reason) = api_health(process_runner.as_ref(), &handle.kubeconfig).await {
        problems.push(format!("API health check failed: {reason}"));
    }

    let diagnostics = manager.diagnostics(&handle).await;
    if !diagnostics.audit_log_present {
        problems.push(format!(
            "audit log not found at {}",
            handle.audit_log.display()
        ));
    }

    if let Err(error) = guard.cleanup(&manager).await {
        problems.push(format!("failed to delete the temporary cluster: {error}"));
    }

    // Best-effort only: deleting the cluster does not remove this run's
    // own host-side workspace (its kubeconfig, rendered kind config, and
    // audit log file all live under `paths.root()`, not inside the
    // deleted container). That workspace is disposable scratch space for
    // `--deep`'s own use, not evidence a caller needs kept -- unlike the
    // cluster delete above, a failure here is not itself a probe
    // failure, and it matters more here than for a one-off test run:
    // `--deep` is a normal diagnostic a user may run repeatedly while
    // troubleshooting, so leaving it unswept would otherwise accumulate
    // small files under the OS temp directory indefinitely.
    let _ = tokio::fs::remove_dir_all(paths.root()).await;

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Checks kube-apiserver health for the cluster at `kubeconfig` via
/// `kubectl --kubeconfig <path> get --raw=/healthz`, run through
/// `runner` — this project's own [`ProcessRunner`], never a direct shell
/// call. Kubernetes's `/healthz` endpoint returns HTTP 200 (surfaced by
/// `kubectl --raw` as a zero exit) when the API server itself is
/// healthy; a non-zero exit alone is a reliable enough signal without
/// also needing to pattern-match the exact response body, which is not
/// guaranteed byte-for-byte across Kubernetes versions.
async fn api_health(runner: &dyn ProcessRunner, kubeconfig: &Path) -> Result<(), String> {
    let spec = CommandSpec {
        program: "kubectl".into(),
        args: vec![
            "--kubeconfig".into(),
            kubeconfig.as_os_str().to_owned(),
            "get".into(),
            "--raw=/healthz".into(),
        ],
        cwd: None,
        env: BTreeMap::new(),
        sensitive_env_keys: BTreeSet::new(),
        timeout: Duration::from_secs(30),
        // No spill directory: `/healthz` answers with a single word,
        // and `doctor` runs before (and independently of) any run, so
        // there is no `RunPaths::logs` to write into.
        spill_dir: None,
    };
    let result = runner
        .run(spec)
        .await
        .map_err(|error| format!("kubectl could not be run: {error}"))?;
    if result.status.success() {
        Ok(())
    } else {
        Err(format!(
            // A bounded tail (Task 9.4 Step 3): this string is printed
            // to the operator's terminal and put in a `DoctorReport`.
            "kubectl get --raw=/healthz exited with {}: {}",
            result.status,
            admissionlab_core::output_tail(&result.stderr).trim()
        ))
    }
}

/// Renders the one-line summary [`run_with`] prints after a `--deep`
/// attempt: `None` on success, or the failure reason otherwise.
fn render_deep_probe_summary(failure: Option<&str>) -> String {
    match failure {
        None => {
            "  deep check: passed (created, verified, and deleted a temporary cluster)".to_owned()
        }
        Some(reason) => format!("  deep check: FAILED - {reason}"),
    }
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
    let Ok(matrix) = load_matrix() else {
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

/// Platforms this check treats as supported.
///
/// Linux is this project's own development and CI platform
/// (`.github/workflows/ci.yml` and `integration.yml` both run on
/// `ubuntu-latest`). `ROADMAP.md`'s Task 4.16 Step 4 commits to a
/// release workflow "building signed/checksummed binaries for Linux
/// amd64/arm64 and macOS amd64/arm64" (ROADMAP.md:2747), so macOS
/// belongs here on the same real, in-repo commitment — not because any
/// macOS CI job exists yet (it does not: Task 9.7/10.1 add that later).
/// Windows is listed too, but on weaker grounds, stated honestly rather
/// than invented: ROADMAP.md:2749 says plainly that "Windows native
/// support is not a v1 commitment because kind/Docker behavior differs;
/// Windows users may use WSL2" — and a WSL2 invocation already reports
/// `std::env::consts::OS` as `"linux"`, covered by the first entry
/// anyway. Keeping `"windows"` here is a deliberately lenient default
/// for the rare *native* invocation this shallow check has no stronger
/// basis to reject outright, given PRODUCT.md §34 does not ask it to.
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
/// `prerequisites_met` is taken as a parameter, computed once by the
/// caller, rather than recomputed here from `report`/`platform_diagnostic`
/// — see `run_with`'s own comment on why: a second, independent copy of
/// that condition could drift from the one that decides the process
/// exit code, letting the summary's closing line and the actual exit
/// code disagree.
///
/// Returns a `String` rather than printing directly, so its exact
/// content is unit-testable without capturing process-wide stdout.
fn render_summary(
    report: &DoctorReport,
    platform_diagnostic: Option<&str>,
    prerequisites_met: bool,
) -> String {
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
    lines.push(if prerequisites_met {
        "All required prerequisites are met.".to_owned()
    } else {
        "Some required prerequisites are missing; see above.".to_owned()
    });

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
    use std::path::{Path, PathBuf};
    use std::process::ExitStatus;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use admissionlab_cluster::{KubernetesImage, KubernetesImageMatrix};
    use admissionlab_core::{
        CommandResult, CommandSpec, OutputOverflow, ProcessError, ProcessRunner, RunDisposition,
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
                    overflow: OutputOverflow::default(),
                }),
                Some(FakeOutcome::Failure { code, stderr }) => Ok(CommandResult {
                    status: exit_status(*code),
                    stdout: Vec::new(),
                    stderr: stderr.clone(),
                    elapsed: Duration::from_millis(1),
                    overflow: OutputOverflow::default(),
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
        let runner = Arc::new(FakeProcessRunner::all_present());
        let code = block_on(run_with(runner, Path::new("/"), &DoctorArgs::default()));
        assert_eq!(code, exit::code_for_disposition(RunDisposition::Passed));
    }

    #[test]
    fn run_with_missing_tool_exits_invalid_input() {
        let runner = Arc::new(FakeProcessRunner::all_present().with("kind", FakeOutcome::Missing));
        let code = block_on(run_with(runner, Path::new("/"), &DoctorArgs::default()));
        assert_eq!(
            code,
            exit::code_for_disposition(RunDisposition::InvalidInput)
        );
    }

    #[test]
    fn run_with_docker_daemon_unreachable_exits_invalid_input() {
        let runner = Arc::new(FakeProcessRunner::all_present().with(
            "docker",
            FakeOutcome::Failure {
                code: 1,
                stderr: b"Cannot connect to the Docker daemon".to_vec(),
            },
        ));
        let code = block_on(run_with(runner, Path::new("/"), &DoctorArgs::default()));
        assert_eq!(
            code,
            exit::code_for_disposition(RunDisposition::InvalidInput)
        );
    }

    #[test]
    fn run_with_applies_the_kubectl_skew_warning_but_it_stays_advisory() {
        let runner = Arc::new(FakeProcessRunner::all_present().with(
            "kubectl",
            FakeOutcome::Success(br#"{"clientVersion":{"gitVersion":"v1.32.1"}}"#.to_vec()),
        ));

        let mut report = block_on(collect_doctor_report(runner.as_ref(), Path::new("/")));
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
        let code = block_on(run_with(runner, Path::new("/"), &DoctorArgs::default()));
        assert_eq!(code, exit::code_for_disposition(RunDisposition::Passed));
    }

    // -------------------------------------------------------------
    // `--deep`: off by default.
    // -------------------------------------------------------------

    #[test]
    fn deep_flag_defaults_to_false() {
        assert!(!DoctorArgs::default().deep);
    }

    #[test]
    fn deep_flag_off_by_default_runs_only_shallow_checks() {
        // `FakeProcessRunner::all_present` scripts a single fixed
        // outcome per *program name* regardless of subcommand, so if
        // `run_with` ever mistakenly invoked the deep probe here despite
        // `deep: false`, `kind create cluster` would "succeed" with no
        // kubeconfig ever written, and `KindClusterManager::create`'s own
        // kubeconfig verification would fail -- this would stop exiting
        // `Passed` and catch the regression.
        let runner = Arc::new(FakeProcessRunner::all_present());
        let code = block_on(run_with(runner, Path::new("/"), &DoctorArgs::default()));
        assert_eq!(code, exit::code_for_disposition(RunDisposition::Passed));
    }

    // -------------------------------------------------------------
    // `--deep`: the real create/verify/delete probe, against a fake
    // `ProcessRunner` distinguishing calls by purpose (not just program
    // name, since `kind`/`kubectl` are each invoked more than once for
    // different reasons -- see `deep_call_label`). The fully successful,
    // real-audit-log path is proven for real by `admissionlab-cluster`'s
    // `tests/kind_smoke.rs`, run with `-- --ignored`; these tests instead
    // cover the control-flow guarantees a fake can honestly prove: the
    // probe always attempts cleanup, surfaces a clear reason on failure,
    // and never panics.
    // -------------------------------------------------------------

    /// A temporary directory that removes itself when dropped.
    ///
    /// Each deep-probe test below holds one for as long as it uses the
    /// workspace underneath it. `Drop` runs on a panicking assertion
    /// too, which an explicit delete at the end of a test does not —
    /// that is what keeps a `cargo test` run from leaving a directory
    /// per test behind in the system temp directory.
    struct TempDir(PathBuf);

    impl TempDir {
        /// The directory's path, valid for as long as this guard lives.
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A fresh, real (absolute, on-disk) `RunPaths` for one deep-probe
    /// test, via a real `ArtifactStore::create_run` -- `run_deep_probe`
    /// writes through an `ArtifactStore` derived from `paths.root()`
    /// (inside `KindClusterManager`), so tests need every directory
    /// `RunPaths` names to genuinely exist. Mirrors
    /// `admissionlab-cluster`'s own `tests/lifecycle_unit.rs::new_run_paths`.
    async fn new_deep_probe_run_paths(run_id: &RunId) -> (TempDir, RunPaths) {
        let unique = RunId::generate();
        let root = TempDir(std::env::temp_dir().join(format!(
            "admissionlab-cli-doctor-deep-test-{}",
            unique.as_str()
        )));
        let store = ArtifactStore::new(root.path());
        let paths = store
            .create_run(run_id)
            .await
            .expect("create_run should succeed under a fresh temp root");
        (root, paths)
    }

    /// Pre-seeds the audit log file at the exact path
    /// `KindClusterManager` reports on for `Side::Baseline` (the side
    /// `run_deep_probe` always uses):
    /// `<logs>/baseline/audit/kube-apiserver-audit.log`. The basename is
    /// `admissionlab_cluster::kind`'s private `AUDIT_LOG_FILE_NAME`,
    /// duplicated here as a literal because it is `pub(crate)` to that
    /// crate and unreachable from this one -- guarded against drift by
    /// that crate's own inline test instead (see its module
    /// documentation).
    fn seed_audit_log(paths: &RunPaths) {
        let audit_dir = paths.logs().join("baseline").join("audit");
        std::fs::create_dir_all(&audit_dir).expect("create scratch audit dir");
        std::fs::write(audit_dir.join("kube-apiserver-audit.log"), b"{}\n")
            .expect("write dummy audit log");
    }

    /// Distinguishes repeated calls to the same external program by a
    /// short label derived from its argv, since the deep probe invokes
    /// `kind`/`kubectl` more than once each for different purposes
    /// (shallow version probe vs. create/delete/list; shallow version
    /// probe vs. health check) -- unlike [`FakeProcessRunner`], which
    /// only needs to key by program name because none of *its* callers
    /// invoke the same program twice for different reasons.
    fn deep_call_label(spec: &CommandSpec) -> String {
        let program = spec.program.to_string_lossy().into_owned();
        match program.as_str() {
            "kind" => match spec
                .args
                .first()
                .map(|arg| arg.to_string_lossy().into_owned())
                .as_deref()
            {
                Some("version") => "kind-version".to_owned(),
                Some("create") => "kind-create".to_owned(),
                Some("delete") => "kind-delete".to_owned(),
                Some("get") => "kind-get-clusters".to_owned(),
                _ => program,
            },
            "kubectl" => {
                if spec.args.iter().any(|arg| arg == "--raw=/healthz") {
                    "kubectl-healthz".to_owned()
                } else {
                    "kubectl-version".to_owned()
                }
            }
            _ => program,
        }
    }

    /// A [`ProcessRunner`] purpose-built for `--deep` tests: scripted per
    /// call *purpose* (see [`deep_call_label`]) rather than per program
    /// name. A scripted `kind-create` success additionally writes a
    /// dummy kubeconfig to its own `--kubeconfig` argument, simulating
    /// what a real `kind create cluster --kubeconfig <path>` does.
    #[derive(Default)]
    struct DeepFakeRunner {
        outcomes: BTreeMap<String, FakeOutcome>,
        calls: Mutex<Vec<CommandSpec>>,
    }

    impl DeepFakeRunner {
        /// Every call the deep probe makes scripted to succeed, except
        /// that the audit log can never be made real by a fake process
        /// runner -- a test wanting a fully successful probe must also
        /// call `seed_audit_log`.
        fn passing() -> Self {
            Self::default()
                .with(
                    "kind-version",
                    FakeOutcome::Success(b"kind v0.33.0 go1.26.7 linux/amd64\n".to_vec()),
                )
                .with(
                    "kubectl-version",
                    FakeOutcome::Success(br#"{"clientVersion":{"gitVersion":"v1.36.4"}}"#.to_vec()),
                )
                .with("helm", FakeOutcome::Success(b"v3.15.2".to_vec()))
                .with("docker", FakeOutcome::Success(b"\"27.5.0\"\n".to_vec()))
                .with("kind-create", FakeOutcome::Success(Vec::new()))
                .with("kind-delete", FakeOutcome::Success(Vec::new()))
                .with("kind-get-clusters", FakeOutcome::Success(Vec::new()))
                .with("kubectl-healthz", FakeOutcome::Success(b"ok".to_vec()))
        }

        fn with(mut self, label: &str, outcome: FakeOutcome) -> Self {
            self.outcomes.insert(label.to_owned(), outcome);
            self
        }

        fn calls(&self) -> Vec<CommandSpec> {
            self.calls.lock().expect("calls mutex poisoned").clone()
        }
    }

    fn deep_fake_kubeconfig_arg(args: &[std::ffi::OsString]) -> Option<std::path::PathBuf> {
        args.iter()
            .position(|arg| arg == "--kubeconfig")
            .and_then(|index| args.get(index + 1))
            .map(std::path::PathBuf::from)
    }

    #[async_trait]
    impl ProcessRunner for DeepFakeRunner {
        async fn run(&self, spec: CommandSpec) -> Result<CommandResult, ProcessError> {
            self.calls
                .lock()
                .expect("calls mutex poisoned")
                .push(spec.clone());

            let label = deep_call_label(&spec);
            match self.outcomes.get(&label) {
                Some(FakeOutcome::Success(stdout)) => {
                    if label == "kind-create"
                        && let Some(path) = deep_fake_kubeconfig_arg(&spec.args)
                    {
                        tokio::fs::write(&path, b"apiVersion: v1\nkind: Config\nclusters: []\n")
                            .await
                            .expect("test fake: write dummy kubeconfig");
                    }
                    Ok(CommandResult {
                        status: exit_status(0),
                        stdout: stdout.clone(),
                        stderr: Vec::new(),
                        elapsed: Duration::from_millis(1),
                        overflow: OutputOverflow::default(),
                    })
                }
                Some(FakeOutcome::Failure { code, stderr }) => Ok(CommandResult {
                    status: exit_status(*code),
                    stdout: Vec::new(),
                    stderr: stderr.clone(),
                    elapsed: Duration::from_millis(1),
                    overflow: OutputOverflow::default(),
                }),
                Some(FakeOutcome::Missing) | None => Err(ProcessError::Spawn {
                    context: Box::new(spec.context()),
                    source: io::Error::new(io::ErrorKind::NotFound, "No such file or directory"),
                }),
            }
        }
    }

    #[test]
    fn deep_probe_succeeds_when_health_and_audit_log_both_check_out() {
        let run_id = RunId::parse("deepprobesuccess001").expect("valid run id");
        let (_root, paths) = block_on(new_deep_probe_run_paths(&run_id));
        seed_audit_log(&paths);

        let runner: Arc<dyn ProcessRunner> = Arc::new(DeepFakeRunner::passing());
        let result = block_on(run_deep_probe(runner, &run_id, &paths));

        assert!(
            result.is_ok(),
            "expected the deep probe to succeed, got {result:?}"
        );
    }

    #[test]
    fn deep_probe_reports_failure_and_still_deletes_when_the_audit_log_is_missing() {
        let run_id = RunId::parse("deepprobeauditmiss01").expect("valid run id");
        let (_root, paths) = block_on(new_deep_probe_run_paths(&run_id));
        // Deliberately not seeded -- see `DeepFakeRunner::passing`'s doc.

        let runner = Arc::new(DeepFakeRunner::passing());
        let trait_runner: Arc<dyn ProcessRunner> = runner.clone();
        let result = block_on(run_deep_probe(trait_runner, &run_id, &paths));

        let error = result.expect_err("expected the missing audit log to fail the probe");
        assert!(error.contains("audit log"), "unexpected error: {error}");

        let calls = runner.calls();
        assert!(
            calls
                .iter()
                .any(|call| call.args.first().is_some_and(|arg| arg == "delete")),
            "expected a delete call even though the audit-log check failed"
        );
    }

    #[test]
    fn deep_probe_reports_failure_and_still_deletes_when_the_health_check_fails() {
        let run_id = RunId::parse("deepprobehealthfail1").expect("valid run id");
        let (_root, paths) = block_on(new_deep_probe_run_paths(&run_id));
        seed_audit_log(&paths);

        let runner = Arc::new(DeepFakeRunner::passing().with(
            "kubectl-healthz",
            FakeOutcome::Failure {
                code: 1,
                stderr: b"connection refused".to_vec(),
            },
        ));
        let trait_runner: Arc<dyn ProcessRunner> = runner.clone();
        let result = block_on(run_deep_probe(trait_runner, &run_id, &paths));

        let error = result.expect_err("expected the failing health check to fail the probe");
        assert!(error.contains("health"), "unexpected error: {error}");

        let calls = runner.calls();
        assert!(
            calls
                .iter()
                .any(|call| call.args.first().is_some_and(|arg| arg == "delete")),
            "expected a delete call even though the health check failed"
        );
    }

    #[test]
    fn deep_probe_reports_a_clear_error_when_cluster_creation_fails() {
        let run_id = RunId::parse("deepprobecreatefail1").expect("valid run id");
        let (_root, paths) = block_on(new_deep_probe_run_paths(&run_id));

        let runner: Arc<dyn ProcessRunner> =
            Arc::new(DeepFakeRunner::passing().with("kind-create", FakeOutcome::Missing));
        let result = block_on(run_deep_probe(runner, &run_id, &paths));

        let error = result.expect_err("expected cluster creation failure to surface");
        assert!(
            error.contains("create"),
            "expected the error to mention cluster creation, got {error:?}"
        );
    }

    #[test]
    fn run_with_deep_flag_attempts_a_full_lifecycle_and_reports_the_missing_audit_log() {
        let runner = Arc::new(DeepFakeRunner::passing());
        let trait_runner: Arc<dyn ProcessRunner> = runner.clone();
        let args = DoctorArgs { deep: true };

        let code = block_on(run_with(trait_runner, Path::new("/"), &args));

        // Honest failure: a fake process runner can never make a real
        // kube-apiserver write an audit log. The fully successful path
        // is exercised for real by `tests/kind_smoke.rs` instead.
        assert_eq!(
            code,
            exit::code_for_disposition(RunDisposition::InternalError)
        );

        let calls = runner.calls();
        assert!(
            calls
                .iter()
                .any(|call| call.args.first().is_some_and(|arg| arg == "create")),
            "expected --deep to attempt a cluster create"
        );
        assert!(
            calls
                .iter()
                .any(|call| call.args.first().is_some_and(|arg| arg == "delete")),
            "expected --deep to attempt cleanup even though the probe failed"
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

        let summary = render_summary(&report, None, true);
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

        let summary = render_summary(
            &report,
            Some("host platform \"plan9\" is not supported"),
            false,
        );
        assert!(summary.contains("NOT FOUND"));
        assert!(summary.contains("plan9"));
        assert!(summary.contains("1.0 GiB"));
        assert!(summary.contains("Some required prerequisites are missing"));
    }
}
