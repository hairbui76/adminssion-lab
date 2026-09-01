//! The self-contained static HTML report.
//!
//! [`write_html_report`] renders a [`LabResult`] into one file that
//! opens from a filesystem, with no server and no network. That is the
//! whole design constraint, and everything below follows from it.
//!
//! # Self-contained means self-contained
//!
//! The page has no `<script src>`, no `<link rel="stylesheet">`, no web
//! font, no CDN, and no image URL. All CSS is inline in
//! `templates/report.html`'s single `<style>` block. A report that
//! renders differently depending on whether a CDN is reachable is not
//! evidence, and an artifact attached to a CI run is routinely opened on
//! a machine that cannot reach one. `tests/html.rs` asserts the absence
//! of every external-resource form rather than trusting this paragraph.
//!
//! # No JavaScript at all
//!
//! The per-fixture drill-down is `<details>`/`<summary>`, which every
//! browser implements natively. Nothing on the page needs script, so
//! there is none -- which also makes the "no external script" property
//! trivially true rather than something to police.
//!
//! Fixtures in the `critical` and `warnings` buckets render with
//! `<details open>`. That is the same rule the terminal report follows:
//! a regression must not be behind an interaction. The quieter buckets
//! start collapsed, because a hundred identical fixtures scrolling past
//! the one that failed is its own way of hiding it.
//!
//! # No templating dependency
//!
//! `templates/report.html` is filled in by literal `{{MARKER}}`
//! substitution ([`render_template`]), not by a template engine. The
//! workspace has no templating crate in its graph today, and this page
//! needs exactly one feature -- put this string here. Adding `tera`,
//! `handlebars`, or `askama` to get it would pull a parser, an
//! expression language, and its own escaping rules into a project whose
//! escaping discipline is the security-relevant part (see below).
//! Substitution is a single left-to-right scan, so a substituted value
//! that happens to contain `{{...}}` is never re-scanned.
//!
//! # Escaping
//!
//! **Every** user- or vendor-supplied string is passed through
//! [`escape_html`] before it reaches the page: fixture identifiers,
//! webhook names, container names, rejection messages, API-server
//! warnings, diagnostic codes and messages, expectation identifiers and
//! reasons, divergence explanations, component names and versions, and
//! every JSON payload's serialized text.
//!
//! The discipline that makes that checkable is structural: markup is
//! only ever written from string literals in this module, and data only
//! ever enters through an escaped value. There is no code path that
//! formats an unescaped `&str` into markup. A webhook whose name is
//! `<script>alert(1)</script>` -- and a webhook name is attacker-
//! controlled in exactly the scenario this tool exists for, a candidate
//! stack under test -- reaches the page as text, and `tests/html.rs`
//! plants that sentinel and checks it.
//!
//! [`escape_html`] escapes the full five: `&`, `<`, `>`, `"`, and `'`.
//! Quotes are escaped even though no value is currently interpolated
//! into an attribute value, because "no value is currently in an
//! attribute" is a property of today's template, not a guarantee.
//!
//! # Redact first
//!
//! Like every renderer here, this one draws whatever it is given. Hand
//! it a redacted result -- see [`crate::redact::redact_result`].

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use admissionlab_admission::{
    AdmissionDecision, AdmissionOutcome, AdmissionTrace, TraceEvidence, WebhookInvocation,
    WebhookOutcome,
};
use admissionlab_core::Diagnostic;
use admissionlab_diff::{DivergenceConfidence, DivergenceEvidence};
use admissionlab_gateway::{
    CONDITION_ACCEPTED, CONDITION_PROGRAMMED, CONDITION_RESOLVED_REFS, ConditionState,
    GatewayCaseResult, GatewayComparability, GatewayEvidenceLevel, HttpProbeResult,
    ObservedCondition, ParentIdentity, RouteEvidence, RouteParentStatus,
};
use admissionlab_policy::{ClassifiedChange, PolicyDisposition, Severity};
use serde_json::Value;

use crate::error::ReportError;
use crate::json::write_atomic;
use crate::model::{
    AdmissionComparison, FixtureBucket, FixtureComparison, GatewayCaseComparison, LabResult,
    RunSummary,
};

/// The page skeleton, embedded at compile time so the built binary
/// carries no runtime file dependency of its own.
const TEMPLATE: &str = include_str!("templates/report.html");

/// Renders `result` as a standalone HTML page and writes it to `path`
/// atomically.
///
/// See this module's documentation for what "standalone" guarantees and
/// how escaping is enforced. Atomicity is the same temp-write-fsync-
/// rename used by [`crate::json::write_json_report`]; see that module's
/// "Why not `ArtifactStore`" section.
///
/// # Errors
///
/// Returns [`ReportError::Io`] if creating, writing, syncing, or
/// renaming the temporary file fails -- including when `path` has no
/// parent directory, or when that directory does not exist.
/// [`ReportError::Serialize`] is never returned: rendering cannot fail.
pub fn write_html_report(path: &Path, result: &LabResult) -> Result<(), ReportError> {
    write_atomic(path, render_html(result).as_bytes())
}

/// Renders `result` as the standalone HTML page's full text.
///
/// Public so the tests can assert on the markup without a filesystem
/// round trip. [`write_html_report`] is defined in terms of this, so the
/// two can never disagree about what the page contains.
#[must_use]
pub fn render_html(result: &LabResult) -> String {
    let verdict = match result.policy.disposition {
        PolicyDisposition::Pass => "pass",
        PolicyDisposition::Warn => "warn",
        PolicyDisposition::Fail => "fail",
    };
    let values = BTreeMap::from([
        (
            "TITLE",
            escape_html(&format!(
                "Admission Lab result - run {}",
                result.run_id.as_str()
            )),
        ),
        ("RUN_ID", escape_html(result.run_id.as_str())),
        ("SCHEMA_VERSION", escape_html(&result.schema_version)),
        ("VERDICT", escape_html(verdict)),
        ("SUMMARY_COUNTS", render_summary_counts(&result.summary)),
        ("ENVIRONMENTS", render_environments(result)),
        ("FIXTURES", render_fixtures(result)),
        ("MIGRATION", render_migration(result)),
        ("STALE_EXPECTATIONS", render_stale_expectations(result)),
        ("DIAGNOSTICS", render_diagnostics(&result.diagnostics)),
    ]);
    render_template(TEMPLATE, &values)
}

/// Replaces every `{{NAME}}` in `template` with `values[NAME]`.
///
/// One left-to-right pass over the template: a value's own text is
/// appended to the output and never re-examined, so a fixture identifier
/// or a rejection message containing `{{FIXTURES}}` cannot cause a
/// second substitution.
///
/// An unknown marker is emitted verbatim rather than silently dropped.
/// The template is compiled in, so a marker with no value is a typo in
/// this crate; leaving it visible makes it fail a test instead of
/// producing a page with a quietly empty section.
fn render_template(template: &str, values: &BTreeMap<&str, String>) -> String {
    let mut out = String::with_capacity(template.len() * 2);
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let Some(end_offset) = rest[start..].find("}}") else {
            break;
        };
        let end = start + end_offset;
        let name = &rest[start + 2..end];
        out.push_str(&rest[..start]);
        match values.get(name) {
            Some(value) => out.push_str(value),
            None => out.push_str(&rest[start..end + 2]),
        }
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Escapes `text` for insertion anywhere in the page.
///
/// All five of `&`, `<`, `>`, `"`, and `'`. See this module's "Escaping"
/// section for why the two quote forms are included even though no value
/// currently lands in an attribute.
#[must_use]
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// One `<li>` per bucket, in the summary's own field order.
fn render_summary_counts(summary: &RunSummary) -> String {
    let mut out = String::new();
    for (label, count) in [
        ("identical", summary.identical),
        ("expected", summary.expected),
        ("warnings", summary.warnings),
        ("critical", summary.critical),
        ("inconclusive", summary.inconclusive),
    ] {
        let _ = write!(
            out,
            "<li class=\"b-{label}\"><span class=\"count\">{count}</span>\
             <span class=\"label\">{label}</span></li>"
        );
    }
    let _ = write!(
        out,
        "<li><span class=\"count\">{total}</span>\
         <span class=\"label\">fixtures total</span></li>",
        total = summary.fixtures_total
    );
    out
}

/// A two-column table of what each side actually was.
fn render_environments(result: &LabResult) -> String {
    let mut out =
        String::from("<table><tr><th>side</th><th>Kubernetes</th><th>components</th></tr>");
    for (side, environment) in [
        ("baseline", &result.environments.baseline),
        ("candidate", &result.environments.candidate),
    ] {
        let components = if environment.components.is_empty() {
            "<span class=\"empty\">none recorded</span>".to_owned()
        } else {
            environment
                .components
                .iter()
                .map(|component| {
                    format!(
                        "{} <code>{}</code>",
                        escape_html(&component.name),
                        escape_html(&component.version)
                    )
                })
                .collect::<Vec<_>>()
                .join("<br />")
        };
        let _ = write!(
            out,
            "<tr><td>{side}</td><td><code>{kubernetes}</code></td><td>{components}</td></tr>",
            kubernetes = escape_html(&environment.kubernetes),
        );
    }
    out.push_str("</table>");
    out
}

/// Every fixture as a drill-down panel, in the run's own order.
fn render_fixtures(result: &LabResult) -> String {
    if result.fixtures.is_empty() {
        return "<p class=\"empty\">No fixtures were replayed.</p>".to_owned();
    }
    result.fixtures.iter().map(render_fixture).collect()
}

/// One fixture's panel: decision pair, changes, divergence, trace, and
/// what raw evidence is available.
fn render_fixture(fixture: &FixtureComparison) -> String {
    let bucket = fixture.bucket();
    let open = match bucket {
        // A regression must not be behind an interaction.
        FixtureBucket::Critical | FixtureBucket::Warnings => " open",
        _ => "",
    };
    let mut out = format!(
        "<details class=\"fixture b-{bucket}\"{open}><summary>\
         <span class=\"badge badge-{bucket}\">{bucket}</span><code>{id}</code>\
         </summary>",
        bucket = bucket.as_str(),
        id = escape_html(fixture.fixture_id.as_str()),
    );

    match (&fixture.admission, &fixture.gateway) {
        (Some(admission), _) => {
            out.push_str("<h3>Decision</h3>");
            out.push_str(&render_decision_table(admission));
            out.push_str("<h3>First divergence</h3>");
            out.push_str(&render_divergence(admission.first_divergence.as_ref()));
            out.push_str("<h3>Changes</h3>");
            out.push_str(&render_changes(&fixture.changes));
            out.push_str("<h3>Webhook trace</h3>");
            out.push_str(&render_trace("baseline", &admission.baseline.trace));
            out.push_str(&render_trace("candidate", &admission.candidate.trace));
            out.push_str("<h3>Raw evidence</h3>");
            out.push_str(&render_raw_availability(admission));
        }
        // A Gateway route contract (ROADMAP Task 6.11). Deliberately the
        // same panel shape as an admission fixture -- an evidence
        // section, then the graded changes -- rather than a second
        // layout, because a reader scanning a report should not have to
        // learn two ways to read a finding.
        (None, Some(gateway)) => {
            out.push_str("<h3>Comparability</h3>");
            out.push_str(&render_gateway_comparability(gateway));
            out.push_str("<h3>Reconciliation</h3>");
            out.push_str(&render_gateway_conditions(gateway));
            out.push_str("<h3>Traffic</h3>");
            out.push_str(&render_gateway_traffic(gateway));
            out.push_str("<h3>Changes</h3>");
            out.push_str(&render_changes(&fixture.changes));
        }
        (None, None) => out.push_str(
            "<p class=\"note\">No evidence was captured for this entry on either side, \
             so nothing about the two sides' behavior was established.</p>",
        ),
    }

    out.push_str("</details>");
    out
}

/// How comparable one route contract's two sides were, in the same words
/// the terminal report uses.
fn render_gateway_comparability(gateway: &GatewayCaseComparison) -> String {
    let (class, text) = match gateway.comparability() {
        GatewayComparability::Comparable => (
            "note",
            "Both sides converged, so differences and absences between them are evidence."
                .to_owned(),
        ),
        GatewayComparability::Partial {
            baseline,
            candidate,
        } => (
            "warn",
            format!(
                "Only one side converged (baseline {}, candidate {}). Differences between \
                 values both sides published are real, but this contract is counted \
                 inconclusive: an empty change list here would not mean the two sides agreed.",
                evidence_level_label(baseline),
                evidence_level_label(candidate),
            ),
        ),
        GatewayComparability::Incomparable {
            baseline,
            candidate,
        } => (
            "warn",
            format!(
                "Neither side converged (baseline {}, candidate {}). Nothing was compared.",
                evidence_level_label(baseline),
                evidence_level_label(candidate),
            ),
        ),
    };
    format!("<p class=\"{class}\">{}</p>", escape_html(&text))
}

/// One side's evidence level, in words.
fn evidence_level_label(level: GatewayEvidenceLevel) -> &'static str {
    match level {
        GatewayEvidenceLevel::Converged => "converged",
        GatewayEvidenceLevel::Unconverged => "unconverged but current",
        GatewayEvidenceLevel::Stale => "stale",
    }
}

/// Both sides' `GatewayClass`, `Gateway` and per-parent `HTTPRoute`
/// conditions, side by side -- the same table shape the decision pair
/// uses.
fn render_gateway_conditions(gateway: &GatewayCaseComparison) -> String {
    let mut out = String::from("<table><tr><th></th><th>baseline</th><th>candidate</th></tr>");
    let row = |label: String, baseline: String, candidate: String| {
        format!(
            "<tr><th>{}</th><td>{}</td><td>{}</td></tr>",
            escape_html(&label),
            escape_html(&baseline),
            escape_html(&candidate),
        )
    };
    let sides = [&gateway.baseline, &gateway.candidate];
    out.push_str(&row(
        "converged".to_owned(),
        converged_text(&gateway.baseline),
        converged_text(&gateway.candidate),
    ));
    out.push_str(&row(
        "GatewayClass Accepted".to_owned(),
        gateway_class_text(&gateway.baseline),
        gateway_class_text(&gateway.candidate),
    ));
    for type_name in [CONDITION_ACCEPTED, CONDITION_PROGRAMMED] {
        out.push_str(&row(
            format!("Gateway {type_name}"),
            condition_text(&sides[0].reconciliation.gateway.condition(type_name)),
            condition_text(&sides[1].reconciliation.gateway.condition(type_name)),
        ));
    }
    // Keyed by the parent identity each side published, so a parent
    // present on only one side shows as `Missing` on the other rather
    // than shifting every row below it.
    let mut parents: Vec<&ParentIdentity> = Vec::new();
    for side in sides {
        for parent in &side.reconciliation.route.parents {
            if !parents.contains(&&parent.parent) {
                parents.push(&parent.parent);
            }
        }
    }
    for parent in parents {
        for type_name in [CONDITION_ACCEPTED, CONDITION_RESOLVED_REFS] {
            out.push_str(&row(
                format!("HTTPRoute via {} {type_name}", parent_text(parent)),
                parent_condition_text(&sides[0].reconciliation.route, parent, type_name),
                parent_condition_text(&sides[1].reconciliation.route, parent, type_name),
            ));
        }
    }
    out.push_str("</table>");
    out
}

/// Whether one side converged, and how long its wait took.
fn converged_text(case: &GatewayCaseResult) -> String {
    format!(
        "{} in {}ms",
        if case.reconciliation.converged {
            "yes"
        } else {
            "NO"
        },
        case.reconciliation.elapsed.as_millis(),
    )
}

/// One side's `GatewayClass` name and `Accepted` condition, or a plain
/// statement that it observed none.
fn gateway_class_text(case: &GatewayCaseResult) -> String {
    match &case.reconciliation.gateway_class {
        Some(class) => format!("{}: {}", class.name, condition_text(&class.accepted)),
        None => "no GatewayClass was observed".to_owned(),
    }
}

/// One parent's condition on one side, looked up by identity.
fn parent_condition_text(
    route: &RouteEvidence,
    parent: &ParentIdentity,
    type_name: &str,
) -> String {
    let matches: Vec<&RouteParentStatus> = route
        .parents
        .iter()
        .filter(|entry| &entry.parent == parent)
        .collect();
    match matches.as_slice() {
        [entry] => condition_text(&entry.condition(type_name)),
        // The ambiguous case `admissionlab_gateway::conditions` refuses
        // to resolve by position, reported rather than resolved here too.
        [] => "this side published no status for this parent".to_owned(),
        several => format!(
            "{} status entries match this parent; none can be chosen",
            several.len()
        ),
    }
}

/// One condition, with its controller-supplied reason when it has one.
fn condition_text(condition: &ObservedCondition) -> String {
    let state = match condition.state {
        ConditionState::True => "True",
        ConditionState::False => "False",
        ConditionState::Unknown => "Unknown",
        ConditionState::Missing => "Missing",
    };
    match &condition.reason {
        Some(reason) => format!("{state} ({reason})"),
        None => state.to_owned(),
    }
}

/// A route's claimed parent, as `namespace/name#listener`.
fn parent_text(parent: &ParentIdentity) -> String {
    let namespace = parent
        .namespace
        .as_deref()
        .unwrap_or("(the route's own namespace)");
    match &parent.section_name {
        Some(listener) => format!("{namespace}/{}#{listener}", parent.name),
        None => format!("{namespace}/{}", parent.name),
    }
}

/// Both sides' probe results, paired by index, plus every probe only one
/// side answered.
///
/// A side with no probe at all says so in words. That is ROADMAP Task
/// 6.11 step 3's explicit skip: the run did not send a request through
/// that route, and an empty row would read as a request that returned
/// nothing.
fn render_gateway_traffic(gateway: &GatewayCaseComparison) -> String {
    let baseline = &gateway.baseline.probes;
    let candidate = &gateway.candidate.probes;
    if baseline.is_empty() && candidate.is_empty() {
        return "<p class=\"note\">No traffic probe was sent on either side. \
                See this run's diagnostics for the reason each side was skipped.</p>"
            .to_owned();
    }
    let mut out = String::from("<table><tr><th>probe</th><th>baseline</th><th>candidate</th></tr>");
    for index in 0..baseline.len().max(candidate.len()) {
        let _: Result<(), std::fmt::Error> = write!(
            out,
            "<tr><th>#{index}</th><td>{}</td><td>{}</td></tr>",
            escape_html(&probe_text(baseline.get(index))),
            escape_html(&probe_text(candidate.get(index))),
        );
    }
    out.push_str("</table>");
    out
}

/// One probe result, or a statement that this side answered none.
fn probe_text(probe: Option<&HttpProbeResult>) -> String {
    let Some(probe) = probe else {
        return "no probe was sent on this side".to_owned();
    };
    format!(
        "HTTP {} from {} ({} attempt(s), {}ms)",
        probe.status,
        probe
            .backend
            .as_deref()
            .unwrap_or("a backend that did not identify itself"),
        probe.attempts,
        probe.elapsed.as_millis(),
    )
}

/// The baseline/candidate decision pair, side by side.
fn render_decision_table(admission: &AdmissionComparison) -> String {
    let row = |label: &str, extract: &dyn Fn(&AdmissionOutcome) -> String| {
        format!(
            "<tr><th>{label}</th><td>{baseline}</td><td>{candidate}</td></tr>",
            baseline = extract(&admission.baseline),
            candidate = extract(&admission.candidate),
        )
    };
    let mut out = String::from("<table><tr><th></th><th>baseline</th><th>candidate</th></tr>");
    out.push_str(&row("decision", &|outcome| {
        decision_html(&outcome.decision)
    }));
    out.push_str(&row("total latency", &|outcome| {
        format!("{} ms", outcome.total_latency.as_millis())
    }));
    out.push_str(&row("trace evidence", &|outcome| {
        format!(
            "<code>{}</code>",
            escape_html(evidence_label(outcome.trace.evidence))
        )
    }));
    out.push_str(&row("API server warnings", &|outcome| {
        if outcome.warnings.is_empty() {
            "<span class=\"empty\">none</span>".to_owned()
        } else {
            outcome
                .warnings
                .iter()
                .map(|warning| escape_html(warning))
                .collect::<Vec<_>>()
                .join("<br />")
        }
    }));
    out.push_str(&row(
        "final object",
        &|outcome| match &outcome.final_object {
            Some(object) => render_json_block(object),
            None => "<span class=\"empty\">not captured</span>".to_owned(),
        },
    ));
    out.push_str("</table>");
    out
}

/// A decision as readable HTML, with the rejecting message escaped.
fn decision_html(decision: &AdmissionDecision) -> String {
    match decision {
        AdmissionDecision::Accepted => "<code>accepted</code>".to_owned(),
        AdmissionDecision::Rejected { code, message } => {
            let code = code.map_or_else(
                || "<span class=\"empty\">no code observed</span>".to_owned(),
                |code| format!("<code>{code}</code>"),
            );
            format!(
                "<code>rejected</code> {code}<br />{message}",
                message = escape_html(message)
            )
        }
        AdmissionDecision::UnsupportedDryRun { message } => format!(
            "<code>unsupported_dry_run</code><br />{message}",
            message = escape_html(message)
        ),
    }
}

/// Every graded change on one fixture, with its severity and whether an
/// expectation accounted for it.
fn render_changes(changes: &[ClassifiedChange]) -> String {
    if changes.is_empty() {
        return "<p class=\"empty\">No behavior changes were claimed for this fixture.</p>"
            .to_owned();
    }
    let mut out = String::from("<ul class=\"changes\">");
    for classified in changes {
        let change = &classified.change;
        let expected = if classified.expected {
            "<span class=\"badge badge-expected\">expected</span>"
        } else {
            ""
        };
        let _ = write!(
            out,
            "<li><span class=\"badge badge-{severity}\">{severity}</span>{expected}\
             <code>{kind}</code>",
            severity = escape_html(severity_label(classified.severity)),
            kind = escape_html(change.kind.as_str()),
        );
        if let Some(subject) = &change.subject {
            let _ = write!(out, " &middot; {}", escape_html(subject));
        }
        if let Some(path) = &change.object_path {
            let _ = write!(out, " &middot; <code>{}</code>", escape_html(path));
        }
        out.push_str(&render_change_payloads(
            change.baseline.as_ref(),
            change.candidate.as_ref(),
        ));
        if let Some(origin) = &change.origin {
            out.push_str(&render_divergence(Some(origin)));
        }
        out.push_str("</li>");
    }
    out.push_str("</ul>");
    out
}

/// One change's before/after payloads, when it has them.
fn render_change_payloads(baseline: Option<&Value>, candidate: Option<&Value>) -> String {
    if baseline.is_none() && candidate.is_none() {
        return String::new();
    }
    let side = |label: &str, value: Option<&Value>| match value {
        Some(value) => format!("<td>{}</td>", render_json_block(value)),
        None => format!(
            "<td><span class=\"empty\">no {label} value</span></td>",
            label = escape_html(label)
        ),
    };
    format!(
        "<table><tr><th>baseline</th><th>candidate</th></tr><tr>{baseline}{candidate}</tr></table>",
        baseline = side("baseline", baseline),
        candidate = side("candidate", candidate),
    )
}

/// A divergence attribution, with its confidence never dropped.
///
/// `inferred` and `unknown` are spelled out rather than shown as a bare
/// tag: Global Constraint 15 forbids presenting a deduction, or an
/// absence of evidence, as an observation, and a one-word label is
/// exactly where that slips.
fn render_divergence(evidence: Option<&DivergenceEvidence>) -> String {
    let Some(evidence) = evidence else {
        return "<p class=\"note\">Not attributed. This means the divergence was not \
                located, never that the two sides agreed.</p>"
            .to_owned();
    };
    let label = match evidence.confidence {
        DivergenceConfidence::Observed => "observed &mdash; seen directly in both sides' evidence",
        DivergenceConfidence::Inferred => {
            "inferred &mdash; deduced from evidence that was incomplete on at least one side"
        }
        DivergenceConfidence::Unknown => {
            "unknown &mdash; a difference exists but the captured evidence does not locate it"
        }
    };
    let mut out = format!(
        "<p class=\"note\"><strong>confidence:</strong> {label}</p><p>{explanation}</p>",
        explanation = escape_html(&evidence.explanation),
    );
    if evidence.baseline_webhook.is_some() || evidence.candidate_webhook.is_some() {
        let _ = write!(
            out,
            "<p class=\"note\">baseline {baseline} &rarr; candidate {candidate}</p>",
            baseline = webhook_side(
                evidence.baseline_webhook.as_deref(),
                evidence.baseline_position
            ),
            candidate = webhook_side(
                evidence.candidate_webhook.as_deref(),
                evidence.candidate_position
            ),
        );
    }
    out
}

/// One side of a divergence's webhook line.
///
/// A missing webhook prints `none`, which is what
/// [`DivergenceEvidence`]'s own documentation says its `None` means -- an
/// added or removed invocation -- never "not captured".
fn webhook_side(webhook: Option<&str>, position: Option<(u32, u32)>) -> String {
    match (webhook, position) {
        (Some(name), Some((round, index))) => format!(
            "<code>{}</code> (round {round}, index {index})",
            escape_html(name)
        ),
        (Some(name), None) => format!("<code>{}</code>", escape_html(name)),
        (None, _) => "<span class=\"empty\">none</span>".to_owned(),
    }
}

/// One side's webhook trace, with its evidence level stated first.
fn render_trace(side: &str, trace: &AdmissionTrace) -> String {
    let mut out = format!(
        "<p class=\"note\"><strong>{side}</strong> &middot; evidence \
         <code>{evidence}</code></p>",
        evidence = escape_html(evidence_label(trace.evidence)),
    );
    if trace.invocations.is_empty() {
        out.push_str("<p class=\"empty\">No invocations recorded.</p>");
        return out;
    }
    out.push_str(
        "<table><tr><th>round</th><th>index</th><th>configuration</th><th>webhook</th>\
         <th>outcome</th><th>mutated</th><th>latency</th><th>patch</th></tr>",
    );
    for invocation in &trace.invocations {
        out.push_str(&render_invocation(invocation));
    }
    out.push_str("</table>");
    out
}

/// One row of a trace table.
///
/// `mutated` and `latency` print `unknown` / `not measured` for `None`
/// rather than `false` / `0`: those two fields exist precisely to keep
/// "not observed" distinguishable from "observed to be nothing".
fn render_invocation(invocation: &WebhookInvocation) -> String {
    let mutated = match invocation.mutated {
        Some(true) => "yes".to_owned(),
        Some(false) => "no".to_owned(),
        None => "<span class=\"empty\">unknown</span>".to_owned(),
    };
    let latency = invocation.latency.map_or_else(
        || "<span class=\"empty\">not measured</span>".to_owned(),
        |latency| format!("{} ms", latency.as_millis()),
    );
    let patch = match &invocation.patch {
        Some(operations) => serde_json::to_value(operations).map_or_else(
            |_| "<span class=\"empty\">unavailable</span>".to_owned(),
            |value| render_json_block(&value),
        ),
        None => "<span class=\"empty\">none observed</span>".to_owned(),
    };
    format!(
        "<tr><td>{round}</td><td>{index}</td><td>{configuration}</td><td>{webhook}</td>\
         <td><code>{outcome}</code></td><td>{mutated}</td><td>{latency}</td><td>{patch}</td></tr>",
        round = invocation.round,
        index = invocation.index,
        configuration = escape_html(&invocation.configuration),
        webhook = escape_html(&invocation.webhook),
        outcome = escape_html(outcome_label(invocation.outcome)),
    )
}

/// Whether a raw object diff can be computed from what this report
/// carries, stated per side.
///
/// The report does not compute one -- classification is
/// `admissionlab-diff`'s job and a raw diff is diagnostic evidence, not
/// a claim -- but it does carry both final objects verbatim, so a reader
/// can produce one. Saying which side is missing is more useful than a
/// bare yes/no, because "not captured" on one side is itself a finding.
fn render_raw_availability(admission: &AdmissionComparison) -> String {
    match (
        admission.baseline.final_object.is_some(),
        admission.candidate.final_object.is_some(),
    ) {
        (true, true) => "<p class=\"note\">Both sides' final objects are captured above, \
                         so a raw object diff can be computed from this report.</p>"
            .to_owned(),
        (true, false) => "<p class=\"note\">Only the baseline side's final object was \
                          captured, so no raw object diff is available.</p>"
            .to_owned(),
        (false, true) => "<p class=\"note\">Only the candidate side's final object was \
                          captured, so no raw object diff is available.</p>"
            .to_owned(),
        (false, false) => "<p class=\"note\">Neither side's final object was captured, \
                           so no raw object diff is available.</p>"
            .to_owned(),
    }
}

/// Every Ingress-to-Gateway migration case, as a compact table per case
/// (ROADMAP Task 8.8).
///
/// Unlike the terminal renderer, the HTML section's *heading* is always
/// in the page (it is part of the compiled-in template) and this
/// function fills it with a sentence saying the lab declared no
/// migration suite. That difference is deliberate and is the same one
/// the Stale expectations and Diagnostics sections already make: a
/// terminal report is read once and scrolls away, so an empty section is
/// noise; a page is navigated, so a section that vanishes leaves a
/// reader wondering whether the tool looked.
///
/// Every difference's `detail` -- the observed statuses, backends and
/// redirect targets -- is rendered in full. ROADMAP Task 8.8 step 2
/// requires a migration report to explain the observed traffic
/// difference rather than an annotation mismatch, and this is where a
/// reader of the HTML artifact finds it.
fn render_migration(result: &LabResult) -> String {
    let Some(cases) = &result.migration else {
        return "<p class=\"empty\">This lab declares no <code>migration:</code> suite, so no \
                Ingress-to-Gateway comparison was performed.</p>"
            .to_owned();
    };
    if cases.is_empty() {
        return "<p class=\"empty\">The migration suite compared no cases.</p>".to_owned();
    }

    let mut out = String::new();
    for case in cases {
        let _ = write!(
            out,
            "<h3><code>{case_id}</code></h3><p class=\"note\">{reason}</p>",
            case_id = escape_html(&case.case_id),
            reason = escape_html(case.comparability.reason()),
        );

        if case.changes.is_empty() {
            out.push_str(
                "<p class=\"empty\">No behavioral difference was observed for this case.</p>",
            );
        } else {
            out.push_str(
                "<table><tr><th>severity</th><th>behavior</th><th>declared</th>\
                 <th>what was observed</th></tr>",
            );
            for graded in &case.changes {
                let _ = write!(
                    out,
                    // The same `badge badge-<severity>` markup a graded
                    // admission change already renders with, so one
                    // severity reads identically wherever it appears on
                    // the page.
                    "<tr><td><span class=\"badge badge-{severity}\">{severity}</span></td>\
                     <td><code>{kind}</code></td><td>{declared}</td><td>{detail}</td></tr>",
                    severity = severity_label(graded.severity),
                    kind = escape_html(graded.change.kind.as_str()),
                    declared = if graded.change.expected {
                        "expected"
                    } else {
                        "not declared"
                    },
                    detail = escape_html(&graded.change.detail),
                );
            }
            out.push_str("</table>");
        }

        if !case.probes.is_empty() {
            out.push_str(
                "<table><tr><th>probe</th><th>Ingress (baseline)</th>\
                 <th>Gateway (candidate)</th></tr>",
            );
            for (index, pair) in case.probes.iter().enumerate() {
                let _ = write!(
                    out,
                    "<tr><td>#{index}</td><td>{baseline}</td><td>{candidate}</td></tr>",
                    baseline = probe_text(Some(&pair.baseline)),
                    candidate = probe_text(Some(&pair.candidate)),
                );
            }
            out.push_str("</table>");
        }

        for expectation in &case.unmatched_expectations {
            let _ = write!(
                out,
                "<p class=\"note\">Declared non-portable but not carried by this case's baseline \
                 manifests: <code>{feature}</code> &mdash; {reason}</p>",
                feature = escape_html(&expectation.feature),
                reason = escape_html(&expectation.reason),
            );
        }
    }
    out
}

/// Expectations that matched nothing.
fn render_stale_expectations(result: &LabResult) -> String {
    let stale = &result.policy.stale_expectations;
    if stale.is_empty() {
        return "<p class=\"empty\">Every expectation matched at least one change.</p>".to_owned();
    }
    let mut out = String::from("<table><tr><th>id</th><th>why it is stale</th></tr>");
    for entry in stale {
        let _ = write!(
            out,
            "<tr><td><code>{id}</code></td><td>{reason}</td></tr>",
            id = escape_html(&entry.id),
            reason = escape_html(&entry.reason),
        );
    }
    out.push_str("</table>");
    out
}

/// Run-level diagnostics, including each context entry.
fn render_diagnostics(diagnostics: &[Diagnostic]) -> String {
    if diagnostics.is_empty() {
        return "<p class=\"empty\">No run-level diagnostics were recorded.</p>".to_owned();
    }
    let mut out = String::from("<table><tr><th>code</th><th>message</th><th>context</th></tr>");
    for diagnostic in diagnostics {
        let context = if diagnostic.context.is_empty() {
            "<span class=\"empty\">none</span>".to_owned()
        } else {
            diagnostic
                .context
                .iter()
                .map(|(key, value)| {
                    format!(
                        "<code>{}</code> = {}",
                        escape_html(key),
                        escape_html(&value.to_string())
                    )
                })
                .collect::<Vec<_>>()
                .join("<br />")
        };
        let _ = write!(
            out,
            "<tr><td><code>{code}</code></td><td>{message}</td><td>{context}</td></tr>",
            code = escape_html(&diagnostic.code),
            message = escape_html(&diagnostic.message),
        );
    }
    out.push_str("</table>");
    out
}

/// A JSON payload as an escaped, pretty-printed `<pre>` block.
fn render_json_block(value: &Value) -> String {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    format!("<pre>{}</pre>", escape_html(&text))
}

/// A severity's own wire name, so the page and the JSON artifact use one
/// vocabulary.
fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Critical => "critical",
    }
}

/// A trace evidence level's wire name.
fn evidence_label(evidence: TraceEvidence) -> &'static str {
    match evidence {
        TraceEvidence::Observed => "observed",
        TraceEvidence::Partial => "partial",
        TraceEvidence::Unavailable => "unavailable",
    }
}

/// A webhook outcome's wire name.
fn outcome_label(outcome: WebhookOutcome) -> &'static str {
    match outcome {
        WebhookOutcome::Allowed => "allowed",
        WebhookOutcome::Denied => "denied",
        WebhookOutcome::Errored => "errored",
        WebhookOutcome::Unknown => "unknown",
    }
}
