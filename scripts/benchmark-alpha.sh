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
# # What this script asserts, and what it only reports
#
# ROADMAP Task 9.8 step 1 turns two of PRODUCT.md §33's numbers into
# enforced budgets rather than trend lines, and this script is where they
# are enforced:
#
#   * the fixture stage over 100 fixtures must finish within
#     approximately five minutes -- §33's "100-fixture admission suite
#     completes within approximately five minutes excluding component
#     installation under normal CI conditions"; and
#   * the semantic comparison of those 100 fixtures must finish in under
#     one second -- §33's "semantic comparison of 100 ordinary fixtures
#     in under one second after artifacts are collected".
#
# Both are asserted against the stage durations the run itself recorded
# in `result.json`, and either one exceeded exits 1 with the measured
# number. Task 5.7 measured 10.88-11.48s of fixture capture and 0.002s of
# comparison on the reference host (docs/architecture.md §6.2), so the
# ceilings sit roughly 26x and 500x above what was measured. They exist
# to catch a tenfold regression, not a ten-percent one: a run that trips
# one has found something worth looking at, not a busy afternoon.
#
# The third §33 number -- "typical `kind` cluster creation under
# approximately 90 seconds per cluster on a healthy CI runner" -- is
# REPORTED and never asserted. That is Task 9.8 step 2's own instruction
# ("a target, not a PR hard fail due to hosted-runner variance") and Task
# 5.7 step 4's before it ("do not make wall-clock kind target a flaky PR
# assertion"). Cluster creation is the stage most exposed to runner
# variance and the one this project controls least. What this script does
# with it instead is keep the samples: every cluster it creates appends
# its own creation time to a shared file that
# `scripts/benchmark-gateway.sh` appends to as well, and every run prints
# the median and p95 over everything accumulated there. See "Cluster
# creation samples" below.
#
# So the exit status says whether the *run* worked and whether the
# asserted budgets held:
#
#   0  the run produced a result (whether the policy passed or failed)
#      and both asserted budgets held
#   1  something about this benchmark, the run itself, or one of the two
#      asserted budgets failed
#
# A policy `fail` is deliberately not a benchmark failure. This lab
# compares two identical stacks and should report no differences, but if
# a Kubernetes release ever starts defaulting a Pod field
# nondeterministically, that is a finding about the product -- printed,
# and worth investigating -- not a reason to lose the measurement that
# was the point of running.
#
# # The knobs
#
#   ADMISSIONLAB_BENCH_MAX_CAPTURE_SECONDS
#       The fixture stage's ceiling, in seconds, measured against the
#       stage's own wall-clock (`timings.fixtureCapture.wallMs`) -- the
#       number that answers "how long did the fixture stage take", which
#       is what §33 budgets. Defaults to §33's five minutes for any
#       corpus of 100 fixtures or fewer, and to 3 seconds per fixture
#       above that. Never tightened below 300s: §33 states its number at
#       100 fixtures, and a 10-fixture exploration must not be held to a
#       30-second budget nobody measured or promised. Set this to raise
#       or lower the ceiling for a runner class whose real behavior you
#       know; it is not a switch that turns enforcement off.
#
#   ADMISSIONLAB_BENCH_MAX_COMPARISON_SECONDS
#       The comparison stage's ceiling, in seconds
#       (`timings.comparisonMs`), on exactly the same rule: §33's one
#       second for any corpus of 100 or fewer, 0.01 seconds per fixture
#       above that.
#
#   ADMISSIONLAB_BENCH_FIXTURES
#       How many Pod fixtures to generate. Defaults to 100, which is the
#       number PRODUCT.md §33's targets are stated over; override it only
#       to explore a curve, and say which number you used when reporting
#       results.
#
#   ADMISSIONLAB_BENCH_SAMPLE_FILE
#       Where per-cluster `kind` creation times accumulate, one sample
#       per line. Defaults to `target/benchmark/kind-create-seconds.tsv`
#       inside the repository (`target/` is git-ignored, so the samples
#       are local evidence and never committed). Point both benchmark
#       scripts at the same file -- which is the default -- and the
#       median and p95 each of them prints is taken over every cluster
#       either of them has ever created on this machine.
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

# The two enforced ceilings (ROADMAP Task 9.8 step 1), each stated at
# PRODUCT.md §33's own corpus size of 100 fixtures and scaled linearly
# only *upward* from there. `budget_scale` is therefore never below 1:
# see this script's header for why a smaller corpus is not held to a
# smaller budget.
budget_scale="$(awk "BEGIN { n = ${fixture_count} / 100; print (n > 1) ? n : 1 }")"
readonly budget_scale

max_capture_seconds="${ADMISSIONLAB_BENCH_MAX_CAPTURE_SECONDS:-$(awk "BEGIN { printf \"%.3f\", 300 * ${budget_scale} }")}"
readonly max_capture_seconds
if ! [[ "${max_capture_seconds}" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
  fail "ADMISSIONLAB_BENCH_MAX_CAPTURE_SECONDS must be a number, got '${max_capture_seconds}'"
fi

max_comparison_seconds="${ADMISSIONLAB_BENCH_MAX_COMPARISON_SECONDS:-$(awk "BEGIN { printf \"%.3f\", 1 * ${budget_scale} }")}"
readonly max_comparison_seconds
if ! [[ "${max_comparison_seconds}" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
  fail "ADMISSIONLAB_BENCH_MAX_COMPARISON_SECONDS must be a number, got '${max_comparison_seconds}'"
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly script_dir
repo_root="$(cd "${script_dir}/.." && pwd)"
readonly repo_root

# Where per-cluster `kind` creation times accumulate. Under `target/`,
# which `.gitignore` already excludes, so a benchmark run can never stage
# its own samples by accident.
sample_file="${ADMISSIONLAB_BENCH_SAMPLE_FILE:-${repo_root}/target/benchmark/kind-create-seconds.tsv}"
readonly sample_file

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
echo "  asserted budgets       fixture stage <= ${max_capture_seconds}s, comparison <= ${max_comparison_seconds}s"
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
# Cluster creation samples (ROADMAP Task 9.8 step 2)
# --------------------------------------------------------------------
#
# Two samples per run -- one per cluster -- appended to a file that
# `scripts/benchmark-gateway.sh` appends to identically, so the summary
# below is taken across every cluster either script has created on this
# machine rather than across the two this run happened to make. A p95
# over two numbers would be the slower of two numbers wearing a
# statistic's name; over an accumulating file it becomes one as the file
# fills, and the summary states how many samples it had either way.
#
# Nothing here asserts. §33's ~90s target is printed beside the measured
# numbers so a reader can see the margin, and that is all.

mkdir -p "$(dirname "${sample_file}")"

# One line per cluster: seconds, which script created it, which side it
# was, and when. Only the first column is read back; the rest is there so
# a surprising sample can be traced to the run that produced it.
record_cluster_sample() {
  if [ "$1" = "-" ] || [ -z "$1" ]; then
    return 0
  fi
  printf '%s\t%s\t%s\t%s\n' \
    "$1" "benchmark-alpha" "$2" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"${sample_file}"
}

record_cluster_sample "${clusters_baseline}" "baseline"
record_cluster_sample "${clusters_candidate}" "candidate"

# Nearest-rank percentiles over the first column, sorted numerically.
# `scripts/benchmark-gateway.sh` computes the same summary over the same
# file with the same awk program; the two are kept identical on purpose,
# so either script alone reports the whole picture.
cluster_summary="$(sort -n "${sample_file}" | awk '
  { v[NR] = $1 + 0 }
  END {
    if (NR == 0) { print "0 - -"; exit }
    median = (NR % 2) ? v[(NR + 1) / 2] : (v[NR / 2] + v[NR / 2 + 1]) / 2
    # Nearest rank: the smallest sample at or above the 95th percentile
    # position. Below 20 samples that position is always NR, so p95 is
    # the slowest sample -- which the printed sample count makes visible
    # rather than dressing up.
    rank = int(NR * 0.95)
    if (rank < NR * 0.95) { rank = rank + 1 }
    if (rank < 1) { rank = 1 }
    printf "%d %.3f %.3f\n", NR, median, v[rank]
  }')"
read -r cluster_samples cluster_median cluster_p95 <<EOF
${cluster_summary}
EOF

echo "  kind cluster creation, across every sample in"
echo "    ${sample_file}"
echo "    samples              ${cluster_samples}"
echo "    median               ${cluster_median}s per cluster"
if [ "${cluster_samples}" -lt 20 ]; then
  echo "    p95                  ${cluster_p95}s per cluster (nearest rank; under 20 samples this is the slowest one)"
else
  echo "    p95                  ${cluster_p95}s per cluster (nearest rank)"
fi
echo "    PRODUCT.md §33       ~90s per cluster -- reported, never asserted"
echo

# --------------------------------------------------------------------
# Enforcement, and cleanup verification
# --------------------------------------------------------------------

leaked="$(kind get clusters 2>/dev/null | grep '^adlab-' || true)"
if [ -n "${leaked}" ]; then
  echo "benchmark-alpha: WARNING: clusters survived the run:" >&2
  echo "${leaked}" >&2
  fail "the run leaked a cluster (PRODUCT.md §33); delete it with: kind delete cluster --name <name>"
fi
echo "benchmark-alpha: no adlab-* cluster survived."

# PRODUCT.md §33, first asserted budget: the fixture stage. Measured
# against the stage's wall-clock, which is what the stage *took*; the
# slower side is printed in the table above and is the number the
# serial-versus-parallel question (Task 5.8) is argued over, but it is
# not what §33 budgets.
if [ "${capture_wall}" = "-" ]; then
  fail "the run recorded no fixture-capture duration, so its budget cannot be checked"
fi
if [ "$(awk "BEGIN { print (${capture_wall} > ${max_capture_seconds}) ? 1 : 0 }")" = "1" ]; then
  fail "fixture stage took ${capture_wall}s over ${fixture_count} fixtures, above the ${max_capture_seconds}s ceiling (PRODUCT.md §33: ~5 minutes per 100 fixtures)"
fi
echo "benchmark-alpha: fixture stage $(secs "${capture_wall}")s is within its ${max_capture_seconds}s budget (PRODUCT.md §33)."

# PRODUCT.md §33, second asserted budget: semantic comparison.
if [ "${comparison_ms}" = "-" ]; then
  fail "the run recorded no comparison duration, so its budget cannot be checked"
fi
if [ "$(awk "BEGIN { print (${comparison_ms} > ${max_comparison_seconds}) ? 1 : 0 }")" = "1" ]; then
  fail "comparison took ${comparison_ms}s over ${fixture_count} fixtures, above the ${max_comparison_seconds}s ceiling (PRODUCT.md §33: under 1 second per 100 fixtures)"
fi
echo "benchmark-alpha: comparison $(secs "${comparison_ms}")s is within its ${max_comparison_seconds}s budget (PRODUCT.md §33)."

echo "benchmark-alpha: done."
