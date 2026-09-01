#!/usr/bin/env bash
#
# The Gateway performance benchmark (ROADMAP Task 9.8 step 3): one real
# `admissionlab test` run of a deterministic basic-routing suite, on two
# real `kind` clusters each running a real NGINX Gateway Fabric, with the
# reconciliation and probe timings the run recorded printed as a stable
# table.
#
# Usage:
#   ./scripts/benchmark-gateway.sh
#
# It builds `admissionlab` in release mode (or uses `ADMISSIONLAB_BIN`),
# builds the echo backend image, generates a throwaway Gateway lab under
# a temporary directory, runs it, prints the table, deletes the lab, and
# verifies no `adlab-*` cluster survived. `scripts/benchmark-alpha.sh` is
# its admission-side counterpart and shares its vocabulary, its exit
# codes, and its cluster-creation sample file.
#
# # The stack: NGINX Gateway Fabric, and why not Istio
#
# Both certified Gateway API implementations would answer the same
# question. NGF is the cheaper of the two by a wide margin, and the
# measurements are already in the repository:
#
#   * `recipes/nginx-gateway-fabric/recipe.yaml` records NGF's Helm
#     install returning in 12.5 seconds on a real Kubernetes 1.36.4
#     cluster, with `GatewayClass/nginx` `Accepted=True` five seconds
#     later. One chart, one control-plane Deployment, one cert-generator
#     Job.
#   * `.github/workflows/nightly.yml`'s `dogfood-gateway` job measures
#     `examples/gateway-istio` — the Istio equivalent — at 204.5 seconds
#     per run on a warm reference machine, and its own comment attributes
#     the cost to two full Istio stacks plus a candidate side that spends
#     its whole reconciliation budget.
#
# A benchmark run twice per release gate should not spend three and a
# half minutes per run establishing a fact a cheaper stack establishes
# just as well. The vendor is not what is being measured here: what is
# being measured is how long Admission Lab takes to apply a suite,
# observe a route converge, and probe a data plane, and NGF exercises
# every one of those code paths (`fixtures/gateway/nginx/` and
# `crates/admissionlab-gateway/tests/portable_contracts.rs` are the
# certification that says so).
#
# # The suite: one route, two probes, both sides identical
#
# The route contract is the portable corpus's contract 1 —
# `fixtures/gateway/portable/README.md`'s "Basic host/path routing:
# `200` from `echo-a` on a matched path; `404` on an unmatched one" —
# reduced to the single `Gateway`/`HTTPRoute` pair that expresses it. Two
# probes, one of each, on one hostname; the 404 half is what makes it a
# *path* contract rather than only a host one, and it is measured
# behavior on both certified implementations rather than an assumption
# about this one.
#
# Both sides install the identical stack and the suite is applied
# identically to each, so the two sides agree by construction and the run
# exits 0. That is the same trade `examples/admission-basic` and
# `scripts/benchmark-alpha.sh` make and for the same reason: a benchmark
# wants a fixed, repeatable amount of work, and a seeded regression would
# add a finding without adding a millisecond of the work being timed.
# `examples/gateway-istio` is where the seeded Gateway regression lives,
# and `.github/workflows/nightly.yml` is what repeats it.
#
# The backend is `fixtures/gateway/backends/echo-a.yaml` itself, applied
# unmodified. That file deliberately declares no `metadata.namespace`,
# and `admissionlab_gateway::apply::apply_gateway_manifests` applies each
# document to the namespace the document names — so it lands in
# `default`, and this suite's `Gateway` and `HTTPRoute` are written into
# `default` with it. One namespace for all three keeps the backend
# reference same-namespace (no `ReferenceGrant`, which is a different
# contract's subject) and lets this script reference the canonical
# backend definition instead of copying sixty lines of Deployment into a
# heredoc that would silently drift from it.
#
# # No arbitrary sleeps, and here is the audit
#
# Task 9.8 step 3 requires it, so every wait on the path from "apply the
# suite" to "the probe answered" is named here with the mechanism that
# bounds it. There is not a `sleep` in this script, and there is no
# fixed-duration wait anywhere below it:
#
#   1. Component readiness (the Gateway API CRDs, the NGF chart, and this
#      suite's own `readiness:` block) — `admissionlab_installer::readiness`
#      polls each declared condition with capped exponential backoff
#      (`BackoffPolicy::default()`: 250ms doubling to a 10s cap) against
#      an absolute deadline, and returns the moment the condition holds.
#   2. Route reconciliation — `admissionlab_gateway::reconcile` polls the
#      `GatewayClass`, `Gateway` and `HTTPRoute` from
#      `INITIAL_POLL_INTERVAL` (100ms), doubling to `MAX_POLL_INTERVAL`
#      (2s), never sleeping past the contract's own
#      `reconciliationTimeoutMillis` deadline, and requires the same
#      status twice before calling it converged. The measured
#      `reconciliation elapsed` this script prints is that loop's own
#      first-poll-to-last-poll wall-clock.
#   3. The port-forward to the data plane —
#      `admissionlab_gateway::port_forward` waits for the child to report
#      itself ready, bounded by `PORT_FORWARD_READY_TIMEOUT` (15s), and
#      proceeds the moment it is.
#   4. The probe itself — `admissionlab_gateway::probe` retries *only* a
#      connection that was refused, at `PROBE_RETRY_INTERVAL` (100ms),
#      inside a bounded window, with `PROBE_REQUEST_TIMEOUT` (30s) on the
#      request. A `404` is an observation and is never retried into a
#      `200`. The `attempts` column below is that retry count, reported
#      rather than normalized away.
#
# So the timings this script prints are the durations of real waits on
# real cluster state, and a regression in any of them is a regression in
# the product rather than in a constant somebody picked.
#
# # What this script asserts, and what it only reports
#
# PRODUCT.md §33 states no Gateway target. It states three numbers, and
# exactly one of them is in this run's scope:
#
#   * "semantic comparison of 100 ordinary fixtures in under one second
#     after artifacts are collected" — ASSERTED. This run's comparison
#     covers the route contract's reconciliation and traffic evidence as
#     well as the admission fixture, and §33's second is the ceiling.
#   * "typical `kind` cluster creation under approximately 90 seconds per
#     cluster" — REPORTED, never asserted, exactly as in
#     `scripts/benchmark-alpha.sh`, and into the same accumulating sample
#     file so the median and p95 printed by either script cover the
#     clusters created by both.
#   * "100-fixture admission suite completes within approximately five
#     minutes" — not asserted here. This lab replays one ConfigMap, and a
#     five-minute ceiling over one fixture is a check that cannot fail.
#     `scripts/benchmark-alpha.sh` enforces that budget over the corpus
#     size §33 states it at.
#
# Two things §33 does not speak to are asserted anyway, because they are
# what makes this suite *deterministic* rather than merely fast, and a
# benchmark of a suite that stopped behaving is worthless:
#
#   * both sides' route reconciliation must converge; and
#   * every probe must return its contracted status on both sides (and
#     its contracted backend, where one is contracted).
#
# The product deliberately does not grade those itself —
# `admissionlab_gateway::case` states the rule: the Gateway engine
# reports what an implementation did, and grading lives in
# `admissionlab-policy`, which has no opinion about a probe's expected
# status. So this script reads back what was observed and checks it
# against the contract it wrote, which is the honest place for that check
# to live.
#
# Gateway stage, reconciliation and probe wall-clock are REPORTED. There
# is no budget to assert them against, and inventing one would be
# inventing a product target in a shell script.
#
# Exit status, the same shape `scripts/benchmark-alpha.sh` uses:
#
#   0  the run produced a result, the suite behaved as contracted, and
#      the asserted budget held
#   1  something about this benchmark, the run itself, the contracted
#      behavior, or the asserted budget failed
#
# # The knobs
#
#   ADMISSIONLAB_BENCH_MAX_COMPARISON_SECONDS
#       The comparison stage's ceiling, in seconds. Defaults to
#       PRODUCT.md §33's one second. Same variable, same meaning, and the
#       same default as in `scripts/benchmark-alpha.sh`.
#
#   ADMISSIONLAB_BENCH_SAMPLE_FILE
#       Where per-cluster `kind` creation times accumulate, one sample
#       per line. Defaults to `target/benchmark/kind-create-seconds.tsv`
#       inside the repository, which is exactly what
#       `scripts/benchmark-alpha.sh` defaults to, so the two scripts
#       share one sample set unless told otherwise.
#
#   ADMISSIONLAB_BENCH_SKIP_IMAGE_BUILD
#       When set to any non-empty value, `scripts/build-test-images.sh`
#       is not run and `admissionlab-echo:dev` must already be in the
#       local Docker image store. Unset (the default) the image is
#       rebuilt every run, which is a cached-layer no-op when nothing
#       changed and is the only thing that stops this benchmark
#       measuring a backend built from some other commit.
#
#   ADMISSIONLAB_BIN
#       An already-built `admissionlab` to measure, instead of building
#       one. The binary must be a *release* build: a debug build measures
#       a program nobody ships.

set -euo pipefail

# --------------------------------------------------------------------
# Configuration
# --------------------------------------------------------------------

# Kubernetes version for both sides: the Tier 1 primary supported
# version (`compatibility/kubernetes.yaml`), which is also the version
# `compatibility/recipes.yaml` certifies the `nginx-gateway-fabric`
# recipe on.
readonly KUBERNETES_VERSION="1.36.4"

# The two vendor pins, both taken from
# `recipes/nginx-gateway-fabric/recipe.yaml` rather than chosen here.
readonly NGF_VERSION="2.6.7"
readonly GATEWAY_API_VERSION="1.5.1"

# The contract, declared once. Every value below is substituted into the
# generated lab AND checked against what the run observed, so the
# expectation exists in exactly one place.
readonly CONTRACT_ID="basic-routing"
readonly ROUTE_NAMESPACE="default"
readonly GATEWAY_NAME="lab-gateway"
readonly ROUTE_NAME="echo-route"
readonly PROBE_HOST="basic.bench.gateway.admissionlab.test"
readonly MATCHED_PREFIX="/bench"
readonly MATCHED_PATH="/bench/probe"
readonly MATCHED_STATUS="200"
readonly MATCHED_BACKEND="echo-a"
readonly UNMATCHED_PATH="/not-bench/probe"
readonly UNMATCHED_STATUS="404"

fail() {
  echo "benchmark-gateway: error: $1" >&2
  exit 1
}

# Spelled to three decimals so it prints the way every other number this
# script emits does, and the way `scripts/benchmark-alpha.sh` prints the
# same default.
max_comparison_seconds="${ADMISSIONLAB_BENCH_MAX_COMPARISON_SECONDS:-1.000}"
readonly max_comparison_seconds
if ! [[ "${max_comparison_seconds}" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
  fail "ADMISSIONLAB_BENCH_MAX_COMPARISON_SECONDS must be a number, got '${max_comparison_seconds}'"
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly script_dir
repo_root="$(cd "${script_dir}/.." && pwd)"
readonly repo_root

sample_file="${ADMISSIONLAB_BENCH_SAMPLE_FILE:-${repo_root}/target/benchmark/kind-create-seconds.tsv}"
readonly sample_file

# `helm` as well as `benchmark-alpha.sh`'s three: the NGF chart is
# installed with it, and a missing `helm` should be a sentence here
# rather than a component failure eight minutes into a run.
for tool in docker kind jq helm; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    fail "'${tool}' is not on PATH"
  fi
done

# --------------------------------------------------------------------
# The binary under measurement, and the backend image
# --------------------------------------------------------------------

if [ -n "${ADMISSIONLAB_BIN:-}" ]; then
  binary="${ADMISSIONLAB_BIN}"
  [ -x "${binary}" ] || fail "ADMISSIONLAB_BIN '${binary}' is not an executable file"
  echo "benchmark-gateway: measuring ${binary} (ADMISSIONLAB_BIN)"
else
  echo "benchmark-gateway: building admissionlab (release)"
  (cd "${repo_root}" && cargo build --release -p admissionlab-cli --bin admissionlab) \
    || fail "cargo build --release failed"
  binary="${repo_root}/target/release/admissionlab"
  [ -x "${binary}" ] || fail "expected a release binary at ${binary}"
fi
readonly binary

# No cluster names: they do not exist yet, because `admissionlab test`
# creates them itself with run-scoped names. This puts the image in the
# local Docker store, and the generated lab's own `images:` list is what
# side-loads it into each cluster after creation (ROADMAP Task 6.11).
if [ -n "${ADMISSIONLAB_BENCH_SKIP_IMAGE_BUILD:-}" ]; then
  docker image inspect admissionlab-echo:dev >/dev/null 2>&1 \
    || fail "ADMISSIONLAB_BENCH_SKIP_IMAGE_BUILD is set but admissionlab-echo:dev is not in the local Docker image store"
  echo "benchmark-gateway: using the admissionlab-echo:dev already in the image store"
else
  echo "benchmark-gateway: building the echo backend image"
  "${repo_root}/scripts/build-test-images.sh" >/dev/null \
    || fail "scripts/build-test-images.sh failed"
fi

# --------------------------------------------------------------------
# The throwaway lab
# --------------------------------------------------------------------

lab_dir="$(mktemp -d "${TMPDIR:-/tmp}/admissionlab-gwbench.XXXXXXXX")"
readonly lab_dir

cleanup() {
  rm -rf "${lab_dir}"
}
trap cleanup EXIT

mkdir -p "${lab_dir}/fixtures" "${lab_dir}/gateway" "${lab_dir}/reports"

# One admission fixture, because a lab always has one: `fixtures.include`
# is required and must match at least one document, and that rule is not
# relaxed by a `gateway:` section being present.
# `examples/gateway-istio/fixtures/configmap-settings.yaml` makes the
# same choice with the same object for the same reason.
cat >"${lab_dir}/fixtures/settings.yaml" <<'YAML'
# The most boring object in Kubernetes. No component in this stack
# touches a ConfigMap, both sides admit it unchanged, and it is counted
# `identical` in every run -- which is the point: it makes the report
# honest that the admission half ran at all.
apiVersion: v1
kind: ConfigMap
metadata:
  name: gateway-bench-settings
  namespace: default
data:
  mode: benchmark
YAML

cat >"${lab_dir}/gateway/route.yaml" <<YAML
# Generated by scripts/benchmark-gateway.sh. The Gateway half of the
# portable corpus's contract 1, in the same namespace as the backend
# \`fixtures/gateway/backends/echo-a.yaml\` lands in (see that script's
# header for why that is \`default\`).
apiVersion: gateway.networking.k8s.io/v1
kind: Gateway
metadata:
  name: ${GATEWAY_NAME}
  namespace: ${ROUTE_NAMESPACE}
spec:
  # NGF's own default class name, created by its Helm chart and the one
  # its controller is started with (\`--gatewayclass=nginx\`). Nothing
  # else in this file is NGF-specific.
  gatewayClassName: nginx
  listeners:
    # One listener, named: the route's \`sectionName\` and the route
    # contract's \`listenerName\` both refer to it, and a single listener
    # is also what lets the endpoint strategy resolve the data-plane port
    # without naming one (see the lab's \`gatewayEndpoint\` block).
    - name: http
      protocol: HTTP
      port: 80
      allowedRoutes:
        namespaces:
          from: Same
---
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: ${ROUTE_NAME}
  namespace: ${ROUTE_NAMESPACE}
spec:
  parentRefs:
    - name: ${GATEWAY_NAME}
      sectionName: http
  hostnames:
    # \`.test\` is reserved by RFC 6761 for exactly this: a name
    # guaranteed never to resolve in the real DNS. The probe sends it as
    # a \`Host\` header over a port-forward to 127.0.0.1, so nothing ever
    # resolves it.
    - ${PROBE_HOST}
  rules:
    - matches:
        - path:
            type: PathPrefix
            # Narrower than \`/\` on purpose: it is what makes
            # ${UNMATCHED_PATH} a request with no matching rule, and so
            # what makes the second probe a path contract instead of a
            # second copy of the first.
            value: ${MATCHED_PREFIX}
      backendRefs:
        # No \`namespace\`: the backend is in this route's own namespace,
        # so no ReferenceGrant is involved. Cross-namespace resolution is
        # a different contract, exercised by
        # \`fixtures/gateway/portable/\` and by \`examples/gateway-istio\`.
        - name: ${MATCHED_BACKEND}
          port: 80
YAML

# Both sides are byte-identical. Written once into a shell variable and
# substituted twice, so they cannot drift within one generated file.
#
# `IFS=` is load-bearing: without it `read` strips the leading whitespace
# of the first line, and this block's indentation is YAML structure.
# `|| true` likewise: `read -d ''` finds no NUL and reports EOF, having
# read everything, which is exactly what is wanted here.
IFS= read -r -d '' side_stack <<YAML || true
  kubernetes: "${KUBERNETES_VERSION}"
  # Side-loaded from the local Docker store right after this side's
  # cluster is created: \`gateway/backends/echo-a.yaml\` references the
  # image with \`imagePullPolicy: IfNotPresent\`, so the node has to
  # already have it.
  images:
    - admissionlab-echo:dev
  components:
    # 1. The Gateway API itself, exactly as
    #    \`recipes/nginx-gateway-fabric/gateway-api-crds.yaml\` certifies
    #    it: the vendored v${GATEWAY_API_VERSION} standard-channel bundle.
    #    \`Established\` is the condition that means the API server is
    #    actually serving the resource, not merely that the CRD object
    #    was accepted.
    - name: gateway-api-crds
      version: "${GATEWAY_API_VERSION}"
      install:
        type: manifests
        paths:
          - ${repo_root}/recipes/nginx-gateway-fabric/gateway-api/standard-install-v${GATEWAY_API_VERSION}.yaml
      readiness:
        - type: customResourceCondition
          apiVersion: apiextensions.k8s.io/v1
          kind: CustomResourceDefinition
          name: gatewayclasses.gateway.networking.k8s.io
          conditionType: Established
          status: "True"
        - type: customResourceCondition
          apiVersion: apiextensions.k8s.io/v1
          kind: CustomResourceDefinition
          name: gateways.gateway.networking.k8s.io
          conditionType: Established
          status: "True"
        - type: customResourceCondition
          apiVersion: apiextensions.k8s.io/v1
          kind: CustomResourceDefinition
          name: httproutes.gateway.networking.k8s.io
          conditionType: Established
          status: "True"

    # 2. NGINX Gateway Fabric, exactly as
    #    \`recipes/nginx-gateway-fabric/recipe.yaml\` certifies it: same
    #    OCI chart reference, same pinned version, same namespace, and
    #    \`releaseName\` deliberately unset so the control-plane
    #    Deployment keeps the name the readiness check below gates on.
    #
    #    \`GatewayClass/nginx\` reaching \`Accepted=True\` is the strong
    #    check: it proves NGF's controller is running and reconciling
    #    Gateway API objects on this cluster, which no Deployment
    #    condition can establish.
    - name: nginx-gateway-fabric
      version: "${NGF_VERSION}"
      install:
        type: helm
        chart: oci://ghcr.io/nginx/charts/nginx-gateway-fabric
        repo: oci://ghcr.io/nginx/charts
        version: "${NGF_VERSION}"
        namespace: nginx-gateway
      readiness:
        - type: deploymentAvailable
          namespace: nginx-gateway
          name: nginx-gateway-fabric
        - type: customResourceCondition
          apiVersion: gateway.networking.k8s.io/v1
          kind: GatewayClass
          name: nginx
          conditionType: Accepted
          status: "True"
YAML
readonly side_stack

cat >"${lab_dir}/admissionlab.yaml" <<YAML
# Generated by scripts/benchmark-gateway.sh. Not a checked-in example:
# this file exists for the length of one benchmark run.
apiVersion: admissionlab.io/v1
kind: Lab

baseline:
${side_stack}

candidate:
${side_stack}

fixtures:
  include:
    - "fixtures/*.yaml"

gateway:
  # Applied to both sides, identically -- which is what makes the two
  # sides' route results comparable at all. The backend is the
  # repository's canonical definition, referenced rather than copied.
  manifests:
    - ${repo_root}/fixtures/gateway/backends/echo-a.yaml
    - gateway/route.yaml

  # Where NGF's provisioned data plane is, so a probe can be sent through
  # it. The same block \`recipes/nginx-gateway-fabric/recipe.yaml\`
  # declares, parsed by the same validator, including its deliberate
  # omission of both \`portName\` and \`port\`: NGF exposes exactly the
  # Gateway's listener ports, this Gateway declares one listener, and a
  # single-port Service resolves unambiguously.
  gatewayEndpoint:
    type: serviceBySelector
    namespace: "{gatewayNamespace}"
    selector:
      # Gateway API's own documented "gateway infrastructure label",
      # which NGF applies because upstream specifies it -- more durable
      # than NGF's \`<gateway>-nginx\` naming convention.
      gateway.networking.k8s.io/gateway-name: "{gatewayName}"

  # Waited out after the suite is applied and before any route is
  # observed. Two Deployments, two different kinds of wait: \`echo-a\` is
  # this suite's own backend (a request routed to a backend with no ready
  # pod is answered by the data plane, which would be a statement about
  # this run's timing rather than about the route), and
  # \`${GATEWAY_NAME}-nginx\` is the data plane NGF provisions from the
  # \`Gateway\` (without it the port-forward can race a Service with no
  # ready endpoint).
  readiness:
    - type: deploymentAvailable
      namespace: ${ROUTE_NAMESPACE}
      name: ${MATCHED_BACKEND}
    - type: deploymentAvailable
      namespace: ${ROUTE_NAMESPACE}
      name: ${GATEWAY_NAME}-nginx

  routes:
    - id: ${CONTRACT_ID}
      gatewayNamespace: ${ROUTE_NAMESPACE}
      gatewayName: ${GATEWAY_NAME}
      routeNamespace: ${ROUTE_NAMESPACE}
      routeName: ${ROUTE_NAME}
      listenerName: http
      probes:
        # Probe 0: the matched path.
        - host: ${PROBE_HOST}
          path: ${MATCHED_PATH}
          method: GET
          expectedStatus: ${MATCHED_STATUS}
          expectedBackend: ${MATCHED_BACKEND}
        # Probe 1: the same hostname, a path outside the rule's prefix,
        # and no rule to match it.
        - host: ${PROBE_HOST}
          path: ${UNMATCHED_PATH}
          method: GET
          expectedStatus: ${UNMATCHED_STATUS}
YAML

echo "benchmark-gateway: generated a one-route NGF lab in ${lab_dir}"

# --------------------------------------------------------------------
# The run
# --------------------------------------------------------------------

readonly log="${lab_dir}/run.log"
readonly result="${lab_dir}/reports/result.json"

echo "benchmark-gateway: running (this creates two kind clusters and installs two NGF stacks; expect several minutes)"
started_ns="$(date +%s%N)"
set +e
"${binary}" test "${lab_dir}/admissionlab.yaml" --report-dir "${lab_dir}/reports" 2>&1 | tee "${log}"
status="${PIPESTATUS[0]}"
set -e
finished_ns="$(date +%s%N)"
readonly status

case "${status}" in
  0) verdict="pass" ;;
  1) verdict="fail (the policy found differences between two identical stacks)" ;;
  *) fail "admissionlab test exited ${status}; see the output above. No measurement was taken." ;;
esac
readonly verdict

[ -f "${result}" ] || fail "the run exited ${status} but wrote no ${result}"

total_seconds="$(awk "BEGIN { printf \"%.2f\", (${finished_ns} - ${started_ns}) / 1000000000 }")"
readonly total_seconds

# --------------------------------------------------------------------
# The table
# --------------------------------------------------------------------

# One jq pass for the stage timings, so every number in the table below
# comes from the same read of the same document. Absent stages print "-"
# rather than 0: the recorder omits a stage it did not measure, and this
# script must not fill that in (Global Constraint 15).
read -r clusters_wall clusters_baseline clusters_candidate \
  install_wall install_baseline install_candidate \
  capture_wall capture_baseline capture_candidate \
  gateway_wall gateway_baseline gateway_candidate \
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
      ($t.gatewaySuite.wallMs | ms),
      ($t.gatewaySuite.baselineMs | ms),
      ($t.gatewaySuite.candidateMs | ms),
      ($t.comparisonMs | ms),
      ($t.elapsedMs | ms)
    ]
  | @tsv
' "${result}")
EOF

if [ "${gateway_wall}" = "-" ]; then
  fail "the run recorded no gatewaySuite stage; this lab declares a gateway: section, so its absence is a product bug, not a slow run"
fi

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
echo "benchmark-gateway: results"
echo "  binary                 ${binary}"
echo "  kubernetes             ${KUBERNETES_VERSION} (both sides)"
echo "  gateway stack          NGINX Gateway Fabric ${NGF_VERSION}, Gateway API v${GATEWAY_API_VERSION} (both sides)"
echo "  route contracts        1 (${CONTRACT_ID}), 2 probes"
echo "  verdict                ${verdict} (exit ${status})"
echo "  asserted budget        comparison <= ${max_comparison_seconds}s"
echo
printf '  %-22s %12s %12s %12s\n' "stage" "wall(s)" "baseline(s)" "candidate(s)"
printf '  %-22s %12s %12s %12s\n' "----------------------" "------------" "------------" "------------"
row "cluster creation" "${clusters_wall}" "${clusters_baseline}" "${clusters_candidate}"
row "installation" "${install_wall}" "${install_baseline}" "${install_candidate}"
row "fixture capture" "${capture_wall}" "${capture_baseline}" "${capture_candidate}"
row "gateway suite" "${gateway_wall}" "${gateway_baseline}" "${gateway_candidate}"
row "comparison" "${comparison_ms}" "" ""
echo
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

# The two numbers Task 9.8 step 3 asks for by name, broken out of the
# stage above: how long each side's route took to reconcile, and how long
# each probe took. Both are the observed evidence's own monotonic
# measurements, not this script's.
echo "  reconciliation (first poll to last, per side)"
jq -r --arg pad "    " '
  .fixtures[]
  | select(.gatewayReconciliation != null)
  | .gatewayReconciliation as $r
  | "\($pad)\(.fixtureId)  baseline \($r.baseline.elapsed / 1000)s converged=\($r.baseline.converged)"
    + "  candidate \($r.candidate.elapsed / 1000)s converged=\($r.candidate.converged)"
' "${result}"
echo
echo "  probes (connection attempt to last body byte, per side)"
jq -r --arg pad "    " '
  .fixtures[]
  | select(.traffic != null)
  | .fixtureId as $id
  | .traffic.pairs[]
  | "\($pad)\($id) probe \(.index)"
    + "  baseline \(.baseline.status) \(.baseline.backend // "-") \(.baseline.elapsed / 1000)s attempts=\(.baseline.attempts)"
    + "  candidate \(.candidate.status) \(.candidate.backend // "-") \(.candidate.elapsed / 1000)s attempts=\(.candidate.attempts)"
' "${result}"
echo

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
# Two samples per run, appended to the file `scripts/benchmark-alpha.sh`
# appends to identically, so the summary below is taken across every
# cluster either script has created on this machine. Nothing here
# asserts; §33's ~90s target is printed beside the measurement so a
# reader can see the margin, and that is all.

mkdir -p "$(dirname "${sample_file}")"

record_cluster_sample() {
  if [ "$1" = "-" ] || [ -z "$1" ]; then
    return 0
  fi
  printf '%s\t%s\t%s\t%s\n' \
    "$1" "benchmark-gateway" "$2" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"${sample_file}"
}

record_cluster_sample "${clusters_baseline}" "baseline"
record_cluster_sample "${clusters_candidate}" "candidate"

# Nearest-rank percentiles over the first column, sorted numerically.
# `scripts/benchmark-alpha.sh` computes the same summary over the same
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
  echo "benchmark-gateway: WARNING: clusters survived the run:" >&2
  echo "${leaked}" >&2
  fail "the run leaked a cluster (PRODUCT.md §33); delete it with: kind delete cluster --name <name>"
fi
echo "benchmark-gateway: no adlab-* cluster survived."

# The suite is deterministic by construction: two identical stacks, one
# suite applied to each. A difference is therefore a finding about
# Admission Lab or about NGF's run-to-run behavior, never about this lab,
# and it is exactly the "unexplained flaky Gateway convergence" the
# Phase 9 exit gate lists as a release blocker.
if [ "${status}" -ne 0 ]; then
  fail "the run reported differences between two identical stacks (exit ${status}); the timings above are still valid, but this suite is supposed to be deterministic"
fi

# Both sides must actually have converged. A route that timed out still
# produces evidence and still produces a timing, and a benchmark that
# reported that timing as though the route had reconciled would be
# measuring a stopwatch running against a wall.
unconverged="$(jq -r '
  .fixtures[]
  | select(.gatewayReconciliation != null)
  | .gatewayReconciliation as $r
  | [["baseline", $r.baseline.converged], ["candidate", $r.candidate.converged]]
  | map(select(.[1] != true))
  | .[]
  | "\(.[0])"
' "${result}")"
if [ -n "${unconverged}" ]; then
  fail "a route did not converge on: $(echo "${unconverged}" | tr '\n' ' ')"
fi
echo "benchmark-gateway: both sides' route reconciliation converged."

# Every probe returned its contracted status on both sides, and the
# matched probe its contracted backend. Checked here because the product
# deliberately does not grade a probe against its contract -- see this
# script's header.
probe_mismatches="$(jq -r \
  --arg contract "${CONTRACT_ID}" \
  --argjson matched_status "${MATCHED_STATUS}" \
  --arg matched_backend "${MATCHED_BACKEND}" \
  --argjson unmatched_status "${UNMATCHED_STATUS}" '
  def expected(i): if i == 0
    then { status: $matched_status, backend: $matched_backend }
    else { status: $unmatched_status, backend: null }
    end;
  .fixtures[]
  | select(.fixtureId == $contract and .traffic != null)
  | .traffic.pairs[]
  | .index as $i
  | expected($i) as $want
  | [["baseline", .baseline], ["candidate", .candidate]]
  | .[]
  | select(
      .[1].status != $want.status
      or ($want.backend != null and .[1].backend != $want.backend)
    )
  | "probe \($i) on \(.[0]): got \(.[1].status) \(.[1].backend // "no backend"), wanted \($want.status) \($want.backend // "any backend")"
' "${result}")"
if [ -n "${probe_mismatches}" ]; then
  echo "${probe_mismatches}" >&2
  fail "a probe did not return its contracted response; see the lines above"
fi

probe_pairs="$(jq -r '[.fixtures[] | select(.traffic != null) | .traffic.pairs[]] | length' "${result}")"
if [ "${probe_pairs}" -ne 2 ]; then
  fail "expected 2 paired probes, the run reported ${probe_pairs}; an unpaired probe means one side answered and the other did not"
fi
echo "benchmark-gateway: both probes returned their contracted response on both sides."

# PRODUCT.md §33's only number in this run's scope.
if [ "${comparison_ms}" = "-" ]; then
  fail "the run recorded no comparison duration, so its budget cannot be checked"
fi
if [ "$(awk "BEGIN { print (${comparison_ms} > ${max_comparison_seconds}) ? 1 : 0 }")" = "1" ]; then
  fail "comparison took ${comparison_ms}s, above the ${max_comparison_seconds}s ceiling (PRODUCT.md §33: under 1 second)"
fi
echo "benchmark-gateway: comparison $(secs "${comparison_ms}")s is within its ${max_comparison_seconds}s budget (PRODUCT.md §33)."

echo "benchmark-gateway: done."
