//! Proves `recipes/test-webhook/recipe.yaml` is a real, valid Task 2.5
//! recipe: loadable through `admissionlab_recipes`' own public loader
//! (`load_recipe_overrides`), passing its full validation, and resolving
//! to exactly the fields this task declares. No cluster, no Docker, no
//! `kind` — this is a pure parsing/validation test, so it runs under the
//! default `cargo test --workspace` (unlike `tests/kind_smoke.rs`, which
//! genuinely needs a live cluster and is `#[ignore]`d).
//!
//! # The `${RECIPE_DIR}` substitution, and why it exists
//!
//! `recipe.yaml`'s own comments explain this in full; the short version:
//! `admissionlab_recipes::model::resolve_manifests` requires every
//! `install.paths` entry to already be an absolute path (Task 2.5's own
//! documented, deliberate limitation — no manifests-based recipe existed
//! yet when that rule was written). [`substituted_recipe_text`] performs
//! exactly the substitution a real loader must: replacing the
//! `${RECIPE_DIR}` placeholder with this checkout's own absolute
//! `recipes/test-webhook` directory, computed via `CARGO_MANIFEST_DIR`
//! the same way every other checked-in-fixture-referencing test in this
//! workspace does (`admissionlab-cluster/tests/kind_config.rs`,
//! `admissionlab-recipes/tests/load.rs`, and others all follow this
//! exact convention already).

use std::path::{Path, PathBuf};

use admissionlab_core::RunId;
use admissionlab_recipes::{Capability, InstallMethod, ReadinessCheck, load_recipe_overrides};

/// This checkout's own `recipes/test-webhook` directory — two levels
/// above this crate's own `CARGO_MANIFEST_DIR`, mirroring the exact
/// convention `admissionlab-recipes/tests/load.rs` already uses for
/// `compatibility/recipes.yaml`.
fn recipe_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../recipes/test-webhook")
}

/// A fresh, guaranteed-unique scratch directory under the OS temp dir.
/// Mirrors `admissionlab-cluster/tests/kind_smoke.rs`'s own
/// `unique_root` helper.
fn unique_scratch_dir(label: &str) -> PathBuf {
    let unique = RunId::generate();
    std::env::temp_dir().join(format!(
        "admissionlab-test-webhook-{label}-{}",
        unique.as_str()
    ))
}

/// Reads `recipes/test-webhook/recipe.yaml` and replaces every
/// `${RECIPE_DIR}` placeholder with `recipe_dir()`'s own absolute path —
/// see this module's own documentation.
fn substituted_recipe_text() -> String {
    let dir = recipe_dir();
    let raw = std::fs::read_to_string(dir.join("recipe.yaml"))
        .expect("recipes/test-webhook/recipe.yaml must exist and be readable");
    let absolute_manifests_dir = dir
        .canonicalize()
        .expect("recipes/test-webhook must exist as a real directory")
        .display()
        .to_string();
    raw.replace("${RECIPE_DIR}", &absolute_manifests_dir)
}

/// Writes `text` as the sole `recipe.yaml` inside a fresh scratch
/// directory and returns that directory, ready to hand to
/// `load_recipe_overrides`.
fn write_override_dir(label: &str, text: &str) -> PathBuf {
    let dir = unique_scratch_dir(label);
    std::fs::create_dir_all(&dir).expect("create scratch override directory");
    std::fs::write(dir.join("recipe.yaml"), text).expect("write substituted recipe.yaml");
    dir
}

#[test]
fn recipe_loads_and_validates_through_the_public_loader() {
    let override_dir = write_override_dir("loads", &substituted_recipe_text());

    let recipes =
        load_recipe_overrides(&override_dir).expect("recipe.yaml must load and validate cleanly");
    assert_eq!(recipes.len(), 1, "exactly one recipe.yaml was written");
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
        "30-deployment.yaml",
    ];
    assert_eq!(manifests.paths.len(), expected_files.len());
    for (path, expected_file) in manifests.paths.iter().zip(expected_files) {
        assert!(
            path.is_absolute(),
            "every resolved manifest path must be absolute, got {}",
            path.display()
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
        ],
        "readiness must gate on both Deployment availability and the webhook configuration \
         object existing -- a Deployment that is Available with no webhook configuration yet is \
         not usable"
    );

    assert!(
        recipe.capabilities.is_empty(),
        "this recipe must not yet claim Capability::Admission -- Task 3.9 implements the \
         admission-review handling that would make that claim true"
    );
    assert!(
        !recipe.capabilities.contains(&Capability::Admission),
        "explicit check for the specific capability a reader might expect, not only emptiness"
    );
    assert!(recipe.normalize_rules.is_empty());

    let _ = std::fs::remove_dir_all(&override_dir);
}

#[test]
fn recipe_is_not_directly_loadable_without_the_recipe_dir_substitution() {
    // The checked-in file, loaded completely unmodified, must fail
    // loudly -- proving the `${RECIPE_DIR}` placeholder really is
    // necessary (this test would still pass vacuously if the recipe
    // schema silently tolerated a relative path, which is exactly the
    // regression this asserts against) rather than a decorative
    // convention nothing actually enforces.
    let dir = recipe_dir();
    let error = load_recipe_overrides(&dir)
        .expect_err("recipe.yaml's unsubstituted ${RECIPE_DIR} placeholders must not resolve");
    let message = error.to_string();
    assert!(
        message.contains("install.paths"),
        "error must name the offending field, got: {message}"
    );
    assert!(
        message.contains("is relative"),
        "error must explain why (a non-absolute path), got: {message}"
    );
}
