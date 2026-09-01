//! The concise terminal summary a human reads immediately after a run.
//!
//! [`render_terminal`] turns a [`LabResult`] into a plain [`String`].
//! Not a writer, not a progress display, not a pager: a string the caller
//! prints. That shape is what makes the guarantee below possible.
//!
//! # Nothing is hidden
//!
//! Every unexpected `critical` and `warning` change is rendered in full,
//! always. There is no `--verbose` behind which a regression waits, no
//! truncation after the first *n* findings, no interactive drill-down
//! that a CI log or a piped terminal would silently collapse into
//! nothing. A tool whose job is to tell you a stack's behavior changed
//! cannot make the telling conditional on the reader's terminal.
//!
//! The `identical` and `expected` buckets *are* summarized to counts,
//! and the fixtures in them are not listed one by one. That is not
//! hiding: those counts appear in the summary, they sum to the total,
//! and the full detail for every one of them is in the JSON and HTML
//! artifacts the same run writes.
//!
//! # Color is the caller's decision, not this function's
//!
//! [`render_terminal`] never reads an environment variable, never
//! inspects a file descriptor, and never asks whether anything is a
//! terminal. It renders whatever [`TerminalOptions::color`] says. A pure
//! function of its two arguments is testable -- the golden tests below
//! render both ways and compare -- and a renderer that sniffed the
//! process's environment would produce different output under `cargo
//! test` than under a CI job for reasons unrelated to the run.
//!
//! The *policy* still lives here, in
//! [`TerminalOptions::for_stream`]: color only when the stream is a TTY
//! **and** `NO_COLOR` is unset. What lives in the CLI is only the two
//! observations that feed it (Task 4.14 wires
//! `std::io::IsTerminal` and `std::env::var_os("NO_COLOR")` to that
//! constructor). Splitting it this way keeps the rule itself under test
//! while keeping the syscalls out of a pure function.
//!
//! # Ordering
//!
//! Findings are rendered in `admissionlab-policy`'s own documented
//! order, by iterating [`PolicyResult::changes`] directly rather than
//! re-sorting. That order is stable across runs and deliberately *not*
//! keyed on severity, so a policy override that re-grades one change
//! does not reshuffle the whole report. Sections group by severity;
//! within a section the policy order is preserved.
//!
//! # Honest attribution
//!
//! A [`DivergenceEvidence`]'s confidence is always printed alongside its
//! explanation, and `inferred`/`unknown` are labeled as what they are.
//! Global Constraint 15 forbids presenting a deduction or an absence of
//! evidence as an observation, and the place that rule is easiest to
//! break is a one-line summary that drops the qualifier to save space.

use std::collections::BTreeMap;
// Every line of this report is written with `write!`/`writeln!` rather
// than `push_str(&format!(...))`: the latter allocates a second string
// per line for no reason. `String`'s `fmt::Write` impl is infallible
// (its `write_str` returns `Ok(())` unconditionally), which is why every
// call site discards the `Result` with `let _ =` rather than propagating
// an error that cannot occur or panicking on one.
use std::fmt::Write as _;

use admissionlab_admission::AdmissionDecision;
use admissionlab_diff::{DivergenceConfidence, DivergenceEvidence};
use admissionlab_gateway::{
    CONDITION_ACCEPTED, CONDITION_PROGRAMMED, CONDITION_RESOLVED_REFS, ConditionState,
    GatewayCaseResult, GatewayComparability, GatewayEvidenceLevel, ObservedCondition,
    ParentIdentity,
};
use admissionlab_policy::{ClassifiedChange, PolicyDisposition, Severity};
use serde_json::Value;

use crate::model::{
    AdmissionComparison, EnvironmentReport, FixtureBucket, FixtureComparison, LabResult, RunSummary,
};

/// How the terminal report should be rendered.
///
/// One field today. It is a struct rather than a bare `bool` parameter
/// because the frozen signature takes `&TerminalOptions`, and because a
/// later task adding (say) a width hint must not change every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalOptions {
    /// Whether to emit ANSI escapes.
    ///
    /// [`Default`] is `false`: the safe answer for a stream nobody has
    /// described is the one that is readable everywhere, including in a
    /// file, a pipe, and a CI log.
    pub color: bool,
}

impl TerminalOptions {
    /// The options for a stream the caller has observed.
    ///
    /// Color is enabled only when `is_terminal` **and** `no_color_unset`
    /// are both true -- the `NO_COLOR` convention: the variable's
    /// *presence* disables color regardless of its value, so a caller
    /// passes `std::env::var_os("NO_COLOR").is_none()`, not a parsed
    /// boolean.
    ///
    /// Both observations are the caller's to make. See this module's
    /// documentation for why the syscalls stay out of here while the
    /// rule stays in.
    #[must_use]
    pub fn for_stream(is_terminal: bool, no_color_unset: bool) -> Self {
        Self {
            color: is_terminal && no_color_unset,
        }
    }
}

/// Renders `result` as the terminal report.
///
/// See this module's documentation for what is always shown, why color
/// is a parameter rather than a probe, and how findings are ordered.
#[must_use]
pub fn render_terminal(result: &LabResult, options: &TerminalOptions) -> String {
    let palette = Palette::new(options.color);
    let fixtures = fixture_index(result);
    let mut out = String::new();

    render_header(&mut out, result, &palette);
    render_environments(&mut out, result, &palette);
    render_summary(&mut out, &result.summary, &palette);
    render_findings(
        &mut out,
        result,
        &fixtures,
        Severity::Critical,
        "Critical",
        &palette,
    );
    render_findings(
        &mut out,
        result,
        &fixtures,
        Severity::Warning,
        "Warnings",
        &palette,
    );
    render_gateway(&mut out, result, &palette);
    render_inconclusive(&mut out, result, &palette);
    render_stale_expectations(&mut out, result, &palette);
    render_diagnostics(&mut out, result, &palette);
    render_timings(&mut out, result, &palette);
    render_verdict(&mut out, result, &palette);

    out
}

/// Fixtures keyed by identifier, for the lookups the findings sections
/// need.
///
/// A [`BTreeMap`] rather than a `HashMap`: nothing here iterates it, but
/// a deterministic renderer should not have a hash-ordered structure in
/// it at all, where a later change that *does* iterate would silently
/// become nondeterministic.
fn fixture_index(result: &LabResult) -> BTreeMap<&str, &FixtureComparison> {
    result
        .fixtures
        .iter()
        .map(|fixture| (fixture.fixture_id.as_str(), fixture))
        .collect()
}

/// The run identity line, and the frozen-schema note.
fn render_header(out: &mut String, result: &LabResult, palette: &Palette) {
    let _ = writeln!(
        out,
        "{bold}Admission Lab result{reset}  run {run}",
        bold = palette.bold,
        reset = palette.reset,
        run = result.run_id.as_str(),
    );
    let _ = writeln!(
        out,
        "{dim}schema {schema} (frozen; additive changes only){reset}\n",
        dim = palette.dim,
        reset = palette.reset,
        schema = result.schema_version,
    );
}

/// Both sides' Kubernetes version and installed components.
///
/// Rendered before the findings because "what changed" is only
/// meaningful once a reader knows what the two sides were.
fn render_environments(out: &mut String, result: &LabResult, palette: &Palette) {
    let _ = writeln!(
        out,
        "{bold}Environments{reset}",
        bold = palette.bold,
        reset = palette.reset
    );
    render_environment(out, "baseline", &result.environments.baseline);
    render_environment(out, "candidate", &result.environments.candidate);
    out.push('\n');
}

/// One side's line in the environments block.
fn render_environment(out: &mut String, side: &str, environment: &EnvironmentReport) {
    let components = if environment.components.is_empty() {
        "no components".to_owned()
    } else {
        environment
            .components
            .iter()
            .map(|component| format!("{} {}", component.name, component.version))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let _ = writeln!(
        out,
        "  {side:<10} Kubernetes {kubernetes}  ({components})",
        kubernetes = environment.kubernetes,
    );
}

/// The five bucket counts.
///
/// All five are always printed, including the zeroes: a reader has to be
/// able to tell "no warnings" from "warnings were not counted", and a
/// line that disappears when it reads zero makes those two look alike.
fn render_summary(out: &mut String, summary: &RunSummary, palette: &Palette) {
    let _ = writeln!(
        out,
        "{bold}Summary{reset}  {total} fixtures",
        bold = palette.bold,
        reset = palette.reset,
        total = summary.fixtures_total,
    );
    for (label, count, color) in [
        ("identical", summary.identical, palette.green),
        ("expected", summary.expected, palette.green),
        ("warnings", summary.warnings, palette.yellow),
        ("critical", summary.critical, palette.red),
        ("inconclusive", summary.inconclusive, palette.dim),
    ] {
        let _ = writeln!(
            out,
            "  {color}{label:<13}{reset}{count}",
            reset = palette.reset,
        );
    }
    out.push('\n');
}

/// Every unexpected change at `severity`, in policy order.
///
/// Reads [`LabResult::policy`]'s flat, run-wide change list rather than
/// each fixture's copy: that list is the one `admissionlab-policy`
/// documents an order for, and using it means this renderer never
/// invents one.
fn render_findings(
    out: &mut String,
    result: &LabResult,
    fixtures: &BTreeMap<&str, &FixtureComparison>,
    severity: Severity,
    heading: &str,
    palette: &Palette,
) {
    let color = match severity {
        Severity::Critical => palette.red,
        Severity::Warning => palette.yellow,
        Severity::Info => palette.dim,
    };
    let matching: Vec<&ClassifiedChange> = result
        .policy
        .changes
        .iter()
        .filter(|classified| classified.severity == severity && !classified.expected)
        .collect();

    let _ = writeln!(
        out,
        "{color}{bold}{heading}{reset}  {count}",
        bold = palette.bold,
        reset = palette.reset,
        count = matching.len(),
    );
    if matching.is_empty() {
        let _ = writeln!(
            out,
            "  {dim}none{reset}\n",
            dim = palette.dim,
            reset = palette.reset
        );
        return;
    }

    for classified in matching {
        render_change(out, classified, fixtures, palette);
    }
    out.push('\n');
}

/// One finding: which object, what kind of change, and where it first
/// diverged.
fn render_change(
    out: &mut String,
    classified: &ClassifiedChange,
    fixtures: &BTreeMap<&str, &FixtureComparison>,
    palette: &Palette,
) {
    let change = &classified.change;
    let subject = change
        .subject
        .as_deref()
        .map_or_else(String::new, |subject| format!(" [{subject}]"));
    let _ = writeln!(
        out,
        "  {bold}{fixture}{reset}{subject}",
        bold = palette.bold,
        reset = palette.reset,
        fixture = change.fixture_id.as_str(),
    );
    let _ = write!(out, "    {kind}", kind = change.kind.as_str());
    if let Some(path) = &change.object_path {
        let _ = write!(out, " at {path}");
    }
    out.push('\n');

    // A Gateway change has no webhook chain to have first diverged in,
    // so it gets the evidence it *does* have -- the two sides' own
    // condition states and probe results, read straight out of the
    // change's payloads -- rather than a "first divergence: not
    // attributed" line answering a question that does not apply to it.
    if fixtures
        .get(change.fixture_id.as_str())
        .is_some_and(|fixture| fixture.gateway.is_some())
    {
        render_gateway_change(out, classified, palette);
        return;
    }

    // A change's own attribution is preferred over the fixture-level
    // one: it was computed for *this* difference, while the fixture's
    // `first_divergence` answers the coarser question of where the two
    // sides first parted at all. The fallback still says something true
    // and is labeled with its own confidence.
    let divergence = change.origin.as_ref().or_else(|| {
        fixtures
            .get(change.fixture_id.as_str())
            .and_then(|fixture| fixture.admission.as_ref())
            .and_then(|admission| admission.first_divergence.as_ref())
    });
    match divergence {
        Some(evidence) => render_divergence(out, evidence, palette),
        None => {
            let _ = writeln!(
                out,
                "    {dim}first divergence: not attributed{reset}",
                dim = palette.dim,
                reset = palette.reset,
            );
        }
    }
}

/// One Gateway finding's evidence: which object it is about, and what
/// each side published.
///
/// Read out of the change's own `baseline`/`candidate` payloads, which
/// `admissionlab_gateway::diff` builds with a fixed set of keys
/// (`object`/`name`/`namespace`/`condition`/`state`/`reason` for a
/// condition change, `probeIndex`/`status`/`backend` for a traffic one).
/// Rendering from the payload rather than from the case evidence is what
/// keeps this line describing *this* change: a route with two parents
/// has one finding per parent, and each one carries its own.
///
/// A side whose payload is absent prints as such. For a traffic change
/// that means exactly one thing -- that side answered no probe, because
/// its route never reached a state worth sending one through -- and it
/// is the "skipped traffic behavior" half of ROADMAP Task 6.12's first
/// user-facing failure.
fn render_gateway_change(out: &mut String, classified: &ClassifiedChange, palette: &Palette) {
    let change = &classified.change;
    if let Some(object) = change
        .baseline
        .as_ref()
        .or(change.candidate.as_ref())
        .and_then(payload_object_line)
    {
        let _ = writeln!(
            out,
            "    {dim}{object}{reset}",
            dim = palette.dim,
            reset = palette.reset,
        );
    }
    let _ = writeln!(
        out,
        "    {dim}baseline{reset} {baseline} {dim}->{reset} {dim}candidate{reset} {candidate}",
        dim = palette.dim,
        reset = palette.reset,
        baseline = payload_side_text(change.baseline.as_ref()),
        candidate = payload_side_text(change.candidate.as_ref()),
    );
}

/// Which object a Gateway change payload is about, as
/// `HTTPRoute default/echo-route via default/lab-gateway#http`.
///
/// [`None`] when the payload names no object at all, which no payload
/// this crate renders does -- the fallible read is here so an unexpected
/// payload shape prints one line less rather than panicking.
fn payload_object_line(payload: &Value) -> Option<String> {
    let object = payload.get("object")?.as_str()?;
    let name = payload.get("name").and_then(Value::as_str)?;
    let mut line = match payload.get("namespace").and_then(Value::as_str) {
        Some(namespace) => format!("{object} {namespace}/{name}"),
        None => format!("{object} {name}"),
    };
    if let Some(parent) = payload.get("parent") {
        let parent_name = parent.get("name").and_then(Value::as_str).unwrap_or("?");
        let namespace = parent
            .get("namespace")
            .and_then(Value::as_str)
            .unwrap_or("(the route's own namespace)");
        let _: Result<(), std::fmt::Error> = write!(line, " via {namespace}/{parent_name}");
        if let Some(listener) = parent.get("sectionName").and_then(Value::as_str) {
            let _: Result<(), std::fmt::Error> = write!(line, "#{listener}");
        }
    }
    Some(line)
}

/// One side of a Gateway change payload, as
/// `ResolvedRefs=False (RefNotPermitted)` or `HTTP 200 from echo-a`.
fn payload_side_text(payload: Option<&Value>) -> String {
    let Some(payload) = payload else {
        return "answered nothing".to_owned();
    };
    if let Some(condition) = payload.get("condition").and_then(Value::as_str) {
        let state = payload
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return match payload.get("reason").and_then(Value::as_str) {
            Some(reason) => format!("{condition}={state} ({reason})"),
            None => format!("{condition}={state}"),
        };
    }
    if let Some(status) = payload.get("status").and_then(Value::as_u64) {
        return match payload.get("backend").and_then(Value::as_str) {
            Some(backend) => format!("HTTP {status} from {backend}"),
            None => format!("HTTP {status} from a backend that did not identify itself"),
        };
    }
    // A membership change (`route_attached`/`route_detached`/
    // `listener_binding_changed`) has no state and no status; the object
    // line above already said which Gateway and listener it is about.
    "present".to_owned()
}

/// A divergence attribution, with its confidence spelled out.
///
/// The confidence label is never dropped and never softened: an
/// `inferred` attribution says it was deduced from incomplete evidence,
/// and an `unknown` one says the evidence does not locate the
/// divergence at all (Global Constraint 15).
fn render_divergence(out: &mut String, evidence: &DivergenceEvidence, palette: &Palette) {
    let label = match evidence.confidence {
        DivergenceConfidence::Observed => "observed",
        DivergenceConfidence::Inferred => "inferred (deduced from incomplete evidence)",
        DivergenceConfidence::Unknown => "unknown (evidence does not locate the divergence)",
    };
    let _ = writeln!(
        out,
        "    {dim}first divergence [{label}]:{reset} {explanation}",
        dim = palette.dim,
        reset = palette.reset,
        explanation = evidence.explanation,
    );
    if let Some(webhook) = webhook_line(evidence) {
        let _ = writeln!(
            out,
            "    {dim}  {webhook}{reset}",
            dim = palette.dim,
            reset = palette.reset,
        );
    }
}

/// The `baseline -> candidate` webhook/position line, when either side
/// has one.
///
/// A side with no invocation at the point of divergence prints `none`,
/// which is what [`DivergenceEvidence`]'s own documentation says its
/// `None` means -- an added or removed invocation -- and never a
/// stand-in for "not captured".
fn webhook_line(evidence: &DivergenceEvidence) -> Option<String> {
    if evidence.baseline_webhook.is_none() && evidence.candidate_webhook.is_none() {
        return None;
    }
    Some(format!(
        "baseline {} -> candidate {}",
        webhook_side(
            evidence.baseline_webhook.as_deref(),
            evidence.baseline_position
        ),
        webhook_side(
            evidence.candidate_webhook.as_deref(),
            evidence.candidate_position
        ),
    ))
}

/// One side of [`webhook_line`].
fn webhook_side(webhook: Option<&str>, position: Option<(u32, u32)>) -> String {
    match (webhook, position) {
        (Some(name), Some((round, index))) => format!("{name} (round {round}, index {index})"),
        (Some(name), None) => name.to_owned(),
        (None, _) => "none".to_owned(),
    }
}

/// Every Gateway route contract the run observed, with each side's
/// reconciliation and traffic evidence (ROADMAP Task 6.11).
///
/// Omitted entirely for an admission-only run: a lab with no `gateway:`
/// section has no Gateway section, rather than an empty heading claiming
/// zero routes. Every other section here is unconditional because its
/// count is meaningful (a run always has some number of critical
/// findings, including none); "how many Gateway routes" is not a
/// question an admission-only run answers at all.
///
/// Present *whatever* the comparison found, including when it found
/// nothing: this section is the evidence, not the findings, and a route
/// whose two sides agreed is exactly as much a part of it as one that
/// regressed. `admissionlab_gateway::diff`'s own documentation is
/// explicit that an empty change list can also mean "nothing was worth
/// comparing", which is why the comparability of each pair is stated
/// here rather than left to be inferred from a silent findings list.
fn render_gateway(out: &mut String, result: &LabResult, palette: &Palette) {
    let cases: Vec<&FixtureComparison> = result
        .fixtures
        .iter()
        .filter(|fixture| fixture.gateway.is_some())
        .collect();
    if cases.is_empty() {
        return;
    }

    let _ = writeln!(
        out,
        "{bold}Gateway{reset}  {count} route contract(s)",
        bold = palette.bold,
        reset = palette.reset,
        count = cases.len(),
    );
    for fixture in cases {
        let Some(gateway) = &fixture.gateway else {
            continue;
        };
        let _ = writeln!(
            out,
            "  {bold}{contract}{reset}  {dim}{comparability}{reset}",
            bold = palette.bold,
            dim = palette.dim,
            reset = palette.reset,
            contract = fixture.fixture_id.as_str(),
            comparability = comparability_label(gateway.comparability()),
        );
        for (side, case) in [
            ("baseline", &gateway.baseline),
            ("candidate", &gateway.candidate),
        ] {
            render_gateway_side(out, side, case, palette);
        }
    }
    out.push('\n');
}

/// One side of one route contract: whether it converged, what its three
/// objects published, and what its probes returned.
fn render_gateway_side(out: &mut String, side: &str, case: &GatewayCaseResult, palette: &Palette) {
    let evidence = &case.reconciliation;
    let _ = writeln!(
        out,
        "    {dim}{side}:{reset} {converged} in {elapsed}",
        dim = palette.dim,
        reset = palette.reset,
        converged = if evidence.converged {
            "converged"
        } else {
            // Never softened to "still settling": the waiter spent the
            // whole deadline, and `admissionlab_gateway::reconcile`
            // returns this as evidence rather than an error precisely
            // so a reader sees it (Global Constraint 15).
            "did NOT converge"
        },
        elapsed = format_millis(evidence.elapsed),
    );
    if let Some(class) = &evidence.gateway_class {
        let _ = writeln!(
            out,
            "      GatewayClass {name}  {accepted}",
            name = class.name,
            accepted = condition_text(&class.accepted),
        );
    }
    let _ = writeln!(
        out,
        "      Gateway {identity}  {conditions}",
        identity = evidence.gateway.identity,
        conditions = conditions_text(&[CONDITION_ACCEPTED, CONDITION_PROGRAMMED], |type_name| {
            evidence.gateway.condition(type_name)
        }),
    );
    for parent in &evidence.route.parents {
        let _ = writeln!(
            out,
            "      HTTPRoute {namespace}/{name} via {parent}  {conditions}",
            namespace = evidence.route.namespace,
            name = evidence.route.name,
            parent = parent_text(&parent.parent),
            conditions = conditions_text(
                &[CONDITION_ACCEPTED, CONDITION_RESOLVED_REFS],
                |type_name| parent.condition(type_name)
            ),
        );
    }
    if evidence.route.parents.is_empty() {
        let _ = writeln!(
            out,
            "      HTTPRoute {namespace}/{name}  {dim}published no parent status at all{reset}",
            dim = palette.dim,
            reset = palette.reset,
            namespace = evidence.route.namespace,
            name = evidence.route.name,
        );
    }
    if case.probes.is_empty() {
        // The explicit skip (ROADMAP Task 6.11 step 3). *Why* it was
        // skipped is a run-level `gateway.probe_skipped` diagnostic,
        // rendered in the Diagnostics section with the specific
        // condition that was not `True`; saying "none" here without
        // saying "no probe was sent" would let an empty list read as a
        // probe that returned nothing.
        let _ = writeln!(
            out,
            "      {dim}traffic: no probe was sent{reset}",
            dim = palette.dim,
            reset = palette.reset,
        );
    }
    for (index, probe) in case.probes.iter().enumerate() {
        let _ = writeln!(
            out,
            "      traffic: probe #{index} -> HTTP {status} from {backend}",
            status = probe.status,
            backend = probe
                .backend
                .as_deref()
                // Never "unknown backend" softened into a name: the
                // response did not identify itself, which is a different
                // fact from "a backend called none answered".
                .unwrap_or("a backend that did not identify itself"),
        );
    }
}

/// How comparable one route contract's two sides were, in words.
fn comparability_label(comparability: GatewayComparability) -> String {
    match comparability {
        GatewayComparability::Comparable => {
            "both sides converged; differences and absences are evidence".to_owned()
        }
        GatewayComparability::Partial {
            baseline,
            candidate,
        } => format!(
            "only one side converged (baseline {}, candidate {}); counted inconclusive",
            evidence_level_label(baseline),
            evidence_level_label(candidate),
        ),
        GatewayComparability::Incomparable {
            baseline,
            candidate,
        } => format!(
            "neither side converged (baseline {}, candidate {}); nothing was compared",
            evidence_level_label(baseline),
            evidence_level_label(candidate),
        ),
    }
}

/// One side's own evidence level, in words.
fn evidence_level_label(level: GatewayEvidenceLevel) -> &'static str {
    match level {
        GatewayEvidenceLevel::Converged => "converged",
        GatewayEvidenceLevel::Unconverged => "unconverged but current",
        GatewayEvidenceLevel::Stale => "stale",
    }
}

/// A run of conditions looked up by type, rendered as
/// `Accepted=True Programmed=False (AddressNotAssigned)`.
fn conditions_text(type_names: &[&str], lookup: impl Fn(&str) -> ObservedCondition) -> String {
    type_names
        .iter()
        .map(|type_name| condition_text(&lookup(type_name)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One condition: its type, its state, and its controller-supplied
/// reason when it published one.
///
/// The reason is shown but never *compared* -- `admissionlab_gateway::diff`
/// is explicit that a reason string is never itself a change. It is here
/// because it is the one piece of evidence that tells a reader why a
/// condition is `False`.
fn condition_text(condition: &ObservedCondition) -> String {
    let state = match condition.state {
        ConditionState::True => "True",
        ConditionState::False => "False",
        ConditionState::Unknown => "Unknown",
        ConditionState::Missing => "Missing",
    };
    match &condition.reason {
        Some(reason) => format!("{}={state} ({reason})", condition.type_name),
        None => format!("{}={state}", condition.type_name),
    }
}

/// A route's claimed parent, as `namespace/name#listener`, with each
/// absent half spelled out rather than silently defaulted.
fn parent_text(parent: &ParentIdentity) -> String {
    let namespace = parent
        .namespace
        .as_deref()
        // Gateway API's own default, stated rather than substituted:
        // the report shows what the status said.
        .unwrap_or("(the route's own namespace)");
    match &parent.section_name {
        Some(listener) => format!("{namespace}/{}#{listener}", parent.name),
        None => format!("{namespace}/{}", parent.name),
    }
}

/// Whole milliseconds, the same resolution
/// `admissionlab_core::StageTimings` publishes.
fn format_millis(elapsed: std::time::Duration) -> String {
    format!("{}ms", elapsed.as_millis())
}

/// Fixtures whose evidence does not support a comparison, with each
/// side's own reason.
///
/// Printed as its own section rather than folded into the summary count
/// because an inconclusive fixture is a gap in what the run established,
/// and a reader deciding whether to trust a `pass` needs to know which
/// fixtures did not contribute to it.
fn render_inconclusive(out: &mut String, result: &LabResult, palette: &Palette) {
    let inconclusive: Vec<&FixtureComparison> = result
        .fixtures
        .iter()
        .filter(|fixture| fixture.bucket() == FixtureBucket::Inconclusive)
        .collect();

    let _ = writeln!(
        out,
        "{bold}Inconclusive{reset}  {count}",
        bold = palette.bold,
        reset = palette.reset,
        count = inconclusive.len(),
    );
    if inconclusive.is_empty() {
        let _ = writeln!(
            out,
            "  {dim}none{reset}\n",
            dim = palette.dim,
            reset = palette.reset
        );
        return;
    }

    for fixture in inconclusive {
        let _ = writeln!(
            out,
            "  {bold}{fixture}{reset}",
            bold = palette.bold,
            reset = palette.reset,
            fixture = fixture.fixture_id.as_str(),
        );
        match (&fixture.admission, &fixture.gateway) {
            (Some(admission), _) => render_incomparable_sides(out, admission, palette),
            // A Gateway route contract whose two sides are not
            // comparable states *why* in its own vocabulary, which is
            // the comparability answer the Gateway section prints beside
            // it rather than a second, differently worded one.
            (None, Some(gateway)) => {
                let _ = writeln!(
                    out,
                    "    {dim}{reason}{reset}",
                    dim = palette.dim,
                    reset = palette.reset,
                    reason = comparability_label(gateway.comparability()),
                );
            }
            (None, None) => {
                let _ = writeln!(
                    out,
                    "    {dim}no evidence was captured on either side{reset}",
                    dim = palette.dim,
                    reset = palette.reset,
                );
            }
        }
    }
    out.push('\n');
}

/// Each side's own explanation for why it produced no comparable
/// decision, verbatim.
fn render_incomparable_sides(out: &mut String, admission: &AdmissionComparison, palette: &Palette) {
    for (side, outcome) in [
        ("baseline", &admission.baseline),
        ("candidate", &admission.candidate),
    ] {
        if let AdmissionDecision::UnsupportedDryRun { message } = &outcome.decision {
            let _ = writeln!(
                out,
                "    {dim}{side}:{reset} {message}",
                dim = palette.dim,
                reset = palette.reset,
            );
        }
    }
}

/// Expectations that matched nothing.
///
/// Shown because a stale entry keeps suppressing nothing, and the only
/// way its author finds out is if the tool says so.
fn render_stale_expectations(out: &mut String, result: &LabResult, palette: &Palette) {
    let stale = &result.policy.stale_expectations;
    if stale.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "{bold}Stale expectations{reset}  {count}",
        bold = palette.bold,
        reset = palette.reset,
        count = stale.len(),
    );
    for entry in stale {
        let _ = writeln!(
            out,
            "  {id}: {reason}",
            id = entry.id,
            reason = entry.reason
        );
    }
    out.push('\n');
}

/// Run-level diagnostics.
fn render_diagnostics(out: &mut String, result: &LabResult, palette: &Palette) {
    if result.diagnostics.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "{bold}Diagnostics{reset}  {count}",
        bold = palette.bold,
        reset = palette.reset,
        count = result.diagnostics.len(),
    );
    for diagnostic in &result.diagnostics {
        let _ = writeln!(
            out,
            "  {code}: {message}",
            code = diagnostic.code,
            message = diagnostic.message
        );
    }
    out.push('\n');
}

/// How long each stage took, on one line, and only when the run
/// measured them.
///
/// One line rather than a table, and omitted entirely rather than
/// rendered empty, for the same reason the rest of this module is shaped
/// the way it is: a terminal report exists to tell a reader what changed,
/// and a performance breakdown is context for that answer rather than
/// part of it. A reader who wants the full structure -- per side, per
/// component, per fixture count -- reads `result.json`'s `timings` block,
/// which is where the machine-readable form lives.
///
/// The line itself is [`admissionlab_core::StageTimings::summary_line`],
/// not a second format written here: `admissionlab` prints the same
/// rendering after cleanup, with the two stages a `result.json` cannot
/// contain (see that type's own documentation), and two renderings of one
/// value would be two things to keep in step.
///
/// Absent stages are absent from the line. Global Constraint 15 again: a
/// stage nobody timed must not read as a stage that took no time.
fn render_timings(out: &mut String, result: &LabResult, palette: &Palette) {
    let Some(timings) = &result.timings else {
        return;
    };
    let _ = writeln!(
        out,
        "{bold}Stage timings{reset}",
        bold = palette.bold,
        reset = palette.reset,
    );
    let _ = writeln!(out, "  {}\n", timings.summary_line());
}

/// The run's overall verdict, last, where a reader looks for it.
fn render_verdict(out: &mut String, result: &LabResult, palette: &Palette) {
    let (label, color) = match result.policy.disposition {
        PolicyDisposition::Pass => ("pass", palette.green),
        PolicyDisposition::Warn => ("warn", palette.yellow),
        PolicyDisposition::Fail => ("fail", palette.red),
    };
    let _ = writeln!(
        out,
        "{color}{bold}Result: {label}{reset}",
        bold = palette.bold,
        reset = palette.reset,
    );
}

/// The ANSI escapes to use, or empty strings when color is off.
///
/// Every escape in this module comes from here, and the no-color palette
/// is all `""`, so "render without color" is the same code path with
/// zero-length markers rather than a second set of format strings that
/// could drift from the colored ones. That is what makes the "identical
/// text modulo escapes" test a real check rather than a coincidence.
struct Palette {
    bold: &'static str,
    dim: &'static str,
    red: &'static str,
    yellow: &'static str,
    green: &'static str,
    reset: &'static str,
}

impl Palette {
    /// The colored palette when `color`, the empty one otherwise.
    fn new(color: bool) -> Self {
        if color {
            Self {
                bold: "\u{1b}[1m",
                dim: "\u{1b}[2m",
                red: "\u{1b}[31m",
                yellow: "\u{1b}[33m",
                green: "\u{1b}[32m",
                reset: "\u{1b}[0m",
            }
        } else {
            Self {
                bold: "",
                dim: "",
                red: "",
                yellow: "",
                green: "",
                reset: "",
            }
        }
    }
}
