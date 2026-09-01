//! ROADMAP Task 6.9 contract tests: the Gateway comparator.
//!
//! Every case here is built from real object shapes -- the checked-in
//! goldens in `testdata/objects/gateway-status/`, or a status assembled
//! by [`route_object`] from the same fields -- and pushed through the
//! same parsers (`gateway_evidence`, `route_evidence`,
//! `gateway_class_evidence`) a live run uses, so a test can never assert
//! against a shape a cluster would not produce.
//!
//! The properties under test are the eight numbered steps of the task,
//! plus the two that decide whether the rest is honest: what an empty
//! result means (`gateway_comparability`) and what a stale status is not
//! evidence of.
//!
//! Each test's doc comment names what would make it fail.

use std::path::PathBuf;
use std::time::Duration;

use admissionlab_core::FixtureId;
use admissionlab_diff::{
    ChangeDirection, SemanticChange, SemanticChangeKind, UNATTRIBUTED_FIXTURE,
};
use admissionlab_gateway::{
    CONDITION_ACCEPTED, CONDITION_PROGRAMMED, CONDITION_RESOLVED_REFS, GatewayCaseComparison,
    GatewayCaseResult, GatewayComparability, GatewayEvidenceLevel, HttpProbeResult,
    ReconciliationEvidence, diff_gateway, gateway_class_evidence, gateway_comparability,
    gateway_evidence, gateway_evidence_level, route_evidence,
};
use serde_json::{Value, json};

/// Loads one golden status object from Task 6.3's
/// `testdata/objects/gateway-status/`.
fn golden(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/objects/gateway-status")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read golden {}: {error}", path.display()));
    serde_norway::from_str(&text)
        .unwrap_or_else(|error| panic!("parse golden {}: {error}", path.display()))
}

/// One `metav1.Condition`, as a controller writes it.
fn condition(type_name: &str, status: &str, reason: &str, observed_generation: i64) -> Value {
    json!({
        "type": type_name,
        "status": status,
        "reason": reason,
        "lastTransitionTime": "2026-09-01T10:16:12Z",
        "observedGeneration": observed_generation,
    })
}

/// One entry of a route's `status.parents`.
fn parent(gateway: &str, section_name: Option<&str>, conditions: Vec<Value>) -> Value {
    let mut parent_ref = json!({
        "group": "gateway.networking.k8s.io",
        "kind": "Gateway",
        "namespace": "gateway-lab",
        "name": gateway,
    });
    if let Some(section_name) = section_name {
        parent_ref["sectionName"] = json!(section_name);
    }
    json!({
        "parentRef": parent_ref,
        "controllerName": "istio.io/gateway-controller",
        // `Value::Array` rather than borrowing the vector: it consumes
        // the argument, which is what makes the by-value parameter the
        // honest signature.
        "conditions": Value::Array(conditions),
    })
}

/// The two conditions a converged parent entry publishes.
fn accepted_and_resolved(generation: i64) -> Vec<Value> {
    vec![
        condition(CONDITION_ACCEPTED, "True", "Accepted", generation),
        condition(CONDITION_RESOLVED_REFS, "True", "ResolvedRefs", generation),
    ]
}

/// An `HTTPRoute` object carrying `parents` in its status.
fn route_object(generation: i64, parents: Vec<Value>) -> Value {
    json!({
        "apiVersion": "gateway.networking.k8s.io/v1",
        "kind": "HTTPRoute",
        "metadata": {
            "name": "echo-a",
            "namespace": "gateway-lab",
            "generation": generation,
        },
        "status": {"parents": Value::Array(parents)},
    })
}

/// One side's result, assembled through the real parsers.
///
/// `elapsed` and `diagnostics` are the two fields nothing here compares
/// (the comparator reads conditions, probes and `converged`), so they
/// are fixed rather than varied per test.
fn case(
    gateway: &Value,
    class: Option<&Value>,
    route: &Value,
    converged: bool,
) -> GatewayCaseResult {
    GatewayCaseResult {
        contract_id: "echo-a-root".to_owned(),
        reconciliation: ReconciliationEvidence {
            gateway_class: class
                .map(|class| gateway_class_evidence(class).expect("golden GatewayClass parses")),
            gateway: gateway_evidence(gateway).expect("golden Gateway parses"),
            route: route_evidence(route).expect("route status parses"),
            elapsed: Duration::from_millis(700),
            converged,
            diagnostics: Vec::new(),
        },
        probes: Vec::new(),
    }
}

/// A fully converged side: the three goldens, unmodified.
fn converged_case() -> GatewayCaseResult {
    case(
        &golden("gateway-programmed.yaml"),
        Some(&golden("gatewayclass-accepted.yaml")),
        &golden("httproute-accepted.yaml"),
        true,
    )
}

/// One probe result. Only the three fields a traffic claim is about
/// vary; the rest are fixed plausible values.
fn probe(status: u16, backend: Option<&str>) -> HttpProbeResult {
    HttpProbeResult {
        status,
        backend: backend.map(str::to_owned),
        response_headers: std::collections::BTreeMap::new(),
        response_body_sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            .to_owned(),
        elapsed: Duration::from_millis(12),
        attempts: 1,
    }
}

/// The wire names of a change list, in order -- what most of these tests
/// assert on, since the order is part of the contract.
fn kinds(changes: &[SemanticChange]) -> Vec<&'static str> {
    changes.iter().map(|change| change.kind.as_str()).collect()
}

/// The one change in a list, or a panic naming what was actually
/// produced.
fn only(changes: &[SemanticChange]) -> &SemanticChange {
    assert_eq!(
        changes.len(),
        1,
        "expected exactly one change, got {:?}",
        kinds(changes)
    );
    &changes[0]
}

/// Fails if two identical sides produce any claim at all -- the property
/// every other assertion here rests on, and the one a comparator that
/// compared reason strings or list order would break.
#[test]
fn identical_sides_produce_no_changes() {
    let baseline = converged_case();
    let candidate = converged_case();

    assert!(diff_gateway(&baseline, &candidate).is_empty());
    assert_eq!(
        gateway_comparability(&baseline, &candidate),
        GatewayComparability::Comparable
    );
    assert!(gateway_comparability(&baseline, &candidate).absence_is_evidence());
}

/// Step 1: fails if `Accepted: True -> False` on a route's parent stops
/// producing an `accepted_condition_changed` carrying both states, the
/// route's bare name as its subject, and a `regression` direction.
#[test]
fn an_accepted_condition_that_turns_false_is_a_regression() {
    let baseline = converged_case();
    let candidate = case(
        &golden("gateway-programmed.yaml"),
        Some(&golden("gatewayclass-accepted.yaml")),
        &route_object(
            1,
            vec![parent(
                "lab-gateway",
                Some("http"),
                vec![
                    condition(CONDITION_ACCEPTED, "False", "NotAllowedByListeners", 1),
                    condition(CONDITION_RESOLVED_REFS, "True", "ResolvedRefs", 1),
                ],
            )],
        ),
        // A settled `False` is a converged verdict, not a timeout --
        // `reconcile.rs` is explicit about that, and this is the case
        // that would silently disappear if the comparator read
        // `converged` as a pass.
        true,
    );

    let changes = diff_gateway(&baseline, &candidate);
    let change = only(&changes);
    assert_eq!(change.kind, SemanticChangeKind::AcceptedConditionChanged);
    assert_eq!(change.subject.as_deref(), Some("echo-a"));
    assert_eq!(change.baseline.as_ref().unwrap()["state"], json!("True"));
    assert_eq!(change.candidate.as_ref().unwrap()["state"], json!("False"));
    assert_eq!(
        change.candidate.as_ref().unwrap()["object"],
        json!("HTTPRoute")
    );
    assert_eq!(
        change.candidate.as_ref().unwrap()["parent"]["sectionName"],
        json!("http")
    );
    assert_eq!(change.direction(), Some(ChangeDirection::Regression));
    assert_eq!(change.object_path, None);
}

/// Step 1: fails if a parent `Gateway` present in the baseline's status
/// and absent from the candidate's stops being a `route_detached`, or if
/// the mirror case stops being a `route_attached`.
#[test]
fn a_parent_gateway_that_disappears_is_a_detachment() {
    let two_parents = route_object(
        1,
        vec![
            parent("lab-gateway", Some("http"), accepted_and_resolved(1)),
            parent("lab-gateway-b", Some("http"), accepted_and_resolved(1)),
        ],
    );
    let one_parent = route_object(
        1,
        vec![parent(
            "lab-gateway",
            Some("http"),
            accepted_and_resolved(1),
        )],
    );
    let gateway = golden("gateway-programmed.yaml");
    let class = golden("gatewayclass-accepted.yaml");
    let attached = case(&gateway, Some(&class), &two_parents, true);
    let detached = case(&gateway, Some(&class), &one_parent, true);

    let lost = diff_gateway(&attached, &detached);
    let change = only(&lost);
    assert_eq!(change.kind, SemanticChangeKind::RouteDetached);
    assert_eq!(change.subject.as_deref(), Some("echo-a"));
    assert_eq!(
        change.baseline.as_ref().unwrap()["gateway"],
        json!({"namespace": "gateway-lab", "name": "lab-gateway-b"})
    );
    assert!(
        change.candidate.is_none(),
        "a detachment has no candidate value, not an empty one"
    );

    let gained = diff_gateway(&detached, &attached);
    assert_eq!(kinds(&gained), ["route_attached"]);
    assert!(gained[0].baseline.is_none());
}

/// Fails if a parent whose namespace is written out and one whose
/// `parentRef` omits it (meaning the route's own namespace, per Gateway
/// API) stop pairing as the same parent -- which would report one
/// implementation's spelling habit as a detach plus an attach.
#[test]
fn an_omitted_parent_namespace_defaults_to_the_routes_own() {
    let mut implicit = parent("lab-gateway", Some("http"), accepted_and_resolved(1));
    implicit["parentRef"]
        .as_object_mut()
        .unwrap()
        .remove("namespace");

    let gateway = golden("gateway-programmed.yaml");
    let class = golden("gatewayclass-accepted.yaml");
    let explicit_side = case(
        &gateway,
        Some(&class),
        &route_object(
            1,
            vec![parent(
                "lab-gateway",
                Some("http"),
                accepted_and_resolved(1),
            )],
        ),
        true,
    );
    let implicit_side = case(
        &gateway,
        Some(&class),
        &route_object(1, vec![implicit]),
        true,
    );

    assert!(diff_gateway(&explicit_side, &implicit_side).is_empty());
}

/// Fails if a route that binds to a different *listener* of the same
/// `Gateway` is reported as a detachment instead of a
/// `listener_binding_changed`, or if the change stops naming the
/// listener as its subject.
#[test]
fn a_different_listener_of_the_same_gateway_is_a_binding_change() {
    let gateway = golden("gateway-programmed.yaml");
    let class = golden("gatewayclass-accepted.yaml");
    let baseline = case(
        &gateway,
        Some(&class),
        &route_object(
            1,
            vec![
                parent("lab-gateway", Some("http"), accepted_and_resolved(1)),
                parent("lab-gateway", Some("http-alt"), accepted_and_resolved(1)),
            ],
        ),
        true,
    );
    let candidate = case(
        &gateway,
        Some(&class),
        &route_object(
            1,
            vec![parent(
                "lab-gateway",
                Some("http"),
                accepted_and_resolved(1),
            )],
        ),
        true,
    );

    let changes = diff_gateway(&baseline, &candidate);
    let change = only(&changes);
    assert_eq!(change.kind, SemanticChangeKind::ListenerBindingChanged);
    assert_eq!(change.subject.as_deref(), Some("http-alt"));
    assert_eq!(
        change.baseline.as_ref().unwrap()["listener"],
        json!("http-alt")
    );
    assert!(change.candidate.is_none());
}

/// Step 2: fails if `ResolvedRefs: True -> False` stops producing the
/// backend-resolution claim *plus* its condition evidence, in that
/// order.
#[test]
fn resolved_refs_turning_false_pairs_a_backend_claim_with_its_condition() {
    let gateway = golden("gateway-programmed.yaml");
    let class = golden("gatewayclass-accepted.yaml");
    let baseline = converged_case();
    let candidate = case(
        &gateway,
        Some(&class),
        &route_object(
            1,
            vec![parent(
                "lab-gateway",
                Some("http"),
                vec![
                    condition(CONDITION_ACCEPTED, "True", "Accepted", 1),
                    condition(CONDITION_RESOLVED_REFS, "False", "BackendNotFound", 1),
                ],
            )],
        ),
        true,
    );

    let changes = diff_gateway(&baseline, &candidate);
    assert_eq!(
        kinds(&changes),
        [
            "backend_resolution_changed",
            "resolved_refs_condition_changed"
        ],
        "the product-level claim comes first, its condition evidence second"
    );
    for change in &changes {
        assert_eq!(change.subject.as_deref(), Some("echo-a"));
        assert_eq!(
            change.candidate.as_ref().unwrap()["reason"],
            json!("BackendNotFound")
        );
        assert_eq!(change.direction(), Some(ChangeDirection::Regression));
    }
}

/// Fails if a `ResolvedRefs` that never resolved on either side, moving
/// only between two not-`True` states, starts claiming that backend
/// resolution itself changed -- it did not; only the way the
/// implementation described it did.
#[test]
fn a_move_between_two_unresolved_states_is_condition_evidence_only() {
    let gateway = golden("gateway-programmed.yaml");
    let class = golden("gatewayclass-accepted.yaml");
    let side = |status: &str| {
        case(
            &gateway,
            Some(&class),
            &route_object(
                1,
                vec![parent(
                    "lab-gateway",
                    Some("http"),
                    vec![
                        condition(CONDITION_ACCEPTED, "True", "Accepted", 1),
                        condition(CONDITION_RESOLVED_REFS, status, "BackendNotFound", 1),
                    ],
                )],
            ),
            true,
        )
    };

    let changes = diff_gateway(&side("False"), &side("Unknown"));
    assert_eq!(kinds(&changes), ["resolved_refs_condition_changed"]);
    assert_eq!(
        changes[0].direction(),
        None,
        "neither side reached True, so neither direction is claimed"
    );
}

/// Fails if a condition whose state is identical on both sides but whose
/// `reason` was reworded produces a change -- Task 4.4's message-drift
/// rule, and the reason step 6 wants direction encoded rather than
/// inferred from these strings.
#[test]
fn a_reworded_reason_is_not_a_behavior_change() {
    let gateway = golden("gateway-programmed.yaml");
    let class = golden("gatewayclass-accepted.yaml");
    let side = |reason: &str| {
        case(
            &gateway,
            Some(&class),
            &route_object(
                1,
                vec![parent(
                    "lab-gateway",
                    Some("http"),
                    vec![
                        condition(CONDITION_ACCEPTED, "True", reason, 1),
                        condition(CONDITION_RESOLVED_REFS, "True", "ResolvedRefs", 1),
                    ],
                )],
            ),
            true,
        )
    };

    assert!(diff_gateway(&side("Accepted"), &side("RouteAccepted")).is_empty());
}

/// Step 3: fails if a `Gateway` that stops being `Programmed` is not
/// reported as `programmed_condition_changed` against the Gateway's own
/// bare name.
#[test]
fn a_gateway_that_stops_being_programmed_is_reported() {
    let baseline = converged_case();
    let candidate = case(
        &golden("gateway-not-programmed.yaml"),
        Some(&golden("gatewayclass-accepted.yaml")),
        &golden("httproute-accepted.yaml"),
        true,
    );

    let changes = diff_gateway(&baseline, &candidate);
    let change = only(&changes);
    assert_eq!(change.kind, SemanticChangeKind::ProgrammedConditionChanged);
    assert_eq!(change.subject.as_deref(), Some("lab-gateway"));
    assert_eq!(
        change.candidate.as_ref().unwrap()["object"],
        json!("Gateway")
    );
    assert_eq!(change.direction(), Some(ChangeDirection::Regression));
}

/// Fails if a `GatewayClass` that stops being accepted is not reported.
/// Its `Accepted` is one of the conditions the convergence rule requires,
/// so losing it is a real regression -- and its payload must be honest
/// that no freshness could be computed, since §1.2 gives
/// `GatewayClassEvidence` no generation to compare against.
#[test]
fn a_gateway_class_that_stops_being_accepted_is_reported() {
    let baseline = converged_case();
    let candidate = case(
        &golden("gateway-programmed.yaml"),
        Some(&golden("gatewayclass-pending.yaml")),
        &golden("httproute-accepted.yaml"),
        true,
    );

    let changes = diff_gateway(&baseline, &candidate);
    let change = only(&changes);
    assert_eq!(change.kind, SemanticChangeKind::AcceptedConditionChanged);
    assert_eq!(change.subject.as_deref(), Some("istio"));
    assert_eq!(
        change.candidate.as_ref().unwrap()["freshness"],
        Value::Null,
        "a GatewayClass carries no generation, so its freshness is unknowable, not `current`"
    );
}

/// Step 4: fails if the same status reached from a different echo
/// backend stops being a `traffic_backend_changed` -- the case a status
/// comparison alone cannot see.
#[test]
fn the_same_status_from_a_different_backend_is_a_traffic_change() {
    let mut baseline = converged_case();
    baseline.probes = vec![probe(200, Some("echo-a"))];
    let mut candidate = converged_case();
    candidate.probes = vec![probe(200, Some("echo-b"))];

    let changes = diff_gateway(&baseline, &candidate);
    let change = only(&changes);
    assert_eq!(change.kind, SemanticChangeKind::TrafficBackendChanged);
    assert_eq!(
        change.subject.as_deref(),
        Some("echo-a"),
        "the subject is the backend that was serving before"
    );
    assert_eq!(
        change.baseline.as_ref().unwrap()["backend"],
        json!("echo-a")
    );
    assert_eq!(change.candidate.as_ref().unwrap()["probeIndex"], json!(0));
    assert_eq!(
        change.direction(),
        None,
        "a traffic change carries no direction: better depends on the probe's contract"
    );
}

/// Step 4: fails if a differing status stops being reported, or if a
/// probe whose backend only *one* side identified starts being reported
/// as a backend change -- `backend: None` means unknown, never "a
/// different one answered".
#[test]
fn a_differing_status_is_reported_and_an_unknown_backend_is_not_guessed_at() {
    let mut baseline = converged_case();
    baseline.probes = vec![probe(200, Some("echo-a"))];
    let mut candidate = converged_case();
    candidate.probes = vec![probe(503, None)];

    let changes = diff_gateway(&baseline, &candidate);
    let change = only(&changes);
    assert_eq!(change.kind, SemanticChangeKind::TrafficStatusChanged);
    assert_eq!(change.subject.as_deref(), Some("echo-a"));
    assert_eq!(change.baseline.as_ref().unwrap()["status"], json!(200));
    assert_eq!(change.candidate.as_ref().unwrap()["status"], json!(503));
    assert_eq!(change.candidate.as_ref().unwrap()["backend"], Value::Null);
}

/// Step 5, the critical branch: fails if a candidate that timed out
/// having lost both a previously stable condition and a previously
/// answered probe stops being reported -- and if the comparison stops
/// declaring itself `Partial` while doing so.
#[test]
fn a_candidate_timeout_that_lost_a_condition_and_a_probe_is_reported() {
    let mut baseline = converged_case();
    baseline.probes = vec![probe(200, Some("echo-a"))];

    let mut candidate = case(
        &golden("gateway-programmed.yaml"),
        Some(&golden("gatewayclass-accepted.yaml")),
        &route_object(
            1,
            vec![parent(
                "lab-gateway",
                Some("http"),
                vec![
                    condition(CONDITION_ACCEPTED, "False", "NotAllowedByListeners", 1),
                    condition(CONDITION_RESOLVED_REFS, "Unknown", "Pending", 1),
                ],
            )],
        ),
        false,
    );
    candidate.probes = Vec::new();

    assert_eq!(
        gateway_comparability(&baseline, &candidate),
        GatewayComparability::Partial {
            baseline: GatewayEvidenceLevel::Converged,
            candidate: GatewayEvidenceLevel::Unconverged,
        }
    );

    let changes = diff_gateway(&baseline, &candidate);
    assert_eq!(
        kinds(&changes),
        [
            "accepted_condition_changed",
            "backend_resolution_changed",
            "resolved_refs_condition_changed",
            "traffic_status_changed",
        ]
    );
    let traffic = changes.last().unwrap();
    assert_eq!(traffic.baseline.as_ref().unwrap()["status"], json!(200));
    assert!(
        traffic.candidate.is_none(),
        "the candidate answered nothing; that must not be rendered as a status"
    );
}

/// Step 5, the other branch: fails if a candidate that timed out without
/// losing any condition or probe produces a claim anyway. The timeout is
/// still visible -- as `Partial` lab evidence, which is what the caller
/// surfaces -- but it is not a behavior change.
#[test]
fn a_candidate_timeout_that_lost_nothing_surfaces_evidence_not_changes() {
    let mut baseline = converged_case();
    baseline.probes = vec![probe(200, Some("echo-a"))];
    let mut candidate = case(
        &golden("gateway-programmed.yaml"),
        Some(&golden("gatewayclass-accepted.yaml")),
        &golden("httproute-accepted.yaml"),
        false,
    );
    candidate.probes = vec![probe(200, Some("echo-a"))];

    assert!(
        diff_gateway(&baseline, &candidate).is_empty(),
        "nothing was lost, so nothing may be claimed"
    );
    let comparability = gateway_comparability(&baseline, &candidate);
    assert!(!comparability.is_comparable());
    assert!(
        comparability.absence_is_evidence(),
        "an unconverged but current status is weak evidence, not absent evidence"
    );
    assert_eq!(
        gateway_evidence_level(&candidate),
        GatewayEvidenceLevel::Unconverged
    );
}

/// Fails if two sides that both timed out start producing claims. With
/// no converged reference, `reconcile.rs`'s own reading applies: a route
/// that times out on both sides is a broken fixture or a slow machine,
/// not a behavior change.
#[test]
fn two_timed_out_sides_are_incomparable_and_claim_nothing() {
    let route = |status: &str| {
        route_object(
            1,
            vec![parent(
                "lab-gateway",
                Some("http"),
                vec![
                    condition(CONDITION_ACCEPTED, status, "Pending", 1),
                    condition(CONDITION_RESOLVED_REFS, "Unknown", "Pending", 1),
                ],
            )],
        )
    };
    let gateway = golden("gateway-programmed.yaml");
    let class = golden("gatewayclass-accepted.yaml");
    let baseline = case(&gateway, Some(&class), &route("Unknown"), false);
    let candidate = case(&gateway, Some(&class), &route("False"), false);

    assert!(diff_gateway(&baseline, &candidate).is_empty());
    assert_eq!(
        gateway_comparability(&baseline, &candidate),
        GatewayComparability::Incomparable {
            baseline: GatewayEvidenceLevel::Unconverged,
            candidate: GatewayEvidenceLevel::Unconverged,
        }
    );
}

/// Step 8: fails if a condition missing from a *stale* candidate status
/// is claimed as a removal. The controller has not processed the current
/// spec, so what it has not said proves nothing -- `conditions.rs`'s
/// "`Missing` is never `False`" rule, carried into the comparison.
#[test]
fn a_condition_absent_from_a_stale_status_is_not_proof_of_removal() {
    let baseline = converged_case();
    // Generation 2, but the controller's status still describes
    // generation 1 -- and publishes no `ResolvedRefs` at all.
    let candidate = case(
        &golden("gateway-programmed.yaml"),
        Some(&golden("gatewayclass-accepted.yaml")),
        &route_object(
            2,
            vec![parent(
                "lab-gateway",
                Some("http"),
                vec![condition(CONDITION_ACCEPTED, "True", "Accepted", 1)],
            )],
        ),
        false,
    );

    assert_eq!(
        gateway_evidence_level(&candidate),
        GatewayEvidenceLevel::Stale
    );
    assert!(!gateway_comparability(&baseline, &candidate).absence_is_evidence());
    assert!(
        diff_gateway(&baseline, &candidate).is_empty(),
        "a stale status's silence is not a removal"
    );
}

/// Step 8's other half: fails if staleness silences a difference between
/// two values both sides actually published. A stale status is still a
/// statement; only its *silences* are uninformative.
#[test]
fn a_stale_status_still_reports_what_it_did_publish() {
    let baseline = converged_case();
    let candidate = case(
        &golden("gateway-programmed.yaml"),
        Some(&golden("gatewayclass-accepted.yaml")),
        &route_object(
            2,
            vec![parent(
                "lab-gateway",
                Some("http"),
                vec![
                    condition(CONDITION_ACCEPTED, "False", "NotAllowedByListeners", 1),
                    condition(CONDITION_RESOLVED_REFS, "True", "ResolvedRefs", 1),
                ],
            )],
        ),
        false,
    );

    let changes = diff_gateway(&baseline, &candidate);
    let change = only(&changes);
    assert_eq!(change.kind, SemanticChangeKind::AcceptedConditionChanged);
    assert_eq!(
        change.candidate.as_ref().unwrap()["freshness"],
        json!("stale"),
        "the payload must say the observation is stale rather than hiding it"
    );
}

/// Fails if a parent absent from a stale candidate status is claimed as
/// a detachment -- the same rule as for a condition, and the shape in
/// which it would most confidently fabricate a regression.
#[test]
fn a_parent_absent_from_a_stale_status_is_not_proof_of_detachment() {
    let gateway = golden("gateway-programmed.yaml");
    let class = golden("gatewayclass-accepted.yaml");
    let baseline = case(
        &gateway,
        Some(&class),
        &route_object(
            1,
            vec![
                parent("lab-gateway", Some("http"), accepted_and_resolved(1)),
                parent("lab-gateway-b", Some("http"), accepted_and_resolved(1)),
            ],
        ),
        true,
    );
    let candidate = case(
        &gateway,
        Some(&class),
        &route_object(
            2,
            vec![parent(
                "lab-gateway",
                Some("http"),
                accepted_and_resolved(1),
            )],
        ),
        false,
    );

    assert!(diff_gateway(&baseline, &candidate).is_empty());
}

/// Step 6: fails if a condition that moves *to* `True` stops being
/// marked an improvement. The marker is all this crate does -- the
/// downgrade to `Info` is `admissionlab_policy::default_change_severity`,
/// which is asserted in that crate's own tests.
#[test]
fn a_condition_that_reaches_true_is_marked_an_improvement() {
    let gateway = golden("gateway-programmed.yaml");
    let class = golden("gatewayclass-accepted.yaml");
    let rejected = case(
        &gateway,
        Some(&class),
        &route_object(
            1,
            vec![parent(
                "lab-gateway",
                Some("http"),
                vec![
                    condition(CONDITION_ACCEPTED, "False", "NotAllowedByListeners", 1),
                    condition(CONDITION_RESOLVED_REFS, "True", "ResolvedRefs", 1),
                ],
            )],
        ),
        true,
    );
    let accepted = converged_case();

    let changes = diff_gateway(&rejected, &accepted);
    let change = only(&changes);
    assert_eq!(change.kind, SemanticChangeKind::AcceptedConditionChanged);
    assert_eq!(change.direction(), Some(ChangeDirection::Improvement));
    assert_eq!(
        change.candidate.as_ref().unwrap()["direction"],
        json!("improvement"),
        "the direction must be in the payload, not only in this crate's head"
    );
    assert!(
        change.baseline.as_ref().unwrap().get("direction").is_none(),
        "direction describes the side the transition moved to"
    );
}

/// Fails if two entries claiming one parent identity are silently
/// resolved by position -- `parent_for`'s `Ambiguous` case, which must
/// not become a verdict that depends on list order.
#[test]
fn an_ambiguous_parent_identity_is_not_compared() {
    let gateway = golden("gateway-programmed.yaml");
    let class = golden("gatewayclass-accepted.yaml");
    let duplicated = |first: &str, second: &str| {
        route_object(
            1,
            vec![
                parent(
                    "lab-gateway",
                    Some("http"),
                    vec![
                        condition(CONDITION_ACCEPTED, first, "Accepted", 1),
                        condition(CONDITION_RESOLVED_REFS, "True", "ResolvedRefs", 1),
                    ],
                ),
                parent(
                    "lab-gateway",
                    Some("http"),
                    vec![
                        condition(CONDITION_ACCEPTED, second, "Accepted", 1),
                        condition(CONDITION_RESOLVED_REFS, "True", "ResolvedRefs", 1),
                    ],
                ),
            ],
        )
    };
    let baseline = case(&gateway, Some(&class), &duplicated("True", "False"), true);
    let candidate = case(&gateway, Some(&class), &duplicated("False", "True"), true);

    assert!(
        diff_gateway(&baseline, &candidate).is_empty(),
        "picking one of two entries by position would flip this verdict"
    );
}

/// Fails if the comparator's output stops being deterministic, or if the
/// documented stage order (class, Gateway, membership, parent
/// conditions, probes) changes -- a report's rows must not move between
/// two runs of the same comparison.
#[test]
fn output_is_deterministic_and_follows_the_documented_stage_order() {
    let mut baseline = converged_case();
    baseline.probes = vec![probe(200, Some("echo-a"))];
    let mut candidate = case(
        &golden("gateway-not-programmed.yaml"),
        Some(&golden("gatewayclass-pending.yaml")),
        &route_object(
            1,
            vec![
                parent(
                    "lab-gateway",
                    Some("http"),
                    vec![
                        condition(CONDITION_ACCEPTED, "False", "NotAllowedByListeners", 1),
                        condition(CONDITION_RESOLVED_REFS, "False", "BackendNotFound", 1),
                    ],
                ),
                parent("lab-gateway-b", Some("http"), accepted_and_resolved(1)),
            ],
        ),
        true,
    );
    candidate.probes = vec![probe(503, Some("echo-b"))];

    let changes = diff_gateway(&baseline, &candidate);
    assert_eq!(
        kinds(&changes),
        [
            // Stage 1: the GatewayClass.
            "accepted_condition_changed",
            // Stage 2: the Gateway itself.
            "programmed_condition_changed",
            // Stage 3: parent membership.
            "route_attached",
            // Stage 4: the parent both sides published.
            "accepted_condition_changed",
            "backend_resolution_changed",
            "resolved_refs_condition_changed",
            // Stage 5: the probes.
            "traffic_status_changed",
            "traffic_backend_changed",
        ]
    );
    assert_eq!(
        changes,
        diff_gateway(&baseline, &candidate),
        "repeating the same comparison must give an identical result"
    );
}

/// Fails if the `Gateway`'s own conditions stop being compared in
/// `REQUIRED_GATEWAY_CONDITIONS` order (`Accepted` before `Programmed`),
/// which is what makes two changes about one object read in a fixed
/// order.
#[test]
fn gateway_conditions_are_compared_in_required_order() {
    let baseline = converged_case();
    let candidate = case(
        &json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "Gateway",
            "metadata": {"name": "lab-gateway", "namespace": "gateway-lab", "generation": 1},
            "spec": {"gatewayClassName": "istio"},
            "status": {"conditions": [
                condition(CONDITION_PROGRAMMED, "False", "Pending", 1),
                condition(CONDITION_ACCEPTED, "False", "Invalid", 1),
            ]},
        }),
        Some(&golden("gatewayclass-accepted.yaml")),
        &golden("httproute-accepted.yaml"),
        true,
    );

    assert_eq!(
        kinds(&diff_gateway(&baseline, &candidate)),
        ["accepted_condition_changed", "programmed_condition_changed"],
        "the compared order must not follow the order the cluster listed them in"
    );
}

/// Fails if changes stop carrying the unattributed sentinel, or if
/// `attributed_to` stops being the seam that replaces it. A report that
/// renders `unattributed` is a report whose caller forgot to stamp.
#[test]
fn changes_carry_the_unattributed_sentinel_until_a_caller_stamps_them() {
    let baseline = converged_case();
    let candidate = case(
        &golden("gateway-not-programmed.yaml"),
        Some(&golden("gatewayclass-accepted.yaml")),
        &golden("httproute-accepted.yaml"),
        true,
    );

    let changes = diff_gateway(&baseline, &candidate);
    assert_eq!(only(&changes).fixture_id.as_str(), UNATTRIBUTED_FIXTURE);

    let stamped = changes[0]
        .clone()
        .attributed_to(&FixtureId::parse("gateway-echo").unwrap());
    assert_eq!(stamped.fixture_id.as_str(), "gateway-echo");
}

/// Fails if [`GatewayCaseComparison`] stops agreeing with the free
/// functions, or if `probe_pairs` fabricates a half for a probe only one
/// side answered.
#[test]
fn the_comparison_type_pairs_probes_by_index_and_never_invents_one() {
    let mut baseline = converged_case();
    baseline.probes = vec![probe(200, Some("echo-a")), probe(404, None)];
    let mut candidate = converged_case();
    candidate.probes = vec![probe(200, Some("echo-a"))];

    let comparison = GatewayCaseComparison {
        baseline: baseline.clone(),
        candidate: candidate.clone(),
    };
    assert!(comparison.is_paired());
    assert_eq!(comparison.contract_id(), "echo-a-root");
    assert_eq!(comparison.comparability(), GatewayComparability::Comparable);
    assert_eq!(comparison.changes(), diff_gateway(&baseline, &candidate));

    let pairs = comparison.probe_pairs();
    assert_eq!(pairs.len(), 1, "only the index both sides answered pairs");
    assert_eq!(pairs[0].contract_id, "echo-a-root");
    assert_eq!(pairs[0].baseline.status, 200);

    // The unpaired baseline probe is a claim, not a pair.
    assert_eq!(kinds(&comparison.changes()), ["traffic_status_changed"]);

    let mispaired = GatewayCaseComparison {
        baseline,
        candidate: GatewayCaseResult {
            contract_id: "other-route".to_owned(),
            ..candidate
        },
    };
    assert!(!mispaired.is_paired());
}

/// Fails if a probe only the *candidate* answered is claimed while the
/// baseline never converged: a side with no data plane to probe has not
/// "lost" anything by being silent, so the claim would run backwards.
#[test]
fn a_probe_only_an_unconverged_baseline_missed_is_not_claimed() {
    let mut baseline = case(
        &golden("gateway-programmed.yaml"),
        Some(&golden("gatewayclass-accepted.yaml")),
        &golden("httproute-accepted.yaml"),
        false,
    );
    baseline.probes = Vec::new();
    let mut candidate = converged_case();
    candidate.probes = vec![probe(200, Some("echo-a"))];

    assert!(diff_gateway(&baseline, &candidate).is_empty());
}

/// Fails if the comparability or evidence-level wire tags drift. Both
/// reach a report alongside the changes they qualify, so a renderer and
/// a CI job read these strings.
#[test]
fn comparability_wire_tags_are_pinned() {
    assert_eq!(
        serde_json::to_value(GatewayComparability::Comparable).unwrap(),
        json!("comparable")
    );
    assert_eq!(
        serde_json::to_value(GatewayComparability::Partial {
            baseline: GatewayEvidenceLevel::Converged,
            candidate: GatewayEvidenceLevel::Stale,
        })
        .unwrap(),
        json!({"partial": {"baseline": "converged", "candidate": "stale"}})
    );
    assert_eq!(
        serde_json::to_value(GatewayComparability::Incomparable {
            baseline: GatewayEvidenceLevel::Unconverged,
            candidate: GatewayEvidenceLevel::Unconverged,
        })
        .unwrap(),
        json!({"incomparable": {"baseline": "unconverged", "candidate": "unconverged"}})
    );
}
