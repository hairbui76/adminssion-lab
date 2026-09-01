#![forbid(unsafe_code)]
//! `admissionlab-echo`: the deterministic HTTP echo backend Gateway
//! data-plane comparisons route traffic to (Task 6.5).
//!
//! Phase 6 compares a baseline and a candidate Gateway stack by sending
//! the *same* HTTP request through each one and asking which workload
//! actually answered it. That question only has a trustworthy answer if
//! the workload behind the Gateway is itself deterministic: an upstream
//! sample application (`httpbin`, `echoserver`, ...) can change its
//! response body between releases and turn an Admission Lab test into a
//! test of somebody else's container, which is the same reasoning
//! PRODUCT.md §30 already applies to `admissionlab-test-webhook` on the
//! admission side.
//!
//! # The frozen response contract
//!
//! Every request that is not `GET /healthz` is answered `200 OK` with
//! exactly this JSON object, and no other keys, in exactly this order
//! (ROADMAP Task 6.5's own "Interfaces" block):
//!
//! ```json
//! {
//!   "backend": "echo-a",
//!   "method": "GET",
//!   "path": "/payments",
//!   "host": "api.example.test",
//!   "headers": {"x-test": "value"}
//! }
//! ```
//!
//! This shape is **frozen**. Task 6.8's HTTP probe engine
//! (`crates/admissionlab-gateway/src/probe.rs`) parses it to fill
//! `HttpProbeResult::backend`, and Task 6.9's comparator turns a change
//! in that field into the `traffic_backend_changed` semantic change --
//! the regression "a route still returns 200 but now reaches a different
//! workload", which is invisible to a status-code-only probe. Adding,
//! removing, renaming or reordering a key here is a contract change to
//! that probe, not a local refactor of this crate. [`echo::EchoBody`] is
//! the single definition of the shape; its own documentation records
//! what each field means and what is deliberately *not* in it.
//!
//! # Where the behavior lives
//!
//! - [`config`]: the two environment variables this binary reads, and
//!   why the backend id is required rather than defaulted.
//! - [`echo`]: the frozen body, and the header normalization rules
//!   (sorted, lowercased, hop-by-hop excluded).
//! - [`delay`]: the optional response delay, for later timeout tests.
//! - [`serve`]: the plain-HTTP listener, routing, and `GET /healthz`.
//!
//! # Plain HTTP, on purpose
//!
//! Unlike `admissionlab-test-webhook`, this server terminates no TLS
//! and holds no certificate of its own: it sits *behind* the
//! Gateway under test, which is the component that terminates TLS, and
//! adding a second TLS hop would only make a data-plane comparison
//! depend on this crate's own certificate handling.
//!
//! # This binary never talks to the Kubernetes API
//!
//! It holds no `kube::Client`, needs no `ServiceAccount`, and reads
//! nothing from the cluster. An echo answer is a pure function of the
//! request that arrived plus the two environment variables in
//! [`config`] -- which is exactly what makes a difference between two
//! echo answers attributable to the Gateway under test rather than to
//! this backend.
//!
//! Every module logs via `tracing` (`RUST_LOG` controls verbosity;
//! `info` by default) rather than printing directly, matching every
//! other binary in this workspace.

pub mod config;
pub mod delay;
pub mod echo;
pub mod serve;
