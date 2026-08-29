//! Black-box tests for the Admission Lab CLI's command surface.
//!
//! `admissionlab-cli` has no `[lib]` target, so this file — which spawns
//! the actual compiled `admissionlab` binary via `assert_cmd` and
//! inspects its real stdout, stderr, and process exit code, the way a
//! user's shell would — is the only place these behaviors can be
//! observed and pinned down.
//!
//! Several tests below exist specifically to enforce the honesty
//! constraint on `test`: at this phase of Admission Lab, `test` has no
//! pipeline behind it, and it must never look like it created clusters,
//! installed anything, or compared baseline/candidate behavior.

use predicates::prelude::*;

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
    // (`output::init_tracing`). Placing it before the subcommand exercises
    // `global = true`; seeing our own "not implemented" message (rather
    // than Clap's "unexpected argument" usage error) proves it parsed.
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("--verbose")
        .arg("doctor")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not implemented"));
}

#[test]
fn doctor_is_reachable_and_reports_not_implemented() {
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("doctor")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not implemented"))
        .stdout(predicates::str::contains("PASS").not())
        .stdout(predicates::str::contains("prerequisite").not());
}

#[test]
fn doctor_deep_flag_parses() {
    // If `--deep` were not a recognized flag, Clap would reject it with
    // its own usage error instead of ever reaching our command handler,
    // so seeing our message (not Clap's) proves the flag was accepted.
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("doctor")
        .arg("--deep")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not implemented"));
}

#[test]
fn doctor_command_exits_with_the_internal_error_code() {
    // Mirrors `test_command_exits_with_the_internal_error_code` below:
    // `doctor` and `test` both route through the same `not_implemented`
    // helper (`commands::not_implemented`), so both must be pinned to the
    // same exit code (6, `RunDisposition::InternalError`'s discriminant).
    // Without this, a future edit that gives `doctor` a different
    // disposition than `test` could pass unnoticed.
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("doctor").assert().code(6);
}

#[test]
fn test_is_reachable_and_reports_not_implemented() {
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("test")
        .arg("somefile.yaml")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not implemented"));
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
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("test")
        .arg("somefile.yaml")
        .arg("--keep-clusters")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not implemented"));
}

#[test]
fn test_config_path_need_not_exist() {
    // Config loading is a later task's job (not this one's); parsing must
    // not touch the filesystem, so a nonexistent path is accepted exactly
    // like a real one would be at this phase.
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("test")
        .arg("/definitely/does/not/exist/admissionlab.yaml")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not implemented"));
}

// ---------------------------------------------------------------------
// The honesty constraint: `test` must never exit 0, and must never
// print anything implying a lab actually ran.
// ---------------------------------------------------------------------

#[test]
fn test_command_does_not_exit_success() {
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("test").arg("somefile.yaml").assert().failure();
}

#[test]
fn test_command_exits_with_the_internal_error_code() {
    // 6 is `RunDisposition::InternalError`'s discriminant (see
    // `admissionlab_core::RunDisposition`). "Not implemented yet" is
    // classified as an internal Admission Lab limitation — not a policy,
    // infrastructure, installation, fixture, or user-input failure — so
    // it is the one disposition among the seven that fits.
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("test").arg("somefile.yaml").assert().code(6);
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
        // The refusal message itself must be exclusive to stderr: an
        // accidental `println!` alongside the `eprintln!` in
        // `commands::not_implemented` would duplicate it onto stdout
        // without any of the checks above catching it.
        .stdout(predicates::str::contains("not implemented").not())
        .stderr(predicates::str::contains("cluster").not())
        .stderr(predicates::str::contains("baseline").not())
        .stderr(predicates::str::contains("candidate").not());
}
