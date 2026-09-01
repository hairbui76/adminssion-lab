#!/usr/bin/env bash
#
# Phase 9 exit gate: builds, packages, checksums, and smoke-tests the
# release artifact for *this* host, exactly the way
# `.github/workflows/release.yml` builds, packages, checksums, and
# smoke-tests the four artifacts a tag produces (ROADMAP Task 9.9).
#
# The point is that a maintainer can answer "would the release workflow
# produce a working artifact?" before pushing a tag, on a laptop, with no
# tag, no draft release, and no signing identity. Everything the workflow
# does that does not require GitHub is done here:
#
#   1. `cargo build --locked --release` for the host target;
#   2. the same tarball layout (`admissionlab-<version>-<target>/`
#      containing the binary, `LICENSE`, and `README.md`);
#   3. a `SHA256SUMS` covering every artifact -- the archive *and* the
#      SBOM -- written and then verified with `--check`;
#   4. `SBOM.spdx.json` from the same pinned generator the workflow uses;
#   5. an unpack-and-run smoke test of the packaged binary: `--version`
#      must equal the version in `crates/admissionlab-cli/Cargo.toml`,
#      and `doctor --help` and `test --help` must exit 0;
#   6. SBOM sanity: it must name `admissionlab-cli` and list a plausible
#      number of packages.
#
# What is deliberately NOT done here, because it cannot be:
#
#   - the other three targets. `ring` (reached through `rustls` ->
#     `kube`) ships per-architecture assembly and a C build script, so a
#     cross-build needs a full cross toolchain and sysroot; the workflow
#     solves that with native runners, and this script honestly checks
#     one platform -- the one it is running on -- rather than pretending
#     to check four.
#   - `cosign sign-blob`. Keyless signing needs a GitHub OIDC token that
#     only the workflow has. `docs/install.md` documents the
#     `cosign verify-blob` side, which is what a downloader runs.
#   - the tag/version equality check. There is no tag here. The version
#     is read from the manifest and the built binary is required to
#     agree with it, which is the half of that check that can fail for a
#     reason other than a typo in `git tag`.
#
# Because this script and the workflow are two implementations of one
# packaging format, they can drift. Two things keep them honest: the
# SBOM generator version below is compared against the workflow's own
# pin and a mismatch fails the run, and every step here names the
# workflow step it mirrors.
#
# Usage:
#   ./scripts/verify-release.sh                # build in a temp dir, remove it on success
#   ./scripts/verify-release.sh --keep         # keep the staging directory and print its path
#   ./scripts/verify-release.sh --out-dir DIR  # stage into DIR (created if absent, always kept)
#
# Exits non-zero and prints a line starting with "verify-release: FAIL"
# on the first problem: a missing tool, a failed build, a checksum
# mismatch, a binary that will not run or reports the wrong version, or
# an SBOM that does not describe this project.
#
# Exit codes: 0 pass, 1 a verification failed, 2 bad arguments, 3 a host
# prerequisite is missing.

set -euo pipefail

# The SBOM generator, pinned exactly. `cargo-sbom` reads the Cargo
# dependency graph rather than scanning a built binary, so the packages
# it lists are the crates `Cargo.lock` pins -- the same source of truth
# `--locked` builds from -- and it emits SPDX 2.3 JSON, which is the
# format `SBOM.spdx.json` promises. `.github/workflows/release.yml`
# installs this same version; the two are compared below.
readonly CARGO_SBOM_VERSION="0.10.0"

# The workspace member whose binary is released. The SBOM is scoped to
# it rather than to the whole workspace: it must describe what ships,
# and the test-only crates (`admissionlab-echo`,
# `admissionlab-test-webhook`) do not.
readonly RELEASE_PACKAGE="admissionlab-cli"

# A floor, not an expectation. The released binary reaches ~240 crates
# through `kube`/`rustls`/`tokio`, so a count in the low tens would mean
# the generator resolved something other than this project (an empty
# workspace, a stub manifest) and produced a well-formed document about
# it. This catches that; it is not a dependency-count budget, which is
# `cargo deny`'s job (Task 9.10).
readonly MIN_SBOM_PACKAGES=100

usage() {
  echo "usage: $0 [--keep | --out-dir <dir>]" >&2
}

keep=false
out_dir=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --keep)
      keep=true
      shift
      ;;
    --out-dir)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then
        echo "verify-release: error: --out-dir needs a directory" >&2
        usage
        exit 2
      fi
      out_dir="$2"
      keep=true
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "verify-release: error: unexpected argument '$1'" >&2
      usage
      exit 2
      ;;
  esac
done
readonly keep
readonly out_dir

fail() {
  echo "verify-release: FAIL: $*" >&2
  exit 1
}

step() {
  echo
  echo "verify-release: == $* =="
}

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
readonly repo_root
readonly workflow="${repo_root}/.github/workflows/release.yml"

# ---------------------------------------------------------------- host

for tool in cargo rustc tar; do
  command -v "${tool}" >/dev/null 2>&1 ||
    { echo "verify-release: error: '${tool}' is not on PATH" >&2; exit 3; }
done

# GNU coreutils on Linux, `shasum` on macOS. Both write and verify the
# same `<hex>  <name>` format, which is the format the release's
# `SHA256SUMS` is in and the format `docs/install.md` tells a downloader
# to check -- so the file this script produces is byte-comparable with
# the one a release publishes.
if command -v sha256sum >/dev/null 2>&1; then
  sha256_write() { sha256sum "$@"; }
  sha256_check() { sha256sum --check --strict "$@"; }
elif command -v shasum >/dev/null 2>&1; then
  sha256_write() { shasum --algorithm 256 "$@"; }
  sha256_check() { shasum --algorithm 256 --check --strict "$@"; }
else
  echo "verify-release: error: neither 'sha256sum' nor 'shasum' is on PATH" >&2
  exit 3
fi

# `cargo pkgid` rather than `cargo metadata | jq`: the workflow can rely
# on `jq` because GitHub's runners ship it, and a contributor's machine
# may not. The output is `<source>#<version>` or `<source>#<name>@<version>`.
version="$(cd "${repo_root}" && cargo pkgid -p "${RELEASE_PACKAGE}" 2>/dev/null | sed 's/.*[#@]//')"
[ -n "${version}" ] || fail "could not read the ${RELEASE_PACKAGE} version from 'cargo pkgid'"
readonly version

target="$(rustc --version --verbose | sed -n 's/^host: //p')"
[ -n "${target}" ] || fail "could not read the host target from 'rustc -vV'"
readonly target

readonly name="admissionlab-${version}-${target}"
readonly archive="${name}.tar.gz"

echo "verify-release: ${RELEASE_PACKAGE} ${version} for ${target}"
echo "verify-release: artifact ${archive}"

case "${target}" in
  *-unknown-linux-gnu | *-apple-darwin) ;;
  *)
    echo "verify-release: note: ${target} is not one of the four released"
    echo "verify-release: note: targets; everything below still runs, but a"
    echo "verify-release: note: pass here does not speak for a release build."
    ;;
esac

if [ -n "${out_dir}" ]; then
  mkdir -p "${out_dir}"
  staging="$(cd "${out_dir}" && pwd)"
else
  staging="$(mktemp -d)"
fi
readonly staging

cleanup() {
  if [ "${keep}" = true ]; then
    echo "verify-release: staging directory kept: ${staging}"
  else
    rm -rf "${staging}"
  fi
}
trap cleanup EXIT

# ------------------------------------------------------------- the pin

step "checking the SBOM generator pin against the release workflow"

# One number, two files. A bump that touches only one of them means the
# release would be described by a generator this gate never ran, so it
# is a failure rather than a warning.
[ -f "${workflow}" ] || fail "no ${workflow} to compare the SBOM pin against"
workflow_pin="$(sed -n 's/^ *CARGO_SBOM_VERSION: *"\{0,1\}\([0-9][^" ]*\)"\{0,1\} *$/\1/p' "${workflow}" | LC_ALL=C sort -u)"
[ -n "${workflow_pin}" ] ||
  fail "release.yml declares no CARGO_SBOM_VERSION; this script and the workflow must pin one generator"
[ "$(printf '%s\n' "${workflow_pin}" | wc -l | tr -d ' ')" = "1" ] ||
  fail "release.yml declares more than one CARGO_SBOM_VERSION: ${workflow_pin}"
[ "${workflow_pin}" = "${CARGO_SBOM_VERSION}" ] ||
  fail "SBOM generator pin drift: this script pins cargo-sbom ${CARGO_SBOM_VERSION}, release.yml pins ${workflow_pin}"
echo "verify-release: cargo-sbom ${CARGO_SBOM_VERSION} in both this script and release.yml"

# --------------------------------------------------------------- build

step "building the release binary (cargo build --locked --release)"

# `--locked` for the reason the workflow uses it: a released binary must
# be built from the dependency versions this repository reviewed, never
# from whatever resolves today. `--target` is passed explicitly so the
# output path is the one the workflow packages from, even on a host
# where the implicit target directory would differ.
(
  cd "${repo_root}"
  cargo build --release --locked -p "${RELEASE_PACKAGE}" --target "${target}"
)

binary="${repo_root}/target/${target}/release/admissionlab"
[ -x "${binary}" ] || fail "no executable at ${binary} after the build"
readonly binary

# ------------------------------------------------------------- package

step "packaging ${archive}"

# The layout the workflow's "Package the artifact" step produces, and
# the layout `.github/actions/admissionlab/action.yml` unpacks and
# `docs/install.md` documents: one top-level directory named after the
# archive, holding the binary next to the licence and the README.
payload="${staging}/payload/${name}"
mkdir -p "${payload}"
cp "${binary}" "${payload}/"
cp "${repo_root}/LICENSE" "${repo_root}/README.md" "${payload}/"
tar --create --gzip --file "${staging}/${archive}" --directory "${staging}/payload" "${name}"
[ -s "${staging}/${archive}" ] || fail "packaging produced an empty ${archive}"

# ---------------------------------------------------------------- sbom

step "generating SBOM.spdx.json (cargo-sbom ${CARGO_SBOM_VERSION})"

installed_sbom_version=""
if command -v cargo-sbom >/dev/null 2>&1; then
  installed_sbom_version="$(cargo-sbom --version 2>/dev/null | sed -n 's/^cargo-sbom \([0-9][^ ]*\).*/\1/p')"
fi

if [ "${installed_sbom_version}" != "${CARGO_SBOM_VERSION}" ]; then
  if [ -n "${installed_sbom_version}" ]; then
    echo "verify-release: cargo-sbom ${installed_sbom_version} is installed; the release pins ${CARGO_SBOM_VERSION}"
  else
    echo "verify-release: cargo-sbom is not installed"
  fi
  echo "verify-release: installing cargo-sbom ${CARGO_SBOM_VERSION} (this replaces any other version on PATH)"
  # `--locked` again, this time for the generator's own dependency
  # graph: the tool that describes our dependencies is itself built
  # from a lockfile rather than from today's resolution.
  cargo install cargo-sbom --version "${CARGO_SBOM_VERSION}" --locked ||
    fail "could not install cargo-sbom ${CARGO_SBOM_VERSION}"
fi

readonly sbom="${staging}/SBOM.spdx.json"
(
  cd "${repo_root}"
  cargo sbom --cargo-package "${RELEASE_PACKAGE}" --output-format spdx_json_2_3
) > "${sbom}" || fail "cargo sbom exited non-zero"
[ -s "${sbom}" ] || fail "cargo sbom produced an empty SBOM.spdx.json"

# ----------------------------------------------------------- checksums

step "writing and verifying SHA256SUMS"

# Every artifact, the SBOM included -- the release signs `SHA256SUMS`
# and nothing else, so anything missing from this file is unsigned and
# unverifiable no matter how carefully it was published. Sorted, so the
# file's contents depend only on the set of artifacts.
#
# `sha256_write` is a shell function (it hides the sha256sum/shasum
# difference), so this is a `while read` loop rather than the workflow's
# `xargs sha256sum`: `xargs` can only run a program.
(
  cd "${staging}"
  : > SHA256SUMS
  find . -maxdepth 1 -type f \( -name '*.tar.gz' -o -name 'SBOM.spdx.json' \) -exec basename {} \; \
    | LC_ALL=C sort \
    | while IFS= read -r artifact; do
        sha256_write "${artifact}" >> SHA256SUMS
      done
) || fail "could not write SHA256SUMS"
[ -s "${staging}/SHA256SUMS" ] || fail "SHA256SUMS is empty"

expected_artifacts=2
actual_artifacts="$(wc -l < "${staging}/SHA256SUMS" | tr -d ' ')"
[ "${actual_artifacts}" = "${expected_artifacts}" ] ||
  fail "SHA256SUMS covers ${actual_artifacts} artifact(s), expected ${expected_artifacts} (${archive} and SBOM.spdx.json)"

grep -qF " ${archive}" "${staging}/SHA256SUMS" || fail "SHA256SUMS does not cover ${archive}"
grep -qF " SBOM.spdx.json" "${staging}/SHA256SUMS" || fail "SHA256SUMS does not cover SBOM.spdx.json"

# The check a downloader runs, run here against the file just written --
# so a checksum format this platform cannot verify fails now rather than
# in someone's install.
(cd "${staging}" && sha256_check SHA256SUMS) || fail "SHA256SUMS did not verify against the artifacts"
cat "${staging}/SHA256SUMS"

# Tamper detection, proving the check above is load-bearing rather than
# vacuous: a copy of the archive with one byte appended must be rejected
# by the same checksum line.
tamper="${staging}/tamper"
mkdir -p "${tamper}"
cp "${staging}/${archive}" "${tamper}/"
printf '\0' >> "${tamper}/${archive}"
grep -F " ${archive}" "${staging}/SHA256SUMS" > "${tamper}/SHA256SUMS"
if (cd "${tamper}" && sha256_check SHA256SUMS >/dev/null 2>&1); then
  fail "a modified ${archive} still passed its SHA256SUMS entry"
fi
rm -rf "${tamper}"
echo "verify-release: a one-byte modification of ${archive} is rejected by SHA256SUMS"

# ---------------------------------------------------------- smoke test

step "smoke-testing the packaged binary"

# From the tarball, not from `target/`: what is verified has to be what
# would be downloaded, unpacked, and put on a PATH.
unpacked="${staging}/unpacked"
mkdir -p "${unpacked}"
tar --extract --gzip --file "${staging}/${archive}" --directory "${unpacked}"

for expected in "${name}/admissionlab" "${name}/LICENSE" "${name}/README.md"; do
  [ -f "${unpacked}/${expected}" ] || fail "the archive is missing ${expected}"
done
[ -x "${unpacked}/${name}/admissionlab" ] || fail "the unpacked binary is not executable"

smoke="${unpacked}/${name}/admissionlab"

reported="$("${smoke}" --version)" || fail "'admissionlab --version' exited non-zero"
[ "${reported}" = "admissionlab ${version}" ] ||
  fail "the packaged binary reports '${reported}', but the manifest says ${version}"
echo "verify-release: --version: ${reported}"

# `doctor` and `test` are the two commands a fresh install runs first
# (`docs/install.md`), and `--help` exercises argument parsing without
# touching Docker, `kind`, or the network -- so this stays a packaging
# check and never becomes a lab run.
for subcommand in doctor test; do
  "${smoke}" "${subcommand}" --help >/dev/null || fail "'admissionlab ${subcommand} --help' exited non-zero"
  echo "verify-release: ${subcommand} --help: exit 0"
done

# --------------------------------------------------------- sbom sanity

step "checking SBOM.spdx.json"

grep -qF '"spdxVersion": "SPDX-2.3"' "${sbom}" || fail "SBOM.spdx.json does not declare SPDX-2.3"
grep -qF "Tool: cargo-sbom-v${CARGO_SBOM_VERSION}" "${sbom}" ||
  fail "SBOM.spdx.json was not produced by cargo-sbom ${CARGO_SBOM_VERSION}"
grep -qF "\"SPDXRef-Package-${RELEASE_PACKAGE}-${version}\"" "${sbom}" ||
  fail "SBOM.spdx.json does not describe ${RELEASE_PACKAGE} ${version}"

sbom_packages="$(grep -c '"SPDXID": "SPDXRef-Package-' "${sbom}" || true)"
[ "${sbom_packages}" -ge "${MIN_SBOM_PACKAGES}" ] ||
  fail "SBOM.spdx.json lists ${sbom_packages} package(s), fewer than the ${MIN_SBOM_PACKAGES} a real build of ${RELEASE_PACKAGE} reaches"
echo "verify-release: SPDX-2.3, ${sbom_packages} packages, ${RELEASE_PACKAGE} ${version} present"

# ---------------------------------------------------------------- pass

step "PASS"
echo "verify-release: ${archive}"
echo "verify-release: SBOM.spdx.json (${sbom_packages} packages)"
echo "verify-release: SHA256SUMS covers both and verifies"
echo "verify-release: the packaged binary runs and reports ${version}"
