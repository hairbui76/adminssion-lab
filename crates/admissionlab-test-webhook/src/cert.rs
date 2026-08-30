//! Deterministic, test-only TLS certificate generation: a fresh
//! self-signed CA and a CA-signed serving certificate, generated
//! entirely in-process via `rcgen` — never by shelling out to a system
//! `openssl` binary, and never read from (or written to) anywhere this
//! repository's own git history could ever see.
//!
//! # Why generated, not checked in
//!
//! Task 2.7's brief states the constraint this module exists to satisfy
//! plainly: "Certificate bootstrapping may use a deterministic test-only
//! CA/Secret generated per cluster; never check a private key into git."
//! [`generate`] is called exactly once per pod, by
//! [`crate::bootstrap::run`] running as an init container — a fresh
//! kind cluster gets a fresh Deployment, gets a fresh pod, gets a fresh
//! call to this function, so every cluster's CA is independent (see this
//! crate's top-level report for why that "per cluster" property matters:
//! a leaked test key from one ephemeral run can never impersonate a
//! webhook in a different one). Nothing in this module ever reads from
//! or writes to a path inside this repository's own working tree — see
//! [`crate::bootstrap`]'s module documentation for exactly where the
//! generated key material is written instead (a pod-local, memory-backed
//! `emptyDir`, never a Kubernetes `Secret` object and never this
//! repository).
//!
//! # Validity window
//!
//! A short, fixed window — [`NOT_BEFORE_SKEW`] before "now" (a small
//! clock-skew buffer, not a meaningfully long grace period) through
//! [`VALIDITY`] after it. A long-lived certificate has no benefit here:
//! this component's whole purpose is being installed fresh into a
//! disposable, short-lived `kind` cluster (PRODUCT.md §8's "disposable
//! and isolated" lab premise), so a validity window measured in hours
//! both comfortably covers any real test run and keeps this test-only
//! CA's blast radius small on principle, independent of the "per
//! cluster" isolation [`generate`] already provides.

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose,
};
use time::{Duration, OffsetDateTime};

/// How far before "now" the generated CA/certificate's `not_before` is
/// backdated — a clock-skew buffer between this process and whatever
/// clock the API server (validating the webhook's cert during a real
/// admission call) or `kubelet` (probing `/healthz`) reads from.
const NOT_BEFORE_SKEW: Duration = Duration::minutes(5);

/// How long past "now" the generated CA/certificate remains valid. See
/// this module's own documentation for why this is short rather than a
/// conventional multi-year certificate lifetime.
const VALIDITY: Duration = Duration::hours(24);

/// The result of [`generate`]: a fresh CA plus a serving certificate it
/// signed, all PEM-encoded and ready to write to disk
/// ([`crate::bootstrap::write_cert_files`]) or install as a webhook
/// configuration's `caBundle` ([`crate::bootstrap::patch_ca_bundle`]).
///
/// Deliberately holds only PEM `String`s, never an `rcgen::KeyPair` or
/// any other in-memory key handle that could be cloned or held longer
/// than needed — the caller decides exactly once where each field goes
/// and drops this value immediately after.
// The shared `_pem` postfix is deliberate, not an oversight
// `clippy::struct_field_names` should trim: every field here is a PEM
// *text* encoding specifically, as opposed to a DER byte encoding this
// same struct could otherwise plausibly hold (`rcgen::Certificate::der`
// exists right alongside `::pem`) -- for a struct whose whole purpose is
// carrying security-sensitive key/certificate material, keeping that
// distinction in the field name itself, not only in each field's own
// doc comment, is worth the lint's disapproval.
#[allow(clippy::struct_field_names)]
pub struct GeneratedCerts {
    /// The self-signed CA certificate, PEM-encoded. Public — this is
    /// exactly what a `ValidatingWebhookConfiguration`'s `caBundle`
    /// needs, and what an admission-review caller uses to validate the
    /// serving certificate below.
    pub ca_cert_pem: String,
    /// The serving certificate, signed by the CA above, PEM-encoded.
    /// Public.
    pub server_cert_pem: String,
    /// The serving certificate's private key, PEM-encoded (PKCS#8).
    /// **Not public**: [`crate::bootstrap::write_cert_files`] is the
    /// only place this ever goes, and only into a memory-backed
    /// `emptyDir` file this repository's own `.gitignore` cannot even
    /// reach because it never touches this working tree at all.
    pub server_key_pem: String,
}

/// Something went wrong generating a certificate. A thin wrapper over
/// [`rcgen::Error`]: every fallible `rcgen` call in [`generate`] is a
/// pure, local, non-I/O operation (key generation, ASN.1 encoding), so
/// in practice this can only follow an invalid `service_name`/
/// `namespace` (for example one containing a byte a DNS name cannot)
/// reaching [`generate`] — a config error passed through what is
/// otherwise this crate's cryptography boundary, not a new failure mode
/// of its own.
#[derive(Debug, thiserror::Error)]
#[error("failed to generate a certificate: {0}")]
pub struct CertError(#[from] rcgen::Error);

/// Generates a fresh, self-signed CA and a CA-signed serving certificate
/// for `service_name.namespace.svc` (and `.svc.cluster.local`) — see
/// this module's own documentation for the validity window and why this
/// is safe to call once per pod rather than something that must be
/// cached or reused.
///
/// # Errors
///
/// Returns [`CertError`] if `service_name`/`namespace` cannot form a
/// valid DNS Subject Alternative Name, or (in practice unreachable for
/// any input this crate's own callers pass, since neither call below
/// does file or network I/O) if certificate encoding otherwise fails.
pub fn generate(service_name: &str, namespace: &str) -> Result<GeneratedCerts, CertError> {
    let now = OffsetDateTime::now_utc();
    let not_before = now - NOT_BEFORE_SKEW;
    let not_after = now + VALIDITY;

    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.not_before = not_before;
    ca_params.not_after = not_after;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params.distinguished_name = common_name_dn("Admission Lab test-webhook CA");

    let ca_key = KeyPair::generate()?;
    let ca_cert = ca_params.self_signed(&ca_key)?;
    // `self_signed` above only borrowed `ca_key` (`&self, signing_key:
    // &impl SigningKey`); ownership is free to move into the `Issuer`
    // here.
    let issuer = Issuer::from_params(&ca_params, ca_key);

    let sans = vec![
        format!("{service_name}.{namespace}.svc"),
        format!("{service_name}.{namespace}.svc.cluster.local"),
    ];
    let mut server_params = CertificateParams::new(sans.clone())?;
    server_params.not_before = not_before;
    server_params.not_after = not_after;
    server_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    server_params.distinguished_name = common_name_dn(&sans[0]);

    let server_key = KeyPair::generate()?;
    let server_cert = server_params.signed_by(&server_key, &issuer)?;

    Ok(GeneratedCerts {
        ca_cert_pem: ca_cert.pem(),
        server_cert_pem: server_cert.pem(),
        server_key_pem: server_key.serialize_pem(),
    })
}

/// A [`DistinguishedName`] carrying only a `CommonName` of `value` —
/// this module's certificates never need more than that.
fn common_name_dn(value: &str) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, value);
    dn
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, RootCertStore};
    use rustls_pki_types::pem::PemObject as _;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, duplex};
    use tokio_rustls::{TlsAcceptor, TlsConnector};

    use super::generate;

    /// The regression-relevant claim this whole module exists to make
    /// true: a client that trusts *only* the generated CA can complete a
    /// real TLS handshake against a server presenting the generated
    /// leaf certificate, for exactly the hostname [`generate`] was
    /// asked to certify — and, just as importantly, *not* for a
    /// different hostname (proving SAN validation is genuinely
    /// exercised here, not vacuously passing because nothing checked
    /// the hostname at all). Drives a real, in-memory
    /// `rustls`/`tokio-rustls` handshake end to end — the same crypto
    /// stack `crate::serve` uses for real — rather than only inspecting
    /// the generated PEM text.
    #[tokio::test]
    async fn ca_and_leaf_cert_form_a_working_chain_for_the_intended_hostname_only() {
        let certs = generate("admissionlab-test-webhook", "admissionlab-test-webhook")
            .expect("certificate generation must succeed for valid DNS-safe inputs");

        assert!(
            handshake_succeeds(
                &certs,
                "admissionlab-test-webhook.admissionlab-test-webhook.svc"
            )
            .await
        );
        assert!(
            handshake_succeeds(
                &certs,
                "admissionlab-test-webhook.admissionlab-test-webhook.svc.cluster.local"
            )
            .await
        );
        assert!(
            !handshake_succeeds(&certs, "some-other-service.some-other-namespace.svc").await,
            "a client verifying against the generated CA must reject a hostname the leaf \
             certificate was never issued for"
        );
    }

    /// Runs one real TLS handshake over an in-memory duplex pipe: the
    /// server presents `certs`' leaf certificate/key, the client trusts
    /// only `certs`' CA and requests `server_name`. Returns whether the
    /// handshake (both sides) completed successfully.
    async fn handshake_succeeds(certs: &super::GeneratedCerts, server_name: &str) -> bool {
        let provider = Arc::new(rustls::crypto::ring::default_provider());

        let server_certs = CertificateDer::pem_slice_iter(certs.server_cert_pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .expect("parse generated server cert PEM");
        let server_key = PrivateKeyDer::from_pem_slice(certs.server_key_pem.as_bytes())
            .expect("generated server key PEM must contain a private key");
        let server_config = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .expect("ring provider supports the default protocol versions")
            .with_no_client_auth()
            .with_single_cert(server_certs, server_key)
            .expect("generated cert/key must match");

        let mut roots = RootCertStore::empty();
        let ca_der = CertificateDer::pem_slice_iter(certs.ca_cert_pem.as_bytes())
            .next()
            .expect("generated CA PEM must contain a certificate")
            .expect("parse generated CA cert PEM");
        roots
            .add(ca_der)
            .expect("generated CA cert must be a valid root");
        let client_config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring provider supports the default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth();

        let (client_io, server_io) = duplex(16 * 1024);

        let server_name = server_name
            .to_owned()
            .try_into()
            .unwrap_or_else(|_| ServerName::try_from("invalid.invalid").unwrap());

        let server_task = tokio::spawn(async move {
            let acceptor = TlsAcceptor::from(Arc::new(server_config));
            let Ok(mut stream) = acceptor.accept(server_io).await else {
                return;
            };
            let mut buf = [0_u8; 5];
            let _ = stream.read_exact(&mut buf).await;
            let _ = stream.write_all(b"world").await;
        });

        let connector = TlsConnector::from(Arc::new(client_config));
        let client_result = connector.connect(server_name, client_io).await;
        let Ok(mut client_stream) = client_result else {
            let _ = server_task.await;
            return false;
        };
        let write_ok = client_stream.write_all(b"hello").await.is_ok();
        let mut response = [0_u8; 5];
        let read_ok = client_stream.read_exact(&mut response).await.is_ok();

        let _ = server_task.await;
        write_ok && read_ok && &response == b"world"
    }

    #[test]
    fn generate_rejects_a_hostname_illegal_dns_label() {
        // A DNS name's SAN encoding is IA5String (7-bit ASCII only); a
        // non-ASCII character cannot appear in one -- proves `generate`
        // surfaces `rcgen`'s own validation as `CertError` rather than
        // panicking or silently producing an unusable SAN.
        let result = generate("not-ascii-\u{e9}", "ns");
        assert!(result.is_err());
    }
}
