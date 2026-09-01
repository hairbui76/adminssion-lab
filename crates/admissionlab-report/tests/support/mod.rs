//! The canonical [`LabResult`] values every test in this crate renders.
//!
//! Two builders live here, and both are shared deliberately rather than
//! rebuilt per test file. [`canonical_result`] is *the* Alpha example:
//! it is what the JSON golden file records byte for byte, what the
//! terminal golden renders, and what the HTML test asserts over. One
//! definition means those three artifacts describe the same run, so a
//! model change that a renderer mishandles shows up as a diff in
//! whichever golden is wrong rather than as three independently-drifting
//! examples.
//!
//! [`sentinel_result`] is the same shape salted with the distinct secret
//! strings in [`SENTINELS`], one per redaction rule, for the
//! whole-result redaction proof.
//!
//! # Coverage
//!
//! [`canonical_result`] deliberately contains at least one of every
//! structure a renderer has to handle:
//!
//! - a fixture with an unexpected **critical** change carrying a
//!   `first_divergence` with `observed` confidence;
//! - a fixture whose only change is **expected**, and whose divergence
//!   attribution is `unknown` -- the case a renderer must label rather
//!   than present as an observation;
//! - an **inconclusive** fixture (one side `unsupported_dry_run`);
//! - an **identical** fixture (comparable, no changes);
//! - a **stale expectation**;
//! - run-level **diagnostics**, including a `Sensitive` context entry;
//! - a webhook trace with a JSON Patch, a `None` latency, and a
//!   `partial` evidence level;
//! - a **Gateway route contract** whose two sides both converged, with
//!   reconciliation evidence (a `GatewayClass`, a `Gateway` and an
//!   `HTTPRoute` parent, all with current conditions) and traffic
//!   evidence: one probe both sides answered *differently* and one probe
//!   only the baseline answered, so the frozen document's `traffic`
//!   section exercises a pair, an unpaired side, and the changes
//!   `admissionlab_gateway::diff` derives from both. Its changes come
//!   from calling that comparator rather than from hand-written payloads,
//!   so the example can never describe a shape the Gateway engine does
//!   not actually produce;
//! - an **Ingress-to-Gateway migration case** (ROADMAP Task 8.8) in the
//!   three-behavior shape `examples/ingress-to-gateway/` demonstrates on
//!   real clusters: a preserved behavior that produces no change at all,
//!   a declared non-portable feature graded `info`, an undeclared
//!   backend regression graded `critical` whose `detail` carries the two
//!   observed backends, two paired probes, and one
//!   declared-but-never-observed expectation -- so the frozen document's
//!   optional `migration` array is exercised on an example that is not
//!   degenerate;
//! - a **stage timings** block shaped exactly as a real `result.json`'s
//!   is: both sides of cluster creation, a per-component install
//!   breakdown, a fixture count, a comparison duration -- and *no*
//!   `reportingMs` and *no* `cleanup`, because the document is written
//!   during the first of those stages and before the second, so neither
//!   can be in it (see `admissionlab_core::timing`). Their absence in the
//!   golden is the assertion that they are absent rather than zero.

// Every test binary in this crate compiles this whole module but uses
// only the builders it needs, so items unused by *one* binary would
// otherwise fail the `-D warnings` gate for reasons that have nothing to
// do with the code under test.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::time::Duration;

use admissionlab_admission::{
    AdmissionDecision, AdmissionOutcome, AdmissionTrace, TraceEvidence, WebhookInvocation,
    WebhookOutcome,
};
use admissionlab_core::{
    CaptureStage, ComponentTiming, Diagnostic, FixtureId, InstallStage, RedactedValue, RunId, Side,
    SideInstallTiming, SideStage, StageTimings,
};
use admissionlab_diff::{
    DivergenceConfidence, DivergenceEvidence, SemanticChange, SemanticChangeKind,
};
use admissionlab_gateway::{
    CONDITION_ACCEPTED, CONDITION_PROGRAMMED, CONDITION_RESOLVED_REFS, ConditionState,
    GatewayCaseComparison, GatewayCaseResult, GatewayClassEvidence, GatewayEvidence,
    GatewayIdentity, HttpProbeResult, MigrationBehaviorChange, MigrationBehaviorKind,
    MigrationComparability, NonPortableFeatureExpectation, ObservedCondition, ParentIdentity,
    ProbePair, ReconciliationEvidence, RouteEvidence, RouteParentStatus,
};
use admissionlab_policy::{
    ClassifiedChange, PolicyDisposition, PolicyResult, Severity, StaleExpectation,
};
use admissionlab_report::{
    AdmissionComparison, ComponentReport, EnvironmentReport, EnvironmentSummary, FixtureComparison,
    GradedMigrationChange, LabResult, MigrationCaseComparison, RunSummary, SCHEMA_VERSION,
};
use serde_json::{Value, json};

/// The distinct secret strings [`sentinel_result`] plants, one per
/// redaction rule.
///
/// Each is unique and improbable so a test can assert on its absence in
/// the serialized redacted document without any chance of an accidental
/// match, and so a failure names exactly which rule did not fire.
pub const SENTINELS: &[&str] = &[
    SECRET_DATA_SENTINEL,
    SECRET_STRING_DATA_SENTINEL,
    ENV_LITERAL_SENTINEL,
    PATCH_ENV_LITERAL_SENTINEL,
    HEADER_SENTINEL,
    HEADER_FIELD_SENTINEL,
    PEM_SENTINEL,
    POINTER_SENTINEL,
];

/// Sits in a Secret's `data` map (rule 1).
pub const SECRET_DATA_SENTINEL: &str = "sentinel-secret-data-aaaa1111";
/// Sits in a Secret's `stringData` map (rule 1).
pub const SECRET_STRING_DATA_SENTINEL: &str = "sentinel-secret-stringdata-bbbb2222";
/// Sits in a credential-named `EnvVar`'s literal `value` (rule 4).
pub const ENV_LITERAL_SENTINEL: &str = "sentinel-env-literal-cccc3333";
/// Sits in a credential-named `EnvVar` injected by a webhook's JSON
/// Patch (rule 4, reached through a patch payload rather than a final
/// object).
pub const PATCH_ENV_LITERAL_SENTINEL: &str = "sentinel-patch-env-dddd4444";
/// Sits after an `Authorization:` header inside free text (rule 2).
pub const HEADER_SENTINEL: &str = "sentinel-bearer-header-eeee5555";
/// Sits under an `authorization` object key (rule 2, object-key form).
pub const HEADER_FIELD_SENTINEL: &str = "sentinel-header-field-ffff6666";
/// Sits inside a PEM private-key block (rule 3).
pub const PEM_SENTINEL: &str = "sentinel-private-key-body-9999aaaa";
/// Sits at the JSON pointer [`SENTINEL_POINTER`] addresses (rule 3,
/// configured-pointer form), under a key no built-in rule recognizes.
pub const POINTER_SENTINEL: &str = "sentinel-pointer-target-7777bbbb";

/// The RFC 6901 pointer a caller configures to reach
/// [`POINTER_SENTINEL`].
///
/// Deliberately addresses a field (`licence`) that matches no credential
/// pattern and no header name, so a passing pointer test proves the
/// pointer rule fired rather than one of the built-in ones. It is
/// written relative to the *payload's* root -- here, the `List` that is
/// the candidate side's whole final object -- which is what
/// `RedactionRules::json_pointers` documents pointers are resolved
/// against.
pub const SENTINEL_POINTER: &str = "/items/1/spec/licence";

/// The full Beta example: four admission fixtures, one per bucket, a
/// Gateway route contract, plus a policy section and run diagnostics.
/// See this module's documentation for what it covers.
#[must_use]
pub fn canonical_result() -> LabResult {
    let fixtures = vec![
        critical_fixture(),
        expected_fixture(),
        inconclusive_fixture(),
        identical_fixture(),
        // Appended last so every existing index into `fixtures` -- and
        // `sentinel_result`'s `fixtures[0]` in particular -- keeps
        // meaning what it meant.
        gateway_fixture(),
    ];
    let changes: Vec<ClassifiedChange> = fixtures
        .iter()
        .flat_map(|fixture| fixture.changes.iter().cloned())
        .collect();

    LabResult {
        schema_version: SCHEMA_VERSION.to_owned(),
        run_id: run_id("beta-demo-run"),
        summary: RunSummary::from_fixtures(&fixtures),
        environments: environments(),
        fixtures,
        policy: PolicyResult {
            disposition: PolicyDisposition::Fail,
            changes,
            stale_expectations: vec![StaleExpectation {
                id: "sidecar-injection-rollout".to_owned(),
                reason: "no matching change was observed in this run".to_owned(),
            }],
        },
        diagnostics: vec![
            Diagnostic {
                code: "metrics.unavailable".to_owned(),
                message: "per-webhook latency metrics were not scraped on the candidate side"
                    .to_owned(),
                context: context([("side", RedactedValue::Public("candidate".to_owned()))]),
            },
            Diagnostic {
                code: "kubeconfig.loaded".to_owned(),
                message: "loaded isolated kubeconfigs for both sides".to_owned(),
                context: context([
                    ("baseline", RedactedValue::Sensitive),
                    ("candidate", RedactedValue::Sensitive),
                ]),
            },
        ],
        migration: Some(vec![migration_case()]),
        timings: Some(timings()),
    }
}

/// The Ingress-to-Gateway migration case [`canonical_result`] carries
/// (ROADMAP Task 8.8).
///
/// Deliberately the three-behavior shape `examples/ingress-to-gateway/`
/// demonstrates on real clusters, so the frozen document's `migration`
/// section is exercised on an example that is not degenerate:
///
/// - a **preserved** behavior (probe 0: the same status from the same
///   backend on both sides), which contributes no change at all;
/// - a **declared non-portable feature**, graded `info` because the case
///   accounted for it in writing;
/// - an **undeclared behavior regression** (probe 1 reached a different
///   backend), graded `critical`, carrying the two observed backends in
///   its `detail` -- which is the evidence ROADMAP Task 8.8 step 2 says a
///   migration report must explain the difference with.
///
/// It also carries one `unmatchedExpectations` entry, so the
/// declared-but-never-observed path has a value in the golden too.
fn migration_case() -> MigrationCaseComparison {
    MigrationCaseComparison {
        case_id: "legacy-echo".to_owned(),
        comparability: MigrationComparability::Comparable,
        changes: vec![
            GradedMigrationChange {
                change: MigrationBehaviorChange {
                    kind: MigrationBehaviorKind::BackendChanged,
                    detail: "probe 1 (GET \
                             http://migrate.ingress.admissionlab.test/legacy/reports): the \
                             Ingress reached backend \"echo-b\" and the Gateway reached \
                             \"echo-a\""
                        .to_owned(),
                    expected: false,
                },
                severity: Severity::Critical,
            },
            GradedMigrationChange {
                change: MigrationBehaviorChange {
                    kind: MigrationBehaviorKind::NonPortableFeature,
                    detail: "nginx.ingress.kubernetes.io/limit-rps on Ingress \
                             admissionlab-migration-demo/echo-ingress has no portable Gateway \
                             API equivalent: per-client request rate limiting in the data plane."
                        .to_owned(),
                    expected: true,
                },
                severity: Severity::Info,
            },
        ],
        probes: vec![
            ProbePair {
                contract_id: "legacy-echo".to_owned(),
                baseline: migration_probe("echo-a", 12),
                candidate: migration_probe("echo-a", 9),
            },
            ProbePair {
                contract_id: "legacy-echo".to_owned(),
                baseline: migration_probe("echo-b", 14),
                candidate: migration_probe("echo-a", 8),
            },
        ],
        unmatched_expectations: vec![NonPortableFeatureExpectation {
            feature: "nginx.ingress.kubernetes.io/canary".to_owned(),
            reason: "the canary Ingress was deleted before this migration; kept here until \
                     the rollout is confirmed"
                .to_owned(),
        }],
    }
}

/// One migration probe result: an ordinary `200` from a named echo
/// backend.
fn migration_probe(backend: &str, millis: u64) -> HttpProbeResult {
    HttpProbeResult {
        status: 200,
        backend: Some(backend.to_owned()),
        response_headers: [("content-type".to_owned(), "application/json".to_owned())]
            .into_iter()
            .collect(),
        response_body_sha256: format!("{millis:064x}"),
        elapsed: Duration::from_millis(millis),
        attempts: 1,
    }
}

/// The stage timings [`canonical_result`] carries.
///
/// Plausible numbers for the run the rest of this module describes: two
/// clusters created concurrently, one component installed per side, four
/// fixtures replayed through both, one Gateway suite applied and
/// observed on both, and a comparison well inside PRODUCT.md §33's
/// sub-second budget. `reporting` and `cleanup` are `None` for the
/// structural reason this module's own documentation gives.
///
/// `fixtures` counts the *admission* corpus, which is four: the Gateway
/// route contract is a compared unit but not a replayed fixture, and its
/// stage is `gatewaySuite`.
fn timings() -> StageTimings {
    StageTimings {
        cluster_creation: Some(SideStage {
            wall: Duration::from_millis(43_512),
            baseline: Some(Duration::from_millis(41_204)),
            candidate: Some(Duration::from_millis(43_118)),
        }),
        installation: Some(InstallStage {
            wall: Duration::from_millis(96_401),
            baseline: Some(side_install(92_310)),
            candidate: Some(side_install(96_377)),
        }),
        gateway_suite: Some(SideStage {
            wall: Duration::from_millis(9_744),
            baseline: Some(Duration::from_millis(9_402)),
            candidate: Some(Duration::from_millis(9_731)),
        }),
        fixture_capture: Some(CaptureStage {
            wall: Duration::from_millis(6_120),
            baseline: Some(Duration::from_millis(5_942)),
            candidate: Some(Duration::from_millis(6_114)),
            fixtures: Some(4),
        }),
        comparison: Some(Duration::from_millis(212)),
        reporting: None,
        cleanup: None,
        elapsed: Duration::from_millis(149_006),
    }
}

/// One side's install breakdown: the single `sidecar-injector` component
/// [`environments`] already names, so the timings block and the
/// environments block describe the same stack.
fn side_install(millis: u64) -> SideInstallTiming {
    SideInstallTiming {
        elapsed: Duration::from_millis(millis),
        components: Some(vec![ComponentTiming {
            name: "sidecar-injector".to_owned(),
            elapsed: Duration::from_millis(millis),
        }]),
    }
}

/// [`canonical_result`]'s shape, salted with every string in
/// [`SENTINELS`].
///
/// The sentinels are placed in the five distinct locations Global
/// Constraint 14 names, spread across three different channels (a final
/// object, a webhook's JSON Patch, and free text), so a rule that only
/// walks one channel fails the test.
#[must_use]
pub fn sentinel_result() -> LabResult {
    let mut result = canonical_result();

    let fixture = &mut result.fixtures[0];
    let admission = fixture
        .admission
        .as_mut()
        .expect("the critical fixture always has an admission comparison");

    admission.candidate.final_object = Some(sentinel_object());
    admission.candidate.warnings.push(format!(
        "upstream replied 401; request line was\nAuthorization: Bearer {HEADER_SENTINEL}\nretrying"
    ));
    admission.candidate.trace.invocations[0].patch = Some(vec![json_patch::PatchOperation::Add(
        json_patch::AddOperation {
            path: "/spec/containers/0/env/1"
                .parse()
                .expect("a literal RFC 6901 pointer parses"),
            value: json!({"name": "VAULT_TOKEN", "value": PATCH_ENV_LITERAL_SENTINEL}),
        },
    )]);

    let change = &mut fixture.changes[0].change;
    change.baseline = Some(json!({"name": "DB_PASSWORD", "value": "old-value"}));
    change.candidate = Some(json!({"name": "DB_PASSWORD", "value": ENV_LITERAL_SENTINEL}));

    result.diagnostics[0].message = format!(
        "webhook bootstrap wrote\n-----BEGIN RSA PRIVATE KEY-----\n{PEM_SENTINEL}\n-----END RSA PRIVATE KEY-----\nto the shared volume"
    );

    // The migration section is its own top-level list (ROADMAP Task
    // 8.8), so a redaction pass that walks `fixtures` and `policy` and
    // stops would miss it entirely. Salted through both of the channels
    // it actually carries: a live response header map, which is the one
    // realistic place a data plane's `Set-Cookie` reaches a report, and
    // the user-written `reason` on a declared non-portability.
    let migration = result
        .migration
        .as_mut()
        .expect("the canonical result always carries a migration case");
    migration[0].probes[0].candidate.response_headers.insert(
        "set-cookie".to_owned(),
        format!("session={HEADER_FIELD_SENTINEL}; Path=/"),
    );
    migration[0].unmatched_expectations[0].reason = format!(
        "kept until the rollout is confirmed; the old key was\n-----BEGIN EC PRIVATE KEY-----\n{PEM_SENTINEL}\n-----END EC PRIVATE KEY-----"
    );

    // The policy section carries the same changes the fixtures do, so it
    // is rebuilt from them rather than salted separately -- a redaction
    // pass that missed `LabResult::policy` would otherwise not be caught.
    result.policy.changes = result
        .fixtures
        .iter()
        .flat_map(|fixture| fixture.changes.iter().cloned())
        .collect();

    result
}

/// The object planted as the candidate's `final_object` in
/// [`sentinel_result`].
///
/// One document carrying four sentinels: a `Secret`'s `data` and
/// `stringData`, a credential-named `EnvVar` literal reached through an
/// embedded `List`, a captured header map, and the pointer target.
fn sentinel_object() -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "List",
        "items": [
            {
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": {"name": "app-credentials"},
                "data": {"password": SECRET_DATA_SENTINEL},
                "stringData": {"apiToken": SECRET_STRING_DATA_SENTINEL}
            },
            {
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": {"name": "app"},
                "spec": {
                    "licence": POINTER_SENTINEL,
                    "containers": [
                        {
                            "name": "app",
                            "image": "registry.example.com/app:2.0.0",
                            "env": [
                                {"name": "DB_PASSWORD", "value": ENV_LITERAL_SENTINEL},
                                {
                                    "name": "DB_HOST",
                                    "valueFrom": {
                                        "secretKeyRef": {"name": "app-credentials", "key": "host"}
                                    }
                                }
                            ]
                        }
                    ]
                }
            }
        ],
        "status": {
            "observedHeaders": {
                "authorization": format!("Bearer {HEADER_FIELD_SENTINEL}"),
                "content-type": "application/json"
            }
        }
    })
}

/// A fixture with one unexpected `critical` change and an `observed`
/// first divergence -- the [`FixtureBucket::Critical`] case.
///
/// [`FixtureBucket::Critical`]: admissionlab_report::FixtureBucket::Critical
fn critical_fixture() -> FixtureComparison {
    let id = fixture_id("deployment-sidecar");
    FixtureComparison {
        fixture_id: id.clone(),
        admission: Some(AdmissionComparison {
            baseline: sidecar_outcome(&id, Side::Baseline),
            candidate: sidecar_outcome(&id, Side::Candidate),
            first_divergence: Some(DivergenceEvidence {
                confidence: DivergenceConfidence::Observed,
                baseline_position: Some((0, 0)),
                candidate_position: Some((0, 0)),
                baseline_webhook: Some("inject.example.com".to_owned()),
                candidate_webhook: Some("inject.example.com".to_owned()),
                explanation:
                    "inject.example.com returned a patch adding container istio-proxy on the \
                     candidate side and no such operation on the baseline side"
                        .to_owned(),
            }),
        }),
        gateway: None,
        changes: vec![ClassifiedChange {
            change: SemanticChange {
                kind: SemanticChangeKind::ContainerAdded,
                fixture_id: id,
                object_path: Some("/spec/template/spec/containers/1".to_owned()),
                subject: Some("istio-proxy".to_owned()),
                baseline: None,
                candidate: Some(json!({
                    "name": "istio-proxy",
                    "image": "registry.example.com/proxy:1.27.0"
                })),
                origin: Some(DivergenceEvidence {
                    confidence: DivergenceConfidence::Observed,
                    baseline_position: None,
                    candidate_position: Some((0, 0)),
                    baseline_webhook: None,
                    candidate_webhook: Some("inject.example.com".to_owned()),
                    explanation: "the container appears in inject.example.com's candidate patch"
                        .to_owned(),
                }),
            },
            severity: Severity::Critical,
            expected: false,
        }],
    }
}

/// One side of [`critical_fixture`]'s admission comparison.
///
/// The two sides differ in exactly the way the fixture is about: only
/// the candidate's injector returned a patch, and only the candidate
/// carries an API-server warning. Everything else is identical, so the
/// change the fixture reports has one visible cause.
fn sidecar_outcome(id: &FixtureId, side: Side) -> AdmissionOutcome {
    let is_candidate = side == Side::Candidate;
    AdmissionOutcome {
        fixture_id: id.clone(),
        side,
        decision: AdmissionDecision::Accepted,
        warnings: if is_candidate {
            vec!["sidecar injector is running a preview build".to_owned()]
        } else {
            Vec::new()
        },
        total_latency: Duration::from_millis(if is_candidate { 58 } else { 42 }),
        final_object: Some(json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {"name": "sidecar-demo"}
        })),
        trace: AdmissionTrace {
            evidence: TraceEvidence::Observed,
            invocations: vec![WebhookInvocation {
                configuration: "sidecar-injector".to_owned(),
                webhook: "inject.example.com".to_owned(),
                round: 0,
                index: 0,
                mutated: Some(true),
                patch: is_candidate.then(|| {
                    vec![json_patch::PatchOperation::Add(json_patch::AddOperation {
                        path: "/spec/template/spec/containers/1"
                            .parse()
                            .expect("a literal RFC 6901 pointer parses"),
                        value: json!({
                            "name": "istio-proxy",
                            "image": "registry.example.com/proxy:1.27.0"
                        }),
                    })]
                }),
                latency: Some(Duration::from_millis(if is_candidate { 19 } else { 7 })),
                outcome: WebhookOutcome::Allowed,
            }],
        },
        diagnostics: Vec::new(),
    }
}

/// A fixture whose only change was explicitly expected, with an
/// `unknown`-confidence divergence -- the [`FixtureBucket::Expected`]
/// case, and the one a renderer must label honestly.
///
/// [`FixtureBucket::Expected`]: admissionlab_report::FixtureBucket::Expected
fn expected_fixture() -> FixtureComparison {
    let id = fixture_id("deployment-image-bump");
    FixtureComparison {
        fixture_id: id.clone(),
        admission: Some(AdmissionComparison {
            baseline: AdmissionOutcome {
                fixture_id: id.clone(),
                side: Side::Baseline,
                decision: AdmissionDecision::Accepted,
                warnings: Vec::new(),
                total_latency: Duration::from_millis(31),
                final_object: Some(json!({"kind": "Deployment", "metadata": {"name": "web"}})),
                trace: AdmissionTrace {
                    evidence: TraceEvidence::Partial,
                    invocations: Vec::new(),
                },
                diagnostics: vec![Diagnostic {
                    code: "audit.incomplete".to_owned(),
                    message: "audit evidence did not cover the whole request window".to_owned(),
                    context: context([(
                        "stage",
                        RedactedValue::Public("ResponseComplete".to_owned()),
                    )]),
                }],
            },
            candidate: AdmissionOutcome {
                fixture_id: id.clone(),
                side: Side::Candidate,
                decision: AdmissionDecision::Accepted,
                warnings: Vec::new(),
                total_latency: Duration::from_millis(33),
                final_object: Some(json!({"kind": "Deployment", "metadata": {"name": "web"}})),
                trace: AdmissionTrace {
                    evidence: TraceEvidence::Partial,
                    invocations: Vec::new(),
                },
                diagnostics: Vec::new(),
            },
            first_divergence: Some(DivergenceEvidence {
                confidence: DivergenceConfidence::Unknown,
                baseline_position: None,
                candidate_position: None,
                baseline_webhook: None,
                candidate_webhook: None,
                explanation: "both traces are identical, so the captured evidence does not \
                              locate where the final objects diverged"
                    .to_owned(),
            }),
        }),
        gateway: None,
        changes: vec![ClassifiedChange {
            change: SemanticChange {
                kind: SemanticChangeKind::ImageChanged,
                fixture_id: id,
                object_path: Some("/spec/template/spec/containers/0/image".to_owned()),
                subject: Some("web".to_owned()),
                baseline: Some(json!("registry.example.com/web:1.4.0")),
                candidate: Some(json!("registry.example.com/web:1.5.0")),
                origin: None,
            },
            severity: Severity::Critical,
            expected: true,
        }],
    }
}

/// A fixture the candidate side could not replay -- the
/// [`FixtureBucket::Inconclusive`] case.
///
/// [`FixtureBucket::Inconclusive`]: admissionlab_report::FixtureBucket::Inconclusive
fn inconclusive_fixture() -> FixtureComparison {
    let id = fixture_id("crd-custom-resource");
    FixtureComparison {
        fixture_id: id.clone(),
        admission: Some(AdmissionComparison {
            baseline: AdmissionOutcome {
                fixture_id: id.clone(),
                side: Side::Baseline,
                decision: AdmissionDecision::Accepted,
                warnings: Vec::new(),
                total_latency: Duration::from_millis(12),
                final_object: None,
                trace: AdmissionTrace {
                    evidence: TraceEvidence::Unavailable,
                    invocations: Vec::new(),
                },
                diagnostics: Vec::new(),
            },
            candidate: AdmissionOutcome {
                fixture_id: id.clone(),
                side: Side::Candidate,
                decision: AdmissionDecision::UnsupportedDryRun {
                    message: "the candidate cluster's CRD does not accept server-side dry-run"
                        .to_owned(),
                },
                warnings: Vec::new(),
                total_latency: Duration::from_millis(9),
                final_object: None,
                trace: AdmissionTrace {
                    evidence: TraceEvidence::Unavailable,
                    invocations: Vec::new(),
                },
                diagnostics: Vec::new(),
            },
            first_divergence: None,
        }),
        gateway: None,
        changes: Vec::new(),
    }
}

/// A fixture both sides agreed on -- the [`FixtureBucket::Identical`]
/// case.
///
/// [`FixtureBucket::Identical`]: admissionlab_report::FixtureBucket::Identical
fn identical_fixture() -> FixtureComparison {
    let id = fixture_id("pod-baseline");
    let outcome = |side: Side| AdmissionOutcome {
        fixture_id: id.clone(),
        side,
        decision: AdmissionDecision::Rejected {
            code: Some(403),
            message: "pods must not run as root".to_owned(),
        },
        warnings: Vec::new(),
        total_latency: Duration::from_millis(15),
        final_object: None,
        trace: AdmissionTrace {
            evidence: TraceEvidence::Observed,
            invocations: vec![WebhookInvocation {
                configuration: "baseline-policy".to_owned(),
                webhook: "validate.example.com".to_owned(),
                round: 0,
                index: 0,
                mutated: Some(false),
                patch: None,
                latency: None,
                outcome: WebhookOutcome::Denied,
            }],
        },
        diagnostics: Vec::new(),
    };
    FixtureComparison {
        fixture_id: id.clone(),
        admission: Some(AdmissionComparison {
            baseline: outcome(Side::Baseline),
            candidate: outcome(Side::Candidate),
            first_divergence: None,
        }),
        gateway: None,
        changes: Vec::new(),
    }
}

/// A Gateway route contract observed on both sides -- the entry that
/// fills the frozen document's `gatewayReconciliation` and `traffic`
/// sections.
///
/// Both sides converged, so the comparison is `comparable` and every
/// difference between them is real evidence. They differ in two ways,
/// each chosen to exercise one part of the `traffic` section: the first
/// probe was answered by both sides with different statuses (a pair),
/// and a second probe was answered only by the baseline (an unpaired
/// result, which `admissionlab_gateway::diff` reports as a change
/// precisely because the baseline converged).
///
/// The changes are whatever `admissionlab_gateway::diff` actually
/// derives from those two case results, graded here rather than
/// invented: this module builds *evidence*, and letting the Gateway
/// comparator produce the claims keeps the example honest about what
/// that comparator emits.
fn gateway_fixture() -> FixtureComparison {
    let id = fixture_id("echo-route-contract");
    let comparison = GatewayCaseComparison {
        baseline: gateway_case(200, 2),
        candidate: gateway_case(503, 1),
    };
    let changes = comparison
        .changes()
        .into_iter()
        .map(|change| ClassifiedChange {
            change: change.attributed_to(&id),
            severity: Severity::Warning,
            expected: false,
        })
        .collect();

    FixtureComparison {
        fixture_id: id,
        admission: None,
        gateway: Some(comparison),
        changes,
    }
}

/// One side of [`gateway_fixture`]: a converged route contract that
/// answered `probes` probes, the first of them with `status`.
fn gateway_case(status: u16, probes: usize) -> GatewayCaseResult {
    GatewayCaseResult {
        contract_id: "echo-route-contract".to_owned(),
        reconciliation: ReconciliationEvidence {
            gateway_class: Some(GatewayClassEvidence {
                name: "lab-gateway-class".to_owned(),
                accepted: condition(CONDITION_ACCEPTED, None),
            }),
            gateway: GatewayEvidence {
                identity: GatewayIdentity {
                    namespace: "default".to_owned(),
                    name: "lab-gateway".to_owned(),
                },
                conditions: [
                    (
                        CONDITION_ACCEPTED.to_owned(),
                        condition(CONDITION_ACCEPTED, Some(3)),
                    ),
                    (
                        CONDITION_PROGRAMMED.to_owned(),
                        condition(CONDITION_PROGRAMMED, Some(3)),
                    ),
                ]
                .into_iter()
                .collect(),
                generation: 3,
                gateway_class_name: Some("lab-gateway-class".to_owned()),
            },
            route: RouteEvidence {
                namespace: "default".to_owned(),
                name: "echo-route".to_owned(),
                generation: 5,
                parents: vec![RouteParentStatus {
                    parent: ParentIdentity {
                        namespace: Some("default".to_owned()),
                        name: "lab-gateway".to_owned(),
                        section_name: Some("http".to_owned()),
                    },
                    controller_name: Some("example.com/gateway-controller".to_owned()),
                    conditions: [
                        (
                            CONDITION_ACCEPTED.to_owned(),
                            condition(CONDITION_ACCEPTED, Some(5)),
                        ),
                        (
                            CONDITION_RESOLVED_REFS.to_owned(),
                            condition(CONDITION_RESOLVED_REFS, Some(5)),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                }],
            },
            elapsed: Duration::from_millis(4_180),
            converged: true,
            diagnostics: Vec::new(),
        },
        probes: (0..probes)
            .map(|index| HttpProbeResult {
                status: if index == 0 { status } else { 204 },
                backend: Some("echo-v1".to_owned()),
                response_headers: [
                    ("content-type".to_owned(), "application/json".to_owned()),
                    ("server".to_owned(), "echo".to_owned()),
                ]
                .into_iter()
                .collect(),
                response_body_sha256: format!("{:064x}", index + 1),
                elapsed: Duration::from_millis(11 + index as u64),
                attempts: 1,
            })
            .collect(),
    }
}

/// A settled, current condition of `type_name`.
///
/// `observed_generation` is passed rather than defaulted so a caller
/// decides whether the condition is *current* -- `None` is what
/// `GatewayClassEvidence` legitimately has (§1.2 gives it no generation
/// to measure against).
fn condition(type_name: &str, observed_generation: Option<i64>) -> ObservedCondition {
    ObservedCondition {
        type_name: type_name.to_owned(),
        state: ConditionState::True,
        reason: Some("Accepted".to_owned()),
        observed_generation,
    }
}

/// Both sides' environment descriptions.
fn environments() -> EnvironmentSummary {
    EnvironmentSummary {
        baseline: EnvironmentReport {
            kubernetes: "v1.34.1".to_owned(),
            components: vec![ComponentReport {
                name: "sidecar-injector".to_owned(),
                version: "1.26.3".to_owned(),
            }],
        },
        candidate: EnvironmentReport {
            kubernetes: "v1.34.1".to_owned(),
            components: vec![ComponentReport {
                name: "sidecar-injector".to_owned(),
                version: "1.27.0".to_owned(),
            }],
        },
    }
}

/// Parses a run identifier that is known to be well formed.
fn run_id(value: &str) -> RunId {
    RunId::parse(value).expect("test run identifiers are well formed")
}

/// Parses a fixture identifier that is known to be well formed.
fn fixture_id(value: &str) -> FixtureId {
    FixtureId::parse(value).expect("test fixture identifiers are well formed")
}

/// Builds a diagnostic context map from borrowed keys.
fn context<const N: usize>(entries: [(&str, RedactedValue); N]) -> BTreeMap<String, RedactedValue> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}
