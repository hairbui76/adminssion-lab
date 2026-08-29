#![forbid(unsafe_code)]

/// Returns this crate's package identity.
///
/// Used by the workspace smoke test to prove that `admissionlab-core`
/// builds, links, and is callable from another crate in the workspace.
#[must_use]
pub const fn crate_identity() -> &'static str {
    "admissionlab-core"
}
