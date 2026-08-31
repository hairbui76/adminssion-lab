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
//!   for the precedence rule and why *inconclusive* outranks everything.
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
//! - [`error`] holds [`ReportError`], the one failure vocabulary shared
//!   by every renderer that touches the filesystem.
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
//! [`model::SCHEMA_VERSION`] is **experimental**. Alpha makes no
//! compatibility promise about the serialized result; the schema is
//! frozen at Beta (Global Constraint 9).
//!
//! [`LabResult`]: model::LabResult
//! [`RunSummary`]: model::RunSummary
//! [`EnvironmentSummary`]: model::EnvironmentSummary
//! [`FixtureComparison`]: model::FixtureComparison
//! [`AdmissionComparison`]: model::AdmissionComparison
//! [`GatewayCaseComparison`]: model::GatewayCaseComparison
//! [`FixtureComparison::bucket`]: model::FixtureComparison::bucket

pub mod error;
pub mod json;
pub mod model;
pub mod redact;
pub mod terminal;

pub use error::ReportError;
pub use json::{render_json, write_json_report};
pub use model::{
    AdmissionComparison, ComponentReport, EnvironmentReport, EnvironmentSummary, FixtureBucket,
    FixtureComparison, GatewayCaseComparison, LabResult, RunSummary, SCHEMA_VERSION,
};
pub use redact::{
    DEFAULT_ENV_NAME_PATTERNS, REDACTED, REDACTED_PRIVATE_KEY, RedactionRules,
    SENSITIVE_HEADER_NAMES, redact_result,
};
pub use terminal::{TerminalOptions, render_terminal};
