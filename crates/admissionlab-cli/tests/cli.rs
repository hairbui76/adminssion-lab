//! Black-box tests for the Admission Lab CLI's command surface: this
//! file spawns the actual compiled `admissionlab` binary via
//! `assert_cmd` and inspects its real stdout, stderr, and process exit
//! code, the way a user's shell would.
//!
//! It is the *outer* half of `admissionlab test`'s coverage.
//! `tests/test_command.rs` is the inner half: it drives
//! `admissionlab_cli::pipeline::run_lab` against fake
//! cluster/install/capture backends, which is the only way to reach the
//! outcomes that need two real API servers disagreeing (a policy
//! failure, a written `result.json` with findings in it). The two are
//! complementary — this file proves the *binary* wires arguments,
//! streams, and exit codes to that pipeline; that file proves the
//! pipeline's own decisions.
//!
//! Tests in the "genuine cluster lifecycle, through stubbed host tools"
//! section put stand-ins for `kind`/`kubectl`/`helm`/`docker` on a
//! controlled `PATH` — mirroring `tests/doctor.rs`'s own stubbing
//! approach for its `--deep` probe — so this file never depends on, or
//! executes, whatever real tools may or may not be installed on the
//! machine running this suite. `admissionlab-cluster`'s
//! `tests/kind_smoke.rs` (run with `-- --ignored`) is where the fully
//! real, unstubbed create/delete cycle is exercised, and Task 4.15's
//! `alpha_e2e` is where a whole real lab is.

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

/// Writes a minimal valid lab configuration requesting `kubernetes_version`
/// for both sides into `dir`, returning its path.
///
/// Mirrors `testdata/configs/minimal-valid.yaml`'s exact shape, but with
/// a caller-chosen version rather than that shared fixture's fixed
/// `"1.29.4"` — `admissionlab-spec`'s own test suite asserts that exact
/// literal, so it must not change, but a real (even if stubbed-`kind`)
/// `admissionlab test` run now genuinely resolves the configured version
/// against the real, compiled-in `compatibility/kubernetes.yaml`
/// (Controller Ruling R25), so tests that exercise that path need a
/// version actually present there.
fn write_lab_config(dir: &Path, kubernetes_version: &str) -> PathBuf {
    let path = dir.join("admissionlab.yaml");
    let contents = format!(
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: Lab\n\
         baseline:\n\
         \x20\x20kubernetes: \"{kubernetes_version}\"\n\
         candidate:\n\
         \x20\x20kubernetes: \"{kubernetes_version}\"\n\
         fixtures:\n\
         \x20\x20include:\n\
         \x20\x20\x20\x20- \"fixtures/**/*.yaml\"\n"
    );
    std::fs::write(&path, contents).expect("failed to write test lab configuration");
    path
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
// A run that fails before it touches any infrastructure must never look
// like a lab that reached a verdict.
// ---------------------------------------------------------------------

#[test]
fn a_failed_configuration_load_never_exits_success() {
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("test").arg("somefile.yaml").assert().failure();
}

#[test]
fn a_failed_configuration_load_emits_no_verdict_looking_output() {
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("test")
        .arg("somefile.yaml")
        .assert()
        .failure()
        // The terminal report — the only thing that renders a verdict —
        // is written by a run that got as far as comparing two sides.
        // A configuration that never loaded produces none of it.
        .stdout(predicates::str::contains("Result:").not())
        .stdout(predicates::str::contains("Summary").not())
        .stdout(predicates::str::contains("cluster").not())
        .stderr(predicates::str::contains("Result:").not())
        .stderr(predicates::str::contains("cluster").not());
}

// ---------------------------------------------------------------------
// Genuine cluster lifecycle, through stubbed host tools.
//
// The pipeline's prerequisite gate probes all four tools before it
// creates anything, so the stub `PATH` provides all four; `kind` is the
// only one whose behavior beyond `version` matters here, because these
// configurations declare no components for `helm`/`kubectl` to install.
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
///
/// `kubectl`, `helm`, and `docker` stand-ins are written alongside it,
/// answering only the version probes `admissionlab_core::probe_tool`
/// makes, so the pipeline's prerequisite gate passes on a `PATH` that
/// contains nothing real. Their exact output shapes are the ones
/// `admissionlab-core`'s own `tests/tool.rs` validated against the real
/// tools.
fn kind_stub_dir(label: &str) -> (PathBuf, PathBuf) {
    let dir = unique_stub_dir(label);
    let invocations_log = dir.join("kind-invocations.log");
    std::fs::write(&invocations_log, "").expect("create invocations log");

    write_raw_stub(
        &dir,
        "kubectl",
        "#!/bin/sh\nprintf '{\"clientVersion\":{\"gitVersion\":\"v1.36.4\"}}'\n",
    );
    write_raw_stub(&dir, "helm", "#!/bin/sh\nprintf 'v3.15.2'\n");
    write_raw_stub(&dir, "docker", "#!/bin/sh\nprintf '\"27.5.0\"\\n'\n");

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

/// Writes one minimal, valid fixture under `dir/fixtures/`, so a
/// configuration written by [`write_lab_config`] (whose include pattern
/// is `fixtures/**/*.yaml`) actually selects something.
///
/// A lab with no fixtures is refused outright as invalid input — a run
/// that replayed nothing must never exit 0 — so every test below that
/// needs the pipeline to get past discovery writes one of these.
fn write_fixture(dir: &Path) {
    let fixtures = dir.join("fixtures");
    std::fs::create_dir_all(&fixtures).expect("failed to create fixtures dir");
    std::fs::write(
        fixtures.join("pod.yaml"),
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: probe\nspec:\n  containers:\n\
         \x20\x20\x20\x20- name: app\n      image: registry.k8s.io/pause:3.10\n",
    )
    .expect("failed to write test fixture");
}

#[test]
fn a_capture_failure_still_writes_diagnostics_and_deletes_both_clusters() {
    // The stub `kind` writes a syntactically valid kubeconfig that names
    // no cluster at all, so both sides come up, both stacks install
    // (neither declares a component), and fixture capture then fails on
    // both sides the moment it tries to reach an API server. That is the
    // exact shape ROADMAP Task 4.14 steps 3 and 4 are about: a
    // later-stage failure must still write what it knows *and* still
    // clean up.
    let (dir, invocations_log) = kind_stub_dir("capture-failure");
    let config = write_lab_config(&dir, "1.36.4");
    write_fixture(&dir);
    let reports = dir.join("artifacts");

    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.env("PATH", &dir)
        .arg("test")
        .arg(&config)
        .arg("--report-dir")
        .arg(&reports)
        .assert()
        // 5 is `RunDisposition::FixtureFailed`'s discriminant.
        .code(5)
        .stdout(predicates::str::contains("created baseline cluster"))
        .stdout(predicates::str::contains("clusters deleted"))
        // No verdict was reached, so none is printed.
        .stdout(predicates::str::contains("Result:").not());

    assert!(
        reports.join("diagnostics.json").is_file(),
        "a later-stage failure must still leave its diagnostics behind"
    );
    assert!(
        !reports.join("result.json").exists(),
        "a run that never compared both sides must not write a result claiming a verdict"
    );

    let verbs = stub_kind_verbs(&invocations_log);
    assert_eq!(
        verbs.iter().filter(|verb| *verb == "create").count(),
        2,
        "expected one `kind create ...` per side, got:\n{verbs:?}"
    );
    assert_eq!(
        verbs.iter().filter(|verb| *verb == "delete").count(),
        2,
        "cleanup must still run after a capture failure, got:\n{verbs:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn keep_clusters_preserves_both_and_prints_exact_delete_commands() {
    let (dir, invocations_log) = kind_stub_dir("keep-clusters");
    let config = write_lab_config(&dir, "1.36.4");
    write_fixture(&dir);

    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.env("PATH", &dir)
        .arg("test")
        .arg(&config)
        .arg("--keep-clusters")
        .assert()
        .code(5)
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

#[test]
fn unsupported_kubernetes_version_exits_invalid_input_before_creating_a_cluster() {
    // The stub `kind` fails loudly on any verb but `create`/`delete`
    // (see `kind_stub_dir`'s own `*) exit 1` catch-all), so this test's
    // point stands: the failure is caught at version-resolution time,
    // not by a `kind create` that went wrong. `kind version` *is*
    // invoked first — the prerequisite gate probes it — which is why
    // this asserts on the verbs rather than on an empty log.
    let (dir, invocations_log) = kind_stub_dir("unsupported-version");
    let config = write_lab_config(&dir, "0.1.0");
    write_fixture(&dir);

    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.env("PATH", &dir)
        .arg("test")
        .arg(&config)
        .assert()
        // 2 is `RunDisposition::InvalidInput`'s discriminant (Controller
        // Ruling R25): an unresolvable Kubernetes version is the user's
        // configuration at fault, discovered before any cluster is
        // created -- not `InfrastructureFailed`.
        .code(2)
        .stderr(predicates::str::contains("failed to prepare lab clusters"))
        .stderr(predicates::str::contains("0.1.0"))
        .stdout(predicates::str::contains("created baseline cluster").not());

    let verbs = stub_kind_verbs(&invocations_log);
    assert!(
        verbs.iter().all(|verb| verb != "create"),
        "an unresolvable version must be caught before any cluster is created, got:\n{verbs:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_fixture_selection_that_matches_nothing_exits_invalid_input_before_any_cluster() {
    // `fixtures.include` is non-empty (that much `resolve_lab` already
    // enforces), but nothing on disk matches it. A run with nothing to
    // replay cannot produce a comparison, so it must be refused rather
    // than pass trivially.
    let (dir, invocations_log) = kind_stub_dir("no-fixtures");
    let config = write_lab_config(&dir, "1.36.4");

    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.env("PATH", &dir)
        .arg("test")
        .arg(&config)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("no fixtures matched"))
        .stderr(predicates::str::contains("fixtures/**/*.yaml"));

    let verbs = stub_kind_verbs(&invocations_log);
    assert!(
        verbs.iter().all(|verb| verb != "create"),
        "nothing to replay must be caught before any cluster is created, got:\n{verbs:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_host_tool_exits_invalid_input_and_points_at_doctor() {
    // A `PATH` with `kind` and nothing else. `admissionlab doctor`
    // answers this with exit 2 (Task 1.4), and `test` must agree — the
    // host does not satisfy what the tool documents it needs, which is
    // the user's to fix, not an infrastructure failure.
    let dir = unique_stub_dir("missing-tools");
    write_raw_stub(
        &dir,
        "kind",
        "#!/bin/sh\nprintf 'kind v0.33.0 go1.26.7 linux/amd64\\n'\n",
    );
    let config = write_lab_config(&dir, "1.36.4");
    write_fixture(&dir);

    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.env("PATH", &dir)
        .arg("test")
        .arg(&config)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("host prerequisites are not met"))
        .stderr(predicates::str::contains("admissionlab doctor"))
        .stdout(predicates::str::contains("created baseline cluster").not());

    let _ = std::fs::remove_dir_all(&dir);
}
