#!/usr/bin/env bash
#
# Builds Admission Lab's own test-only container images and loads them
# into one or more already-running `kind` clusters, so the manifests
# that reference them (`imagePullPolicy: IfNotPresent`) can start
# without ever reaching a real registry (Task 1's own `kind`-cluster
# isolation guarantee: nothing this project installs depends on network
# access to a container registry at test time).
#
# Two images, both of them workloads Admission Lab runs *against
# itself* so its own test suite does not depend entirely on external
# vendor behavior:
#
#   admissionlab-test-webhook:dev  Task 2.7 / 3.9 -- PRODUCT.md §30's
#                                  deterministic dogfood admission
#                                  webhook (`crates/admissionlab-test-webhook`),
#                                  referenced by
#                                  `recipes/test-webhook/manifests/30-deployment.yaml`.
#   admissionlab-echo:dev          Task 6.5 -- the deterministic HTTP
#                                  echo backend Gateway data-plane
#                                  comparisons route traffic to
#                                  (`crates/admissionlab-echo`),
#                                  referenced by
#                                  `fixtures/gateway/backends/echo-a.yaml`
#                                  and `echo-b.yaml`.
#
# Usage:
#   ./scripts/build-test-images.sh [<kind-cluster-name> ...]
#
# Each named cluster must already exist (`kind create cluster` having
# already run) -- this script only builds and loads images, the same
# division of responsibility `admissionlab-cluster::KindClusterManager`
# itself keeps between cluster lifecycle and everything installed inside
# one.
#
# With NO cluster names, both images are built and nothing is loaded.
# That mode exists for the case `admissionlab test` itself creates the
# clusters mid-run (ROADMAP Task 6.11): the cluster names are not known
# until the run is already underway, and the run side-loads the images
# itself from the local Docker store through its configuration's
# `images:` list (`admissionlab_spec::EnvironmentSpec::images`). What a
# caller must still do beforehand is put the images *in* that store,
# which is exactly what this mode does. See
# `crates/admissionlab-cli/tests/gateway_e2e.rs`, which uses it.
#
# Both images are always built and always loaded, rather than taking an
# image name as an argument: a cluster that has one but not the other
# fails later, in the middle of a run, with an `ErrImageNeverPull` that
# reads as a fixture problem rather than as a missing build step. The
# marginal cost is a cached-layer rebuild.
#
# Each `IMAGE_TAG` below must match the `image:` field of the manifest
# that references it exactly -- both are hardcoded, cross-referencing
# comments in each file, since a shell script and a YAML manifest have
# no mechanism to share one literal.
#
# Exits non-zero and prints a line starting with "build-test-images:
# error" the moment a real problem is found: a missing tool, a missing
# Dockerfile, a failed build, or a failed load into any named cluster
# (this script does not stop at the first failed *load* -- it attempts
# every image on every named cluster and reports every failure it saw,
# so one broken cluster name in a multi-cluster invocation does not hide
# a second one. A failed *build* is fatal immediately: there is nothing
# to load).

set -euo pipefail

usage() {
  echo "usage: $0 [<kind-cluster-name> ...]" >&2
  echo "  with no cluster names, both images are built and none is loaded" >&2
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  usage
  exit 0
fi

# Each entry is "<image tag>|<Dockerfile path, relative to the repo
# root>". Cross-referenced in the manifests named in this script's own
# header comment -- keep both sides in sync by hand if either changes.
readonly IMAGES=(
  "admissionlab-test-webhook:dev|crates/admissionlab-test-webhook/Dockerfile"
  "admissionlab-echo:dev|crates/admissionlab-echo/Dockerfile"
)

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly script_dir
repo_root="$(cd "${script_dir}/.." && pwd)"
readonly repo_root

fail() {
  echo "build-test-images: error: $1" >&2
  exit 1
}

for tool in docker kind; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    fail "'${tool}' is not on PATH"
  fi
done

# Every Dockerfile is checked before anything is built, so a typo in the
# second entry is reported before the first image's (slow) build rather
# than after it.
for image in "${IMAGES[@]}"; do
  dockerfile="${repo_root}/${image#*|}"
  if [ ! -f "${dockerfile}" ]; then
    fail "Dockerfile not found at ${dockerfile}"
  fi
done

built_tags=()
for image in "${IMAGES[@]}"; do
  image_tag="${image%%|*}"
  dockerfile="${repo_root}/${image#*|}"
  echo "build-test-images: building ${image_tag} from ${dockerfile} (context: ${repo_root})"
  # The build context is the repository root for both images: each
  # Dockerfile copies the whole `crates/` tree because Cargo cannot
  # resolve this workspace without every member's manifest present.
  if ! docker build --file "${dockerfile}" --tag "${image_tag}" "${repo_root}"; then
    fail "docker build failed for ${image_tag}"
  fi
  echo "build-test-images: built ${image_tag}"
  built_tags+=("${image_tag}")
done

if [ "$#" -eq 0 ]; then
  echo "build-test-images: done -- ${built_tags[*]} are in the local image store; no cluster was named, so nothing was loaded"
  exit 0
fi

failures=()
for cluster_name in "$@"; do
  for image in "${IMAGES[@]}"; do
    image_tag="${image%%|*}"
    echo "build-test-images: loading ${image_tag} into kind cluster '${cluster_name}'"
    if kind load docker-image "${image_tag}" --name "${cluster_name}"; then
      echo "build-test-images: loaded ${image_tag} into '${cluster_name}'"
    else
      echo "build-test-images: error: failed to load ${image_tag} into '${cluster_name}'" >&2
      failures+=("${image_tag} -> ${cluster_name}")
    fi
  done
done

if [ "${#failures[@]}" -ne 0 ]; then
  fail "kind load docker-image failed for: ${failures[*]}"
fi

echo "build-test-images: done -- ${built_tags[*]} are available in: $*"
