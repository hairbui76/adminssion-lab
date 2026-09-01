//! The Gateway suite's configuration surface (ROADMAP Task 6.1): which
//! Gateway API fixtures a lab persists, and what each route is
//! contracted to do.
//!
//! # Where these types are defined, and why not here
//!
//! Task 6.1's file list names this module as
//! [`GatewaySuiteSpec`]/[`RouteContract`]/[`HttpProbeContract`]'s home,
//! but §1.2's cross-task type registry independently freezes
//! [`admissionlab_spec::ResolvedLab::gateway`] as an
//! `Option<GatewaySuiteSpec>`. Both statements hold only if one crate
//! defines the type and the other names it, and the direction is forced
//! by the dependency graph: this crate depends on `admissionlab-core`,
//! which depends on `admissionlab-spec`, so a `spec -> gateway` edge
//! would be a cycle Cargo rejects.
//!
//! So the three hand-written configuration types are defined in
//! [`admissionlab_spec::model`] -- next to [`admissionlab_spec::LabSpec`]
//! and every other type a user types into `admissionlab.yaml`, sharing
//! its `camelCase` wire convention, its `deny_unknown_fields` strictness
//! and its generated JSON Schema -- and this module **re-exports those
//! exact types**. It does not declare parallel ones: §1.2's "the
//! following names are canonical" rule forbids a synonym, and a twin
//! `GatewayRouteContract` that had to be kept in step by hand is the
//! precise failure that rule exists to prevent.
//!
//! What this module *does* own is everything Phase 6 needs that a user
//! never writes: [`GatewayIdentity`] (§1.2's canonical name for "which
//! `Gateway`", used by the observed-evidence types in later tasks) and
//! the small amount of vocabulary that turns a contract into a lookup
//! against a live cluster. The dividing line is the same one
//! [`admissionlab_spec::ComponentSpec`] and
//! [`admissionlab_spec::ResolvedComponent`] already draw: the
//! hand-written form belongs to `admissionlab-spec`; what this project
//! *observed* belongs to the crate that observed it.
//!
//! # What is validated, and where
//!
//! Every strict-configuration rule Task 6.1 Step 1 asks for --
//! duplicate contract ids, an HTTP method outside Gateway API's own
//! `HTTPMethod` enumeration, a status code outside `100..=599`, an
//! empty manifest list, a `Gateway` identified by an empty string --
//! is enforced by [`admissionlab_spec::resolve_lab`], at configuration
//! load time, before any cluster is created. That placement is
//! deliberate rather than incidental: the rules are about the document,
//! the document is parsed in `admissionlab-spec`, and a second
//! validation pass in this crate could only ever disagree with the
//! first. `tests/model.rs` drives those rules through this crate's
//! re-exports, so the surface Tasks 6.2-6.9 actually consume is the one
//! under test.

pub use admissionlab_spec::{
    ALLOWED_HTTP_METHODS, DEFAULT_RECONCILIATION_TIMEOUT, GatewaySuiteSpec, HttpProbeContract,
    RouteContract, is_valid_http_status,
};

/// Which `Gateway` object something refers to: §1.2's canonical Gateway
/// identity.
///
/// `namespace` is non-optional because a `Gateway` is always namespaced
/// (unlike a `GatewayClass`, which is cluster-scoped and identified by
/// name alone -- see `GatewayClassEvidence` in Task 6.3). Distinct from
/// `ParentIdentity` (Task 6.3), which describes a route's *claimed*
/// parent as the route wrote it -- where the namespace really can be
/// absent, meaning "the route's own namespace" -- rather than a
/// resolved, existing object.
///
/// `Serialize` (but not `Deserialize`): this appears inside
/// [`crate::conditions::GatewayEvidence`], which is captured once from a
/// live cluster and only ever serialized *outward* into a run's report
/// -- the same one-way asymmetry, for the same reason,
/// `admissionlab_admission::AdmissionOutcome` documents.
// ROADMAP Task 7.2: `GatewayIdentity` is embedded in the frozen
// `admissionlab.io/result/v1` result document (inside
// `GatewayEvidence`), so the generated schema has to describe it.
// Derive only -- no field, name, or semantic change.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct GatewayIdentity {
    /// The `Gateway`'s namespace.
    pub namespace: String,
    /// The `Gateway`'s name.
    pub name: String,
}

impl std::fmt::Display for GatewayIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.namespace, self.name)
    }
}

/// The [`GatewayIdentity`] a [`RouteContract`] names as its target.
///
/// A free function rather than an inherent method because
/// [`RouteContract`] is defined in `admissionlab-spec` (see this
/// module's documentation) and Rust does not allow inherent impls on a
/// foreign type. Deliberately not a `From`/`Into` impl either: this is a
/// projection of two of a contract's eight fields, not a conversion of
/// the whole value, and spelling that out at the call site is clearer
/// than an anonymous `.into()`.
///
/// This is a plain restatement of what the user wrote -- Task 6.1 Step 2
/// forbids inferring a Gateway from anything else, so there is nothing
/// here to resolve, look up, or default.
#[must_use]
pub fn contract_gateway_identity(contract: &RouteContract) -> GatewayIdentity {
    GatewayIdentity {
        namespace: contract.gateway_namespace.clone(),
        name: contract.gateway_name.clone(),
    }
}
