//! Behavioral tests for `admissionlab-recipes`' loading, parsing, and
//! validation.
//!
//! Three properties are load-bearing here and are each covered by tests
//! below:
//!
//! - **No regression-classification logic can enter a recipe.** PRODUCT.md
//!   §14 / Global Constraint 6: a recipe must not contain `failOn`,
//!   `severity`, or any other semantic regression-classification key, at
//!   any nesting level. This is the load-bearing guarantee Task 2.5
//!   exists to enforce (see the "No classification logic" section below)
//!   -- and a fully valid recipe using only what PRODUCT.md §14 allows
//!   must still load, so the rejection tests are not vacuously passing a
//!   validator that rejects everything.
//! - **Built-in recipes are embedded, not read from disk at runtime.**
//!   No recipe shipped through Task 2.7; Task 2.8 adds the first one
//!   (`kyverno`, wired into `BUILTIN_RECIPES`) — the tests below prove
//!   both that the loading mechanism itself works and that this one
//!   real entry resolves correctly, end to end. A real, live-cluster
//!   install of it is proven separately, in `tests/kyverno_recipe.rs`
//!   (`#[ignore]`d — needs Docker and `kind`).
//! - **A local override directory is never consulted unless a caller
//!   explicitly names one.** No environment variable, home directory, or
//!   current-working-directory convention is consulted implicitly.
//!
//! A final section separately validates `compatibility/recipes.yaml`'s
//! shape and the specific facts it records (Controller Ruling R28).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use admissionlab_recipes::{
    Capability, InstallMethod, ReadinessCheck, RecipeError, RecipeNormalizeRule,
    load_builtin_recipes, load_recipe_compatibility, load_recipe_overrides, load_recipes,
};

// ---------------------------------------------------------------------
// Test support
// ---------------------------------------------------------------------

/// A fresh, guaranteed-unique directory under the system temp directory,
/// for tests that need real recipe files on disk. Mirrors
/// `admissionlab-spec`'s own `tests/load.rs` helper of the same name and
/// shape.
fn unique_temp_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-recipes-load-test-{}-{label}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create unique temp dir");
    dir
}

/// Strips the leading whitespace common to every non-empty line, so a
/// YAML literal can be indented to match the surrounding Rust code
/// without that indentation becoming part of the (indentation-sensitive)
/// YAML content. Mirrors `admissionlab-spec`'s own `tests/load.rs`
/// helper of the same name and shape.
fn dedent(text: &str) -> String {
    let indent = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    text.lines()
        .map(|line| line.get(indent..).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Writes `yaml` (dedented) to `file_name` inside a fresh unique temp
/// directory and returns that directory.
fn write_recipe_dir(label: &str, file_name: &str, yaml: &str) -> PathBuf {
    let dir = unique_temp_dir(label);
    std::fs::write(dir.join(file_name), dedent(yaml)).expect("write temp recipe file");
    dir
}

/// Serializes changes to the process's current directory so
/// CWD-manipulating tests in this file cannot race each other; restores
/// the original directory on drop (including on panic/failed assertion).
/// Mirrors `admissionlab-spec`'s own `tests/load.rs` `CwdGuard`.
static CWD_LOCK: Mutex<()> = Mutex::new(());

struct CwdGuard {
    original: PathBuf,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl CwdGuard {
    fn change_to(dir: &Path) -> Self {
        let lock = CWD_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original = std::env::current_dir().expect("read current directory");
        std::env::set_current_dir(dir).expect("change current directory");
        Self {
            original,
            _lock: lock,
        }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

/// A complete, valid recipe using only fields PRODUCT.md §14 allows:
/// installation defaults, readiness checks, known harmless normalization
/// rules, and capability metadata. `name` is `"demo-webhook"` throughout
/// this file's tests that use it verbatim.
const VALID_RECIPE_YAML: &str = r#"
name: demo-webhook
version: "1.2.3"
install:
  type: helm
  chart: demo/webhook
  repo: https://example.invalid/charts
  version: "1.2.3"
readiness:
  - type: deploymentAvailable
    namespace: demo
    name: demo-webhook-controller
  - type: webhookConfigurationPresent
    name: demo-webhook.example.invalid
normalizeRules:
  - type: removeAnnotation
    annotation: demo.example.invalid/last-request-timestamp
  - type: sortNamedArray
    pointer: /webhooks
    key: name
capabilities:
  - admission
"#;

/// A minimal valid recipe body, for tests that only care about one
/// injected extra field/value and do not need the full fixture above.
fn minimal_recipe_with_extra(extra_yaml_line: &str) -> String {
    format!(
        r#"
        name: evil
        version: "1.0.0"
        install:
          type: helm
          chart: evil/evil
          repo: https://example.invalid/charts
          version: "1.0.0"
        {extra_yaml_line}
        "#
    )
}

// ---------------------------------------------------------------------
// Positive case: a fully valid recipe loads and resolves every field
// ---------------------------------------------------------------------
//
// This is the counterweight to every rejection test below: a validator
// that rejected everything would trivially pass those tests too, so this
// one proves the schema actually accepts real, well-formed content.

#[test]
fn valid_recipe_loads_and_resolves_every_field() {
    let dir = write_recipe_dir("valid-recipe", "demo.yaml", VALID_RECIPE_YAML);

    let recipes = load_recipe_overrides(&dir).expect("a fully valid recipe must load");
    assert_eq!(recipes.len(), 1);
    let recipe = &recipes[0];
    assert_eq!(recipe.name, "demo-webhook");
    assert_eq!(recipe.version, "1.2.3");

    match &recipe.install {
        InstallMethod::Helm(helm) => {
            assert_eq!(helm.chart, "demo/webhook");
            assert_eq!(helm.repo_url, "https://example.invalid/charts");
            assert_eq!(helm.version, "1.2.3");
            // repo_name/release_name/namespace default to the recipe's
            // own name, mirroring admissionlab-spec's own Helm-install
            // defaulting (component::resolve_helm).
            assert_eq!(helm.repo_name, "demo-webhook");
            assert_eq!(helm.release_name, "demo-webhook");
            assert_eq!(helm.namespace, "demo-webhook");
            assert!(helm.values_files.is_empty());
            assert!(helm.set_values.is_empty());
        }
        InstallMethod::Manifests(_) => panic!("expected a Helm install method"),
    }

    assert_eq!(recipe.readiness.len(), 2);
    assert!(matches!(
        &recipe.readiness[0],
        ReadinessCheck::DeploymentAvailable { namespace, name }
            if namespace == "demo" && name == "demo-webhook-controller"
    ));
    assert!(matches!(
        &recipe.readiness[1],
        ReadinessCheck::WebhookConfigurationPresent { name }
            if name == "demo-webhook.example.invalid"
    ));

    assert_eq!(recipe.normalize_rules.len(), 2);
    assert!(matches!(
        &recipe.normalize_rules[0],
        RecipeNormalizeRule::RemoveAnnotation(annotation)
            if annotation == "demo.example.invalid/last-request-timestamp"
    ));
    assert!(matches!(
        &recipe.normalize_rules[1],
        RecipeNormalizeRule::SortNamedArray { pointer, key }
            if pointer == "/webhooks" && key == "name"
    ));

    assert_eq!(recipe.capabilities, BTreeSet::from([Capability::Admission]));
}

#[test]
fn explicit_repo_name_release_name_and_namespace_override_the_recipe_name_default() {
    // The positive-case test above only exercises the *default* path
    // (repoName/releaseName/namespace all omitted, falling back to the
    // recipe's own name). This closes the other half: an explicit value
    // for each must be honored instead of the default -- exactly the
    // kind of untested-but-assumed-correct branch that turned out to
    // hide a real bug elsewhere in this same file (see
    // `every_readiness_check_and_normalize_rule_variant_resolves_correctly`'s
    // own comment).
    let dir = write_recipe_dir(
        "explicit-helm-overrides",
        "demo.yaml",
        r#"
        name: istiod
        version: "1.30.4"
        install:
          type: helm
          chart: istio/istiod
          repo: https://istio-release.storage.googleapis.com/charts
          version: "1.30.4"
          repoName: istio
          releaseName: istiod
          namespace: istio-system
        "#,
    );

    let recipes = load_recipe_overrides(&dir).expect("explicit Helm overrides must load");
    match &recipes[0].install {
        InstallMethod::Helm(helm) => {
            assert_eq!(helm.repo_name, "istio");
            assert_eq!(helm.release_name, "istiod");
            assert_eq!(
                helm.namespace, "istio-system",
                "an explicit namespace must override the recipe-name default (\"istiod\"), \
                 exactly the istio/istiod real-world case that motivates the override existing \
                 at all"
            );
        }
        InstallMethod::Manifests(_) => panic!("expected a Helm install method"),
    }
}

#[test]
fn manifests_install_method_recipe_loads() {
    let dir = write_recipe_dir(
        "manifests-recipe",
        "demo.yaml",
        r#"
        name: raw-webhook
        version: "1.0.0"
        install:
          type: manifests
          paths:
            - /opt/admissionlab-recipes-fixture/webhook.yaml
        "#,
    );

    let recipes = load_recipe_overrides(&dir).expect("a manifests install recipe must load");
    match &recipes[0].install {
        InstallMethod::Manifests(manifests) => {
            assert_eq!(
                manifests.paths,
                vec![PathBuf::from(
                    "/opt/admissionlab-recipes-fixture/webhook.yaml"
                )]
            );
        }
        InstallMethod::Helm(_) => panic!("expected a Manifests install method"),
    }
}

#[test]
fn every_readiness_check_and_normalize_rule_variant_resolves_correctly() {
    // The main positive-case test above only exercises
    // DeploymentAvailable/WebhookConfigurationPresent and
    // RemoveAnnotation/SortNamedArray. This test closes the remaining
    // gap -- DaemonSetReady, JobComplete, CustomResourceCondition (both
    // with and without its optional `namespace`), and RemovePointer --
    // so every variant's `camelCase` field renaming and resolution logic
    // is actually exercised by a passing test at least once, not merely
    // assumed correct by analogy to the ones that are.
    let dir = write_recipe_dir(
        "every-variant",
        "demo.yaml",
        r#"
        name: full-coverage
        version: "1.0.0"
        install:
          type: helm
          chart: demo/full-coverage
          repo: https://example.invalid/charts
          version: "1.0.0"
        readiness:
          - type: daemonSetReady
            namespace: demo-ns
            name: demo-daemonset
          - type: jobComplete
            namespace: demo-ns
            name: demo-job
          - type: customResourceCondition
            apiVersion: demo.example.invalid/v1
            kind: DemoPolicy
            namespace: demo-ns
            name: demo-policy
            conditionType: Ready
            status: "True"
          - type: customResourceCondition
            apiVersion: demo.example.invalid/v1
            kind: DemoClusterPolicy
            name: demo-cluster-policy
            conditionType: Ready
            status: "True"
        normalizeRules:
          - type: removePointer
            pointer: /metadata/annotations/demo.example.invalid~1last-request-timestamp
        "#,
    );

    let recipes =
        load_recipe_overrides(&dir).expect("every readiness/normalize variant must resolve");
    let recipe = &recipes[0];

    assert!(matches!(
        &recipe.readiness[0],
        ReadinessCheck::DaemonSetReady { namespace, name }
            if namespace == "demo-ns" && name == "demo-daemonset"
    ));
    assert!(matches!(
        &recipe.readiness[1],
        ReadinessCheck::JobComplete { namespace, name }
            if namespace == "demo-ns" && name == "demo-job"
    ));
    assert!(matches!(
        &recipe.readiness[2],
        ReadinessCheck::CustomResourceCondition {
            api_version,
            kind,
            namespace: Some(namespace),
            name,
            condition_type,
            status,
        } if api_version == "demo.example.invalid/v1"
            && kind == "DemoPolicy"
            && namespace == "demo-ns"
            && name == "demo-policy"
            && condition_type == "Ready"
            && status == "True"
    ));
    assert!(
        matches!(
            &recipe.readiness[3],
            ReadinessCheck::CustomResourceCondition {
                kind,
                namespace: None,
                name,
                ..
            } if kind == "DemoClusterPolicy" && name == "demo-cluster-policy"
        ),
        "a cluster-scoped custom resource condition (no namespace) must resolve to None, \
         got {:?}",
        recipe.readiness[3]
    );

    assert!(matches!(
        &recipe.normalize_rules[0],
        RecipeNormalizeRule::RemovePointer(pointer)
            if pointer == "/metadata/annotations/demo.example.invalid~1last-request-timestamp"
    ));
}

// ---------------------------------------------------------------------
// The load-bearing guarantee: no classification logic (PRODUCT.md §14)
// ---------------------------------------------------------------------

#[test]
fn top_level_fail_on_is_rejected() {
    let dir = write_recipe_dir(
        "fail-on-top-level",
        "evil.yaml",
        &minimal_recipe_with_extra(r#"failOn: ["breaking-change"]"#),
    );

    let err = load_recipe_overrides(&dir).expect_err("a top-level failOn key must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("failOn"),
        "error must name the offending field; got {message:?}"
    );
}

#[test]
fn top_level_severity_is_rejected() {
    let dir = write_recipe_dir(
        "severity-top-level",
        "evil.yaml",
        &minimal_recipe_with_extra("severity: critical"),
    );

    let err = load_recipe_overrides(&dir).expect_err("a top-level severity key must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("severity"),
        "error must name the offending field; got {message:?}"
    );
}

#[test]
fn fail_on_nested_inside_a_normalize_rule_is_rejected() {
    let dir = write_recipe_dir(
        "fail-on-nested-normalize",
        "evil.yaml",
        r#"
        name: evil
        version: "1.0.0"
        install:
          type: helm
          chart: evil/evil
          repo: https://example.invalid/charts
          version: "1.0.0"
        normalizeRules:
          - type: removeAnnotation
            annotation: some.annotation/here
            failOn: true
        "#,
    );

    let err = load_recipe_overrides(&dir)
        .expect_err("a failOn key nested inside a normalize rule must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("failOn"),
        "error must name the offending field; got {message:?}"
    );
}

#[test]
fn severity_nested_inside_a_readiness_check_is_rejected() {
    let dir = write_recipe_dir(
        "severity-nested-readiness",
        "evil.yaml",
        r#"
        name: evil
        version: "1.0.0"
        install:
          type: helm
          chart: evil/evil
          repo: https://example.invalid/charts
          version: "1.0.0"
        readiness:
          - type: deploymentAvailable
            namespace: evil
            name: evil-controller
            severity: high
        "#,
    );

    let err = load_recipe_overrides(&dir)
        .expect_err("a severity key nested inside a readiness check must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("severity"),
        "error must name the offending field; got {message:?}"
    );
}

#[test]
fn other_classification_shaped_keys_beyond_fail_on_and_severity_are_rejected() {
    // Proves the defense is a general allow-list (`deny_unknown_fields`
    // on every raw struct), not a hardcoded blocklist of exactly the two
    // strings PRODUCT.md §14 names as examples: an invented
    // classification-flavored key -- literally neither "failOn" nor
    // "severity" -- must be rejected by the very same mechanism.
    for key in [
        "classification",
        "regressionClass",
        "riskLevel",
        "priority",
        "ignoreRegression",
        "blocking",
        "gate",
        "expectedChange",
    ] {
        let dir = write_recipe_dir(
            &format!("other-classification-key-{key}"),
            "evil.yaml",
            &minimal_recipe_with_extra(&format!("{key}: true")),
        );

        let err = load_recipe_overrides(&dir)
            .unwrap_err_with_context(&format!("a top-level {key:?} key must be rejected"));
        assert!(err.contains(key), "error must name {key:?}; got {err:?}");
    }
}

#[test]
fn inventing_a_new_normalize_rule_type_is_rejected() {
    // A hypothetical "type: treatAsEquivalent" rule would be
    // classification logic disguised as normalization -- structurally
    // impossible here: `RecipeNormalizeRule`'s variant set is closed and
    // owned by admissionlab-spec, not this crate, so an invented variant
    // name is simply an unrecognized enum tag.
    let dir = write_recipe_dir(
        "invented-normalize-rule-type",
        "evil.yaml",
        r#"
        name: evil
        version: "1.0.0"
        install:
          type: helm
          chart: evil/evil
          repo: https://example.invalid/charts
          version: "1.0.0"
        normalizeRules:
          - type: treatAsEquivalent
            reason: "this difference is expected, not a regression"
        "#,
    );

    let err =
        load_recipe_overrides(&dir).expect_err("an invented normalize rule type must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("treatAsEquivalent"),
        "error must name the unrecognized variant; got {message:?}"
    );
}

#[test]
fn unknown_capability_string_is_rejected() {
    let dir = write_recipe_dir(
        "unknown-capability",
        "evil.yaml",
        &minimal_recipe_with_extra(r#"capabilities: ["breakingChangeApprover"]"#),
    );

    let err = load_recipe_overrides(&dir)
        .expect_err("an unrecognized capability string must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("breakingChangeApprover"),
        "error must name the offending value; got {message:?}"
    );
}

// ---------------------------------------------------------------------
// Other semantic validation
// ---------------------------------------------------------------------

#[test]
fn empty_recipe_name_is_rejected() {
    let dir = write_recipe_dir(
        "empty-name",
        "evil.yaml",
        r#"
        name: ""
        version: "1.0.0"
        install:
          type: helm
          chart: evil/evil
          repo: https://example.invalid/charts
          version: "1.0.0"
        "#,
    );

    let err = load_recipe_overrides(&dir).expect_err("an empty recipe name must be rejected");
    assert!(err.to_string().contains("name"));
}

#[test]
fn blank_recipe_version_is_rejected() {
    let dir = write_recipe_dir(
        "blank-version",
        "evil.yaml",
        r#"
        name: evil
        version: "   "
        install:
          type: helm
          chart: evil/evil
          repo: https://example.invalid/charts
          version: "1.0.0"
        "#,
    );

    let err = load_recipe_overrides(&dir).expect_err("a whitespace-only version must be rejected");
    assert!(err.to_string().contains("version"));
}

#[test]
fn floating_helm_version_is_rejected() {
    let dir = write_recipe_dir(
        "floating-helm-version",
        "evil.yaml",
        r#"
        name: evil
        version: "1.0.0"
        install:
          type: helm
          chart: evil/evil
          repo: https://example.invalid/charts
          version: "^1.0"
        "#,
    );

    let err = load_recipe_overrides(&dir)
        .expect_err("a floating (non-pinned) Helm chart version must be rejected");
    assert!(err.to_string().contains("not an exact pinned version"));
}

#[test]
fn unrecognized_install_type_is_rejected() {
    let dir = write_recipe_dir(
        "unknown-install-type",
        "evil.yaml",
        r#"
        name: evil
        version: "1.0.0"
        install:
          type: kustomize
          path: overlays/prod
        "#,
    );

    let err =
        load_recipe_overrides(&dir).expect_err("an unrecognized install type must be rejected");
    assert!(err.to_string().contains("kustomize"));
}

#[test]
fn unrecognized_readiness_check_type_is_rejected() {
    let dir = write_recipe_dir(
        "unknown-readiness-type",
        "evil.yaml",
        r#"
        name: evil
        version: "1.0.0"
        install:
          type: helm
          chart: evil/evil
          repo: https://example.invalid/charts
          version: "1.0.0"
        readiness:
          - type: podRunning
            namespace: evil
            name: evil-pod
        "#,
    );

    let err = load_recipe_overrides(&dir)
        .expect_err("an unrecognized readiness check type must be rejected");
    assert!(err.to_string().contains("podRunning"));
}

#[test]
fn empty_manifests_paths_is_rejected() {
    let dir = write_recipe_dir(
        "empty-manifests-paths",
        "evil.yaml",
        r#"
        name: evil
        version: "1.0.0"
        install:
          type: manifests
          paths: []
        "#,
    );

    let err =
        load_recipe_overrides(&dir).expect_err("an empty manifests path list must be rejected");
    assert!(err.to_string().contains("paths"));
}

#[test]
fn relative_manifest_path_in_a_recipe_is_rejected() {
    let dir = write_recipe_dir(
        "relative-manifest-path",
        "evil.yaml",
        r#"
        name: evil
        version: "1.0.0"
        install:
          type: manifests
          paths:
            - relative/webhook.yaml
        "#,
    );

    let err = load_recipe_overrides(&dir)
        .expect_err("a relative manifest path in a recipe must be rejected");
    assert!(err.to_string().contains("relative"));
}

// ---------------------------------------------------------------------
// Built-in recipes: embedded, not read from disk
// ---------------------------------------------------------------------

#[test]
fn builtin_recipes_load_without_touching_the_filesystem() {
    // Task 2.8 added the first real built-in recipe (kyverno); Task 2.9
    // added the second (istio) -- this proves load_builtin_recipes
    // resolves both purely from their embedded include_str! text:
    // succeeding here needs no filesystem setup at all (no directory
    // created, no current directory changed), unlike every
    // override-directory test below.
    let recipes = load_builtin_recipes().expect("loading the embedded built-in set must not fail");
    assert_eq!(
        recipes.len(),
        2,
        "expected exactly the kyverno and istio recipes as of Task 2.9; got {recipes:?}"
    );
    let mut names: Vec<&str> = recipes.iter().map(|r| r.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["istio", "kyverno"]);
}

// ---------------------------------------------------------------------
// Local override directory: explicit opt-in only
// ---------------------------------------------------------------------

#[test]
fn override_directory_is_never_consulted_via_the_current_working_directory() {
    let root = unique_temp_dir("cwd-must-not-be-implicit-source");
    // Every plausible "implicit local override" location a future
    // maintainer might be tempted to check automatically, all seeded
    // with a fully valid recipe so a false negative (accidentally *not*
    // testing the real code path) is not possible.
    for candidate in [
        "recipes",
        "recipes-local",
        ".admissionlab/recipes",
        "admissionlab-recipes",
    ] {
        let candidate_dir = root.join(candidate);
        std::fs::create_dir_all(&candidate_dir).unwrap();
        std::fs::write(candidate_dir.join("evil.yaml"), dedent(VALID_RECIPE_YAML)).unwrap();
    }

    let _guard = CwdGuard::change_to(&root);
    let recipes = load_recipes(None).expect("load_recipes(None) must succeed");
    // Exactly the built-in set (kyverno and istio, as of Task 2.9) --
    // never "demo-webhook", the name every seeded current-directory
    // candidate above declares.
    assert_eq!(
        recipes.len(),
        2,
        "load_recipes(None) must never discover a recipe via the current working directory; \
         got {recipes:?}"
    );
    let names: Vec<&str> = recipes.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["istio", "kyverno"], "sorted by name");
}

#[test]
fn explicit_override_directory_is_loaded() {
    let dir = write_recipe_dir("explicit-override", "demo.yaml", VALID_RECIPE_YAML);

    let recipes =
        load_recipes(Some(&dir)).expect("an explicitly named override directory must load");
    // The built-in kyverno and istio recipes, plus this override -- a
    // distinct name, so it is added alongside rather than replacing
    // anything; see `two_distinct_override_recipes_both_load_sorted_by_name`
    // below for the multi-override case and `crate::load`'s own
    // `override_replaces_a_builtin_of_the_same_name` unit test for the
    // "replaces" half of this behavior, which needs a same-named
    // built-in this integration test does not have.
    assert_eq!(recipes.len(), 3);
    let names: Vec<&str> = recipes.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["demo-webhook", "istio", "kyverno"],
        "sorted by name"
    );
}

#[test]
fn two_distinct_override_recipes_both_load_sorted_by_name() {
    let dir = unique_temp_dir("two-overrides");
    std::fs::write(dir.join("a.yaml"), dedent(VALID_RECIPE_YAML)).unwrap();
    std::fs::write(
        dir.join("b.yaml"),
        dedent(
            VALID_RECIPE_YAML
                .replace("demo-webhook", "another-webhook")
                .as_str(),
        ),
    )
    .unwrap();

    let recipes = load_recipes(Some(&dir)).expect("two distinct override recipes must load");
    let names: Vec<&str> = recipes.iter().map(|r| r.name.as_str()).collect();
    // Both overrides, plus the built-in kyverno and istio recipes (Task
    // 2.8/2.9) -- all four sorted by name together.
    assert_eq!(
        names,
        vec!["another-webhook", "demo-webhook", "istio", "kyverno"]
    );
}

#[test]
fn duplicate_recipe_name_within_the_override_directory_is_rejected() {
    let dir = unique_temp_dir("duplicate-override-name");
    std::fs::write(dir.join("a.yaml"), dedent(VALID_RECIPE_YAML)).unwrap();
    std::fs::write(dir.join("b.yaml"), dedent(VALID_RECIPE_YAML)).unwrap();

    let err = load_recipe_overrides(&dir)
        .expect_err("two override files declaring the same recipe name must be rejected");
    assert!(err.to_string().contains("demo-webhook"));
}

#[test]
fn missing_override_directory_is_a_loud_error_not_silently_empty() {
    let dir = unique_temp_dir("never-created").join("does-not-exist");

    let err = load_recipe_overrides(&dir).expect_err("a nonexistent override directory must error");
    assert!(matches!(err, RecipeError::Io { .. }));
}

#[test]
fn non_yaml_files_in_the_override_directory_are_ignored() {
    let dir = write_recipe_dir("non-yaml-ignored", "demo.yaml", VALID_RECIPE_YAML);
    std::fs::write(dir.join("README.md"), "not a recipe\n").unwrap();
    std::fs::create_dir(dir.join("subdir")).unwrap();

    let recipes = load_recipe_overrides(&dir)
        .expect("non-YAML files and subdirectories must not break override loading");
    assert_eq!(recipes.len(), 1);
}

// ---------------------------------------------------------------------
// compatibility/recipes.yaml: shape and facts (Controller Ruling R28)
//
// Task 2.8 promoted the struct these tests used to parse this file
// (formerly private to this test file) into a real, `pub`
// `admissionlab_recipes::compat` API — `load_recipe_compatibility`,
// `RecipeCompatibilityMatrix`, and friends. `crates/admissionlab-recipes/tests/kyverno_recipe.rs`
// is now this file's real, non-test consumer: it reads the `kyverno`
// entry's `certified` list at test time to decide which Kubernetes
// version to install and certify against, rather than hardcoding a copy
// of it. The tests below still independently assert this file's shape
// and content -- unchanged from before Task 2.8 -- just through the
// promoted API instead of a private duplicate of it.
// ---------------------------------------------------------------------

#[test]
fn compatibility_recipes_yaml_has_the_expected_shape() {
    let matrix = load_recipe_compatibility()
        .expect("compatibility/recipes.yaml must parse against the recipe-compatibility shape");
    assert!(!matrix.recipes.is_empty());
    for recipe in &matrix.recipes {
        assert!(!recipe.name.trim().is_empty());
        assert!(!recipe.version.trim().is_empty());
        assert!(
            !recipe.kubernetes.certified.is_empty(),
            "{}: certified must not be empty",
            recipe.name
        );
    }
}

#[test]
fn kyverno_certified_kubernetes_excludes_the_tier_1_primary() {
    let matrix = load_recipe_compatibility().expect("compatibility/recipes.yaml must parse");
    let kyverno = matrix.entry("kyverno").expect("a kyverno entry must exist");

    assert_eq!(kyverno.version, "3.9.0");
    assert_eq!(kyverno.kubernetes.certified, vec!["1.35.8".to_string()]);
    assert!(
        !kyverno.kubernetes.certified.contains(&"1.36.4".to_string()),
        "Kyverno's own documented support window (v1.33-v1.35) does not reach 1.36; \
         certifying it against Admission Lab's Tier-1 primary would assert a claim Kyverno's \
         own docs do not make"
    );

    let range =
        kyverno.kubernetes.documented_range.as_ref().expect(
            "kyverno must record the vendor-documented window that justifies the narrowing",
        );
    assert_eq!((range.min.as_str(), range.max.as_str()), ("1.33", "1.35"));
}

#[test]
fn istio_certified_kubernetes_matches_the_full_supported_matrix() {
    let matrix = load_recipe_compatibility().expect("compatibility/recipes.yaml must parse");
    let istio = matrix.entry("istio").expect("an istio entry must exist");

    assert_eq!(istio.version, "1.30.4");
    assert!(
        istio.kubernetes.documented_range.is_none(),
        "Istio's charts declare no kubeVersion constraint and no vendor-stated window was \
         found in research; this must stay unset rather than fabricated"
    );
    assert_eq!(
        istio.kubernetes.certified,
        vec![
            "1.35.8".to_string(),
            "1.36.4".to_string(),
            "1.37.0".to_string()
        ],
        "with no documented constraint narrowing it, Istio's certified set should match \
         Admission Lab's full supported matrix -- unlike Kyverno's"
    );
}

/// Small local extension used only by
/// `other_classification_shaped_keys_beyond_fail_on_and_severity_are_rejected`
/// to attach a per-iteration panic message to `Result::unwrap_err`
/// without repeating `format!` at every call site.
trait UnwrapErrWithContext {
    fn unwrap_err_with_context(self, context: &str) -> String;
}

impl<T: std::fmt::Debug> UnwrapErrWithContext for Result<T, RecipeError> {
    fn unwrap_err_with_context(self, context: &str) -> String {
        match self {
            Ok(value) => panic!("{context}: unexpectedly succeeded with {value:?}"),
            Err(err) => err.to_string(),
        }
    }
}
