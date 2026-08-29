//! Process exit codes for the Admission Lab CLI.
//!
//! `admissionlab-core`'s [`RunDisposition`] already assigns each outcome a
//! stable numeric identity — 0 through 6, in the exact order its variants
//! are declared (see its own doc comment) — through the fieldless enum's
//! built-in discriminant. This module's only job is turning that
//! discriminant into the process's actual [`ExitCode`], so the 0-6
//! numbering is never re-derived or duplicated here.

use std::process::ExitCode;

use admissionlab_core::RunDisposition;

/// The process exit code Admission Lab commits to for `disposition`.
///
/// Reuses `RunDisposition`'s own discriminant (`disposition as u8`)
/// rather than a second hand-written match arm, so this can never drift
/// from the canonical 0-6 ordering documented on [`RunDisposition`]
/// itself: [`RunDisposition::Passed`] is the only disposition that maps
/// to [`ExitCode::SUCCESS`]; every other variant maps to a distinct
/// non-zero code.
#[must_use]
pub fn code_for_disposition(disposition: RunDisposition) -> ExitCode {
    ExitCode::from(disposition as u8)
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;

    use admissionlab_core::RunDisposition;

    use super::code_for_disposition;

    #[test]
    fn passed_maps_to_process_success() {
        assert_eq!(
            code_for_disposition(RunDisposition::Passed),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn every_other_disposition_maps_to_a_nonzero_code() {
        for disposition in [
            RunDisposition::PolicyFailed,
            RunDisposition::InvalidInput,
            RunDisposition::InfrastructureFailed,
            RunDisposition::InstallationFailed,
            RunDisposition::FixtureFailed,
            RunDisposition::InternalError,
        ] {
            assert_ne!(
                code_for_disposition(disposition),
                ExitCode::SUCCESS,
                "{disposition:?} must not map to a process success code"
            );
        }
    }

    #[test]
    fn distinct_dispositions_map_to_distinct_codes() {
        assert_ne!(
            code_for_disposition(RunDisposition::PolicyFailed),
            code_for_disposition(RunDisposition::InternalError)
        );
    }
}
