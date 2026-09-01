# Admission Lab

**Catch Kubernetes admission regressions before they reach production.**

Admission Lab creates two throwaway `kind` clusters — a *baseline* and a
*candidate* — installs a different version of your admission stack into each,
replays the same fixtures through both real API servers, and tells you exactly
what changed. Not "the chart upgraded cleanly", but "after this upgrade, the
`istio-proxy` container is injected into `web-*` pods, `pod-deny` is now
admitted, and the first place the two stacks diverged was
`inject.example.com`'s patch in round 0". The verdict is deterministic, the
evidence comes from real Kubernetes API servers, and nothing touches your
production cluster.

> **Public Alpha.** Admission Lab is pre-1.0. Alpha covers **admission**
> regression only. The `admissionlab.io/result/v1alpha1` result schema is
> **experimental** and may change without a compatibility guarantee until
> Public Beta. Gateway API behavior comparison is **planned for Public Beta**
> and is not available today — no Gateway behavior is produced, reported, or
> supported by this release.

---

## Contents

- [Install](#install)
- [Prerequisites](#prerequisites)
- [30-second quickstart](#30-second-quickstart)
- [What the output means](#what-the-output-means)
- [Exit codes](#exit-codes)
- [Cleanup](#cleanup)
- [Server-side dry-run: what it can and cannot see](#server-side-dry-run-what-it-can-and-cannot-see)
- [Documentation](#documentation)

---

## Install

### From source (the Alpha path)

Admission Lab builds with the exact toolchain pinned in
`rust-toolchain.toml`; `rustup` installs it automatically the first time you
run `cargo` in the repository.

```bash
git clone https://github.com/hairbui76/adminssion-lab.git
cd adminssion-lab
cargo install --path crates/admissionlab-cli --locked
```

Or without cloning:

```bash
cargo install --git https://github.com/hairbui76/adminssion-lab.git admissionlab-cli --locked
```

Both install a single binary named `admissionlab`.

### From the Releases page

Once a version is tagged, the
[Releases page](https://github.com/hairbui76/adminssion-lab/releases)
carries prebuilt archives for **Linux x86_64, Linux aarch64, macOS Apple
Silicon, and macOS Intel**, plus a `SHA256SUMS` file and a keyless Sigstore
signature over it (`SHA256SUMS.sig` / `SHA256SUMS.pem`). Verify before you
run:

```bash
sha256sum --check --ignore-missing SHA256SUMS
```

Each release's notes carry the matching `cosign verify-blob` invocation.

### Windows

There is **no native Windows build and no native Windows support**. `kind` and
Docker behave differently enough on native Windows that Admission Lab does not
commit to it for v1. Windows users should run Admission Lab under **WSL2**,
using the Linux x86_64 build and a Linux Docker daemon inside WSL2.

---

## Prerequisites

Admission Lab drives four external tools as bounded subprocesses and does not
reimplement them:

| Tool | Why |
| --- | --- |
| `docker` | runs the `kind` node containers (the daemon must be reachable) |
| `kind` | creates and deletes the two ephemeral clusters |
| `kubectl` | applies raw manifests during component installation |
| `helm` | installs charted components |

Admission Lab also wants roughly 10 GiB of free disk on the run root's
filesystem — below that it warns, but does not refuse to run.

`admissionlab doctor` checks all of it and creates nothing:

```console
$ admissionlab doctor
Admission Lab doctor
  platform: linux (supported)
  kind: found (v0.33.0)
  kubectl: found (v1.32.11) - kubectl client v1.32.11 is 3 Kubernetes minor versions away from Admission Lab's supported range (1.37, 1.36, 1.35); Kubernetes tolerates only ±1 minor of client/server skew, which can produce confusing failures against a provisioned cluster
  helm: found (v3.20.0)
  docker: found (29.4.1)
  docker daemon: reachable

All required prerequisites are met.
```

`admissionlab doctor --deep` additionally creates one real ephemeral cluster,
verifies its API health and that its audit log exists, and deletes it again.
Plain `doctor` never creates anything.

Supported Kubernetes versions are pinned, with digests, in
`compatibility/kubernetes.yaml`. At the time of writing that is **1.35, 1.36
(primary), and 1.37**.

---

## 30-second quickstart

### 1. Write a lab configuration

Save this as `admissionlab.yaml`. It compares two Kyverno chart versions on the
same Kubernetes version — the shape of almost every real upgrade question.

```yaml
apiVersion: admissionlab.io/v1alpha1
kind: Lab

baseline:
  kubernetes: "1.36.4"
  components:
    - name: kyverno
      version: "3.8.2"
      install:
        type: helm
        chart: kyverno/kyverno
        repo: https://kyverno.github.io/kyverno/
        version: "3.8.2"
        namespace: kyverno
      readiness:
        - type: deploymentAvailable
          namespace: kyverno
          name: kyverno-admission-controller
        - type: webhookConfigurationPresent
          name: kyverno-resource-validating-webhook-cfg

candidate:
  kubernetes: "1.36.4"
  components:
    - name: kyverno
      version: "3.9.0"
      install:
        type: helm
        chart: kyverno/kyverno
        repo: https://kyverno.github.io/kyverno/
        version: "3.9.0"
        namespace: kyverno
      readiness:
        - type: deploymentAvailable
          namespace: kyverno
          name: kyverno-admission-controller
        - type: webhookConfigurationPresent
          name: kyverno-resource-validating-webhook-cfg

fixtures:
  include:
    - "fixtures/**/pod-*.yaml"
```

Every path in the file — fixture globs, `expectationsFile`, manifest paths,
Helm values files — resolves against **the configuration file's own
directory**, never the working directory.

**Do not skip `readiness`.** `helm upgrade --install` returns as soon as a
release is applied, not when its controller is serving. A lab that replays
fixtures inside that window compares two stacks that were not yet doing
anything — and reports no changes. See [`docs/config.md`](docs/config.md) for
the full reference and all five check types.

The smallest configuration that loads at all is just five keys:

```yaml
apiVersion: admissionlab.io/v1alpha1
kind: Lab
baseline:
  kubernetes: "1.36.4"
candidate:
  kubernetes: "1.36.4"
fixtures:
  include:
    - "fixtures/**/pod-*.yaml"
```

### 2. Write fixtures

A fixture is an ordinary Kubernetes object. Admission Lab replays each one as a
server-side dry-run CREATE against both clusters and compares what came back.

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: web-frontend
  namespace: admissionlab-fixtures
spec:
  containers:
    - name: app
      image: registry.k8s.io/pause:3.10
```

Fixtures need their namespace to already exist — a dry-run CREATE persists
nothing, so a `Namespace` fixture cannot create one for the pods that follow.
Keep setup manifests *outside* your include glob and apply them yourself. See
[`docs/fixtures.md`](docs/fixtures.md).

### 3. Run it

```bash
admissionlab doctor
admissionlab test admissionlab.yaml
```

That is the whole interface. `test` takes one positional configuration path and
three flags:

```
      --keep-clusters     Preserve baseline/candidate clusters after the run
                          instead of deleting them
  -v, --verbose           Raise Admission Lab's own crates to `debug`-level
                          logging (ignored when RUST_LOG is set)
      --report-dir <DIR>  Write `result.json` and `report.html` into this
                          directory instead of the run's own `reports/`
```

---

## What the output means

Admission Lab renders the same result three ways — terminal, `result.json`, and
a standalone `report.html`. All three are drawn from one redacted value, so
they never disagree.

Here is the terminal report for the canonical example result the report crate
renders in `crates/admissionlab-report/tests/terminal.rs` — this is checked-in
rendered output, not a mock-up:

```text
Admission Lab result  run alpha-demo-run
schema admissionlab.io/result/v1alpha1 (experimental; stable at Beta)

Environments
  baseline   Kubernetes v1.34.1  (sidecar-injector 1.26.3)
  candidate  Kubernetes v1.34.1  (sidecar-injector 1.27.0)

Summary  4 fixtures
  identical    1
  expected     1
  warnings     0
  critical     1
  inconclusive 1

Critical  1
  deployment-sidecar [istio-proxy]
    container_added at /spec/template/spec/containers/1
    first divergence [observed]: the container appears in inject.example.com's candidate patch
      baseline none -> candidate inject.example.com (round 0, index 0)

Warnings  0
  none

Inconclusive  1
  crd-custom-resource
    candidate: the candidate cluster's CRD does not accept server-side dry-run

Stale expectations  1
  sidecar-injection-rollout: no matching change was observed in this run

Diagnostics  2
  metrics.unavailable: per-webhook latency metrics were not scraped on the candidate side
  kubeconfig.loaded: loaded isolated kubeconfigs for both sides

Result: fail
```

Reading it:

- **Summary buckets** partition every fixture: `identical` (both sides agreed),
  `expected` (differences an `expectations.yaml` entry accounts for),
  `warnings`, `critical`, and `inconclusive` (a side could not be compared at
  all — that is a third state, never silently folded into "agreed").
- **First divergence** names the earliest point in the admission chain where
  the two sides stopped matching, tagged `observed`, `partial`, or `unknown`.
  When the evidence is not there, Admission Lab says so. It never invents a
  cause.
- **Stale expectations** are entries in your `expectations.yaml` that matched
  nothing this run — usually a change that has since been fixed and an entry
  you can delete.
- **Diagnostics** record what the run could and could not observe. Missing
  per-webhook latency metrics are reported as unavailable, and never as zero.

Every change is graded by a default severity table before your `policy` section
is applied: a newly denied object, a removed container, a changed service
account or security context are **critical**; an added container or a changed
environment is a **warning**; a changed image is **informational**. The full
table is in [`docs/config.md`](docs/config.md).

Raw evidence for every fixture — the request sent, the response received, the
audit window, the metric samples — is written under the run workspace at
`${TMPDIR}/admissionlab-runs/<run-id>/raw/<side>/<fixture-id>/`, mode `0700`.
The reports point at it.

---

## Exit codes

The numbering is frozen. A CI job can branch on it.

| Code | Meaning | Typical cause |
| ---: | --- | --- |
| `0` | Completed and passed | No unexpected critical change. **Warnings still exit `0`** — Alpha has no separate "completed with warnings" code, and folding warnings into `1` would fail every job on a difference a human merely ought to see. |
| `1` | Completed, regression policy failed | An unexpected critical change, or a `policy.failOn` category was observed. |
| `2` | Invalid user configuration or fixture definition | A malformed `admissionlab.yaml`, an unknown `policy.failOn` name, a fixture with no `metadata.name`, a duplicate fixture ID, an unpinned Helm version, an unsupported Kubernetes version, a glob matching zero fixtures — **or a missing host prerequisite**, so that `test` and `doctor` never disagree about "you have not installed `kind`". Every check in this class runs before any cluster is created. |
| `3` | Lab infrastructure failure | `kind`/Docker could not create a cluster, the run workspace could not be written, the report directory was not writable — **or cleanup failed.** A run that leaked a cluster never exits `0`. |
| `4` | Installation or readiness failure | A chart would not install, or a component never became ready within the timeout. |
| `5` | Fixture execution or capture failure | The dry-run CREATE could not be issued, the fixture's `apiVersion`/`kind` does not resolve on the cluster, or evidence could not be written. A fixture the API server *rejected* is not this — that is an ordinary observed outcome. |
| `6` | Internal Admission Lab error | A bug. Please report it. |

A run that fails at or after installation still writes a `diagnostics.json`
into the report directory before cleanup, naming the stage that failed and
every diagnostic collected up to that point. It deliberately does **not** write
a `result.json`: a run that never compared both sides has not earned a verdict,
and manufacturing one would be a fabrication.

---

## Cleanup

**Clusters are deleted by default, on success and on failure.** There is one
path from "the clusters exist" to "the process returns", and cleanup is on it.

To keep them for debugging:

```bash
admissionlab test --keep-clusters admissionlab.yaml
```

That prints each cluster's name, its kubeconfig path, and the exact command to
remove it:

```text
Clusters preserved (--keep-clusters was set); nothing was deleted.
  baseline cluster "adlab-baseline-01hq..."
    kubeconfig: /tmp/admissionlab-runs/<run-id>/kubeconfigs/baseline.kubeconfig
    delete with: kind delete cluster --name adlab-baseline-01hq...
```

Clusters are always named `adlab-<side>-<short-run-id>`, so
`kind get clusters | grep '^adlab-'` finds anything a killed run left behind.

---

## Server-side dry-run: what it can and cannot see

**This is the single most important semantic limitation in Alpha. Read it
before you trust a green run.**

Admission Lab replays every admission fixture as a Kubernetes **server-side
dry-run CREATE** (`?dryRun=All`) against a real API server. This is
authoritative for Alpha: it runs the real admission chain — real mutating
webhooks, real reinvocation, real validating webhooks, real API-server
validation — and returns the actual admitted, mutated object. There is no
in-process simulator anywhere in the result path, and there is no fallback to a
persisted CREATE.

What a dry-run request **does not** observe:

- **Anything that only happens on persistence.** Nothing is written to etcd, so
  finalizers, generated names, resourceVersion allocation, and storage-layer
  conversion are not exercised.
- **Anything a controller does after admission.** A Deployment is not
  reconciled, a ReplicaSet is not created, a pod is not scheduled, and a
  sidecar is not started. Admission Lab compares what admission *returned*, not
  what the cluster would eventually converge to.
- **Anything requiring the object to already exist.** A dry-run CREATE persists
  nothing, which is why a `Namespace` fixture cannot set up the namespace your
  pod fixtures need — that setup has to be applied for real, outside the
  fixture glob.
- **Webhook side effects.** A webhook that writes to an external system on
  admission may or may not honor `dryRun: true` in the AdmissionReview it
  receives. Admission Lab cannot verify that it did.

What Admission Lab guarantees in exchange: **a fixture that cannot be evaluated
under server-side dry-run fails explicitly, and the semantics are never
silently switched.** Concretely:

- A fixture whose `apiVersion`/`kind` does not resolve on the cluster, or whose
  dry-run CREATE cannot be issued at all, **fails the run with exit `5`**. It is
  never quietly skipped and never scored as agreement.
- The result model carries a distinct `unsupported_dry_run` decision. When
  either side reports it, the two sides are marked **incomparable** and the
  fixture lands in the `inconclusive` bucket — a third state, so an empty change
  list can never be misread as "the two stacks agreed".
- A webhook rejection is *not* this. A `403 Forbidden` from a policy engine or a
  `500` from an unreachable webhook means dry-run worked exactly as intended and
  observed a real rejection; that is a comparable outcome, not an unsupported
  one.

Fixture execution is **serial within each cluster** in Alpha, which is what
makes audit-log correlation deterministic. Baseline and candidate run
concurrently with each other because the two clusters are isolated.

Gateway behavior is different in kind — reconciliation and data-plane
programming need durable objects — which is one reason Gateway support is
**planned for Public Beta** rather than shipped here.

---

## Documentation

| Document | What is in it |
| --- | --- |
| [`docs/architecture.md`](docs/architecture.md) | As-built crate map and dependency rules, the run pipeline's stages, the evidence model, audit correlation, and why fixture execution is serial |
| [`docs/config.md`](docs/config.md) | Full `admissionlab.yaml` v1alpha1 reference: every field, every default, path resolution, `policy`, overrides, and `expectations.yaml` |
| [`docs/fixtures.md`](docs/fixtures.md) | Fixture format, discovery globs, identity and hashing, the setup-outside-the-glob pattern, and the dogfood webhook's annotation vocabulary |
| [`docs/github-action.md`](docs/github-action.md) | The composite action: pinned/checksummed installation, every input, the artifacts it uploads on a failing run, exit-code behavior, and what the job summary says |
| [`docs/recipes.md`](docs/recipes.md) | What a recipe is, the certified set, the capability model, override directories, and why recipes may never classify regressions |
| [`docs/security.md`](docs/security.md) | Threat model, the trust boundary around third-party charts, exactly what is redacted and what is not, and the audit-policy Secret exclusion |
| [`docs/troubleshooting.md`](docs/troubleshooting.md) | Real failure modes and their fixes, keyed to the exit codes above |

The canonical worked example lives in
[`examples/kyverno-istio-upgrade/`](examples/kyverno-istio-upgrade/) — a
complete lab configuration with its fixture corpus, both stack definitions, and
an `expectations.yaml`, reproducing a real admission regression end to end.
Start there if you would rather read a working lab than a reference.

[`examples/gateway-istio/`](examples/gateway-istio/) is its Gateway
counterpart: two identical real Istio installs serving the Gateway API, told
apart by one line of a `ReferenceGrant`. It exits `1` naming the route, the
condition that changed (`ResolvedRefs`), the reason Gateway API specifies for
it (`RefNotPermitted`), and the traffic probe that was skipped because of it.
Build the echo backend first with `./scripts/build-test-images.sh`.

`PRODUCT.md` is the product specification, `ROADMAP.md` the implementation
plan, and `CONTRIBUTING.md` explains how to propose changes and run the
verification suite.

---

## Design commitments

These are constraints, not aspirations. A change that breaks one of them is a
bug.

- **Local-first.** Every workflow runs on your machine with no Admission Lab
  server, account, or hosted service. There are no paid tiers.
- **No production access.** The default flow requires no production kubeconfig
  and copies no production secrets.
- **Real API servers are authoritative.** No in-process simulator may ever
  replace a real cluster result.
- **Isolated by default.** Baseline and candidate are separate ephemeral
  clusters and never share mutable cluster state.
- **Deterministic.** Classification, first-divergence claims, and pass/fail
  decisions are computed, not inferred. No AI is used for correctness.
- **Vendor-neutral core.** Recipes may supply install, readiness,
  normalization, and capability metadata. They may never contain regression
  classification logic.
- **Never fabricated.** Missing evidence is reported as unknown or partial. It
  is never filled in with a plausible guess.

---

## License

Apache-2.0. See [`LICENSE`](LICENSE).
