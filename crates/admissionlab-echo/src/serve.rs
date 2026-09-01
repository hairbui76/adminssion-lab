//! The plain-HTTP listener: routing, the health endpoint, and the
//! accept loop.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET` [`HEALTHZ_PATH`] | `200 OK`, `ok\n`, `text/plain` -- never JSON, never delayed |
//! | anything else | `200 OK` with the frozen [`crate::echo::EchoBody`], after any delay |
//! | anything else with an unusable [`crate::delay::DELAY_HEADER`] | `400 Bad Request`, plain text |
//!
//! # Why there is no 404
//!
//! An echo backend that rejected unknown paths would be unable to
//! answer the one question Phase 6 asks it -- "which workload did this
//! route actually reach?" -- for any path a fixture had not been
//! written for in advance. Every method and every path is echoed, which
//! is also what makes these two Deployments reusable by every future
//! `HTTPRoute` fixture without touching this crate.
//!
//! `GET /healthz` is the single reserved (method, path) pair, matched
//! exactly (not as a prefix), because the pod's own readiness and
//! liveness probes need one endpoint whose answer does not depend on
//! configuration. A `POST /healthz`, or a `GET /healthz/extra`, is an
//! ordinary echo -- so a fixture that genuinely wants to route traffic
//! at `/healthz` can, and only the exact probe request is intercepted.
//! Fixtures should nonetheless avoid routing probe traffic at
//! `GET /healthz` for the obvious reason: it would be answered by this
//! branch and carry no backend id.
//!
//! # Why the health endpoint is never delayed
//!
//! [`crate::delay`] applies to echoed responses only. A backend
//! deployed with a two-second delay so a fixture can exercise a Gateway
//! timeout must still pass its own readiness probe -- otherwise
//! configuring the delay would take the pod out of the Service's
//! endpoints and the fixture would measure "no backend at all" rather
//! than "a slow backend", which are different regressions with
//! different causes.
//!
//! # The request body is read and discarded
//!
//! The frozen contract has no body field, so nothing here inspects
//! what was sent. The body is still drained (up to
//! [`MAX_BODY_BYTES`]) rather than ignored: an unread body makes
//! `hyper` close the connection instead of reusing it, and a client
//! still writing a large request when that happens sees a connection
//! reset rather than the `200` this backend actually produced -- a
//! failure a probe would report as a data-plane error. Draining through
//! `Limited` bounds what one connection can make this process allocate,
//! and does so without trusting a `Content-Length` a chunked request
//! need not send.
//!
//! # No framework, and no TLS
//!
//! A hand-rolled accept loop over `hyper`'s HTTP/1.1 server connection
//! builder, matching `admissionlab-test-webhook`'s own `serve` module
//! and this project's general preference for explicit, minimal-
//! dependency code over a framework for two routes. Plain HTTP, because
//! this workload sits behind the Gateway under test -- see [`crate`]'s
//! own documentation.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::{Buf, Bytes};
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::body::Body;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::config::EchoConfig;
use crate::delay;
use crate::echo::EchoBody;

/// The port this server listens on, and what
/// `fixtures/gateway/backends/echo-a.yaml`'s container port and
/// `Service.spec.ports[].targetPort` both name. Not configurable:
/// nothing in these fixtures ever needs a different value, and an
/// environment variable for it would only be a way for the manifest and
/// the binary to disagree (see [`crate::config`]'s own module
/// documentation). Tests bind their own ephemeral port through
/// [`serve_on`] instead, which is why that function takes a listener
/// rather than a port.
pub const PORT: u16 = 8080;

/// The health endpoint the pod's own probes call.
pub const HEALTHZ_PATH: &str = "/healthz";

/// The largest request body this server will read before discarding it.
///
/// A megabyte is far more than any Gateway probe sends and small enough
/// that a hostile or buggy client cannot make this process buffer
/// meaningfully; the body is never used for anything -- see this
/// module's own documentation.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Something went wrong starting the server. Serving itself is
/// infallible at the HTTP layer -- every request has an answer -- so
/// the only fatal error is being unable to listen at all.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The listening socket could not be bound.
    #[error("failed to bind {addr}: {source}")]
    Bind {
        /// The address that could not be bound.
        addr: SocketAddr,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

/// Binds [`PORT`] on every interface and serves forever.
///
/// # Errors
///
/// Returns [`ServeError::Bind`] if the listening socket cannot be
/// bound. Does not return on success -- the accept loop runs until the
/// process is terminated.
pub async fn run(config: EchoConfig) -> Result<(), ServeError> {
    let addr = SocketAddr::from(([0, 0, 0, 0], PORT));
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|source| ServeError::Bind { addr, source })?;
    tracing::info!(
        %addr,
        backend = %config.backend_id,
        default_delay_ms = u64::try_from(config.default_delay.as_millis()).unwrap_or(u64::MAX),
        "listening for HTTP connections"
    );
    serve_on(listener, Arc::new(config)).await;
    Ok(())
}

/// Serves `listener` forever, answering every connection with
/// [`handle`].
///
/// Takes an already-bound listener so a test can serve on an ephemeral
/// port (`127.0.0.1:0`) and drive the real accept loop, rather than a
/// hand-copied reimplementation of it that could drift from what the
/// container actually runs. An accept failure is logged and the loop
/// continues; this function does not return.
pub async fn serve_on(listener: TcpListener, config: Arc<EchoConfig>) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!(%error, "failed to accept a connection; continuing");
                continue;
            }
        };
        let config = Arc::clone(&config);
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(move |req| {
                let config = Arc::clone(&config);
                async move { handle(req, &config).await }
            });
            if let Err(error) = http1::Builder::new().serve_connection(io, service).await {
                tracing::debug!(%peer, %error, "connection error");
            }
        });
    }
}

/// Answers one request, per the table in this module's own
/// documentation.
///
/// Generic over the request body type rather than fixed to
/// `hyper::body::Incoming`, so this exact function is what unit tests
/// drive with a plain in-memory body and no live connection at all --
/// the same reasoning `admissionlab-test-webhook`'s own `serve::handle`
/// records.
///
/// # Errors
///
/// Never. The [`Infallible`] error type is
/// `hyper::service::service_fn`'s requirement, not a possibility: an
/// unusable delay header is a `400` response, not an error.
///
/// # Panics
///
/// Never in practice. [`EchoBody`] is five owned `String`s and a
/// `BTreeMap<String, String>` -- a shape `serde_json` cannot fail to
/// serialize (it has no non-string map keys, no floats, and no custom
/// `Serialize` implementation that could return an error), so the
/// `expect` on that serialization is unreachable rather than a
/// swallowed failure mode. It is an `expect` rather than a fallible
/// path because a `500` produced by an impossible branch would be a
/// worse answer than a panic: a probe would record it as a data-plane
/// failure of the Gateway under test.
pub async fn handle<B>(
    req: Request<B>,
    config: &EchoConfig,
) -> Result<Response<Full<Bytes>>, Infallible>
where
    B: Body,
    B::Data: Buf,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if req.method() == Method::GET && req.uri().path() == HEALTHZ_PATH {
        return Ok(text(StatusCode::OK, "ok\n".to_owned()));
    }

    let (parts, body) = req.into_parts();
    // Logged with the *full* request target, query included, which the
    // frozen response body deliberately does not carry -- see
    // `crate::echo`'s own module documentation.
    tracing::info!(
        method = %parts.method,
        target = %parts.uri,
        backend = %config.backend_id,
        "echoing a request"
    );

    let delay = match delay::resolve(&parts.headers, config) {
        Ok(delay) => delay,
        Err(error) => {
            // Refused, never silently ignored: a timeout fixture whose
            // delay quietly became zero would assert nothing at all
            // (see `crate::delay`'s own module documentation).
            tracing::warn!(%error, "refusing a request with an unusable delay header");
            return Ok(text(StatusCode::BAD_REQUEST, format!("{error}\n")));
        }
    };

    drain(body).await;
    delay::apply(delay).await;

    let echo = EchoBody::build(
        &config.backend_id,
        &parts.method,
        &parts.uri,
        &parts.headers,
    );
    let json = serde_json::to_vec(&echo).expect("the frozen echo body always serializes");
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(json)))
        .expect("a static status/header with an owned body is always well-formed"))
}

/// Reads and discards up to [`MAX_BODY_BYTES`] of `body` -- see this
/// module's own documentation for why an unread body is worth
/// draining. A body that exceeds the cap, or that fails mid-read, is
/// logged and otherwise ignored: the response does not depend on it.
async fn drain<B>(body: B)
where
    B: Body,
    B::Data: Buf,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if let Err(error) = Limited::new(body, MAX_BODY_BYTES).collect().await {
        tracing::debug!(%error, "could not read the request body; echoing anyway");
    }
}

/// A plain-text response -- the health endpoint and the one failure
/// shape.
fn text(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(body)))
        .expect("a static status/header with an owned body is always well-formed")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use http_body_util::{BodyExt as _, Full};
    use hyper::{Method, Request, StatusCode};
    use tokio::time::Instant;

    use super::{HEALTHZ_PATH, handle};
    use crate::config::EchoConfig;
    use crate::delay::{DELAY_HEADER, MAX_DELAY_MS};
    use crate::echo::EchoBody;

    fn config(default_delay_ms: u64) -> EchoConfig {
        EchoConfig {
            backend_id: "echo-a".to_owned(),
            default_delay: Duration::from_millis(default_delay_ms),
        }
    }

    /// Calls the real [`handle`] with an in-memory body -- no live
    /// connection and no hand-copied routing to drift out of sync (see
    /// [`handle`]'s own documentation).
    async fn call(
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        config: &EchoConfig,
    ) -> (StatusCode, Vec<u8>) {
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let req = builder
            .body(Full::<Bytes>::default())
            .expect("well-formed test request");
        let response = handle(req, config).await.expect("handle is infallible");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("collecting a Full body cannot fail")
            .to_bytes()
            .to_vec();
        (status, body)
    }

    #[tokio::test]
    async fn get_healthz_is_plain_text_ok() {
        let (status, body) = call(Method::GET, HEALTHZ_PATH, &[], &config(0)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"ok\n", "the probe endpoint is never JSON");
    }

    /// Only the exact `GET /healthz` pair is reserved -- see this
    /// module's own documentation.
    #[tokio::test]
    async fn other_requests_at_the_health_path_are_echoed() {
        for (method, path) in [
            (Method::POST, HEALTHZ_PATH),
            (Method::GET, "/healthz/extra"),
        ] {
            let (status, body) = call(method.clone(), path, &[], &config(0)).await;
            assert_eq!(status, StatusCode::OK);
            let echo: EchoBody =
                serde_json::from_slice(&body).expect("every non-probe request is echoed as JSON");
            assert_eq!(echo.method, method.as_str());
            assert_eq!(echo.path, path);
        }
    }

    /// There is no 404: an unknown path is exactly what a routing
    /// fixture needs echoed back.
    #[tokio::test]
    async fn an_arbitrary_path_is_echoed_with_this_backends_id() {
        let (status, body) = call(
            Method::PUT,
            "/anything/at/all",
            &[("host", "api.example.test")],
            &config(0),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let echo: EchoBody = serde_json::from_slice(&body).expect("echo responses are JSON");
        assert_eq!(echo.backend, "echo-a");
        assert_eq!(echo.method, "PUT");
        assert_eq!(echo.path, "/anything/at/all");
        assert_eq!(echo.host, "api.example.test");
    }

    /// Tokio's paused clock: the full configured delay is observed with
    /// no wall-clock cost.
    #[tokio::test(start_paused = true)]
    async fn the_environment_delay_is_applied_to_an_echoed_response() {
        let started = Instant::now();
        let (status, _) = call(Method::GET, "/payments", &[], &config(MAX_DELAY_MS)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(started.elapsed(), Duration::from_millis(MAX_DELAY_MS));
    }

    #[tokio::test(start_paused = true)]
    async fn the_request_header_overrides_the_environment_delay() {
        let started = Instant::now();
        let (status, _) = call(
            Method::GET,
            "/payments",
            &[(DELAY_HEADER, "1500")],
            &config(250),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(started.elapsed(), Duration::from_millis(1500));
    }

    /// A slow backend must still pass its own readiness probe -- see
    /// this module's own documentation.
    #[tokio::test(start_paused = true)]
    async fn the_health_endpoint_is_never_delayed() {
        let started = Instant::now();
        let (status, _) = call(Method::GET, HEALTHZ_PATH, &[], &config(MAX_DELAY_MS)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unusable_delay_header_is_a_bad_request_not_a_silent_zero() {
        let started = Instant::now();
        let (status, body) = call(
            Method::GET,
            "/payments",
            &[(DELAY_HEADER, "later")],
            &config(0),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            started.elapsed(),
            Duration::ZERO,
            "a refused request is refused immediately"
        );
        let message = String::from_utf8(body).expect("the failure body is text");
        assert!(
            message.contains(DELAY_HEADER),
            "the refusal names the header at fault, got {message:?}"
        );
    }
}
