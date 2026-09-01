//! ROADMAP Task 6.1: the Gateway fixture and traffic-contract model.
//!
//! These tests drive the model through **this crate's re-exports**
//! (`admissionlab_gateway::model`), not through `admissionlab-spec`
//! directly, on purpose: the surface Tasks 6.2-6.9 will consume is the
//! one named here, so if the re-export were ever replaced by a locally
//! declared twin (the synonym §1.2's registry forbids -- see
//! `src/model.rs`'s documentation), these tests would keep passing
//! against the wrong type only if the twin were kept perfectly in step.
//! `reexported_types_are_the_spec_crate_types` closes that gap by
//! asserting the identity directly.
//!
//! Loading always goes through the real `load_lab` + `resolve_lab`
//! pipeline against a file on disk, never a hand-built struct: Task 6.1
//! Step 1 asks for *config* tests, and a struct literal would skip
//! exactly the parsing and validation being tested.

use std::path::PathBuf;
use std::time::Duration;

use admissionlab_gateway::model::{
    ALLOWED_HTTP_METHODS, DEFAULT_RECONCILIATION_TIMEOUT, GatewayIdentity, GatewaySuiteSpec,
    HttpProbeContract, RouteContract, contract_gateway_identity, is_valid_http_status,
};
use admissionlab_spec::{SpecError, load_lab, resolve_lab};

/// The checked-in, fully valid Gateway lab configuration
/// (`testdata/configs/gateway-valid.yaml`), which lives at the workspace
/// root -- two levels above this crate's own `CARGO_MANIFEST_DIR`.
fn gateway_valid_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/configs/gateway-valid.yaml")
}

/// A minimal, otherwise-valid lab document whose `gateway:` section is
/// `gateway_section`, written to a fresh file under the OS temp
/// directory and loaded through the real pipeline.
///
/// Returns the resolved `gateway` suite, or the `SpecError` the pipeline
/// rejected the document with. The surrounding admission-lab fields are
/// the same ones `testdata/configs/minimal-valid.yaml` uses, so nothing
/// outside the `gateway` section can be what fails.
fn resolve_gateway_section(
    label: &str,
    gateway_section: &str,
) -> Result<Option<GatewaySuiteSpec>, SpecError> {
    let document = format!(
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: Lab\n\
         baseline:\n  kubernetes: \"1.33.4\"\n\
         candidate:\n  kubernetes: \"1.33.4\"\n\
         fixtures:\n  include:\n    - \"fixtures/**/*.yaml\"\n\
         {gateway_section}"
    );

    let directory = std::env::temp_dir().join(format!(
        "admissionlab-gateway-model-{label}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).expect("create temp directory");
    let path = directory.join("admissionlab.yaml");
    std::fs::write(&path, document).expect("write temp lab configuration");

    let loaded = load_lab(&path)?;
    resolve_lab(loaded).map(|lab| lab.gateway)
}

/// The `message` of a [`SpecError::Validation`], or a panic naming what
/// arrived instead. Every Task 6.1 rejection is a validation failure
/// (the documents below all *parse* cleanly), so a parse error here
/// means the test is testing something other than what it claims.
fn validation_message(error: &SpecError) -> &str {
    match error {
        SpecError::Validation { message, .. } => message,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// A single-route `gateway:` section with `body` spliced in as the
/// route's own fields, for the many one-field-at-a-time rejection tests.
fn one_route_section(body: &str) -> String {
    format!(
        "gateway:\n  manifests:\n    - fixtures/gateway/all.yaml\n  routes:\n    - {}\n",
        body.trim_start()
    )
}

#[test]
fn checked_in_valid_configuration_loads_and_resolves() {
    // The whole-file happy path: if this fails, every rejection test
    // below is proving nothing, because they would also fail.
    let loaded =
        load_lab(&gateway_valid_path()).expect("testdata/configs/gateway-valid.yaml loads");
    let resolved = resolve_lab(loaded).expect("gateway-valid.yaml resolves");

    let gateway = resolved
        .gateway
        .expect("gateway-valid.yaml declares a gateway suite");

    // `from_secs(90)`, not `from_millis(90_000)`: the *wire* value is
    // 90000 milliseconds (see the file), and
    // `reconciliation_timeout_is_read_as_milliseconds` is what pins that
    // reading; here the point is only the resulting duration.
    assert_eq!(gateway.reconciliation_timeout, Duration::from_secs(90));
    assert_eq!(gateway.manifests.len(), 5);
    assert_eq!(
        gateway
            .routes
            .iter()
            .map(|route| route.id.as_str())
            .collect::<Vec<_>>(),
        ["echo-a-root", "echo-b-prefix", "echo-b-second-listener"],
        "routes keep the order they were written in"
    );

    let first = &gateway.routes[0];
    assert_eq!(first.listener_name.as_deref(), Some("http"));
    assert_eq!(first.probes.len(), 2);
    assert_eq!(first.probes[0].expected_backend.as_deref(), Some("echo-a"));
    assert_eq!(
        first.probes[1]
            .headers
            .get("x-lab-probe")
            .map(String::as_str),
        Some("reconciliation"),
        "probe headers survive resolution"
    );

    let unmatched = &gateway.routes[1].probes[1];
    assert_eq!(unmatched.expected_status, 404);
    assert!(
        unmatched.expected_backend.is_none(),
        "a probe that expects no backend to be reached must not invent one"
    );

    assert!(
        gateway.routes[2].probes.is_empty(),
        "a reconciliation-only contract carries no probes"
    );
}

#[test]
fn manifest_paths_resolve_against_the_configuration_file_directory() {
    // The one thing resolution actually rewrites. Fails if `manifests`
    // were carried through unresolved (which would make a lab break the
    // moment it is run from any other working directory) or resolved
    // against `current_dir` instead.
    let configuration = gateway_valid_path();
    let configuration_directory = configuration
        .parent()
        .expect("the testdata path has a parent");

    let loaded = load_lab(&configuration).expect("load");
    let gateway = resolve_lab(loaded)
        .expect("resolve")
        .gateway
        .expect("gateway suite");

    assert_eq!(
        gateway.manifests[0],
        configuration_directory.join("fixtures/gateway/namespace.yaml")
    );
    for manifest in &gateway.manifests {
        assert!(
            manifest.starts_with(configuration_directory),
            "{} was not resolved against the configuration file's directory",
            manifest.display()
        );
    }
}

#[test]
fn absent_gateway_section_resolves_to_none() {
    // Global Constraint 8: an admission-only lab is the common case, and
    // it must stay writable without any Gateway vocabulary at all.
    let resolved = resolve_gateway_section("absent", "").expect("a lab without a gateway section");
    assert!(resolved.is_none());
}

#[test]
fn reconciliation_timeout_defaults_when_omitted() {
    let gateway = resolve_gateway_section(
        "default-timeout",
        "gateway:\n  manifests:\n    - fixtures/gateway/all.yaml\n  \
         routes:\n    - id: only\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
         routeNamespace: gw\n      routeName: echo\n",
    )
    .expect("a gateway suite that omits reconciliationTimeout")
    .expect("gateway suite");

    assert_eq!(
        gateway.reconciliation_timeout, DEFAULT_RECONCILIATION_TIMEOUT,
        "an omitted reconciliationTimeout must take the documented default, not Duration::ZERO"
    );
    assert_eq!(DEFAULT_RECONCILIATION_TIMEOUT, Duration::from_secs(120));
}

#[test]
fn reconciliation_timeout_is_read_as_milliseconds() {
    // Pins the wire representation: `1500` must mean 1.5 seconds, not
    // 1500 seconds and not serde's default `{secs, nanos}` object.
    let gateway = resolve_gateway_section(
        "millis",
        "gateway:\n  manifests:\n    - m.yaml\n  reconciliationTimeout: 1500\n  \
         routes:\n    - id: only\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
         routeNamespace: gw\n      routeName: echo\n",
    )
    .expect("resolve")
    .expect("gateway suite");

    assert_eq!(gateway.reconciliation_timeout, Duration::from_millis(1500));
}

#[test]
fn zero_reconciliation_timeout_is_rejected() {
    let error = resolve_gateway_section(
        "zero-timeout",
        "gateway:\n  manifests:\n    - m.yaml\n  reconciliationTimeout: 0\n  \
         routes:\n    - id: only\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
         routeNamespace: gw\n      routeName: echo\n",
    )
    .expect_err("a zero reconciliation timeout must be rejected");

    assert!(
        validation_message(&error).starts_with("gateway.reconciliationTimeout:"),
        "message must locate the offending field, got {:?}",
        validation_message(&error)
    );
}

#[test]
fn duplicate_contract_ids_are_rejected() {
    // Task 6.1 Step 1's headline rule. Duplicated ids would silently
    // make baseline/candidate correlation ambiguous later, which is
    // exactly the kind of quiet wrong answer this project must not
    // produce -- so it fails at load time instead.
    let error = resolve_gateway_section(
        "duplicate-ids",
        "gateway:\n  manifests:\n    - m.yaml\n  routes:\n    \
         - id: echo\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
         routeNamespace: gw\n      routeName: echo-a\n    \
         - id: echo\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
         routeNamespace: gw\n      routeName: echo-b\n",
    )
    .expect_err("two contracts sharing an id must be rejected");

    let message = validation_message(&error);
    assert!(
        message.starts_with("gateway.routes[1].id:"),
        "the *second* occurrence must be named, got {message:?}"
    );
    assert!(
        message.contains("duplicate route contract id \"echo\""),
        "the message must quote the duplicated id, got {message:?}"
    );
}

#[test]
fn invalid_http_methods_are_rejected() {
    // A typo, a real-but-unroutable method, and a lowercase spelling of
    // a legal one. The last is the interesting case: accepting `get` by
    // up-casing it would let a configuration probe something other than
    // what it says, since HTTP methods are case-sensitive and Gateway
    // API's HTTPMethod enum lists only uppercase spellings.
    for method in ["GTE", "LINK", "get"] {
        let error = resolve_gateway_section(
            "bad-method",
            &one_route_section(&format!(
                "id: only\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
                 routeNamespace: gw\n      routeName: echo\n      probes:\n        \
                 - host: a.example\n          path: /\n          method: {method}\n          \
                 expectedStatus: 200\n"
            )),
        )
        .expect_err("an HTTP method outside Gateway API's HTTPMethod enum must be rejected");

        let message = validation_message(&error);
        assert!(
            message.starts_with("gateway.routes[0].probes[0].method:"),
            "message must locate the offending probe, got {message:?}"
        );
        assert!(
            message.contains(method),
            "message must quote the rejected method, got {message:?}"
        );
    }
}

#[test]
fn every_allowed_http_method_is_accepted() {
    // The complement of the test above: proves the allow-list is not
    // accidentally narrower than it claims (a rejection test alone
    // passes just as well against a list that rejects everything).
    for method in ALLOWED_HTTP_METHODS {
        let gateway = resolve_gateway_section(
            "good-method",
            &one_route_section(&format!(
                "id: only\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
                 routeNamespace: gw\n      routeName: echo\n      probes:\n        \
                 - host: a.example\n          path: /\n          method: {method}\n          \
                 expectedStatus: 200\n"
            )),
        )
        .unwrap_or_else(|error| panic!("{method} must be accepted, got {error}"))
        .expect("gateway suite");

        assert_eq!(gateway.routes[0].probes[0].method, method);
    }
    assert_eq!(
        ALLOWED_HTTP_METHODS.len(),
        9,
        "Gateway API v1's HTTPMethod enumeration has exactly nine values"
    );
}

#[test]
fn invalid_expected_statuses_are_rejected() {
    // `0` and `99` are below the range; `600` and `1000` are above it.
    // `599` is unassigned but reachable, and must stay legal -- see
    // `is_valid_http_status`'s documentation.
    for status in ["0", "99", "600", "1000"] {
        let error = resolve_gateway_section(
            "bad-status",
            &one_route_section(&format!(
                "id: only\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
                 routeNamespace: gw\n      routeName: echo\n      probes:\n        \
                 - host: a.example\n          path: /\n          method: GET\n          \
                 expectedStatus: {status}\n"
            )),
        )
        .expect_err("a status code outside 100..=599 must be rejected");

        let message = validation_message(&error);
        assert!(
            message.starts_with("gateway.routes[0].probes[0].expectedStatus:"),
            "message must locate the offending probe, got {message:?}"
        );
    }

    for status in [100, 200, 404, 503, 599] {
        assert!(
            is_valid_http_status(status),
            "{status} is a reachable HTTP status code and must stay legal"
        );
    }
    for status in [0, 99, 600, u16::MAX] {
        assert!(!is_valid_http_status(status));
    }
}

#[test]
fn an_absent_gateway_identity_is_a_missing_required_field() {
    // Task 6.1 Step 2, the *absence* half: `gatewayName` has no default
    // and no inference, so a contract that omits it must fail while
    // parsing rather than being filled in from the route's own
    // namespace, from `parentRefs`, or from the only Gateway in the
    // manifest directory. (The empty-string half is
    // `empty_required_route_fields_are_rejected` below.)
    let missing = resolve_gateway_section(
        "missing-gateway",
        &one_route_section(
            "id: only\n      gatewayNamespace: gw\n      routeNamespace: gw\n      \
             routeName: echo\n",
        ),
    )
    .expect_err("a contract without gatewayName must be rejected");

    assert!(
        matches!(missing, SpecError::Parse { .. }),
        "an absent required field is a parse failure, got {missing:?}"
    );
    assert!(
        missing.to_string().contains("gatewayName"),
        "the parse error must name the missing field, got {missing}"
    );
}

#[test]
fn empty_required_route_fields_are_rejected() {
    for (label, field, body) in [
        (
            "empty-gateway-namespace",
            "gatewayNamespace",
            "id: only\n      gatewayNamespace: \"\"\n      gatewayName: lab\n      \
             routeNamespace: gw\n      routeName: echo\n",
        ),
        (
            "empty-gateway-name",
            "gatewayName",
            "id: only\n      gatewayNamespace: gw\n      gatewayName: \"  \"\n      \
             routeNamespace: gw\n      routeName: echo\n",
        ),
        (
            "empty-route-namespace",
            "routeNamespace",
            "id: only\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
             routeNamespace: \"\"\n      routeName: echo\n",
        ),
        (
            "empty-route-name",
            "routeName",
            "id: only\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
             routeNamespace: gw\n      routeName: \"\"\n",
        ),
        (
            "empty-id",
            "id",
            "id: \"\"\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
             routeNamespace: gw\n      routeName: echo\n",
        ),
        (
            // Present-but-empty must not silently widen back to "any
            // listener", which is already spelled by omitting the field.
            "empty-listener",
            "listenerName",
            "id: only\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
             routeNamespace: gw\n      routeName: echo\n      listenerName: \"\"\n",
        ),
        (
            // Same reasoning for `expectedBackend`.
            "empty-backend",
            "expectedBackend",
            "id: only\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
             routeNamespace: gw\n      routeName: echo\n      probes:\n        \
             - host: a.example\n          path: /\n          method: GET\n          \
             expectedStatus: 200\n          expectedBackend: \"\"\n",
        ),
        (
            "empty-host",
            "host",
            "id: only\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
             routeNamespace: gw\n      routeName: echo\n      probes:\n        \
             - host: \"\"\n          path: /\n          method: GET\n          \
             expectedStatus: 200\n",
        ),
    ] {
        let error = resolve_gateway_section(label, &one_route_section(body))
            .expect_err(&format!("an empty {field} must be rejected"));
        let message = validation_message(&error);
        assert!(
            message.contains(field) && message.contains("must not be empty"),
            "{label}: message must name {field}, got {message:?}"
        );
    }
}

#[test]
fn probe_path_must_start_with_a_slash() {
    let error = resolve_gateway_section(
        "bad-path",
        &one_route_section(
            "id: only\n      gatewayNamespace: gw\n      gatewayName: lab\n      \
             routeNamespace: gw\n      routeName: echo\n      probes:\n        \
             - host: a.example\n          path: healthz\n          method: GET\n          \
             expectedStatus: 200\n",
        ),
    )
    .expect_err("a probe path without a leading slash cannot match any HTTPRoute");

    let message = validation_message(&error);
    assert!(
        message.starts_with("gateway.routes[0].probes[0].path:") && message.contains("\"healthz\""),
        "got {message:?}"
    );
}

#[test]
fn empty_manifests_and_routes_are_rejected() {
    // A suite with no manifests installs nothing; a suite with no routes
    // observes nothing. Both would otherwise resolve cleanly, create two
    // clusters, and report success -- the quiet no-op this project
    // exists to catch.
    let no_manifests = resolve_gateway_section(
        "no-manifests",
        "gateway:\n  manifests: []\n  routes:\n    - id: only\n      gatewayNamespace: gw\n      \
         gatewayName: lab\n      routeNamespace: gw\n      routeName: echo\n",
    )
    .expect_err("an empty manifest list must be rejected");
    assert!(validation_message(&no_manifests).starts_with("gateway.manifests:"));

    let no_routes = resolve_gateway_section(
        "no-routes",
        "gateway:\n  manifests:\n    - m.yaml\n  routes: []\n",
    )
    .expect_err("an empty route list must be rejected");
    assert!(validation_message(&no_routes).starts_with("gateway.routes:"));
}

#[test]
fn unknown_fields_in_the_gateway_section_are_rejected() {
    // `deny_unknown_fields` reaches the new types too: a misspelling
    // must be a loud parse error, never a silently ignored key that
    // makes a contract test something other than what was written.
    for section in [
        "gateway:\n  manifest:\n    - m.yaml\n  routes:\n    - id: only\n      \
         gatewayNamespace: gw\n      gatewayName: lab\n      routeNamespace: gw\n      \
         routeName: echo\n",
        "gateway:\n  manifests:\n    - m.yaml\n  routes:\n    - id: only\n      \
         gatewayNamespace: gw\n      gatewayName: lab\n      routeNamespace: gw\n      \
         routeName: echo\n      severity: critical\n",
    ] {
        let error = resolve_gateway_section("unknown-field", section)
            .expect_err("an unknown key must be rejected");
        assert!(
            matches!(error, SpecError::Parse { .. }),
            "expected a parse error, got {error:?}"
        );
    }
}

#[test]
fn contract_gateway_identity_restates_the_contract() {
    // `contract_gateway_identity` must be a projection and nothing more:
    // no defaulting, no fallback to the route's own namespace. A
    // contract that named the wrong Gateway must keep naming it.
    let contract = RouteContract {
        id: "only".to_string(),
        gateway_namespace: "istio-system".to_string(),
        gateway_name: "lab-gateway".to_string(),
        route_namespace: "gateway-lab".to_string(),
        route_name: "echo".to_string(),
        listener_name: None,
        probes: Vec::new(),
    };

    assert_eq!(
        contract_gateway_identity(&contract),
        GatewayIdentity {
            namespace: "istio-system".to_string(),
            name: "lab-gateway".to_string(),
        },
        "the identity comes from gatewayNamespace/gatewayName, never from the route's own \
         namespace"
    );
    assert_eq!(
        contract_gateway_identity(&contract).to_string(),
        "istio-system/lab-gateway"
    );
}

#[test]
fn reexported_types_are_the_spec_crate_types() {
    // The anti-synonym check (§1.2). These assignments only compile
    // while `admissionlab_gateway::model`'s names *are*
    // `admissionlab_spec`'s -- a locally declared twin with identical
    // fields would fail here even though every other test in this file
    // would still pass.
    let probe: HttpProbeContract = admissionlab_spec::HttpProbeContract {
        host: "a.example".to_string(),
        path: "/".to_string(),
        method: "GET".to_string(),
        headers: std::collections::BTreeMap::new(),
        expected_status: 200,
        expected_backend: None,
    };
    let contract: RouteContract = admissionlab_spec::RouteContract {
        id: "only".to_string(),
        gateway_namespace: "gw".to_string(),
        gateway_name: "lab".to_string(),
        route_namespace: "gw".to_string(),
        route_name: "echo".to_string(),
        listener_name: None,
        probes: vec![probe],
    };
    let suite: GatewaySuiteSpec = admissionlab_spec::GatewaySuiteSpec {
        manifests: vec![PathBuf::from("m.yaml")],
        routes: vec![contract],
        reconciliation_timeout: DEFAULT_RECONCILIATION_TIMEOUT,
        gateway_endpoint: None,
        readiness: Vec::new(),
    };

    assert_eq!(suite.routes[0].probes[0].expected_status, 200);
}
