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

`--report-dir <DIR>` relocates `result.json` and `report.html` only. Raw evidence
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

## Still stuck

- Re-run with `-v` (or `RUST_LOG=debug`, which always wins) and read the
  diagnostics section of the report — it is written to say what could and could
  not be observed.
- Reproduce with `--keep-clusters` and inspect the cluster directly.
- Open an issue with the `result.json` or `diagnostics.json`, your
  `admissionlab.yaml`, and the `admissionlab doctor` output. For anything you
  believe is a security issue, follow [`SECURITY.md`](../SECURITY.md) instead.
