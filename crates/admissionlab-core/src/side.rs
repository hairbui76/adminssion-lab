//! Which side of a baseline/candidate comparison a value belongs to.

use std::fmt;

/// Which side of a baseline/candidate comparison a value belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// The unmodified stack being compared against.
    Baseline,
    /// The stack under test for regressions.
    Candidate,
}

impl Side {
    /// Returns the stable lowercase name used in artifact paths and JSON
    /// output: `"baseline"` or `"candidate"`.
    ///
    /// These exact strings are a stable serialization value: later tasks'
    /// audit correlation and report schema depend on them never changing.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Candidate => "candidate",
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
