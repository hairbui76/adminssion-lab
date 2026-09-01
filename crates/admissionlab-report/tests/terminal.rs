//! Golden and behavioral tests for the terminal report.
//!
//! The golden is written inline rather than in `testdata/`: it is the
//! *shape of one function's output*, it is small enough to read in a
//! diff, and having the expected text sit three lines from the assertion
//! is what makes a reviewer notice when a rendering change quietly drops
//! a qualifier. The JSON report's golden lives in `testdata/` for the
//! opposite reason -- that one is a machine-readable contract other
//! tools consume.

mod support;

use admissionlab_diff::DivergenceConfidence;
use admissionlab_policy::Severity;
use admissionlab_report::{LabResult, TerminalOptions, render_terminal};
use support::canonical_result;

/// The exact terminal report for [`canonical_result`].
///
/// Covers all five bucket counts, a critical finding with its object,
/// its semantic-change kind, its object path and its first divergence,
/// two Gateway traffic findings in the warnings section (one of them a
/// probe only the baseline answered), the Gateway route-contract section
/// with both sides' conditions and probes, the Ingress-to-Gateway
/// migration section (ROADMAP Task 8.8) with a `critical` backend
/// regression carrying its observed evidence, an `info` declared
/// non-portability, both paired probes and one declared-but-never-
/// observed expectation, an inconclusive fixture with the candidate's
/// own verbatim reason, a stale expectation, diagnostics, the one-line
/// stage timings block, and the verdict.
///
/// The timings line omits `report` and `cleanup`, and that is the point
/// of having it in the golden: a `result.json` is written *during* the
/// reporting stage and *before* cleanup, so neither stage can be inside
/// the value this renders (see `admissionlab_core::timing`). They are
/// absent, not zero.
const GOLDEN: &str = r#"Admission Lab result  run beta-demo-run
schema admissionlab.io/result/v1 (frozen; additive changes only)

Environments
  baseline   Kubernetes v1.34.1  (sidecar-injector 1.26.3)
  candidate  Kubernetes v1.34.1  (sidecar-injector 1.27.0)

Summary  5 fixtures
  identical    1
  expected     1
  warnings     1
  critical     1
  inconclusive 1

Critical  1
  deployment-sidecar [istio-proxy]
    container_added at /spec/template/spec/containers/1
    first divergence [observed]: the container appears in inject.example.com's candidate patch
      baseline none -> candidate inject.example.com (round 0, index 0)

Warnings  2
  echo-route-contract [echo-route]
    traffic_status_changed
    baseline HTTP 200 from echo-v1 -> candidate HTTP 503 from echo-v1
  echo-route-contract [echo-route]
    traffic_status_changed
    baseline HTTP 204 from echo-v1 -> candidate answered nothing

Gateway  1 route contract(s)
  echo-route-contract  both sides converged; differences and absences are evidence
    baseline: converged in 4180ms
      GatewayClass lab-gateway-class  Accepted=True (Accepted)
      Gateway default/lab-gateway  Accepted=True (Accepted) Programmed=True (Accepted)
      HTTPRoute default/echo-route via default/lab-gateway#http  Accepted=True (Accepted) ResolvedRefs=True (Accepted)
      traffic: probe #0 -> HTTP 200 from echo-v1
      traffic: probe #1 -> HTTP 204 from echo-v1
    candidate: converged in 4180ms
      GatewayClass lab-gateway-class  Accepted=True (Accepted)
      Gateway default/lab-gateway  Accepted=True (Accepted) Programmed=True (Accepted)
      HTTPRoute default/echo-route via default/lab-gateway#http  Accepted=True (Accepted) ResolvedRefs=True (Accepted)
      traffic: probe #0 -> HTTP 503 from echo-v1

Migration  1 Ingress-to-Gateway case(s)
  legacy-echo  both sides answered the case's probes, so a difference and an absence of differences are both evidence
    critical backend_changed
      probe 1 (GET http://migrate.ingress.admissionlab.test/legacy/reports): the Ingress reached backend "echo-b" and the Gateway reached "echo-a"
    info     non_portable_feature (expected)
      nginx.ingress.kubernetes.io/limit-rps on Ingress admissionlab-migration-demo/echo-ingress has no portable Gateway API equivalent: per-client request rate limiting in the data plane.
    probe #0: Ingress HTTP 200 from echo-a -> Gateway HTTP 200 from echo-a
    probe #1: Ingress HTTP 200 from echo-b -> Gateway HTTP 200 from echo-a
    declared non-portable but never observed: nginx.ingress.kubernetes.io/canary (the canary Ingress was deleted before this migration; kept here until the rollout is confirmed)

Inconclusive  1
  crd-custom-resource
    candidate: the candidate cluster's CRD does not accept server-side dry-run

Stale expectations  1
  sidecar-injection-rollout: no matching change was observed in this run

Diagnostics  2
  metrics.unavailable: per-webhook latency metrics were not scraped on the candidate side
  kubeconfig.loaded: loaded isolated kubeconfigs for both sides

Stage timings
  clusters 43.51s (baseline 41.20s, candidate 43.12s), install 96.40s, capture 6.12s (baseline 5.94s, candidate 6.11s) [4 fixture(s)/side], gateway 9.74s (baseline 9.40s, candidate 9.73s), compare 0.21s, elapsed 149.01s

Result: fail
"#;

/// Removes every ANSI escape sequence, so a colored render can be
/// compared against a plain one.
///
/// Handles only the CSI sequences this renderer emits (`ESC [ ... m`),
/// which is all it needs to: a sequence this does not recognize would
/// leave an `ESC` behind and fail the assertion, which is the correct
/// outcome for a renderer that started emitting something unexpected.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            out.push(character);
            continue;
        }
        // Skip the `[`, then everything up to and including the final
        // byte in the `@`..=`~` range that terminates a CSI sequence.
        for next in chars.by_ref() {
            if ('\u{40}'..='\u{7e}').contains(&next) && next != '[' {
                break;
            }
        }
    }
    out
}

#[test]
fn the_canonical_summary_matches_the_golden() {
    let rendered = render_terminal(&canonical_result(), &TerminalOptions::default());

    assert_eq!(rendered, GOLDEN, "rendered report was:\n{rendered}");
}

#[test]
fn the_plain_render_contains_no_ansi_escapes() {
    let rendered = render_terminal(&canonical_result(), &TerminalOptions { color: false });

    assert!(
        !rendered.contains('\u{1b}'),
        "a report written to a file, a pipe, or a CI log must carry no escapes"
    );
}

#[test]
fn the_colored_render_is_the_same_text_modulo_escapes() {
    let result = canonical_result();
    let plain = render_terminal(&result, &TerminalOptions { color: false });
    let colored = render_terminal(&result, &TerminalOptions { color: true });

    assert!(
        colored.contains('\u{1b}'),
        "the colored render must actually emit escapes, or this test proves nothing"
    );
    assert_eq!(
        strip_ansi(&colored),
        plain,
        "color must change only the escapes, never a character of the report"
    );
}

#[test]
fn color_is_enabled_only_for_a_tty_with_no_color_unset() {
    assert!(TerminalOptions::for_stream(true, true).color);
    assert!(
        !TerminalOptions::for_stream(false, true).color,
        "a pipe or a file gets no escapes"
    );
    assert!(
        !TerminalOptions::for_stream(true, false).color,
        "`NO_COLOR` being set disables color even on a TTY"
    );
    assert!(!TerminalOptions::for_stream(false, false).color);
    assert!(
        !TerminalOptions::default().color,
        "the safe default for an undescribed stream is no color"
    );
}

#[test]
fn an_unknown_confidence_divergence_is_labeled_as_such() {
    // The canonical result's `unknown` attribution hangs off a change
    // that was expected, so it is promoted to an unexpected warning here
    // to bring it into a rendered section.
    let rendered = render_terminal(&promoted_expected_change(), &TerminalOptions::default());

    assert!(
        rendered.contains(
            "first divergence [unknown (evidence does not locate the divergence)]: both traces \
             are identical"
        ),
        "an unlocated divergence must never read like an observation; report was:\n{rendered}"
    );
}

#[test]
fn an_inferred_confidence_divergence_is_labeled_as_such() {
    let mut result = promoted_expected_change();
    let origin = result.fixtures[1]
        .admission
        .as_ref()
        .and_then(|admission| admission.first_divergence.clone())
        .expect("the expected fixture carries a fixture-level divergence");
    result.policy.changes[1].change.origin = Some(admissionlab_diff::DivergenceEvidence {
        confidence: DivergenceConfidence::Inferred,
        ..origin
    });

    let rendered = render_terminal(&result, &TerminalOptions::default());

    assert!(
        rendered.contains("first divergence [inferred (deduced from incomplete evidence)]:"),
        "a deduction must never read like an observation; report was:\n{rendered}"
    );
}

#[test]
fn an_unattributed_change_says_so_rather_than_staying_silent() {
    let mut result = canonical_result();
    result.policy.changes[0].change.origin = None;
    result.fixtures[0]
        .admission
        .as_mut()
        .expect("the critical fixture has an admission comparison")
        .first_divergence = None;

    let rendered = render_terminal(&result, &TerminalOptions::default());

    assert!(
        rendered.contains("first divergence: not attributed"),
        "silence would read as `there was no divergence`; report was:\n{rendered}"
    );
}

#[test]
fn every_warning_is_rendered_with_no_truncation() {
    let mut result = canonical_result();
    let template = result.policy.changes[0].clone();
    result.policy.changes = (0..25)
        .map(|index| {
            let mut classified = template.clone();
            classified.severity = Severity::Warning;
            classified.expected = false;
            classified.change.subject = Some(format!("sidecar-{index}"));
            classified
        })
        .collect();

    let rendered = render_terminal(&result, &TerminalOptions::default());

    assert!(rendered.contains("Warnings  25"));
    for index in 0..25 {
        assert!(
            rendered.contains(&format!("[sidecar-{index}]")),
            "warning {index} was not rendered; nothing may be hidden behind a cutoff"
        );
    }
}

#[test]
fn expected_changes_are_not_rendered_as_findings() {
    let rendered = render_terminal(&canonical_result(), &TerminalOptions::default());

    assert!(
        !rendered.contains("image_changed"),
        "an expected change is counted, not reported as a finding"
    );
    assert!(
        rendered.contains("expected     1"),
        "but its count is still visible"
    );
}

#[test]
fn findings_are_rendered_in_policy_order() {
    let mut result = canonical_result();
    let template = result.policy.changes[0].clone();
    result.policy.changes = ["zulu", "alpha", "mike"]
        .into_iter()
        .map(|subject| {
            let mut classified = template.clone();
            classified.expected = false;
            classified.change.subject = Some(subject.to_owned());
            classified
        })
        .collect();

    let rendered = render_terminal(&result, &TerminalOptions::default());
    let positions: Vec<usize> = ["[zulu]", "[alpha]", "[mike]"]
        .into_iter()
        .map(|needle| {
            rendered
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} is missing from:\n{rendered}"))
        })
        .collect();

    assert!(
        positions[0] < positions[1] && positions[1] < positions[2],
        "the renderer must preserve `admissionlab-policy`'s order rather than re-sorting"
    );
}

#[test]
fn rendering_is_deterministic() {
    let result = canonical_result();

    assert_eq!(
        render_terminal(&result, &TerminalOptions::default()),
        render_terminal(&result, &TerminalOptions::default())
    );
}

#[test]
fn a_result_without_timings_renders_no_stage_timings_block() {
    let mut result = canonical_result();
    result.timings = None;

    let rendered = render_terminal(&result, &TerminalOptions::default());

    assert!(
        !rendered.contains("Stage timings"),
        "a run that measured nothing must render no timings heading at all, rather than a \
         heading over zeroes: {rendered}"
    );
}

/// [`canonical_result`] with its expected change promoted to an
/// unexpected warning, so the `unknown`-confidence attribution it hangs
/// off reaches a rendered section.
fn promoted_expected_change() -> LabResult {
    let mut result = canonical_result();
    result.policy.changes[1].expected = false;
    result.policy.changes[1].severity = Severity::Warning;
    result
}
