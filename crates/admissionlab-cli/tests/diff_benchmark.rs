//! PRODUCT.md §33's one hard timing promise, as a test: "semantic
//! comparison of 100 ordinary fixtures in under one second after
//! artifacts are collected" (ROADMAP Task 5.7 step 3).
//!
//! ```bash
//! cargo test --release -p admissionlab-cli --test diff_benchmark -- --ignored --nocapture
//! ```
//!
//! # Why this one target is asserted and the others are not
//!
//! ROADMAP Task 5.7 step 4 is explicit that the `kind` wall-clock targets
//! must not become flaky PR assertions, and it is right: cluster
//! creation, image pulls, and Helm installs are dominated by Docker, the
//! network, and whatever else the runner is doing. This target is the
//! opposite kind of number. It measures a pure function of in-memory data
//! -- normalize, semantic diff, first-divergence attribution, policy
//! grading -- with no clock, no filesystem, no network and no cluster
//! anywhere in it, so a regression here is a regression in this
//! repository's own code and nothing else. `scripts/benchmark-alpha.sh`
//! is where the cluster-bound numbers are *reported* rather than
//! asserted.
//!
//! # `--release`, and `#[ignore]`
//!
//! Both for the same reason: the budget is a statement about the shipped
//! binary. A debug build of this workspace runs the comparison several
//! times slower (no inlining, `serde_json` and the JSON-pointer walks
//! unoptimized), so asserting the one-second bound on a debug build would
//! either fail constantly or, if the bound were loosened to fit, stop
//! being the product's promise. `#[ignore]` keeps it out of
//! `cargo test --workspace`, which is a debug build; the command above is
//! what CI runs.
//!
//! # What "100 ordinary fixtures" is made of here
//!
//! Synthesized in memory, deterministically, by [`corpus`] -- no cluster
//! and no checked-in artifact, because the property under test is the
//! *cost* of the comparison and a captured corpus would only add a file
//! read to it. "Ordinary" is doing real work in that sentence, so the
//! synthesis is deliberately representative rather than minimal:
//!
//! - every fixture's object is a Deployment with three containers, each
//!   with an image, environment variables (literal and `valueFrom`),
//!   resource requests, and volume mounts, plus pod-level volumes,
//!   labels, and annotations -- the shape
//!   `admissionlab_diff::diff_workload_objects` actually walks;
//! - every fixture's trace carries three webhook invocations, one of
//!   which returned a two-operation JSON Patch, so
//!   `admissionlab_normalize::normalize_trace` and
//!   `admissionlab_diff::diff_admission_trace` have real patches to
//!   compare rather than empty ones;
//! - one fixture in three genuinely differs between the sides, cycling
//!   through an image change, an injected container, and a changed
//!   environment value. A corpus where both sides agreed everywhere
//!   would benchmark the cheapest path through the diff and prove
//!   nothing about the expensive one, and it would leave the policy
//!   engine with nothing to grade.
//!
//! The measured pass is exactly what `pipeline::run_lab`'s comparison
//! stage does, called through the same two public entry points in the
//! same order -- not a reimplementation that could be faster than the
//! product.

use std::path::Path;
use std::time::{Duration, Instant};

use admissionlab_admission::{
    AdmissionDecision, AdmissionOutcome, AdmissionTrace, TraceEvidence, WebhookInvocation,
    WebhookOutcome,
};
use admissionlab_cli::pipeline::compare::compare;
use admissionlab_core::{FixtureId, Side};
use admissionlab_fixtures::FixtureSource;
use admissionlab_policy::{ResolvedExpectations, evaluate_with_expectations, resolve_policy};
use admissionlab_spec::{ResolvedLab, load_lab, resolve_lab};
use serde_json::{Value, json};

/// How many fixtures the target is stated over (PRODUCT.md §33).
const FIXTURES: usize = 100;

/// The budget, from PRODUCT.md §33: "semantic comparison of 100 ordinary
/// fixtures in under one second after artifacts are collected".
const BUDGET: Duration = Duration::from_secs(1);

/// [`FIXTURES`] as an `f64`, for the per-fixture average this file
/// prints. Written as a literal rather than cast from the `usize`:
/// `clippy::cast_precision_loss` is right that `usize as f64` is lossy in
/// general, and a second literal that a compile-time assertion pins to
/// the first is clearer than an `allow` on the one place it is not.
const FIXTURES_F64: f64 = 100.0;

const _: () = assert!(FIXTURES == 100, "FIXTURES and FIXTURES_F64 must agree");

/// The workspace's checked-in minimal configuration, resolved.
///
/// Loading a real file rather than hand-building a [`ResolvedLab`], whose
/// `ResolvedFixtureSelection` holds compiled `globset::Glob` values --
/// the same reason `pipeline::compare`'s own unit tests do this. It
/// carries no components, so the normalization profile under test is the
/// built-in tier exactly as a bare lab would run it.
fn minimal_lab() -> ResolvedLab {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/configs/minimal-valid.yaml");
    let loaded = load_lab(&path).expect("minimal-valid.yaml must load");
    resolve_lab(loaded).expect("minimal-valid.yaml must resolve")
}

/// The `i`th fixture's identifier. Zero-padded so discovery order and
/// lexicographic order agree, as they do for a real corpus.
fn fixture_id(index: usize) -> FixtureId {
    FixtureId::parse(&format!("deployment-{index:03}")).expect("a generated id is well formed")
}

/// One container, with the fields a workload diff actually walks.
fn container(name: &str, image: &str, secret: &str) -> Value {
    json!({
        "name": name,
        "image": image,
        "imagePullPolicy": "IfNotPresent",
        "env": [
            {"name": "LOG_LEVEL", "value": "info"},
            {"name": "REGION", "value": "eu-west-1"},
            {"name": "API_TOKEN", "valueFrom": {"secretKeyRef": {"name": secret, "key": "token"}}}
        ],
        "resources": {
            "requests": {"cpu": "100m", "memory": "128Mi"},
            "limits": {"cpu": "500m", "memory": "512Mi"}
        },
        "volumeMounts": [
            {"name": "config", "mountPath": "/etc/app"},
            {"name": "cache", "mountPath": "/var/cache/app"}
        ],
        "ports": [{"containerPort": 8080, "name": "http"}]
    })
}

/// One side's admitted object for fixture `index`.
///
/// `variant` selects which of the three differences this side carries;
/// `None` is the unmodified shape both sides share for the two fixtures
/// in three that do not diverge.
fn workload(index: usize, variant: Option<Divergence>) -> Value {
    let mut containers = vec![
        container("app", "registry.example.com/app:1.4.0", "app-credentials"),
        container(
            "sidecar",
            "registry.example.com/sidecar:2.1.0",
            "mesh-certs",
        ),
        container("exporter", "registry.example.com/exporter:0.9.1", "metrics"),
    ];
    match variant {
        Some(Divergence::Image) => {
            containers[0]["image"] = json!("registry.example.com/app:1.5.0");
        }
        Some(Divergence::InjectedContainer) => {
            containers.push(container(
                "istio-proxy",
                "registry.example.com/proxy:1.27.0",
                "mesh-certs",
            ));
        }
        Some(Divergence::Environment) => {
            containers[1]["env"][0]["value"] = json!("debug");
        }
        None => {}
    }
    json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": format!("workload-{index:03}"),
            "namespace": "admissionlab-bench",
            "uid": format!("00000000-0000-0000-0000-{index:012}"),
            "resourceVersion": "12345",
            "creationTimestamp": "2026-01-01T00:00:00Z",
            "labels": {"app": "bench", "tier": "backend"},
            "annotations": {
                "deployment.kubernetes.io/revision": "1",
                "kubectl.kubernetes.io/last-applied-configuration": "{}"
            }
        },
        "spec": {
            "replicas": 3,
            "selector": {"matchLabels": {"app": "bench"}},
            "template": {
                "metadata": {"labels": {"app": "bench"}},
                "spec": {
                    "containers": containers,
                    "initContainers": [
                        container("init", "registry.example.com/init:1.0.0", "app-credentials")
                    ],
                    "volumes": [
                        {"name": "config", "configMap": {"name": format!("config-{index:03}")}},
                        {"name": "cache", "emptyDir": {}}
                    ],
                    "serviceAccountName": "bench-runner",
                    "automountServiceAccountToken": false
                }
            }
        }
    })
}

/// Which difference a diverging fixture carries.
#[derive(Debug, Clone, Copy)]
enum Divergence {
    /// A changed container image.
    Image,
    /// A container only the candidate side has.
    InjectedContainer,
    /// A changed literal environment value.
    Environment,
}

/// A webhook chain with three invocations, one of which patched.
fn trace(index: usize, mutated: bool) -> AdmissionTrace {
    let patch = vec![
        json_patch::PatchOperation::Add(json_patch::AddOperation {
            path: "/metadata/annotations/bench.example.com~1injected"
                .parse()
                .expect("a literal RFC 6901 pointer parses"),
            value: json!("true"),
        }),
        json_patch::PatchOperation::Replace(json_patch::ReplaceOperation {
            path: "/spec/template/spec/containers/0/imagePullPolicy"
                .parse()
                .expect("a literal RFC 6901 pointer parses"),
            value: json!("Always"),
        }),
    ];
    AdmissionTrace {
        evidence: TraceEvidence::Observed,
        invocations: vec![
            WebhookInvocation {
                configuration: "bench-mutating".to_owned(),
                webhook: "mutate.example.com".to_owned(),
                round: 0,
                index: 0,
                mutated: Some(true),
                patch: Some(patch),
                latency: Some(Duration::from_millis(7 + (index % 5) as u64)),
                outcome: WebhookOutcome::Allowed,
            },
            WebhookInvocation {
                configuration: "bench-mutating".to_owned(),
                webhook: "annotate.example.com".to_owned(),
                round: 0,
                index: 1,
                mutated: Some(mutated),
                patch: None,
                latency: Some(Duration::from_millis(3)),
                outcome: WebhookOutcome::Allowed,
            },
            WebhookInvocation {
                configuration: "bench-validating".to_owned(),
                webhook: "validate.example.com".to_owned(),
                round: 1,
                index: 0,
                mutated: Some(false),
                patch: None,
                latency: Some(Duration::from_millis(2)),
                outcome: WebhookOutcome::Allowed,
            },
        ],
    }
}

/// One side's captured outcome for fixture `index`.
fn outcome(index: usize, side: Side, variant: Option<Divergence>) -> AdmissionOutcome {
    AdmissionOutcome {
        fixture_id: fixture_id(index),
        side,
        decision: AdmissionDecision::Accepted,
        warnings: Vec::new(),
        total_latency: Duration::from_millis(20 + (index % 7) as u64),
        final_object: Some(workload(index, variant)),
        trace: trace(index, side == Side::Candidate),
        diagnostics: Vec::new(),
    }
}

/// The discovered-fixture list the comparison iterates.
///
/// `object` is the fixture's own source document rather than either
/// side's admitted result; nothing in the comparison reads it, but a
/// [`FixtureSource`] is not honest without one.
fn source(index: usize) -> FixtureSource {
    FixtureSource {
        id: fixture_id(index),
        path: Path::new("/fixtures").join(format!("deployment-{index:03}.yaml")),
        document_index: 0,
        sha256: format!("{index:064x}"),
        object: workload(index, None),
    }
}

/// 100 fixtures and their 200 outcomes, one fixture in three diverging.
fn corpus() -> (Vec<FixtureSource>, Vec<AdmissionOutcome>) {
    let mut fixtures = Vec::with_capacity(FIXTURES);
    let mut outcomes = Vec::with_capacity(FIXTURES * 2);
    for index in 0..FIXTURES {
        fixtures.push(source(index));
        let variant = match index % 3 {
            0 => Some(Divergence::Image),
            1 => Some(Divergence::InjectedContainer),
            _ => None,
        };
        // The environment variant rides on every ninth fixture, so all
        // three shapes appear rather than only the first two.
        let variant = if index % 9 == 4 {
            Some(Divergence::Environment)
        } else {
            variant
        };
        outcomes.push(outcome(index, Side::Baseline, None));
        outcomes.push(outcome(index, Side::Candidate, variant));
    }
    (fixtures, outcomes)
}

#[test]
#[ignore = "release-mode performance budget; run with --release --ignored (see this file's header)"]
fn the_semantic_comparison_of_a_hundred_fixtures_stays_under_one_second() {
    let lab = minimal_lab();
    let policy = resolve_policy(&lab.policy).expect("the default policy compiles");
    let expectations = ResolvedExpectations::none();
    let (fixtures, outcomes) = corpus();

    // Every allocation the corpus needs is already done: what is timed
    // below is the comparison itself, which is what the target is about
    // ("after artifacts are collected").
    let started = Instant::now();
    // `None`: this benchmark measures the admission comparison, which is
    // what PRODUCT.md §33's sub-second target is about. A lab with no
    // `gateway:` section passes no Gateway results either.
    let comparison = compare(&lab, &fixtures, &outcomes, None).expect("the comparison succeeds");
    let graded = evaluate_with_expectations(&policy, &expectations, &comparison.changes());
    let elapsed = started.elapsed();

    assert_eq!(
        comparison.fixtures.len(),
        FIXTURES,
        "every fixture must be compared, or the measurement is of a smaller job"
    );
    assert!(
        !graded.changes.is_empty(),
        "a corpus where nothing diverged would benchmark the cheap path and prove nothing"
    );
    println!(
        "diff benchmark: {FIXTURES} fixtures in {:.3}s ({:.2}ms/fixture), {} graded change(s), \
         budget {:.3}s",
        elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1000.0 / FIXTURES_F64,
        graded.changes.len(),
        BUDGET.as_secs_f64(),
    );
    assert!(
        elapsed < BUDGET,
        "PRODUCT.md §33 budgets one second for the semantic comparison of 100 fixtures; this \
         pass took {elapsed:?}"
    );
}
