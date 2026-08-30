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
# `docker`, a failed create or delete, a surviving `adlab-*` cluster, or
# `kind get clusters` itself failing (never conflated with "zero
# clusters" -- see `fetch_clusters` below).
#
# Scope: this validates exactly one thing -- single-cluster, sequential
# create/delete never leaks. Each iteration creates and deletes ONE
# cluster before the next iteration starts; it never runs two deletes
# concurrently, so it does NOT exercise (and cannot regress-test) the
# concurrent-teardown kubeconfig-lock race commit 4b8241b fixed. That
# path is covered elsewhere: at the argv level by
# `admissionlab-cluster`'s `tests/lifecycle_unit.rs::concurrent_deletes_each_carry_their_own_kubeconfig_path`
# (two concurrent deletes against a fake `ProcessRunner`, asserting both
# carry their own `--kubeconfig`), and at the real-OS level by the
# repeated real `admissionlab test` runs recorded in
# `.superpowers/sdd/ROADMAP/task-1.10-report.md`. A PASS here says
# nothing about concurrent teardown.

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

fail() {
  echo "verify-cleanup: FAIL — $1" >&2
  exit 1
}

# Fetches `kind get clusters`' raw output, failing loudly through
# `fail` (never silently) if the command itself fails. This is the
# fix for a real defect: `kind get clusters | grep -c '^adlab-' ||
# true` prints "0" whether `kind` genuinely reported zero clusters OR
# `kind` itself failed (a transient Docker hiccup, a timeout, an
# output-format change) and produced no output at all -- both cases
# are indistinguishable once `2>/dev/null` discards stderr and
# `|| true` neutralizes the pipeline's exit status. Over an unattended
# ~53-minute, 100-iteration run, that failure mode is realistic, not
# hypothetical, and it is exactly the kind of leak this script exists
# to catch -- a check that cannot fail is worse than no check, because
# it reads as coverage. Fetching once here and deriving both the count
# and (on a real leak) the diagnostic listing from this same captured
# output also means there is only ever one `kind get clusters`
# invocation per iteration to reason about, not two.
fetch_clusters() {
  local output
  local status=0
  output="$(kind get clusters 2>&1)" || status=$?
  if [ "${status}" -ne 0 ]; then
    fail "kind get clusters failed (exit ${status}): ${output}"
  fi
  printf '%s\n' "${output}"
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

  cluster_list="$(fetch_clusters)"
  remaining="$(printf '%s\n' "${cluster_list}" | grep -c '^adlab-' || true)"
  if [ "${remaining}" -ne 0 ]; then
    echo "verify-cleanup: surviving adlab-* cluster(s):" >&2
    printf '%s\n' "${cluster_list}" | grep '^adlab-' >&2 || true
    fail "${remaining} adlab-* cluster(s) still present after iteration ${iteration}"
  fi

  iteration_end="$(date +%s)"
  echo "verify-cleanup: [${iteration}/${iterations}] ok, $(( iteration_end - iteration_start ))s, no adlab-* cluster remains"

  iteration=$(( iteration + 1 ))
done

end_epoch="$(date +%s)"
echo "verify-cleanup: PASS — ${iterations} create/delete cycle(s) completed in $(( end_epoch - start_epoch ))s total, no adlab-* cluster ever survived an iteration"
