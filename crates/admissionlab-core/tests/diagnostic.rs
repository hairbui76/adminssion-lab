//! Behavioral tests for the secret-safe diagnostic vocabulary:
//! [`DiagnosticLevel`], [`Diagnostic`], and [`RedactedValue`].
//!
//! Admission Lab runs third-party Helm charts and admission webhooks as
//! untrusted workloads and handles Kubernetes Secrets, so every test in
//! this file is ultimately in service of one property: a
//! [`RedactedValue::Sensitive`] context entry must never reach a log line
//! or a serialized report as anything other than the literal text
//! `[REDACTED]`.

use std::collections::BTreeMap;

use admissionlab_core::{Diagnostic, DiagnosticLevel, RedactedValue};

// ---------------------------------------------------------------------
// RedactedValue — standalone serialization (brief Step 1)
// ---------------------------------------------------------------------

#[test]
fn sensitive_context_serializes_as_redacted() {
    let value = RedactedValue::Sensitive;
    assert_eq!(serde_json::to_string(&value).unwrap(), r#""[REDACTED]""#);
}

#[test]
fn public_context_serializes_as_its_inner_string() {
    let value = RedactedValue::Public("admission-lab-baseline".to_string());
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        r#""admission-lab-baseline""#
    );
}

// ---------------------------------------------------------------------
// Diagnostic — whole-struct serialization
// ---------------------------------------------------------------------

#[test]
fn diagnostic_whole_struct_serialization_redacts_sensitive_context() {
    let mut context = BTreeMap::new();
    context.insert("kubeconfig-token".to_string(), RedactedValue::Sensitive);
    context.insert(
        "namespace".to_string(),
        RedactedValue::Public("admission-lab-baseline".to_string()),
    );
    let diagnostic = Diagnostic {
        code: "install.failed".to_string(),
        message: "helm install failed".to_string(),
        context,
    };

    let json = serde_json::to_string(&diagnostic).unwrap();

    // The sensitive context value is redacted...
    assert!(
        json.contains(r#""kubeconfig-token":"[REDACTED]""#),
        "expected redacted context entry in {json}"
    );
    // ...while public fields and public context values survive verbatim.
    assert!(json.contains(r#""code":"install.failed""#));
    assert!(json.contains(r#""message":"helm install failed""#));
    assert!(json.contains(r#""namespace":"admission-lab-baseline""#));
    // No trace of the enum's variant name leaks through as a stand-in for
    // the redacted value: only the deliberate "[REDACTED]" sentinel does.
    assert!(
        !json.contains("Sensitive"),
        "serialized diagnostic must not mention the RedactedValue variant \
         name; found it in {json}"
    );
}

#[test]
fn diagnostic_with_no_sensitive_context_contains_no_redacted_sentinel() {
    let mut context = BTreeMap::new();
    context.insert(
        "namespace".to_string(),
        RedactedValue::Public("admission-lab-candidate".to_string()),
    );
    let diagnostic = Diagnostic {
        code: "cluster.ready".to_string(),
        message: "cluster is ready".to_string(),
        context,
    };

    let json = serde_json::to_string(&diagnostic).unwrap();

    assert!(!json.contains("[REDACTED]"));
}

#[test]
fn diagnostic_context_serializes_in_sorted_key_order() {
    // `context` is a BTreeMap, so key order in the serialized object is
    // deterministic (sorted), not insertion order — this matters for
    // reproducible reports and diffable snapshots.
    let mut context = BTreeMap::new();
    context.insert(
        "zzz-last".to_string(),
        RedactedValue::Public("z".to_string()),
    );
    context.insert(
        "aaa-first".to_string(),
        RedactedValue::Public("a".to_string()),
    );
    let diagnostic = Diagnostic {
        code: "example".to_string(),
        message: "example message".to_string(),
        context,
    };

    let json = serde_json::to_string(&diagnostic).unwrap();

    let first_pos = json.find("aaa-first").unwrap();
    let last_pos = json.find("zzz-last").unwrap();
    assert!(first_pos < last_pos);
}

// ---------------------------------------------------------------------
// DiagnosticLevel
// ---------------------------------------------------------------------

#[test]
fn diagnostic_level_has_three_variants() {
    // Exhaustive match: fails to compile if a variant is added, removed,
    // or renamed without updating this test.
    let variants = [
        DiagnosticLevel::Info,
        DiagnosticLevel::Warning,
        DiagnosticLevel::Error,
    ];
    assert_eq!(variants.len(), 3);
    for variant in variants {
        match variant {
            DiagnosticLevel::Info | DiagnosticLevel::Warning | DiagnosticLevel::Error => {}
        }
    }
}
