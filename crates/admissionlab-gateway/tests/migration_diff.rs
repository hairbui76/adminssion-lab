//! ROADMAP Task 8.5: the migration behavior comparator.
//!
//! Everything here is synthesized. That is the point: the comparator is
//! a pure function of two results, one case and one set of planned
//! documents, so its whole rule set can be driven exhaustively in
//! milliseconds instead of being sampled by whichever combinations a
//! real cluster happened to produce. The *live* half of this phase is
//! `tests/ingress_e2e.rs`, which proves the baseline evidence these
//! tables are shaped like is the evidence a real `ingress-nginx`
//! actually yields.
//!
//! What is covered, in the order the file is laid out:
//!
//! 1. the seven frozen wire tags, snapshotted;
//! 2. every [`MigrationBehaviorKind`] a traffic comparison can produce,
//!    each through the rule that produces it and each with a
//!    counter-case that must produce *nothing*;
//! 3. probe pairing by index, and what an unpaired probe does not
//!    claim;
//! 4. the annotation catalog: an expected feature, an unexpected one, a
//!    declaration that matched nothing, and the catalog's own
//!    invariants;
//! 5. the incomparable seam, including the denial case Task 8.4 makes
//!    real;
//! 6. determinism.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use admissionlab_gateway::{
    ApplyCategory, GatewayCaseResult, HttpProbeContract, HttpProbeResult, IngressCaseResult,
    MigrationBehaviorChange, MigrationBehaviorKind, MigrationCaseSpec, MigrationComparability,
    NONPORTABLE_INGRESS_ANNOTATIONS, PlannedObject, ReconciliationEvidence, compare_migration_case,
    compare_migration_traffic, gateway_evidence, migration_comparability, nonportable_annotation,
    nonportable_changes, route_evidence, unmatched_nonportable_expectations,
};
use admissionlab_spec::NonPortableFeatureExpectation;

// =====================================================================
// Builders. Every one of them produces a value a real run could have
// produced; nothing here is a shape the engine cannot emit.
// =====================================================================

/// One probe result: the three fields this comparator reads, plus the
/// three it deliberately does not.
fn probe(status: u16, backend: Option<&str>, headers: &[(&str, &str)]) -> HttpProbeResult {
    HttpProbeResult {
        status,
        backend: backend.map(ToOwned::to_owned),
        response_headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        // Deliberately identical on both sides of every pair below, and
        // deliberately never read: see `migration.rs`'s "What the
        // evidence can and cannot show" for why a body hash is not
        // usable evidence here.
        response_body_sha256: "0".repeat(64),
        elapsed: Duration::from_millis(3),
        attempts: 1,
    }
}

/// A redirect response: a status, and the `Location` it carried. The
/// shape most of the redirect table is made of, so each row states the
/// two targets being compared and nothing else.
fn redirect(status: u16, location: &str) -> HttpProbeResult {
    probe(status, None, &[("location", location)])
}

fn contract(path: &str, expected_status: u16) -> HttpProbeContract {
    HttpProbeContract {
        host: "shop.admissionlab.test".to_owned(),
        path: path.to_owned(),
        method: "GET".to_owned(),
        headers: BTreeMap::new(),
        expected_status,
        expected_backend: None,
    }
}

fn case(probes: Vec<HttpProbeContract>) -> MigrationCaseSpec {
    MigrationCaseSpec {
        id: "shop".to_owned(),
        baseline_ingress_manifests: vec![PathBuf::from("ingress/shop.yaml")],
        candidate_gateway_manifests: vec![PathBuf::from("gateway/shop.yaml")],
        probes,
        expected_nonportable: Vec::new(),
    }
}

/// A baseline that was admitted and served -- the only shape a
/// comparable case has.
fn served(probes: Vec<HttpProbeResult>) -> IngressCaseResult {
    IngressCaseResult {
        admitted: true,
        ready: true,
        probes,
        diagnostics: Vec::new(),
    }
}

fn candidate(probes: Vec<HttpProbeResult>) -> GatewayCaseResult {
    GatewayCaseResult {
        contract_id: "shop".to_owned(),
        reconciliation: converged(),
        probes,
    }
}

/// A converged reconciliation, built through this crate's own parsers
/// rather than by filling in a struct literal, so it is a value a real
/// controller status could have produced.
///
/// The migration comparator never reads any of it -- an `Ingress`
/// publishes nothing a `Gateway`'s conditions could be compared against,
/// which is exactly why `MigrationBehaviorKind` has no condition
/// vocabulary -- so it is the least interesting value in this file and
/// is deliberately the same one everywhere.
fn converged() -> ReconciliationEvidence {
    let gateway = serde_json::json!({
        "metadata": {"namespace": "shop", "name": "shop-gateway", "generation": 1},
        "status": {"conditions": [
            {"type": "Accepted", "status": "True", "observedGeneration": 1},
            {"type": "Programmed", "status": "True", "observedGeneration": 1},
        ]},
    });
    let route = serde_json::json!({
        "metadata": {"namespace": "shop", "name": "shop-route", "generation": 1},
        "status": {"parents": [{
            "parentRef": {"namespace": "shop", "name": "shop-gateway"},
            "conditions": [
                {"type": "Accepted", "status": "True", "observedGeneration": 1},
                {"type": "ResolvedRefs", "status": "True", "observedGeneration": 1},
            ],
        }]},
    });
    ReconciliationEvidence {
        gateway_class: None,
        gateway: gateway_evidence(&gateway).expect("the synthesized Gateway status parses"),
        route: route_evidence(&route).expect("the synthesized HTTPRoute status parses"),
        elapsed: Duration::from_millis(700),
        converged: true,
        diagnostics: Vec::new(),
    }
}

/// One planned document, as `plan_gateway_apply` would have produced it.
fn document(kind: &str, name: &str, annotations: &[(&str, &str)]) -> PlannedObject {
    let annotations: serde_json::Map<String, serde_json::Value> = annotations
        .iter()
        .map(|(key, value)| ((*key).to_owned(), serde_json::json!(value)))
        .collect();
    let object = serde_json::json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": kind,
        "metadata": {
            "name": name,
            "namespace": "shop",
            "annotations": annotations,
        },
    });
    PlannedObject {
        source: PathBuf::from("ingress/shop.yaml"),
        document_index: 0,
        api_version: "networking.k8s.io/v1".to_owned(),
        kind: kind.to_owned(),
        name: name.to_owned(),
        namespace: Some("shop".to_owned()),
        category: ApplyCategory::for_kind(kind),
        object,
    }
}

fn kinds(changes: &[MigrationBehaviorChange]) -> Vec<MigrationBehaviorKind> {
    changes.iter().map(|change| change.kind).collect()
}

// =====================================================================
// 1. The frozen vocabulary.
// =====================================================================

/// The seven wire tags, snapshotted. §1.2 freezes the *names*; this
/// freezes what they serialize to, which is what a report and a
/// downstream consumer actually see.
#[test]
fn every_behavior_kind_has_its_frozen_wire_tag() {
    let expected = [
        (
            MigrationBehaviorKind::HostBehaviorChanged,
            "host_behavior_changed",
        ),
        (
            MigrationBehaviorKind::PathBehaviorChanged,
            "path_behavior_changed",
        ),
        (
            MigrationBehaviorKind::TlsBehaviorChanged,
            "tls_behavior_changed",
        ),
        (MigrationBehaviorKind::BackendChanged, "backend_changed"),
        (
            MigrationBehaviorKind::RewriteBehaviorChanged,
            "rewrite_behavior_changed",
        ),
        (
            MigrationBehaviorKind::RedirectBehaviorChanged,
            "redirect_behavior_changed",
        ),
        (
            MigrationBehaviorKind::NonPortableFeature,
            "non_portable_feature",
        ),
    ];
    assert_eq!(expected.len(), 7, "the enum is frozen at seven variants");
    for (kind, tag) in expected {
        assert_eq!(kind.as_str(), tag);
        assert_eq!(kind.to_string(), tag);
        assert_eq!(
            serde_json::to_value(kind).expect("a kind serializes"),
            serde_json::json!(tag),
            "the serialized tag and `as_str` must not drift apart"
        );
    }
}

// =====================================================================
// 2. The traffic rules, one table.
// =====================================================================

/// One row of the traffic table: why it exists, the contract's path
/// (which is what says whether a probe exercises host or path
/// behavior), the two responses, and the kinds the comparator must
/// claim.
type TrafficRow = (
    &'static str,
    &'static str,
    HttpProbeResult,
    HttpProbeResult,
    Vec<MigrationBehaviorKind>,
);

/// The rows in which neither side sent a `Location`: the backend rule
/// and the three arms of the plain status rule, each with the near-miss
/// that must claim nothing.
///
/// Functions rather than `const`s because every row builds an
/// `HttpProbeResult`, and split from the test that drives them so the
/// tables can grow without the assertions growing with them. Split from
/// [`redirect_traffic_table`] along the comparator's own first
/// decision, so a reader can see each half of the rule beside its own
/// evidence.
fn plain_traffic_table() -> Vec<TrafficRow> {
    vec![
        (
            "identical answers claim nothing",
            "/checkout",
            probe(200, Some("shop"), &[]),
            probe(200, Some("shop"), &[]),
            vec![],
        ),
        (
            "a different workload answered",
            "/checkout",
            probe(200, Some("shop"), &[]),
            probe(200, Some("shop-v2"), &[]),
            vec![MigrationBehaviorKind::BackendChanged],
        ),
        (
            "an unidentified backend is not a different one",
            "/checkout",
            probe(200, Some("shop"), &[]),
            probe(200, None, &[]),
            vec![],
        ),
        (
            "a status difference on a path probe is path behavior",
            "/checkout",
            probe(200, Some("shop"), &[]),
            probe(404, None, &[]),
            vec![MigrationBehaviorKind::PathBehaviorChanged],
        ),
        (
            "a status difference on a root probe is host behavior",
            "/",
            probe(200, Some("shop"), &[]),
            probe(404, None, &[]),
            vec![MigrationBehaviorKind::HostBehaviorChanged],
        ),
        (
            "the same backend answering differently means what reached it differed",
            "/api/v1/orders",
            probe(200, Some("shop"), &[]),
            probe(404, Some("shop"), &[]),
            vec![MigrationBehaviorKind::RewriteBehaviorChanged],
        ),
        (
            "two facts about one probe are two changes",
            "/checkout",
            probe(200, Some("shop"), &[]),
            probe(503, Some("shop-v2"), &[]),
            vec![
                MigrationBehaviorKind::BackendChanged,
                MigrationBehaviorKind::PathBehaviorChanged,
            ],
        ),
    ]
}

/// The rows in which at least one side sent a `Location`: the redirect
/// axis, which the comparator consults before the status, because a
/// redirect is an answer rather than a failure to answer.
fn redirect_traffic_table() -> Vec<TrafficRow> {
    vec![
        (
            "one side redirects and the other does not",
            "/legacy",
            redirect(301, "http://shop.admissionlab.test/new"),
            probe(200, Some("shop"), &[]),
            vec![MigrationBehaviorKind::RedirectBehaviorChanged],
        ),
        (
            "both redirect to the same place with different codes",
            "/legacy",
            redirect(301, "http://shop.admissionlab.test/new"),
            redirect(308, "http://shop.admissionlab.test/new"),
            vec![MigrationBehaviorKind::RedirectBehaviorChanged],
        ),
        (
            "the redirect scheme moved: ssl-redirect against a filter that names no scheme",
            "/secure",
            redirect(308, "https://shop.admissionlab.test/secure"),
            redirect(302, "http://shop.admissionlab.test/secure"),
            vec![MigrationBehaviorKind::TlsBehaviorChanged],
        ),
        (
            "the redirect host moved",
            "/legacy",
            redirect(301, "http://shop.admissionlab.test/new"),
            redirect(301, "http://other.admissionlab.test/new"),
            vec![MigrationBehaviorKind::HostBehaviorChanged],
        ),
        (
            "the redirect target's path moved: a rewrite of the target",
            "/legacy",
            redirect(301, "http://shop.admissionlab.test/new"),
            redirect(301, "http://shop.admissionlab.test/v2/new"),
            vec![MigrationBehaviorKind::RewriteBehaviorChanged],
        ),
        (
            "an absolute target against a relative one is not a TLS difference",
            "/legacy",
            redirect(301, "http://shop.admissionlab.test/new"),
            redirect(301, "/new"),
            vec![MigrationBehaviorKind::RedirectBehaviorChanged],
        ),
        (
            "two relative targets differing only in path is a rewrite of the target",
            "/legacy",
            redirect(301, "/new"),
            redirect(301, "/v2/new"),
            vec![MigrationBehaviorKind::RewriteBehaviorChanged],
        ),
        (
            "the port is not compared: it is the port-forward's, not the route's",
            "/legacy",
            redirect(301, "http://shop.admissionlab.test/new"),
            redirect(301, "http://shop.admissionlab.test:8080/new"),
            vec![],
        ),
    ]
}

/// Drives both tables: every row's kinds, and the two invariants every
/// traffic change must satisfy whatever kind it is.
#[test]
fn each_kind_is_produced_by_the_evidence_that_defines_it() {
    let table = plain_traffic_table()
        .into_iter()
        .chain(redirect_traffic_table());
    for (why, path, baseline_probe, candidate_probe, expected) in table {
        let spec = case(vec![contract(path, 200)]);
        let changes = compare_migration_traffic(
            &spec,
            &served(vec![baseline_probe]),
            &candidate(vec![candidate_probe]),
        );
        assert_eq!(kinds(&changes), expected, "{why}");
        for change in &changes {
            // The request is rendered by `describe_probe_request`, which
            // is what redacts a credential-bearing header -- so a detail
            // built any other way would be a redaction hole.
            assert!(
                change
                    .detail
                    .starts_with("probe 0 (GET http://shop.admissionlab.test"),
                "{why}: a traffic detail must name the probe it is about, as \
                 `describe_probe_request` renders it; got {:?}",
                change.detail
            );
            assert!(
                !change.expected,
                "{why}: no traffic difference is ever declared expected -- \
                 `expected_nonportable` is a vocabulary about features"
            );
        }
    }
}

// =====================================================================
// 3. Pairing.
// =====================================================================

/// Probes are paired by index, and the change details say which index --
/// so a case whose second probe differs claims exactly one change, about
/// probe 1.
#[test]
fn probes_are_paired_by_index() {
    let spec = case(vec![contract("/a", 200), contract("/b", 200)]);
    let comparison = compare_migration_case(
        &spec,
        &served(vec![probe(200, Some("a"), &[]), probe(200, Some("b"), &[])]),
        &candidate(vec![probe(200, Some("a"), &[]), probe(200, Some("z"), &[])]),
        &[],
    );

    assert_eq!(
        kinds(&comparison.changes),
        [MigrationBehaviorKind::BackendChanged]
    );
    assert!(
        comparison.changes[0].detail.starts_with("probe 1 "),
        "got {:?}",
        comparison.changes[0].detail
    );
    assert_eq!(comparison.probes.len(), 2);
    for pair in &comparison.probes {
        assert_eq!(pair.contract_id, "shop");
    }
}

/// A probe only one side answered is not a pair and is not a claim:
/// there is no kind that could honestly carry "one side answered a
/// request the other did not", and inventing one would run the claim
/// backwards.
#[test]
fn an_unpaired_probe_is_neither_a_pair_nor_a_change() {
    let spec = case(vec![contract("/a", 200), contract("/b", 200)]);
    let comparison = compare_migration_case(
        &spec,
        &served(vec![probe(200, Some("a"), &[]), probe(200, Some("b"), &[])]),
        &candidate(vec![probe(200, Some("a"), &[])]),
        &[],
    );
    assert!(
        comparison.changes.is_empty(),
        "got {:?}",
        comparison.changes
    );
    assert_eq!(comparison.probes.len(), 1);
}

// =====================================================================
// 4. The annotation catalog.
// =====================================================================

/// A cataloged annotation the case declared is reported and marked
/// expected; one it did not declare is reported and marked unexpected.
/// Both are reported -- Task 8.5 Step 3 makes an expected feature
/// *visible*, not silent.
#[test]
fn a_cataloged_annotation_is_reported_expected_only_when_it_was_declared() {
    let documents = [document(
        "Ingress",
        "shop",
        &[
            ("nginx.ingress.kubernetes.io/canary", "true"),
            (
                "nginx.ingress.kubernetes.io/configuration-snippet",
                "more_set_headers \"X: y\";",
            ),
            // Not cataloged: Gateway API's URLRewrite filter covers it,
            // and flagging it would train a reader to ignore the list.
            ("nginx.ingress.kubernetes.io/rewrite-target", "/"),
        ],
    )];

    let mut spec = case(vec![contract("/", 200)]);
    spec.expected_nonportable = vec![NonPortableFeatureExpectation {
        feature: "nginx.ingress.kubernetes.io/canary".to_owned(),
        reason: "the canary Ingress is retired in the same change as this migration".to_owned(),
    }];

    let changes = nonportable_changes(&spec, &documents);
    assert_eq!(
        kinds(&changes),
        [
            MigrationBehaviorKind::NonPortableFeature,
            MigrationBehaviorKind::NonPortableFeature
        ],
        "exactly the two cataloged annotations, and not rewrite-target"
    );
    // Annotation order, which is the catalog's order.
    assert!(
        changes[0]
            .detail
            .starts_with("nginx.ingress.kubernetes.io/canary on Ingress shop/shop")
    );
    assert!(changes[0].expected, "declared in expected_nonportable");
    assert!(
        changes[1]
            .detail
            .starts_with("nginx.ingress.kubernetes.io/configuration-snippet")
    );
    assert!(!changes[1].expected, "never declared");
    // The reason travels with the finding: a user acts on it.
    assert!(
        changes[1].detail.contains("raw NGINX directives"),
        "got {:?}",
        changes[1].detail
    );
}

/// One change per annotation, naming every object that carries it --
/// the finding is "this migration drops feature X", not "object Y has
/// an annotation".
#[test]
fn one_change_per_annotation_names_every_object_carrying_it() {
    let documents = [
        document(
            "Ingress",
            "shop",
            &[("nginx.ingress.kubernetes.io/canary", "true")],
        ),
        document(
            "Ingress",
            "shop-canary",
            &[("nginx.ingress.kubernetes.io/canary", "true")],
        ),
    ];
    let changes = nonportable_changes(&case(vec![contract("/", 200)]), &documents);
    let [change] = changes.as_slice() else {
        panic!("expected one change per annotation, got {changes:?}");
    };
    assert!(
        change.detail.contains("Ingress shop/shop"),
        "got {:?}",
        change.detail
    );
    assert!(
        change.detail.contains("Ingress shop/shop-canary"),
        "got {:?}",
        change.detail
    );
}

/// A declared non-portability the manifests do not carry is surfaced --
/// but never as a change, because nothing was observed.
#[test]
fn a_declaration_that_matched_nothing_is_surfaced_but_is_not_a_change() {
    let mut spec = case(vec![contract("/", 200)]);
    spec.expected_nonportable = vec![
        NonPortableFeatureExpectation {
            feature: "nginx.ingress.kubernetes.io/server-snippet".to_owned(),
            reason: "removed in the same change".to_owned(),
        },
        // A shorthand rather than the annotation key: reported the same
        // way, deliberately -- see the function's own documentation.
        NonPortableFeatureExpectation {
            feature: "canary".to_owned(),
            reason: "a typo for the full key".to_owned(),
        },
    ];
    let documents = [document(
        "Ingress",
        "shop",
        &[("nginx.ingress.kubernetes.io/canary", "true")],
    )];

    let unmatched = unmatched_nonportable_expectations(&spec, &documents);
    assert_eq!(
        unmatched
            .iter()
            .map(|expectation| expectation.feature.as_str())
            .collect::<Vec<_>>(),
        ["nginx.ingress.kubernetes.io/server-snippet", "canary"],
        "declaration order, and both causes reported the same way"
    );

    // And it is nowhere in the comparison itself.
    let changes = nonportable_changes(&spec, &documents);
    assert_eq!(changes.len(), 1);
    assert!(
        !changes[0].detail.contains("server-snippet"),
        "an expectation nobody exercised is not an observation"
    );
    // The `canary` shorthand did not accidentally mark the real finding
    // expected: the match is on the whole key.
    assert!(!changes[0].expected);
}

/// The catalog's own invariants: sorted, unique, every entry an
/// `ingress-nginx` key with a reason and a real upstream citation.
#[test]
fn the_catalog_is_sorted_documented_and_free_of_duplicates() {
    let keys: Vec<&str> = NONPORTABLE_INGRESS_ANNOTATIONS
        .iter()
        .map(|entry| entry.annotation)
        .collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        keys, sorted,
        "the catalog must be sorted by annotation and free of duplicates -- its order is the \
         order findings are reported in"
    );

    for entry in &NONPORTABLE_INGRESS_ANNOTATIONS {
        assert!(
            entry.annotation.starts_with("nginx.ingress.kubernetes.io/"),
            "{:?} is not an ingress-nginx annotation",
            entry.annotation
        );
        assert!(
            entry.reason.len() > 40,
            "{:?} has no real reason",
            entry.annotation
        );
        assert!(
            entry.documentation.starts_with(
                "https://kubernetes.github.io/ingress-nginx/user-guide/nginx-configuration/annotations/#"
            ),
            "{:?} must cite the upstream annotation reference; got {:?}",
            entry.annotation,
            entry.documentation
        );
        assert_eq!(nonportable_annotation(entry.annotation), Some(entry));
    }

    // The lookup is exact: a near-miss inside the canary family must not
    // borrow another entry's reason.
    assert!(nonportable_annotation("nginx.ingress.kubernetes.io/canary-weight-total").is_none());
    assert!(nonportable_annotation("nginx.ingress.kubernetes.io/rewrite-target").is_none());
    assert!(nonportable_annotation("canary").is_none());
}

// =====================================================================
// 5. The incomparable seam.
// =====================================================================

/// The case Task 8.4 makes real: the API server refused the baseline, so
/// there is no legacy behavior to compare the candidate's against. No
/// traffic claim is made and no probe is paired -- but the manifests'
/// own non-portable features are still reported, because an `Ingress`
/// names `canary` whether or not a webhook let it through.
#[test]
fn a_denied_baseline_is_incomparable_but_its_manifests_are_still_read() {
    let denied = IngressCaseResult {
        admitted: false,
        ready: false,
        probes: Vec::new(),
        diagnostics: Vec::new(),
    };
    let candidate = candidate(vec![probe(200, Some("shop"), &[])]);
    assert_eq!(
        migration_comparability(&denied, &candidate),
        MigrationComparability::BaselineNotAdmitted
    );
    assert!(!MigrationComparability::BaselineNotAdmitted.is_comparable());
    assert!(
        MigrationComparability::BaselineNotAdmitted
            .reason()
            .contains("refused")
    );

    let documents = [document(
        "Ingress",
        "shop",
        &[("nginx.ingress.kubernetes.io/canary", "true")],
    )];
    let comparison = compare_migration_case(
        &case(vec![contract("/", 200)]),
        &denied,
        &candidate,
        &documents,
    );
    assert!(comparison.probes.is_empty(), "nothing to pair");
    assert_eq!(
        kinds(&comparison.changes),
        [MigrationBehaviorKind::NonPortableFeature],
        "no traffic claim, but the manifest finding stands"
    );
}

/// The other two incomparable shapes, and the comparable one.
#[test]
fn comparability_names_the_earliest_missing_evidence() {
    let answered = vec![probe(200, Some("shop"), &[])];
    let table = [
        (
            served(answered.clone()),
            candidate(answered.clone()),
            MigrationComparability::Comparable,
        ),
        (
            IngressCaseResult {
                admitted: true,
                ready: false,
                probes: Vec::new(),
                diagnostics: Vec::new(),
            },
            candidate(answered.clone()),
            MigrationComparability::BaselineNotServing,
        ),
        (
            // `ready: true` with no probes cannot arise from the runner,
            // but this function is public and takes a value anyone can
            // build: an empty probe list is "no reference behavior"
            // however the flag reads.
            IngressCaseResult {
                admitted: true,
                ready: true,
                probes: Vec::new(),
                diagnostics: Vec::new(),
            },
            candidate(answered.clone()),
            MigrationComparability::BaselineNotServing,
        ),
        (
            served(answered.clone()),
            candidate(Vec::new()),
            MigrationComparability::CandidateNotServing,
        ),
    ];
    for (baseline, candidate, expected) in table {
        assert_eq!(migration_comparability(&baseline, &candidate), expected);
        assert_eq!(
            expected.is_comparable(),
            expected == MigrationComparability::Comparable
        );
        assert!(!expected.reason().is_empty());
    }
}

// =====================================================================
// 6. Determinism (Global Constraint 7).
// =====================================================================

/// The same inputs produce the same comparison, byte for byte, and the
/// two halves are emitted in the documented order: traffic changes in
/// probe-index order, then non-portable features in annotation order.
#[test]
fn the_comparison_is_deterministic_and_ordered() {
    let spec = case(vec![contract("/", 200), contract("/checkout", 200)]);
    let baseline = served(vec![
        redirect(301, "https://shop.admissionlab.test/"),
        probe(200, Some("shop"), &[]),
    ]);
    let candidate = candidate(vec![
        redirect(301, "http://shop.admissionlab.test/"),
        probe(404, None, &[]),
    ]);
    let documents = [
        document(
            "Ingress",
            "shop",
            &[
                (
                    "nginx.ingress.kubernetes.io/whitelist-source-range",
                    "10.0.0.0/8",
                ),
                (
                    "nginx.ingress.kubernetes.io/auth-url",
                    "https://auth.test/verify",
                ),
            ],
        ),
        document("Service", "shop", &[]),
    ];

    let first = compare_migration_case(&spec, &baseline, &candidate, &documents);
    let second = compare_migration_case(&spec, &baseline, &candidate, &documents);
    assert_eq!(first, second);

    assert_eq!(
        kinds(&first.changes),
        [
            MigrationBehaviorKind::TlsBehaviorChanged,
            MigrationBehaviorKind::PathBehaviorChanged,
            MigrationBehaviorKind::NonPortableFeature,
            MigrationBehaviorKind::NonPortableFeature,
        ],
        "probe 0, then probe 1, then the annotations in catalog order"
    );
    assert!(
        first.changes[2]
            .detail
            .starts_with("nginx.ingress.kubernetes.io/auth-url")
    );
    assert!(
        first.changes[3]
            .detail
            .starts_with("nginx.ingress.kubernetes.io/whitelist-source-range")
    );
    assert_eq!(first.probes.len(), 2);

    // And the whole comparison serializes with the frozen tags.
    let rendered = serde_json::to_value(&first).expect("a comparison serializes");
    assert_eq!(rendered["changes"][0]["kind"], "tls_behavior_changed");
    assert_eq!(rendered["changes"][2]["expected"], false);
}
