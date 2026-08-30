#!/usr/bin/env bash
#
# Builds the `admissionlab-test-webhook` container image (Task 2.7 --
# PRODUCT.md §30's deterministic dogfood admission webhook,
# `crates/admissionlab-test-webhook`) and loads it into one or more
# already-running `kind` clusters, so `recipes/test-webhook/recipe.yaml`'s
# `imagePullPolicy: IfNotPresent` Deployment can start without ever
# reaching a real registry (Task 1's own `kind`-cluster isolation
# guarantee: nothing this project installs depends on network access to
# a container registry at test time).
#
# Usage:
#   ./scripts/build-test-images.sh <kind-cluster-name> [<kind-cluster-name> ...]
#
# Each named cluster must already exist (`kind create cluster` having
# already run) -- this script only builds and loads an image, the same
# division of responsibility `admissionlab-cluster::KindClusterManager`
# itself keeps between cluster lifecycle and everything installed inside
# one.
#
# `IMAGE_TAG` below must match `recipes/test-webhook/manifests/30-deployment.yaml`'s
# `image:` field exactly -- both are hardcoded, cross-referencing
# comments in each file, since a shell script and a YAML manifest have
# no mechanism to share one literal.
#
# Exits non-zero and prints a line starting with "build-test-images:
# error" the moment a real problem is found: a missing tool, a missing
# Dockerfile, a failed build, or a failed load into any named cluster
# (this script does not stop at the first cluster's failure -- it
# attempts every named cluster and reports every failure it saw, so one
# broken cluster name in a multi-cluster invocation does not hide a
# second one).

set -euo pipefail

usage() {
  echo "usage: $0 <kind-cluster-name> [<kind-cluster-name> ...]" >&2
}

if [ "$#" -lt 1 ]; then
  usage
  exit 2
fi

# Cross-referenced in recipes/test-webhook/manifests/30-deployment.yaml's
# own comment -- keep both in sync by hand if this ever changes.
readonly IMAGE_TAG="admissionlab-test-webhook:dev"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly script_dir
repo_root="$(cd "${script_dir}/.." && pwd)"
readonly repo_root
dockerfile="${repo_root}/crates/admissionlab-test-webhook/Dockerfile"
readonly dockerfile

fail() {
  echo "build-test-images: error: $1" >&2
  exit 1
}

for tool in docker kind; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    fail "'${tool}' is not on PATH"
  fi
done

if [ ! -f "${dockerfile}" ]; then
  fail "Dockerfile not found at ${dockerfile}"
fi

echo "build-test-images: building ${IMAGE_TAG} from ${dockerfile} (context: ${repo_root})"
if ! docker build --file "${dockerfile}" --tag "${IMAGE_TAG}" "${repo_root}"; then
  fail "docker build failed for ${IMAGE_TAG}"
fi
echo "build-test-images: built ${IMAGE_TAG}"

failed_clusters=()
for cluster_name in "$@"; do
  echo "build-test-images: loading ${IMAGE_TAG} into kind cluster '${cluster_name}'"
  if kind load docker-image "${IMAGE_TAG}" --name "${cluster_name}"; then
    echo "build-test-images: loaded ${IMAGE_TAG} into '${cluster_name}'"
  else
    echo "build-test-images: error: failed to load ${IMAGE_TAG} into '${cluster_name}'" >&2
    failed_clusters+=("${cluster_name}")
  fi
done

if [ "${#failed_clusters[@]}" -ne 0 ]; then
  fail "kind load docker-image failed for: ${failed_clusters[*]}"
fi

echo "build-test-images: done -- ${IMAGE_TAG} is available in: $*"
