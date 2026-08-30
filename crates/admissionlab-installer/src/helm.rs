//! [`HelmInstaller`]: the Helm-backed [`ComponentInstaller`] (Task 2.2).
//! Installs one [`admissionlab_spec::component::HelmInstallSpec`] onto a
//! cluster by shelling out to `helm` through
//! [`admissionlab_core::ProcessRunner`] — never `std::process::Command`
//! or `tokio::process::Command` directly (Global Constraint 12) — and
//! reports what happened as an [`InstallRecord`].
//!
//! # The three `helm` invocations
//!
//! 1. `helm repo add <repo_name> <repo_url> --force-update` — registers
//!    or refreshes the chart repository. `--force-update` means a
//!    repository already registered under this name (for example, by an
//!    earlier run reusing the same local Helm client config) is
//!    refreshed rather than rejected as already existing.
//! 2. `helm upgrade --install <release> <chart> --version <version>
//!    --namespace <namespace> --create-namespace --kubeconfig
//!    <kubeconfig> --timeout <duration> [--values <path>]...
//!    [--set-string <key>=<value>]...` — the actual install.
//!    - `--create-namespace` is load-bearing, not cosmetic: neither
//!      `istio/base` nor `istio/istiod` creates their own target
//!      namespace object (verified against the real charts), so a
//!      Task 2.9 install into a fresh cluster fails without it.
//!    - Each of `helm.values_files` becomes its own `--values <path>`
//!      pair — never comma-joined — so a path containing a space stays
//!      exactly one argv element rather than silently becoming two.
//!    - Each of `helm.set_values` becomes its own
//!      `--set-string <key>=<value>` pair, in key order (`set_values` is
//!      a `BTreeMap`, so this is deterministic). Always `--set-string`,
//!      never `--set`: `--set` type-infers its value, so a literal
//!      string like `"1.2.3"`, `"true"`, or `"01"` would silently become
//!      a number, a boolean, or a mangled string.
//!    - `--kubeconfig` is always `cluster.kubeconfig` — this install's
//!      own cluster handle — never the ambient `$KUBECONFIG` or
//!      `~/.kube/config`.
//! 3. `helm get metadata <release> --namespace <namespace> --kubeconfig
//!    <kubeconfig> -o json` — a best-effort read of what actually got
//!    installed, for [`InstallRecord::resolved_version`]. See
//!    [`HelmInstaller::capture_resolved_version`] for what happens when
//!    this fails: never a fabricated version (Global Constraint 15).
//!
//! A component whose resolved install method is
//! [`admissionlab_spec::InstallMethod::Manifests`] is rejected up front
//! with [`InstallError::UnsupportedMethod`] — none of the three `helm`
//! invocations above ever run for it.
//!
//! # Two timeouts, deliberately different
//!
//! Every `helm` invocation gets both an outer
//! [`admissionlab_core::CommandSpec::timeout`] (enforced by
//! [`admissionlab_core::ProcessRunner`], which kills and reaps the
//! child unconditionally once it elapses) and, for the install step
//! only, Helm's own `--timeout` flag. These are deliberately different
//! numbers — see [`HELM_UPGRADE_TIMEOUT`] and [`UPGRADE_PROCESS_TIMEOUT`]
//! for the exact values and reasoning. In short: Helm's own (shorter)
//! timeout is the primary mechanism, sized to almost always fire first
//! and produce a clean, informative non-zero exit with Helm's own
//! message on stderr; the outer (longer) one is a backstop against
//! `helm` itself hanging past its own accounting, in which case
//! `ProcessRunner::run`'s documented guarantee still holds: the child is
//! killed and reaped before `install` ever returns.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use admissionlab_core::{
    ClusterHandle, CommandResult, CommandSpec, Diagnostic, ProcessRunner, RedactedValue,
};
use admissionlab_spec::component::HelmInstallSpec;
use admissionlab_spec::{InstallMethod, ResolvedComponent};
use async_trait::async_trait;

use crate::{ComponentInstaller, InstallError, InstallRecord};

/// The `helm` program name, resolved via `PATH` — never an absolute
/// path, matching `admissionlab_cluster::kind::KIND_PROGRAM`'s own
/// convention for external tools.
const HELM_PROGRAM: &str = "helm";

/// How long `helm repo add` may run before it is killed and reported as
/// timed out.
///
/// `helm repo add` only fetches and parses one repository's `index.yaml`
/// over HTTP(S) — no image pull, no cluster interaction — so this is
/// sized like `admissionlab_core::tool::PROBE_TIMEOUT` (a simple,
/// client-side check) but with headroom for the one difference that
/// check doesn't have: a real network round trip to a caller-supplied
/// URL, which can be slow on a loaded CI runner even though it normally
/// completes in well under a second.
const REPO_ADD_TIMEOUT: Duration = Duration::from_secs(60);

/// The `--timeout` passed to `helm upgrade --install` itself, bounding
/// how long Helm's own client waits for any individual Kubernetes
/// operation (chart hooks, primarily) to complete.
///
/// Chosen strictly shorter than [`UPGRADE_PROCESS_TIMEOUT`] (by two
/// minutes) so that, in the overwhelming majority of failure cases,
/// Helm's own timeout logic fires first: the child then exits non-zero
/// on its own, with an informative Helm-authored message on stderr,
/// which reaches the caller as a normal [`InstallError::CommandFailed`]
/// — far more diagnosable than a hard `SIGKILL` with no such message.
/// 480 seconds (8 minutes) is chosen over Helm's own 5-minute default
/// because a real admission-stack install is documented to be slower
/// than even a cold `kind create cluster` (measured at roughly 105
/// seconds on this machine), and image pulls are what dominates that
/// time; 5 minutes leaves too little margin for a legitimately
/// slow-but-succeeding cold pull of a real chart's images on a loaded CI
/// runner, which would otherwise convert a would-have-succeeded install
/// into a spurious timeout failure.
const HELM_UPGRADE_TIMEOUT: Duration = Duration::from_secs(480);

/// The outer [`admissionlab_core::ProcessRunner`] timeout for the `helm
/// upgrade --install` invocation — a backstop, not the primary
/// mechanism.
///
/// Set two minutes above [`HELM_UPGRADE_TIMEOUT`] so Helm's own
/// `--timeout` almost always has room to fire and report a clean
/// failure first (see that constant's documentation). This only fires
/// at all if the `helm` process itself hangs past its own accounted-for
/// timeout — for example stuck on something `--timeout` does not cover
/// — in which case the child is still killed and reaped before `install`
/// returns, so a hung `helm` can never leak a process the way an
/// unbounded wait would.
const UPGRADE_PROCESS_TIMEOUT: Duration = Duration::from_secs(600);

/// How long `helm get metadata` may run before it is treated as failed.
///
/// Like [`REPO_ADD_TIMEOUT`], this is a lightweight, best-effort
/// enrichment step (Task 2.2 brief Step 3) — reading one release's
/// already-stored metadata back from the cluster it was just installed
/// onto, not waiting on any new work — so it is sized generously for a
/// slow/loaded CI runner rather than tuned to the common near-instant
/// case, without approaching the weight of an actual install.
const GET_METADATA_TIMEOUT: Duration = Duration::from_secs(30);

/// What [`InstallRecord::resolved_version`] holds when `helm get
/// metadata` could not be run, exited non-zero, or produced output this
/// module could not parse: an honest "not confirmed" value, never the
/// requested/pinned version reused as a guess (Global Constraint 15).
/// See [`HelmInstaller::capture_resolved_version`].
const UNCONFIRMED_VERSION: &str = "unknown";

/// Drives `helm` (via a shared [`ProcessRunner`]) to install a single
/// resolved Helm component. Holds only the runner, so one instance is
/// safe to reuse — behind an `Arc`, as a later stack-orchestration task
/// does — across every component of every cluster of a run.
pub struct HelmInstaller {
    runner: Arc<dyn ProcessRunner>,
}

impl HelmInstaller {
    /// Creates an installer that drives `helm` through `runner`.
    #[must_use]
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner }
    }

    /// Runs `spec` (one `helm` invocation for `component`) and maps its
    /// outcome to `Result<CommandResult, InstallError>`: a
    /// [`admissionlab_core::ProcessError`] becomes
    /// [`InstallError::Process`], and a non-zero exit becomes
    /// [`InstallError::CommandFailed`].
    async fn run_and_check(
        &self,
        component: &str,
        spec: CommandSpec,
    ) -> Result<CommandResult, InstallError> {
        let context = Box::new(spec.context());
        let result = self
            .runner
            .run(spec)
            .await
            .map_err(|source| InstallError::Process {
                component: component.to_owned(),
                source,
            })?;
        if result.status.success() {
            Ok(result)
        } else {
            Err(InstallError::CommandFailed {
                component: component.to_owned(),
                context,
                status: result.status,
                stdout: result.stdout,
                stderr: result.stderr,
            })
        }
    }

    /// Best-effort: runs `helm get metadata ... -o json` for `helm`'s
    /// release and returns the chart version it reports, alongside any
    /// diagnostic explaining why the version could not be confirmed.
    ///
    /// Never fails the caller's install (Task 2.2 brief Step 3): a
    /// spawn failure, a timeout, a non-zero exit, or output this module
    /// cannot parse as JSON with a usable `version` field all degrade to
    /// [`UNCONFIRMED_VERSION`] plus exactly one [`Diagnostic`] explaining
    /// why — never a fabricated version (Global Constraint 15).
    async fn capture_resolved_version(
        &self,
        component: &str,
        helm: &HelmInstallSpec,
        kubeconfig: &Path,
    ) -> (String, Vec<Diagnostic>) {
        let spec = get_metadata_spec(helm, kubeconfig);
        let context = spec.context();
        let reason = match self.runner.run(spec).await {
            Ok(result) if result.status.success() => match parse_chart_version(&result.stdout) {
                Ok(version) => return (version, Vec::new()),
                Err(reason) => reason,
            },
            Ok(result) => format!(
                "`{context}` exited with {}: {}",
                result.status,
                String::from_utf8_lossy(&result.stderr).trim(),
            ),
            Err(error) => format!("could not run `{context}`: {error}"),
        };
        (
            UNCONFIRMED_VERSION.to_owned(),
            vec![metadata_unavailable_diagnostic(
                component,
                &helm.release_name,
                &reason,
            )],
        )
    }
}

#[async_trait]
impl ComponentInstaller for HelmInstaller {
    async fn install(
        &self,
        cluster: &ClusterHandle,
        component: &ResolvedComponent,
    ) -> Result<InstallRecord, InstallError> {
        let helm = match &component.install {
            InstallMethod::Helm(helm) => helm,
            InstallMethod::Manifests(_) => {
                return Err(InstallError::UnsupportedMethod {
                    component: component.name.clone(),
                    expected: "Helm",
                    actual: "Manifests",
                });
            }
        };

        let started_at = SystemTime::now();
        let start = Instant::now();

        self.run_and_check(&component.name, repo_add_spec(helm))
            .await?;
        self.run_and_check(
            &component.name,
            upgrade_install_spec(helm, &cluster.kubeconfig),
        )
        .await?;

        let (resolved_version, diagnostics) = self
            .capture_resolved_version(&component.name, helm, &cluster.kubeconfig)
            .await;

        Ok(InstallRecord {
            component: component.name.clone(),
            method: "helm".to_owned(),
            resolved_version,
            started_at,
            elapsed: start.elapsed(),
            diagnostics,
        })
    }
}

/// Builds the argv for `helm repo add <repo_name> <repo_url>
/// --force-update`.
fn repo_add_spec(helm: &HelmInstallSpec) -> CommandSpec {
    CommandSpec {
        program: HELM_PROGRAM.into(),
        args: vec![
            "repo".into(),
            "add".into(),
            helm.repo_name.as_str().into(),
            helm.repo_url.as_str().into(),
            "--force-update".into(),
        ],
        cwd: None,
        env: BTreeMap::new(),
        sensitive_env_keys: BTreeSet::new(),
        timeout: REPO_ADD_TIMEOUT,
    }
}

/// Builds the argv for `helm upgrade --install`. See the module
/// documentation for the exact flag set and ordering rules.
fn upgrade_install_spec(helm: &HelmInstallSpec, kubeconfig: &Path) -> CommandSpec {
    let mut args: Vec<OsString> = vec![
        "upgrade".into(),
        "--install".into(),
        helm.release_name.as_str().into(),
        helm.chart.as_str().into(),
        "--version".into(),
        helm.version.as_str().into(),
        "--namespace".into(),
        helm.namespace.as_str().into(),
        "--create-namespace".into(),
        "--kubeconfig".into(),
        kubeconfig.as_os_str().to_owned(),
        "--timeout".into(),
        helm_timeout_arg(HELM_UPGRADE_TIMEOUT),
    ];

    for values_file in &helm.values_files {
        args.push("--values".into());
        args.push(values_file.as_os_str().to_owned());
    }
    for (key, value) in &helm.set_values {
        args.push("--set-string".into());
        args.push(format!("{key}={value}").into());
    }

    CommandSpec {
        program: HELM_PROGRAM.into(),
        args,
        cwd: None,
        env: BTreeMap::new(),
        sensitive_env_keys: BTreeSet::new(),
        timeout: UPGRADE_PROCESS_TIMEOUT,
    }
}

/// Builds the argv for `helm get metadata <release> --namespace
/// <namespace> --kubeconfig <kubeconfig> -o json`.
fn get_metadata_spec(helm: &HelmInstallSpec, kubeconfig: &Path) -> CommandSpec {
    CommandSpec {
        program: HELM_PROGRAM.into(),
        args: vec![
            "get".into(),
            "metadata".into(),
            helm.release_name.as_str().into(),
            "--namespace".into(),
            helm.namespace.as_str().into(),
            "--kubeconfig".into(),
            kubeconfig.as_os_str().to_owned(),
            "-o".into(),
            "json".into(),
        ],
        cwd: None,
        env: BTreeMap::new(),
        sensitive_env_keys: BTreeSet::new(),
        timeout: GET_METADATA_TIMEOUT,
    }
}

/// Formats `timeout` the way Helm's Go `time.Duration` flag parser
/// expects (for example `480s`).
fn helm_timeout_arg(timeout: Duration) -> OsString {
    format!("{}s", timeout.as_secs()).into()
}

/// Parses `helm get metadata -o json`'s stdout and returns the chart
/// version at its `"version"` field.
///
/// Parsed as JSON via `serde_json::Value`, not scraped as text, so a
/// well-formed-but-differently-shaped response degrades to an `Err`
/// rather than a mis-extracted value — mirroring
/// `admissionlab_core::tool::parse_kubectl_version`'s own approach to
/// `kubectl version --output=json`.
///
/// # Errors
///
/// Returns a human-readable explanation, never panics, if `stdout` is
/// not valid JSON or has no non-empty string `"version"` field.
fn parse_chart_version(stdout: &[u8]) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_slice(stdout).map_err(|error| {
        format!("could not parse `helm get metadata -o json` output as JSON: {error}")
    })?;
    value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .filter(|version| !version.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            "`helm get metadata -o json` output had no usable \"version\" field".to_owned()
        })
}

/// Builds the [`Diagnostic`] recorded when [`HelmInstaller::capture_resolved_version`]
/// could not confirm the installed chart version, for the given
/// `component`/`release_name`/human-readable `reason`.
fn metadata_unavailable_diagnostic(
    component: &str,
    release_name: &str,
    reason: &str,
) -> Diagnostic {
    let mut context = BTreeMap::new();
    context.insert(
        "component".to_owned(),
        RedactedValue::Public(component.to_owned()),
    );
    context.insert(
        "release_name".to_owned(),
        RedactedValue::Public(release_name.to_owned()),
    );
    context.insert(
        "reason".to_owned(),
        RedactedValue::Public(reason.to_owned()),
    );
    Diagnostic {
        code: "installer.helm.resolved_version_unavailable".to_owned(),
        message: format!(
            "could not confirm the installed chart version for component {component:?} via \
             `helm get metadata`: {reason}"
        ),
        context,
    }
}
