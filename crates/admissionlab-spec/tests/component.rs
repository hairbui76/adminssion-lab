//! Tests for the resolved component/install/readiness model
//! (`admissionlab_spec::component`) and its conversion from
//! `admissionlab_spec::ComponentSpec` (via `admissionlab_spec::resolve_lab`).
//!
//! Four properties are load-bearing here and are each covered below:
//!
//! - **Exactly one install method.** A component with no `install` block
//!   is rejected at resolve time; a component whose `install` block tries
//!   to mix fields from both the Helm and manifests variants is rejected
//!   at load time (parse time) by the existing tagged-enum
//!   `deny_unknown_fields` shape.
//! - **An explicit, pinned Helm chart version.** An omitted, empty, or
//!   floating (range/`latest`/wildcard/partial) version is rejected;
//!   every exact-pin form real Helm charts actually use is accepted.
//! - **Install-block path resolution uses the configuration file's
//!   directory, never the current working directory** — the same
//!   property `tests/load.rs`'s own
//!   `relative_paths_resolve_against_config_directory_not_cwd` pins for
//!   `expectationsFile`, but here for `install.helm.valuesFiles` and
//!   `install.manifests.paths`.
//! - **Conversion produces the full resolved shape**: defaulted
//!   `repoName`/`releaseName`/`namespace`, an explicit top-level
//!   `version` overriding the install method's own version, and a
//!   required top-level `version` when the install method (manifests)
//!   provides none.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use admissionlab_spec::{
    InstallMethod, ManifestInstallSpec, ReadinessCheck, load_lab, resolve_lab,
};

// ---------------------------------------------------------------------
// Test support
//
// Each integration test file in this crate's `tests/` directory is
// compiled as its own separate binary, so nothing here is shared with
// `tests/load.rs`'s own (near-identical) helpers.
// ---------------------------------------------------------------------

/// A temporary directory that removes itself when dropped.
///
/// A test holds one for as long as it uses paths underneath it. `Drop`
/// runs on a panicking assertion too, which an explicit delete at the
/// end of a test does not — that is what keeps a `cargo test` run from
/// leaving a directory per test behind in the system temp directory.
struct TempDir(PathBuf);

impl TempDir {
    /// The directory's path, valid for as long as this guard lives.
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fresh, guaranteed-unique directory under the system temp directory.
fn unique_temp_dir(label: &str) -> TempDir {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-spec-component-test-{}-{label}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create unique temp dir");
    TempDir(dir)
}

/// Writes `yaml` (see [`dedent`]) to `admissionlab.yaml` inside a fresh
/// unique temp directory and returns that directory's guard alongside
/// the config's path. The caller must keep the guard alive for as long
/// as it uses the path.
fn write_config(label: &str, yaml: &str) -> (TempDir, PathBuf) {
    let dir = unique_temp_dir(label);
    let path = dir.path().join("admissionlab.yaml");
    std::fs::write(&path, dedent(yaml)).expect("write temp config");
    (dir, path)
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

/// Serializes changes to the process's current directory so the two
/// CWD-manipulating tests below cannot race each other; restores the
/// original directory on drop (including on panic/failed assertion).
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
// Exactly one install method
// ---------------------------------------------------------------------

#[test]
fn component_without_install_block_is_rejected() {
    let (_temp_dir, path) = write_config(
        "no-install",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
          components:
            - name: cert-manager
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).expect("a component without install must still parse");
    let err = resolve_lab(loaded).expect_err("a component with no install method must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("baseline.components[0].install"),
        "error must locate the missing install method; got {message:?}"
    );
}

#[test]
fn install_block_mixing_helm_and_manifest_fields_is_rejected_at_load_time() {
    let (_temp_dir, path) = write_config(
        "mixed-install-fields",
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
                paths:
                  - manifests/webhook.yaml
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    // The internally tagged, `deny_unknown_fields` `InstallMethodSpec`
    // shape already makes this a parse-time failure: once `type: helm`
    // selects the Helm variant, `paths` (a manifests-only field) is an
    // unknown field for it. This is "must fail loudly at load time" for
    // the "declaring both Helm and manifests" failure mode.
    let err = load_lab(&path).expect_err(
        "an install block mixing a manifests-only field into a Helm block must be rejected",
    );
    let message = err.to_string();
    assert!(
        message.contains("paths"),
        "error must name the field that does not belong to the selected variant; got {message:?}"
    );
}

// ---------------------------------------------------------------------
// Explicit, pinned Helm chart version (and a required repository)
// ---------------------------------------------------------------------

#[test]
fn helm_install_without_version_is_rejected() {
    let (_temp_dir, path) = write_config(
        "helm-no-version",
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
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let err = resolve_lab(loaded).expect_err("a Helm install with no version must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("baseline.components[0].install.version"),
        "error must locate the missing version; got {message:?}"
    );
}

#[test]
fn helm_install_without_repo_is_rejected() {
    let (_temp_dir, path) = write_config(
        "helm-no-repo",
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
                version: "1.14.4"
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let err = resolve_lab(loaded).expect_err("a Helm install with no repo must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("baseline.components[0].install.repo"),
        "error must locate the missing repo; got {message:?}"
    );
}

#[test]
fn helm_install_with_empty_chart_is_rejected() {
    let (_temp_dir, path) = write_config(
        "helm-empty-chart",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
          components:
            - name: cert-manager
              install:
                type: helm
                chart: ""
                repo: https://charts.jetstack.io
                version: "1.14.4"
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let err = resolve_lab(loaded).expect_err("an empty chart must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("baseline.components[0].install.chart"),
        "error must locate the empty chart; got {message:?}"
    );
}

#[test]
fn helm_floating_versions_are_rejected() {
    // Every one of these fails the "exact major.minor.patch pin" grammar
    // `require_pinned_helm_version` enforces, each for a documented
    // reason: an empty/omitted version, the literal word `latest`
    // (case-insensitively, simply because it is not numeric), an
    // unbounded range using an operator prefix, a bare major or
    // major.minor (which Helm's own constraint parser — Masterminds
    // semver — expands into a range exactly like an explicit operator
    // would), a wildcard segment, a whitespace- or comma-separated
    // constraint set, a version with the wrong number of segments, and a
    // dangling empty prerelease/build suffix.
    let floating = [
        "",
        "latest",
        "LATEST",
        "^3.9",
        ">=3.9",
        ">=3.9.0",
        "~1.2.3",
        "3",
        "3.9",
        "1.2.x",
        "1.2.*",
        ">=1.0.0 <2.0.0",
        "1.0.0,2.0.0",
        "1.2.3.4",
        "1.2.3-",
        "1.2.3+",
    ];

    for version in floating {
        let (_temp_dir, path) = write_config(
            "helm-floating",
            &format!(
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
                        version: {version:?}
                candidate:
                  kubernetes: "1.29.4"
                fixtures:
                  include: ["fixtures/**/*.yaml"]
                "#
            ),
        );

        let loaded =
            load_lab(&path).unwrap_or_else(|e| panic!("{version:?} must still parse: {e}"));
        let err = resolve_lab(loaded)
            .expect_err(&format!("floating version {version:?} must be rejected"));
        let message = err.to_string();
        assert!(
            message.contains("baseline.components[0].install.version"),
            "error for {version:?} must locate the version field; got {message:?}"
        );
    }
}

#[test]
fn helm_pinned_versions_are_accepted() {
    let pinned = [
        "1.14.4",
        "v1.14.4",
        "V1.14.4",
        "1.2.3-rc.1",
        "2.0.0+build.5",
        "1.2.3-rc.1+build.5",
        "0.0.1",
    ];

    for version in pinned {
        let (_temp_dir, path) = write_config(
            "helm-pinned",
            &format!(
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
                        version: {version:?}
                candidate:
                  kubernetes: "1.29.4"
                fixtures:
                  include: ["fixtures/**/*.yaml"]
                "#
            ),
        );

        let loaded = load_lab(&path).unwrap();
        let resolved = resolve_lab(loaded)
            .unwrap_or_else(|e| panic!("pinned version {version:?} must be accepted: {e}"));
        match &resolved.baseline.components[0].install {
            InstallMethod::Helm(helm) => assert_eq!(helm.version, version),
            InstallMethod::Manifests(_) => panic!("expected a Helm install"),
        }
    }
}

// ---------------------------------------------------------------------
// Conversion produces the full resolved shape
// ---------------------------------------------------------------------

#[test]
fn helm_component_resolves_to_expected_shape_with_defaults() {
    let (_temp_dir, path) = write_config(
        "helm-full-shape",
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
    let config_dir = path.parent().unwrap().to_path_buf();

    let loaded = load_lab(&path).unwrap();
    let resolved = resolve_lab(loaded).expect("a fully specified Helm component must resolve");
    let component = &resolved.baseline.components[0];

    assert_eq!(component.name, "cert-manager");
    // No top-level `version` was set, so it must fall back to the
    // install method's own pinned version.
    assert_eq!(component.version, "v1.14.4");
    // This component declares no `readiness`, and an absent section
    // resolves to no checks rather than to a defaulted one — see
    // `readiness_checks_resolve_from_yaml` for the populated case.
    assert!(component.readiness.is_empty());
    assert!(component.recipe_normalize_rules.is_empty());
    assert!(component.capabilities.is_empty());

    match &component.install {
        InstallMethod::Helm(helm) => {
            assert_eq!(helm.chart, "cert-manager");
            assert_eq!(helm.repo_url, "https://charts.jetstack.io");
            assert_eq!(helm.version, "v1.14.4");
            // `repoName`/`releaseName`/`namespace` were not set in this
            // config, so all three default to the component's own
            // resolved name — see `component.rs`'s `resolve_helm`.
            assert_eq!(helm.repo_name, "cert-manager");
            assert_eq!(helm.release_name, "cert-manager");
            assert_eq!(helm.namespace, "cert-manager");
            assert!(helm.set_values.is_empty());
            assert_eq!(
                helm.values_files,
                vec![config_dir.join("values/cert-manager.yaml")]
            );
        }
        InstallMethod::Manifests(_) => panic!("expected a Helm install"),
    }
}

#[test]
fn helm_install_overrides_repo_name_release_name_namespace_and_set_values() {
    let (_temp_dir, path) = write_config(
        "helm-explicit-overrides",
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
                repoName: jetstack
                version: "1.14.4"
                releaseName: cert-manager-release
                namespace: cert-manager-ns
                setValues:
                  installCRDs: "true"
                  global.leaderElection.namespace: cert-manager-ns
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let resolved = resolve_lab(loaded).expect("explicit overrides must resolve");
    let component = &resolved.baseline.components[0];

    match &component.install {
        InstallMethod::Helm(helm) => {
            assert_eq!(helm.repo_name, "jetstack");
            assert_eq!(helm.release_name, "cert-manager-release");
            assert_eq!(helm.namespace, "cert-manager-ns");
            assert_eq!(
                helm.set_values.get("installCRDs").map(String::as_str),
                Some("true")
            );
            assert_eq!(
                helm.set_values
                    .get("global.leaderElection.namespace")
                    .map(String::as_str),
                Some("cert-manager-ns")
            );
        }
        InstallMethod::Manifests(_) => panic!("expected a Helm install"),
    }
}

#[test]
fn helm_install_namespace_override_resolves_correctly_for_istiod() {
    // The motivating case: a component named `istiod` (matching the real
    // `istio/istiod` chart's own name) must not silently default its
    // namespace to `istiod` — the chart's real convention is
    // `istio-system`, and getting this wrong would make the install
    // *appear* to succeed while placing the control plane somewhere
    // nothing expects it.
    let (_temp_dir, path) = write_config(
        "helm-istiod-namespace",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
          components:
            - name: istiod
              install:
                type: helm
                chart: istiod
                repo: https://istio-release.storage.googleapis.com/charts
                version: "1.21.0"
                namespace: istio-system
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let resolved = resolve_lab(loaded).expect("an explicit namespace override must resolve");
    let component = &resolved.baseline.components[0];

    match &component.install {
        InstallMethod::Helm(helm) => {
            assert_eq!(
                helm.namespace, "istio-system",
                "an explicit namespace override must win over the component-name default"
            );
        }
        InstallMethod::Manifests(_) => panic!("expected a Helm install"),
    }
}

#[test]
fn top_level_version_overrides_helm_install_version() {
    let (_temp_dir, path) = write_config(
        "helm-version-override",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
          components:
            - name: cert-manager
              version: "1.14.4-custom"
              install:
                type: helm
                chart: cert-manager
                repo: https://charts.jetstack.io
                version: v1.14.4
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let resolved = resolve_lab(loaded).unwrap();
    let component = &resolved.baseline.components[0];

    assert_eq!(component.version, "1.14.4-custom");
    match &component.install {
        InstallMethod::Helm(helm) => assert_eq!(helm.version, "v1.14.4"),
        InstallMethod::Manifests(_) => panic!("expected a Helm install"),
    }
}

#[test]
fn manifests_install_requires_explicit_component_version() {
    let (_temp_dir, path) = write_config(
        "manifests-no-version",
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
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let err = resolve_lab(loaded)
        .expect_err("a manifests install with no explicit component version must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("baseline.components[0].version"),
        "error must locate the missing version; got {message:?}"
    );
}

#[test]
fn manifests_component_resolves_to_expected_shape() {
    let (_temp_dir, path) = write_config(
        "manifests-full-shape",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
          components:
            - name: raw-webhook
              version: "1.0.0"
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
    let config_dir = path.parent().unwrap().to_path_buf();

    let loaded = load_lab(&path).unwrap();
    let resolved = resolve_lab(loaded).expect("a fully specified manifests component must resolve");
    let component = &resolved.baseline.components[0];

    assert_eq!(component.name, "raw-webhook");
    assert_eq!(component.version, "1.0.0");
    assert_eq!(
        component.install,
        InstallMethod::Manifests(ManifestInstallSpec {
            paths: vec![
                config_dir.join("manifests/webhook.yaml"),
                config_dir.join("manifests/rbac.yaml"),
            ],
        })
    );
}

#[test]
fn manifests_paths_must_not_be_empty() {
    let (_temp_dir, path) = write_config(
        "manifests-empty-paths",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.29.4"
          components:
            - name: raw-webhook
              version: "1.0.0"
              install:
                type: manifests
                paths: []
        candidate:
          kubernetes: "1.29.4"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let err = resolve_lab(loaded).expect_err(
        "an empty manifests path list would install nothing and report success -- it must be \
         rejected, not resolved",
    );
    let message = err.to_string();
    assert!(
        message.contains("baseline.components[0].install.paths"),
        "error must locate the offending field; got {message:?}"
    );
}

// ---------------------------------------------------------------------
// Install-block path resolution: config directory, never the current
// working directory
// ---------------------------------------------------------------------

#[test]
fn helm_values_files_resolve_against_config_directory_not_cwd() {
    let root = unique_temp_dir("helm-values-cwd");
    let lab_dir = root.path().join("lab");
    let elsewhere_dir = root.path().join("elsewhere");
    std::fs::create_dir_all(&lab_dir).unwrap();
    std::fs::create_dir_all(&elsewhere_dir).unwrap();

    std::fs::write(lab_dir.join("values.yaml"), "correct\n").unwrap();
    // Decoy at the same relative path, anchored at the CWD instead: a
    // buggy implementation that resolved `valuesFiles` against the
    // current working directory (rather than the configuration file's
    // own directory) would silently land here instead of erroring.
    std::fs::write(elsewhere_dir.join("values.yaml"), "decoy\n").unwrap();
    std::fs::write(
        lab_dir.join("admissionlab.yaml"),
        dedent(
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
                    version: "1.14.4"
                    valuesFiles:
                      - values.yaml
            candidate:
              kubernetes: "1.29.4"
            fixtures:
              include: ["fixtures/**/*.yaml"]
            "#,
        ),
    )
    .unwrap();

    // Canonicalizing a still-relative resolved path requires the CWD it
    // is relative to, so this must happen before the guard restores the
    // original directory.
    let resolved_canonical = {
        let _guard = CwdGuard::change_to(&elsewhere_dir);

        // Exactly the realistic scenario: `admissionlab test
        // ../lab/admissionlab.yaml` run from an unrelated directory.
        let loaded = load_lab(Path::new("../lab/admissionlab.yaml"))
            .expect("relative config path must resolve against the current directory to open");
        let resolved = resolve_lab(loaded).expect("config must resolve");
        let resolved_values_file = match &resolved.baseline.components[0].install {
            InstallMethod::Helm(helm) => helm.values_files[0].clone(),
            InstallMethod::Manifests(_) => panic!("expected a Helm install"),
        };
        std::fs::canonicalize(&resolved_values_file).unwrap_or_else(|e| {
            panic!("resolved values file {resolved_values_file:?} must exist: {e}")
        })
    };

    let correct_canonical = std::fs::canonicalize(lab_dir.join("values.yaml")).unwrap();
    let decoy_canonical = std::fs::canonicalize(elsewhere_dir.join("values.yaml")).unwrap();

    assert_eq!(
        resolved_canonical, correct_canonical,
        "install.helm.valuesFiles must resolve against the config file's own directory"
    );
    assert_ne!(
        resolved_canonical, decoy_canonical,
        "install.helm.valuesFiles must not resolve against the current working directory"
    );
}

#[test]
fn manifests_paths_resolve_against_config_directory_not_cwd() {
    let root = unique_temp_dir("manifests-paths-cwd");
    let lab_dir = root.path().join("lab");
    let elsewhere_dir = root.path().join("elsewhere");
    std::fs::create_dir_all(&lab_dir).unwrap();
    std::fs::create_dir_all(&elsewhere_dir).unwrap();

    std::fs::write(lab_dir.join("webhook.yaml"), "correct\n").unwrap();
    std::fs::write(elsewhere_dir.join("webhook.yaml"), "decoy\n").unwrap();
    std::fs::write(
        lab_dir.join("admissionlab.yaml"),
        dedent(
            r#"
            apiVersion: admissionlab.io/v1alpha1
            kind: Lab
            baseline:
              kubernetes: "1.29.4"
              components:
                - name: raw-webhook
                  version: "1.0.0"
                  install:
                    type: manifests
                    paths:
                      - webhook.yaml
            candidate:
              kubernetes: "1.29.4"
            fixtures:
              include: ["fixtures/**/*.yaml"]
            "#,
        ),
    )
    .unwrap();

    let resolved_canonical = {
        let _guard = CwdGuard::change_to(&elsewhere_dir);

        let loaded = load_lab(Path::new("../lab/admissionlab.yaml"))
            .expect("relative config path must resolve against the current directory to open");
        let resolved = resolve_lab(loaded).expect("config must resolve");
        let resolved_path = match &resolved.baseline.components[0].install {
            InstallMethod::Manifests(manifests) => manifests.paths[0].clone(),
            InstallMethod::Helm(_) => panic!("expected a Manifests install"),
        };
        std::fs::canonicalize(&resolved_path)
            .unwrap_or_else(|e| panic!("resolved manifest path {resolved_path:?} must exist: {e}"))
    };

    let correct_canonical = std::fs::canonicalize(lab_dir.join("webhook.yaml")).unwrap();
    let decoy_canonical = std::fs::canonicalize(elsewhere_dir.join("webhook.yaml")).unwrap();

    assert_eq!(
        resolved_canonical, correct_canonical,
        "install.manifests.paths must resolve against the config file's own directory"
    );
    assert_ne!(
        resolved_canonical, decoy_canonical,
        "install.manifests.paths must not resolve against the current working directory"
    );
}

// ---------------------------------------------------------------------
// `readiness`
// ---------------------------------------------------------------------

#[test]
fn readiness_checks_resolve_from_yaml() {
    // All five variants at once, written exactly as
    // `recipes/*/recipe.yaml` writes them -- the whole point of the
    // shared spelling is that a certified recipe's readiness section can
    // be transcribed into a lab file without translation.
    let (_temp_dir, path) = write_config(
        "readiness-all-variants",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.35.8"
          components:
            - name: kyverno
              install:
                type: helm
                chart: kyverno/kyverno
                repo: https://kyverno.github.io/kyverno/
                version: "3.9.0"
                namespace: kyverno
              readiness:
                - type: deploymentAvailable
                  namespace: kyverno
                  name: kyverno-admission-controller
                - type: daemonSetReady
                  namespace: kube-system
                  name: some-agent
                - type: jobComplete
                  namespace: kyverno
                  name: some-migration
                - type: webhookConfigurationPresent
                  name: kyverno-resource-mutating-webhook-cfg
                - type: customResourceCondition
                  apiVersion: kyverno.io/v1
                  kind: ClusterPolicy
                  name: alpha-corpus
                  conditionType: Ready
                  status: "True"
        candidate:
          kubernetes: "1.35.8"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let loaded = load_lab(&path).unwrap();
    let resolved = resolve_lab(loaded).expect("a component with readiness checks must resolve");
    let component = &resolved.baseline.components[0];

    assert_eq!(
        component.readiness,
        vec![
            ReadinessCheck::DeploymentAvailable {
                namespace: "kyverno".to_owned(),
                name: "kyverno-admission-controller".to_owned(),
            },
            ReadinessCheck::DaemonSetReady {
                namespace: "kube-system".to_owned(),
                name: "some-agent".to_owned(),
            },
            ReadinessCheck::JobComplete {
                namespace: "kyverno".to_owned(),
                name: "some-migration".to_owned(),
            },
            ReadinessCheck::WebhookConfigurationPresent {
                name: "kyverno-resource-mutating-webhook-cfg".to_owned(),
            },
            ReadinessCheck::CustomResourceCondition {
                api_version: "kyverno.io/v1".to_owned(),
                kind: "ClusterPolicy".to_owned(),
                // Cluster-scoped: `namespace` was omitted, and an
                // omitted namespace must stay `None` rather than
                // becoming an empty string that would then be sent to
                // the API server as a real (nonexistent) namespace.
                namespace: None,
                name: "alpha-corpus".to_owned(),
                condition_type: "Ready".to_owned(),
                status: "True".to_owned(),
            },
        ],
        "every readiness variant must survive resolution in written order"
    );
}

#[test]
fn an_unknown_readiness_check_type_is_rejected() {
    // The closed variant set is the point: a misspelled or invented
    // check must fail at load time, not silently resolve to no wait at
    // all -- which would be indistinguishable from a component that
    // legitimately has nothing to wait on.
    let (_temp_dir, path) = write_config(
        "readiness-unknown-type",
        r#"
        apiVersion: admissionlab.io/v1alpha1
        kind: Lab
        baseline:
          kubernetes: "1.35.8"
          components:
            - name: kyverno
              version: "3.9.0"
              install:
                type: manifests
                paths: ["policies.yaml"]
              readiness:
                - type: podRunning
                  namespace: kyverno
                  name: whatever
        candidate:
          kubernetes: "1.35.8"
        fixtures:
          include: ["fixtures/**/*.yaml"]
        "#,
    );

    let error = load_lab(&path).expect_err("an unknown readiness check type must be rejected");
    let rendered = error.to_string();
    assert!(
        rendered.contains("podRunning"),
        "the error must name the offending value: {rendered}"
    );
}
