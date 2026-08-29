//! Kubeconfig isolation: where each cluster's kubeconfig lives, and how
//! it is brought under this project's own permission guarantee after
//! `kind` writes it.
//!
//! PRODUCT.md §29.2 requires that v1 never need production cluster
//! credentials, and every ephemeral cluster this project creates gets
//! its own kubeconfig rather than sharing (or worse, mutating) the
//! user's own `~/.kube/config`. Two things make that true here:
//!
//! - [`kubeconfig_path`] gives each `(run, side)` pair its own path
//!   under [`RunPaths::kubeconfigs`], so a baseline and a candidate
//!   cluster created for the same run never share a kubeconfig file.
//! - `kind create cluster --kubeconfig <path>` (built in `kind.rs`,
//!   invoked from `lifecycle.rs`) tells `kind` to write directly to that
//!   path, which by itself already keeps `$KUBECONFIG`/
//!   `~/.kube/config` untouched: that flag is documented to override
//!   both.
//!
//! # Why this module still re-reads and re-writes the file
//!
//! `admissionlab_core::ArtifactStore::write_bytes_atomic` is what
//! automatically restricts a file under `RunPaths::kubeconfigs` to mode
//! `0600` on Unix — but only for a file it actually writes. `kind`
//! writes the kubeconfig itself (that is the entire point of
//! `--kubeconfig`), so that write does not go through
//! `ArtifactStore` at all, and this crate cannot control what
//! permissions `kind`'s own kubeconfig-writing code happens to use.
//!
//! [`secure_kubeconfig`] closes that gap deterministically instead of
//! hoping: it reads back the exact bytes `kind` wrote, and immediately
//! rewrites those same bytes to the same path through
//! `ArtifactStore::write_bytes_atomic` — which is what actually applies
//! the `0600` restriction, atomically, regardless of whatever mode the
//! file had a moment earlier. This doubles as this crate's kubeconfig
//! "health check" (Task 1.7 brief Step 4): reading the file back and
//! confirming it is non-empty, valid UTF-8, and valid YAML is exactly
//! the verification that must fail — triggering `lifecycle.rs`'s
//! rollback — when `kind` reports success but did not actually leave a
//! usable kubeconfig behind.

use std::path::{Path, PathBuf};

use admissionlab_core::{ArtifactStore, ClusterError, RunPaths, Side};

/// The path this run's `side` cluster's kubeconfig lives at:
/// `<run>/kubeconfigs/<side>.kubeconfig`.
///
/// Namespaced by `side` specifically so a baseline and a candidate
/// cluster from the same run — which share one [`RunPaths`] — never
/// collide on the same file, even when both are being created
/// concurrently.
pub(crate) fn kubeconfig_path(paths: &RunPaths, side: Side) -> PathBuf {
    paths
        .kubeconfigs()
        .join(format!("{}.kubeconfig", side.as_str()))
}

/// Reads back the kubeconfig `kind` wrote to `path`, verifies it is
/// present and structurally plausible, and rewrites it through `store`
/// so it ends up with this project's `0600` guarantee regardless of
/// whatever permissions `kind` itself used. See the module documentation
/// for why both the verification and the rewrite happen here, together.
///
/// # Errors
///
/// Returns [`ClusterError::Io`] if `path` cannot be read.
/// Returns [`ClusterError::InvalidKubeconfig`] if the file is empty, is
/// not valid UTF-8, or does not parse as YAML.
/// Returns [`ClusterError::ArtifactWrite`] if rewriting it through
/// `store` fails.
pub(crate) async fn secure_kubeconfig(
    store: &ArtifactStore,
    path: &Path,
) -> Result<(), ClusterError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|source| ClusterError::Io {
            operation: "read kubeconfig written by kind",
            path: path.to_path_buf(),
            source,
        })?;

    if bytes.is_empty() {
        return Err(ClusterError::InvalidKubeconfig {
            path: path.to_path_buf(),
            reason: "file is empty".to_owned(),
        });
    }

    let text = String::from_utf8(bytes.clone()).map_err(|_utf8_error| {
        ClusterError::InvalidKubeconfig {
            path: path.to_path_buf(),
            reason: "file is not valid UTF-8".to_owned(),
        }
    })?;

    serde_norway::from_str::<serde_norway::Value>(&text).map_err(|source| {
        ClusterError::InvalidKubeconfig {
            path: path.to_path_buf(),
            reason: format!("file is not valid YAML: {source}"),
        }
    })?;

    store
        .write_bytes_atomic(path, &bytes)
        .await
        .map_err(|source| ClusterError::ArtifactWrite {
            context: "kubeconfig (re-secured for 0600)",
            source,
        })
}
