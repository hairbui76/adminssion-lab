# Contributing to Admission Lab

Thank you for your interest in contributing. This document explains the
project's non-negotiable principles, how to propose changes, and how to
run the verification suite before opening a pull request.

## Project principles

Admission Lab must remain local-first, deterministic, vendor-neutral at the
core, real-cluster authoritative, CI-friendly, safe by default, fully open
source, and useful without a server.

In practice this means:

- **Local-first.** Every important workflow must run without an Admission
  Lab server or account. Contributions must not introduce a hard dependency
  on a hosted service for core functionality.
- **Real-cluster authoritative.** The authoritative result comes from a real
  Kubernetes API server and real component installation, never an in-process
  simulation of admission or Gateway behavior.
- **Vendor-neutral at the core.** Vendor-specific recipes may simplify
  installation and normalization, but they must not own the regression
  engine. Generic crates (`admissionlab-core`, `admissionlab-admission`,
  `admissionlab-normalize`, `admissionlab-diff`, `admissionlab-policy`,
  `admissionlab-gateway`, etc.) must contain no vendor-specific logic.
- **Deterministic decisions.** A result must be reproducible from recorded
  inputs except for explicitly identified external nondeterminism.
  Pass/warn/fail classification must be deterministic; AI is not required
  for v1 and is out of scope.
- **No proprietary gating.** The project is Apache-2.0 and fully open
  source. Essential functionality must never be gated behind a proprietary
  service or license.

Every significant feature proposal (issue, discussion, or pull request
description) should answer:

1. Which concrete admission or Gateway regression does this enable
   Admission Lab to detect, explain, or gate? ("What regression does this
   catch?" is the feature test — every feature must earn its place by
   answering this.)
2. Why can't the existing core model express it?
3. Does it preserve deterministic behavior?
4. Does it introduce a vendor-specific dependency into the generic engine?
5. Can it remain useful in local/CI workflows without a central service?

Features that cannot answer these questions should normally stay out of
scope. If in doubt, open an issue to discuss before writing code.

## Workspace layout

Admission Lab is a Rust Cargo workspace under `crates/`. Each crate has a
single responsibility; see `PRODUCT.md` section 11 for the full boundary
list. Do not let a generic crate accumulate unrelated responsibilities, and
do not introduce a dependency edge from a generic crate onto
`admissionlab-cli`.

## Prerequisites

- The exact Rust toolchain pinned in `rust-toolchain.toml` (installed
  automatically by `rustup` when you run any `cargo` command in this
  repository).
- [`kind`](https://kind.sigs.k8s.io/), `kubectl`, and `helm` on `PATH` for
  workflows that exercise a real cluster.

## Verifying a change

Before opening a pull request, run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check --workspace --all-targets
cargo test --workspace
```

All four must pass with no warnings. `clippy::pedantic` is enabled
workspace-wide as a warning and is treated as a hard error in CI; prefer
fixing the flagged code (adding `#[must_use]`, documentation, `const`,
etc.) over adding `#[allow(...)]`.

If your change touches dependencies, also run:

```bash
cargo deny check
```

`deny.toml` allows only a fixed set of permissive licenses and rejects
dependencies from unreviewed git or registry sources. New dependencies must
satisfy that policy or the addition must explain, in the pull request, why
an exception is warranted.

## Commit and pull request expectations

- Keep commits focused; prefer several small, reviewable commits over one
  large one.
- Write commit subjects in the imperative mood with a short scope prefix
  (for example `build:`, `feat:`, `fix:`, `docs:`) describing the change.
- Describe, in the pull request body, which regression the change catches
  or which existing behavior it preserves, and how you verified it (see
  above).

## Reporting security issues

Do not open a public issue for a security vulnerability. See
[`SECURITY.md`](SECURITY.md) for how to report one privately.

## Code of conduct

Participation in this project is governed by our
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).
