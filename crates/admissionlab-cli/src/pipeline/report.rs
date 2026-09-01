//! Assembling the report-ready [`LabResult`] and writing this run's
//! artifacts.
//!
//! Everything a human or a CI job reads comes from here, and all of it
//! from one value: [`build_result`] composes the comparison, the policy
//! verdict, and the observed environments into a [`LabResult`], and
//! [`write_reports`] redacts it **once** and renders the three views
//! (`admissionlab_report`'s own "redact once, render many" contract —
//! putting redaction inside each renderer would mean three
//! implementations that can drift and a fourth renderer that forgets).
//!
//! # Redaction configuration is a seam, and it is empty today
//!
//! [`write_reports`] uses [`RedactionRules::default`], which is the
//! whole of Global Constraint 14's non-configurable half: Secret data,
//! authorization headers, private keys, and credential-like environment
//! names are redacted with or without any configuration. The rules type
//! also carries two *user-supplied* additions — extra JSON pointers
//! ("configured sensitive paths") and extra credential-name substrings —
//! and Admission Lab has nowhere to configure them yet:
//! `admissionlab_spec::LabSpec` has no `redaction` section, so
//! `ResolvedLab` carries nothing to build them from.
//!
//! That gap is stated here rather than papered over. The seam is
//! narrow and obvious when a later task adds the YAML: resolve the
//! section into a `RedactionRules` and pass it to this function instead
//! of the default. Nothing else in the pipeline needs to change, because
//! this is already the single chokepoint every renderer reads through.
//!
//! # The failure artifact
//!
//! A run that fails at or after capture has no honest [`LabResult`] to
//! write — a `LabResult` requires a policy verdict, and manufacturing a
//! `pass` for a run that never compared anything is exactly the
//! fabrication Global Constraint 15 forbids. It does have real
//! diagnostics, though, and throwing them away because the run failed is
//! how a user ends up rerunning a twenty-minute lab to learn something
//! it already knew. [`FailureArtifact`] is that middle ground: a small,
//! machine-readable `diagnostics.json` naming the stage that failed, the
//! failure itself, and every diagnostic collected up to that point,
//! written *before* cleanup runs (Task 4.14 step 3).
//!
//! # The job summary is written on both paths
//!
//! `--github-summary` (Task 5.4) names a file the GitHub Action appends
//! to `$GITHUB_STEP_SUMMARY`. A run that reached a verdict writes
//! `admissionlab_report::render_github_summary` of the same redacted
//! result the other two artifacts render; a run that did not writes
//! [`render_no_verdict_summary`], which says so and names the stage that
//! failed. There is no third possibility, and neither file ever states a
//! verdict the run did not reach — the failure summary has no verdict
//! word in it at all.

use std::io;
use std::path::{Path, PathBuf};

use admissionlab_core::{Diagnostic, InstalledLab, PreparedLab, RunId, SideInstall, StageTimings};
use admissionlab_policy::PolicyResult;
use admissionlab_report::{
    ComponentReport, EnvironmentReport, EnvironmentSummary, FixtureComparison, LabResult,
    RedactionRules, ReportError, RunSummary, SCHEMA_VERSION, TerminalOptions, escape_markdown,
    redact_result, render_terminal, write_github_summary, write_html_report, write_json_report,
};
use serde::Serialize;

use crate::pipeline::compare::Comparison;

/// File name of the machine-readable result inside the report
/// directory.
pub const RESULT_JSON: &str = "result.json";
/// File name of the standalone HTML report inside the report directory.
pub const REPORT_HTML: &str = "report.html";
/// File name of the failure-path diagnostics artifact inside the report
/// directory. See this module's documentation for why a failed run
/// writes this instead of a [`LabResult`].
pub const DIAGNOSTICS_JSON: &str = "diagnostics.json";

/// What [`write_reports`] produced.
#[derive(Debug, Clone)]
pub struct WrittenReports {
    /// The rendered terminal report. Returned rather than printed so the
    /// caller owns where it goes (and so a test can read it without
    /// capturing process-wide stdout).
    pub terminal: String,
    /// Where `result.json` was written.
    pub result_json: PathBuf,
    /// Where `report.html` was written.
    pub report_html: PathBuf,
    /// Where the GitHub job summary was written, when one was asked for.
    pub github_summary: Option<PathBuf>,
}

/// Composes one run's report-ready result.
///
/// `comparison` supplies the per-fixture evidence and the run-level
/// diagnostics it observed; `policy` supplies every graded change, in
/// `admissionlab-policy`'s own deterministic order. Each fixture's
/// `changes` are the subset of that graded list attributed to it —
/// filtered from the same list rather than graded a second time, so a
/// fixture's drill-down can never disagree with the run-wide view about
/// a change's severity or its expected flag.
///
/// [`RunSummary::from_fixtures`] does the counting: it is defined
/// entirely in terms of `FixtureComparison::bucket`, so the summary and
/// the per-fixture buckets a renderer shows can never disagree either.
///
/// `timings` is this run's stage durations as of *now* (Task 5.7), or
/// `None` for a caller that measured nothing. "As of now" is the whole
/// contract: this function is called at the end of the comparison stage,
/// so the value it embeds cannot include the reporting stage that renders
/// it or the cleanup that follows — see
/// [`LabResult::timings`] for what that absence means and where those two
/// stages are reported instead.
#[must_use]
pub fn build_result(
    run_id: &RunId,
    environments: EnvironmentSummary,
    comparison: &Comparison,
    policy: PolicyResult,
    diagnostics: Vec<Diagnostic>,
    timings: Option<StageTimings>,
) -> LabResult {
    let fixtures: Vec<FixtureComparison> = comparison
        .fixtures
        .iter()
        .map(|compared| FixtureComparison {
            fixture_id: compared.fixture_id.clone(),
            admission: compared.admission.clone(),
            gateway: compared.gateway.clone(),
            changes: policy
                .changes
                .iter()
                .filter(|classified| {
                    classified.change.fixture_id.as_str() == compared.fixture_id.as_str()
                })
                .cloned()
                .collect(),
        })
        .collect();

    LabResult {
        schema_version: SCHEMA_VERSION.to_owned(),
        run_id: run_id.clone(),
        summary: RunSummary::from_fixtures(&fixtures),
        environments,
        fixtures,
        policy,
        diagnostics,
        timings,
    }
}

/// What each side actually was.
///
/// `kubernetes` is the version this run *provisioned* — the value
/// `resolve_node_image` pinned against the checked-in compatibility
/// matrix before the cluster was created — not a value read back from
/// the running API server, because no stage of the Alpha pipeline asks
/// the server for its own version. That is a real (if narrow) gap
/// between this field's name and its content, so it is stated rather
/// than glossed: a later task that queries `/version` can tighten it
/// without changing this signature.
///
/// `components` come from the *install* records, not from the
/// configuration: `InstalledComponent::resolved_version` is the version
/// actually installed, confirmed against the cluster where the installer
/// could confirm it (Global Constraint 15 governs what it holds when it
/// could not), which is what a provenance-carrying report needs.
#[must_use]
pub fn environment_summary(prepared: &PreparedLab, installed: &InstalledLab) -> EnvironmentSummary {
    EnvironmentSummary {
        baseline: environment_report(
            &prepared.baseline.spec.kubernetes_version,
            &installed.baseline,
        ),
        candidate: environment_report(
            &prepared.candidate.spec.kubernetes_version,
            &installed.candidate,
        ),
    }
}

/// One side's environment report.
fn environment_report(kubernetes: &str, install: &SideInstall) -> EnvironmentReport {
    EnvironmentReport {
        kubernetes: kubernetes.to_owned(),
        components: install
            .components
            .iter()
            .map(|component| ComponentReport {
                name: component.name.clone(),
                version: component.resolved_version.clone(),
            })
            .collect(),
    }
}

/// Redacts `result` once and renders every view of it.
///
/// `directory` must already exist, and so must `github_summary`'s parent
/// directory when one is given. Returns the rendered terminal report for
/// the caller to print, and where each file landed.
///
/// `github_summary` is `--github-summary`'s path: the same redacted
/// value, rendered as the capped Markdown a GitHub Actions job summary
/// wants. It is written here rather than by the caller for the reason
/// this module exists — redaction happens once, and every rendering hangs
/// off that single redacted value, so a renderer added later cannot be
/// the one that forgets.
///
/// # Errors
///
/// Returns [`ReportError`] if any artifact could not be serialized or
/// written. The JSON report is written first: it is the machine-readable
/// one a CI job consumes, so if only one can be produced it should be
/// that one. The summary is written last because it is the one view whose
/// content is wholly derived from the others.
pub fn write_reports(
    directory: &Path,
    github_summary: Option<&Path>,
    result: &LabResult,
    terminal: TerminalOptions,
) -> Result<WrittenReports, ReportError> {
    // Global Constraint 14's one chokepoint. Every renderer below reads
    // the redacted value; none of them sees `result` itself.
    let published = redact_result(result, &RedactionRules::default());

    let result_json = directory.join(RESULT_JSON);
    write_json_report(&result_json, &published)?;
    let report_html = directory.join(REPORT_HTML);
    write_html_report(&report_html, &published)?;
    let github_summary = match github_summary {
        Some(path) => {
            write_github_summary(path, &published)?;
            Some(path.to_path_buf())
        }
        None => None,
    };

    Ok(WrittenReports {
        terminal: render_terminal(&published, &terminal),
        result_json,
        report_html,
        github_summary,
    })
}

/// What a run that failed before it could compare anything leaves
/// behind. See this module's documentation for why this exists rather
/// than a half-built [`LabResult`].
#[derive(Debug, Clone, Serialize)]
pub struct FailureArtifact {
    /// The run this describes.
    #[serde(rename = "runId")]
    pub run_id: String,
    /// The pipeline stage that failed, as a short stable label (for
    /// example `"capture"`).
    pub stage: &'static str,
    /// The failure itself, rendered.
    pub failure: String,
    /// Every diagnostic collected before the run gave up — install
    /// diagnostics, and the diagnostics on whatever fixtures were
    /// captured before the failing one.
    pub diagnostics: Vec<Diagnostic>,
}

/// How many characters of the failure text the no-verdict summary
/// carries.
///
/// The same reasoning as `admissionlab_report::MAX_CELL_CHARS`, at a
/// different scale: this is one prose block rather than a table cell, and
/// the whole point of it is to save a reader the trip to the job log, so
/// 160 characters would cut most install failures off mid-sentence. It is
/// still a cap, because the text is vendor-derived (a `helm` failure
/// carries whatever the chart's hooks printed) and a job summary that a
/// single unbounded error can blow past `SUMMARY_BYTE_BUDGET` is not
/// bounded at all.
const MAX_FAILURE_CHARS: usize = 2_000;

/// The job summary for a run that produced no verdict.
///
/// This is deliberately *not* a result: it states that the run failed,
/// names the stage it failed at, and quotes the failure. It renders no
/// verdict word, no bucket counts, and no findings, because a run that
/// never compared both sides has none of those — inventing any of them is
/// the fabrication Global Constraint 15 forbids, and this file is read in
/// a pull request by someone deciding whether the change is safe.
///
/// `run_id` is absent for a failure that happened before the run
/// workspace existed (a bad configuration, a host missing a
/// prerequisite), which is exactly when there is no run to name.
///
/// Every interpolated value goes through
/// `admissionlab_report::escape_markdown`, for the reason that renderer
/// documents at length: a step summary renders raw HTML, and the failure
/// text is vendor-derived.
#[must_use]
pub fn render_no_verdict_summary(
    run_id: Option<&str>,
    stage: &str,
    failure: &str,
    diagnostics: usize,
) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    // Infallible for `String`; discarded exactly as `admissionlab-report`'s
    // own renderers discard it.
    let _ = writeln!(out, "## Admission Lab: NO RESULT\n");
    let _ = writeln!(
        out,
        "This run failed at the `{stage}` stage before it could compare baseline and candidate, \
         so it has **no pass/fail verdict**. `admissionlab test` exited non-zero and the step log \
         above has its full output.\n",
        stage = escape_markdown(stage),
    );

    let _ = writeln!(out, "| | |");
    let _ = writeln!(out, "| --- | --- |");
    let _ = writeln!(
        out,
        "| Run | {run} |",
        run = run_id.map_or_else(
            || "not started — the failure came before a run workspace existed".to_owned(),
            escape_markdown,
        ),
    );
    let _ = writeln!(out, "| Failed stage | {} |", escape_markdown(stage));
    let _ = writeln!(out, "| Diagnostics collected | {diagnostics} |\n");

    let _ = writeln!(out, "### What failed\n");
    let _ = writeln!(out, "> {}\n", escape_markdown(&truncate(failure)));

    let _ = writeln!(out, "### Evidence\n");
    let _ = writeln!(
        out,
        "- `{DIAGNOSTICS_JSON}` — the failed stage, this failure, and every diagnostic collected \
         before it. Uploaded with this run when the failure happened after the run workspace \
         existed."
    );
    let _ = writeln!(
        out,
        "- No `{RESULT_JSON}` and no `{REPORT_HTML}`: both carry a policy verdict, and this run \
         never earned one."
    );
    out
}

/// `text` capped at [`MAX_FAILURE_CHARS`] characters, with a trailing `…`
/// when it was cut.
///
/// Iterates [`char`]s rather than slicing bytes, so a multi-byte
/// character is never split — the same rule, for the same reason, as
/// `admissionlab_report::github`'s own truncation.
fn truncate(text: &str) -> String {
    let mut out: String = text.chars().take(MAX_FAILURE_CHARS).collect();
    if text.chars().nth(MAX_FAILURE_CHARS).is_some() {
        out.push('…');
    }
    out
}

/// Writes [`render_no_verdict_summary`] to `path`, creating its parent
/// directory if it does not exist.
///
/// Unlike [`write_reports`], this one does create directories: it runs on
/// paths where the run may have failed *because* the report directory
/// could not be prepared, and a summary that cannot be written is the one
/// case where the reader is left with nothing at all.
///
/// # Errors
///
/// Returns the underlying [`io::Error`]. Callers report it rather than
/// propagating it: this function only ever runs on a path that is already
/// failing, and a summary that could not be written must never replace
/// the failure it was describing.
pub async fn write_no_verdict_summary(
    path: &Path,
    run_id: Option<&str>,
    stage: &str,
    failure: &str,
    diagnostics: usize,
) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    let text = render_no_verdict_summary(run_id, stage, failure, diagnostics);
    tokio::fs::write(path, text.as_bytes()).await
}

/// Writes `artifact` as `diagnostics.json` in `directory`, which must
/// already exist.
///
/// Deliberately *not* routed through `admissionlab_core::ArtifactStore`:
/// `--report-dir` may point anywhere the user chose, and that store's
/// containment check would (correctly) reject a path outside the run
/// root. The same reasoning `admissionlab_report::json` gives for
/// mirroring rather than reusing that store applies here.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] if the file could not be
/// serialized or written.
pub async fn write_failure_artifact(
    directory: &Path,
    artifact: &FailureArtifact,
) -> io::Result<PathBuf> {
    let path = directory.join(DIAGNOSTICS_JSON);
    let mut text = serde_json::to_string_pretty(artifact).map_err(io::Error::other)?;
    text.push('\n');
    tokio::fs::write(&path, text.as_bytes()).await?;
    Ok(path)
}
