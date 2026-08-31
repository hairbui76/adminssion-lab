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

use std::io;
use std::path::{Path, PathBuf};

use admissionlab_core::{Diagnostic, InstalledLab, PreparedLab, RunId, SideInstall};
use admissionlab_policy::PolicyResult;
use admissionlab_report::{
    ComponentReport, EnvironmentReport, EnvironmentSummary, FixtureComparison, LabResult,
    RedactionRules, ReportError, RunSummary, SCHEMA_VERSION, TerminalOptions, redact_result,
    render_terminal, write_html_report, write_json_report,
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
#[must_use]
pub fn build_result(
    run_id: &RunId,
    environments: EnvironmentSummary,
    comparison: &Comparison,
    policy: PolicyResult,
    diagnostics: Vec<Diagnostic>,
) -> LabResult {
    let fixtures: Vec<FixtureComparison> = comparison
        .fixtures
        .iter()
        .map(|compared| FixtureComparison {
            fixture_id: compared.fixture_id.clone(),
            admission: compared.admission.clone(),
            // Always `None` in Alpha: Gateway behavior cannot enter the
            // critical path until the Alpha gate passes (Global
            // Constraint 8).
            gateway: None,
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

/// Redacts `result` once and renders all three views of it.
///
/// `directory` must already exist. Returns the rendered terminal report
/// for the caller to print, and where the two files landed.
///
/// # Errors
///
/// Returns [`ReportError`] if either artifact could not be serialized or
/// written. The JSON report is written first: it is the machine-readable
/// one a CI job consumes, so if only one of the two can be produced it
/// should be that one.
pub fn write_reports(
    directory: &Path,
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

    Ok(WrittenReports {
        terminal: render_terminal(&published, &terminal),
        result_json,
        report_html,
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
