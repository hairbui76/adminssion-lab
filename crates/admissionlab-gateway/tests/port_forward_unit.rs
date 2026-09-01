//! ROADMAP Task 6.7: the managed `kubectl port-forward`.
//!
//! Three separable things, tested three ways, none of which needs a
//! cluster or a real `kubectl`:
//!
//! - **The argv** ([`admissionlab_gateway::port_forward_command`]) is
//!   asserted element by element. Argv *is* the safety surface here: no
//!   shell, no concatenation, an unconditional `--kubeconfig` pointing
//!   at the cluster's own isolated file, and loopback-only binding. A
//!   test that inferred these from a process's behavior would prove much
//!   less than one that reads the vector.
//! - **The parser** ([`admissionlab_gateway::parse_forwarding_line`]) is
//!   a pure function over one line of `kubectl` output, including the
//!   bracketed IPv6 form v1 never asks for but must not mis-parse.
//! - **The readiness rule**
//!   ([`admissionlab_gateway::await_forwarding_address`]) is driven
//!   through the [`PortForwardOutput`] seam against scripted output, so
//!   every branch -- a noisy prefix, a premature exit, an elapsed window,
//!   a stdout that just ends -- is exercised, which no amount of real
//!   `kubectl` could do reliably.
//!
//! **Not covered here, deliberately:** that a real `kubectl
//! port-forward` against a real cluster prints the line this parser
//! expects, and that traffic actually flows through it. Both are
//! live-cluster facts, scoped to the Phase 6 exit gate the same way
//! `tests/apply_unit.rs` scopes out server-side apply's real behavior.
//! The *format* claim is the one thing this file cannot prove on its
//! own, which is why the parser is deliberately indifferent to
//! everything in the line except the shape it needs.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use admissionlab_core::{
    ClusterHandle, ClusterSpec, CommandSpec, ProcessError, ProcessSpawner, Side,
};
use admissionlab_gateway::{
    GatewayEndpoint, GatewayError, KUBECTL_PROGRAM, LOCAL_ADDRESS, PORT_FORWARD_READY_TIMEOUT,
    PortForwardOutput, await_forwarding_address, parse_forwarding_line, port_forward_command,
    start_service_port_forward,
};
use async_trait::async_trait;

const CLUSTER: &str = "port-forward-test-cluster";

fn endpoint() -> GatewayEndpoint {
    GatewayEndpoint {
        namespace: "gateway-lab".to_owned(),
        service: "lab-gateway-istio".to_owned(),
        port: 80,
    }
}

// =========================================================================
// The argv (Step 1)
// =========================================================================

#[test]
fn the_argv_is_exactly_the_roadmaps_own() {
    let kubeconfig = Path::new("/run/admissionlab/baseline.kubeconfig");
    let spec = port_forward_command(kubeconfig, &endpoint());

    assert_eq!(spec.program, OsString::from(KUBECTL_PROGRAM));
    assert_eq!(
        spec.args,
        vec![
            OsString::from("--kubeconfig"),
            OsString::from(kubeconfig),
            OsString::from("-n"),
            OsString::from("gateway-lab"),
            OsString::from("port-forward"),
            OsString::from("service/lab-gateway-istio"),
            // The leading colon is the whole point: an empty local port
            // asks the OS to choose one.
            OsString::from(":80"),
            OsString::from("--address"),
            OsString::from(LOCAL_ADDRESS),
        ],
        "the argv must match ROADMAP Task 6.7 Step 1 element for element"
    );
}

#[test]
fn the_argv_carries_no_environment_and_no_working_directory() {
    let spec = port_forward_command(Path::new("/tmp/kubeconfig"), &endpoint());
    assert!(
        spec.env.is_empty(),
        "a cluster is never selected through KUBECONFIG in the environment -- only through the \
         explicit --kubeconfig flag above"
    );
    assert!(spec.sensitive_env_keys.is_empty());
    assert_eq!(spec.cwd, None);
}

#[test]
fn every_argument_is_its_own_argv_element_even_with_shell_metacharacters() {
    // Global Constraint 12: nothing here is ever concatenated into a
    // string a shell could interpret. A Service name is API-server
    // validated in practice, so this is defence in depth -- but a
    // regression that started building `sh -c "..."` would be invisible
    // without it.
    let hostile = GatewayEndpoint {
        namespace: "ns; rm -rf /".to_owned(),
        service: "svc $(touch /tmp/pwned)".to_owned(),
        port: 8080,
    };
    let spec = port_forward_command(Path::new("/tmp/kubeconfig"), &hostile);
    assert!(spec.args.contains(&OsString::from("ns; rm -rf /")));
    assert!(
        spec.args
            .contains(&OsString::from("service/svc $(touch /tmp/pwned)"))
    );
    assert_eq!(spec.args.len(), 9, "no argument was split or merged");
}

#[test]
fn the_local_address_is_loopback_only() {
    assert_eq!(
        LOCAL_ADDRESS, "127.0.0.1",
        "v1 binds loopback only: a forward into a disposable cluster must never be reachable \
         from the network the developer's machine is on"
    );
}

// =========================================================================
// The parser (Steps 2 and 5)
// =========================================================================

#[test]
fn the_ipv4_announcement_is_parsed() {
    assert_eq!(
        parse_forwarding_line("Forwarding from 127.0.0.1:41235 -> 8080"),
        Some("127.0.0.1:41235".parse::<SocketAddr>().expect("valid"))
    );
}

/// v1 asks for `127.0.0.1` only, so `kubectl` never prints this form
/// today -- but it does print it when asked for `::1`, and a parser that
/// mangled the brackets would silently produce the wrong port the first
/// time a fixture needed one.
#[test]
fn the_ipv6_announcement_is_parsed() {
    assert_eq!(
        parse_forwarding_line("Forwarding from [::1]:41235 -> 8080"),
        Some("[::1]:41235".parse::<SocketAddr>().expect("valid"))
    );
}

#[test]
fn surrounding_whitespace_does_not_defeat_the_parser() {
    assert_eq!(
        parse_forwarding_line("  Forwarding from 127.0.0.1:1 -> 80\r\n"),
        Some("127.0.0.1:1".parse::<SocketAddr>().expect("valid"))
    );
}

#[test]
fn anything_that_is_not_an_announcement_is_none_rather_than_a_guess() {
    for line in [
        "",
        "Handling connection for 41235",
        // The real shape of kubectl's most common failure.
        "error: unable to listen on any of the requested ports: [{80 41235}]",
        // Announcement-shaped but with no parseable address: reported as
        // "not an announcement", never as a fabricated port.
        "Forwarding from not-an-address -> 8080",
        "Forwarding from 127.0.0.1 -> 8080",
        "Forwarding from 127.0.0.1:notaport -> 8080",
        // No arrow at all.
        "Forwarding from 127.0.0.1:41235",
        "forwarding from 127.0.0.1:41235 -> 8080",
    ] {
        assert_eq!(
            parse_forwarding_line(line),
            None,
            "{line:?} must not parse as an announcement"
        );
    }
}

// =========================================================================
// The readiness rule (Steps 2 and 3)
// =========================================================================

/// A scripted [`PortForwardOutput`]: a queue of stdout answers, a fixed
/// stderr capture, and a record of whether the child was terminated.
struct ScriptedOutput {
    /// Each entry is one answer from `next_line`, in order: a line, an
    /// end-of-stdout, or an elapsed readiness window.
    answers: std::collections::VecDeque<Answer>,
    stderr: String,
    exit_description: String,
    terminated: bool,
    waited: bool,
}

enum Answer {
    Line(&'static str),
    Eof,
    Timeout,
}

impl ScriptedOutput {
    fn new(answers: Vec<Answer>, stderr: &str) -> Self {
        Self {
            answers: answers.into(),
            stderr: stderr.to_owned(),
            exit_description: "kubectl exited with status 1".to_owned(),
            terminated: false,
            waited: false,
        }
    }
}

#[async_trait]
impl PortForwardOutput for ScriptedOutput {
    async fn next_line(&mut self, _timeout: Duration) -> Result<Option<String>, ProcessError> {
        match self.answers.pop_front() {
            Some(Answer::Line(line)) => Ok(Some(line.to_owned())),
            Some(Answer::Eof) | None => Ok(None),
            Some(Answer::Timeout) => Err(ProcessError::Spawn {
                // Any `ProcessError` exercises the same branch; the
                // production path produces `OutputTimedOut`, which
                // carries a `CommandContext` this test has no reason to
                // build.
                context: Box::new(
                    port_forward_command(Path::new("/tmp/kubeconfig"), &endpoint()).context(),
                ),
                source: std::io::Error::other("scripted readiness-window timeout"),
            }),
        }
    }

    async fn stderr_text(&mut self) -> String {
        self.stderr.clone()
    }

    async fn exit_description(&mut self) -> String {
        self.waited = true;
        self.exit_description.clone()
    }

    async fn terminate(&mut self) {
        self.terminated = true;
    }
}

#[tokio::test]
async fn the_first_announcement_ends_the_wait_and_noise_before_it_is_ignored() {
    // kubectl is free to print things this crate does not control; only
    // an unrecognized line that is *also* the last one is a failure.
    let mut output = ScriptedOutput::new(
        vec![
            Answer::Line("W0101 12:00:00.000000   1 warnings.go:70] some deprecation notice"),
            Answer::Line("Forwarding from 127.0.0.1:41235 -> 80"),
            Answer::Line("Forwarding from [::1]:41235 -> 80"),
        ],
        "",
    );

    let addr = await_forwarding_address(&mut output, CLUSTER, "gateway-lab/lab-gateway-istio:80")
        .await
        .expect("the announcement must end the wait");
    assert_eq!(
        addr,
        "127.0.0.1:41235".parse::<SocketAddr>().expect("valid"),
        "the first announcement wins, so a second address is never silently preferred"
    );
    assert!(
        !output.terminated,
        "a forward that became usable must not be killed"
    );
}

#[tokio::test]
async fn a_premature_exit_is_a_failure_carrying_kubectls_own_stderr() {
    // The single most common real failure: the Service exists but has no
    // ready endpoints, so kubectl gives up immediately. The reason lives
    // in kubectl's stderr, and this crate must carry it verbatim rather
    // than reword it.
    let stderr = "error: unable to forward port because pod is not running. Current status=Pending";
    let mut output = ScriptedOutput::new(vec![Answer::Eof], stderr);

    let error = await_forwarding_address(&mut output, CLUSTER, "gateway-lab/lab-gateway-istio:80")
        .await
        .expect_err("a kubectl that exits before announcing a port is a failure");

    match &error {
        GatewayError::PortForwardFailed {
            cluster,
            endpoint,
            reason,
            stderr: captured,
        } => {
            assert_eq!(cluster, CLUSTER);
            assert_eq!(endpoint, "gateway-lab/lab-gateway-istio:80");
            assert!(reason.contains("closed its stdout"), "got: {reason}");
            assert!(
                reason.contains("exited with status 1"),
                "the exit status must be reported, got: {reason}"
            );
            assert_eq!(captured, stderr, "kubectl's own words, verbatim");
        }
        other => panic!("expected PortForwardFailed, got {other:?}"),
    }
    assert!(
        error.to_string().contains(stderr),
        "the rendered message must show kubectl's stderr, got: {error}"
    );
    assert!(
        output.waited,
        "the exited child must be waited on, so its status is reported rather than guessed"
    );
}

#[tokio::test]
async fn an_elapsed_readiness_window_fails_and_kills_the_child() {
    let mut output = ScriptedOutput::new(
        vec![Answer::Line("some chatter"), Answer::Timeout],
        "nothing useful",
    );

    let error = await_forwarding_address(&mut output, CLUSTER, "gateway-lab/lab-gateway-istio:80")
        .await
        .expect_err("a kubectl that never announces a port is a failure");

    assert!(
        matches!(error, GatewayError::PortForwardFailed { .. }),
        "got {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains(&format!("{PORT_FORWARD_READY_TIMEOUT:?}")),
        "the message must name the window that elapsed, got: {error}"
    );
    assert!(
        output.terminated,
        "a forward that never became usable has no reason to keep running -- unlike \
         ManagedChild::next_stdout_line, this caller knows the answer"
    );
}

#[tokio::test]
async fn a_stdout_that_ends_without_an_announcement_is_a_failure_not_a_hang() {
    let mut output = ScriptedOutput::new(vec![Answer::Line("only chatter")], "");
    let error = await_forwarding_address(&mut output, CLUSTER, "gateway-lab/lab-gateway-istio:80")
        .await
        .expect_err("chatter alone is not an announcement");
    assert!(matches!(error, GatewayError::PortForwardFailed { .. }));
}

// =========================================================================
// Spawn failure
// =========================================================================

/// A [`ProcessSpawner`] that never spawns anything, so the "kubectl is
/// not installed" path can be exercised without depending on whether
/// this machine happens to have `kubectl` on its `PATH`.
struct RefusingSpawner;

#[async_trait]
impl ProcessSpawner for RefusingSpawner {
    async fn spawn(
        &self,
        spec: CommandSpec,
    ) -> Result<admissionlab_core::ManagedChild, ProcessError> {
        Err(ProcessError::Spawn {
            context: Box::new(spec.context()),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        })
    }
}

fn cluster_handle() -> ClusterHandle {
    ClusterHandle {
        spec: ClusterSpec {
            side: Side::Baseline,
            name: CLUSTER.to_owned(),
            kubernetes_version: "1.36.0".to_owned(),
            node_image: "kindest/node:v1.36.0".to_owned(),
            images: Vec::new(),
        },
        kubeconfig: PathBuf::from("/tmp/admissionlab-port-forward-test.kubeconfig"),
        audit_log: PathBuf::from("/tmp/admissionlab-port-forward-test-audit.log"),
    }
}

#[tokio::test]
async fn a_kubectl_that_cannot_be_spawned_is_unavailable_not_failed() {
    // The Unavailable/Failed split: no `kubectl` at all is a broken
    // machine, not a broken Gateway, and the two have entirely different
    // fixes.
    let error = start_service_port_forward(&RefusingSpawner, &cluster_handle(), &endpoint())
        .await
        .expect_err("a kubectl that cannot be spawned must fail the forward");

    match error {
        GatewayError::PortForwardUnavailable {
            cluster, endpoint, ..
        } => {
            assert_eq!(cluster, CLUSTER);
            assert_eq!(endpoint, "gateway-lab/lab-gateway-istio:80");
        }
        other => panic!("expected PortForwardUnavailable, got {other:?}"),
    }
}
