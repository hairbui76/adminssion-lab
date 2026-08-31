//! Proves `recipes/test-webhook/recipe.yaml` is a real, valid Task 2.5
//! recipe: loadable through `admissionlab_recipes`' own public loader
//! (`load_recipe_overrides`), directly from its real, checked-in
//! location, with no test-side path rewriting of any kind; passing its
//! full validation; and resolving to exactly the fields this task
//! declares. No cluster, no Docker, no `kind` — this is a pure
//! parsing/validation test, so it runs under the default `cargo test
//! --workspace` (unlike `tests/kind_smoke.rs`, which genuinely needs a
//! live cluster and is `#[ignore]`d).
//!
//! # Relative paths, resolved against this recipe's own directory
//!
//! `recipe.yaml`'s own header comment explains this in full (Task
//! 2.10): `install.paths` is written as plain paths relative to
//! `recipes/test-webhook/` itself (`manifests/00-namespace.yaml` and so
//! on), and `admissionlab_recipes::load_recipe_overrides` resolves each
//! one against the directory the recipe file was actually found in —
//! this recipe's own directory, when loaded the way this file loads it.
//! Nothing here substitutes a placeholder or rewrites a copy of the
//! checked-in file before loading it: this test calls
//! `load_recipe_overrides` directly on `recipes/test-webhook`, the same
//! call a real caller (for example the Phase 2 exit gate) makes. Task
//! 2.7's own version of this file rewrote a placeholder token into a
//! scratch copy before loading it (see Task 2.10's own report for the
//! exact name) — that placeholder and the helper that substituted it
//! are both gone; the token does not appear anywhere in this repository
//! any more.

use std::path::{Path, PathBuf};

use admissionlab_installer::load_manifest_bundle;
use admissionlab_recipes::{Capability, InstallMethod, ReadinessCheck, load_recipe_overrides};

/// This checkout's own `recipes/test-webhook` directory — two levels
/// above this crate's own `CARGO_MANIFEST_DIR`, mirroring the exact
/// convention `admissionlab-recipes/tests/load.rs` already uses for
/// `compatibility/recipes.yaml`.
fn recipe_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../recipes/test-webhook")
}

#[test]
fn recipe_loads_and_validates_through_the_public_loader() {
    let dir = recipe_dir();

    let recipes = load_recipe_overrides(&dir)
        .expect("recipe.yaml must load and validate cleanly straight from its own directory");
    assert_eq!(
        recipes.len(),
        1,
        "exactly one recipe.yaml lives in this directory"
    );
    let recipe = &recipes[0];

    assert_eq!(recipe.name, "test-webhook");
    assert_eq!(recipe.version, "0.1.0");

    let InstallMethod::Manifests(manifests) = &recipe.install else {
        panic!(
            "expected a Manifests install method, got {:?}",
            recipe.install
        );
    };
    let expected_files = [
        "00-namespace.yaml",
        "10-rbac.yaml",
        "20-webhook-configuration.yaml",
        "21-mutating-webhook-configurations.yaml",
        "30-deployment.yaml",
    ];
    assert_eq!(manifests.paths.len(), expected_files.len());
    for (path, expected_file) in manifests.paths.iter().zip(expected_files) {
        assert!(
            path.is_absolute(),
            "every resolved manifest path must be absolute, got {}",
            path.display()
        );
        assert!(
            path.starts_with(&dir),
            "{} must resolve inside this recipe's own directory ({}) -- the directory \
             confinement Task 2.10 adds, exercised here against the real checked-in recipe \
             rather than a synthetic one",
            path.display(),
            dir.display()
        );
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(expected_file),
            "manifest install order must match recipe.yaml's own declared order"
        );
        assert!(
            path.is_file(),
            "{} must actually exist on disk (recipe.yaml must not drift from manifests/)",
            path.display()
        );
    }

    assert_eq!(
        recipe.readiness,
        vec![
            ReadinessCheck::DeploymentAvailable {
                namespace: "admissionlab-test-webhook".to_owned(),
                name: "admissionlab-test-webhook".to_owned(),
            },
            ReadinessCheck::WebhookConfigurationPresent {
                name: "admissionlab-test-webhook".to_owned(),
            },
            ReadinessCheck::WebhookConfigurationPresent {
                name: "admissionlab-test-webhook-mutate-containers".to_owned(),
            },
            ReadinessCheck::WebhookConfigurationPresent {
                name: "admissionlab-test-webhook-mutate-labels".to_owned(),
            },
        ],
        "readiness must gate on Deployment availability and on *every* webhook configuration \
         object existing -- with failurePolicy: Fail on all three, a fixture submitted while one \
         of them is still missing gets a quietly different answer rather than an error"
    );

    // Claimed as of Task 3.9, and only because it is now true: this
    // recipe installs real mutating and validating webhook
    // configurations backed by a server that answers admission reviews
    // (`crates/admissionlab-test-webhook/tests/behavior.rs` is the
    // regression test for the behavior itself). Task 2.7's version of
    // this test asserted the opposite for exactly the same reason --
    // Global Constraint 15: never claim a capability the component does
    // not functionally provide.
    assert!(
        recipe.capabilities.contains(&Capability::Admission),
        "this recipe installs a working admission webhook, so it must claim \
         Capability::Admission"
    );
    assert!(recipe.normalize_rules.is_empty());
}

/// Pins `30-deployment.yaml`'s `spec.replicas` at exactly `1` — see
/// that file's own comment on the line, and
/// `crates/admissionlab-test-webhook/src/bootstrap.rs`'s module
/// documentation ("Every guarantee above is per-pod; none of it
/// composes across pods"), for the full reasoning: `caBundle`
/// (`20-webhook-configuration.yaml`) is one cluster-wide value, written
/// last-write-wins by whichever pod's `bootstrap` init container ran
/// last, with no cross-pod coordination at all. Two pods would each
/// generate an independent CA and race to overwrite it, and neither of
/// this recipe's own readiness checks (`deploymentAvailable`,
/// `webhookConfigurationPresent`) would ever notice — both would report
/// ready while the webhook served an untrusted certificate on whichever
/// pod lost the race.
///
/// Parses the real, checked-in manifest file through
/// `admissionlab_installer::load_manifest_bundle` (Task 2.3's own,
/// already-tested manifest loader) rather than a hand-rolled YAML
/// read, so this test exercises the same parsing path production code
/// does. A future edit that raises `replicas` — by hand, or as an
/// unnoticed side effect of some other change to this file — fails
/// here, in a cluster-free test that runs on every `cargo test
/// --workspace`, instead of only surfacing later as an intermittent
/// TLS trust failure once two pods are briefly both `Running` (for
/// example during a `RollingUpdate` surge).
#[test]
fn deployment_replicas_is_pinned_to_one() {
    let deployment_manifest = recipe_dir().join("manifests/30-deployment.yaml");
    let bundle = load_manifest_bundle(std::slice::from_ref(&deployment_manifest))
        .expect("30-deployment.yaml must parse as valid Kubernetes manifests");

    let deployment_documents: Vec<_> = bundle
        .documents
        .iter()
        .filter(|document| {
            document.get("kind").and_then(|kind| kind.as_str()) == Some("Deployment")
        })
        .collect();
    assert_eq!(
        deployment_documents.len(),
        1,
        "30-deployment.yaml must declare exactly one Deployment document, found: {:?}",
        bundle.documents
    );
    let deployment = deployment_documents[0];

    let replicas = deployment
        .pointer("/spec/replicas")
        .and_then(serde_json::Value::as_i64);
    assert_eq!(
        replicas,
        Some(1),
        "spec.replicas must stay pinned at 1 -- caBundle is a single cluster-wide \
         value with no cross-pod coordination; see this test's own doc comment \
         and crates/admissionlab-test-webhook/src/bootstrap.rs's module \
         documentation before changing this"
    );
}
