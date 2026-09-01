# syntax=docker/dockerfile:1
#
# Builds `admissionlab-echo` (Task 6.5) -- Admission Lab's own
# deterministic HTTP echo backend, the workload Gateway data-plane
# comparisons route traffic to. See `src/lib.rs`'s own module
# documentation for the frozen response contract this binary serves and
# why the component exists at all.
#
# Built and loaded into a `kind` cluster by `scripts/build-test-images.sh`
# alongside `admissionlab-test-webhook`'s image -- see that script for
# the exact `docker build`/`kind load` invocation and the reasoning
# behind `IMAGE_TAG`. Never pushed to a registry: this image only ever
# needs to exist inside one ephemeral `kind` cluster's own node, for the
# lifetime of one test run. The ROADMAP's Task 6.5 Step 5 digest pinning
# applies to a *published* image at release time; the local development
# path is `kind load docker-image`, which is what the fixture manifests'
# `imagePullPolicy: IfNotPresent` is written for.
#
# This Dockerfile is deliberately near-identical to
# `crates/admissionlab-test-webhook/Dockerfile`: one build convention for
# both of this project's test-only workload images, so a change to how
# they are built is a change in two obviously-parallel files rather than
# an archaeology exercise. The differences are noted where they occur.
#
# # Build stage: the project's own pinned toolchain, nothing more
#
# Note this build compiles *inside* the image: the host-machine
# `CC=/usr/bin/cc`/`CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER`
# workaround this repository's other build instructions require (a
# version-switcher script shadowing the real compiler on one specific
# development machine) does not apply here -- this stage's own `cc` is
# the base image's, with nothing shadowing it.
#
# `rust-toolchain.toml` (pinning `1.98.0`) is deliberately **not** copied
# into the build context: the base image below already *is* exactly that
# toolchain, so there is nothing for a `rust-toolchain.toml`-triggered
# `rustup` re-resolution to do except add an avoidable network dependency
# during the build.
FROM docker.io/library/rust:1.98.0-slim-bookworm AS builder

# No `build-essential` here, unlike `admissionlab-test-webhook`'s build
# stage. That stage needs a C toolchain because `ring` (its TLS crypto
# backend, via `rustls`/`kube`) compiles C through `cc-rs`; this binary
# terminates no TLS and touches no Kubernetes API, so `cargo build -p
# admissionlab-echo` resolves a dependency graph with no `-sys` crate
# and no build script that shells out to a compiler in it (verified by
# building this image, not assumed). Anything that reintroduces one --
# a TLS client, a Kubernetes client -- reintroduces this line too.

WORKDIR /workspace

# The whole workspace, not just this one crate: `Cargo.toml`'s
# `[workspace] members = ["crates/*"]` means Cargo reads every member's
# own `Cargo.toml` during resolution even though only `admissionlab-echo`
# is actually built below (this crate has no path dependency on any
# other workspace crate at all -- see its own `Cargo.toml` -- but the
# workspace manifest still needs every member present to resolve).
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release --locked -p admissionlab-echo

# # Runtime stage: distroless, non-root, no shell
#
# The same `gcr.io/distroless/cc-debian12` image, at the same digest, as
# `crates/admissionlab-test-webhook/Dockerfile` -- deliberately, rather
# than the smaller `base-debian12` this binary would technically fit in:
# that digest has already been verified and is already loaded into every
# `kind` node this project's own scripts touch, so reusing it means one
# pinned base image to re-verify when it is bumped instead of two, and
# no second unverified digest entering the repository. (Digest verified
# by pulling the `nonroot` tag and reading `docker inspect`'s own
# `RepoDigests` back on 2026-08-30, in Task 2.7's working session; this
# task reuses that verified value rather than re-deriving it.) The
# marginal image-size difference is irrelevant for an image that only
# ever exists inside one ephemeral cluster's node.
#
# `:nonroot` resolves to a fixed, non-zero UID/GID (65532), matching this
# image's `USER` below and the fixtures' own `runAsUser`/`runAsGroup`
# (`fixtures/gateway/backends/echo-a.yaml`, `echo-b.yaml`). No shell, no
# package manager, nothing beyond what a compiled Rust binary linked
# against glibc actually needs to run.
FROM gcr.io/distroless/cc-debian12@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f AS runtime

COPY --from=builder /workspace/target/release/admissionlab-echo /usr/local/bin/admissionlab-echo

USER 65532:65532

# 8080, matching `admissionlab_echo::serve::PORT` and both fixtures'
# `containerPort`. Documentation only -- `EXPOSE` publishes nothing by
# itself -- but it keeps the three places that name this port together.
EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/admissionlab-echo"]
