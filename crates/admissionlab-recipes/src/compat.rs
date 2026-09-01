//! Reading `compatibility/recipes.yaml`: which Kubernetes patch
//! version(s) a built-in recipe is certified against, at which CI tier
//! each of those is proven, and the vendor-documented range (if any)
//! that certification was narrowed from.
//!
//! # Task 7.4 extends this module rather than adding a second one
//!
//! ROADMAP Task 7.4's file list says *create*
//! `crates/admissionlab-recipes/src/compatibility.rs`. It is
//! implemented here instead, because this module already exists and
//! already does exactly the job that file was to do: it owns
//! `compatibility/recipes.yaml`'s embedded bytes, its typed shape, and
//! its only loader. Two modules over one file would mean two
//! `include_str!`s of the same bytes, two deserializations that could
//! disagree about the shape, and a reader having to guess which is
//! authoritative. What Task 7.4 actually adds — [`CertificationTier`]
//! on every certified version, the flattened
//! [`CertifiedCombination`] view, and [`validate_compatibility`] — is an
//! extension of this file's subject, not a second subject.
//!
//! # This file exists to make one thing load-bearing that was not
//!
//! `compatibility/recipes.yaml` was introduced by Task 2.5 with **no
//! production consumer** — only `tests/load.rs` parsed it, through a
//! small struct private to that test file, so a bad hand-edit would
//! fail CI but nothing at runtime ever read the file at all. That
//! file's own header comment named this crate's eventual certification
//! tooling as the plausible first real reader.
//!
//! [`load_recipe_compatibility`] is that reader. [`kyverno_recipe.rs`
//! (the certification test)][crate] derives which Kubernetes version(s)
//! to install Kyverno against by calling [`RecipeCompatibilityMatrix::entry`]
//! on this module's output, rather than hardcoding `"1.35.8"` next to a
//! comment pointing at the file — so an edit to `certified` in
//! `compatibility/recipes.yaml` actually changes what that test does on
//! its next run, instead of silently drifting out of sync with a
//! hardcoded copy. A row nothing consumes is not a certification.
//!
//! # Embedded at compile time, mirroring every other checked-in matrix
//!
//! Exactly the same mechanism and reasoning
//! `admissionlab_cluster::version::load_matrix` already uses for
//! `compatibility/kubernetes.yaml`, and [`crate::load::load_builtin_recipes`]
//! uses for `recipes/*.yaml`: `include_str!` embeds this file's bytes
//! into the compiled binary, so which Kubernetes versions a recipe is
//! certified against can only change by editing this checked-in file and
//! rebuilding — never by anything observed at runtime. This module makes
//! no filesystem or network call of its own.
//!
//! # Shape mirrors `compatibility/recipes.yaml`'s own documented shape
//!
//! See that file's header comment for the authoritative description of
//! every field below; this module's types are a direct, `pub`
//! transcription of it, not a reinterpretation. [`DocumentedKubernetesRange`]
//! is `Option`-wrapped ([`KubernetesCompatibility::documented_range`])
//! because an entry with no vendor-stated window (Istio, as of this
//! writing) records that absence explicitly as `null` rather than
//! omitting the key — Global Constraint 15: missing data is
//! unavailable/unknown, never fabricated, and never silently
//! indistinguishable from "nobody filled this in yet."

use std::collections::BTreeSet;
use std::fmt;

use serde::Deserialize;
use thiserror::Error;

use crate::model::Recipe;

/// How many Kubernetes minor versions must be marked `supported: true`
/// in `compatibility/kubernetes.yaml` for a release candidate.
///
/// Global Constraint 10 and PRODUCT.md §32: Admission Lab targets "the
/// latest three upstream-supported Kubernetes minor versions at release
/// time". A Rust constant rather than a field in either checked-in YAML
/// file, deliberately — the number is a product decision, and a decision
/// nobody can edit in passing while changing something else is the point
/// of putting it here.
///
/// The roadmap's own exception ("unless upstream support window
/// temporarily differs and release notes explain it") is
/// [`SupportWindowException`], which *is* expressible in
/// `compatibility/recipes.yaml` — as a reviewable diff that must name
/// its own justification. See [`validate_compatibility`].
pub const RELEASE_SUPPORTED_MINORS: usize = 3;

/// `compatibility/recipes.yaml`'s contents, embedded into the binary at
/// compile time. The path is relative to *this source file*
/// (`crates/admissionlab-recipes/src/compat.rs`): three directories up
/// reaches the workspace root, then into `compatibility/` — the same
/// depth `crate::load`'s own `BUILTIN_RECIPES` `include_str!` paths use
/// from this same `src/` directory.
const COMPATIBILITY_RECIPES_YAML: &str = include_str!("../../../compatibility/recipes.yaml");

/// The full set of recipe-to-Kubernetes-version certifications recorded
/// in `compatibility/recipes.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct RecipeCompatibilityMatrix {
    /// The declared exception to [`RELEASE_SUPPORTED_MINORS`], or `None`
    /// (the normal case, and the shape of the file today) when the
    /// upstream support window is the ordinary three minors.
    ///
    /// Absent by default rather than explicitly `null`, unlike
    /// [`KubernetesCompatibility::documented_range`]: the two `None`s
    /// mean different things. An absent vendor support statement is a
    /// *fact about the world* that a reader must be able to tell apart
    /// from an unfilled field (Global Constraint 15), so it is written
    /// out. An absent exception is the absence of a *decision this
    /// project made*, which needs no such distinction — nobody can
    /// forget to declare an exception they never took.
    #[serde(default)]
    pub support_window_exception: Option<SupportWindowException>,
    /// Every recorded entry, in the order written in the source file.
    /// More than one entry may share a [`RecipeCompatibilityEntry::name`]
    /// over time, as `compatibility/recipes.yaml`'s own header comment
    /// documents (a new pinned version is appended, never edited in
    /// place) — [`RecipeCompatibilityMatrix::entry`] returns the first
    /// match, but nothing in this workspace has needed to disambiguate
    /// further yet.
    pub recipes: Vec<RecipeCompatibilityEntry>,
}

impl RecipeCompatibilityMatrix {
    /// Finds the entry whose [`RecipeCompatibilityEntry::name`] equals
    /// `name`, if any.
    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&RecipeCompatibilityEntry> {
        self.recipes.iter().find(|entry| entry.name == name)
    }

    /// Every certified combination this matrix records, flattened into
    /// one row per (Kubernetes version, recipe, recipe version) — the
    /// shape a CI job matrix and a lookup both want, and the shape
    /// ROADMAP Task 7.4 freezes as [`CertifiedCombination`].
    ///
    /// Order is the source file's own: entries in the order written,
    /// and within an entry, certified versions in the order written.
    /// Deterministic, so a generated CI matrix does not reorder itself
    /// between runs over an unchanged file.
    #[must_use]
    pub fn certified_combinations(&self) -> Vec<CertifiedCombination> {
        self.recipes
            .iter()
            .flat_map(|entry| {
                entry
                    .kubernetes
                    .certified
                    .iter()
                    .map(move |certified| CertifiedCombination {
                        kubernetes: certified.version.clone(),
                        recipe: entry.name.clone(),
                        recipe_version: entry.version.clone(),
                        tier: certified.tier,
                    })
            })
            .collect()
    }

    /// Whether this matrix certifies `recipe` at `recipe_version` on
    /// Kubernetes `kubernetes`.
    ///
    /// The question `admissionlab test` asks before warning about an
    /// uncertified stack — and it is only ever a *warning*: Global
    /// Constraint 6 makes generic, user-defined stacks first-class, so
    /// nothing in this workspace may refuse a run over this answer.
    #[must_use]
    pub fn certifies(&self, recipe: &str, recipe_version: &str, kubernetes: &str) -> bool {
        self.recipes.iter().any(|entry| {
            entry.name == recipe
                && entry.version == recipe_version
                && entry
                    .kubernetes
                    .certified
                    .iter()
                    .any(|certified| certified.version == kubernetes)
        })
    }
}

/// A declared, checked-in exception to [`RELEASE_SUPPORTED_MINORS`].
///
/// ROADMAP Task 7.4 step 1 allows the supported-minor count to differ
/// from three "unless upstream support window temporarily differs and
/// release notes explain it." This type is that escape hatch, and its
/// three required fields are what make it an *explanation* rather than
/// a bypass: the count being claimed, why upstream differs, and where
/// the release notes say so.
///
/// It is a field of a checked-in file rather than a flag or an
/// environment variable on purpose. Dropping (or gaining) a supported
/// Kubernetes minor must be a deliberate, reviewable file change — the
/// same rule `compatibility/kubernetes.yaml`'s own header states for
/// itself, and the same reason Global Constraint 10 exists at all.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupportWindowException {
    /// How many `supported: true` minors `compatibility/kubernetes.yaml`
    /// is expected to carry while this exception stands. Must differ
    /// from [`RELEASE_SUPPORTED_MINORS`] — an exception that restates
    /// the default is a stale one, and [`validate_compatibility`] says
    /// so rather than letting it sit unnoticed.
    pub expected_supported_minors: usize,
    /// What upstream did, in the author's own words. Never empty.
    pub reason: String,
    /// Where the release notes explaining this exception live — a
    /// repository path or a URL. Never empty.
    pub release_notes: String,
}

/// One certified (Kubernetes version, recipe, recipe version)
/// combination, with the CI tier that proves it.
///
/// The flattened view of `compatibility/recipes.yaml`, frozen by
/// ROADMAP Task 7.4. The file itself groups certified versions under
/// their recipe (which is how a human reads and edits it); this is the
/// same information one row at a time, which is how a CI matrix and a
/// lookup both consume it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CertifiedCombination {
    /// The exact Kubernetes patch version, for example `"1.36.4"` —
    /// always one of `compatibility/kubernetes.yaml`'s own
    /// `supported: true` entries.
    pub kubernetes: String,
    /// The recipe's name, matching [`Recipe::name`].
    pub recipe: String,
    /// The recipe's pinned version, matching [`Recipe::version`].
    pub recipe_version: String,
    /// The most frequent schedule that runs this combination.
    pub tier: CertificationTier,
}

/// How often a [`CertifiedCombination`] is actually exercised in CI.
///
/// PRODUCT.md §32 and ROADMAP Task 7.5's three tiers. The variants are
/// ordered from most to least frequent, and that order is the derived
/// [`Ord`]: tiers are **cumulative downward**, so a tier's CI job runs
/// every combination whose tier is less than or equal to its own
/// (`tier <= Nightly` is exactly Tier 2's job matrix). Each combination
/// is therefore written down once, at its cheapest schedule, rather than
/// repeated in every tier that runs it.
///
/// A tier says nothing about confidence. Every certified combination
/// has been installed and verified by a real test in this repository;
/// the tier records only how often that verification is repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CertificationTier {
    /// Tier 1. Runs on every commit that touches what it covers.
    PerCommit,
    /// Tier 2. Runs in `.github/workflows/nightly.yml`.
    Nightly,
    /// Tier 3. Runs weekly, on manual dispatch, and for a release
    /// candidate.
    WeeklyRelease,
}

impl CertificationTier {
    /// This tier's wire spelling in `compatibility/recipes.yaml`, and
    /// the string `scripts/recipe-matrix.py` matches on.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PerCommit => "perCommit",
            Self::Nightly => "nightly",
            Self::WeeklyRelease => "weeklyRelease",
        }
    }
}

impl fmt::Display for CertificationTier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One recipe's recorded Kubernetes-version certification.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecipeCompatibilityEntry {
    /// The recipe's name — matches [`crate::Recipe::name`] (for example
    /// `"kyverno"`).
    pub name: String,
    /// The recipe's own pinned version — matches
    /// [`crate::Recipe::version`].
    pub version: String,
    /// This recipe version's Kubernetes compatibility.
    pub kubernetes: KubernetesCompatibility,
}

/// The Kubernetes-version facts recorded for one
/// [`RecipeCompatibilityEntry`]: the vendor's own documented support
/// window (if any was found), and the exact certified set derived from
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubernetesCompatibility {
    /// The vendor-documented Kubernetes minor-version window, at
    /// whatever granularity the vendor itself states it. `None` — always
    /// an explicit `null` in the source YAML, never an omitted key —
    /// means no vendor statement was found, not "supported" and not
    /// "unsupported" (Global Constraint 15).
    #[serde(default)]
    pub documented_range: Option<DocumentedKubernetesRange>,
    /// The exact Kubernetes patch version(s) this recipe version is
    /// certified against: a subset of `compatibility/kubernetes.yaml`'s
    /// own `supported: true` entries, hand-reviewed and checked in here
    /// (never computed at runtime). This is the field
    /// `kyverno_recipe.rs` reads to decide which Kubernetes version to
    /// provision its cluster at — see this module's own documentation.
    pub certified: Vec<CertifiedKubernetes>,
}

impl KubernetesCompatibility {
    /// Just the certified Kubernetes versions, in the order written.
    ///
    /// The shape every caller wanted before Task 7.4 added
    /// [`CertifiedKubernetes::tier`], kept as a method so that a
    /// certification test choosing which cluster to create does not have
    /// to name a field it has no opinion about.
    #[must_use]
    pub fn certified_versions(&self) -> Vec<&str> {
        self.certified
            .iter()
            .map(|certified| certified.version.as_str())
            .collect()
    }
}

/// One certified Kubernetes patch version, and the CI tier that proves
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CertifiedKubernetes {
    /// The exact Kubernetes patch version, for example `"1.36.4"`.
    pub version: String,
    /// How often this combination is actually exercised in CI.
    pub tier: CertificationTier,
}

/// A vendor-documented Kubernetes minor-version window, for example
/// `min: "1.33", max: "1.35"`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentedKubernetesRange {
    /// The lowest Kubernetes minor version the vendor states support
    /// for, for example `"1.33"`.
    pub min: String,
    /// The highest Kubernetes minor version the vendor states support
    /// for, for example `"1.35"`.
    pub max: String,
}

/// `compatibility/recipes.yaml` is not valid YAML matching
/// [`RecipeCompatibilityMatrix`]'s shape.
///
/// In practice this can only follow a bad hand-edit to that checked-in
/// file — the embedded copy [`load_recipe_compatibility`] reads never
/// changes at runtime — and `tests/load.rs` catches it directly against
/// the real file, the same way `admissionlab_cluster::version::VersionError::Malformed`
/// is caught for `compatibility/kubernetes.yaml`.
#[derive(Debug, Error)]
pub enum RecipeCompatibilityError {
    /// The underlying YAML parse failure.
    #[error("failed to parse compatibility/recipes.yaml: {source}")]
    Malformed {
        /// The underlying parse failure.
        #[source]
        source: serde_norway::Error,
    },
}

/// Loads the checked-in [`RecipeCompatibilityMatrix`] embedded into this
/// binary at compile time from `compatibility/recipes.yaml`. See this
/// module's documentation for why this is embedded rather than read
/// from disk or fetched at runtime.
///
/// # Errors
///
/// Returns [`RecipeCompatibilityError::Malformed`] if the embedded
/// `compatibility/recipes.yaml` is not valid YAML matching
/// [`RecipeCompatibilityMatrix`]'s shape.
pub fn load_recipe_compatibility() -> Result<RecipeCompatibilityMatrix, RecipeCompatibilityError> {
    serde_norway::from_str(COMPATIBILITY_RECIPES_YAML)
        .map_err(|source| RecipeCompatibilityError::Malformed { source })
}

/// The environment variable a certification test reads to run against
/// exactly one of its certified Kubernetes versions instead of all of
/// them.
///
/// # Why this exists
///
/// The recipe certification tests each loop over their entry's whole
/// `certified` list, creating one disposable cluster per version. That
/// is the right shape for a developer running one test by hand, and the
/// wrong shape for `.github/workflows/recipe-matrix.yml`, whose matrix
/// is one job per [`CertifiedCombination`]: without a way to say "this
/// row only", a Tier-1 job would install every certified version of its
/// recipe and the tier assignments in `compatibility/recipes.yaml`
/// would describe nothing real.
///
/// Unset — every developer, and every workflow that predates the
/// generated matrix — means "all certified versions," exactly as before.
/// Reading the variable is the *test's* job, never this crate's: nothing
/// in `admissionlab-recipes` observes its environment (see this module's
/// "Embedded at compile time"), so tests pass what they read to
/// [`narrow_certified_versions`], which is pure.
pub const CERTIFY_KUBERNETES_ENV: &str = "ADMISSIONLAB_CERTIFY_KUBERNETES";

/// Narrows `entry`'s certified Kubernetes versions to `requested`, or
/// returns all of them when `requested` is `None`.
///
/// See [`CERTIFY_KUBERNETES_ENV`] for what asks for a single version and
/// why.
///
/// # Errors
///
/// Returns [`UncertifiedSelection`] when `requested` names a version
/// this entry does not certify. Deliberately an error rather than an
/// empty result: a CI job told to certify a combination that is not in
/// the matrix must fail loudly, not pass by running nothing.
pub fn narrow_certified_versions<'a>(
    entry: &'a RecipeCompatibilityEntry,
    requested: Option<&str>,
) -> Result<Vec<&'a str>, UncertifiedSelection> {
    let all = entry.kubernetes.certified_versions();
    let Some(requested) = requested else {
        return Ok(all);
    };
    match all.iter().find(|version| **version == requested) {
        Some(found) => Ok(vec![*found]),
        None => Err(UncertifiedSelection {
            recipe: entry.name.clone(),
            recipe_version: entry.version.clone(),
            requested: requested.to_owned(),
            certified: all.iter().map(|version| (*version).to_owned()).collect(),
        }),
    }
}

/// [`CERTIFY_KUBERNETES_ENV`] asked for a Kubernetes version an entry
/// does not certify.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "{recipe} {recipe_version} is not certified on Kubernetes {requested} in \
     compatibility/recipes.yaml (certified: {certified:?}); refusing to run zero certifications \
     and report a pass"
)]
pub struct UncertifiedSelection {
    /// The recipe whose entry was consulted.
    pub recipe: String,
    /// That entry's pinned recipe version.
    pub recipe_version: String,
    /// The Kubernetes version that was asked for.
    pub requested: String,
    /// What the entry actually certifies.
    pub certified: Vec<String>,
}

/// One Kubernetes release Admission Lab itself provisions: a
/// `supported: true` entry of `compatibility/kubernetes.yaml`.
///
/// # Why this is a local type rather than `admissionlab_cluster::KubernetesImage`
///
/// `admissionlab-cluster` is where `compatibility/kubernetes.yaml`'s
/// loader lives, and this crate does **not** depend on it (nor on
/// `admissionlab-core`, which it would drag in). That is not an
/// accident to be worked around: this crate's whole point is that it
/// carries recipe metadata and no orchestration, and its dependency
/// list is one of the two mechanisms `lib.rs` names as enforcing Global
/// Constraint 6.
///
/// So [`validate_compatibility`] takes the supported set as data
/// instead. The caller — `tests/compatibility.rs` and
/// `admissionlab-cli`, both of which already have
/// `admissionlab-cluster` — converts in one `filter`/`map`. The
/// alternative, re-`include_str!`ing `compatibility/kubernetes.yaml`
/// here, would put a second parser for that file in the workspace, and
/// two parsers for one support matrix is exactly the drift this
/// validation exists to catch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportedKubernetes {
    /// The minor version, for example `"1.36"`.
    pub minor: String,
    /// The exact patch version, for example `"1.36.4"`.
    pub version: String,
}

/// One thing wrong with the checked-in compatibility metadata.
///
/// `locator`/`message` deliberately mirror
/// `admissionlab_policy::validate_policy_spec`'s own problem shape, and
/// for the same reason: several independent problems in one checked-in
/// file should all be reported at once, each naming where it is, rather
/// than one rerun at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityProblem {
    /// Where the problem is — a file path, optionally with the entry
    /// inside it, for example
    /// `"compatibility/recipes.yaml: kyverno 3.9.0"`.
    pub locator: String,
    /// What is wrong, and what would fix it.
    pub message: String,
}

/// Validates the checked-in compatibility metadata against the supported
/// Kubernetes matrix and the recipes that actually exist.
///
/// Returns every problem found, in a deterministic order; an empty
/// vector means the metadata is valid. Never `Result`, for the reason
/// [`CompatibilityProblem`] documents.
///
/// The five rules, and what each one prevents:
///
/// 1. **Exactly [`RELEASE_SUPPORTED_MINORS`] supported minors** in
///    `supported` (ROADMAP Task 7.4 step 1) — unless
///    [`RecipeCompatibilityMatrix::support_window_exception`] declares a
///    different count, names why upstream differs, and cites the release
///    notes that explain it. An exception that restates the default is
///    itself reported, so a temporary exception cannot quietly become
///    permanent. This is what stops a release candidate shipping with a
///    dropped or a silently added minor.
/// 2. **Every certified Kubernetes version is a supported one.** A row
///    certifying a version Admission Lab cannot even provision is a
///    claim about a cluster nobody can create; it is also exactly what a
///    support drop leaves behind if `compatibility/kubernetes.yaml` is
///    edited without this file.
/// 3. **Every entry names known pinned install metadata** (step 2): some
///    loaded [`Recipe`] must have that exact `name` *and* that exact
///    `version`. A matrix row naming a version no recipe pins certifies
///    something this repository cannot install — the drift that happens
///    when a recipe's chart pin is bumped and this file is not.
/// 4. **No duplicate combination.** Two rows for one (Kubernetes,
///    recipe, recipe version) triple would run the same certification
///    twice in CI and would let two different tiers claim one
///    combination.
/// 5. **No empty certified set,** and no duplicate `(name, version)`
///    entry. An entry certifying nothing is a recipe that looks covered
///    in this file and is covered by no job at all.
///
/// `recipes` is normally [`crate::load::load_builtin_recipes`]'s output.
/// A caller that has also loaded overrides may pass those too; the rule
/// is only ever "some loaded recipe pins this", never "the built-in one
/// does".
#[must_use]
pub fn validate_compatibility(
    matrix: &RecipeCompatibilityMatrix,
    supported: &[SupportedKubernetes],
    recipes: &[Recipe],
) -> Vec<CompatibilityProblem> {
    let mut problems = Vec::new();
    validate_support_window(matrix, supported, &mut problems);

    let supported_versions: BTreeSet<&str> = supported
        .iter()
        .map(|release| release.version.as_str())
        .collect();

    let mut seen_entries: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut seen_combinations: BTreeSet<(&str, &str, &str)> = BTreeSet::new();

    for entry in &matrix.recipes {
        let locator = format!(
            "compatibility/recipes.yaml: {} {}",
            entry.name, entry.version
        );

        if !seen_entries.insert((entry.name.as_str(), entry.version.as_str())) {
            problems.push(CompatibilityProblem {
                locator: locator.clone(),
                message: "duplicate entry: this recipe and version are already recorded above. \
                          Merge the two entries' certified versions into one."
                    .to_owned(),
            });
        }

        if !recipes
            .iter()
            .any(|recipe| recipe.name == entry.name && recipe.version == entry.version)
        {
            problems.push(CompatibilityProblem {
                locator: locator.clone(),
                message: format!(
                    "no loaded recipe pins {} at version {}, so this entry certifies install \
                     metadata that does not exist. Loaded: {}",
                    entry.name,
                    entry.version,
                    describe_recipes(recipes)
                ),
            });
        }

        if entry.kubernetes.certified.is_empty() {
            problems.push(CompatibilityProblem {
                locator: locator.clone(),
                message: "certified is empty: this entry claims a recipe is covered while no CI \
                          tier runs it. Remove the entry, or certify a version."
                    .to_owned(),
            });
        }

        for certified in &entry.kubernetes.certified {
            if !supported_versions.contains(certified.version.as_str()) {
                problems.push(CompatibilityProblem {
                    locator: locator.clone(),
                    message: format!(
                        "certified Kubernetes {} is not a supported: true version in \
                         compatibility/kubernetes.yaml (supported: {}). Certifying a version \
                         Admission Lab cannot provision claims a cluster nobody can create.",
                        certified.version,
                        join(supported_versions.iter().copied()),
                    ),
                });
            }
            if !seen_combinations.insert((
                certified.version.as_str(),
                entry.name.as_str(),
                entry.version.as_str(),
            )) {
                problems.push(CompatibilityProblem {
                    locator: locator.clone(),
                    message: format!(
                        "duplicate certified combination: {} {} on Kubernetes {} is recorded more \
                         than once, so two tiers could each claim it.",
                        entry.name, entry.version, certified.version,
                    ),
                });
            }
        }
    }

    problems
}

/// Rule 1 of [`validate_compatibility`]: the supported-minor count, and
/// the declared exception to it.
fn validate_support_window(
    matrix: &RecipeCompatibilityMatrix,
    supported: &[SupportedKubernetes],
    problems: &mut Vec<CompatibilityProblem>,
) {
    // Counted over distinct minors, not entries: two patch releases of
    // one minor marked supported would still be one supported minor,
    // and Global Constraint 10 counts minors.
    let minors: BTreeSet<&str> = supported
        .iter()
        .map(|release| release.minor.as_str())
        .collect();

    let expected = match &matrix.support_window_exception {
        None => RELEASE_SUPPORTED_MINORS,
        Some(exception) => {
            let locator = "compatibility/recipes.yaml: supportWindowException".to_owned();
            if exception.expected_supported_minors == RELEASE_SUPPORTED_MINORS {
                problems.push(CompatibilityProblem {
                    locator: locator.clone(),
                    message: format!(
                        "this exception expects {RELEASE_SUPPORTED_MINORS} supported minors, which \
                         is the ordinary rule — so it grants nothing and is stale. Delete it."
                    ),
                });
            }
            if exception.reason.trim().is_empty() {
                problems.push(CompatibilityProblem {
                    locator: locator.clone(),
                    message: "reason is empty: an exception to Global Constraint 10 must say what \
                              upstream did."
                        .to_owned(),
                });
            }
            if exception.release_notes.trim().is_empty() {
                problems.push(CompatibilityProblem {
                    locator,
                    message:
                        "releaseNotes is empty: the roadmap allows a differing support window \
                              only when release notes explain it, so this must name where they do."
                            .to_owned(),
                });
            }
            exception.expected_supported_minors
        }
    };

    if minors.len() != expected {
        let exception_note = if matrix.support_window_exception.is_some() {
            " (the count this file's supportWindowException declares)"
        } else {
            " (Global Constraint 10; declare a supportWindowException in \
             compatibility/recipes.yaml if upstream's own window temporarily differs)"
        };
        problems.push(CompatibilityProblem {
            locator: "compatibility/kubernetes.yaml".to_owned(),
            message: format!(
                "{} Kubernetes minor(s) are marked supported: true ({}), but a release candidate \
                 must support exactly {expected}{exception_note}.",
                minors.len(),
                join(minors.iter().copied()),
            ),
        });
    }
}

/// `name version` for every loaded recipe, for a problem message that
/// has just said one of them is missing.
fn describe_recipes(recipes: &[Recipe]) -> String {
    if recipes.is_empty() {
        return "no recipes were loaded at all".to_owned();
    }
    join(
        recipes
            .iter()
            .map(|recipe| format!("{} {}", recipe.name, recipe.version)),
    )
}

/// `a, b, c` — one place, so every message in this module punctuates a
/// list the same way.
fn join<T: fmt::Display>(items: impl Iterator<Item = T>) -> String {
    items
        .map(|item| item.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
