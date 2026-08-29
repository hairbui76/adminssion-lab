//! Rendering a `kind` cluster configuration with kube-apiserver audit
//! logging wired in.
//!
//! [`render_kind_config`] produces the YAML text that would be handed to
//! `kind create cluster --config -`; it performs no filesystem or
//! process I/O itself. The audit *policy document*'s content is a
//! separate, independent concern ([`crate::audit::render_audit_policy`]):
//! this module only wires up *where* that policy file — once rendered
//! and written to disk by a later task — is mounted into the node and
//! referenced by kube-apiserver.
//!
//! # Two hops of mounting
//!
//! kind's own per-node `extraMounts` bind-mounts a path on the real
//! Docker host into the *node* (the container standing in for a
//! Kubernetes node). Kubeadm's
//! `ClusterConfiguration.apiServer.extraVolumes` then bind-mounts a path
//! *on that node* into the kube-apiserver static pod's own container.
//! Both hops are needed: the audit policy file (read by kube-apiserver)
//! and the audit log directory (written by kube-apiserver) must reach
//! the apiserver container, but originate on the real host so later
//! tasks can provide/read them independent of the cluster's own
//! lifetime.
//!
//! This module hard-codes the *node-internal* paths that first hop's
//! destination — kube-apiserver's `extraArgs` and kubeadm's
//! `extraVolumes` must reference exact, fixed paths agreeing with each
//! other. Only the *host* side of each mount is caller-supplied (see
//! [`KindClusterConfigInput`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

/// The node-internal file kube-apiserver is configured to read its audit
/// policy from, and the file kind's node `extraMounts` places the host's
/// [`KindClusterConfigInput::audit_policy_host_path`] at.
const AUDIT_POLICY_NODE_FILE: &str = "/etc/kubernetes/policies/admissionlab-audit-policy.yaml";

/// The node-internal directory kubeadm's `audit-policies` extraVolume
/// bind-mounts into the kube-apiserver static pod, read-only. Parent
/// directory of [`AUDIT_POLICY_NODE_FILE`].
const AUDIT_POLICY_NODE_DIR: &str = "/etc/kubernetes/policies";

/// The node-internal directory kube-apiserver writes its audit log
/// beneath, and the directory kind's node `extraMounts` places the
/// host's [`KindClusterConfigInput::audit_log_host_dir`] at.
const AUDIT_LOG_NODE_DIR: &str = "/var/log/kubernetes";

/// The node-internal file kube-apiserver is configured to write its
/// audit log to. Lives inside [`AUDIT_LOG_NODE_DIR`].
const AUDIT_LOG_NODE_FILE: &str = "/var/log/kubernetes/kube-apiserver-audit.log";

/// Inputs needed to render one `kind` cluster configuration with audit
/// logging enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindClusterConfigInput {
    /// The `kind` cluster's name (what kind's own `--name` flag would
    /// otherwise take). Becomes part of the Docker container name
    /// (`<name>-control-plane`) and the kubeconfig context name, so
    /// [`render_kind_config`] validates it: only ASCII lowercase
    /// letters, digits, and `-`, and it must start and end with a
    /// letter or digit.
    pub name: String,
    /// The node's container image reference, ideally digest-pinned (see
    /// `crate::version::resolve_node_image`, added by a sibling task).
    /// Passed through verbatim — this module does not parse or validate
    /// its shape.
    pub node_image: String,
    /// Host path to the rendered audit policy file — the text
    /// [`crate::audit::render_audit_policy`] produces, once written to
    /// disk by the caller. Mounted read-only into the node at
    /// [`AUDIT_POLICY_NODE_FILE`].
    pub audit_policy_host_path: PathBuf,
    /// Host directory the kube-apiserver audit log is made durable
    /// under. Mounted read-write into the node at [`AUDIT_LOG_NODE_DIR`],
    /// so entries the apiserver writes land here on the real host —
    /// outside the ephemeral node container's own lifetime.
    pub audit_log_host_dir: PathBuf,
}

/// Something about a [`KindClusterConfigInput`] made it impossible to
/// render into a valid `kind` cluster configuration.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ClusterConfigError {
    /// [`KindClusterConfigInput::name`] was empty.
    #[error("cluster name must not be empty")]
    EmptyName,
    /// [`KindClusterConfigInput::name`] contained a character other than
    /// an ASCII lowercase letter, digit, or `-`, or started/ended with
    /// `-`.
    #[error(
        "cluster name {name:?} is not a valid kind cluster name: only ASCII lowercase \
         letters, digits, and '-' are allowed, and it must start and end with a \
         lowercase letter or digit"
    )]
    InvalidName {
        /// The rejected name, exactly as given.
        name: String,
    },
    /// [`KindClusterConfigInput::node_image`] was empty (or
    /// all-whitespace).
    #[error("node image reference must not be empty")]
    EmptyNodeImage,
    /// A host path was not valid UTF-8, so it cannot be embedded as a
    /// YAML string scalar.
    #[error("{field} {path:?} is not valid UTF-8")]
    NonUtf8Path {
        /// Which [`KindClusterConfigInput`] field failed (for example
        /// `"audit_policy_host_path"`).
        field: &'static str,
        /// The path that failed to convert.
        path: PathBuf,
    },
}

// ---------------------------------------------------------------------
// Rendering model (private: mirrors kind's own v1alpha4 shape plus the
// embedded kubeadm ClusterConfiguration patch). Never exposed: the
// crate's public contract is exactly `KindClusterConfigInput` and
// `render_kind_config`, per this task's interface.
// ---------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct KindConfig {
    kind: &'static str,
    #[serde(rename = "apiVersion")]
    api_version: &'static str,
    name: String,
    nodes: Vec<KindNode>,
}

#[derive(Debug, Serialize)]
struct KindNode {
    role: &'static str,
    image: String,
    #[serde(rename = "extraMounts")]
    extra_mounts: Vec<ExtraMount>,
    #[serde(rename = "kubeadmConfigPatches")]
    kubeadm_config_patches: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ExtraMount {
    #[serde(rename = "hostPath")]
    host_path: String,
    #[serde(rename = "containerPath")]
    container_path: String,
    #[serde(rename = "readOnly")]
    read_only: bool,
}

#[derive(Debug, Serialize)]
struct KubeadmClusterConfigurationPatch {
    kind: &'static str,
    #[serde(rename = "apiServer")]
    api_server: ApiServerPatch,
}

#[derive(Debug, Serialize)]
struct ApiServerPatch {
    #[serde(rename = "extraArgs")]
    extra_args: BTreeMap<&'static str, &'static str>,
    #[serde(rename = "extraVolumes")]
    extra_volumes: Vec<ExtraVolume>,
}

#[derive(Debug, Serialize)]
struct ExtraVolume {
    name: &'static str,
    #[serde(rename = "hostPath")]
    host_path: &'static str,
    #[serde(rename = "mountPath")]
    mount_path: &'static str,
    #[serde(rename = "readOnly")]
    read_only: bool,
    #[serde(rename = "pathType")]
    path_type: &'static str,
}

/// Renders `input` into a complete `kind` cluster configuration
/// (`kind.x-k8s.io/v1alpha4`, a single control-plane node) with
/// kube-apiserver audit logging enabled: an `extraArgs`/`extraVolumes`
/// kubeadm `ClusterConfiguration` patch plus the matching node
/// `extraMounts` that make the policy file and log directory reach the
/// node from the real host (see this module's documentation for why two
/// mount hops are needed).
///
/// # Errors
///
/// Returns [`ClusterConfigError::EmptyName`] or
/// [`ClusterConfigError::InvalidName`] if `input.name` is empty or not a
/// valid `kind` cluster name, [`ClusterConfigError::EmptyNodeImage`] if
/// `input.node_image` is empty, or [`ClusterConfigError::NonUtf8Path`]
/// if either host path is not valid UTF-8.
///
/// # Panics
///
/// Does not panic: every value serialized here is either a fixed
/// `&'static str`/`bool` constant or a `String` already validated by
/// this function, so the internal YAML rendering step cannot fail.
pub fn render_kind_config(input: &KindClusterConfigInput) -> Result<String, ClusterConfigError> {
    validate_name(&input.name)?;
    if input.node_image.trim().is_empty() {
        return Err(ClusterConfigError::EmptyNodeImage);
    }
    let audit_policy_host_path =
        path_to_utf8("audit_policy_host_path", &input.audit_policy_host_path)?;
    let audit_log_host_dir = path_to_utf8("audit_log_host_dir", &input.audit_log_host_dir)?;

    let patch = KubeadmClusterConfigurationPatch {
        kind: "ClusterConfiguration",
        api_server: ApiServerPatch {
            extra_args: BTreeMap::from([
                ("audit-log-path", AUDIT_LOG_NODE_FILE),
                ("audit-policy-file", AUDIT_POLICY_NODE_FILE),
            ]),
            extra_volumes: vec![
                ExtraVolume {
                    name: "audit-policies",
                    host_path: AUDIT_POLICY_NODE_DIR,
                    mount_path: AUDIT_POLICY_NODE_DIR,
                    read_only: true,
                    path_type: "DirectoryOrCreate",
                },
                ExtraVolume {
                    name: "audit-logs",
                    host_path: AUDIT_LOG_NODE_DIR,
                    mount_path: AUDIT_LOG_NODE_DIR,
                    read_only: false,
                    path_type: "DirectoryOrCreate",
                },
            ],
        },
    };
    let patch_text = serde_norway::to_string(&patch).expect(
        "KubeadmClusterConfigurationPatch holds only fixed &'static str/bool constants, \
         which always serialize",
    );

    let config = KindConfig {
        kind: "Cluster",
        api_version: "kind.x-k8s.io/v1alpha4",
        name: input.name.clone(),
        nodes: vec![KindNode {
            role: "control-plane",
            image: input.node_image.clone(),
            extra_mounts: vec![
                ExtraMount {
                    host_path: audit_policy_host_path.to_owned(),
                    container_path: AUDIT_POLICY_NODE_FILE.to_owned(),
                    read_only: true,
                },
                ExtraMount {
                    host_path: audit_log_host_dir.to_owned(),
                    container_path: AUDIT_LOG_NODE_DIR.to_owned(),
                    read_only: false,
                },
            ],
            kubeadm_config_patches: vec![patch_text],
        }],
    };

    Ok(serde_norway::to_string(&config).expect(
        "KindConfig holds only already-validated UTF-8 strings and fixed &'static str/bool \
         constants, which always serialize",
    ))
}

/// Validates that `name` is safe to use as a `kind` cluster name: only
/// ASCII lowercase letters, digits, and `-`, starting and ending with a
/// letter or digit. This is what keeps a validated name safe to reuse
/// directly as a Docker container name suffix (`<name>-control-plane`),
/// a kubeconfig context name, and a DNS-1123 label.
fn validate_name(name: &str) -> Result<(), ClusterConfigError> {
    if name.is_empty() {
        return Err(ClusterConfigError::EmptyName);
    }
    let is_name_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let valid = name.chars().all(|c| is_name_char(c) || c == '-')
        && name.starts_with(is_name_char)
        && name.ends_with(is_name_char);
    if !valid {
        return Err(ClusterConfigError::InvalidName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// Converts `path` to `&str`, or a [`ClusterConfigError::NonUtf8Path`]
/// naming `field` if it is not valid UTF-8.
fn path_to_utf8<'a>(field: &'static str, path: &'a Path) -> Result<&'a str, ClusterConfigError> {
    path.to_str()
        .ok_or_else(|| ClusterConfigError::NonUtf8Path {
            field,
            path: path.to_path_buf(),
        })
}
