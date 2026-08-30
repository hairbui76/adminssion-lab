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
//! Three properties are load-bearing here and are each covered by tests
//! in `tests/process.rs`:
//!
//! - **No shell, ever.** `run` builds a `tokio::process::Command` from
//!   `spec.program`/`spec.args` directly and passes each argument
//!   through as its own argv element. There is no code path that
//!   concatenates arguments into a string and hands it to `sh -c`,
//!   `bash -c`, or any shell (Global Constraint 12 / PRODUCT.md §29.4).
//! - **A timeout kills and reaps the child; it never abandons it.**
//!   Wrapping `child.wait()` in a timeout and simply returning once the
//!   timeout elapses would leave the child running as an orphan —
//!   exactly the failure mode that leaks a `kind` cluster. On timeout,
//!   `run` calls `Child::kill`, which sends the kill signal *and* awaits
//!   the child's exit, so control does not return to the caller until
//!   the process is gone.
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
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use thiserror::Error;
use tokio::io::AsyncReadExt as _;
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
    /// Everything the child wrote to stdout, in full.
    pub stdout: Vec<u8>,
    /// Everything the child wrote to stderr, in full.
    pub stderr: Vec<u8>,
    /// Wall-clock time from spawn to exit.
    pub elapsed: Duration,
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
        /// Everything captured on stdout before the child was killed.
        stdout: Vec<u8>,
        /// Everything captured on stderr before the child was killed.
        stderr: Vec<u8>,
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
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }

        let start = Instant::now();
        let mut child = command.spawn().map_err(|source| ProcessError::Spawn {
            context: Box::new(spec.context()),
            source,
        })?;

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
        let stdout_task = tokio::spawn(read_all(stdout_pipe));
        let stderr_task = tokio::spawn(read_all(stderr_pipe));

        let start_timeout = tokio::time::timeout(spec.timeout, child.wait()).await;
        let elapsed = start.elapsed();

        let status = match start_timeout {
            Ok(wait_result) => wait_result.map_err(|source| ProcessError::Io {
                context: Box::new(spec.context()),
                source,
            })?,
            Err(_elapsed) => {
                // `Child::kill` sends the kill signal *and* awaits the
                // child's exit (it is documented as equivalent to
                // sending SIGKILL followed by `wait`), so by the time
                // this resolves successfully the process has already
                // been killed and reaped — nothing further is needed to
                // avoid leaking it.
                child
                    .kill()
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
                let stdout = join_output(stdout_task).await.unwrap_or_default();
                let stderr = join_output(stderr_task).await.unwrap_or_default();
                return Err(ProcessError::TimedOut {
                    context: Box::new(spec.context()),
                    timeout: spec.timeout,
                    elapsed,
                    stdout,
                    stderr,
                });
            }
        };

        let stdout = join_output(stdout_task)
            .await
            .map_err(|source| ProcessError::Io {
                context: Box::new(spec.context()),
                source,
            })?;
        let stderr = join_output(stderr_task)
            .await
            .map_err(|source| ProcessError::Io {
                context: Box::new(spec.context()),
                source,
            })?;

        Ok(CommandResult {
            status,
            stdout,
            stderr,
            elapsed,
        })
    }
}

/// Reads `pipe` to EOF and returns everything read.
async fn read_all<R>(mut pipe: R) -> io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    pipe.read_to_end(&mut buf).await?;
    Ok(buf)
}

/// Awaits a `tokio::spawn`ed [`read_all`] task, flattening a task-join
/// failure (which should only occur if the task panicked) into the same
/// `io::Result` shape as the read itself.
async fn join_output(handle: tokio::task::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    match handle.await {
        Ok(result) => result,
        Err(join_error) => Err(io::Error::other(join_error)),
    }
}
