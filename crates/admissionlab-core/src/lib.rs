#![forbid(unsafe_code)]

pub mod artifact;
pub mod cluster;
pub mod diagnostic;
pub mod error;
pub mod ids;
pub mod process;
pub mod side;
pub mod tool;

pub use artifact::{ArtifactError, ArtifactStore, RunDisposition, RunPaths};
pub use cluster::{
    ClusterDiagnostics, ClusterError, ClusterHandle, ClusterManager, ClusterSpec, RollbackOutcome,
};
pub use diagnostic::{Diagnostic, DiagnosticLevel, RedactedValue};
pub use error::IdParseError;
pub use ids::{FixtureId, RunId};
pub use process::{
    CommandContext, CommandResult, CommandSpec, ProcessError, ProcessRunner, TokioProcessRunner,
};
pub use side::Side;
pub use tool::{
    DISK_WARNING_THRESHOLD_BYTES, DoctorReport, ToolName, ToolStatus, collect_doctor_report,
    disk_space_warning, probe_tool,
};

/// Returns this crate's package identity.
///
/// Used by the workspace smoke test to prove that `admissionlab-core`
/// builds, links, and is callable from another crate in the workspace.
#[must_use]
pub const fn crate_identity() -> &'static str {
    "admissionlab-core"
}
