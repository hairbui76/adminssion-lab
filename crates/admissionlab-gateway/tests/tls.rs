//! ROADMAP Task 8.6: isolated TLS test certificates.
//!
//! The contract that needs no TLS server: which hostnames may be issued
//! for, that every call is independent, the shape of what comes back,
//! and — the one that matters most — that the private key cannot be
//! rendered by any means this crate offers.
//!
//! The claims that can only be proven by a real handshake (a trusted CA
//! verifies, a wrong host fails, an expired certificate is rejected) are
//! unit tests in `src/tls.rs`, next to the private validity-window
//! helper two of them need. See that module's `mod tests` for why the
//! split falls there rather than duplicating the handshake harness.

use admissionlab_gateway::GatewayError;
use admissionlab_gateway::tls::{
    CERTIFICATE_VALIDITY, NOT_BEFORE_SKEW, TEST_TLD, TestCertificate, generate_test_certificate,
    probe_server_name, test_certificate_client_config,
};

/// The `reason` of a [`GatewayError::TestCertificate`], or a panic
/// naming what arrived instead.
fn rejection(host: &str) -> String {
    match generate_test_certificate(host) {
        Err(GatewayError::TestCertificate { reason, .. }) => reason,
        Err(other) => panic!("expected a TestCertificate error, got {other:?}"),
        Ok(_) => panic!("{host:?} must not be issued a certificate"),
    }
}

// ---------------------------------------------------------------------
// Only `.test`, and never production trust material
// ---------------------------------------------------------------------

#[test]
fn a_test_hostname_is_issued() {
    let certificate =
        generate_test_certificate("gateway.lab.test").expect("a .test host is issuable");
    assert_eq!(certificate.host, "gateway.lab.test");
}

#[test]
fn a_real_looking_domain_is_refused() {
    // The property this rule exists for. A generator that could be asked
    // for a name someone could actually reach would, sooner or later, be
    // asked for one.
    for host in [
        "api.yourbank.com",
        "kubernetes.default.svc",
        "gateway.lab.example",
        "localhost",
        "gateway.lab.tset",
    ] {
        let reason = rejection(host);
        assert!(
            reason.contains(TEST_TLD),
            "the refusal must name the only TLD that is allowed, got: {reason}"
        );
    }
}

#[test]
fn the_bare_reserved_tld_is_refused() {
    // `.test` itself is the reserved TLD, not a hostname under it, and
    // a certificate for it would cover every name in the namespace.
    for host in ["test", ".test"] {
        let reason = rejection(host);
        assert!(!reason.is_empty(), "a refusal always says why");
    }
}

#[test]
fn an_empty_or_whitespace_host_is_refused() {
    for host in ["", "   "] {
        assert!(rejection(host).contains("must not be empty"));
    }
}

#[test]
fn the_host_suffix_check_is_case_insensitive_and_the_host_is_trimmed() {
    // DNS labels are case-insensitive, so `.TEST` is the same TLD; the
    // stored host keeps the caller's own spelling, trimmed, because it
    // is what a probe will present as SNI.
    let certificate =
        generate_test_certificate("  Gateway.Lab.TEST  ").expect("case and padding are tolerated");
    assert_eq!(certificate.host, "Gateway.Lab.TEST");
}

#[test]
fn a_non_ascii_label_is_refused_rather_than_silently_mangled() {
    // A DNS subject alternative name is IA5String (7-bit ASCII), so a
    // non-ASCII label cannot be encoded at all. Surfaced as an error
    // rather than producing a certificate with an unusable SAN.
    let reason = rejection("caf\u{e9}.lab.test");
    assert!(!reason.is_empty());
}

#[test]
fn every_call_mints_an_independent_ca() {
    // The isolation premise: nothing is cached, reused, or read from
    // disk, so a key that leaked from one run cannot impersonate
    // anything in another. (That two CAs genuinely do not verify each
    // other's leaves is asserted by a real handshake, in `src/tls.rs`.)
    let first = generate_test_certificate("gateway.lab.test").expect("first");
    let second = generate_test_certificate("gateway.lab.test").expect("second");

    assert_ne!(first.ca_pem, second.ca_pem, "each call mints a fresh CA");
    assert_ne!(first.cert_pem, second.cert_pem, "and a fresh leaf");
    assert_ne!(
        first.expose_key_pem(),
        second.expose_key_pem(),
        "and a fresh key"
    );
}

#[test]
fn the_lifetime_is_short_and_backdated() {
    // Documented values, asserted rather than only described: a
    // certificate that quietly gained a multi-year lifetime would still
    // pass every handshake test in this crate.
    assert!(
        CERTIFICATE_VALIDITY <= time::Duration::hours(24),
        "test material must expire soon after the run that made it"
    );
    assert!(
        NOT_BEFORE_SKEW > time::Duration::ZERO && NOT_BEFORE_SKEW < CERTIFICATE_VALIDITY,
        "the skew buffer must be real but must not meaningfully extend the window"
    );
}

// ---------------------------------------------------------------------
// The key cannot be rendered
// ---------------------------------------------------------------------

/// A fragment of the generated key, taken through the one accessor that
/// exists, used to prove no *other* rendering contains it.
fn key_fragment(certificate: &TestCertificate) -> String {
    let pem = String::from_utf8(certificate.expose_key_pem().to_vec()).expect("the PEM is UTF-8");
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    assert!(
        body.len() > 32,
        "a generated key has a substantial base64 body"
    );
    body[..32].to_owned()
}

#[test]
fn debug_never_reveals_the_key() {
    let certificate = generate_test_certificate("gateway.lab.test").expect("issued");
    let fragment = key_fragment(&certificate);

    // The whole struct, derived `Debug` and all -- which is exactly how
    // a key leaks in practice: through a `{:?}` somebody added to an
    // unrelated struct that happens to contain one.
    let rendered = format!("{certificate:?}");
    assert!(
        !rendered.contains(&fragment),
        "a derived Debug of the certificate must not contain key material"
    );
    assert!(
        rendered.contains("[REDACTED]"),
        "and it must say that something was withheld, got: {rendered}"
    );
    // The public halves are still visible: a certificate is material a
    // reader may legitimately need, and blanking it would remove
    // evidence for no benefit.
    assert!(rendered.contains("gateway.lab.test"));

    assert_eq!(format!("{:?}", certificate.key_pem), "[REDACTED]");
    assert_eq!(format!("{}", certificate.key_pem), "[REDACTED]");
}

#[test]
fn serializing_the_key_yields_only_the_redaction_literal() {
    let certificate = generate_test_certificate("gateway.lab.test").expect("issued");
    let fragment = key_fragment(&certificate);

    let json = serde_json::to_string(&certificate.key_pem).expect("SensitiveBytes serializes");
    assert_eq!(json, "\"[REDACTED]\"");
    assert!(!json.contains(&fragment));
}

#[test]
fn the_generated_key_is_shaped_for_the_report_redaction_backstop() {
    // The write-site contract (see `admissionlab_gateway::tls`'s "Where
    // the private key may go") keeps a key out of every payload in the
    // first place. `admissionlab_report::redact` is the backstop if one
    // ever escapes anyway: it replaces any PEM block whose label
    // contains `PRIVATE KEY` -- including a block truncated mid-key --
    // with `[REDACTED PRIVATE KEY]`. That rule and its tests already
    // exist (ROADMAP Task 4.10); what could still drift is *this* side,
    // if the generator ever emitted a label the rule does not match. So
    // that is what is asserted here, rather than a second copy of the
    // report crate's own tests.
    let certificate = generate_test_certificate("gateway.lab.test").expect("issued");
    let pem = String::from_utf8(certificate.expose_key_pem().to_vec()).expect("the PEM is UTF-8");

    let label = pem
        .lines()
        .next()
        .expect("a PEM block opens with a BEGIN line");
    assert!(
        label.starts_with("-----BEGIN ") && label.ends_with("-----"),
        "the key must be a well-formed PEM block, got: {label}"
    );
    assert!(
        label.to_ascii_uppercase().contains("PRIVATE KEY"),
        "the label must contain the exact marker the report's redaction rule matches on, got: \
         {label}"
    );

    // The complement, and the reason the rule is a label match rather
    // than a blanket PEM match: a certificate is public material a
    // reader may need, and the report deliberately keeps it.
    let cert_label = String::from_utf8(certificate.cert_pem.clone())
        .expect("the PEM is UTF-8")
        .lines()
        .next()
        .expect("a PEM block opens with a BEGIN line")
        .to_owned();
    assert!(
        !cert_label.to_ascii_uppercase().contains("PRIVATE KEY"),
        "the leaf certificate must not look like key material, got: {cert_label}"
    );
}

// ---------------------------------------------------------------------
// The client-configuration seam (the handoff to Task 8.7)
// ---------------------------------------------------------------------

#[test]
fn a_generated_ca_builds_a_client_configuration() {
    let certificate = generate_test_certificate("gateway.lab.test").expect("issued");
    test_certificate_client_config(&certificate.ca_pem).expect("the generated CA is a usable root");
}

#[test]
fn a_ca_pem_with_no_certificate_is_refused() {
    // Never a silently empty root store: a client that trusts nothing
    // would fail every handshake, which reads as "the data plane is
    // broken" rather than "the trust material was never loaded".
    for pem in [
        b"".as_slice(),
        b"not a PEM file at all\n".as_slice(),
        b"-----BEGIN CERTIFICATE-----\n".as_slice(),
    ] {
        let error = test_certificate_client_config(pem)
            .expect_err("a CA PEM with no usable certificate must be refused");
        assert!(matches!(error, GatewayError::TestCertificate { .. }));
    }
}

#[test]
fn the_probe_server_name_is_the_contract_host_not_the_dialled_address() {
    // The asymmetry the whole seam exists for: a probe connects to
    // 127.0.0.1 but is verified against the contract's own hostname.
    let name = probe_server_name("gateway.lab.test").expect("a DNS name");
    assert!(matches!(name, rustls::pki_types::ServerName::DnsName(_)));

    // An IP literal is refused: a certificate this module generates has
    // a DNS SAN and no IP SAN, so verifying against `127.0.0.1` could
    // never succeed, and failing here says why instead of failing later
    // as an opaque handshake error.
    let error =
        probe_server_name("127.0.0.1").expect_err("an address is not what the probe verifies");
    assert!(matches!(error, GatewayError::TestCertificate { .. }));
}
