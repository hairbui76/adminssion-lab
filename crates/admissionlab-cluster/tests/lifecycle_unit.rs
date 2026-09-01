//! Unit tests for `admissionlab_cluster`'s `kind`-backed
//! [`admissionlab_core::ClusterManager`] implementation
//! ([`admissionlab_cluster::KindClusterManager`]) and its cluster-naming
//! helpers ([`admissionlab_cluster::cluster_name`],
//! [`admissionlab_cluster::validate_cluster_name`]).
//!
//! No test here executes a real `kind`, `docker`, or `kubectl`: every
//! test drives [`KindClusterManager`] through [`FakeProcessRunner`], a
//! [`ProcessRunner`] that never spawns a real process (mirroring
//! `admissionlab-core`'s own `tests/tool.rs`). Task 1.9 owns the real
//! Docker/kind integration test.
//!
//! Covers Task 1.7 brief:
//! - Step 1 (exact argv for create/delete) —
//!   `create_invokes_kind_with_generated_config_and_explicit_kubeconfig_path`,
//!   `delete_invokes_kind_with_exact_cluster_name`.
//! - Step 3 (cluster names) — the `cluster_name`/`validate_cluster_name`
//!   tests, including the DNS-1123 trailing-hyphen regression carried
//!   forward from Task 0.3.
//! - Step 4 (rollback on partial create failure) — the
//!   `create_rolls_back_*`/`create_rollback_*`/`create_does_not_roll_back_*`
//!   tests.
//!
//! Plus: absolute-`RunPaths` validation, kubeconfig/audit-log isolation
//! between baseline and candidate sharing one `RunPaths`, `diagnostics`
//! never failing, and (in both argv tests) that neither `create` nor
//! `delete` layers anything onto the child's inherited environment --
//! the invariant the no-`KUBECONFIG`-leak guarantee rests on.
//!
//! One check does *not* live here: the proof that `kind.rs`'s private
//! `AUDIT_LOG_FILE_NAME` constant matches what `render_kind_config`
//! actually configures lives in `kind.rs`'s own inline `#[cfg(test)]`
//! module instead, because it needs to reference that `pub(crate)`
//! constant directly -- an external test crate like this one cannot see
//! it, and a copy of the check living here could only ever hardcode the
//! same literal a second time, which would not detect the constant
//! drifting.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use admissionlab_cluster::{KindClusterManager, cluster_name, validate_cluster_name};
use admissionlab_core::{
    ArtifactStore, ClusterError, ClusterHandle, ClusterManager, ClusterSpec, CommandResult,
    CommandSpec, OutputOverflow, ProcessError, ProcessRunner, RollbackOutcome, RunId, RunPaths,
    Side,
};
use async_trait::async_trait;

// ---------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------

/// A fresh `tokio` runtime for driving one test's async calls. Mirrors
/// `admissionlab-core`'s `tests/tool.rs`/`tests/artifact.rs` hand-rolled
/// runtime rather than `#[tokio::test]`, which would need this crate's
/// dev-only `tokio` dependency to also carry the `macros` feature for no
/// other reason.
fn test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build test tokio runtime")
}

fn block_on<F: Future>(future: F) -> F::Output {
    test_runtime().block_on(future)
}

/// A fresh, guaranteed-unique scratch directory under the OS temp dir.
/// Mirrors `admissionlab-core`'s `tests/artifact.rs`'s
/// `unique_store_root` rather than pulling in a new dependency just for
/// test-only temp directories.
fn unique_root(label: &str) -> PathBuf {
    let unique = RunId::generate();
    std::env::temp_dir().join(format!(
        "admissionlab-cluster-lifecycle-test-{label}-{}",
        unique.as_str()
    ))
}

/// Creates a fresh, real (absolute, on-disk) [`RunPaths`] for one test,
/// via a real [`ArtifactStore::create_run`] -- `KindClusterManager`
/// writes through an `ArtifactStore` it derives from `paths.root()`, so
/// tests that call `create` need every directory `RunPaths` names to
/// genuinely exist.
async fn new_run_paths(label: &str) -> RunPaths {
    let root = unique_root(label);
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();
    store
        .create_run(&run_id)
        .await
        .expect("create_run should succeed under a fresh temp root")
}

fn baseline_spec(name: &str) -> ClusterSpec {
    ClusterSpec {
        side: Side::Baseline,
        name: name.to_owned(),
        kubernetes_version: "1.36.4".to_owned(),
        node_image: "kindest/node:v1.36.4@sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed".to_owned(),
        images: Vec::new(),
    }
}

/// Builds a normal-exit [`ExitStatus`] reporting `code`, using the
/// well-known Unix `wait(2)` encoding, mirroring
/// `admissionlab-core`'s `tests/tool.rs`'s own `exit_status` helper.
fn exit_status(code: i32) -> ExitStatus {
    ExitStatus::from_raw(code << 8)
}

/// Scans `args` for `--kubeconfig` and returns the path that follows it.
/// Used for both `create` and `delete` argv -- both now always carry
/// this flag.
fn kubeconfig_arg(args: &[OsString]) -> PathBuf {
    args.iter()
        .position(|arg| arg == "--kubeconfig")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from)
        .expect("test fake: this kind invocation must carry --kubeconfig")
}

/// One scripted response [`FakeProcessRunner`] gives for a `kind`
/// subcommand, keyed by [`CommandSpec::args`]'s first element
/// (`"create"`, `"delete"`, or `"get"`).
#[derive(Clone)]
enum FakeOutcome {
    /// The command ran and exited 0 with this stdout.
    Success(&'static [u8]),
    /// The command ran but exited non-zero, with this stderr.
    Failure(&'static [u8]),
    /// The command exceeded its timeout (simulates `kind` being killed
    /// mid-provisioning, before its own cleanup-on-failure logic could
    /// run).
    TimedOut,
    /// The command could not be spawned at all (simulates `kind` not
    /// being on `PATH`).
    Missing,
}

/// A [`ProcessRunner`] that never spawns a real process: it returns a
/// scripted [`FakeOutcome`] keyed by the `kind` subcommand
/// (`args.first()`), and records every [`CommandSpec`] it was given so
/// a test can assert on the exact argv `KindClusterManager` built.
///
/// A scripted `create` success additionally writes `kubeconfig_write`'s
/// bytes (when `Some`) to the path named by its own `--kubeconfig`
/// argument, simulating what a real `kind create cluster --kubeconfig
/// <path>` does -- `None` simulates `kind` reporting success while
/// leaving no usable kubeconfig behind (Task 1.7 brief Step 4's
/// scenario).
struct FakeProcessRunner {
    outcomes: BTreeMap<&'static str, FakeOutcome>,
    kubeconfig_write: Option<&'static [u8]>,
    calls: Mutex<Vec<CommandSpec>>,
}

impl FakeProcessRunner {
    fn new() -> Self {
        Self {
            outcomes: BTreeMap::new(),
            kubeconfig_write: Some(b"apiVersion: v1\nkind: Config\nclusters: []\n"),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn with(mut self, subcommand: &'static str, outcome: FakeOutcome) -> Self {
        self.outcomes.insert(subcommand, outcome);
        self
    }

    /// Simulates `kind create cluster` reporting success without
    /// actually leaving a usable kubeconfig behind.
    fn without_kubeconfig_write(mut self) -> Self {
        self.kubeconfig_write = None;
        self
    }

    fn calls(&self) -> Vec<CommandSpec> {
        self.calls.lock().expect("calls mutex poisoned").clone()
    }
}

#[async_trait]
impl ProcessRunner for FakeProcessRunner {
    async fn run(&self, spec: CommandSpec) -> Result<CommandResult, ProcessError> {
        self.calls
            .lock()
            .expect("calls mutex poisoned")
            .push(spec.clone());

        let subcommand = spec
            .args
            .first()
            .map(|arg| arg.to_string_lossy().into_owned())
            .unwrap_or_default();

        match self.outcomes.get(subcommand.as_str()) {
            Some(FakeOutcome::Success(stdout)) => {
                if subcommand == "create"
                    && let Some(bytes) = self.kubeconfig_write
                {
                    let path = kubeconfig_arg(&spec.args);
                    tokio::fs::write(&path, bytes)
                        .await
                        .expect("test fake: write dummy kubeconfig");
                }
                Ok(CommandResult {
                    status: exit_status(0),
                    stdout: stdout.to_vec(),
                    stderr: Vec::new(),
                    elapsed: Duration::from_millis(1),
                    overflow: OutputOverflow::default(),
                })
            }
            Some(FakeOutcome::Failure(stderr)) => Ok(CommandResult {
                status: exit_status(1),
                stdout: Vec::new(),
                stderr: stderr.to_vec(),
                elapsed: Duration::from_millis(1),
                overflow: OutputOverflow::default(),
            }),
            Some(FakeOutcome::TimedOut) => Err(ProcessError::TimedOut {
                context: Box::new(spec.context()),
                timeout: spec.timeout,
                elapsed: spec.timeout,
                stdout: Vec::new(),
                stderr: Vec::new(),
                overflow: Box::new(OutputOverflow::default()),
            }),
            Some(FakeOutcome::Missing) | None => Err(ProcessError::Spawn {
                context: Box::new(spec.context()),
                source: io::Error::new(io::ErrorKind::NotFound, "No such file or directory"),
            }),
        }
    }
}

// ---------------------------------------------------------------------
// Step 1: exact argv for create/delete
// ---------------------------------------------------------------------

#[test]
fn create_invokes_kind_with_generated_config_and_explicit_kubeconfig_path() {
    let paths = block_on(new_run_paths("create-argv"));
    let runner = Arc::new(FakeProcessRunner::new().with("create", FakeOutcome::Success(b"")));
    let manager = KindClusterManager::new(runner.clone());
    let spec = baseline_spec("adlab-baseline-argvtest0001");

    let handle = block_on(manager.create(&spec, &paths)).expect("create should succeed");

    let expected_kind_config = paths.logs().join("baseline-kind-config.yaml");
    let expected_kubeconfig = paths.kubeconfigs().join("baseline.kubeconfig");
    assert_eq!(handle.kubeconfig, expected_kubeconfig);

    let calls = runner.calls();
    assert_eq!(
        calls.len(),
        1,
        "exactly one kind invocation for a clean create"
    );
    assert_eq!(calls[0].program, OsString::from("kind"));
    assert_eq!(
        calls[0].args,
        vec![
            OsString::from("create"),
            OsString::from("cluster"),
            OsString::from("--name"),
            OsString::from(spec.name.clone()),
            OsString::from("--config"),
            expected_kind_config.into_os_string(),
            OsString::from("--kubeconfig"),
            expected_kubeconfig.into_os_string(),
        ]
    );
    // Regression guard for the no-KUBECONFIG-leak invariant (PRODUCT.md
    // §29.2): isolation today rests entirely on `create` never layering
    // anything onto the child's inherited environment, so a future edit
    // that starts passing an env value through (for example a stray
    // `KUBECONFIG` override) is caught here rather than only by a
    // reviewer's grep of `lifecycle.rs`.
    assert!(calls[0].env.is_empty());
    assert!(calls[0].sensitive_env_keys.is_empty());
}

#[test]
fn delete_invokes_kind_with_exact_cluster_name_and_its_own_kubeconfig() {
    let runner = Arc::new(FakeProcessRunner::new().with("delete", FakeOutcome::Success(b"")));
    let manager = KindClusterManager::new(runner.clone());
    let handle = ClusterHandle {
        spec: baseline_spec("adlab-baseline-deleteargv01"),
        kubeconfig: PathBuf::from("/tmp/wherever.kubeconfig"),
        audit_log: PathBuf::from("/tmp/wherever-audit.log"),
    };

    block_on(manager.delete(&handle)).expect("delete should succeed");

    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].program, OsString::from("kind"));
    assert_eq!(
        calls[0].args,
        vec![
            OsString::from("delete"),
            OsString::from("cluster"),
            OsString::from("--name"),
            OsString::from("adlab-baseline-deleteargv01"),
            OsString::from("--kubeconfig"),
            OsString::from("/tmp/wherever.kubeconfig"),
        ],
        "delete must pass --kubeconfig with this cluster's own path -- never the user's ~/.kube/config -- so two concurrent deletes never race on ~/.kube/config.lock (see kind.rs's delete_argv documentation)"
    );
    // Same regression guard as the create-argv test above: `delete`
    // must not layer anything onto the child's inherited environment
    // either.
    assert!(calls[0].env.is_empty());
    assert!(calls[0].sensitive_env_keys.is_empty());
}

// ---------------------------------------------------------------------
// Step 3: cluster names
// ---------------------------------------------------------------------

#[test]
fn cluster_name_is_dns1123_valid_for_a_generated_run_id() {
    let run_id = RunId::generate();
    let name = cluster_name(Side::Baseline, &run_id)
        .expect("a generated run id must produce a valid name");
    assert!(name.starts_with("adlab-baseline-"));
    validate_cluster_name(&name).expect("the assembled name must itself validate");
}

#[test]
fn cluster_name_differs_between_sides_for_the_same_run_id() {
    let run_id = RunId::generate();
    let baseline = cluster_name(Side::Baseline, &run_id).expect("baseline name");
    let candidate = cluster_name(Side::Candidate, &run_id).expect("candidate name");
    assert_ne!(baseline, candidate);
}

#[test]
fn cluster_name_rejects_a_run_id_whose_short_prefix_ends_in_a_hyphen() {
    // Task 0.3's `validate_id` accepts a trailing hyphen even though
    // `RunId::generate()` never produces one -- a *parsed* run id (for
    // example via `reproduce`) could. This proves `cluster_name`
    // validates the assembled name rather than trusting concatenation:
    // without that check, this would silently produce the invalid
    // `"adlab-baseline-abcdefg-"`.
    let run_id = RunId::parse("abcdefg-").expect("validate_id currently accepts a trailing hyphen");
    let result = cluster_name(Side::Baseline, &run_id);
    assert!(
        matches!(result, Err(ClusterError::InvalidName { .. })),
        "expected InvalidName, got {result:?}"
    );
}

#[test]
fn validate_cluster_name_accepts_a_well_formed_name() {
    validate_cluster_name("adlab-baseline-abc123456789").expect("well-formed name must validate");
}

#[test]
fn validate_cluster_name_rejects_empty() {
    assert!(validate_cluster_name("").is_err());
}

#[test]
fn validate_cluster_name_rejects_uppercase() {
    assert!(validate_cluster_name("Adlab-baseline-abc").is_err());
}

#[test]
fn validate_cluster_name_rejects_leading_hyphen() {
    assert!(validate_cluster_name("-adlab-baseline-abc").is_err());
}

#[test]
fn validate_cluster_name_rejects_trailing_hyphen() {
    assert!(validate_cluster_name("adlab-baseline-abc-").is_err());
}

#[test]
fn validate_cluster_name_rejects_a_name_that_makes_the_control_plane_suffix_exceed_63_chars() {
    let name = format!("adlab-candidate-{}", "a".repeat(40));
    assert!(name.len() + "-control-plane".len() > 63);
    assert!(validate_cluster_name(&name).is_err());
}

#[test]
fn validate_cluster_name_accepts_a_name_right_at_the_control_plane_budget() {
    // 63 - "-control-plane".len() (14) == 49 characters exactly.
    let name = "a".repeat(49);
    assert_eq!(name.len() + "-control-plane".len(), 63);
    validate_cluster_name(&name).expect("a name exactly at the budget must be accepted");
}

// ---------------------------------------------------------------------
// Absolute host paths
// ---------------------------------------------------------------------

#[test]
fn create_rejects_a_non_absolute_run_paths_root_before_running_kind() {
    let relative_root = PathBuf::from("relative/artifact/root");
    let run_id = RunId::generate();
    let paths = RunPaths::new(&relative_root, &run_id);
    let runner = Arc::new(FakeProcessRunner::new());
    let manager = KindClusterManager::new(runner.clone());
    let spec = baseline_spec("adlab-baseline-relpathtest1");

    let error = block_on(manager.create(&spec, &paths))
        .expect_err("a relative RunPaths root must be rejected");

    assert!(
        matches!(error, ClusterError::NonAbsolutePath { .. }),
        "expected NonAbsolutePath, got {error:?}"
    );
    assert!(
        runner.calls().is_empty(),
        "kind must never be invoked when the run paths root is not absolute"
    );
}

// ---------------------------------------------------------------------
// Name validation happens before any kind invocation
// ---------------------------------------------------------------------

#[test]
fn create_rejects_invalid_name_before_running_kind() {
    let paths = block_on(new_run_paths("invalid-name"));
    let runner = Arc::new(FakeProcessRunner::new());
    let manager = KindClusterManager::new(runner.clone());
    let mut spec = baseline_spec("adlab-baseline-placeholder01");
    spec.name = "adlab-baseline-trailing-".to_owned();

    let error = block_on(manager.create(&spec, &paths)).expect_err("invalid name must be rejected");

    assert!(
        matches!(error, ClusterError::InvalidName { .. }),
        "expected InvalidName, got {error:?}"
    );
    assert!(
        runner.calls().is_empty(),
        "kind must never be invoked for an invalid cluster name"
    );
}

// ---------------------------------------------------------------------
// Step 4: rollback on partial create failure
// ---------------------------------------------------------------------

#[test]
fn create_rolls_back_when_kubeconfig_export_fails_after_kind_reports_success() {
    let paths = block_on(new_run_paths("rollback-deleted"));
    let runner = Arc::new(
        FakeProcessRunner::new()
            .with("create", FakeOutcome::Success(b""))
            .with("delete", FakeOutcome::Success(b""))
            .without_kubeconfig_write(),
    );
    let manager = KindClusterManager::new(runner.clone());
    let spec = baseline_spec("adlab-baseline-rollback0001");

    let error = block_on(manager.create(&spec, &paths))
        .expect_err("create must fail when kind never wrote a usable kubeconfig");

    match error {
        ClusterError::CreateFailedWithRollback { source, rollback } => {
            assert!(
                matches!(*source, ClusterError::Io { .. }),
                "expected the original cause to be the missing-kubeconfig read failure, got {source:?}"
            );
            assert!(
                matches!(rollback, RollbackOutcome::Deleted),
                "expected the rollback delete to have succeeded"
            );
        }
        other => panic!("expected CreateFailedWithRollback, got {other:?}"),
    }

    let calls = runner.calls();
    assert_eq!(
        calls.len(),
        2,
        "expected exactly one create attempt and one rollback delete"
    );
    assert_eq!(calls[0].args[0], OsString::from("create"));
    let expected_kubeconfig = paths.kubeconfigs().join("baseline.kubeconfig");
    assert_eq!(
        calls[1].args,
        vec![
            OsString::from("delete"),
            OsString::from("cluster"),
            OsString::from("--name"),
            OsString::from(spec.name.clone()),
            OsString::from("--kubeconfig"),
            expected_kubeconfig.into_os_string(),
        ],
        "rollback must delete by the exact same cluster name and this cluster's own          kubeconfig path -- never the user's ~/.kube/config"
    );
}

#[test]
fn create_rollback_preserves_original_error_even_when_delete_also_fails() {
    let paths = block_on(new_run_paths("rollback-failed"));
    let runner = Arc::new(
        FakeProcessRunner::new()
            .with("create", FakeOutcome::Success(b""))
            .with(
                "delete",
                FakeOutcome::Failure(b"docker: cannot connect to the daemon"),
            )
            .without_kubeconfig_write(),
    );
    let manager = KindClusterManager::new(runner.clone());
    let spec = baseline_spec("adlab-baseline-rollback0002");

    let error = block_on(manager.create(&spec, &paths))
        .expect_err("create must fail when kind never wrote a usable kubeconfig");

    let rendered = error.to_string();
    assert!(
        rendered.contains("also failed"),
        "the outer error must say cleanup also failed; got {rendered:?}"
    );

    match error {
        ClusterError::CreateFailedWithRollback { source, rollback } => {
            assert!(
                matches!(*source, ClusterError::Io { .. }),
                "the original failure must survive a failed rollback unchanged, got {source:?}"
            );
            assert!(
                matches!(rollback, RollbackOutcome::Failed(_)),
                "expected the rollback delete to have itself failed"
            );
        }
        other => panic!("expected CreateFailedWithRollback, got {other:?}"),
    }
}

#[test]
fn create_does_not_roll_back_when_the_kind_binary_itself_is_missing() {
    let paths = block_on(new_run_paths("no-rollback-on-spawn"));
    let runner = Arc::new(FakeProcessRunner::new().with("create", FakeOutcome::Missing));
    let manager = KindClusterManager::new(runner.clone());
    let spec = baseline_spec("adlab-baseline-nokindbinary1");

    let error = block_on(manager.create(&spec, &paths)).expect_err("create must fail");

    assert!(
        matches!(error, ClusterError::Process(ProcessError::Spawn { .. })),
        "expected a plain Spawn failure, not a rollback wrapper; got {error:?}"
    );
    assert_eq!(
        runner.calls().len(),
        1,
        "no rollback delete should have been attempted when kind never even started"
    );
}

#[test]
fn create_rolls_back_on_a_create_timeout_not_only_a_non_zero_exit() {
    let paths = block_on(new_run_paths("rollback-timeout"));
    let runner = Arc::new(
        FakeProcessRunner::new()
            .with("create", FakeOutcome::TimedOut)
            .with("delete", FakeOutcome::Success(b"")),
    );
    let manager = KindClusterManager::new(runner.clone());
    let spec = baseline_spec("adlab-baseline-rollbacktmout");

    let error = block_on(manager.create(&spec, &paths)).expect_err("create must fail on timeout");

    match error {
        ClusterError::CreateFailedWithRollback { source, rollback } => {
            assert!(
                matches!(
                    *source,
                    ClusterError::Process(ProcessError::TimedOut { .. })
                ),
                "got {source:?}"
            );
            assert!(matches!(rollback, RollbackOutcome::Deleted));
        }
        other => panic!("expected CreateFailedWithRollback, got {other:?}"),
    }
    assert_eq!(runner.calls().len(), 2);
}

#[test]
fn create_rolls_back_when_kind_create_exits_non_zero() {
    let paths = block_on(new_run_paths("rollback-nonzero"));
    let runner = Arc::new(
        FakeProcessRunner::new()
            .with(
                "create",
                FakeOutcome::Failure(b"ERROR: failed to create cluster"),
            )
            .with("delete", FakeOutcome::Success(b"")),
    );
    let manager = KindClusterManager::new(runner.clone());
    let spec = baseline_spec("adlab-baseline-rollbacknzero");

    let error = block_on(manager.create(&spec, &paths)).expect_err("create must fail");

    match error {
        ClusterError::CreateFailedWithRollback { source, rollback } => {
            assert!(
                matches!(*source, ClusterError::CommandFailed { .. }),
                "got {source:?}"
            );
            assert!(matches!(rollback, RollbackOutcome::Deleted));
        }
        other => panic!("expected CreateFailedWithRollback, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Baseline/candidate isolation sharing one RunPaths
// ---------------------------------------------------------------------

#[test]
fn baseline_and_candidate_use_independent_kubeconfig_and_audit_paths() {
    let paths = block_on(new_run_paths("isolation"));
    let runner = Arc::new(FakeProcessRunner::new().with("create", FakeOutcome::Success(b"")));
    let manager = KindClusterManager::new(runner.clone());

    let spec_baseline = baseline_spec("adlab-baseline-isolation001");
    let spec_candidate = ClusterSpec {
        side: Side::Candidate,
        name: "adlab-candidate-isolation001".to_owned(),
        ..spec_baseline.clone()
    };

    let handle_baseline =
        block_on(manager.create(&spec_baseline, &paths)).expect("baseline create should succeed");
    let handle_candidate =
        block_on(manager.create(&spec_candidate, &paths)).expect("candidate create should succeed");

    assert_ne!(handle_baseline.kubeconfig, handle_candidate.kubeconfig);
    assert_ne!(handle_baseline.audit_log, handle_candidate.audit_log);

    let calls = runner.calls();
    assert_eq!(calls.len(), 2);
    let baseline_kubeconfig_arg = kubeconfig_arg(&calls[0].args);
    let candidate_kubeconfig_arg = kubeconfig_arg(&calls[1].args);
    assert_ne!(baseline_kubeconfig_arg, candidate_kubeconfig_arg);
}

#[test]
fn concurrent_deletes_each_carry_their_own_kubeconfig_path() {
    // Regression test for a real defect this exact concurrency pattern
    // surfaced: without `--kubeconfig`, `kind delete cluster` falls back
    // to locking and rewriting the user's own `~/.kube/config`, so two
    // concurrent deletes -- exactly what
    // `admissionlab_core::run::LabRunner::cleanup` does for baseline and
    // candidate -- race on `~/.kube/config.lock`, and the loser reports
    // a spurious failure for a delete that actually succeeded (see
    // `kind.rs`'s `delete_argv` documentation for the reproduction).
    // Passing each delete its own cluster's isolated kubeconfig path
    // removes the shared file there is to race on in the first place;
    // this test proves both concurrent calls each carry their own,
    // distinct path.
    let runner = Arc::new(FakeProcessRunner::new().with("delete", FakeOutcome::Success(b"")));
    let manager = KindClusterManager::new(runner.clone());
    let baseline_spec = baseline_spec("adlab-baseline-concurrentdel1");
    let candidate_spec = ClusterSpec {
        side: Side::Candidate,
        name: "adlab-candidate-concurrentdel1".to_owned(),
        ..baseline_spec.clone()
    };
    let baseline = ClusterHandle {
        spec: baseline_spec,
        kubeconfig: PathBuf::from("/tmp/adlab-concurrent-del/baseline.kubeconfig"),
        audit_log: PathBuf::from("/tmp/adlab-concurrent-del/baseline-audit.log"),
    };
    let candidate = ClusterHandle {
        spec: candidate_spec,
        kubeconfig: PathBuf::from("/tmp/adlab-concurrent-del/candidate.kubeconfig"),
        audit_log: PathBuf::from("/tmp/adlab-concurrent-del/candidate-audit.log"),
    };

    let (baseline_result, candidate_result) =
        block_on(async { tokio::join!(manager.delete(&baseline), manager.delete(&candidate)) });

    baseline_result.expect("baseline delete should succeed");
    candidate_result.expect("candidate delete should succeed");

    let calls = runner.calls();
    assert_eq!(calls.len(), 2, "expected exactly one delete call per side");

    let kubeconfig_args: Vec<PathBuf> = calls
        .iter()
        .map(|call| kubeconfig_arg(&call.args))
        .collect();
    assert!(
        kubeconfig_args.contains(&baseline.kubeconfig),
        "expected baseline's delete to carry --kubeconfig {:?}, got {kubeconfig_args:?}",
        baseline.kubeconfig
    );
    assert!(
        kubeconfig_args.contains(&candidate.kubeconfig),
        "expected candidate's delete to carry --kubeconfig {:?}, got {kubeconfig_args:?}",
        candidate.kubeconfig
    );
    assert_ne!(
        kubeconfig_args[0], kubeconfig_args[1],
        "concurrent deletes must never share a kubeconfig path -- a shared path (or the user's own ~/.kube/config, if the flag were omitted entirely) is exactly the kind of shared file two concurrent `kind delete cluster` invocations would lock and race on for real"
    );
}

// ---------------------------------------------------------------------
// diagnostics never fails
// ---------------------------------------------------------------------

#[test]
fn diagnostics_reports_kubeconfig_and_audit_log_presence_and_cluster_existence() {
    let root = unique_root("diagnostics-present");
    block_on(tokio::fs::create_dir_all(&root)).expect("create scratch dir");
    let kubeconfig_path = root.join("baseline.kubeconfig");
    block_on(tokio::fs::write(
        &kubeconfig_path,
        b"apiVersion: v1\nkind: Config\n",
    ))
    .expect("write dummy kubeconfig");
    let audit_log_path = root.join("does-not-exist-audit.log");

    let handle = ClusterHandle {
        spec: baseline_spec("adlab-baseline-diagtest0001"),
        kubeconfig: kubeconfig_path,
        audit_log: audit_log_path,
    };
    let runner = Arc::new(FakeProcessRunner::new().with(
        "get",
        FakeOutcome::Success(b"adlab-baseline-diagtest0001\nadlab-candidate-diagtest0001\n"),
    ));
    let manager = KindClusterManager::new(runner);

    let diagnostics = block_on(manager.diagnostics(&handle));

    assert_eq!(diagnostics.cluster_name, "adlab-baseline-diagtest0001");
    assert_eq!(diagnostics.cluster_exists, Some(true));
    assert!(diagnostics.kubeconfig_present);
    assert!(!diagnostics.audit_log_present);
}

#[test]
fn diagnostics_reports_absence_when_kind_lists_a_different_cluster() {
    let handle = ClusterHandle {
        spec: baseline_spec("adlab-baseline-diagtest0003"),
        kubeconfig: PathBuf::from("/nonexistent/kubeconfig"),
        audit_log: PathBuf::from("/nonexistent/audit.log"),
    };
    let runner = Arc::new(
        FakeProcessRunner::new().with("get", FakeOutcome::Success(b"adlab-candidate-someother\n")),
    );
    let manager = KindClusterManager::new(runner);

    let diagnostics = block_on(manager.diagnostics(&handle));

    assert_eq!(diagnostics.cluster_exists, Some(false));
    assert!(!diagnostics.kubeconfig_present);
    assert!(!diagnostics.audit_log_present);
}

#[test]
fn diagnostics_never_fails_when_kind_cannot_be_reached() {
    let handle = ClusterHandle {
        spec: baseline_spec("adlab-baseline-diagtest0002"),
        kubeconfig: PathBuf::from("/nonexistent/kubeconfig"),
        audit_log: PathBuf::from("/nonexistent/audit.log"),
    };
    let runner = Arc::new(FakeProcessRunner::new()); // "get" unscripted -> Spawn error
    let manager = KindClusterManager::new(runner);

    let diagnostics = block_on(manager.diagnostics(&handle));

    assert_eq!(diagnostics.cluster_exists, None);
    assert!(
        !diagnostics.notes.is_empty(),
        "an undeterminable probe must leave an explanatory note, not silently succeed"
    );
    assert!(!diagnostics.kubeconfig_present);
    assert!(!diagnostics.audit_log_present);
}
