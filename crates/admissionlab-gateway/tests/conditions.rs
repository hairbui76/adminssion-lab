//! ROADMAP Task 6.3: normalized Gateway API condition evidence.
//!
//! Every test that reads a status reads a **checked-in golden file**
//! from `testdata/objects/gateway-status/`, never a JSON literal built
//! inline. That is the point of Step 4: an inline literal is written by
//! whoever writes the assertion and will agree with it by construction,
//! while a golden file is a realistic Gateway API v1 object whose shape
//! is fixed by the upstream API rather than by this test. Each golden's
//! header comment records exactly which upstream types, condition types
//! and reason constants its shape comes from -- so "is this what a real
//! cluster looks like?" is answerable by reading the fixture, not by
//! trusting the parser.
//!
//! The four states (`True`, `False`, `Unknown`, `Missing`) and staleness
//! each have their own golden, because those are the distinctions a
//! reader could silently collapse and still pass a happy-path test.

use std::path::PathBuf;

use admissionlab_gateway::conditions::{
    CONDITION_ACCEPTED, CONDITION_PROGRAMMED, CONDITION_RESOLVED_REFS, ConditionFreshness,
    ConditionState, ParentLookup, gateway_class_evidence, gateway_evidence, route_evidence,
};
use admissionlab_gateway::{GatewayError, RouteContract};

/// Loads one golden status object from
/// `testdata/objects/gateway-status/`, which lives at the workspace root
/// (two levels above this crate's own `CARGO_MANIFEST_DIR`).
fn golden(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/objects/gateway-status")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read golden {}: {error}", path.display()));
    serde_norway::from_str(&text)
        .unwrap_or_else(|error| panic!("parse golden {}: {error}", path.display()))
}

/// A contract naming `gateway-lab/lab-gateway`, optionally narrowed to
/// one listener. The route fields are filled in but never read by the
/// parent lookup, which only uses the Gateway identity and the listener.
fn contract(listener: Option<&str>) -> RouteContract {
    RouteContract {
        id: "echo".to_string(),
        gateway_namespace: "gateway-lab".to_string(),
        gateway_name: "lab-gateway".to_string(),
        route_namespace: "gateway-lab".to_string(),
        route_name: "echo-a".to_string(),
        listener_name: listener.map(str::to_owned),
        probes: Vec::new(),
    }
}

// =========================================================================
// Step 1: conditions are found by type, never by position
// =========================================================================

#[test]
fn conditions_are_read_by_type_not_by_list_order() {
    // `httproute-accepted.yaml` writes `ResolvedRefs` *before* `Accepted`
    // -- the reverse of the usual printed order -- specifically so a
    // reader that indexed into the list would fail here.
    let route = route_evidence(&golden("httproute-accepted.yaml")).expect("parse");
    let parent = match route.parent_for(&contract(Some("http"))) {
        ParentLookup::Found(parent) => parent,
        other => panic!("expected exactly one matching parent, got {other:?}"),
    };

    assert_eq!(
        parent.condition(CONDITION_ACCEPTED).state,
        ConditionState::True
    );
    assert_eq!(
        parent.condition(CONDITION_RESOLVED_REFS).state,
        ConditionState::True
    );
    assert_eq!(
        parent.condition(CONDITION_ACCEPTED).reason.as_deref(),
        Some("Accepted"),
        "the reason must come from the Accepted entry, not from whichever entry was first"
    );
    assert_eq!(
        parent.condition(CONDITION_RESOLVED_REFS).reason.as_deref(),
        Some("ResolvedRefs")
    );

    // The Gateway golden also writes `Programmed` before `Accepted`.
    let gateway = gateway_evidence(&golden("gateway-programmed.yaml")).expect("parse");
    assert_eq!(
        gateway.condition(CONDITION_ACCEPTED).state,
        ConditionState::True
    );
    assert_eq!(
        gateway.condition(CONDITION_PROGRAMMED).reason.as_deref(),
        Some("Programmed")
    );
}

// =========================================================================
// Step 2: reason is kept; message never becomes a contract
// =========================================================================

#[test]
fn reason_is_preserved_and_message_is_not_stored_at_all() {
    // `gateway-programmed.yaml`'s Programmed condition carries a long,
    // implementation-specific message ("Resource programmed, assigned to
    // service in the \"gateway-lab\" namespace"). The reason must
    // survive; the message must not appear anywhere in the normalized
    // evidence -- not as a field, and so not in its serialized form
    // either, which is what a structural comparator or a golden
    // snapshot would see.
    let raw = golden("gateway-programmed.yaml");
    let message = raw
        .pointer("/status/conditions/0/message")
        .and_then(serde_json::Value::as_str)
        .expect("the golden carries a message to begin with");
    assert!(message.contains("Resource programmed"));

    let gateway = gateway_evidence(&raw).expect("parse");
    assert_eq!(
        gateway.condition(CONDITION_PROGRAMMED).reason.as_deref(),
        Some("Programmed"),
        "reason is a CamelCase token from a closed set and is kept"
    );

    let serialized = serde_json::to_string(&gateway).expect("serialize evidence");
    assert!(
        !serialized.contains("Resource programmed"),
        "free-form message text must not reach the normalized evidence, or it becomes a \
         pass/fail contract the first time something walks these fields; got {serialized}"
    );
    assert!(
        !serialized.contains("message"),
        "there must be no message field at all; got {serialized}"
    );
}

// =========================================================================
// Step 4: the four states, each from its own realistic golden
// =========================================================================

#[test]
fn a_true_condition_reads_as_true() {
    let gateway = gateway_evidence(&golden("gateway-programmed.yaml")).expect("parse");
    let programmed = gateway.condition(CONDITION_PROGRAMMED);

    assert_eq!(programmed.state, ConditionState::True);
    assert!(programmed.state.is_settled());
    assert_eq!(programmed.observed_generation, Some(1));
    assert_eq!(
        programmed.freshness(gateway.generation),
        ConditionFreshness::Current
    );
}

#[test]
fn a_false_condition_reads_as_false_and_is_still_settled() {
    // `Programmed: False / AddressNotAssigned` alongside
    // `Accepted: True`: a valid object no data plane was told about.
    // `is_settled` is `true` on purpose -- the controller reached a
    // verdict, and deciding whether that verdict is a *regression* is
    // Task 6.9's job, not this module's.
    let gateway = gateway_evidence(&golden("gateway-not-programmed.yaml")).expect("parse");

    assert_eq!(
        gateway.condition(CONDITION_ACCEPTED).state,
        ConditionState::True
    );
    let programmed = gateway.condition(CONDITION_PROGRAMMED);
    assert_eq!(programmed.state, ConditionState::False);
    assert!(
        programmed.state.is_settled(),
        "a definitive False is a settled verdict, not an absence of one"
    );
    assert_eq!(programmed.reason.as_deref(), Some("AddressNotAssigned"));
}

#[test]
fn an_unknown_condition_is_not_false() {
    // The distinction that stops a transient mid-reconcile poll from
    // being reported as a rejection.
    let gateway = gateway_evidence(&golden("gateway-unknown-programmed.yaml")).expect("parse");
    let programmed = gateway.condition(CONDITION_PROGRAMMED);

    assert_eq!(programmed.state, ConditionState::Unknown);
    assert_ne!(programmed.state, ConditionState::False);
    assert!(
        !programmed.state.is_settled(),
        "\"the controller cannot tell yet\" is not a verdict"
    );
    assert_eq!(programmed.reason.as_deref(), Some("Pending"));
}

#[test]
fn an_absent_condition_is_missing_not_false_or_unknown() {
    // `gateway-missing-programmed.yaml` publishes Accepted and nothing
    // else. All three of these assertions matter: collapsing Missing
    // into False would invent a rejection, and collapsing it into
    // Unknown would claim the controller said something it did not.
    let gateway = gateway_evidence(&golden("gateway-missing-programmed.yaml")).expect("parse");
    let programmed = gateway.condition(CONDITION_PROGRAMMED);

    assert_eq!(programmed.state, ConditionState::Missing);
    assert_ne!(programmed.state, ConditionState::False);
    assert_ne!(programmed.state, ConditionState::Unknown);
    assert!(!programmed.state.is_settled());
    assert_eq!(
        programmed.type_name, CONDITION_PROGRAMMED,
        "a missing condition still knows which condition it is about"
    );
    assert!(
        programmed.reason.is_none() && programmed.observed_generation.is_none(),
        "a condition nobody published carries no reason and no generation"
    );

    // A route can be missing a condition too, and for the same reason: a
    // controller that could not attach the route has nothing to say
    // about its backends.
    let route = route_evidence(&golden("httproute-not-accepted.yaml")).expect("parse");
    let parent = &route.parents[0];
    assert_eq!(
        parent.condition(CONDITION_ACCEPTED).state,
        ConditionState::False
    );
    assert_eq!(
        parent.condition(CONDITION_ACCEPTED).reason.as_deref(),
        Some("NoMatchingParent")
    );
    assert_eq!(
        parent.condition(CONDITION_RESOLVED_REFS).state,
        ConditionState::Missing
    );
}

// =========================================================================
// Step 3: staleness
// =========================================================================

#[test]
fn an_older_observed_generation_is_stale_even_when_every_condition_is_true() {
    // The trap: `gateway-stale-status.yaml` is generation 3 with an
    // all-`True` status published for generation 2. A reader that only
    // looked at `status` would call this fully programmed.
    let gateway = gateway_evidence(&golden("gateway-stale-status.yaml")).expect("parse");

    assert_eq!(gateway.generation, 3);
    for type_name in [CONDITION_ACCEPTED, CONDITION_PROGRAMMED] {
        let condition = gateway.condition(type_name);
        assert_eq!(condition.state, ConditionState::True, "{type_name}");
        assert_eq!(
            condition.freshness(gateway.generation),
            ConditionFreshness::Stale,
            "{type_name} was set based on generation 2 but the object is at generation 3"
        );
    }

    // Same on a route's parent status, which is where Task 6.4's
    // convergence rule actually applies it.
    let route = route_evidence(&golden("httproute-stale-status.yaml")).expect("parse");
    assert_eq!(route.generation, 4);
    let parent = match route.parent_for(&contract(Some("http"))) {
        ParentLookup::Found(parent) => parent,
        other => panic!("expected one parent, got {other:?}"),
    };
    assert_eq!(
        parent
            .condition(CONDITION_ACCEPTED)
            .freshness(route.generation),
        ConditionFreshness::Stale
    );
}

#[test]
fn an_absent_observed_generation_is_unknown_not_current() {
    // `gatewayclass-pending.yaml`'s Accepted condition omits
    // `observedGeneration` entirely -- the state Gateway API's own
    // initial `Pending` condition is in. Treating that as current would
    // let a never-reconciled object pass a freshness check.
    let class = gateway_class_evidence(&golden("gatewayclass-pending.yaml")).expect("parse");

    assert_eq!(class.name, "unclaimed");
    assert_eq!(class.accepted.state, ConditionState::Unknown);
    assert!(class.accepted.observed_generation.is_none());
    assert_eq!(class.accepted.freshness(1), ConditionFreshness::Unknown);
    assert_ne!(class.accepted.freshness(1), ConditionFreshness::Current);
}

#[test]
fn a_missing_condition_has_unknown_freshness() {
    let gateway = gateway_evidence(&golden("gateway-missing-programmed.yaml")).expect("parse");

    assert_eq!(
        gateway
            .condition(CONDITION_PROGRAMMED)
            .freshness(gateway.generation),
        ConditionFreshness::Unknown,
        "a condition nobody published cannot be fresh or stale"
    );
    assert_eq!(
        gateway
            .condition(CONDITION_ACCEPTED)
            .freshness(gateway.generation),
        ConditionFreshness::Current
    );
}

#[test]
fn freshness_merge_reports_the_strongest_finding() {
    use ConditionFreshness::{Current, Stale, Unknown};

    // Stale beats everything: it is a positive finding, while Unknown is
    // only an absence of information, so reporting Unknown when
    // something is provably stale would hide the stronger fact.
    assert_eq!(Stale.merge(Unknown), Stale);
    assert_eq!(Unknown.merge(Stale), Stale);
    assert_eq!(Stale.merge(Current), Stale);
    assert_eq!(Unknown.merge(Current), Unknown);
    assert_eq!(Current.merge(Unknown), Unknown);
    assert_eq!(Current.merge(Current), Current);
}

// =========================================================================
// Parent lookup
// =========================================================================

#[test]
fn an_absent_parent_namespace_defaults_to_the_routes_own_namespace() {
    // Gateway API's own defaulting rule. `httproute-backend-not-found.yaml`
    // writes a `parentRef` with no namespace; a contract naming
    // `gateway-lab` (the route's namespace) must match it, and one
    // naming a different namespace must not -- so an absent namespace is
    // resolved, not treated as a wildcard.
    let route = route_evidence(&golden("httproute-backend-not-found.yaml")).expect("parse");
    assert_eq!(route.parents[0].parent.namespace, None);

    match route.parent_for(&contract(Some("http"))) {
        ParentLookup::Found(parent) => {
            assert_eq!(
                parent.condition(CONDITION_RESOLVED_REFS).state,
                ConditionState::False
            );
            assert_eq!(
                parent.condition(CONDITION_RESOLVED_REFS).reason.as_deref(),
                Some("BackendNotFound")
            );
            assert_eq!(
                parent.controller_name.as_deref(),
                Some("istio.io/gateway-controller")
            );
        }
        other => panic!("expected the default-namespace parent to match, got {other:?}"),
    }

    let elsewhere = RouteContract {
        gateway_namespace: "istio-system".to_string(),
        ..contract(Some("http"))
    };
    assert_eq!(
        route.parent_for(&elsewhere),
        ParentLookup::Absent,
        "an absent parentRef.namespace means the route's own namespace, never any namespace"
    );
}

#[test]
fn several_matching_parents_are_ambiguous_not_first_wins() {
    // `httproute-two-parents.yaml` has two entries for the same Gateway
    // that *disagree* (`Accepted: True` on `http`, `Accepted: False` on
    // `http-alt`). Without a listener the contract matches both, and
    // taking the first would make the answer depend on list order.
    let route = route_evidence(&golden("httproute-two-parents.yaml")).expect("parse");

    assert_eq!(
        route.parent_for(&contract(None)),
        ParentLookup::Ambiguous(2),
        "a contract that does not name a listener must not silently pick one of two parents"
    );

    // Naming the listener resolves it, and the two listeners really do
    // give different answers.
    match route.parent_for(&contract(Some("http"))) {
        ParentLookup::Found(parent) => assert_eq!(
            parent.condition(CONDITION_ACCEPTED).state,
            ConditionState::True
        ),
        other => panic!("expected the http listener's entry, got {other:?}"),
    }
    match route.parent_for(&contract(Some("http-alt"))) {
        ParentLookup::Found(parent) => {
            assert_eq!(
                parent.condition(CONDITION_ACCEPTED).state,
                ConditionState::False
            );
            assert_eq!(
                parent.condition(CONDITION_ACCEPTED).reason.as_deref(),
                Some("NotAllowedByListeners")
            );
        }
        other => panic!("expected the http-alt listener's entry, got {other:?}"),
    }
    assert_eq!(
        route.parent_for(&contract(Some("no-such-listener"))),
        ParentLookup::Absent
    );
}

#[test]
fn a_route_with_no_status_parses_to_zero_parents() {
    // The state every freshly applied route is in, and the first thing
    // Task 6.4's waiter sees on every healthy run. It must be a valid
    // observation, not a parse failure.
    let route = route_evidence(&golden("httproute-no-status.yaml")).expect("parse");

    assert_eq!(route.namespace, "gateway-lab");
    assert_eq!(route.name, "echo-a");
    assert_eq!(route.generation, 1);
    assert!(route.parents.is_empty());
    assert_eq!(
        route.parent_for(&contract(Some("http"))),
        ParentLookup::Absent
    );
}

// =========================================================================
// Everything else about the objects
// =========================================================================

#[test]
fn a_gateway_reports_its_identity_and_its_class() {
    let gateway = gateway_evidence(&golden("gateway-programmed.yaml")).expect("parse");

    assert_eq!(gateway.identity.namespace, "gateway-lab");
    assert_eq!(gateway.identity.name, "lab-gateway");
    assert_eq!(gateway.identity.to_string(), "gateway-lab/lab-gateway");
    assert_eq!(
        gateway.gateway_class_name.as_deref(),
        Some("istio"),
        "Task 6.4 polls the GatewayClass only when the Gateway names one, and this is how it \
         knows -- read from the same object read that produced the conditions"
    );
}

#[test]
fn a_gateway_class_reports_its_accepted_condition() {
    let class = gateway_class_evidence(&golden("gatewayclass-accepted.yaml")).expect("parse");

    assert_eq!(class.name, "istio");
    assert_eq!(class.accepted.state, ConditionState::True);
    assert_eq!(class.accepted.reason.as_deref(), Some("Accepted"));
    assert_eq!(class.accepted.freshness(1), ConditionFreshness::Current);
}

#[test]
fn condition_states_serialize_with_kubernetes_own_spellings() {
    // A report that spelled these differently from the cluster it read
    // them from would be needlessly harder to check by hand.
    for (state, expected) in [
        (ConditionState::True, "\"True\""),
        (ConditionState::False, "\"False\""),
        (ConditionState::Unknown, "\"Unknown\""),
        (ConditionState::Missing, "\"Missing\""),
    ] {
        assert_eq!(serde_json::to_string(&state).expect("serialize"), expected);
    }
}

// =========================================================================
// Malformed objects are errors, never guesses
// =========================================================================

#[test]
fn an_out_of_enum_condition_status_is_rejected_not_read_as_unknown() {
    // Mapping an unparseable status onto `Unknown` would put words in a
    // controller's mouth. Gateway API's CRDs enumerate the three legal
    // values, so this is unreachable against a real API server -- which
    // makes rejecting it a check on this project's own parsing.
    let mut object = golden("gateway-programmed.yaml");
    object["status"]["conditions"][0]["status"] = serde_json::json!("true");

    match gateway_evidence(&object).expect_err("an out-of-enum status must be rejected") {
        GatewayError::MalformedStatus { object, reason } => {
            assert!(object.contains("gateway-lab/lab-gateway"), "got {object}");
            assert!(reason.contains("\"true\""), "got {reason}");
        }
        other => panic!("expected MalformedStatus, got {other:?}"),
    }
}

#[test]
fn a_duplicated_condition_type_is_rejected_rather_than_last_one_wins() {
    // Two entries of the same type could disagree, and choosing between
    // them would be a guess. Kubernetes's own condition-list semantics
    // forbid the duplicate in the first place.
    let mut object = golden("gateway-programmed.yaml");
    let duplicate = object["status"]["conditions"][0].clone();
    object["status"]["conditions"]
        .as_array_mut()
        .expect("conditions is a list")
        .push(duplicate);

    match gateway_evidence(&object).expect_err("a duplicated condition type must be rejected") {
        GatewayError::MalformedStatus { reason, .. } => {
            assert!(reason.contains("Programmed"), "got {reason}");
        }
        other => panic!("expected MalformedStatus, got {other:?}"),
    }
}

#[test]
fn an_absent_generation_is_an_error_not_a_fabricated_zero() {
    // `generation` is what every `observedGeneration` is compared
    // against; defaulting it to 0 would turn every status into a
    // fresh-looking one and make the staleness check meaningless.
    let mut object = golden("gateway-programmed.yaml");
    object["metadata"]
        .as_object_mut()
        .expect("metadata is an object")
        .remove("generation");

    match gateway_evidence(&object).expect_err("a missing generation must be rejected") {
        GatewayError::MalformedStatus { reason, .. } => {
            assert!(reason.contains("metadata.generation"), "got {reason}");
        }
        other => panic!("expected MalformedStatus, got {other:?}"),
    }
}

#[test]
fn a_parent_status_entry_without_a_name_is_rejected() {
    let mut object = golden("httproute-accepted.yaml");
    object["status"]["parents"][0]["parentRef"]
        .as_object_mut()
        .expect("parentRef is an object")
        .remove("name");

    match route_evidence(&object).expect_err("a nameless parentRef must be rejected") {
        GatewayError::MalformedStatus { reason, .. } => {
            assert!(reason.contains("parentRef"), "got {reason}");
        }
        other => panic!("expected MalformedStatus, got {other:?}"),
    }
}

#[test]
fn every_golden_file_is_actually_read_by_a_test() {
    // Guards against a golden being added, documented, and then never
    // exercised -- which would make the directory look like coverage it
    // is not. If this fails, either wire the new file into a test or
    // delete it.
    let directory =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/objects/gateway-status");
    let mut names: Vec<String> = std::fs::read_dir(&directory)
        .expect("read golden directory")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();

    assert_eq!(
        names,
        [
            "gateway-missing-programmed.yaml",
            "gateway-not-programmed.yaml",
            "gateway-programmed.yaml",
            "gateway-stale-status.yaml",
            "gateway-unknown-programmed.yaml",
            "gatewayclass-accepted.yaml",
            "gatewayclass-pending.yaml",
            "httproute-accepted.yaml",
            "httproute-backend-not-found.yaml",
            "httproute-no-status.yaml",
            "httproute-not-accepted.yaml",
            "httproute-stale-status.yaml",
            "httproute-two-parents.yaml",
        ],
        "add the new golden to a test (and to this list), or remove it"
    );

    // And every one of them parses, which is what makes the list above a
    // claim about realistic shapes rather than about filenames.
    for name in &names {
        let object = golden(name);
        let kind = object["kind"].as_str().expect("every golden names a kind");
        let parsed = match kind {
            "Gateway" => gateway_evidence(&object).map(|_| ()),
            "GatewayClass" => gateway_class_evidence(&object).map(|_| ()),
            "HTTPRoute" => route_evidence(&object).map(|_| ()),
            other => panic!("{name}: unexpected kind {other}"),
        };
        parsed.unwrap_or_else(|error| panic!("{name} must parse: {error}"));
    }
}
