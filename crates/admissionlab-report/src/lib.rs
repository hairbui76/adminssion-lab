#![forbid(unsafe_code)]
//! Rendering one comparison run into the artifacts a human and a CI job
//! read.
//!
//! This crate is the end of the pipeline. `admissionlab-diff` claims
//! what changed, `admissionlab-policy` grades how much each claim
//! matters, and this crate turns the two -- plus the raw captured
//! evidence -- into a [`model::LabResult`] and renders it. It decides
//! nothing: §1.1 is explicit that report rendering never decides
//! severity, and nothing here re-grades, re-orders by grade, or
//! second-guesses a comparison.
//!
//! # The pieces
//!
//! - [`model`] holds [`LabResult`] and the types §1.2's cross-task type
//!   registry assigns to this crate ([`RunSummary`],
//!   [`EnvironmentSummary`], [`FixtureComparison`],
//!   [`AdmissionComparison`], and the reserved
//!   [`GatewayCaseComparison`]). It also defines how a run's fixtures
//!   are counted into five buckets -- see [`FixtureComparison::bucket`]
//!   for the precedence rule and why *inconclusive* outranks everything
//!   -- and, from ROADMAP Task 8.8, [`MigrationCaseComparison`], which is
//!   a *sibling* of the fixture list rather than an entry in it (see
//!   [`model::LabResult::migration`] for why the two vocabularies are
//!   kept apart).
//! - [`redact`] holds [`redact_result`], the single chokepoint where
//!   Global Constraint 14's redaction happens for every renderer at
//!   once.
//! - [`terminal`] holds [`render_terminal`], the concise summary a human
//!   reads immediately after a run. Its module documentation covers what
//!   is never hidden and why color is a caller's decision rather than
//!   something the renderer probes for.
//! - [`json`] holds [`write_json_report`], the machine-readable artifact
//!   other tools consume. Its module documentation covers the schema's
//!   experimental status, the three things that make its output
//!   byte-deterministic, and why it mirrors
//!   `admissionlab_core::ArtifactStore`'s atomic write rather than
//!   calling it.
//! - [`github`] holds [`render_github_summary`], the capped Markdown a
//!   GitHub Actions job summary shows in the pull request. Its module
//!   documentation covers why it is the one renderer that deliberately
//!   shows less than everything, the byte budget the caps buy, and why
//!   escaping is a security property rather than a cosmetic one there.
//! - [`html`] holds [`write_html_report`], the standalone page a human
//!   drills into. Its module documentation covers what "self-contained"
//!   guarantees, why there is no templating dependency and no
//!   JavaScript, and how the escaping discipline is enforced
//!   structurally rather than by review.
//! - [`error`] holds [`ReportError`], the one failure vocabulary shared
//!   by every renderer that touches the filesystem.
//! - [`wire`] holds [`ResultDocument`], the **frozen**
//!   `admissionlab.io/result/v1` document a [`LabResult`]
//!   serializes as. Read its module documentation before changing
//!   anything a consumer can see: it carries the three evidence
//!   sections, the explicit availability fields Global Constraint 15
//!   requires, the casing rule, and the change-identifier scheme.
//! - [`schema`] holds [`result_v1_json_schema`], which generates
//!   `schemas/result-v1.json` from those same types.
//!
//! # Redact once, render many
//!
//! Every renderer this crate grows reads an *already redacted*
//! [`LabResult`]. [`redact_result`] is pure and returns a new value, so
//! the intended call shape is one redaction followed by however many
//! renders the run needs:
//!
//! ```text
//! let published = redact_result(&raw, &rules);
//! write_json_report(&json_path, &published)?;   // Task 4.12
//! write_html_report(&html_path, &published)?;   // Task 4.13
//! print!("{}", render_terminal(&published, &options));  // Task 4.11
//! ```
//!
//! Putting redaction inside each renderer instead would mean three
//! implementations that can drift, and a fourth renderer that forgets.
//!
//! # Schema stability
//!
//! [`model::SCHEMA_VERSION`] is `admissionlab.io/result/v1` and is
//! **frozen** (ROADMAP Task 9.1, Global Constraint 9). Within `v1.x` a
//! reader may be given additional optional fields; no existing field's
//! meaning changes silently, no semantic-change wire string is renamed,
//! and removing or renaming a field requires a new result schema version
//! and a migration note. The published schema lives at
//! `schemas/result-v1.json` and a full example at
//! `testdata/golden/result-v1.json`; both are regenerated and compared by
//! `tests/result_schema.rs`, and `tests/stable_schema.rs` measures the
//! stable schema against the frozen `schemas/result-v1beta1.json`.
//!
//! [`LabResult`]: model::LabResult
//! [`RunSummary`]: model::RunSummary
//! [`EnvironmentSummary`]: model::EnvironmentSummary
//! [`FixtureComparison`]: model::FixtureComparison
//! [`AdmissionComparison`]: model::AdmissionComparison
//! [`GatewayCaseComparison`]: model::GatewayCaseComparison
//! [`FixtureComparison::bucket`]: model::FixtureComparison::bucket
//! [`ResultDocument`]: wire::ResultDocument

pub mod error;
pub mod github;
pub mod html;
pub mod json;
pub mod model;
pub mod redact;
pub mod schema;
pub mod terminal;
pub mod wire;

pub use error::ReportError;
pub use github::{
    MAX_CELL_CHARS, MAX_LISTED_FINDINGS, SUMMARY_BYTE_BUDGET, escape_markdown,
    render_github_summary, write_github_summary,
};
pub use html::{escape_html, render_html, write_html_report};
pub use json::{render_json, write_json_report};
pub use model::{
    AdmissionComparison, ComponentReport, EnvironmentReport, EnvironmentSummary, FixtureBucket,
    FixtureComparison, GatewayCaseComparison, GradedMigrationChange, LabResult,
    MigrationCaseComparison, RunSummary, SCHEMA_VERSION,
};
pub use redact::{
    DEFAULT_ENV_NAME_PATTERNS, REDACTED, REDACTED_PRIVATE_KEY, RedactionRules,
    SENSITIVE_FIELD_NAMES, SENSITIVE_HEADER_NAMES, redact_result,
};
pub use schema::result_v1_json_schema;
pub use terminal::{TerminalOptions, render_terminal};
pub use wire::{
    AdmissionSection, ChangeDocument, DivergenceAttribution, FixtureDocument,
    MigrationChangeDocument, MigrationSection, NonPortableExpectationDocument, PolicyDocument,
    ProbeExchange, ReconciliationSection, ResultDocument, SideEvidenceLevel, SideTraceEvidence,
    TrafficEvidence, TrafficSection, semantic_change_id,
};
