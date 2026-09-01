//! Unit tests for `admissionlab_installer`'s Helm installation backend
//! ([`admissionlab_installer::HelmInstaller`]).
//!
//! No test here executes a real `helm`, `kind`, `docker`, or `kubectl`:
//! every test drives [`HelmInstaller`] through [`FakeProcessRunner`], a
//! [`ProcessRunner`] that never spawns a real process (mirroring
//! `admissionlab-cluster`'s `tests/lifecycle_unit.rs` and
//! `admissionlab-core`'s `tests/tool.rs`).
//!
//! Covers Task 2.2 brief:
//! - Step 1 (exact argv) — `repo_add_uses_exact_argv_with_force_update`,
//!   `upgrade_install_uses_exact_argv_with_required_flags`,
//!   `multiple_values_files_each_produce_their_own_values_flag`,
//!   `set_values_use_set_string_not_set_with_dotted_keys_intact`,
//!   `kubeconfig_is_always_the_clusters_own_and_never_layered_via_env`,
//!   `values_file_path_with_space_stays_one_argv_element`.
//! - Non-zero exits and spawn failures surfacing as `InstallError`
//!   rather than a panic — `upgrade_install_nonzero_exit_*`,
//!   `repo_add_nonzero_exit_*`, `helm_not_found_*`.
//! - Step 3 (`helm get metadata`) — the `get_metadata_*` tests.
//!   `get_metadata_nonzero_exit_leaves_resolved_version_unknown_with_diagnostic`
//!   additionally asserts `resolved_version` differs from the
//!   requested/pinned version, proving the "never fabricated" property
//!   directly rather than merely checking it equals the sentinel.
//! - A non-Helm install method is rejected without running anything
//!   (`manifests_install_method_is_rejected_without_invoking_runner`),
//!   and the three commands run in the documented order
//!   (`calls_happen_in_order_repo_add_then_upgrade_then_get_metadata`).
//! - Helm state isolation from the user's real `~/.config/helm` /
//!   `~/.cache/helm` — `helm_state_env_vars_are_set_on_every_invocation_and_point_inside_the_run_workspace`
//!   and `helm_state_directory_differs_between_baseline_and_candidate`
//!   (found in review; see `helm.rs`'s module documentation for the
//!   empirical verification this is based on).
//!   `kubeconfig_is_always_the_clusters_own_and_never_layered_via_env`
//!   is updated accordingly: it now asserts no `KUBECONFIG` override is
//!   ever layered in, rather than that `env` is empty outright.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use admissionlab_core::{
    ClusterHandle, ClusterSpec, CommandResult, CommandSpec, ProcessError, ProcessRunner, RunId,
    RunPaths, Side,
};
use admissionlab_installer::{ComponentInstaller, HelmInstaller, InstallError};
use admissionlab_spec::component::HelmInstallSpec;
use admissionlab_spec::{InstallMethod, ManifestInstallSpec, ResolvedComponent};
use async_trait::async_trait;

// ---------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------

/// Builds a normal-exit [`ExitStatus`] reporting `code`, using the
/// well-known Unix `wait(2)` encoding, mirroring
/// `admissionlab-cluster`'s `tests/lifecycle_unit.rs`'s own
/// `exit_status` helper.
fn exit_status(code: i32) -> ExitStatus {
    ExitStatus::from_raw(code << 8)
}

/// A fresh, arbitrary [`RunPaths`] for one test. `RunPaths::new` performs
/// no filesystem IO, so this never touches disk; each call gets its own
/// [`RunId`] so two tests never accidentally compute the same path.
fn test_run_paths() -> RunPaths {
    RunPaths::new(Path::new("/fake-run-root"), &RunId::generate())
}

/// A representative, fully resolved Helm install spec, matching the
/// shape `admissionlab_spec::resolve_lab` would produce for a simple
/// component. Individual tests override only the fields they care
/// about.
fn default_helm_spec() -> HelmInstallSpec {
    HelmInstallSpec {
        repo_name: "ingress-nginx".to_owned(),
        repo_url: "https://kubernetes.github.io/ingress-nginx".to_owned(),
        chart: "ingress-nginx/ingress-nginx".to_owned(),
        version: "4.11.3".to_owned(),
        release_name: "ingress-nginx".to_owned(),
        namespace: "ingress-nginx".to_owned(),
        values_files: Vec::new(),
        set_values: BTreeMap::new(),
    }
}

/// Wraps `helm` in a [`ResolvedComponent`] named after its own release.
fn component_with(helm: HelmInstallSpec) -> ResolvedComponent {
    ResolvedComponent {
        name: "ingress-nginx".to_owned(),
        version: helm.version.clone(),
        install: InstallMethod::Helm(helm),
        readiness: Vec::new(),
        recipe_normalize_rules: Vec::new(),
        capabilities: BTreeSet::new(),
    }
}

/// A resolved component whose install method is `Manifests`, not
/// `Helm` — used to prove [`HelmInstaller`] rejects it up front.
fn manifests_component() -> ResolvedComponent {
    ResolvedComponent {
        name: "raw-manifests".to_owned(),
        version: "1.0.0".to_owned(),
        install: InstallMethod::Manifests(ManifestInstallSpec {
            paths: vec![PathBuf::from("/config/manifests/whatever.yaml")],
        }),
        readiness: Vec::new(),
        recipe_normalize_rules: Vec::new(),
        capabilities: BTreeSet::new(),
    }
}

/// A [`ClusterHandle`] whose kubeconfig is `kubeconfig` — deliberately
/// distinctive in every test so a test can prove this exact path (and
/// not some other, ambient one) was passed to `helm`.
fn cluster_handle(kubeconfig: &str) -> ClusterHandle {
    ClusterHandle {
        spec: ClusterSpec {
            side: Side::Baseline,
            name: "adlab-baseline-testcluster01".to_owned(),
            kubernetes_version: "1.36.4".to_owned(),
            node_image: "kindest/node:v1.36.4@sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed".to_owned(),
            images: Vec::new(),
        },
        kubeconfig: PathBuf::from(kubeconfig),
        audit_log: PathBuf::from("/run/adlab/baseline-audit.log"),
    }
}

/// Like [`cluster_handle`], but for an arbitrary [`Side`] — used to prove
/// baseline and candidate get their own, distinct Helm state directories.
fn cluster_handle_with_side(kubeconfig: &str, side: Side) -> ClusterHandle {
    ClusterHandle {
        spec: ClusterSpec {
            side,
            name: format!("adlab-{}-testcluster01", side.as_str()),
            kubernetes_version: "1.36.4".to_owned(),
            node_image: "kindest/node:v1.36.4@sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed".to_owned(),
            images: Vec::new(),
        },
        kubeconfig: PathBuf::from(kubeconfig),
        audit_log: PathBuf::from(format!("/run/adlab/{}-audit.log", side.as_str())),
    }
}

/// One scripted response [`FakeProcessRunner`] gives for a `helm`
/// invocation, keyed by [`step_key`].
#[derive(Clone)]
enum FakeOutcome {
    /// The command ran and exited 0 with this stdout.
    Success(&'static [u8]),
    /// The command ran but exited non-zero, with this stderr.
    Failure(&'static [u8]),
    /// The command exceeded its timeout.
    TimedOut,
    /// The command could not be spawned at all (simulates `helm` not
    /// being on `PATH`).
    Missing,
}

/// Identifies which `helm` step `args` invokes, by its leading
/// subcommand tokens. Anything unrecognized (including an empty argv)
/// maps to `"unknown"`, which — with no outcome registered for it — the
/// fake treats the same as [`FakeOutcome::Missing`].
fn step_key(args: &[OsString]) -> &'static str {
    let first = args.first().and_then(|arg| arg.to_str());
    let second = args.get(1).and_then(|arg| arg.to_str());
    match (first, second) {
        (Some("repo"), Some("add")) => "repo add",
        (Some("upgrade"), _) => "upgrade",
        (Some("get"), Some("metadata")) => "get metadata",
        _ => "unknown",
    }
}

/// A [`ProcessRunner`] that never spawns a real process: it returns a
/// scripted [`FakeOutcome`] keyed by [`step_key`], and records every
/// [`CommandSpec`] it was given so a test can assert on the exact argv
/// [`HelmInstaller`] built.
struct FakeProcessRunner {
    outcomes: BTreeMap<&'static str, FakeOutcome>,
    calls: Mutex<Vec<CommandSpec>>,
}

impl FakeProcessRunner {
    fn new() -> Self {
        Self {
            outcomes: BTreeMap::new(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn with(mut self, step: &'static str, outcome: FakeOutcome) -> Self {
        self.outcomes.insert(step, outcome);
        self
    }

    fn calls(&self) -> Vec<CommandSpec> {
        self.calls.lock().expect("calls mutex poisoned").clone()
    }
}

/// A [`FakeProcessRunner`] that scripts a successful repo-add, upgrade,
/// and metadata call — the "everything succeeds" baseline most
/// argv-shape tests build on.
fn happy_path_runner() -> FakeProcessRunner {
    FakeProcessRunner::new()
        .with("repo add", FakeOutcome::Success(b""))
        .with(
            "upgrade",
            FakeOutcome::Success(b"Release \"ingress-nginx\" has been upgraded.\n"),
        )
        .with(
            "get metadata",
            FakeOutcome::Success(
                br#"{"name":"ingress-nginx","chart":"ingress-nginx","version":"4.11.3","appVersion":"1.11.3","namespace":"ingress-nginx","revision":1}"#,
            ),
        )
}

#[async_trait]
impl ProcessRunner for FakeProcessRunner {
    async fn run(&self, spec: CommandSpec) -> Result<CommandResult, ProcessError> {
        self.calls
            .lock()
            .expect("calls mutex poisoned")
            .push(spec.clone());

        match self.outcomes.get(step_key(&spec.args)) {
            Some(FakeOutcome::Success(stdout)) => Ok(CommandResult {
                status: exit_status(0),
                stdout: stdout.to_vec(),
                stderr: Vec::new(),
                elapsed: Duration::from_millis(1),
            }),
            Some(FakeOutcome::Failure(stderr)) => Ok(CommandResult {
                status: exit_status(1),
                stdout: Vec::new(),
                stderr: stderr.to_vec(),
                elapsed: Duration::from_millis(1),
            }),
            Some(FakeOutcome::TimedOut) => Err(ProcessError::TimedOut {
                context: Box::new(spec.context()),
                timeout: spec.timeout,
                elapsed: spec.timeout,
                stdout: Vec::new(),
                stderr: Vec::new(),
            }),
            Some(FakeOutcome::Missing) | None => Err(ProcessError::Spawn {
                context: Box::new(spec.context()),
                source: io::Error::new(io::ErrorKind::NotFound, "No such file or directory"),
            }),
        }
    }
}

/// Scans `args` for `flag` and returns the single value that follows it.
fn find_flag<'a>(args: &'a [OsString], flag: &str) -> Option<&'a OsString> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
}

/// Scans `args` for every occurrence of `flag` and returns the value
/// that follows each one, in order.
fn find_all_flag_values<'a>(args: &'a [OsString], flag: &str) -> Vec<&'a OsString> {
    args.iter()
        .zip(args.iter().skip(1))
        .filter(|(candidate, _)| *candidate == flag)
        .map(|(_, value)| value)
        .collect()
}

// ---------------------------------------------------------------------
// Step 1: exact argv
// ---------------------------------------------------------------------

#[tokio::test]
async fn repo_add_uses_exact_argv_with_force_update() {
    let runner = Arc::new(happy_path_runner());
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let component = component_with(helm.clone());
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    let calls = runner.calls();
    assert_eq!(calls[0].program, OsString::from("helm"));
    assert_eq!(
        calls[0].args,
        vec![
            OsString::from("repo"),
            OsString::from("add"),
            OsString::from(helm.repo_name.clone()),
            OsString::from(helm.repo_url.clone()),
            OsString::from("--force-update"),
        ]
    );
}

#[tokio::test]
async fn upgrade_install_uses_exact_argv_with_required_flags() {
    let runner = Arc::new(happy_path_runner());
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let component = component_with(helm.clone());
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    let calls = runner.calls();
    let upgrade = &calls[1];
    assert_eq!(upgrade.program, OsString::from("helm"));
    assert_eq!(upgrade.args[0], OsString::from("upgrade"));
    assert_eq!(upgrade.args[1], OsString::from("--install"));
    assert_eq!(upgrade.args[2], OsString::from(helm.release_name.clone()));
    assert_eq!(upgrade.args[3], OsString::from(helm.chart.clone()));

    assert_eq!(
        find_flag(&upgrade.args, "--version"),
        Some(&OsString::from(helm.version.clone()))
    );
    assert_eq!(
        find_flag(&upgrade.args, "--namespace"),
        Some(&OsString::from(helm.namespace.clone()))
    );
    assert!(
        upgrade.args.iter().any(|arg| arg == "--create-namespace"),
        "--create-namespace is load-bearing: neither istio/base nor \
         istio/istiod creates its own target namespace"
    );
    assert_eq!(
        find_flag(&upgrade.args, "--kubeconfig"),
        Some(&OsString::from("/run/adlab/baseline.kubeconfig"))
    );
    assert!(
        find_flag(&upgrade.args, "--timeout").is_some(),
        "helm's own --timeout must be passed explicitly, not left to helm's default"
    );

    // No values files and no set_values were configured, so neither
    // flag should appear at all.
    assert!(!upgrade.args.iter().any(|arg| arg == "--values"));
    assert!(!upgrade.args.iter().any(|arg| arg == "--set-string"));
}

#[tokio::test]
async fn multiple_values_files_each_produce_their_own_values_flag() {
    let runner = Arc::new(happy_path_runner());
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let mut helm = default_helm_spec();
    helm.values_files = vec![
        PathBuf::from("/config/base-values.yaml"),
        PathBuf::from("/config/overrides/prod.yaml"),
    ];
    let component = component_with(helm);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    let calls = runner.calls();
    let upgrade_args = &calls[1].args;

    assert_eq!(
        upgrade_args.iter().filter(|arg| *arg == "--values").count(),
        2,
        "each values file must get its own --values flag, never comma-joined"
    );
    assert_eq!(
        find_all_flag_values(upgrade_args, "--values"),
        vec![
            &OsString::from("/config/base-values.yaml"),
            &OsString::from("/config/overrides/prod.yaml"),
        ]
    );
}

#[tokio::test]
async fn set_values_use_set_string_not_set_with_dotted_keys_intact() {
    let runner = Arc::new(happy_path_runner());
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let mut helm = default_helm_spec();
    helm.set_values = BTreeMap::from([
        (
            "global.leaderElection.namespace".to_owned(),
            "candidate-system".to_owned(),
        ),
        ("controller.image.tag".to_owned(), "1.2.3".to_owned()),
    ]);
    let component = component_with(helm);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    let calls = runner.calls();
    let upgrade_args = &calls[1].args;

    assert!(
        !upgrade_args.iter().any(|arg| arg == "--set"),
        "must never use --set (type-inferring) for literal string overrides"
    );
    assert_eq!(
        upgrade_args
            .iter()
            .filter(|arg| *arg == "--set-string")
            .count(),
        2
    );
    assert_eq!(
        find_all_flag_values(upgrade_args, "--set-string"),
        vec![
            // BTreeMap iterates in sorted key order: "controller..." <
            // "global...".
            &OsString::from("controller.image.tag=1.2.3"),
            &OsString::from("global.leaderElection.namespace=candidate-system"),
        ],
        "dotted keys must survive verbatim, joined to their value as key=value"
    );
}

#[tokio::test]
async fn kubeconfig_is_always_the_clusters_own_and_never_layered_via_env() {
    let runner = Arc::new(happy_path_runner());
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let component = component_with(helm);
    let distinctive_kubeconfig = "/run/adlab/run-9f2c/candidate.kubeconfig";
    let cluster = cluster_handle(distinctive_kubeconfig);

    installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    let calls = runner.calls();
    for call in &calls {
        // The only env this installer ever sets is its own Helm state
        // isolation (HELM_REPOSITORY_CONFIG/HELM_REPOSITORY_CACHE, proven
        // by `helm_state_env_vars_are_set_on_every_invocation_and_point_inside_the_run_workspace`
        // below) -- kubeconfig selection must go through --kubeconfig
        // alone, never a KUBECONFIG env override.
        assert!(
            !call.env.contains_key(&OsString::from("KUBECONFIG")),
            "no helm invocation may layer a KUBECONFIG env override -- kubeconfig \
             selection must go through --kubeconfig alone"
        );
        assert!(call.sensitive_env_keys.is_empty());
    }

    assert_eq!(
        find_flag(&calls[1].args, "--kubeconfig"),
        Some(&OsString::from(distinctive_kubeconfig)),
        "helm upgrade --install must use this cluster's own kubeconfig"
    );
    assert_eq!(
        find_flag(&calls[2].args, "--kubeconfig"),
        Some(&OsString::from(distinctive_kubeconfig)),
        "helm get metadata must use this cluster's own kubeconfig"
    );
    // `helm repo add` is a local, cluster-independent operation and
    // must never receive --kubeconfig at all.
    assert!(find_flag(&calls[0].args, "--kubeconfig").is_none());
}

#[tokio::test]
async fn values_file_path_with_space_stays_one_argv_element() {
    let runner = Arc::new(happy_path_runner());
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let mut helm = default_helm_spec();
    helm.values_files = vec![PathBuf::from("/config/needs quoting/values.yaml")];
    let component = component_with(helm);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    let calls = runner.calls();
    let upgrade_args = &calls[1].args;

    assert_eq!(
        find_flag(upgrade_args, "--values"),
        Some(&OsString::from("/config/needs quoting/values.yaml")),
        "a values file path containing a space must remain exactly one argv element"
    );
    // If the path had been split on its embedded space (for example by
    // building it through a joined/shell-interpreted string instead of
    // one OsString), this element would be the truncated `/config/needs`
    // and a stray `quoting/values.yaml` would appear as its own element.
    assert_eq!(
        upgrade_args
            .iter()
            .filter(|arg| *arg == "quoting/values.yaml")
            .count(),
        0
    );
}

// ---------------------------------------------------------------------
// Non-zero exits and spawn failures: InstallError, never a panic
// ---------------------------------------------------------------------

#[tokio::test]
async fn upgrade_install_nonzero_exit_surfaces_as_install_error_with_stderr() {
    let runner = Arc::new(FakeProcessRunner::new().with("repo add", FakeOutcome::Success(b"")).with(
        "upgrade",
        FakeOutcome::Failure(
            b"Error: INSTALLATION FAILED: failed post-install: timed out waiting for the condition\n",
        ),
    ));
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let component = component_with(helm);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let error = installer
        .install(&cluster, &component)
        .await
        .expect_err("a non-zero helm exit must surface as an error, not a panic");

    match error {
        InstallError::CommandFailed {
            component: failed_component,
            stderr,
            status,
            ..
        } => {
            assert_eq!(failed_component, "ingress-nginx");
            assert!(!status.success());
            assert!(String::from_utf8_lossy(&stderr).contains("INSTALLATION FAILED"));
        }
        other => panic!("expected InstallError::CommandFailed, got {other:?}"),
    }

    assert_eq!(
        runner.calls().len(),
        2,
        "repo add then the failed upgrade -- get metadata must never be attempted after a \
         failed install"
    );
}

#[tokio::test]
async fn repo_add_nonzero_exit_surfaces_as_install_error_and_skips_upgrade() {
    let runner = Arc::new(FakeProcessRunner::new().with(
        "repo add",
        FakeOutcome::Failure(
            b"Error: looks like \"https://example.invalid\" is not a valid chart repository\n",
        ),
    ));
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let component = component_with(helm);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let error = installer
        .install(&cluster, &component)
        .await
        .expect_err("a non-zero repo add exit must surface as an error");

    assert!(matches!(error, InstallError::CommandFailed { .. }));
    assert_eq!(
        runner.calls().len(),
        1,
        "helm upgrade --install must never run after repo add fails"
    );
}

#[tokio::test]
async fn helm_not_found_surfaces_as_install_error_process_variant() {
    let runner = Arc::new(FakeProcessRunner::new().with("repo add", FakeOutcome::Missing));
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let component = component_with(helm);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let error = installer
        .install(&cluster, &component)
        .await
        .expect_err("a missing helm binary must surface as an error");

    match error {
        InstallError::Process { component, source } => {
            assert_eq!(component, "ingress-nginx");
            assert!(matches!(source, ProcessError::Spawn { .. }));
        }
        other => panic!("expected InstallError::Process, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Step 3: helm get metadata / resolved_version
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_metadata_success_captures_resolved_version_with_no_diagnostic() {
    let runner = Arc::new(happy_path_runner());
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let component = component_with(helm);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let record = installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    assert_eq!(record.component, "ingress-nginx");
    assert_eq!(record.method, "helm");
    assert_eq!(record.resolved_version, "4.11.3");
    assert!(record.diagnostics.is_empty());
}

#[tokio::test]
async fn get_metadata_nonzero_exit_leaves_resolved_version_unknown_with_diagnostic() {
    let runner = Arc::new(
        FakeProcessRunner::new()
            .with("repo add", FakeOutcome::Success(b""))
            .with("upgrade", FakeOutcome::Success(b""))
            .with(
                "get metadata",
                FakeOutcome::Failure(b"Error: release: not found\n"),
            ),
    );
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let requested_version = helm.version.clone();
    let component = component_with(helm);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let record = installer
        .install(&cluster, &component)
        .await
        .expect("a metadata failure must not fail the whole install");

    assert_eq!(record.resolved_version, "unknown");
    assert_ne!(
        record.resolved_version, requested_version,
        "resolved_version must never fall back to the requested/pinned version -- that would \
         be fabricating a confirmation that never happened"
    );
    assert_eq!(record.diagnostics.len(), 1);
    assert!(!record.diagnostics[0].code.is_empty());
}

#[tokio::test]
async fn get_metadata_malformed_json_leaves_resolved_version_unknown_with_diagnostic() {
    let runner = Arc::new(
        FakeProcessRunner::new()
            .with("repo add", FakeOutcome::Success(b""))
            .with("upgrade", FakeOutcome::Success(b""))
            .with("get metadata", FakeOutcome::Success(b"not json at all")),
    );
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let component = component_with(helm);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let record = installer
        .install(&cluster, &component)
        .await
        .expect("a metadata parse failure must not fail the whole install");

    assert_eq!(record.resolved_version, "unknown");
    assert_eq!(record.diagnostics.len(), 1);
}

#[tokio::test]
async fn get_metadata_missing_version_field_leaves_resolved_version_unknown() {
    let runner = Arc::new(
        FakeProcessRunner::new()
            .with("repo add", FakeOutcome::Success(b""))
            .with("upgrade", FakeOutcome::Success(b""))
            .with(
                "get metadata",
                FakeOutcome::Success(br#"{"name":"ingress-nginx","chart":"ingress-nginx"}"#),
            ),
    );
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let component = component_with(helm);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let record = installer
        .install(&cluster, &component)
        .await
        .expect("well-formed JSON missing a version field must not fail the whole install");

    assert_eq!(record.resolved_version, "unknown");
    assert_eq!(record.diagnostics.len(), 1);
}

#[tokio::test]
async fn get_metadata_process_error_leaves_resolved_version_unknown_with_diagnostic() {
    let runner = Arc::new(
        FakeProcessRunner::new()
            .with("repo add", FakeOutcome::Success(b""))
            .with("upgrade", FakeOutcome::Success(b""))
            .with("get metadata", FakeOutcome::TimedOut),
    );
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let component = component_with(helm);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let record = installer
        .install(&cluster, &component)
        .await
        .expect("a metadata process failure must not fail the whole install");

    assert_eq!(record.resolved_version, "unknown");
    assert_eq!(record.diagnostics.len(), 1);
}

#[tokio::test]
async fn get_metadata_uses_json_output_flag() {
    let runner = Arc::new(happy_path_runner());
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let component = component_with(helm.clone());
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    let calls = runner.calls();
    let metadata_args = &calls[2].args;
    assert_eq!(metadata_args[0], OsString::from("get"));
    assert_eq!(metadata_args[1], OsString::from("metadata"));
    assert_eq!(metadata_args[2], OsString::from(helm.release_name.clone()));
    assert_eq!(
        find_flag(metadata_args, "--namespace"),
        Some(&OsString::from(helm.namespace.clone()))
    );
    assert_eq!(
        find_flag(metadata_args, "-o"),
        Some(&OsString::from("json"))
    );
}

// ---------------------------------------------------------------------
// Method dispatch and call ordering
// ---------------------------------------------------------------------

#[tokio::test]
async fn manifests_install_method_is_rejected_without_invoking_runner() {
    let runner = Arc::new(FakeProcessRunner::new());
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let component = manifests_component();
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let error = installer
        .install(&cluster, &component)
        .await
        .expect_err("a manifests component must be rejected");

    assert!(matches!(error, InstallError::UnsupportedMethod { .. }));
    assert!(
        runner.calls().is_empty(),
        "the Helm installer must not run anything for a non-Helm component"
    );
}

#[tokio::test]
async fn calls_happen_in_order_repo_add_then_upgrade_then_get_metadata() {
    let runner = Arc::new(happy_path_runner());
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let component = component_with(helm);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    let calls = runner.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(step_key(&calls[0].args), "repo add");
    assert_eq!(step_key(&calls[1].args), "upgrade");
    assert_eq!(step_key(&calls[2].args), "get metadata");
}

// ---------------------------------------------------------------------
// Helm state isolation: never the user's real ~/.config/helm or
// ~/.cache/helm (found in review after the initial Task 2.2 report --
// see helm.rs's module documentation for the empirical verification
// this fix is based on).
// ---------------------------------------------------------------------

#[tokio::test]
async fn helm_state_env_vars_are_set_on_every_invocation_and_point_inside_the_run_workspace() {
    let runner = Arc::new(happy_path_runner());
    let run_paths = test_run_paths();
    let installer = HelmInstaller::new(runner.clone(), &run_paths);
    let helm = default_helm_spec();
    let component = component_with(helm);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    let expected_config = run_paths
        .logs()
        .join("baseline-helm")
        .join("repositories.yaml")
        .into_os_string();
    let expected_cache = run_paths
        .logs()
        .join("baseline-helm")
        .join("repository")
        .into_os_string();

    let calls = runner.calls();
    assert_eq!(calls.len(), 3);
    for call in &calls {
        assert_eq!(
            call.env.get(&OsString::from("HELM_REPOSITORY_CONFIG")),
            Some(&expected_config),
            "HELM_REPOSITORY_CONFIG must point inside this run's own workspace, never the \
             user's real ~/.config/helm/repositories.yaml"
        );
        assert_eq!(
            call.env.get(&OsString::from("HELM_REPOSITORY_CACHE")),
            Some(&expected_cache),
            "HELM_REPOSITORY_CACHE must point inside this run's own workspace, never the \
             user's real ~/.cache/helm/repository"
        );
        // Exactly these two keys: nothing else is layered in, so nothing
        // else (KUBECONFIG included) can be inherited by accident either.
        assert_eq!(call.env.len(), 2);
        assert!(call.sensitive_env_keys.is_empty());
    }
}

#[tokio::test]
async fn helm_state_directory_differs_between_baseline_and_candidate() {
    let runner = Arc::new(happy_path_runner());
    let run_paths = test_run_paths();
    // One installer instance, reused for both sides -- proving the
    // per-side directory comes from each call's own `ClusterHandle`,
    // not from anything fixed at construction time.
    let installer = HelmInstaller::new(runner.clone(), &run_paths);

    let baseline_cluster =
        cluster_handle_with_side("/run/adlab/baseline.kubeconfig", Side::Baseline);
    installer
        .install(&baseline_cluster, &component_with(default_helm_spec()))
        .await
        .expect("baseline install should succeed");

    let candidate_cluster =
        cluster_handle_with_side("/run/adlab/candidate.kubeconfig", Side::Candidate);
    installer
        .install(&candidate_cluster, &component_with(default_helm_spec()))
        .await
        .expect("candidate install should succeed");

    let calls = runner.calls();
    assert_eq!(
        calls.len(),
        6,
        "3 helm invocations per install, for two installs"
    );

    let baseline_config = calls[0]
        .env
        .get(&OsString::from("HELM_REPOSITORY_CONFIG"))
        .expect("baseline repo add must set HELM_REPOSITORY_CONFIG");
    let candidate_config = calls[3]
        .env
        .get(&OsString::from("HELM_REPOSITORY_CONFIG"))
        .expect("candidate repo add must set HELM_REPOSITORY_CONFIG");

    assert_ne!(
        baseline_config, candidate_config,
        "baseline and candidate must never share a Helm repository config file -- sharing \
         one would race under concurrent installs the same way `kind delete` once raced on \
         ~/.kube/config"
    );
    assert_eq!(
        baseline_config,
        &run_paths
            .logs()
            .join("baseline-helm")
            .join("repositories.yaml")
            .into_os_string()
    );
    assert_eq!(
        candidate_config,
        &run_paths
            .logs()
            .join("candidate-helm")
            .join("repositories.yaml")
            .into_os_string()
    );
}
