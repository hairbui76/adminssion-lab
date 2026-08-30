#![forbid(unsafe_code)]
//! Vendor-neutral recipe metadata: curated, convenience installation
//! metadata for a known admission-stack component (Kyverno, Istio, and
//! so on) — pinned versions, readiness checks, harmless response
//! normalization rules, capability metadata, and (recorded in
//! `compatibility/recipes.yaml`, read through this crate's own
//! [`compat`] module — see that file's own header comments for what it
//! records and why) compatibility-test metadata. See PRODUCT.md §14
//! ("Recipe Model").
//!
//! # A recipe never contains regression-classification logic
//!
//! PRODUCT.md §14 states this in one sentence: "Recipes must not contain
//! regression-classification business logic." Global Constraint 6 says
//! the same: the core is vendor-neutral, and a recipe may provide
//! install/readiness/normalization/capability metadata, but never
//! classification logic. If a vendor could ship a recipe that decides
//! what counts as a regression, the engine stops being vendor-neutral
//! and its results stop being trustworthy.
//!
//! This is enforced two ways:
//!
//! - **Structurally, by the dependency graph.** This crate depends on
//!   neither `admissionlab-diff` nor `admissionlab-policy` — the crates
//!   that actually decide what counts as a regression — and cannot reach
//!   either transitively (verify with `cargo metadata --no-deps`, not by
//!   grepping `Cargo.toml`; see `Cargo.toml`'s own comment on the
//!   `admissionlab-spec` dependency). A recipe's Rust representation
//!   simply has no vocabulary capable of expressing "this difference is
//!   a regression" or "this difference is not," because the crate that
//!   defines that vocabulary is unreachable from here.
//! - **By construction, in the recipe schema itself.** See
//!   [`model`]'s own module documentation for the full mechanism: every
//!   raw recipe field is drawn from an explicit allow-list
//!   (`#[serde(deny_unknown_fields)]` at every nesting level, plus
//!   closed enums for readiness checks, normalize rules, and
//!   capabilities), so `failOn`, `severity`, or any other
//!   classification-shaped key — not only the two PRODUCT.md §14 names
//!   as examples — fails to parse at all, rather than silently being
//!   accepted and ignored (or worse, honored).
//!
//! # The resolved vocabulary lives in `admissionlab-spec`, not here
//!
//! [`Recipe`] references [`InstallMethod`], [`ReadinessCheck`],
//! [`RecipeNormalizeRule`], and [`Capability`] directly rather than
//! redefining them. Controller Ruling R30 (Task 2.5): those types
//! already live in `admissionlab-spec`, because
//! `admissionlab_spec::ResolvedComponent::capabilities` references
//! [`Capability`] and `admissionlab-spec` must stay a leaf crate — moving
//! them here (or duplicating them) would either close a cycle the moment
//! `admissionlab_spec::resolve_lab` needed to produce a component's
//! capabilities, or fork the vocabulary into two incompatible copies
//! across the workspace. They are re-exported at this crate's root
//! purely for caller convenience: a consumer of [`Recipe`] does not need
//! its own direct `admissionlab-spec` dependency merely to name the
//! types [`Recipe`]'s own fields use.
//!
//! What this crate *does* define: the raw YAML schema a recipe author
//! writes and the strict validation that turns it into a [`Recipe`]
//! ([`model`]), the string vocabulary a recipe's `capabilities:` list
//! uses — recipe-specific *logic*, not the [`Capability`] enum itself —
//! ([`capability`]), and how a [`Recipe`] is actually obtained
//! ([`load`]): the built-in set embedded into the binary at compile
//! time, and an optional, explicitly-opted-into local override
//! directory. See [`load`]'s module documentation for both design
//! decisions and why each is deliberate.
//!
//! Stack installation orchestration (Task 2.6) that actually drives a
//! resolved [`Recipe`]'s [`Recipe::install`]/[`Recipe::readiness`] lives
//! in `admissionlab-installer`, not here. The certified Kyverno recipe
//! (Task 2.8) is this crate's first real built-in content — see
//! [`load`]'s own `BUILTIN_RECIPES`; Istio's (Task 2.9) is not yet
//! implemented.

mod capability;
pub mod compat;
pub mod load;
pub mod model;

pub use admissionlab_spec::component::HelmInstallSpec;
pub use admissionlab_spec::{
    Capability, InstallMethod, ManifestInstallSpec, ReadinessCheck, RecipeNormalizeRule,
};
pub use compat::{
    DocumentedKubernetesRange, KubernetesCompatibility, RecipeCompatibilityEntry,
    RecipeCompatibilityError, RecipeCompatibilityMatrix, load_recipe_compatibility,
};
pub use load::{load_builtin_recipes, load_recipe_overrides, load_recipes};
pub use model::{Recipe, RecipeError};
