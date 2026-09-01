//! ROADMAP Task 8.3: the Ingress-to-Gateway migration pairing model.
//!
//! These tests drive the model through **this crate's re-exports**
//! (`admissionlab_gateway::migration`), not through `admissionlab-spec`
//! directly, for the reason `tests/model.rs` gives for the Gateway
//! suite: the surface Tasks 8.4-8.5 will consume is the one named here,
//! and `reexported_types_are_the_spec_crate_types` closes the gap that a
//! locally declared twin would open.
//!
//! Loading always goes through the real `load_any_supported_lab`
//! pipeline against a file on disk, never a hand-built struct. Task 8.3
//! is a *configuration* contract, and a struct literal would skip
//! exactly the parsing, path resolution and validation being tested.
//!
//! `migration:` is a `v1beta1`-only section, so every document here
//! declares `admissionlab.io/v1beta1`. That is itself asserted, in
//! `an_alpha_document_resolves_to_no_migration_suite`.

use std::path::{Path, PathBuf};

use admissionlab_gateway::migration::{
    MigrationCaseSpec, MigrationSuiteSpec, NonPortableFeatureExpectation,
    expected_nonportable_features,
};
use admissionlab_spec::{SpecError, load_any_supported_lab};

/// A directory under the OS temp directory that deletes itself when the
/// value goes out of scope -- including on a failed assertion, because
/// [`Drop`] runs while a panic unwinds. The same helper, for the same
/// reason, `admissionlab-spec`'s `tests/migrate_alpha_beta.rs` uses: a
/// test that leaks a directory on every run leaks one on every run of
/// CI too.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "admissionlab-gateway-migration-{}-{label}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

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

/// The checked-in, fully valid migration lab configuration
/// (`testdata/configs/migration-valid.yaml`), which lives at the
/// workspace root -- two levels above this crate's own
/// `CARGO_MANIFEST_DIR`.
fn migration_valid_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/configs/migration-valid.yaml")
}

/// The header every ad-hoc document below shares: a minimal, otherwise
/// valid `v1beta1` lab, so nothing outside the `migration:` section can
/// be what fails.
const BETA_HEADER: &str = "apiVersion: admissionlab.io/v1beta1\n\
     kind: Lab\n\
     baseline:\n  kubernetes: \"1.33.4\"\n\
     candidate:\n  kubernetes: \"1.33.4\"\n\
     fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n";

/// Loads a `v1beta1` document whose `migration:` section is
/// `migration_section`, through the real pipeline, and returns the
/// resolved suite (or the error the pipeline rejected it with).
fn resolve_migration_section(
    label: &str,
    migration_section: &str,
) -> Result<Option<MigrationSuiteSpec>, SpecError> {
    let directory = TempDir::new(label);
    let path = directory.write_config(&format!("{BETA_HEADER}{migration_section}"));
    load_any_supported_lab(&path).map(|lab| lab.migration)
}

/// The `message` of a [`SpecError::Validation`], or a panic naming what
/// arrived instead. Every Task 8.3 rejection is a validation failure
/// (the documents below all *parse* cleanly), so a parse error here
/// would mean the test is testing something other than what it claims.
fn validation_message(error: &SpecError) -> String {
    match error {
        SpecError::Validation { message, .. } => message.clone(),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// A one-case `migration:` section with `body` spliced in as the case's
/// own fields, for the many one-field-at-a-time rejection tests.
fn one_case_section(body: &str) -> String {
    format!("migration:\n  cases:\n    - {}\n", body.trim_start())
}

/// The canonical valid one-case body, with both manifest lists and one
/// probe -- the baseline every rejection test below mutates exactly one
/// thing away from.
const VALID_CASE_BODY: &str = "id: basic\n      \
     baselineIngressManifests:\n        - ingress/basic.yaml\n      \
     candidateGatewayManifests:\n        - gateway/basic.yaml\n      \
     probes:\n        \
     - host: shop.lab.example\n          path: /\n          method: GET\n          \
     expectedStatus: 200\n";

fn reject(label: &str, section: &str) -> String {
    let error = resolve_migration_section(label, section)
        .expect_err("this document must be rejected at load time");
    validation_message(&error)
}

// ---------------------------------------------------------------------
// The section is optional, and only v1beta1 has it
// ---------------------------------------------------------------------

#[test]
fn a_lab_without_a_migration_section_resolves_to_none() {
    let resolved = resolve_migration_section("absent", "").expect("a lab may omit the section");
    assert_eq!(
        resolved, None,
        "a `migration:` section is opt-in; a lab that is not migrating off Ingress declares none"
    );
}

#[test]
fn an_alpha_document_resolves_to_no_migration_suite() {
    // `admissionlab.io/v1alpha1` has no `migration:` key at all, so the
    // migration to the current model maps it to `None` -- a translation
    // of what an Alpha document could only have meant, never a default
    // invented for a field its author could not have written.
    let directory = TempDir::new("alpha");
    let path = directory.write_config(
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: Lab\n\
         baseline:\n  kubernetes: \"1.33.4\"\n\
         candidate:\n  kubernetes: \"1.33.4\"\n\
         fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n",
    );
    let resolved = load_any_supported_lab(&path).expect("an alpha document still loads");
    assert_eq!(resolved.migration, None);
}

#[test]
fn an_alpha_document_may_not_declare_a_migration_section() {
    // The complement of the test above, and the one that makes it mean
    // something: v1alpha1 is `deny_unknown_fields`, so writing the Beta
    // section into an Alpha document is a loud parse error rather than a
    // silently ignored key.
    let directory = TempDir::new("alpha-with-section");
    let path = directory.write_config(&format!(
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: Lab\n\
         baseline:\n  kubernetes: \"1.33.4\"\n\
         candidate:\n  kubernetes: \"1.33.4\"\n\
         fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n\
         {}",
        one_case_section(VALID_CASE_BODY)
    ));
    let error = load_any_supported_lab(&path)
        .expect_err("v1alpha1 has no `migration:` key and denies unknown fields");
    assert!(
        format!("{error}").contains("migration"),
        "the error must name the offending key, got: {error}"
    );
}

// ---------------------------------------------------------------------
// Step 1: explicit baseline/candidate pairing, and path resolution
// ---------------------------------------------------------------------

#[test]
fn the_checked_in_migration_lab_resolves_with_both_sides_paired() {
    let resolved =
        load_any_supported_lab(&migration_valid_path()).expect("migration-valid.yaml resolves");
    let migration = resolved
        .migration
        .expect("migration-valid.yaml declares a migration suite");

    let ids: Vec<&str> = migration
        .cases
        .iter()
        .map(|case| case.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["basic-host-and-path", "nginx-annotations-with-known-gaps"]
    );

    let basic = &migration.cases[0];
    assert_eq!(basic.baseline_ingress_manifests.len(), 3);
    assert_eq!(basic.candidate_gateway_manifests.len(), 4);
    assert_eq!(basic.probes.len(), 3);
    assert!(
        basic.expected_nonportable.is_empty(),
        "a migration expected to be lossless declares nothing non-portable"
    );

    // Nothing about the candidate list is derived from the baseline
    // list: the two are independent, user-written sets, and the file
    // proves it by giving them different lengths and different contents.
    assert_ne!(
        basic.baseline_ingress_manifests,
        basic.candidate_gateway_manifests
    );
}

#[test]
fn manifest_paths_on_both_sides_resolve_against_the_configuration_directory() {
    let path = migration_valid_path();
    let config_dir = path.parent().expect("the testdata path has a parent");
    let resolved = load_any_supported_lab(&path).expect("migration-valid.yaml resolves");
    let migration = resolved.migration.expect("a migration suite");

    for case in &migration.cases {
        for manifest in case
            .baseline_ingress_manifests
            .iter()
            .chain(&case.candidate_gateway_manifests)
        {
            assert!(
                manifest.starts_with(config_dir),
                "{} must be resolved against the configuration file's own directory, not the \
                 process's working directory",
                manifest.display()
            );
        }
    }
}

#[test]
fn an_absolute_manifest_path_is_left_alone() {
    let absolute = if cfg!(windows) {
        "C:/absolute/ingress.yaml"
    } else {
        "/absolute/ingress.yaml"
    };
    let resolved = resolve_migration_section(
        "absolute",
        &one_case_section(&format!(
            "id: basic\n      \
             baselineIngressManifests:\n        - {absolute}\n      \
             candidateGatewayManifests:\n        - gateway/basic.yaml\n      \
             probes:\n        \
             - host: shop.lab.example\n          path: /\n          method: GET\n          \
             expectedStatus: 200\n"
        )),
    )
    .expect("an absolute path is valid")
    .expect("a migration suite");

    assert_eq!(
        resolved.cases[0].baseline_ingress_manifests[0],
        Path::new(absolute)
    );
}

#[test]
fn an_empty_case_list_is_rejected() {
    let message = reject("no-cases", "migration:\n  cases: []\n");
    assert!(message.contains("must not be empty"), "{message}");
}

#[test]
fn a_case_with_no_baseline_manifests_is_rejected() {
    let message = reject(
        "no-baseline",
        &one_case_section(
            "id: basic\n      \
             baselineIngressManifests: []\n      \
             candidateGatewayManifests:\n        - gateway/basic.yaml\n      \
             probes:\n        \
             - host: shop.lab.example\n          path: /\n          method: GET\n          \
             expectedStatus: 200\n",
        ),
    );
    assert!(message.contains("must not be empty"), "{message}");
    assert!(
        message.contains("does not convert"),
        "the message must say why half a pairing is not one: {message}"
    );
}

#[test]
fn a_case_with_no_candidate_manifests_is_rejected() {
    let message = reject(
        "no-candidate",
        &one_case_section(
            "id: basic\n      \
             baselineIngressManifests:\n        - ingress/basic.yaml\n      \
             candidateGatewayManifests: []\n      \
             probes:\n        \
             - host: shop.lab.example\n          path: /\n          method: GET\n          \
             expectedStatus: 200\n",
        ),
    );
    assert!(message.contains("must not be empty"), "{message}");
}

#[test]
fn a_case_may_not_omit_either_manifest_list() {
    // Distinct from the two tests above: an omitted key is a *parse*
    // error (neither list has a `serde` default), which is what makes
    // "explicit pairing" a property of the type rather than only of the
    // validator.
    let error = resolve_migration_section(
        "omitted-candidate",
        &one_case_section(
            "id: basic\n      \
             baselineIngressManifests:\n        - ingress/basic.yaml\n      \
             probes:\n        \
             - host: shop.lab.example\n          path: /\n          method: GET\n          \
             expectedStatus: 200\n",
        ),
    )
    .expect_err("a case with no `candidateGatewayManifests` key must not parse");
    assert!(
        format!("{error}").contains("candidateGatewayManifests"),
        "the error must name the missing key, got: {error}"
    );
}

#[test]
fn a_case_with_an_empty_id_is_rejected() {
    let message = reject(
        "empty-id",
        &one_case_section(
            "id: \"   \"\n      \
             baselineIngressManifests:\n        - ingress/basic.yaml\n      \
             candidateGatewayManifests:\n        - gateway/basic.yaml\n      \
             probes:\n        \
             - host: shop.lab.example\n          path: /\n          method: GET\n          \
             expectedStatus: 200\n",
        ),
    );
    assert!(message.contains("must not be empty"), "{message}");
}

#[test]
fn two_cases_may_not_share_an_id() {
    let case = VALID_CASE_BODY;
    let message = reject(
        "duplicate-id",
        &format!("migration:\n  cases:\n    - {case}    - {case}"),
    );
    assert!(
        message.contains("duplicate migration case id \"basic\""),
        "{message}"
    );
}

#[test]
fn an_unknown_key_inside_a_case_is_rejected() {
    let error = resolve_migration_section(
        "unknown-key",
        &one_case_section(
            "id: basic\n      \
             baselineIngressManifests:\n        - ingress/basic.yaml\n      \
             candidateGatewayManifests:\n        - gateway/basic.yaml\n      \
             convertAutomatically: true\n      \
             probes:\n        \
             - host: shop.lab.example\n          path: /\n          method: GET\n          \
             expectedStatus: 200\n",
        ),
    )
    .expect_err("every migration struct is deny_unknown_fields");
    assert!(
        format!("{error}").contains("convertAutomatically"),
        "the error must name the offending key, got: {error}"
    );
}

// ---------------------------------------------------------------------
// Probes: the same vocabulary, validated by the same rules
// ---------------------------------------------------------------------

#[test]
fn a_case_with_no_probes_is_rejected() {
    let message = reject(
        "no-probes",
        &one_case_section(
            "id: basic\n      \
             baselineIngressManifests:\n        - ingress/basic.yaml\n      \
             candidateGatewayManifests:\n        - gateway/basic.yaml\n      \
             probes: []\n",
        ),
    );
    assert!(message.contains("must not be empty"), "{message}");
    assert!(
        message.contains("status vocabulary"),
        "the message must say why a migration case differs from a route contract here: {message}"
    );
}

#[test]
fn a_probe_method_outside_the_gateway_api_enumeration_is_rejected() {
    let message = reject(
        "bad-method",
        &one_case_section(
            "id: basic\n      \
             baselineIngressManifests:\n        - ingress/basic.yaml\n      \
             candidateGatewayManifests:\n        - gateway/basic.yaml\n      \
             probes:\n        \
             - host: shop.lab.example\n          path: /\n          method: get\n          \
             expectedStatus: 200\n",
        ),
    );
    // Byte-for-byte the message a `gateway:` probe would get: one probe
    // validator, so the two sections can never disagree about what a
    // valid probe is.
    assert!(
        message.contains("is not an HTTP method a Gateway API HTTPRoute can match"),
        "{message}"
    );
}

#[test]
fn a_probe_status_outside_the_http_range_is_rejected() {
    let message = reject(
        "bad-status",
        &one_case_section(
            "id: basic\n      \
             baselineIngressManifests:\n        - ingress/basic.yaml\n      \
             candidateGatewayManifests:\n        - gateway/basic.yaml\n      \
             probes:\n        \
             - host: shop.lab.example\n          path: /\n          method: GET\n          \
             expectedStatus: 999\n",
        ),
    );
    assert!(message.contains("must be in 100..=599"), "{message}");
}

#[test]
fn a_probe_path_without_a_leading_slash_is_rejected() {
    let message = reject(
        "bad-path",
        &one_case_section(
            "id: basic\n      \
             baselineIngressManifests:\n        - ingress/basic.yaml\n      \
             candidateGatewayManifests:\n        - gateway/basic.yaml\n      \
             probes:\n        \
             - host: shop.lab.example\n          path: api\n          method: GET\n          \
             expectedStatus: 200\n",
        ),
    );
    assert!(message.contains("must start with \"/\""), "{message}");
}

#[test]
fn a_probe_locator_names_the_migration_case_it_came_from() {
    let message = reject(
        "probe-locator",
        &one_case_section(
            "id: basic\n      \
             baselineIngressManifests:\n        - ingress/basic.yaml\n      \
             candidateGatewayManifests:\n        - gateway/basic.yaml\n      \
             probes:\n        \
             - host: \"\"\n          path: /\n          method: GET\n          \
             expectedStatus: 200\n",
        ),
    );
    // The message body is the shared probe validator's; the *locator*
    // is what tells a user which of two sections and which case the
    // rejected probe sits in.
    assert!(message.contains("must not be empty"), "{message}");
}

#[test]
fn a_probe_locator_is_reported_for_the_offending_case_and_index() {
    let error = resolve_migration_section(
        "probe-index",
        &one_case_section(
            "id: basic\n      \
             baselineIngressManifests:\n        - ingress/basic.yaml\n      \
             candidateGatewayManifests:\n        - gateway/basic.yaml\n      \
             probes:\n        \
             - host: shop.lab.example\n          path: /\n          method: GET\n          \
             expectedStatus: 200\n        \
             - host: shop.lab.example\n          path: /\n          method: GET\n          \
             expectedStatus: 7\n",
        ),
    )
    .expect_err("the second probe is invalid");
    assert!(
        format!("{error}").contains("migration.cases[0].probes[1]"),
        "the locator must name the exact probe, got: {error}"
    );
}

// ---------------------------------------------------------------------
// Step 2: non-portable expectations carry a human reason
// ---------------------------------------------------------------------

/// A one-case section whose single case declares `expectations` as its
/// `expectedNonportable` list.
fn case_with_expectations(expectations: &str) -> String {
    one_case_section(&format!(
        "id: basic\n      \
         baselineIngressManifests:\n        - ingress/basic.yaml\n      \
         candidateGatewayManifests:\n        - gateway/basic.yaml\n      \
         probes:\n        \
         - host: shop.lab.example\n          path: /\n          method: GET\n          \
         expectedStatus: 200\n      \
         expectedNonportable:\n{expectations}"
    ))
}

#[test]
fn a_nonportable_expectation_carries_a_feature_and_a_reason() {
    let resolved = resolve_migration_section(
        "nonportable",
        &case_with_expectations(
            "        - feature: nginx.ingress.kubernetes.io/configuration-snippet\n          \
             reason: no portable Gateway API equivalent; the header it added is retired\n",
        ),
    )
    .expect("a well-formed expectation is accepted")
    .expect("a migration suite");

    assert_eq!(
        resolved.cases[0].expected_nonportable,
        vec![NonPortableFeatureExpectation {
            feature: "nginx.ingress.kubernetes.io/configuration-snippet".to_owned(),
            reason: "no portable Gateway API equivalent; the header it added is retired".to_owned(),
        }]
    );
}

#[test]
fn a_nonportable_expectation_without_a_reason_is_rejected() {
    // A reason is not decoration: an accepted behavioral difference with
    // no written justification is indistinguishable from someone quietly
    // silencing a real regression.
    let error = resolve_migration_section(
        "no-reason",
        &case_with_expectations(
            "        - feature: nginx.ingress.kubernetes.io/configuration-snippet\n",
        ),
    )
    .expect_err("an expectation with no `reason` key must not parse");
    assert!(
        format!("{error}").contains("reason"),
        "the error must name the missing key, got: {error}"
    );
}

#[test]
fn a_nonportable_expectation_with_an_empty_reason_is_rejected() {
    let message = reject(
        "empty-reason",
        &case_with_expectations(
            "        - feature: nginx.ingress.kubernetes.io/configuration-snippet\n          \
             reason: \"  \"\n",
        ),
    );
    assert!(message.contains("must not be empty"), "{message}");
}

#[test]
fn a_nonportable_expectation_with_an_empty_feature_is_rejected() {
    let message = reject(
        "empty-feature",
        &case_with_expectations("        - feature: \"\"\n          reason: something was lost\n"),
    );
    assert!(message.contains("must not be empty"), "{message}");
}

#[test]
fn two_expectations_may_not_name_the_same_feature() {
    let message = reject(
        "duplicate-feature",
        &case_with_expectations(
            "        - feature: nginx.ingress.kubernetes.io/auth-url\n          \
             reason: expressed as a policy attachment instead\n        \
             - feature: nginx.ingress.kubernetes.io/auth-url\n          \
             reason: and also for this other unrelated thing\n",
        ),
    );
    assert!(
        message.contains("duplicate non-portable feature"),
        "{message}"
    );
    assert!(
        message.contains("one feature has one reason"),
        "the message must say why two entries for one feature is not a merge: {message}"
    );
}

#[test]
fn the_same_feature_may_be_declared_in_two_different_cases() {
    // Uniqueness is per case, deliberately: two migrations may each lose
    // `configuration-snippet`, for two different reasons, and forcing
    // one shared entry would make one of the two reasons wrong.
    let case = |id: &str| {
        format!(
            "id: {id}\n      \
             baselineIngressManifests:\n        - ingress/{id}.yaml\n      \
             candidateGatewayManifests:\n        - gateway/{id}.yaml\n      \
             probes:\n        \
             - host: shop.lab.example\n          path: /\n          method: GET\n          \
             expectedStatus: 200\n      \
             expectedNonportable:\n        \
             - feature: nginx.ingress.kubernetes.io/configuration-snippet\n          \
             reason: this case drops the header entirely\n"
        )
    };
    let resolved = resolve_migration_section(
        "same-feature-two-cases",
        &format!(
            "migration:\n  cases:\n    - {}    - {}",
            case("first"),
            case("second")
        ),
    )
    .expect("the same feature in two cases is legitimate")
    .expect("a migration suite");

    assert_eq!(resolved.cases.len(), 2);
}

#[test]
fn expected_nonportable_features_projects_the_declared_names() {
    let resolved =
        load_any_supported_lab(&migration_valid_path()).expect("migration-valid.yaml resolves");
    let migration = resolved.migration.expect("a migration suite");

    assert!(
        expected_nonportable_features(&migration.cases[0]).is_empty(),
        "a case declaring nothing non-portable expects nothing"
    );

    let features = expected_nonportable_features(&migration.cases[1]);
    assert_eq!(
        features.into_iter().collect::<Vec<_>>(),
        vec![
            "nginx.ingress.kubernetes.io/auth-url",
            "nginx.ingress.kubernetes.io/configuration-snippet",
        ],
        "the set is ordered by name, independent of declaration order"
    );

    // The set never collapses anything: duplicates are rejected at load
    // time, so its size always equals the declared list's length.
    assert_eq!(
        expected_nonportable_features(&migration.cases[1]).len(),
        migration.cases[1].expected_nonportable.len()
    );
}

// ---------------------------------------------------------------------
// Step 3: migration expectations are not regression expectations
// ---------------------------------------------------------------------

#[test]
fn a_migration_suite_and_an_expectations_file_are_independent_sections() {
    // The two mechanisms answer different questions about different
    // vocabularies (see `NonPortableFeatureExpectation`'s "Why this is
    // not expectations.yaml"), and neither is required by the other:
    // this document declares both, and the migration suite carries its
    // own accepted difference without an `expectations.yaml` entry.
    let resolved = resolve_migration_section(
        "with-expectations-file",
        &format!(
            "expectationsFile: expectations.yaml\n{}",
            case_with_expectations(
                "        - feature: nginx.ingress.kubernetes.io/server-snippet\n          \
                 reason: replaced by an implementation-specific filter\n",
            )
        ),
    )
    .expect("a lab may declare both")
    .expect("a migration suite");

    assert_eq!(resolved.cases[0].expected_nonportable.len(), 1);
}

#[test]
fn a_migration_case_has_no_severity_or_failon_vocabulary() {
    // Deliberately spelled as a rejection rather than only as prose: if
    // someone ever adds a grading field to a migration case, this test
    // is what fails. Classification lives in `policy` and nowhere else.
    let error = resolve_migration_section(
        "severity",
        &case_with_expectations(
            "        - feature: nginx.ingress.kubernetes.io/auth-url\n          \
             reason: replaced by a policy attachment\n          severity: warning\n",
        ),
    )
    .expect_err("a non-portable expectation grades nothing");
    assert!(
        format!("{error}").contains("severity"),
        "the error must name the offending key, got: {error}"
    );
}

// ---------------------------------------------------------------------
// The re-export is the spec crate's type, not a twin
// ---------------------------------------------------------------------

#[test]
fn reexported_types_are_the_spec_crate_types() {
    // These assignments do not compile if `admissionlab_gateway::migration`
    // ever declares its own structurally identical twin instead of
    // re-exporting -- which is the synonym §1.2's registry forbids.
    fn takes_the_spec_type(
        case: &admissionlab_spec::MigrationCaseSpec,
    ) -> std::collections::BTreeSet<&str> {
        expected_nonportable_features(case)
    }
    let _ = takes_the_spec_type;

    let case: MigrationCaseSpec = admissionlab_spec::MigrationCaseSpec {
        id: "basic".to_owned(),
        baseline_ingress_manifests: vec![PathBuf::from("ingress/basic.yaml")],
        candidate_gateway_manifests: vec![PathBuf::from("gateway/basic.yaml")],
        probes: Vec::new(),
        expected_nonportable: vec![admissionlab_spec::NonPortableFeatureExpectation {
            feature: "f".to_owned(),
            reason: "r".to_owned(),
        }],
    };
    let suite: MigrationSuiteSpec = admissionlab_spec::MigrationSuiteSpec {
        cases: vec![case],
        // ROADMAP Task 8.8's per-side data-plane blocks, both `None`
        // here: they are optional in the type so that a document written
        // before they existed still parses, and this assertion is about
        // the re-export naming the same type rather than about a
        // runnable suite.
        baseline: None,
        candidate: None,
    };
    let _: admissionlab_spec::MigrationSuiteSpec = suite;
}
