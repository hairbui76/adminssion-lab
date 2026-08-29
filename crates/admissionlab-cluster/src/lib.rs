#![forbid(unsafe_code)]
//! Rendering the static inputs `kind` needs to create Admission Lab's
//! ephemeral baseline/candidate clusters: the cluster configuration,
//! with kube-apiserver audit logging wired in.
//!
//! - [`config`] renders a `kind.x-k8s.io/v1alpha4` cluster configuration
//!   from a small set of caller-supplied inputs
//!   ([`config::render_kind_config`]).
//! - [`audit`] renders the fixed `audit.k8s.io/v1` policy document every
//!   such cluster mounts ([`audit::render_audit_policy`]).
//!
//! Every function here is a pure, in-memory renderer: this crate
//! performs no filesystem writes, spawns no subprocesses, and never
//! creates, inspects, or deletes a real `kind` cluster. Actually driving
//! the `kind` cluster lifecycle with these rendered inputs is a later
//! task's responsibility.

pub mod audit;
pub mod config;

pub use audit::render_audit_policy;
pub use config::{ClusterConfigError, KindClusterConfigInput, render_kind_config};
