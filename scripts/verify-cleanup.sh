#!/usr/bin/env bash
#
# Phase 1 exit gate: loops `kind` cluster create/delete this many times
# and fails loudly if any `adlab-*` cluster is still present at any
# point — the guarantee Task 1.10's `LabRunner` orchestration exists to
# enforce for real lab runs (PRODUCT.md §33: "no leaked cluster after
# normal failure paths").
#
# Usage:
#   ./scripts/verify-cleanup.sh <iterations>
#
# For normal CI cost, run 10 iterations on PR/release candidates and 100
# iterations manually/nightly before Public Alpha. Measured on the
# reference machine, one create/delete cycle takes roughly 32 seconds
# with the node image already pulled locally (roughly 105 seconds on a
# cold first pull) — the same `kind create cluster`/`kind delete
# cluster` commands `KindClusterManager` itself issues, with no `--wait`
# flag, so these timings stay comparable to
# `admissionlab_cluster::kind::CREATE_TIMEOUT`'s own measurement. 100
# iterations is therefore roughly 53 minutes warm, so this script prints
# progress after every iteration rather than running silently for that
# long.
#
# Exits non-zero and prints a line starting with "verify-cleanup: FAIL"
# the moment any problem is detected: a bad argument, a missing `kind`/
# `docker`, a failed create or delete, or a surviving `adlab-*` cluster.

set -euo pipefail

# Pinned node image for the primary supported Kubernetes version (see
# `compatibility/kubernetes.yaml`'s `1.36.4` entry) — the exact digest
# this project has validated `kind` against, never a floating tag.
readonly NODE_IMAGE="kindest/node:v1.36.4@sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed"

usage() {
  echo "usage: $0 <iterations>" >&2
}

if [ "$#" -ne 1 ]; then
  usage
  exit 2
fi

iterations="$1"

case "${iterations}" in
  '' | *[!0-9]*)
    echo "verify-cleanup: error: <iterations> must be a positive integer, got: '${iterations}'" >&2
    usage
    exit 2
    ;;
esac

if [ "${iterations}" -lt 1 ]; then
  echo "verify-cleanup: error: <iterations> must be at least 1, got: ${iterations}" >&2
  exit 2
fi

for tool in kind docker; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "verify-cleanup: error: '${tool}' is not on PATH" >&2
    exit 3
  fi
done

# Counts how many `adlab-*` clusters `kind` currently reports. Never
# lets a "found none" result (grep's own non-zero exit when nothing
# matches) trip `set -e`/`pipefail` — that is the expected, passing case
# on every iteration.
count_adlab_clusters() {
  kind get clusters 2>/dev/null | grep -c '^adlab-' || true
}

fail() {
  echo "verify-cleanup: FAIL — $1" >&2
  exit 1
}

echo "verify-cleanup: starting ${iterations} iteration(s), node image ${NODE_IMAGE}"

start_epoch="$(date +%s)"
iteration=1
while [ "${iteration}" -le "${iterations}" ]; do
  cluster_name="adlab-verify-cleanup-${iteration}"
  iteration_start="$(date +%s)"

  echo "verify-cleanup: [${iteration}/${iterations}] creating cluster '${cluster_name}'..."
  if ! kind create cluster --name "${cluster_name}" --image "${NODE_IMAGE}"; then
    kind delete cluster --name "${cluster_name}" >/dev/null 2>&1 || true
    fail "kind create cluster failed on iteration ${iteration}"
  fi

  echo "verify-cleanup: [${iteration}/${iterations}] deleting cluster '${cluster_name}'..."
  if ! kind delete cluster --name "${cluster_name}"; then
    fail "kind delete cluster failed on iteration ${iteration}"
  fi

  remaining="$(count_adlab_clusters)"
  if [ "${remaining}" -ne 0 ]; then
    echo "verify-cleanup: surviving adlab-* cluster(s):" >&2
    kind get clusters 2>/dev/null | grep '^adlab-' >&2 || true
    fail "${remaining} adlab-* cluster(s) still present after iteration ${iteration}"
  fi

  iteration_end="$(date +%s)"
  echo "verify-cleanup: [${iteration}/${iterations}] ok, $(( iteration_end - iteration_start ))s, no adlab-* cluster remains"

  iteration=$(( iteration + 1 ))
done

end_epoch="$(date +%s)"
echo "verify-cleanup: PASS — ${iterations} create/delete cycle(s) completed in $(( end_epoch - start_epoch ))s total, no adlab-* cluster ever survived an iteration"
