#![forbid(unsafe_code)]
//! The `v1alpha1` `admissionlab.yaml` configuration contract: the model
//! users hand-write, a strict loader, and path/semantic resolution.
//!
//! - [`model`] defines the raw, as-written shape ([`LabSpec`] and
//!   everything it references).
//! - [`load_lab`] reads and strictly parses a configuration file.
//! - [`resolve_lab`] validates a loaded configuration and resolves every
//!   relative path inside it against that file's own directory.
//!
//! This crate defines the configuration *contract* only. It does not
//! implement recipe resolution, installers, fixture discovery, policy
//! evaluation, or expectations loading — those belong to the later tasks
//! that own each concern, some of which extend types defined here (see
//! each type's documentation for what is and is not this crate's
//! responsibility).

pub mod error;
pub mod load;
pub mod model;
pub mod resolve;
mod validate;

pub use error::SpecError;
pub use load::load_lab;
pub use model::{
    ComponentSpec, EnvironmentSpec, FixtureSelectionSpec, GatewaySuiteSpec, HelmInstallSpec,
    InstallMethodSpec, LabSpec, LatencyPolicy, ManifestsInstallSpec, MigrationSuiteSpec,
    PolicyOverrideSpec, PolicySpec,
};
pub use resolve::{
    LoadedLab, ResolvedComponent, ResolvedEnvironment, ResolvedFixtureSelection, ResolvedLab,
    resolve_lab,
};
