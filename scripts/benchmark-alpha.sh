#!/usr/bin/env bash
#
# The Alpha performance benchmark (ROADMAP Task 5.7 step 2): one real
# `admissionlab test` run over 100 Pod fixtures, on two real `kind`
# clusters, with the per-stage timings the run recorded printed as a
# stable table.
#
# Usage:
#   ./scripts/benchmark-alpha.sh
#
# It builds `admissionlab` in release mode (or uses `ADMISSIONLAB_BIN`),
# generates a throwaway 100-fixture lab under a temporary directory, runs
# it, prints the table, deletes the lab, and verifies no `adlab-*` cluster
# survived.
#
# # This script reports; it does not judge
#
# ROADMAP Task 5.7 step 4 is explicit: "do not make wall-clock kind
# target a flaky PR assertion; report trend and enforce only egregious
# regressions in scheduled CI". So there is no wall-clock assertion here
# by default, and the exit status says only whether the *run* worked:
#
#   0  the run produced a result (whether the policy passed or failed)
#   1  something about this benchmark or the run itself failed
#
# A policy `fail` is deliberately not a benchmark failure. This lab
# compares two identical stacks and should report no differences, but if
# a Kubernetes release ever starts defaulting a Pod field
# nondeterministically, that is a finding about the product -- printed,
# and worth investigating -- not a reason to lose the measurement that
# was the point of running.
#
# The enforcement hook for scheduled CI is one environment variable:
#
#   ADMISSIONLAB_BENCH_MAX_CAPTURE_SECONDS
#       When set to a number, the slower side's fixture-capture duration
#       must be at or under it, or this script exits 1 with the measured
#       value. Unset (the default) it asserts nothing. Set it in a
#       scheduled job, generously -- it exists to catch a tenfold
#       regression, not a ten-percent one.
#
#   ADMISSIONLAB_BENCH_FIXTURES
#       How many Pod fixtures to generate. Defaults to 100, which is the
#       number PRODUCT.md §33's targets are stated over; override it only
#       to explore a curve, and say which number you used when reporting
#       results.
#
#   ADMISSIONLAB_BIN
#       An already-built `admissionlab` to measure, instead of building
#       one. The binary must be a *release* build: a debug build measures
#       a program nobody ships.
#
# # The stack: two bare clusters plus one `manifests` component
#
# The cheapest stack that still exercises real admission capture, and the
# reason it is honest rather than merely cheap:
#
#   * Every fixture is a real server-side dry-run CREATE against a real
#     kube-apiserver, and every one of them runs the API server's whole
#     built-in admission plugin chain, is correlated against the real
#     audit log, and produces a real evidence bundle. That is the work
#     this benchmark exists to time, and installing Kyverno or Istio
#     would not add a single fixture to it -- it would add several
#     minutes of image pulls to the *install* stage, which is exactly the
#     stage PRODUCT.md §33's suite target excludes.
#   * No admission webhook is installed, so nothing is injected and the
#     two sides agree by construction. `examples/admission-basic` makes
#     the same trade for the same reason and says so at length.
#   * `recipes/test-webhook` was the alternative considered. It is
#     rejected here for a mechanical reason, not a philosophical one: its
#     image has to be `kind load`ed into each cluster *by name*, and this
#     script never learns those names -- `admissionlab test` creates and
#     deletes both clusters itself, with run-scoped names, inside the
#     process being measured. Loading it would mean keeping clusters
#     alive across two invocations, which is a different experiment.
#
# One `manifests` component *is* installed on each side, and it is not
# optional: it applies a namespace and a ServiceAccount synchronously.
# The in-tree ServiceAccount admission plugin resolves every Pod's
# `serviceAccountName` and rejects the request when the account does not
# exist, and a namespace's own `default` account is created
# asynchronously by a controller -- so a hundred Pod fixtures replayed
# seconds after the clusters come up would be racing that controller.
# `fixtures/core/alpha-corpus/00-setup.yaml` documents this in full; the
# same fix is applied here. Every fixture also carries
# `automountServiceAccountToken: false`, which stops the same plugin
# projecting a `kube-api-access-<random>` volume whose name differs on
# every admission.
#
# As a bonus the component makes the install stage non-empty, so the
# per-side and per-component install timings this script prints have
# something real in them.
#
# # The fixtures are generated, and that is not a Global Constraint 11
# # problem
#
# GC11 keeps *generated fuzz fixtures* out of the v1 product. What this
# script writes is neither fuzz nor a fixture corpus: it is one
# `FixtureMatrix` document (Task 5.10) with N hand-shaped cases that
# differ only in `metadata.name`, generated into a temporary directory,
# measured, and deleted. Nothing it writes is committed, nothing it
# writes is a test corpus, and no run of the product ever reads it. The
# alternative -- checking a hundred near-identical Pods into `fixtures/`
# -- would put a generated corpus in the repository, which is the thing
# GC11 is actually about.
#
# A matrix rather than a hundred files, because that is what a real
# 100-case corpus would be written as, so the discovery-and-expansion
# cost this benchmark measures is the cost a real user pays.

set -euo pipefail

# --------------------------------------------------------------------
# Configuration
# --------------------------------------------------------------------

# Kubernetes version for both sides. The Tier 1 primary supported
# version (`compatibility/kubernetes.yaml`), and the same on both sides
# so the node image is pulled once and reused -- a version-skew lab is a
# different measurement.
readonly KUBERNETES_VERSION="1.36.4"

fixture_count="${ADMISSIONLAB_BENCH_FIXTURES:-100}"
readonly fixture_count

fail() {
  echo "benchmark-alpha: error: $1" >&2
  exit 1
}

if ! [[ "${fixture_count}" =~ ^[0-9]+$ ]] || [ "${fixture_count}" -lt 1 ]; then
  fail "ADMISSIONLAB_BENCH_FIXTURES must be a positive integer, got '${fixture_count}'"
fi

max_capture_seconds="${ADMISSIONLAB_BENCH_MAX_CAPTURE_SECONDS:-}"
readonly max_capture_seconds
if [ -n "${max_capture_seconds}" ] && ! [[ "${max_capture_seconds}" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
  fail "ADMISSIONLAB_BENCH_MAX_CAPTURE_SECONDS must be a number, got '${max_capture_seconds}'"
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly script_dir
repo_root="$(cd "${script_dir}/.." && pwd)"
readonly repo_root

for tool in docker kind jq; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    fail "'${tool}' is not on PATH"
  fi
done

# --------------------------------------------------------------------
# The binary under measurement
# --------------------------------------------------------------------

if [ -n "${ADMISSIONLAB_BIN:-}" ]; then
  binary="${ADMISSIONLAB_BIN}"
  [ -x "${binary}" ] || fail "ADMISSIONLAB_BIN '${binary}' is not an executable file"
  echo "benchmark-alpha: measuring ${binary} (ADMISSIONLAB_BIN)"
else
  echo "benchmark-alpha: building admissionlab (release)"
  (cd "${repo_root}" && cargo build --release -p admissionlab-cli --bin admissionlab) \
    || fail "cargo build --release failed"
  binary="${repo_root}/target/release/admissionlab"
  [ -x "${binary}" ] || fail "expected a release binary at ${binary}"
fi
readonly binary

# --------------------------------------------------------------------
# The throwaway lab
# --------------------------------------------------------------------

lab_dir="$(mktemp -d "${TMPDIR:-/tmp}/admissionlab-bench.XXXXXXXX")"
readonly lab_dir

cleanup() {
  rm -rf "${lab_dir}"
}
trap cleanup EXIT

mkdir -p "${lab_dir}/fixtures" "${lab_dir}/reports"

cat >"${lab_dir}/setup.yaml" <<'YAML'
# Applied for real (never replayed): a dry-run CREATE persists nothing,
# so a namespace that only ever existed as one would not be there when
# the pods are replayed into it.
apiVersion: v1
kind: Namespace
metadata:
  name: admissionlab-bench
  labels:
    app.kubernetes.io/part-of: admission-lab
---
# Created synchronously by the same `kubectl apply` as the namespace, so
# no fixture races the controller that would otherwise create the
# namespace's `default` account. See this script's header.
apiVersion: v1
kind: ServiceAccount
metadata:
  name: bench-runner
  namespace: admissionlab-bench
automountServiceAccountToken: false
YAML

cat >"${lab_dir}/admissionlab.yaml" <<YAML
# Generated by scripts/benchmark-alpha.sh. Not a checked-in example: this
# file exists for the length of one benchmark run.
apiVersion: admissionlab.io/v1alpha1
kind: Lab

baseline:
  kubernetes: "${KUBERNETES_VERSION}"
  components:
    - name: bench-setup
      version: "1"
      install:
        type: manifests
        paths:
          - setup.yaml

candidate:
  kubernetes: "${KUBERNETES_VERSION}"
  components:
    - name: bench-setup
      version: "1"
      install:
        type: manifests
        paths:
          - setup.yaml

fixtures:
  # Selects the matrix document and nothing else. Deliberately not
  # "fixtures/*.yaml": \`globset\`'s \`*\` matches a path separator, and the
  # matrix base beside it must not be replayed as a fixture in its own
  # right or this run would compare N+1 objects while reporting N.
  include:
    - "fixtures/*.matrix.yaml"
YAML

cat >"${lab_dir}/fixtures/pod-base.yaml" <<'YAML'
# The base every generated case varies. A complete, valid Pod:
# `automountServiceAccountToken: false` and a ServiceAccount that
# `setup.yaml` created are both load-bearing (see this script's header).
apiVersion: v1
kind: Pod
metadata:
  name: bench-pod
  namespace: admissionlab-bench
spec:
  serviceAccountName: bench-runner
  automountServiceAccountToken: false
  containers:
    - name: app
      image: registry.k8s.io/pause:3.10
      resources:
        requests:
          cpu: 10m
          memory: 16Mi
YAML

{
  echo "# Generated by scripts/benchmark-alpha.sh: ${fixture_count} cases,"
  echo "# each varying only \`metadata.name\` so every fixture is a distinct,"
  echo "# recognizable object in the report."
  echo "apiVersion: admissionlab.io/v1alpha1"
  echo "kind: FixtureMatrix"
  echo "spec:"
  echo "  id: bench"
  echo "  base: pod-base.yaml"
  echo "  cases:"
  index=0
  while [ "${index}" -lt "${fixture_count}" ]; do
    printf -v padded '%03d' "${index}"
    echo "    - id: pod-${padded}"
    echo "      patches:"
    echo "        - op: replace"
    echo "          path: /metadata/name"
    echo "          value: bench-pod-${padded}"
    index=$((index + 1))
  done
} >"${lab_dir}/fixtures/pods.matrix.yaml"

echo "benchmark-alpha: generated a ${fixture_count}-fixture lab in ${lab_dir}"

# --------------------------------------------------------------------
# The run
# --------------------------------------------------------------------

readonly log="${lab_dir}/run.log"
readonly result="${lab_dir}/reports/result.json"

echo "benchmark-alpha: running (this creates two kind clusters; expect several minutes)"
started_ns="$(date +%s%N)"
set +e
"${binary}" test "${lab_dir}/admissionlab.yaml" --report-dir "${lab_dir}/reports" 2>&1 | tee "${log}"
status="${PIPESTATUS[0]}"
set -e
finished_ns="$(date +%s%N)"
readonly status

case "${status}" in
  0) verdict="pass" ;;
  1) verdict="fail (the policy found differences; the measurement is still valid)" ;;
  *) fail "admissionlab test exited ${status}; see the output above. No measurement was taken." ;;
esac
readonly verdict

[ -f "${result}" ] || fail "the run exited ${status} but wrote no ${result}"

total_seconds="$(awk "BEGIN { printf \"%.2f\", (${finished_ns} - ${started_ns}) / 1000000000 }")"
readonly total_seconds

# --------------------------------------------------------------------
# The table
# --------------------------------------------------------------------

# One jq pass, so every number below comes from the same read of the same
# document. Absent stages print "-" rather than 0: the recorder omits a
# stage it did not measure, and this script must not fill that in (Global
# Constraint 15).
read -r clusters_wall clusters_baseline clusters_candidate \
  install_wall install_baseline install_candidate \
  capture_wall capture_baseline capture_candidate capture_fixtures \
  comparison_ms elapsed_ms <<EOF
$(jq -r '
  def ms: if . == null then "-" else (. / 1000 | tostring) end;
  .timings as $t
  | [
      ($t.clusterCreation.wallMs | ms),
      ($t.clusterCreation.baselineMs | ms),
      ($t.clusterCreation.candidateMs | ms),
      ($t.installation.wallMs | ms),
      ($t.installation.baseline.elapsedMs | ms),
      ($t.installation.candidate.elapsedMs | ms),
      ($t.fixtureCapture.wallMs | ms),
      ($t.fixtureCapture.baselineMs | ms),
      ($t.fixtureCapture.candidateMs | ms),
      ($t.fixtureCapture.fixtures // "-" | tostring),
      ($t.comparisonMs | ms),
      ($t.elapsedMs | ms)
    ]
  | @tsv
' "${result}")
EOF

# The slower side is the one a serial-versus-parallel decision (ROADMAP
# Task 5.8) is made against: both sides run concurrently, so the run
# waits for whichever is slower.
capture_slower="$(awk "BEGIN {
  b = \"${capture_baseline}\"; c = \"${capture_candidate}\";
  if (b == \"-\" && c == \"-\") { print \"-\" }
  else if (b == \"-\") { print c }
  else if (c == \"-\") { print b }
  else { print (b + 0 > c + 0) ? b : c }
}")"
readonly capture_slower

if [ "${capture_slower}" != "-" ] && [ "${capture_fixtures}" != "-" ]; then
  per_fixture="$(awk "BEGIN { printf \"%.3f\", ${capture_slower} / ${capture_fixtures} }")"
else
  per_fixture="-"
fi
readonly per_fixture

# A seconds value at a fixed three decimals, or "-" left alone. Every
# number in the table goes through this, so a stage that reports 0.000 is
# visibly a measured zero and a stage that reports "-" is visibly absent.
secs() {
  if [ "$1" = "-" ] || [ -z "$1" ]; then
    printf '%s' "$1"
  else
    awk "BEGIN { printf \"%.3f\", $1 }"
  fi
}

row() {
  printf '  %-22s %12s %12s %12s\n' \
    "$1" "$(secs "$2")" "$(secs "$3")" "$(secs "$4")"
}

echo
echo "benchmark-alpha: results"
echo "  binary                 ${binary}"
echo "  kubernetes             ${KUBERNETES_VERSION} (both sides)"
echo "  fixtures               ${fixture_count} per side"
echo "  verdict                ${verdict} (exit ${status})"
echo
printf '  %-22s %12s %12s %12s\n' "stage" "wall(s)" "baseline(s)" "candidate(s)"
printf '  %-22s %12s %12s %12s\n' "----------------------" "------------" "------------" "------------"
row "cluster creation" "${clusters_wall}" "${clusters_baseline}" "${clusters_candidate}"
row "installation" "${install_wall}" "${install_baseline}" "${install_candidate}"
row "fixture capture" "${capture_wall}" "${capture_baseline}" "${capture_candidate}"
row "comparison" "${comparison_ms}" "" ""
echo
echo "  per-fixture capture    ${per_fixture}s (slower side / ${capture_fixtures} fixtures)"
echo "  result.json elapsed    $(secs "${elapsed_ms}")s (run start to the moment result.json was assembled)"
echo "  admissionlab test      ${total_seconds}s total wall-clock, measured by this script"
echo

# `reportingMs` and `cleanup` cannot be inside `result.json` -- the
# document is written during the first and before the second. The run
# prints them itself, on one line, after cleanup.
if grep -q 'stage timings:' "${log}"; then
  echo "  the run's own final line (the only place reporting and cleanup appear):"
  echo "    $(grep 'stage timings:' "${log}" | tail -n 1 | sed 's/^admissionlab: //')"
  echo
fi

# Per-component install breakdown, when there is one.
jq -r '
  .timings.installation as $i
  | [["baseline", $i.baseline], ["candidate", $i.candidate]]
  | map(select(.[1] != null and .[1].components != null))
  | .[]
  | .[0] as $side
  | .[1].components[]
  | "  install component      \($side) \(.name) \(.elapsedMs / 1000)s"
' "${result}"
echo

# --------------------------------------------------------------------
# Optional enforcement, and cleanup verification
# --------------------------------------------------------------------

leaked="$(kind get clusters 2>/dev/null | grep '^adlab-' || true)"
if [ -n "${leaked}" ]; then
  echo "benchmark-alpha: WARNING: clusters survived the run:" >&2
  echo "${leaked}" >&2
  fail "the run leaked a cluster (PRODUCT.md §33); delete it with: kind delete cluster --name <name>"
fi
echo "benchmark-alpha: no adlab-* cluster survived."

if [ -n "${max_capture_seconds}" ]; then
  if [ "${capture_slower}" = "-" ]; then
    fail "ADMISSIONLAB_BENCH_MAX_CAPTURE_SECONDS is set but the run recorded no capture duration"
  fi
  over="$(awk "BEGIN { print (${capture_slower} > ${max_capture_seconds}) ? 1 : 0 }")"
  if [ "${over}" = "1" ]; then
    fail "fixture capture took ${capture_slower}s, over the ADMISSIONLAB_BENCH_MAX_CAPTURE_SECONDS=${max_capture_seconds} ceiling"
  fi
  echo "benchmark-alpha: capture ${capture_slower}s is within the ${max_capture_seconds}s ceiling."
fi

echo "benchmark-alpha: done."
