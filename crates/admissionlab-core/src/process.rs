//! The single chokepoint through which every external command Admission
//! Lab runs — `kind`, `kubectl`, `helm`, and later a metrics scrape —
//! actually executes. Every safety property this module gets right (or
//! wrong) is a safety property of the whole project.
//!
//! [`CommandSpec`] describes one command to run; [`ProcessRunner::run`]
//! runs it and returns a [`CommandResult`] (even for a non-zero exit —
//! that is a normal outcome, not a runner failure) or a [`ProcessError`]
//! for a failure of the *runner itself* (could not spawn, timed out,
//! could not be killed, or some other I/O failure). [`TokioProcessRunner`]
//! is the one production implementation, built on
//! `tokio::process::Command`.
//!
//! # Two shapes of child, one chokepoint
//!
//! [`ProcessRunner::run`] owns a child from spawn to exit: it runs to
//! completion within a timeout, and the caller gets everything it wrote.
//! That is the right shape for `kind create cluster`, `helm install`, or
//! `kubectl apply` — commands that finish.
//!
//! [`ProcessSpawner::spawn`] is the shape for a child that *does not*
//! finish: `kubectl port-forward`, which must stay alive for as long as
//! traffic is being sent through it (ROADMAP Task 6.7). It returns a
//! live [`ManagedChild`] the caller reads from, waits on, and explicitly
//! kills. [`TokioProcessRunner`] implements both traits, on purpose:
//! this module is *the* place an external process is created, and every
//! safety property above — no shell, drained pipes, an isolated
//! environment, redaction — has to hold identically for both shapes. Two
//! separate implementation types would be two places to get them right.
//!
//! **Timeout ownership (Global Constraint 13) differs between the two,
//! and deliberately.** [`ProcessRunner::run`] honors
//! [`CommandSpec::timeout`]: the whole command is bounded by it.
//! [`ProcessSpawner::spawn`] **ignores that field entirely**, because
//! there is no honest value for it: the correct lifetime of a
//! port-forward is "until the Gateway probes are done", which is a fact
//! about the run and not about the process. A fixed timeout here would
//! either be too short (killing a working forward mid-suite) or so long
//! as to be decorative. A long-lived child is therefore bounded by the
//! run's own cleanup — [`ManagedChild::kill`], the caller's guard, and
//! `kill_on_drop` as a backstop — while every *bounded wait* on it
//! ([`ManagedChild::next_stdout_line`]) takes its own explicit timeout
//! argument at the call site, where the caller knows what it is waiting
//! for. See [`ProcessSpawner::spawn`] for why `CommandSpec` is reused
//! rather than forked into a near-identical spawn-only type.
//!
//! Three properties are load-bearing here and are each covered by tests
//! in `tests/process.rs`:
//!
//! - **No shell, ever.** `run` builds a `tokio::process::Command` from
//!   `spec.program`/`spec.args` directly and passes each argument
//!   through as its own argv element. There is no code path that
//!   concatenates arguments into a string and hands it to `sh -c`,
//!   `bash -c`, or any shell (Global Constraint 12 / PRODUCT.md §29.4).
//! - **A timeout kills and reaps the child — and everything the child
//!   started; it never abandons either.** Wrapping `child.wait()` in a
//!   timeout and simply returning once the timeout elapses would leave
//!   the child running as an orphan — exactly the failure mode that
//!   leaks a `kind` cluster. On timeout, `run` calls
//!   [`terminate_child`], which signals the child's whole process group
//!   and does not resolve until the direct child has been killed *and*
//!   reaped, so control does not return to the caller until the process
//!   is gone. See "Process groups" below for the sequence and for what
//!   that guarantee reduces to on a non-Unix target.
//! - **Draining stdout/stderr cannot deadlock.** `helm install` and
//!   `kubectl apply` routinely write more than the OS pipe buffer
//!   (~64 KiB) to stdout or stderr. Calling `child.wait()` before both
//!   pipes have been drained blocks forever once either buffer fills:
//!   the child blocks writing into a full pipe nothing is reading, while
//!   the runner blocks in `wait()` for a child that can now never exit.
//!   `run` drains both pipes on their own `tokio::spawn`ed tasks,
//!   started before `wait()` is ever awaited, so both streams keep
//!   moving regardless of how `wait()` resolves.
//!
//! # Process groups: killing the tree, not the trunk (Task 9.4)
//!
//! Killing the process this module spawned is not the same as killing
//! everything it started. `kind create cluster` shells out to `docker`;
//! `helm` can invoke a downloader plugin. Signalling only the direct
//! child leaves those grandchildren running, reparented to init, holding
//! exactly the cluster state a timeout exists to reclaim.
//!
//! Every child spawned here — bounded ([`ProcessRunner::run`]) and
//! long-lived ([`ProcessSpawner::spawn`]) alike — is therefore placed in
//! **its own process group** on Unix, via
//! `tokio::process::Command::process_group(0)`. That is a *safe* method:
//! tokio re-exposes `std::os::unix::process::CommandExt::process_group`
//! (stable since Rust 1.64), and the `setpgid` call it performs in the
//! forked child is made by the standard library itself, inside its own
//! `unsafe` block. Nothing here needs `pre_exec`, so this crate's
//! workspace-wide `unsafe_code = "forbid"` is honored rather than worked
//! around. Because the group is created with `0`, its group id *is* the
//! child's own pid, which is the number [`ManagedChild::id`] already
//! reports.
//!
//! Termination then follows one sequence, shared by the bounded runner's
//! timeout path and [`ManagedChild::kill`] (see [`terminate_child`]):
//!
//! 1. `SIGTERM` to the whole group, so a well-behaved tool gets to clean
//!    up after itself (`kind` removes its container; `helm` releases its
//!    lock).
//! 2. Wait up to [`PROCESS_GROUP_TERMINATION_GRACE`] for the direct
//!    child to exit.
//! 3. `SIGKILL` to the whole group — unconditionally, even when the
//!    child exited politely in step 2, because a *grandchild* that
//!    ignored the `SIGTERM` is exactly the orphan this exists to
//!    prevent, and an empty group makes this a no-op (`ESRCH`, treated
//!    as success).
//!
//! Signals are sent with `nix::sys::signal::killpg`, a safe wrapper —
//! the same reason `tool.rs` already depends on `nix` for `statvfs`. The
//! child is only ever signalled *before* it is reaped, so its pid (and
//! therefore its group id) cannot have been recycled by the OS onto some
//! unrelated process in the meantime.
//!
//! **On a non-Unix target there are no process groups here.** The
//! grouping call and the signal sequence both compile away, and
//! termination degrades to what this module has always done: kill the
//! direct child (`Child::kill`, backstopped by `kill_on_drop`). A
//! grandchild of a Windows child is *not* cleaned up by this module; a
//! future task wanting that would need a Job Object. This is stated
//! rather than silently assumed because "the timeout killed it" means
//! materially less there.
//!
//! One deliberate consequence on Unix: a child in its own process group
//! no longer receives the terminal's `SIGINT` when an operator presses
//! Ctrl-C. Lifetime is managed explicitly here — by timeout, by `kill`,
//! by `kill_on_drop` — rather than by whichever signal the tty happened
//! to broadcast, and that is the point of the isolation.
//!
//! # Output is bounded, and overflow goes to a file (Task 9.4)
//!
//! `run` used to accumulate stdout and stderr with no limit at all, so a
//! tool that decided to print a gigabyte would be answered by this
//! process allocating a gigabyte. Both streams are now capped at
//! [`MAX_RETAINED_OUTPUT_BYTES`] in memory, and a caller that has a
//! place to put the rest — a run's own `RunPaths::logs` directory — sets
//! [`CommandSpec::spill_dir`], which makes the runner write the
//! *complete* stream to a file there and report its path in
//! [`CommandResult::overflow`]. Nothing is ever silently lost: with a
//! spill directory the full output is on disk, and without one the
//! number of bytes dropped is still reported (Global Constraint 15).
//! [`output_tail`] is the matching rendering rule — an error quotes a
//! bounded *tail* of a stream, never the whole thing.
//!
//! # Environment: inherited, not exclusive
//!
//! `CommandSpec::env` entries are layered onto this process's own
//! inherited environment (`tokio::process::Command`'s default), not a
//! replacement for it: existing variables such as `PATH` remain visible
//! to the child unless `CommandSpec::env` itself overrides them. This
//! matters because the external tools this runner exists to invoke
//! (`kind`, `kubectl`, `helm`) generally need an intact `PATH`, `HOME`,
//! and similar ambient configuration to function at all.
//!
//! The child's stdin is always `/dev/null`: nothing in Admission Lab's
//! automated pipeline can ever supply interactive input, so inheriting
//! this process's stdin would only create a way for a child tool's
//! unexpected prompt to hang forever independent of the timeout.
//!
//! # Redaction: `CommandSpec` vs. `CommandContext`
//!
//! [`CommandSpec::env`] is a plain `BTreeMap<OsString, OsString>` with no
//! per-entry sensitivity flag — that is the canonical, cross-task shape
//! of this type, and it is also exactly what must reach the child
//! process unredacted for a real credential to do its job. Redaction
//! therefore cannot live on `CommandSpec.env` itself without either
//! breaking that shape or storing a second, redacted copy of every
//! secret right next to the original (defeating the purpose).
//!
//! Instead, [`CommandSpec::context`] derives a separate, safe-to-log type,
//! [`CommandContext`], on demand. Building it is the *only* place a
//! decision about sensitivity is made, and PRODUCT.md §29.3 lists two
//! *distinct* obligations here, not one — either is independently
//! sufficient to redact an entry:
//!
//! - **Heuristic, by key name.** Any `env` key that looks credential-like
//!   (case-insensitively containing a marker such as `TOKEN`, `SECRET`,
//!   `KEY`, or `PASSWORD` — see [`SENSITIVE_ENV_KEY_MARKERS`], mirroring
//!   §29.3's "common credential/token environment-variable values") is
//!   redacted automatically, with no caller action required. This catches
//!   the common case but is necessarily incomplete: a key named `DB_PASS`,
//!   `BEARER`, `SESSION_COOKIE`, or `DOCKER_CONFIG_JSON` matches none of
//!   these markers.
//! - **Explicit, by caller.** [`CommandSpec::sensitive_env_keys`] lets a
//!   caller name exactly which keys must be redacted regardless of what
//!   their name looks like — PRODUCT.md §29.3's separately-listed "values
//!   explicitly marked sensitive by configuration," and this codebase's
//!   established convention (see `diagnostic.rs`'s `RedactedValue::Public`:
//!   "a value the caller has decided is safe to display verbatim") that
//!   this decision belongs to the caller, not to a pattern match alone. A
//!   later task building a `CommandSpec` for a secret-bearing env var that
//!   doesn't happen to match the heuristic should add its key here rather
//!   than contort the variable's name to fit the heuristic.
//!
//! The two checks are OR'd together in [`CommandSpec::context`]: a key
//! redacted by either source stays redacted, and neither can un-redact
//! what the other flagged. Either way the result is
//! [`RedactedValue::Sensitive`], which carries no payload at all (the
//! guarantee [`RedactedValue`] already provides for
//! [`crate::diagnostic::Diagnostic`]), so the raw value is never copied
//! into `CommandContext` in the first place — there is no field for it to
//! leak from later, no matter how a `CommandContext` is formatted,
//! logged, or serialized. [`ProcessError`]'s variants carry a
//! `CommandContext`, never a `CommandSpec`, for exactly this reason, and
//! `CommandSpec`'s own `Debug` implementation is hand-written (not
//! derived) to redact through the same path, so that even an incidental
//! `{:?}` of a `CommandSpec` anywhere in the codebase stays safe.
//!
//! Both mechanisms are necessarily best-effort — a heuristic match or a
//! caller's own list, not a guarantee about any specific value — and both
//! are biased toward over-redaction on purpose: a public value hidden by
//! mistake is merely inconvenient, while a credential logged by mistake is
//! an incident.
//!
//! **This is a local layer, not the project's only one.** Task 4.10
//! ("Build report-ready result model and central redaction pass") adds a
//! further, centrally-configured redaction pass over the assembled
//! `LabResult.diagnostics` before a report is written. That later pass
//! cannot substitute for this one: it only ever sees diagnostics that made
//! it into a final `LabResult`, while this module sits on the path of
//! every `tracing` log line later tasks (1.4, 1.7, 2.2, 2.3, 3.8, ...)
//! emit about a command *as it runs* — often well before any report
//! exists, for example while narrating a `kind` bring-up or a `helm
//! install` failure. PRODUCT.md §29.3 scopes the obligation to "reports
//! **and logs**"; for the logs half, this module is the last line of
//! defense, not a stopgap Task 4.10 will later replace.
//!
//! **What this module does not redact.** Only `env` *values* are ever
//! classified. `program`, `args`, and `cwd` are always copied into
//! `CommandContext` verbatim and are never redacted by either mechanism
//! above — proving argv reached the child unmodified is one of this
//! module's core jobs, so hiding it would be counterproductive, and there
//! is no `args`/argv equivalent of `sensitive_env_keys`. A secret passed
//! as a CLI flag rather than an environment variable (for example `helm
//! install --set password=...` instead of an env var) gets **no**
//! protection from this module. Tasks 2.2 (Helm installer) and 2.3
//! (kubectl manifest apply), which build `helm`/`kubectl` argv directly,
//! need to route any credential-like value through `env` rather than
//! `args` if they want this module's redaction to apply to it.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command as TokioCommand;

use crate::diagnostic::RedactedValue;

/// One external command to run: the program, its argv, an optional
/// working directory, environment overrides, and a timeout.
///
/// Every field here except `sensitive_env_keys` is passed to the child
/// process exactly as given — including `env`, which is *not* redacted
/// (see the module documentation for how a safe-to-log description is
/// derived instead, via [`CommandSpec::context`]). There is no shell
/// involved in interpreting any of these fields: `program` is executed
/// directly and each element of `args` becomes exactly one argv element.
#[derive(Clone)]
pub struct CommandSpec {
    /// The program to execute. Not looked up through a shell: this is
    /// passed straight to the OS's process-creation call, so it must
    /// either be an absolute/relative path or a bare name the OS itself
    /// resolves via `PATH`.
    pub program: OsString,
    /// Command-line arguments, in order. Each element becomes exactly
    /// one argv element; no quoting, splitting, or shell expansion is
    /// ever applied to any of them.
    pub args: Vec<OsString>,
    /// The child's working directory. `None` inherits this process's
    /// current working directory.
    pub cwd: Option<PathBuf>,
    /// Environment variables to set for the child, layered onto this
    /// process's own inherited environment (an entry here overrides an
    /// inherited variable of the same name; every other inherited
    /// variable is left untouched). See the module documentation for how
    /// credential-like entries — whether caught by the heuristic or named
    /// in `sensitive_env_keys` — are kept out of diagnostics without being
    /// withheld from the child.
    pub env: BTreeMap<OsString, OsString>,
    /// Additional `env` keys to treat as credential-like in
    /// [`CommandSpec::context`], beyond whatever
    /// [`SENSITIVE_ENV_KEY_MARKERS`] already catches by name. This is the
    /// caller-facing half of redaction (PRODUCT.md §29.3's "values
    /// explicitly marked sensitive by configuration"): use it for a
    /// secret-bearing key whose name doesn't happen to match the
    /// heuristic, such as `DB_PASS` or `SESSION_COOKIE`. Never consulted
    /// when building the child's actual environment — only `env` itself
    /// is; this field exists purely to inform redaction.
    pub sensitive_env_keys: BTreeSet<OsString>,
    /// How long to let the child run before it is killed and
    /// [`ProcessError::TimedOut`] is reported.
    pub timeout: Duration,
    /// Where [`ProcessRunner::run`] may write a stream that outgrows
    /// [`MAX_RETAINED_OUTPUT_BYTES`], so that capping memory does not
    /// mean losing evidence.
    ///
    /// `None` — the value every caller had before Task 9.4, and still
    /// the right one for a command whose output is a handful of bytes —
    /// means overflow is discarded, though still *counted* in
    /// [`CommandResult::overflow`] rather than silently dropped.
    /// `Some(dir)` means the runner writes each oversized stream in full
    /// to a file under `dir` (created if missing) and reports the path.
    /// The intended value is a run's own
    /// [`crate::RunPaths::logs`] directory, which exists precisely for
    /// "process and audit logs captured during the run"; no new artifact
    /// location is introduced for this.
    ///
    /// Opting in is per call site and deliberate: only a caller that
    /// both holds a `RunPaths` and runs a command capable of producing
    /// real volume (`kind create cluster`, `helm upgrade --install`,
    /// `kubectl apply`) sets it. A version probe or a `doctor` check
    /// leaves it `None` rather than littering the logs directory with
    /// empty files for output that was never going to exceed a line.
    ///
    /// Ignored by [`ProcessSpawner::spawn`], like `timeout`: a
    /// long-lived child's capture is bounded by
    /// [`MAX_CAPTURED_STREAM_BYTES`] and is a diagnostic tail by
    /// design, not a transcript worth persisting for as long as a
    /// `kubectl port-forward` lives.
    pub spill_dir: Option<PathBuf>,
}

impl CommandSpec {
    /// Derives a redaction-safe description of this command, suitable
    /// for attaching to a [`ProcessError`] or a
    /// [`crate::diagnostic::Diagnostic`].
    ///
    /// Every `env` entry whose key either looks credential-like (see
    /// [`SENSITIVE_ENV_KEY_MARKERS`]) or is named in
    /// `self.sensitive_env_keys` is replaced with
    /// [`RedactedValue::Sensitive`], which carries no payload, so the raw
    /// value is never copied into the returned [`CommandContext`] — it
    /// cannot later leak through any `Debug`/`Display` of that type no
    /// matter how the result is logged or serialized. The two checks are
    /// OR'd: either one alone is enough to redact an entry. This has no
    /// effect on what is actually passed to the child process: that
    /// always uses `self.env` untouched.
    #[must_use]
    pub fn context(&self) -> CommandContext {
        CommandContext {
            program: self.program.clone(),
            args: self.args.clone(),
            cwd: self.cwd.clone(),
            env: self
                .env
                .iter()
                .map(|(key, value)| {
                    let sensitive =
                        env_key_looks_sensitive(key) || self.sensitive_env_keys.contains(key);
                    let rendered = if sensitive {
                        RedactedValue::Sensitive
                    } else {
                        RedactedValue::Public(value.to_string_lossy().into_owned())
                    };
                    (key.to_string_lossy().into_owned(), rendered)
                })
                .collect(),
        }
    }
}

// Hand-written rather than derived: a derived `Debug` would render every
// `env` value verbatim, including anything credential-like. Deferring to
// `CommandContext`'s own (derived, already-redacted) `Debug` means an
// accidental `{:?}` of a `CommandSpec` anywhere — now or in any later
// task that consumes this type — can never leak a secret, without every
// call site having to remember to redact it first.
impl fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("env", &self.context().env)
            // Safe to show directly, unlike `env`: this holds only key
            // *names* the caller marked sensitive, never values.
            .field("sensitive_env_keys", &self.sensitive_env_keys)
            .field("timeout", &self.timeout)
            .field("spill_dir", &self.spill_dir)
            .finish()
    }
}

/// Case-insensitive substrings that mark a [`CommandSpec`] environment
/// *key* as credential-like for [`CommandSpec::context`], independent of
/// whatever value it holds. This is the automatic half of redaction; see
/// [`CommandSpec::sensitive_env_keys`] for the caller-facing half that
/// catches a credential-bearing key this list doesn't recognize by name.
///
/// Deliberately biased toward over-redaction (for example, `PUBLIC_KEY`
/// still matches `KEY`): a value hidden that did not need to be is
/// merely inconvenient, while a credential that reaches a log is a
/// security incident. Mirrors PRODUCT.md §29.3's "common credential/token
/// environment-variable values".
const SENSITIVE_ENV_KEY_MARKERS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "KEY",
    "AUTH",
    "CERT",
    "PRIVATE",
];

/// Returns whether `key` looks credential-like by
/// [`SENSITIVE_ENV_KEY_MARKERS`].
///
/// `pub` (not only used by [`CommandSpec::context`] above): Task 2.4's
/// `admissionlab-installer` reuses this exact heuristic to redact literal
/// `env[].value` entries in a captured `Deployment`/`DaemonSet`/`Job`'s
/// pod template before it can reach a user-facing readiness diagnostic
/// (PRODUCT.md §29.3 applies there too — a hardcoded credential in an
/// env var is exactly the same anti-pattern regardless of whether it
/// reached this crate via a child process's environment or via a
/// captured Kubernetes object). Re-exported here rather than duplicated
/// so the two call sites can never quietly drift into differently
/// behaving heuristics.
#[must_use]
pub fn env_key_looks_sensitive(key: &OsStr) -> bool {
    let upper = key.to_string_lossy().to_ascii_uppercase();
    SENSITIVE_ENV_KEY_MARKERS
        .iter()
        .any(|marker| upper.contains(marker))
}

/// A redaction-safe description of a [`CommandSpec`], produced by
/// [`CommandSpec::context`].
///
/// `program`, `args`, and `cwd` are copied verbatim: argv content is not
/// treated as credential-like (proving it reached the child unmodified
/// is one of this module's core jobs, so hiding it would be
/// counterproductive), and knowing which program ran and where is
/// exactly the information a failure diagnostic needs. Only `env` values
/// are classified and, when credential-like, replaced with
/// [`RedactedValue::Sensitive`], which carries no payload — the raw
/// value is never copied into this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandContext {
    /// The program that was (or was about to be) run.
    pub program: OsString,
    /// Its arguments, verbatim.
    pub args: Vec<OsString>,
    /// Its working directory, if one was set.
    pub cwd: Option<PathBuf>,
    /// Its environment overrides, with credential-like values redacted.
    pub env: BTreeMap<String, RedactedValue>,
}

impl fmt::Display for CommandContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.program.to_string_lossy())?;
        for arg in &self.args {
            write!(f, " {}", arg.to_string_lossy())?;
        }
        if let Some(cwd) = &self.cwd {
            write!(f, " (cwd: {})", cwd.display())?;
        }
        if !self.env.is_empty() {
            write!(f, " (env:")?;
            for (key, value) in &self.env {
                write!(f, " {key}={value}")?;
            }
            write!(f, ")")?;
        }
        Ok(())
    }
}

/// The outcome of one external command that ran to completion — with
/// *any* exit status, including non-zero — within its timeout.
///
/// A non-zero `status` is a normal, successful [`CommandResult`], not a
/// [`ProcessError`]: interpreting exit codes is a concern for whichever
/// later task calls a specific tool (`kind`, `kubectl`, `helm`), not for
/// this runner. [`ProcessError`] is reserved for failures of the runner
/// itself: the command could not be spawned at all, it exceeded its
/// timeout, or an I/O error occurred while communicating with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    /// The child's exit status.
    pub status: ExitStatus,
    /// What the child wrote to stdout, up to
    /// [`MAX_RETAINED_OUTPUT_BYTES`]. See `overflow` for whether that
    /// cap actually bit, and where the rest went.
    pub stdout: Vec<u8>,
    /// What the child wrote to stderr, up to
    /// [`MAX_RETAINED_OUTPUT_BYTES`]. See `overflow`.
    pub stderr: Vec<u8>,
    /// Wall-clock time from spawn to exit.
    pub elapsed: Duration,
    /// What (if anything) did not fit in `stdout`/`stderr`, and where
    /// the complete stream was written if [`CommandSpec::spill_dir`] was
    /// set. [`OutputOverflow::is_empty`] is the common case: the command
    /// produced less than the cap and these two fields are exactly
    /// everything it wrote.
    pub overflow: OutputOverflow,
}

/// What a single stream lost to [`MAX_RETAINED_OUTPUT_BYTES`], and where
/// to find it instead.
///
/// Default (and overwhelmingly common) is "nothing overflowed": zero
/// bytes omitted, no spill file. Reported rather than inferred, because a
/// truncated capture mistaken for a complete one is the kind of thing
/// Global Constraint 15 exists to forbid — a caller parsing `stdout` can
/// check this and say "the output was truncated" instead of "the tool
/// printed nothing useful".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamOverflow {
    /// How many bytes the child wrote to this stream beyond what was
    /// retained in memory. Zero when the whole stream fit.
    ///
    /// Non-zero does **not** imply the bytes are gone: when `spill_path`
    /// is `Some`, that file holds the stream in full, including these.
    pub omitted_bytes: u64,
    /// The file the complete stream was written to, when
    /// [`CommandSpec::spill_dir`] was set *and* the stream actually
    /// outgrew the in-memory cap. `None` when no spill directory was
    /// configured, when the stream fit in memory (no file is created for
    /// output that did not need one), or when writing the spill file
    /// itself failed — in which case a `tracing::warn!` names the path
    /// and the reason, and `omitted_bytes` still reports the loss.
    pub spill_path: Option<PathBuf>,
}

impl StreamOverflow {
    /// Whether the whole stream fit in memory.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.omitted_bytes == 0
    }
}

impl fmt::Display for StreamOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return write!(f, "complete");
        }
        write!(f, "{} further bytes ", self.omitted_bytes)?;
        match &self.spill_path {
            Some(path) => write!(f, "written to {}", path.display()),
            None => write!(f, "were discarded (no spill directory was configured)"),
        }
    }
}

/// Both of a [`CommandResult`]'s streams' [`StreamOverflow`]s.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputOverflow {
    /// What stdout lost, if anything.
    pub stdout: StreamOverflow,
    /// What stderr lost, if anything.
    pub stderr: StreamOverflow,
}

impl OutputOverflow {
    /// Whether both streams were captured in full.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.stdout.is_empty() && self.stderr.is_empty()
    }
}

/// Failure modes of [`ProcessRunner::run`] itself, as opposed to a
/// non-zero exit from the command it ran (see [`CommandResult`]).
///
/// Every variant carries a [`CommandContext`] rather than a
/// [`CommandSpec`]: the context is already redaction-safe, so formatting
/// a `ProcessError` (via `Display`, `Debug`, or a
/// [`crate::diagnostic::Diagnostic`] built from one) can never expose a
/// credential-like environment value, no matter how the error is
/// eventually logged.
#[derive(Debug, Error)]
pub enum ProcessError {
    /// The child process could not be spawned at all (for example, the
    /// program does not exist or is not executable).
    #[error("failed to spawn `{context}`: {source}")]
    Spawn {
        /// A safe-to-log description of the command that could not be
        /// spawned.
        context: Box<CommandContext>,
        /// The underlying OS error.
        #[source]
        source: io::Error,
    },
    /// The command exceeded its timeout. The child has already been
    /// killed and reaped by the time this is returned: nothing further
    /// is needed to avoid leaking the process.
    #[error("`{context}` exceeded its {timeout:?} timeout (ran for {elapsed:?}) and was killed")]
    TimedOut {
        /// A safe-to-log description of the command that timed out.
        context: Box<CommandContext>,
        /// The timeout that was exceeded.
        timeout: Duration,
        /// Wall-clock time from spawn to the timeout firing.
        elapsed: Duration,
        /// What was captured on stdout before the child was killed,
        /// bounded by [`MAX_RETAINED_OUTPUT_BYTES`].
        stdout: Vec<u8>,
        /// What was captured on stderr before the child was killed,
        /// bounded by [`MAX_RETAINED_OUTPUT_BYTES`].
        stderr: Vec<u8>,
        /// What those two captures lost to the cap, and where the
        /// complete streams were written if [`CommandSpec::spill_dir`]
        /// was set. A timed-out `helm install` is precisely the case
        /// where the interesting output is the part that did not fit, so
        /// this is carried here and not only on the success path.
        ///
        /// Boxed for the reason `context` is: a `ProcessError` is
        /// returned by value up every layer of this project's error
        /// types, so its largest variant is a cost every `Result` on
        /// that path pays (`clippy::result_large_err`). The success path
        /// keeps [`CommandResult::overflow`] unboxed, where the value is
        /// used directly rather than propagated.
        overflow: Box<OutputOverflow>,
    },
    /// The command exceeded its timeout, and killing it *also* failed.
    /// Unlike every other variant, this means the process may still be
    /// running: callers that track cluster/process lifecycle should
    /// treat this as more severe than a plain [`ProcessError::TimedOut`].
    #[error("`{context}` exceeded its {timeout:?} timeout but could not be killed: {source}")]
    KillFailed {
        /// A safe-to-log description of the command that could not be
        /// killed.
        context: Box<CommandContext>,
        /// The timeout that was exceeded.
        timeout: Duration,
        /// The underlying OS error from attempting to kill the child.
        #[source]
        source: io::Error,
    },
    /// A bounded wait for a line of output from a *long-lived*
    /// [`ManagedChild`] elapsed.
    ///
    /// Deliberately not [`ProcessError::TimedOut`], whose documentation
    /// guarantees the child has already been killed and reaped: here the
    /// child is still running, and deciding what to do about that (kill
    /// it, wait longer, report it) belongs to the caller that knows what
    /// it was waiting for. Reusing the other variant would make that
    /// guarantee false for half its occurrences, which is worse than a
    /// second variant.
    ///
    /// `stdout`/`stderr` are the bounded captures taken so far (see
    /// [`MAX_CAPTURED_STREAM_BYTES`]), so a caller can say *what the
    /// child did say* instead of only that it did not say the expected
    /// thing.
    #[error("`{context}` produced no further output within {timeout:?} (waited {elapsed:?})")]
    OutputTimedOut {
        /// A safe-to-log description of the still-running command.
        context: Box<CommandContext>,
        /// The bound that elapsed.
        timeout: Duration,
        /// How long was actually waited.
        elapsed: Duration,
        /// What the child had written to stdout so far.
        stdout: Vec<u8>,
        /// What the child had written to stderr so far.
        stderr: Vec<u8>,
    },

    /// Some other I/O failure occurred while running or communicating
    /// with the child (for example, a failure reading one of its
    /// pipes).
    #[error("io error running `{context}`: {source}")]
    Io {
        /// A safe-to-log description of the command that failed.
        context: Box<CommandContext>,
        /// The underlying OS error.
        #[source]
        source: io::Error,
    },
}

// =========================================================================
// Process-group termination (ROADMAP Task 9.4, Step 1)
// =========================================================================

/// How long a child — and everything else in its process group — is
/// given to exit after `SIGTERM` before `SIGKILL` is sent.
///
/// Five seconds is chosen against what the tools this project actually
/// runs need in order to leave nothing behind: `kind` removes its Docker
/// container on `SIGTERM`, `helm` releases the lease on its release
/// secret, `kubectl port-forward` closes its SPDY stream. All of those
/// are sub-second in practice; the margin is for a loaded CI machine,
/// not for a tool that is going to take its time. It is *not* a second
/// timeout in the Global Constraint 13 sense — the command's own
/// deadline has already fired by the time this applies — so it is
/// deliberately short enough that a hung child cannot add a meaningful
/// delay to a run's failure path.
pub const PROCESS_GROUP_TERMINATION_GRACE: Duration = Duration::from_secs(5);

/// Places `command`'s child in a new process group of its own, so that
/// [`terminate_child`] can later signal the child *and its descendants*
/// as a unit. See the module documentation's "Process groups" section
/// for the safe-API provenance and for what happens on a target with no
/// process groups (this becomes a no-op, and only the direct child is
/// ever killed).
#[cfg(unix)]
fn isolate_process_group(command: &mut TokioCommand) {
    // `0` means "a new group whose id is the child's own pid", which is
    // what makes `ManagedChild::id` usable as the group id below.
    command.process_group(0);
}

#[cfg(not(unix))]
fn isolate_process_group(_command: &mut TokioCommand) {}

/// Sends `signal` to every process in group `pgid`.
///
/// `ESRCH` — no process in that group — is success, not failure: it is
/// the normal answer once the group has emptied out, and the whole point
/// of the `SIGKILL` sweep in [`terminate_child`] is that it is harmless
/// when there is nothing left to sweep.
#[cfg(unix)]
fn signal_process_group(pgid: u32, signal: nix::sys::signal::Signal) -> io::Result<()> {
    let raw = i32::try_from(pgid)
        .map_err(|_| io::Error::other(format!("process id {pgid} does not fit in a pid_t")))?;
    match nix::sys::signal::killpg(nix::unistd::Pid::from_raw(raw), signal) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(errno) => Err(io::Error::from(errno)),
    }
}

/// `SIGTERM` the group, wait [`PROCESS_GROUP_TERMINATION_GRACE`] for the
/// direct child, then `SIGKILL` the group regardless. Returns whether
/// the direct child was reaped in the process.
///
/// The final `SIGKILL` is sent even when the child exited politely,
/// because a grandchild that ignored the `SIGTERM` is exactly the orphan
/// this exists to prevent and the child's own exit says nothing about
/// it. `false` (the child is still unreaped) hands the caller back to a
/// plain `Child::kill`, which both signals it again and reaps it.
#[cfg(unix)]
async fn shut_down_process_group(child: &mut tokio::process::Child, pgid: u32) -> bool {
    use nix::sys::signal::Signal;

    if signal_process_group(pgid, Signal::SIGTERM).is_err() {
        // The group could not be signalled at all (it was never created,
        // or this process lost the right to signal it). Don't spend the
        // grace period waiting for a signal nobody received.
        return false;
    }
    let reaped = matches!(
        tokio::time::timeout(PROCESS_GROUP_TERMINATION_GRACE, child.wait()).await,
        Ok(Ok(_))
    );
    let _ = signal_process_group(pgid, Signal::SIGKILL);
    reaped
}

#[cfg(not(unix))]
async fn shut_down_process_group(_child: &mut tokio::process::Child, _pgid: u32) -> bool {
    false
}

/// Terminates `child` and, on Unix, everything else in its process
/// group; returns once the child has been reaped.
///
/// The single implementation of the sequence documented in this module's
/// "Process groups" section, shared by [`ProcessRunner::run`]'s timeout
/// path and [`ManagedChild::kill`] so the two cannot drift.
///
/// `pid` is the child's pid, which (because every child here is spawned
/// with [`isolate_process_group`]) is also its process group id. `None`
/// — a child already reaped, so its id is gone — skips straight to
/// `Child::kill`, which is then a no-op that reports the truth.
async fn terminate_child(child: &mut tokio::process::Child, pid: Option<u32>) -> io::Result<()> {
    if let Some(pid) = pid
        && shut_down_process_group(child, pid).await
    {
        return Ok(());
    }
    // Either there was no group to signal, or the child outlived the
    // grace period. `Child::kill` sends the signal *and* awaits the
    // exit, so the process is gone (and reaped) when this resolves.
    child.kill().await
}

// =========================================================================
// Bounded output, spilled overflow, bounded error excerpts (Task 9.4)
// =========================================================================

/// The most of each of [`ProcessRunner::run`]'s streams kept in memory.
///
/// Deliberately the same 64 KiB as [`MAX_CAPTURED_STREAM_BYTES`], the
/// long-lived children's cap: one number for "how much command output
/// this process is willing to hold", so a reviewer does not have to work
/// out which shape of child is subject to which limit. It is far more
/// than every command this project runs today actually produces (`helm
/// upgrade --install` prints a NOTES block; `kubectl apply` prints a line
/// per object; `kind create cluster` prints a dozen progress lines), and
/// small enough that a tool that decides to print a gigabyte cannot make
/// this process allocate one.
///
/// Overflow past this point is **not** silently dropped: see
/// [`CommandSpec::spill_dir`] and [`CommandResult::overflow`].
pub const MAX_RETAINED_OUTPUT_BYTES: usize = MAX_CAPTURED_STREAM_BYTES;

/// The most captured command output any single error rendering may
/// embed, via [`output_tail`].
///
/// Four KiB is roughly fifty lines — enough that the actual failure
/// message from `kubectl`, `helm`, or `kind` is present in full, since
/// those tools put the reason at the *end* of the stream, and little
/// enough that an error can be logged, put in a diagnostic, and rendered
/// into a report without any of those having to defend itself against
/// megabytes.
pub const MAX_ERROR_TAIL_BYTES: usize = 4 * 1024;

/// Renders the last [`MAX_ERROR_TAIL_BYTES`] of captured command output
/// as text, for embedding in an error message or a diagnostic.
///
/// The **tail**, not the head: every tool this project runs reports why
/// it failed at the end of its output, after whatever progress it had
/// been narrating. Quoting the beginning would reliably show the
/// uninteresting half.
///
/// When anything is omitted the result says so, in bytes, so a reader is
/// never left to guess whether they are looking at the whole story
/// (Global Constraint 15); and the excerpt is advanced to the next line
/// boundary inside the window so it begins with a whole line rather than
/// mid-token. Bytes are decoded lossily for the reason
/// [`ManagedChild::next_stdout_line`] documents: a stream that is not
/// valid UTF-8 is still evidence.
#[must_use]
pub fn output_tail(bytes: &[u8]) -> String {
    if bytes.len() <= MAX_ERROR_TAIL_BYTES {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut omitted = bytes.len() - MAX_ERROR_TAIL_BYTES;
    let mut tail = &bytes[omitted..];
    if let Some(index) = tail.iter().position(|byte| *byte == b'\n')
        && index + 1 < tail.len()
    {
        omitted += index + 1;
        tail = &tail[index + 1..];
    }
    format!(
        "[... {omitted} earlier bytes of output omitted ...]\n{}",
        String::from_utf8_lossy(tail)
    )
}

/// Distinguishes one command's spill files from the next's within a
/// single process. A plain counter rather than a timestamp or a random
/// id: two commands started in the same millisecond must not collide,
/// and the sequence also happens to record the order they ran in, which
/// is what someone reading a `logs` directory after a failure wants.
static NEXT_SPILL_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Builds the two spill file paths for one command, or `None` when the
/// caller configured no [`CommandSpec::spill_dir`].
///
/// Both streams of one command share a sequence number so they sort
/// together, and carry the program's own name so a `logs` directory
/// listing says what produced each file without anything having to be
/// opened.
fn spill_paths(spec: &CommandSpec) -> Option<(PathBuf, PathBuf)> {
    let dir = spec.spill_dir.as_ref()?;
    let sequence = NEXT_SPILL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let program = spill_program_label(&spec.program);
    Some((
        dir.join(format!("command-{sequence:06}-{program}-stdout.log")),
        dir.join(format!("command-{sequence:06}-{program}-stderr.log")),
    ))
}

/// The file-name-safe, bounded label for `program` used in a spill file
/// name.
///
/// A `CommandSpec::program` is an arbitrary `OsString` — a bare name, an
/// absolute path, in principle non-UTF-8 — and it is *not* trusted to be
/// a filename. Only the final component is used, every byte outside
/// `[A-Za-z0-9._-]` becomes `_`, and the result is clamped so that a
/// pathological program name cannot produce an unusable path.
fn spill_program_label(program: &OsStr) -> String {
    let name = Path::new(program)
        .file_name()
        .unwrap_or(program)
        .to_string_lossy();
    let label: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(32)
        .collect();
    if label.is_empty() {
        "command".to_owned()
    } else {
        label
    }
}

/// One drained stream: what was kept, and what was not.
#[derive(Debug, Default)]
struct StreamCapture {
    retained: Vec<u8>,
    overflow: StreamOverflow,
}

/// Reads `pipe` to EOF, keeping at most [`MAX_RETAINED_OUTPUT_BYTES`] in
/// memory and — once (and only once) that cap is exceeded — writing the
/// stream in full to `spill_path`.
///
/// The pipe is always read to the end regardless of the cap, for the
/// reason this module's own documentation gives for `pump`: a pipe
/// nobody drains is a deadlock, and that is a correctness property
/// independent of whether anyone still wants the bytes.
///
/// The spill file is created lazily, on the first byte past the cap, and
/// the already-retained prefix is written to it first — so the file, when
/// it exists at all, is the *complete* stream rather than the remainder
/// of one, and a command whose output fit in memory leaves no file
/// behind at all.
///
/// A failure to write the spill (a full disk, a `logs` directory that
/// could not be created) never fails the command: it is reported through
/// `tracing::warn!` and the capture degrades to counting the bytes it
/// could not keep, which is what [`StreamOverflow`] already exists to
/// say honestly.
async fn capture_stream<R>(mut pipe: R, spill_path: Option<PathBuf>) -> io::Result<StreamCapture>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut capture = StreamCapture::default();
    let mut spill_path = spill_path;
    let mut spill: Option<tokio::fs::File> = None;
    let mut buffer = vec![0u8; 32 * 1024];

    loop {
        let read = pipe.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        let room = MAX_RETAINED_OUTPUT_BYTES.saturating_sub(capture.retained.len());

        if room < chunk.len() {
            if spill.is_none()
                && let Some(path) = spill_path.take()
            {
                match open_spill_file(&path, &capture.retained).await {
                    Ok(file) => {
                        capture.overflow.spill_path = Some(path);
                        spill = Some(file);
                    }
                    Err(source) => tracing::warn!(
                        path = %path.display(),
                        error = %source,
                        "could not open a spill file for oversized command output; the overflow \
                         will be counted but not kept"
                    ),
                }
            }
            capture.overflow.omitted_bytes += (chunk.len() - room) as u64;
        }
        capture
            .retained
            .extend_from_slice(&chunk[..room.min(chunk.len())]);

        if let Some(file) = spill.as_mut()
            && let Err(source) = file.write_all(chunk).await
        {
            let path = capture.overflow.spill_path.take();
            tracing::warn!(
                path = path.as_ref().map(|path| path.display().to_string()),
                error = %source,
                "could not write oversized command output to its spill file; the overflow will \
                 be counted but not kept"
            );
            spill = None;
        }
    }

    if let Some(mut file) = spill
        && let Err(source) = file.flush().await
    {
        let path = capture.overflow.spill_path.take();
        tracing::warn!(
            path = path.as_ref().map(|path| path.display().to_string()),
            error = %source,
            "could not flush a command output spill file"
        );
    }
    Ok(capture)
}

/// Creates `path` (and any missing parent directory) and writes `prefix`
/// — the part of the stream already held in memory — as its first bytes.
///
/// `create_dir_all` rather than assuming the directory is there: a
/// `RunPaths::logs` directory normally exists by the time any command
/// runs, but a spill that failed because of a missing directory would
/// lose exactly the output a failing run most needs.
async fn open_spill_file(path: &Path, prefix: &[u8]) -> io::Result<tokio::fs::File> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::File::create(path).await?;
    file.write_all(prefix).await?;
    Ok(file)
}

/// The chokepoint every external command Admission Lab runs goes
/// through. See the module documentation for the safety properties this
/// interface exists to guarantee.
#[async_trait]
pub trait ProcessRunner: Send + Sync {
    /// Runs `spec` to completion (or until it is killed for exceeding
    /// its timeout) and returns what happened.
    ///
    /// A non-zero exit status is reported as `Ok(CommandResult { .. })`,
    /// not an error: only a failure of the runner itself — the command
    /// could not be spawned, it exceeded its timeout, or some other I/O
    /// error occurred — is an [`Err`].
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] if the command could not be spawned, if
    /// it exceeded `spec.timeout` (in which case it has already been
    /// killed and reaped before this returns), if killing a timed-out
    /// command itself failed, or if some other I/O error occurred while
    /// communicating with it.
    async fn run(&self, spec: CommandSpec) -> Result<CommandResult, ProcessError>;
}

/// The production [`ProcessRunner`], backed by `tokio::process::Command`.
///
/// Holds no state: every invocation of [`TokioProcessRunner::run`] is
/// independent.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioProcessRunner;

impl TokioProcessRunner {
    /// Creates a new runner.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ProcessRunner for TokioProcessRunner {
    /// # Panics
    ///
    /// Panics only if `tokio::process::Command` fails to honor its own
    /// `Stdio::piped()` configuration for stdout/stderr — an internal
    /// invariant of this implementation, not a condition any caller
    /// input can trigger.
    async fn run(&self, spec: CommandSpec) -> Result<CommandResult, ProcessError> {
        let mut command = TokioCommand::new(&spec.program);
        command
            .args(&spec.args)
            .envs(&spec.env)
            // Nothing in Admission Lab's automated pipeline can ever
            // supply interactive input; inheriting stdin would only give
            // an unexpected child prompt a way to hang independent of
            // the timeout below.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A genuine second line of defense, not merely a passive
            // backstop: tokio's orphan queue reaps a `kill_on_drop`
            // child in the background once its `Child` handle is
            // dropped, independent of whether *this* function's own
            // explicit `child.kill().await` on the timeout path below
            // ran or was itself somehow buggy — confirmed directly by
            // temporarily breaking that explicit call during this
            // module's development and observing this still caught it.
            // The explicit call remains primary because it is
            // synchronous with `run` returning (the caller can rely on
            // "no leaked child" the instant `run` resolves, not
            // "eventually, once tokio's background reaper gets to it"),
            // and because it alone reports `ProcessError::KillFailed` if
            // killing fails outright.
            .kill_on_drop(true);
        // Task 9.4: the child owns a process group of its own, so the
        // timeout path below can terminate the whole tree rather than
        // just the trunk. See this module's "Process groups" section.
        isolate_process_group(&mut command);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }

        let spill = spill_paths(&spec);

        let start = Instant::now();
        let mut child = command.spawn().map_err(|source| ProcessError::Spawn {
            context: Box::new(spec.context()),
            source,
        })?;
        // Captured while the child is definitely unreaped, so it is
        // still a valid process (and process group) id to signal.
        let child_pid = child.id();

        let stdout_pipe = child
            .stdout
            .take()
            .expect("child was spawned with Stdio::piped() stdout");
        let stderr_pipe = child
            .stderr
            .take()
            .expect("child was spawned with Stdio::piped() stderr");

        // Drain both pipes on their own tasks, started before `wait()`
        // is ever awaited below. Reading only after `wait()` returns
        // deadlocks as soon as either stream exceeds the OS pipe buffer
        // (~64 KiB, and `helm install`/`kubectl apply` routinely exceed
        // that): the child blocks writing into a full pipe nothing is
        // reading, while this task blocks in `wait()` for a child that
        // can now never finish. Spawning these as independent tasks
        // (rather than joining them into the same future `timeout`
        // below races) also means a timeout does not discard whatever
        // they had already read: they keep accumulating in the
        // background and are collected once the child has actually been
        // killed and reaped.
        let (stdout_spill, stderr_spill) = match spill {
            Some((stdout_spill, stderr_spill)) => (Some(stdout_spill), Some(stderr_spill)),
            None => (None, None),
        };
        let stdout_task = tokio::spawn(capture_stream(stdout_pipe, stdout_spill));
        let stderr_task = tokio::spawn(capture_stream(stderr_pipe, stderr_spill));

        let start_timeout = tokio::time::timeout(spec.timeout, child.wait()).await;
        let elapsed = start.elapsed();

        let status = match start_timeout {
            Ok(wait_result) => wait_result.map_err(|source| ProcessError::Io {
                context: Box::new(spec.context()),
                source,
            })?,
            Err(_elapsed) => {
                // `terminate_child` signals the child's whole process
                // group (`SIGTERM`, a grace period, then `SIGKILL`) and
                // does not resolve until the direct child has been
                // killed *and reaped*, so by the time this succeeds
                // neither the process nor any grandchild it started is
                // still holding cluster state. See this module's
                // "Process groups" section.
                terminate_child(&mut child, child_pid)
                    .await
                    .map_err(|source| ProcessError::KillFailed {
                        context: Box::new(spec.context()),
                        timeout: spec.timeout,
                        source,
                    })?;

                // Best-effort: the command already failed with a
                // timeout, so a problem collecting the drain tasks'
                // output degrades to empty partial output rather than
                // masking the timeout with a different error.
                let stdout = join_capture(stdout_task).await.unwrap_or_default();
                let stderr = join_capture(stderr_task).await.unwrap_or_default();
                return Err(ProcessError::TimedOut {
                    context: Box::new(spec.context()),
                    timeout: spec.timeout,
                    elapsed,
                    stdout: stdout.retained,
                    stderr: stderr.retained,
                    overflow: Box::new(OutputOverflow {
                        stdout: stdout.overflow,
                        stderr: stderr.overflow,
                    }),
                });
            }
        };

        let stdout = join_capture(stdout_task)
            .await
            .map_err(|source| ProcessError::Io {
                context: Box::new(spec.context()),
                source,
            })?;
        let stderr = join_capture(stderr_task)
            .await
            .map_err(|source| ProcessError::Io {
                context: Box::new(spec.context()),
                source,
            })?;

        Ok(CommandResult {
            status,
            stdout: stdout.retained,
            stderr: stderr.retained,
            elapsed,
            overflow: OutputOverflow {
                stdout: stdout.overflow,
                stderr: stderr.overflow,
            },
        })
    }
}

// =========================================================================
// Long-lived children (ROADMAP Task 6.7)
// =========================================================================

/// The largest amount of each of a [`ManagedChild`]'s streams this
/// module keeps in memory, and the largest amount of stdout it will hand
/// to a caller as lines.
///
/// A long-lived child can write forever — `kubectl port-forward` prints
/// a line per accepted connection — so an unbounded capture would grow
/// without limit for as long as the forward is useful, which is exactly
/// the period during which nothing is going to notice. 64 KiB is far
/// more than the handful of lines any diagnostic in this project reads
/// and small enough to be irrelevant to a run's memory.
///
/// Past the cap, output is still *read* (so the child never blocks on a
/// full pipe — the deadlock this module's own documentation describes)
/// and simply discarded. [`ManagedChild::stdout_truncated`] /
/// [`ManagedChild::stderr_truncated`] report when that has happened, so
/// a truncated capture is never mistaken for a complete one.
pub const MAX_CAPTURED_STREAM_BYTES: usize = 64 * 1024;

/// The longest single line [`ManagedChild::next_stdout_line`] will
/// return. A longer line is truncated to this many bytes; the remainder
/// is consumed from the pipe and discarded, so a child that never emits
/// a newline cannot make this process allocate without limit.
pub const MAX_LINE_BYTES: usize = 8 * 1024;

/// Spawns a child process that outlives the call, for a caller that will
/// manage its lifetime explicitly.
///
/// See this module's "Two shapes of child" section for how this differs
/// from [`ProcessRunner`], and for why `spec.timeout` is ignored here.
#[async_trait]
pub trait ProcessSpawner: Send + Sync {
    /// Spawns `spec` and returns a live [`ManagedChild`].
    ///
    /// `spec.program`, `spec.args`, `spec.cwd` and `spec.env` are honored
    /// exactly as [`ProcessRunner::run`] honors them — argv-only, no
    /// shell, stdin `/dev/null`, environment layered onto the inherited
    /// one. `spec.timeout` is **ignored**; see this module's own
    /// documentation for why a long-lived child has no honest fixed
    /// timeout, and [`ManagedChild::next_stdout_line`] for where a bound
    /// does belong.
    ///
    /// `CommandSpec` is reused rather than forked into a spawn-only type
    /// with no `timeout` field, even though one field is unused: the
    /// redaction discipline ([`CommandSpec::context`],
    /// [`CommandSpec::sensitive_env_keys`], the hand-written `Debug`)
    /// is the whole point of this type, and a parallel type would either
    /// duplicate that logic or quietly do without it.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Spawn`] if the child could not be started
    /// at all. A child that starts and then exits immediately is *not* an
    /// error here — it is a successful spawn whose
    /// [`ManagedChild::wait`] reports a non-zero status, the same line
    /// [`CommandResult`] draws.
    async fn spawn(&self, spec: CommandSpec) -> Result<ManagedChild, ProcessError>;
}

/// One bounded capture of a child's stream.
#[derive(Debug, Default)]
struct CapturedStream {
    bytes: Vec<u8>,
    truncated: bool,
}

impl CapturedStream {
    /// Appends as much of `chunk` as fits under
    /// [`MAX_CAPTURED_STREAM_BYTES`], marking the capture truncated if
    /// anything had to be dropped.
    fn push(&mut self, chunk: &[u8]) {
        let room = MAX_CAPTURED_STREAM_BYTES.saturating_sub(self.bytes.len());
        if room < chunk.len() {
            self.truncated = true;
        }
        self.bytes
            .extend_from_slice(&chunk[..room.min(chunk.len())]);
    }
}

/// A running child process whose lifetime the caller owns.
///
/// Obtained from [`ProcessSpawner::spawn`]. The caller reads lines from
/// its stdout ([`ManagedChild::next_stdout_line`]), inspects what it has
/// written so far ([`ManagedChild::captured_stderr`]), and — this is the
/// load-bearing part — **explicitly terminates it**
/// ([`ManagedChild::kill`]) when it is no longer needed.
///
/// # Why termination cannot be left to `Drop`
///
/// Killing a process and reaping it is asynchronous, and Rust's `Drop`
/// is not: a `Drop` implementation cannot `.await` a `Child::kill`, so
/// it cannot guarantee the process is gone by the time it returns. Three
/// layers therefore exist, in order of strength:
///
/// 1. **[`ManagedChild::kill`], the primary mechanism.** Synchronous
///    with its own completion: when it resolves `Ok`, the process has
///    been signalled *and* reaped. A caller that wants "no leaked
///    process" as a fact rather than a hope must call it.
/// 2. **`kill_on_drop(true)`, the backstop.** Dropping the inner `Child`
///    hands it to tokio's orphan reaper, which kills it in the
///    background. This catches a panic or an early `return` that skipped
///    step 1 — but only while the tokio runtime is still alive, and only
///    "eventually".
/// 3. **This type's own `Drop`, the confession.** It cannot wait, so
///    instead it signals best-effort (`start_kill`) and emits a
///    `tracing::warn!` naming the pid and the exact command to run by
///    hand. That mirrors what
///    `admissionlab_core::run::preserved_cluster_report` and the
///    `cluster.delete_failed` diagnostic already do for a cluster that
///    may have leaked: when Admission Lab cannot guarantee cleanup, it
///    says so and tells the operator precisely what to type, rather than
///    staying quiet and hoping.
#[derive(Debug)]
pub struct ManagedChild {
    /// The child's OS process id, so a diagnostic can name it and an
    /// operator can act on it.
    pub id: u32,
    /// A redaction-safe description of what was spawned, for errors and
    /// for the leak warning.
    context: Box<CommandContext>,
    child: tokio::process::Child,
    /// Complete stdout lines, in order, up to
    /// [`MAX_CAPTURED_STREAM_BYTES`]. Closed when the child's stdout
    /// reaches EOF or the cap is hit.
    stdout_lines: tokio::sync::mpsc::UnboundedReceiver<String>,
    stdout: std::sync::Arc<std::sync::Mutex<CapturedStream>>,
    stderr: std::sync::Arc<std::sync::Mutex<CapturedStream>>,
    /// The two background drain tasks, joined once the child is gone so
    /// that a caller reading [`ManagedChild::captured_stderr`] after
    /// [`ManagedChild::wait`]/[`ManagedChild::kill`] sees everything the
    /// child wrote rather than however much had happened to be read.
    pumps: Vec<tokio::task::JoinHandle<()>>,
    /// Set once the child has been killed or waited on, so `Drop` knows
    /// whether it has anything to warn about.
    reaped: bool,
}

/// How long [`ManagedChild::wait`]/[`ManagedChild::kill`] will wait for
/// the background drain tasks to finish once the child itself is gone.
///
/// They normally finish immediately: a dead process's pipes are at EOF.
/// The bound exists for the one case where they would not — a grandchild
/// that inherited the pipe and outlived its parent — where hanging
/// forever waiting for output from a process this code never spawned
/// would be strictly worse than returning with a capture that says it
/// was truncated. No tool this project spawns behaves that way today;
/// the bound is here so that a tool which starts to cannot hang a run.
const PUMP_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

impl ManagedChild {
    /// A redaction-safe description of the command this child is running.
    #[must_use]
    pub fn context(&self) -> &CommandContext {
        &self.context
    }

    /// Waits up to `timeout` for the child's next complete line of
    /// stdout.
    ///
    /// `Ok(Some(line))` is a line, with its trailing newline removed and
    /// its bytes decoded lossily (a child's output is bytes; a
    /// diagnostic is text, and dropping a line because one byte was not
    /// UTF-8 would destroy evidence — the same choice, for the same
    /// reason, `admissionlab_echo::echo` documents for header values).
    ///
    /// `Ok(None)` means there will never be another line: the child
    /// closed its stdout (in practice, it exited) or it has written more
    /// than [`MAX_CAPTURED_STREAM_BYTES`]. A caller waiting for a
    /// specific line should treat this as "it is not coming" and consult
    /// [`ManagedChild::wait`] for why.
    ///
    /// The `timeout` argument is where Global Constraint 13's bound
    /// lives for a long-lived child: the *process* is unbounded, each
    /// *wait* on it is not. See this module's own documentation.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::OutputTimedOut`] if `timeout` elapses
    /// first. The child is left running: this function never kills it,
    /// because whether a silent child should die is the caller's
    /// decision.
    pub async fn next_stdout_line(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<String>, ProcessError> {
        let start = Instant::now();
        match tokio::time::timeout(timeout, self.stdout_lines.recv()).await {
            Ok(line) => Ok(line),
            Err(_elapsed) => Err(ProcessError::OutputTimedOut {
                context: self.context.clone(),
                timeout,
                elapsed: start.elapsed(),
                stdout: self.captured_stdout(),
                stderr: self.captured_stderr(),
            }),
        }
    }

    /// Everything the child has written to stdout so far, up to
    /// [`MAX_CAPTURED_STREAM_BYTES`].
    #[must_use]
    pub fn captured_stdout(&self) -> Vec<u8> {
        Self::snapshot(&self.stdout).0
    }

    /// Whether [`ManagedChild::captured_stdout`] is missing output the
    /// child actually produced.
    #[must_use]
    pub fn stdout_truncated(&self) -> bool {
        Self::snapshot(&self.stdout).1
    }

    /// Everything the child has written to stderr so far, up to
    /// [`MAX_CAPTURED_STREAM_BYTES`].
    #[must_use]
    pub fn captured_stderr(&self) -> Vec<u8> {
        Self::snapshot(&self.stderr).0
    }

    /// Whether [`ManagedChild::captured_stderr`] is missing output the
    /// child actually produced.
    #[must_use]
    pub fn stderr_truncated(&self) -> bool {
        Self::snapshot(&self.stderr).1
    }

    /// Reads one capture. A poisoned lock is recovered from rather than
    /// propagated: the only writer is this module's own pump task, which
    /// holds the lock across nothing that can panic, and losing a
    /// diagnostic capture is never worth turning into a panic of its own.
    fn snapshot(stream: &std::sync::Mutex<CapturedStream>) -> (Vec<u8>, bool) {
        let guard = stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (guard.bytes.clone(), guard.truncated)
    }

    /// Whether the child has already exited, without waiting for it.
    ///
    /// `Ok(None)` means it is still running.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Io`] if the status could not be checked.
    pub fn try_status(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        match self.child.try_wait() {
            Ok(status) => {
                if status.is_some() {
                    self.reaped = true;
                }
                Ok(status)
            }
            Err(source) => Err(ProcessError::Io {
                context: self.context.clone(),
                source,
            }),
        }
    }

    /// Waits for the child to exit on its own and returns its status.
    ///
    /// Unbounded on purpose — a caller that needs a bound wraps this in
    /// its own `tokio::time::timeout`, at the call site where the right
    /// bound is known. Used in practice to collect the status of a child
    /// that has *already* been observed to exit (its stdout reached EOF),
    /// where there is nothing to bound.
    ///
    /// When this returns, the stream captures are **complete**: the
    /// background drain tasks have been joined (see
    /// [`PUMP_DRAIN_TIMEOUT`]), so
    /// [`ManagedChild::captured_stderr`] is everything the child wrote
    /// up to the cap rather than however much had been read by chance.
    /// Without that barrier a diagnostic built from a dead child's
    /// stderr would be a race, and would usually lose.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Io`] if the child could not be waited on.
    pub async fn wait(&mut self) -> Result<ExitStatus, ProcessError> {
        let status = self.child.wait().await.map_err(|source| ProcessError::Io {
            context: self.context.clone(),
            source,
        })?;
        self.reaped = true;
        self.drain_pumps().await;
        Ok(status)
    }

    /// Joins both drain tasks, bounded by [`PUMP_DRAIN_TIMEOUT`].
    ///
    /// Best-effort by construction: a task that panicked or a bound that
    /// elapsed leaves the captures as they are, which they already
    /// report honestly through `*_truncated`. Turning either into an
    /// error would replace a real exit status (or a real kill failure)
    /// with a complaint about bookkeeping.
    async fn drain_pumps(&mut self) {
        let pumps = std::mem::take(&mut self.pumps);
        for pump in pumps {
            if tokio::time::timeout(PUMP_DRAIN_TIMEOUT, pump)
                .await
                .is_err()
            {
                break;
            }
        }
    }

    /// Kills the child, everything else in its process group, and reaps
    /// it.
    ///
    /// When this resolves `Ok`, the process is gone: [`terminate_child`]
    /// signals the group (`SIGTERM`, up to
    /// [`PROCESS_GROUP_TERMINATION_GRACE`], then `SIGKILL`) and does not
    /// resolve until the direct child has been reaped — exactly what
    /// [`ProcessRunner::run`]'s own timeout path relies on. Calling it on
    /// a child that has already exited succeeds and does nothing (an
    /// empty process group is `ESRCH`, which this treats as success).
    ///
    /// A tool that ignores `SIGTERM` therefore makes this take up to the
    /// grace period rather than returning immediately. That is the
    /// deliberate trade: `kubectl port-forward` (the one long-lived
    /// child this project spawns) exits on `SIGTERM` in well under a
    /// second, and the alternative — `SIGKILL` first — would deny every
    /// tool the chance to remove what it created.
    ///
    /// As with [`ManagedChild::wait`], the stream captures are complete
    /// once this returns `Ok`.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::KillFailed`] if the child could not be
    /// killed, which — as that variant documents — means it may still be
    /// running.
    pub async fn kill(&mut self) -> Result<(), ProcessError> {
        let result = terminate_child(&mut self.child, Some(self.id)).await;
        // Reaped either way: `kill` on an already-exited child is `Ok`,
        // and on failure the flag stops `Drop` from adding a second,
        // redundant warning to an error the caller is already holding.
        self.reaped = true;
        if result.is_ok() {
            self.drain_pumps().await;
        }
        result.map_err(|source| ProcessError::KillFailed {
            context: self.context.clone(),
            // No timeout was in play; a long-lived child is not bounded
            // by one (see this module's documentation). Reported as zero
            // rather than as a fabricated plausible value.
            timeout: Duration::ZERO,
            source,
        })
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // Best-effort only, and in that order on purpose. `Drop` cannot
        // await, so the graded `SIGTERM`/grace/`SIGKILL` sequence
        // `kill()` runs is not available here; what *is* available is
        // one synchronous `SIGTERM` to the whole group, which at least
        // reaches a grandchild that `start_kill` alone would never
        // touch. `start_kill` then signals the direct child and returns
        // immediately, without waiting for it to go away.
        // `kill_on_drop(true)` on the inner `Child` reaps it a moment
        // later via tokio's orphan reaper; the explicit call is here so
        // the intent is visible at the point it matters rather than
        // buried in a builder flag.
        signal_process_group_best_effort(self.id);
        let _ = self.child.start_kill();
        let remedy = manual_termination_command(self.id);
        tracing::warn!(
            pid = self.id,
            command = %self.context,
            "a managed child process was dropped without an explicit kill(); it has been \
             signalled best-effort, but termination could not be awaited -- if pid {} is still \
             running, stop it with: {remedy}",
            self.id,
        );
    }
}

/// Sends one `SIGTERM` to `pgid`, ignoring every failure — the most a
/// synchronous context such as [`ManagedChild`]'s `Drop` can honestly
/// do. Compiles to nothing where there are no process groups.
#[cfg(unix)]
fn signal_process_group_best_effort(pgid: u32) {
    let _ = signal_process_group(pgid, nix::sys::signal::Signal::SIGTERM);
}

#[cfg(not(unix))]
fn signal_process_group_best_effort(_pgid: u32) {}

/// The exact command an operator should type to stop a child Admission
/// Lab could not guarantee it terminated, together with anything that
/// child itself started.
///
/// On Unix that is `kill -- -<pid>`: because every child spawned here
/// leads its own process group (see this module's "Process groups"
/// section), the negative-pid form reaches the grandchildren too, and a
/// bare `kill <pid>` would leave exactly the orphans the operator is
/// being asked to clean up. Elsewhere there is no group to name, so the
/// advice is the direct child and nothing more — which is also all this
/// module itself can do there.
///
/// Public so that every place that has to make this confession — this
/// module's own `Drop`, and `admissionlab-gateway`'s port-forward
/// warning — prints the *same* command, rather than an operator seeing
/// two different remedies for one leaked process.
#[must_use]
pub fn manual_termination_command(pid: u32) -> String {
    if cfg!(unix) {
        format!("kill -- -{pid}")
    } else {
        format!("kill {pid}")
    }
}

#[async_trait]
impl ProcessSpawner for TokioProcessRunner {
    async fn spawn(&self, spec: CommandSpec) -> Result<ManagedChild, ProcessError> {
        let mut command = TokioCommand::new(&spec.program);
        command
            .args(&spec.args)
            .envs(&spec.env)
            // Identical to `run`'s configuration, and for identical
            // reasons — see that function and this module's own
            // documentation. `spec.timeout` is the one field that is
            // deliberately not consulted.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Identical to `run`'s, and load-bearing for the same reason:
        // `ManagedChild::kill` and this type's `Drop` both terminate the
        // whole group, so a long-lived child cannot leave a grandchild
        // behind either. `spec.spill_dir` is the second field (with
        // `timeout`) this shape of child deliberately ignores; see
        // `CommandSpec::spill_dir`.
        isolate_process_group(&mut command);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }

        let mut child = command.spawn().map_err(|source| ProcessError::Spawn {
            context: Box::new(spec.context()),
            source,
        })?;

        // A just-spawned child always has an id; `Child::id` only
        // returns `None` after it has been waited on, which cannot have
        // happened yet.
        let id = child.id().ok_or_else(|| ProcessError::Spawn {
            context: Box::new(spec.context()),
            source: io::Error::other("the spawned child reported no process id"),
        })?;

        let stdout_pipe = child
            .stdout
            .take()
            .expect("child was spawned with Stdio::piped() stdout");
        let stderr_pipe = child
            .stderr
            .take()
            .expect("child was spawned with Stdio::piped() stderr");

        let stdout = std::sync::Arc::new(std::sync::Mutex::new(CapturedStream::default()));
        let stderr = std::sync::Arc::new(std::sync::Mutex::new(CapturedStream::default()));
        let (sender, stdout_lines) = tokio::sync::mpsc::unbounded_channel();

        // Both pipes are drained from the moment of spawn, on their own
        // tasks — the same anti-deadlock rule `run` follows, and more
        // urgent here: a long-lived child writes for as long as it lives,
        // so a pipe nobody reads is certain to fill rather than merely
        // likely to.
        let pumps = vec![
            tokio::spawn(pump(
                stdout_pipe,
                Some(sender),
                std::sync::Arc::clone(&stdout),
            )),
            tokio::spawn(pump(stderr_pipe, None, std::sync::Arc::clone(&stderr))),
        ];

        Ok(ManagedChild {
            id,
            context: Box::new(spec.context()),
            child,
            stdout_lines,
            stdout,
            stderr,
            pumps,
            reaped: false,
        })
    }
}

/// Drains one of a [`ManagedChild`]'s pipes to EOF, capturing what it
/// reads (bounded) and, for stdout, forwarding each complete line.
///
/// Keeps reading past the capture cap rather than stopping: the point is
/// to keep the pipe empty so the child never blocks writing into it,
/// which is a correctness property independent of whether anyone still
/// wants the bytes.
async fn pump<R>(
    pipe: R,
    mut lines: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    captured: std::sync::Arc<std::sync::Mutex<CapturedStream>>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = tokio::io::BufReader::new(pipe);
    let mut line = Vec::new();
    loop {
        line.clear();
        match read_bounded_line(&mut reader, &mut line, &captured).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let over_cap = captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .truncated;
        if over_cap {
            // Past the cap, stop handing lines to the caller too: an
            // unbounded channel nobody drains is the same unbounded
            // growth the byte cap exists to prevent.
            lines = None;
        }
        if let Some(sender) = &lines {
            let text = String::from_utf8_lossy(trim_newline(&line)).into_owned();
            if sender.send(text).is_err() {
                // The receiver (the `ManagedChild`) is gone; nothing is
                // listening, but the pipe must still be drained.
                lines = None;
            }
        }
    }
}

/// Reads up to and including the next `\n` from `reader`, appending at
/// most [`MAX_LINE_BYTES`] of it to `out`, appending every byte it
/// consumes to `captured` (which applies its own, larger cap), and
/// returning how many bytes were consumed from the pipe (`0` only at
/// EOF).
///
/// Not `AsyncBufReadExt::read_until`, which is unbounded: a child that
/// writes a gigabyte with no newline in it would make that function
/// allocate a gigabyte. This consumes exactly the same bytes — so the
/// pipe keeps moving — while allocating at most `MAX_LINE_BYTES` for the
/// line.
///
/// The capture is fed here, byte by consumed byte, rather than from the
/// assembled line: otherwise a stream with no newlines in it at all
/// would be captured only up to `MAX_LINE_BYTES`, and
/// [`MAX_CAPTURED_STREAM_BYTES`] would silently mean something different
/// depending on whether the child happened to emit newlines.
async fn read_bounded_line<R>(
    reader: &mut R,
    out: &mut Vec<u8>,
    captured: &std::sync::Mutex<CapturedStream>,
) -> io::Result<usize>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt as _;

    let mut consumed = 0usize;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(consumed);
        }
        let (take, done) = match available.iter().position(|byte| *byte == b'\n') {
            Some(index) => (index + 1, true),
            None => (available.len(), false),
        };
        let chunk = &available[..take];
        captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(chunk);
        let room = MAX_LINE_BYTES.saturating_sub(out.len());
        out.extend_from_slice(&chunk[..room.min(take)]);
        reader.consume(take);
        consumed += take;
        if done {
            return Ok(consumed);
        }
    }
}

/// Strips one trailing `\n`, and a `\r` before it, from `line`.
fn trim_newline(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Awaits a `tokio::spawn`ed [`capture_stream`] task, flattening a
/// task-join failure (which should only occur if the task panicked) into
/// the same `io::Result` shape as the read itself.
async fn join_capture(
    handle: tokio::task::JoinHandle<io::Result<StreamCapture>>,
) -> io::Result<StreamCapture> {
    match handle.await {
        Ok(result) => result,
        Err(join_error) => Err(io::Error::other(join_error)),
    }
}
