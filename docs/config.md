# Configuration reference — `admissionlab.io/v1beta1`

The complete reference for `admissionlab.yaml` (the `Lab` document) and its
companion `expectations.yaml` (the `Expectations` document).

A machine-readable JSON Schema for the `Lab` document lives at
[`schemas/admissionlab-v1beta1.json`](../schemas/admissionlab-v1beta1.json).
Point your editor at it and a wrong `apiVersion` or a misspelled key is flagged
as you type.

> **Public Beta.** `admissionlab.io/v1beta1` is the current configuration
> version and the one every file under [`examples/`](../examples/) is written
> in. It grows only by **addition** from here — a new optional field with a
> default that preserves existing behavior. A rename or a removal needs a new
> `apiVersion` and a new migration, exactly as `v1alpha1` → `v1beta1` had.
> Nothing is *stable* until v1.0.

> **`admissionlab.io/v1alpha1` still loads, unchanged.** You do not have to
> migrate anything to run this release. See
> [Reading a `v1alpha1` configuration](#reading-a-v1alpha1-configuration) below
> and [`docs/schema-migrations.md`](schema-migrations.md) for the field-by-field
> record.

---

## Contents

- [Reading a `v1alpha1` configuration](#reading-a-v1alpha1-configuration)
- [Two strictness rules that apply everywhere](#two-strictness-rules-that-apply-everywhere)
- [Path resolution](#path-resolution)
- [Top-level fields](#top-level-fields)
- [`baseline` / `candidate`](#baseline--candidate)
- [`components[]`](#components)
- [`install`](#install)
- [`fixtures`](#fixtures)
- [`gateway`](#gateway)
- [`migration`](#migration)
- [`policy`](#policy)
- [Semantic change kinds and default severities](#semantic-change-kinds-and-default-severities)
- [`expectations.yaml`](#expectationsyaml)
- [Validation order](#validation-order)

---

## Reading a `v1alpha1` configuration

A `v1alpha1` lab document loads and runs exactly as it always did. The loader
accepts both versions, migrates a `v1alpha1` document to the `v1beta1` model in
memory, and resolves that — so there is one resolver, one resolved shape, and
no behavior that depends on which version you wrote.

**Two keys were renamed, and only two.** Both times, to put the unit in the
name: the value was always plain milliseconds, and a bare integer named
`absoluteIncrease` or `reconciliationTimeout` does not say so. A reader had to
open this page to find out whether `50` meant fifty milliseconds or fifty
seconds — which is precisely the kind of question a configuration file should
answer by itself.

| `v1alpha1` | `v1beta1` | Value |
| --- | --- | --- |
| `policy.latency.absoluteIncrease` | `policy.latency.absoluteIncreaseMillis` | Unchanged. Milliseconds in both. |
| `gateway.reconciliationTimeout` | `gateway.reconciliationTimeoutMillis` | Unchanged. Milliseconds in both. |

Nothing else moved: every other key is spelled identically in both versions.

**The two spellings do not mix.** A `v1beta1` document that says
`absoluteIncrease` is a named parse error, and so is a `v1alpha1` document that
says `absoluteIncreaseMillis`. That is deliberate — an alias would let a file
mean something it does not say — and it is the one thing to watch for when you
migrate: change the `apiVersion` line and the two keys **together**.

To migrate by hand: rewrite `apiVersion:` to `admissionlab.io/v1beta1`, then
rename whichever of those two keys your file actually uses. There is no
migration command, because there is nothing a command would do that a
three-line edit does not.

[`docs/schema-migrations.md`](schema-migrations.md) is the full record: what
each rename was for, which names were examined and deliberately kept, and how
the same discipline applies to the result and run-manifest documents. The
`v1alpha1` schema itself stays checked in, frozen, at
[`schemas/admissionlab-v1alpha1.json`](../schemas/admissionlab-v1alpha1.json) —
an editor pointed at it still validates an Alpha file correctly.

`testdata/configs/renamed-fields-v1alpha1.yaml` is a complete `v1alpha1` lab —
every optional section populated, both renamed keys in their old spelling —
kept in this repository, and loaded by
`crates/admissionlab-spec/tests/migrate_alpha_beta.rs` on every test run, for
no other purpose than to keep proving that an Alpha file nobody touched still
resolves to exactly what it always did.

---

## Two strictness rules that apply everywhere

1. **Unknown keys are hard errors.** Every mapping in the document is parsed
   with `deny_unknown_fields`. Writing `candiate:` instead of `candidate:` is a
   named parse failure, not a silently ignored typo.
2. **`camelCase` on the wire.** You write `apiVersion`, `expectationsFile`,
   `failOn`, `relativeMultiplier`, `valuesFiles`, `setValues`, `repoName`,
   `releaseName`, `absoluteIncreaseMillis`, `objectPath`, `fixtureGlob`.

`kind` is checked against its one legal value (`Lab`) immediately after
parsing, and `apiVersion` against the supported set —
`admissionlab.io/v1beta1` (current) and `admissionlab.io/v1alpha1` (migrated on
load). Any other value is refused by name, listing both.

---

## Path resolution

**Every relative path in the document resolves against the directory containing
the configuration file — never against the process's working directory.**

This applies to:

- `fixtures.include` glob patterns (the root they are matched relative to);
- `expectationsFile`;
- `install.valuesFiles[]` (Helm);
- `install.paths[]` (raw manifests).

So this works from anywhere:

```bash
cd /some/unrelated/place
admissionlab test ~/labs/upgrade/admissionlab.yaml
```

and `fixtures.include: ["fixtures/**/pod-*.yaml"]` still means
`~/labs/upgrade/fixtures/**/pod-*.yaml`. The configuration path is used exactly
as you give it and is never canonicalized, so a relative config path stays
correctly relative to your current directory too.

Absolute paths are used as written.

---

## Top-level fields

```text
apiVersion: admissionlab.io/v1beta1
kind: Lab
baseline: { ... }        # required
candidate: { ... }       # required
fixtures: { ... }        # required
gateway: { ... }         # optional; omit for an admission-only lab
migration: { ... }       # optional; omit unless migrating off Ingress
policy: { ... }          # optional; every field defaults
expectationsFile: ...    # optional
```

| Field | Type | Required | Default | Notes |
| --- | --- | --- | --- | --- |
| `apiVersion` | string | yes | — | `admissionlab.io/v1beta1`, or `admissionlab.io/v1alpha1` (migrated on load). Nothing else. |
| `kind` | string | yes | — | Must be exactly `Lab`. |
| `baseline` | object | yes | — | The unmodified stack being compared against. |
| `candidate` | object | yes | — | The stack under test. |
| `fixtures` | object | yes | — | Which fixtures to replay through both sides. |
| `gateway` | object | no | none | The Gateway behavior suite. Omit the whole section for an admission-only lab; see [`gateway`](#gateway). |
| `migration` | object | no | none | The Ingress-to-Gateway migration suite. `v1beta1` only — a `v1alpha1` document has no such key and always resolves to none. See [`migration`](#migration). |
| `policy` | object | no | all defaults | Omit the whole section to accept every default. |
| `expectationsFile` | path | no | none | Path to an `Expectations` document, resolved against the config file's directory. A missing file here is exit `2` — you named it, so it is your configuration at fault. |

---

## `baseline` / `candidate`

```yaml
baseline:
  kubernetes: "1.36.4"
  images: []
  components: []
```

| Field | Type | Required | Default | Notes |
| --- | --- | --- | --- | --- |
| `kubernetes` | string | yes | — | The Kubernetes version to provision. Must be non-empty. It is resolved against `compatibility/kubernetes.yaml`, which pins an exact patch version and node-image digest per minor — a version outside that matrix fails with exit `2` before any cluster is created. |
| `images` | list of strings | no | `[]` | Container images already in your **local** image store to side-load into this side's cluster before anything is installed. See below. |
| `components` | list | no | `[]` | Components to install on top of the base cluster, **in installation order**. A bare Kubernetes cluster with no components is a valid environment. |

### `images[]`

For workloads that were built locally and never pushed anywhere. A `kind` node
pulls a registry image itself, so most labs list nothing here; a manifest that
references a locally built tag with `imagePullPolicy: IfNotPresent` needs the
image to be *in the node* first, and without this it fails minutes into the run
with an `ErrImageNeverPull` that reads as a broken fixture.

```yaml
baseline:
  kubernetes: "1.36.4"
  images:
    - admissionlab-echo:dev
```

Each entry is one image reference, passed to the cluster backend as a single
argument — never through a shell. Loading happens once, immediately after the
cluster is created and before the first component is installed, and a failure to
load fails the cluster rather than surfacing later as a scheduling error. Build
the image before the run (`./scripts/build-test-images.sh` with no arguments
builds Admission Lab's own two and loads nothing).

Both sides list their images independently, so a lab that side-loads a different
build into the candidate is expressible — and is exactly as visible in the
configuration as a different chart version would be.

Baseline and candidate are always separate ephemeral clusters and never share
mutable state. They may legitimately install different Kubernetes versions —
that is how you test a Kubernetes upgrade rather than a component upgrade.

---

## `components[]`

```yaml
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
```

| Field | Type | Required | Default | Notes |
| --- | --- | --- | --- | --- |
| `name` | string | **yes today** | — | Must be non-empty, and unique **within one environment**. Baseline and candidate are expected to use the *same* name for the same component — that is how the two are paired for comparison. Declared optional in the schema because recipe-derived naming is planned; resolution requires it today. |
| `version` | string | see notes | derived | The component's version as its install method understands it. Required unless the install method already carries an unambiguous version — a pinned Helm chart version, which is then used as the default. |
| `install` | object | **yes today** | — | How to install the component. Required: recipe-driven installation does not exist yet (see below). |
| `readiness` | list | no | `[]` | Conditions this component must satisfy before the next component on the same side is installed, and before any fixture is replayed. See below — **for anything that serves admission, leaving this empty is almost always wrong.** |
| `recipe` | string | no | none | **Accepted but inert.** The field parses and is carried through, but nothing resolves it: an explicit `install` block is required whether or not you set `recipe`. See [`docs/recipes.md`](recipes.md) for what recipes currently do and do not drive. |

### `readiness[]`

`helm upgrade --install` returns as soon as a release's manifests are applied,
and `kubectl apply` as soon as its objects exist. Neither waits for a controller
to be *running*, for a `caBundle` to be filled in, or for a webhook
configuration a controller creates at **runtime** to appear at all.

A lab that replays fixtures inside that window observes a stack that is not yet
the stack under test — and does so at a different moment on each side, which is
exactly the nondeterminism this tool exists to avoid. Symptom: a run that finds
suspiciously few changes.

Five closed check types. The vocabulary and every field spelling match
`recipes/*/recipe.yaml`'s own `readiness` section one for one, so a recipe's
checks can be transcribed into a lab file unchanged. Note what transcribing does
and does not carry: you get the recipe's readiness *checks*, not its
certification — a lab you assembled by hand is certified by nothing, whichever
pins you copied into it. See [`docs/compatibility.md`](compatibility.md).

| `type` | Fields | Waits for |
| --- | --- | --- |
| `deploymentAvailable` | `namespace`, `name` | The Deployment's `Available` condition to be `True`. |
| `daemonSetReady` | `namespace`, `name` | Every desired DaemonSet pod scheduled and ready. |
| `jobComplete` | `namespace`, `name` | The Job to complete successfully. |
| `webhookConfigurationPresent` | `name` | A `ValidatingWebhookConfiguration` **or** `MutatingWebhookConfiguration` of that name to exist. The validating kind is looked up first, so one check type covers both. |
| `customResourceCondition` | `apiVersion`, `kind`, `name`, `conditionType`, `status`, and optional `namespace` | A custom resource's named condition to equal a given status. |

```yaml
readiness:
  - type: deploymentAvailable
    namespace: kyverno
    name: kyverno-admission-controller
  - type: webhookConfigurationPresent
    name: kyverno-resource-mutating-webhook-cfg
  - type: customResourceCondition
    apiVersion: kyverno.io/v1
    kind: ClusterPolicy
    name: require-labels
    conditionType: Ready
    status: "True"
```

Checks are evaluated in the order written, and all must pass within the
component's install timeout.

**`webhookConfigurationPresent` proves existence, not enforcement.** A
configuration a controller creates at runtime typically starts with an empty
`webhooks: []` list, so its existence is not proof that any particular policy's
rule has been folded into it. When you apply your own policy after the chart,
wait for that policy with `customResourceCondition` as well — that is exactly
what the last entry above does.

---

## `install`

`install` is an internally tagged union: a `type` discriminant alongside the
variant's own fields. Setting a field belonging to the other variant is a parse
error.

### `type: helm`

```yaml
install:
  type: helm
  chart: istio/istiod
  repo: https://istio-release.storage.googleapis.com/charts
  version: "1.30.4"
  namespace: istio-system
  releaseName: istiod
  repoName: istio
  valuesFiles:
    - values/istiod-lab.yaml
  setValues:
    global.logAsJson: "true"
```

| Field | Type | Required | Default | Notes |
| --- | --- | --- | --- | --- |
| `chart` | string | yes | — | The chart reference, in Helm's own `<repoName>/<chartName>` form. Must be non-empty. Local paths and `oci://` references parse but cannot be resolved today. |
| `repo` | string | **yes today** | — | The Helm repository URL. Required for every Helm install, because a repo-relative chart reference is the only form resolution can act on. |
| `version` | string | yes | — | **Must be an exact pin.** See below. |
| `repoName` | string | no | the component's `name` | The local name `repo` is registered under (`helm repo add <repoName> <repo>`). Purely local bookkeeping. Note that `chart` must start with this name. |
| `releaseName` | string | no | the component's `name` | The Helm release name. |
| `namespace` | string | no | the component's `name` | **This default is often wrong.** `istio/istiod` conventionally installs into `istio-system`, not `istiod`. Set it explicitly whenever the chart's convention differs from your component name. |
| `valuesFiles` | list of paths | no | `[]` | Values override files, resolved against the config file's directory. |
| `setValues` | map of string→string | no | `{}` | Literal `--set-string` key/value overrides. |

#### What "pinned" means

`version` must match: an optional `v`/`V` prefix, then exactly three
dot-separated numeric segments (`MAJOR.MINOR.PATCH`), then an optional
`-prerelease` and/or `+build` suffix. `"3.9.0"`, `"v1.14.4"`,
`"1.2.3-rc.1"`, and `"1.2.3+build.5"` are accepted.

Rejected as floating, with exit `2`: an empty or omitted version, `latest`,
ranges (`^3.9`, `>=3.9`, `~1.2.3`), wildcards (`1.2.x`, `1.2.*`), and partial
versions (`3`, `3.9`). Helm expands every one of those into a *range*, so the
chart actually installed could differ between the baseline run and the
candidate run — which would make the comparison meaningless.

### `type: manifests`

```yaml
install:
  type: manifests
  paths:
    - manifests/00-namespace.yaml
    - manifests/10-rbac.yaml
    - manifests/20-webhook-configuration.yaml
```

| Field | Type | Required | Default | Notes |
| --- | --- | --- | --- | --- |
| `paths` | list of paths | yes | — | Manifest files or directories, applied in order, resolved against the config file's directory. **Must not be empty** — an install that installs nothing and calls it success is exactly the quiet no-op this tool exists to catch. |

### Installation timeout

Each component gets **600 seconds** to install and become ready. This is not
configurable. A component that exceeds it fails the run with exit `4`.

---

## `fixtures`

```yaml
fixtures:
  include:
    - "fixtures/core/admission/pod-*.yaml"
    - "fixtures/mesh/**/*.yaml"
```

| Field | Type | Required | Default | Notes |
| --- | --- | --- | --- | --- |
| `include` | list of globs | yes | — | **Must not be empty.** A file is selected if **at least one** pattern matches (logical OR). There is no `exclude` list. |

Globs are matched against each file's path *relative to the configuration
file's directory*. Note that `*` also matches `/` in these patterns, so
`fixtures/*.yaml` reaches nested files too — use a more specific pattern if
that is not what you want.

A pattern set matching **zero** fixtures fails the run with exit `2`: there is
nothing to replay, so no comparison can be produced.

See [`docs/fixtures.md`](fixtures.md) for the fixture format itself, ID
derivation, and the setup-outside-the-glob pattern.

---

## `gateway`

The Gateway behavior suite: Gateway API objects persisted in **both** sides'
clusters, the routes whose reconciliation is compared, and the HTTP requests
sent through the resulting data plane. Omit the whole section — as almost every
lab does — for an admission-only run.

Unlike admission fixtures, these manifests are **applied for real, not
dry-run**: a controller cannot reconcile an object that was never persisted, and
a Gateway with no status has nothing to compare. What makes that safe is the
disposable cluster; Admission Lab never applies them anywhere else.

```yaml
gateway:
  manifests:
    - gateway/suite.yaml
  gatewayEndpoint:
    type: serviceBySelector
    namespace: "{gatewayNamespace}"
    selector:
      gateway.networking.k8s.io/gateway-name: "{gatewayName}"
    portName: http
  readiness:
    - type: deploymentAvailable
      namespace: demo
      name: echo-a
  reconciliationTimeoutMillis: 120000
  routes:
    - id: echo-route
      gatewayNamespace: demo
      gatewayName: lab-gateway
      routeNamespace: demo
      routeName: echo-route
      listenerName: http
      probes:
        - host: echo.example.test
          path: /
          method: GET
          expectedStatus: 200
          expectedBackend: echo-a
```

| Field | Type | Required | Default | Notes |
| --- | --- | --- | --- | --- |
| `manifests` | list of paths | yes | — | Kubernetes manifest files defining the suite: namespaces, backends, `GatewayClass`, `Gateway`, `HTTPRoute`, `ReferenceGrant`. Must be non-empty. Resolved against the config file's directory. Applied in a fixed category order, never deleted. |
| `routes` | list | yes | — | The route contracts observed and probed. Must be non-empty; every `id` must be unique. |
| `gatewayEndpoint` | object | no | none | How to find the `Service` fronting each Gateway's data plane. **Without it no traffic probe is ever sent** — every probe is recorded as an explicit skip, and only reconciliation is compared. |
| `readiness` | list | no | `[]` | Conditions the suite's own manifests must satisfy after they are applied and before any route is observed. Same vocabulary as [`components[].readiness`](#readiness). |
| `reconciliationTimeoutMillis` | integer (ms) | no | `120000` | How long each route gets, per side, to reach a stable, current status in which it is carrying traffic. Must be non-zero. |

### `routes[]`

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `id` | string | yes | Unique within the suite, and how the two sides' results are paired. It is also the identifier this route is reported under, so it must be lowercase letters, digits and `-`, and must not collide with a fixture id. |
| `gatewayNamespace` / `gatewayName` | string | yes | The `Gateway` this route attaches to. Never inferred — see below. |
| `routeNamespace` / `routeName` | string | yes | The `HTTPRoute` under contract. |
| `listenerName` | string | no | Which listener, by `parentRef.sectionName`. Omitting it is unambiguous only while the route reports exactly one parent entry for this Gateway. |
| `probes` | list | no | HTTP requests to send once the route is carrying traffic. May be empty: a contract that only asserts a route reconciles is a complete test. |

Gateway identity is always explicit. Admission Lab never guesses the target
`Gateway` from "the first one in the manifest directory" or from the route's own
`parentRefs`: a contract that read its target out of the fixture it is testing
could never detect the fixture pointing at the wrong Gateway, because it would
follow it there.

### `probes[]`

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `host` | string | yes | The `Host` header, which is what a listener's `hostname` and a route's `hostnames` match on. The request still *arrives* at a local port-forward. |
| `path` | string | yes | Must begin with `/`. |
| `method` | string | yes | Uppercase, from Gateway API's own `HTTPMethod` set. |
| `headers` | map | no | Extra request headers. |
| `expectedStatus` | integer | yes | `100`–`599`. |
| `expectedBackend` | string | no | Which backend must answer, as it identifies itself. Omit for a probe asserting a status no backend produced. |

`expectedStatus`/`expectedBackend` say what a route *should* do; they carry no
severity and are not a second grader. Whether an observed difference matters is
[`policy`](#policy)'s decision, exactly as it is for admission.

### When a probe is skipped

A probe is sent only when the route is actually carrying traffic for its
contract: the `Gateway` is `Programmed`, and the contract's own parent entry is
`Accepted` with `ResolvedRefs`. Anything else is recorded as a skip **with the
specific condition, state and controller reason that caused it** — in
`probes.json` beside the request that was not sent, as a `gateway.probe_skipped`
diagnostic in the terminal report and `result.json`, and, when the other side
*did* answer, as a `traffic_status_changed` finding.

Probing anyway would record the data plane's own error page (Gateway API
specifies a `503` for an unaccepted route and a `500` for an unresolved backend)
and then compare that invented status against a real one. The condition change
is the finding; a status code produced by the same broken state is not a second,
independent one.

### `gatewayEndpoint`

Two forms, `serviceBySelector` (preferred) and `serviceByName`. Two placeholders
are substituted in `namespace`, `selector` *values*, and `name`:
`{gatewayName}` and `{gatewayNamespace}`. Anything else in braces is an error at
load time, not a literal.

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `type` | string | yes | `serviceBySelector` or `serviceByName`. |
| `namespace` | string | yes | Usually `"{gatewayNamespace}"`. |
| `selector` | map | `serviceBySelector` | Every pair must match. Keys are literal. |
| `name` | string | `serviceByName` | The `Service`'s exact name. |
| `portName` / `port` | string / integer | no | Which port to forward to. Required whenever the matched `Service` exposes more than one — Admission Lab reports an ambiguous port rather than picking one. |

This is the same block a certified recipe declares under `gatewayEndpoint:`,
and it parses into the same thing. A lab file spells it out for the same reason
it spells out `install:` and `readiness:`: `admissionlab.yaml` has no recipe
resolution, so a lab that hand-writes how an implementation is installed must
equally hand-write where its data plane is.

---

## `migration`

The Ingress-to-Gateway migration suite: for each case, the legacy `Ingress`
manifests applied to the **baseline** cluster, the Gateway API manifests applied
to the **candidate** cluster, and the HTTP requests replayed through **both**.
Omit the whole section for a lab that is not migrating off `Ingress`.

**Admission Lab converts nothing.** Both halves of every case are written by a
person, or produced by some other tool (`ingress2gateway`, a vendor's migration
guide, a hand edit); this suite checks the conversion *you already performed*.
That is not a missing feature waiting on a converter — a suite that generated
the candidate manifests would compare a converter against itself, and would
report "no behavior change" for precisely the mistakes it exists to catch.

Like `gateway`, these manifests are **applied for real, not dry-run**, and are
safe for the same reason: the disposable cluster.

```yaml
migration:
  baseline:
    gatewayEndpoint:                       # where the Ingress controller's
      type: serviceByName                  # ONE SHARED data plane is
      namespace: ingress-nginx-legacy
      name: ingress-nginx-legacy-controller
      portName: http
  candidate:
    gatewayEndpoint:                       # where the implementation's
      type: serviceBySelector              # PER-GATEWAY data plane is
      namespace: "{gatewayNamespace}"
      selector:
        gateway.networking.k8s.io/gateway-name: "{gatewayName}"
  cases:
    - id: legacy-echo
      baselineIngressManifests:
        - baseline/ingress.yaml
      candidateGatewayManifests:
        - candidate/gateway.yaml
      probes:
        - host: migrate.example.test
          path: /legacy/health
          method: GET
          expectedStatus: 200
          expectedBackend: echo-a
      expectedNonportable:
        - feature: nginx.ingress.kubernetes.io/limit-rps
          reason: rate limiting moves to the platform's shared edge proxy
```

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `cases[]` | list | yes | At least one. Every `id` unique. |
| `baseline` / `candidate` | mapping | in practice | Each carries one [`gatewayEndpoint`](#gatewayendpoint), in the same shape and validated by the same rules as `gateway`'s. |

**Two endpoint blocks, not one**, and the asymmetry is structural: an `Ingress`
controller is *one shared* `Service` in the controller's own namespace with
nothing to substitute, while a Gateway API implementation provisions *one
`Service` per `Gateway`* and must be templated on the Gateway being probed. A
single strategy cannot be both.

Both blocks are **optional in the schema and required to run**. Optional,
because they were added after the `migration:` section itself and an addition
must leave documents written before it valid ([`docs/schema-migrations.md`](schema-migrations.md)).
Required, because a suite with nowhere to send its probes would install two
stacks, compare nothing, and report success — so `admissionlab test` refuses it
in its pre-flight validation, before any cluster is created, naming the missing
field.

### `cases[]`

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `id` | string | yes | Unique within the suite. Lowercase letters, digits and `-` — it is a path segment under `raw/<side>/migration/`. |
| `baselineIngressManifests[]` | list of paths | yes | Non-empty. Applied to the baseline cluster. |
| `candidateGatewayManifests[]` | list of paths | yes | Non-empty. Applied to the candidate cluster. Must apply **exactly one** `Gateway` and **exactly one** `HTTPRoute`: their identities are read from the manifests themselves rather than configured a second time, and two of either would make "did the route reconcile" a guess. |
| `probes[]` | list | yes | Non-empty, and the same shape as [`gateway`'s probes](#probes). Unlike a route contract, a migration case with no probes asserts nothing at all: an `Ingress` and an `HTTPRoute` share no status vocabulary, so traffic is the only thing the two sides can be compared on. |
| `expectedNonportable[]` | list | no | See below. |

Each probe's `expectedStatus`/`expectedBackend` describe **what the baseline
does today** — that is what "the behavior being migrated" means. The candidate
is observed answering the same request, whatever it answers, and a difference is
the finding.

### `expectedNonportable[]`

`Ingress` features you already know have no faithful Gateway API equivalent,
each with a written justification.

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `feature` | string | yes | The annotation key **verbatim** (`nginx.ingress.kubernetes.io/limit-rps`, not `limit-rps`). Non-empty, unique within its case. |
| `reason` | string | yes | Non-empty. What the feature did, and what the migration does instead. |

A declared feature that the baseline manifests really carry is reported as a
`non_portable_feature` change with `expected: true` and graded `info`; an
undeclared one is graded `warning`. A declaration that matches nothing is
reported as an unmatched expectation and does not move the verdict — the
migration analogue of a stale `expectations.yaml` entry.

**This is deliberately not `expectations.yaml`.** That file selects on a
`SemanticChangeKind`, the closed set of *admission* differences; a
non-portability is a fact about an `Ingress`. It is also permanent where a
regression expectation is transient: `limit-rps` does not become portable later,
and reporting it as stale every run would train reviewers to ignore staleness.

### How a migration case reaches the report and the verdict

A migration case is **not** a `fixtures[]` entry in `result.json`: its findings
are a separate change vocabulary from the semantic changes a fixture carries, so
they live in an optional top-level `migration` array and are not counted in
`summary`'s five buckets. They do reach `policy.disposition`, which is the run's
verdict and what the exit code comes from — an unexplained traffic difference is
`critical` (exit 1), an undeclared non-portability is `warning`, and a case whose
two sides could not be compared at all is at least `warning`. See
[`examples/ingress-to-gateway/`](../examples/ingress-to-gateway/) for a worked
lab that produces one of each.

---

## `policy`

Every field defaults independently, so `policy` may be omitted entirely.

```yaml
policy:
  failOn:
    - container_added
    - webhook_invocation_changed
  overrides:
    - kind: container_added
      fixtures: "web-*"
      subject: istio-proxy
      severity: warning
  latency:
    absoluteIncreaseMillis: 100
    relativeMultiplier: 2.0
```

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `failOn` | set of change-kind names | `[]` | Categories that fail the run when observed, *in addition to* the default-critical set. Names are the wire names in the table below — exactly the strings a JSON report prints, so a name copied out of a report always works. An unknown name fails at load time, before any cluster exists. Duplicates collapse. |
| `overrides` | list | `[]` | Targeted exceptions, see below. |
| `latency.absoluteIncreaseMillis` | integer milliseconds | `100` | Written as a plain integer (`absoluteIncreaseMillis: 50`), not a duration object. Spelled `absoluteIncrease` in `v1alpha1`; see [Reading a `v1alpha1` configuration](#reading-a-v1alpha1-configuration). |
| `latency.relativeMultiplier` | number | `2.0` | — |

### Latency thresholds

A candidate observation counts as a latency regression only when it exceeds
**both** thresholds: `baseline + absoluteIncreaseMillis` **and**
`baseline × relativeMultiplier`. With the defaults, a webhook must be at least
100 ms slower *and* at least 2× the baseline before `webhook_latency_changed`
is reported.

The conjunction is the point. A `0 ms` / `1.0×` threshold would flag every
webhook whose latency merely failed to improve, and drown the real regressions.

Per-webhook latency is an **optional** observed signal. When the metrics are
absent or ambiguous, that is reported as unknown — never as zero, and never as
a failure on its own.

### `overrides[]`

An override narrows *which* regressions it applies to and re-grades them to a
specific severity instead of failing the run outright.

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `kind` | change-kind name | yes | Validated against the table below. |
| `severity` | `info` \| `warning` \| `critical` | yes | Case-sensitive; `Warning` is rejected. |
| `fixtures` | glob | no | Restrict to fixtures whose ID matches. Must be a compilable glob, and must not be present-but-empty. |
| `subject` | string | no | Restrict to a specific subject — a container name, a webhook name. |
| `path` | RFC 6901 pointer | no | Restrict to a field path **inside the compared object**, e.g. `/spec/containers/0/image`. Never a filesystem path, and never resolved against the config directory. |

Omitting a narrowing field leaves that dimension unrestricted. A field that is
present but empty is rejected — it is a restriction nothing could ever satisfy.

---

## Semantic change kinds and default severities

These are the names accepted by `policy.failOn`, `policy.overrides[].kind`, and
`expectations[].kind`, with the severity each is graded at *before* your policy
is applied.

| Wire name | Default severity | Meaning |
| --- | --- | --- |
| `newly_denied` | **critical** | Baseline admitted the object; candidate rejected it. |
| `newly_allowed` | **critical** | Baseline rejected the object; candidate admitted it. Critical by design — a policy that stopped enforcing is as much a regression as one that started. |
| `container_added` | warning | A container present only in the candidate's admitted object. Usually a sidecar injected on purpose. |
| `container_removed` | **critical** | A container present only in the baseline's object — functionality silently dropped. |
| `init_container_added` | warning | |
| `init_container_removed` | **critical** | |
| `volume_added` | warning | |
| `volume_removed` | **critical** | |
| `volume_mount_changed` | warning | A container's volume mounts differ. |
| `environment_changed` | warning | A container's environment differs. |
| `image_changed` | info | Image references move on nearly every run of a real pipeline; grading this higher by default would cry wolf. |
| `service_account_changed` | **critical** | Identity changes change what the workload is authorized to do. |
| `security_context_changed` | **critical** | |
| `resource_requirement_changed` | warning | Requests or limits differ. |
| `webhook_failed` | **critical** | A webhook failed on one side and not the other — a broken admission chain, whatever the object ended up looking like. |
| `webhook_invocation_changed` | warning | The observed set or ordering of webhook invocations differs. |
| `webhook_latency_changed` | warning | Exceeded both latency thresholds. Never fails a run by itself. |

The nine kinds below are produced by Gateway comparisons (Phase 6). They are
named, graded, and excepted exactly like the admission kinds above.

| Wire name | Default severity | Meaning |
| --- | --- | --- |
| `route_attached` | info | The candidate's route status names a parent `Gateway` the baseline's did not — a path that did not carry traffic before. |
| `route_detached` | **critical** | A parent `Gateway` the baseline's route status named is absent from the candidate's. |
| `backend_resolution_changed` | **critical** | Whether the route's backend references resolve changed: exactly one side published `ResolvedRefs: True`. |
| `listener_binding_changed` | **critical** | The route binds to a different set of a `Gateway`'s listeners (`parentRef.sectionName`). |
| `accepted_condition_changed` | **critical** | An `Accepted` condition's state differs, on a `GatewayClass`, a `Gateway`, or a route's parent entry. |
| `resolved_refs_condition_changed` | **critical** | A route parent's `ResolvedRefs` condition state differs. |
| `programmed_condition_changed` | **critical** | A `Gateway`'s `Programmed` condition state differs. |
| `traffic_status_changed` | **critical** | A probe through the data plane returned a different HTTP status, or the candidate answered nothing where the baseline answered. |
| `traffic_backend_changed` | **critical** | The same probe reached a different backend workload, as each backend identified itself. |

**Improvements are downgraded.** A condition change that moves *to* `True`
(`accepted_condition_changed`, `resolved_refs_condition_changed`,
`programmed_condition_changed`) is graded `info` instead of `critical`: the
candidate accepted, resolved, or programmed something the baseline did not.
Moving away from `True` stays `critical`. The direction is recorded in the
change's own `candidate.direction` field (`improvement` / `regression`), never
inferred from a controller's `reason` text. `failOn` and `overrides` still have
the last word over a downgraded change, in both directions.

Severities are `info`, `warning`, `critical` — one spelling each,
case-sensitive.

**A run fails (exit `1`) when an unexpected `critical` change is observed.** A
run with only warnings **passes with exit `0`**; the warnings appear in the
terminal summary, in `result.json`, and in the HTML report regardless.

---

## `expectations.yaml`

An expectations file records the behavior changes a team has already reviewed
and decided to accept. Marking a change expected **does not change its severity
and does not hide it from the report** — it only stops it from failing the run.

Point at it from the lab document:

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
expectationsFile: expectations.yaml
```

And write it as its own document. **Note the `apiVersion`: it is
`v1alpha1`, and that is correct.** `Expectations` is a separate document with
its own version line, and Public Beta promoted the `Lab` document only —
`admissionlab.io/v1alpha1` is the one value this file's loader accepts, whatever
the lab beside it says. Setting it to `v1beta1` to match is exit `2`.

```yaml
apiVersion: admissionlab.io/v1alpha1
kind: Expectations
expectations:
  - id: istio-sidecar-injection
    fixtures: "web-*"
    kind: container_added
    selector:
      subject: istio-proxy
    reason: >-
      The candidate stack enables Istio automatic sidecar injection in the
      `web` namespace, so every web-facing fixture gains an `istio-proxy`
      container. Tracked in PLATFORM-2291; remove this entry once the
      baseline stack also injects it.

  - id: istio-proxy-run-as-user
    fixtures: "web-*"
    kind: security_context_changed
    selector:
      subject: istio-proxy
      objectPath: /spec/containers/1/securityContext/runAsUser
    reason: >-
      The injected `istio-proxy` container runs as UID 1337, which the
      baseline stack never set because it never injected the container.
      Reviewed by the platform security team on 2026-08-20.
```

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `apiVersion` | string | yes | Must be exactly `admissionlab.io/v1alpha1` — **not** the lab document's `v1beta1`. This document versions independently of the `Lab` one and has not been promoted; when it is, [`docs/schema-migrations.md`](schema-migrations.md) will carry that migration too. |
| `kind` | string | yes | Must be exactly `Expectations`. |
| `expectations[].id` | string | yes | Stable, file-unique handle. Appears in the report and in stale-expectation warnings, so renaming it renames it everywhere. |
| `expectations[].fixtures` | glob | yes | A glob over fixture IDs. **`"*"` is how you say "any fixture"** — spelled out rather than implied by omission, because an expectation silently spanning every fixture in the repository is not something to arrive at by leaving a line out. |
| `expectations[].kind` | change-kind name | yes | Validated by the parser itself, at the offending line, with the valid names listed. |
| `expectations[].selector` | object | no | Further narrowing: `fixtureGlob`, `subject`, `objectPath`. Every dimension is `AND`ed with `fixtures`, which always applies — setting `selector.fixtureGlob` narrows further rather than replacing `fixtures`. |
| `expectations[].reason` | string | yes | Required and non-empty. Written for the human reviewing the file, not for the machine. |

Entries are matched **in the order written**. An expectation that matched
nothing in a run is reported as a **stale expectation** — normally a change
that has since been fixed and an entry you can delete.

---

## Validation order

Everything the tool can check without a cluster, it checks first — so a
misspelled key never costs you two `kind` clusters and two Helm installs:

1. load and parse `admissionlab.yaml`;
2. resolve paths and validate the document (versions non-empty, component names
   unique, Helm versions pinned, globs compilable, manifest paths non-empty);
3. validate `policy` names (change kinds, severities, override globs);
4. load and validate `expectationsFile`;
5. discover fixtures on the filesystem (parse, identity, duplicates);
6. check host prerequisites, the same check `admissionlab doctor` performs.

Every failure in steps 1–6 exits `2`. Only after all six pass does anything get
created.

---

## Not configurable

Stated so you do not go looking:

- **Report redaction rules.** Secret data, authorization headers, private keys,
  and credential-like environment values are redacted unconditionally, with or
  without configuration. The additional user-supplied JSON pointers and
  credential-name patterns described in [`docs/security.md`](security.md) exist
  in the library but have no YAML surface yet.
- **Component install timeout** (fixed at 600 s), **fixture concurrency**
  (serial within each cluster, by design), **audit policy**, and the **run root**
  (`${TMPDIR}/admissionlab-runs`).
- **Migration serving and reconciliation budgets.** A `migration:` case gets two
  minutes for its candidate route to reconcile and two minutes per side for the
  case's probes to be served, both fixed. `gateway.reconciliationTimeout` is the
  Gateway suite's knob and does not apply here.
