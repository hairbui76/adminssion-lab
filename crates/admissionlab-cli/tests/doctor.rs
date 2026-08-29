//! Black-box tests for `admissionlab doctor`'s command surface.
//!
//! Like `tests/cli.rs`, this spawns the real compiled `admissionlab`
//! binary via `assert_cmd` and inspects its real stdout, stderr, and
//! exit code. Unlike `tests/cli.rs`, `doctor` (Task 1.4) actually shells
//! out to `kind`/`kubectl`/`helm`/`docker` — so every test here that
//! reaches that probing logic first replaces the spawned process's
//! `PATH` with a directory of small, fake stand-in scripts this file
//! writes itself. No test in this file executes a real `kind`,
//! `kubectl`, `helm`, or `docker`; most of `doctor`'s actual probing and
//! parsing coverage lives in `admissionlab-core`'s `tests/tool.rs` (a
//! fake `ProcessRunner`, no subprocess at all) and in this crate's own
//! `commands::doctor` inline tests (a fake `ProcessRunner` calling
//! `run_with` directly). This file exists only to prove the wiring from
//! argument parsing down to the process exit code holds together
//! end to end through the real binary.
//!
//! Unix-only: the stand-in scripts below are `/bin/sh` scripts, and this
//! project's CI only runs on Linux (see `.github/workflows/ci.yml`).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use predicates::prelude::*;

// ---------------------------------------------------------------------
// Fake-`PATH` scaffolding.
// ---------------------------------------------------------------------

/// A fresh, guaranteed-unique scratch directory under the OS temp dir.
/// Mirrors `admissionlab-core`'s own `tests/process.rs`/`tests/domain.rs`
/// unique-temp-dir pattern rather than pulling in a new dependency just
/// for test-only temp directories.
fn unique_stub_dir(label: &str) -> PathBuf {
    let unique = admissionlab_core::RunId::generate();
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-cli-doctor-test-{label}-{}",
        unique.as_str()
    ));
    std::fs::create_dir_all(&dir).expect("failed to create stub PATH dir");
    dir
}

/// Writes an executable `/bin/sh` stand-in for one tool at `dir/name`,
/// so `doctor`'s real `ProcessRunner` finds and runs *this* — never a
/// real `kind`, `kubectl`, `helm`, or `docker` — when this directory is
/// the only entry on the spawned `admissionlab` process's `PATH`.
///
/// `stdout`/`stderr` are written with `printf '%s'`, never `echo` or a
/// heredoc, so no trailing newline is ever added beyond what the caller
/// includes explicitly — matching this crate's own verified real-tool
/// output shapes (for example, `helm`'s bare version has no trailing
/// newline).
fn write_stub(dir: &Path, name: &str, exit_code: i32, stdout: &str, stderr: &str) {
    let path = dir.join(name);
    let script = format!(
        "#!/bin/sh\nprintf '%s' '{stdout}'\nprintf '%s' '{stderr}' 1>&2\nexit {exit_code}\n"
    );
    std::fs::write(&path, script).expect("failed to write stub script");
    let mut permissions = std::fs::metadata(&path)
        .expect("failed to stat stub script")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("failed to make stub script executable");
}

/// Builds a stub `PATH` directory with all four tools present, returning
/// the real, verified output shapes from Task 1.4's brief (`kind`
/// v0.33.0, `kubectl` v1.32.1, `helm` v3.15.2, `docker` server 27.5.0).
fn all_tools_stub_dir(label: &str) -> PathBuf {
    let dir = unique_stub_dir(label);
    write_stub(&dir, "kind", 0, "kind v0.33.0 go1.26.7 linux/amd64\n", "");
    write_stub(
        &dir,
        "kubectl",
        0,
        r#"{"clientVersion":{"gitVersion":"v1.32.1"}}"#,
        "",
    );
    write_stub(&dir, "helm", 0, "v3.15.2", "");
    write_stub(&dir, "docker", 0, "\"27.5.0\"\n", "");
    dir
}

// ---------------------------------------------------------------------
// Shallow checks, end to end through the real binary.
// ---------------------------------------------------------------------

#[test]
fn all_tools_present_exits_success_and_prints_versions() {
    let dir = all_tools_stub_dir("all-present");

    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.env("PATH", &dir)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("kind"))
        .stdout(predicates::str::contains("v0.33.0"))
        .stdout(predicates::str::contains("kubectl"))
        .stdout(predicates::str::contains("v1.32.1"))
        .stdout(predicates::str::contains("helm"))
        .stdout(predicates::str::contains("v3.15.2"))
        .stdout(predicates::str::contains("docker"))
        .stdout(predicates::str::contains("27.5.0"))
        .stdout(predicates::str::contains("reachable"))
        // v1.32.1 is several minors behind this repository's real
        // compatibility/kubernetes.yaml matrix — the skew warning must
        // still fire (advisory only: the command still exits success).
        .stdout(predicates::str::contains("minor"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_tool_exits_invalid_input_and_names_it() {
    let dir = unique_stub_dir("missing-kind");
    // `kind` deliberately absent from this PATH.
    write_stub(
        &dir,
        "kubectl",
        0,
        r#"{"clientVersion":{"gitVersion":"v1.36.4"}}"#,
        "",
    );
    write_stub(&dir, "helm", 0, "v3.15.2", "");
    write_stub(&dir, "docker", 0, "\"27.5.0\"\n", "");

    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.env("PATH", &dir)
        .arg("doctor")
        .assert()
        .code(2)
        .stdout(predicates::str::contains("kind"))
        .stdout(predicates::str::contains("NOT FOUND"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn docker_daemon_down_exits_invalid_input_and_is_distinct_from_missing() {
    let dir = all_tools_stub_dir("docker-down");
    // Overwrite just the `docker` stub: present, but its server-version
    // probe fails, simulating an unreachable daemon.
    write_stub(
        &dir,
        "docker",
        1,
        "",
        "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. \
         Is the docker daemon running?",
    );

    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.env("PATH", &dir)
        .arg("doctor")
        .assert()
        .code(2)
        // Distinguishable from "missing": the docker entry is "found",
        // not "NOT FOUND", even though the daemon is unreachable.
        .stdout(predicates::str::contains("docker: found"))
        .stdout(predicates::str::contains("unreachable"))
        .stdout(predicates::str::contains("NOT FOUND").not());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn doctor_never_touches_kubeconfig_or_other_user_state() {
    let dir = all_tools_stub_dir("no-mutate");
    let kubeconfig = std::env::temp_dir().join(format!(
        "admissionlab-cli-doctor-test-kubeconfig-{}",
        admissionlab_core::RunId::generate().as_str()
    ));
    assert!(!kubeconfig.exists());

    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.env("PATH", &dir)
        .env("KUBECONFIG", &kubeconfig)
        .arg("doctor")
        .assert()
        .success();

    assert!(
        !kubeconfig.exists(),
        "doctor must never create or write a kubeconfig"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// `--deep`: hidden by default, honest where reachable at all.
// ---------------------------------------------------------------------

// These two are specifically about the default (`unstable-doctor-deep`
// off) build's surface, so they only run in that configuration — with
// the feature enabled (`cargo test --features unstable-doctor-deep`),
// `--deep` is *supposed* to parse and appear in `--help`, which is
// exactly what `deep_flag_is_honest_when_the_preview_feature_is_enabled`
// below checks instead.
#[cfg(not(feature = "unstable-doctor-deep"))]
#[test]
fn deep_flag_is_hidden_without_the_preview_feature() {
    // No stub PATH needed: without `unstable-doctor-deep`, `--deep` is
    // not on `DoctorArgs` at all, so Clap rejects it before any
    // subprocess would ever be spawned.
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("doctor")
        .arg("--deep")
        .assert()
        .failure()
        .stderr(predicates::str::contains("--deep"));
}

#[cfg(not(feature = "unstable-doctor-deep"))]
#[test]
fn help_does_not_advertise_deep_without_the_preview_feature() {
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("doctor")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("--deep").not());
}

#[cfg(feature = "unstable-doctor-deep")]
#[test]
fn deep_flag_is_honest_when_the_preview_feature_is_enabled() {
    let dir = all_tools_stub_dir("deep-preview");

    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.env("PATH", &dir)
        .arg("doctor")
        .arg("--deep")
        .assert()
        // Task 1.4 must never let `--deep` look like it succeeded, even
        // though every shallow check above still passes.
        .failure()
        .stdout(predicates::str::contains("kind")) // shallow checks still ran and printed
        .stderr(predicates::str::contains("not implemented"))
        .stdout(predicates::str::contains("cluster created").not())
        .stdout(predicates::str::contains("cluster deleted").not())
        .stderr(predicates::str::contains("success").not());

    let _ = std::fs::remove_dir_all(&dir);
}
