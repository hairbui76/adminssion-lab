//! Turning both sides' captured outcomes into a comparison (Task 4.14's
//! normalize → semantic diff → first divergence stages).
//!
//! Everything in this module is pure: given the same outcomes and the
//! same resolved lab, it produces the same [`Comparison`], with no clock,
//! no filesystem, and no network (Global Constraint 7). It decides no
//! severities — that is `admissionlab-policy`'s job, applied to the
//! [`SemanticChange`]s this module emits — and it renders nothing.
//!
//! # The `RecipeNormalizeRule` → `NormalizeRule` conversion lives here
//!
//! `admissionlab_normalize::rules`'s own module documentation names this
//! conversion as a genuine seam that deliberately lives in *neither*
//! crate: `admissionlab_spec::RecipeNormalizeRule` is the configuration
//! vocabulary and `admissionlab_normalize::NormalizeRule` is the engine
//! vocabulary, they are free to diverge, and making either crate depend
//! on the other for a three-arm `match` would couple them for no reason
//! — in particular it would make every future engine-only rule kind
//! automatically become recipe-authorable surface, which is a Global
//! Constraint 6 hole opened by accident.
//!
//! That documentation asks the seam to land in "the crate that already
//! depends on both (the assembler)". This is that crate and this is that
//! assembler, so [`normalize_rule_from_recipe`] is written here. It is a
//! total, mechanical mapping of three variants onto three variants, with
//! no wildcard arm, so a fourth variant on either side is a compile
//! error rather than a silent drop.
//!
//! # Which recipe rules apply, and in what order
//!
//! [`normalization_profile`] builds one profile for the whole run and
//! applies it to *both* sides. Normalization exists to make two
//! observations comparable, so a rule that ran on only one side would
//! manufacture a difference (or hide one) rather than remove noise —
//! which is why the baseline's and the candidate's recipe rules are
//! unioned rather than applied per side. Duplicates are collapsed (the
//! two sides usually run the same recipes, one version apart), and the
//! surviving order is baseline-then-candidate declaration order, so the
//! profile is deterministic.
//!
//! The `user` tier stays empty: `admissionlab.yaml` has no field for
//! user-authored normalization rules yet (see
//! `admissionlab_spec::ResolvedComponent`, whose
//! `recipe_normalize_rules` is itself the only source today). When a
//! later task adds one, it fills that tier here.
//!
//! # Why the workload diff is gated on both sides having admitted
//!
//! [`diff_workload_objects`] compares two *admitted objects*. A fixture
//! the API server rejected has no final object at all
//! (`AdmissionOutcome::final_object` is `None`), and a fixture whose
//! decision flipped has one on exactly one side. Handing that to a
//! workload comparison would either compare an object against nothing —
//! reporting every container in the surviving object as added or removed
//! — or compare two objects whose difference is entirely explained by
//! the decision flip [`diff_admission_decision`] has *already* reported
//! as `newly_denied`/`newly_allowed`. Both readings are noise laid on
//! top of a claim the run already made correctly, so the workload diff
//! runs only when both sides were comparable, both accepted, and both
//! captured a final object.
//!
//! The trace diff has no such gate: a rejected fixture still ran through
//! a webhook chain, and how that chain differs is exactly what a newly
//! denied request needs explaining.
//!
//! # Metric-sourced webhook evidence is never a semantic change
//!
//! `admissionlab-admission` records a rejection-counter increase as a
//! `Diagnostic` (`admission.webhook_rejection_metric`) and explicitly
//! not as a webhook invocation, because a metric increase proves a
//! rejection was *counted*, not that a nameable webhook ran at a
//! particular round and index. This module keeps that line: it never
//! reads those diagnostics into a
//! [`SemanticChangeKind::WebhookFailed`](admissionlab_diff::SemanticChangeKind::WebhookFailed)
//! claim (Global Constraint 15). What it does instead is
//! [`capture_diagnostics`]: one run-level [`Diagnostic`] per distinct
//! capture diagnostic *code*, saying how many fixtures recorded it, so
//! the evidence is visible in the terminal summary and the JSON report
//! without being promoted into a finding it does not support. Each
//! fixture's own diagnostics stay on its own `AdmissionOutcome`, where
//! `admissionlab_report::LabResult`'s frozen contract puts them.

use std::collections::{BTreeMap, HashMap};

use admissionlab_admission::{AdmissionDecision, AdmissionOutcome};
use admissionlab_core::{Diagnostic, FixtureId, RedactedValue, Side};
use admissionlab_diff::{
    DivergenceEvidence, SemanticChange, decision_comparability, diff_admission_decision,
    diff_admission_trace, diff_workload_objects, first_divergence_with_objects,
};
use admissionlab_fixtures::FixtureSource;
use admissionlab_normalize::{
    NormalizationProfile, NormalizeError, NormalizeRule, NormalizedObject, NormalizedTrace,
    normalize_object, normalize_trace,
};
use admissionlab_report::AdmissionComparison;
use admissionlab_spec::{RecipeNormalizeRule, ResolvedComponent, ResolvedLab};

/// One fixture's comparison, before policy has graded anything.
#[derive(Debug, Clone)]
pub struct ComparedFixture {
    /// Which fixture this is.
    pub fixture_id: FixtureId,
    /// Both sides' captured admission behavior and the first divergence
    /// between them. `None` when at least one side produced no outcome
    /// at all, which `admissionlab_report::FixtureComparison::bucket`
    /// counts as inconclusive rather than identical.
    pub admission: Option<AdmissionComparison>,
    /// Every change claimed for this fixture, already attributed to it.
    pub changes: Vec<SemanticChange>,
}

/// Every fixture's comparison, plus what the comparison itself observed
/// about the run.
#[derive(Debug, Clone)]
pub struct Comparison {
    /// One entry per discovered fixture, in discovery order.
    pub fixtures: Vec<ComparedFixture>,
    /// Run-level diagnostics this comparison produced — see this
    /// module's documentation for what does and does not end up here.
    pub diagnostics: Vec<Diagnostic>,
}

impl Comparison {
    /// Every change from every fixture, in fixture order.
    ///
    /// This is what `admissionlab_policy::evaluate_with_expectations`
    /// grades: it needs the whole run's changes at once, and it applies
    /// its own deterministic ordering to them, so the order here only
    /// has to be stable, which fixture order makes it.
    #[must_use]
    pub fn changes(&self) -> Vec<SemanticChange> {
        self.fixtures
            .iter()
            .flat_map(|fixture| fixture.changes.iter().cloned())
            .collect()
    }
}

/// Converts one recipe-supplied normalization rule into the engine's own
/// vocabulary.
///
/// See this module's documentation for why this conversion lives here
/// rather than in `admissionlab-spec` or `admissionlab-normalize`.
/// Total and mechanical: three variants onto three variants, matched
/// exhaustively so a new variant on either side has to be handled
/// deliberately.
#[must_use]
pub fn normalize_rule_from_recipe(rule: &RecipeNormalizeRule) -> NormalizeRule {
    match rule {
        RecipeNormalizeRule::RemovePointer(pointer) => {
            NormalizeRule::RemovePointer(pointer.clone())
        }
        RecipeNormalizeRule::RemoveAnnotation(key) => NormalizeRule::RemoveAnnotation(key.clone()),
        RecipeNormalizeRule::SortNamedArray { pointer, key } => NormalizeRule::SortNamedArray {
            pointer: pointer.clone(),
            key: key.clone(),
        },
    }
}

/// The profile every object in this run is normalized under: Admission
/// Lab's built-in rules, plus every recipe rule either side's components
/// contributed.
///
/// See this module's documentation for why both sides' rules are unioned
/// into one profile and why the `user` tier is empty today.
#[must_use]
pub fn normalization_profile(lab: &ResolvedLab) -> NormalizationProfile {
    let mut recipe = Vec::new();
    for component in components_of(lab) {
        for rule in &component.recipe_normalize_rules {
            let converted = normalize_rule_from_recipe(rule);
            if !recipe.contains(&converted) {
                recipe.push(converted);
            }
        }
    }
    NormalizationProfile {
        recipe,
        ..NormalizationProfile::built_in()
    }
}

/// Both sides' components, baseline first, in declaration order.
fn components_of(lab: &ResolvedLab) -> impl Iterator<Item = &ResolvedComponent> {
    lab.baseline
        .components
        .iter()
        .chain(lab.candidate.components.iter())
}

/// Compares every discovered fixture's two captured outcomes.
///
/// `outcomes` is what the capture backend observed, in capture order and
/// with both sides interleaved; each one names its own side and fixture,
/// so this pairs them by identity rather than by position. A fixture
/// with no outcome on one or both sides is reported as inconclusive
/// (with a run-level diagnostic saying so), never as identical.
///
/// # Errors
///
/// Returns [`NormalizeError`] if the normalization profile itself is
/// unusable — a rule whose JSON Pointer does not parse, or one that
/// would remove the whole document. That is a property of the profile,
/// not of any one object, so it fails the run rather than silently
/// producing objects normalized under a profile the user did not get.
pub fn compare(
    lab: &ResolvedLab,
    fixtures: &[FixtureSource],
    outcomes: &[AdmissionOutcome],
) -> Result<Comparison, NormalizeError> {
    let profile = normalization_profile(lab);
    let index: HashMap<(&str, Side), &AdmissionOutcome> = outcomes
        .iter()
        .map(|outcome| ((outcome.fixture_id.as_str(), outcome.side), outcome))
        .collect();

    let mut compared = Vec::with_capacity(fixtures.len());
    let mut diagnostics = Vec::new();
    let mut warnings: BTreeMap<String, usize> = BTreeMap::new();

    for fixture in fixtures {
        let id = fixture.id.as_str();
        let baseline = index.get(&(id, Side::Baseline)).copied();
        let candidate = index.get(&(id, Side::Candidate)).copied();
        let (Some(baseline), Some(candidate)) = (baseline, candidate) else {
            diagnostics.push(missing_outcome_diagnostic(
                &fixture.id,
                baseline.is_some(),
                candidate.is_some(),
            ));
            compared.push(ComparedFixture {
                fixture_id: fixture.id.clone(),
                admission: None,
                changes: Vec::new(),
            });
            continue;
        };

        let pair = compare_pair(lab, &profile, baseline, candidate, &mut warnings)?;
        compared.push(pair);
    }

    diagnostics.extend(normalization_diagnostics(&warnings));
    diagnostics.extend(capture_diagnostics(outcomes));

    Ok(Comparison {
        fixtures: compared,
        diagnostics,
    })
}

/// Compares one fixture's two outcomes.
fn compare_pair(
    lab: &ResolvedLab,
    profile: &NormalizationProfile,
    baseline: &AdmissionOutcome,
    candidate: &AdmissionOutcome,
    warnings: &mut BTreeMap<String, usize>,
) -> Result<ComparedFixture, NormalizeError> {
    let fixture_id = baseline.fixture_id.clone();

    // Already attributed: an `AdmissionOutcome` carries its own fixture
    // identity, so `diff_admission_decision` stamps these itself (see
    // `admissionlab_diff::SemanticChange::attributed_to`).
    let mut changes = diff_admission_decision(baseline, candidate);
    let comparable = decision_comparability(baseline, candidate).is_comparable();

    let objects = normalized_objects(profile, baseline, candidate, comparable, warnings)?;
    if let Some((baseline_object, candidate_object)) = &objects {
        changes.extend(
            diff_workload_objects(baseline_object, candidate_object)
                .into_iter()
                .map(|change| change.attributed_to(&fixture_id)),
        );
    }

    let baseline_trace = normalize_trace(&baseline.trace);
    let candidate_trace = normalize_trace(&candidate.trace);
    changes.extend(
        diff_admission_trace(&baseline_trace, &candidate_trace, &lab.policy.latency)
            .into_iter()
            .map(|change| change.attributed_to(&fixture_id)),
    );

    let first_divergence =
        attribute_divergence(&baseline_trace, &candidate_trace, objects.as_ref());

    Ok(ComparedFixture {
        fixture_id,
        admission: Some(AdmissionComparison {
            baseline: baseline.clone(),
            candidate: candidate.clone(),
            first_divergence,
        }),
        changes,
    })
}

/// Normalizes both sides' final objects, when there are two of them to
/// compare. See this module's documentation for the gate.
///
/// Records every normalization warning into `warnings` (counted by
/// message) rather than dropping it: a warning means a rule removed a
/// whole subtree, which is a suppression the user cannot see by reading
/// the diff, because the diff no longer contains it.
fn normalized_objects(
    profile: &NormalizationProfile,
    baseline: &AdmissionOutcome,
    candidate: &AdmissionOutcome,
    comparable: bool,
    warnings: &mut BTreeMap<String, usize>,
) -> Result<Option<(NormalizedObject, NormalizedObject)>, NormalizeError> {
    if !comparable
        || baseline.decision != AdmissionDecision::Accepted
        || candidate.decision != AdmissionDecision::Accepted
    {
        return Ok(None);
    }
    let (Some(baseline_value), Some(candidate_value)) =
        (&baseline.final_object, &candidate.final_object)
    else {
        return Ok(None);
    };

    let baseline_object = normalize_object(baseline_value, profile)?;
    let candidate_object = normalize_object(candidate_value, profile)?;
    for object in [&baseline_object, &candidate_object] {
        for warning in &object.evidence.warnings {
            *warnings.entry(warning.clone()).or_insert(0) += 1;
        }
    }
    Ok(Some((baseline_object, candidate_object)))
}

/// Attributes where the two sides first diverged.
///
/// Always attempted, including for a fixture whose decisions were not
/// comparable: `first_divergence_with_objects` grades its own evidence
/// (`DivergenceConfidence::Observed`/`Inferred`/`Unknown`) and reports
/// an unavailable trace as unknown rather than as agreement, so asking
/// it can never manufacture a claim — while *not* asking would throw
/// away the one answer a reader of an inconclusive fixture most wants.
///
/// `objects_differ` is computed from the two **normalized** objects, not
/// the raw ones: that is the same comparison every semantic diff is made
/// against, so "the traces agree but the objects do not" means the same
/// thing here as it does everywhere else in the run.
fn attribute_divergence(
    baseline_trace: &NormalizedTrace,
    candidate_trace: &NormalizedTrace,
    objects: Option<&(NormalizedObject, NormalizedObject)>,
) -> Option<DivergenceEvidence> {
    let objects_differ =
        objects.is_some_and(|(baseline, candidate)| baseline.value != candidate.value);
    first_divergence_with_objects(baseline_trace, candidate_trace, objects_differ)
}

/// The diagnostic for a fixture that produced no outcome on one or both
/// sides.
///
/// Reachable only if a capture backend returned fewer outcomes than the
/// fixtures it was given — which the production one never does on a
/// successful run — or on the partial-evidence path where a run failed
/// mid-capture and the comparison is being made over what was captured
/// anyway. Either way the honest answer is that this fixture cannot be
/// compared, not that it matched.
fn missing_outcome_diagnostic(
    fixture_id: &FixtureId,
    has_baseline: bool,
    has_candidate: bool,
) -> Diagnostic {
    let missing = match (has_baseline, has_candidate) {
        (false, false) => "neither side",
        (false, true) => "the baseline side",
        (true, false) => "the candidate side",
        (true, true) => "no side",
    };
    let mut context = BTreeMap::new();
    context.insert(
        "fixture".to_owned(),
        RedactedValue::Public(fixture_id.as_str().to_owned()),
    );
    Diagnostic {
        code: "compare.missing_outcome".to_owned(),
        message: format!(
            "fixture {:?} was captured on {missing}, so its two sides could not be compared; it \
             is counted as inconclusive rather than identical",
            fixture_id.as_str()
        ),
        context,
    }
}

/// One run-level diagnostic per distinct normalization warning.
///
/// Counted rather than repeated per fixture: a profile that removes a
/// whole subtree does so for every object it matches, so the same
/// sentence would otherwise appear once per fixture and drown the rest
/// of the run's diagnostics. The count is what a reader actually needs
/// (how much of the corpus this suppressed), and the profile that caused
/// it is the same for the whole run.
fn normalization_diagnostics(warnings: &BTreeMap<String, usize>) -> Vec<Diagnostic> {
    warnings
        .iter()
        .map(|(warning, count)| {
            let mut context = BTreeMap::new();
            context.insert(
                "normalized_objects".to_owned(),
                RedactedValue::Public(count.to_string()),
            );
            Diagnostic {
                code: "normalize.suppressed".to_owned(),
                message: format!("{warning} (applied to {count} normalized object(s))"),
                context,
            }
        })
        .collect()
}

/// One run-level diagnostic per distinct capture diagnostic code.
///
/// See this module's documentation: this is how metric-sourced webhook
/// evidence (`admission.webhook_rejection_metric`) and unavailable audit
/// correlation (`admission.audit_correlation_failed`) reach
/// `LabResult::diagnostics` — as a count of affected fixtures, never as
/// a fabricated semantic change, and never by copying every fixture's
/// own diagnostics up to the run level (which would contradict
/// `LabResult::diagnostics`'s own documented contract that per-fixture
/// diagnostics stay on that fixture's outcome, where the JSON and HTML
/// reports already render them).
fn capture_diagnostics(outcomes: &[AdmissionOutcome]) -> Vec<Diagnostic> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut examples: BTreeMap<&str, &str> = BTreeMap::new();
    for outcome in outcomes {
        for diagnostic in &outcome.diagnostics {
            *counts.entry(diagnostic.code.as_str()).or_insert(0) += 1;
            examples
                .entry(diagnostic.code.as_str())
                .or_insert(diagnostic.message.as_str());
        }
    }

    counts
        .into_iter()
        .map(|(code, count)| {
            let mut context = BTreeMap::new();
            context.insert(
                "observations".to_owned(),
                RedactedValue::Public(count.to_string()),
            );
            let example = examples.get(code).copied().unwrap_or_default();
            Diagnostic {
                code: code.to_owned(),
                message: format!(
                    "{count} captured fixture outcome(s) recorded this diagnostic; see each \
                     fixture's own evidence for the full detail. First: {example}"
                ),
                context,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use admissionlab_admission::{AdmissionTrace, TraceEvidence};
    use admissionlab_spec::{InstallMethod, ManifestInstallSpec, load_lab, resolve_lab};
    use serde_json::json;

    use super::*;

    /// The workspace's checked-in minimal configuration, resolved.
    /// Loading a real file is simpler than hand-building a
    /// `ResolvedLab`, whose `ResolvedFixtureSelection` holds compiled
    /// `globset::Glob` values — the same reason
    /// `admissionlab-core`'s own `tests/run_lifecycle.rs` does this.
    fn minimal_lab() -> ResolvedLab {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/configs/minimal-valid.yaml");
        let loaded = load_lab(&path).expect("minimal-valid.yaml must load");
        resolve_lab(loaded).expect("minimal-valid.yaml must resolve")
    }

    fn component_with_rules(name: &str, rules: Vec<RecipeNormalizeRule>) -> ResolvedComponent {
        ResolvedComponent {
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            install: InstallMethod::Manifests(ManifestInstallSpec {
                paths: vec![PathBuf::from("/fake.yaml")],
            }),
            readiness: Vec::new(),
            recipe_normalize_rules: rules,
            capabilities: BTreeSet::new(),
        }
    }

    fn fixture(id: &str) -> FixtureSource {
        FixtureSource {
            id: FixtureId::parse(id).expect("valid fixture id"),
            path: PathBuf::from("/fixtures/pod.yaml"),
            document_index: 0,
            sha256: "0".repeat(64),
            object: json!({"apiVersion": "v1", "kind": "Pod"}),
        }
    }

    fn accepted(id: &str, side: Side, object: serde_json::Value) -> AdmissionOutcome {
        AdmissionOutcome {
            fixture_id: FixtureId::parse(id).expect("valid fixture id"),
            side,
            decision: AdmissionDecision::Accepted,
            warnings: Vec::new(),
            total_latency: std::time::Duration::from_millis(5),
            final_object: Some(object),
            trace: AdmissionTrace {
                evidence: TraceEvidence::Observed,
                invocations: Vec::new(),
            },
            diagnostics: Vec::new(),
        }
    }

    fn pod(image: &str) -> serde_json::Value {
        json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "app", "uid": "not-comparable"},
            "spec": {"containers": [{"name": "app", "image": image}]}
        })
    }

    #[test]
    fn recipe_rules_convert_variant_for_variant() {
        assert_eq!(
            normalize_rule_from_recipe(&RecipeNormalizeRule::RemovePointer("/a".to_owned())),
            NormalizeRule::RemovePointer("/a".to_owned())
        );
        assert_eq!(
            normalize_rule_from_recipe(&RecipeNormalizeRule::RemoveAnnotation("k/v".to_owned())),
            NormalizeRule::RemoveAnnotation("k/v".to_owned())
        );
        assert_eq!(
            normalize_rule_from_recipe(&RecipeNormalizeRule::SortNamedArray {
                pointer: "/spec/x".to_owned(),
                key: "name".to_owned(),
            }),
            NormalizeRule::SortNamedArray {
                pointer: "/spec/x".to_owned(),
                key: "name".to_owned(),
            }
        );
    }

    #[test]
    fn the_profile_keeps_the_built_ins_and_unions_both_sides_recipe_rules_without_duplicates() {
        let mut lab = minimal_lab();
        let shared = RecipeNormalizeRule::RemovePointer("/metadata/annotations/shared".to_owned());
        lab.baseline.components = vec![component_with_rules(
            "kyverno",
            vec![
                shared.clone(),
                RecipeNormalizeRule::RemoveAnnotation("baseline-only".to_owned()),
            ],
        )];
        lab.candidate.components = vec![component_with_rules("kyverno", vec![shared])];

        let profile = normalization_profile(&lab);
        assert_eq!(
            profile.built_in,
            admissionlab_normalize::built_in_rules(),
            "the built-in tier must survive untouched"
        );
        assert_eq!(
            profile.recipe,
            vec![
                NormalizeRule::RemovePointer("/metadata/annotations/shared".to_owned()),
                NormalizeRule::RemoveAnnotation("baseline-only".to_owned()),
            ],
            "both sides' rules, deduplicated, in baseline-then-candidate order"
        );
        assert!(
            profile.user.is_empty(),
            "there is no user rule surface in admissionlab.yaml yet"
        );
    }

    #[test]
    fn an_identical_pair_produces_no_changes_and_a_real_admission_comparison() {
        let lab = minimal_lab();
        let fixtures = vec![fixture("pod-0")];
        let outcomes = vec![
            accepted("pod-0", Side::Baseline, pod("nginx:1")),
            accepted("pod-0", Side::Candidate, pod("nginx:1")),
        ];

        let comparison = compare(&lab, &fixtures, &outcomes).expect("comparison must succeed");
        assert_eq!(comparison.fixtures.len(), 1);
        assert!(comparison.fixtures[0].changes.is_empty());
        assert!(comparison.fixtures[0].admission.is_some());
        assert!(comparison.changes().is_empty());
    }

    #[test]
    fn a_changed_image_is_claimed_and_attributed_to_its_own_fixture() {
        let lab = minimal_lab();
        let fixtures = vec![fixture("pod-0")];
        let outcomes = vec![
            accepted("pod-0", Side::Baseline, pod("nginx:1")),
            accepted("pod-0", Side::Candidate, pod("nginx:2")),
        ];

        let comparison = compare(&lab, &fixtures, &outcomes).expect("comparison must succeed");
        let changes = comparison.changes();
        assert_eq!(
            changes.len(),
            1,
            "expected one image change, got {changes:?}"
        );
        assert_eq!(
            changes[0].kind,
            admissionlab_diff::SemanticChangeKind::ImageChanged
        );
        assert_eq!(
            changes[0].fixture_id.as_str(),
            "pod-0",
            "the caller must stamp the real fixture id over the `unattributed` sentinel"
        );
    }

    #[test]
    fn a_rejected_side_produces_a_decision_change_and_no_workload_noise() {
        let lab = minimal_lab();
        let fixtures = vec![fixture("pod-0")];
        let mut candidate = accepted("pod-0", Side::Candidate, pod("nginx:1"));
        candidate.decision = AdmissionDecision::Rejected {
            code: Some(403),
            message: "denied by policy".to_owned(),
        };
        // A rejected fixture has no admitted object at all.
        candidate.final_object = None;
        let outcomes = vec![accepted("pod-0", Side::Baseline, pod("nginx:1")), candidate];

        let comparison = compare(&lab, &fixtures, &outcomes).expect("comparison must succeed");
        let changes = comparison.changes();
        assert_eq!(
            changes.iter().map(|change| change.kind).collect::<Vec<_>>(),
            vec![admissionlab_diff::SemanticChangeKind::ObjectNewlyDenied],
            "the decision flip is the whole claim; there is no second object to diff"
        );
    }

    #[test]
    fn a_fixture_captured_on_only_one_side_is_inconclusive_and_says_so() {
        let lab = minimal_lab();
        let fixtures = vec![fixture("pod-0")];
        let outcomes = vec![accepted("pod-0", Side::Baseline, pod("nginx:1"))];

        let comparison = compare(&lab, &fixtures, &outcomes).expect("comparison must succeed");
        assert!(
            comparison.fixtures[0].admission.is_none(),
            "no admission evidence means inconclusive, never identical"
        );
        assert!(
            comparison
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "compare.missing_outcome"),
            "the gap must be reported, not silently counted"
        );
    }

    #[test]
    fn capture_diagnostics_are_summarized_run_level_and_never_become_changes() {
        let lab = minimal_lab();
        let fixtures = vec![fixture("pod-0")];
        let mut baseline = accepted("pod-0", Side::Baseline, pod("nginx:1"));
        baseline.diagnostics = vec![Diagnostic {
            code: "admission.webhook_rejection_metric".to_owned(),
            message: "kube-apiserver's rejection counter for webhook w rose by 1".to_owned(),
            context: BTreeMap::new(),
        }];
        let outcomes = vec![baseline, accepted("pod-0", Side::Candidate, pod("nginx:1"))];

        let comparison = compare(&lab, &fixtures, &outcomes).expect("comparison must succeed");
        assert!(
            comparison
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "admission.webhook_rejection_metric"),
            "metric evidence must reach the run-level diagnostics"
        );
        assert!(
            comparison.changes().is_empty(),
            "a rejection-counter increase is evidence, never a fabricated WebhookFailed change"
        );
    }
}
