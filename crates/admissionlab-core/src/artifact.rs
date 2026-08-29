//! Run outcome classification and on-disk artifact layout.
//!
//! This module defines how a run's overall result is classified
//! ([`RunDisposition`]), where a run's artifacts live on disk
//! ([`RunPaths`]), and the store that actually creates that layout and
//! writes into it ([`ArtifactStore`]). [`RunDisposition`] and [`RunPaths`]
//! describe the *result* side of a run, as opposed to its identity
//! ([`crate::ids`]) or which cluster a value came from ([`crate::side`]).
//!
//! [`RunPaths`] only computes paths; it never touches the filesystem.
//! [`ArtifactStore`] is where all of this module's filesystem IO lives:
//! it creates the directories [`RunPaths`] names and writes files into
//! them.
//!
//! # Atomicity
//!
//! [`ArtifactStore::write_json_atomic`] and
//! [`ArtifactStore::write_bytes_atomic`] both guarantee that a reader
//! observing the destination path sees either its previous contents in
//! full or the new contents in full — never a partial or truncated file,
//! regardless of when this process is interrupted. This is achieved by
//! writing to a temporary file created as a sibling of the destination
//! (in the same directory, so the final `rename` — atomic only within a
//! single filesystem — never crosses a filesystem boundary), fully
//! writing and `fsync`ing its contents, then renaming it onto the
//! destination. The temporary file is `fsync`ed (not merely flushed to
//! the OS's own buffers) before the rename so that a crash immediately
//! after a successful call cannot lose the write: whichever of the old
//! or new file a subsequent reader observes, its content is always
//! completely and durably written. This module does not additionally
//! `fsync` the containing directory after the rename; that would harden
//! against the rename operation itself failing to survive a crash at
//! that exact instant, which does not change whether a reader can ever
//! observe a *partial* file (they still see either the old or the new
//! file, never a corrupt hybrid) and is not a guarantee this local,
//! single-writer artifact store needs to make.
//!
//! [`ArtifactStore::write_json_atomic`] serializes its value into an
//! in-memory buffer *before* any filesystem interaction begins. A
//! serialization failure — including one partway through encoding, for
//! example a field that fails after earlier fields already succeeded —
//! is therefore indistinguishable from never having called the method at
//! all as far as the filesystem is concerned: no temporary file is ever
//! created. For every other failure that can occur after a temporary
//! file has been created (a write error, a permission-change error, or a
//! failed rename), [`ArtifactStore::write_bytes_atomic`] removes that
//! temporary file on the error path before returning, so a failed call
//! never leaves stray temp files behind either way.
//!
//! # Permissions
//!
//! On Unix, [`RunPaths::raw`] and [`RunPaths::kubeconfigs`] — the two
//! directories that hold or will hold genuinely sensitive content, per
//! `PRODUCT.md` §29 — are restricted to mode `0700` (owner
//! read/write/execute only) by [`ArtifactStore::create_run`]. A file
//! written under [`RunPaths::kubeconfigs`] is additionally restricted to
//! mode `0600` by [`ArtifactStore::write_bytes_atomic`], applied to the
//! temporary file *before* it is renamed into place so there is no
//! window in which the kubeconfig is reachable at its final path with
//! looser, default permissions. Every mode is set explicitly after
//! creation rather than requested from the creation call itself, because
//! the process umask would otherwise mask down whatever the creation
//! call requested. On a non-Unix platform this restriction cannot be
//! enforced (there is no equivalent permission model to set it with);
//! rather than silently doing nothing, [`ArtifactStore::create_run`] and
//! [`ArtifactStore::write_bytes_atomic`] emit a `tracing` warning each
//! time this happens, so the gap is visible at runtime instead of hidden.
//!
//! # Path safety
//!
//! [`crate::RunId`] already rejects `..` and path separators, so a path
//! built from [`RunPaths`] (as returned by [`ArtifactStore::create_run`])
//! can never resolve outside this store's root. But
//! [`ArtifactStore::write_json_atomic`] and
//! [`ArtifactStore::write_bytes_atomic`] accept an arbitrary caller-given
//! `&Path`, so both are contracted to reject one that does not resolve
//! inside the store's root, returning [`ArtifactError::PathEscapesRoot`]
//! rather than writing anywhere. The check canonicalizes both the
//! store's root and the destination's parent directory before comparing
//! one against the other — canonicalizing is what resolves `..`
//! components and symlinks, which a raw, non-canonicalizing
//! `Path::starts_with` cannot do (a path like `<root>/foo/../../etc`
//! textually starts with `<root>` component-by-component even though it
//! actually names a location nowhere near it). The destination's
//! *parent* is canonicalized rather than the destination itself, because
//! canonicalization requires the path to already exist on disk and the
//! destination file usually does not yet — writing it is the point —
//! while its parent directory does, having already been created by
//! [`ArtifactStore::create_run`].

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

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
/// writing into them is the job of [`ArtifactStore`].
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

/// Failure modes of [`ArtifactStore`]'s directory creation and atomic
/// write operations.
#[derive(Debug, Error)]
pub enum ArtifactError {
    /// A path given to [`ArtifactStore::write_json_atomic`] or
    /// [`ArtifactStore::write_bytes_atomic`] does not resolve to a
    /// location inside this store's root.
    ///
    /// See the module documentation's "Path safety" section for why
    /// this can only happen for a caller-supplied path, never one
    /// built from [`RunPaths`].
    #[error("path {} escapes the artifact store root {}", .path.display(), .root.display())]
    PathEscapesRoot {
        /// The path that was rejected.
        path: PathBuf,
        /// This store's root, which `path` must resolve inside of.
        root: PathBuf,
    },

    /// Serializing a value passed to [`ArtifactStore::write_json_atomic`]
    /// as JSON failed.
    ///
    /// Because [`ArtifactStore::write_json_atomic`] serializes into an
    /// in-memory buffer before touching the filesystem at all, this
    /// variant is only ever produced before any I/O has happened: the
    /// destination file is untouched (or, if it already existed,
    /// unchanged) and no temporary file was ever created.
    #[error("failed to serialize artifact as JSON: {0}")]
    Serialize(#[source] serde_json::Error),

    /// An I/O operation failed while creating a directory, writing a
    /// file, or otherwise interacting with the filesystem.
    #[error("failed to {operation} `{}`: {source}", .path.display())]
    Io {
        /// A short description of what was being attempted, for example
        /// `"create directory"` or `"rename temporary file into place
        /// at"`.
        operation: &'static str,
        /// The path the operation was acting on.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: io::Error,
    },
}

impl ArtifactError {
    /// Builds an [`ArtifactError::Io`] describing an I/O failure while
    /// performing `operation` on `path`.
    fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Creates and writes into a run's on-disk artifact workspace.
///
/// Holds only the shared root directory under which every run gets its
/// own namespaced subtree (see [`RunPaths`]). See the module
/// documentation for the atomicity, permission, and path-safety
/// guarantees [`ArtifactStore::create_run`], [`ArtifactStore::write_json_atomic`],
/// and [`ArtifactStore::write_bytes_atomic`] provide.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    /// Creates a store rooted at `root`.
    ///
    /// This performs no filesystem IO itself: `root` need not exist yet.
    /// [`ArtifactStore::create_run`] creates it, along with everything
    /// under it, on demand.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Creates every directory this run's [`RunPaths`] names and returns
    /// them.
    ///
    /// Idempotent: an already-existing directory (for example, from a
    /// previous call with the same id) is left as-is rather than
    /// rejected, matching [`std::fs::create_dir_all`]'s own behavior.
    ///
    /// See the module documentation's "Permissions" section for the
    /// owner-only restriction this applies to [`RunPaths::raw`] and
    /// [`RunPaths::kubeconfigs`] on Unix.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError::Io`] if creating any directory, or
    /// restricting one of the two sensitive directories to owner-only
    /// permissions, fails.
    pub async fn create_run(&self, id: &RunId) -> Result<RunPaths, ArtifactError> {
        let paths = RunPaths::new(&self.root, id);

        for dir in [
            paths.root(),
            paths.raw(),
            paths.normalized(),
            paths.reports(),
            paths.logs(),
            paths.kubeconfigs(),
        ] {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|source| ArtifactError::io("create directory", dir, source))?;
        }

        // Restricted *after* creation, not via the creation call's mode
        // argument: see the module documentation's "Permissions"
        // section for why (the umask would mask it down regardless).
        for dir in [paths.raw(), paths.kubeconfigs()] {
            set_owner_only_mode(dir, 0o700).await?;
        }

        Ok(paths)
    }

    /// Serializes `value` as pretty-printed JSON and writes it to `path`
    /// with the same atomicity guarantee as
    /// [`ArtifactStore::write_bytes_atomic`], which this delegates to.
    ///
    /// See the module documentation's "Atomicity" section for why
    /// serializing into an in-memory buffer first means a serialization
    /// failure — even one partway through encoding `value` — never
    /// touches the filesystem at all.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError::Serialize`] if `value` fails to
    /// serialize. Otherwise, returns whatever
    /// [`ArtifactStore::write_bytes_atomic`] would return for the
    /// resulting bytes — including [`ArtifactError::PathEscapesRoot`] if
    /// `path` does not resolve inside this store's root.
    pub async fn write_json_atomic<T: Serialize>(
        &self,
        path: &Path,
        value: &T,
    ) -> Result<(), ArtifactError> {
        let bytes = serde_json::to_vec_pretty(value).map_err(ArtifactError::Serialize)?;
        self.write_bytes_atomic(path, &bytes).await
    }

    /// Writes `bytes` to `path` atomically. See the module
    /// documentation's "Atomicity" section for the exact guarantee this
    /// provides and how it is achieved, and its "Path safety" section
    /// for the containment check this performs before touching the
    /// filesystem.
    ///
    /// `path`'s immediate parent directory must already exist; this
    /// method never creates directories itself (see
    /// [`ArtifactStore::create_run`]).
    ///
    /// See the module documentation's "Permissions" section for the
    /// owner-only restriction this applies when `path` lies under
    /// [`RunPaths::kubeconfigs`], on Unix.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError::PathEscapesRoot`] if `path` does not
    /// resolve inside this store's root. Returns [`ArtifactError::Io`]
    /// if creating, writing, syncing, or renaming the temporary file
    /// fails, or if `path`'s parent directory does not exist.
    pub async fn write_bytes_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), ArtifactError> {
        let (parent, file_name) = self.validate_write_path(path).await?;

        let temp_path = parent.join(format!(
            ".{}.tmp-{}",
            file_name.to_string_lossy(),
            Uuid::new_v4()
        ));

        if let Err(err) = write_and_sync(&temp_path, bytes).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(err);
        }

        if is_kubeconfig_path(path)
            && let Err(err) = set_owner_only_mode(&temp_path, 0o600).await
        {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(err);
        }

        if let Err(source) = tokio::fs::rename(&temp_path, path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(ArtifactError::io(
                "rename temporary file into place at",
                path,
                source,
            ));
        }

        Ok(())
    }

    /// Validates that `path`'s parent directory resolves to a location
    /// inside this store's root, returning that parent and `path`'s
    /// file name on success. See the module documentation's "Path
    /// safety" section for why the parent (rather than `path` itself)
    /// is canonicalized, and why canonicalizing is necessary at all.
    async fn validate_write_path<'a>(
        &self,
        path: &'a Path,
    ) -> Result<(&'a Path, &'a OsStr), ArtifactError> {
        let parent = path.parent().ok_or_else(|| self.path_escapes_root(path))?;
        let file_name = path
            .file_name()
            .ok_or_else(|| self.path_escapes_root(path))?;

        let canonical_root = tokio::fs::canonicalize(&self.root)
            .await
            .map_err(|source| ArtifactError::io("canonicalize store root", &self.root, source))?;
        let canonical_parent = tokio::fs::canonicalize(parent).await.map_err(|source| {
            ArtifactError::io("canonicalize destination directory", parent, source)
        })?;

        if canonical_parent.starts_with(&canonical_root) {
            Ok((parent, file_name))
        } else {
            Err(self.path_escapes_root(path))
        }
    }

    /// Builds an [`ArtifactError::PathEscapesRoot`] for `path` against
    /// this store's root.
    fn path_escapes_root(&self, path: &Path) -> ArtifactError {
        ArtifactError::PathEscapesRoot {
            path: path.to_path_buf(),
            root: self.root.clone(),
        }
    }
}

/// Creates `temp_path`, writes `bytes` to it in full, and `fsync`s it
/// before returning — so that once this succeeds, `bytes` are durable on
/// disk (not merely buffered in the OS page cache) and safe to rename
/// into place.
async fn write_and_sync(temp_path: &Path, bytes: &[u8]) -> Result<(), ArtifactError> {
    use tokio::io::AsyncWriteExt as _;

    let mut file = tokio::fs::File::create(temp_path)
        .await
        .map_err(|source| ArtifactError::io("create temporary file", temp_path, source))?;
    file.write_all(bytes)
        .await
        .map_err(|source| ArtifactError::io("write temporary file", temp_path, source))?;
    file.sync_all()
        .await
        .map_err(|source| ArtifactError::io("sync temporary file", temp_path, source))?;
    Ok(())
}

/// Returns whether `path` lies under a directory named `kubeconfigs`
/// anywhere in its ancestry — the marker [`ArtifactStore::write_bytes_atomic`]
/// uses to decide whether a file it is about to write contains
/// kubeconfig material and must therefore be created at mode `0600`
/// rather than the platform default.
///
/// This is a location-based inference, not a content inspection:
/// `write_bytes_atomic` has no separate "this is a kubeconfig" parameter
/// (see the interface this type owns), so the destination path —
/// specifically, whether it falls under [`RunPaths::kubeconfigs`] — is
/// the only signal available. Every caller in this codebase that writes
/// a kubeconfig does so through exactly that directory, so in practice
/// this inference is exact, not merely a heuristic.
fn is_kubeconfig_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "kubeconfigs")
}

/// Restricts `path` (a file or directory that must already exist) to
/// `mode`, owner-only, on Unix.
///
/// Set *after* creation rather than requested via the creation call's
/// own mode argument: the process umask masks down whatever a creation
/// call requests, so setting it explicitly afterward is the only way to
/// guarantee the resulting mode is exactly `mode`.
#[cfg(unix)]
async fn set_owner_only_mode(path: &Path, mode: u32) -> Result<(), ArtifactError> {
    use std::os::unix::fs::PermissionsExt as _;

    let permissions = std::fs::Permissions::from_mode(mode);
    tokio::fs::set_permissions(path, permissions)
        .await
        .map_err(|source| ArtifactError::io("set owner-only permissions on", path, source))
}

/// Non-Unix fallback for [`set_owner_only_mode`]: there is no portable
/// equivalent permission model to restrict `path` with, so this cannot
/// enforce anything. Rather than doing that silently, it emits a
/// `tracing` warning so the gap is visible at runtime instead of hidden
/// — see the module documentation's "Permissions" section.
#[cfg(not(unix))]
// `async` here has nothing to await, but the signature must match the
// `#[cfg(unix)]` version above exactly: every call site (in `create_run`
// and `write_bytes_atomic`) calls this function unconditionally followed
// by `.await`, without itself branching on platform.
#[allow(clippy::unused_async)]
async fn set_owner_only_mode(path: &Path, _mode: u32) -> Result<(), ArtifactError> {
    tracing::warn!(
        path = %path.display(),
        "owner-only permissions are not supported on this platform; this \
         artifact path may be more permissive than intended"
    );
    Ok(())
}
