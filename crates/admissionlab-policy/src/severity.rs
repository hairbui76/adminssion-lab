//! How bad a behavior change is, and the Alpha default answer for every
//! [`SemanticChangeKind`].
//!
//! [`default_severity`] is the complete, frozen Alpha mapping (Task 4.8
//! step 1, seventeen rows). It is a pure total function of the change's
//! kind alone -- no clock, no network, no model, no per-run state
//! (Global Constraint 7) -- so the same kind always grades the same way
//! before a lab's own policy gets a say. [`crate::evaluate`] is what
//! layers `PolicySpec::fail_on` and `PolicySpec::overrides` on top of
//! this baseline; nothing in a *recipe* may participate (Global
//! Constraint 6: recipes carry install/readiness/normalization metadata,
//! never classification logic), which is exactly why this table lives in
//! Rust here rather than in any recipe's YAML.
//!
//! # Two deliberately conservative rows
//!
//! `newly_allowed` and `security_context_changed` are both
//! [`Severity::Critical`] by default even though either can be a
//! perfectly intended improvement -- a policy loosened on purpose, a
//! `securityContext` field newly defaulted by an admission controller.
//! Grading those accurately would need a partial order over security
//! postures ("is this context weaker or stronger than that one?"), and
//! an *incomplete* such classifier is worse than none: it would quietly
//! grade a genuine weakening as `Info` on whichever axis it failed to
//! model. Alpha therefore does not attempt one. The escape hatch is
//! explicit and per-lab, not global: a `policy.overrides` entry naming
//! the kind (and, ideally, the fixtures and subject it is expected on)
//! downgrades it, and Task 4.9's expectations file marks specific
//! instances as intended without downgrading the severity at all.
//!
//! # Adding an eighteenth kind
//!
//! [`default_severity`] and [`kind_index`] share one exhaustive `match`
//! ([`classify`]), so a new [`SemanticChangeKind`] variant is a compile
//! error here rather than a silently ungraded change. [`ALL_KINDS`]'s
//! length is pinned in the same expression: giving the new variant the
//! next index makes the array literal's declared length wrong, which is
//! also a compile error. `tests/evaluate.rs` then asserts the two agree
//! (every kind's index is its actual position in `ALL_KINDS`), so the
//! set cannot be complete-but-misordered either.

use admissionlab_diff::SemanticChangeKind;
use serde::Serialize;

/// How bad one behavior change is.
///
/// Ordered `Info < Warning < Critical` -- the derived [`Ord`] follows
/// declaration order, and [`crate::evaluate`] relies on it to pick the
/// worst unexpected change in a run. Keep the variants in this order.
///
/// Derives no `Default`. There is no neutral severity: every value of
/// this type exists because [`default_severity`] or an explicit
/// `policy.overrides` entry said something specific, and a `Default`
/// impl would let a future refactor silently grade an unclassified
/// change as whichever variant happened to be first.
///
/// [`Serialize`] only, never [`serde::Deserialize`]. A severity reaches
/// a report outward (as the pinned lowercase names below) but is never
/// read back in that way: the one place a user *writes* a severity is
/// `PolicyOverrideSpec::severity`, which is a `String` in the
/// configuration model and is parsed here by [`Severity::from_name`] --
/// which can name the file location and list the valid spellings in its
/// error, where a serde failure could only say "unknown variant".
///
/// Each wire tag is pinned with an explicit `#[serde(rename)]` rather
/// than derived from the Rust identifier, matching
/// [`SemanticChangeKind`]'s own discipline: these strings appear in JSON
/// reports and in hand-written `policy.overrides` entries, so renaming a
/// Rust variant must never silently change them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Severity {
    /// A real difference that is not, on its own, evidence of a problem.
    #[serde(rename = "info")]
    Info,
    /// A difference a human should look at before shipping.
    #[serde(rename = "warning")]
    Warning,
    /// A difference that fails the run unless it was explicitly
    /// expected.
    #[serde(rename = "critical")]
    Critical,
}

impl Severity {
    /// Every severity, weakest first -- the order [`Ord`] sorts them in.
    ///
    /// Used to render the valid spellings in [`Severity::from_name`]'s
    /// error message, so that message can never drift from what the
    /// parser actually accepts.
    pub const ALL: [Self; 3] = [Self::Info, Self::Warning, Self::Critical];

    /// Returns this severity's stable wire name -- exactly the string
    /// `serde` serializes it as (asserted in `tests/evaluate.rs`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    /// Parses a severity written in configuration, ignoring surrounding
    /// whitespace.
    ///
    /// Case-sensitive on purpose: `"Warning"` is rejected rather than
    /// accepted as an alias. One spelling per value keeps configuration
    /// files, JSON reports, and this crate's own error messages using
    /// the same string, and a rejection here is loud and immediate (see
    /// [`crate::validate_policy_spec`]) rather than a silent
    /// misclassification later.
    ///
    /// Returns [`None`] for any other input; the caller supplies the
    /// document locator the message needs.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|severity| severity.as_str() == name.trim())
    }
}

/// Every [`SemanticChangeKind`], in the order Task 4.8's default-severity
/// table declares them.
///
/// This is the list a name in `policy.failOn` or `policy.overrides[].kind`
/// is validated against ([`kind_from_name`]) and the list rendered in the
/// resulting error message. It is deliberately *not* used to iterate
/// changes or to order a report: report ordering is
/// [`crate::evaluate`]'s documented sort key, which is keyed on wire
/// names precisely so that reordering this array can never reorder a
/// report.
///
/// Its completeness is compiler-enforced -- see this module's
/// documentation.
pub const ALL_KINDS: [SemanticChangeKind; 17] = [
    SemanticChangeKind::ObjectNewlyDenied,
    SemanticChangeKind::ObjectNewlyAllowed,
    SemanticChangeKind::ContainerAdded,
    SemanticChangeKind::ContainerRemoved,
    SemanticChangeKind::InitContainerAdded,
    SemanticChangeKind::InitContainerRemoved,
    SemanticChangeKind::VolumeAdded,
    SemanticChangeKind::VolumeRemoved,
    SemanticChangeKind::VolumeMountChanged,
    SemanticChangeKind::EnvironmentChanged,
    SemanticChangeKind::ImageChanged,
    SemanticChangeKind::ServiceAccountChanged,
    SemanticChangeKind::SecurityContextChanged,
    SemanticChangeKind::ResourceRequirementChanged,
    SemanticChangeKind::WebhookFailed,
    SemanticChangeKind::WebhookInvocationChanged,
    SemanticChangeKind::WebhookLatencyChanged,
];

/// The single exhaustive `match` behind both [`default_severity`] and
/// [`kind_index`]: each kind's position in [`ALL_KINDS`] paired with its
/// Alpha default severity.
///
/// One `match` rather than two so the two facts cannot drift apart, and
/// so a new [`SemanticChangeKind`] variant produces exactly one compile
/// error to fix rather than one per table. See this module's
/// documentation for how that also pins [`ALL_KINDS`]'s length.
const fn classify(kind: SemanticChangeKind) -> (usize, Severity) {
    match kind {
        // A request the baseline admitted and the candidate rejects
        // breaks deploys outright.
        SemanticChangeKind::ObjectNewlyDenied => (0, Severity::Critical),
        // Conservative by design: see this module's documentation.
        SemanticChangeKind::ObjectNewlyAllowed => (1, Severity::Critical),
        // Added workload surface is usually a sidecar being injected on
        // purpose; worth a human's eyes, not a failed run.
        SemanticChangeKind::ContainerAdded => (2, Severity::Warning),
        // Removed workload surface silently drops functionality that
        // was running in the baseline.
        SemanticChangeKind::ContainerRemoved => (3, Severity::Critical),
        SemanticChangeKind::InitContainerAdded => (4, Severity::Warning),
        SemanticChangeKind::InitContainerRemoved => (5, Severity::Critical),
        SemanticChangeKind::VolumeAdded => (6, Severity::Warning),
        SemanticChangeKind::VolumeRemoved => (7, Severity::Critical),
        SemanticChangeKind::VolumeMountChanged => (8, Severity::Warning),
        SemanticChangeKind::EnvironmentChanged => (9, Severity::Warning),
        // Image references legitimately move on nearly every run of a
        // real pipeline; grading this above `Info` by default would
        // make the tool cry wolf.
        SemanticChangeKind::ImageChanged => (10, Severity::Info),
        // Identity changes change what the workload is authorized to
        // do.
        SemanticChangeKind::ServiceAccountChanged => (11, Severity::Critical),
        // Conservative by design: see this module's documentation.
        SemanticChangeKind::SecurityContextChanged => (12, Severity::Critical),
        SemanticChangeKind::ResourceRequirementChanged => (13, Severity::Warning),
        // A webhook failing on one side and not the other is a broken
        // admission chain, whatever the object ended up looking like.
        SemanticChangeKind::WebhookFailed => (14, Severity::Critical),
        SemanticChangeKind::WebhookInvocationChanged => (15, Severity::Warning),
        // Latency is an optional, best-effort signal that must never
        // fail a run by itself (Global Constraint 19).
        SemanticChangeKind::WebhookLatencyChanged => (16, Severity::Warning),
    }
}

/// Returns the Alpha default severity for `kind`, before any lab's own
/// `policy.failOn` or `policy.overrides` are applied.
#[must_use]
pub fn default_severity(kind: SemanticChangeKind) -> Severity {
    classify(kind).1
}

/// Returns `kind`'s position in [`ALL_KINDS`].
///
/// Exists to make "[`ALL_KINDS`] lists every variant, in this order" a
/// checkable claim rather than a review-time one; `tests/evaluate.rs`
/// asserts it against the array itself.
#[must_use]
pub fn kind_index(kind: SemanticChangeKind) -> usize {
    classify(kind).0
}

/// Resolves a semantic-kind name written in configuration
/// (`policy.failOn`, `policy.overrides[].kind`,
/// `expectations[].kind`) to the kind it names, ignoring surrounding
/// whitespace.
///
/// Matches against [`SemanticChangeKind::as_str`], the same pinned wire
/// strings a JSON report prints, so a name a user copies out of a report
/// is always a name this accepts. Returns [`None`] for anything else --
/// including a plausible-looking near miss such as `"image_change"` --
/// so that an unknown name fails loudly at load time instead of silently
/// matching nothing forever (Task 4.8 step 3).
#[must_use]
pub fn kind_from_name(name: &str) -> Option<SemanticChangeKind> {
    let name = name.trim();
    ALL_KINDS
        .into_iter()
        .find(|kind| SemanticChangeKind::as_str(*kind) == name)
}

/// Renders every valid semantic-kind name, comma-separated in
/// [`ALL_KINDS`] order, for an error message listing what was expected.
pub(crate) fn kind_name_list() -> String {
    ALL_KINDS
        .iter()
        .map(|kind| SemanticChangeKind::as_str(*kind))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Renders every valid severity name, comma-separated, for an error
/// message listing what was expected.
pub(crate) fn severity_name_list() -> String {
    Severity::ALL
        .iter()
        .map(|severity| Severity::as_str(*severity))
        .collect::<Vec<_>>()
        .join(", ")
}
