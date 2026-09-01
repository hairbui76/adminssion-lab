//! Watching for `SIGINT`/`SIGTERM`, and what the process does about each
//! one (ROADMAP Task 9.6).
//!
//! `admissionlab_core::Cancellation` is the *state* an interrupted run
//! carries; this module is the only thing that writes to it, and the two
//! halves are split exactly there. Signals are a property of a process,
//! `tokio::signal` needs a runtime, and `admissionlab-core` is a library
//! that must stay usable inside a caller that installs its own handlers
//! — so nothing below this file's `install` call ever learns that a
//! signal is what stopped the run, only that a stop was asked for.
//!
//! # The state machine, in one sentence
//!
//! The first signal cancels cooperatively and says so; the second gives
//! up and exits, printing what is left running first. That is the whole
//! of it, and [`observe_signal`] is that sentence as a function — pure,
//! taking the handle and the signal and returning which of the two
//! happened — so the branch that decides whether an operator's second
//! Ctrl-C is honored can be tested without a process, a runtime, or a
//! real signal.
//!
//! # Why the second one is honored at all
//!
//! Because refusing it is a lie about who is in control. Cooperative
//! teardown is bounded but not instant: a `kind delete cluster` that has
//! gone slow, an install that is still inside its own timeout, and an
//! operator watching a terminal has every right to decide they are not
//! waiting for it. What Admission Lab owes them at that point is not
//! obedience-with-a-clean-conscience — it is the exact commands to undo
//! what the process is about to abandon, written synchronously to stderr
//! before it goes (see [`Cancellation`]'s own documentation for why only
//! stably-named resources are ever on that list).
//!
//! # Why these writes bypass `pipeline::Console`
//!
//! `Console` holds `&mut` borrows of the run's own streams, and the run
//! owns it for its whole duration; a signal watch running concurrently
//! with that run cannot borrow it, and threading a shared, locked stderr
//! through every stage would complicate the pipeline for the sake of two
//! messages. The prefix is the same one `Console` prints
//! (`admissionlab:`), spelled here as its own constant rather than
//! exposed from there, so an interrupt still reads as the same tool in a
//! CI log.

use std::io::Write as _;

use admissionlab_core::{CancelSignal, Cancellation};

/// The same prefix `pipeline::Console` puts on every line. Duplicated,
/// with intent: see this module's documentation.
const PREFIX: &str = "admissionlab:";

/// What the process should do about a signal that just arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptAction {
    /// The first request: cancel cooperatively. The run stops starting
    /// new work, tears down what it has, and exits with the canceled
    /// code.
    Cancel,
    /// A repeat request: the operator is not waiting for teardown. Print
    /// what will be left behind and exit immediately.
    Force,
}

/// Records `signal` against `cancellation` and decides which of the two
/// things happens.
///
/// The count comes from `Cancellation::request` rather than from a
/// second flag kept here, so two signals arriving at once can never both
/// read "first": the counter is atomic and each caller sees its own
/// position in it.
#[must_use]
pub fn observe_signal(cancellation: &Cancellation, signal: CancelSignal) -> InterruptAction {
    if cancellation.request(signal) == 1 {
        InterruptAction::Cancel
    } else {
        InterruptAction::Force
    }
}

/// Starts watching for `SIGINT` and `SIGTERM` on the current `tokio`
/// runtime.
///
/// Must be called from inside a runtime context (in practice, at the top
/// of the `block_on` that drives the run), and returns immediately: the
/// watch is a spawned task that lives as long as the runtime does.
///
/// A signal that arrives *before* this is called keeps the platform's
/// default disposition and kills the process outright — unavoidable, and
/// the reason this is the first thing each command's `block_on` does
/// rather than something set up after the backend is built.
///
/// Installation failing (the OS refusing another signal handler) is
/// reported through `tracing` and otherwise ignored: a run that cannot
/// be interrupted *cleanly* is still a run worth doing, and the default
/// disposition — dying on the signal, leaking whatever existed — is
/// exactly what happens today.
pub fn install(cancellation: Cancellation) {
    tokio::spawn(watch(cancellation));
}

/// The watch loop: every signal, in arrival order, through
/// [`observe_signal`].
#[cfg(unix)]
async fn watch(cancellation: Cancellation) {
    use tokio::signal::unix::{SignalKind, signal};

    // Both streams are created up front: a `SIGTERM` that arrives while
    // this task is still setting up its second handler would otherwise
    // fall through to the default disposition, which is the one case
    // this whole module exists to prevent.
    let mut interrupt = match signal(SignalKind::interrupt()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, "could not watch for SIGINT; Ctrl-C will kill this run outright");
            return;
        }
    };
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, "could not watch for SIGTERM; a kill will end this run outright");
            return;
        }
    };

    loop {
        // `recv()` yields `None` only when the stream is closed, which
        // (for a process-wide signal handler that is never
        // deregistered) means there is nothing left to watch for.
        let received = tokio::select! {
            signal = interrupt.recv() => match signal {
                Some(()) => CancelSignal::Interrupt,
                None => return,
            },
            signal = terminate.recv() => match signal {
                Some(()) => CancelSignal::Terminate,
                None => return,
            },
        };
        act(&cancellation, received);
    }
}

/// The non-Unix watch: `Ctrl-C` and nothing else, because there is no
/// `SIGTERM` to watch for.
#[cfg(not(unix))]
async fn watch(cancellation: Cancellation) {
    loop {
        if tokio::signal::ctrl_c().await.is_err() {
            tracing::warn!("could not watch for Ctrl-C; it will end this run outright");
            return;
        }
        act(&cancellation, CancelSignal::Interrupt);
    }
}

/// Carries out whichever action `signal` calls for. Never returns on the
/// forced path.
fn act(cancellation: &Cancellation, signal: CancelSignal) {
    match observe_signal(cancellation, signal) {
        InterruptAction::Cancel => announce(signal),
        InterruptAction::Force => force_exit(cancellation, signal),
    }
}

/// Tells the operator what the first signal did, and what a second one
/// would do.
///
/// Stating the escape hatch here is the point: without it, an operator
/// watching a run that is midway through a bounded stage has no way to
/// know that pressing Ctrl-C again is a supported thing to do rather
/// than a way to corrupt something.
fn announce(signal: CancelSignal) {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(
        err,
        "{PREFIX} {signal} received; not starting any further work. Tearing down: reports first, \
         then the clusters. Send it again to exit immediately instead."
    );
    let _ = err.flush();
}

/// Prints what the process is abandoning and exits with the signal's
/// code.
///
/// Synchronous, unbuffered, and on the way out: no `await`, no runtime
/// scheduling, nothing that a second interrupt could interleave with.
/// `std::process::exit` runs no destructor, which is precisely why the
/// list is printed — nothing else will clean up after this.
fn force_exit(cancellation: &Cancellation, signal: CancelSignal) -> ! {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(
        err,
        "{PREFIX} second {signal}; exiting now without finishing teardown."
    );
    let commands = cancellation.pending_cleanup_commands();
    if commands.is_empty() {
        // Not "nothing leaked": nothing was *registered*. A run
        // interrupted twice while `kind` was still creating its first
        // cluster has no cluster name to print, and the container may
        // well outlive this process, so the honest answer is where to
        // look rather than a claim about what is there.
        let _ = writeln!(
            err,
            "{PREFIX} no named resource was registered for manual cleanup; if this run had \
             started provisioning, check `kind get clusters` for adlab-* entries and delete each \
             with `kind delete cluster --name <name>`."
        );
    } else {
        let _ = writeln!(err, "{PREFIX} these may still exist; remove them with:");
        for command in commands {
            let _ = writeln!(err, "{PREFIX}   {command}");
        }
    }
    let _ = err.flush();
    std::process::exit(i32::from(signal.exit_code()));
}

#[cfg(test)]
mod tests {
    use super::{InterruptAction, observe_signal};
    use admissionlab_core::{CancelSignal, Cancellation};

    #[test]
    fn the_first_signal_cancels_and_the_second_forces() {
        let cancellation = Cancellation::new();
        assert_eq!(
            observe_signal(&cancellation, CancelSignal::Interrupt),
            InterruptAction::Cancel
        );
        assert!(cancellation.is_requested());
        assert_eq!(cancellation.signal(), Some(CancelSignal::Interrupt));
        assert_eq!(
            observe_signal(&cancellation, CancelSignal::Interrupt),
            InterruptAction::Force
        );
    }

    /// Every signal after the second is still a force, rather than the
    /// state machine wrapping around into another cooperative cancel.
    #[test]
    fn a_third_signal_is_still_a_force() {
        let cancellation = Cancellation::new();
        let actions: Vec<_> = std::iter::repeat_n(CancelSignal::Interrupt, 4)
            .map(|signal| observe_signal(&cancellation, signal))
            .collect();
        assert_eq!(
            actions,
            vec![
                InterruptAction::Cancel,
                InterruptAction::Force,
                InterruptAction::Force,
                InterruptAction::Force,
            ]
        );
    }

    /// A `SIGTERM` following a `SIGINT` forces the exit, but the run is
    /// still reported as canceled by the signal that actually stopped
    /// it — the exit code must not change under a second, different
    /// signal.
    #[test]
    fn a_later_signal_does_not_rewrite_what_stopped_the_run() {
        let cancellation = Cancellation::new();
        assert_eq!(
            observe_signal(&cancellation, CancelSignal::Terminate),
            InterruptAction::Cancel
        );
        assert_eq!(
            observe_signal(&cancellation, CancelSignal::Interrupt),
            InterruptAction::Force
        );
        assert_eq!(cancellation.signal(), Some(CancelSignal::Terminate));
        assert_eq!(
            cancellation.signal().map(CancelSignal::exit_code),
            Some(143)
        );
    }

    #[test]
    fn registered_cleanup_commands_survive_until_they_are_cleared() {
        let cancellation = Cancellation::new();
        cancellation.register_cleanup_command("kind delete cluster --name adlab-baseline-01");
        cancellation.register_cleanup_command("kind delete cluster --name adlab-candidate-01");
        assert_eq!(
            cancellation.pending_cleanup_commands(),
            vec![
                "kind delete cluster --name adlab-baseline-01".to_owned(),
                "kind delete cluster --name adlab-candidate-01".to_owned(),
            ]
        );
        cancellation.clear_cleanup_commands();
        assert!(cancellation.pending_cleanup_commands().is_empty());
    }
}
