# syntax=docker/dockerfile:1
#
# Builds `admissionlab-test-webhook` (Task 2.7) — Admission Lab's own
# deterministic dogfood admission webhook (PRODUCT.md §30). See
# `src/main.rs`'s own module documentation for what the resulting binary
# does (`bootstrap`/`serve` subcommands) and why.
#
# Built and loaded into a `kind` cluster by `scripts/build-test-images.sh`
# — see that script for the exact `docker build`/`kind load` invocation
# and the reasoning behind `IMAGE_TAG` below. Never pushed to a registry:
# this image only ever needs to exist inside one ephemeral `kind`
# cluster's own node, for the lifetime of one test run.
#
# # Build stage: the project's own pinned toolchain, nothing more
#
# Note this build compiles *inside* the image: the host-machine
# `CC=/usr/bin/cc`/`CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER` workaround
# this repository's other build instructions require (a version-switcher
# script shadowing the real compiler on one specific development machine)
# does not apply here — this stage's own `cc` is whatever `build-essential`
# installs, with nothing shadowing it.
#
# `rust-toolchain.toml` (pinning `1.98.0`) is deliberately **not** copied
# into the build context: the base image below already *is* exactly that
# toolchain, so there is nothing for a `rust-toolchain.toml`-triggered
# `rustup` re-resolution to do except add an avoidable network dependency
# during the build.
FROM docker.io/library/rust:1.98.0-slim-bookworm AS builder

# `ring` (this binary's TLS crypto backend, via `rustls`/`kube`) compiles
# C code through `cc-rs` at build time — the same reason this whole
# workspace already requires a working C toolchain on the host machine
# (see the root `Cargo.toml`'s own comment on the `kube` dependency).
# `--no-install-recommends` keeps this stage from pulling in anything
# beyond the compiler itself; irrelevant to the final image's size
# regardless, since this whole stage is discarded below.
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace

# The whole workspace, not just this one crate: `Cargo.toml`'s
# `[workspace] members = ["crates/*"]` means Cargo reads every member's
# own `Cargo.toml` during resolution even though only
# `admissionlab-test-webhook` is actually built below (Task 2.7's own
# binary has no path dependency on any other workspace crate at all —
# only its `[dev-dependencies]`, for its own tests, do — but the
# workspace manifest itself still needs every member present to resolve
# at all).
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release --locked -p admissionlab-test-webhook

# # Runtime stage: distroless, non-root, no shell
#
# `gcr.io/distroless/cc-debian12`: glibc + libgcc/libstdc++ only (this
# binary is a `ring`-backed, dynamically-linked `x86_64-unknown-linux-gnu`
# build — no `musl` cross-compilation, and therefore no `scratch`/
# `static` distroless variant, which would need one) — no shell, no
# package manager, nothing beyond what a compiled Rust binary linked
# against glibc actually needs to run. Pinned by digest (not merely the
# `nonroot` tag) for the same "deterministic, reproducible build" reason
# `scripts/verify-cleanup.sh`'s own `kindest/node` pin already documents
# for this project — a tag can move to a different image tomorrow, a
# digest cannot. `:nonroot` resolves to a fixed, non-zero UID/GID
# (65532), matching this image's `USER` below and the Deployment
# manifest's own `runAsUser`/`runAsGroup` (`recipes/test-webhook/manifests/30-deployment.yaml`).
# Digest verified by pulling the `nonroot` tag and reading
# `docker inspect`'s own `RepoDigests` back on 2026-08-30 (this task's
# own working session) — not copied from memory or documentation.
FROM gcr.io/distroless/cc-debian12@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f AS runtime

COPY --from=builder /workspace/target/release/admissionlab-test-webhook /usr/local/bin/admissionlab-test-webhook

USER 65532:65532

ENTRYPOINT ["/usr/local/bin/admissionlab-test-webhook"]
