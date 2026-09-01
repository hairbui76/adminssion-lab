//! The echo backend's contract, asserted over a real TCP connection.
//!
//! `src/echo.rs`, `src/delay.rs` and `src/serve.rs` each unit-test
//! their own half of the behavior directly, with an in-memory body and
//! no socket. This file exists for what those cannot cover: that the
//! *bytes on the wire* are the frozen contract Task 6.8's HTTP probe
//! will parse, produced by the same accept loop the container actually
//! runs (`serve::serve_on`), through a real `hyper` HTTP/1.1
//! connection, with the request written by hand as raw bytes.
//!
//! Raw bytes rather than an HTTP client library, deliberately: an
//! exact-match assertion on the response body is only meaningful if the
//! test controls exactly which request headers were sent, and every
//! client adds headers of its own (`user-agent`, `accept`,
//! `content-length`) that would then have to be echoed back and
//! accounted for. Writing the request by hand also makes the
//! hop-by-hop cases (`Connection: close, X-Per-Connection`) expressible
//! at all -- most clients will not let a caller set those.
//!
//! Every request here ends with `Connection: close` unless the test is
//! specifically about connection reuse, so the server closes the socket
//! after answering and the response can simply be read to end-of-file
//! rather than re-implementing chunked/`Content-Length` framing in a
//! test helper.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use admissionlab_echo::config::{BACKEND_ID_ENV, DELAY_MS_ENV, EchoConfig};
use admissionlab_echo::delay::DELAY_HEADER;
use admissionlab_echo::echo::EchoBody;
use admissionlab_echo::serve;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

/// Starts a real server on an ephemeral loopback port and returns the
/// address it is listening on.
///
/// The accept loop is spawned and never joined: it does not return by
/// construction (see [`serve::serve_on`]), and the whole point of the
/// test process exiting is that it takes its listeners with it.
/// Binding `127.0.0.1:0` keeps each test on its own port with no
/// coordination between them and nothing exposed off the machine.
async fn start(backend_id: &str, default_delay: Duration) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback port is always available");
    let addr = listener
        .local_addr()
        .expect("a bound listener has a local address");
    let config = Arc::new(EchoConfig {
        backend_id: backend_id.to_owned(),
        default_delay,
    });
    tokio::spawn(serve::serve_on(listener, config));
    addr
}

/// Writes `request` verbatim and reads the whole response until the
/// server closes the connection. Returns the response's head (status
/// line and headers) and its body, split on the blank line.
async fn send(addr: SocketAddr, request: &str) -> (String, String) {
    let raw = send_raw(addr, request).await;
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .expect("an HTTP response separates head from body with a blank line");
    (head.to_owned(), body.to_owned())
}

/// [`send`] without the head/body split -- for the one test that sends
/// two requests on one connection and reads both answers.
async fn send_raw(addr: SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(addr)
        .await
        .expect("the test server is listening");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("writing a small request cannot fail");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("the server closes the connection after answering");
    String::from_utf8(raw).expect("these responses are UTF-8")
}

/// Parses the response body as the frozen echo body, failing loudly
/// with the raw text if it is not one.
fn parse(body: &str) -> EchoBody {
    serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("expected a frozen echo body, got {body:?}: {error}"))
}

/// The exact bytes of the frozen contract (ROADMAP Task 6.5's own
/// "Interfaces" block), including key order: five keys, `backend`
/// first, `headers` last. Task 6.8's probe parses `backend` out of
/// exactly this; a change that breaks this assertion is a change to
/// that probe's input, not a refactor of this crate.
#[tokio::test]
async fn the_frozen_json_shape_is_what_goes_on_the_wire() {
    let addr = start("echo-a", Duration::ZERO).await;
    let (head, body) = send(
        addr,
        "GET /payments HTTP/1.1\r\n\
         Host: api.example.test\r\n\
         X-Test: value\r\n\
         Connection: close\r\n\r\n",
    )
    .await;

    assert!(
        head.starts_with("HTTP/1.1 200 OK\r\n"),
        "an echoed request is a 200, got {head:?}"
    );
    assert!(
        head.contains("content-type: application/json"),
        "the echo body is JSON, got {head:?}"
    );
    assert_eq!(
        body,
        r#"{"backend":"echo-a","method":"GET","path":"/payments","host":"api.example.test","headers":{"host":"api.example.test","x-test":"value"}}"#
    );
}

/// Two backends, one request: the only difference in the answers is the
/// identity, which is the entire signal Task 6.9's comparator reads as
/// `traffic_backend_changed`.
#[tokio::test]
async fn the_backend_id_is_the_configured_one_and_nothing_else() {
    let request = "GET /payments HTTP/1.1\r\nHost: api.example.test\r\nConnection: close\r\n\r\n";
    let a = start("echo-a", Duration::ZERO).await;
    let b = start("echo-b", Duration::ZERO).await;

    let (_, body_a) = send(a, request).await;
    let (_, body_b) = send(b, request).await;

    assert_eq!(parse(&body_a).backend, "echo-a");
    assert_eq!(parse(&body_b).backend, "echo-b");
    assert_eq!(
        body_a.replace("echo-a", "echo-b"),
        body_b,
        "the backend id must be the only difference between two backends' answers"
    );
}

/// The `Host` header is what an `HTTPRoute`'s `hostnames` matched on, so
/// it is echoed verbatim into its own field as well as into the header
/// map.
#[tokio::test]
async fn the_host_header_is_echoed_verbatim() {
    let addr = start("echo-a", Duration::ZERO).await;
    let (_, body) = send(
        addr,
        "GET / HTTP/1.1\r\nHost: shop.example.test:8443\r\nConnection: close\r\n\r\n",
    )
    .await;
    let echo = parse(&body);
    assert_eq!(echo.host, "shop.example.test:8443");
    assert_eq!(
        echo.headers.get("host").map(String::as_str),
        Some("shop.example.test:8443")
    );
}

/// An HTTP/1.0 request need not carry a `Host` at all. Reported as an
/// empty string -- "this request carried no host" -- never as a
/// fabricated one (Global Constraint 15).
#[tokio::test]
async fn a_request_with_no_host_header_reports_an_empty_host() {
    let addr = start("echo-a", Duration::ZERO).await;
    let (head, body) = send(addr, "GET /payments HTTP/1.0\r\n\r\n").await;
    assert!(head.starts_with("HTTP/1.0 200 OK\r\n"), "got {head:?}");
    let echo = parse(&body);
    assert_eq!(echo.host, "");
    assert!(!echo.headers.contains_key("host"));
}

/// Hop-by-hop headers are transport artifacts every intermediary
/// rewrites; echoing them would make two identical routings differ for
/// reasons unrelated to the Gateway under test (see `src/echo.rs`'s own
/// module documentation). `Connection: close, X-Per-Connection` covers
/// both halves of the rule at once: the fixed list, and the names the
/// request's own `Connection` header nominates.
#[tokio::test]
async fn hop_by_hop_headers_never_reach_the_wire() {
    let addr = start("echo-a", Duration::ZERO).await;
    let (_, body) = send(
        addr,
        "GET /payments HTTP/1.1\r\n\
         Host: api.example.test\r\n\
         Connection: close, X-Per-Connection\r\n\
         X-Per-Connection: transport detail\r\n\
         Keep-Alive: timeout=5\r\n\
         TE: trailers\r\n\
         Proxy-Authorization: Basic dXNlcjpwYXNz\r\n\
         X-Kept: routing evidence\r\n\r\n",
    )
    .await;

    let echo = parse(&body);
    assert_eq!(
        echo.headers.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["host", "x-kept"],
        "only end-to-end headers are evidence of routing"
    );
}

/// Proxy-injected headers *are* echoed: they are real observed behavior
/// of the data plane under test, and deciding they are noise is the
/// comparator's job, not this backend's (see `src/echo.rs`).
#[tokio::test]
async fn proxy_injected_headers_reach_the_wire() {
    let addr = start("echo-a", Duration::ZERO).await;
    let (_, body) = send(
        addr,
        "GET / HTTP/1.1\r\n\
         Host: api.example.test\r\n\
         X-Forwarded-For: 10.0.0.1\r\n\
         X-Forwarded-Proto: https\r\n\
         X-Envoy-Attempt-Count: 1\r\n\
         Connection: close\r\n\r\n",
    )
    .await;

    let echo = parse(&body);
    assert_eq!(
        echo.headers.get("x-forwarded-for").map(String::as_str),
        Some("10.0.0.1")
    );
    assert_eq!(
        echo.headers
            .get("x-envoy-attempt-count")
            .map(String::as_str),
        Some("1")
    );
}

/// Sorted on the wire, not merely sorted once parsed: a comparator that
/// hashes the response body (Task 6.8's `response_body_sha256`) sees
/// the bytes, so the ordering has to be in them.
#[tokio::test]
async fn echoed_headers_are_sorted_on_the_wire() {
    let addr = start("echo-a", Duration::ZERO).await;
    let (_, body) = send(
        addr,
        "GET / HTTP/1.1\r\n\
         Host: api.example.test\r\n\
         Z-Last: z\r\n\
         A-First: a\r\n\
         M-Middle: m\r\n\
         Connection: close\r\n\r\n",
    )
    .await;

    // Sliced from the `headers` object onwards: `host` is also a
    // top-level key of the frozen shape, and it is the *header* map's
    // ordering under test here.
    let (_, map) = body
        .split_once(r#""headers":{"#)
        .unwrap_or_else(|| panic!("the frozen shape ends with a headers object, got {body:?}"));
    let positions: Vec<usize> = ["\"a-first\"", "\"host\"", "\"m-middle\"", "\"z-last\""]
        .iter()
        .map(|needle| {
            map.find(needle)
                .unwrap_or_else(|| panic!("{needle} must appear in {body:?}"))
        })
        .collect();
    let mut sorted = positions.clone();
    sorted.sort_unstable();
    assert_eq!(
        positions, sorted,
        "header names must appear in sorted order in the response bytes: {body:?}"
    );
}

/// The query is deliberately not part of `path` and does not leak into
/// it -- see `src/echo.rs`'s own module documentation for the trade-off
/// and for where a query rewrite *is* recoverable (the pod's logs).
#[tokio::test]
async fn the_query_string_is_not_part_of_the_echoed_path() {
    let addr = start("echo-a", Duration::ZERO).await;
    let (_, body) = send(
        addr,
        "GET /payments?tier=gold&page=2 HTTP/1.1\r\nHost: api.example.test\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(parse(&body).path, "/payments");
}

/// The probe endpoint the fixtures' readiness probes call: `200`,
/// plain text, no JSON.
#[tokio::test]
async fn the_health_endpoint_answers_the_probe() {
    let addr = start("echo-a", Duration::ZERO).await;
    let (head, body) = send(
        addr,
        "GET /healthz HTTP/1.1\r\nHost: 10.244.0.5:8080\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "got {head:?}");
    assert!(
        head.contains("content-type: text/plain"),
        "the probe endpoint is not JSON, got {head:?}"
    );
    assert_eq!(body, "ok\n");
}

/// A real (small) sleep on a real connection: the paused-clock unit
/// tests in `src/serve.rs` prove the delay is awaited, this proves the
/// awaited delay is actually visible to a client on the other end of a
/// socket. The health endpoint is checked on the same slow backend --
/// a pod configured slow must still pass readiness, or the fixture
/// would measure "no backend" instead of "a slow backend".
#[tokio::test]
async fn a_configured_delay_is_visible_to_a_client_but_never_delays_the_probe() {
    let delay = Duration::from_millis(400);
    let addr = start("echo-a", delay).await;

    let started = Instant::now();
    let (_, body) = send(
        addr,
        "GET /payments HTTP/1.1\r\nHost: api.example.test\r\nConnection: close\r\n\r\n",
    )
    .await;
    let echoed_after = started.elapsed();
    assert_eq!(parse(&body).backend, "echo-a");
    assert!(
        echoed_after >= delay,
        "the echoed response must not arrive before the configured delay, got {echoed_after:?}"
    );

    let started = Instant::now();
    let (_, body) = send(
        addr,
        "GET /healthz HTTP/1.1\r\nHost: api.example.test\r\nConnection: close\r\n\r\n",
    )
    .await;
    let probed_after = started.elapsed();
    assert_eq!(body, "ok\n");
    assert!(
        probed_after < delay,
        "the readiness probe must not be delayed, got {probed_after:?}"
    );
}

/// One deployed backend serving both a fast and a slow probe, with no
/// rollout in between -- see `src/delay.rs`'s own module documentation
/// for why that matters.
#[tokio::test]
async fn a_per_request_delay_header_is_honored() {
    let addr = start("echo-a", Duration::ZERO).await;
    let started = Instant::now();
    let (head, _) = send(
        addr,
        &format!(
            "GET /payments HTTP/1.1\r\nHost: api.example.test\r\n{DELAY_HEADER}: 300\r\nConnection: close\r\n\r\n"
        ),
    )
    .await;
    let elapsed = started.elapsed();
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "got {head:?}");
    assert!(
        elapsed >= Duration::from_millis(300),
        "the per-request delay must be waited, got {elapsed:?}"
    );
}

/// Refused, never treated as "no delay": a timeout fixture whose delay
/// silently became zero would pass while asserting nothing.
#[tokio::test]
async fn an_unusable_delay_header_is_refused() {
    let addr = start("echo-a", Duration::ZERO).await;
    for value in ["later", "-1", "1.5", "60001"] {
        let (head, body) = send(
            addr,
            &format!(
                "GET /payments HTTP/1.1\r\nHost: api.example.test\r\n{DELAY_HEADER}: {value}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        assert!(
            head.starts_with("HTTP/1.1 400 Bad Request\r\n"),
            "{value:?} must be refused, got {head:?}"
        );
        assert!(
            body.contains(DELAY_HEADER),
            "the refusal names the header at fault, got {body:?}"
        );
    }
}

/// Two requests, one connection: proves the request body really is
/// drained rather than left unread (an unread body makes `hyper` close
/// the connection instead of reusing it -- see `src/serve.rs`'s own
/// module documentation), and that a request with a body is echoed like
/// any other.
#[tokio::test]
async fn a_body_is_drained_so_the_connection_stays_reusable() {
    let addr = start("echo-a", Duration::ZERO).await;
    let raw = send_raw(
        addr,
        "POST /orders HTTP/1.1\r\n\
         Host: api.example.test\r\n\
         Content-Length: 11\r\n\r\n\
         hello there\
         GET /orders HTTP/1.1\r\n\
         Host: api.example.test\r\n\
         Connection: close\r\n\r\n",
    )
    .await;

    assert_eq!(
        raw.matches("HTTP/1.1 200 OK").count(),
        2,
        "both requests on the reused connection must be answered: {raw:?}"
    );
    assert!(
        raw.contains(r#""method":"POST","path":"/orders""#),
        "the request with a body is echoed like any other: {raw:?}"
    );
    assert!(
        raw.contains(r#""method":"GET","path":"/orders""#),
        "the second request on the same connection is echoed: {raw:?}"
    );
    assert!(
        !raw.contains("hello there"),
        "the frozen contract has no body field: {raw:?}"
    );
}

/// The one thing a unit test cannot prove (this crate forbids
/// `unsafe_code`, and `std::env::remove_var` is `unsafe`): the real
/// binary, run with no backend id in its real environment, exits
/// non-zero instead of serving with a defaulted identity. See
/// `src/config.rs`'s own module documentation for why a defaulted
/// identity would hide exactly the regression this component exists to
/// catch.
///
/// `env_remove` matters even though the variable is not normally set:
/// the test inherits this process's environment, and a developer who
/// happened to export it would otherwise start a real server here that
/// never exits.
#[test]
fn the_binary_refuses_to_start_without_a_backend_id() {
    let assert = assert_cmd::Command::cargo_bin("admissionlab-echo")
        .expect("the binary under test is built by `cargo test`")
        .env_remove(BACKEND_ID_ENV)
        .env_remove(DELAY_MS_ENV)
        .timeout(Duration::from_secs(30))
        .assert()
        .failure();
    // `tracing_subscriber::fmt` writes to stdout by default, which is
    // where this binary's fatal errors land -- see `src/main.rs`.
    assert.stdout(predicates::str::contains(BACKEND_ID_ENV));
}

/// An empty value is a manifest that set the variable and got the value
/// wrong, which is no better than not setting it at all.
#[test]
fn the_binary_refuses_to_start_with_an_empty_backend_id() {
    assert_cmd::Command::cargo_bin("admissionlab-echo")
        .expect("the binary under test is built by `cargo test`")
        .env(BACKEND_ID_ENV, "   ")
        .env_remove(DELAY_MS_ENV)
        .timeout(Duration::from_secs(30))
        .assert()
        .failure()
        .stdout(predicates::str::contains("empty"));
}

/// A malformed delay is fatal at startup too: a backend that quietly
/// ignored it would silently be fast, and the fixture that asked for
/// slow would assert nothing.
#[test]
fn the_binary_refuses_to_start_with_a_malformed_delay() {
    assert_cmd::Command::cargo_bin("admissionlab-echo")
        .expect("the binary under test is built by `cargo test`")
        .env(BACKEND_ID_ENV, "echo-a")
        .env(DELAY_MS_ENV, "250ms")
        .timeout(Duration::from_secs(30))
        .assert()
        .failure()
        .stdout(predicates::str::contains(DELAY_MS_ENV));
}
