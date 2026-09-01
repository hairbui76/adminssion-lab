# Troubleshooting

Real failure modes, keyed to the exit code you got.

Start here, always:

```bash
admissionlab doctor
admissionlab -v test admissionlab.yaml     # Admission Lab's own crates at debug
RUST_LOG=debug admissionlab test admissionlab.yaml   # RUST_LOG always wins over -v
```

---

## Contents

- [Exit code quick reference](#exit-code-quick-reference)
- [Exit 2 — configuration, fixtures, or prerequisites](#exit-2--configuration-fixtures-or-prerequisites)
- [Exit 3 — infrastructure](#exit-3--infrastructure)
- [Exit 4 — installation and readiness](#exit-4--installation-and-readiness)
- [Exit 5 — fixture execution](#exit-5--fixture-execution)
- [Results that look wrong](#results-that-look-wrong)
- [Keeping clusters and cleaning up by hand](#keeping-clusters-and-cleaning-up-by-hand)
- [Where the evidence lives](#where-the-evidence-lives)
- [The forced-failure catalog](#the-forced-failure-catalog)

---

## Exit code quick reference

| Code | Meaning |
| ---: | --- |
| `0` | Passed. Warnings still exit `0`. |
| `1` | Completed; the regression policy failed. |
| `2` | Invalid configuration, invalid fixture definition, or a missing host prerequisite. |
| `3` | Lab infrastructure failure — including a failed cleanup. |
| `4` | Installation or readiness failure. |
| `5` | Fixture execution or capture failure. |
| `6` | Internal Admission Lab error — please report it. |

Everything in the `2` class is checked **before any cluster is created**, so
those failures are fast and cost nothing.

---

## Exit 2 — configuration, fixtures, or prerequisites

### `doctor` reports a missing tool

```text
  kind: not found
```

Install it. Admission Lab shells out to `kind`, `kubectl`, `helm`, and `docker`
and does not vendor any of them. Note that `admissionlab test` performs the same
check and returns the **same** exit `2` — the two commands are not allowed to
disagree about what "you have not installed `kind`" means.

### `doctor` reports the Docker daemon is unreachable

```text
  docker daemon: unreachable
```

- Is the daemon running? `docker info`
- Are you in the `docker` group, or is the socket readable? A rootless or
  Colima/Podman setup needs `DOCKER_HOST` pointed at the right socket.
- On WSL2, is Docker running *inside* WSL2, or is it Docker Desktop with WSL2
  integration enabled for this distribution?

### `doctor` warns about kubectl version skew

```text
  kubectl: found (v1.32.11) - kubectl client v1.32.11 is 3 Kubernetes minor
  versions away from Admission Lab's supported range ...
```

This is a **warning**, not a failure — prerequisites are still met. Kubernetes
tolerates ±1 minor of client/server skew; further out, `kubectl apply` against a
provisioned cluster can fail in confusing ways during component installation.
Upgrade `kubectl` if you see unexplained install failures.

### `doctor` warns about disk space

Below roughly 10 GiB free on the run root's filesystem you get a warning. It is
advisory and does not fail the gate — but two `kind` clusters plus node images
plus chart images add up, and running out mid-run fails much less clearly than a
warning up front.

### `unknown field \`candiate\``

Every mapping in the configuration is parsed strictly. The error names the file,
the field, and the line. Check spelling and `camelCase`: `expectationsFile`,
`failOn`, `valuesFiles`, `setValues`, `repoName`, `releaseName`,
`absoluteIncrease`, `relativeMultiplier`, `objectPath`, `fixtureGlob`.

### `"3.9" is not an exact pinned version`

Helm chart versions must be exact `MAJOR.MINOR.PATCH` pins (an optional `v`
prefix and `-prerelease`/`+build` suffixes are allowed). `latest`, `^3.9`,
`>=3.9`, `~1.2.3`, `1.2.x`, `3`, and `3.9` are all rejected: Helm expands them
into ranges, so baseline and candidate could silently install different charts
and make the comparison meaningless.

### The Kubernetes version cannot be provisioned

Admission Lab resolves `kubernetes:` against `compatibility/kubernetes.yaml`,
which pins an exact patch version and node-image digest per supported minor. It
never fetches this over the network. If your version is not in that file, the
error says so and names the minor — including the specific "no longer supported"
case for a minor that was recently valid.

### `no fixtures matched`

The include globs selected nothing, so there is nothing to replay.

- Globs resolve against **the configuration file's directory**, not your working
  directory. `cd`-ing elsewhere does not change them; being *in* the wrong
  directory when you wrote them does.
- Symlinked files and symlinked directories are never followed.
- Check with `ls` from the config's own directory.

### A fixture is rejected at discovery

| Error | Fix |
| --- | --- |
| `metadata.name` missing | Every fixture needs a non-empty string name. A present-but-empty or non-string name reports identically. |
| `apiVersion` / `kind` missing | Same rule. |
| `generateName` is not supported | Alpha requires deterministic names. Write an explicit `metadata.name`. |
| duplicate fixture ID | Two documents slug to the same ID — the error names both files and both document indices. Rename one. Remember slugging is lossy: `a.b` and `a-b` collide. |
| not an object | A document parsed to an array, a string, or a scalar. A YAML list at the top level is a common cause. |

### An unknown `policy.failOn` or `severity` name

Names must match exactly the wire strings a JSON report prints — a name copied
out of a report always works. Near misses (`image_change` for `image_changed`)
are rejected loudly rather than silently matching nothing forever. Severities are
`info`, `warning`, `critical`, case-sensitive. The full table is in
[`docs/config.md`](config.md#semantic-change-kinds-and-default-severities).

### `expectationsFile` not found

A missing expectations file is exit `2`, not an infrastructure error: you named
the path, so it is your configuration at fault. It resolves against the
configuration file's directory.

---

## Exit 3 — infrastructure

### `kind` cannot create a cluster

Usual suspects, in order:

1. **Docker is out of disk or memory.** `docker system df`, then
   `docker system prune`. Node images are large.
2. **Leftover clusters from a killed run.** `kind get clusters | grep '^adlab-'`
   and delete them — see [below](#keeping-clusters-and-cleaning-up-by-hand).
3. **Too many inotify watches.** A classic on Linux with several `kind` clusters:
   the control plane fails in obscure ways. Raise
   `fs.inotify.max_user_watches` and `fs.inotify.max_user_instances`.
4. **Node image pull failed.** Admission Lab requests a digest-pinned image; a
   proxy or registry mirror that cannot serve it fails here.

### Cleanup failed

A run whose clusters could not be deleted **never exits `0`** — a passing run is
downgraded to `3`, because `0` is a positive claim that the run completed
cleanly, and a machine left with two clusters running has not.

A run that already failed keeps its more specific code; the leaked clusters are
still reported loudly on stderr with the exact `kind delete cluster --name`
command for each.

### The report directory is not writable

`--report-dir` is created if it does not exist, but a read-only path or a full
disk fails here.

---

## Exit 4 — installation and readiness

### A component timed out

Each component gets **600 seconds** to install and become ready. This is not
configurable in Alpha.

- On a cold machine, image pulls dominate. Pre-pulling the chart's images, or
  simply re-running once the layers are cached, often resolves it.
- A chart whose default values need a `LoadBalancer`, a `StorageClass`, or
  `metrics-server` may never become ready on a bare `kind` cluster. Check the
  chart's requirements; consider `setValues` overrides.

### `helm` failed

The whole error chain is rendered, so the `helm` exit status or the Kubernetes
validation message reaches you rather than being swallowed by a wrapper. Common
causes:

- **The chart reference is not in `<repoName>/<chartName>` form.** A bare
  `kyverno` fails with *"non-absolute URLs should be in form of
  repo_name/path_to_chart"*. `repoName` defaults to the component's `name`, so
  the chart reference must start with that name unless you set `repoName`
  explicitly.
- **The namespace default is wrong.** `namespace` defaults to the component's
  `name`, which is right surprisingly often and wrong in exactly the cases that
  matter — `istio/istiod` conventionally installs into `istio-system`. Set it
  explicitly.
- **CRDs from a chart you omitted.** A chart that assumes a companion chart's
  CRDs fails at apply time.

### The component installed but nothing is being tested

See [Results that look wrong](#a-run-passes-but-nothing-was-actually-compared).

---

## Exit 5 — fixture execution

### `serviceaccount "default" not found`

**The single most common first-run failure.** A freshly created cluster's
namespaces are genuinely bare for a moment, and a namespace that does not exist
has no `default` ServiceAccount at all.

The root cause is almost always that **the fixture's namespace was never
created**. A `Namespace` document replayed as a fixture does not create anything
— a dry-run CREATE persists nothing — so every pod fixture that follows fails.

The fix, in full, is [the setup-outside-the-glob
pattern](fixtures.md#setup-manifests-live-outside-the-glob):

1. Keep namespace and ServiceAccount manifests **outside** your include glob —
   name them `00-*.yaml` while fixtures are `pod-*.yaml`, and include only
   `pod-*.yaml`.
2. Apply that setup for real into both clusters, either yourself before the run
   or as a `type: manifests` component so Admission Lab applies it during
   installation.

If your fixtures reference a non-`default` ServiceAccount, create that too — the
same rule applies.

### `connection refused` calling a webhook

```text
Internal error occurred: failed calling webhook "...": failed to call webhook: ...
```

The webhook's Service or pod is not serving yet. With `failurePolicy: Fail` the
API server turns that into a rejection.

**The fix is almost always a missing `readiness` list on the component.**
`helm upgrade --install` returns as soon as the release is applied; it does not
wait for a controller to be serving. Add the checks the component actually needs
— see [`docs/config.md`](config.md#readiness):

```yaml
readiness:
  - type: deploymentAvailable
    namespace: kyverno
    name: kyverno-admission-controller
  - type: webhookConfigurationPresent
    name: kyverno-resource-validating-webhook-cfg
```

Two things that trip people up even with a readiness list:

- Some controllers create their webhook configurations **at runtime**, after the
  Deployment is up — Kyverno does this for both resource-facing configurations —
  so gate on the configurations, not only the Deployment.
- Each such configuration starts with an empty `webhooks: []` list, so its
  existence is not proof that your policy is enforced. Install policies as a
  later `type: manifests` component and wait for each with
  `customResourceCondition`.

Note that this is *not* an unsupported fixture: dry-run worked exactly as
intended and observed a real rejection. It is a comparable outcome, and if it
happens on one side only you will see `webhook_failed` graded **critical**.

### `unsupported resource` for a CRD

The fixture's `apiVersion`/`kind` did not resolve against the cluster's
discovered API surface. Two indistinguishable causes, and the error says so
rather than claiming certainty: the resource is genuinely absent, or its CRD was
installed after API discovery was cached.

Make sure the CRD is installed as a component (so it lands during the install
stage, before capture) rather than applied by hand mid-run.

### `could not execute dry-run CREATE`

No response could be obtained at all — an unusable kubeconfig, a transport
failure, or a 2xx body that did not decode as JSON. This is never an admission
decision. Check that the cluster is still alive: a `kind` node that was OOM-killed
mid-run presents exactly this way.

---

## Results that look wrong

### A run passes but nothing was actually compared

Check the summary. If `identical` equals the fixture count and there are no
changes anywhere, consider whether **either stack was actually doing anything**.
An API server that never called a webhook admits every fixture unchanged, and
that is indistinguishable from two stacks that agree — except that it happens on
*both* sides.

The usual cause is a component with **no `readiness` list**: install returned,
fixtures replayed, and the controller was still starting. Add readiness checks
(see [`docs/config.md`](config.md#readiness)).

The other usual cause is a webhook that was never routed to your fixtures — a
`namespaceSelector` or `objectSelector` the fixture does not match. Confirm by
looking at the trace evidence: a fixture that was genuinely processed records
webhook invocations. A run with `unknown` evidence everywhere and no invocations
is a run where nothing was in the admission path.

### `warnings` but exit `0`

By design. Alpha has no separate "completed with warnings" exit code, and
folding warnings into `1` would fail every CI job on a difference a human merely
ought to look at. The warnings are in the terminal summary, in `result.json`, and
in the HTML report. To make a category fail, add it to `policy.failOn`.

### `inconclusive` fixtures

A side could not be compared at all. This is a third state, deliberately never
folded into "agreed", so an empty change list can never be misread as agreement.
The report carries the side's own verbatim reason.

### First divergence says `unknown` or `partial`

Missing evidence is reported as missing. Admission Lab will not name a first
cause it did not observe.

The usual reason is that the audit evidence needed to attribute the change was
not available for that fixture. Check the run's diagnostics — they say what
could and could not be observed. Note also that Secret-touching requests are
excluded from the audit log entirely, by policy, so a fixture involving Secrets
has less trace evidence available by construction.

### Per-webhook latency is missing

Latency is an **optional** signal. Missing or ambiguous metrics are reported as
unavailable and never as zero, and never fail a run on their own. A
`metrics.unavailable` diagnostic tells you which side.

### A stale expectation

An entry in your `expectations.yaml` matched nothing this run. Usually the
change it accounted for has been fixed and the entry can be deleted. It does not
fail the run.

---

## Keeping clusters and cleaning up by hand

Clusters are deleted by default, on success and on failure. To keep them:

```bash
admissionlab test --keep-clusters admissionlab.yaml
```

which prints exactly what you need:

```text
Clusters preserved (--keep-clusters was set); nothing was deleted.
  baseline cluster "adlab-baseline-01hq..."
    kubeconfig: /tmp/admissionlab-runs/<run-id>/kubeconfigs/baseline.kubeconfig
    delete with: kind delete cluster --name adlab-baseline-01hq...
```

Then poke at it:

```bash
export KUBECONFIG=/tmp/admissionlab-runs/<run-id>/kubeconfigs/candidate.kubeconfig
kubectl get pods -A
kubectl get validatingwebhookconfigurations,mutatingwebhookconfigurations
kubectl logs -n kyverno deploy/kyverno-admission-controller
```

### Cleaning up after a killed run

Every Admission Lab cluster is named `adlab-<side>-<short-run-id>`:

```bash
kind get clusters | grep '^adlab-'
kind get clusters | grep '^adlab-' | xargs -r -n1 kind delete cluster --name
```

Workspaces are not garbage-collected:

```bash
ls "${TMPDIR:-/tmp}/admissionlab-runs"
rm -rf "${TMPDIR:-/tmp}/admissionlab-runs"
```

And if a killed run left containers behind without a `kind` cluster entry:

```bash
docker ps -a --filter 'name=adlab-'
```

---

## Where the evidence lives

```text
${TMPDIR:-/tmp}/admissionlab-runs/<run-id>/
  raw/<side>/<fixture-id>/     request, response, audit window, metric samples  (mode 0700)
  normalized/                  normalized objects
  reports/result.json          the machine-readable result
  reports/report.html          the standalone HTML report
  reports/diagnostics.json     written instead of result.json when a run fails after install
  logs/                        diagnostic logs
  kubeconfigs/                 per-cluster kubeconfigs  (mode 0700, files 0600)
  run.json                     run metadata
```

`--report-dir <DIR>` relocates the rendered reports — `result.json`,
`report.html`, and the `diagnostics.json` a failed run writes instead of a
`result.json` — and nothing else. Raw evidence
always stays in the run workspace, which is where every path inside the reports
points.

A run that fails at or after installation writes `diagnostics.json` — the failed
stage plus every diagnostic collected up to that point — **before** cleanup runs,
so the evidence survives the clusters. It deliberately does not write a
`result.json`: a run that never compared both sides has not earned a verdict.

**Raw evidence is not redacted.** It is protected by file permissions, not by
the report redaction pass. Read it before attaching it to a public issue; the
rendered reports are what is intended for sharing. See
[`docs/security.md`](security.md#what-redaction-does-not-cover).

---

## The forced-failure catalog

Four failures that Admission Lab deliberately breaks itself with every night,
in `.github/workflows/nightly.yml`. They are catalogued here because they are
the four you are most likely to hit for real, and because a failure mode that is
tested on a schedule should be one you can look up rather than one you have to
reverse-engineer from a stack trace.

Each entry says what the failure looks like, which exit code it produces, where
the evidence lands, and what to do about it. What the nightly suite asserts is
listed too — if your symptom does not match, that difference is itself
information.

| Failure | Exit | Evidence written | Clusters afterward |
| --- | ---: | --- | --- |
| [Install timeout](#a-component-never-becomes-ready-install-timeout) | `4` | `diagnostics.json`, `run.json` (stage `installation`) | deleted |
| [Webhook timeout](#a-webhook-answers-too-late-webhook-timeout) | `0` or `1` — this is a *result*, not a run failure | full reports; the fixture's `raw/` bundle | deleted |
| [`kind` failure](#kind-itself-fails-infrastructure-failure) | `3` | `run.json` (stage `cluster_creation`); no reports | none created, or deleted |
| [Artifact write failure](#an-artifact-cannot-be-written) | `3` (or `6`) | the error itself, on stderr | deleted |

In every one of the four, **no `adlab-*` cluster survives**. That is PRODUCT.md
§33's "no leaked cluster after normal failure paths", and it is the one
assertion every nightly job ends with. If you ever see one of these failures
leave a cluster behind, that is a bug worth reporting on its own — attach the
output of `kind get clusters`.

You can run the leak check yourself, any time:

```bash
./scripts/verify-cleanup.sh --check-only
```

It exits `0` when nothing is present, `1` (printing the exact
`kind delete cluster --name` line for each) when something is, and `3` when
`kind` or `docker` is missing — never conflating "no clusters" with "could not
ask".

### A component never becomes ready (install timeout)

```text
admissionlab: both stacks failed to install: baseline: component "never-ready"
  failed to install: component "never-ready" did not become ready in time:
  DeploymentAvailable { namespace: "nightly-never-ready", name: "never-ready" }
  was never satisfied (waited 599.865215718s); candidate: ...
```

The message names the component, the side, the specific readiness check that was
never satisfied, and how long it waited. When only one side fails, only that side
appears.

**Exit `4`.** Each component gets 600 seconds to install *and* satisfy every
one of its `readiness` checks; see
[Exit 4 — a component timed out](#a-component-timed-out) for the usual causes.

**Where the diagnostics land.** `diagnostics.json` in your `--report-dir` (or
the run's own `reports/`), written **before** cleanup so it outlives the
clusters, plus `run.json` in the run workspace with `status: failed` and
`stage: installation`. There is deliberately **no** `result.json`: the run never
compared both sides, so it has no verdict to state.

**What to do.** Read `diagnostics.json` first — it names the component and the
side. Then re-run with `--keep-clusters` and look at the workload directly:

```bash
admissionlab test --keep-clusters admissionlab.yaml
export KUBECONFIG=<the printed candidate kubeconfig>
kubectl -n <namespace> get deploy,pods
kubectl -n <namespace> describe pod <the pending one>
```

`ImagePullBackOff` (a private registry, a mirror that cannot serve the tag), a
`Pending` pod (a `LoadBalancer` Service or a `StorageClass` a bare `kind`
cluster does not have), and a `CrashLoopBackOff` all present identically at the
Admission Lab level and completely differently under `describe`.

**What nightly forces.** A `manifests` component whose Deployment references
`registry.admissionlab.invalid/never-exists:0.0.0` — the `.invalid` TLD can
never resolve — with a `deploymentAvailable` readiness check on it. The apply
succeeds; the readiness wait spends the whole 600-second budget. The job asserts
exit `4`, that `diagnostics.json` exists, that `result.json` does *not*, and
that no cluster leaked.

### A webhook answers too late (webhook timeout)

```text
Internal error occurred: failed calling webhook "...": failed to call webhook:
Post "https://...?timeout=10s": context deadline exceeded
```

**Not a run failure.** The run exits `0` or `1` on its policy verdict like any
other. A webhook that never answered is a real, observed admission outcome: with
`failurePolicy: Fail` the API server rejects the request, and Admission Lab
records `AdmissionDecision::Rejected` for that fixture. If it happens on one
side only, you will see it graded `webhook_failed`, **critical**.

This is deliberately distinguishable from two neighbours, and the difference is
in the evidence rather than in anything Admission Lab assumes:

| | Status | Message | Elapsed |
| --- | --- | --- | --- |
| A denial | `403` | "... denied the request: ..." | fast |
| A call failure | `500` | "failed calling webhook ..." | fast |
| A **timeout** | `500` | "failed calling webhook ... context deadline exceeded" | at least the webhook's `timeoutSeconds` |

**Where the diagnostics land.** The fixture's own evidence bundle:
`raw/<side>/<fixture-id>/response.json` for the API server's verbatim answer and
`outcome.json` for the decision and the measured `total_latency`. The latency is
what tells a timeout apart from an immediate call failure.

**What to do.** A webhook that times out under a lab's load and not under yours
is usually a readiness problem rather than a latency problem — see
[`connection refused` calling a webhook](#connection-refused-calling-a-webhook),
whose fix (a real `readiness` list on the component) is the same. If the webhook
genuinely is slow, raise its `timeoutSeconds` in *its own* configuration; there
is nothing to configure on the Admission Lab side, because the timeout is the
API server's and the lab is only reporting it.

**What nightly forces.** `fixtures/core/admission/pod-timeout.yaml` asks the
dogfood webhook for a 15 000 ms delay against its own `timeoutSeconds: 10`
(`recipes/test-webhook`). The capture test asserts a rejection that took at least
ten seconds, that names a webhook call failure, and that is *not* reported as a
denial.

### `kind` itself fails (infrastructure failure)

```text
admissionlab: failed to prepare lab clusters: both clusters failed to create:
  baseline: `kind create cluster --name adlab-baseline-... --config ...
  --kubeconfig ...` exited with exit status: 1 (cleanup deleted the cluster)
```

**Exit `3`.** The message carries the whole argv, so you can run the failing
command yourself. `(cleanup deleted the cluster)` at the end means a partially
created cluster was torn down; if cleanup had *also* failed, that would be said
here too, with the manual `kind delete cluster` command.

**Where the diagnostics land.** `run.json` in the run workspace, with
`status: failed` and `stage: cluster_creation`. No `result.json` and no
`diagnostics.json`: nothing was installed and nothing was captured, so there is
nothing yet to write into them. The console message *is* the diagnostic here,
which is why it names the command rather than summarizing it.

**What to do.** See [Exit 3 — `kind` cannot create a
cluster](#kind-cannot-create-a-cluster) for the four usual causes, in order.
Re-running the printed command by hand is the fastest way to see `kind`'s own
output, which Admission Lab bounds and captures but does not interleave into its
own progress lines.

**What nightly forces.** A `kind` shim on `PATH` that fails `create cluster` and
forwards everything else to the real binary — the failure is injected at the
process boundary, which is where the real one lives (Global Constraint 2: `kind`
is a bounded subprocess, not something Admission Lab reimplements). The job
asserts exit `3`, that `run.json` records `failed`/`cluster_creation`, that the
console names the failing command, and that nothing leaked.

### An artifact cannot be written

```text
failed to create temporary file
  `/tmp/admissionlab-runs/<run-id>/reports/.result.json.tmp-<uuid>`: No space
  left on device
```

**Exit `3`** for an I/O failure. (Exit `6` is reserved for the two shapes that
cannot be your fault: a path that escapes the store root, and a value that fails
to serialize. Both mean a bug in Admission Lab — please report them.)

Every artifact is written atomically: into a temporary sibling file, synced, and
renamed into place. So a failed write never leaves a half-written `result.json`,
never leaves a stray temporary file, and never damages a file that was already
there. The error names the operation that failed and the exact path it was
acting on.

**Where the diagnostics land.** On stderr, and nowhere else — writing a
diagnostic file is the thing that just failed. Read the message: it distinguishes
"create temporary file" (the destination directory is unwritable, full, or not
actually a directory) from "rename temporary file into place at" (something else
holds that name) from "sync temporary file" (the filesystem rejected the write
after accepting it, typically ENOSPC on a sparse or network mount).

**What to do.** Check free space and permissions on **both** roots, which are
usually different filesystems:

```bash
df -h "${TMPDIR:-/tmp}" .          # the run workspace, and --report-dir
ls -ld "${TMPDIR:-/tmp}/admissionlab-runs"
```

`admissionlab doctor` warns below roughly 10 GiB free on the run root before a
run starts, which is the check that exists to stop you discovering this at
minute nine of a lab.

**What nightly forces.** Not a real disk failure — a real one is not
reproducible on demand, and a mocked `io::Error` would only prove that the code
propagates an error somebody handed it. Instead
`crates/admissionlab-core/tests/artifact.rs` puts the filesystem into two states
the OS is *required* to reject (a destination whose parent is a regular file, and
a destination that is an existing directory) and asserts, for each, the error
variant, that it names the operation and the path, and that nothing is left
behind.

---

## Still stuck

- Re-run with `-v` (or `RUST_LOG=debug`, which always wins) and read the
  diagnostics section of the report — it is written to say what could and could
  not be observed.
- Reproduce with `--keep-clusters` and inspect the cluster directly.
- Open an issue with the `result.json` or `diagnostics.json`, your
  `admissionlab.yaml`, and the `admissionlab doctor` output. For anything you
  believe is a security issue, follow [`SECURITY.md`](../SECURITY.md) instead.
