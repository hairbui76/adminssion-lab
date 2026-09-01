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
use admissionlab_policy::{
    ClassifiedChange, PolicyDisposition, PolicyResult, Severity, StaleExpectation,
};
use admissionlab_report::{
    AdmissionComparison, ComponentReport, EnvironmentReport, EnvironmentSummary, FixtureComparison,
    LabResult, RunSummary, SCHEMA_VERSION,
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

/// The full Alpha example: four fixtures, one per bucket, plus a policy
/// section and run diagnostics. See this module's documentation for what
/// it covers.
#[must_use]
pub fn canonical_result() -> LabResult {
    let fixtures = vec![
        critical_fixture(),
        expected_fixture(),
        inconclusive_fixture(),
        identical_fixture(),
    ];
    let changes: Vec<ClassifiedChange> = fixtures
        .iter()
        .flat_map(|fixture| fixture.changes.iter().cloned())
        .collect();

    LabResult {
        schema_version: SCHEMA_VERSION.to_owned(),
        run_id: run_id("alpha-demo-run"),
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
        timings: Some(timings()),
    }
}

/// The stage timings [`canonical_result`] carries.
///
/// Plausible numbers for the run the rest of this module describes: two
/// clusters created concurrently, one component installed per side, four
/// fixtures replayed through both, and a comparison well inside
/// PRODUCT.md §33's sub-second budget. `reporting` and `cleanup` are
/// `None` for the structural reason this module's own documentation
/// gives.
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
        gateway_suite: None,
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
