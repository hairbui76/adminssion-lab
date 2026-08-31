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
use admissionlab_policy::{ClassifiedChange, PolicyDisposition, Severity};

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

/// The run identity line, and the experimental-schema note.
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
        "{dim}schema {schema} (experimental; stable at Beta){reset}\n",
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
        match &fixture.admission {
            Some(admission) => render_incomparable_sides(out, admission, palette),
            None => {
                let _ = writeln!(
                    out,
                    "    {dim}no admission evidence was captured{reset}",
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
