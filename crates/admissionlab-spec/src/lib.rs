#![forbid(unsafe_code)]
//! The `admissionlab.yaml` configuration contract: the model users
//! hand-write, a strict loader for every version still supported, a
//! migration between them, path/semantic resolution, and JSON Schema
//! generation for editor completion and validation.
//!
//! # Three wire versions, one resolved model
//!
//! ROADMAP Task 9.1 freezes the lab document at the stable
//! `admissionlab.io/v1`, keeping `admissionlab.io/v1beta1` (Task 7.1) and
//! `admissionlab.io/v1alpha1` readable. The whole of that lives in four
//! modules and two functions:
//!
//! - [`v1alpha1`] is the **frozen** Public Alpha wire model
//!   ([`V1Alpha1Lab`]). It never changes shape again.
//! - [`v1beta1`] is the **frozen** Public Beta wire model
//!   ([`V1Beta1Lab`]).
//! - [`v1`] is the current, stable wire model ([`V1Lab`]). Its freeze
//!   review renamed nothing, so it shares every nested type with
//!   [`v1beta1`] and differs from it only in the `apiVersion` it accepts
//!   — see that module's "Zero wire changes from `v1beta1`".
//! - [`model`] holds every hand-written type all three versions spell
//!   *identically* — which is most of them; a type lives there exactly
//!   while no supported version disagrees about it.
//! - [`migrate_v1alpha1_to_v1beta1`] and [`migrate_v1beta1_to_v1`] are
//!   the bridges, one per version boundary, and both are pure.
//!
//! Above this crate, none of that is visible. [`load_any_supported_lab`]
//! reads whatever a user wrote and returns a [`ResolvedLab`], which
//! carries no version marker at all: every document is migrated forward
//! to the current model *before* resolution, so there is exactly one
//! resolver and one resolved shape for the rest of the workspace to
//! consume. See [`ResolvedLab`]'s "Version-independent by construction",
//! and `migrate.rs` for the field-by-field record of what each freeze
//! review changed (two wire names at the Beta boundary, none at the
//! stable one) and what it deliberately kept.
//!
//! # The pieces
//!
//! - [`model`] defines the version-independent raw, as-written shapes.
//! - [`component`] defines the resolved component/install/readiness
//!   vocabulary ([`ResolvedComponent`], [`InstallMethod`],
//!   [`ReadinessCheck`], [`RecipeNormalizeRule`], [`Capability`]) that
//!   [`resolve_lab`] converts each [`ComponentSpec`] into.
//! - [`load_any_supported_lab`] reads, parses, migrates, and resolves a
//!   configuration file in one step. [`load_lab`] is the narrower
//!   `v1alpha1`-only reader that hands back the *unresolved* document.
//! - [`resolve_lab`] validates a loaded configuration and resolves every
//!   relative path inside it against that file's own directory.
//! - [`v1_json_schema`], [`v1beta1_json_schema`] and
//!   [`v1alpha1_json_schema`] generate the JSON Schemas checked in at
//!   `schemas/admissionlab-v1.json`, `schemas/admissionlab-v1beta1.json`
//!   and `schemas/admissionlab-v1alpha1.json`.
//!
//! # The `gateway` and `migration` sections live here, but Gateway
//! *behavior* does not
//!
//! [`GatewaySuiteSpec`], [`RouteContract`], and [`HttpProbeContract`]
//! (ROADMAP Task 6.1), and [`MigrationSuiteSpec`],
//! [`MigrationCaseSpec`] and [`NonPortableFeatureExpectation`] (Task
//! 8.3), are defined alongside every other hand-written configuration
//! type; `admissionlab_gateway::model` and
//! `admissionlab_gateway::migration` re-export them rather than
//! declaring twins. See [`GatewaySuiteSpec`]'s own documentation for why
//! the definition has to live on this side of the dependency graph, and
//! for the line between "what a user writes" (here) and "what a cluster
//! was observed to do" (`admissionlab-gateway`).
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
//!
//! The same rule settles the version split's own name collisions:
//! [`PolicySpec`], [`LatencyPolicy`], and [`GatewaySuiteSpec`] at this
//! crate's root are the **current** types — declared in [`v1beta1`],
//! shared unchanged by [`v1`], and what [`ResolvedLab`] carries. Their
//! frozen Alpha twins keep the same simple names inside [`v1alpha1`] and
//! are reached as `v1alpha1::PolicySpec` and so on. `LabSpec` at the root
//! is the Alpha root document ([`V1Alpha1Lab`]'s underlying struct),
//! because [`load_lab`]/[`LoadedLab`] are the Alpha entry points that
//! name it; the current root is [`V1Lab`].

pub mod component;
pub mod error;
pub mod load;
pub mod migrate;
pub mod model;
pub mod resolve;
pub mod schema;
pub mod v1;
pub mod v1alpha1;
pub mod v1beta1;
mod validate;

pub use component::{
    Capability, GATEWAY_NAME_PLACEHOLDER, GATEWAY_NAMESPACE_PLACEHOLDER, GATEWAY_PLACEHOLDERS,
    GatewayEndpointStrategy, InstallMethod, ManifestInstallSpec, ReadinessCheck,
    RecipeNormalizeRule, ResolvedComponent, resolve_gateway_endpoint, resolve_readiness,
    substitute_gateway_placeholders,
};
pub use error::SpecError;
pub use load::{SUPPORTED_API_VERSIONS, declared_api_version, load_any_supported_lab, load_lab};
pub use migrate::{MigrationError, migrate_v1alpha1_to_v1beta1, migrate_v1beta1_to_v1};
pub use model::{
    ALLOWED_HTTP_METHODS, ComponentSpec, CustomResourceConditionSpec,
    DEFAULT_RECONCILIATION_TIMEOUT, EnvironmentSpec, FixtureSelectionSpec, GatewayEndpointSpec,
    GatewaySuiteSpec, HelmInstallSpec, HttpProbeContract, InstallMethodSpec, LatencyPolicy,
    ManifestsInstallSpec, MigrationCaseSpec, MigrationSideSpec, MigrationSuiteSpec,
    NamedObjectSpec, NamespacedObjectSpec, NonPortableFeatureExpectation, PolicyOverrideSpec,
    PolicySpec, ReadinessCheckSpec, RouteContract, is_valid_http_status,
};
pub use resolve::{
    LoadedLab, ResolvedEnvironment, ResolvedFixtureSelection, ResolvedLab, resolve_lab,
};
pub use schema::{v1_json_schema, v1alpha1_json_schema, v1beta1_json_schema};
pub use v1::V1Lab;
pub use v1alpha1::{LabSpec, V1Alpha1Lab};
pub use v1beta1::V1Beta1Lab;
