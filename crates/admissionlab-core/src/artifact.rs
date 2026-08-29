//! Run outcome classification and on-disk artifact layout.
//!
//! This module defines how a run's overall result is classified
//! ([`RunDisposition`]) and where a run's artifacts live on disk
//! ([`RunPaths`]). Both describe the *result* side of a run, as opposed to
//! its identity ([`crate::ids`]) or which cluster a value came from
//! ([`crate::side`]).
//!
//! [`RunPaths`] only computes paths; it never touches the filesystem. The
//! `ArtifactStore` that actually creates these directories and performs
//! atomic writes is added by a later task.

use std::path::{Path, PathBuf};

use crate::ids::RunId;

/// How a run ended.
///
/// Exactly these seven variants, in this declaration order: a later task
/// maps them one-to-one to CLI exit codes 0 through 6, so neither the set
/// nor the order may change without updating that mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunDisposition {
    /// The run completed and the regression policy passed.
    Passed,
    /// The run completed but the regression policy failed.
    PolicyFailed,
    /// The user-provided configuration or fixture definition was invalid.
    InvalidInput,
    /// Lab infrastructure (for example, cluster creation) failed.
    InfrastructureFailed,
    /// Component installation or readiness failed.
    InstallationFailed,
    /// Fixture execution or capture failed.
    FixtureFailed,
    /// An internal Admission Lab error occurred.
    InternalError,
}

/// Filesystem locations for one run's artifacts, rooted under a shared
/// artifact store root and namespaced by [`RunId`].
///
/// Constructing a [`RunPaths`] performs **no filesystem IO**: it only
/// computes paths. It never creates directories, checks whether paths
/// exist, or otherwise touches the disk. Creating these directories and
/// writing into them is the job of the `ArtifactStore` added by a later
/// task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPaths {
    root: PathBuf,
    raw: PathBuf,
    normalized: PathBuf,
    reports: PathBuf,
    logs: PathBuf,
    kubeconfigs: PathBuf,
    run_json: PathBuf,
}

impl RunPaths {
    /// Computes the canonical artifact layout for `run_id` under `root`.
    ///
    /// This performs no filesystem IO: it does not create directories,
    /// check for existing files, or otherwise touch the disk.
    #[must_use]
    pub fn new(root: &Path, run_id: &RunId) -> Self {
        let run_root = root.join(run_id.as_str());
        Self {
            raw: run_root.join("raw"),
            normalized: run_root.join("normalized"),
            reports: run_root.join("reports"),
            logs: run_root.join("logs"),
            kubeconfigs: run_root.join("kubeconfigs"),
            run_json: run_root.join("run.json"),
            root: run_root,
        }
    }

    /// The run's own root directory: `<root>/<run_id>`.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory for raw, unnormalized captured admission objects.
    #[must_use]
    pub fn raw(&self) -> &Path {
        &self.raw
    }

    /// Directory for normalized (nondeterminism-stripped) objects.
    #[must_use]
    pub fn normalized(&self) -> &Path {
        &self.normalized
    }

    /// Directory for rendered terminal/JSON/HTML reports.
    #[must_use]
    pub fn reports(&self) -> &Path {
        &self.reports
    }

    /// Directory for process and audit logs captured during the run.
    #[must_use]
    pub fn logs(&self) -> &Path {
        &self.logs
    }

    /// Directory for isolated baseline/candidate kubeconfigs.
    #[must_use]
    pub fn kubeconfigs(&self) -> &Path {
        &self.kubeconfigs
    }

    /// Path to the run's metadata/manifest file.
    #[must_use]
    pub fn run_json(&self) -> &Path {
        &self.run_json
    }
}
