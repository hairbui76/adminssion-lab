//! `serve` mode: the HTTPS server behind this recipe's `Service` — a
//! health endpoint plus the three admission-review routes Task 3.9 adds.
//!
//! Task 2.7 shipped `GET /healthz` and deliberately nothing else,
//! because the webhook configuration it installed could never route a
//! real admission request here. Task 3.9 is the task that made those
//! configurations real, so the routes exist now:
//!
//! | Route | Served by | Configuration |
//! | --- | --- | --- |
//! | `GET` [`HEALTHZ_PATH`] | unchanged, still `200 OK`/`ok\n` | the pod's own probes |
//! | `POST` [`MUTATE_LABELS_PATH`] | [`crate::mutate`], [`MutationScope::Labels`] | `admissionlab-test-webhook-mutate-labels` |
//! | `POST` [`MUTATE_CONTAINERS_PATH`] | [`crate::mutate`], [`MutationScope::Workload`] | `admissionlab-test-webhook-mutate-containers` |
//! | `POST` [`VALIDATE_PATH`] | [`crate::validate`] | `admissionlab-test-webhook` |
//!
//! `GET /healthz` is byte-for-byte what it was: the readiness and
//! liveness probes in `recipes/test-webhook/manifests/30-deployment.yaml`
//! are unchanged, and this module's own `tests` still pin its status
//! and body.
//! Routing is exact-match on the (method, path) pair — anything else is
//! `404 Not Found`, including a `GET` of an admission path — so a
//! webhook configuration whose `clientConfig.service.path` drifts from
//! one of these constants fails loudly (a `404` is a webhook call
//! failure) instead of quietly admitting everything.
//! `crates/admissionlab-test-webhook/tests/behavior.rs` pins each
//! constant against the manifest that names it, so that drift is caught
//! by `cargo test` rather than by a cluster.
//!
//! # The wire types are hand-written, not borrowed
//!
//! `kube` does ship `kube::core::admission` behind its `admission`
//! feature, and this crate deliberately does not use it: that type's
//! `patch` field is a plain `Vec<u8>` with no `serde` attribute, so
//! `serde_json` renders it as a JSON *array of numbers*, while
//! Kubernetes decodes `AdmissionResponse.patch` as a Go `[]byte` — that
//! is, a base64 **string**. [`AdmissionResponse`] below instead types
//! that field as [`ByteString`], `k8s-openapi`'s own base64-coded byte
//! string, which is the same codec every `caBundle` in this recipe
//! already round-trips through ([`crate::bootstrap`]). Hand-writing the
//! four small structs also keeps this crate's dependency graph exactly
//! where Task 2.7 left it — no new workspace entry, no new feature — in
//! the same spirit as the hand-rolled `hyper` server below.
//!
//! Only the fields this webhook actually reads are declared on the
//! request side (`uid`, `object`); `serde` ignores the rest of a real
//! `AdmissionReview`, which is a large object this webhook has no
//! opinion about.
//!
//! # Failure shapes, and why they differ
//!
//! - A request whose body is not a usable `AdmissionReview` (too large,
//!   unreadable, not JSON, no `request` field) gets `400 Bad Request`
//!   with a plain-text body. There is no `uid` to answer with, so there
//!   is no well-formed admission response to send; the API server treats
//!   the non-2xx as a call failure and applies `failurePolicy`.
//! - An `AdmissionReview` that parses but carries an unusable
//!   `test.admissionlab.io/*` annotation gets a real `200 OK` denial
//!   naming the annotation — see [`crate::behavior`]'s own documentation
//!   for why a denial rather than a silent allow.
//! - [`crate::behavior::FAIL`] gets `500 Internal Server Error`. That is
//!   the controlled failure PRODUCT.md §30 asks for: a webhook that does
//!   not answer, exercising the `failurePolicy` path itself rather than
//!   the deny path.
//!
//! No framework (`axum`/`tower`) for four routes: a hand-rolled accept
//! loop using `hyper`'s HTTP/1.1 server connection builder directly,
//! matching this project's general preference for explicit,
//! minimal-dependency code over pulling in a framework (see the root
//! `Cargo.toml`'s comments on why every crate this needs was already
//! resolved transitively via `kube`).
//!
//! # Why `serve` mode never talks to the Kubernetes API
//!
//! Unlike `bootstrap` mode ([`crate::bootstrap`]), this module holds no
//! `kube::Client`, needs no `ServiceAccount` token, and reads no
//! environment variable at all (see [`crate::config`]'s own module
//! documentation): its only inputs are the certificate/key files
//! `bootstrap` mode already wrote to [`CERT_DIR`] before this container
//! ever starts (Kubernetes guarantees init containers complete first —
//! see [`crate::bootstrap`]'s module documentation), and the admission
//! request bodies the API server sends it. That is what makes this
//! webhook's behavior reproducible: an admission answer here is a pure
//! function of the object in the request (see [`crate::behavior`]),
//! never of anything this process could have read from the cluster.
//!
//! Kubernetes' own `httpGet` liveness/readiness probes never verify a
//! probed server's TLS certificate regardless (`kubelet`'s prober always
//! skips certificate verification for an HTTPS probe), so this server's
//! self-signed-chain-of-trust correctness is irrelevant to *its own*
//! readiness gating — but it is exactly what every admission-review
//! caller depends on, which is why `bootstrap` mode writes the
//! generated CA into all three webhook configurations' `caBundle`.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::{Buf, Bytes};
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::body::Body;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use k8s_openapi::ByteString;
use rustls::ServerConfig;
use rustls_pki_types::pem::PemObject as _;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::behavior::{self, Behavior};
use crate::mutate::{self, MutationScope};
use crate::validate::{self, Decision};

/// Where this mode reads `tls.crt`/`tls.key` from — the same
/// `emptyDir` mount [`crate::bootstrap`] writes them to. Deliberately an
/// independent copy of the same literal path as
/// [`crate::bootstrap::CERT_DIR`], not a shared constant either module
/// imports from the other: the two modes never share any other state,
/// and coupling them through one more item would blur the "these are
/// two independent container processes" boundary this crate's design
/// otherwise keeps clean (see [`crate::bootstrap`]'s own module
/// documentation). `tests::cert_dir_matches_bootstrap` below is the
/// actual regression check that the two literals stay in sync —
/// `bootstrap::CERT_DIR` is `pub(crate)` for exactly that test to read,
/// nothing else.
const CERT_DIR: &str = "/certs";

/// The port this mode listens on, and what
/// `recipes/test-webhook/manifests/30-deployment.yaml`'s container port
/// and `Service.spec.ports[].targetPort` both name. Not configurable:
/// nothing in this deployment ever needs a different value (see
/// [`crate::config`]'s own module documentation for why fixed
/// implementation constants, not environment variables, are this
/// crate's default for exactly this shape of value).
const PORT: u16 = 8443;

/// The health endpoint the pod's own probes call. Unchanged since Task
/// 2.7.
pub const HEALTHZ_PATH: &str = "/healthz";

/// `MutatingWebhookConfiguration` `admissionlab-test-webhook-mutate-labels`'s
/// `clientConfig.service.path`.
pub const MUTATE_LABELS_PATH: &str = "/mutate-labels";

/// `MutatingWebhookConfiguration` `admissionlab-test-webhook-mutate-containers`'s
/// `clientConfig.service.path`.
pub const MUTATE_CONTAINERS_PATH: &str = "/mutate-containers";

/// `ValidatingWebhookConfiguration` `admissionlab-test-webhook`'s
/// `clientConfig.service.path`.
pub const VALIDATE_PATH: &str = "/validate";

/// The `apiVersion` of every response this server sends, matching the
/// single entry in every webhook configuration's
/// `admissionReviewVersions`.
const ADMISSION_API_VERSION: &str = "admission.k8s.io/v1";

/// The `kind` of every request and response on the admission routes.
const ADMISSION_KIND: &str = "AdmissionReview";

/// The only `patchType` Kubernetes defines.
const JSON_PATCH: &str = "JSONPatch";

/// The largest admission-review body this server will read, in bytes.
///
/// Kubernetes' own object size ceiling is roughly 1.5 MiB, and an
/// `AdmissionReview` wraps one object plus request metadata, so 4 MiB
/// accepts every request the API server can legitimately send while
/// still bounding what a single connection can make this process
/// allocate. Enforced by streaming through [`Limited`] rather than by
/// checking `Content-Length`, which a chunked request need not send.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// The HTTP status a denial carries in `response.status.code`. `403` is
/// what the API server itself renders a webhook denial as; sending
/// anything else would make this webhook's rejections read differently
/// from every other webhook's in a captured result.
const DENIED_CODE: u16 = 403;

/// Everything that can go wrong running `serve` mode end to end.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// `tls.crt` under [`CERT_DIR`] could not be opened or contained
    /// something that did not parse as PEM. `rustls_pki_types::pem::Error`
    /// covers both a missing/unreadable file and a malformed PEM
    /// section under one type (`PemObject::pem_file_iter`'s own
    /// documentation: "errors opening the file are reported from this
    /// function directly, errors reading from the file are reported
    /// from the returned iterator") — both surface here identically,
    /// since either way `serve` mode cannot start.
    #[error("failed to read/parse certificates from {}: {source}", path.display())]
    Certificates {
        /// The file that could not be read or parsed.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: rustls_pki_types::pem::Error,
    },
    /// `tls.crt` under [`CERT_DIR`] parsed successfully but contained no
    /// PEM certificate section at all.
    #[error("{} contains no PEM certificate", path.display())]
    NoCertificate {
        /// The file that contained no certificate.
        path: PathBuf,
    },
    /// `tls.key` under [`CERT_DIR`] could not be opened, contained
    /// something that did not parse as PEM, or contained no private key
    /// section at all (`PemObject::from_pem_file`'s own
    /// `Error::NoItemsFound`, covered by the same type as every other
    /// failure here — see [`ServeError::Certificates`]'s own
    /// documentation for why that is deliberate, not a loss of
    /// information).
    #[error("failed to read/parse the private key from {}: {source}", path.display())]
    PrivateKey {
        /// The file that could not be read or parsed.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: rustls_pki_types::pem::Error,
    },
    /// The loaded certificate/key could not build a TLS server
    /// configuration (for example the key does not match the
    /// certificate's public key).
    #[error("failed to build a TLS server configuration: {0}")]
    TlsConfig(#[source] rustls::Error),
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

// ---------------------------------------------------------------------
// Wire types -- see this module's own documentation for why these are
// hand-written rather than taken from `kube::core::admission`.
// ---------------------------------------------------------------------

/// The incoming `AdmissionReview` envelope, narrowed to the one field
/// this webhook reads.
#[derive(Debug, Deserialize)]
struct AdmissionReviewRequest {
    /// Absent on a malformed request, and on the *response* half of an
    /// `AdmissionReview` — either way there is nothing to answer.
    request: Option<AdmissionRequest>,
}

/// The incoming `AdmissionReview.request`, narrowed to the two fields
/// this webhook reads: the `uid` every response must echo, and the
/// object whose annotations select the behavior.
#[derive(Debug, Deserialize)]
struct AdmissionRequest {
    /// Echoed verbatim in [`AdmissionResponse::uid`]; the API server
    /// discards a response whose `uid` does not match.
    uid: String,
    /// The object under admission. `null` for a `DELETE`, which this
    /// webhook's configurations never match, so an absent object simply
    /// selects [`Behavior::default`].
    #[serde(default)]
    object: Option<Value>,
}

/// The outgoing `AdmissionReview` envelope.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdmissionReviewResponse {
    /// Always [`ADMISSION_API_VERSION`].
    api_version: &'static str,
    /// Always [`ADMISSION_KIND`].
    kind: &'static str,
    /// The verdict.
    response: AdmissionResponse,
}

/// The outgoing `AdmissionReview.response`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdmissionResponse {
    /// Copied from [`AdmissionRequest::uid`].
    uid: String,
    /// Whether the object is admitted.
    allowed: bool,
    /// Only ever set on a denial — the API server ignores it otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<AdmissionStatus>,
    /// The RFC 6902 patch, base64-encoded on the wire by [`ByteString`].
    /// Omitted entirely when there is no mutation — see
    /// [`crate::mutate::build_patch`] for why an omitted patch and an
    /// empty patch are not interchangeable here.
    #[serde(skip_serializing_if = "Option::is_none")]
    patch: Option<ByteString>,
    /// [`JSON_PATCH`] whenever `patch` is set, absent whenever it is not.
    #[serde(skip_serializing_if = "Option::is_none")]
    patch_type: Option<&'static str>,
}

/// The `status` of a denial — a `metav1.Status`, narrowed to the two
/// fields a webhook denial actually populates.
#[derive(Debug, Serialize)]
struct AdmissionStatus {
    /// Always [`DENIED_CODE`].
    code: u16,
    /// What the client is told.
    message: String,
}

/// Which handler an incoming (method, path) pair selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// `GET` [`HEALTHZ_PATH`].
    Healthz,
    /// `POST` on one of the two mutating paths.
    Mutate(MutationScope),
    /// `POST` [`VALIDATE_PATH`].
    Validate,
}

/// Runs `serve` mode: loads the certificate/key `bootstrap` mode wrote,
/// builds a TLS server configuration from them, and serves the routes
/// listed in this module's own documentation over HTTPS on [`PORT`],
/// forever (a connection or accept failure is logged and this loop
/// continues; only a bind failure is fatal).
///
/// # Errors
///
/// Returns [`ServeError`] if the certificate/key cannot be loaded or
/// used to build a TLS configuration, or if the listening socket cannot
/// be bound. Does not return on success — the accept loop runs until the
/// process is terminated.
pub async fn run() -> Result<(), ServeError> {
    let cert_dir = Path::new(CERT_DIR);
    let certs = load_certs(&cert_dir.join("tls.crt"))?;
    let key = load_key(&cert_dir.join("tls.key"))?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(ServeError::TlsConfig)?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(ServeError::TlsConfig)?;
    let acceptor = TlsAcceptor::from(Arc::new(server_config));

    let addr = SocketAddr::from(([0, 0, 0, 0], PORT));
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|source| ServeError::Bind { addr, source })?;
    tracing::info!(%addr, "listening for HTTPS connections");

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!(%error, "failed to accept a connection; continuing");
                continue;
            }
        };
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let tls_stream = match acceptor.accept(stream).await {
                Ok(stream) => stream,
                Err(error) => {
                    tracing::debug!(%peer, %error, "TLS handshake failed");
                    return;
                }
            };
            let io = TokioIo::new(tls_stream);
            if let Err(error) = http1::Builder::new()
                .serve_connection(io, service_fn(handle))
                .await
            {
                tracing::debug!(%peer, %error, "connection error");
            }
        });
    }
}

/// Answers one request. Infallible at the HTTP layer: every failure mode
/// — an unreadable body, unparseable JSON, an unusable annotation, a
/// requested failure — is itself one of the responses documented in this
/// module's own "Failure shapes" section, never a dropped connection.
///
/// Generic over the request body type rather than fixed to
/// `hyper::body::Incoming`, so this exact function (not a hand-copied
/// reimplementation of its routing that could silently drift from it)
/// is what this module's own `tests` and
/// `crates/admissionlab-test-webhook/tests/behavior.rs` drive, with a
/// plain in-memory body and no live connection at all — the project's
/// own stated standard: write tests that would fail if the behavior
/// regressed.
///
/// # Errors
///
/// Never. The [`Infallible`] error type is `hyper::service::service_fn`'s
/// requirement, not a possibility.
pub async fn handle<B>(req: Request<B>) -> Result<Response<Full<Bytes>>, Infallible>
where
    B: Body,
    B::Data: Buf,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let Some(route) = route(req.method(), req.uri().path()) else {
        return Ok(text(StatusCode::NOT_FOUND, "not found\n"));
    };
    if route == Route::Healthz {
        return Ok(text(StatusCode::OK, "ok\n"));
    }
    Ok(admit(route, req.into_body()).await)
}

/// Exact (method, path) matching — see this module's own documentation
/// for why nothing here is a prefix or a fallback.
fn route(method: &Method, path: &str) -> Option<Route> {
    // Matched on `path` with a method guard rather than on the pair:
    // `http::Method`'s constants are not structural-match, so they
    // cannot appear in a pattern at all.
    match path {
        HEALTHZ_PATH if *method == Method::GET => Some(Route::Healthz),
        MUTATE_LABELS_PATH if *method == Method::POST => Some(Route::Mutate(MutationScope::Labels)),
        MUTATE_CONTAINERS_PATH if *method == Method::POST => {
            Some(Route::Mutate(MutationScope::Workload))
        }
        VALIDATE_PATH if *method == Method::POST => Some(Route::Validate),
        _ => None,
    }
}

/// Reads `body` as an `AdmissionReview` and answers it on `route`.
async fn admit<B>(route: Route, body: B) -> Response<Full<Bytes>>
where
    B: Body,
    B::Data: Buf,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let bytes = match Limited::new(body, MAX_BODY_BYTES).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) => {
            tracing::debug!(%error, "could not read the admission review body");
            return text(
                StatusCode::BAD_REQUEST,
                "unreadable admission review body\n",
            );
        }
    };

    let review: AdmissionReviewRequest = match serde_json::from_slice(&bytes) {
        Ok(review) => review,
        Err(error) => {
            tracing::debug!(%error, "could not parse the admission review body");
            return text(StatusCode::BAD_REQUEST, "malformed admission review\n");
        }
    };
    let Some(request) = review.request else {
        return text(StatusCode::BAD_REQUEST, "admission review has no request\n");
    };

    let object = request.object.unwrap_or(Value::Null);
    let behavior = match behavior::parse(&object) {
        Ok(behavior) => behavior,
        Err(error) => {
            // Denied, never ignored -- see `crate::behavior`'s own
            // module documentation for why a fixture's typo must not be
            // indistinguishable from a stack that stopped mutating.
            tracing::warn!(%error, "denying a fixture with an unusable behavior annotation");
            return denied(&request.uid, &error.to_string());
        }
    };

    match route {
        Route::Mutate(scope) => mutated(&request.uid, scope, &behavior, &object),
        Route::Validate => validated(&request.uid, &behavior).await,
        // `handle` answers `Route::Healthz` before ever calling this,
        // and `route` produces nothing else; this arm exists so adding a
        // future route is a compile error here rather than a silently
        // wrong answer.
        Route::Healthz => text(StatusCode::NOT_FOUND, "not found\n"),
    }
}

/// The mutating routes' answer: allowed, with a base64 JSON Patch when
/// [`crate::mutate::build_patch`] produced operations and no `patch`
/// field at all when it did not.
fn mutated(
    uid: &str,
    scope: MutationScope,
    behavior: &Behavior,
    object: &Value,
) -> Response<Full<Bytes>> {
    let ops = mutate::build_patch(scope, behavior, object);
    if ops.is_empty() {
        tracing::debug!(uid, ?scope, "no mutation for this object");
        return review(StatusCode::OK, allowed(uid));
    }

    let encoded = serde_json::to_vec(&ops)
        .expect("a patch built from serde_json values always re-serializes");
    tracing::info!(uid, ?scope, operations = ops.len(), "patching this object");
    review(
        StatusCode::OK,
        AdmissionResponse {
            patch: Some(ByteString(encoded)),
            patch_type: Some(JSON_PATCH),
            ..allowed(uid)
        },
    )
}

/// The validating route's answer, after any requested delay.
async fn validated(uid: &str, behavior: &Behavior) -> Response<Full<Bytes>> {
    match validate::evaluate(behavior).await {
        Decision::Allow => review(StatusCode::OK, allowed(uid)),
        Decision::Deny { message } => {
            tracing::info!(uid, %message, "denying this object");
            denied(uid, &message)
        }
        Decision::Fail => {
            tracing::info!(uid, "failing this request on purpose");
            text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "deliberate failure requested by test.admissionlab.io/fail\n",
            )
        }
    }
}

/// An allowed response with no patch — the base every other response
/// here is built from.
fn allowed(uid: &str) -> AdmissionResponse {
    AdmissionResponse {
        uid: uid.to_owned(),
        allowed: true,
        status: None,
        patch: None,
        patch_type: None,
    }
}

/// A denial carrying `message`.
fn denied(uid: &str, message: &str) -> Response<Full<Bytes>> {
    review(
        StatusCode::OK,
        AdmissionResponse {
            allowed: false,
            status: Some(AdmissionStatus {
                code: DENIED_CODE,
                message: message.to_owned(),
            }),
            ..allowed(uid)
        },
    )
}

/// Serializes `response` into an `AdmissionReview` envelope.
fn review(status: StatusCode, response: AdmissionResponse) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(&AdmissionReviewResponse {
        api_version: ADMISSION_API_VERSION,
        kind: ADMISSION_KIND,
        response,
    })
    .expect("this crate's own admission response types always serialize");

    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .expect("a static status/header with an owned body is always well-formed")
}

/// A plain-text response — `/healthz` and every non-admission failure
/// shape.
fn text(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from_static(body.as_bytes())))
        .expect("a static status/header/body response is always well-formed")
}

/// Reads and parses every PEM certificate in `path`, in file order, via
/// `rustls_pki_types`' own `PemObject` trait (see [`ServeError::Certificates`]'s
/// own documentation for why `rustls-pemfile` was replaced with this).
fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, ServeError> {
    let to_error = |source| ServeError::Certificates {
        path: path.to_path_buf(),
        source,
    };
    let certs = CertificateDer::pem_file_iter(path)
        .map_err(to_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(to_error)?;
    if certs.is_empty() {
        return Err(ServeError::NoCertificate {
            path: path.to_path_buf(),
        });
    }
    Ok(certs)
}

/// Reads and parses the first PEM private key in `path`.
fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>, ServeError> {
    PrivateKeyDer::from_pem_file(path).map_err(|source| ServeError::PrivateKey {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http_body_util::{BodyExt as _, Full};
    use hyper::{Method, Request, StatusCode};

    use super::handle;

    /// Calls the real [`handle`] directly with an in-memory body — no
    /// live connection, no `hyper::body::Incoming`, and no hand-copied
    /// reimplementation of its routing to drift out of sync with it (see
    /// [`handle`]'s own documentation for why it is generic over the
    /// body type specifically so this is possible).
    async fn call(method: Method, path: &str) -> (StatusCode, Vec<u8>) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .body(Full::<Bytes>::default())
            .expect("well-formed test request");
        let response = handle(req).await.expect("handle is infallible");
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
    async fn get_healthz_is_ok() {
        let (status, body) = call(Method::GET, "/healthz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"ok\n");
    }

    #[tokio::test]
    async fn post_healthz_is_not_found() {
        let (status, _) = call(Method::POST, "/healthz").await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "only GET /healthz may succeed"
        );
    }

    #[tokio::test]
    async fn get_unknown_path_is_not_found() {
        let (status, _) = call(Method::GET, "/").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_healthz_with_trailing_content_is_not_found() {
        // Exact-match only, not a prefix match -- proves the path check
        // is `==`, not `starts_with`.
        let (status, _) = call(Method::GET, "/healthz/extra").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// The admission routes are `POST`-only. A `GET` of one is a `404`,
    /// not an empty allow — a webhook configuration that somehow issued
    /// the wrong method must fail its call rather than appear to admit
    /// everything.
    #[tokio::test]
    async fn get_an_admission_path_is_not_found() {
        for path in [
            super::MUTATE_LABELS_PATH,
            super::MUTATE_CONTAINERS_PATH,
            super::VALIDATE_PATH,
        ] {
            let (status, _) = call(Method::GET, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "for {path}");
        }
    }

    /// An empty body is not an `AdmissionReview`; there is no `uid` to
    /// answer with, so this is a `400`, not a `200` allow — see this
    /// module's own "Failure shapes" documentation.
    #[tokio::test]
    async fn an_empty_admission_body_is_a_bad_request() {
        for path in [
            super::MUTATE_LABELS_PATH,
            super::MUTATE_CONTAINERS_PATH,
            super::VALIDATE_PATH,
        ] {
            let (status, _) = call(Method::POST, path).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "for {path}");
        }
    }

    /// [`super::CERT_DIR`] and `crate::bootstrap::CERT_DIR` are two
    /// independent `const` literals (see [`super::CERT_DIR`]'s own
    /// documentation for why they are not one shared constant) that
    /// must nonetheless name the same path -- `bootstrap` writes
    /// `tls.crt`/`tls.key` to its own copy, `serve` reads them back from
    /// this one. This is the actual regression check for that: if a
    /// future edit changes one literal without the other, this test
    /// fails instead of the drift only surfacing as a real pod failing
    /// to find its certificate at runtime.
    #[test]
    fn cert_dir_matches_bootstrap() {
        assert_eq!(super::CERT_DIR, crate::bootstrap::CERT_DIR);
    }
}
