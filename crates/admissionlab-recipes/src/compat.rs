//! Reading `compatibility/recipes.yaml`: which Kubernetes patch
//! version(s) a built-in recipe is certified against, and the
//! vendor-documented range (if any) that certification was narrowed
//! from.
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

use serde::Deserialize;
use thiserror::Error;

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
pub struct RecipeCompatibilityMatrix {
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
    pub certified: Vec<String>,
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
