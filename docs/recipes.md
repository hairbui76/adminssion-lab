# Recipes

A **recipe** is curated, checked-in installation metadata for a known component
of an admission, Gateway API, or legacy Ingress stack: a pinned chart, the
readiness checks that actually prove the component is serving, harmless
response-normalization rules, and a declaration of which capabilities the
component provides.

Recipes exist to save you from rediscovering that `istio/istiod` installs into
`istio-system` rather than `istiod`, or that Kyverno's resource-facing webhook
configurations are created by its controller at runtime and not rendered by its
chart.

> **Wiring status, stated up front.** Recipes are a fully implemented,
> validated, tested format, five of whose recipes carry certified Kubernetes
> rows — but they are **not yet wired into `admissionlab.yaml`**. The
> `recipe:` field on a component parses and is carried through, and nothing
> resolves it: an explicit `install:` block is required today whether or not you
> set it. Recipes are consumed
> through the Rust API and by this repository's own certification tests. Treat
> this page as the reference for the format and for the pins the certified
> recipes carry — pins you can copy straight into an `install:` block — not as a
> feature you can switch on from YAML.

---

## Contents

- [The hard rule: a recipe never classifies a regression](#the-hard-rule-a-recipe-never-classifies-a-regression)
- [Anatomy of a recipe](#anatomy-of-a-recipe)
- [The recipes this project ships](#the-recipes-this-project-ships) (and
  [`docs/compatibility.md`](compatibility.md) for what a certification means)
- [Capability model](#capability-model)
- [Override directories](#override-directories)
- [Using a recipe's pins today](#using-a-recipes-pins-today)

---

## The hard rule: a recipe never classifies a regression

Global Constraint 6, and PRODUCT.md §14: the core is vendor-neutral. A recipe
may supply install, readiness, normalization, and capability metadata. It may
**never** contain regression-classification logic.

The reason is not stylistic. If a vendor could ship a recipe that decides what
counts as a regression in their own component, the engine stops being
vendor-neutral and its verdicts stop being worth anything.

This is enforced twice, and neither enforcement is a code-review convention:

- **Structurally, by the dependency graph.** The recipes crate depends on
  neither `admissionlab-diff` nor `admissionlab-policy` — the two crates that
  decide what counts as a regression — and cannot reach either transitively. A
  recipe's Rust representation has no vocabulary capable of expressing "this
  difference is a regression", because the crate defining that vocabulary is
  unreachable from it.
- **By construction, in the schema.** Every field at every nesting level is
  drawn from an explicit allow-list (`deny_unknown_fields` throughout, plus
  closed enums for readiness checks, normalization rules, and capabilities). A
  `failOn:`, a `severity:`, or any other classification-shaped key **fails to
  parse** rather than being quietly accepted and ignored — or, worse, honored.

Normalization rules are the closest a recipe gets to influencing a result, and
they are deliberately narrow: remove a JSON pointer, remove an annotation, sort
a named array. Those describe *known nondeterminism in a component's own
output* — a generated CA bundle, a timestamp annotation — not a judgment about
whether a difference matters.

---

## Anatomy of a recipe

A recipe is a single `recipe.yaml`:

```yaml
name: kyverno
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
  - type: webhookConfigurationPresent
    name: kyverno-resource-mutating-webhook-cfg
capabilities:
  - admission
```

| Section | Purpose |
| --- | --- |
| `name` | The recipe's identity. Also the default for `repoName`, `releaseName`, and `namespace` when the install block omits them. |
| `version` | The component version this recipe pins. |
| `install` | The same `type: helm` / `type: manifests` union `admissionlab.yaml` uses — see [`docs/config.md`](config.md#install). Helm versions must be exact pins there too. |
| `readiness` | An ordered list of checks that must all pass before the component counts as installed. |
| `capabilities` | What the component actually provides. See below. |

### Readiness checks

Five closed variants: `deploymentAvailable`, `daemonSetReady`, `jobComplete`,
`webhookConfigurationPresent`, and `customResourceCondition`. The vocabulary and
every field spelling are identical to a lab document's own `readiness` section —
deliberately, so a recipe's checks transcribe into `admissionlab.yaml`
unchanged. The full table is in
[`docs/config.md`](config.md#readiness).

The checks prove existence, availability, and named conditions. None of them can
assert that a *field* is non-empty — notably,
`webhookConfigurationPresent` confirms the object exists, **not** that any
specific policy's rule has been folded into it. Kyverno's two resource-facing
configurations, for example, are created by its controller at runtime and start
with an empty `webhooks: []` list. A caller that applies a `ClusterPolicy`
afterwards must separately wait for that policy's own `Ready` condition, with
`customResourceCondition`.

Nor is there a variant that can assert an injector's `caBundle` has been filled
in. Istio's `istiod` was measured filling it roughly 3.3 s *before* the
Deployment became `Available` — evidence, not a guarantee, and the recipe schema
cannot encode the stronger property. If sidecar injection ever fails with a
fail-closed `connection refused` from the injector, that ordering is the first
thing to re-measure.

A readiness model that accepts "the Deployment is Available but its webhook
configurations do not yet exist" is exactly what these lists exist to prevent —
with `failurePolicy: Fail` on every webhook, a fixture submitted during that
window gets a *different, quietly wrong* result rather than an error.

---

## The recipes this project ships

Eight, of which two are embedded in the compiled binary; the others ship as
on-disk recipes because they install raw manifests from paths that only exist on
disk, or because they are composed into a stack with one that does.

**"Ships a recipe" and "is certified" are different claims.** Five of the eight
below carry certified Kubernetes rows in
[`compatibility/recipes.yaml`](../compatibility/recipes.yaml) — `kyverno`,
`istio`, `istio-gateway`, `nginx-gateway-fabric` and `ingress-nginx-legacy`. The
other three do not, and that is deliberate rather than an omission:
`test-webhook` is Admission Lab's own dogfood instrument rather than a stack
anyone compares, and the two Gateway API CRD bundles install
CustomResourceDefinitions and have no behavior of their own to certify against a
Kubernetes version independently of the implementation that serves them. See
[`docs/compatibility.md`](compatibility.md) for the certified table and what a
certification actually asserts.

| Recipe | Version | Certified? | Install | Built in? | Notes |
| --- | --- | --- | --- | --- | --- |
| `kyverno` | `3.9.0` (appVersion v1.19.0) | yes — Kubernetes `1.35.8` only | `kyverno/kyverno` from `https://kyverno.github.io/kyverno/`, namespace `kyverno` | yes | Installed entirely at chart defaults. Readiness gates only `kyverno-admission-controller` — the chart's other three Deployments (background, cleanup, reports) sit outside the admission path. This chart line is the last to support the legacy `ClusterPolicy`/`Policy` API its fixtures use. |
| `istio` | `1.30.4` | yes — `1.35.8`, `1.36.4`, `1.37.0` | `istio/istiod` from `https://istio-release.storage.googleapis.com/charts`, namespace `istio-system` | yes | **`istio/base` is deliberately omitted.** Verified empirically: `istiod` alone reaches Available, serves working sidecar injection, and logs no errors. `istio/base` supplies cluster-wide Istio CRDs this recipe's scope never touches. |
| `test-webhook` | `0.1.0` | **no** | five raw manifests from `recipes/test-webhook/manifests/` | no — loaded as an on-disk override | Admission Lab's own deterministic dogfood webhook. Not built in because a built-in recipe's text is embedded at compile time and has no directory to resolve relative manifest paths against. |
| `gateway-api-crds` | `1.5.1` (Gateway API) | **no** — certified as half of `istio-gateway`, never alone | the vendored `standard-install.yaml` bundle under `recipes/istio-gateway/gateway-api/` | no — loaded as an on-disk override | Half of the Istio Gateway API stack, composed **first**. Byte-identical to the upstream release artifact, with its SHA-256 re-checked by the recipe's own test. Declares no capability: it installs an API, not an implementation of one. |
| `istio-gateway` | `1.30.4` | yes — `1.35.8`, `1.36.4`, `1.37.0` | `istio/istiod` from `https://istio-release.storage.googleapis.com/charts`, namespace `istio-system` | no — composed with `gateway-api-crds` above | The other half: the same chart pin as `istio` (machine-checked against it), plus the `gatewayApi` capability and the `gatewayEndpoint` strategy that locates a Gateway's data-plane Service by its well-known `gateway.networking.k8s.io/gateway-name` label. |
| `gateway-api-crds-nginx` | `1.5.1` (Gateway API) | **no** — certified as half of `nginx-gateway-fabric`, never alone | the vendored bundle under `recipes/nginx-gateway-fabric/gateway-api/` | no — loaded as an on-disk override | The same Gateway API release `gateway-api-crds` pins, under its own recipe name because two recipes may not share one. NGF 2.6.7 and Istio 1.30.4 happen to build against the same bundle; the duplication is deliberate rather than a shared file, so either implementation's pin can move without touching the other's. |
| `nginx-gateway-fabric` | `2.6.7` (Gateway API `1.5.1`) | yes — `1.35.8`, `1.36.4`, `1.37.0` | `oci://ghcr.io/nginx/charts/nginx-gateway-fabric`, namespace `nginx-gateway` | no — composed with `gateway-api-crds-nginx` above | The **second certified Gateway API implementation**, and this project's first **OCI** chart reference: NGF publishes its chart only to that registry, so no `helm repo add` runs for it. `releaseName` is deliberately unset — the chart names every object after the release, so the default (this recipe's own name) is what makes the `nginx-gateway/nginx-gateway-fabric` readiness gate name a real Deployment. Readiness additionally waits for the `nginx` `GatewayClass` to be `Accepted`, so the controller is proven *serving the API* and not merely running. |
| `ingress-nginx-legacy` | `4.15.1` (controller v1.15.1) | yes — `1.36.4` only, **migration testing only** | `ingress-nginx/ingress-nginx` from `https://kubernetes.github.io/ingress-nginx`, namespace `ingress-nginx-legacy` | no — loaded as an on-disk override | **The upstream project is retired and its repository archived.** This recipe exists so Admission Lab can test migrations *away* from it, and its presence here is not a recommendation to run it. `4.15.1` is the project's final release, so this is one pin that will never need a bump. It claims the `legacyIngress` capability and deliberately **not** `admission`. See [`recipes/ingress-nginx-legacy/README.md`](../recipes/ingress-nginx-legacy/README.md) for the retirement dates, the upstream's own wording, and Admission Lab's stance. |

Both `recipes/istio-gateway/` and `recipes/nginx-gateway-fabric/` are one
**stack of two components**, not one recipe: the schema has exactly one
`install:` per recipe, so "install the CRDs, then the implementation" is
expressed as an ordered component list, which `install_stack` installs
sequentially, waiting out each component's readiness before the next begins.

Per-recipe Kubernetes certification lives in `compatibility/recipes.yaml`, not
in the recipe files — and it is read by the certification tests at test time
rather than copied, so a recipe and its test cannot silently drift apart.

Two entries are deliberately **narrower** than Admission Lab's own supported
set, and in opposite directions:

- **`kyverno` is certified on `1.35.8` and nowhere else** — deliberately not on
  Admission Lab's own primary `1.36.4` — because Kyverno's documentation for
  this chart line states support for Kubernetes v1.33–v1.35, and certifying past
  it would claim a window the vendor does not.
- **`ingress-nginx-legacy` is certified on `1.36.4` only, which is *outside* its
  own `documentedRange`** of 1.31–1.35 — the only row in the file that is.
  Global Constraint 10 admits the archived recipe at v1 only if it passes on the
  primary supported Kubernetes version, and an archived project will never
  publish a table that includes 1.36, so waiting for one means never shipping
  the migration recipe. Both halves are recorded rather than reconciled: the
  vendor's frozen claim stays as written, and the certified row is Admission
  Lab's own measurement.

`istio`, `istio-gateway`, and `nginx-gateway-fabric` have no vendor constraint
narrow enough to bite — the first two have no published range at all, and NGF's
"1.31+" states no maximum, which is recorded as *unknown* rather than read as
permission — so all three are certified across Admission Lab's entire supported
set, each on its own schedule.

**Which recipe is certified on which Kubernetes version is a shorter list than
"the recipes above" and "the versions Admission Lab provisions" —
[`docs/compatibility.md`](compatibility.md) is that list**, together with what a
certification actually asserts, what happens when you ask for a combination
outside it (a warning, never a refusal), and how the matrix is proposed and
reviewed.

Each recipe directory carries a `README.md` with the full pin rationale,
including the measurements behind the readiness ordering.

### What recipes deliberately do not support

The recipe schema has no `setValues` or `valuesFiles` fields. Every recipe here
therefore installs its chart entirely at default values. That is a
current limitation of the schema, stated here so you do not go looking for the
key.

---

## Capability model

A recipe declares what its component actually provides:

```yaml
capabilities:
  - admission
```

Three capabilities mean something today:

| Capability | What it claims | Declared by |
| --- | --- | --- |
| `admission` | The component participates in the admission chain. | `kyverno`, `istio` |
| `gatewayApi` | The component implements the Gateway API. Paired with the `gatewayEndpoint` metadata the Gateway engine needs to find a `Gateway`'s data plane — by the well-known `gateway.networking.k8s.io/gateway-name` label, for both certified implementations. | `istio-gateway`, `nginx-gateway-fabric` |
| `legacyIngress` | The component serves `networking.k8s.io/v1` `Ingress` objects. It exists so a migration suite can capture the *source* side's traffic behavior, and its `gatewayEndpoint` names the controller Service directly by name. | `ingress-nginx-legacy` |

`legacyIngress` is deliberately its own capability rather than a flavor of
`gatewayApi`: the two describe different APIs with different objects and
different status vocabularies, and collapsing them would let a migration suite
ask an Ingress controller a Gateway API question. Note also what
`ingress-nginx-legacy` does *not* claim — it ships a validating admission
webhook and still does not declare `admission`, because it is never installed as
a stack anyone replays an admission fixture corpus through.

Neither Gateway API CRD bundle declares any capability at all: it installs an
API, not an implementation of one.

A capability is a statement of fact, not an aspiration. The `test-webhook`
recipe deliberately claimed **no** capability until its webhook actually
implemented admission-review handling and its webhook configurations actually
routed fixture pods to it. Claiming a capability a component does not
functionally provide is exactly the fabrication Global Constraint 15 rules out.

---

## Override directories

Recipes can be loaded from a local directory, and this is **always an explicit
opt-in**. Nothing in the codebase discovers an override directory on its own:
there is no environment variable, no working-directory search, and no implicit
default. A caller names the directory or no override directory is consulted.

Loading rules:

- Every `.yaml` / `.yml` file **directly inside** the directory is loaded — not
  recursively. Other extensions and subdirectories are skipped silently; neither
  is a candidate recipe document.
- Files are processed sorted by name, so the result is deterministic.
- A relative `install.paths` entry resolves against **the recipe file's own
  directory** — the same rule `admissionlab.yaml` uses for its own relative
  paths.
- That path resolution is **confined to the recipe's own directory tree**. A
  `../` sequence cannot walk a manifest path outside it, even by accident.
  Everything a recipe causes to be installed is an untrusted test workload
  (PRODUCT.md §29.1), and confinement is one of the guards on that.
- **Two files declaring the same recipe `name` is an error**, not something
  resolved by file order.
- An override with the same name as a built-in replaces the built-in.

---

## Using a recipe's pins today

Until recipes are wired into the lab document, the practical way to use a
certified recipe is to copy its pin into an `install:` block. This is the
Kyverno recipe transcribed into a lab document, comparing it against the
previous chart version:

```yaml
apiVersion: admissionlab.io/v1
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

fixtures:
  include:
    - "fixtures/kyverno/smoke/1*.yaml"
    - "fixtures/kyverno/smoke/2*.yaml"
```

Note what that lab is and is not: the *candidate* side is the certified
`kyverno` 3.9.0 pin on a Kubernetes version `compatibility/recipes.yaml`
certifies it on. The baseline's 3.8.2 is the version you are upgrading *from*,
and Admission Lab certifies nothing about it — which is fine, and is exactly the
ordinary case, but it is why a run like this prints an uncertified-combination
warning naming that side. See [`docs/compatibility.md`](compatibility.md).

**Transcribe the `readiness` list too.** A lab document's own `readiness`
section uses the identical vocabulary, so the recipe's entries paste in
verbatim:

```yaml
apiVersion: admissionlab.io/v1
kind: Lab
baseline:
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
        - type: webhookConfigurationPresent
          name: kyverno-resource-mutating-webhook-cfg
candidate:
  kubernetes: "1.35.8"
fixtures:
  include:
    - "fixtures/kyverno/smoke/1*.yaml"
```

Without it, `helm upgrade --install` returns as soon as the release is applied
and nothing waits for Kyverno's runtime-created webhook configurations to
appear. Fixtures replayed inside that window are admitted by an API server that
never called a webhook — a run that compares two stacks which were not yet doing
anything, and reports no changes.

Existence is still not enforcement: if you apply your own ClusterPolicies after
the chart, install them as a later `type: manifests` component and wait for each
policy's own `Ready` condition with `customResourceCondition`.

---

## Writing your own

The format is small enough to write by hand; start from
[`recipes/kyverno/recipe.yaml`](../recipes/kyverno/recipe.yaml) or
[`recipes/test-webhook/recipe.yaml`](../recipes/test-webhook/recipe.yaml) and
read the neighbouring `README.md`.

If you propose a recipe for the certified set, `CONTRIBUTING.md`'s feature test
applies, plus the recipe-specific one: it must pin exact versions, its readiness
checks must prove the component is actually *serving* rather than merely
scheduled, and it must contain nothing that classifies a difference.
