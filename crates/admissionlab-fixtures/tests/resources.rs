//! Black-box tests for [`admissionlab_fixtures::ResourceResolver`] /
//! [`admissionlab_fixtures::KubeResourceResolver`] (Task 3.2), exercised
//! only through this crate's public API.
//!
//! Named `resources.rs`, not `discovery_unit.rs` as the brief originally
//! named it: controller supplement §1 (Task 3.2; see
//! `.superpowers/sdd/ROADMAP/task-3.2-supplement.md`) renames the module
//! this file tests to `src/resources.rs`, so its own test file follows
//! that name too.
//!
//! # What lives here versus in `src/resources.rs`'s own `tests` module
//!
//! `KubeResourceResolver::resolve`'s success path needs a real
//! `kube::Client` talking to *something* that answers Kubernetes
//! discovery requests -- a `tower_test`-mocked service, in this crate's
//! offline tests -- but there is no public seam on
//! [`admissionlab_fixtures::KubeResourceResolver`] to hand it one; the
//! only entry point a black-box caller has is
//! [`admissionlab_fixtures::ResourceResolver::resolve`], which always
//! builds its own `Client` from a [`admissionlab_core::ClusterHandle`]'s
//! real, on-disk kubeconfig. `src/resources.rs`'s own (white-box)
//! `tests` module drives the mock-backed discovery/resolution logic
//! directly, the same split
//! `admissionlab_installer::readiness`'s own module documentation
//! describes and uses. What genuinely *is* reachable from here, with no
//! live cluster, is exactly what mirrors that module's own external
//! test file (`admissionlab-installer/tests/readiness_unit.rs` does not
//! itself cover the `client_for` failure paths either -- those live in
//! `readiness.rs`'s internal `tests` too): the failure paths a
//! deliberately bad kubeconfig produces.

use std::path::PathBuf;

use admissionlab_core::{ClusterSpec, RunId, Side};
use admissionlab_fixtures::{FixtureError, KubeResourceResolver, ResourceResolver};

/// A fresh, guaranteed-unique path under the OS temp dir for one test's
/// kubeconfig file -- mirrors
/// `admissionlab_installer::readiness`'s own `unique_kubeconfig_path`
/// test helper.
fn unique_kubeconfig_path(label: &str) -> PathBuf {
    let unique = RunId::generate();
    std::env::temp_dir().join(format!(
        "admissionlab-fixtures-resources-external-test-{label}-{}.yaml",
        unique.as_str()
    ))
}

/// A minimal, otherwise-valid [`admissionlab_core::ClusterHandle`]
/// pointing at `kubeconfig`. Only `kubeconfig` varies per test; every
/// other field is a fixed, inert placeholder [`KubeResourceResolver`]
/// never inspects before it needs a client.
fn cluster_handle_with_kubeconfig(kubeconfig: PathBuf) -> admissionlab_core::ClusterHandle {
    admissionlab_core::ClusterHandle {
        spec: ClusterSpec {
            side: Side::Baseline,
            name: "resources-external-test-cluster".to_string(),
            kubernetes_version: "1.36.0".to_string(),
            node_image: "kindest/node:v1.36.0".to_string(),
        },
        kubeconfig,
        audit_log: std::env::temp_dir()
            .join("admissionlab-fixtures-resources-external-test-audit.log"),
    }
}

#[tokio::test]
async fn resolve_fails_with_resource_discovery_unavailable_when_kubeconfig_is_missing() {
    let cluster = cluster_handle_with_kubeconfig(unique_kubeconfig_path("missing"));
    let resolver = KubeResourceResolver::new();

    let error = resolver
        .resolve(&cluster, "v1", "ConfigMap")
        .await
        .expect_err("a nonexistent kubeconfig path must not resolve successfully");

    match error {
        FixtureError::ResourceDiscoveryUnavailable { cluster, reason } => {
            assert_eq!(cluster, "resources-external-test-cluster");
            assert!(
                !reason.is_empty(),
                "ResourceDiscoveryUnavailable::reason must carry a real message, not an empty one"
            );
        }
        other => panic!("expected ResourceDiscoveryUnavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn resolve_fails_with_resource_discovery_unavailable_when_kubeconfig_is_malformed() {
    let path = unique_kubeconfig_path("malformed");
    std::fs::write(&path, "not: [valid yaml: at all -- }}}").expect("write malformed kubeconfig");
    let cluster = cluster_handle_with_kubeconfig(path.clone());
    let resolver = KubeResourceResolver::new();

    let error = resolver
        .resolve(&cluster, "v1", "ConfigMap")
        .await
        .expect_err("malformed kubeconfig YAML must not parse into a usable client");

    assert!(matches!(
        error,
        FixtureError::ResourceDiscoveryUnavailable { .. }
    ));

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn resolve_dispatches_through_a_trait_object() {
    // Fails if `ResourceResolver` were accidentally not object-safe, or
    // if dynamic dispatch somehow bypassed `KubeResourceResolver`'s own
    // `resolve` (in which case this would not compile, or would not
    // reach the same `client_for` failure the direct-call test above
    // does).
    let cluster = cluster_handle_with_kubeconfig(unique_kubeconfig_path("trait-object"));
    let resolver: Box<dyn ResourceResolver> = Box::new(KubeResourceResolver::new());

    let error = resolver
        .resolve(&cluster, "v1", "ConfigMap")
        .await
        .expect_err("a nonexistent kubeconfig path must not resolve successfully");

    assert!(matches!(
        error,
        FixtureError::ResourceDiscoveryUnavailable { .. }
    ));
}

#[tokio::test]
async fn invalidate_before_any_resolve_is_a_harmless_no_op() {
    let resolver = KubeResourceResolver::new();
    let cluster = cluster_handle_with_kubeconfig(unique_kubeconfig_path("never-resolved"));

    // Fails only if this panics or hangs -- there is nothing else to
    // assert about removing an absent cache entry through the public
    // API alone.
    resolver.invalidate(&cluster).await;
}
