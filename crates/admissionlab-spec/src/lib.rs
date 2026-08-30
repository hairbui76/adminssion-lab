#![forbid(unsafe_code)]
//! The `v1alpha1` `admissionlab.yaml` configuration contract: the model
//! users hand-write, a strict loader, path/semantic resolution, and JSON
//! Schema generation for editor completion and validation.
//!
//! - [`model`] defines the raw, as-written shape ([`LabSpec`] and
//!   everything it references).
//! - [`component`] defines the resolved component/install/readiness
//!   vocabulary ([`ResolvedComponent`], [`InstallMethod`],
//!   [`ReadinessCheck`], [`RecipeNormalizeRule`], [`Capability`]) that
//!   [`resolve_lab`] converts each [`ComponentSpec`] into.
//! - [`load_lab`] reads and strictly parses a configuration file.
//! - [`resolve_lab`] validates a loaded configuration and resolves every
//!   relative path inside it against that file's own directory.
//! - [`v1alpha1_json_schema`] generates the JSON Schema checked in at
//!   `schemas/admissionlab-v1alpha1.json`.
//!
//! This crate defines the configuration *contract* only. It does not
//! implement recipe resolution, installers, fixture discovery, policy
//! evaluation, or expectations loading — those belong to the later tasks
//! that own each concern, some of which extend types defined here (see
//! each type's documentation for what is and is not this crate's
//! responsibility).
//!
//! # Two `HelmInstallSpec`s
//!
//! [`model::HelmInstallSpec`] (the raw, user-facing YAML shape) and
//! [`component::HelmInstallSpec`] (the resolved shape [`resolve_lab`]
//! produces) are two distinct types that share a name by design — see
//! [`component`]'s module documentation. A single `pub use` list cannot
//! re-export two same-named items under the same bare name, so only the
//! raw one is flattened to this crate's root below, exactly as it always
//! has been; reach the resolved one via `component::HelmInstallSpec`.
//! [`model::ManifestsInstallSpec`] and [`component::ManifestInstallSpec`]
//! do *not* collide (`Manifests` versus `Manifest`), so the resolved one
//! is flattened to the root like everything else.

pub mod component;
pub mod error;
pub mod load;
pub mod model;
pub mod resolve;
pub mod schema;
mod validate;

pub use component::{
    Capability, InstallMethod, ManifestInstallSpec, ReadinessCheck, RecipeNormalizeRule,
    ResolvedComponent,
};
pub use error::SpecError;
pub use load::load_lab;
pub use model::{
    ComponentSpec, EnvironmentSpec, FixtureSelectionSpec, GatewaySuiteSpec, HelmInstallSpec,
    InstallMethodSpec, LabSpec, LatencyPolicy, ManifestsInstallSpec, MigrationSuiteSpec,
    PolicyOverrideSpec, PolicySpec,
};
pub use resolve::{
    LoadedLab, ResolvedEnvironment, ResolvedFixtureSelection, ResolvedLab, resolve_lab,
};
pub use schema::v1alpha1_json_schema;
