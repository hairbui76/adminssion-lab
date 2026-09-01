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
//!    refreshed rather than rejected as already existing. **Skipped
//!    entirely for an OCI chart reference** — see "OCI chart references"
//!    below.
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
//! # OCI chart references (ROADMAP Task 8.1)
//!
//! A chart reference beginning with `oci://` is **self-locating**: the
//! reference itself names the registry, the repository path and the
//! chart, so `helm upgrade --install <release> oci://host/path/chart
//! --version <v>` resolves without any repository having been registered
//! first. Step 1 above is therefore skipped for such a chart, and
//! [`is_oci_chart`] is the single predicate that decides it.
//!
//! Skipping is not an optimization — it is the only thing that works.
//! `helm repo add` speaks the classic HTTP chart-repository protocol
//! (fetch and parse `index.yaml`) and an OCI registry serves no such
//! document. Measured directly against `helm` v3.20.0 while writing this:
//!
//! ```text
//! $ helm repo add ngf oci://ghcr.io/nginx/charts/nginx-gateway-fabric --force-update
//! Error: looks like "oci://ghcr.io/nginx/charts/nginx-gateway-fabric" is not a valid
//! chart repository or cannot be reached: failed to perform "FetchReference" on source:
//! invalid reference
//! ```
//!
//! The same `helm` then installed that exact chart successfully with no
//! repository registered at all, which is the behavior this module now
//! mirrors.
//!
//! What made this necessary rather than hypothetical: NGINX Gateway
//! Fabric (`recipes/nginx-gateway-fabric/`, Task 8.1) publishes its chart
//! **only** to `oci://ghcr.io/nginx/charts/nginx-gateway-fabric` — its
//! own documentation gives no `helm repo add` command, and F5's classic
//! repository at `https://helm.nginx.com/stable` does not carry the chart
//! (checked against that repository's own `index.yaml`, which lists
//! `nginx-ingress`, `nginx-service-mesh` and others, but no
//! `nginx-gateway-fabric`). The alternative install path, raw manifests,
//! is closed to that project for an unrelated reason recorded in
//! `recipes/nginx-gateway-fabric/README.md`.
//!
//! **The predicate is the chart, never an empty `repo_url`.**
//! [`HelmInstallSpec::repo_url`] stays a required, non-empty field for an
//! OCI install too, where it carries the registry path the chart
//! reference is rooted at — real provenance a reader and a report both
//! want, and one this module deliberately does not turn into a sentinel
//! value by leaving it blank.
//!
//! # Helm state isolation
//!
//! `helm repo add` and `helm upgrade --install` (which, for a
//! `repo/chart`-shorthand reference like `helm.chart`, resolves that
//! reference through the local repository config/cache rather than
//! only talking to the cluster) both read and write **local, ambient
//! Helm client state** — by default `~/.config/helm/repositories.yaml`
//! and `~/.cache/helm/repository`, entirely independent of
//! `--kubeconfig`. Passing `env: BTreeMap::new()` (this module's first
//! shape, found in review) does not opt out of that default: per
//! [`admissionlab_core::process`]'s own documented "inherited, not
//! exclusive" semantics, the child still inherits *this process's own*
//! environment, so on a real host `helm repo add` would add an entry to
//! the operator's genuine personal `~/.config/helm/repositories.yaml` —
//! the same shape of bug Phase 1 found and fixed for `kind delete` and
//! `~/.kube/config` (see `admissionlab_cluster::kind::delete_argv`'s
//! documentation), one layer up: a subprocess reaching for shared user
//! state because nothing told it where its own state should live
//! instead. PRODUCT.md §29's safe-by-default stance is the same
//! principle applied here.
//!
//! The fix, verified empirically against a real `helm` v3.15.2 binary
//! before being written here: setting three environment variables,
//! `HELM_REPOSITORY_CONFIG`, `HELM_REPOSITORY_CACHE` and
//! `HELM_REGISTRY_CONFIG` (from `helm env`'s full variable list — the
//! others are Kubernetes-connection overrides this module already
//! bypasses by always passing `--kubeconfig`/`--namespace`/`--version`
//! explicitly), to paths inside this
//! run's own workspace gives complete isolation: a real `helm repo add`
//! run this way left the operator's actual
//! `~/.config/helm/repositories.yaml` and `~/.cache/helm/repository`
//! byte-for-byte/file-for-file unchanged (`sha256sum` before and after
//! matched, and a directory listing of the real cache showed no new or
//! modified files), never touched `~/.config/helm/registry/config.json`
//! at all, and `helm repo list` run with the same two variables listed
//! only the isolated repository just added — not any of the operator's
//! real ones. The same run also proved `helm repo add` creates the
//! *entire* directory chain for both variables itself when neither
//! exists yet, so [`HelmInstaller`] never needs to `mkdir` this
//! directory before invoking `helm`.
//!
//! `HELM_REGISTRY_CONFIG` was added to that set by Task 8.1 and is the
//! same guarantee extended to the path this module had previously never
//! taken: an OCI `helm upgrade --install` resolves its chart through
//! Helm's *registry* client, whose credential store is
//! `~/.config/helm/registry/config.json` — a different file from the two
//! above, and one the sentence directly above this used to be able to
//! claim was never touched precisely *because* no OCI reference ever
//! reached this module. Now that one does, the claim is kept by
//! redirecting the variable rather than by the absence of the feature.
//!
//! **Per-side, not per-run.** [`helm_state_dir`] namespaces this
//! directory by [`admissionlab_core::Side`] —
//! `<run's logs dir>/<side>-helm/` — not merely by run. A single shared
//! per-run file would still let a concurrent baseline/candidate install
//! (the shape a later stack-orchestration task is expected to use, the
//! same way Task 1.10 already creates both clusters concurrently) race
//! two `helm repo add` processes on the same `repositories.yaml`; giving
//! each side its own file removes that race entirely rather than merely
//! narrowing its window, at no extra cost (the directory is created on
//! demand by `helm` itself either way). This isolates baseline from
//! candidate — the only concurrency this codebase establishes today
//! (Task 1.10 creates both clusters at once) — but **not** two
//! components installed concurrently onto the *same* side: they would
//! still share one `<side>-helm/repositories.yaml`. Not a defect as of
//! this task (nothing here installs components concurrently), but
//! Task 2.6's stack-orchestration design should account for it before
//! choosing whether components within one side install one at a time or
//! concurrently.
//!
//! **Lives under [`admissionlab_core::RunPaths::logs`], not a new
//! `RunPaths` field.** `RunPaths` already has exactly this shape of
//! precedent: `admissionlab_cluster::lifecycle::ClusterLayout` stores
//! each side's *generated, non-secret, backend-scratch* files (its
//! rendered `kind` configuration, its audit policy document) directly
//! under `paths.logs()`, side-prefixed the same way
//! (`<side>-kind-config.yaml`). Helm's repository config/cache is the
//! same *kind* of thing — a backend's own working state, not a captured
//! admission object (`raw`/`normalized`), not a rendered report
//! (`reports`), and not cluster credential material (`kubeconfigs`,
//! which is further mode-`0600`-restricted per file — appropriate for a
//! kubeconfig, not for a public chart repository index). Reusing `logs`
//! avoids adding a field to a type documented as canonical for what
//! amounts to the same category of content it already holds.
//!
//! [`HelmInstaller::new`] therefore takes `&RunPaths` (not only a
//! [`ProcessRunner`]) and stores `paths.logs()`; [`ComponentInstaller::install`]'s
//! own signature is unchanged — the per-side directory is derived
//! inside `install` from `cluster.spec.side`, which that (unmodified)
//! signature already provides.
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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use admissionlab_core::{
    ClusterHandle, CommandResult, CommandSpec, Diagnostic, ProcessRunner, RedactedValue, RunPaths,
    Side,
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
/// operation it performs *without* `--wait` — in practice, running
/// pre-/post-install and pre-/post-upgrade hook Jobs to completion, and
/// waiting for any chart-bundled `CustomResourceDefinition`s to become
/// established. This module deliberately never passes `--wait` or
/// `--wait-for-jobs` (readiness is [`admissionlab_spec::ReadinessCheck`]'s
/// concern, probed separately by Task 2.4 once this install already
/// returned), so `--timeout` here does **not** bound, and this step does
/// not wait for, the main `Deployment`/`DaemonSet`/`StatefulSet`'s pods
/// actually scheduling or pulling their images — that is a materially
/// different (and for a real admission stack, usually much larger)
/// window than hook-Job/CRD-establishment waiting, and budgeting it is
/// Task 2.4's job, sized independently once this task's install has
/// already returned rather than here.
///
/// Chosen strictly shorter than [`UPGRADE_PROCESS_TIMEOUT`] (by two
/// minutes) so that, in the overwhelming majority of failure cases,
/// Helm's own timeout logic fires first: the child then exits non-zero
/// on its own, with an informative Helm-authored message on stderr,
/// which reaches the caller as a normal [`InstallError::CommandFailed`]
/// — far more diagnosable than a hard `SIGKILL` with no such message.
/// 480 seconds (8 minutes) — about 1.6x Helm's own 5-minute default —
/// gives comfortable headroom for a real chart's hook Job (some
/// admission-stack charts run one to seed CRDs, issue a webhook
/// certificate, or run a pre-flight check, and that Job's own container
/// image may itself need a cold pull) without the exact number being
/// load-bearing: because a too-generous value here only delays a
/// failure report rather than causing incorrect behavior, rounding up is
/// the safe direction when this task has no measured reference point for
/// "typical hook Job duration" the way `admissionlab_cluster::kind`'s
/// own `CREATE_TIMEOUT` (a `kind`-lifecycle constant this crate has no
/// dependency on, so it cannot be named as a doc link here) has directly
/// measured cold/warm `kind create cluster` figures to reason from.
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
///
/// `pub` because reproduction has to recognize it: a run manifest records
/// this value in `ComponentProvenance::version` exactly as it records a
/// real one, and `admissionlab_core::reproduce` must treat it as "no
/// recorded pin" rather than install `--version unknown`. That module
/// duplicates the literal (it sits below this crate and cannot import
/// it) and `admissionlab-cli`'s `tests/reproduce_command.rs` asserts the
/// two are equal, which is what keeps the duplication honest.
pub const UNCONFIRMED_VERSION: &str = "unknown";

/// Drives `helm` (via a shared [`ProcessRunner`]) to install a single
/// resolved Helm component. Holds the runner plus this run's own `logs`
/// directory (see the module documentation's "Helm state isolation"
/// section for what that directory is used for and why), so one
/// instance is safe to reuse across every component of both clusters
/// (baseline and candidate) of the run it was built for — the per-side
/// directory each `install` call actually uses is derived fresh each
/// time from `cluster.spec.side`, not fixed at construction, so a
/// single instance correctly serves both sides.
pub struct HelmInstaller {
    runner: Arc<dyn ProcessRunner>,
    /// This run's [`RunPaths::logs`] directory, captured once at
    /// construction. [`helm_state_dir`] joins this with a per-side
    /// subdirectory name on every `install` call.
    logs_dir: PathBuf,
}

impl HelmInstaller {
    /// Creates an installer that drives `helm` through `runner`, storing
    /// Helm's own client-side state (repository config and cache; see
    /// the module documentation) under `paths`'s `logs` directory.
    #[must_use]
    pub fn new(runner: Arc<dyn ProcessRunner>, paths: &RunPaths) -> Self {
        Self {
            runner,
            logs_dir: paths.logs().to_path_buf(),
        }
    }

    /// The one place every `helm`-targeting [`CommandSpec`] in this
    /// module is built: `program` is always [`HELM_PROGRAM`], `cwd` is
    /// always `None`, `sensitive_env_keys` is always empty (nothing this
    /// module ever passes to `helm`'s argv or env is credential-like),
    /// and `env` is always this run's Helm state isolation environment
    /// for `side` (see the module documentation's "Helm state
    /// isolation" section) -- computed here from `self.logs_dir`, not
    /// accepted as a parameter callers could substitute or omit. Every
    /// `helm` invocation this module makes (today: repo-add, upgrade
    /// --install, get metadata; a future `helm rollback`/`helm
    /// uninstall` for Task 2.6 would be no exception) must go through
    /// this method to become a runnable [`CommandSpec`] -- there is no
    /// other way in this module to produce one whose `program` is
    /// `helm`, so a new call site cannot silently reintroduce the
    /// original defect (`env: BTreeMap::new()`, inherited-ambient-
    /// environment) found in review: doing so would require bypassing
    /// this constructor and hand-building a `CommandSpec` directly, a
    /// deliberate departure from how every existing call site works,
    /// not an easy oversight.
    fn helm_command(&self, side: Side, args: Vec<OsString>, timeout: Duration) -> CommandSpec {
        let state_dir = helm_state_dir(&self.logs_dir, side);
        CommandSpec {
            program: HELM_PROGRAM.into(),
            args,
            cwd: None,
            env: helm_isolation_env(&state_dir),
            sensitive_env_keys: BTreeSet::new(),
            timeout,
        }
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
        side: Side,
    ) -> (String, Vec<Diagnostic>) {
        let spec = self.helm_command(
            side,
            get_metadata_args(helm, kubeconfig),
            GET_METADATA_TIMEOUT,
        );
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
        let side = cluster.spec.side;

        // An OCI chart reference names its own registry, so there is no
        // repository to register -- and `helm repo add` cannot parse one
        // at all. See the module documentation's "OCI chart references".
        if !is_oci_chart(&helm.chart) {
            self.run_and_check(
                &component.name,
                self.helm_command(side, repo_add_args(helm), REPO_ADD_TIMEOUT),
            )
            .await?;
        }
        self.run_and_check(
            &component.name,
            self.helm_command(
                side,
                upgrade_install_args(helm, &cluster.kubeconfig),
                UPGRADE_PROCESS_TIMEOUT,
            ),
        )
        .await?;

        let (resolved_version, diagnostics) = self
            .capture_resolved_version(&component.name, helm, &cluster.kubeconfig, side)
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

/// The scheme that marks a [`HelmInstallSpec::chart`] as an OCI
/// registry reference rather than a classic `<repo>/<chart>` shorthand.
///
/// Helm's own spelling, lowercase and with the `//`, exactly as its
/// documentation and its `helm pull`/`helm install` argument parser
/// write it.
const OCI_CHART_PREFIX: &str = "oci://";

/// Whether `chart` is an OCI registry reference, and therefore needs no
/// `helm repo add` step (module documentation, "OCI chart references").
///
/// A prefix test on the chart reference and nothing else: this is the
/// same thing `helm` itself decides from, and deriving it from the chart
/// means no second field can disagree with it.
fn is_oci_chart(chart: &str) -> bool {
    chart.starts_with(OCI_CHART_PREFIX)
}

/// Builds the argv (excluding the program name) for `helm repo add
/// <repo_name> <repo_url> --force-update`. Pure argv construction only
/// -- [`HelmInstaller::helm_command`] is what turns this into a runnable
/// [`CommandSpec`], carrying the isolation environment and timeout.
///
/// Never called for an OCI chart reference; see [`is_oci_chart`].
fn repo_add_args(helm: &HelmInstallSpec) -> Vec<OsString> {
    vec![
        "repo".into(),
        "add".into(),
        helm.repo_name.as_str().into(),
        helm.repo_url.as_str().into(),
        "--force-update".into(),
    ]
}

/// Builds the argv (excluding the program name) for `helm upgrade
/// --install`. See the module documentation for the exact flag set and
/// ordering rules. Pure argv construction only -- see [`repo_add_args`]'s
/// documentation for why this returns a bare `Vec<OsString>` rather than
/// a [`CommandSpec`].
fn upgrade_install_args(helm: &HelmInstallSpec, kubeconfig: &Path) -> Vec<OsString> {
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

    args
}

/// Builds the argv (excluding the program name) for `helm get metadata
/// <release> --namespace <namespace> --kubeconfig <kubeconfig> -o json`.
/// Pure argv construction only -- see [`repo_add_args`]'s documentation
/// for why this returns a bare `Vec<OsString>` rather than a
/// [`CommandSpec`].
fn get_metadata_args(helm: &HelmInstallSpec, kubeconfig: &Path) -> Vec<OsString> {
    vec![
        "get".into(),
        "metadata".into(),
        helm.release_name.as_str().into(),
        "--namespace".into(),
        helm.namespace.as_str().into(),
        "--kubeconfig".into(),
        kubeconfig.as_os_str().to_owned(),
        "-o".into(),
        "json".into(),
    ]
}

/// Computes the per-side directory this run's `helm` invocations store
/// their own client state under: `<run's logs dir>/<side>-helm/`. See
/// the module documentation's "Helm state isolation" section for why
/// this is per-side (not per-run) and why it lives under `logs`.
///
/// Pure: never touches the filesystem. The directory need not already
/// exist — `helm repo add` creates the full chain itself (verified
/// empirically; see the module documentation).
fn helm_state_dir(logs_dir: &Path, side: Side) -> PathBuf {
    logs_dir.join(format!("{}-helm", side.as_str()))
}

/// Builds the `HELM_REPOSITORY_CONFIG`/`HELM_REPOSITORY_CACHE`/
/// `HELM_REGISTRY_CONFIG` environment that isolates every `helm`
/// invocation in this module from the real, ambient `~/.config/helm` and
/// `~/.cache/helm` (see the module documentation's "Helm state
/// isolation" section).
///
/// All three are set unconditionally, for every invocation, rather than
/// `HELM_REGISTRY_CONFIG` only when the chart happens to be an OCI
/// reference: an isolation guarantee that depends on a per-install
/// condition is one somebody has to re-derive at every call site, and
/// pointing an unused variable at a file `helm` then never creates costs
/// nothing.
fn helm_isolation_env(state_dir: &Path) -> BTreeMap<OsString, OsString> {
    let mut env = BTreeMap::new();
    env.insert(
        OsString::from("HELM_REPOSITORY_CONFIG"),
        state_dir.join("repositories.yaml").into_os_string(),
    );
    env.insert(
        OsString::from("HELM_REPOSITORY_CACHE"),
        state_dir.join("repository").into_os_string(),
    );
    env.insert(
        OsString::from("HELM_REGISTRY_CONFIG"),
        state_dir.join("registry-config.json").into_os_string(),
    );
    env
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
