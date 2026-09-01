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
//! - [`conditions`] (Task 6.3) normalizes what an implementation
//!   published about a `GatewayClass`, a `Gateway` and an `HTTPRoute`:
//!   four condition states (including `Missing`, which is not `False`),
//!   lookup that never depends on list order, and staleness as a
//!   computed relationship rather than a stored flag. Read that module
//!   before trusting any condition this crate reports.
//! - [`reconcile`] (Task 6.4) waits for a route to reach a stable,
//!   current status and reports [`ReconciliationEvidence`]. Its module
//!   documentation carries the roadmap's convergence rule verbatim,
//!   clause by clause, and explains why a timeout is evidence rather
//!   than a verdict.
//! - [`endpoint`] (Task 6.6) turns a recipe-declared
//!   [`GatewayEndpointStrategy`] into the one concrete
//!   namespace/Service/port a port-forward and a probe need. Read that
//!   module before assuming anything about how a Gateway's data plane is
//!   located: the mapping is vendor metadata, the ambiguity rules are
//!   deliberate, and the port rule has two decisions in it.
//! - [`port_forward`] (Task 6.7) opens a `kubectl port-forward` to that
//!   endpoint and manages its lifetime. Read that module's "Timeout
//!   ownership" and "Termination" sections before changing anything
//!   about when the child dies.
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
pub mod conditions;
pub mod endpoint;
pub mod error;
pub mod model;
pub mod port_forward;
pub mod reconcile;

pub use apply::{
    AppliedGatewayFixture, ApplyCategory, FIELD_MANAGER, GatewayApplyPlan, PlannedObject,
    apply_gateway_manifests, apply_gateway_plan_with_client, plan_gateway_apply,
};
pub use conditions::{
    CONDITION_ACCEPTED, CONDITION_PROGRAMMED, CONDITION_RESOLVED_REFS, ConditionFreshness,
    ConditionState, GatewayClassEvidence, GatewayEvidence, ObservedCondition, ParentIdentity,
    ParentLookup, RouteEvidence, RouteParentStatus, gateway_class_evidence, gateway_evidence,
    observed_conditions, route_evidence,
};
pub use endpoint::{
    GatewayEndpoint, GatewayEndpointResolver, GatewayEndpointStrategy, KubeGatewayEndpointResolver,
    resolve_gateway_endpoint_with_client,
};
pub use error::{EndpointLookup, GatewayError};
pub use port_forward::{
    KUBECTL_PROGRAM, LOCAL_ADDRESS, PORT_FORWARD_READY_TIMEOUT, PortForwardHandle,
    PortForwardOutput, await_forwarding_address, parse_forwarding_line, port_forward_command,
    start_service_port_forward,
};
pub use reconcile::{
    DIAGNOSTIC_GATEWAY_CLASS_ABSENT, DIAGNOSTIC_PARENT_ABSENT, DIAGNOSTIC_PARENT_AMBIGUOUS,
    DIAGNOSTIC_STALE_STATUS, DIAGNOSTIC_TIMEOUT, GATEWAY_API_GROUP, GATEWAY_API_VERSION,
    GatewayStatusSource, INITIAL_POLL_INTERVAL, KubeGatewayStatusSource, MAX_POLL_INTERVAL,
    REQUIRED_GATEWAY_CLASS_CONDITIONS, REQUIRED_GATEWAY_CONDITIONS,
    REQUIRED_ROUTE_PARENT_CONDITIONS, ReconciliationEvidence, STABILITY_INTERVAL,
    wait_for_route_reconciliation, wait_for_route_reconciliation_with_client,
    wait_for_route_reconciliation_with_source,
};

pub use model::{
    ALLOWED_HTTP_METHODS, DEFAULT_RECONCILIATION_TIMEOUT, GatewayIdentity, GatewaySuiteSpec,
    HttpProbeContract, RouteContract, contract_gateway_identity, is_valid_http_status,
};
