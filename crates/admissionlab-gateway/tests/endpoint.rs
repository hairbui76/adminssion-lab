//! ROADMAP Task 6.6: resolving a recipe-declared
//! [`GatewayEndpointStrategy`] to one concrete
//! [`admissionlab_gateway::GatewayEndpoint`].
//!
//! Everything here drives
//! [`admissionlab_gateway::resolve_gateway_endpoint_with_client`]
//! against a `tower_test::mock`-backed `kube::Client` -- the same
//! technique `tests/apply_unit.rs` and `tests/reconcile_unit.rs` already
//! use -- so the assertions are about the *requests that actually go
//! out* (which path, for which namespace, after which substitution) and
//! about the objects that come back, never about a stand-in for either.
//!
//! Four groups, matching Task 6.6's own steps:
//!
//! - **Substitution.** `{gatewayNamespace}`/`{gatewayName}` reach the
//!   wire, and an unrecognized placeholder fails before any request is
//!   sent.
//! - **Selector matching.** Exactly one match resolves; zero and several
//!   are both errors carrying every candidate name that was considered
//!   (Task 6.6 Step 2). An exact-name lookup that 404s reports that
//!   *nothing was enumerated*, which is a different fact from "the
//!   namespace was empty".
//! - **Port resolution** (Task 6.6 Step 3), including the two decisions
//!   `admissionlab_gateway::endpoint`'s module documentation records: a
//!   bare `port` is validated against the `Service` rather than trusted,
//!   and a single-port `Service` needs no port field at all while a
//!   multi-port one does.
//! - **Not covered here, deliberately:** whether `client_for` connects
//!   using a real `kind`-produced kubeconfig, which is the same
//!   live-cluster gap `tests/apply_unit.rs` scopes out.

use std::collections::BTreeMap;
use std::pin::pin;

use admissionlab_gateway::{
    GatewayEndpoint, GatewayEndpointStrategy, GatewayError, GatewayIdentity,
    resolve_gateway_endpoint_with_client,
};
use http::{Request, Response};
use kube::client::Body;
use tower_test::mock;

const CLUSTER: &str = "gateway-endpoint-test-cluster";
const GATEWAY_NAME_LABEL: &str = "gateway.networking.k8s.io/gateway-name";

// =========================================================================
// Fixture helpers
// =========================================================================

/// The Gateway every test resolves for: `gateway-lab/lab-gateway`.
fn gateway() -> GatewayIdentity {
    GatewayIdentity {
        namespace: "gateway-lab".to_owned(),
        name: "lab-gateway".to_owned(),
    }
}

fn selector(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// The strategy `recipes/istio-gateway/recipe.yaml` (Task 6.10) is
/// expected to declare: the Gateway's own namespace, the upstream
/// well-known gateway-name label, and a named port.
fn istio_strategy(port_name: Option<&str>, port: Option<u16>) -> GatewayEndpointStrategy {
    GatewayEndpointStrategy::ServiceBySelector {
        namespace: "{gatewayNamespace}".to_owned(),
        selector: selector(&[(GATEWAY_NAME_LABEL, "{gatewayName}")]),
        port_name: port_name.map(ToOwned::to_owned),
        port,
    }
}

/// One `Service` object, as the API server would return it.
fn service(
    name: &str,
    labels: &[(&str, &str)],
    ports: &[(Option<&str>, i32)],
) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {
            "name": name,
            "namespace": "gateway-lab",
            "labels": labels
                .iter()
                .map(|(key, value)| ((*key).to_owned(), serde_json::json!(value)))
                .collect::<serde_json::Map<_, _>>(),
        },
        "spec": {
            "ports": ports
                .iter()
                .map(|(name, port)| match name {
                    Some(name) => serde_json::json!({"name": name, "port": port}),
                    None => serde_json::json!({"port": port}),
                })
                .collect::<Vec<_>>(),
        },
    })
}

/// A `ServiceList` wrapping `items`.
fn service_list(items: &[serde_json::Value]) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "ServiceList",
        "metadata": {"resourceVersion": "1"},
        "items": items,
    })
}

/// The API server's own `Status` body for a 404.
fn not_found(name: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "Status",
        "apiVersion": "v1",
        "status": "Failure",
        "message": format!("services \"{name}\" not found"),
        "reason": "NotFound",
        "code": 404,
    })
}

/// Resolves `strategy` against a mock apiserver that answers each
/// request, in order, from `responses` (an HTTP status and a body), and
/// returns the outcome alongside every request URI that actually went
/// out.
async fn resolve_against(
    strategy: &GatewayEndpointStrategy,
    responses: Vec<(u16, serde_json::Value)>,
) -> (Result<GatewayEndpoint, GatewayError>, Vec<String>) {
    let (mock_service, handle) = mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(mock_service, "gateway-lab");
    let expected = responses.len();

    let responder = tokio::spawn(async move {
        let mut handle = pin!(handle);
        let mut uris = Vec::new();
        for (status, body) in responses {
            let Some((request, send)) = handle.next_request().await else {
                break;
            };
            uris.push(request.uri().to_string());
            send.send_response(
                Response::builder()
                    .status(status)
                    .body(Body::from(
                        serde_json::to_vec(&body).expect("serialize response body"),
                    ))
                    .expect("build response"),
            );
        }
        uris
    });

    let resolved =
        resolve_gateway_endpoint_with_client(client, CLUSTER, &gateway(), strategy).await;
    let uris = responder.await.expect("mock responder must not panic");
    assert!(
        uris.len() <= expected,
        "the responder answered more requests than were scripted"
    );
    (resolved, uris)
}

// =========================================================================
// Substitution
// =========================================================================

#[tokio::test]
async fn the_gateways_namespace_and_name_reach_the_wire_substituted() {
    let (resolved, uris) = resolve_against(
        &istio_strategy(Some("http"), None),
        vec![(
            200,
            service_list(&[service(
                "lab-gateway-istio",
                &[(GATEWAY_NAME_LABEL, "lab-gateway")],
                &[(Some("status-port"), 15021), (Some("http"), 80)],
            )]),
        )],
    )
    .await;

    assert_eq!(
        resolved.expect("the Istio-shaped strategy must resolve"),
        GatewayEndpoint {
            namespace: "gateway-lab".to_owned(),
            service: "lab-gateway-istio".to_owned(),
            port: 80,
        }
    );
    assert_eq!(
        uris.len(),
        1,
        "a selector strategy lists the namespace once"
    );
    assert!(
        uris[0].starts_with("/api/v1/namespaces/gateway-lab/services"),
        "`{{gatewayNamespace}}` must have become the Gateway's own namespace on the wire, got \
         {}",
        uris[0]
    );
}

#[tokio::test]
async fn an_exact_name_strategy_reads_the_substituted_name_directly() {
    let (resolved, uris) = resolve_against(
        &GatewayEndpointStrategy::ServiceByName {
            namespace: "{gatewayNamespace}".to_owned(),
            name: "{gatewayName}-istio".to_owned(),
            port_name: None,
            port: None,
        },
        vec![(
            200,
            service("lab-gateway-istio", &[], &[(Some("http"), 80)]),
        )],
    )
    .await;

    assert_eq!(resolved.expect("must resolve").port, 80);
    assert_eq!(
        uris,
        ["/api/v1/namespaces/gateway-lab/services/lab-gateway-istio"],
        "an exact-name lookup reads one object rather than listing the namespace"
    );
}

/// The failure mode the closed placeholder vocabulary exists to prevent:
/// left literal, `{gateway}` would produce a selector matching nothing
/// and be reported as "this Gateway has no data plane".
#[tokio::test]
async fn an_unknown_placeholder_fails_before_any_request_is_sent() {
    let (resolved, uris) = resolve_against(
        &GatewayEndpointStrategy::ServiceBySelector {
            namespace: "{gatewayNamespace}".to_owned(),
            selector: selector(&[(GATEWAY_NAME_LABEL, "{gateway}")]),
            port_name: None,
            port: None,
        },
        Vec::new(),
    )
    .await;

    match resolved.expect_err("{gateway} is not a placeholder this project defines") {
        GatewayError::EndpointStrategyInvalid { gateway, reason } => {
            assert_eq!(gateway, "gateway-lab/lab-gateway");
            assert!(reason.contains("{gateway}"), "got: {reason}");
        }
        other => panic!("expected EndpointStrategyInvalid, got {other:?}"),
    }
    assert!(
        uris.is_empty(),
        "a strategy that cannot be substituted must never reach the API server"
    );
}

// =========================================================================
// Selector matching (Step 2)
// =========================================================================

#[tokio::test]
async fn every_selector_pair_must_match_for_a_service_to_be_a_candidate() {
    let strategy = GatewayEndpointStrategy::ServiceBySelector {
        namespace: "gateway-lab".to_owned(),
        selector: selector(&[(GATEWAY_NAME_LABEL, "{gatewayName}"), ("role", "ingress")]),
        port_name: None,
        port: None,
    };
    let (resolved, _) = resolve_against(
        &strategy,
        vec![(
            200,
            service_list(&[
                // Satisfies the first pair only.
                service(
                    "half-match",
                    &[(GATEWAY_NAME_LABEL, "lab-gateway")],
                    &[(Some("http"), 80)],
                ),
                service(
                    "both",
                    &[(GATEWAY_NAME_LABEL, "lab-gateway"), ("role", "ingress")],
                    &[(Some("http"), 80)],
                ),
            ]),
        )],
    )
    .await;

    assert_eq!(
        resolved
            .expect("exactly one Service satisfies every pair")
            .service,
        "both"
    );
}

#[tokio::test]
async fn zero_matches_reports_every_service_that_was_considered() {
    let (resolved, _) = resolve_against(
        &istio_strategy(None, None),
        vec![(
            200,
            service_list(&[
                service("echo-a", &[("app", "echo-a")], &[(Some("http"), 8080)]),
                service("echo-b", &[("app", "echo-b")], &[(Some("http"), 8080)]),
                // The right label, the wrong Gateway.
                service(
                    "other-gateway-istio",
                    &[(GATEWAY_NAME_LABEL, "other-gateway")],
                    &[(Some("http"), 80)],
                ),
            ]),
        )],
    )
    .await;

    match resolved.expect_err("no Service carries this Gateway's label") {
        GatewayError::EndpointNotFound { lookup, considered } => {
            assert_eq!(lookup.cluster, CLUSTER);
            assert_eq!(lookup.namespace, "gateway-lab");
            assert_eq!(lookup.gateway, "gateway-lab/lab-gateway");
            assert!(
                lookup
                    .criteria
                    .contains("gateway.networking.k8s.io/gateway-name=lab-gateway"),
                "the criteria must show the substituted selector, got: {}",
                lookup.criteria
            );
            assert_eq!(
                considered,
                Some(vec![
                    "echo-a".to_owned(),
                    "echo-b".to_owned(),
                    "other-gateway-istio".to_owned(),
                ]),
                "every candidate name must be reported, in name order"
            );
        }
        other => panic!("expected EndpointNotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn several_matches_is_an_ambiguity_naming_all_of_them_and_never_a_pick() {
    let (resolved, _) = resolve_against(
        &istio_strategy(None, Some(80)),
        vec![(
            200,
            service_list(&[
                service(
                    "zebra-istio",
                    &[(GATEWAY_NAME_LABEL, "lab-gateway")],
                    &[(Some("http"), 80)],
                ),
                service("unrelated", &[], &[(Some("http"), 80)]),
                service(
                    "alpha-istio",
                    &[(GATEWAY_NAME_LABEL, "lab-gateway")],
                    &[(Some("http"), 80)],
                ),
            ]),
        )],
    )
    .await;

    match resolved.expect_err("two Services match the selector equally well") {
        GatewayError::EndpointAmbiguous { lookup, candidates } => {
            assert_eq!(lookup.namespace, "gateway-lab");
            assert_eq!(
                candidates,
                vec!["alpha-istio".to_owned(), "zebra-istio".to_owned()],
                "only the matches are candidates, and their order must not depend on list order"
            );
        }
        other => panic!("expected EndpointAmbiguous, got {other:?}"),
    }
}

/// `None` ("nothing was enumerated") and `Some(vec![])` ("the namespace
/// was listed and is empty") are different facts, and the error keeps
/// them apart rather than collapsing both into an empty list.
#[tokio::test]
async fn an_absent_named_service_reports_that_nothing_was_enumerated() {
    let (resolved, _) = resolve_against(
        &GatewayEndpointStrategy::ServiceByName {
            namespace: "gateway-lab".to_owned(),
            name: "lab-gateway-istio".to_owned(),
            port_name: None,
            port: None,
        },
        vec![(404, not_found("lab-gateway-istio"))],
    )
    .await;

    match resolved.expect_err("the named Service does not exist") {
        GatewayError::EndpointNotFound { lookup, considered } => {
            assert_eq!(considered, None);
            assert!(
                lookup.criteria.contains("lab-gateway-istio"),
                "got: {}",
                lookup.criteria
            );
            let message = GatewayError::EndpointNotFound {
                lookup,
                considered: Some(Vec::new()),
            }
            .to_string();
            assert!(
                message.contains("no Services at all"),
                "an empty listing must read differently from no listing, got: {message}"
            );
        }
        other => panic!("expected EndpointNotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn a_cluster_that_cannot_be_listed_is_unavailable_not_not_found() {
    let (resolved, _) = resolve_against(
        &istio_strategy(None, None),
        vec![(
            500,
            serde_json::json!({
                "kind": "Status",
                "apiVersion": "v1",
                "status": "Failure",
                "message": "etcd is unavailable",
                "code": 500,
            }),
        )],
    )
    .await;

    match resolved.expect_err("a 500 is not an answer about any Service") {
        GatewayError::ObservationUnavailable { cluster, .. } => assert_eq!(cluster, CLUSTER),
        other => panic!("expected ObservationUnavailable, got {other:?}"),
    }
}

// =========================================================================
// Port resolution (Step 3)
// =========================================================================

/// Drives a single matched, multi-port `Service` through one strategy's
/// port fields.
async fn resolve_ports(
    port_name: Option<&str>,
    port: Option<u16>,
    ports: &[(Option<&str>, i32)],
) -> Result<GatewayEndpoint, GatewayError> {
    let (resolved, _) = resolve_against(
        &istio_strategy(port_name, port),
        vec![(
            200,
            service_list(&[service(
                "lab-gateway-istio",
                &[(GATEWAY_NAME_LABEL, "lab-gateway")],
                ports,
            )]),
        )],
    )
    .await;
    resolved
}

const ISTIO_PORTS: [(Option<&str>, i32); 3] = [
    (Some("status-port"), 15021),
    (Some("http"), 80),
    (Some("https"), 443),
];

#[tokio::test]
async fn a_named_port_resolves_to_its_number() {
    assert_eq!(
        resolve_ports(Some("https"), None, &ISTIO_PORTS)
            .await
            .expect("https is exposed")
            .port,
        443
    );
}

#[tokio::test]
async fn an_absent_port_name_lists_every_port_the_service_exposes() {
    match resolve_ports(Some("grpc"), None, &ISTIO_PORTS)
        .await
        .expect_err("the Service exposes no port named grpc")
    {
        GatewayError::EndpointPortUnresolved {
            service,
            reason,
            ports,
            ..
        } => {
            assert_eq!(service, "gateway-lab/lab-gateway-istio");
            assert!(reason.contains("grpc"), "got: {reason}");
            assert_eq!(
                ports,
                vec![
                    "status-port=15021".to_owned(),
                    "http=80".to_owned(),
                    "https=443".to_owned(),
                ],
                "every exposed port must be reported, in declaration order"
            );
        }
        other => panic!("expected EndpointPortUnresolved, got {other:?}"),
    }
}

/// A bare `port` is checked against the `Service` rather than trusted --
/// see `admissionlab_gateway::endpoint`'s module documentation for why.
#[tokio::test]
async fn an_explicit_port_must_actually_be_exposed() {
    assert_eq!(
        resolve_ports(None, Some(443), &ISTIO_PORTS)
            .await
            .expect("443 is exposed")
            .port,
        443
    );

    let error = resolve_ports(None, Some(8080), &ISTIO_PORTS)
        .await
        .expect_err("8080 is not a port of this Service");
    assert!(
        error.to_string().contains("does not expose port 8080"),
        "got: {error}"
    );
}

#[tokio::test]
async fn a_port_name_and_a_port_that_disagree_are_rejected_rather_than_ranked() {
    let error = resolve_ports(Some("http"), Some(443), &ISTIO_PORTS)
        .await
        .expect_err("the two fields name different ports");
    let message = error.to_string();
    assert!(message.contains("\"http\""), "got: {message}");
    assert!(message.contains("443"), "got: {message}");

    // The agreeing case still resolves, so the check is about
    // disagreement rather than about naming both fields at all.
    assert_eq!(
        resolve_ports(Some("http"), Some(80), &ISTIO_PORTS)
            .await
            .expect("both fields name the same port")
            .port,
        80
    );
}

#[tokio::test]
async fn a_single_port_service_needs_neither_field() {
    assert_eq!(
        resolve_ports(None, None, &[(Some("http"), 80)])
            .await
            .expect("one port is not an ambiguity")
            .port,
        80
    );
    assert_eq!(
        resolve_ports(None, None, &[(None, 8080)])
            .await
            .expect("an unnamed single port is still unambiguous")
            .port,
        8080
    );
}

#[tokio::test]
async fn a_multi_port_service_with_neither_field_is_an_ambiguity() {
    match resolve_ports(None, None, &ISTIO_PORTS)
        .await
        .expect_err("three ports and no way to choose")
    {
        GatewayError::EndpointPortUnresolved { reason, ports, .. } => {
            assert!(
                reason.contains("neither a portName nor a port"),
                "got: {reason}"
            );
            assert_eq!(ports.len(), 3);
        }
        other => panic!("expected EndpointPortUnresolved, got {other:?}"),
    }
}

#[tokio::test]
async fn a_service_exposing_no_ports_is_reported_rather_than_defaulted() {
    let error = resolve_ports(None, None, &[])
        .await
        .expect_err("there is no port to forward to");
    let message = error.to_string();
    assert!(message.contains("exposes no ports"), "got: {message}");
    assert!(
        message.contains("exposed: none"),
        "an empty port list must read as \"none\" rather than as a blank, got: {message}"
    );
}

/// A `Service` port is an `i32` in the Kubernetes schema; a value
/// outside `1..=65535` means the object is not what this code believes
/// it is, so it is reported rather than truncated into a plausible port.
#[tokio::test]
async fn a_port_outside_the_tcp_range_is_reported_not_truncated() {
    let error = resolve_ports(Some("http"), None, &[(Some("http"), 70_000)])
        .await
        .expect_err("70000 is not a TCP port");
    assert!(
        error.to_string().contains("not a TCP port number"),
        "got: {error}"
    );
}
