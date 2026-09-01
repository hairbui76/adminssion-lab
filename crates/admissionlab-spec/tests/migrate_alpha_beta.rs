//! ROADMAP Task 7.1: promoting the lab configuration to
//! `admissionlab.io/v1beta1` without breaking a single Public Alpha file.
//!
//! Four properties are load-bearing here, and each has tests below:
//!
//! - **Alpha configurations still work, identically.** Every checked-in
//!   `v1alpha1` document resolves through
//!   [`admissionlab_spec::load_any_supported_lab`] to exactly what the
//!   original `load_lab` + `resolve_lab` pair produced. Not "equivalently":
//!   equal, as values. Those documents live in `testdata/configs/` — see
//!   [`valid_alpha_documents`] for why they are the proof and `examples/`
//!   no longer is.
//! - **The two versions describe one lab.** A hand-written `v1beta1`
//!   twin of an Alpha file resolves to the same [`ResolvedLab`]. This is
//!   the assertion that catches a migration which compiles, parses, and
//!   moves a value to the wrong field.
//! - **The renames are real.** An Alpha key in a Beta document (and vice
//!   versa) is rejected by `deny_unknown_fields` rather than silently
//!   accepted, so "we renamed it" is a fact about the parser rather than
//!   a claim in a changelog.
//! - **Migration is total, and its one precondition is honest.** No
//!   `v1alpha1` document that the loader accepts can fail to migrate;
//!   the only [`MigrationError`] is the caller-precondition described in
//!   `migrate.rs`, and it is reachable only by building a document by
//!   hand.

use std::path::{Path, PathBuf};

use admissionlab_spec::{
    MigrationError, ResolvedLab, SpecError, V1Alpha1Lab, V1Beta1Lab, load_any_supported_lab,
    load_lab, migrate_v1alpha1_to_v1beta1, resolve_lab, v1alpha1, v1beta1,
};

// ---------------------------------------------------------------------
// Test support
// ---------------------------------------------------------------------

/// The workspace root, two levels above this crate's own
/// `CARGO_MANIFEST_DIR`.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Path to one of the checked-in fixtures under `testdata/configs/`.
fn testdata_config(name: &str) -> PathBuf {
    workspace_root().join("testdata/configs").join(name)
}

/// A temporary directory that deletes itself when the test's binding
/// goes out of scope — including on a failed assertion, because [`Drop`]
/// runs while a panic unwinds.
///
/// Deliberately not a "unique directory left behind for inspection"
/// helper: a test that leaks a directory on every run leaks one on every
/// run of CI too.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "admissionlab-spec-migrate-{}-{label}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    /// Writes `yaml` to `admissionlab.yaml` in this directory and returns
    /// its path.
    fn write_config(&self, yaml: &str) -> PathBuf {
        let path = self.path.join("admissionlab.yaml");
        std::fs::write(&path, yaml).expect("write temp config");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Every checked-in `v1alpha1` lab document that is expected to load and
/// resolve cleanly: the living proof that an Alpha file nobody touched
/// still runs.
///
/// **These live in `testdata/configs/`, and `examples/` is deliberately
/// not among them any more.** ROADMAP Task 7.1 Step 2 parked the Alpha
/// read-support proof on `examples/` because that task left them
/// unmigrated; Task 7.7 migrates them, because the examples a user
/// copies from must showcase the *current* version. The proof therefore
/// moved here rather than evaporating, and
/// `renamed-fields-v1alpha1.yaml` is deliberately the maximal one —
/// every optional section populated, both renamed keys in their Alpha
/// spelling, components with recipes, a full gateway suite with probes —
/// so what is being kept alive is a realistic Alpha configuration and
/// not a three-line stub.
fn valid_alpha_documents() -> Vec<PathBuf> {
    vec![
        testdata_config("minimal-valid.yaml"),
        testdata_config("gateway-valid.yaml"),
        testdata_config("renamed-fields-v1alpha1.yaml"),
    ]
}

/// The `examples/` a user copies from — every one of which must be a
/// `v1beta1` document, and must load.
fn example_documents() -> Vec<PathBuf> {
    let root = workspace_root();
    vec![
        root.join("examples/admission-basic/admissionlab.yaml"),
        root.join("examples/gateway-istio/admissionlab.yaml"),
        root.join("examples/kyverno-istio-upgrade/admissionlab.yaml"),
    ]
}

/// A [`ResolvedLab`] with `source_path` replaced, so two labs loaded from
/// *different files in the same directory* can be compared for equality
/// on everything else. Every other path in a resolved lab is joined onto
/// the configuration's own directory, which the twins share.
fn without_source_path(mut lab: ResolvedLab) -> ResolvedLab {
    lab.source_path = PathBuf::new();
    lab
}

// ---------------------------------------------------------------------
// Step 2: Alpha configurations keep working, unchanged
// ---------------------------------------------------------------------

#[test]
fn every_checked_in_alpha_document_resolves_identically_through_the_new_loader() {
    for path in valid_alpha_documents() {
        let before = resolve_lab(
            load_lab(&path)
                .unwrap_or_else(|e| panic!("{} must load as v1alpha1: {e}", path.display())),
        )
        .unwrap_or_else(|e| panic!("{} must resolve: {e}", path.display()));

        let after = load_any_supported_lab(&path).unwrap_or_else(|e| {
            panic!(
                "{} must load through the version loader: {e}",
                path.display()
            )
        });

        assert_eq!(
            after,
            before,
            "{} resolved differently through load_any_supported_lab",
            path.display()
        );
    }
}

#[test]
fn the_alpha_read_support_fixtures_really_are_alpha_documents() {
    // Not a tautology dressed as a test: the assertion above is only
    // evidence for "Alpha files keep working" for as long as those files
    // really are Alpha files. Migrate one of them in passing and this
    // test names it, so the compatibility promise cannot silently stop
    // being tested by a file that quietly stopped being Alpha.
    for path in valid_alpha_documents() {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()));
        assert!(
            text.contains(v1alpha1::API_VERSION),
            "{} is no longer a v1alpha1 document; the Alpha read-support proof needs a \
             replacement fixture, and it needs to be a realistic one",
            path.display()
        );
    }
}

#[test]
fn the_examples_directory_is_written_in_the_current_v1beta1() {
    // The reversed guard (ROADMAP Task 7.7 step 6). `examples/` is what a
    // user copies, so it must showcase the version this release actually
    // documents — an example still written in the previous version is
    // how a deprecated spelling outlives its deprecation. Alpha
    // read-support is proven by `testdata/configs/` instead, which is
    // where a compatibility fixture belongs: nobody copies it into a
    // repository by accident.
    for path in example_documents() {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()));
        assert!(
            text.contains(v1beta1::API_VERSION),
            "{} does not declare {}; every example must showcase the current \
             configuration version",
            path.display(),
            v1beta1::API_VERSION
        );
        assert!(
            !text.contains(v1alpha1::API_VERSION),
            "{} still mentions {}; migrate it, or move the comment that names the old \
             version into docs/schema-migrations.md",
            path.display(),
            v1alpha1::API_VERSION
        );
    }
}

#[test]
fn every_example_loads_and_resolves_through_the_version_loader() {
    // The migration was only safe if the migrated files still work. This
    // is the cheap half of that proof — every example parses, validates
    // and resolves — and `admissionlab-cli`'s `#[ignore]`d `alpha_e2e`
    // is the expensive half, driving `examples/kyverno-istio-upgrade`
    // through two real clusters.
    for path in example_documents() {
        load_any_supported_lab(&path)
            .unwrap_or_else(|e| panic!("{} must load and resolve: {e}", path.display()));
    }
}

#[test]
fn an_invalid_alpha_document_fails_the_same_way_through_both_loaders() {
    for name in ["unknown-field.yaml", "missing-candidate.yaml"] {
        let path = testdata_config(name);
        let before = resolve_lab(match load_lab(&path) {
            Ok(loaded) => loaded,
            Err(error) => {
                let after = load_any_supported_lab(&path).expect_err("must also fail");
                assert_eq!(
                    error.to_string(),
                    after.to_string(),
                    "{name} reported a different error through the version loader"
                );
                continue;
            }
        });
        let after = load_any_supported_lab(&path);
        assert_eq!(
            before.map(|_| ()).map_err(|e| e.to_string()),
            after.map(|_| ()).map_err(|e| e.to_string()),
            "{name} reported a different error through the version loader"
        );
    }
}

// ---------------------------------------------------------------------
// Step 1: the Beta document, and the twins that must agree
// ---------------------------------------------------------------------

#[test]
fn a_beta_twin_of_the_minimal_config_resolves_to_the_same_lab() {
    let alpha = load_any_supported_lab(&testdata_config("minimal-valid.yaml"))
        .expect("minimal-valid.yaml must load");
    let beta = load_any_supported_lab(&testdata_config("minimal-valid-v1beta1.yaml"))
        .expect("minimal-valid-v1beta1.yaml must load");

    assert_eq!(without_source_path(beta), without_source_path(alpha));
}

#[test]
fn a_hand_written_beta_twin_resolves_to_the_same_lab_as_its_alpha_original() {
    // The maximal pair: every optional section populated, and every
    // renamed key present on both sides in its own spelling. If a
    // migration moved a value to the wrong field, or dropped one, this
    // is the assertion that sees it.
    let alpha = load_any_supported_lab(&testdata_config("renamed-fields-v1alpha1.yaml"))
        .expect("renamed-fields-v1alpha1.yaml must load");
    let beta = load_any_supported_lab(&testdata_config("renamed-fields-v1beta1.yaml"))
        .expect("renamed-fields-v1beta1.yaml must load");

    assert_eq!(without_source_path(beta), without_source_path(alpha));
}

#[test]
fn the_renamed_duration_keys_are_really_renamed() {
    // `deny_unknown_fields` in both directions is what makes the rename a
    // fact rather than an alias: neither version quietly accepts the
    // other's spelling, so a config cannot end up meaning something it
    // does not say.
    let dir = TempDir::new("renames");

    let beta_with_alpha_keys = dir.write_config(
        "apiVersion: admissionlab.io/v1beta1\n\
         kind: Lab\n\
         baseline:\n  kubernetes: \"1.29.4\"\n\
         candidate:\n  kubernetes: \"1.29.4\"\n\
         fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n\
         policy:\n  latency:\n    absoluteIncrease: 50\n",
    );
    let error = load_any_supported_lab(&beta_with_alpha_keys)
        .expect_err("a v1beta1 document must reject the v1alpha1 key");
    assert!(
        matches!(error, SpecError::Parse { .. }) && error.to_string().contains("absoluteIncrease"),
        "expected an unknown-field parse error naming absoluteIncrease, got: {error}"
    );

    let alpha_with_beta_keys = dir.write_config(
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: Lab\n\
         baseline:\n  kubernetes: \"1.29.4\"\n\
         candidate:\n  kubernetes: \"1.29.4\"\n\
         fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n\
         policy:\n  latency:\n    absoluteIncreaseMillis: 50\n",
    );
    let error = load_any_supported_lab(&alpha_with_beta_keys)
        .expect_err("a v1alpha1 document must reject the v1beta1 key");
    assert!(
        matches!(error, SpecError::Parse { .. })
            && error.to_string().contains("absoluteIncreaseMillis"),
        "expected an unknown-field parse error naming absoluteIncreaseMillis, got: {error}"
    );
}

#[test]
fn an_unsupported_api_version_is_refused_and_names_every_supported_version() {
    let dir = TempDir::new("unsupported");
    let path = dir.write_config(
        "apiVersion: admissionlab.io/v1\n\
         kind: Lab\n\
         baseline:\n  kubernetes: \"1.29.4\"\n\
         candidate:\n  kubernetes: \"1.29.4\"\n\
         fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n",
    );

    let error = load_any_supported_lab(&path).expect_err("v1 is not a supported version");
    let message = error.to_string();
    assert!(matches!(error, SpecError::Validation { .. }), "{message}");
    for version in [v1beta1::API_VERSION, v1alpha1::API_VERSION] {
        assert!(
            message.contains(version),
            "the error must name {version}, got: {message}"
        );
    }
    assert!(
        message.contains("admissionlab.io/v1\""),
        "the error must quote what was actually found, got: {message}"
    );
}

#[test]
fn a_document_without_an_api_version_names_every_supported_version() {
    let dir = TempDir::new("no-api-version");
    let path = dir.write_config(
        "kind: Lab\n\
         baseline:\n  kubernetes: \"1.29.4\"\n\
         candidate:\n  kubernetes: \"1.29.4\"\n\
         fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n",
    );

    let message = load_any_supported_lab(&path)
        .expect_err("a document with no apiVersion cannot be read")
        .to_string();
    for version in [v1beta1::API_VERSION, v1alpha1::API_VERSION] {
        assert!(
            message.contains(version),
            "the error must name {version}, got: {message}"
        );
    }
}

#[test]
fn a_beta_document_with_the_wrong_kind_is_refused() {
    let dir = TempDir::new("wrong-kind");
    let path = dir.write_config(
        "apiVersion: admissionlab.io/v1beta1\n\
         kind: FixtureMatrix\n\
         baseline:\n  kubernetes: \"1.29.4\"\n\
         candidate:\n  kubernetes: \"1.29.4\"\n\
         fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n",
    );

    let message = load_any_supported_lab(&path)
        .expect_err("kind must be Lab")
        .to_string();
    assert!(
        message.contains("kind:") && message.contains("\"Lab\""),
        "got: {message}"
    );
}

// ---------------------------------------------------------------------
// Step 3: migration is pure, total, and exhaustively checked
// ---------------------------------------------------------------------

#[test]
fn migration_carries_every_field_of_a_maximal_alpha_document_across() {
    // The exhaustiveness this test contributes is on the *Beta* side:
    // `migrate_v1alpha1_to_v1beta1` already destructures its Alpha input
    // exhaustively (so a new Alpha field is a compile error there), and
    // the destructure below does the same for the Beta output, so a new
    // Beta field is a compile error here — where the question "does
    // migration have an answer for it?" has to be asked.
    let alpha = load_lab(&testdata_config("renamed-fields-v1alpha1.yaml"))
        .expect("the maximal alpha fixture must load")
        .raw;

    let beta = migrate_v1alpha1_to_v1beta1(alpha.clone()).expect("a valid alpha document migrates");

    let V1Beta1Lab {
        api_version,
        kind,
        baseline,
        candidate,
        fixtures,
        policy,
        expectations_file,
        gateway,
    } = beta;

    // The one value that changes, and it is the only one.
    assert_eq!(api_version, v1beta1::API_VERSION);
    assert_eq!(alpha.api_version, v1alpha1::API_VERSION);

    assert_eq!(kind, alpha.kind);
    assert_eq!(baseline, alpha.baseline);
    assert_eq!(candidate, alpha.candidate);
    assert_eq!(fixtures, alpha.fixtures);
    assert_eq!(expectations_file, alpha.expectations_file);

    assert_eq!(policy.fail_on, alpha.policy.fail_on);
    assert_eq!(policy.overrides, alpha.policy.overrides);
    assert_eq!(
        policy.latency.absolute_increase, alpha.policy.latency.absolute_increase,
        "the rename is a wire-name change only; the Duration must survive it unchanged"
    );
    assert!(
        (policy.latency.relative_multiplier - alpha.policy.latency.relative_multiplier).abs()
            < f64::EPSILON
    );

    let gateway = gateway.expect("the maximal fixture declares a gateway suite");
    let alpha_gateway = alpha
        .gateway
        .as_ref()
        .expect("the maximal fixture declares a gateway suite");
    assert_eq!(gateway.manifests, alpha_gateway.manifests);
    assert_eq!(gateway.routes, alpha_gateway.routes);
    assert_eq!(
        gateway.reconciliation_timeout,
        alpha_gateway.reconciliation_timeout
    );
    assert_eq!(gateway.gateway_endpoint, alpha_gateway.gateway_endpoint);
    assert_eq!(gateway.readiness, alpha_gateway.readiness);
}

#[test]
fn migration_is_pure_and_repeatable() {
    // No I/O, no environment, no path resolution: the same input must
    // give the same output every time, and `manifests` must still hold
    // the *unresolved* paths the user wrote (resolution happens after
    // migration, and only there).
    let alpha = load_lab(&testdata_config("renamed-fields-v1alpha1.yaml"))
        .expect("load")
        .raw;

    let once = migrate_v1alpha1_to_v1beta1(alpha.clone()).expect("migrates");
    let twice = migrate_v1alpha1_to_v1beta1(alpha).expect("migrates");
    assert_eq!(once, twice);

    let gateway = once.gateway.expect("gateway suite");
    assert_eq!(
        gateway.manifests[0],
        Path::new("fixtures/gateway/namespace.yaml"),
        "migration must not resolve paths"
    );
}

#[test]
fn no_document_the_loader_accepts_can_fail_to_migrate() {
    // The totality property, asserted over every real Alpha document
    // this repository has rather than over one hand-picked example.
    for path in valid_alpha_documents() {
        let alpha = load_lab(&path)
            .unwrap_or_else(|e| panic!("{} must load: {e}", path.display()))
            .raw;
        assert!(
            migrate_v1alpha1_to_v1beta1(alpha).is_ok(),
            "{} loaded as v1alpha1 but could not migrate; migration is supposed to be total",
            path.display()
        );
    }
}

#[test]
fn migration_refuses_a_document_that_is_not_a_v1alpha1_lab() {
    // The one precondition, and the only reachable `MigrationError`: a
    // hand-built value whose header says it is something else. Not an
    // invented ambiguity — see `migrate.rs`'s "Then why does this return
    // a `Result`?" — and unreachable through the loader, which is what
    // the test above asserts from the other side.
    let mut alpha: V1Alpha1Lab = load_lab(&testdata_config("minimal-valid.yaml"))
        .expect("load")
        .raw;

    let wrong_version = V1Alpha1Lab {
        api_version: "admissionlab.io/v1beta1".to_owned(),
        ..alpha.clone()
    };
    assert_eq!(
        migrate_v1alpha1_to_v1beta1(wrong_version),
        Err(MigrationError::UnexpectedApiVersion {
            found: "admissionlab.io/v1beta1".to_owned(),
            expected: v1alpha1::API_VERSION,
            target: v1beta1::API_VERSION,
        })
    );

    alpha.kind = "FixtureMatrix".to_owned();
    assert_eq!(
        migrate_v1alpha1_to_v1beta1(alpha),
        Err(MigrationError::UnexpectedKind {
            found: "FixtureMatrix".to_owned(),
            expected: "Lab",
        })
    );
}

#[test]
fn a_migration_failure_reaches_resolve_lab_as_a_named_validation_error() {
    // `resolve_lab` takes a `LoadedLab` a caller may have built by hand,
    // so it inherits migration's precondition. It must surface as an
    // ordinary configuration error naming the offending field, not as a
    // panic and not as a silent success.
    let mut loaded = load_lab(&testdata_config("minimal-valid.yaml")).expect("load");
    loaded.raw.api_version = "admissionlab.io/v1".to_owned();

    let error = resolve_lab(loaded).expect_err("a mismatched header cannot resolve");
    let message = error.to_string();
    assert!(matches!(error, SpecError::Validation { .. }), "{message}");
    assert!(message.contains("apiVersion"), "{message}");
}
