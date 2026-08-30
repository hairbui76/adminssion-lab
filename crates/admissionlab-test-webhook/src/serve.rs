//! `serve` mode: a minimal HTTPS server answering `GET /healthz` — and
//! nothing else. This is deliberately the *entire* HTTP surface Task 2.7
//! implements: PRODUCT.md §30's controlled admission behaviors (allow,
//! deny, add label, add/remove container, controlled delay, controlled
//! failure, fixture-annotation-dependent behavior, multi-webhook
//! ordering) are Task 3.9's job. There is no admission-review route
//! here at all — not a stub, not a placeholder that always allows,
//! nothing — because `recipes/test-webhook/manifests/20-webhook-configuration.yaml`'s
//! `namespaceSelector` (see that file's own comments) never routes a
//! real admission request to this server in the first place during
//! Phase 2, so there is nothing here that would ever handle one.
//!
//! No framework (`axum`/`tower`) for one static route: a hand-rolled
//! accept loop using `hyper`'s HTTP/1.1 server connection builder
//! directly, matching this project's general preference for explicit,
//! minimal-dependency code over pulling in a framework for a single
//! endpoint (see the root `Cargo.toml`'s comments on why every crate
//! this needs was already resolved transitively via `kube`).
//!
//! # Why `serve` mode never talks to the Kubernetes API
//!
//! Unlike `bootstrap` mode ([`crate::bootstrap`]), this module holds no
//! `kube::Client`, needs no `ServiceAccount` token, and reads no
//! environment variable at all (see [`crate::config`]'s own module
//! documentation): its only inputs are the certificate/key files
//! `bootstrap` mode already wrote to [`CERT_DIR`] before this container
//! ever starts (Kubernetes guarantees init containers complete first —
//! see [`crate::bootstrap`]'s module documentation). Kubernetes' own
//! `httpGet` liveness/readiness probes never verify a probed server's
//! TLS certificate regardless (`kubelet`'s prober always skips
//! certificate verification for an HTTPS probe), so this server's
//! self-signed-chain-of-trust correctness is irrelevant to *its own*
//! readiness gating — only to a real admission-review caller, which
//! Phase 2 never sends one of.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::ServerConfig;
use rustls_pki_types::pem::PemObject as _;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

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

const HEALTHZ_PATH: &str = "/healthz";

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

/// Runs `serve` mode: loads the certificate/key `bootstrap` mode wrote,
/// builds a TLS server configuration from them, and serves `GET
/// /healthz` (`200 OK`) — any other path or method gets `404 Not
/// Found` — over HTTPS on [`PORT`], forever (a connection or accept
/// failure is logged and this loop continues; only a bind failure is
/// fatal).
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

/// Answers exactly `GET /healthz` with `200 OK`; every other request
/// gets `404 Not Found`. Infallible: there is no failure mode short of a
/// process-level panic, which this function never risks (no I/O, no
/// parsing beyond what `hyper` already validated to construct `req`).
///
/// Generic over the request body type (`B`, unused) rather than fixed to
/// `hyper::body::Incoming`: this handler only ever inspects `req.method()`/
/// `req.uri()`, and being generic lets `tests` below call it directly
/// with a plain `Request<()>` built with no live connection at all,
/// instead of a hand-copied reimplementation of this same branching
/// logic tested against itself — the project's own stated standard
/// ("write tests that would fail if the behaviour regressed") ruled out
/// a copy that could silently drift from this function.
async fn handle<B>(req: Request<B>) -> Result<Response<Full<Bytes>>, Infallible> {
    let (status, body): (StatusCode, &'static [u8]) =
        if req.method() == Method::GET && req.uri().path() == HEALTHZ_PATH {
            (StatusCode::OK, b"ok\n")
        } else {
            (StatusCode::NOT_FOUND, b"not found\n")
        };

    Ok(Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from_static(body)))
        .expect("a static status/header/body response is always well-formed"))
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
    use http_body_util::BodyExt as _;
    use hyper::{Method, Request, StatusCode};

    use super::handle;

    /// Calls the real [`handle`] directly with a body-less `Request<()>`
    /// — no live connection, no `hyper::body::Incoming`, and no
    /// hand-copied reimplementation of its branching logic to drift out
    /// of sync with it (see [`handle`]'s own documentation for why it is
    /// generic over the body type specifically so this is possible).
    async fn call(method: Method, path: &str) -> (StatusCode, Vec<u8>) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .body(())
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
