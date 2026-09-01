//! A managed local port-forward to a Gateway's data plane (ROADMAP Task
//! 6.7).
//!
//! [`start_service_port_forward`] runs
//!
//! ```text
//! kubectl --kubeconfig <path> -n <namespace> port-forward service/<name> :<remote-port> --address 127.0.0.1
//! ```
//!
//! waits for `kubectl` to announce which local port it chose, and hands
//! back a [`PortForwardHandle`] whose `local_addr` is where Task 6.8's
//! HTTP probes actually connect.
//!
//! # Why a port-forward at all
//!
//! A `kind` cluster's `Service` addresses are not routable from the host,
//! and a `Gateway`'s own `status.addresses` is routinely an address
//! nothing outside the cluster can dial. `kubectl port-forward` is the
//! one mechanism that works identically for every implementation without
//! the fixture having to arrange a `NodePort`, a host network, or a
//! `LoadBalancer` — so what a probe measures stays a property of the
//! Gateway under test rather than of the lab's plumbing.
//!
//! # The local port is chosen by the OS, not by this crate
//!
//! The argv's `:<remote-port>` form — an empty local port — asks
//! `kubectl` to bind an ephemeral one. Picking a fixed local port here
//! would make two concurrent runs (or a baseline and a candidate forward
//! held at once) collide on the same socket, and would fail against
//! whatever else on the developer's machine happened to hold it. The
//! cost is that the chosen port is only knowable by *reading what
//! `kubectl` says*, which is what [`parse_forwarding_line`] does and why
//! this module has a parser at all.
//!
//! `--address 127.0.0.1` is passed explicitly rather than relying on
//! `kubectl`'s default: it binds the loopback interface only, so a
//! forward into a disposable cluster is never reachable from the network
//! the developer's machine is on. v1 binds exactly this one address; the
//! parser nonetheless understands the bracketed IPv6 form `kubectl`
//! prints when asked for one, so the day a fixture needs `::1` the
//! parsing half is already correct and tested rather than being written
//! under pressure.
//!
//! # Timeout ownership (Global Constraint 13)
//!
//! Two different bounds apply here, and conflating them would be wrong
//! in both directions:
//!
//! - **Becoming ready is bounded**, by [`PORT_FORWARD_READY_TIMEOUT`]. A
//!   `kubectl` that never announces a port is a failure, and waiting
//!   forever for it would hang a run. That bound is load-bearing rather
//!   than defensive: measured directly against a real `kind` cluster
//!   while writing this module, `kubectl port-forward` to a `Service`
//!   with **no ready endpoints does not fail — it waits, silently**,
//!   printing nothing at all to either stream until it is killed. With
//!   no bound here, the single most likely Gateway fixture failure (a
//!   data plane that never came up) would hang the suite instead of
//!   reporting itself.
//! - **Staying alive is not.** A working forward must last exactly as
//!   long as the probes that use it, which is a fact about the suite and
//!   not about the process — so nothing here imposes a lifetime on it.
//!   `admissionlab_core::CommandSpec::timeout` is therefore not what
//!   bounds this child (`ProcessSpawner::spawn` ignores that field by
//!   design; see `admissionlab_core::process`'s own documentation), and
//!   the spec this module builds sets it to
//!   [`PORT_FORWARD_READY_TIMEOUT`] purely so the value is not a lie
//!   sitting in a struct — the readiness wait uses it directly.
//!
//! # Termination: what is guaranteed, and what is only best-effort
//!
//! [`PortForwardHandle::close`] is the guarantee: when it returns `Ok`,
//! the `kubectl` process has been killed *and reaped*, because
//! `admissionlab_core::ManagedChild::kill` does not resolve until it
//! has been.
//!
//! Since Task 9.4 that guarantee covers a little more ground than this
//! module needs: `ManagedChild` places its child in its own process
//! group and terminates the *group* (`SIGTERM`, a grace period, then
//! `SIGKILL`), so nothing the child started can outlive it either.
//! `kubectl port-forward` spawns no subprocesses of its own, so for
//! this module the practical change is only that `close` gives
//! `kubectl` a chance to shut its SPDY stream down cleanly before it is
//! killed outright, instead of being `SIGKILL`ed immediately. Nothing
//! here has to opt in, and nothing here may opt out: it is a property
//! of the one chokepoint every child in this project goes through.
//!
//! Everything else is a backstop, because Rust's `Drop` is synchronous
//! and killing a process is not — a `Drop` cannot await the termination
//! it starts, so it cannot promise one. Rather than reimplement that
//! problem here, [`PortForwardHandle`] delegates it wholly to
//! `ManagedChild`, which already resolves it in three documented layers:
//! an explicit `kill`, `kill_on_drop(true)` as a runtime backstop, and a
//! `Drop` that signals best-effort and emits a `tracing::warn!` naming
//! the pid together with the exact command an operator should run
//! (`kill <pid>`). That warning is deliberately *not* duplicated at this
//! level: one leaked process should produce one warning naming one pid,
//! and a second wrapper printing its own copy would only make an
//! operator wonder whether two things leaked.
//!
//! This mirrors what Phase 1 already established for a resource that
//! might survive a failure: `admissionlab_core::run`'s
//! `cluster.delete_failed` diagnostic and `preserved_cluster_report`
//! both end in the exact, copy-pasteable command that removes the thing
//! (`kind delete cluster --name <name>`). When Admission Lab cannot
//! guarantee cleanup, it says so and says what to type.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use admissionlab_core::{ClusterHandle, CommandSpec, ManagedChild, ProcessError, ProcessSpawner};
use async_trait::async_trait;

use crate::endpoint::GatewayEndpoint;
use crate::error::GatewayError;

/// The program [`start_service_port_forward`] runs. A bare name, resolved
/// by the OS through `PATH` — never a shell (Global Constraint 12), and
/// never a path this crate guesses at. `admissionlab_core::ToolName` and
/// `admissionlab_installer::manifests` spell it identically.
pub const KUBECTL_PROGRAM: &str = "kubectl";

/// The only local address a v1 port-forward binds. See this module's
/// documentation for why loopback-only is explicit rather than assumed.
pub const LOCAL_ADDRESS: &str = "127.0.0.1";

/// How long [`start_service_port_forward`] waits for `kubectl` to
/// announce the local port it bound.
///
/// Bounds *becoming ready*, never *staying alive* — see this module's
/// "Timeout ownership" section. Fifteen seconds is far longer than a
/// healthy `kubectl port-forward` needs (it prints its `Forwarding from`
/// line as soon as the API server accepts the SPDY/websocket upgrade,
/// normally well under a second) and short enough that a forward to a
/// Service with no ready endpoints fails the suite promptly instead of
/// stalling it.
pub const PORT_FORWARD_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// The prefix `kubectl port-forward` writes before the address it bound.
const FORWARDING_PREFIX: &str = "Forwarding from ";

/// The separator between the local address and the remote port in that
/// same line.
const FORWARDING_ARROW: &str = " -> ";

/// A running `kubectl port-forward`, and the local address it bound.
///
/// Obtained from [`start_service_port_forward`]. Terminate it with
/// [`PortForwardHandle::close`]; see this module's "Termination" section
/// for what happens if you do not.
#[derive(Debug)]
pub struct PortForwardHandle {
    /// Where the Gateway's data plane can be reached on this machine.
    /// Always on [`LOCAL_ADDRESS`], with an OS-chosen port.
    pub local_addr: SocketAddr,
    /// The `kubectl` process itself. Private: a caller that could reach
    /// in and `wait()` on it would block until the forward died, which
    /// is never what a caller wants and is not what this type is for.
    child: ManagedChild,
    /// The cluster's own name and the endpoint being forwarded to,
    /// carried purely so [`PortForwardHandle::close`]'s error can name
    /// them -- the same reason
    /// [`crate::reconcile::KubeGatewayStatusSource`] carries a cluster
    /// name it never uses to build a client. Neither is part of this
    /// type's surface: an error that said `cluster: ""` because the
    /// handle had thrown the name away would be worse than a private
    /// field.
    cluster: String,
    endpoint: String,
}

impl PortForwardHandle {
    /// The `kubectl` process's OS process id.
    ///
    /// Exposed so a run's own diagnostics can name it — the same value
    /// `ManagedChild`'s leak warning prints, so an operator reading
    /// either sees one number rather than two.
    #[must_use]
    pub fn child_id(&self) -> u32 {
        self.child.id
    }

    /// Kills the port-forward and reaps it.
    ///
    /// Consumes the handle: a closed forward's `local_addr` connects to
    /// nothing, and letting a caller keep one around would only invite a
    /// probe against a dead socket.
    ///
    /// # Errors
    ///
    /// Returns [`GatewayError::PortForwardUnavailable`] if the process
    /// could not be killed — which, as
    /// `admissionlab_core::ProcessError::KillFailed` documents, means it
    /// may still be running.
    pub async fn close(mut self) -> Result<(), GatewayError> {
        self.child
            .kill()
            .await
            .map_err(|source| GatewayError::PortForwardUnavailable {
                cluster: self.cluster.clone(),
                endpoint: self.endpoint.clone(),
                reason: source.to_string(),
            })
    }
}

/// Builds the exact [`CommandSpec`] a port-forward runs.
///
/// Public so `tests/port_forward_unit.rs` can assert the argv element by
/// element without spawning anything: argv is the whole safety surface
/// here (no shell, no string concatenation, no interpolation into a
/// single argument), so it is worth asserting directly rather than
/// inferring from a process's behavior.
///
/// The argv is exactly ROADMAP Task 6.7 Step 1's, in that order:
///
/// ```text
/// kubectl --kubeconfig <path> -n <namespace> port-forward service/<name> :<remote-port> --address 127.0.0.1
/// ```
///
/// `--kubeconfig` is unconditional and comes from the
/// [`ClusterHandle`]'s own isolated file, never from an ambient
/// `KUBECONFIG` or `~/.kube/config` — the same rule
/// `admissionlab_installer::manifests` states for every `kubectl` it
/// runs, and the property that makes a Phase 6 forward incapable of
/// reaching a production cluster.
///
/// No environment overrides at all: nothing about a port-forward is
/// credential-bearing, and selecting a cluster through an environment
/// variable is exactly the ambient-configuration path this project
/// avoids everywhere else.
#[must_use]
pub fn port_forward_command(kubeconfig: &Path, endpoint: &GatewayEndpoint) -> CommandSpec {
    let args: Vec<OsString> = vec![
        "--kubeconfig".into(),
        kubeconfig.as_os_str().to_owned(),
        "-n".into(),
        endpoint.namespace.as_str().into(),
        "port-forward".into(),
        format!("service/{}", endpoint.service).into(),
        // The leading `:` is what asks for an OS-chosen local port -- see
        // this module's documentation.
        format!(":{}", endpoint.port).into(),
        "--address".into(),
        LOCAL_ADDRESS.into(),
    ];
    CommandSpec {
        program: KUBECTL_PROGRAM.into(),
        args,
        cwd: None,
        env: BTreeMap::new(),
        sensitive_env_keys: BTreeSet::new(),
        // Not what bounds this child -- `ProcessSpawner::spawn` ignores
        // it. Set to the readiness bound rather than to something
        // arbitrary so the value in the struct is the one bound that
        // genuinely applies to starting this command.
        timeout: PORT_FORWARD_READY_TIMEOUT,
        // Ignored by `ProcessSpawner::spawn`, exactly as `timeout` is:
        // a long-lived child's capture is bounded by
        // `MAX_CAPTURED_STREAM_BYTES` and is a diagnostic tail by
        // design. `None` rather than a path, so the field states the
        // truth rather than implying a spill that will never happen.
        spill_dir: None,
    }
}

/// Parses one line of `kubectl port-forward` stdout into the local
/// address it announced, or `None` if the line is not such an
/// announcement.
///
/// `kubectl` writes exactly one line per bound address, in the form
///
/// ```text
/// Forwarding from 127.0.0.1:41235 -> 8080
/// Forwarding from [::1]:41235 -> 8080
/// ```
///
/// Both forms are parsed, even though v1 only ever asks for the first
/// (see this module's documentation), and both are covered by
/// `tests/port_forward_unit.rs`.
///
/// The IPv4 form above is not a guess about `kubectl`'s output: it was
/// captured verbatim from this exact argv against a real `kind` cluster
/// while writing this module (`Forwarding from 127.0.0.1:43427 -> 8080`,
/// one line, no others, with `--address 127.0.0.1`). Note that the
/// number after the arrow is the pod's `targetPort`, not the `Service`
/// port this crate asked for — which is precisely why this parser reads
/// only the address *before* the arrow and ignores everything after it.
///
/// Deliberately strict about the *shape* and indifferent to the rest of
/// the line: the port number is taken from the announcement itself
/// rather than assumed, because the whole point of the `:` argv form is
/// that this crate does not know it in advance. Anything that does not
/// parse as a socket address returns `None` and is treated by
/// [`await_forwarding_address`] as an uninteresting line rather than as
/// a fabricated port (Global Constraint 15).
#[must_use]
pub fn parse_forwarding_line(line: &str) -> Option<SocketAddr> {
    let rest = line.trim().strip_prefix(FORWARDING_PREFIX)?;
    let (local, _remote) = rest.split_once(FORWARDING_ARROW)?;
    local.trim().parse::<SocketAddr>().ok()
}

/// The part of a running `kubectl` [`await_forwarding_address`] needs.
///
/// A trait rather than a concrete `ManagedChild` for the reason
/// [`crate::reconcile::GatewayStatusSource`] is one: the readiness rule
/// — which lines are ignored, what a closed stdout means, what happens
/// when the window elapses — is the part worth testing exhaustively, and
/// it can only be tested exhaustively against scripted output. A real
/// `ManagedChild` can only be obtained by spawning a real process, so
/// without this seam every one of those cases would need a real
/// `kubectl` and a real cluster.
#[async_trait]
pub trait PortForwardOutput: Send {
    /// The child's next line of stdout, or `Ok(None)` if there will
    /// never be another (it closed stdout, in practice by exiting).
    ///
    /// # Errors
    ///
    /// `admissionlab_core::ProcessError::OutputTimedOut` if `timeout`
    /// elapses with no line. The child is left running.
    async fn next_line(&mut self, timeout: Duration) -> Result<Option<String>, ProcessError>;

    /// Everything the child wrote to stderr, as text.
    ///
    /// Called only on a failure path, after the child has been waited on
    /// or killed, so the capture is complete.
    async fn stderr_text(&mut self) -> String;

    /// Waits for the child to exit and describes how, for a diagnostic.
    /// For example `"exited with status 1"`.
    async fn exit_description(&mut self) -> String;

    /// Kills the child. Best-effort: the caller is already reporting a
    /// failure and has nothing better to do with a second one.
    async fn terminate(&mut self);
}

/// Starts a port-forward to `endpoint` and waits for it to become
/// usable.
///
/// # Errors
///
/// Returns [`GatewayError::PortForwardUnavailable`] if `kubectl` could
/// not be spawned at all, and [`GatewayError::PortForwardFailed`] if it
/// started but never announced a local port — because it exited, because
/// [`PORT_FORWARD_READY_TIMEOUT`] elapsed, or because its stdout ended
/// without a parseable announcement. In every failure case the child has
/// been terminated before this returns: a `kubectl` that could not be
/// used is never left running.
pub async fn start_service_port_forward(
    runner: &dyn ProcessSpawner,
    cluster: &ClusterHandle,
    endpoint: &GatewayEndpoint,
) -> Result<PortForwardHandle, GatewayError> {
    let spec = port_forward_command(&cluster.kubeconfig, endpoint);
    let mut child =
        runner
            .spawn(spec)
            .await
            .map_err(|source| GatewayError::PortForwardUnavailable {
                cluster: cluster.spec.name.clone(),
                endpoint: endpoint.to_string(),
                reason: source.to_string(),
            })?;

    let described = endpoint.to_string();
    let local_addr = await_forwarding_address(&mut child, &cluster.spec.name, &described).await?;
    Ok(PortForwardHandle {
        local_addr,
        child,
        cluster: cluster.spec.name.clone(),
        endpoint: described,
    })
}

/// Reads `output` until it announces a local address, or fails.
///
/// The readiness rule, in full:
///
/// - A line that [`parse_forwarding_line`] understands ends the wait
///   successfully. The **first** such line wins, which is unambiguous
///   for v1 (one `--address`, so one announcement); a future fixture
///   binding several would need to say which one it meant rather than
///   inheriting this default silently.
/// - Any other line is ignored and the wait continues. `kubectl` is free
///   to print warnings, deprecation notices, or `Handling connection
///   for ...` lines, and treating an unrecognized line as a failure
///   would make this fragile against output this crate does not control.
/// - `Ok(None)` — stdout closed — means the process is gone (or has
///   given up on stdout) without ever announcing a port. That is a
///   failure carrying `kubectl`'s own stderr, which is where the real
///   reason lives.
/// - An elapsed [`PORT_FORWARD_READY_TIMEOUT`] is a failure *and* kills
///   the child: unlike `ManagedChild::next_stdout_line`, which
///   deliberately leaves that decision to its caller, this function is
///   that caller and it knows the answer — a forward that never became
///   usable has no reason to keep running.
///
/// `cluster_name` and `endpoint` are used only to label errors.
///
/// # Errors
///
/// Returns [`GatewayError::PortForwardFailed`] in each of the failure
/// cases above.
pub async fn await_forwarding_address(
    output: &mut dyn PortForwardOutput,
    cluster_name: &str,
    endpoint: &str,
) -> Result<SocketAddr, GatewayError> {
    let deadline = tokio::time::Instant::now() + PORT_FORWARD_READY_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let line = match output.next_line(remaining).await {
            Ok(Some(line)) => line,
            Ok(None) => {
                let reason = format!(
                    "kubectl closed its stdout without announcing a local port ({})",
                    output.exit_description().await
                );
                return Err(failure(output, cluster_name, endpoint, reason).await);
            }
            Err(source) => {
                output.terminate().await;
                let reason = format!(
                    "no local port was announced within {PORT_FORWARD_READY_TIMEOUT:?} ({source})"
                );
                return Err(failure(output, cluster_name, endpoint, reason).await);
            }
        };
        if let Some(local_addr) = parse_forwarding_line(&line) {
            return Ok(local_addr);
        }
    }
}

/// Builds a [`GatewayError::PortForwardFailed`], attaching whatever
/// `kubectl` wrote to stderr.
async fn failure(
    output: &mut dyn PortForwardOutput,
    cluster_name: &str,
    endpoint: &str,
    reason: String,
) -> GatewayError {
    GatewayError::PortForwardFailed {
        cluster: cluster_name.to_owned(),
        endpoint: endpoint.to_owned(),
        reason,
        stderr: output.stderr_text().await,
    }
}

#[async_trait]
impl PortForwardOutput for ManagedChild {
    async fn next_line(&mut self, timeout: Duration) -> Result<Option<String>, ProcessError> {
        self.next_stdout_line(timeout).await
    }

    async fn stderr_text(&mut self) -> String {
        // A bounded tail (Task 9.4 Step 3), lossily decoded. The
        // capture is already capped at `MAX_CAPTURED_STREAM_BYTES`, but
        // this string ends up inside a `GatewayError`'s own `Display`
        // and from there in a report, so it is bounded again to
        // `MAX_ERROR_TAIL_BYTES` -- the tail, because `kubectl` says why
        // it failed last. Decoded lossily for the reason
        // `admissionlab_echo::echo` documents for header values: a byte
        // sequence that is not UTF-8 is still evidence that the tool
        // said something, and dropping the whole message because of one
        // byte would destroy it.
        let text = admissionlab_core::output_tail(&self.captured_stderr());
        if self.stderr_truncated() {
            format!("{text}\n[stderr truncated]")
        } else {
            text
        }
    }

    async fn exit_description(&mut self) -> String {
        match self.wait().await {
            Ok(status) => match status.code() {
                Some(code) => format!("kubectl exited with status {code}"),
                // Killed by a signal: no exit code exists, and inventing
                // one would misreport how it died.
                None => format!("kubectl was terminated by a signal ({status})"),
            },
            Err(source) => format!("kubectl's exit status could not be read: {source}"),
        }
    }

    async fn terminate(&mut self) {
        if let Err(source) = self.kill().await {
            // The same remedy string `ManagedChild`'s own leak warning
            // prints, from the same function, so an operator who sees
            // both is told to run one command rather than two different
            // ones (Task 9.4: on Unix that command terminates the
            // child's whole process group, not just the child).
            let remedy = admissionlab_core::manual_termination_command(self.id);
            tracing::warn!(
                pid = self.id,
                error = %source,
                "could not kill the port-forward process; if pid {} is still running, stop it \
                 with: {remedy}",
                self.id,
            );
        }
    }
}
