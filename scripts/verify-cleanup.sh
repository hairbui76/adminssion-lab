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
#   ./scripts/verify-cleanup.sh --check-only
#   ./scripts/verify-cleanup.sh --after-interrupt
#
# `--check-only` runs no cluster lifecycle at all: it asserts, once, that
# no `adlab-*` cluster is present right now, and exits. It exists so that
# "this job leaked nothing" is one command with one implementation
# instead of an eight-line `kind get clusters | grep` snippet copied into
# the tail of every CI job -- and copied, in practice, with the
# `2>/dev/null ... || true` defect `fetch_clusters` below exists to
# avoid. `.github/workflows/nightly.yml` ends every one of its jobs with
# it (ROADMAP Task 5.9 step 1).
#
# `--check-only` deliberately does NOT delete what it finds. A leaked
# cluster is evidence, and a check that tidies away its own evidence
# makes the next question -- what state was it left in? -- unanswerable.
# It prints the exact `kind delete cluster --name` command for each
# instead. (`.github/workflows/integration.yml`'s inline guard does
# best-effort delete, because there the leak is one matrix entry's and
# the runner VM is shared with nothing; a nightly job that fails here
# should be diagnosable.)
#
# `--after-interrupt` is the deliberate opposite of `--check-only`, and it
# exists for exactly one path: a run an operator interrupted *twice*.
# `admissionlab` cancels cooperatively on the first `SIGINT`/`SIGTERM` and
# tears everything down itself; a second one is the operator saying they
# are not waiting, and the process exits without unwinding, printing the
# `kind delete cluster` commands for whatever it is abandoning (ROADMAP
# Task 9.6). So after a forced exit a surviving cluster is *expected*
# rather than a leak, "assert nothing is there" is the wrong question, and
# preserving the evidence would only leave a container running.
#
# This mode therefore sweeps: it lists every surviving `adlab-*` cluster,
# deletes each one, and says what it removed (or that there was nothing).
# It exits 0 whether or not it found anything — finding a cluster is not a
# failure here — and non-zero only if a delete itself failed, which is the
# one case where a human still has work to do.
#
# It is NOT a substitute for `--check-only` anywhere else. A normal or
# failed or singly-interrupted run leaking a cluster is a real defect, and
# sweeping one away silently is precisely how such a defect stays
# undiscovered.
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
  echo "       $0 --check-only" >&2
  echo "       $0 --after-interrupt" >&2
}

if [ "$#" -ne 1 ]; then
  usage
  exit 2
fi

# `--check-only` takes the place of <iterations>; the two modes are
# mutually exclusive by construction because there is exactly one
# argument. `check_only` is resolved here, before the numeric validation
# below, so that `--check-only` never has to survive being parsed as an
# integer.
check_only=false
after_interrupt=false
iterations=0
if [ "$1" = "--check-only" ]; then
  check_only=true
elif [ "$1" = "--after-interrupt" ]; then
  after_interrupt=true
else
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
fi
readonly check_only
readonly after_interrupt
readonly iterations

# `docker` is required by both modes even though `--check-only` never
# invokes it directly: `kind get clusters` enumerates Docker containers,
# so a missing or unreachable Docker daemon is exactly the case where
# `kind` produces no output and a naive check reports "no leaks". Failing
# here, with exit 3, keeps that indistinguishable-from-clean case out of
# the check's own result.
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

# Asserts that no `adlab-*` cluster exists right now, failing through
# `fail` (exit 1) if any does. `$1` says *when* the assertion was made,
# and lands verbatim in the failure message ("after iteration 7", "right
# now"), because "1 adlab-* cluster is still present" is only actionable
# alongside what had just finished running.
#
# On a real leak it prints the surviving names and the exact
# `kind delete cluster --name` command for each. It does not run them:
# see this script's header for why `--check-only` preserves the evidence
# rather than tidying it away.
assert_no_leaked_clusters() {
  local when="$1"
  local cluster_list
  local remaining

  cluster_list="$(fetch_clusters)"
  remaining="$(printf '%s\n' "${cluster_list}" | grep -c '^adlab-' || true)"
  if [ "${remaining}" -ne 0 ]; then
    echo "verify-cleanup: surviving adlab-* cluster(s):" >&2
    printf '%s\n' "${cluster_list}" | grep '^adlab-' >&2 || true
    echo "verify-cleanup: delete them with:" >&2
    printf '%s\n' "${cluster_list}" | grep '^adlab-' \
      | sed 's/^/  kind delete cluster --name /' >&2 || true
    fail "${remaining} adlab-* cluster(s) still present ${when}"
  fi
}

# How hard the sweep tries to delete one cluster, and how long it waits
# between attempts.
#
# Retries are not defensive padding here, they are the actual shape of
# this situation: the forced exit this mode cleans up after can land in
# the middle of the run's own `kind delete`, leaving the Docker daemon
# still removing a node container. `kind delete` on that cluster fails
# with "removal of container ... is already in progress" until the daemon
# finishes, which takes seconds. Half a minute of attempts covers that
# comfortably; a single attempt fails a sweep for a cluster that was
# already on its way out.
readonly SWEEP_ATTEMPTS=6
readonly SWEEP_RETRY_SECONDS=5

# Whether `kind` still reports a cluster named `$1`.
cluster_is_present() {
  fetch_clusters | grep -Fxq -- "$1"
}

# Deletes one cluster, tolerating a removal that was already in flight.
# Returns non-zero only if the cluster is still there when the attempts
# run out — "somebody else deleted it" is a deleted cluster.
delete_cluster_with_retries() {
  local name="$1"
  local attempt=1

  while [ "${attempt}" -le "${SWEEP_ATTEMPTS}" ]; do
    if kind delete cluster --name "${name}"; then
      echo "verify-cleanup: deleted '${name}'"
      return 0
    fi
    if ! cluster_is_present "${name}"; then
      echo "verify-cleanup: '${name}' is gone; its removal was already in flight"
      return 0
    fi
    echo "verify-cleanup: '${name}' did not delete (attempt ${attempt}/${SWEEP_ATTEMPTS}); retrying in ${SWEEP_RETRY_SECONDS}s" >&2
    sleep "${SWEEP_RETRY_SECONDS}"
    attempt=$(( attempt + 1 ))
  done

  if cluster_is_present "${name}"; then
    return 1
  fi
  echo "verify-cleanup: '${name}' is gone after ${SWEEP_ATTEMPTS} attempt(s)"
  return 0
}

# Deletes every surviving `adlab-*` cluster and reports what it removed.
# See this script's header for why this mode deletes what `--check-only`
# deliberately preserves, and why finding a cluster here is not a failure.
sweep_after_interrupt() {
  local cluster_list
  local survivors
  local failed=0

  cluster_list="$(fetch_clusters)"
  survivors="$(printf '%s\n' "${cluster_list}" | grep '^adlab-' || true)"
  if [ -z "${survivors}" ]; then
    echo "verify-cleanup: PASS — no adlab-* cluster survived the interrupt; nothing to sweep"
    exit 0
  fi

  echo "verify-cleanup: sweeping cluster(s) left by an interrupted run:"
  printf '%s\n' "${survivors}" | sed 's/^/  /'
  while IFS= read -r cluster_name; do
    [ -n "${cluster_name}" ] || continue
    if ! delete_cluster_with_retries "${cluster_name}"; then
      echo "verify-cleanup: could not delete '${cluster_name}'" >&2
      failed=1
    fi
  done <<EOF
${survivors}
EOF

  if [ "${failed}" -ne 0 ]; then
    fail "at least one adlab-* cluster could not be deleted; delete it by hand"
  fi
  # Re-checked rather than assumed: a delete that reported success and
  # left the cluster present is the one outcome a sweep must not paper
  # over.
  assert_no_leaked_clusters "after sweeping an interrupted run"
  echo "verify-cleanup: PASS — swept every adlab-* cluster left by the interrupted run"
  exit 0
}

if [ "${check_only}" = "true" ]; then
  assert_no_leaked_clusters "right now"
  echo "verify-cleanup: PASS — no adlab-* cluster is present"
  exit 0
fi

if [ "${after_interrupt}" = "true" ]; then
  sweep_after_interrupt
fi

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

  assert_no_leaked_clusters "after iteration ${iteration}"

  iteration_end="$(date +%s)"
  echo "verify-cleanup: [${iteration}/${iterations}] ok, $(( iteration_end - iteration_start ))s, no adlab-* cluster remains"

  iteration=$(( iteration + 1 ))
done

end_epoch="$(date +%s)"
echo "verify-cleanup: PASS — ${iterations} create/delete cycle(s) completed in $(( end_epoch - start_epoch ))s total, no adlab-* cluster ever survived an iteration"
