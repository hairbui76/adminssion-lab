//! Hardening tests for the external process chokepoint (ROADMAP Task
//! 9.4), covering the three properties `tests/process.rs` deliberately
//! does *not*: that terminating a child terminates everything it
//! started, that its output cannot grow without bound in memory (nor be
//! silently lost when it is capped), and that an argument containing
//! shell metacharacters is delivered as data rather than executed.
//!
//! # Why this is a second file rather than more tests in `process.rs`
//!
//! `tests/process.rs` proves the *shape* of the runner: argv fidelity,
//! separate captures, redaction, a timeout that kills its child. This
//! file proves what happens under adversarial conditions — a child that
//! forks, a child that refuses to die, a child that prints megabytes, an
//! argument that looks like a command — and each of those needs helper
//! behaviors (a process that spawns its own child, a process that blocks
//! `SIGTERM`) that have no place in the other file's helper set. Keeping
//! them apart also keeps `cargo test --test process_hardening` a
//! meaningful command on its own, which is what the roadmap's own
//! verification line runs.
//!
//! # Why this file has no test harness
//!
//! The same reason `tests/process.rs` has none, and it applies with more
//! force here: these tests spawn this very binary as a child (and, in
//! one case, as a *grandchild*) so that they depend on no external
//! program at all. libtest's generated `main` treats argv as test
//! filters and prints a banner to the real stdout before any test body
//! runs, which would corrupt the byte-exact captures below. So this
//! target opts out (`harness = false` in `Cargo.toml`) and provides its
//! own `main`, which dispatches on [`HELPER_MODE_VAR`] before anything
//! else.
//!
//! # Platform honesty
//!
//! Process groups are a Unix concept and this project's liveness check
//! reads `/proc`, so the group tests are `cfg(unix)` and the
//! "and it is really gone" half of them is `cfg(target_os = "linux")`.
//! Where a property cannot be checked on a target it is skipped
//! explicitly and said so, never asserted vacuously — the same rule
//! `tests/process.rs` follows for its own `/proc` checks.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{self, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use admissionlab_core::{
    CommandResult, CommandSpec, MAX_ERROR_TAIL_BYTES, MAX_RETAINED_OUTPUT_BYTES,
    PROCESS_GROUP_TERMINATION_GRACE, ProcessError, ProcessRunner, ProcessSpawner,
    TokioProcessRunner, output_tail,
};

// =====================================================================
// Entry point: helper dispatch, or the hand-rolled test driver.
// =====================================================================

/// Sentinel environment variable. Absent: this binary runs the test
/// table below. Present: its value selects a helper behavior.
const HELPER_MODE_VAR: &str = "ADMISSIONLAB_HARDENING_HELPER";

/// Milliseconds a sleeping helper sleeps before doing its final work.
const HELPER_SLEEP_MS_VAR: &str = "ADMISSIONLAB_HARDENING_SLEEP_MS";
/// A path a helper writes *after* its sleep completes — never when it is
/// killed first. The absence of this file after a kill is the portable
/// proof that the kill actually landed.
const HELPER_DONE_FILE_VAR: &str = "ADMISSIONLAB_HARDENING_DONE_FILE";
/// Bytes the `big-output` helper writes to stdout.
const HELPER_STDOUT_LEN_VAR: &str = "ADMISSIONLAB_HARDENING_STDOUT_LEN";
/// Bytes the `big-output` helper writes to stderr.
const HELPER_STDERR_LEN_VAR: &str = "ADMISSIONLAB_HARDENING_STDERR_LEN";
/// The `spawn-grandchild` helper's grandchild's own done-file.
const HELPER_GRANDCHILD_DONE_FILE_VAR: &str = "ADMISSIONLAB_HARDENING_GRANDCHILD_DONE_FILE";
/// Milliseconds the `spawn-grandchild` helper's grandchild sleeps.
const HELPER_GRANDCHILD_SLEEP_MS_VAR: &str = "ADMISSIONLAB_HARDENING_GRANDCHILD_SLEEP_MS";

/// Environment variables a shell sets on a command it interprets. Their
/// absence in the child is direct evidence that no shell was interposed
/// (see `no_shell_is_interposed_between_this_process_and_the_child`).
const SHELL_MARKER_VARS: &[&str] = &["BASH_EXECUTION_STRING", "ZSH_EXECUTION_STRING"];

/// What `report-identity` prints for a variable that is not set.
const MISSING_MARKER: &str = "<missing>";

fn main() -> ExitCode {
    if let Ok(mode) = std::env::var(HELPER_MODE_VAR) {
        return run_helper(&mode);
    }
    run_tests()
}

// =====================================================================
// Helper mode: this binary, re-invoked as the child (or grandchild).
// =====================================================================

fn run_helper(mode: &str) -> ExitCode {
    match mode {
        "echo-argv" => helper_echo_argv(),
        "report-identity" => helper_report_identity(),
        "big-output" => helper_big_output(),
        "sleep-then-exit" => helper_sleep_then_exit(),
        "block-sigterm-then-sleep" => helper_block_sigterm_then_sleep(),
        "spawn-grandchild" => helper_spawn_grandchild(),
        other => panic!("test bug: unknown helper mode {other:?}"),
    }
}

fn helper_env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Writes every argument after argv[0] to stdout as raw bytes, each
/// terminated by NUL — the one byte that can never appear inside a Unix
/// argv element, so the framing survives arguments containing newlines,
/// quotes, or invalid UTF-8.
fn helper_echo_argv() -> ExitCode {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for arg in std::env::args_os().skip(1) {
        out.write_all(arg.as_bytes()).expect("write arg bytes");
        out.write_all(b"\0").expect("write NUL");
    }
    out.flush().expect("flush");
    ExitCode::SUCCESS
}

/// Writes, NUL-terminated: this process's own argv[0], then one field
/// per [`SHELL_MARKER_VARS`] entry (its value, or [`MISSING_MARKER`]).
///
/// argv[0] is the load-bearing field: had this process been started
/// through `sh -c`, argv[0] would be the shell's idea of the program
/// name and the real argv would have been re-derived from a string.
fn helper_report_identity() -> ExitCode {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let argv0 = std::env::args_os().next().unwrap_or_default();
    out.write_all(argv0.as_bytes()).expect("write argv0");
    out.write_all(b"\0").expect("write NUL");
    for name in SHELL_MARKER_VARS {
        let value = std::env::var(name).unwrap_or_else(|_| MISSING_MARKER.to_string());
        out.write_all(value.as_bytes()).expect("write marker");
        out.write_all(b"\0").expect("write NUL");
    }
    out.flush().expect("flush");
    ExitCode::SUCCESS
}

/// Writes `HELPER_STDOUT_LEN_VAR` bytes of cycling `a`-`z` to stdout and
/// `HELPER_STDERR_LEN_VAR` bytes of cycling `A`-`Z` to stderr, in
/// interleaved chunks. The two alphabets are disjoint so a test can tell
/// at a glance whether any byte crossed streams, and the cycle is
/// deterministic so a spilled file can be checked byte for byte.
fn helper_big_output() -> ExitCode {
    const CHUNK: usize = 8 * 1024;

    let stdout_len: usize = helper_env_or(HELPER_STDOUT_LEN_VAR, "0")
        .parse()
        .expect("valid stdout len");
    let stderr_len: usize = helper_env_or(HELPER_STDERR_LEN_VAR, "0")
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

/// Reports its pid, sleeps, then writes its done-file and exits. Default
/// `SIGTERM` disposition: this is the *cooperative* child, and it dies
/// the moment the termination sequence's first signal arrives.
fn helper_sleep_then_exit() -> ExitCode {
    print_pid_line();
    std::thread::sleep(Duration::from_millis(
        helper_env_or(HELPER_SLEEP_MS_VAR, "0")
            .parse()
            .expect("valid sleep ms"),
    ));
    write_done_file(HELPER_DONE_FILE_VAR);
    ExitCode::SUCCESS
}

/// The uncooperative child: **blocks** `SIGTERM` (it stays pending and
/// is never delivered), reports its pid, sleeps, then writes its
/// done-file.
///
/// Blocking rather than installing an ignoring handler is not a
/// stylistic choice: `sigaction`/`signal` are `unsafe` in every wrapper,
/// and this workspace forbids `unsafe_code` outright.
/// `SigSet::thread_block` is a safe `nix` call that produces exactly the
/// behavior this test needs — a process that outlives `SIGTERM` and can
/// only be stopped by `SIGKILL`.
#[cfg(unix)]
fn helper_block_sigterm_then_sleep() -> ExitCode {
    let mut blocked = nix::sys::signal::SigSet::empty();
    blocked.add(nix::sys::signal::Signal::SIGTERM);
    blocked.thread_block().expect("block SIGTERM");

    print_pid_line();
    std::thread::sleep(Duration::from_millis(
        helper_env_or(HELPER_SLEEP_MS_VAR, "0")
            .parse()
            .expect("valid sleep ms"),
    ));
    write_done_file(HELPER_DONE_FILE_VAR);
    ExitCode::SUCCESS
}

#[cfg(not(unix))]
fn helper_block_sigterm_then_sleep() -> ExitCode {
    panic!("test bug: the block-sigterm helper is Unix-only")
}

/// Spawns a *grandchild* (this binary again, in `sleep-then-exit`),
/// announces both pids, then sleeps far longer than any test waits.
///
/// The grandchild's own stdout/stderr are `/dev/null`, not this
/// process's pipes, on purpose: a grandchild holding the pipe open would
/// make "the runner finished draining" depend on the grandchild dying,
/// which is the very thing under test — a test that hung when the group
/// kill failed would report far less than one that fails.
fn helper_spawn_grandchild() -> ExitCode {
    let exe = std::env::current_exe().expect("current_exe");
    let mut command = std::process::Command::new(exe);
    command
        .env(HELPER_MODE_VAR, "sleep-then-exit")
        .env(
            HELPER_SLEEP_MS_VAR,
            helper_env_or(HELPER_GRANDCHILD_SLEEP_MS_VAR, "60000"),
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Ok(done) = std::env::var(HELPER_GRANDCHILD_DONE_FILE_VAR) {
        command.env(HELPER_DONE_FILE_VAR, done);
    } else {
        command.env_remove(HELPER_DONE_FILE_VAR);
    }
    // Never `wait()`ed on, deliberately: this helper is killed by the
    // test long before its grandchild exits, and the whole point is that
    // the grandchild is a process this crate's runner did not create and
    // does not hold a handle to -- exactly the `docker` that a `kind`
    // leaves behind. Reaping it here would defeat the test.
    #[allow(clippy::zombie_processes)]
    let grandchild = command.spawn().expect("spawn grandchild");

    {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        writeln!(out, "PID={}", std::process::id()).expect("write pid line");
        writeln!(out, "GRANDCHILD={}", grandchild.id()).expect("write grandchild line");
        out.flush().expect("flush");
    }

    std::thread::sleep(Duration::from_secs(600));
    write_done_file(HELPER_DONE_FILE_VAR);
    ExitCode::SUCCESS
}

fn print_pid_line() {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "PID={}", std::process::id()).expect("write pid line");
    out.flush().expect("flush pid line");
}

fn write_done_file(var: &str) {
    if let Ok(path) = std::env::var(var) {
        std::fs::write(&path, b"done").expect("write done-file sentinel");
    }
}

// =====================================================================
// Hand-rolled test driver.
// =====================================================================

type TestFn = fn(&tokio::runtime::Runtime);

const TESTS: &[(&str, TestFn)] = &[
    // ---- Step 1: process groups ----
    (
        "timeout_kills_the_whole_process_group_not_only_the_direct_child",
        timeout_kills_the_whole_process_group_not_only_the_direct_child,
    ),
    (
        "a_cooperative_child_never_pays_the_termination_grace_period",
        a_cooperative_child_never_pays_the_termination_grace_period,
    ),
    (
        "a_child_that_ignores_sigterm_is_force_killed_after_the_grace_period",
        a_child_that_ignores_sigterm_is_force_killed_after_the_grace_period,
    ),
    (
        "managed_child_kill_terminates_the_whole_process_group",
        managed_child_kill_terminates_the_whole_process_group,
    ),
    // ---- Step 2: output caps and spill ----
    (
        "oversized_output_is_capped_in_memory_and_the_loss_is_reported",
        oversized_output_is_capped_in_memory_and_the_loss_is_reported,
    ),
    (
        "oversized_output_is_spilled_to_the_configured_directory_in_full",
        oversized_output_is_spilled_to_the_configured_directory_in_full,
    ),
    (
        "output_that_fits_in_memory_leaves_no_spill_file_behind",
        output_that_fits_in_memory_leaves_no_spill_file_behind,
    ),
    (
        "a_timed_out_command_still_reports_where_its_output_went",
        a_timed_out_command_still_reports_where_its_output_went,
    ),
    // ---- Step 3: bounded error excerpts ----
    (
        "an_error_excerpt_is_a_bounded_tail_not_the_whole_stream",
        an_error_excerpt_is_a_bounded_tail_not_the_whole_stream,
    ),
    // ---- Step 4: adversarial argv ----
    (
        "adversarial_argv_reaches_the_child_byte_for_byte",
        adversarial_argv_reaches_the_child_byte_for_byte,
    ),
    (
        "argv_that_looks_like_shell_commands_is_never_executed",
        argv_that_looks_like_shell_commands_is_never_executed,
    ),
    (
        "no_shell_is_interposed_between_this_process_and_the_child",
        no_shell_is_interposed_between_this_process_and_the_child,
    ),
];

fn run_tests() -> ExitCode {
    // Panics are captured and reported by this driver; the default hook
    // would print each one to the real stderr as noise first.
    std::panic::set_hook(Box::new(|_info| {}));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build test tokio runtime");

    let mut failures: Vec<(&str, String)> = Vec::new();
    for &(name, test_fn) in TESTS {
        print!("test {name} ... ");
        let _ = io::stdout().flush();
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
// Test support.
// =====================================================================

/// Builds a [`CommandSpec`] that re-invokes this test binary in helper
/// `mode`.
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
        spill_dir: None,
    }
}

fn set_env(spec: &mut CommandSpec, key: &str, value: impl Into<OsString>) {
    spec.env.insert(OsString::from(key), value.into());
}

fn run_spec(
    rt: &tokio::runtime::Runtime,
    spec: CommandSpec,
) -> Result<CommandResult, ProcessError> {
    rt.block_on(TokioProcessRunner::new().run(spec))
}

/// A guaranteed-unique scratch directory that removes itself when the
/// test ends, however it ends. Mirrors `tests/process.rs`'s
/// `unique_scratch_dir`, with the cleanup made a `Drop` so a failing
/// assertion cannot leave a directory behind.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(label: &str) -> Self {
        let unique = admissionlab_core::RunId::generate();
        let path = std::env::temp_dir().join(format!(
            "admissionlab-process-hardening-{label}-{}",
            unique.as_str()
        ));
        std::fs::create_dir_all(&path).expect("create scratch dir");
        Self {
            path: path.canonicalize().expect("canonicalize scratch dir"),
        }
    }

    fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Every entry currently in the directory, sorted, as file names.
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.path)
            .expect("read scratch dir")
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Splits a NUL-terminated byte stream into its elements, dropping the
/// empty element the trailing terminator produces.
fn split_nul_terminated(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut parts: Vec<Vec<u8>> = bytes.split(|byte| *byte == 0).map(<[u8]>::to_vec).collect();
    if parts.last().is_some_and(Vec::is_empty) {
        parts.pop();
    }
    parts
}

/// Parses a `KEY=<digits>` line out of captured stdout.
fn parse_labelled_pid(stdout: &[u8], label: &str) -> u32 {
    let text = String::from_utf8_lossy(stdout);
    let prefix = format!("{label}=");
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("expected a {prefix} line in {text:?}"));
    line.trim().parse().unwrap_or_else(|_| panic!("valid pid"))
}

/// Whether `/proc/<pid>` still exists, polled briefly.
///
/// A zombie still has a `/proc` entry, so this specifically catches
/// "signalled but never reaped" as well as "still running" — see
/// `tests/process.rs`, which uses the same check for the same reason.
#[cfg(target_os = "linux")]
fn proc_dir_exists(pid: u32) -> bool {
    let path = Path::new("/proc").join(pid.to_string());
    for _ in 0..40 {
        if !path.exists() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    path.exists()
}

/// The expected content of a `big-output` stream: `len` bytes of the
/// cycling alphabet starting at `first`.
fn expected_stream(first: u8, last: u8, len: usize) -> Vec<u8> {
    (first..=last).cycle().take(len).collect()
}

// =====================================================================
// Step 1 — process groups.
// =====================================================================

/// The failure this whole step exists to prevent: a `kind`/`helm` that
/// timed out, was killed, and left the `docker` it had started running.
/// The helper stands in for that by spawning a grandchild of its own;
/// after the timeout, *neither* process may survive.
fn timeout_kills_the_whole_process_group_not_only_the_direct_child(rt: &tokio::runtime::Runtime) {
    if !cfg!(unix) {
        println!("(skipped: process groups are Unix-only) ");
        return;
    }
    let scratch = ScratchDir::new("group-timeout");
    let grandchild_done = scratch.join("grandchild-done");

    let mut spec = helper_spec("spawn-grandchild", Duration::from_millis(300));
    set_env(
        &mut spec,
        HELPER_GRANDCHILD_DONE_FILE_VAR,
        grandchild_done.as_os_str(),
    );
    // Long enough that the grandchild is certainly still sleeping when
    // the timeout fires, short enough that this test can afford to wait
    // past it and observe that the sentinel never appeared.
    set_env(&mut spec, HELPER_GRANDCHILD_SLEEP_MS_VAR, "3000");

    let started = Instant::now();
    let err = run_spec(rt, spec).expect_err("the helper sleeps far past its timeout");
    let ProcessError::TimedOut { stdout, .. } = &err else {
        panic!("expected ProcessError::TimedOut, got: {err:?}");
    };

    let child_pid = parse_labelled_pid(stdout, "PID");
    let grandchild_pid = parse_labelled_pid(stdout, "GRANDCHILD");
    assert_ne!(
        child_pid, grandchild_pid,
        "the helper must have spawned a real second process"
    );

    // Portable proof, needing neither /proc nor `unsafe`: the grandchild
    // writes its sentinel only *after* its sleep, so if the file never
    // appears it cannot have run to completion. Waited from spawn, with
    // a comfortable margin for a loaded machine.
    let grace = (Duration::from_millis(3000) + Duration::from_millis(1500))
        .saturating_sub(started.elapsed());
    std::thread::sleep(grace);
    assert!(
        !grandchild_done.exists(),
        "the grandchild finished its sleep: killing the timed-out child did not reach the \
         process it had started, which is exactly the orphaned-`docker` failure mode"
    );

    #[cfg(target_os = "linux")]
    {
        assert!(
            !proc_dir_exists(child_pid),
            "the direct child (pid {child_pid}) is still present after run() returned"
        );
        assert!(
            !proc_dir_exists(grandchild_pid),
            "the grandchild (pid {grandchild_pid}) survived its parent's termination: it was \
             reparented rather than killed with the group"
        );
    }
}

/// The grace period is a ceiling, not a cost. A child with the default
/// `SIGTERM` disposition dies on the first signal, so the termination
/// sequence must return promptly rather than always waiting out
/// [`PROCESS_GROUP_TERMINATION_GRACE`] — otherwise every timeout in the
/// project would silently grow by five seconds.
fn a_cooperative_child_never_pays_the_termination_grace_period(rt: &tokio::runtime::Runtime) {
    let mut spec = helper_spec("sleep-then-exit", Duration::from_millis(200));
    set_env(&mut spec, HELPER_SLEEP_MS_VAR, "60000");

    let started = Instant::now();
    let err = run_spec(rt, spec).expect_err("a 60s sleep must exceed a 200ms timeout");
    let elapsed = started.elapsed();

    assert!(
        matches!(&err, ProcessError::TimedOut { .. }),
        "expected ProcessError::TimedOut, got: {err:?}"
    );
    assert!(
        elapsed < PROCESS_GROUP_TERMINATION_GRACE,
        "run() took {elapsed:?}, at least as long as the {PROCESS_GROUP_TERMINATION_GRACE:?} \
         grace period, for a child that exits on SIGTERM immediately: the sequence is waiting \
         out the grace period unconditionally instead of only when it is needed"
    );
}

/// The other half of the sequence: a child that will not die on
/// `SIGTERM` must be force-killed once the grace period expires, and
/// must actually be gone when `run` returns — a timeout that reported
/// success while leaving the process alive would be worse than no
/// timeout at all.
fn a_child_that_ignores_sigterm_is_force_killed_after_the_grace_period(
    rt: &tokio::runtime::Runtime,
) {
    if !cfg!(unix) {
        println!("(skipped: signal blocking is Unix-only) ");
        return;
    }
    let scratch = ScratchDir::new("grace-then-force");
    let done_file = scratch.join("done");

    // Sleeps well past the grace period, so a run that returned before
    // force-killing would leave a process that later writes the
    // sentinel.
    let child_sleep = PROCESS_GROUP_TERMINATION_GRACE + Duration::from_secs(6);
    let mut spec = helper_spec("block-sigterm-then-sleep", Duration::from_millis(200));
    set_env(
        &mut spec,
        HELPER_SLEEP_MS_VAR,
        child_sleep.as_millis().to_string(),
    );
    set_env(&mut spec, HELPER_DONE_FILE_VAR, done_file.as_os_str());

    let started = Instant::now();
    let err = run_spec(rt, spec).expect_err("the helper outlives its timeout");
    let elapsed = started.elapsed();

    let ProcessError::TimedOut { stdout, .. } = &err else {
        panic!("expected ProcessError::TimedOut, got: {err:?}");
    };
    let pid = parse_labelled_pid(stdout, "PID");

    assert!(
        elapsed >= PROCESS_GROUP_TERMINATION_GRACE,
        "run() returned after only {elapsed:?} for a child that cannot receive SIGTERM: the \
         grace period was not honored, so a well-behaved tool would be denied the chance to \
         clean up after itself"
    );
    assert!(
        elapsed < PROCESS_GROUP_TERMINATION_GRACE + Duration::from_secs(4),
        "run() took {elapsed:?}: after the grace period the child must be SIGKILLed at once, \
         not waited out"
    );

    std::thread::sleep(
        (child_sleep + Duration::from_millis(1500)).saturating_sub(started.elapsed()),
    );
    assert!(
        !done_file.exists(),
        "the child finished its sleep and wrote its sentinel: SIGKILL never followed the \
         grace period"
    );

    #[cfg(target_os = "linux")]
    assert!(
        !proc_dir_exists(pid),
        "pid {pid} still has a /proc entry: it was neither killed nor reaped"
    );
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

/// The long-lived shape of child gets the same guarantee: a
/// `kubectl port-forward` (or anything it started) must not survive
/// `ManagedChild::kill`.
fn managed_child_kill_terminates_the_whole_process_group(rt: &tokio::runtime::Runtime) {
    if !cfg!(unix) {
        println!("(skipped: process groups are Unix-only) ");
        return;
    }
    let scratch = ScratchDir::new("group-managed-kill");
    let grandchild_done = scratch.join("grandchild-done");

    let mut spec = helper_spec("spawn-grandchild", Duration::from_millis(1));
    set_env(
        &mut spec,
        HELPER_GRANDCHILD_DONE_FILE_VAR,
        grandchild_done.as_os_str(),
    );
    set_env(&mut spec, HELPER_GRANDCHILD_SLEEP_MS_VAR, "3000");

    let started = Instant::now();
    let mut child = rt
        .block_on(TokioProcessRunner::new().spawn(spec))
        .expect("spawn must succeed");

    // Both announcement lines, so the grandchild is known to exist
    // before anything is killed.
    let line_timeout = Duration::from_secs(10);
    let first = rt
        .block_on(child.next_stdout_line(line_timeout))
        .expect("read")
        .expect("PID line");
    let second = rt
        .block_on(child.next_stdout_line(line_timeout))
        .expect("read")
        .expect("GRANDCHILD line");
    let grandchild_pid =
        parse_labelled_pid(format!("{first}\n{second}\n").as_bytes(), "GRANDCHILD");

    rt.block_on(child.kill()).expect("kill must succeed");

    std::thread::sleep(
        (Duration::from_millis(3000) + Duration::from_millis(1500))
            .saturating_sub(started.elapsed()),
    );
    assert!(
        !grandchild_done.exists(),
        "the grandchild outlived ManagedChild::kill(): killing a port-forward must reach \
         everything it started, not only the process this crate holds a handle to"
    );

    #[cfg(target_os = "linux")]
    assert!(
        !proc_dir_exists(grandchild_pid),
        "the grandchild (pid {grandchild_pid}) survived ManagedChild::kill()"
    );
    #[cfg(not(target_os = "linux"))]
    let _ = grandchild_pid;
}

// =====================================================================
// Step 2 — output caps and spill.
// =====================================================================

/// Without a spill directory the cap still applies, and the loss is
/// still *stated*. A truncated capture silently presented as complete is
/// the fabrication Global Constraint 15 forbids.
fn oversized_output_is_capped_in_memory_and_the_loss_is_reported(rt: &tokio::runtime::Runtime) {
    let stdout_len = MAX_RETAINED_OUTPUT_BYTES * 3;
    let stderr_len = MAX_RETAINED_OUTPUT_BYTES * 2;

    let mut spec = helper_spec("big-output", Duration::from_secs(60));
    set_env(&mut spec, HELPER_STDOUT_LEN_VAR, stdout_len.to_string());
    set_env(&mut spec, HELPER_STDERR_LEN_VAR, stderr_len.to_string());

    let result = run_spec(rt, spec).expect("big-output must succeed");
    assert!(
        result.status.success(),
        "the child must run to completion, not deadlock on a pipe nobody drains past the cap"
    );

    assert_eq!(result.stdout.len(), MAX_RETAINED_OUTPUT_BYTES);
    assert_eq!(result.stderr.len(), MAX_RETAINED_OUTPUT_BYTES);
    // The *prefix* is kept, and it is the child's real output: a cap
    // that quietly reordered or replaced bytes would pass a length check.
    assert_eq!(
        result.stdout,
        expected_stream(b'a', b'z', MAX_RETAINED_OUTPUT_BYTES)
    );
    assert_eq!(
        result.stderr,
        expected_stream(b'A', b'Z', MAX_RETAINED_OUTPUT_BYTES)
    );

    assert!(!result.overflow.is_empty());
    assert_eq!(
        result.overflow.stdout.omitted_bytes,
        (stdout_len - MAX_RETAINED_OUTPUT_BYTES) as u64
    );
    assert_eq!(
        result.overflow.stderr.omitted_bytes,
        (stderr_len - MAX_RETAINED_OUTPUT_BYTES) as u64
    );
    assert_eq!(
        result.overflow.stdout.spill_path, None,
        "no spill directory was configured, so no file may be claimed"
    );
    assert_eq!(result.overflow.stderr.spill_path, None);
}

/// With a spill directory the overflow is not merely counted: the file
/// holds the stream *in full*, prefix included, so nothing a failing
/// command said is actually lost.
fn oversized_output_is_spilled_to_the_configured_directory_in_full(rt: &tokio::runtime::Runtime) {
    let scratch = ScratchDir::new("spill");
    let stdout_len = MAX_RETAINED_OUTPUT_BYTES * 3 + 7;
    let stderr_len = MAX_RETAINED_OUTPUT_BYTES * 2 + 3;

    let mut spec = helper_spec("big-output", Duration::from_secs(60));
    spec.spill_dir = Some(scratch.path().to_path_buf());
    set_env(&mut spec, HELPER_STDOUT_LEN_VAR, stdout_len.to_string());
    set_env(&mut spec, HELPER_STDERR_LEN_VAR, stderr_len.to_string());

    let result = run_spec(rt, spec).expect("big-output must succeed");

    for (label, overflow, first, last, len) in [
        ("stdout", &result.overflow.stdout, b'a', b'z', stdout_len),
        ("stderr", &result.overflow.stderr, b'A', b'Z', stderr_len),
    ] {
        let path = overflow
            .spill_path
            .as_ref()
            .unwrap_or_else(|| panic!("{label} overflowed the cap, so a spill path is required"));
        assert!(
            path.starts_with(scratch.path()),
            "{label} spilled to {}, outside the configured directory",
            path.display()
        );
        assert!(
            path.to_string_lossy().contains(label),
            "{label}'s spill file {} must name the stream it holds",
            path.display()
        );
        let spilled = std::fs::read(path).expect("read spill file");
        assert_eq!(
            spilled.len(),
            len,
            "{label}'s spill file must hold the complete stream, not only the part that did \
             not fit in memory"
        );
        assert_eq!(spilled, expected_stream(first, last, len));
        assert_eq!(
            overflow.omitted_bytes,
            (len - MAX_RETAINED_OUTPUT_BYTES) as u64
        );
    }

    // Exactly two files, one per stream: a spill must not multiply.
    assert_eq!(
        scratch.entries().len(),
        2,
        "expected exactly one spill file per stream, found {:?}",
        scratch.entries()
    );
}

/// The common case must stay clean: a command whose output fits leaves
/// the logs directory exactly as it found it, so an operator reading
/// `logs/` after a run sees files only for commands that produced real
/// volume.
fn output_that_fits_in_memory_leaves_no_spill_file_behind(rt: &tokio::runtime::Runtime) {
    let scratch = ScratchDir::new("no-spill");

    let mut spec = helper_spec("big-output", Duration::from_secs(30));
    spec.spill_dir = Some(scratch.path().to_path_buf());
    set_env(&mut spec, HELPER_STDOUT_LEN_VAR, "512");
    set_env(&mut spec, HELPER_STDERR_LEN_VAR, "512");

    let result = run_spec(rt, spec).expect("big-output must succeed");
    assert_eq!(result.stdout.len(), 512);
    assert_eq!(result.stderr.len(), 512);
    assert!(
        result.overflow.is_empty(),
        "nothing overflowed, so nothing may be reported as omitted"
    );
    assert!(
        scratch.entries().is_empty(),
        "a command whose output fit created spill files anyway: {:?}",
        scratch.entries()
    );
}

/// A timed-out command is exactly when the output that did not fit
/// matters most, so `ProcessError::TimedOut` carries the same overflow
/// report the success path does.
fn a_timed_out_command_still_reports_where_its_output_went(rt: &tokio::runtime::Runtime) {
    let scratch = ScratchDir::new("spill-timeout");

    // Writes far more than the cap, then sleeps past its timeout: the
    // output is real and complete before the kill lands.
    let stdout_len = MAX_RETAINED_OUTPUT_BYTES * 2;
    let mut spec = helper_spec("big-output", Duration::from_millis(700));
    spec.spill_dir = Some(scratch.path().to_path_buf());
    set_env(&mut spec, HELPER_STDOUT_LEN_VAR, stdout_len.to_string());
    set_env(&mut spec, HELPER_STDERR_LEN_VAR, "0");
    // `big-output` exits when it is done writing, so make the command
    // itself the slow part by giving it a timeout it cannot beat.
    spec.timeout = Duration::from_nanos(1);

    match run_spec(rt, spec) {
        Err(ProcessError::TimedOut {
            stdout, overflow, ..
        }) => {
            assert!(
                stdout.len() <= MAX_RETAINED_OUTPUT_BYTES,
                "a timed-out command's partial capture must obey the same cap"
            );
            // Whether the child got far enough to overflow is a race
            // this test cannot (and should not) pin down; what must hold
            // is that the report is *consistent* with what was kept.
            if overflow.stdout.omitted_bytes > 0 {
                let path = overflow
                    .stdout
                    .spill_path
                    .as_ref()
                    .expect("output was omitted with a spill directory configured");
                assert!(path.starts_with(scratch.path()));
            }
        }
        Err(other) => panic!("expected ProcessError::TimedOut, got: {other:?}"),
        Ok(result) => panic!(
            "a 1ns timeout must not produce a successful result (status {:?})",
            result.status
        ),
    }
}

// =====================================================================
// Step 3 — bounded error excerpts.
// =====================================================================

/// Whatever a tool prints, an error rendering quotes a bounded tail of
/// it. The three properties together are what make that safe to log: it
/// is short, it is the *end* (where tools put the reason), and it says
/// how much it left out.
fn an_error_excerpt_is_a_bounded_tail_not_the_whole_stream(_rt: &tokio::runtime::Runtime) {
    let reason = "Error: release admissionlab-baseline failed: timed out waiting for condition";
    let mut noisy = vec![b'x'; MAX_ERROR_TAIL_BYTES * 4];
    noisy.push(b'\n');
    noisy.extend_from_slice(reason.as_bytes());

    let rendered = output_tail(&noisy);
    assert!(
        rendered.len() < MAX_ERROR_TAIL_BYTES * 2,
        "an excerpt of {} bytes is not bounded in any useful sense",
        rendered.len()
    );
    assert!(
        rendered.ends_with(reason),
        "the excerpt dropped the end of the stream, which is where every tool this project \
         runs puts the reason it failed"
    );
    assert!(
        rendered.starts_with("[..."),
        "an excerpt that omitted output must say so: {rendered:.80}"
    );
    assert!(
        rendered.contains("omitted"),
        "the omission notice must be readable, not a bare number: {rendered:.80}"
    );

    // Short output is passed through untouched: no notice, no
    // truncation, nothing to explain.
    let short = b"error: unable to listen on any of the requested ports";
    assert_eq!(output_tail(short), String::from_utf8_lossy(short));

    // Exactly at the bound is still "everything", not "almost
    // everything" -- an off-by-one here would add a confusing notice to
    // a complete excerpt.
    let exact = vec![b'y'; MAX_ERROR_TAIL_BYTES];
    assert_eq!(output_tail(&exact).len(), MAX_ERROR_TAIL_BYTES);

    // Invalid UTF-8 is evidence too: it must be decoded lossily, never
    // dropped.
    let mut invalid = vec![0x80u8, 0xFF];
    invalid.extend_from_slice(b" trailing");
    assert!(output_tail(&invalid).ends_with(" trailing"));
}

// =====================================================================
// Step 4 — adversarial argv.
// =====================================================================

/// Every argument here is chosen because a shell would do something to
/// it: split it, expand it, execute part of it, or swallow it entirely.
/// They must arrive as bytes.
const ADVERSARIAL_ARGS: &[&str] = &[
    "plain",
    "two words",
    "  leading and trailing  ",
    "semi;colon",
    "amp&&ersand",
    "pipe|to|nothing",
    "redirect > /tmp/admissionlab-should-not-exist",
    "$(echo substituted)",
    "`echo backticked`",
    "${HOME}",
    "$HOME",
    "glob*",
    "brace{a,b}",
    "tilde~expansion",
    "single'quote",
    "double\"quote",
    "back\\slash",
    "new\nline",
    "carriage\rreturn",
    "tab\there",
    "",
    "--flag=value with spaces",
    "unicode \u{1F680} and \u{00E9}",
];

fn adversarial_argv_reaches_the_child_byte_for_byte(rt: &tokio::runtime::Runtime) {
    let expected: Vec<OsString> = ADVERSARIAL_ARGS.iter().map(OsString::from).collect();

    let mut spec = helper_spec("echo-argv", Duration::from_secs(30));
    spec.args.clone_from(&expected);

    let result = run_spec(rt, spec).expect("echo-argv must succeed");
    assert!(result.status.success());

    let received = split_nul_terminated(&result.stdout);
    let expected_bytes: Vec<Vec<u8>> = expected.iter().map(|a| a.as_bytes().to_vec()).collect();

    assert_eq!(
        received.len(),
        expected_bytes.len(),
        "the child received {} arguments for {} sent: a shell had split or dropped some",
        received.len(),
        expected_bytes.len()
    );
    for (index, (got, want)) in received.iter().zip(&expected_bytes).enumerate() {
        assert_eq!(
            got,
            want,
            "argv[{}] was mangled: got {:?}, expected {:?}",
            index + 1,
            String::from_utf8_lossy(got),
            String::from_utf8_lossy(want)
        );
    }
}

/// Byte-exactness proves the arguments were not *rewritten*; this proves
/// they were not *run*. Each argument names a file that only exists if
/// something interpreted it, and none of those files may appear.
fn argv_that_looks_like_shell_commands_is_never_executed(rt: &tokio::runtime::Runtime) {
    let scratch = ScratchDir::new("argv-injection");
    let names = [
        "semicolon",
        "substitution",
        "backtick",
        "andand",
        "pipe",
        "newline",
        "redirect",
    ];
    let targets: Vec<PathBuf> = names.iter().map(|name| scratch.join(name)).collect();
    let quoted: Vec<String> = targets
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();

    let injections = [
        format!("; touch {}", quoted[0]),
        format!("$(touch {})", quoted[1]),
        format!("`touch {}`", quoted[2]),
        format!("&& touch {}", quoted[3]),
        format!("| touch {}", quoted[4]),
        format!("\ntouch {}\n", quoted[5]),
        format!("> {}", quoted[6]),
    ];

    let mut spec = helper_spec("echo-argv", Duration::from_secs(30));
    spec.args = injections.iter().map(OsString::from).collect();

    let result = run_spec(rt, spec).expect("echo-argv must succeed");
    assert!(result.status.success());

    // Delivered literally...
    let received = split_nul_terminated(&result.stdout);
    let expected: Vec<Vec<u8>> = injections
        .iter()
        .map(|arg| arg.as_bytes().to_vec())
        .collect();
    assert_eq!(received, expected);

    // ...and not one of them ran.
    for (name, path) in names.iter().zip(&targets) {
        assert!(
            !path.exists(),
            "the {name} injection created {}: an argument was interpreted by a shell \
             instead of being passed through as data (Global Constraint 12)",
            path.display()
        );
    }
    assert!(
        scratch.entries().is_empty(),
        "the injection arguments created files in the scratch directory: {:?}",
        scratch.entries()
    );
}

/// The structural half of the same claim: the child's own argv[0] is the
/// program that was asked for, and its environment carries none of the
/// markers a shell sets on a command it interprets. A `sh -c` in the
/// loop would fail both — argv[0] would be the shell, and the argv the
/// child saw would have been re-derived from a single string.
fn no_shell_is_interposed_between_this_process_and_the_child(rt: &tokio::runtime::Runtime) {
    let exe = std::env::current_exe().expect("current_exe");
    let spec = helper_spec("report-identity", Duration::from_secs(30));

    let result = run_spec(rt, spec).expect("report-identity must succeed");
    assert!(result.status.success());

    let fields = split_nul_terminated(&result.stdout);
    assert_eq!(
        fields.len(),
        1 + SHELL_MARKER_VARS.len(),
        "expected argv[0] plus one field per shell marker"
    );

    assert_eq!(
        fields[0].as_slice(),
        exe.as_os_str().as_bytes(),
        "the child's argv[0] is {:?}, not the program this crate asked for: something is \
         resolving the command on its own",
        String::from_utf8_lossy(&fields[0])
    );

    for (name, value) in SHELL_MARKER_VARS.iter().zip(&fields[1..]) {
        assert_eq!(
            String::from_utf8_lossy(value),
            MISSING_MARKER,
            "the child's environment carries {name}, which only a shell interpreting a \
             command string sets"
        );
    }
}
