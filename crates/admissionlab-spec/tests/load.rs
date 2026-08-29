//! Behavioral tests for the `v1alpha1` configuration loader
//! ([`admissionlab_spec::load_lab`]) and resolver
//! ([`admissionlab_spec::resolve_lab`]).
//!
//! Three properties are load-bearing here and are each covered by tests
//! below:
//!
//! - **Strict parsing.** An unknown/misspelled key is a loud, named
//!   error — never silently ignored.
//! - **Path resolution uses the configuration file's directory, never the
//!   current working directory.** See
//!   `relative_paths_resolve_against_config_directory_not_cwd`, which
//!   changes the process's current directory and would fail if the
//!   implementation consulted it.
//! - **Semantic validation** rejects an empty Kubernetes version, a
//!   duplicate component name within one environment, a component
//!   missing a name, and an empty fixture include list.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use admissionlab_spec::{InstallMethodSpec, load_lab, resolve_lab};

// ---------------------------------------------------------------------
// Test support
// ---------------------------------------------------------------------

/// Path to one of the three checked-in fixtures under
/// `testdata/configs/`, which lives at the workspace root (two levels
/// above this crate's own `CARGO_MANIFEST_DIR`).
fn testdata_config(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/configs")
        .join(name)
}

/// A fresh, guaranteed-unique directory under the system temp directory,
/// for tests that need real files on disk (`load_lab` reads from a real
/// path; it has no in-memory-string entry point).
fn unique_temp_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-spec-load-test-{}-{label}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create unique temp dir");
    dir
}

/// Writes `yaml` (see [`dedent`]) to `admissionlab.yaml` inside a fresh
/// unique temp directory and returns its path.
fn write_config(label: &str, yaml: &str) -> PathBuf {
    let dir = unique_temp_dir(label);
    let path = dir.join("admissionlab.yaml");
    std::fs::write(&path, dedent(yaml)).expect("write temp config");
    path
}

/// Strips the leading whitespace common to every non-empty line, so a
/// YAML literal can be indented to match the surrounding Rust code
/// without that indentation becoming part of the (indentation-sensitive)
/// YAML content.
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

/// Serializes changes to the process's current directory so
/// CWD-manipulating tests in this file cannot race each other; restores
/// the original directory on drop (including on panic/failed assertion).
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

// ---------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------

#[test]
fn minimal_valid_config_loads_and_resolves() {
    let path = testdata_config("minimal-valid.yaml");

    let loaded = load_lab(&path).expect("minimal-valid.yaml must load");
    assert_eq!(loaded.source_path, path);
    assert_eq!(loaded.raw.api_version, "admissionlab.io/v1alpha1");
    assert_eq!(loaded.raw.kind, "Lab");
    assert_eq!(loaded.raw.baseline.kubernetes, "1.29.4");
    assert_eq!(loaded.raw.candidate.kubernetes, "1.29.4");
    assert!(loaded.raw.baseline.components.is_empty());
    assert_eq!(loaded.raw.expectations_file, None);

    let resolved = resolve_lab(loaded).expect("minimal-valid.yaml must resolve");
    assert_eq!(resolved.baseline.kubernetes, "1.29.4");
    assert_eq!(resolved.candidate.kubernetes, "1.29.4");
    assert!(resolved.baseline.components.is_empty());
    assert!(resolved.candidate.components.is_empty());
    assert_eq!(resolved.fixtures.include.len(), 1);
    assert_eq!(
        resolved.fixtures.root,
        path.parent().expect("config has a parent directory")
    );
    assert_eq!(resolved.expectations_file, None);
    assert_eq!(resolved.gateway, None);
    assert_eq!(resolved.migration, None);
}

#[test]
fn policy_defaults_when_omitted() {
    let loaded = load_lab(&testdata_config("minimal-valid.yaml")).unwrap();
    assert!(loaded.raw.policy.fail_on.is_empty());
    assert!(loaded.raw.policy.overrides.is_empty());
    assert_eq!(
        loaded.raw.policy.latency.absolute_increase,
        std::time::Duration::ZERO
    );
    assert!((loaded.raw.policy.latency.relative_multiplier - 1.0).abs() < f64::EPSILON);
}

// ---------------------------------------------------------------------
// Strict parsing
// ---------------------------------------------------------------------

#[test]
fn unknown_field_is_rejected_and_names_the_field() {
    let err = load_lab(&testdata_config("unknown-field.yaml"))
        .expect_err("a misspelled `candidate` key must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("candiate"),
        "error must name the offending field; got {message:?}"
    );
}

#[test]
fn missing_candidate_is_rejected() {
    let err = load_lab(&testdata_config("missing-candidate.yaml"))
        .expect_err("an entirely absent `candidate` section must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("candidate"),
        "error must mention the missing field; got {message:?}"
    );
}

#[test]
fn wrong_api_version_is_rejected() {
    let path = write_config(
        "wrong-api-version",
        r#"
        apiVersion: admissionlab.io/v1beta1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let err = load_lab(&path).expect_err("a wrong apiVersion must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("apiVersion"),
        "error must mention apiVersion; got {message:?}"
    );
}

#[test]
fn wrong_kind_is_rejected() {
    let path = write_config(
        "wrong-kind",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: LabSuite
        baseline:
          kubernetes: "1.29.4"
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let err = load_lab(&path).expect_err("a wrong kind must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("kind"),
        "error must mention kind; got {message:?}"
    );
}

// ---------------------------------------------------------------------
// Path resolution: config directory, never the current working directory
// ---------------------------------------------------------------------

#[test]
fn relative_paths_resolve_against_config_directory_not_cwd() {
    let root = unique_temp_dir("cwd-resolution");
    let lab_dir = root.join("lab");
    let elsewhere_dir = root.join("elsewhere");
    std::fs::create_dir_all(&lab_dir).unwrap();
    std::fs::create_dir_all(&elsewhere_dir).unwrap();

    // The correct target: an expectations file next to the config.
    std::fs::write(lab_dir.join("expectations.yaml"), "correct\n").unwrap();
    // A decoy at the *same relative path* but anchored at the CWD
    // instead: a buggy implementation that resolved against the current
    // working directory would silently land here instead of erroring.
    std::fs::write(elsewhere_dir.join("expectations.yaml"), "decoy\n").unwrap();
    std::fs::write(
        lab_dir.join("admissionlab.yaml"),
        dedent(
            r#"
            apiVersion: admissionlab.io/v1alpha1
            kind: Lab
            baseline:
              kubernetes: "1.29.4"
            candidate:
              kubernetes: "1.29.4"
            fixtures:
              include: ["fixtures/**/*.yaml"]
            expectationsFile: expectations.yaml
            "#,
        ),
    )
    .unwrap();

    // Canonicalizing a still-relative resolved path requires the CWD it
    // is relative to, so every canonicalize() call that touches a
    // resolved (possibly relative) path must happen before the guard
    // restores the original directory.
    let (expectations_canonical, fixtures_root_canonical) = {
        let _guard = CwdGuard::change_to(&elsewhere_dir);

        // Exactly the realistic scenario: `admissionlab test
        // ../lab/admissionlab.yaml` run from an unrelated directory.
        let loaded = load_lab(Path::new("../lab/admissionlab.yaml"))
            .expect("relative config path must resolve against the current directory to open");
        let resolved = resolve_lab(loaded).expect("config must resolve");

        let expectations_file = resolved
            .expectations_file
            .expect("expectationsFile must resolve to Some");
        let expectations_canonical =
            std::fs::canonicalize(&expectations_file).unwrap_or_else(|e| {
                panic!("resolved expectations path {expectations_file:?} must exist: {e}")
            });
        let fixtures_root_canonical = std::fs::canonicalize(&resolved.fixtures.root)
            .unwrap_or_else(|e| {
                panic!(
                    "resolved fixtures root {:?} must exist: {e}",
                    resolved.fixtures.root
                )
            });

        (expectations_canonical, fixtures_root_canonical)
    };

    let correct_canonical = std::fs::canonicalize(lab_dir.join("expectations.yaml")).unwrap();
    let decoy_canonical = std::fs::canonicalize(elsewhere_dir.join("expectations.yaml")).unwrap();
    let expected_root_canonical = std::fs::canonicalize(&lab_dir).unwrap();

    assert_eq!(
        expectations_canonical, correct_canonical,
        "expectationsFile must resolve against the config file's own directory"
    );
    assert_ne!(
        expectations_canonical, decoy_canonical,
        "expectationsFile must not resolve against the current working directory"
    );
    assert_eq!(
        fixtures_root_canonical, expected_root_canonical,
        "fixtures root must be the config file's own directory, not the cwd"
    );
}

// ---------------------------------------------------------------------
// Semantic validation
// ---------------------------------------------------------------------

#[test]
fn empty_baseline_kubernetes_version_is_rejected() {
    let path = write_config(
        "empty-baseline-k8s",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: ""
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).expect("parses fine; emptiness is a semantic rule");
    let err = resolve_lab(loaded).expect_err("empty baseline kubernetes version must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("baseline.kubernetes"),
        "error must locate the offending field; got {message:?}"
    );
}

#[test]
fn empty_candidate_kubernetes_version_is_rejected() {
    let path = write_config(
        "empty-candidate-k8s",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
        candidate:
          kubernetes: "   "
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let err = resolve_lab(loaded)
        .expect_err("whitespace-only candidate kubernetes version must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("candidate.kubernetes"),
        "error must locate the offending field; got {message:?}"
    );
}

#[test]
fn duplicate_component_names_within_an_environment_are_rejected() {
    let path = write_config(
        "duplicate-component-names",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
          components:
            - name: cert-manager
            - name: cert-manager
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let err = resolve_lab(loaded).expect_err("duplicate component names must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("baseline.components"),
        "error must locate the offending list; got {message:?}"
    );
    assert!(
        message.contains("cert-manager"),
        "error must name the duplicated component; got {message:?}"
    );
}

#[test]
fn same_component_name_in_baseline_and_candidate_is_allowed() {
    // This is the tool's normal use case: baseline and candidate install
    // the *same*-named component (compared to each other), so duplicate
    // detection must be scoped per-environment, not across both.
    let path = write_config(
        "same-name-both-sides",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
          components:
            - name: cert-manager
        candidate:
          kubernetes: "1.29.4"
          components:
            - name: cert-manager
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let resolved = resolve_lab(loaded)
        .expect("the same component name in baseline and candidate must be allowed");
    assert_eq!(resolved.baseline.components[0].name, "cert-manager");
    assert_eq!(resolved.candidate.components[0].name, "cert-manager");
}

#[test]
fn component_missing_name_is_rejected() {
    let path = write_config(
        "component-missing-name",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
          components:
            - version: "1.0.0"
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let err = resolve_lab(loaded).expect_err("a component without a name must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("baseline.components[0]"),
        "error must locate the offending component; got {message:?}"
    );
}

#[test]
fn empty_fixture_include_list_is_rejected() {
    let path = write_config(
        "empty-fixture-include",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: []
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let err = resolve_lab(loaded).expect_err("an empty fixture include list must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("fixtures.include"),
        "error must locate the offending field; got {message:?}"
    );
}

#[test]
fn invalid_fixture_glob_pattern_is_rejected() {
    let path = write_config(
        "invalid-glob",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/[unterminated"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let err = resolve_lab(loaded).expect_err("an unterminated character class must be rejected");
    assert!(matches!(
        err,
        admissionlab_spec::SpecError::InvalidGlob { .. }
    ));
}

// ---------------------------------------------------------------------
// InstallMethodSpec: Helm and raw manifests
// ---------------------------------------------------------------------

#[test]
fn helm_install_method_parses() {
    let path = write_config(
        "helm-install",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
          components:
            - name: cert-manager
              install:
                type: helm
                chart: cert-manager
                repo: https://charts.jetstack.io
                version: v1.14.4
                valuesFiles:
                  - values/cert-manager.yaml
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).expect("a helm install block must parse");
    let install = loaded.raw.baseline.components[0]
        .install
        .as_ref()
        .expect("install must be present");
    match install {
        InstallMethodSpec::Helm(helm) => {
            assert_eq!(helm.chart, "cert-manager");
            assert_eq!(helm.repo.as_deref(), Some("https://charts.jetstack.io"));
            assert_eq!(helm.version.as_deref(), Some("v1.14.4"));
            assert_eq!(
                helm.values_files,
                vec![PathBuf::from("values/cert-manager.yaml")]
            );
        }
        InstallMethodSpec::Manifests(_) => panic!("expected a Helm install method"),
    }

    // Resolution still succeeds — resolve_lab only needs `name` for its
    // validation, and does not (yet) descend into `install` (see
    // `ComponentSpec::install`'s documentation).
    let resolved = resolve_lab(loaded).unwrap();
    assert_eq!(resolved.baseline.components[0].name, "cert-manager");
}

#[test]
fn manifests_install_method_parses() {
    let path = write_config(
        "manifests-install",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
          components:
            - name: raw-webhook
              install:
                type: manifests
                paths:
                  - manifests/webhook.yaml
                  - manifests/rbac.yaml
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).expect("a manifests install block must parse");
    let install = loaded.raw.baseline.components[0]
        .install
        .as_ref()
        .expect("install must be present");
    match install {
        InstallMethodSpec::Manifests(manifests) => {
            assert_eq!(
                manifests.paths,
                vec![
                    PathBuf::from("manifests/webhook.yaml"),
                    PathBuf::from("manifests/rbac.yaml"),
                ]
            );
        }
        InstallMethodSpec::Helm(_) => panic!("expected a Manifests install method"),
    }
}

#[test]
fn unrecognized_install_method_type_is_rejected() {
    let path = write_config(
        "unknown-install-type",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
          components:
            - name: x
              install:
                type: kustomize
                path: overlays/prod
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let err = load_lab(&path).expect_err("an unrecognized install type must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("kustomize"),
        "error must name the unrecognized type; got {message:?}"
    );
}

// ---------------------------------------------------------------------
// LatencyPolicy: human-friendly YAML representation
// ---------------------------------------------------------------------

#[test]
fn latency_absolute_increase_is_milliseconds() {
    let path = write_config(
        "latency-policy",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        policy:
          failOn: ["breaking-change"]
          latency:
            absoluteIncrease: 250
            relativeMultiplier: 1.5
        "#,
    );

    let loaded = load_lab(&path).expect("policy.latency must parse");
    assert_eq!(
        loaded.raw.policy.latency.absolute_increase,
        std::time::Duration::from_millis(250)
    );
    assert!((loaded.raw.policy.latency.relative_multiplier - 1.5).abs() < f64::EPSILON);
    assert!(loaded.raw.policy.fail_on.contains("breaking-change"));
}
