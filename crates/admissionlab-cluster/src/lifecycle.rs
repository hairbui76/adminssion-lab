//! [`KindClusterManager`]: the `kind`-backed
//! [`admissionlab_core::ClusterManager`] implementation. Everything that
//! actually brings a `kind` cluster up or down — writing the files it
//! needs, invoking `kind` through
//! [`admissionlab_core::ProcessRunner`], and rolling back a partial
//! create — lives here. `kind.rs` supplies the pure facts (argv,
//! timeouts, naming rules) this module drives; `kubeconfig.rs` supplies
//! kubeconfig-specific path/verification logic.
//!
//! # Per-run, per-side file layout
//!
//! [`RunPaths`] is shared by both clusters of one run (there is exactly
//! one per run, not one per side), so every path [`ClusterLayout`]
//! derives from it is namespaced by [`admissionlab_core::Side`] as well
//! as by run. This matters most for the audit log: `kube-apiserver`
//! inside *each* node writes to the same fixed node-internal path
//! (`/var/log/kubernetes/kube-apiserver-audit.log`), so if a baseline and
//! a candidate cluster's node containers were bind-mounted to the *same*
//! host directory, their audit logs would collide — the second cluster's
//! writes landing in the same file as the first's. Every path below is
//! therefore namespaced under `paths.logs()` by `spec.side.as_str()`
//! before it is used, which also means a baseline and a candidate
//! cluster created concurrently for the same run (as a later task does)
//! never contend on the same file.
//!
//! # Absolute paths
//!
//! `kind`'s Docker bind mounts (used for the audit policy file and audit
//! log directory) require absolute host paths; a relative one would fail
//! late, inside `kind` itself, rather than here.
//! `admissionlab_core::RunPaths` computes every path it exposes by
//! joining onto whatever root it was constructed with (see
//! `RunPaths::new` and `RunPaths::root`), so every path
//! [`ClusterLayout`] derives from `paths` is absolute exactly when
//! `paths.root()` is. [`KindClusterManager::create`] validates that
//! once, up front (`paths.root().is_absolute()`), rather than trusting
//! whatever produced `paths` to have guaranteed it — this is a
//! `RunPaths` value a caller could in principle have built directly
//! (`RunPaths::new` performs no I/O and no validation of its own) rather
//! than via `admissionlab_core::ArtifactStore::create_run`.
//!
//! # Rollback
//!
//! From the moment `kind create cluster` is actually invoked onward, any
//! failure other than the command never having started at all (a
//! [`ProcessError::Spawn`] — in practice, the `kind` binary itself is
//! missing) is treated as "a node might now exist," and triggers a
//! best-effort `kind delete cluster --name <name>` before the original
//! error is returned (Task 1.7 brief Step 4; PRODUCT.md §33 "no leaked
//! cluster after normal failure paths"). This is deliberately broader
//! than just the two failure modes the brief names by name (kubeconfig
//! export, health check): a timeout kills `kind` out from under itself
//! before its own documented default cleanup-on-failure behavior ever
//! gets to run, and a non-zero exit is not something this project has
//! empirically verified always leaves nothing behind either. Attempting
//! cleanup unconditionally past that point costs nothing when `kind` did
//! already clean up on its own (`kind delete cluster` on an
//! already-gone cluster is cheap) and is the only way to be sure
//! otherwise.
//!
//! Whatever the rollback delete's own outcome, the *original* failure is
//! always preserved: see
//! [`admissionlab_core::ClusterError::CreateFailedWithRollback`].

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use admissionlab_core::{
    ArtifactStore, ClusterDiagnostics, ClusterError, ClusterHandle, ClusterManager, ClusterSpec,
    CommandSpec, ProcessError, ProcessRunner, RollbackOutcome, RunPaths,
};
use async_trait::async_trait;

use crate::audit::render_audit_policy;
use crate::config::{KindClusterConfigInput, render_kind_config};
use crate::kind;
use crate::kubeconfig;

/// Every path one cluster (one `(run, side)` pair) needs, derived from a
/// run's shared [`RunPaths`]. See the module documentation's "Per-run,
/// per-side file layout" section for why each is namespaced by side.
struct ClusterLayout {
    /// Host path for this side's copy of the rendered audit policy
    /// document. Each side gets its own copy (rather than sharing one)
    /// so two concurrent `create` calls never write the same
    /// destination file at once, even though the content would be
    /// identical either way.
    audit_policy_path: PathBuf,
    /// Host directory this side's node's `/var/log/kubernetes` is
    /// bind-mounted from.
    audit_log_dir: PathBuf,
    /// The exact file `kube-apiserver` writes its audit log to, once
    /// mounted through: `audit_log_dir` plus the fixed basename kubeadm
    /// configures inside the node.
    audit_log_file: PathBuf,
    /// Host path for this side's generated `kind` cluster configuration
    /// file (what `--config` points at).
    kind_config_path: PathBuf,
    /// Host path for this side's isolated kubeconfig (what
    /// `--kubeconfig` points at).
    kubeconfig_path: PathBuf,
}

impl ClusterLayout {
    fn new(paths: &RunPaths, spec: &ClusterSpec) -> Self {
        let side = spec.side.as_str();
        let audit_log_dir = paths.logs().join(side).join("audit");
        let audit_log_file = audit_log_dir.join(kind::AUDIT_LOG_FILE_NAME);
        Self {
            audit_policy_path: paths.logs().join(format!("{side}-audit-policy.yaml")),
            audit_log_dir,
            audit_log_file,
            kind_config_path: paths.logs().join(format!("{side}-kind-config.yaml")),
            kubeconfig_path: kubeconfig::kubeconfig_path(paths, spec.side),
        }
    }
}

/// The `kind`-backed [`ClusterManager`] implementation.
///
/// Holds only a shared [`ProcessRunner`]: every other piece of state a
/// call needs (file paths, the artifact store to write through) is
/// derived fresh from that call's own `&RunPaths`/`&ClusterHandle`
/// argument, so one `KindClusterManager` is safe to reuse — behind an
/// `Arc`, as `admissionlab-core`'s future `LabRunner` does — across many
/// clusters and runs.
pub struct KindClusterManager {
    runner: Arc<dyn ProcessRunner>,
}

impl KindClusterManager {
    /// Creates a manager that drives `kind` through `runner`.
    #[must_use]
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner }
    }

    /// Runs `kind create cluster` for `name`, using `config_path` and
    /// `kubeconfig_path` exactly as `kind.rs`'s `create_argv` builds
    /// them. Returns the redaction-safe command context alongside a
    /// `CommandFailed` error so a non-zero exit still reports what ran.
    async fn run_create(
        &self,
        name: &str,
        config_path: &std::path::Path,
        kubeconfig_path: &std::path::Path,
    ) -> Result<(), ClusterError> {
        let spec = CommandSpec {
            program: kind::KIND_PROGRAM.into(),
            args: kind::create_argv(name, config_path, kubeconfig_path),
            cwd: None,
            env: BTreeMap::new(),
            sensitive_env_keys: BTreeSet::new(),
            timeout: kind::CREATE_TIMEOUT,
        };
        self.run_and_check(spec).await
    }

    /// Runs `kind delete cluster` for `name`, using `kind.rs`'s
    /// `delete_argv`. Shared by the public [`ClusterManager::delete`]
    /// and by [`KindClusterManager::rollback`], so both use exactly the
    /// same argv and timeout.
    async fn run_delete(&self, name: &str) -> Result<(), ClusterError> {
        let spec = CommandSpec {
            program: kind::KIND_PROGRAM.into(),
            args: kind::delete_argv(name),
            cwd: None,
            env: BTreeMap::new(),
            sensitive_env_keys: BTreeSet::new(),
            timeout: kind::DELETE_TIMEOUT,
        };
        self.run_and_check(spec).await
    }

    /// Runs `spec` and maps its outcome to `Result<(), ClusterError>`:
    /// a [`ProcessError`] converts via `#[from]`, and a non-zero exit
    /// becomes [`ClusterError::CommandFailed`].
    async fn run_and_check(&self, spec: CommandSpec) -> Result<(), ClusterError> {
        let context = Box::new(spec.context());
        let result = self.runner.run(spec).await?;
        if result.status.success() {
            Ok(())
        } else {
            Err(ClusterError::CommandFailed {
                context,
                status: result.status,
                stdout: result.stdout,
                stderr: result.stderr,
            })
        }
    }

    /// Attempts a best-effort `kind delete cluster --name <name>` after
    /// `source` (a failure that occurred at or after invoking
    /// `kind create cluster`), and wraps whatever happened together with
    /// `source` so the original failure is never lost. See the module
    /// documentation's "Rollback" section.
    async fn rollback(&self, name: &str, source: ClusterError) -> ClusterError {
        let rollback = match self.run_delete(name).await {
            Ok(()) => RollbackOutcome::Deleted,
            Err(delete_error) => RollbackOutcome::Failed(Box::new(delete_error)),
        };
        ClusterError::CreateFailedWithRollback {
            source: Box::new(source),
            rollback,
        }
    }

    /// Runs `kind get clusters` and returns the cluster names it lists,
    /// one per non-empty line — or a human-readable reason the probe
    /// could not be completed. Used only by `diagnostics`, which never
    /// fails: every error case here becomes a note instead.
    async fn list_clusters(&self) -> Result<Vec<String>, String> {
        let spec = CommandSpec {
            program: kind::KIND_PROGRAM.into(),
            args: kind::get_clusters_argv(),
            cwd: None,
            env: BTreeMap::new(),
            sensitive_env_keys: BTreeSet::new(),
            timeout: kind::DIAGNOSTICS_TIMEOUT,
        };
        let context = spec.context();
        let result = self
            .runner
            .run(spec)
            .await
            .map_err(|error| format!("could not list kind clusters: {error}"))?;
        if !result.status.success() {
            return Err(format!(
                "`{context}` exited with {}: {}",
                result.status,
                String::from_utf8_lossy(&result.stderr).trim(),
            ));
        }
        Ok(String::from_utf8_lossy(&result.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }
}

#[async_trait]
impl ClusterManager for KindClusterManager {
    async fn create(
        &self,
        spec: &ClusterSpec,
        paths: &RunPaths,
    ) -> Result<ClusterHandle, ClusterError> {
        kind::validate_cluster_name(&spec.name)?;

        if !paths.root().is_absolute() {
            return Err(ClusterError::NonAbsolutePath {
                field: "RunPaths root",
                path: paths.root().to_path_buf(),
            });
        }

        let layout = ClusterLayout::new(paths, spec);
        let store = ArtifactStore::new(paths.root());

        store
            .write_bytes_atomic(&layout.audit_policy_path, render_audit_policy().as_bytes())
            .await
            .map_err(|source| ClusterError::ArtifactWrite {
                context: "audit policy file",
                source,
            })?;

        tokio::fs::create_dir_all(&layout.audit_log_dir)
            .await
            .map_err(|source| ClusterError::Io {
                operation: "create audit log host directory",
                path: layout.audit_log_dir.clone(),
                source,
            })?;

        let rendered_config = render_kind_config(&KindClusterConfigInput {
            name: spec.name.clone(),
            node_image: spec.node_image.clone(),
            audit_policy_host_path: layout.audit_policy_path.clone(),
            audit_log_host_dir: layout.audit_log_dir.clone(),
        })
        .map_err(|source| ClusterError::KindConfigRender(source.to_string()))?;

        store
            .write_bytes_atomic(&layout.kind_config_path, rendered_config.as_bytes())
            .await
            .map_err(|source| ClusterError::ArtifactWrite {
                context: "kind cluster configuration",
                source,
            })?;

        // From here on, `kind create cluster` may have created a node
        // even if something afterward fails -- see the module
        // documentation's "Rollback" section for exactly which failures
        // that covers and why.
        if let Err(error) = self
            .run_create(
                &spec.name,
                &layout.kind_config_path,
                &layout.kubeconfig_path,
            )
            .await
        {
            return Err(match error {
                ClusterError::Process(ProcessError::Spawn { .. }) => error,
                other => self.rollback(&spec.name, other).await,
            });
        }

        if let Err(error) = kubeconfig::secure_kubeconfig(&store, &layout.kubeconfig_path).await {
            return Err(self.rollback(&spec.name, error).await);
        }

        Ok(ClusterHandle {
            spec: spec.clone(),
            kubeconfig: layout.kubeconfig_path,
            audit_log: layout.audit_log_file,
        })
    }

    async fn delete(&self, handle: &ClusterHandle) -> Result<(), ClusterError> {
        self.run_delete(&handle.spec.name).await
    }

    async fn diagnostics(&self, handle: &ClusterHandle) -> ClusterDiagnostics {
        let mut notes = Vec::new();

        let cluster_exists = match self.list_clusters().await {
            Ok(names) => Some(names.iter().any(|existing| existing == &handle.spec.name)),
            Err(reason) => {
                notes.push(reason);
                None
            }
        };

        let (kubeconfig_present, kubeconfig_note) = describe_file(&handle.kubeconfig).await;
        notes.extend(kubeconfig_note);

        let (audit_log_present, audit_log_note) = describe_file(&handle.audit_log).await;
        notes.extend(audit_log_note);

        ClusterDiagnostics {
            cluster_name: handle.spec.name.clone(),
            cluster_exists,
            kubeconfig_present,
            audit_log_present,
            notes,
        }
    }
}

/// Checks whether `path` exists and is a non-empty file, without ever
/// failing the caller: an inability to determine that (anything other
/// than "does not exist") is reported as a note rather than propagated,
/// since [`ClusterManager::diagnostics`] must never fail (see this
/// crate's `lifecycle.rs` module documentation and Global Constraint 15).
async fn describe_file(path: &std::path::Path) -> (bool, Option<String>) {
    match tokio::fs::metadata(path).await {
        Ok(metadata) if metadata.len() > 0 => (true, None),
        Ok(_empty) => (
            false,
            Some(format!("{} exists but is empty", path.display())),
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (false, None),
        Err(error) => (
            false,
            Some(format!("could not check {}: {error}", path.display())),
        ),
    }
}
