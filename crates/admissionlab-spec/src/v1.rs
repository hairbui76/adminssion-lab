//! The **stable** `admissionlab.io/v1` lab document: the v1.0
//! configuration contract ROADMAP Task 9.1 freezes.
//!
//! # What the stable freeze commits this project to
//!
//! Everything the Beta freeze committed to (`migrate.rs` holds the
//! field-by-field record), plus one clause it did not have: within
//! `v1.x`, an existing field's *meaning* can never change either, and a
//! removal or a rename needs a `v2` — not merely "a new `apiVersion`".
//! The full rule, with the test that pins each clause, is
//! `docs/schema-migrations.md`'s "The stable-schema rule".
//!
//! # Zero wire changes from `v1beta1` — and what that buys
//!
//! Task 9.1 Step 1 re-audited every public Beta field for necessity and
//! naming consistency (the table is in `docs/schema-migrations.md`'s
//! `v1beta1 -> v1` migration note). Nothing was renamed and nothing was
//! removed, because the Beta freeze had already done that review once,
//! deliberately, and a second rename round would have cost every existing
//! user a rewrite to buy nothing. **A `v1` document is therefore a
//! `v1beta1` document with one line changed: its `apiVersion`.**
//!
//! That is why this module is short. [`V1Lab`] is a distinct Rust type —
//! it has to be, because the generated schema `const`-locks `apiVersion`
//! and takes its `title` from the Rust type name, so
//! `schemas/admissionlab-v1.json` and `schemas/admissionlab-v1beta1.json`
//! are genuinely different published artifacts — but every type *inside*
//! it is the very same type [`crate::v1beta1`] declares, re-exported
//! below rather than copied. That is the same rule [`crate::model`]
//! already applies to types every supported version spells identically,
//! applied one version later: a copy could drift from its twin, and
//! `migrate_v1beta1_to_v1` would then have real work to do and a real way
//! to get it wrong. Sharing makes the migration a field-for-field move
//! that the compiler checks.
//!
//! # This is the version the rest of the workspace sees
//!
//! [`crate::ResolvedLab`] is still version-independent by construction,
//! and the pivot every supported document is migrated *to* is now this
//! one: `v1alpha1 -> v1beta1 -> v1 -> resolve`. There is still exactly
//! one resolver and one resolved shape, and no crate above this one ever
//! names an `apiVersion`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::model::{EnvironmentSpec, FixtureSelectionSpec, kind_schema};

pub use crate::v1beta1::{
    GatewaySuiteSpec, LatencyPolicy, MigrationCaseSpec, MigrationSideSpec, MigrationSuiteSpec,
    NonPortableFeatureExpectation, PolicySpec,
};

/// The `apiVersion` of a stable lab document, and the value
/// [`crate::load_any_supported_lab`] accepts without migrating first.
pub const API_VERSION: &str = "admissionlab.io/v1";

/// Schema-only override for [`V1Lab::api_version`]: a JSON Schema `const`
/// locking the property to [`API_VERSION`], so an editor validating a
/// YAML file against the generated schema flags a wrong `apiVersion`
/// directly instead of only failing at load time. Does not affect
/// deserialization — the field's Rust type stays `String` and
/// [`crate::load_any_supported_lab`] still performs the authoritative
/// runtime check.
fn api_version_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "const": API_VERSION
    })
}

/// The root of a stable `admissionlab.yaml` configuration file.
///
/// Field for field, type for type, and default for default, this is
/// [`crate::V1Beta1Lab`] — see this module's "Zero wire changes from
/// `v1beta1`". The two differ in exactly one place, and it is the one
/// place they have to: the `apiVersion` a document must carry to be
/// parsed by this model.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct V1Lab {
    /// Must equal [`API_VERSION`]; checked by
    /// [`crate::load_any_supported_lab`] and `const`-locked in the
    /// generated schema (see [`api_version_schema`]).
    #[schemars(schema_with = "api_version_schema")]
    pub api_version: String,
    /// Must equal [`crate::model::KIND`]; checked by
    /// [`crate::load_any_supported_lab`] and `const`-locked in the
    /// generated schema (see [`crate::model::kind_schema`]).
    #[schemars(schema_with = "kind_schema")]
    pub kind: String,
    /// The unmodified stack being compared against.
    pub baseline: EnvironmentSpec,
    /// The stack under test for regressions.
    pub candidate: EnvironmentSpec,
    /// Which fixtures to replay through both environments.
    pub fixtures: FixtureSelectionSpec,
    /// Regression policy. Omit entirely to accept every field's default
    /// (see [`PolicySpec`]'s `Default` impl).
    #[serde(default)]
    pub policy: PolicySpec,
    /// Path to an expectations file, resolved by [`crate::resolve_lab`]
    /// against this configuration file's own directory.
    ///
    /// The `Expectations` document versions independently of this one and
    /// is still `admissionlab.io/v1alpha1`; promoting the lab document
    /// did not promote it (see `docs/schema-migrations.md`).
    #[serde(default)]
    pub expectations_file: Option<PathBuf>,
    /// The Gateway behavior suite. Omit the section entirely for an
    /// admission-only lab — [`crate::ResolvedLab::gateway`] is then
    /// `None`.
    #[serde(default)]
    pub gateway: Option<GatewaySuiteSpec>,
    /// The Ingress-to-Gateway migration suite. Omit the section entirely
    /// — as every lab that is not migrating off `Ingress` does — and
    /// [`crate::ResolvedLab::migration`] is `None`.
    ///
    /// Part of the stable contract, not an experiment carried into it:
    /// the section landed in `v1beta1` after that version's freeze, as
    /// the additive change the Beta rule allowed, and Task 9.1's field
    /// audit reviewed it on the same terms as every field that predates
    /// it.
    #[serde(default)]
    pub migration: Option<MigrationSuiteSpec>,
}
