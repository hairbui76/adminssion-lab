#![forbid(unsafe_code)]
//! The Gateway behavior engine for Admission Lab (ROADMAP Phase 6).
//!
//! Where `admissionlab-admission` observes what an API server *decided*
//! about one object, this crate observes what a Gateway API
//! implementation *did* with a set of objects: whether its controllers
//! reconciled them, what conditions they published, and (from Task 6.6
//! onward) what real HTTP requests through the resulting data plane
//! actually returned.
//!
//! - [`model`] (Task 6.1) is the configuration surface: the Gateway
//!   fixture manifests and the per-route reconciliation/traffic
//!   contract. See that module for why the canonical types are defined
//!   in `admissionlab-spec` and re-exported here rather than declared
//!   twice.
//! - [`apply`] (Task 6.2) installs a suite's manifests into a lab
//!   cluster: parse and hash everything first, then apply in a fixed
//!   category order through the dynamic API, and never delete. Read that
//!   module's documentation before changing anything about ordering or
//!   apply semantics.
//! - [`error`] defines [`GatewayError`], this crate's one error type,
//!   and documents where it draws the line between "the API server
//!   refused this" and "no answer could be obtained at all".
//!
//! # Gateway fixtures are persisted, not dry-run
//!
//! Global Constraint 16 makes Kubernetes server-side dry-run the
//! authoritative *admission* fixture execution mode for Alpha, and
//! `admissionlab_fixtures::execute` implements exactly that. Gateway
//! fixtures are the roadmap's own, explicit exception (Phase 6's
//! "Execution distinction"):
//!
//! > Gateway fixtures are persisted in the disposable cluster because
//! > controller reconciliation and data-plane programming require
//! > durable resources. Persisted Gateway fixtures are isolated by the
//! > ephemeral cluster; Admission Lab never applies them to production.
//!
//! A dry-run `Gateway` is never seen by a controller, never programs a
//! listener, and never has a status to observe -- so the whole of Phase
//! 6 would be unobservable under dry-run. The property that makes
//! persisting them safe is not a check inside this crate; it is the
//! disposable cluster itself, and the fact that every client this crate
//! builds comes from a
//! [`admissionlab_core::ClusterHandle`]'s own isolated kubeconfig rather
//! than from an ambient `~/.kube/config`.

pub mod apply;
pub mod error;
pub mod model;

pub use apply::{
    AppliedGatewayFixture, ApplyCategory, FIELD_MANAGER, GatewayApplyPlan, PlannedObject,
    apply_gateway_manifests, apply_gateway_plan_with_client, plan_gateway_apply,
};
pub use error::GatewayError;
pub use model::{
    ALLOWED_HTTP_METHODS, DEFAULT_RECONCILIATION_TIMEOUT, GatewayIdentity, GatewaySuiteSpec,
    HttpProbeContract, RouteContract, contract_gateway_identity, is_valid_http_status,
};
