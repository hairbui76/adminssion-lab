//! Behavioral tests for the safe external process runner: [`CommandSpec`],
//! [`CommandResult`], [`CommandContext`], [`ProcessError`], and the
//! [`ProcessRunner`] trait's `tokio`-backed implementation,
//! [`TokioProcessRunner`].
//!
//! Every external command Admission Lab ever runs (`kind`, `kubectl`,
//! `helm`, and the metrics scrape in later tasks) goes through this one
//! chokepoint, so this file is ultimately in service of three properties:
//!
//! 1. Argv reaches the child byte-for-byte, with no shell ever in the
//!    loop (Global Constraint 12 / PRODUCT.md §29.4).
//! 2. A command that exceeds its timeout is actually killed and reaped —
//!    never abandoned as an orphan holding cluster state.
//! 3. A credential-like environment value — whether recognized by name or
//!    explicitly marked sensitive by the caller — reaches the child
//!    normally but can never render as anything but `[REDACTED]` in a
//!    diagnostic (PRODUCT.md §29.3 / Global Constraint 14).
//!
//! # Why this file has no test harness
//!
//! These tests spawn real child processes and must do so without
//! depending on any external binary (`/bin/sh`, `echo`, `sleep`, `cat`)
//! so they work the same way in any environment. The standard trick is
//! for the test binary to re-invoke *itself* as the child, behaving as a
//! small configurable helper when a sentinel environment variable
//! ([`HELPER_MODE_VAR`]) is set instead of running the test suite.
//!
//! Doing that from *inside* a `#[test]` function does not work: Rust's
//! default libtest harness generates its own `fn main` that parses argv
//! as test-name filters and prints its own "running N tests" banner to
//! the real stdout *before* any `#[test]` body runs — which would
//! corrupt the very byte-for-byte stdout captures these tests rely on.
//! So this target opts out of the default harness (`harness = false` in
//! `Cargo.toml`) and provides its own `fn main`, which checks
//! [`HELPER_MODE_VAR`] first, before anything else, and only runs the
//! test table below if it is unset.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{self, Write as _};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
// `Path` is used only by the Linux-only `proc_dir_exists` below (see its
// `#[cfg(target_os = "linux")]`); gating the import to match keeps it from
// becoming an unused-import hard error under `-D warnings` on any other
// target, which is exactly the class of bug this import previously caused
// on macOS (see the fix report).
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use admissionlab_core::{
    CommandContext, CommandResult, CommandSpec, ProcessError, ProcessRunner, RedactedValue,
    TokioProcessRunner,
};

// =====================================================================
// Entry point: helper dispatch, or the hand-rolled test driver.
// =====================================================================

/// Sentinel environment variable. Absent: this binary runs the test
/// table below. Present: its value selects a helper behavior (see
/// [`run_helper`]) and the test table never runs.
const HELPER_MODE_VAR: &str = "ADMISSIONLAB_PROCESS_TEST_HELPER";

fn main() -> ExitCode {
    if let Ok(mode) = std::env::var(HELPER_MODE_VAR) {
        return run_helper(&mode);
    }
    run_tests()
}

// =====================================================================
// Helper mode: this binary, re-invoked as the child under test.
// =====================================================================

/// Env var (helper mode `sleep-then-exit`): milliseconds to sleep before
/// writing final output and exiting.
const HELPER_SLEEP_MS_VAR: &str = "ADMISSIONLAB_HELPER_SLEEP_MS";
/// Env var (helper mode `sleep-then-exit`): the exit code (0-255) to
/// exit with once done sleeping.
const HELPER_EXIT_CODE_VAR: &str = "ADMISSIONLAB_HELPER_EXIT_CODE";
/// Env var (helper mode `big-output`): number of bytes to write to
/// stdout.
const HELPER_STDOUT_LEN_VAR: &str = "ADMISSIONLAB_HELPER_STDOUT_LEN";
/// Env var (helper mode `big-output`): number of bytes to write to
/// stderr.
const HELPER_STDERR_LEN_VAR: &str = "ADMISSIONLAB_HELPER_STDERR_LEN";
/// Env var (helper mode `sleep-then-exit`): if set, a file path this
/// process writes to *after* its sleep completes (never on being
/// killed, since a kill ends the process before it gets there). Lets a
/// test prove a kill actually prevented the child from finishing its
/// work, portably (no `/proc`, no platform-specific liveness check).
const HELPER_DONE_FILE_VAR: &str = "ADMISSIONLAB_HELPER_DONE_FILE";
/// Env var (helper mode `print-env-var`): the name of the environment
/// variable this process should look up and report.
const HELPER_ENV_VAR_NAME_VAR: &str = "ADMISSIONLAB_HELPER_ENV_VAR_NAME";
/// Sentinel stdout value `print-env-var` writes when the variable it was
/// asked to look up is not set at all.
const ENV_VAR_MISSING_MARKER: &str = "<missing>";

/// Dispatches to one helper behavior. Each mode is a self-contained,
/// synchronous (no tokio) child-process behavior that a test spawns via
/// [`TokioProcessRunner`] to exercise one property of the runner.
fn run_helper(mode: &str) -> ExitCode {
    match mode {
        "echo-argv" => helper_echo_argv(),
        "print-cwd" => helper_print_cwd(),
        "print-env-var" => helper_print_env_var(),
        "sleep-then-exit" => helper_sleep_then_exit(),
        "big-output" => helper_big_output(),
        other => panic!("test bug: unknown helper mode {other:?}"),
    }
}

fn helper_env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Writes every argument this process received (excluding argv[0], the
/// program path) to stdout as raw bytes separated by NUL. NUL is used as
/// the wire separator because it is the one byte value that can never
/// legally appear inside a Unix argv element (argv strings are
/// NUL-terminated at the OS level), so the encoding is unambiguous even
/// for arguments containing embedded newlines, empty strings, or
/// non-UTF-8 bytes.
fn helper_echo_argv() -> ExitCode {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for arg in std::env::args_os().skip(1) {
        out.write_all(arg.as_bytes()).expect("write arg bytes");
        out.write_all(b"\0").expect("write NUL separator");
    }
    out.flush().expect("flush stdout");
    ExitCode::SUCCESS
}

/// Writes this process's current working directory to stdout verbatim,
/// proving whether `CommandSpec::cwd` was honored.
fn helper_print_cwd() -> ExitCode {
    let cwd = std::env::current_dir().expect("current_dir");
    let stdout = io::stdout();
    let mut out = stdout.lock();
    out.write_all(cwd.as_os_str().as_bytes())
        .expect("write cwd");
    out.flush().expect("flush stdout");
    ExitCode::SUCCESS
}

/// Immediately reports this process's own PID on stdout (so a test can
/// prove the exact OS process it spawned is gone after a timeout), then
/// sleeps, then exits with a configurable code. Used both for the
/// timeout-kills-the-child test (long sleep) and the
/// non-zero-exit-is-not-a-runner-error test (short/no sleep).
///
/// If [`HELPER_DONE_FILE_VAR`] is set, a file is written at that path
/// *after* the sleep completes — a portable (no `/proc`, no `unsafe`)
/// way for a test to prove a kill actually landed before the child could
/// finish: if the file never appears within a grace period comfortably
/// longer than the configured sleep, the child cannot have run to
/// completion.
fn helper_sleep_then_exit() -> ExitCode {
    {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        writeln!(out, "PID={}", std::process::id()).expect("write pid line");
        out.flush().expect("flush pid line");
    }

    let sleep_ms: u64 = helper_env_or(HELPER_SLEEP_MS_VAR, "0")
        .parse()
        .expect("valid sleep ms");
    std::thread::sleep(Duration::from_millis(sleep_ms));

    if let Ok(done_file) = std::env::var(HELPER_DONE_FILE_VAR) {
        std::fs::write(&done_file, b"done").expect("write done-file sentinel");
    }

    let exit_code: u8 = helper_env_or(HELPER_EXIT_CODE_VAR, "0")
        .parse()
        .expect("valid exit code");

    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "done sleeping").expect("write done line");
    out.flush().expect("flush done line");

    ExitCode::from(exit_code)
}

/// Looks up the environment variable named by `HELPER_ENV_VAR_NAME_VAR`
/// in *this* process's own environment and writes its value to stdout,
/// or [`ENV_VAR_MISSING_MARKER`] if it is unset. Used to prove that
/// `CommandSpec::env` is layered onto this process's inherited
/// environment rather than replacing it: a variable a test sets only on
/// the parent (never mentioned in `CommandSpec::env`) must still be
/// visible here, and a variable present in both must resolve to the
/// `CommandSpec::env` value.
fn helper_print_env_var() -> ExitCode {
    let name = std::env::var(HELPER_ENV_VAR_NAME_VAR).expect("helper env var name must be set");
    let value = std::env::var(&name).unwrap_or_else(|_| ENV_VAR_MISSING_MARKER.to_string());
    let stdout = io::stdout();
    let mut out = stdout.lock();
    out.write_all(value.as_bytes()).expect("write value");
    out.flush().expect("flush stdout");
    ExitCode::SUCCESS
}

/// Writes `HELPER_STDOUT_LEN_VAR` bytes to stdout (cycling `a`-`z`) and
/// `HELPER_STDERR_LEN_VAR` bytes to stderr (cycling `A`-`Z`),
/// interleaved in small chunks so both pipes are filling up at
/// (approximately) the same time rather than one stream completing
/// before the other starts. Both alphabets are disjoint, so a test can
/// tell at a glance whether any byte crossed streams.
fn helper_big_output() -> ExitCode {
    const CHUNK: usize = 8 * 1024;

    let stdout_len: usize = helper_env_or(HELPER_STDOUT_LEN_VAR, "32")
        .parse()
        .expect("valid stdout len");
    let stderr_len: usize = helper_env_or(HELPER_STDERR_LEN_VAR, "32")
        .parse()
        .expect("valid stderr len");

    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();

    let mut lower = (b'a'..=b'z').cycle();
    let mut upper = (b'A'..=b'Z').cycle();
    let mut stdout_written = 0usize;
    let mut stderr_written = 0usize;

    while stdout_written < stdout_len || stderr_written < stderr_len {
        if stdout_written < stdout_len {
            let n = CHUNK.min(stdout_len - stdout_written);
            let chunk: Vec<u8> = (&mut lower).take(n).collect();
            out.write_all(&chunk).expect("write stdout chunk");
            stdout_written += n;
        }
        if stderr_written < stderr_len {
            let n = CHUNK.min(stderr_len - stderr_written);
            let chunk: Vec<u8> = (&mut upper).take(n).collect();
            err.write_all(&chunk).expect("write stderr chunk");
            stderr_written += n;
        }
    }
    out.flush().expect("flush stdout");
    err.flush().expect("flush stderr");
    ExitCode::SUCCESS
}

// =====================================================================
// Hand-rolled test driver (this target opts out of the default harness;
// see the module doc for why).
// =====================================================================

type TestFn = fn(&tokio::runtime::Runtime);

const TESTS: &[(&str, TestFn)] = &[
    (
        "argv_round_trips_exactly_including_shell_metacharacters",
        argv_round_trips_exactly_including_shell_metacharacters,
    ),
    (
        "argv_round_trips_non_utf8_bytes",
        argv_round_trips_non_utf8_bytes,
    ),
    ("cwd_is_honored", cwd_is_honored),
    (
        "stdout_and_stderr_are_captured_separately_and_not_interleaved",
        stdout_and_stderr_are_captured_separately_and_not_interleaved,
    ),
    (
        "large_concurrent_output_on_both_streams_does_not_deadlock",
        large_concurrent_output_on_both_streams_does_not_deadlock,
    ),
    (
        "nonzero_exit_status_is_a_successful_result_not_a_runner_error",
        nonzero_exit_status_is_a_successful_result_not_a_runner_error,
    ),
    (
        "timeout_kills_and_reaps_the_child_process",
        timeout_kills_and_reaps_the_child_process,
    ),
    (
        "timeout_error_carries_the_partial_output_captured_so_far",
        timeout_error_carries_the_partial_output_captured_so_far,
    ),
    (
        "env_is_layered_onto_the_inherited_environment",
        env_is_layered_onto_the_inherited_environment,
    ),
    (
        "command_context_redacts_credential_like_env_keys",
        command_context_redacts_credential_like_env_keys,
    ),
    (
        "caller_marked_sensitive_env_value_reaches_child_but_is_redacted_in_diagnostics",
        caller_marked_sensitive_env_value_reaches_child_but_is_redacted_in_diagnostics,
    ),
    (
        "command_context_preserves_non_sensitive_env_values",
        command_context_preserves_non_sensitive_env_values,
    ),
    (
        "command_spec_debug_output_never_contains_a_redacted_raw_value",
        command_spec_debug_output_never_contains_a_redacted_raw_value,
    ),
    (
        "process_error_display_never_contains_a_redacted_raw_value",
        process_error_display_never_contains_a_redacted_raw_value,
    ),
];

fn run_tests() -> ExitCode {
    // Default panics print straight to this process's own stderr; that's
    // unhelpful noise here because every failure is already captured and
    // reported explicitly below.
    std::panic::set_hook(Box::new(|_info| {}));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build test tokio runtime");

    let mut failures: Vec<(&str, String)> = Vec::new();
    for &(name, test_fn) in TESTS {
        print!("test {name} ... ");
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| test_fn(&rt))) {
            Ok(()) => println!("ok"),
            Err(payload) => {
                println!("FAILED");
                failures.push((name, panic_message(&*payload)));
            }
        }
    }

    println!();
    if failures.is_empty() {
        println!("test result: ok. {} passed; 0 failed.", TESTS.len());
        ExitCode::SUCCESS
    } else {
        for (name, message) in &failures {
            println!("---- {name} ----\n{message}\n");
        }
        println!(
            "test result: FAILED. {} passed; {} failed.",
            TESTS.len() - failures.len(),
            failures.len()
        );
        ExitCode::FAILURE
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "test panicked with a non-string payload".to_string()
    }
}

// =====================================================================
// Test support: building a CommandSpec that re-invokes this binary.
// =====================================================================

/// Builds a [`CommandSpec`] that re-invokes this same test binary in
/// helper `mode`, with `timeout`. Callers customize `args`/`cwd`/`env`
/// (beyond the sentinel) afterward.
fn helper_spec(mode: &str, timeout: Duration) -> CommandSpec {
    let exe = std::env::current_exe().expect("current_exe");
    let mut env = BTreeMap::new();
    env.insert(OsString::from(HELPER_MODE_VAR), OsString::from(mode));
    CommandSpec {
        program: exe.into_os_string(),
        args: Vec::new(),
        cwd: None,
        env,
        sensitive_env_keys: BTreeSet::new(),
        timeout,
    }
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

fn run_spec(
    rt: &tokio::runtime::Runtime,
    spec: CommandSpec,
) -> Result<CommandResult, ProcessError> {
    let runner = TokioProcessRunner::new();
    rt.block_on(runner.run(spec))
}

/// A fresh, guaranteed-unique scratch directory under the OS temp dir,
/// created for one test. Mirrors the unique-temp-dir pattern already
/// established in `tests/domain.rs`'s `RunPaths` tests, rather than
/// pulling in a new dependency just for test-only temp directories.
fn unique_scratch_dir(label: &str) -> PathBuf {
    let unique = admissionlab_core::RunId::generate();
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-core-process-test-{label}-{}",
        unique.as_str()
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir.canonicalize().expect("canonicalize scratch dir")
}

// =====================================================================
// 1. Argv preservation — proves no shell is ever involved.
// =====================================================================

fn argv_round_trips_exactly_including_shell_metacharacters(rt: &tokio::runtime::Runtime) {
    let tricky: Vec<OsString> = [
        "plain",
        "has space",
        "has\ttab",
        "single'quote",
        "double\"quote",
        "back`tick",
        "dollar$var",
        "semi;colon",
        "pipe|char",
        "amp&ersand",
        "redirect>out",
        "redirect<in",
        "glob*star",
        "paren(then)",
        "brace{then}",
        "bracket[then]",
        "newline\nembedded",
        "",
        "trailing-backslash\\",
        "unicode-\u{1F680}-emoji",
        "$(rm -rf /)",
        "`rm -rf /`",
    ]
    .iter()
    .map(OsString::from)
    .collect();

    let mut spec = helper_spec("echo-argv", DEFAULT_TIMEOUT);
    spec.args.clone_from(&tricky);

    let result = run_spec(rt, spec).expect("echo-argv must succeed");
    assert!(result.status.success(), "helper exited non-zero");

    let received = split_nul_terminated(&result.stdout);
    let expected: Vec<Vec<u8>> = tricky.iter().map(|a| a.as_bytes().to_vec()).collect();
    assert_eq!(
        received, expected,
        "argv did not round-trip exactly; a shell would have mangled these metacharacters"
    );
}

fn argv_round_trips_non_utf8_bytes(rt: &tokio::runtime::Runtime) {
    // A lone continuation byte (0x80) and a lone leading byte of a
    // 2-byte sequence (0xFF) are both invalid UTF-8 on their own, so
    // this argument cannot round-trip through anything that assumes
    // UTF-8 (such as `to_string_lossy`, which would replace these with
    // U+FFFD). `Vec<u8>`/`OsString` fidelity is exactly what this task's
    // interfaces exist to guarantee.
    let non_utf8 = OsString::from_vec(vec![b'a', 0x80, b'b', 0xFF, b'c']);
    let mut spec = helper_spec("echo-argv", DEFAULT_TIMEOUT);
    spec.args = vec![non_utf8.clone()];

    let result = run_spec(rt, spec).expect("echo-argv must succeed");
    let received = split_nul_terminated(&result.stdout);
    assert_eq!(received, vec![non_utf8.as_bytes().to_vec()]);
}

/// Splits a NUL-terminated (not NUL-separated) byte stream into its
/// elements, dropping the final empty element produced by the trailing
/// terminator. Mirrors the wire format `helper_echo_argv` writes.
fn split_nul_terminated(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut parts: Vec<Vec<u8>> = bytes.split(|&b| b == 0).map(<[u8]>::to_vec).collect();
    if parts.last().is_some_and(Vec::is_empty) {
        parts.pop();
    }
    parts
}

// =====================================================================
// 2. cwd is honored.
// =====================================================================

fn cwd_is_honored(rt: &tokio::runtime::Runtime) {
    let dir = unique_scratch_dir("cwd");

    let mut spec = helper_spec("print-cwd", DEFAULT_TIMEOUT);
    spec.cwd = Some(dir.clone());

    let result = run_spec(rt, spec).expect("print-cwd must succeed");
    assert!(result.status.success());
    assert_eq!(result.stdout, dir.as_os_str().as_bytes());

    let _ = std::fs::remove_dir_all(&dir);
}

// =====================================================================
// 3. stdout/stderr captured separately, never interleaved.
// =====================================================================

fn stdout_and_stderr_are_captured_separately_and_not_interleaved(rt: &tokio::runtime::Runtime) {
    let mut spec = helper_spec("big-output", DEFAULT_TIMEOUT);
    spec.env
        .insert(OsString::from(HELPER_STDOUT_LEN_VAR), OsString::from("40"));
    spec.env
        .insert(OsString::from(HELPER_STDERR_LEN_VAR), OsString::from("40"));

    let result = run_spec(rt, spec).expect("big-output must succeed");
    assert!(result.status.success());

    assert_eq!(result.stdout, cycling_pattern(b'a'..=b'z', 40));
    assert_eq!(result.stderr, cycling_pattern(b'A'..=b'Z', 40));
    assert!(
        result.stdout.iter().all(u8::is_ascii_lowercase),
        "stdout contains a byte that could only have come from stderr's alphabet: {:?}",
        result.stdout
    );
    assert!(
        result.stderr.iter().all(u8::is_ascii_uppercase),
        "stderr contains a byte that could only have come from stdout's alphabet: {:?}",
        result.stderr
    );
}

fn cycling_pattern(alphabet: std::ops::RangeInclusive<u8>, len: usize) -> Vec<u8> {
    alphabet.cycle().take(len).collect()
}

// =====================================================================
// 4. Large concurrent output on both streams must not deadlock
//    (hazard 2: draining after `wait()` blocks once either stream
//    exceeds the OS pipe buffer, ~64 KiB).
// =====================================================================

fn large_concurrent_output_on_both_streams_does_not_deadlock(rt: &tokio::runtime::Runtime) {
    const LEN: usize = 200_000; // comfortably over the ~64 KiB pipe buffer
    let mut spec = helper_spec("big-output", Duration::from_secs(30));
    spec.env.insert(
        OsString::from(HELPER_STDOUT_LEN_VAR),
        OsString::from(LEN.to_string()),
    );
    spec.env.insert(
        OsString::from(HELPER_STDERR_LEN_VAR),
        OsString::from(LEN.to_string()),
    );

    let result = run_spec(rt, spec).expect("big-output must succeed without deadlocking");
    assert!(result.status.success());
    assert_eq!(result.stdout.len(), LEN);
    assert_eq!(result.stderr.len(), LEN);
    assert_eq!(result.stdout, cycling_pattern(b'a'..=b'z', LEN));
    assert_eq!(result.stderr, cycling_pattern(b'A'..=b'Z', LEN));
}

// =====================================================================
// 5. A non-zero exit status is a successful `CommandResult`, not a
//    `ProcessError` — the runner does not editorialize about exit
//    codes.
// =====================================================================

fn nonzero_exit_status_is_a_successful_result_not_a_runner_error(rt: &tokio::runtime::Runtime) {
    let mut spec = helper_spec("sleep-then-exit", DEFAULT_TIMEOUT);
    spec.env
        .insert(OsString::from(HELPER_EXIT_CODE_VAR), OsString::from("7"));

    let result = run_spec(rt, spec).expect("a non-zero exit must still be Ok(..)");
    assert!(!result.status.success());
    assert_eq!(result.status.code(), Some(7));
}

// =====================================================================
// 6. Timeout kills and reaps the child (hazard 1).
//
// This project commits to macOS release binaries and macOS CI runners
// (see the roadmap), which have no `/proc`. The primary proof below
// must therefore be portable; the `/proc`-based reap check is kept only
// as an *additional*, explicitly `cfg`-gated assertion on Linux, so its
// absence elsewhere is visible in the source rather than silently
// vacuous.
// =====================================================================

/// Timeout used by both timeout tests below: comfortably shorter than
/// [`TIMEOUT_TEST_SLEEP`], while long enough (given this test binary's
/// own process-spawn overhead) not to be flaky under load.
const TIMEOUT_TEST_TIMEOUT: Duration = Duration::from_millis(300);
/// Sleep duration used by both timeout tests below: comfortably longer
/// (6-7x) than [`TIMEOUT_TEST_TIMEOUT`], so a runner that failed to kill
/// the child cannot be mistaken for a correct one by scheduling jitter
/// alone.
const TIMEOUT_TEST_SLEEP: Duration = Duration::from_millis(2000);

fn timeout_kills_and_reaps_the_child_process(rt: &tokio::runtime::Runtime) {
    let dir = unique_scratch_dir("timeout-kill");
    let done_file = dir.join("done");

    let mut spec = helper_spec("sleep-then-exit", TIMEOUT_TEST_TIMEOUT);
    spec.env.insert(
        OsString::from(HELPER_SLEEP_MS_VAR),
        OsString::from(TIMEOUT_TEST_SLEEP.as_millis().to_string()),
    );
    spec.env.insert(
        OsString::from(HELPER_DONE_FILE_VAR),
        OsString::from(done_file.as_os_str()),
    );

    let started = std::time::Instant::now();
    let err = run_spec(rt, spec).expect_err("a command that outlives its timeout must error");
    let wall_clock = started.elapsed();

    assert!(
        matches!(&err, ProcessError::TimedOut { .. }),
        "expected ProcessError::TimedOut, got: {err:?}"
    );

    // The runner must not have simply waited the sleep out in the
    // background: it should report back close to the timeout, not close
    // to the (much longer) sleep duration.
    assert!(
        wall_clock < Duration::from_secs(1),
        "run() took {wall_clock:?}, which looks like it waited out the child's full sleep \
         instead of killing it promptly after the timeout"
    );

    // Portable proof (works on every platform this crate targets,
    // including macOS, which has no /proc, and needs no `unsafe`): the
    // helper only ever writes the done-file *after* its sleep completes,
    // so if that file ever appears, the child was not actually stopped
    // by the kill. Wait past the point — measured from `started`, i.e.
    // from spawn, not from when `run()` returned — at which an un-killed
    // child would have finished sleeping and written it, with a
    // comfortable buffer for scheduling jitter.
    let grace_period =
        (TIMEOUT_TEST_SLEEP + Duration::from_millis(1000)).saturating_sub(started.elapsed());
    std::thread::sleep(grace_period);
    assert!(
        !done_file.exists(),
        "the done-file appeared after run() returned: the child was not actually killed \
         before it could finish its sleep and run to completion"
    );

    // Linux-only additional proof: not just signalled, but reaped (no
    // lingering zombie left behind by the time `run()` returns) — an
    // end-to-end guarantee of `run()` as a whole (its explicit
    // `child.kill().await` on the timeout path, backstopped by
    // `kill_on_drop`, both contribute to it), which is what actually
    // matters operationally ("no leaked cluster after normal failure
    // paths"). A zombie still has a /proc entry, so this specifically
    // catches "killed but left unreaped" — which the portable check
    // above cannot distinguish from "properly cleaned up" (both leave
    // no done-file either way).
    #[cfg(target_os = "linux")]
    {
        let ProcessError::TimedOut { stdout, .. } = &err else {
            unreachable!("variant already checked above")
        };
        let pid = parse_pid_line(stdout);
        assert!(
            !proc_dir_exists(pid),
            "child pid {pid} still has a /proc entry after run() returned a Timeout error: \
             it was not fully reaped, so it is a lingering zombie"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

fn timeout_error_carries_the_partial_output_captured_so_far(rt: &tokio::runtime::Runtime) {
    let mut spec = helper_spec("sleep-then-exit", TIMEOUT_TEST_TIMEOUT);
    spec.env.insert(
        OsString::from(HELPER_SLEEP_MS_VAR),
        OsString::from(TIMEOUT_TEST_SLEEP.as_millis().to_string()),
    );

    let err = run_spec(rt, spec).expect_err("a command that outlives its timeout must error");
    let ProcessError::TimedOut { stdout, .. } = &err else {
        panic!("expected ProcessError::TimedOut, got: {err:?}");
    };

    // `helper_sleep_then_exit` writes and flushes its PID line *before*
    // sleeping, so it is always in flight before the kill can land; a
    // runner that discarded output on the timeout path (rather than
    // reporting what it had already captured) would fail this.
    let text = String::from_utf8_lossy(stdout);
    assert!(
        text.starts_with("PID="),
        "expected the pre-sleep PID line to have been captured before the kill; got {text:?}"
    );
}

/// Linux-only: parses the `PID=<digits>` line `helper_sleep_then_exit`
/// writes. Defined only under `cfg(target_os = "linux")`, alongside its
/// one caller, so an accidental non-Linux build cannot silently compile
/// a liveness check that would be vacuous there (see the section note
/// above).
#[cfg(target_os = "linux")]
fn parse_pid_line(stdout: &[u8]) -> u32 {
    let text = std::str::from_utf8(stdout).expect("pid line must be valid utf-8");
    let line = text.lines().next().expect("at least one line of output");
    let digits = line
        .strip_prefix("PID=")
        .unwrap_or_else(|| panic!("expected a PID= line, got {line:?}"));
    digits.parse().expect("valid pid")
}

#[cfg(target_os = "linux")]
fn proc_dir_exists(pid: u32) -> bool {
    // /proc/<pid> exists for a running process *and* for a zombie that
    // has exited but not yet been reaped by its parent; it disappears
    // only once the parent has collected the exit status. A short
    // polling window absorbs any filesystem-visibility delay without
    // weakening what the assertion actually proves.
    let path = Path::new("/proc").join(pid.to_string());
    for _ in 0..20 {
        if !path.exists() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    path.exists()
}

// =====================================================================
// 7. env is layered onto the inherited environment, not exclusive of
//    it.
// =====================================================================

fn env_is_layered_onto_the_inherited_environment(rt: &tokio::runtime::Runtime) {
    // This workspace forbids `unsafe` outright, which rules out using
    // this test process's own `std::env::set_var`/`remove_var` (both
    // `unsafe fn` as of the 2024 edition) to manufacture an ambient
    // variable. Instead, use `PATH`: every process on this platform
    // already inherits one, so it stands in for "a variable set only on
    // the parent, never mentioned in `CommandSpec::env`" without this
    // test mutating global process state at all.
    let ambient_path = std::env::var("PATH").expect("PATH must be set in the test environment");

    // Case 1: PATH is never mentioned in `CommandSpec::env` below, yet
    // the child must still see the parent's value — env is layered onto
    // the inherited environment, not a replacement for it.
    let mut inherits_spec = helper_spec("print-env-var", DEFAULT_TIMEOUT);
    inherits_spec.env.insert(
        OsString::from(HELPER_ENV_VAR_NAME_VAR),
        OsString::from("PATH"),
    );
    let inherited = run_spec(rt, inherits_spec).expect("print-env-var must succeed");
    assert_eq!(inherited.stdout, ambient_path.as_bytes());

    // Case 2: a `CommandSpec::env` entry for the same key overrides the
    // inherited value rather than being shadowed by it.
    let mut overrides_spec = helper_spec("print-env-var", DEFAULT_TIMEOUT);
    overrides_spec.env.insert(
        OsString::from(HELPER_ENV_VAR_NAME_VAR),
        OsString::from("PATH"),
    );
    overrides_spec.env.insert(
        OsString::from("PATH"),
        OsString::from("/definitely-not-a-real-path"),
    );
    let overridden = run_spec(rt, overrides_spec).expect("print-env-var must succeed");
    assert_eq!(overridden.stdout, b"/definitely-not-a-real-path");
    assert_ne!(overridden.stdout, ambient_path.as_bytes());
}

// =====================================================================
// 8. Redacted diagnostic rendering (Step 3 / hazard 3).
// =====================================================================

fn command_context_redacts_credential_like_env_keys(_rt: &tokio::runtime::Runtime) {
    let mut env = BTreeMap::new();
    env.insert(
        OsString::from("HELM_REGISTRY_TOKEN"),
        OsString::from("super-secret-value"),
    );
    env.insert(
        OsString::from("KUBECONFIG_PASSWORD"),
        OsString::from("another-secret"),
    );
    let spec = CommandSpec {
        program: OsString::from("helm"),
        args: vec![OsString::from("install")],
        cwd: None,
        env,
        sensitive_env_keys: BTreeSet::new(),
        timeout: DEFAULT_TIMEOUT,
    };

    let context: CommandContext = spec.context();
    assert_eq!(
        context.env.get("HELM_REGISTRY_TOKEN"),
        Some(&RedactedValue::Sensitive)
    );
    assert_eq!(
        context.env.get("KUBECONFIG_PASSWORD"),
        Some(&RedactedValue::Sensitive)
    );

    let rendered = format!("{context}");
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("super-secret-value"));
    assert!(!rendered.contains("another-secret"));
}

/// Proves, in one run, both halves of the caller-facing redaction
/// mechanism at once: `DB_PASS` matches none of
/// `SENSITIVE_ENV_KEY_MARKERS` (unlike the marker-based test above), so
/// without `sensitive_env_keys` it would render in full. With it:
/// - the *same* `CommandSpec`'s diagnostic-facing `context()`/`Debug`
///   already redact it before anything is spawned;
/// - spawning that *same* spec still delivers the raw value to the real
///   child completely unmodified.
fn caller_marked_sensitive_env_value_reaches_child_but_is_redacted_in_diagnostics(
    rt: &tokio::runtime::Runtime,
) {
    // Deliberately a key `SENSITIVE_ENV_KEY_MARKERS` does NOT match on
    // its own (unlike `HELM_REGISTRY_TOKEN`/`KUBECONFIG_PASSWORD` in the
    // marker-based test above) — chosen to mirror the exact kind of gap
    // called out for this mechanism: a real secret whose name doesn't
    // happen to contain a recognized marker. Without `sensitive_env_keys`
    // this would render in full.
    const SECRET_KEY: &str = "DB_PASS";
    const SECRET_VALUE: &str = "hunter2-real-secret";

    let mut spec = helper_spec("print-env-var", DEFAULT_TIMEOUT);
    spec.env.insert(
        OsString::from(HELPER_ENV_VAR_NAME_VAR),
        OsString::from(SECRET_KEY),
    );
    spec.env
        .insert(OsString::from(SECRET_KEY), OsString::from(SECRET_VALUE));
    spec.sensitive_env_keys.insert(OsString::from(SECRET_KEY));

    // Half 1: diagnostics redact it, computed before the spec is spawned
    // (and hence before `run_spec` below consumes it by value).
    let context = spec.context();
    assert_eq!(context.env.get(SECRET_KEY), Some(&RedactedValue::Sensitive));
    let debug_output = format!("{spec:?}");
    assert!(!debug_output.contains(SECRET_VALUE));
    assert!(debug_output.contains("[REDACTED]"));

    // Half 2: the same spec, actually run, still delivers the raw value
    // to the child untouched.
    let result = run_spec(rt, spec).expect("print-env-var must succeed");
    assert_eq!(result.stdout, SECRET_VALUE.as_bytes());
}

fn command_context_preserves_non_sensitive_env_values(_rt: &tokio::runtime::Runtime) {
    let mut env = BTreeMap::new();
    env.insert(
        OsString::from("NAMESPACE"),
        OsString::from("admission-lab-baseline"),
    );
    let spec = CommandSpec {
        program: OsString::from("kubectl"),
        args: vec![],
        cwd: None,
        env,
        sensitive_env_keys: BTreeSet::new(),
        timeout: DEFAULT_TIMEOUT,
    };

    let context = spec.context();
    assert_eq!(
        context.env.get("NAMESPACE"),
        Some(&RedactedValue::Public("admission-lab-baseline".to_string()))
    );

    let rendered = format!("{context}");
    assert!(rendered.contains("admission-lab-baseline"));
    assert!(!rendered.contains("[REDACTED]"));
}

fn command_spec_debug_output_never_contains_a_redacted_raw_value(_rt: &tokio::runtime::Runtime) {
    let mut env = BTreeMap::new();
    env.insert(
        OsString::from("API_SECRET"),
        OsString::from("do-not-leak-me"),
    );
    env.insert(
        OsString::from("REGION"),
        OsString::from("public-region-value"),
    );
    let spec = CommandSpec {
        program: OsString::from("kind"),
        args: vec![OsString::from("create"), OsString::from("cluster")],
        cwd: None,
        env,
        sensitive_env_keys: BTreeSet::new(),
        timeout: DEFAULT_TIMEOUT,
    };

    let debug_output = format!("{spec:?}");
    assert!(
        !debug_output.contains("do-not-leak-me"),
        "CommandSpec's Debug impl leaked a credential-like env value: {debug_output}"
    );
    assert!(debug_output.contains("[REDACTED]"));
    assert!(debug_output.contains("public-region-value"));
}

fn process_error_display_never_contains_a_redacted_raw_value(rt: &tokio::runtime::Runtime) {
    let mut env = BTreeMap::new();
    env.insert(
        OsString::from("HELM_TOKEN"),
        OsString::from("leaked-if-buggy"),
    );
    let spec = CommandSpec {
        program: OsString::from("/nonexistent/definitely-not-a-real-binary-admissionlab"),
        args: vec![],
        cwd: None,
        env,
        sensitive_env_keys: BTreeSet::new(),
        timeout: DEFAULT_TIMEOUT,
    };

    let err = run_spec(rt, spec).expect_err("spawning a nonexistent binary must fail");
    assert!(matches!(err, ProcessError::Spawn { .. }));
    let rendered = err.to_string();
    assert!(!rendered.contains("leaked-if-buggy"));

    let debug_rendered = format!("{err:?}");
    assert!(!debug_rendered.contains("leaked-if-buggy"));
}
