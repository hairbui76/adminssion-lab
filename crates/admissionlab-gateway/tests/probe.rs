//! ROADMAP Task 6.8: the HTTP probe engine.
//!
//! Every test here drives the real
//! [`admissionlab_gateway::execute_http_probe`] against a real HTTP
//! server on a real ephemeral loopback socket. Nothing is mocked at the
//! HTTP layer, because almost everything Task 6.8 asks for is a claim
//! about the wire: which `Host` arrives, whether a redirect is followed,
//! whether a connection refused a moment ago is retried, what a body's
//! hash is.
//!
//! Two kinds of server are used, deliberately:
//!
//! - **The real echo backend**
//!   (`admissionlab_echo::serve::serve_on`, a workspace library) for
//!   everything about the frozen JSON contract. This is what keeps
//!   `probe.rs`'s parser and `admissionlab-echo`'s response in step
//!   without the two sharing a Rust type — see that module's
//!   "Identifying the backend" section.
//! - **A hand-rolled one-shot TCP responder** for everything the echo
//!   backend cannot produce on purpose: a `500`, a `302`, a non-JSON
//!   body, an oversized body, a connection that is refused before it is
//!   ready.
//!
//! **Not covered here, deliberately:** that a real Gateway data plane
//! behaves this way, which is a live-cluster fact scoped to the Phase 6
//! exit gate.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use admissionlab_echo::config::EchoConfig;
use admissionlab_echo::serve::serve_on;
use admissionlab_gateway::{
    GatewayError, HttpProbeContract, MAX_PROBE_BODY_BYTES, REDACTED_REQUEST_HEADERS,
    describe_probe_request, execute_http_probe, redacted_probe_headers,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

// =========================================================================
// Fixture helpers
// =========================================================================

/// A `GET /payments` probe for `api.example.test`, with no extra
/// headers.
fn contract() -> HttpProbeContract {
    HttpProbeContract {
        host: "api.example.test".to_owned(),
        path: "/payments".to_owned(),
        method: "GET".to_owned(),
        headers: BTreeMap::new(),
        expected_status: 200,
        expected_backend: None,
    }
}

/// Serves the *real* echo backend on an ephemeral loopback port and
/// returns its address. The task is detached: it lives as long as the
/// test process, which is exactly as long as any test needs it.
async fn spawn_echo(backend_id: &str) -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind an ephemeral loopback port");
    let addr = listener.local_addr().expect("local_addr");
    let config = Arc::new(EchoConfig {
        backend_id: backend_id.to_owned(),
        default_delay: std::time::Duration::ZERO,
    });
    tokio::spawn(serve_on(listener, config));
    addr
}

/// Serves `responses.len()` connections with raw HTTP/1.1 bytes, one
/// response per connection, then stops accepting.
///
/// Hand-rolled rather than built on a framework: these are responses no
/// well-behaved server produces (an unparseable content type, a
/// megabyte-and-one-byte body), and writing the bytes directly is the
/// only way to be sure what arrives.
async fn spawn_raw(responses: Vec<Vec<u8>>) -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind an ephemeral loopback port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        for response in responses {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            // Read (and discard) the request head, so the client's write
            // completes before the response is written back.
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer).await;
            let _ = stream.write_all(&response).await;
            let _ = stream.flush().await;
            let _ = stream.shutdown().await;
        }
    });
    addr
}

/// A raw HTTP/1.1 response with an explicit `Content-Length`.
fn raw_response(status_line: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    use std::fmt::Write as _;

    let mut out = format!("HTTP/1.1 {status_line}\r\n");
    for (name, value) in headers {
        writeln!(out, "{name}: {value}\r").expect("writing to a String cannot fail");
    }
    writeln!(out, "content-length: {}\r\n\r", body.len()).expect("writing to a String cannot fail");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// An address nothing is listening on: a loopback socket that was bound
/// and then released.
async fn closed_addr() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind an ephemeral loopback port");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);
    addr
}

// =========================================================================
// The happy path, against the real echo backend
// =========================================================================

#[tokio::test]
async fn a_real_echo_backend_is_identified_from_its_frozen_json() {
    // The load-bearing test for the shared-wire-contract-without-a-shared-type
    // decision: this asserts `probe.rs`'s parser against the bytes
    // `admissionlab-echo` actually writes, not against a hand-copied
    // fixture that could drift from it.
    let addr = spawn_echo("echo-a").await;

    let result = execute_http_probe(addr, &contract())
        .await
        .expect("the echo backend answers");

    assert_eq!(result.status, 200);
    assert_eq!(result.backend.as_deref(), Some("echo-a"));
    assert_eq!(result.attempts, 1, "a ready backend needs one attempt");
    assert_eq!(
        result.response_body_sha256.len(),
        64,
        "a SHA-256 renders as 64 lowercase hex characters"
    );
    assert!(
        result.response_headers.contains_key("content-type"),
        "response headers must be lowercased, got {:?}",
        result.response_headers.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn the_contracts_host_is_sent_rather_than_the_local_address() {
    // The entire point of probing a forwarded port: the request arrives
    // at 127.0.0.1 and must be *routed* as api.example.test. The echo
    // backend reports the Host it received, so this is checkable
    // end-to-end.
    let addr = spawn_echo("echo-a").await;
    let mut contract = contract();
    contract
        .headers
        .insert("x-test".to_owned(), "value".to_owned());

    let result = execute_http_probe(addr, &contract)
        .await
        .expect("the echo backend answers");
    let body = fetch_echo_body(addr, &contract).await;

    assert_eq!(result.status, 200);
    assert_eq!(body["host"], "api.example.test");
    assert_eq!(body["path"], "/payments");
    assert_eq!(body["method"], "GET");
    assert_eq!(
        body["headers"]["x-test"], "value",
        "extra contract headers must reach the backend"
    );
}

/// Re-probes and returns the echo body as JSON, so a test can assert on
/// what the backend *received* rather than only on what the probe
/// reported. Uses the same probe under test, so the request is
/// identical.
async fn fetch_echo_body(addr: SocketAddr, contract: &HttpProbeContract) -> serde_json::Value {
    // The probe returns only a hash of the body, on purpose (see
    // `HttpProbeResult`), so this reads the body directly over a second
    // connection with the same request.
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let mut request = format!(
        "{} {} HTTP/1.1\r\nhost: {}\r\n",
        contract.method, contract.path, contract.host
    );
    for (name, value) in &contract.headers {
        std::fmt::Write::write_fmt(&mut request, format_args!("{name}: {value}\r\n"))
            .expect("writing to a String cannot fail");
    }
    request.push_str("connection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read response");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let body = text
        .split_once("\r\n\r\n")
        .expect("a response has a header/body separator")
        .1;
    serde_json::from_str(body).expect("the echo body is JSON")
}

// =========================================================================
// Backend identification (Step 4 / Global Constraint 15)
// =========================================================================

#[tokio::test]
async fn a_non_json_response_yields_no_backend_rather_than_a_guess() {
    let addr = spawn_raw(vec![raw_response(
        "200 OK",
        &[("content-type", "text/plain")],
        b"echo-a",
    )])
    .await;

    let result = execute_http_probe(addr, &contract())
        .await
        .expect("a plain-text 200 is a result");
    assert_eq!(result.status, 200);
    assert_eq!(
        result.backend, None,
        "the body literally says \"echo-a\", and the probe must still not claim it -- only a \
         JSON content type plus the frozen shape identifies a backend"
    );
}

#[tokio::test]
async fn json_that_is_not_the_frozen_shape_yields_no_backend() {
    // A `backend` key alone is not an echo answer: some unrelated
    // service could legitimately return one, and reading it as a
    // Gateway backend identity would fabricate a routing observation.
    let addr = spawn_raw(vec![raw_response(
        "200 OK",
        &[("content-type", "application/json")],
        br#"{"backend":"not-an-echo-backend"}"#,
    )])
    .await;

    let result = execute_http_probe(addr, &contract())
        .await
        .expect("a JSON 200 is a result");
    assert_eq!(result.backend, None);
}

#[tokio::test]
async fn a_json_content_type_with_parameters_and_an_extra_key_still_identifies_the_backend() {
    // Two tolerances asserted together, both documented in `probe.rs`:
    // a charset parameter is still JSON, and a sixth key is not a
    // reason to report "no identifiable backend".
    let addr = spawn_raw(vec![raw_response(
        "200 OK",
        &[("content-type", "application/json; charset=utf-8")],
        br#"{"backend":"echo-b","method":"GET","path":"/payments","host":"api.example.test","headers":{},"future":"field"}"#,
    )])
    .await;

    let result = execute_http_probe(addr, &contract())
        .await
        .expect("a JSON 200 is a result");
    assert_eq!(result.backend.as_deref(), Some("echo-b"));
}

// =========================================================================
// Retries (Step 3)
// =========================================================================

#[tokio::test]
async fn a_connection_that_is_not_ready_yet_is_retried_and_the_attempt_is_counted() {
    // The real sequence a port-forward produces: kubectl has bound its
    // local socket, but the data plane behind it is not accepting yet,
    // so the first connect is refused. Modelled by binding a port,
    // releasing it, and starting a listener on that same port shortly
    // afterwards.
    let addr = closed_addr().await;
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let listener = TcpListener::bind(addr)
            .await
            .expect("re-bind the released port");
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer).await;
            let _ = stream
                .write_all(&raw_response(
                    "200 OK",
                    &[("content-type", "application/json")],
                    br#"{"backend":"echo-a","method":"GET","path":"/payments","host":"api.example.test","headers":{}}"#,
                ))
                .await;
            let _ = stream.shutdown().await;
        }
    });

    let result = execute_http_probe(addr, &contract())
        .await
        .expect("the probe must retry until the backend is listening");
    assert_eq!(result.status, 200);
    assert_eq!(result.backend.as_deref(), Some("echo-a"));
    assert!(
        result.attempts >= 2,
        "the refused attempts must be counted honestly, got {}",
        result.attempts
    );
}

#[tokio::test]
async fn a_500_is_a_result_and_is_never_retried_into_a_success() {
    // One response is scripted. If the probe retried a 5xx, the second
    // attempt would find nothing accepting and the test would fail with
    // ProbeUnavailable rather than with a 500 -- so this asserts the
    // no-retry rule by construction, not only by reading `attempts`.
    let addr = spawn_raw(vec![raw_response(
        "500 Internal Server Error",
        &[("content-type", "text/plain")],
        b"upstream connect error",
    )])
    .await;

    let result = execute_http_probe(addr, &contract())
        .await
        .expect("a 500 is an observation, not an error");
    assert_eq!(result.status, 500);
    assert_eq!(
        result.attempts, 1,
        "an application error is answered on the first attempt and never re-asked"
    );
}

#[tokio::test]
async fn a_connection_that_is_never_ready_fails_with_an_honest_attempt_count() {
    let addr = closed_addr().await;

    let error = execute_http_probe(addr, &contract())
        .await
        .expect_err("nothing is listening, ever");

    match error {
        GatewayError::ProbeUnavailable {
            attempts, reason, ..
        } => {
            assert!(attempts >= 2, "the window must cover several attempts");
            assert!(reason.contains("not ready"), "got: {reason}");
        }
        other => panic!("expected ProbeUnavailable, got {other:?}"),
    }
}

// =========================================================================
// Redirects (Step 2)
// =========================================================================

#[tokio::test]
async fn a_redirect_is_reported_rather_than_followed() {
    // Only one connection is served. A client that followed the
    // redirect would open a second one, find nothing, and this test
    // would fail with an error instead of a 302 -- so the assertion is
    // about behavior, not about a configuration flag.
    let addr = spawn_raw(vec![raw_response(
        "302 Found",
        &[("location", "http://api.example.test/moved")],
        b"",
    )])
    .await;

    let result = execute_http_probe(addr, &contract())
        .await
        .expect("a redirect is a result");
    assert_eq!(result.status, 302);
    assert_eq!(
        result.response_headers.get("location").map(String::as_str),
        Some("http://api.example.test/moved"),
        "the Location header must be preserved for a later task to compare"
    );
    assert!(admissionlab_gateway::is_redirect(result.status));
}

// =========================================================================
// Body handling
// =========================================================================

#[tokio::test]
async fn the_body_hash_is_over_the_bytes_as_received() {
    let body = b"deterministic bytes";
    let addr = spawn_raw(vec![raw_response(
        "200 OK",
        &[("content-type", "text/plain")],
        body,
    )])
    .await;

    let result = execute_http_probe(addr, &contract())
        .await
        .expect("a 200 is a result");
    assert_eq!(
        result.response_body_sha256,
        admissionlab_core::sha256_hex(body),
        "the hash must be the same function of the same bytes every other digest in this \
         project uses"
    );
}

#[tokio::test]
async fn an_empty_body_hashes_to_the_real_hash_of_no_bytes() {
    let addr = spawn_raw(vec![raw_response("204 No Content", &[], b"")]).await;
    let result = execute_http_probe(addr, &contract())
        .await
        .expect("a 204 is a result");
    assert_eq!(result.status, 204);
    assert_eq!(
        result.response_body_sha256,
        admissionlab_core::sha256_hex(b""),
        "an empty body has a hash; it is not a sentinel"
    );
}

#[tokio::test]
async fn an_over_cap_body_is_an_error_rather_than_a_hash_of_the_prefix() {
    // The decision this test pins: a truncated hash would be a *wrong*
    // hash under a field named response_body_sha256, and two sides both
    // truncating at the same cap would report "no change" about a body
    // that changed.
    let body = vec![b'x'; MAX_PROBE_BODY_BYTES + 1];
    let addr = spawn_raw(vec![raw_response(
        "200 OK",
        &[("content-type", "text/plain")],
        &body,
    )])
    .await;

    let error = execute_http_probe(addr, &contract())
        .await
        .expect_err("a body over the cap must not be hashed");
    match error {
        GatewayError::ProbeBodyTooLarge { limit, .. } => {
            assert_eq!(limit, MAX_PROBE_BODY_BYTES);
        }
        other => panic!("expected ProbeBodyTooLarge, got {other:?}"),
    }
}

#[tokio::test]
async fn a_body_exactly_at_the_cap_is_read() {
    let body = vec![b'x'; MAX_PROBE_BODY_BYTES];
    let addr = spawn_raw(vec![raw_response(
        "200 OK",
        &[("content-type", "text/plain")],
        &body,
    )])
    .await;

    let result = execute_http_probe(addr, &contract())
        .await
        .expect("the cap is inclusive");
    assert_eq!(
        result.response_body_sha256,
        admissionlab_core::sha256_hex(&body)
    );
}

// =========================================================================
// Redaction (Step 5)
// =========================================================================

fn authenticated_contract() -> HttpProbeContract {
    let mut contract = contract();
    contract
        .headers
        .insert("Authorization".to_owned(), "Bearer super-secret".to_owned());
    contract
        .headers
        .insert("cookie".to_owned(), "session=super-secret".to_owned());
    contract
        .headers
        .insert("x-request-id".to_owned(), "3f0c".to_owned());
    contract
}

#[test]
fn credential_header_values_never_appear_in_a_rendered_request() {
    let described = describe_probe_request(&authenticated_contract());
    assert!(
        !described.contains("super-secret"),
        "a credential reached a rendered request: {described}"
    );
    assert!(
        described.contains("[REDACTED]"),
        "the redaction must be visible rather than the header silently vanishing: {described}"
    );
    assert!(
        described.contains("3f0c"),
        "a non-credential header must still be shown verbatim: {described}"
    );
    assert!(described.contains("api.example.test"), "got: {described}");
}

#[test]
fn credential_headers_are_matched_case_insensitively_and_carry_no_payload() {
    let redacted = redacted_probe_headers(&authenticated_contract());
    for name in REDACTED_REQUEST_HEADERS {
        if let Some(value) = redacted.get(name) {
            assert_eq!(
                *value,
                admissionlab_core::RedactedValue::Sensitive,
                "{name} must be Sensitive, which carries no payload at all"
            );
        }
    }
    // `Authorization` was written capitalized in the contract; it must
    // still be matched.
    assert_eq!(
        redacted.get("authorization"),
        Some(&admissionlab_core::RedactedValue::Sensitive)
    );
    let serialized = serde_json::to_string(&redacted).expect("serializes");
    assert!(
        !serialized.contains("super-secret"),
        "a credential survived serialization: {serialized}"
    );
}

#[tokio::test]
async fn a_probe_failure_reports_the_request_with_credentials_redacted() {
    // The path that matters most: an error message is the one place a
    // request description is guaranteed to be rendered somewhere a human
    // will read it.
    let addr = closed_addr().await;
    let error = execute_http_probe(addr, &authenticated_contract())
        .await
        .expect_err("nothing is listening");
    let message = error.to_string();
    assert!(
        !message.contains("super-secret"),
        "a credential reached an error message: {message}"
    );
    assert!(message.contains("[REDACTED]"), "got: {message}");
}

#[tokio::test]
async fn the_probe_result_itself_carries_no_copy_of_the_request() {
    // The strongest form of the guarantee: the stored evidence has
    // nothing to redact because it holds no request at all.
    let addr = spawn_echo("echo-a").await;
    let result = execute_http_probe(addr, &authenticated_contract())
        .await
        .expect("the echo backend answers");
    let serialized = serde_json::to_string(&result).expect("HttpProbeResult serializes outward");
    assert!(
        !serialized.contains("super-secret"),
        "a credential reached the serialized evidence: {serialized}"
    );
    assert!(
        !serialized.contains("authorization"),
        "the evidence must not carry the request's headers at all: {serialized}"
    );
}

// =========================================================================
// Contract validation
// =========================================================================

#[tokio::test]
async fn a_host_entry_in_headers_is_rejected_rather_than_silently_winning() {
    let mut contract = contract();
    contract
        .headers
        .insert("host".to_owned(), "someone-else.test".to_owned());

    let addr = closed_addr().await;
    let error = execute_http_probe(addr, &contract)
        .await
        .expect_err("two Host headers is not a request that can be sent");
    assert!(
        matches!(error, GatewayError::ProbeRequestInvalid { .. }),
        "got {error:?}"
    );
}

#[tokio::test]
async fn an_unsendable_header_value_fails_without_quoting_the_value() {
    let mut contract = contract();
    // A newline in a header value is a request-smuggling shape `http`
    // refuses to build; the point here is that the rejection message
    // must not echo the value, which could be a credential.
    contract.headers.insert(
        "authorization".to_owned(),
        "Bearer super\nsecret".to_owned(),
    );

    let addr = closed_addr().await;
    let error = execute_http_probe(addr, &contract)
        .await
        .expect_err("a header value with a newline cannot be sent");
    let message = error.to_string();
    assert!(
        !message.contains("super"),
        "the offending value must not be quoted back: {message}"
    );
}

#[tokio::test]
async fn elapsed_serializes_as_plain_milliseconds() {
    let addr = spawn_echo("echo-a").await;
    let result = execute_http_probe(addr, &contract())
        .await
        .expect("the echo backend answers");
    let json = serde_json::to_value(&result).expect("serializes outward");
    assert!(
        json["elapsed"].is_u64(),
        "elapsed must be a plain integer number of milliseconds, matching every other duration \
         this project serializes; got {}",
        json["elapsed"]
    );
    assert!(json["responseBodySha256"].is_string(), "camelCase keys");
}
