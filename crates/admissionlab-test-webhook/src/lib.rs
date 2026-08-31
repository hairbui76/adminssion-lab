#![forbid(unsafe_code)]
//! `admissionlab-test-webhook`: Admission Lab's own deterministic
//! dogfood admission webhook (PRODUCT.md §30).
//!
//! Admission Lab compares baseline and candidate admission stacks; to
//! test *itself* it needs a component whose admission behavior is known
//! and deterministic, so a change in a vendor's release (Kyverno,
//! Istio, ...) never breaks Admission Lab's own test suite for reasons
//! unrelated to Admission Lab (PRODUCT.md §30: "This prevents core
//! tests from depending entirely on external vendor behavior").
//!
//! # The two container processes
//!
//! The binary ([`main`](../main.rs), `src/main.rs`) has exactly two
//! subcommands, run as this recipe's Deployment's init and main
//! containers respectively (`recipes/test-webhook/manifests/30-deployment.yaml`):
//!
//! - `bootstrap` ([`bootstrap::run`]): generates a fresh, test-only,
//!   per-cluster CA and serving certificate, writes the serving
//!   certificate/key where `serve` reads them from, and updates every
//!   webhook configuration this recipe installs so their `caBundle`s
//!   validate that certificate.
//! - `serve` ([`serve::run`]): the HTTPS server — `GET /healthz` plus
//!   the three admission-review routes.
//!
//! # Why this is a library crate as well as a binary
//!
//! Task 2.7 shipped this as a binary only, which was right when the
//! whole HTTP surface was one static route testable from inside the
//! module. Task 3.9's contract is larger and crosses files: the exact
//! JSON Patch bytes on the wire, and the fact that each route constant
//! here matches the `clientConfig.service.path` of the webhook
//! configuration that calls it. `crates/admissionlab-test-webhook/tests/behavior.rs`
//! asserts both, against the real checked-in manifests — and an
//! integration test can only reach [`serve::handle`] and
//! [`serve::VALIDATE_PATH`] if they are library items. `src/main.rs` is
//! then a thin `clap` front end over this library, holding no logic of
//! its own.
//!
//! # Where the behavior lives
//!
//! - [`behavior`]: the `test.admissionlab.io/*` annotation vocabulary,
//!   and the only place a behavior is ever decided.
//! - [`mutate`]: RFC 6902 patch construction for the mutating routes.
//! - [`validate`]: deny, controlled delay, controlled failure.
//! - [`serve`]: TLS, routing, and the `AdmissionReview` wire types.
//! - [`bootstrap`] / [`cert`]: the init container's certificate work.
//! - [`config`]: the three environment variables `bootstrap` reads.
//!
//! Every module logs via `tracing` (`RUST_LOG` controls verbosity;
//! `info` by default) rather than printing directly, so pod logs stay
//! structured and filterable the same way every other binary in this
//! workspace already logs.

pub mod behavior;
pub mod bootstrap;
pub mod cert;
pub mod config;
pub mod mutate;
pub mod serve;
pub mod validate;
