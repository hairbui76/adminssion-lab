//! ROADMAP Task 7.4: the certified compatibility matrix — its checked-in
//! content, its validation rules, and the tier assignments the Task 7.5
//! workflows generate their job matrices from.
//!
//! # What each half of this file is for
//!
//! The first half asserts facts about the **real, checked-in**
//! `compatibility/recipes.yaml` and `compatibility/kubernetes.yaml`: that
//! they are valid against each other and against the recipes this
//! workspace actually ships, and that the certified rows and tiers are
//! exactly the reviewed ones. These are the tests that fail when someone
//! edits either file — which is the entire point of a checked-in support
//! matrix (`compatibility/kubernetes.yaml`'s own header: "a deliberate,
//! reviewed change to this checked-in file").
//!
//! The second half drives
//! [`admissionlab_recipes::validate_compatibility`] over **synthetic**
//! matrices, one per rule. A validation rule that is only ever run
//! against valid input has never been shown to reject anything; each of
//! these constructs exactly the mistake the rule exists to catch, and
//! asserts the message names it.
//!
//! # Why the supported Kubernetes set arrives as data
//!
//! `admissionlab-recipes` does not depend on `admissionlab-cluster` (see
//! [`admissionlab_recipes::SupportedKubernetes`]'s own documentation for
//! why that edge must not exist), so it cannot read
//! `compatibility/kubernetes.yaml` itself. This test *does* have
//! `admissionlab-cluster` as a dev-dependency, so it loads that file
//! through the one loader that owns it
//! ([`admissionlab_cluster::load_matrix`]) and converts — which means
//! this test validates the two real files against each other, not a
//! transcription of one of them.
//!
//! Nothing here creates a cluster, spawns a process, or touches the
//! network: every input is embedded in the binary at compile time.

use admissionlab_recipes::{
    CertificationTier, CertifiedCombination, CertifiedKubernetes, CompatibilityProblem,
    KubernetesCompatibility, RELEASE_SUPPORTED_MINORS, Recipe, RecipeCompatibilityEntry,
    RecipeCompatibilityMatrix, SupportWindowException, SupportedKubernetes, load_builtin_recipes,
    load_recipe_compatibility, narrow_certified_versions, validate_compatibility,
};

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

/// The real, checked-in recipe compatibility matrix.
fn checked_in_matrix() -> RecipeCompatibilityMatrix {
    load_recipe_compatibility().expect("compatibility/recipes.yaml must parse")
}

/// The real, checked-in supported Kubernetes set, read through
/// `admissionlab-cluster`'s own loader and converted to the shape
/// `validate_compatibility` takes. See this file's module documentation.
fn checked_in_supported() -> Vec<SupportedKubernetes> {
    let matrix = admissionlab_cluster::load_matrix().expect("compatibility/kubernetes.yaml");
    matrix
        .releases
        .iter()
        .filter(|release| release.supported)
        .map(|release| SupportedKubernetes {
            minor: release.minor.clone(),
            version: release.version.clone(),
        })
        .collect()
}

/// Every recipe this repository ships, from both places one can live.
///
/// `kyverno` and `istio` are `BUILTIN_RECIPES` entries, embedded at
/// compile time. `istio-gateway` is not, and cannot be: its stack's
/// other half installs raw manifests by a path relative to its own
/// directory, and an embedded built-in has no filesystem location to
/// resolve one against (`admissionlab_recipes::load`'s own
/// documentation). It is loaded through `load_recipe_overrides` from
/// `recipes/istio-gateway/`, exactly as
/// `tests/istio_gateway_recipe.rs` loads it to install it.
///
/// Both sources are passed to `validate_compatibility` because its rule
/// is "some loaded recipe pins this", never "a built-in does" — a
/// certified recipe that happens to be loaded from a directory is no
/// less pinned than one that happens to be embedded.
fn checked_in_recipes() -> Vec<Recipe> {
    let mut recipes = load_builtin_recipes().expect("the built-in recipes must load");
    // `nginx-gateway-fabric` (Task 8.1) is directory-loaded for exactly
    // the reason `istio-gateway` is: its stack's other half installs the
    // vendored Gateway API CRD bundle by a path relative to its own
    // directory. See `tests/nginx_gateway_recipe.rs`'s
    // `load_nginx_gateway_recipes`.
    //
    // `ingress-nginx-legacy` (Task 8.2) is directory-loaded for a
    // different reason: unlike the other two it *could* have been a
    // built-in (it installs purely via Helm). It deliberately is not:
    // the built-in set is what a binary offers without being pointed
    // anywhere, and an archived upstream whose own maintainers say not
    // to deploy it does not belong there. See
    // `tests/ingress_nginx_legacy.rs`'s `load_legacy_recipe`.
    for directory in [
        "recipes/istio-gateway",
        "recipes/nginx-gateway-fabric",
        "recipes/ingress-nginx-legacy",
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(directory)
            .canonicalize()
            .unwrap_or_else(|error| panic!("{directory} must exist in this checkout: {error}"));
        recipes.extend(
            admissionlab_recipes::load_recipe_overrides(&path)
                .unwrap_or_else(|error| panic!("{directory} must load: {error}")),
        );
    }
    recipes
}

/// A supported-Kubernetes entry, deriving the minor from the patch
/// version so a synthetic case cannot accidentally disagree with itself.
fn supported(version: &str) -> SupportedKubernetes {
    let (major, rest) = version
        .split_once('.')
        .expect("a major.minor.patch version");
    let (minor, _patch) = rest.split_once('.').expect("a major.minor.patch version");
    SupportedKubernetes {
        minor: format!("{major}.{minor}"),
        version: version.to_owned(),
    }
}

/// One matrix entry certifying `versions`, every one of them at
/// [`CertificationTier::PerCommit`] (which rule under test cares about
/// the tier is stated by each test that does).
fn entry(name: &str, version: &str, versions: &[&str]) -> RecipeCompatibilityEntry {
    RecipeCompatibilityEntry {
        name: name.to_owned(),
        version: version.to_owned(),
        kubernetes: KubernetesCompatibility {
            documented_range: None,
            certified: versions
                .iter()
                .map(|kubernetes| CertifiedKubernetes {
                    version: (*kubernetes).to_owned(),
                    tier: CertificationTier::PerCommit,
                })
                .collect(),
        },
    }
}

/// A synthetic matrix with no support-window exception.
fn matrix(entries: Vec<RecipeCompatibilityEntry>) -> RecipeCompatibilityMatrix {
    RecipeCompatibilityMatrix {
        support_window_exception: None,
        recipes: entries,
    }
}

/// Panics unless exactly one problem was found whose message contains
/// `needle`, returning nothing — the shape almost every synthetic case
/// below wants.
fn assert_one_problem(problems: &[CompatibilityProblem], needle: &str) {
    assert_eq!(
        problems.len(),
        1,
        "expected exactly one problem mentioning {needle:?}, got {problems:#?}"
    );
    assert!(
        problems[0].message.contains(needle),
        "expected a problem mentioning {needle:?}, got {problems:#?}"
    );
}

// ---------------------------------------------------------------------
// The checked-in files
// ---------------------------------------------------------------------

#[test]
fn the_checked_in_compatibility_metadata_is_valid() {
    let problems = validate_compatibility(
        &checked_in_matrix(),
        &checked_in_supported(),
        &checked_in_recipes(),
    );
    assert!(
        problems.is_empty(),
        "compatibility/recipes.yaml and compatibility/kubernetes.yaml disagree: {problems:#?}"
    );
}

#[test]
fn exactly_three_kubernetes_minors_are_supported_and_no_exception_is_declared() {
    let supported = checked_in_supported();
    let mut minors: Vec<&str> = supported
        .iter()
        .map(|release| release.minor.as_str())
        .collect();
    minors.sort_unstable();
    minors.dedup();
    assert_eq!(
        minors.len(),
        RELEASE_SUPPORTED_MINORS,
        "Global Constraint 10: a release candidate supports exactly {RELEASE_SUPPORTED_MINORS} \
         Kubernetes minors; found {minors:?}"
    );
    assert!(
        checked_in_matrix().support_window_exception.is_none(),
        "the support-window escape hatch is declared while upstream's window is the ordinary \
         three minors; it must be deleted rather than left standing"
    );
}

/// The reviewed tier assignment, restated here so that moving a
/// combination between tiers — which changes how often it is certified,
/// and which of Task 7.5's three workflows pays for it — fails a
/// millisecond-scale test rather than only changing CI cost silently.
/// `compatibility/recipes.yaml`'s own per-entry comments justify each
/// row; this asserts the file still says what those comments explain.
#[test]
fn the_certified_combinations_are_exactly_the_reviewed_ones() {
    // The version each recipe pins. `istio` and `istio-gateway` share
    // one, deliberately -- see `compatibility/recipes.yaml`'s
    // `istio-gateway` entry.
    let pinned_version = |recipe: &str| {
        match recipe {
            "kyverno" => "3.9.0",
            "istio" | "istio-gateway" => "1.30.4",
            "nginx-gateway-fabric" => "2.6.7",
            "ingress-nginx-legacy" => "4.15.1",
            other => panic!("this test has no pinned version for recipe {other:?}"),
        }
        .to_owned()
    };
    let combination = |kubernetes: &str, recipe: &str, tier| CertifiedCombination {
        kubernetes: kubernetes.to_owned(),
        recipe: recipe.to_owned(),
        recipe_version: pinned_version(recipe),
        tier,
    };
    assert_eq!(
        checked_in_matrix().certified_combinations(),
        vec![
            combination("1.35.8", "kyverno", CertificationTier::PerCommit),
            combination("1.35.8", "istio", CertificationTier::Nightly),
            combination("1.36.4", "istio", CertificationTier::PerCommit),
            combination("1.37.0", "istio", CertificationTier::Nightly),
            combination("1.35.8", "istio-gateway", CertificationTier::WeeklyRelease),
            combination("1.36.4", "istio-gateway", CertificationTier::PerCommit),
            combination("1.37.0", "istio-gateway", CertificationTier::WeeklyRelease),
            // Task 8.1, retiered by Task 8.9. The primary minor per
            // commit; the other two at Tier 2 rather than Tier 3, which
            // is where they sat when 8.1 landed. The asymmetry with
            // `istio-gateway` above is deliberate and its reasons are in
            // the entry's own comment -- 8.9 step 1 asks for NGF in
            // Tier 2, the Phase 8 gate rests on NGF specifically, and
            // NGF's stack is the cheaper of the two to install.
            combination("1.35.8", "nginx-gateway-fabric", CertificationTier::Nightly),
            combination(
                "1.36.4",
                "nginx-gateway-fabric",
                CertificationTier::PerCommit
            ),
            combination("1.37.0", "nginx-gateway-fabric", CertificationTier::Nightly),
            // Task 8.2, confirmed by Task 8.9 step 2. One row, Tier 3,
            // on the Tier-1 primary Kubernetes version only -- the
            // migration-specific placement, and the only place the
            // archived stack is installed outside the Tier-3 migration
            // demo. See that entry's own comment.
            combination(
                "1.36.4",
                "ingress-nginx-legacy",
                CertificationTier::WeeklyRelease
            ),
        ]
    );
}

/// The Phase 7 exit gate requires Tier 2 to run "across all three
/// Kubernetes minors". Tiers are cumulative downward, so Tier 2's job
/// matrix is every combination at or above `Nightly` in frequency.
#[test]
fn tier_2_covers_every_supported_kubernetes_minor() {
    let matrix = checked_in_matrix();
    let mut covered: Vec<String> = matrix
        .certified_combinations()
        .into_iter()
        .filter(|combination| combination.tier <= CertificationTier::Nightly)
        .map(|combination| combination.kubernetes)
        .collect();
    covered.sort();
    covered.dedup();

    let mut supported: Vec<String> = checked_in_supported()
        .into_iter()
        .map(|release| release.version)
        .collect();
    supported.sort();

    assert_eq!(
        covered, supported,
        "the nightly tier must certify a recipe on every supported Kubernetes version"
    );
}

/// Tier 1 is per commit, and a per-commit job that installs three full
/// stacks per recipe is the cost this tiering exists to avoid: at most
/// one Kubernetes version per recipe may be `perCommit`.
#[test]
fn tier_1_certifies_at_most_one_kubernetes_version_per_recipe() {
    for entry in &checked_in_matrix().recipes {
        let per_commit = entry
            .kubernetes
            .certified
            .iter()
            .filter(|certified| certified.tier == CertificationTier::PerCommit)
            .count();
        assert!(
            per_commit <= 1,
            "{} {} marks {per_commit} Kubernetes versions perCommit; Tier 1 runs on every commit \
             and must stay one cluster per recipe",
            entry.name,
            entry.version
        );
    }
}

#[test]
fn certifies_answers_the_lookup_a_lab_run_makes() {
    let matrix = checked_in_matrix();
    assert!(matrix.certifies("kyverno", "3.9.0", "1.35.8"));
    // Kyverno's own documented window stops at 1.35, so Admission Lab's
    // Tier-1 primary is deliberately not certified for it.
    assert!(!matrix.certifies("kyverno", "3.9.0", "1.36.4"));
    // A version of the recipe nothing pins.
    assert!(!matrix.certifies("kyverno", "4.0.0", "1.35.8"));
    // A stack Admission Lab has no opinion about at all.
    assert!(!matrix.certifies("my-webhook", "1.0.0", "1.36.4"));
}

// ---------------------------------------------------------------------
// Step 1: exactly three supported minors, and the documented escape
// hatch
// ---------------------------------------------------------------------

#[test]
fn a_fourth_supported_minor_is_rejected_without_a_declared_exception() {
    let supported = [
        supported("1.34.11"),
        supported("1.35.8"),
        supported("1.36.4"),
        supported("1.37.0"),
    ];
    let problems = validate_compatibility(&matrix(Vec::new()), &supported, &[]);
    assert_one_problem(&problems, "must support exactly 3");
    assert_eq!(problems[0].locator, "compatibility/kubernetes.yaml");
    assert!(
        problems[0].message.contains("supportWindowException"),
        "the refusal must name the escape hatch that would allow it: {problems:#?}"
    );
}

#[test]
fn two_supported_minors_are_rejected_without_a_declared_exception() {
    let supported = [supported("1.36.4"), supported("1.37.0")];
    let problems = validate_compatibility(&matrix(Vec::new()), &supported, &[]);
    assert_one_problem(&problems, "must support exactly 3");
}

#[test]
fn several_patch_versions_of_one_minor_still_count_as_one_minor() {
    // Global Constraint 10 counts minors, not entries: pinning two patch
    // releases of 1.37 must not look like a fourth supported minor.
    let supported = [
        supported("1.35.8"),
        supported("1.36.4"),
        supported("1.37.0"),
        supported("1.37.1"),
    ];
    assert!(
        validate_compatibility(&matrix(Vec::new()), &supported, &[]).is_empty(),
        "four supported entries across three minors is three supported minors"
    );
}

#[test]
fn a_declared_exception_permits_the_count_it_names() {
    let mut with_exception = matrix(Vec::new());
    with_exception.support_window_exception = Some(SupportWindowException {
        expected_supported_minors: 2,
        reason: "upstream 1.38 slipped and 1.35 reached EOL before it shipped".to_owned(),
        release_notes: "docs/release-notes/v1.0.0.md".to_owned(),
    });
    let supported = [supported("1.36.4"), supported("1.37.0")];
    assert!(
        validate_compatibility(&with_exception, &supported, &[]).is_empty(),
        "a declared, explained exception is exactly what the roadmap allows"
    );
}

#[test]
fn a_declared_exception_does_not_excuse_a_count_it_does_not_name() {
    let mut with_exception = matrix(Vec::new());
    with_exception.support_window_exception = Some(SupportWindowException {
        expected_supported_minors: 2,
        reason: "upstream 1.38 slipped".to_owned(),
        release_notes: "docs/release-notes/v1.0.0.md".to_owned(),
    });
    // The exception claims two; the file carries four.
    let supported = [
        supported("1.34.11"),
        supported("1.35.8"),
        supported("1.36.4"),
        supported("1.37.0"),
    ];
    let problems = validate_compatibility(&with_exception, &supported, &[]);
    assert_one_problem(&problems, "must support exactly 2");
}

#[test]
fn an_exception_restating_the_default_is_reported_as_stale() {
    let mut with_exception = matrix(Vec::new());
    with_exception.support_window_exception = Some(SupportWindowException {
        expected_supported_minors: RELEASE_SUPPORTED_MINORS,
        reason: "left over from the last release".to_owned(),
        release_notes: "docs/release-notes/v0.9.0.md".to_owned(),
    });
    let supported = [
        supported("1.35.8"),
        supported("1.36.4"),
        supported("1.37.0"),
    ];
    let problems = validate_compatibility(&with_exception, &supported, &[]);
    assert_one_problem(&problems, "stale");
}

#[test]
fn an_exception_without_release_notes_or_a_reason_is_rejected() {
    let mut with_exception = matrix(Vec::new());
    with_exception.support_window_exception = Some(SupportWindowException {
        expected_supported_minors: 2,
        reason: "   ".to_owned(),
        release_notes: String::new(),
    });
    let supported = [supported("1.36.4"), supported("1.37.0")];
    let problems = validate_compatibility(&with_exception, &supported, &[]);
    assert_eq!(problems.len(), 2, "{problems:#?}");
    assert!(
        problems
            .iter()
            .any(|problem| problem.message.contains("reason is empty"))
    );
    assert!(
        problems
            .iter()
            .any(|problem| problem.message.contains("releaseNotes is empty"))
    );
}

// ---------------------------------------------------------------------
// Step 2: rows must reference install metadata that exists
// ---------------------------------------------------------------------

#[test]
fn a_row_naming_a_recipe_version_nothing_pins_is_an_error() {
    // 4.0.0 is a plausible next Kyverno chart, and exactly the drift
    // this rule catches: the matrix is bumped, the recipe is not.
    let matrix = matrix(vec![entry("kyverno", "4.0.0", &["1.35.8"])]);
    let problems = validate_compatibility(&matrix, &checked_in_supported(), &checked_in_recipes());
    assert_one_problem(&problems, "no loaded recipe pins kyverno at version 4.0.0");
    assert_eq!(
        problems[0].locator,
        "compatibility/recipes.yaml: kyverno 4.0.0"
    );
    assert!(
        problems[0].message.contains("kyverno 3.9.0"),
        "the message must say what IS pinned so the fix is obvious: {problems:#?}"
    );
}

#[test]
fn a_row_naming_a_recipe_that_does_not_exist_is_an_error() {
    let matrix = matrix(vec![entry("not-a-recipe", "1.0.0", &["1.35.8"])]);
    let problems = validate_compatibility(&matrix, &checked_in_supported(), &checked_in_recipes());
    assert_one_problem(&problems, "no loaded recipe pins not-a-recipe");
}

#[test]
fn the_real_matrix_rows_all_reference_a_real_pinned_recipe() {
    let recipes = checked_in_recipes();
    for entry in &checked_in_matrix().recipes {
        assert!(
            recipes
                .iter()
                .any(|recipe| recipe.name == entry.name && recipe.version == entry.version),
            "compatibility/recipes.yaml certifies {} {}, which no built-in recipe pins",
            entry.name,
            entry.version
        );
    }
}

// ---------------------------------------------------------------------
// Certified versions must be provisionable, rows must be unique
// ---------------------------------------------------------------------

#[test]
fn certifying_an_unsupported_kubernetes_version_is_an_error() {
    // 1.34.11 is in `compatibility/kubernetes.yaml` with
    // `supported: false` — a real, once-valid version that is exactly
    // what a stale certification row would still name.
    let matrix = matrix(vec![entry("kyverno", "3.9.0", &["1.34.11"])]);
    let problems = validate_compatibility(&matrix, &checked_in_supported(), &checked_in_recipes());
    assert_one_problem(&problems, "is not a supported: true version");
}

#[test]
fn a_duplicate_combination_is_rejected() {
    let matrix = matrix(vec![entry("kyverno", "3.9.0", &["1.35.8", "1.35.8"])]);
    let problems = validate_compatibility(&matrix, &checked_in_supported(), &checked_in_recipes());
    assert_one_problem(&problems, "duplicate certified combination");
}

#[test]
fn a_duplicate_entry_for_one_recipe_version_is_rejected() {
    let matrix = matrix(vec![
        entry("kyverno", "3.9.0", &["1.35.8"]),
        entry("kyverno", "3.9.0", &["1.35.8"]),
    ]);
    let problems = validate_compatibility(&matrix, &checked_in_supported(), &checked_in_recipes());
    assert_eq!(problems.len(), 2, "{problems:#?}");
    assert!(
        problems
            .iter()
            .any(|problem| problem.message.contains("duplicate entry"))
    );
    assert!(
        problems
            .iter()
            .any(|problem| problem.message.contains("duplicate certified combination"))
    );
}

#[test]
fn an_entry_certifying_nothing_is_rejected() {
    let matrix = matrix(vec![entry("kyverno", "3.9.0", &[])]);
    let problems = validate_compatibility(&matrix, &checked_in_supported(), &checked_in_recipes());
    assert_one_problem(&problems, "certified is empty");
}

#[test]
fn every_problem_is_reported_at_once_rather_than_one_rerun_at_a_time() {
    let matrix = matrix(vec![
        entry("kyverno", "4.0.0", &["1.34.11"]),
        entry("not-a-recipe", "1.0.0", &[]),
    ]);
    let problems = validate_compatibility(&matrix, &checked_in_supported(), &checked_in_recipes());
    assert_eq!(problems.len(), 4, "{problems:#?}");
}

// ---------------------------------------------------------------------
// Tier vocabulary and the single-row narrowing a CI matrix job uses
// ---------------------------------------------------------------------

#[test]
fn tiers_order_from_most_to_least_frequent() {
    assert!(CertificationTier::PerCommit < CertificationTier::Nightly);
    assert!(CertificationTier::Nightly < CertificationTier::WeeklyRelease);
    assert_eq!(CertificationTier::PerCommit.as_str(), "perCommit");
    assert_eq!(CertificationTier::Nightly.as_str(), "nightly");
    assert_eq!(CertificationTier::WeeklyRelease.as_str(), "weeklyRelease");
    assert_eq!(
        CertificationTier::WeeklyRelease.to_string(),
        "weeklyRelease"
    );
}

#[test]
fn every_tier_spelling_in_the_file_parses_back_to_its_variant() {
    // The wire spellings are what `scripts/recipe-matrix.py` matches on
    // and what a workflow's own `tier:` input carries, so a rename in
    // either direction must fail here.
    let parsed: RecipeCompatibilityMatrix = serde_norway::from_str(
        "recipes:\n  \
         - name: a\n    version: \"1\"\n    kubernetes:\n      documentedRange: null\n      \
         certified:\n        \
         - version: \"1.35.8\"\n          tier: perCommit\n        \
         - version: \"1.36.4\"\n          tier: nightly\n        \
         - version: \"1.37.0\"\n          tier: weeklyRelease\n",
    )
    .expect("every documented tier spelling must parse");
    assert_eq!(
        parsed.recipes[0]
            .kubernetes
            .certified
            .iter()
            .map(|certified| certified.tier)
            .collect::<Vec<_>>(),
        vec![
            CertificationTier::PerCommit,
            CertificationTier::Nightly,
            CertificationTier::WeeklyRelease,
        ]
    );
}

#[test]
fn an_unknown_tier_spelling_is_a_parse_error_rather_than_a_default() {
    let error = serde_norway::from_str::<RecipeCompatibilityMatrix>(
        "recipes:\n  - name: a\n    version: \"1\"\n    kubernetes:\n      \
         documentedRange: null\n      certified:\n        - version: \"1.35.8\"\n          \
         tier: hourly\n",
    )
    .expect_err("an unknown tier must not silently become a default");
    assert!(
        error.to_string().contains("hourly"),
        "the parse error must name the bad tier: {error}"
    );
}

#[test]
fn narrowing_with_no_request_returns_every_certified_version() {
    let matrix = checked_in_matrix();
    let istio = matrix.entry("istio").expect("an istio entry");
    assert_eq!(
        narrow_certified_versions(istio, None).expect("no request narrows nothing"),
        vec!["1.35.8", "1.36.4", "1.37.0"]
    );
}

#[test]
fn narrowing_to_a_certified_version_returns_only_that_one() {
    let matrix = checked_in_matrix();
    let istio = matrix.entry("istio").expect("an istio entry");
    assert_eq!(
        narrow_certified_versions(istio, Some("1.37.0")).expect("1.37.0 is certified for istio"),
        vec!["1.37.0"]
    );
}

#[test]
fn narrowing_to_an_uncertified_version_fails_rather_than_running_nothing() {
    let matrix = checked_in_matrix();
    let kyverno = matrix.entry("kyverno").expect("a kyverno entry");
    let error = narrow_certified_versions(kyverno, Some("1.36.4"))
        .expect_err("a CI job pointed at an uncertified combination must fail loudly");
    assert_eq!(error.requested, "1.36.4");
    assert_eq!(error.certified, vec!["1.35.8".to_owned()]);
    let rendered = error.to_string();
    assert!(rendered.contains("kyverno 3.9.0"), "{rendered}");
    assert!(rendered.contains("1.36.4"), "{rendered}");
}
