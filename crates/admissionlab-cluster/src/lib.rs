#![forbid(unsafe_code)]
//! Rendering the static inputs `kind` needs to create Admission Lab's
//! ephemeral baseline/candidate clusters, and actually driving that
//! cluster's lifecycle through them.
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
//! - [`kind`] holds pure facts about talking to the `kind` CLI: argv,
//!   timeouts, and the cluster-naming rules ([`cluster_name`],
//!   [`validate_cluster_name`]).
//! - [`lifecycle`] implements
//!   [`admissionlab_core::ClusterManager`] against `kind`
//!   ([`KindClusterManager`]): creating and deleting isolated clusters
//!   through [`admissionlab_core::ProcessRunner`], with rollback on a
//!   partial create failure.
//! - `kubeconfig` (crate-private) isolates each cluster's kubeconfig and
//!   applies this project's `0600` guarantee to it after `kind` writes
//!   it directly.
//!
//! `config`, `audit`, and `version` remain pure, in-memory renderers and
//! lookups with no filesystem, process, or network I/O of their own.
//! [`lifecycle::KindClusterManager`] is where this crate actually
//! performs I/O: writing the files a cluster needs and invoking `kind`
//! itself, always through
//! [`admissionlab_core::ProcessRunner`] rather than a direct
//! `std::process`/`tokio::process` call (Global Constraint 12).
//!
//! Controller Ruling R22: [`admissionlab_core::ClusterManager`] and its
//! data types (`ClusterSpec`, `ClusterHandle`, `ClusterError`,
//! `ClusterDiagnostics`) are defined in `admissionlab-core`, not here —
//! see that trait's module documentation for why keeping them there
//! (rather than where the plan's crate map originally implied) is what
//! avoids a dependency cycle. This crate provides the concrete `kind`
//! implementation of that trait.

pub mod audit;
pub mod config;
pub mod kind;
mod kubeconfig;
pub mod lifecycle;
pub mod version;

pub use audit::render_audit_policy;
pub use config::{ClusterConfigError, KindClusterConfigInput, render_kind_config};
pub use kind::{cluster_name, validate_cluster_name};
pub use lifecycle::KindClusterManager;
pub use version::{
    KubernetesImage, KubernetesImageMatrix, ResolvedKubernetes, VersionError, load_matrix,
    resolve_node_image,
};
