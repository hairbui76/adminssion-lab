#![forbid(unsafe_code)]

pub mod artifact;
pub mod diagnostic;
pub mod error;
pub mod ids;
pub mod side;

pub use artifact::{RunDisposition, RunPaths};
pub use diagnostic::{Diagnostic, DiagnosticLevel, RedactedValue};
pub use error::IdParseError;
pub use ids::{FixtureId, RunId};
pub use side::Side;

/// Returns this crate's package identity.
///
/// Used by the workspace smoke test to prove that `admissionlab-core`
/// builds, links, and is callable from another crate in the workspace.
#[must_use]
pub const fn crate_identity() -> &'static str {
    "admissionlab-core"
}
