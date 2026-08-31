# Fixtures

A fixture is an ordinary Kubernetes object. Admission Lab replays each one as a
**server-side dry-run CREATE** against both clusters and compares what the two
admission chains returned.

---

## Contents

- [Format](#format)
- [Fixture IDs](#fixture-ids)
- [Discovery](#discovery)
- [Content hashing](#content-hashing)
- [Setup manifests live outside the glob](#setup-manifests-live-outside-the-glob)
- [The dry-run contract](#the-dry-run-contract)
- [The dogfood webhook's annotation vocabulary](#the-dogfood-webhooks-annotation-vocabulary)

---

## Format

A fixture file is a YAML (or JSON — JSON is valid YAML, and nothing here
branches on file extension) document stream. Each non-empty document is one
fixture.

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: web-frontend
  namespace: admissionlab-fixtures
  labels:
    app: web
spec:
  containers:
    - name: app
      image: registry.k8s.io/pause:3.10
```

Every document must satisfy, in this order:

| Requirement | Failure |
| --- | --- |
| The document is a mapping | `NotAnObject` — the error names what was found instead (`an array`, `a string`, …) |
| `apiVersion` is a non-empty string | `MissingField { field: "apiVersion" }` |
| `kind` is a non-empty string | `MissingField { field: "kind" }` |
| `metadata.name` is a non-empty string | `MissingField { field: "metadata.name" }` |

`metadata.namespace` is optional; when present and non-empty it is folded into
the fixture ID. At replay time the object's own `metadata.namespace` selects the
namespace, falling back to `default` when absent.

A field that is present but is not a non-empty string is treated exactly as if
it were absent — a missing `metadata` block, a `metadata` that is not a mapping,
an empty-string name, and a numeric name all produce the same
`metadata.name` error.

### `generateName` is rejected

```yaml
metadata:
  generateName: web-   # rejected
```

Alpha requires a deterministic `metadata.name` on every fixture: no name-rewrite
contract exists yet that could make a generated name reproducible across runs,
and a fixture whose identity changes every run cannot be compared between two
clusters. If both `name` and `generateName` are set, `name` wins and the
document is accepted.

### Multi-document files

Documents are indexed by their **zero-based position in the raw stream**. A
document that parses to nothing — a comment-only section, a bare `---` — is
skipped rather than rejected, and **skipped documents are not renumbered**. So
in a file whose first document is empty, the first real fixture is at
`document_index` 1.

All discovery failures stop the run immediately, before any cluster exists, with
exit `2`.

---

## Fixture IDs

The ID is what appears in the report, what `policy.overrides[].fixtures` and
`expectations[].fixtures` globs match against, and what names the raw evidence
directory. It is derived, never written by hand:

```text
<slug of path relative to the config dir>-<slug of kind>[-<slug of namespace>]-<slug of name>-<document index>
```

`slug` lowercases and collapses every run of characters outside `[a-z0-9]` into
a single `-`, trimming leading and trailing dashes. **The file extension is not
stripped.** So `fixtures/pods/web.yaml`, document 0, holding a `Pod` named
`web-frontend` in namespace `admissionlab-fixtures`, gets:

```text
fixtures-pods-web-yaml-pod-admissionlab-fixtures-web-frontend-0
```

The ID depends only on the *relative* path, so it is identical whether the repo
is checked out at `/home/you/lab` or `/build/workspace`. It contains no absolute
path, no clock, and no randomness.

Slugging is lossy on purpose — `a.b` and `a-b` both slug to `a-b` — so two
different fixtures can collide. That is detected, not tolerated: a duplicate ID
fails the run with exit `2`, naming both files and both document indices.

---

## Discovery

```yaml
fixtures:
  include:
    - "fixtures/core/admission/pod-*.yaml"
    - "fixtures/mesh/**/*.yaml"
```

- **Root.** Globs are matched against each file's path relative to **the
  directory containing your `admissionlab.yaml`**. The working directory is
  never consulted.
- **OR, not AND.** A file is selected if at least one pattern matches. There is
  no `exclude` list in Alpha.
- **`*` matches `/`.** These are `globset` patterns with the separator not
  treated as literal, so `fixtures/*.yaml` also reaches nested files. Write a
  narrower pattern if that is not what you want.
- **Files only.** Directories are walked, not matched. Sockets, FIFOs, and
  device nodes are skipped.
- **Symlinks are never followed** — neither symlinked files nor symlinked
  directories. This avoids cycles and keeps every fixture physically inside the
  root.
- **Deterministic order.** Matched files are sorted by their relative path
  *string*, byte-lexicographically — not by path components. This means
  `a-b.yaml` sorts before `a/x.yaml` (`-` is `0x2D`, `/` is `0x2F`). Within a
  file, documents keep file order. Directory-read order is never relied on.
- **Non-UTF-8 paths.** A matched file whose relative path is not UTF-8 fails the
  run. An unrelated non-UTF-8 filename elsewhere under the root does not.
- **Zero matches** fails the run with exit `2`: with nothing to replay, no
  comparison can be produced.

---

## Content hashing

Each fixture carries the **SHA-256 of the whole file's raw on-disk bytes**, as
lowercase hex. It is computed once per file and shared by every fixture in that
file.

This is provenance and change detection — it is not keyed, not a MAC, and not an
authentication mechanism. The file-level granularity has one consequence worth
knowing: two fixtures in the same multi-document file cannot distinguish "my
content changed" from "my sibling's did".

---

## Setup manifests live outside the glob

**A dry-run CREATE persists nothing.** A `Namespace` fixture replayed under
dry-run does not create a namespace, so every pod fixture that follows would come
back `404 NotFound` — an outcome about Admission Lab's own setup, not about the
stack under test.

Alpha therefore has **no first-class fixture setup stage**. The convention that
works, and the one this repository's own corpus uses:

1. Name setup manifests so they fall **outside** your include glob. The
   checked-in corpus in `fixtures/core/admission/` puts setup in
   `00-namespace.yaml` and every replayable fixture in `pod-*.yaml`:

   ```yaml
   fixtures:
     include:
       - "fixtures/core/admission/pod-*.yaml"
   ```

2. Apply the setup for real — a genuine, non-dry-run `kubectl apply` — into both
   clusters before the run, or install it as a `type: manifests` component so
   Admission Lab applies it during the install stage.

A fresh cluster's `default` namespace is genuinely bare. The classic symptom of
skipping this step is `serviceaccount "default" not found`; see
[`docs/troubleshooting.md`](troubleshooting.md#serviceaccount-default-not-found).

Setup often carries more than existence. `fixtures/core/admission/00-namespace.yaml`
labels its namespace `admissionlab.dev/test-webhook: "enabled"`, which is the
`namespaceSelector` opt-in on all three of the dogfood webhook's configurations.
Without that label every fixture would be admitted by an API server that never
called a webhook — indistinguishable from a stack whose webhooks stopped
running.

The vendor smoke corpora use the same convention with a numeric prefix encoding
apply order: `fixtures/kyverno/smoke/00-namespaces.yaml` and
`10-validate-policy.yaml`/`20-mutate-policy.yaml` are setup; the `1x-`/`2x-`
pods are the fixtures.

---

## The dry-run contract

Each fixture becomes exactly one Kubernetes CREATE with `dryRun=All` and field
manager `admissionlab`. There is no persisted-CREATE fallback anywhere in the
code.

- **The object is sent byte for byte.** Admission Lab injects no label, no
  annotation, and no field into your fixture before submitting it.
- **A rejection is a successful observation.** A `403 Forbidden` from a policy
  engine, or a `500` from an unreachable webhook, is an ordinary captured
  outcome — one of the two things the whole pipeline exists to see. It is *not*
  a fixture failure.
- **API-server `Warning` headers are captured losslessly**, decoded permissively
  so none is silently dropped. An empty warnings list means zero were observed.
- **Execution is serial within each cluster** (Global Constraint 17), which is
  what makes audit-log correlation deterministic. The baseline and candidate
  clusters process their copies concurrently with each other because they are
  isolated.

### When a fixture cannot be evaluated

Global Constraint 16: a fixture that cannot be safely evaluated under
server-side dry-run **fails explicitly**. The semantics are never silently
switched.

| Situation | Result |
| --- | --- |
| The fixture's `apiVersion`/`kind` does not resolve on the cluster's discovered API surface | Fails the run, exit `5`. Two indistinguishable causes: the resource is genuinely absent, or its CRD was installed after discovery was cached. |
| The dry-run CREATE could not be issued at all — unusable kubeconfig, transport failure, a 2xx body that did not decode | Fails the run, exit `5`. |
| API discovery itself could not be queried | Fails the run, exit `5`. |

The result model additionally carries a distinct `unsupported_dry_run` decision.
When either side reports it, the two sides are marked **incomparable**, the
fixture lands in the `inconclusive` bucket, and no semantic changes are emitted —
a third state, precisely so an empty change list can never be misread as
agreement.

A note on scope, because it is easy to over-read: nothing produces
`unsupported_dry_run` on Kubernetes 1.35–1.37 today. The upstream trigger would
be a matching webhook declaring `sideEffects: Some` or `Unknown`, and the
`admissionregistration.k8s.io/v1` API — the only version those releases serve —
rejects both values outright at write time. The state and its incomparable
downstream handling exist so that the day a cluster *can* report it, the answer
is already correct.

See the [README's dry-run section](../README.md#server-side-dry-run-what-it-can-and-cannot-see)
for what dry-run cannot observe at all.

---

## The dogfood webhook's annotation vocabulary

Admission Lab ships its own deterministic admission webhook, installed by the
`recipes/test-webhook/` recipe. It exists so Admission Lab's own test suite
never breaks because a vendor changed behavior underneath it — and it is a
useful way to write a fixture with a *known* admission outcome.

A fixture opts into a behavior by carrying an annotation under
`test.admissionlab.io/`:

| Annotation | Value | Behavior |
| --- | --- | --- |
| `test.admissionlab.io/add-label` | `key=value` (key non-empty; value **may** be empty) | Adds or overwrites `metadata.labels[key]`. |
| `test.admissionlab.io/add-container` | `name=image` (both non-empty) | Appends the container to `spec.containers`. |
| `test.admissionlab.io/add-init-container` | `name=image` | Appends to `spec.initContainers`. |
| `test.admissionlab.io/remove-container` | a bare container name (no `=`) | Removes that container from `spec.containers`. |
| `test.admissionlab.io/remove-init-container` | a bare name | Removes it from `spec.initContainers`. |
| `test.admissionlab.io/add-volume` | a bare name | Appends `{name, emptyDir: {}}` to `spec.volumes` — `emptyDir` because it needs no cluster state. |
| `test.admissionlab.io/deny` | any non-empty message | The validating webhook denies with this message; the API server returns `403 Forbidden`. |
| `test.admissionlab.io/delay-ms` | integer `0`–`60000` | Sleeps that long before deciding, on allow as much as on deny. |
| `test.admissionlab.io/fail` | exactly `"true"` or `"false"` | `true` makes the webhook return HTTP `500`; with `failurePolicy: Fail` the request is rejected. |

Cross-cutting rules:

- Values are trimmed, and each half of a `k=v` form is trimmed, so
  `"  sidecar = image  "` is accepted.
- **An unknown key under the prefix is an error, not a no-op.** The plural typo
  `test.admissionlab.io/add-labels` is rejected. Keys outside the prefix are
  ignored entirely.
- **Any parse failure denies the request**, naming the annotation and its value.
  A typo must never be indistinguishable from "this stack stopped mutating".
- Error selection is deterministic: annotations are collected into a sorted map,
  so the lowest key in sort order is the one reported.
- `fail` outranks `deny`.
- **Operations are idempotent.** An add whose effect is already present emits no
  patch operation at all, which is what makes `reinvocationPolicy: IfNeeded`
  safe. Within one array, a `remove` is emitted before an append, and the
  append's idempotency check sees the post-remove state — so
  `remove-container: x` plus `add-container: x=img` is a well-defined
  replacement.

One label, not an annotation, is also load-bearing:
`test.admissionlab.io/containers: "enabled"` is the `objectSelector` on the
containers-mutating webhook configuration. It **must** be written into the
fixture manifest itself: kube-apiserver never invokes a webhook for the first
time during reinvocation, so a label added by another webhook cannot bring it in
later.

A worked example, combining a workload mutation with a label mutation:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: reinvocation-demo
  namespace: admissionlab-fixtures
  labels:
    test.admissionlab.io/containers: "enabled"
  annotations:
    test.admissionlab.io/add-volume: "scratch"
    test.admissionlab.io/add-label: "admissionlab.dev/reinvoked=true"
spec:
  containers:
    - name: app
      image: registry.k8s.io/pause:3.10
```

The checked-in corpus in `fixtures/core/admission/` covers `add-label`,
`add-container`, `add-init-container`, `remove-init-container`, reinvocation,
`deny`, `delay-ms`, `fail`, and a no-annotation control.
