//! Sending one real HTTP request through a Gateway's data plane and
//! recording what came back (ROADMAP Task 6.8).
//!
//! [`execute_http_probe`] is the whole of it: connect to the local
//! address a Task 6.7 port-forward bound, send the request an
//! [`HttpProbeContract`] describes, and return an [`HttpProbeResult`].
//! It reports what happened; it never judges it. A `403` is a result, a
//! backend that could not be identified is `None`, and only the
//! *inability to obtain a response at all* is an error.
//!
//! # The client, and why it is `hyper`'s low-level one
//!
//! `hyper::client::conn::http1` — one TCP connection, one handshake, one
//! request — rather than a high-level client. Two of Task 6.8's own
//! requirements are properties of this choice rather than settings on
//! top of it:
//!
//! - **Redirects are not followed** because there is nothing here that
//!   could follow one. `hyper::client::conn` has no redirect logic at
//!   all, so a `302` is reported as a `302` with its `Location` header
//!   intact. A high-level client would follow redirects by default, and
//!   "we remembered to turn it off" is a weaker guarantee than "there is
//!   no code that does it". Task 8.x compares redirect behavior across
//!   implementations, which only works if the probe records the redirect
//!   rather than resolving it.
//! - **The retry rule needs the seam.** "Retry only a connection that is
//!   not ready yet" requires telling *the connection failed before any
//!   response* apart from *the server answered 503*, and that
//!   distinction is exactly the boundary between `handshake`/
//!   `send_request` and the `Response` they produce.
//!
//! See this crate's `Cargo.toml` for why `hyper` rather than `reqwest`
//! (which the workspace tech-stack line names but `Cargo.lock` does not
//! yet resolve).
//!
//! Each attempt opens its own connection and closes it afterwards.
//! Nothing is pooled: a probe measures one request through a data plane,
//! and a reused connection would let one probe's keep-alive state change
//! what the next one observes.
//!
//! # What gets retried, and what never does
//!
//! Only a connection that is not ready yet, and only within
//! [`PROBE_READINESS_WINDOW`]. Concretely: any failure *before* a
//! response is received — the TCP connect was refused or reset, the
//! HTTP/1.1 handshake failed, the connection closed while the request
//! was being sent. This window exists because a port-forward being up
//! does not mean the Gateway behind it is: `kubectl` binds its local
//! socket as soon as the API server accepts the upgrade, and the first
//! connection through it can be refused while the data plane's listener
//! is still being programmed.
//!
//! **A response is never retried, whatever it says.** A `500` and a
//! `404` are observations, and retrying one until it turned into a `200`
//! would replace the measurement with a search for the answer the
//! contract wanted. [`HttpProbeResult::attempts`] is the real count,
//! including the successful attempt, so a `1` and a `2` are
//! distinguishable facts rather than a normalized constant.
//!
//! Two bounds, both explicit (Global Constraint 13):
//! [`PROBE_REQUEST_TIMEOUT`] bounds one attempt, so a data plane that
//! accepts a connection and then never answers cannot hang a run; and
//! [`PROBE_READINESS_WINDOW`] bounds how long attempts are repeated.
//!
//! # Identifying the backend
//!
//! [`HttpProbeResult::backend`] is filled **only** when the response
//! carries a JSON content type *and* its body matches the echo backend's
//! frozen shape. Everything else is `None`: a plain-text `404` from the
//! Gateway itself, an HTML error page, a JSON document from some other
//! service. `None` means "which workload answered is unknown", never "no
//! workload answered" and never a guess (Global Constraint 15) — Task
//! 6.9 turns a *change* in this field into `traffic_backend_changed`, so
//! a fabricated value here would fabricate a regression.
//!
//! **The wire contract is shared with `admissionlab-echo`; the Rust type
//! is not.** [`EchoBody`] here is a second declaration of the same five
//! keys `admissionlab_echo::echo::EchoBody` declares, rather than an
//! import of it. That crate is a *deployed workload* — it exists to be
//! built into a container image and run inside the cluster under test —
//! and making the comparison engine depend on it at build time would
//! couple the engine's dependency graph to a workload's. What keeps the
//! two from drifting is not a shared type but a test: `tests/probe.rs`
//! runs the real `admissionlab_echo::serve::serve_on` on an ephemeral
//! port and probes it, so this parser is asserted against the bytes that
//! server actually writes. A shared type would have made them compile
//! together; the test makes them *agree*, which is the property that
//! matters.
//!
//! **Extra keys are tolerated; all five are required.** The five keys
//! are what distinguish an echo answer from arbitrary JSON: a body of
//! `{"backend": "something"}` from an unrelated service must not be read
//! as an echo answer, and requiring `method`/`path`/`host`/`headers`
//! alongside it is what prevents that. Unknown keys are ignored rather
//! than rejected, because the failure modes are asymmetric: a future
//! echo that added a sixth field would, under `deny_unknown_fields`,
//! make every probe report `backend: None` — which reads as "the request
//! reached no identifiable workload", a false regression, from a change
//! that broke nothing.
//!
//! # Response headers
//!
//! Lowercased, in a [`BTreeMap`], with a repeated header joined by
//! `", "` — the same three rules, for the same reasons,
//! `admissionlab_echo::echo` applies to the request headers it echoes
//! (RFC 9110 §5.3 defines that join as equivalent, and neither header
//! order nor letter case is semantically meaningful, so neither should
//! read as a difference to a comparator).
//!
//! Nothing is filtered out — not `date`, not `server`, not
//! `transfer-encoding`. These do differ between implementations and will
//! make a naive header comparison noisy, but that is a question for Task
//! 6.9's comparator, which can choose to ignore a header; it cannot
//! recover one this module discarded. The same argument
//! `admissionlab_echo::echo` records for proxy-injected request headers.
//!
//! # Redaction
//!
//! An [`HttpProbeContract`]'s `headers` can carry an `Authorization` or
//! a `Cookie` — that is a legitimate thing to write in a fixture that
//! probes an authenticated route. Those *values* must never reach a
//! report, a log, or an error message (PRODUCT.md §29.3 / Global
//! Constraint 14), so every place this module describes a request goes
//! through [`describe_probe_request`], which renders them as
//! `admissionlab_core::RedactedValue::Sensitive` — a variant that
//! carries no payload, so there is nothing left to leak. The header
//! itself still reaches the server, exactly as
//! `admissionlab_core::CommandSpec` sends an unredacted environment to a
//! child while its `CommandContext` cannot.
//!
//! [`HttpProbeResult`] contains no copy of the request at all, so the
//! stored evidence path has nothing to redact in the first place — the
//! strongest version of this guarantee available.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use admissionlab_core::{RedactedValue, sha256_hex};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty};
use hyper::body::Incoming;
use hyper::header::{CONTENT_TYPE, HOST, HeaderName, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::net::TcpStream;

use crate::error::GatewayError;
use crate::model::HttpProbeContract;

/// The largest response body a probe will read.
///
/// One mebibyte, deliberately the same value
/// `admissionlab_echo::serve`'s own `MAX_BODY_BYTES` uses for the
/// request bodies it drains: the two caps face each other across the
/// same connection, and picking different numbers would only invite a
/// fixture that works in one direction and not the other.
///
/// A body larger than this is [`GatewayError::ProbeBodyTooLarge`], not a
/// truncated hash — see that variant for why a hash of a prefix would be
/// worse than an error.
pub const MAX_PROBE_BODY_BYTES: usize = 1024 * 1024;

/// How long [`execute_http_probe`] keeps retrying a connection that is
/// not ready yet. See this module's "What gets retried" section.
pub const PROBE_READINESS_WINDOW: Duration = Duration::from_secs(5);

/// How long to wait between two connection attempts.
pub const PROBE_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// How long one attempt may take, from opening the TCP connection to
/// having read the whole response body.
///
/// Bounds a *single* attempt; [`PROBE_READINESS_WINDOW`] separately
/// bounds how long attempts are repeated. Thirty seconds is far longer
/// than any Gateway fixture's own latency expectation and short enough
/// that a data plane which accepts a connection and then goes silent
/// fails the probe instead of hanging the run.
pub const PROBE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The request headers whose *values* are never rendered into a report,
/// a log, or an error message. Lowercase, matching the normalized form
/// header names are compared in.
///
/// Exactly PRODUCT.md §29.3's two HTTP credential carriers. Not a
/// substring heuristic like
/// `admissionlab_core::env_key_looks_sensitive`'s: an HTTP header name
/// is drawn from a registered, well-known vocabulary rather than being
/// invented per deployment, so an exact list is both complete for what
/// it covers and free of the false positives a substring match would
/// produce (`x-request-id` contains no marker; `proxy-authorization`
/// would need one anyway — and does, below).
pub const REDACTED_REQUEST_HEADERS: [&str; 3] = ["authorization", "cookie", "proxy-authorization"];

/// What one probe request through a Gateway's data plane returned.
///
/// `Serialize` but not `Deserialize`, the same one-way asymmetry
/// [`crate::reconcile::ReconciliationEvidence`] and
/// `admissionlab_admission::AdmissionOutcome` document: this is
/// something Admission Lab observed, and it only ever travels outward
/// into a run's report.
///
/// Carries no copy of the request — see this module's "Redaction"
/// section.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HttpProbeResult {
    /// The HTTP status code the data plane returned.
    pub status: u16,
    /// Which workload answered, when the response identified itself as
    /// an Admission Lab echo backend. `None` means unknown — never
    /// guessed. See this module's "Identifying the backend" section.
    pub backend: Option<String>,
    /// Every response header, lowercased and sorted, with a repeated
    /// header joined by `", "`. Nothing is filtered — see this module's
    /// "Response headers" section.
    pub response_headers: BTreeMap<String, String>,
    /// The SHA-256 of the response body, lowercase hex, computed by
    /// `admissionlab_core::sha256_hex` over the bytes exactly as
    /// received. An empty body hashes to SHA-256 of the empty string,
    /// which is a real hash of a real (empty) body rather than a
    /// sentinel.
    pub response_body_sha256: String,
    /// Wall-clock time from the first connection attempt to the last
    /// byte of the response body, measured with [`Instant`] so it is
    /// monotonic. Includes every retried attempt: this is how long the
    /// probe took, not how long the successful attempt took.
    #[serde(serialize_with = "serialize_duration_millis")]
    #[schemars(with = "u64")]
    pub elapsed: Duration,
    /// How many connection attempts were made, including the one that
    /// produced this response. `1` for a probe that connected first
    /// time. See this module's "What gets retried" section for why this
    /// is reported rather than normalized away.
    pub attempts: u32,
}

/// The echo backend's frozen response body, as this module parses it.
///
/// A second declaration of `admissionlab_echo::echo::EchoBody`'s five
/// keys rather than an import of that type — see this module's
/// "Identifying the backend" section for why, and for what keeps the two
/// in step.
///
/// Every field is required (that is what distinguishes an echo answer
/// from arbitrary JSON); unknown fields are tolerated (no
/// `deny_unknown_fields`, deliberately).
#[derive(Debug, Deserialize)]
struct EchoBody {
    backend: String,
    #[allow(dead_code, reason = "required for the shape to match, never read")]
    method: String,
    #[allow(dead_code, reason = "required for the shape to match, never read")]
    path: String,
    #[allow(dead_code, reason = "required for the shape to match, never read")]
    host: String,
    #[allow(dead_code, reason = "required for the shape to match, never read")]
    headers: BTreeMap<String, String>,
}

/// Sends `contract`'s request to `endpoint` and reports what came back.
///
/// `endpoint` is a *local* address — the one
/// [`crate::port_forward::PortForwardHandle::local_addr`] bound — while
/// the `Host` header is `contract.host`, which is what a Gateway
/// listener's `hostname` and an `HTTPRoute`'s `hostnames` are actually
/// matched against. Those two being different is the entire point: the
/// request has to *arrive* at 127.0.0.1 and be *routed* as
/// `api.example.test`.
///
/// # Errors
///
/// Returns [`GatewayError::ProbeRequestInvalid`] if `contract` cannot be
/// turned into an HTTP request, [`GatewayError::ProbeUnavailable`] if no
/// response could be obtained within [`PROBE_READINESS_WINDOW`] or the
/// body could not be read, and [`GatewayError::ProbeBodyTooLarge`] if
/// the response body exceeds [`MAX_PROBE_BODY_BYTES`]. An HTTP error
/// status is never an error here.
pub async fn execute_http_probe(
    endpoint: SocketAddr,
    contract: &HttpProbeContract,
) -> Result<HttpProbeResult, GatewayError> {
    let described = describe_probe_request(contract);
    // Built once, up front: a contract that cannot be turned into a
    // request is a configuration error, and retrying it five seconds'
    // worth of times would only delay saying so.
    build_request(contract, &described)?;

    let started = Instant::now();
    let deadline = started + PROBE_READINESS_WINDOW;
    let mut attempts: u32 = 0;

    loop {
        attempts = attempts.saturating_add(1);
        let request = build_request(contract, &described)?;
        match attempt(endpoint, request).await {
            Ok(response) => {
                let (status, headers, body) = read_response(endpoint, &described, response).await?;
                return Ok(HttpProbeResult {
                    status,
                    backend: parse_backend(&headers, &body),
                    response_headers: headers,
                    response_body_sha256: sha256_hex(&body),
                    elapsed: started.elapsed(),
                    attempts,
                });
            }
            Err(not_ready) => {
                if Instant::now() + PROBE_RETRY_INTERVAL >= deadline {
                    return Err(GatewayError::ProbeUnavailable {
                        endpoint: endpoint.to_string(),
                        request: described,
                        reason: format!(
                            "the connection was not ready within {PROBE_READINESS_WINDOW:?}: \
                             {not_ready}"
                        ),
                        attempts,
                    });
                }
                tokio::time::sleep(PROBE_RETRY_INTERVAL).await;
            }
        }
    }
}

/// One attempt: connect, handshake, send. `Err(reason)` means *no
/// response was received*, which is the only thing this module retries.
///
/// The connection task is spawned and then dropped along with the
/// response: `hyper`'s low-level client needs the connection future
/// driven concurrently with the request, and nothing here wants to reuse
/// it afterwards.
async fn attempt(
    endpoint: SocketAddr,
    request: Request<Empty<Bytes>>,
) -> Result<Response<Incoming>, String> {
    let attempt = async {
        let stream = TcpStream::connect(endpoint)
            .await
            .map_err(|source| format!("could not connect: {source}"))?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|source| format!("HTTP/1.1 handshake failed: {source}"))?;
        // Driven on its own task for as long as the response body is
        // being read; it ends when the connection closes. Its result is
        // deliberately ignored: a connection that ends after the
        // response has been read is normal, and a connection that fails
        // before then surfaces on `send_request` or on the body read.
        tokio::spawn(async move {
            let _ = connection.await;
        });
        sender
            .send_request(request)
            .await
            .map_err(|source| format!("the request could not be sent: {source}"))
    };

    match tokio::time::timeout(PROBE_REQUEST_TIMEOUT, attempt).await {
        Ok(result) => result,
        Err(_elapsed) => Err(format!("no response within {PROBE_REQUEST_TIMEOUT:?}")),
    }
}

/// Reads a response's status, normalized headers, and body.
///
/// Past this point nothing is retried: the server has answered, and
/// whatever it answered is the observation.
async fn read_response(
    endpoint: SocketAddr,
    described: &str,
    response: Response<Incoming>,
) -> Result<(u16, BTreeMap<String, String>, Vec<u8>), GatewayError> {
    let status = response.status().as_u16();
    let headers = normalize_headers(response.headers());

    let mut body = response.into_body();
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|source| GatewayError::ProbeUnavailable {
            endpoint: endpoint.to_string(),
            request: described.to_owned(),
            reason: format!("the response body could not be read: {source}"),
            // The response arrived, so exactly one attempt reached this
            // far. Reported rather than left out: a body that failed
            // mid-read is a different failure from a connection that was
            // never ready.
            attempts: 1,
        })?;
        if let Some(chunk) = frame.data_ref() {
            if bytes.len() + chunk.len() > MAX_PROBE_BODY_BYTES {
                return Err(GatewayError::ProbeBodyTooLarge {
                    endpoint: endpoint.to_string(),
                    request: described.to_owned(),
                    limit: MAX_PROBE_BODY_BYTES,
                });
            }
            bytes.extend_from_slice(chunk);
        }
    }

    Ok((status, headers, bytes))
}

/// Builds the request one attempt sends.
///
/// `Host` comes from [`HttpProbeContract::host`] and the URI is
/// origin-form (the path alone), which is what an HTTP/1.1 request to a
/// forwarded local socket looks like on the wire. `hyper`'s low-level
/// client adds no `Host` of its own, so this is the only one sent.
///
/// A `host` entry in [`HttpProbeContract::headers`] is rejected rather
/// than merged: two `Host` headers is not a request RFC 9110 allows, and
/// silently letting one win would make the probe measure a hostname the
/// contract does not obviously name.
fn build_request(
    contract: &HttpProbeContract,
    described: &str,
) -> Result<Request<Empty<Bytes>>, GatewayError> {
    let invalid = |reason: String| GatewayError::ProbeRequestInvalid {
        request: described.to_owned(),
        reason,
    };

    let method = Method::from_bytes(contract.method.as_bytes()).map_err(|source| {
        invalid(format!(
            "{:?} is not a valid HTTP method: {source}",
            contract.method
        ))
    })?;
    let host = HeaderValue::from_str(&contract.host).map_err(|source| {
        invalid(format!(
            "{:?} is not a valid Host header: {source}",
            contract.host
        ))
    })?;

    let mut builder = Request::builder()
        .method(method)
        .uri(contract.path.as_str())
        .header(HOST, host);

    for (name, value) in &contract.headers {
        let header = HeaderName::from_bytes(name.as_bytes())
            .map_err(|source| invalid(format!("{name:?} is not a valid header name: {source}")))?;
        if header == HOST {
            return Err(invalid(
                "a `host` entry in `headers` would send a second Host header; set the contract's \
                 own `host` field instead"
                    .to_owned(),
            ));
        }
        let value = HeaderValue::from_str(value).map_err(|source| {
            // The *value* is never quoted into this message: it is
            // exactly what might be a credential.
            invalid(format!(
                "the value of {name:?} is not a valid header value: {source}"
            ))
        })?;
        builder = builder.header(header, value);
    }

    builder
        .body(Empty::<Bytes>::new())
        .map_err(|source| invalid(format!("{source}")))
}

/// Applies this module's documented normalization to a response's
/// headers.
fn normalize_headers(headers: &hyper::HeaderMap) -> BTreeMap<String, String> {
    headers
        .keys()
        .map(|name| {
            let joined = headers
                .get_all(name)
                .iter()
                // Lossy for the reason `admissionlab_echo::echo::decode`
                // gives: a header value is bytes and a report is text,
                // and dropping a header because one byte was not UTF-8
                // would destroy evidence that it arrived.
                .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
                .collect::<Vec<_>>()
                .join(", ");
            (name.as_str().to_owned(), joined)
        })
        .collect()
}

/// Reads the echo backend's identity out of a response, or `None`.
///
/// Two gates, both required: the response must declare a JSON content
/// type, and the body must parse as the frozen five-key shape. See this
/// module's "Identifying the backend" section.
fn parse_backend(headers: &BTreeMap<String, String>, body: &[u8]) -> Option<String> {
    if !is_json_content_type(headers.get(CONTENT_TYPE.as_str())?) {
        return None;
    }
    serde_json::from_slice::<EchoBody>(body)
        .ok()
        .map(|echo| echo.backend)
}

/// Whether a `Content-Type` value names JSON.
///
/// Matches the media type only, ignoring parameters (`application/json;
/// charset=utf-8` is JSON) and case (RFC 9110 §8.3.1 makes the type and
/// subtype case-insensitive). `+json` structured suffixes count too: a
/// backend answering `application/problem+json` is still returning JSON
/// this parser can read, and refusing to look would only lose an
/// identification the body could have supplied.
fn is_json_content_type(value: &str) -> bool {
    let media_type = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    media_type == "application/json" || media_type.ends_with("+json")
}

/// Describes `contract`'s request for a log, a report, or an error, with
/// every [`REDACTED_REQUEST_HEADERS`] value replaced.
///
/// The only way this module ever renders a request. See its "Redaction"
/// section, and [`redacted_probe_headers`] for the structured form a
/// [`admissionlab_core::Diagnostic`] would carry.
#[must_use]
pub fn describe_probe_request(contract: &HttpProbeContract) -> String {
    let headers = redacted_probe_headers(contract)
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>();
    format!(
        "{} http://{}{}{}",
        contract.method,
        contract.host,
        contract.path,
        if headers.is_empty() {
            String::new()
        } else {
            format!(" (headers: {})", headers.join(", "))
        }
    )
}

/// `contract`'s request headers, with every credential-carrying value
/// replaced by `admissionlab_core::RedactedValue::Sensitive` — which
/// holds no payload, so the raw value is never copied into the returned
/// map and cannot leak from it however it is formatted or serialized.
///
/// Header names are lowercased so the match is against the normalized
/// form: HTTP field names are case-insensitive, and a contract writing
/// `Authorization` must be redacted exactly as one writing
/// `authorization` is.
///
/// The `Host` header is included, from [`HttpProbeContract::host`],
/// because it is part of the request being described and is never
/// credential-bearing.
#[must_use]
pub fn redacted_probe_headers(contract: &HttpProbeContract) -> BTreeMap<String, RedactedValue> {
    let mut redacted = BTreeMap::new();
    redacted.insert(
        HOST.as_str().to_owned(),
        RedactedValue::Public(contract.host.clone()),
    );
    for (name, value) in &contract.headers {
        let name = name.to_ascii_lowercase();
        let rendered = if REDACTED_REQUEST_HEADERS.contains(&name.as_str()) {
            RedactedValue::Sensitive
        } else {
            RedactedValue::Public(value.clone())
        };
        redacted.insert(name, rendered);
    }
    redacted
}

/// Whether `status` is a code Gateway API implementations use for a
/// redirect. Exposed so a later task can ask the question without
/// re-deriving the list; this module itself never follows one.
#[must_use]
pub fn is_redirect(status: u16) -> bool {
    StatusCode::from_u16(status).is_ok_and(|status| status.is_redirection())
}

/// Serializes a [`Duration`] as a plain integer number of milliseconds.
///
/// The fourth copy of this three-line helper in the workspace, after
/// `admissionlab_admission::outcome`, `admissionlab_spec::model`, and
/// [`crate::reconcile`]'s own — kept local for the reason that module
/// records rather than promoted to a shared crate for three lines.
fn serialize_duration_millis<S: serde::Serializer>(
    value: &Duration,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let millis = u64::try_from(value.as_millis()).unwrap_or(u64::MAX);
    serializer.serialize_u64(millis)
}
