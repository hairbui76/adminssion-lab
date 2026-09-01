//! The GitHub Actions job summary a reviewer reads without leaving the
//! pull request.
//!
//! [`render_github_summary`] turns a [`LabResult`] into GitHub-flavored
//! Markdown. Like every renderer here it is a pure function returning a
//! [`String`]: it opens no file, reads no environment variable, and does
//! not know that `$GITHUB_STEP_SUMMARY` exists. The CLI writes the text
//! to a file and the Action wrapper (Task 5.4) pipes that file into the
//! step summary; deciding *where* the text goes is their job, and keeping
//! it out of here is what makes the output testable byte for byte.
//!
//! # A pointer, not a report
//!
//! This is the one renderer in the crate that deliberately does **not**
//! show everything. The terminal report renders every unexpected finding
//! with no truncation, and the JSON and HTML artifacts carry the whole
//! run including every webhook invocation and every patch. A job summary
//! is a different thing: it is read inside a web page that GitHub
//! truncates at 1 MiB, next to the logs of every other step, by someone
//! deciding whether this run needs their attention at all.
//!
//! So the summary is capped by construction -- at most
//! [`MAX_LISTED_FINDINGS`] critical and [`MAX_LISTED_FINDINGS`] warning
//! rows, each field truncated at [`MAX_CELL_CHARS`] characters -- and
//! every cap it hits is stated in the output ("and 490 more ...") rather
//! than applied silently. Nothing is *hidden*: the five bucket counts are
//! always complete, both finding counts are always complete, and the
//! final section names the artifacts that hold the full evidence. A
//! reader is never left thinking they have seen everything when they have
//! not.
//!
//! What never appears here at all: invocation lists, JSON Patches, and
//! payload bodies. Those are the parts that make a report unbounded, and
//! they live in `report.html` and `result.json`.
//!
//! # Size discipline
//!
//! [`SUMMARY_BYTE_BUDGET`] is the size this renderer commits to staying
//! under, and it is a *derived* number rather than a hope: the output is
//! a fixed frame of literal text plus at most `2 * MAX_LISTED_FINDINGS`
//! table rows, each carrying at most seven vendor-derived fields of at
//! most `MAX_CELL_CHARS` characters, each of which escaping can expand by
//! at most five bytes per character. That product is the bound, and
//! `tests/github.rs` renders a 500-finding result against it. Row caps
//! alone would not be enough -- one webhook with a megabyte-long name
//! would blow past 1 MiB in a single row -- which is why the per-field
//! truncation exists as well.
//!
//! # Escaping, and why it is a security property here
//!
//! Every object- or vendor-derived string reaches the page through
//! [`escape_markdown`]. Two distinct things go wrong without it:
//!
//! 1. **The table breaks.** A `|` anywhere in a webhook name, a fixture
//!    identifier, or a divergence explanation splits that row into extra
//!    cells and silently misaligns every value after it -- a report that
//!    attributes one fixture's finding to another is worse than no
//!    report.
//! 2. **GitHub renders the HTML.** Step summaries support raw HTML, so a
//!    webhook named `<img src=x onerror=...>` is *markup* in this
//!    context. And a webhook name is attacker-controlled in exactly the
//!    scenario this tool exists for: a candidate stack under test. Every
//!    such string must arrive as literal text.
//!
//! [`escape_markdown`] therefore covers both layers. Per character it
//! applies the HTML rule first (`&`, `<`, `>` become entities) and the
//! Markdown rule otherwise -- one pass rather than two, precisely so the
//! second rule cannot re-escape the `#` inside an entity the first rule
//! emitted and turn `&#39;` into visible garbage.
//!
//! The Markdown set is the inline one: `` ` ``, `*`, `_`, `[`, `]`, `!`,
//! `~`, `|`, and `\` itself. Block-level markers (`-`, `#`, `>` at the
//! start of a line) are not in it, because they cannot fire: **no line of
//! this document begins with vendor data**. Every data-bearing line
//! starts with literal text this module wrote -- `| ` for a table row,
//! `Run ` for the identity line -- which is a structural invariant of the
//! writing code, not a property of any particular input. `>` is
//! entity-escaped regardless, since it is also an HTML character.
//!
//! Control characters (a newline in a rejection message, most of all)
//! become a single space: a table cell is one line by construction, and a
//! raw newline in one would end the table.
//!
//! # No code spans around data
//!
//! Vendor-derived values are never wrapped in backticks, even where a
//! monospace rendering would look nicer. Inside a code span a backslash
//! escape is *literal*, so escaped text placed there would render its own
//! backslashes and a backtick in the value would close the span early.
//! Code spans are used only around this module's own literals.
//!
//! # Redact first
//!
//! Like every renderer here, this one draws whatever it is given. Hand it
//! a redacted result -- see [`crate::redact::redact_result`].

use std::collections::BTreeMap;
// Same convention as `terminal.rs`: lines are written with `write!` /
// `writeln!` rather than `push_str(&format!(...))`, and the infallible
// `fmt::Write` impl for `String` is why every call site discards the
// `Result` with `let _ =`.
use std::fmt::Write as _;
use std::path::Path;

use admissionlab_diff::{DivergenceConfidence, DivergenceEvidence};
use admissionlab_policy::{ClassifiedChange, PolicyDisposition, Severity};

use crate::error::ReportError;
use crate::json::write_atomic;
use crate::model::{FixtureComparison, LabResult, RunSummary};

/// How many findings each of the two tables lists before it stops and
/// says how many it did not.
///
/// Ten is a screenful. A reviewer scanning a pull request either acts on
/// the first few or opens the artifacts; a summary that listed two
/// hundred rows would push every other step's output off the page and
/// still not be the place to read two hundred findings.
pub const MAX_LISTED_FINDINGS: usize = 10;

/// How many characters of any single vendor-derived field a table cell
/// carries before it is truncated with a trailing `…`.
///
/// Counted in [`char`]s, not bytes, so truncation never splits a UTF-8
/// sequence. Generous enough that every string this project produces
/// itself survives intact, and small enough that the [`SUMMARY_BYTE_BUDGET`]
/// arithmetic holds for a string this project did *not* produce -- a
/// webhook name or a rejection message from the stack under test, which
/// has no length limit at all.
pub const MAX_CELL_CHARS: usize = 160;

/// The size [`render_github_summary`]'s output is guaranteed to stay
/// under, in bytes.
///
/// GitHub truncates a step summary at 1 MiB and this budget is an eighth
/// of that, so the summary cannot be the reason a job's output is cut
/// off. See this module's "Size discipline" section for how the number is
/// derived from [`MAX_LISTED_FINDINGS`] and [`MAX_CELL_CHARS`]; it is a
/// documented ceiling for tests to assert against, not a runtime check --
/// the caps are what enforce it.
pub const SUMMARY_BYTE_BUDGET: usize = 128 * 1024;

/// Writes [`render_github_summary`]'s output to `path`, atomically.
///
/// The file is what an Admission Lab caller appends to
/// `$GITHUB_STEP_SUMMARY`; this function still does not know that
/// variable exists (see this module's documentation) — it writes a file
/// wherever it was told to, and the caller decides where that is and what
/// reads it. Written through the same temp-file-and-rename path as the
/// JSON and HTML reports, so a reader never sees a half-written summary.
///
/// `path`'s parent directory must already exist, exactly as
/// [`write_html_report`](crate::write_html_report) requires.
///
/// # Errors
///
/// Returns [`ReportError::Io`] if the file could not be written.
/// Rendering itself cannot fail.
pub fn write_github_summary(path: &Path, result: &LabResult) -> Result<(), ReportError> {
    write_atomic(path, render_github_summary(result).as_bytes())
}

/// Renders `result` as a GitHub Actions job summary.
///
/// The output is GitHub-flavored Markdown ending in a newline, is a pure
/// function of `result`, and is byte-identical across runs. See this
/// module's documentation for what it deliberately omits and where that
/// detail lives instead.
#[must_use]
pub fn render_github_summary(result: &LabResult) -> String {
    let fixtures = fixture_index(result);
    let mut out = String::new();

    render_verdict(&mut out, result);
    render_summary(&mut out, &result.summary);
    render_findings(
        &mut out,
        result,
        &fixtures,
        Severity::Critical,
        "Critical findings",
        "critical",
    );
    render_findings(
        &mut out,
        result,
        &fixtures,
        Severity::Warning,
        "Warnings",
        "warning",
    );
    render_artifacts(&mut out);

    out
}

/// Escapes `text` so it renders as literal characters in a GitHub job
/// summary, inside a Markdown table cell.
///
/// Covers both layers a step summary interprets -- HTML and Markdown --
/// and flattens control characters to spaces. See this module's
/// "Escaping" section for the rule set, the one-pass ordering, and why
/// block-level markers are deliberately absent from it.
///
/// Public because the escaping is the security-relevant part of this
/// renderer and a test that cannot call it directly can only assert on it
/// indirectly, exactly like [`crate::html::escape_html`].
#[must_use]
pub fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            // The HTML layer first: GitHub renders raw HTML in step
            // summaries, so these three are markup before they are
            // anything else.
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            // Then the Markdown inline layer. `|` is the one that
            // silently corrupts a table rather than merely restyling it.
            '\\' | '`' | '*' | '_' | '[' | ']' | '!' | '~' | '|' => {
                out.push('\\');
                out.push(character);
            }
            // A cell is one line. A newline here would terminate the
            // table and orphan every row after it.
            other if other.is_control() => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

/// One vendor-derived value, truncated then escaped, ready to be written
/// between two `|` characters.
///
/// Truncation happens *before* escaping so the limit is a limit on what
/// the reader sees rather than on how much escaping happened to expand
/// it -- and so the same input always truncates at the same place
/// regardless of which characters in it needed escaping.
fn cell(text: &str) -> String {
    escape_markdown(&truncate(text))
}

/// `text` if it is at most [`MAX_CELL_CHARS`] characters, otherwise its
/// first [`MAX_CELL_CHARS`] characters followed by `…`.
///
/// Iterating [`char`]s rather than slicing bytes is what keeps a
/// multi-byte character from being cut in half; the ellipsis is a single
/// character so a reader can tell a truncated value from one that merely
/// ends in three dots.
fn truncate(text: &str) -> String {
    let mut out: String = text.chars().take(MAX_CELL_CHARS).collect();
    if text.chars().nth(MAX_CELL_CHARS).is_some() {
        out.push('…');
    }
    out
}

/// Fixtures keyed by identifier, for the divergence fallback each
/// finding row may need.
///
/// A [`BTreeMap`] for the same reason `terminal.rs` uses one: a
/// deterministic renderer should not have a hash-ordered structure in it
/// at all, even one nothing currently iterates.
fn fixture_index(result: &LabResult) -> BTreeMap<&str, &FixtureComparison> {
    result
        .fixtures
        .iter()
        .map(|fixture| (fixture.fixture_id.as_str(), fixture))
        .collect()
}

/// The heading, the verdict, and what the process exit code will be.
///
/// The status word is `PASS`/`WARN`/`FAIL` in plain letters -- no emoji,
/// no color chip. A summary is read in web pages, in email notifications,
/// and by screen readers, and a green circle that renders as nothing (or
/// as the word "green circle") is not a verdict.
///
/// The exit code is spelled out because it is the thing the surrounding
/// workflow acts on, and because `warn` mapping to `0` is genuinely
/// surprising: a reader who sees unexpected differences listed below and
/// a passing job needs the summary to explain that, not to leave them
/// hunting for a broken gate. The mapping is
/// `admissionlab_cli::exit`'s (ROADMAP §0.4's frozen table) and is
/// restated rather than imported: this crate does not depend on the CLI,
/// and both sides are frozen.
fn render_verdict(out: &mut String, result: &LabResult) {
    let (status, meaning) = match result.policy.disposition {
        PolicyDisposition::Pass => (
            "PASS",
            "No unexpected differences. `admissionlab test` exits 0.",
        ),
        PolicyDisposition::Warn => (
            "WARN",
            "Unexpected differences, none critical. `admissionlab test` exits 0.",
        ),
        PolicyDisposition::Fail => (
            "FAIL",
            "At least one unexpected critical difference. `admissionlab test` exits 1.",
        ),
    };
    let _ = writeln!(out, "## Admission Lab: {status}\n");
    let _ = writeln!(out, "{meaning}\n");
    let _ = writeln!(
        out,
        "Run {run} — result schema {schema} (experimental; stable at Beta).\n",
        run = cell(result.run_id.as_str()),
        schema = cell(&result.schema_version),
    );
}

/// The five bucket counts, plus the total, as a compact table.
///
/// All five are listed including the zeroes, for the same reason the
/// terminal report lists them: a row that disappears at zero makes "no
/// warnings" and "warnings were not counted" look alike. The total is a
/// row rather than prose because [`RunSummary`]'s five counts are defined
/// to sum to it, and a reader who wants to check that should not have to
/// look in two places.
fn render_summary(out: &mut String, summary: &RunSummary) {
    let _ = writeln!(out, "### Fixtures\n");
    let _ = writeln!(out, "| Bucket | Fixtures |");
    let _ = writeln!(out, "| --- | ---: |");
    for (label, count) in [
        ("identical", summary.identical),
        ("expected", summary.expected),
        ("warnings", summary.warnings),
        ("critical", summary.critical),
        ("inconclusive", summary.inconclusive),
    ] {
        let _ = writeln!(out, "| {label} | {count} |");
    }
    let _ = writeln!(
        out,
        "| **total** | **{total}** |\n",
        total = summary.fixtures_total
    );
}

/// Every unexpected change at `severity`, capped at
/// [`MAX_LISTED_FINDINGS`] rows.
///
/// Reads [`LabResult::policy`]'s run-wide change list in its own order,
/// exactly as `terminal.rs` does: that is the order
/// `admissionlab-policy` documents, so the first ten rows here are the
/// first ten a reader would see anywhere else, and neither renderer
/// invents a ranking of its own.
///
/// The heading always carries the *complete* count, not the number of
/// rows drawn, so a capped table can never be mistaken for a short one.
fn render_findings(
    out: &mut String,
    result: &LabResult,
    fixtures: &BTreeMap<&str, &FixtureComparison>,
    severity: Severity,
    heading: &str,
    noun: &str,
) {
    let matching: Vec<&ClassifiedChange> = result
        .policy
        .changes
        .iter()
        .filter(|classified| classified.severity == severity && !classified.expected)
        .collect();

    let _ = writeln!(out, "### {heading} ({count})\n", count = matching.len());
    if matching.is_empty() {
        let _ = writeln!(out, "None.\n");
        return;
    }

    // "Evidence" rather than "First divergence": the column carries a
    // webhook-chain attribution for an admission finding and the two
    // sides' own condition/probe values for a Gateway one, and a header
    // naming only the first would mislabel half the rows in a lab that
    // runs both.
    let _ = writeln!(out, "| Fixture | Subject | Change | Evidence |");
    let _ = writeln!(out, "| --- | --- | --- | --- |");
    for classified in matching.iter().take(MAX_LISTED_FINDINGS) {
        render_finding_row(out, classified, fixtures);
    }
    out.push('\n');

    let omitted = matching.len().saturating_sub(MAX_LISTED_FINDINGS);
    if omitted > 0 {
        let _ = writeln!(
            out,
            "and {omitted} more {noun} findings — the complete list is in the `result.json` \
             artifact.\n",
        );
    }
}

/// One finding's row: which fixture, which object, what changed, and
/// where the two sides first parted.
fn render_finding_row(
    out: &mut String,
    classified: &ClassifiedChange,
    fixtures: &BTreeMap<&str, &FixtureComparison>,
) {
    let change = &classified.change;
    let subject = change
        .subject
        .as_deref()
        .map_or_else(|| "(none)".to_owned(), cell);
    let mut kind = cell(change.kind.as_str());
    if let Some(path) = &change.object_path {
        let _ = write!(kind, " at {path}", path = cell(path));
    }
    let _ = writeln!(
        out,
        "| {fixture} | {subject} | {kind} | {divergence} |",
        fixture = cell(change.fixture_id.as_str()),
        divergence = divergence_cell(classified, fixtures),
    );
}

/// The first-divergence one-liner, with its confidence label and the
/// webhook on each side.
///
/// The change's own attribution is preferred over the fixture-level one
/// for the same reason `terminal.rs` prefers it: it was computed for
/// *this* difference. The fallback is still labeled with its own
/// confidence, and an absent attribution says so rather than rendering an
/// empty cell that would read as "the sides did not diverge".
fn divergence_cell(
    classified: &ClassifiedChange,
    fixtures: &BTreeMap<&str, &FixtureComparison>,
) -> String {
    let change = &classified.change;
    // A Gateway finding has no webhook chain to have diverged in, so it
    // gets the evidence it does have: what each side published. Same
    // rule, same reason, as `terminal.rs`'s own Gateway arm.
    if fixtures
        .get(change.fixture_id.as_str())
        .is_some_and(|fixture| fixture.gateway.is_some())
    {
        return cell(&format!(
            "baseline {} -> candidate {}",
            gateway_side_text(change.baseline.as_ref()),
            gateway_side_text(change.candidate.as_ref()),
        ));
    }
    let divergence = change.origin.as_ref().or_else(|| {
        fixtures
            .get(change.fixture_id.as_str())
            .and_then(|fixture| fixture.admission.as_ref())
            .and_then(|admission| admission.first_divergence.as_ref())
    });
    let Some(evidence) = divergence else {
        return "not attributed".to_owned();
    };

    let mut rendered = format!(
        "{label}: {explanation}",
        label = confidence_label(evidence.confidence),
        explanation = cell(&evidence.explanation),
    );
    if let Some(webhooks) = webhook_suffix(evidence) {
        rendered.push_str(&webhooks);
    }
    rendered
}

/// One side of a Gateway change's payload, in the same words
/// `terminal.rs` renders it in: a condition state with its reason, a
/// probe status with its backend, or the plain statement that this side
/// answered nothing.
fn gateway_side_text(payload: Option<&serde_json::Value>) -> String {
    let Some(payload) = payload else {
        return "answered nothing".to_owned();
    };
    if let Some(condition) = payload.get("condition").and_then(serde_json::Value::as_str) {
        let state = payload
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        return match payload.get("reason").and_then(serde_json::Value::as_str) {
            Some(reason) => format!("{condition}={state} ({reason})"),
            None => format!("{condition}={state}"),
        };
    }
    if let Some(status) = payload.get("status").and_then(serde_json::Value::as_u64) {
        return match payload.get("backend").and_then(serde_json::Value::as_str) {
            Some(backend) => format!("HTTP {status} from {backend}"),
            None => format!("HTTP {status}"),
        };
    }
    "present".to_owned()
}

/// How a [`DivergenceConfidence`] is spelled out.
///
/// Word for word the same labels `terminal.rs` uses, and for the same
/// reason: Global Constraint 15 forbids presenting a deduction or an
/// absence of evidence as an observation, and the qualifier is exactly
/// what a one-line summary is tempted to drop for space.
fn confidence_label(confidence: DivergenceConfidence) -> &'static str {
    match confidence {
        DivergenceConfidence::Observed => "observed",
        DivergenceConfidence::Inferred => "inferred (deduced from incomplete evidence)",
        DivergenceConfidence::Unknown => "unknown (evidence does not locate the divergence)",
    }
}

/// ` (baseline X -> candidate Y)`, when either side names a webhook.
///
/// A side with no invocation at the point of divergence renders as
/// `none`, which is what [`DivergenceEvidence`] documents its `None` to
/// mean -- an added or removed invocation -- and never a stand-in for
/// "not captured". Positions are left to the terminal and HTML reports;
/// a job summary that spelled out rounds and indexes would be trading
/// the readability that is its only reason to exist.
fn webhook_suffix(evidence: &DivergenceEvidence) -> Option<String> {
    if evidence.baseline_webhook.is_none() && evidence.candidate_webhook.is_none() {
        return None;
    }
    Some(format!(
        " (baseline {baseline} -> candidate {candidate})",
        baseline = webhook_side(evidence.baseline_webhook.as_deref()),
        candidate = webhook_side(evidence.candidate_webhook.as_deref()),
    ))
}

/// One side of [`webhook_suffix`].
fn webhook_side(webhook: Option<&str>) -> String {
    webhook.map_or_else(|| "none".to_owned(), cell)
}

/// Where the evidence this summary does not carry actually lives.
///
/// Named by *artifact* name rather than by filesystem path, because a
/// path inside the runner's workspace is meaningless to the reader: the
/// job is over, the runner is gone, and what is left is the uploaded
/// artifact bundle attached to the workflow run. Naming
/// `report.html` as `/home/runner/work/.../report.html` would be telling
/// a reader to look somewhere that no longer exists.
fn render_artifacts(out: &mut String) {
    let _ = writeln!(out, "### Full evidence\n");
    let _ = writeln!(
        out,
        "This summary lists at most {MAX_LISTED_FINDINGS} findings per severity and carries no \
         webhook traces, patches, or object bodies. Everything is in the artifacts uploaded with \
         this workflow run:\n"
    );
    for (artifact, description) in [
        (
            "`result.json`",
            "the machine-readable result: every fixture, every graded change, and both sides' \
             captured admission outcomes.",
        ),
        (
            "`report.html`",
            "the standalone report page: per-fixture drill-down with the full webhook trace and \
             every patch.",
        ),
        (
            "run manifest",
            "what this run actually ran (tool versions, images, and configuration digests), for \
             reproducing it.",
        ),
    ] {
        let _ = writeln!(out, "- {artifact} — {description}");
    }
}
