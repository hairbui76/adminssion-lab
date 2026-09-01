# Legacy community `ingress-nginx` (migration compatibility recipe)

> ## ⚠️ THE UPSTREAM PROJECT IS RETIRED AND ITS REPOSITORY IS ARCHIVED
>
> [`kubernetes/ingress-nginx`](https://github.com/kubernetes/ingress-nginx)
> is **archived and read-only**. Its own README says, verbatim:
>
> > Best-effort maintenance will continue until March 2026.
> > Afterward, there will be no further releases, no bugfixes, and no
> > updates to resolve any security vulnerabilities that may be
> > discovered.
>
> and, under "Usage warnings":
>
> > If you are not already using ingress-nginx, you should not be
> > deploying it as it is not being developed. Instead you should
> > identify a Gateway API implementation and use it.
>
> **Admission Lab agrees, and this recipe is not an exception to that
> advice.** It exists for exactly one purpose: so that a team migrating
> *away* from `ingress-nginx` can put its real behavior on one side of a
> comparison and a Gateway API implementation on the other. ROADMAP Task
> 8.2 Step 2 states the stance plainly — install it "only for migration
> compatibility tests, not as the product's strategic ingress
> recommendation."
>
> Nothing installs this recipe unless a lab file names it. Admission
> Lab's own strategic Gateway recipes are
> [`recipes/istio-gateway/`](../istio-gateway/README.md) and
> `recipes/nginx-gateway-fabric/`.

Everything below was verified against a real `kind` cluster
(Kubernetes 1.36.4) while writing this recipe, or fetched from an
upstream artifact. Where a number appears, it was measured; where a
denial message appears, it came back from a live API server.

## Contents

| File | What it is |
| ---- | ---------- |
| `recipe.yaml` | The recipe: the pinned Helm install, readiness gates, the `legacyIngress` capability, and the controller-Service endpoint strategy |
| `README.md` | This file: provenance, retirement status, pins, findings |

The fixtures this recipe is exercised with live in
[`fixtures/migration/ingress-nginx/`](../../fixtures/migration/ingress-nginx/),
and the test that certifies both is
`crates/admissionlab-recipes/tests/ingress_nginx_legacy.rs`.

## Retirement timeline (provenance)

| Date | Event | Source |
| ---- | ----- | ------ |
| 2025-11-11 | Retirement announced | [kubernetes.io/blog/2025/11/11/ingress-nginx-retirement/](https://www.kubernetes.io/blog/2025/11/11/ingress-nginx-retirement/) |
| 2026-01-29 | Follow-up statement from Steering and the Security Response Committee | [kubernetes.io/blog/2026/01/29/ingress-nginx-statement/](https://www.kubernetes.io/blog/2026/01/29/ingress-nginx-statement/) |
| 2026-03-19 | Final release cut: controller `v1.15.1` / chart `4.15.1` (also `v1.14.5`/`4.14.5` and `v1.13.9`/`4.13.9`) | Repository commit `dbb11b92dd` on `main` |
| 2026-03-23 | Repository archived (`"archived": true` from the GitHub API; last push `2026-03-23T16:16:29Z`) | github.com/kubernetes/ingress-nginx |

Two things worth stating because they are widely mis-remembered:

- **The March 2025 event was not the retirement.** It was
  CVE-2025-1974 ("IngressNightmare"), disclosed 2025-03-24. The
  retirement announcement is eight months later.
- **InGate is not the migration target.** The kubernetes.dev
  announcement says of it: "InGate development never progressed far
  enough to create a mature replacement; it will also be retired." The
  target upstream names is the Gateway API generally — which is exactly
  what Admission Lab's migration comparison puts on the other side.

Upstream also commits that "existing project artifacts such as Helm
charts and container images will remain available," which is what makes
a pinned install of an archived project reproducible at all. It is not a
guarantee with a maintainer behind it, which is one more reason this
recipe pins a digest-carrying chart version rather than a range.

## The pins

```
chart:       ingress-nginx/ingress-nginx 4.15.1
repository:  https://kubernetes.github.io/ingress-nginx
appVersion:  1.15.1        (controller v1.15.1)
namespace:   ingress-nginx-legacy
release:     ingress-nginx-legacy
kubeVersion: >=1.21.0-0    (Chart.yaml; see "Kubernetes support" below)
```

Images the chart itself pins, at that chart version
([`values.yaml` at tag `helm-chart-4.15.1`](https://raw.githubusercontent.com/kubernetes/ingress-nginx/helm-chart-4.15.1/charts/ingress-nginx/values.yaml)):

| Image | Tag | Digest |
| ----- | --- | ------ |
| `registry.k8s.io/ingress-nginx/controller` | `v1.15.1` | `sha256:594ceea76b01c592858f803f9ff4d2cb40542cae2060410b2c95f75907d659e1` |
| `registry.k8s.io/ingress-nginx/controller-chroot` | `v1.15.1` | `sha256:af31d00c9d82c612896b380a9003bd36843b7647b98e4588251c66325317bc72` (`digestChroot`; unused — `controller.image.chroot` defaults to `false`) |
| `registry.k8s.io/ingress-nginx/kube-webhook-certgen` | `v1.6.9` | `sha256:01038e7de14b78d702d2849c3aad72fd25903c4765af63cf16aa3398f5d5f2dd` (the admission-webhook certificate Jobs) |

Admission Lab does not re-pin these; the chart already does, and
overriding them would mean this recipe installed something other than
what chart 4.15.1 *is*. They are recorded here so a reader can verify
what a run pulled without reading the chart.

**4.15.1 is the last one there will be.** ROADMAP Task 8.2 Step 1
forbids a floating or "latest" version for this recipe; here that rule
costs nothing, because the project is archived and 4.15.1 is terminal.
The pin's real job is the opposite of the usual one — not "do not drift
forward" but "record exactly which frozen artifact this behavior was
observed from."

**Why the HTTPS repository and not OCI.** The chart is also mirrored to
`registry.k8s.io/ingress-nginx/charts/ingress-nginx`, but its OCI tags
are `v`-prefixed (`v4.15.1`). Helm derives an OCI tag from the chart
version, so `--version 4.15.1` against the OCI reference is a 404, not
an install. The HTTPS repository (`index.yaml`, confirmed serving 200
and listing 4.15.1) takes the version string as written.

## Kubernetes support

`Chart.yaml` declares `kubeVersion: '>=1.21.0-0'` — an open lower bound
with no upper bound, which is Helm's install-time gate and not a
compatibility statement (see `compatibility/recipes.yaml`'s own header
for why this project never treats it as one).

Upstream's supported-versions table for controller v1.15.1 lists
Kubernetes **1.31 – 1.35**. Admission Lab's own primary is **1.36.4**,
which is outside that window — and this recipe is certified on 1.36.4
anyway, deliberately:

- Global Constraint 10 admits this recipe at v1 *only* "when that
  archived release passes the primary supported Kubernetes integration
  job." Certifying it anywhere but the primary would not satisfy the
  constraint that lets it ship.
- The vendor's window can no longer move. An archived project will never
  publish a table that includes 1.36, so waiting for one means never
  shipping the migration recipe. The honest thing is to state that the
  vendor's documented range stops at 1.35, and to record that Admission
  Lab measured it working on 1.36.4 itself.

`compatibility/recipes.yaml`'s `ingress-nginx-legacy` entry records both
halves: `documentedRange` `1.31`–`1.35` (the vendor's own claim,
unchanged) and a `certified` row for `1.36.4` (Admission Lab's own
measurement). That is the distinction that file exists to express.

The row is Tier 3 (`weeklyRelease`), matching ROADMAP Task 8.9, which
places the legacy stack in a migration-specific tier rather than in the
general recipe matrix.

Task 8.9 landed that placement without inventing a fourth tier word or a
`migrationOnly:` flag, because what makes it migration-specific is what
runs rather than a marker. This recipe is installed in exactly two CI
jobs, both Tier 3:

- its own certification row, which runs
  `crates/admissionlab-recipes/tests/ingress_nginx_legacy.rs` — install
  the chart, route real traffic through a real `Ingress`, and prove the
  validating webhook rejects
  `fixtures/migration/ingress-nginx/webhook-deny.yaml`. It exercises no
  general fixture corpus; this recipe claims the `legacyIngress`
  capability and deliberately not `admission`;
- `.github/workflows/recipe-matrix.yml`'s `migration-demo` job, which
  drives `examples/ingress-to-gateway/` end to end against two real
  clusters (ROADMAP Task 8.8).

"Do not multiply every general matrix by legacy versions" (Task 8.9
step 2) would look like extra rows in this recipe's `certified` list.
There is exactly one, on one Kubernetes version, at the least frequent
tier.

## Installed at chart defaults — a finding

This recipe sets no Helm values at all. The recipe schema has no
`setValues`/`valuesFiles` (see `RawHelmInstall`'s documentation in
`crates/admissionlab-recipes/src/model.rs`, which defers adding them
until a recipe genuinely needs one). This task went looking for that
need and did not find it:

| Default | Value | Why it is fine on `kind` |
| ------- | ----- | ------------------------ |
| `controller.service.type` | `LoadBalancer` | `EXTERNAL-IP` stays `<pending>` forever on `kind`, and it does not matter: `kubectl port-forward` targets the Service's endpoints, not its external address. A real HTTP 200 came back through exactly this Service. |
| `controller.nodeSelector` | `{kubernetes.io/os: linux}` | The `ingress-ready=true` selector, tolerations and `hostPort` that `kind` guides mention come from upstream's separate `deploy/static/provider/kind/deploy.yaml`, **not** from this chart. A single-node `kind` cluster schedules the chart's controller with no help. |
| `controller.hostPort.enabled` / `hostNetwork` | `false` / `false` | Nothing needs a host port under a port-forward model. |
| `controller.admissionWebhooks.enabled` | `true` | Already on. This is what the deny fixture and the second readiness gate depend on. |
| `controller.allowSnippetAnnotations` | `false` | Relied upon, not changed. |
| `controller.watchIngressWithoutClass` | `false` | Relied upon. It is why every fixture `Ingress` must name `ingressClassName: nginx`. |
| `controller.ingressClassResource` | `nginx`, `k8s.io/ingress-nginx`, `default: false` | The class the fixtures name. |

The one default worth knowing about but *not* worth overriding is
`controller.publishService.enabled: true`, which makes the controller
publish a `LoadBalancer` address into each `Ingress`'s
`status.loadBalancer`. On `kind` there is no such address, so that
status stays empty. Admission Lab never reads it — routing is proven by
sending a real request — so leaving the default in place keeps this
recipe closer to a stock install than a values override would.

## Object names

The Helm release name defaults to the recipe name, `ingress-nginx-legacy`.
This chart's `fullname` helper collapses `<release>-<chart>` to just the
release when the release name already contains the chart name — which
`ingress-nginx-legacy` does — so the names are one prefix, not two:

| Object | Name |
| ------ | ---- |
| `Deployment` / `Service` (data plane) | `ingress-nginx-legacy-controller` in `ingress-nginx-legacy` |
| `Service` (webhook backend) | `ingress-nginx-legacy-controller-admission` |
| `ValidatingWebhookConfiguration` | `ingress-nginx-legacy-admission` |
| Webhook entry | `validate.nginx.ingress.kubernetes.io`, path `/networking/v1/ingresses`, `failurePolicy: Fail` |
| `IngressClass` | `nginx` (cluster-scoped; the chart's default name) |

Verified with `helm template` and again against the live cluster. The
recipe's tests assert them, so a chart bump that renamed one would fail
in milliseconds rather than mid-run.

## The endpoint strategy has no placeholders, on purpose

`recipe.yaml`'s `gatewayEndpoint` is a `serviceByName` with a literal
namespace and a literal name — no `{gatewayName}`, no
`{gatewayNamespace}`. That is the structural difference between an
`Ingress` controller and a Gateway API implementation:

- Istio provisions **one data-plane `Service` per `Gateway`**, in the
  `Gateway`'s own namespace, so `recipes/istio-gateway/recipe.yaml` must
  template its strategy on the object being resolved.
- An `Ingress` controller is **one shared data plane** for the whole
  cluster. Every `Ingress`, in every namespace, is served by
  `ingress-nginx-legacy-controller`. There is no per-object Service, so
  there is nothing to substitute.

`portName: http` is required rather than omitted: the Service exposes
both `http`/80 and `https`/443, and with neither `portName` nor `port`
set the resolver has two candidates and correctly refuses to guess.

### The schema change this needed

Before this task, `resolve_recipe` required `gatewayEndpoint` and the
`gatewayApi` capability to be **both present or both absent** (Task
6.6). That rule was right for its two cases and too narrow for a third:
this recipe serves traffic and must be probed, but it is not a Gateway
API implementation and must not claim to be one.

The rule is now stated over the set of capabilities that *serve traffic
Admission Lab probes* — today `gatewayApi` and `legacyIngress`:

> A recipe declaring either of them must carry a `gatewayEndpoint`, and
> a `gatewayEndpoint` is only meaningful on a recipe declaring one of
> them.

Both original errors survive unchanged in force; the field name
`gatewayEndpoint` is kept (it is shared with `admissionlab.yaml`'s own
`gateway.gatewayEndpoint:` block, so renaming it is a schema break for a
cosmetic gain). See `crates/admissionlab-recipes/src/model.rs` for the
rule and `capability.rs` for the vocabulary.

## Readiness, and why two gates

1. `deploymentAvailable ingress-nginx-legacy/ingress-nginx-legacy-controller`
   — the controller is running.
2. `webhookConfigurationPresent ingress-nginx-legacy-admission` — the
   validating webhook is registered.

The second is not decoration. The chart creates the
`ValidatingWebhookConfiguration` at install time and two Helm hook Jobs
(`...-admission-create`, `pre-install`; `...-admission-patch`,
`post-install`) generate the serving certificate and inject the
`caBundle`. Helm waits for a hook Job to complete before returning, so
the injection has happened by the time these gates run — measured: when
gate 1 passed, the webhook object carried a **756-byte `caBundle`**.

Its `failurePolicy` is `Fail`. A run that applied the deny fixture
before the CA bundle landed would *still* see the API server reject the
`Ingress` — for a TLS handshake error, not for the reason the fixture is
testing. Gate 2 is what keeps a passing deny assertion attributable.

## The deny input: `/etc/nginx`, and the two candidates it beat

`fixtures/migration/ingress-nginx/webhook-deny.yaml` submits an
`Ingress` whose path is `/etc/nginx`. The controller's "deep inspector"
refuses it outright:

```go
// internal/ingress/inspector/rules.go @ controller-v1.15.1
invalidEtcDir = regexp.MustCompile("/etc/(passwd|shadow|group|nginx|ingress-controller)")
```

Measured, from a live API server:

```
Error from server (BadRequest): admission webhook
"validate.nginx.ingress.kubernetes.io" denied the request: invalid
object: invalid rule in ingress
admissionlab-ingress-nginx-deny/echo-ingress-denied: invalid http path:
invalid value found: /etc/nginx
```

Three other candidates were tried on the same cluster first:

| Candidate | Result | Verdict |
| --------- | ------ | ------- |
| Invalid regex path (`/foo(` with `use-regex: "true"`) | **Admitted**, exit 0 | Rejected. This release runs no regex-compile validation in the admission path, and `pathType: ImplementationSpecific` — which a `use-regex` path needs — is explicitly skipped by the path-type validator. A fixture built on it would assert nothing. |
| `nginx.ingress.kubernetes.io/configuration-snippet` (snippets disabled by default) | Denied: `... annotation cannot be used. Snippet directives are disabled by the Ingress administrator` | Works, but weaker — see below. |
| An `Ingress` that fails `nginx -t` template rendering | Not available | `testTemplate` is commented out in `CheckIngress` to mitigate CVE-2025-1974. This release never shells out to `nginx -t`. |

**Why the snippet candidate lost even though it works.** `CheckIngress`
filters on the ingress class *before* it reaches the annotation loop,
and an `Ingress` whose class the controller's informer has not yet
synced returns `nil` — **silently admitted**. Right after an install
that is a real race, and its failure mode is the worst kind available: a
deny test that passes by admitting. The deep-inspector check runs
*first*, ahead of that filter. Verified on the live cluster: the `/etc/nginx`
`Ingress` is denied identically **with and without**
`ingressClassName: nginx`, so no informer state can make the fixture
quietly stop testing anything.

## What was measured

Kubernetes 1.36.4 on a single-node `kind` cluster
(`kindest/node:v1.36.4@sha256:099e0493…`), chart 4.15.1 at defaults:

| Step | Measurement |
| ---- | ----------- |
| `helm upgrade --install` returned | 22.5 s |
| Controller `Deployment` `Available=True` | 7.6 s after that |
| `ValidatingWebhookConfiguration` `caBundle` at that moment | 756 bytes |
| `Ingress` → port-forward → `Host: basic.ingress.admissionlab.test` | `HTTP/1.1 200 OK`, body `{"backend":"echo-a", ...}` |
| Deny fixture | `BadRequest`, exit 1, message above |

Response headers the controller adds to a probe (`x-request-id`,
`x-forwarded-*`, `x-real-ip`, `x-scheme`) are the only nondeterminism it
contributes, and `admissionlab_gateway::probe` already normalizes probe
responses for every implementation — which is why `recipe.yaml` declares
no `normalizeRules`. Read back after reconciliation, the fixture's own
objects carry no controller-authored annotation, label or `spec` field.

## What this recipe deliberately does not do

- **No classification logic.** Global Constraint 6 / PRODUCT.md §14: a
  recipe supplies install, readiness, normalization and capability
  metadata only. Whether a difference between this stack and a Gateway
  API stack is a *regression* is decided elsewhere, by
  `admissionlab-diff` and `admissionlab-policy`.
- **No `admission` capability**, even though it demonstrably serves a
  validating webhook. That capability is a claim of certification
  against Admission Lab's admission fixture corpus; this webhook
  validates `Ingress` objects and nothing else.
- **No TLS, no `defaultBackend`, no metrics.** Every one is a chart
  default (off) that the migration comparison does not read.
