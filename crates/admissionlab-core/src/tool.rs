//! Host tool discovery for `admissionlab doctor`: probing whether `kind`,
//! `kubectl`, `helm`, and `docker` are present and usable, and whether
//! there is enough free disk space to run a lab.
//!
//! PRODUCT.md §34 ("Diagnostics and `doctor`") requires `doctor` to
//! inspect Docker/container-runtime reachability, `kind`/`kubectl`/
//! `helm` availability and version, and a disk-space warning threshold.
//! This module is the single place that logic lives: [`probe_tool`]
//! inspects one external tool, [`collect_doctor_report`] assembles all
//! four plus the disk-space check into one [`DoctorReport`], and
//! [`DoctorReport::meets_prerequisites`] is the one pass/fail judgment
//! over that report.
//!
//! **One check, two callers.** `admissionlab-cli`'s `doctor` command
//! (Task 1.4) calls [`collect_doctor_report`] to print a diagnostic
//! summary; a future task wiring prerequisite checking into `test`
//! (PRODUCT.md §33's "sufficient diagnostics ... so users do not need to
//! rerun solely to discover which setup stage failed") calls the exact
//! same function as a gate before creating any cluster. Nothing here
//! decides a process exit code or renders text for a terminal — that is
//! each caller's own policy — but there is deliberately only one
//! implementation of "did the host probe find what Admission Lab
//! needs," so `doctor` and `test` can never silently disagree about it.
//!
//! # Testability: every probe takes a [`ProcessRunner`]
//!
//! Every function here that runs an external command takes
//! `&dyn ProcessRunner` rather than constructing a
//! [`crate::process::TokioProcessRunner`] itself, so tests can exercise
//! every outcome (a tool missing entirely, a daemon that is unreachable,
//! a version string that does not parse) with a fake in-process
//! implementation and never spawn a real `kind`, `kubectl`, `helm`, or
//! `docker` (see `tests/tool.rs`).
//!
//! # `found` vs. `docker_reachable`: two different questions
//!
//! [`ToolStatus::found`] answers "could this program be started at
//! all?" — `true` for any [`ProcessRunner::run`] call that returns
//! `Ok(_)`, regardless of exit status, and `false` only when `run`
//! itself fails (in practice, the program was not on `PATH`). A tool can
//! be found and still fail: `kind`, `kubectl`, and `helm`'s version
//! probes are simple, offline, client-only calls that are not expected
//! to fail once the binary exists, but `docker version --format
//! {{json .Server.Version}}` is different by design — it asks the
//! *daemon* to report its version, so it fails whenever the daemon is
//! unreachable even though the `docker` client binary itself is present
//! and ran successfully. [`DoctorReport::docker_reachable`] captures
//! exactly that second, separate condition: it is `true` only when the
//! Docker tool was found *and* that probe produced a version, so
//! "`docker` is not installed" and "`docker` is installed but the
//! daemon is down" are always distinguishable — the two conditions have
//! different user remedies (install Docker vs. start Docker) and PRODUCT.md
//! §34 separately calls out both "Docker/container runtime reachability"
//! and "`kind`/`kubectl`/`helm` availability/version" as distinct things
//! to inspect.
//!
//! # Disk space
//!
//! [`disk_space_warning`] reports free space on the filesystem
//! containing a caller-given path against [`DISK_WARNING_THRESHOLD_BYTES`]
//! (10 GiB — see that constant's own documentation for the reasoning).
//! Free space is read via `statvfs(2)` through the `nix` crate on Unix:
//! there is no portable safe wrapper in `std`, and calling `libc`'s
//! `statvfs` directly would require an `unsafe` FFI call, which is
//! forbidden workspace-wide (`unsafe_code = "forbid"`). On a non-Unix
//! target there is no equivalent this crate can call without `unsafe`
//! either, so the check degrades to "indeterminate" (a [`Some`] message
//! saying so, logged via a `tracing` warning) rather than silently
//! reporting no warning — the same "make the gap visible, do not hide
//! it" convention `artifact.rs` already uses for permission bits it
//! cannot set on non-Unix.
//!
//! This deliberately checks the filesystem under a caller-supplied path
//! (in practice, `admissionlab-cli` passes the current working
//! directory), not wherever the Docker daemon actually stores images —
//! finding the daemon's storage root would itself require a further
//! `docker info` probe and can be a *different* filesystem entirely on
//! Docker Desktop / remote-daemon setups. That is out of scope for this
//! shallow check; the summary always names the path it checked so a
//! user is never left guessing which disk the warning is about.
//!
//! Never fabricated: a threshold breach and an unreadable filesystem are
//! both reported through [`disk_space_warning`]'s one `Option<String>`,
//! but only ever with a real number this process actually read or an
//! honest statement that it could not read one — never an invented
//! value (Global Constraint 15).

use std::path::Path;
use std::time::Duration;

use crate::process::{CommandSpec, ProcessError, ProcessRunner};

/// One external tool Admission Lab depends on and `doctor` inspects.
///
/// Deliberately just these four: PRODUCT.md §34 lists exactly
/// `kind`/`kubectl`/`helm` availability and Docker/container-runtime
/// reachability as what `doctor` must inspect at this (shallow) phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolName {
    /// `kind`, the ephemeral-cluster provisioner.
    Kind,
    /// `kubectl`, the Kubernetes API client.
    Kubectl,
    /// `helm`, the chart installer used to bring up admission stacks.
    Helm,
    /// `docker`, the container runtime `kind` provisions nodes into.
    Docker,
}

impl ToolName {
    /// Every tool `doctor` probes, in the order [`collect_doctor_report`]
    /// probes and reports them.
    pub const ALL: [Self; 4] = [Self::Kind, Self::Kubectl, Self::Helm, Self::Docker];

    /// The program name passed to [`ProcessRunner::run`] as
    /// [`CommandSpec::program`] — also this tool's display name.
    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Self::Kind => "kind",
            Self::Kubectl => "kubectl",
            Self::Helm => "helm",
            Self::Docker => "docker",
        }
    }
}

impl std::fmt::Display for ToolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.program())
    }
}

/// One tool's discovered status, as reported by [`probe_tool`].
///
/// `found` and `version` are independent: a tool can be found with no
/// version (its version probe failed or produced unparseable output —
/// see [`ToolStatus::diagnostic`]) but a tool can never have a `version`
/// without `found` being `true`. A malformed or missing version is
/// always `None`, never a fabricated value (Global Constraint 15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolStatus {
    /// Which tool this is.
    pub name: ToolName,
    /// Whether the program could be started at all. `false` only when
    /// [`ProcessRunner::run`] itself failed (in practice, not found on
    /// `PATH`) — see the module documentation's "`found` vs.
    /// `docker_reachable`" section.
    pub found: bool,
    /// The tool's self-reported version, if a probe ran and its output
    /// could be parsed. `None` whenever `found` is `false`, and also
    /// whenever the probe ran but exited non-zero or produced output
    /// this module could not parse — never a guessed or partial value.
    pub version: Option<String>,
    /// A human-readable explanation for anything a reader of this status
    /// alone would otherwise find surprising: why `found` is `false`, why
    /// `version` is `None` despite `found` being `true`, or (for
    /// `Kubectl` specifically) an advisory note unrelated to either,
    /// such as a Kubernetes client/server minor-version skew warning.
    /// `None` when there is nothing to add.
    pub diagnostic: Option<String>,
}

/// Shallow host-prerequisite findings for `admissionlab doctor` (and,
/// via [`DoctorReport::meets_prerequisites`], for `test`'s prerequisite
/// gate — see the module documentation's "one check, two callers").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    /// One entry per [`ToolName::ALL`], in that order.
    pub tools: Vec<ToolStatus>,
    /// Whether the Docker daemon answered the server-version probe.
    /// Distinct from the `Docker` entry in `tools`'s `found` — see the
    /// module documentation's "`found` vs. `docker_reachable`" section.
    pub docker_reachable: bool,
    /// A free-disk-space warning, if free space on the checked
    /// filesystem is below [`DISK_WARNING_THRESHOLD_BYTES`] or could not
    /// be determined at all. `None` only when the check ran and found
    /// sufficient space.
    pub disk_warning: Option<String>,
}

impl DoctorReport {
    /// Looks up one tool's status by name.
    #[must_use]
    pub fn tool(&self, name: ToolName) -> Option<&ToolStatus> {
        self.tools.iter().find(|status| status.name == name)
    }

    /// Whether this report describes a host that satisfies what
    /// Admission Lab needs to run a lab: every probed tool was found,
    /// and the Docker daemon is reachable.
    ///
    /// [`DoctorReport::disk_warning`] never affects this: PRODUCT.md §34
    /// calls it a "disk-space warning threshold", not a hard
    /// requirement, so low or indeterminate disk space is surfaced to
    /// the user without turning an otherwise-usable host into one that
    /// fails prerequisites.
    #[must_use]
    pub fn meets_prerequisites(&self) -> bool {
        self.tools.iter().all(|tool| tool.found) && self.docker_reachable
    }
}

/// How long a version probe may run before it is treated as failed.
///
/// Generous for what are simple, offline, client-only invocations (none
/// of `kind version`, `kubectl version --client=true`, `helm version
/// --template`, or `docker version` talk to a cluster or the network),
/// so this is sized for a slow/loaded CI runner rather than the common
/// case.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Probes one tool via `runner` and reports what was found.
///
/// Runs the exact argv PRODUCT.md's verified probe commands specify
/// (see this crate's `tests/tool.rs` for the literal output shapes this
/// was validated against): `kind version`; `kubectl version
/// --client=true --output=json`; `helm version --template {{.Version}}`;
/// `docker version --format {{json .Server.Version}}`.
///
/// Never panics: a spawn failure, a non-zero exit, or unparseable output
/// all degrade to a [`ToolStatus`] with `diagnostic` set rather than a
/// panic or a fabricated `version`.
pub async fn probe_tool(runner: &dyn ProcessRunner, name: ToolName) -> ToolStatus {
    let spec = CommandSpec {
        program: name.program().into(),
        args: probe_args(name),
        cwd: None,
        env: std::collections::BTreeMap::new(),
        sensitive_env_keys: std::collections::BTreeSet::new(),
        timeout: PROBE_TIMEOUT,
    };

    match runner.run(spec).await {
        Ok(result) if result.status.success() => match parse_version(name, &result.stdout) {
            Ok(version) => ToolStatus {
                name,
                found: true,
                version: Some(version),
                diagnostic: None,
            },
            Err(diagnostic) => ToolStatus {
                name,
                found: true,
                version: None,
                diagnostic: Some(diagnostic),
            },
        },
        // Ran, but exited non-zero: the program is present (this is not
        // "not found") but its version probe failed. For `Docker` this
        // is exactly the daemon-unreachable case; `collect_doctor_report`
        // reads it back out of `found`/`version` rather than needing a
        // separate signal here.
        Ok(result) => ToolStatus {
            name,
            found: true,
            version: None,
            diagnostic: Some(describe_nonzero_exit(name, result.status, &result.stderr)),
        },
        Err(error) => ToolStatus {
            name,
            found: false,
            version: None,
            diagnostic: Some(describe_process_error(name, &error)),
        },
    }
}

/// Probes every [`ToolName::ALL`] via `runner`, checks free disk space at
/// `disk_check_path`, and assembles the result into one [`DoctorReport`].
///
/// See the module documentation's "one check, two callers" section: this
/// is the single implementation both `doctor` and (later) `test`'s
/// prerequisite gate call.
pub async fn collect_doctor_report(
    runner: &dyn ProcessRunner,
    disk_check_path: &Path,
) -> DoctorReport {
    let mut tools = Vec::with_capacity(ToolName::ALL.len());
    for name in ToolName::ALL {
        tools.push(probe_tool(runner, name).await);
    }

    let docker_reachable = tools
        .iter()
        .find(|tool| tool.name == ToolName::Docker)
        .is_some_and(|docker| docker.found && docker.version.is_some());

    let disk_warning = disk_space_warning(disk_check_path, DISK_WARNING_THRESHOLD_BYTES);

    DoctorReport {
        tools,
        docker_reachable,
        disk_warning,
    }
}

/// The argv (excluding the program name) for `name`'s version probe.
fn probe_args(name: ToolName) -> Vec<std::ffi::OsString> {
    match name {
        ToolName::Kind => vec!["version".into()],
        ToolName::Kubectl => vec![
            "version".into(),
            "--client=true".into(),
            "--output=json".into(),
        ],
        ToolName::Helm => vec!["version".into(), "--template".into(), "{{.Version}}".into()],
        ToolName::Docker => vec![
            "version".into(),
            "--format".into(),
            "{{json .Server.Version}}".into(),
        ],
    }
}

/// Parses `stdout` from `name`'s successful (exit-0) version probe.
///
/// # Errors
///
/// Returns a human-readable explanation, never panics, whenever `stdout`
/// does not match the shape `name`'s probe command is documented to
/// produce.
fn parse_version(name: ToolName, stdout: &[u8]) -> Result<String, String> {
    match name {
        ToolName::Kind => parse_kind_version(stdout),
        ToolName::Kubectl => parse_kubectl_version(stdout),
        ToolName::Helm => parse_helm_version(stdout),
        ToolName::Docker => parse_docker_version(stdout),
    }
}

/// Parses `kind version`'s output, for example
/// `"kind v0.33.0 go1.26.7 linux/amd64"`: space-separated, the version is
/// the second field.
fn parse_kind_version(stdout: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(stdout);
    text.split_whitespace()
        .nth(1)
        .filter(|field| !field.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("could not parse `kind version` output: {text:?}"))
}

/// Parses `kubectl version --client=true --output=json`'s output: JSON
/// with the version at `.clientVersion.gitVersion`. Parsed as JSON, not
/// matched with a regex, so any well-formed-but-differently-shaped
/// response degrades to a diagnostic rather than a mis-extracted value.
fn parse_kubectl_version(stdout: &[u8]) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_slice(stdout).map_err(|error| {
        format!("could not parse `kubectl version --output=json` output as JSON: {error}")
    })?;
    value
        .get("clientVersion")
        .and_then(|client_version| client_version.get("gitVersion"))
        .and_then(serde_json::Value::as_str)
        .filter(|version| !version.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            "`kubectl version --output=json` output had no clientVersion.gitVersion".to_owned()
        })
}

/// Parses `helm version --template {{.Version}}`'s output: a bare
/// version string with no surrounding structure.
fn parse_helm_version(stdout: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        Err("`helm version --template {{.Version}}` produced no output".to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

/// Parses `docker version --format {{json .Server.Version}}`'s output.
///
/// The output is JSON-*quoted* (for example `"27.5.0"`, quotes
/// included, since the format string wraps the value in Go's
/// `{{json ...}}` template function) — trimming whitespace and using it
/// verbatim would leave the literal `"` characters in the reported
/// version and, later, in a run manifest's provenance record. This
/// parses it as a JSON string literal instead, so the returned value is
/// the unquoted `27.5.0`, and any output that is not validly
/// JSON-quoted (for example a caller mistakenly feeding this the client
/// version's raw, unquoted form) is rejected as malformed rather than
/// silently accepted with its quoting intact.
fn parse_docker_version(stdout: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    serde_json::from_str::<String>(trimmed).map_err(|error| {
        format!(
            "could not parse `docker version --format {{{{json .Server.Version}}}}` \
             output {trimmed:?} as a JSON string: {error}"
        )
    })
}

/// Describes a version probe that ran but exited non-zero, for
/// [`ToolStatus::diagnostic`].
fn describe_nonzero_exit(
    name: ToolName,
    status: std::process::ExitStatus,
    stderr: &[u8],
) -> String {
    let code_description = status.code().map_or_else(
        || "terminated by signal".to_owned(),
        |code| format!("exit code {code}"),
    );
    let stderr_text = String::from_utf8_lossy(stderr);
    let stderr_trimmed = stderr_text.trim();
    if stderr_trimmed.is_empty() {
        format!(
            "`{}` version probe failed ({code_description})",
            name.program()
        )
    } else {
        format!(
            "`{}` version probe failed ({code_description}): {stderr_trimmed}",
            name.program()
        )
    }
}

/// Describes a [`ProcessError`] from attempting to run `name`'s version
/// probe at all, for [`ToolStatus::diagnostic`].
fn describe_process_error(name: ToolName, error: &ProcessError) -> String {
    format!("`{}` was not usable: {error}", name.program())
}

/// Free-space threshold below which [`disk_space_warning`] surfaces a
/// warning: 10 GiB.
///
/// A lab run needs room for two ephemeral `kind` node images pulled
/// concurrently (baseline and candidate, each on the order of 1-2 GiB
/// depending on the pinned Kubernetes minor — see
/// `compatibility/kubernetes.yaml`), plus whatever admission stack(s) a
/// lab configuration installs into each (a Gateway API implementation,
/// an admission/policy controller, or both), each of which can pull a
/// further several hundred MiB to low GiB of its own images and
/// dependent workloads, plus headroom for Docker's own layer cache
/// across a run rather than assuming every layer is already warm. 10
/// GiB is comfortably above what a single baseline+candidate pair with
/// one or two admission components needs, while still catching a host
/// that is close enough to full that an image pull is likely to fail
/// partway through — which is a worse failure mode than a warning ahead
/// of time, per PRODUCT.md §33's "sufficient diagnostics on first
/// failure so users do not need to rerun solely to discover which setup
/// stage failed." This is advisory, not a hard requirement (see
/// [`DoctorReport::meets_prerequisites`]): a host below this line may
/// still complete a run.
pub const DISK_WARNING_THRESHOLD_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// Checks free space on the filesystem containing `path` against
/// `threshold_bytes` and returns a warning message if it is short, or if
/// free space could not be determined at all.
///
/// Returns `None` only when the check succeeded and found at least
/// `threshold_bytes` free. A message is otherwise always built from a
/// real number this process actually read, or is an honest statement
/// that no number could be obtained — never a fabricated value (Global
/// Constraint 15).
#[must_use]
pub fn disk_space_warning(path: &Path, threshold_bytes: u64) -> Option<String> {
    match available_bytes(path) {
        Ok(available) if available < threshold_bytes => Some(format!(
            "only {} free at {} (below the {} warning threshold); a baseline/candidate \
             `kind` cluster pair and their admission stacks may fail partway through an \
             image pull",
            format_gib(available),
            path.display(),
            format_gib(threshold_bytes),
        )),
        Ok(_) => None,
        Err(reason) => Some(format!(
            "could not determine free disk space at {}: {reason}",
            path.display()
        )),
    }
}

/// Formats `bytes` as whole GiB plus one decimal digit (for example
/// `"9.3 GiB"`), using only integer arithmetic so this never needs an
/// integer-to-float cast (and the precision loss that would come with
/// one) for a value that is only ever displayed, never computed with
/// further.
fn format_gib(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    let whole = bytes / GIB;
    let tenths = (bytes % GIB) * 10 / GIB;
    format!("{whole}.{tenths} GiB")
}

/// Reads free space at `path` in bytes.
///
/// # Errors
///
/// Returns a human-readable explanation, never panics, if `statvfs`
/// fails (for example, `path` does not exist) or — on a non-Unix
/// target — unconditionally, since this crate has no `unsafe`-free way
/// to ask the OS.
#[cfg(unix)]
fn available_bytes(path: &Path) -> Result<u64, String> {
    nix::sys::statvfs::statvfs(path)
        .map(|stats| {
            // `blocks_available()`/`fragment_size()` are `libc`
            // `fsblkcnt_t`/`c_ulong`, which alias different widths on
            // different Unix targets (for example `u64` on Linux vs.
            // `u32` for `fsblkcnt_t` on Apple platforms). Widening both
            // through `u128` first — always a genuine, non-identity
            // conversion on every such target, since neither alias is
            // ever `u128` itself — means this multiplication needs no
            // per-platform cast and never trips `clippy::useless_conversion`
            // on whichever field happens to already be 64-bit on a given
            // target. The final narrowing back to `u64` is real disk
            // sizes only, so `unwrap_or(u64::MAX)` never actually
            // saturates in practice; it exists so a future,
            // implausibly large `statvfs` result degrades to "very
            // large" rather than panicking.
            let blocks_available = u128::from(stats.blocks_available());
            let fragment_size = u128::from(stats.fragment_size());
            u64::try_from(blocks_available * fragment_size).unwrap_or(u64::MAX)
        })
        .map_err(|errno| format!("statvfs failed: {errno}"))
}

/// Non-Unix fallback for [`available_bytes`]: there is no portable
/// equivalent this crate can call without `unsafe`, which is forbidden
/// workspace-wide. Rather than silently reporting "no warning", this
/// emits a `tracing` warning so the gap is visible at runtime — the same
/// convention `artifact.rs`'s non-Unix permission fallback already uses.
#[cfg(not(unix))]
fn available_bytes(_path: &Path) -> Result<u64, String> {
    tracing::warn!(
        "free disk space cannot be checked on this platform; doctor's disk-space \
         warning will report as indeterminate rather than a real reading"
    );
    Err("disk-space checks are only implemented for Unix platforms".to_owned())
}
