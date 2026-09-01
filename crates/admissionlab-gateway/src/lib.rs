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
//! - [`probe`] (Task 6.8) sends one real HTTP request through that
//!   forward and records what came back: a status, a backend identity
//!   when the response identified itself, normalized headers, a body
//!   hash, and an honest attempt count. Read that module before
//!   assuming anything about retries, redirects, or what `backend: None`
//!   means.
//! - [`case`] bundles Tasks 6.4 and 6.8's evidence into the
//!   [`GatewayCaseResult`] Tasks 6.9 and 6.11 consume.
//! - [`diff`] (Task 6.9) is the only module here that looks at *two*
//!   sides and claims something: it turns a baseline and a candidate
//!   [`GatewayCaseResult`] into `admissionlab_diff::SemanticChange`s.
//!   Read its documentation before trusting an empty result -- it
//!   explains what `converged` is not, how parents and probes are
//!   paired, why stale evidence silences absence claims, and which half
//!   of the direction rule belongs to `admissionlab-policy`.
//! - [`tls`] (Task 8.6) mints an ephemeral CA and leaf certificate for
//!   a `.test` hostname, and builds the `rustls::ClientConfig` that
//!   trusts only that CA. Read its "Where the private key may go" before
//!   calling [`TestCertificate::expose_key_pem`], and its "The handoff
//!   to Task 8.7" before wiring TLS into [`probe`].
//! - [`migration`] (Task 8.3) is the Ingress-to-Gateway migration
//!   suite's configuration surface: explicit baseline/candidate manifest
//!   pairings, the probes replayed through both, and the non-portable
//!   features an author has accepted in writing. Read that module before
//!   assuming this project converts anything -- it does not, on purpose,
//!   and the module explains why a self-converting suite could not
//!   detect its own converter's mistakes. Task 8.5 added the observed
//!   half beside it: [`compare_migration_case`] classifies what an
//!   `Ingress` and its replacement `HTTPRoute` really did with the same
//!   requests, in [`MigrationBehaviorKind`]'s own vocabulary -- which is
//!   deliberately *not* `admissionlab_diff::SemanticChangeKind`, for
//!   reasons that module states in full.
//! - [`ingress`] (Task 8.4) is the *baseline* half of a migration
//!   comparison: it persists one migration case's `Ingress` manifests,
//!   records a validating webhook's refusal as admission evidence rather
//!   than as a failure, and -- because an `Ingress` has no status worth
//!   waiting on -- proves readiness with traffic under a deadline. Read
//!   its "THE FINDING" before assuming an `Ingress` can be waited on the
//!   way a `Gateway` can.
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
pub mod case;
pub mod conditions;
pub mod diff;
pub mod endpoint;
pub mod error;
pub mod ingress;
pub mod migration;
pub mod model;
pub mod port_forward;
pub mod probe;
pub mod reconcile;
pub mod tls;

pub use apply::{
    AppliedGatewayFixture, ApplyCategory, FIELD_MANAGER, GatewayApplyPlan, PlannedObject,
    apply_gateway_manifests, apply_gateway_plan_with_client, plan_gateway_apply,
};
pub use case::GatewayCaseResult;
pub use diff::{
    GatewayCaseComparison, GatewayComparability, GatewayEvidenceLevel, ProbePair, diff_gateway,
    gateway_comparability, gateway_evidence_level,
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
pub use ingress::{
    DIAGNOSTIC_INGRESS_DENIED, DIAGNOSTIC_INGRESS_NOT_SERVING, INGRESS_GROUP,
    INGRESS_REPROBE_INTERVAL, INGRESS_RESOURCE, IngressCaseResult, admission_denial,
    applied_ingress_identity, probe_matches_contract, run_ingress_case,
    run_ingress_case_with_resolver,
};
pub use migration::{
    MigrationBehaviorChange, MigrationBehaviorKind, MigrationCaseSpec, MigrationComparability,
    MigrationComparison, MigrationSuiteSpec, NONPORTABLE_INGRESS_ANNOTATIONS,
    NonPortableAnnotation, NonPortableFeatureExpectation, compare_migration_case,
    compare_migration_traffic, expected_nonportable_features, migration_comparability,
    nonportable_annotation, nonportable_changes, observed_nonportable_annotations,
    unmatched_nonportable_expectations,
};
pub use port_forward::{
    KUBECTL_PROGRAM, LOCAL_ADDRESS, PORT_FORWARD_READY_TIMEOUT, PortForwardHandle,
    PortForwardOutput, await_forwarding_address, parse_forwarding_line, port_forward_command,
    start_service_port_forward,
};
pub use probe::{
    HttpProbeResult, MAX_PROBE_BODY_BYTES, PROBE_READINESS_WINDOW, PROBE_REQUEST_TIMEOUT,
    PROBE_RETRY_INTERVAL, REDACTED_REQUEST_HEADERS, describe_probe_request, execute_http_probe,
    is_redirect, redacted_probe_headers,
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
pub use tls::{
    CERTIFICATE_VALIDITY, NOT_BEFORE_SKEW, TEST_TLD, TestCertificate, generate_test_certificate,
    probe_server_name, test_certificate_client_config,
};
