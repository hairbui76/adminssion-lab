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

## Non-negotiable engineering constraints

These are not style preferences. A change that breaks one of them is a bug
regardless of what else it improves, and most are pinned by a test that will
fail rather than let it merge.

- **Argv, never a shell string.** External commands are built as argument
  vectors. A shell command string assembled from user input — a configuration
  value, a fixture, a recipe field, a cluster response — is never acceptable.
- **Every external command is bounded.** A timeout, separate stdout and stderr
  capture, recorded version/provenance, and structured error context. A
  subprocess must not outlive the run that spawned it.
- **Reports redact.** Secret data, authorization headers, private keys,
  configured sensitive paths, and credential-like values are redacted by
  Admission Lab's own rendering, once, into one value from which the terminal,
  JSON, and HTML views are all drawn. A new rendering path that bypasses that
  is a security bug — see [`SECURITY.md`](SECURITY.md).
- **Missing evidence is never fabricated.** Absent or ambiguous data is
  represented as unavailable, unknown, or inconclusive. It is never filled in
  with a plausible value, and a missing signal never becomes a zero.
- **Server-side dry-run is the authoritative admission mode.** The response
  object is the final admitted/mutated object. A fixture that cannot safely be
  evaluated that way must fail explicitly as unsupported rather than silently
  switch semantics, and there is no in-process simulator anywhere in the result
  path.
- **Fixture execution is serial within each cluster.** That is what makes
  audit-log correlation deterministic. Parallel execution is allowed only after
  request-level correlation exists and is tested.
- **Per-webhook latency is an optional observed signal.** Absent or ambiguous
  metrics never fail a run by themselves.
- **Recipes may never classify.** Install, readiness, normalization, and
  capability metadata only. The recipes crate cannot reach
  `admissionlab-diff` or `admissionlab-policy`, and the schema's allow-list
  makes a classification-shaped key fail to parse.

## What v1 freezes

[`docs/versioning.md`](docs/versioning.md) is the full statement; the part that
constrains a pull request is short. Within `v1.x`:

- **Document schemas are additive only.** A new field is optional and absent
  from the schema's `required` list. No field meaning changes, no required
  field is removed, and no semantic-change wire string
  (`newly_denied`, `container_removed`, …) is renamed. Superset tests compare
  the generated `v1` schema against every frozen predecessor and will fail the
  build.
- **The CLI surface is frozen.** No command, positional argument, or long flag
  is renamed or removed, and none changes whether it takes a value. Adding a
  new optional flag with a backwards-compatible default is the only change
  inside the contract. `crates/admissionlab-cli/tests/exit_codes.rs` pins the
  surface mechanically; rewording a help *description* is free.
- **Exit codes are never reassigned**, including `130`/`143` for a canceled
  run.

If your change genuinely requires breaking one of these, say so in the pull
request before writing it. It is a `v2` conversation, not a review comment.

**Add a `CHANGELOG.md` entry** for anything a user would notice: a new field or
flag, a new semantic-change kind, a severity change, a certified row, a
supported Kubernetes minor, a fix to a wrong classification. Keep a Changelog
categories, under `[Unreleased]`. Internal refactors need no entry.

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

## Proposing a certified recipe

A **certification** is a claim that this repository installed that recipe at
that version on that exact Kubernetes patch version and observed the component
doing its job. Adding a row to `compatibility/recipes.yaml` therefore means
adding all of:

1. exact version pins — no ranges, no floating tags;
2. readiness checks that prove the component is *serving*, not merely
   scheduled;
3. a certification test that actually installs it and exercises real behavior,
   registered in `scripts/recipe-matrix.py` (which refuses to build a matrix
   for a recipe it has no test for);
4. the CI tier that will run it — a certification nobody schedules is a claim
   rather than evidence;
5. the vendor's own documented Kubernetes range at the vendor's own
   granularity, or an explicit `null` when no statement exists. An absent or
   unbounded constraint means *unknown*, never *supported*.

And, as always, nothing in the recipe that classifies a difference. See
[`docs/recipes.md`](docs/recipes.md) and
[`docs/compatibility.md`](docs/compatibility.md).

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
