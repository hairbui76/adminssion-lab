//! ROADMAP Task 6.4: the reconciliation waiter.
//!
//! Two seams, two kinds of test.
//!
//! Most tests drive
//! [`admissionlab_gateway::wait_for_route_reconciliation_with_source`]
//! through [`ScriptedSource`], which hands back a different status on
//! each poll. That is the only way to state the interesting properties
//! at all -- a transient flip damped by the two-poll rule, a status that
//! is stable but stale and so never converges, a route that never
//! settles before its deadline -- because every one of them is a
//! statement about a *sequence* of observations, not about a single
//! response.
//!
//! One test drives
//! [`admissionlab_gateway::wait_for_route_reconciliation_with_client`]
//! against a `tower_test::mock`-backed `kube::Client`, so the wire
//! format is asserted against the real
//! [`admissionlab_gateway::KubeGatewayStatusSource`] and not only
//! against the scripted stand-in: the exact request paths, the
//! `GatewayClass` being read only because the `Gateway` named one, and a
//! 404 being retried rather than failing.
//!
//! Every status these tests use is either a checked-in golden from
//! `testdata/objects/gateway-status/` or that golden with one field
//! changed, so the shapes stay the realistic ones Task 6.3's fixtures
//! document the provenance of.
//!
//! These tests run in real time. The convergence rule is stated in
//! wall-clock terms ("at least 250ms apart"), so a converging test takes
//! at least that long by construction -- deliberately not faked with a
//! paused clock, since the thing under test *is* the timing.

use std::path::PathBuf;
use std::pin::pin;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use admissionlab_gateway::conditions::{CONDITION_ACCEPTED, CONDITION_PROGRAMMED, ConditionState};
use admissionlab_gateway::{
    DIAGNOSTIC_GATEWAY_CLASS_ABSENT, DIAGNOSTIC_PARENT_ABSENT, DIAGNOSTIC_PARENT_AMBIGUOUS,
    DIAGNOSTIC_STALE_STATUS, DIAGNOSTIC_TIMEOUT, GatewayError, GatewayStatusSource,
    ReconciliationEvidence, RouteContract, STABILITY_INTERVAL,
    wait_for_route_reconciliation_with_client, wait_for_route_reconciliation_with_source,
};
use async_trait::async_trait;
use http::{Request, Response};
use kube::client::Body;
use tower_test::mock;

/// Loads one golden status object from Task 6.3's
/// `testdata/objects/gateway-status/`.
fn golden(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/objects/gateway-status")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read golden {}: {error}", path.display()));
    serde_norway::from_str(&text)
        .unwrap_or_else(|error| panic!("parse golden {}: {error}", path.display()))
}

/// The contract every test uses: `gateway-lab/echo-a` attached to
/// `gateway-lab/lab-gateway`'s `http` listener.
fn contract() -> RouteContract {
    RouteContract {
        id: "echo-a-root".to_string(),
        gateway_namespace: "gateway-lab".to_string(),
        gateway_name: "lab-gateway".to_string(),
        route_namespace: "gateway-lab".to_string(),
        route_name: "echo-a".to_string(),
        listener_name: Some("http".to_string()),
        probes: Vec::new(),
    }
}

/// What one scripted poll hands back.
#[derive(Clone)]
struct Poll {
    gateway: Option<serde_json::Value>,
    gateway_class: Option<serde_json::Value>,
    route: Option<serde_json::Value>,
}

impl Poll {
    /// A fully converged poll: everything `True` and current.
    fn converged() -> Self {
        Self {
            gateway: Some(golden("gateway-programmed.yaml")),
            gateway_class: Some(golden("gatewayclass-accepted.yaml")),
            route: Some(golden("httproute-accepted.yaml")),
        }
    }

    /// The same, with the route status replaced.
    fn with_route(mut self, route: serde_json::Value) -> Self {
        self.route = Some(route);
        self
    }

    /// The same, with the Gateway status replaced.
    fn with_gateway(mut self, gateway: serde_json::Value) -> Self {
        self.gateway = Some(gateway);
        self
    }
}

/// A [`GatewayStatusSource`] that returns a scripted sequence, one
/// entry per *route* read (which is the first read of every poll), and
/// repeats the last entry forever once the script runs out.
///
/// Repeating rather than panicking is deliberate: a test about a timeout
/// cannot know in advance how many polls fit in its deadline, and
/// pinning that number would make the test depend on machine speed
/// rather than on the rule.
struct ScriptedSource {
    script: Vec<Poll>,
    polls: Mutex<usize>,
    /// Owned rather than a literal returned from `cluster_name`: the
    /// trait's `-> &str` is tied to `&self`, so an impl cannot narrow it
    /// to `&'static str`.
    cluster_name: String,
}

impl ScriptedSource {
    fn new(script: Vec<Poll>) -> Self {
        Self {
            script,
            polls: Mutex::new(0),
            cluster_name: "reconcile-test-cluster".to_string(),
        }
    }

    /// The entry for the current poll, without advancing.
    fn current(&self) -> Poll {
        let index = *self.polls.lock().expect("poll counter");
        self.script[index.min(self.script.len() - 1)].clone()
    }

    /// How many polls have completed.
    fn poll_count(&self) -> usize {
        *self.polls.lock().expect("poll counter")
    }
}

#[async_trait]
impl GatewayStatusSource for ScriptedSource {
    fn cluster_name(&self) -> &str {
        &self.cluster_name
    }

    async fn get_route(
        &self,
        _namespace: &str,
        _name: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError> {
        let poll = self.current();
        // The route is read first in every poll, so advancing here is
        // what makes one script entry equal one poll.
        *self.polls.lock().expect("poll counter") += 1;
        Ok(poll.route)
    }

    async fn get_gateway(
        &self,
        _namespace: &str,
        _name: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError> {
        // `current()` after the route read already advanced the counter,
        // so step back one to stay within the same poll.
        let index = self.poll_count().saturating_sub(1);
        Ok(self.script[index.min(self.script.len() - 1)]
            .clone()
            .gateway)
    }

    async fn get_gateway_class(
        &self,
        _name: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError> {
        let index = self.poll_count().saturating_sub(1);
        Ok(self.script[index.min(self.script.len() - 1)]
            .clone()
            .gateway_class)
    }
}

/// Runs the waiter over `script` with a deadline `timeout` from now.
async fn wait(
    script: Vec<Poll>,
    timeout: Duration,
) -> Result<ReconciliationEvidence, GatewayError> {
    let source = ScriptedSource::new(script);
    wait_for_route_reconciliation_with_source(&source, &contract(), Instant::now() + timeout).await
}

/// The diagnostic codes on some evidence, in order.
fn codes(evidence: &ReconciliationEvidence) -> Vec<&str> {
    evidence
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect()
}

// =========================================================================
// The convergence rule
// =========================================================================

#[tokio::test]
async fn a_stable_status_converges_after_two_polls_at_least_250ms_apart() {
    let started = Instant::now();
    let evidence = wait(vec![Poll::converged()], Duration::from_secs(10))
        .await
        .expect("both objects exist");

    assert!(evidence.converged);
    assert!(
        evidence.diagnostics.is_empty(),
        "a clean convergence has nothing to report, got {:?}",
        codes(&evidence)
    );
    assert!(
        started.elapsed() >= STABILITY_INTERVAL,
        "convergence must not be declared on a single poll, nor on two polls closer together \
         than {STABILITY_INTERVAL:?}"
    );

    // And the evidence is the real observation, not a summary.
    assert_eq!(
        evidence.gateway.identity.to_string(),
        "gateway-lab/lab-gateway"
    );
    assert_eq!(
        evidence.gateway.condition(CONDITION_PROGRAMMED).state,
        ConditionState::True
    );
    assert_eq!(
        evidence
            .gateway_class
            .as_ref()
            .expect("the Gateway names a class, and it exists")
            .name,
        "istio"
    );
    assert_eq!(evidence.route.name, "echo-a");
}

#[tokio::test]
async fn one_transient_status_update_is_damped_rather_than_taken_as_the_answer() {
    // Poll 1 sees `Programmed: Unknown / Pending` -- a perfectly normal
    // mid-reconcile state. Polls 2 and 3 see the settled status. Without
    // the two-poll rule, poll 2 alone would decide; with it, the answer
    // is the one poll 3 confirms.
    //
    // If the rule were dropped, this test would still pass on the value
    // but `poll_count` would be 2, which is why the count is asserted.
    let source = ScriptedSource::new(vec![
        Poll::converged().with_gateway(golden("gateway-unknown-programmed.yaml")),
        Poll::converged(),
    ]);
    let started = Instant::now();

    let evidence = wait_for_route_reconciliation_with_source(
        &source,
        &contract(),
        Instant::now() + Duration::from_secs(10),
    )
    .await
    .expect("both objects exist");

    assert!(evidence.converged);
    assert_eq!(
        source.poll_count(),
        3,
        "the transient first poll must not count towards stability, so convergence needs a \
         third poll"
    );
    assert!(started.elapsed() >= STABILITY_INTERVAL);
    assert_eq!(
        evidence.gateway.condition(CONDITION_PROGRAMMED).state,
        ConditionState::True,
        "the settled value wins, not the transient Unknown"
    );
}

#[tokio::test]
async fn a_settled_false_converges_and_is_not_called_a_regression() {
    // A route the implementation definitively rejected has *finished*
    // reconciling. Reporting `converged: false` here would conflate "the
    // controller has not decided" with "the controller decided no", and
    // reporting a regression would usurp Task 6.9's comparator, which is
    // the only thing that sees both sides.
    let evidence = wait(
        vec![Poll::converged().with_gateway(golden("gateway-not-programmed.yaml"))],
        Duration::from_secs(10),
    )
    .await
    .expect("both objects exist");

    assert!(
        evidence.converged,
        "a settled False is a settled verdict; convergence is about stability, not success"
    );
    assert_eq!(
        evidence.gateway.condition(CONDITION_PROGRAMMED).state,
        ConditionState::False
    );
    assert_eq!(
        evidence
            .gateway
            .condition(CONDITION_PROGRAMMED)
            .reason
            .as_deref(),
        Some("AddressNotAssigned"),
        "the reason survives into the evidence for the comparator to use"
    );
    assert!(
        evidence.diagnostics.is_empty(),
        "nothing here is this module's business to flag, got {:?}",
        codes(&evidence)
    );
}

#[tokio::test]
async fn a_stale_status_never_converges_however_stable_it_is() {
    // `gateway-stale-status.yaml` is generation 3 with an all-`True`
    // status published for generation 2. It is perfectly stable, so a
    // waiter that only compared consecutive polls would converge on the
    // second one and report a fully programmed Gateway.
    let started = Instant::now();
    let evidence = wait(
        vec![Poll::converged().with_gateway(golden("gateway-stale-status.yaml"))],
        Duration::from_millis(700),
    )
    .await
    .expect("both objects exist");

    assert!(
        !evidence.converged,
        "a status describing an older generation is not a current status, no matter how stable"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(700),
        "it waited out the deadline"
    );
    assert!(
        codes(&evidence).contains(&DIAGNOSTIC_STALE_STATUS),
        "the reason it did not converge must be stated, got {:?}",
        codes(&evidence)
    );
    assert!(codes(&evidence).contains(&DIAGNOSTIC_TIMEOUT));
}

#[tokio::test]
async fn a_missing_required_condition_never_converges() {
    // `Missing` is not a settled True/False, so the rule's "required
    // positive conditions are present" clause fails. This is the
    // distinction that stops a route the controller has not read yet
    // from being reported as reconciled.
    let evidence = wait(
        vec![Poll::converged().with_gateway(golden("gateway-missing-programmed.yaml"))],
        Duration::from_millis(600),
    )
    .await
    .expect("both objects exist");

    assert!(!evidence.converged);
    assert_eq!(
        evidence.gateway.condition(CONDITION_PROGRAMMED).state,
        ConditionState::Missing,
        "and the evidence says exactly which condition was absent"
    );
    assert_eq!(
        evidence.gateway.condition(CONDITION_ACCEPTED).state,
        ConditionState::True
    );
}

#[tokio::test]
async fn a_never_reconciled_route_times_out_with_evidence_not_an_error() {
    // Step 4. The route exists but has no status at all -- what a
    // freshly applied route looks like when nothing ever picks it up.
    let evidence = wait(
        vec![Poll::converged().with_route(golden("httproute-no-status.yaml"))],
        Duration::from_millis(600),
    )
    .await
    .expect("a timeout is Ok with evidence, never an Err");

    assert!(!evidence.converged);
    assert!(evidence.elapsed >= Duration::from_millis(600));
    assert!(evidence.route.parents.is_empty());

    assert_eq!(
        codes(&evidence),
        [DIAGNOSTIC_PARENT_ABSENT, DIAGNOSTIC_TIMEOUT],
        "the evidence says both that no parent entry matched and that the deadline passed"
    );
    let timeout = evidence
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == DIAGNOSTIC_TIMEOUT)
        .expect("timeout diagnostic");
    assert!(
        timeout.message.contains("not as a regression"),
        "the diagnostic must not let a reader mistake a timeout for a verdict, got {:?}",
        timeout.message
    );
}

#[tokio::test]
async fn an_ambiguous_parent_never_converges_and_says_why() {
    // Two matching `status.parents` entries that disagree. Picking one
    // would make the answer depend on list order.
    let ambiguous = RouteContract {
        route_name: "echo-b".to_string(),
        listener_name: None,
        ..contract()
    };
    let source = ScriptedSource::new(vec![
        Poll::converged().with_route(golden("httproute-two-parents.yaml")),
    ]);

    let evidence = wait_for_route_reconciliation_with_source(
        &source,
        &ambiguous,
        Instant::now() + Duration::from_millis(600),
    )
    .await
    .expect("both objects exist");

    assert!(!evidence.converged);
    assert_eq!(
        codes(&evidence),
        [DIAGNOSTIC_PARENT_AMBIGUOUS, DIAGNOSTIC_TIMEOUT]
    );
    let ambiguity = &evidence.diagnostics[0];
    assert!(
        ambiguity.message.contains("set listenerName"),
        "the diagnostic must say how to fix it, got {:?}",
        ambiguity.message
    );
}

#[tokio::test]
async fn a_gateway_naming_a_class_that_does_not_exist_never_converges() {
    // The Gateway itself declared the class as a prerequisite. Ignoring
    // its absence would let a route "converge" against a Gateway nothing
    // will ever program.
    let mut poll = Poll::converged();
    poll.gateway_class = None;

    let evidence = wait(vec![poll], Duration::from_millis(600))
        .await
        .expect("the Gateway and route both exist");

    assert!(!evidence.converged);
    assert!(evidence.gateway_class.is_none());
    assert_eq!(
        codes(&evidence),
        [DIAGNOSTIC_GATEWAY_CLASS_ABSENT, DIAGNOSTIC_TIMEOUT],
        "`gateway_class: None` alone cannot distinguish \"named none\" from \"named a missing \
         one\"; the diagnostic is what does"
    );
}

#[tokio::test]
async fn a_gateway_that_never_exists_is_an_error_not_fabricated_evidence() {
    // `ReconciliationEvidence.gateway` is not optional, so there is
    // nothing honest to put there. Inventing an empty GatewayEvidence
    // with a made-up generation is what Global Constraint 15 forbids.
    let mut poll = Poll::converged();
    poll.gateway = None;
    poll.gateway_class = None;

    match wait(vec![poll], Duration::from_millis(400))
        .await
        .expect_err("an object that never existed must not be fabricated")
    {
        GatewayError::ObjectAbsent { cluster, object } => {
            assert_eq!(cluster, "reconcile-test-cluster");
            assert_eq!(object, "Gateway gateway-lab/lab-gateway");
        }
        other => panic!("expected ObjectAbsent, got {other:?}"),
    }
}

#[tokio::test]
async fn evidence_serializes_elapsed_as_milliseconds() {
    let evidence = wait(vec![Poll::converged()], Duration::from_secs(10))
        .await
        .expect("converges");
    let json: serde_json::Value =
        serde_json::to_value(&evidence).expect("evidence serializes outward");

    assert!(
        json["elapsed"].is_u64(),
        "elapsed must be a plain integer number of milliseconds, matching every other duration \
         this project serializes; got {}",
        json["elapsed"]
    );
    assert_eq!(json["converged"], serde_json::json!(true));
}

// =========================================================================
// The real kube-backed source
// =========================================================================

#[tokio::test]
async fn the_kube_source_reads_the_right_paths_and_retries_a_404() {
    // Drives the production `KubeGatewayStatusSource` for real, so the
    // request paths and the Gateway-API plurals are asserted against the
    // wire and not against the scripted stand-in.
    //
    // Poll 1: the route 404s (it was applied a moment ago and this
    // apiserver read has not caught up), and the Gateway is read anyway.
    // Polls 2 and 3: everything is there and converged.
    let (mock_service, handle) = mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(mock_service, "gateway-lab");

    let responder = tokio::spawn(async move {
        let mut handle = pin!(handle);
        let mut paths = Vec::new();
        // Three reads per poll (route, Gateway, GatewayClass), three
        // polls: the 404 poll, the first poll that sees a settled
        // status, and the poll that confirms it 250ms later.
        for index in 0..9 {
            let (request, send) = handle.next_request().await.expect("a request");
            let path = request.uri().path().to_string();
            let response = if index == 0 {
                Response::builder()
                    .status(404)
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "kind": "Status",
                            "apiVersion": "v1",
                            "status": "Failure",
                            "message": "httproutes.gateway.networking.k8s.io \"echo-a\" not found",
                            "reason": "NotFound",
                            "code": 404,
                        }))
                        .expect("serialize status"),
                    ))
                    .expect("build response")
            } else {
                let golden_name = if path.contains("/httproutes/") {
                    "httproute-accepted.yaml"
                } else if path.contains("/gateways/") {
                    "gateway-programmed.yaml"
                } else {
                    "gatewayclass-accepted.yaml"
                };
                Response::new(Body::from(
                    serde_json::to_vec(&golden(golden_name)).expect("serialize golden"),
                ))
            };
            send.send_response(response);
            paths.push(path);
        }
        paths
    });

    let evidence = wait_for_route_reconciliation_with_client(
        client,
        "kube-source-test-cluster",
        &contract(),
        Instant::now() + Duration::from_secs(10),
    )
    .await
    .expect("the route converges once it appears");
    let paths = responder.await.expect("mock responder must not panic");

    assert!(evidence.converged, "a transient 404 is retried, not fatal");
    assert_eq!(
        paths,
        [
            // Poll 1: the route 404s, and the Gateway (and, because the
            // Gateway names one, the GatewayClass) are still read -- the
            // waiter does not abandon a poll because one object is not
            // there yet.
            "/apis/gateway.networking.k8s.io/v1/namespaces/gateway-lab/httproutes/echo-a",
            "/apis/gateway.networking.k8s.io/v1/namespaces/gateway-lab/gateways/lab-gateway",
            "/apis/gateway.networking.k8s.io/v1/gatewayclasses/istio",
            // Poll 2: the first settled observation.
            "/apis/gateway.networking.k8s.io/v1/namespaces/gateway-lab/httproutes/echo-a",
            "/apis/gateway.networking.k8s.io/v1/namespaces/gateway-lab/gateways/lab-gateway",
            "/apis/gateway.networking.k8s.io/v1/gatewayclasses/istio",
            // Poll 3, at least 250ms later: the one that confirms it.
            "/apis/gateway.networking.k8s.io/v1/namespaces/gateway-lab/httproutes/echo-a",
            "/apis/gateway.networking.k8s.io/v1/namespaces/gateway-lab/gateways/lab-gateway",
            "/apis/gateway.networking.k8s.io/v1/gatewayclasses/istio",
        ],
        "the plurals are Gateway API's own (gateways, httproutes, gatewayclasses), a \
         GatewayClass is cluster-scoped so its path carries no /namespaces/ segment, and no \
         discovery handshake is issued at all"
    );
}
