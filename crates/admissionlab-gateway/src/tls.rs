//! Isolated, ephemeral TLS material for probing a Gateway's HTTPS
//! listeners (ROADMAP Task 8.6).
//!
//! [`generate_test_certificate`] mints a fresh CA and a fresh leaf
//! certificate for one `.test` hostname, in process, with a short
//! lifetime. [`test_certificate_client_config`] turns that CA into a
//! `rustls::ClientConfig` that trusts **only** it. Between them they are
//! the whole of what a TLS probe needs: something to install into the
//! disposable cluster as the listener's serving certificate, and
//! something for the probe side to verify it with.
//!
//! # Never production trust material, by construction
//!
//! Four properties, each enforced rather than merely intended:
//!
//! 1. **A fresh CA per call.** Nothing is cached, reused across probes,
//!    or read from disk. Two calls produce two unrelated CAs, so a key
//!    that leaked out of one run cannot impersonate anything in another.
//! 2. **`.test` hostnames only.** [`generate_test_certificate`] rejects
//!    any host that is not under the [`TEST_TLD`] top-level domain (RFC
//!    6761 §6.2 reserves `.test` for exactly this and guarantees it is
//!    never delegated). A tool that could be asked to mint
//!    `api.yourbank.com` would, sooner or later, be asked to.
//! 3. **A short life.** [`CERTIFICATE_VALIDITY`] after "now", with
//!    [`NOT_BEFORE_SKEW`] of backdating so a clock difference between
//!    this process and the cluster does not reject a certificate that
//!    was valid when it was issued.
//! 4. **Visibly test-labelled.** Both subjects carry an Admission Lab
//!    test-only Common Name, so a certificate that somehow escaped its
//!    run identifies itself in any viewer.
//!
//! # Where the private key may go
//!
//! [`TestCertificate::key_pem`] is an
//! [`admissionlab_core::SensitiveBytes`], so it renders as `[REDACTED]`
//! in `Debug`, `Display` and `Serialize` alike, and the bytes come out
//! only through [`TestCertificate::expose_key_pem`] -- one blunt,
//! greppable call whose every appearance in a diff is a reviewable
//! event.
//!
//! **The contract for that call site**, and the whole of it: a generated
//! key may be written to
//!
//! - a file inside the run workspace, created with mode `0600`
//!   (`admissionlab_core::RunPaths`' own directory, which the run owns
//!   and cleans up), or
//! - a Kubernetes `Secret` in the *disposable* lab cluster, applied
//!   through a [`admissionlab_core::ClusterHandle`]'s isolated
//!   kubeconfig,
//!
//! and nowhere else. Not into a log line, not into a
//! [`admissionlab_core::Diagnostic`] (that is what
//! `RedactedValue::Sensitive` is for -- it keeps nothing), not into a
//! report, and never into this repository's working tree.
//!
//! Should a key ever escape into a payload anyway, the report's own
//! redaction pass is the backstop: `admissionlab_report::redact`
//! replaces any PEM block whose label contains `PRIVATE KEY` with
//! `[REDACTED PRIVATE KEY]`, marker to marker, including a block
//! truncated mid-key. `tests/tls.rs` asserts that what this module
//! generates really does carry such a label, so the two halves cannot
//! drift into a rule that matches nothing.
//!
//! # The handoff to Task 8.7
//!
//! This module deliberately does **not** touch `probe.rs`. Task 8.7 owns
//! the TLS contract and the probe path; what it needs from here is a
//! seam, and the seam is [`test_certificate_client_config`] plus
//! [`probe_server_name`]:
//!
//! ```text
//!   let config = test_certificate_client_config(&certificate.ca_pem)?;
//!   let name    = probe_server_name(&contract.host)?;
//!   let stream  = TcpStream::connect(local_addr).await?;      // 127.0.0.1:<forwarded port>
//!   let stream  = TlsConnector::from(Arc::new(config))
//!                     .connect(name, stream).await?;          // SNI/verification use `name`
//!   let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
//! ```
//!
//! The load-bearing detail is the split between *where the connection
//! goes* and *what the connection claims to be talking to*. A probe
//! dials `127.0.0.1:<port>` -- the address Task 6.7's port-forward bound
//! -- while SNI and certificate verification are performed against the
//! contract's own `host`, which is what a Gateway listener's `hostname`
//! matches on. That is the identical asymmetry
//! [`crate::probe::execute_http_probe`] already documents for the plain
//! `Host` header, one layer down; `rustls` takes the name as an argument
//! to `connect` rather than reading it from the socket, so nothing has
//! to be overridden or patched to get it.
//!
//! Note that the roadmap's own Step 3 says "configure *reqwest* probe
//! trust". `probe.rs` deliberately uses `hyper`'s low-level client
//! instead (Task 6.8's documented decision -- redirects that cannot be
//! followed, and the connect/response seam the retry rule needs), and
//! that decision is what this seam is shaped for. Nothing is lost: what
//! Step 3 asks for is a client that trusts the generated CA and resolves
//! the probe's hostname to the forwarded local port, and the four lines
//! above are exactly that.

use std::sync::Arc;

use admissionlab_core::SensitiveBytes;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject as _;
use time::{Duration, OffsetDateTime};

use crate::error::GatewayError;

/// The only top-level domain [`generate_test_certificate`] will issue
/// for, including the leading dot.
///
/// RFC 6761 §6.2 reserves `.test` for testing and guarantees it is never
/// delegated in the global DNS, so a certificate issued for a name under
/// it cannot be presented for a name anyone could actually reach. See
/// this module's "Never production trust material".
pub const TEST_TLD: &str = ".test";

/// How long past "now" a generated certificate remains valid.
///
/// Twenty-four hours: comfortably longer than any lab run (a `kind`
/// cluster pair, an install, and a fixture replay is minutes, not days)
/// and short enough that material which escaped a run stops being usable
/// almost immediately. The same window, for the same reason,
/// `admissionlab_test_webhook::cert` uses for its own generated serving
/// certificate.
pub const CERTIFICATE_VALIDITY: Duration = Duration::hours(24);

/// How far before "now" a generated certificate's `notBefore` is
/// backdated.
///
/// Five minutes of clock-skew buffer between this process and whatever
/// clock verifies the certificate -- a `kind` node's, or a probe running
/// on a differently-synchronized host. Not a grace period: a certificate
/// that is *already valid* when it is issued is the point, and five
/// minutes is small enough that it does not meaningfully extend the
/// window above.
pub const NOT_BEFORE_SKEW: Duration = Duration::minutes(5);

/// The Common Name of every generated CA. Deliberately unmistakable in
/// any certificate viewer: material that somehow outlived its run should
/// say what it is on its face.
const CA_COMMON_NAME: &str = "Admission Lab ephemeral test CA (DO NOT TRUST)";

/// A freshly generated, single-use CA and the leaf certificate it signed
/// for one `.test` hostname.
///
/// §1.2's registry freezes the field names and their types. `cert_pem`
/// and `ca_pem` are plain `Vec<u8>` because a certificate is public
/// material a reader may legitimately need; `key_pem` is
/// [`SensitiveBytes`] because a private key is not, and the difference
/// between the two is the whole of what this type encodes.
///
/// PEM text rather than DER bytes throughout: PEM is what a Kubernetes
/// TLS `Secret` carries (`tls.crt`/`tls.key`), what a Gateway listener's
/// `certificateRefs` resolves to, and what `rustls-pki-types` parses
/// back. Holding DER here would mean re-encoding at every destination.
///
/// **No `Serialize`.** Not because `key_pem` would leak -- it renders as
/// `[REDACTED]`, which is exactly the point of its type -- but because a
/// serialized `TestCertificate` would be a document that *looks* like
/// key material and is not, and nothing in this project has a reason to
/// write one. See this module's "Where the private key may go" for the
/// only two destinations that exist.
#[derive(Debug, Clone)]
pub struct TestCertificate {
    /// The `.test` hostname this certificate was issued for, exactly as
    /// [`generate_test_certificate`] was called with (trimmed). It is
    /// the leaf's sole DNS Subject Alternative Name, and the name a
    /// probe must present as SNI -- see [`probe_server_name`].
    pub host: String,
    /// The leaf certificate, PEM-encoded. Public: this is what a
    /// listener serves.
    pub cert_pem: Vec<u8>,
    /// The leaf's private key, PEM-encoded (PKCS#8). See this module's
    /// "Where the private key may go", and
    /// [`TestCertificate::expose_key_pem`].
    pub key_pem: SensitiveBytes,
    /// The CA certificate that signed `cert_pem`, PEM-encoded. Public:
    /// this is what a probe adds to its root store, and it is the only
    /// thing [`test_certificate_client_config`] needs.
    pub ca_pem: Vec<u8>,
}

impl TestCertificate {
    /// The private key's PEM bytes.
    ///
    /// A named accessor rather than a public field, so that reaching the
    /// key is an explicit act with an explicit name -- see
    /// [`SensitiveBytes::expose`], which this delegates to, for why
    /// "blunt and greppable" is the design and not an accident. Call
    /// this at the one site that writes the key to the run workspace
    /// (mode `0600`) or to a `Secret` in the disposable cluster, and
    /// nowhere else.
    #[must_use]
    pub fn expose_key_pem(&self) -> &[u8] {
        self.key_pem.expose()
    }
}

/// Generates a fresh CA and a CA-signed leaf certificate for `host`.
///
/// `host` must be a non-empty name under [`TEST_TLD`]. Everything else
/// about the result is fixed by this module: a new CA every call, a
/// [`CERTIFICATE_VALIDITY`] window backdated by [`NOT_BEFORE_SKEW`], the
/// host as the leaf's only DNS SAN, and `serverAuth` as its only
/// extended key usage.
///
/// # Errors
///
/// Returns [`GatewayError::TestCertificate`] if `host` is empty, is not
/// under [`TEST_TLD`], is the bare TLD itself, or cannot be encoded as a
/// DNS SAN (a SAN is `IA5String`, so a non-ASCII label is rejected by
/// `rcgen` and surfaced here rather than silently producing an unusable
/// certificate).
pub fn generate_test_certificate(host: &str) -> Result<TestCertificate, GatewayError> {
    let host = validate_test_host(host)?;
    let now = OffsetDateTime::now_utc();
    generate_for_window(host, now - NOT_BEFORE_SKEW, now + CERTIFICATE_VALIDITY)
}

/// Rejects any host [`generate_test_certificate`] must not issue for,
/// returning it trimmed.
///
/// The rule is a suffix match on [`TEST_TLD`], plus "there is a label in
/// front of it". An *allow-list*, deliberately, for the reason
/// `admissionlab_spec::validate::require_pinned_helm_version` gives for
/// its own grammar: a typo (`lab.tset`), a real domain
/// (`api.yourbank.com`), and a name that resolves on the operator's
/// corporate DNS are all rejected by one rule, without anyone having to
/// enumerate the wrong answers. Case-insensitive, because DNS labels
/// are.
fn validate_test_host(host: &str) -> Result<String, GatewayError> {
    let trimmed = host.trim();
    let invalid = |reason: &str| GatewayError::TestCertificate {
        host: trimmed.to_owned(),
        reason: reason.to_owned(),
    };

    if trimmed.is_empty() {
        return Err(invalid("the host must not be empty"));
    }
    let lowered = trimmed.to_ascii_lowercase();
    if !lowered.ends_with(TEST_TLD) {
        return Err(invalid(&format!(
            "Admission Lab only issues test certificates for names under the reserved {TEST_TLD} \
             top-level domain (RFC 6761 §6.2), so that generated material can never be presented \
             for a name anyone could actually reach"
        )));
    }
    if lowered.len() == TEST_TLD.len() || lowered.strip_suffix(TEST_TLD).is_some_and(str::is_empty)
    {
        return Err(invalid(&format!(
            "{TEST_TLD:?} is the reserved top-level domain itself, not a hostname under it"
        )));
    }

    Ok(trimmed.to_owned())
}

/// The whole of [`generate_test_certificate`]'s certificate work, with
/// the validity window supplied rather than computed.
///
/// Private, and split out for exactly one reason: this module's unit
/// tests need a certificate that is *already expired* in order to prove
/// that a client built by [`test_certificate_client_config`] rejects
/// one. Backdating is a property no caller should be able to ask for --
/// a public "issue me an expired certificate" entry point is an
/// invitation to a test that passes because verification was silently
/// disabled -- so the knob stays inside the module that needs it, in
/// the way `rcgen` itself exposes it.
fn generate_for_window(
    host: String,
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
) -> Result<TestCertificate, GatewayError> {
    let failed = |reason: &rcgen::Error| GatewayError::TestCertificate {
        host: host.clone(),
        reason: reason.to_string(),
    };

    let mut ca_params = CertificateParams::new(Vec::<String>::new()).map_err(|e| failed(&e))?;
    ca_params.not_before = not_before;
    ca_params.not_after = not_after;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params.distinguished_name = common_name_dn(CA_COMMON_NAME);

    let ca_key = KeyPair::generate().map_err(|e| failed(&e))?;
    let ca_cert = ca_params.self_signed(&ca_key).map_err(|e| failed(&e))?;
    // `self_signed` only borrowed the key, so ownership is free to move
    // into the issuer here.
    let issuer = Issuer::from_params(&ca_params, ca_key);

    let mut leaf_params = CertificateParams::new(vec![host.clone()]).map_err(|error| {
        GatewayError::TestCertificate {
            host: host.clone(),
            // The one failure a caller can actually cause with a
            // syntactically `.test` name: a DNS SAN is IA5String, so a
            // non-ASCII label cannot be encoded at all.
            reason: format!(
                "{host:?} cannot be encoded as a DNS subject alternative name: {error}"
            ),
        }
    })?;
    leaf_params.not_before = not_before;
    leaf_params.not_after = not_after;
    leaf_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf_params.distinguished_name = common_name_dn(&format!("{host} (Admission Lab test)"));

    let leaf_key = KeyPair::generate().map_err(|e| failed(&e))?;
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &issuer)
        .map_err(|e| failed(&e))?;

    Ok(TestCertificate {
        cert_pem: leaf_cert.pem().into_bytes(),
        key_pem: SensitiveBytes::new(leaf_key.serialize_pem().into_bytes()),
        ca_pem: ca_cert.pem().into_bytes(),
        host,
    })
}

/// A [`DistinguishedName`] carrying only a `CommonName`.
fn common_name_dn(value: &str) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, value);
    dn
}

/// Builds a `rustls::ClientConfig` whose root store contains **only**
/// `ca_pem` -- the CA from a [`TestCertificate`].
///
/// This is the trust half of the TLS probe seam (see this module's "The
/// handoff to Task 8.7"). The resulting configuration verifies the
/// server certificate and the hostname exactly as any other client would;
/// what makes it usable against a lab's self-signed material is only
/// that the generated CA is in the store, and what makes it *safe* is
/// that nothing else is. The platform's own trust store is deliberately
/// not loaded: a probe that also trusted the operator's real roots could
/// complete a handshake against something that is not the lab, and the
/// resulting observation would be about the wrong server.
///
/// No client certificate is presented (`with_no_client_auth`): a Gateway
/// data plane under test is not asked to authenticate the probe, and
/// offering a certificate would change what is being measured.
///
/// # Errors
///
/// Returns [`GatewayError::TestCertificate`] if `ca_pem` contains no
/// certificate, or if the certificate it contains is not one `rustls`
/// will accept as a root.
pub fn test_certificate_client_config(ca_pem: &[u8]) -> Result<ClientConfig, GatewayError> {
    let invalid = |reason: String| GatewayError::TestCertificate {
        // Not a hostname: this half of the API is about a CA, and
        // inventing a host for the message would name something that was
        // never involved.
        host: "<generated CA>".to_owned(),
        reason,
    };

    let mut roots = RootCertStore::empty();
    let mut added = 0_usize;
    for certificate in CertificateDer::pem_slice_iter(ca_pem) {
        let certificate =
            certificate.map_err(|error| invalid(format!("could not parse the CA PEM: {error}")))?;
        roots.add(certificate).map_err(|error| {
            invalid(format!("the CA certificate is not a usable root: {error}"))
        })?;
        added += 1;
    }
    if added == 0 {
        return Err(invalid(
            "the CA PEM contains no certificate at all".to_owned(),
        ));
    }

    // The same provider `admissionlab-test-webhook`'s serving side
    // builds with, chosen explicitly rather than relying on a
    // process-wide default: `rustls`'s `ClientConfig::builder()` reads an
    // install-once global, and a library that depends on whether some
    // other crate installed one first is a library that fails in
    // whichever binary links it second.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| {
            invalid(format!(
                "the ring provider does not support the default protocol versions: {error}"
            ))
        })
        .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
}

/// The `rustls` server name a probe must present for `host`: the name
/// SNI carries and the name the certificate is verified against.
///
/// The other half of the seam. A probe connects to `127.0.0.1:<port>`
/// but must be verified against the contract's own hostname, and this is
/// the value that makes those two different -- `rustls` takes the name
/// as an argument to `connect` rather than deriving it from the socket,
/// so no resolver override or `/etc/hosts` entry is involved.
///
/// Owned (`ServerName<'static>`), so the value outlives the borrowed
/// `host` and can be moved into a connection future.
///
/// # Errors
///
/// Returns [`GatewayError::TestCertificate`] if `host` is not a
/// syntactically valid DNS name, or is an IP address literal.
///
/// The IP case is rejected deliberately rather than passed through, even
/// though `rustls` would accept it as a `ServerName::IpAddress`: a
/// certificate this module generates carries a DNS subject alternative
/// name and no IP SAN, so verification against `127.0.0.1` could never
/// succeed. Failing here says exactly that; passing it through would
/// fail later as an opaque handshake error, at the point where a reader
/// is most likely to conclude the data plane is broken. It is also the
/// specific mistake this seam invites — the probe *does* dial an IP, and
/// the whole point is that the name it verifies is a different thing.
pub fn probe_server_name(host: &str) -> Result<ServerName<'static>, GatewayError> {
    let invalid = |reason: String| GatewayError::TestCertificate {
        host: host.to_owned(),
        reason,
    };

    let name = ServerName::try_from(host.to_owned()).map_err(|error| {
        invalid(format!(
            "not a server name TLS can be verified against: {error}"
        ))
    })?;
    if matches!(name, ServerName::IpAddress(_)) {
        return Err(invalid(
            "an IP address is not what a probe verifies: a generated test certificate carries a \
             DNS subject alternative name, so verification must use the contract's own hostname \
             even though the connection is dialled to a local address"
                .to_owned(),
        ));
    }
    Ok(name)
}

/// Everything this module claims that can only be proven by a real TLS
/// handshake.
///
/// These are unit tests rather than `tests/tls.rs` integration tests for
/// one concrete reason: two of them need [`generate_for_window`], which
/// is private on purpose (see that function for why "issue me an expired
/// certificate" is not a public entry point), and the rest share the
/// same handshake harness. Splitting them would mean two copies of the
/// harness, free to drift into asserting subtly different things about
/// the same certificates. `tests/tls.rs` holds the contract that needs
/// no server: the `.test` rule, the redaction discipline, and the shape
/// of what is generated.
///
/// Every one of them drives a genuine `rustls`/`tokio-rustls` handshake
/// over an in-memory duplex pipe rather than inspecting PEM text. What
/// this module claims is that the material *works*, and a certificate
/// can be perfectly well-formed and still fail to verify.
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rustls_pki_types::PrivateKeyDer;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, duplex};
    use tokio_rustls::{TlsAcceptor, TlsConnector};

    use super::*;

    /// Runs one real TLS handshake and a one-round-trip exchange: the
    /// server presents `certificate`'s leaf, the client trusts only
    /// `trusted_ca` and requests `server_name`. Returns whether the whole
    /// exchange succeeded.
    async fn handshake_succeeds_against(
        certificate: &TestCertificate,
        trusted_ca: &[u8],
        server_name: &str,
    ) -> bool {
        let provider = Arc::new(rustls::crypto::ring::default_provider());

        let leaf = CertificateDer::pem_slice_iter(&certificate.cert_pem)
            .collect::<Result<Vec<_>, _>>()
            .expect("the generated leaf PEM parses");
        let key = PrivateKeyDer::from_pem_slice(certificate.expose_key_pem())
            .expect("the generated key PEM contains a private key");
        let server_config = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .expect("the ring provider supports the default protocol versions")
            .with_no_client_auth()
            .with_single_cert(leaf, key)
            .expect("the generated certificate and key match");

        let client_config = test_certificate_client_config(trusted_ca)
            .expect("a generated CA builds a client configuration");
        let Ok(name) = probe_server_name(server_name) else {
            return false;
        };

        let (client_io, server_io) = duplex(16 * 1024);
        let server = tokio::spawn(async move {
            let acceptor = TlsAcceptor::from(Arc::new(server_config));
            let Ok(mut stream) = acceptor.accept(server_io).await else {
                return;
            };
            let mut request = [0_u8; 5];
            let _ = stream.read_exact(&mut request).await;
            let _ = stream.write_all(b"world").await;
        });

        let connector = TlsConnector::from(Arc::new(client_config));
        let Ok(mut stream) = connector.connect(name, client_io).await else {
            let _ = server.await;
            return false;
        };
        let wrote = stream.write_all(b"hello").await.is_ok();
        let mut response = [0_u8; 5];
        let read = stream.read_exact(&mut response).await.is_ok();

        let _ = server.await;
        wrote && read && &response == b"world"
    }

    /// The common case: a client trusting the certificate's own CA.
    async fn handshake_succeeds(certificate: &TestCertificate, server_name: &str) -> bool {
        handshake_succeeds_against(certificate, &certificate.ca_pem, server_name).await
    }

    #[tokio::test]
    async fn the_generated_material_completes_a_handshake_for_its_own_host() {
        let certificate =
            generate_test_certificate("gateway.lab.test").expect("a .test host generates");
        assert!(
            handshake_succeeds(&certificate, "gateway.lab.test").await,
            "a client trusting the generated CA must accept the generated leaf"
        );
    }

    #[tokio::test]
    async fn a_handshake_for_a_different_host_fails() {
        // Proves hostname verification is genuinely exercised: without
        // this, the test above could pass vacuously against a client
        // that checked nothing.
        let certificate =
            generate_test_certificate("gateway.lab.test").expect("a .test host generates");
        assert!(
            !handshake_succeeds(&certificate, "other.lab.test").await,
            "the leaf's only SAN is its own host; a different name must not verify"
        );
    }

    #[tokio::test]
    async fn a_foreign_ca_does_not_verify_the_certificate() {
        // The isolation claim, stated as a handshake: each call mints an
        // unrelated CA, so material from one run cannot be verified by
        // another run's trust. This is also what proves
        // `test_certificate_client_config` trusts *only* what it was
        // given.
        let certificate =
            generate_test_certificate("gateway.lab.test").expect("a .test host generates");
        let unrelated =
            generate_test_certificate("gateway.lab.test").expect("a second, independent CA");

        assert!(
            !handshake_succeeds_against(&certificate, &unrelated.ca_pem, "gateway.lab.test").await,
            "a client trusting a different CA must reject this leaf, even for the same hostname"
        );
    }

    #[tokio::test]
    async fn an_expired_certificate_is_rejected_by_a_client_that_trusts_its_ca() {
        // `CERTIFICATE_VALIDITY` is only worth stating if something
        // enforces it.
        let now = OffsetDateTime::now_utc();
        let expired = generate_for_window(
            "expired.lab.test".to_owned(),
            now - Duration::hours(48),
            now - Duration::hours(24),
        )
        .expect("generation itself does not care about the window");

        assert!(
            !handshake_succeeds(&expired, "expired.lab.test").await,
            "a certificate whose notAfter has passed must not verify, however trusted its CA"
        );

        // The control: the same code path with a live window does
        // complete, so the assertion above is about expiry rather than
        // about the harness being broken.
        let live = generate_for_window(
            "live.lab.test".to_owned(),
            now - NOT_BEFORE_SKEW,
            now + CERTIFICATE_VALIDITY,
        )
        .expect("a live window generates");
        assert!(
            handshake_succeeds(&live, "live.lab.test").await,
            "the same harness must accept a certificate that is currently valid"
        );
    }

    #[tokio::test]
    async fn a_not_yet_valid_certificate_is_rejected() {
        // What makes `NOT_BEFORE_SKEW`'s backdating load-bearing rather
        // than decorative: without it, a cluster whose clock is a little
        // behind this process would see exactly this.
        let now = OffsetDateTime::now_utc();
        let future = generate_for_window(
            "future.lab.test".to_owned(),
            now + Duration::hours(1),
            now + Duration::hours(24),
        )
        .expect("generation itself does not care about the window");

        assert!(
            !handshake_succeeds(&future, "future.lab.test").await,
            "a certificate that is not valid yet must not verify"
        );
    }
}
