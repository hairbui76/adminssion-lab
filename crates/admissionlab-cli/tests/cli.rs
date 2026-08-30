//! Black-box tests for the Admission Lab CLI's command surface.
//!
//! `admissionlab-cli` has no `[lib]` target, so this file — which spawns
//! the actual compiled `admissionlab` binary via `assert_cmd` and
//! inspects its real stdout, stderr, and process exit code, the way a
//! user's shell would — is the only place these behaviors can be
//! observed and pinned down.
//!
//! Several tests below exist specifically to enforce the honesty
//! constraint on `test` (see `commands::test`'s own module
//! documentation): it must never look like a lab actually ran to a
//! pass/fail verdict, whether it fails before touching any
//! infrastructure (an invalid configuration) or after genuinely
//! creating and destroying both clusters.
//!
//! Tests in the "genuine cluster lifecycle, through a stubbed `kind`"
//! section stub only `kind` on a controlled `PATH` — mirroring
//! `tests/doctor.rs`'s own stubbing approach for its `--deep` probe —
//! so this file still never depends on, or executes, whatever real
//! `kind`/`docker` may or may not be installed on the machine running
//! this suite. `admissionlab-cluster`'s `tests/kind_smoke.rs` (run with
//! `-- --ignored`) is where the fully real, unstubbed create/delete
//! cycle is exercised.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use predicates::prelude::*;

/// The workspace's checked-in `testdata/configs/`, two levels above this
/// crate's own `CARGO_MANIFEST_DIR` — same convention
/// `admissionlab-spec`'s and `admissionlab-core`'s own test suites use.
fn testdata_config(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/configs")
        .join(name)
}

#[test]
fn help_lists_core_commands() {
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("doctor"))
        .stdout(predicates::str::contains("test"));
}

#[test]
fn version_prints_the_package_version() {
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn verbose_flag_is_accepted_globally() {
    // `-v`/`--verbose` is not one of this task's advertised subcommands,
    // but Decision 6 makes this the task that gives it a real source
    // (`output::init_tracing`). Placing it before the subcommand
    // exercises `global = true`; reaching our own "failed to load lab
    // configuration" message (rather than Clap's "unexpected argument"
    // usage error) proves it parsed.
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("--verbose")
        .arg("test")
        .arg("somefile.yaml")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "failed to load lab configuration",
        ));
}

#[test]
fn test_requires_config_argument() {
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("test")
        .assert()
        .failure()
        .stderr(predicates::str::contains("CONFIG"));
}

#[test]
fn test_keep_clusters_flag_parses() {
    // `--keep-clusters` must parse without a Clap usage error; reaching
    // our own "failed to load lab configuration" message (rather than a
    // Clap "unexpected argument" usage error) proves it did, even though
    // this particular config path never exists long enough to matter.
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("test")
        .arg("somefile.yaml")
        .arg("--keep-clusters")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "failed to load lab configuration",
        ));
}

// ---------------------------------------------------------------------
// Configuration loading (Task 1.10): distinct from an infrastructure
// failure, and exits before any cluster is ever touched.
// ---------------------------------------------------------------------

#[test]
fn missing_config_file_exits_invalid_input_and_names_the_path() {
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("test")
        .arg("/definitely/does/not/exist/admissionlab.yaml")
        .assert()
        // 2 is `RunDisposition::InvalidInput`'s discriminant: a missing
        // or unparsable configuration is the user's input, not the lab
        // infrastructure, failing (PRODUCT.md §27).
        .code(2)
        .stderr(predicates::str::contains(
            "failed to load lab configuration",
        ))
        .stderr(predicates::str::contains(
            "/definitely/does/not/exist/admissionlab.yaml",
        ));
}

#[test]
fn invalid_config_content_exits_invalid_input() {
    // `unknown-field.yaml` parses as YAML but fails `LabSpec`'s strict
    // `deny_unknown_fields` — a distinct failure moment from a missing
    // file (load_lab's parse stage, not its read stage), and it must
    // map to the same InvalidInput disposition.
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("test")
        .arg(testdata_config("unknown-field.yaml"))
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "failed to load lab configuration",
        ));
}

// ---------------------------------------------------------------------
// The honesty constraint: `test` must never exit 0, and must never
// print anything implying a lab actually ran to a verdict — even when
// it fails before touching any infrastructure.
// ---------------------------------------------------------------------

#[test]
fn test_command_does_not_exit_success() {
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("test").arg("somefile.yaml").assert().failure();
}

#[test]
fn test_command_does_not_emit_lab_success_looking_output() {
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("test")
        .arg("somefile.yaml")
        .assert()
        .failure()
        .stdout(predicates::str::contains("PASS").not())
        .stdout(predicates::str::contains("cluster").not())
        .stdout(predicates::str::contains("baseline").not())
        .stdout(predicates::str::contains("candidate").not())
        .stdout(predicates::str::contains("regression").not())
        .stderr(predicates::str::contains("cluster").not())
        .stderr(predicates::str::contains("baseline").not())
        .stderr(predicates::str::contains("candidate").not());
}

// ---------------------------------------------------------------------
// Genuine cluster lifecycle, through a stubbed `kind` (Task 1.10).
//
// `KindClusterManager::create`/`delete` invoke only `kind` — never
// `kubectl`/`helm`/`docker` — so this section's stub `PATH` needs only a
// `kind` stand-in.
// ---------------------------------------------------------------------

/// A fresh, guaranteed-unique scratch directory under the OS temp dir.
/// Mirrors `tests/doctor.rs`'s own `unique_stub_dir` pattern.
fn unique_stub_dir(label: &str) -> PathBuf {
    let unique = admissionlab_core::RunId::generate();
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-cli-test-command-test-{label}-{}",
        unique.as_str()
    ));
    std::fs::create_dir_all(&dir).expect("failed to create stub PATH dir");
    dir
}

/// Writes `script` verbatim as an executable stand-in at `dir/name`.
/// Mirrors `tests/doctor.rs`'s own `write_raw_stub`.
fn write_raw_stub(dir: &Path, name: &str, script: &str) {
    let path = dir.join(name);
    std::fs::write(&path, script).expect("failed to write stub script");
    let mut permissions = std::fs::metadata(&path)
        .expect("failed to stat stub script")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("failed to make stub script executable");
}

/// Builds a stub `PATH` directory whose `kind` stand-in is sophisticated
/// enough to carry `admissionlab test` through a full two-cluster
/// create/delete cycle: `create` writes a dummy kubeconfig to its own
/// `--kubeconfig` argument (as real `kind` does) and `delete` succeeds.
/// Every invocation is appended to the returned invocations-log path
/// (one line, the full argv, per call) so a test can assert exactly
/// which clusters were created and/or deleted.
fn kind_stub_dir(label: &str) -> (PathBuf, PathBuf) {
    let dir = unique_stub_dir(label);
    let invocations_log = dir.join("kind-invocations.log");
    std::fs::write(&invocations_log, "").expect("create invocations log");

    let kind_script = format!(
        "#!/bin/sh\n\
         echo \"$@\" >> \"{log}\"\n\
         case \"$1\" in\n\
         \x20\x20create)\n\
         \x20\x20\x20\x20prev=\"\"\n\
         \x20\x20\x20\x20for arg in \"$@\"; do\n\
         \x20\x20\x20\x20\x20\x20if [ \"$prev\" = \"--kubeconfig\" ]; then\n\
         \x20\x20\x20\x20\x20\x20\x20\x20printf 'apiVersion: v1\\nkind: Config\\nclusters: []\\n' > \"$arg\"\n\
         \x20\x20\x20\x20\x20\x20fi\n\
         \x20\x20\x20\x20\x20\x20prev=\"$arg\"\n\
         \x20\x20\x20\x20done\n\
         \x20\x20\x20\x20exit 0\n\
         \x20\x20\x20\x20;;\n\
         \x20\x20delete)\n\
         \x20\x20\x20\x20exit 0\n\
         \x20\x20\x20\x20;;\n\
         \x20\x20*)\n\
         \x20\x20\x20\x20exit 1\n\
         \x20\x20\x20\x20;;\n\
         esac\n",
        log = invocations_log.display(),
    );
    write_raw_stub(&dir, "kind", &kind_script);

    (dir, invocations_log)
}

/// The `create`/`delete` lines the stub `kind` recorded, in call order.
fn stub_kind_verbs(invocations_log: &Path) -> Vec<String> {
    std::fs::read_to_string(invocations_log)
        .expect("read stub kind invocations log")
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

#[test]
fn deletes_both_clusters_and_honestly_reports_the_pipeline_gap() {
    let (dir, invocations_log) = kind_stub_dir("delete-both");

    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.env("PATH", &dir)
        .arg("test")
        .arg(testdata_config("minimal-valid.yaml"))
        .assert()
        // 6 is `RunDisposition::InternalError`'s discriminant: both
        // clusters genuinely came up and were torn down, but fixture
        // execution/comparison — required for any real verdict — are
        // not implemented yet, so this can never be 0.
        .code(6)
        .stdout(predicates::str::contains("created baseline cluster"))
        .stdout(predicates::str::contains("candidate cluster"))
        .stdout(predicates::str::contains("clusters deleted"))
        // The honesty constraint holds even on the genuine-cluster path:
        // nothing here may look like a completed, let alone passing, lab.
        .stdout(predicates::str::contains("PASS").not())
        .stdout(predicates::str::contains("regression").not())
        .stderr(predicates::str::contains("not implemented"))
        .stderr(predicates::str::contains("not a pass or a fail"));

    let verbs = stub_kind_verbs(&invocations_log);
    assert_eq!(
        verbs.iter().filter(|verb| *verb == "create").count(),
        2,
        "expected one `kind create ...` per side, got:\n{verbs:?}"
    );
    assert_eq!(
        verbs.iter().filter(|verb| *verb == "delete").count(),
        2,
        "expected one `kind delete ...` per side (cleanup), got:\n{verbs:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn keep_clusters_preserves_both_and_prints_exact_delete_commands() {
    let (dir, invocations_log) = kind_stub_dir("keep-clusters");

    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.env("PATH", &dir)
        .arg("test")
        .arg(testdata_config("minimal-valid.yaml"))
        .arg("--keep-clusters")
        .assert()
        .code(6)
        .stdout(predicates::str::contains("preserved"))
        .stdout(predicates::str::contains(
            "kind delete cluster --name adlab-baseline-",
        ))
        .stdout(predicates::str::contains(
            "kind delete cluster --name adlab-candidate-",
        ))
        .stdout(predicates::str::contains("kubeconfig:"));

    let verbs = stub_kind_verbs(&invocations_log);
    assert_eq!(
        verbs.iter().filter(|verb| *verb == "create").count(),
        2,
        "expected one `kind create ...` per side, got:\n{verbs:?}"
    );
    assert!(
        verbs.iter().all(|verb| verb != "delete"),
        "--keep-clusters must never call `kind delete`, got:\n{verbs:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
