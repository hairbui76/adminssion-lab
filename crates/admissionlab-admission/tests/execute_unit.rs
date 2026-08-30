//! Task 3.4 brief Step 1/3/4: proves the dry-run CREATE request shape,
//! the `Accepted`/`Rejected` classification, and `Warning` header
//! capture, against a `tower_test::mock`-backed `kube::Client` -- no
//! `kind` cluster required.
//!
//! Every assertion here drives
//! [`admissionlab_admission::execute_create_with_client`] (this crate's
//! offline-testable seam -- see that function's own documentation), the
//! same classification code path [`admissionlab_admission::KubeAdmissionExecutor::execute_create`]
//! uses in production, differing only in how the `kube::Client` was
//! built.

use std::pin::pin;

use admissionlab_admission::{AdmissionDecision, execute_create_with_client};
use admissionlab_core::FixtureId;
use admissionlab_fixtures::{FixtureSource, ResolvedResource};
use http::{Request, Response};
use kube::client::Body;
use kube::core::{ApiResource, GroupVersion};
use serde_json::json;
use tower_test::mock;

/// A [`FixtureSource`] wrapping `object`, with an otherwise-fixed,
/// inert identity nothing under test inspects.
fn fixture_with_object(object: serde_json::Value) -> FixtureSource {
    FixtureSource {
        id: FixtureId::parse("execute-unit-fixture-0").expect("valid FixtureId"),
        path: std::path::PathBuf::from("fixture.yaml"),
        document_index: 0,
        sha256: "0".repeat(64),
        object,
    }
}

/// A [`ResolvedResource`] for the core `v1` `ConfigMap` resource,
/// namespaced -- what Task 3.2's real resolver would have produced for
/// this task's own fixtures.
fn configmap_resource() -> ResolvedResource {
    ResolvedResource {
        api_resource: ApiResource::from_gvk(
            &"v1".parse::<GroupVersion>().unwrap().with_kind("ConfigMap"),
        ),
        namespaced: true,
    }
}

/// A [`ResolvedResource`] for a cluster-scoped custom resource
/// (`kyverno.io/v2` `ClusterPolicy`), proving the `Api::all_with` branch
/// (no namespace segment in the request path) separately from the
/// namespaced case every other test here exercises.
fn cluster_policy_resource() -> ResolvedResource {
    ResolvedResource {
        api_resource: ApiResource::from_gvk(
            &"kyverno.io/v2"
                .parse::<GroupVersion>()
                .unwrap()
                .with_kind("ClusterPolicy"),
        ),
        namespaced: false,
    }
}

#[tokio::test]
async fn request_is_a_create_with_dry_run_all_and_the_fixtures_own_namespace() {
    // Fails if the request used any method other than POST, targeted
    // the wrong path (proving `resource`/namespace were actually used
    // to build the URL, not hardcoded), or omitted/misspelled the
    // `dryRun=All` query parameter -- the one assertion Task 3.4 brief
    // Step 1 calls out as mattering most. Verified against the request
    // the mock service actually received, not against a value this
    // test's own code computed.
    let (mock_service, handle) = mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(mock_service, "default");

    let fixture = fixture_with_object(json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {"name": "demo", "namespace": "kyverno-test"},
        "data": {"key": "value"},
    }));
    let resource = configmap_resource();

    let responder = tokio::spawn(async move {
        let mut handle = pin!(handle);
        let (request, send) = handle.next_request().await.expect("one CREATE request");

        assert_eq!(request.method(), http::Method::POST);
        assert_eq!(
            request.uri().path(),
            "/api/v1/namespaces/kyverno-test/configmaps",
            "must target the fixture's own namespace, not the client's default"
        );
        let query = request.uri().query().unwrap_or_default();
        assert!(
            query.split('&').any(|pair| pair == "dryRun=All"),
            "query {query:?} must carry dryRun=All"
        );

        send.send_response(
            Response::builder()
                .status(201)
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "apiVersion": "v1",
                        "kind": "ConfigMap",
                        "metadata": {"name": "demo", "namespace": "kyverno-test"},
                        "data": {"key": "value"},
                    }))
                    .expect("serialize admitted object"),
                ))
                .expect("build 201 response"),
        );
    });

    let response = execute_create_with_client(client, "test-cluster", &fixture, &resource)
        .await
        .expect("mocked dry-run CREATE must succeed");

    assert_eq!(response.decision, AdmissionDecision::Accepted);
    assert_eq!(
        response.response_object,
        Some(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {"name": "demo", "namespace": "kyverno-test"},
            "data": {"key": "value"},
        }))
    );

    responder.await.expect("mock responder task must not panic");
}

#[tokio::test]
async fn cluster_scoped_resource_omits_a_namespace_segment() {
    // Fails if `resource.namespaced == false` were ignored and a
    // namespace segment (or the fixture's absent one) were appended
    // anyway.
    let (mock_service, handle) = mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(mock_service, "default");

    let fixture = fixture_with_object(json!({
        "apiVersion": "kyverno.io/v2",
        "kind": "ClusterPolicy",
        "metadata": {"name": "demo-policy"},
    }));
    let resource = cluster_policy_resource();

    let responder = tokio::spawn(async move {
        let mut handle = pin!(handle);
        let (request, send) = handle.next_request().await.expect("one CREATE request");
        assert_eq!(request.uri().path(), "/apis/kyverno.io/v2/clusterpolicies");
        send.send_response(
            Response::builder()
                .status(201)
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "apiVersion": "kyverno.io/v2",
                        "kind": "ClusterPolicy",
                        "metadata": {"name": "demo-policy"},
                    }))
                    .expect("serialize admitted object"),
                ))
                .expect("build 201 response"),
        );
    });

    let response = execute_create_with_client(client, "test-cluster", &fixture, &resource)
        .await
        .expect("mocked dry-run CREATE must succeed");
    assert_eq!(response.decision, AdmissionDecision::Accepted);

    responder.await.expect("mock responder task must not panic");
}

#[tokio::test]
async fn a_rejection_is_classified_as_rejected_with_its_code_and_message() {
    // Fails if a non-2xx response were treated as `Accepted` (ignoring
    // the status code), if `code`/`message` were dropped or fabricated
    // rather than taken from the response body, or if this were
    // misclassified as `UnsupportedDryRun` -- a genuine `Forbidden`
    // policy denial is exactly the case that must stay `Rejected` (see
    // `admissionlab_admission::execute`'s own module documentation for
    // why this shape, captured live against a real cluster's
    // `PodSecurity` admission plugin, maps here).
    let (mock_service, handle) = mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(mock_service, "default");

    let fixture = fixture_with_object(json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {"name": "demo", "namespace": "default"},
    }));
    let resource = configmap_resource();

    let responder = tokio::spawn(async move {
        let mut handle = pin!(handle);
        let (_request, send) = handle.next_request().await.expect("one CREATE request");
        send.send_response(
            Response::builder()
                .status(403)
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "kind": "Status",
                        "apiVersion": "v1",
                        "status": "Failure",
                        "message": "configmaps \"demo\" is forbidden: denied by policy",
                        "reason": "Forbidden",
                        "code": 403,
                    }))
                    .expect("serialize Status"),
                ))
                .expect("build 403 response"),
        );
    });

    let response = execute_create_with_client(client, "test-cluster", &fixture, &resource)
        .await
        .expect("a real 403 response is a successful observation, not an Err");

    assert_eq!(
        response.decision,
        AdmissionDecision::Rejected {
            code: Some(403),
            message: "configmaps \"demo\" is forbidden: denied by policy".to_string(),
        }
    );
    assert_eq!(
        response.response_object, None,
        "a rejection must never report a response_object"
    );

    responder.await.expect("mock responder task must not panic");
}

#[tokio::test]
async fn warning_response_headers_are_captured_verbatim() {
    // Fails if `Warning` headers were dropped (as they would be through
    // `Api::create`'s own convenience path -- see this crate's
    // `execute` module documentation for why `Client::send` is used
    // instead), or if the captured value were reformatted rather than
    // reported exactly as the header carried it.
    let (mock_service, handle) = mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(mock_service, "default");

    let fixture = fixture_with_object(json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {"name": "demo", "namespace": "default"},
    }));
    let resource = configmap_resource();

    let responder = tokio::spawn(async move {
        let mut handle = pin!(handle);
        let (_request, send) = handle.next_request().await.expect("one CREATE request");
        send.send_response(
            Response::builder()
                .status(201)
                .header(http::header::WARNING, "299 - \"deprecated field in use\"")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "apiVersion": "v1",
                        "kind": "ConfigMap",
                        "metadata": {"name": "demo", "namespace": "default"},
                    }))
                    .expect("serialize admitted object"),
                ))
                .expect("build 201 response with a Warning header"),
        );
    });

    let response = execute_create_with_client(client, "test-cluster", &fixture, &resource)
        .await
        .expect("mocked dry-run CREATE must succeed");

    assert_eq!(
        response.warnings,
        vec!["299 - \"deprecated field in use\"".to_string()]
    );

    responder.await.expect("mock responder task must not panic");
}

#[tokio::test]
async fn no_warning_header_is_reported_as_an_empty_list() {
    // Distinguishes "captured, and there were none" from a test that
    // would pass vacuously regardless of whether capture logic ran at
    // all -- pairs with the previous test's non-empty case.
    let (mock_service, handle) = mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(mock_service, "default");

    let fixture = fixture_with_object(json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {"name": "demo", "namespace": "default"},
    }));
    let resource = configmap_resource();

    let responder = tokio::spawn(async move {
        let mut handle = pin!(handle);
        let (_request, send) = handle.next_request().await.expect("one CREATE request");
        send.send_response(
            Response::builder()
                .status(201)
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "apiVersion": "v1",
                        "kind": "ConfigMap",
                        "metadata": {"name": "demo", "namespace": "default"},
                    }))
                    .expect("serialize admitted object"),
                ))
                .expect("build 201 response"),
        );
    });

    let response = execute_create_with_client(client, "test-cluster", &fixture, &resource)
        .await
        .expect("mocked dry-run CREATE must succeed");

    assert!(response.warnings.is_empty());

    responder.await.expect("mock responder task must not panic");
}
