#![forbid(unsafe_code)]
//! Installer behavior for the components an `admissionlab_spec::LabSpec`
//! resolves into.
//!
//! This crate provides *behavior* only — the Helm backend (Task 2.2), the
//! raw-manifest backend (Task 2.3), readiness probing (Task 2.4), and
//! stack installation orchestration (Task 2.6). The vocabulary that
//! behavior operates on — `admissionlab_spec::InstallMethod`,
//! `admissionlab_spec::ReadinessCheck`, and the fully resolved
//! `admissionlab_spec::ResolvedComponent` — is defined in
//! `admissionlab-spec`, not here (Controller Ruling R30): `spec` must
//! stay a leaf crate, and this crate needs `Diagnostic` (from
//! `admissionlab-core`) and `ClusterHandle` (from `admissionlab-cluster`)
//! for the behavior it *does* own, so defining the resolved vocabulary
//! here instead would close `spec -> installer -> core -> spec` into a
//! cycle the moment `spec::resolve_lab` needed to produce one.
//!
//! (Plain code spans rather than doc links above: this crate does not
//! depend on `admissionlab-spec` yet, so rustdoc could not resolve a
//! link into it.)
//!
//! Nothing has landed here yet: as of Task 2.1 this crate has no
//! behavior to implement (that starts at Task 2.2) and therefore no
//! dependency on `admissionlab-spec` either — one will be added once a
//! task here actually calls into it.
