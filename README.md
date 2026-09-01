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

> **Public Beta.** Admission Lab is pre-1.0. Beta covers **admission**
> regression *and* Gateway API behavior — reconciliation and real HTTP traffic,
> compared as separate evidence.
>
> The document contracts are frozen at `v1beta1` and grow only by **addition**
> from here: a new optional field, never a rename or a removal. Configuration is
> [`admissionlab.io/v1beta1`](schemas/admissionlab-v1beta1.json), the result is
> [`admissionlab.io/result/v1beta1`](schemas/result-v1beta1.json), the run
> manifest is
> [`admissionlab.io/run-manifest/v1beta1`](schemas/run-manifest-v1beta1.json).
> `admissionlab.io/v1alpha1` configurations still load unchanged —
> [`docs/schema-migrations.md`](docs/schema-migrations.md) is the record.
>
> **Frozen is not stable.** Until v1.0, a new `apiVersion` with a migration
> remains possible; what is ruled out is a silent change under an existing one.
> Which Kubernetes × stack combinations this project has actually *proven* is a
> shorter list than the ones it will happily run —
> [`docs/compatibility.md`](docs/compatibility.md) draws that line.

---

## Contents

- [Install](#install)
- [Prerequisites](#prerequisites)
- [30-second quickstart](#30-second-quickstart)
- [What the output means](#what-the-output-means)
- [Exit codes](#exit-codes)
- [Cleanup](#cleanup)
- [Server-side dry-run: what it can and cannot see](#server-side-dry-run-what-it-can-and-cannot-see)
- [Gateway: three layers of evidence](#gateway-three-layers-of-evidence)
- [Schemas](#schemas)
- [Documentation](#documentation)

---

## Install

### From source

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
apiVersion: admissionlab.io/v1beta1
kind: Lab

baseline:
  kubernetes: "1.35.8"
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
  kubernetes: "1.35.8"
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

Kubernetes `1.35.8` is not arbitrary: it is the version Admission Lab certifies
this Kyverno chart line on. The **candidate** side above is therefore a
certified combination; the **baseline**'s `3.8.2` is the version you are
upgrading *from*, and nothing certifies it — which is normal, and is why a run
like this prints a warning naming that side and then runs anyway. Admission Lab
never refuses a combination it has not certified. See
[`docs/compatibility.md`](docs/compatibility.md).

**Do not skip `readiness`.** `helm upgrade --install` returns as soon as a
release is applied, not when its controller is serving. A lab that replays
fixtures inside that window compares two stacks that were not yet doing
anything — and reports no changes. See [`docs/config.md`](docs/config.md) for
the full reference and all five check types.

The smallest configuration that loads at all is just five keys:

```yaml
apiVersion: admissionlab.io/v1beta1
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
four flags:

```
      --keep-clusters          Preserve baseline/candidate clusters after the
                               run instead of deleting them
  -v, --verbose                Raise Admission Lab's own crates to `debug`-level
                               logging (ignored when RUST_LOG is set)
      --report-dir <DIR>       Write `result.json` and `report.html` into this
                               directory instead of the run's own `reports/`
      --github-summary <FILE>  Write a GitHub Actions job summary (Markdown) to
                               this file. Written whatever happens, and never
                               stating a verdict the run did not reach
```

---

## What the output means

Admission Lab renders the same result three ways — terminal, `result.json`, and
a standalone `report.html`. All three are drawn from one redacted value, so
they never disagree.

Here is the terminal report for the canonical example result the report crate
renders in `crates/admissionlab-report/tests/terminal.rs` — this is checked-in
rendered output, not a mock-up. It is a lab with a `gateway:` section, so it
shows both engines at once:

```text
Admission Lab result  run beta-demo-run
schema admissionlab.io/result/v1beta1 (frozen; additive changes only)

Environments
  baseline   Kubernetes v1.34.1  (sidecar-injector 1.26.3)
  candidate  Kubernetes v1.34.1  (sidecar-injector 1.27.0)

Summary  5 fixtures
  identical    1
  expected     1
  warnings     1
  critical     1
  inconclusive 1

Critical  1
  deployment-sidecar [istio-proxy]
    container_added at /spec/template/spec/containers/1
    first divergence [observed]: the container appears in inject.example.com's candidate patch
      baseline none -> candidate inject.example.com (round 0, index 0)

Warnings  2
  echo-route-contract [echo-route]
    traffic_status_changed
    baseline HTTP 200 from echo-v1 -> candidate HTTP 503 from echo-v1
  echo-route-contract [echo-route]
    traffic_status_changed
    baseline HTTP 204 from echo-v1 -> candidate answered nothing

Gateway  1 route contract(s)
  echo-route-contract  both sides converged; differences and absences are evidence
    baseline: converged in 4180ms
      GatewayClass lab-gateway-class  Accepted=True (Accepted)
      Gateway default/lab-gateway  Accepted=True (Accepted) Programmed=True (Accepted)
      HTTPRoute default/echo-route via default/lab-gateway#http  Accepted=True (Accepted) ResolvedRefs=True (Accepted)
      traffic: probe #0 -> HTTP 200 from echo-v1
      traffic: probe #1 -> HTTP 204 from echo-v1
    candidate: converged in 4180ms
      GatewayClass lab-gateway-class  Accepted=True (Accepted)
      Gateway default/lab-gateway  Accepted=True (Accepted) Programmed=True (Accepted)
      HTTPRoute default/echo-route via default/lab-gateway#http  Accepted=True (Accepted) ResolvedRefs=True (Accepted)
      traffic: probe #0 -> HTTP 503 from echo-v1

Inconclusive  1
  crd-custom-resource
    candidate: the candidate cluster's CRD does not accept server-side dry-run

Stale expectations  1
  sidecar-injection-rollout: no matching change was observed in this run

Diagnostics  2
  metrics.unavailable: per-webhook latency metrics were not scraped on the candidate side
  kubeconfig.loaded: loaded isolated kubeconfigs for both sides

Stage timings
  clusters 43.51s (baseline 41.20s, candidate 43.12s), install 96.40s, capture 6.12s (baseline 5.94s, candidate 6.11s) [4 fixture(s)/side], gateway 9.74s (baseline 9.40s, candidate 9.73s), compare 0.21s, elapsed 149.01s

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
| `0` | Completed and passed | No unexpected critical change. **Warnings still exit `0`** — there is no separate "completed with warnings" code, and folding warnings into `1` would fail every job on a difference a human merely ought to see. |
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

**This is the single most important semantic limitation of the admission
engine. Read it before you trust a green run.**

Admission Lab replays every admission fixture as a Kubernetes **server-side
dry-run CREATE** (`?dryRun=All`) against a real API server. This is
authoritative: it runs the real admission chain — real mutating
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

Fixture execution is **serial within each cluster**, which is what makes
audit-log correlation deterministic. Baseline and candidate run concurrently
with each other because the two clusters are isolated.

Gateway behavior is different in kind — a controller cannot reconcile an object
that was never persisted — so the `gateway:` suite is the roadmap's own explicit
exception to this rule, and applies its manifests **for real**. What makes that
safe is that the cluster is disposable and the client is built only from that
cluster's own kubeconfig; Admission Lab never applies them anywhere else. See
below.

---

## Gateway: three layers of evidence

A `gateway:` section in your lab configuration adds a second engine. It observes
Gateway API behavior as **three separate kinds of evidence**, and never lets one
stand in for another:

| Layer | The question it answers |
| --- | --- |
| **Admission** | What did the API server decide about each fixture object? |
| **Reconciliation** | What did the implementation publish in `status` — which conditions, with which reasons, and did they settle? |
| **Traffic** | What did a real HTTP request through the real data plane get back, and which backend answered it? |

They are sibling sections in `result.json` (`admission`,
`gatewayReconciliation`, `traffic`), always written, so "there was no Gateway
suite" and "the suite produced nothing" are never the same shape.

**Three things are worth knowing before you read a Gateway report:**

**A status is *converged* when it has stopped changing, not when it is good.** A
route the implementation has definitively rejected has finished reconciling: the
evidence says `converged: true` with a `False` condition, and that is correct.
Convergence is decided by observing the required conditions settled — `True` or
`False`, never `Unknown`, never missing — with a **current `observedGeneration`**,
identically across **two consecutive polls at least 250 ms apart**. A status
whose `observedGeneration` lags its object's `generation` is *stale*: it
describes a spec that has since changed, so it cannot converge, it raises a
diagnostic, and it stops the comparator from reading an absent condition as a
removed one.

**`Programmed: True` does not prove traffic works.** It is the controller's
statement about its own bookkeeping — this Gateway has been configured into the
data plane, as far as the implementation can tell. It does not say that a
backend exists, that it is ready, that your route's rules match, or that a
request would be answered by the workload you meant. That is why the traffic
probe is a *separate* layer of evidence rather than something inferred from
conditions: the only thing that establishes a route carries traffic is a request
that got an answer.

**A probe that could not be sent is a skip with a reason, never a failure and
never silence.** Probes are sent only when the `Gateway` is `Programmed` and the
contract's own route parent is both `Accepted` and `ResolvedRefs`. Otherwise the
report records the exact condition, state and controller reason that stopped it
— `Programmed=False (AddressNotAssigned)`, say — because probing anyway would
record the data plane's own error page (Gateway API specifies `503` for an
unaccepted route, `500` for an unresolved backend) and then compare that
invented status against a real one. If the other side *did* answer, that
difference is reported as a `traffic_status_changed` finding, which is the true
claim: the baseline answered a probe the candidate did not.

[`docs/architecture.md` §7](docs/architecture.md#7-the-gateway-engine) is the
full engine description — apply ordering, persisted-fixture isolation, endpoint
resolution, the port-forward, and the probe contract.
[`docs/config.md`](docs/config.md#gateway) is the field reference.

---

## Schemas

Three versioned document families, all frozen at `v1beta1` and all checked in:

| Document | `apiVersion` | Schema |
| --- | --- | --- |
| Lab configuration | `admissionlab.io/v1beta1` | [`schemas/admissionlab-v1beta1.json`](schemas/admissionlab-v1beta1.json) |
| Lab configuration (previous, still readable) | `admissionlab.io/v1alpha1` | [`schemas/admissionlab-v1alpha1.json`](schemas/admissionlab-v1alpha1.json) |
| Result (`result.json`) | `admissionlab.io/result/v1beta1` | [`schemas/result-v1beta1.json`](schemas/result-v1beta1.json) |
| Run manifest (`run.json`) | `admissionlab.io/run-manifest/v1beta1` | [`schemas/run-manifest-v1beta1.json`](schemas/run-manifest-v1beta1.json) |
| Run manifest (previous, still readable) | `admissionlab.io/run-manifest/v1alpha1` | [`schemas/run-manifest-v1alpha1.json`](schemas/run-manifest-v1alpha1.json) |

Point your editor at the configuration schema and a wrong `apiVersion` or a
misspelled key is flagged as you type.

[`docs/schema-migrations.md`](docs/schema-migrations.md) is the contract: which
version each document is at, what an older one still means, how a reader must
behave on a version it does not know, and a note for every version step this
project has taken.

**`expectations.yaml` is the one exception**, and deliberately so: the
`Expectations` document versions independently of the `Lab` one, has not been
promoted, and is still `admissionlab.io/v1alpha1`. Changing it to match the lab
file beside it is a configuration error.

---

## Documentation

| Document | What is in it |
| --- | --- |
| [`docs/architecture.md`](docs/architecture.md) | As-built crate map and dependency rules, the run pipeline's stages, the evidence model, audit correlation, why fixture execution is serial, and the Gateway engine end to end (§7) |
| [`docs/compatibility.md`](docs/compatibility.md) | Certified vs supported vs merely configurable: the certified table, what a certification asserts, the CI tiers, the three-minors rule, and what happens on a combination nobody certified |
| [`docs/config.md`](docs/config.md) | Full `admissionlab.yaml` `v1beta1` reference: every field, every default, path resolution, `gateway`, `policy`, overrides, `expectations.yaml`, and how a `v1alpha1` file still loads |
| [`docs/fixtures.md`](docs/fixtures.md) | Fixture format, discovery globs, identity and hashing, the setup-outside-the-glob pattern, and the dogfood webhook's annotation vocabulary |
| [`docs/github-action.md`](docs/github-action.md) | The composite action: pinned/checksummed installation, every input, the artifacts it uploads on a failing run, exit-code behavior, and what the job summary says |
| [`docs/recipes.md`](docs/recipes.md) | What a recipe is, the pins each built-in recipe carries, the capability model, override directories, and why recipes may never classify regressions |
| [`docs/schema-migrations.md`](docs/schema-migrations.md) | The three versioned document families, the pre-v1.0 compatibility rule, how a reader must behave on a version it does not know, and the migration note for every version step |
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

[`examples/ingress-to-gateway/`](examples/ingress-to-gateway/) is the migration
counterpart, and the only example whose two sides run *different* stacks: the
archived community `ingress-nginx` on the baseline, NGINX Gateway Fabric on the
candidate, asked the same two HTTP questions. It demonstrates one behavior
preserved, one non-portable feature accepted in writing
(`nginx.ingress.kubernetes.io/limit-rps`, which Gateway API v1 cannot express),
and one unintended regression — a hand-written `HTTPRoute` that accepts both
hostnames and sends both to the same backend, where the `Ingress` served one
from each. It exits `1` naming the *observed* backends rather than a manifest
difference, which is the whole point: the route reconciles cleanly, every probe
returns `200`, and no status, condition or manifest diff says anything is
wrong.

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
