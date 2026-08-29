#![forbid(unsafe_code)]
//! Rendering the static inputs `kind` needs to create Admission Lab's
//! ephemeral baseline/candidate clusters: the cluster configuration
//! (with kube-apiserver audit logging wired in) and the Kubernetes
//! version-to-node-image matrix it resolves against.
//!
//! - [`config`] renders a `kind.x-k8s.io/v1alpha4` cluster configuration
//!   from a small set of caller-supplied inputs
//!   ([`config::render_kind_config`]).
//! - [`audit`] renders the fixed `audit.k8s.io/v1` policy document every
//!   such cluster mounts ([`audit::render_audit_policy`]).
//! - [`version`] maps a requested Kubernetes version to the exact,
//!   digest-pinned `kind` node image Admission Lab has validated
//!   ([`version::resolve_node_image`]), backed by the checked-in
//!   `compatibility/kubernetes.yaml` matrix ([`version::load_matrix`]).
//!
//! Every function here is a pure, in-memory renderer or lookup: this
//! crate performs no filesystem writes, spawns no subprocesses, makes no
//! network calls, and never creates, inspects, or deletes a real `kind`
//! cluster. Actually driving the `kind` cluster lifecycle with these
//! rendered inputs is a later task's responsibility.

pub mod audit;
pub mod config;
pub mod version;

pub use audit::render_audit_policy;
pub use config::{ClusterConfigError, KindClusterConfigInput, render_kind_config};
pub use version::{
    KubernetesImage, KubernetesImageMatrix, ResolvedKubernetes, VersionError, load_matrix,
    resolve_node_image,
};
