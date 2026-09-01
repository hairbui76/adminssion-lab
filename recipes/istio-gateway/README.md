# Istio Gateway API (certified recipe)

[Istio](https://istio.io/)'s implementation of the [Kubernetes Gateway
API](https://gateway-api.sigs.k8s.io/): `istiod` watches `Gateway`
objects, provisions an Envoy data plane for each one, and routes real
HTTP traffic through it according to `HTTPRoute`. Both projects are
Apache-2.0. This is the recipe Admission Lab's Gateway behavior engine
(ROADMAP Phase 6) is certified against — a different Istio feature, and
a different recipe, from the sidecar-injection one in
[`recipes/istio/`](../istio/README.md).

Everything below was verified against real `kind` clusters while writing
this recipe. Where a number appears, it was measured; where a behavior is
described, it was observed. The measurements are collected in
"[What was measured](#what-was-measured)".

## This directory is a two-component stack

| File | Recipe name | Installs | Version |
| ---- | ----------- | -------- | ------- |
| `gateway-api-crds.yaml` | `gateway-api-crds` | The vendored Gateway API CRD bundle, as raw manifests | Gateway API `1.5.1` |
| `recipe.yaml` | `istio-gateway` | `istio/istiod` via Helm, and the `gatewayApi` capability | Istio `1.30.4` |

**They must be composed in that order**, CRDs first.
`admissionlab_installer::install_stack` takes an ordered component list
and fully waits out each component's readiness before the next one's
install begins, so ordering the two is all that "install the API, then
its implementation" requires — and `gateway-api-crds`'s own readiness
checks (every CRD `Established`) are what make that ordering an assertion
rather than an assumption.

Why two recipes rather than one: this crate's recipe schema has exactly
one `install:` block per recipe and no shape for installing two things.
That is not a gap this task worked around — it is the answer
[`recipes/istio/README.md`](../istio/README.md) already recorded for the
same situation ("a future task that needs Istio custom resources should
add `istio/base` as its own, separate built-in recipe and compose it
ahead of this one in a stack, rather than this recipe silently growing a
second Helm install it does not otherwise need"). Two components in a
stack is how this project spells "install A, then B".

Neither document is wired into `BUILTIN_RECIPES`. `gateway-api-crds`
installs a manifest by a path relative to its own directory, and an
embedded built-in recipe has no filesystem location to resolve one
against (`admissionlab_recipes::load`'s own documentation), so it *could
not* be a built-in; loading the other half by a different mechanism would
mean one stack loaded two ways. Both are loaded with
`load_recipe_overrides(recipes/istio-gateway)`, exactly as
[`recipes/test-webhook/`](../test-webhook/) already is.

## The Gateway API CRD pin: v1.5.1, vendored byte-for-byte

```
url:    https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.5.1/standard-install.yaml
sha256: 751002b3b91a87f7ae3bd2517c79a47a8d7ed6702901808a1cf9bd97d284f9b8
size:   1024333 bytes
path:   recipes/istio-gateway/gateway-api/standard-install-v1.5.1.yaml
```

**Which version, and how it was chosen.** Not "the newest": the bundle
the pinned Istio release actually builds against. `istio/istio` at tag
`1.30.4` declares `sigs.k8s.io/gateway-api v1.5.1` in its own `go.mod`
(fetched from `raw.githubusercontent.com`), so v1.5.1 is the API version
`istiod` 1.30.4's Gateway controller was compiled and tested against.
When `recipes/istio/recipe.yaml`'s chart pin moves, this pin is
re-derived from that release's `go.mod` — never bumped on its own
schedule.

**Standard channel, not experimental.** Everything this recipe's fixtures
use (`GatewayClass`, `Gateway`, `HTTPRoute`, `ReferenceGrant`) is in the
standard channel. The experimental channel adds alpha APIs whose schema
can change between patch releases — a moving target underneath a tool
whose entire job is telling a real behavior change from noise. Upstream's
own v1.5.1 release notes add a second, practical reason: *"The
Experimental channel CRDs are too large for a standard `kubectl apply`.
To work around this please use `kubectl apply --server-side=true`
instead"* — and `admissionlab_installer::manifests` applies
`--server-side=false`, deliberately and by frozen design (see that
module's `"--server-side=false"'s known failure mode, made legible`
section).

**Vendored rather than fetched, and that was not a preference.**
`install.type: manifests` names local files; nothing in this project
fetches a manifest over the network at install time. `compatibility/kubernetes.yaml`
states the same rule for node images ("Admission Lab never fetches this
information over the network at runtime"), and a pinned URL plus a
checksum would still be a network dependency in the middle of every
cluster install — one that fails differently on a machine with no route
to github.com. The vendored file is byte-identical to the upstream
release artifact, and
`crates/admissionlab-recipes/tests/istio_gateway_recipe.rs` recomputes
its SHA-256 on every `cargo test` run, so an edit to a vendored
third-party artifact is a test failure rather than something review has
to catch.

### The one number worth watching: 246,139 of 262,144 bytes

Client-side `kubectl apply` stores the entire applied object in the
`kubectl.kubernetes.io/last-applied-configuration` annotation, and
Kubernetes caps total `metadata.annotations` at a hard 262,144 bytes.
Measured on a live cluster immediately after this bundle was applied
with `--server-side=false`:

| CRD | `last-applied-configuration` | Headroom |
| --- | ---------------------------- | -------- |
| `httproutes.gateway.networking.k8s.io` | **246,139 bytes** | 16,005 bytes (6.1%) |

So the standard channel fits — today, with 6% to spare. This is recorded
rather than left to be rediscovered because the margin is thin enough
that a future Gateway API release could cross it. If one does, nothing
silently degrades: `admissionlab_installer::manifests` recognizes
Kubernetes's own validation message for exactly this failure and reports
`InstallError::ManifestExceedsAnnotationLimit`, naming the cause. The
remedy at that point is a decision (server-side apply as an opt-in
install field, or a narrower vendored bundle), not something this recipe
should pre-empt.

## Shared version discipline with `recipes/istio`

`recipes/istio/recipe.yaml` is the **source of truth** for which Istio
release this project installs. `recipes/istio-gateway/recipe.yaml`
restates the same `chart`, `repo`, `version` and `namespace` because YAML
has no include, no cross-file anchor and no variable — there is no
mechanism by which one recipe document can reference another's field.

What makes the restatement safe is not this paragraph:

```
crates/admissionlab-recipes/tests/istio_gateway_recipe.rs
  → check_shared_istio_install()
```

loads **both** recipes and fails if the chart, the repository URL, the
chart version, the target namespace, or the recipe version has drifted
apart, naming both files. It is not `#[ignore]`d — it needs no cluster
and runs under plain `cargo test --workspace` — so bumping one recipe
alone breaks the build in milliseconds. Duplication a machine checks is a
different thing from duplication a reviewer is asked to.

One field is deliberately *not* shared: `install.repoName`, which
`recipes/istio` leaves unset (its recipe name already is `istio`) and
this recipe must set explicitly. `admissionlab_installer::helm` runs
`helm repo add <repoName> <repo>` and then installs the literal chart
reference `istio/istiod`, so the alias registered has to be the one that
reference names; the default would be the recipe's own name,
`istio-gateway`, and the chart reference would name an alias that was
never added.

## Only `istiod` — verified, again, for a different feature

`recipes/istio/README.md` established that `istio/istiod` alone (no
`istio/base`, so none of Istio's own CRDs) is enough for sidecar
injection. That finding does not automatically carry to Gateway API, so
it was re-verified rather than assumed. With only `istio/istiod` and the
Gateway API CRDs installed:

- `istiod` creates `GatewayClass/istio` (controller
  `istio.io/gateway-controller`) for itself, `Accepted=True`, along with
  `GatewayClass/istio-remote` (controller `istio.io/unmanaged-gateway`,
  which provisions nothing and this recipe never uses).
- A `Gateway` naming that class gets a `Deployment`, a `Service`, a
  `ServiceAccount`, an `HorizontalPodAutoscaler` and a
  `PodDisruptionBudget`, all owned by the `Gateway` object.
- Real HTTP traffic flows through the resulting Envoy to a backend.

Nothing needed `istio/base`, `istio/cni`, `istio/ztunnel`, or the ambient
profile: Istio's Gateway API support lives entirely in `istiod`'s own
deployment controller.

## Readiness: four checks, four different questions

`recipe.yaml` gates on:

1. `Deployment istio-system/istiod` `Available` — the control plane
   process exists and passes its own health check.
2. `CustomResourceDefinition gateways.gateway.networking.k8s.io`
   `Established`, and
3. the same for `httproutes...` — the API this recipe claims to serve is
   actually being served. `gateway-api-crds.yaml` already asserts this
   (for four kinds); this recipe restates it for the two its own
   capability is *about* so that composing it without the CRD component
   fails readiness naming the missing CRD, rather than "installing
   successfully" and failing much later when a fixture's `Gateway` is
   rejected as an unknown kind.
4. `GatewayClass/istio` `Accepted=True` — the strongest of the four, and
   the reason the first three are not the whole story. A Deployment
   condition cannot tell you that `istiod`'s *Gateway controller* is
   running and reconciling Gateway API objects on this cluster; an
   `Accepted` GatewayClass can, because `istiod` writes it only after
   that controller starts and only when the CRDs exist.

`gateway-api-crds.yaml` gates on `Established` for exactly the four kinds
the fixtures create (`GatewayClass`, `Gateway`, `HTTPRoute`,
`ReferenceGrant`) — not for the four the bundle also installs and nothing
here uses (`BackendTLSPolicy`, `GRPCRoute`, `TLSRoute`, `ListenerSet`),
because gating on a kind this project never creates would turn an
unrelated upstream change into a false install failure.

## Endpoint resolution: by label, not by name

```yaml
gatewayEndpoint:
  type: serviceBySelector
  namespace: "{gatewayNamespace}"
  selector:
    gateway.networking.k8s.io/gateway-name: "{gatewayName}"
  portName: http
```

Read off a live cluster, for `Gateway/lab-gateway` in
`admissionlab-istio-gateway-same`, Istio provisioned:

```
Service/lab-gateway-istio            # <gateway name>-<class name>
  labels:
    gateway.networking.k8s.io/gateway-name: lab-gateway
    gateway.networking.k8s.io/gateway-class-name: istio
    gateway.istio.io/managed: istio.io-gateway-controller
  ports:
    - name: status-port   port: 15021
    - name: http          port: 80
```

- **Selector, not name**, even though `<gateway>-<class>` is perfectly
  predictable: the derived name embeds the GatewayClass, so a fixture
  using a second class would need a second recipe to find its Service —
  and the label is Gateway API's own documented "gateway infrastructure
  label", which Istio applies because upstream specifies it, while the
  name pattern is Istio's own convention.
- **`portName: http` is required, not optional.** The provisioned Service
  exposes two ports; with neither `portName` nor `port` set,
  `admissionlab_gateway::endpoint` has two candidates and correctly
  refuses to guess. The port *name* is Istio's; the port *number* is the
  fixture's listener choice, which is why the name is what this recipe
  pins.
- **Only the gateway-name label is selected on.** Adding
  `gateway-class-name` would pin the strategy to one class for no gain: a
  Gateway's name is already unique within its namespace, so this selector
  can never match a second Gateway's data plane.

## No normalization rules, and the evidence for that

`recipe.yaml` declares no `normalizeRules:`. That is a finding, not an
omission. Every object the fixtures create was read back from a live
cluster after Istio had reconciled it: no Istio-authored annotation,
label or `spec` field appears on any of them. The only additions are
Gateway API's own CRD schema defaults (`parentRefs[].group`/`kind`,
`backendRefs[].group`/`kind`/`weight: 1`) — written by the API server
from the CRD schema, identical on every cluster serving the same bundle
version, and therefore not nondeterminism to normalize away.

Istio's genuinely nondeterministic output lives on objects Istio
*creates* (which no fixture declares and no comparison reads) and in HTTP
responses — the `x-envoy-*`/`x-request-id` headers a probe sees — which
`admissionlab_gateway::probe` already handles in code that applies to
every implementation rather than to this one vendor. A rule added "just
in case" would be the failure mode PRODUCT.md §14 warns about: a
normalization rule deletes evidence, so an unnecessary one silently
narrows what a comparison can ever detect.

## Fixtures: `fixtures/gateway/istio/`

Two files, each self-contained (`apply_gateway_manifests` applies each
document to the namespace the document itself names, so a fixture that
left a namespace unstated would land in `default`):

- **`same-namespace.yaml`** — `Namespace`, the `ConfigMap` described
  below, `echo-a` (Service + Deployment), `Gateway/lab-gateway` (class
  `istio`, one listener named `http` on port 80), and
  `HTTPRoute/echo-route` for `same.gateway.admissionlab.test` →
  `echo-a:80`.
- **`cross-namespace.yaml`** — the same shape with the backend (`echo-b`)
  moved into a second namespace, the route's `backendRefs[0]` naming that
  namespace explicitly, and a `ReferenceGrant` **in the backend's
  namespace** permitting `HTTPRoute`s from the route's namespace to
  reference `Service/echo-b` by name. Without the grant, Gateway API
  requires the implementation to report `ResolvedRefs=False`
  (`RefNotPermitted`).

Each fixture carries a **copy** of `fixtures/gateway/backends/echo-{a,b}.yaml`'s
two objects with `metadata.namespace` added and nothing else changed.
That copy is machine-checked:
`fixture_backends_match_the_shared_echo_backend_definition` parses both
the shared definition and the fixture's copy and fails if they differ in
any field other than `metadata.namespace`. Including the file by
reference instead would apply it to no namespace at all — the shared
backends deliberately declare none, so the applying suite can choose one,
and `apply_gateway_manifests` has no such parameter.

### THE FINDING: on `kind`, a Gateway is never `Programmed` by default

Istio's provisioned data-plane `Service` defaults to `type:
LoadBalancer`. A bare `kind` cluster has no load-balancer controller, so
the external address is never assigned, and Istio reports — correctly,
and **permanently**:

```
Programmed=False (AddressNotAssigned: Assigned to service(s)
lab-gateway-istio.<ns>.svc.cluster.local:80, but failed to assign to all
requested addresses: address pending for hostname
"lab-gateway-istio.<ns>.svc.cluster.local")
```

This is a terminal state, not a slow one — waiting longer never fixes it,
and a reconciliation timeout tuned upward would only take longer to
report the same thing. Both fixtures therefore carry a `ConfigMap`
referenced from `Gateway.spec.infrastructure.parametersRef`:

```yaml
data:
  service: |
    spec:
      type: ClusterIP
```

which is Istio's own documented per-Gateway override (the `istiod`
chart's `values.yaml` documents the identical shape under
`gatewayClasses.<class>.service.spec.type` and states "Per-Gateway
configuration can also be set in the `Gateway.spec.infrastructure.parametersRef`
field"). With it, the same Gateway reached `Programmed=True` in about 2
seconds.

**It lives in the fixture, not in this recipe, on purpose.** Which
Service type a Gateway wants is a property of the environment a fixture
runs in. A recipe that forced `ClusterIP` onto every Gateway installed
through it would be deciding a vendor behavior question on the user's
behalf (Global Constraint 6 — a recipe supplies install/readiness/
normalization/capability metadata, not behavior policy), and a fixture
that genuinely wants a routable address on a LoadBalancer-capable cluster
simply omits the `ConfigMap`. The apply engine already orders
`ConfigMap` (`Configuration`) before `Gateway`, so the override is always
in place before Istio's deployment controller first reads it.

## What was measured

On the reference machine (warm node image, warm Docker layer cache),
Kubernetes 1.35.8 unless stated:

| Step | Measurement |
| ---- | ----------- |
| `kubectl apply` of the whole 1 MB CRD bundle (client-side) | 0.53 s |
| `helm upgrade --install istiod` → returns | 0.67 s |
| `Deployment/istiod` → `Available=True` | 12.5 s after that |
| `GatewayClass/istio` → `Accepted=True` | present ~1 s after istiod was Available |
| `Gateway` → `Programmed=True` (with the `ClusterIP` override) | ~2 s from apply |
| `Gateway` → `Programmed` (without it) | never — `AddressNotAssigned`, terminal |
| Route `Accepted`/`ResolvedRefs` (both fixtures) | `True` within the same ~2 s window |
| Data-plane pod → `Ready` | ≤25 s from apply (already `1/1` when first polled) |
| HTTP probe through the port-forward | 200, correct backend, first attempt |

And from the certification run itself — three disposable clusters
(1.35.8, 1.36.4, 1.37.0), two fixtures each, all six passing:

| Fixture | Reconciliation | Endpoint resolved | Probe |
| ------- | -------------- | ----------------- | ----- |
| `same-namespace` | 267 ms, 1 observation, `converged=true` | `admissionlab-istio-gateway-same/lab-gateway-istio:80` | `200`, backend `echo-a`, 6.6-8.3 ms, 1 attempt |
| `cross-namespace` | 266 ms, 1 observation, `converged=true` | `admissionlab-istio-gateway-cross-route/lab-gateway-istio:80` | `200`, backend `echo-b`, 5.2-6.0 ms, 1 attempt |

Whole test, end to end — three `kind` clusters created and deleted, the
echo image built and loaded into each, both components installed on each,
four Gateway API objects reconciled and four HTTP requests routed:
**269.95 s**, on a warm node-image and Docker layer cache. The figures
above are stable across all three Kubernetes versions to within a few
milliseconds.

### "Converged" is not "finished" — measured on three clusters

The first version of the integration test called
`wait_for_route_reconciliation` immediately after applying a fixture and
asserted on what came back. On Kubernetes 1.35.8, 1.36.4 and 1.37.0 alike
it got, every single time:

```
reconciled in 270ms, converged=true
expected Gateway condition "Programmed" to be True,
  got False (reason AddressNotAssigned)
```

Nothing was wrong. `wait_for_route_reconciliation` answers *"has this
route's status stopped changing?"* — settled conditions, current for the
object's generation, identical across two polls at least 250 ms apart —
and 270 ms after apply Istio had already written a status that satisfies
all of that: `Accepted=True`, `Programmed=False`, because the data plane
it is a statement about did not exist yet. The convergence rule was
right; the assertion was asking a stability question and reading the
answer as a finality one.

Both halves of the fix are in the test, and both are worth copying into
any future Gateway recipe's certification:

1. **Wait for the data-plane `Deployment` before asking about
   `Programmed`.** That condition is a statement about the data plane, so
   waiting for it first is the correct ordering, not a workaround.
2. **Re-observe until correct, with a deadline.**
   `observe_until_reconciled` re-runs the whole convergence rule until
   every certified condition is `True`, and on timeout reports *which
   condition* never became true rather than a bare timeout.

The reconciliation budget in the test
(`RECONCILIATION_TIMEOUT = 120 s`) is deliberately far above the measured
convergence. What varies is not Istio's status write but everything
around it — a cold `istiod` still electing to reconcile, a loaded CI
runner, the data-plane `Deployment` being scheduled. A timeout is
evidence rather than a verdict in this project
(`admissionlab_gateway::reconcile` returns `converged: false`, not an
error), so an over-generous bound costs nothing on the happy path while a
tight one turns a slow runner into a false certification failure.

A route being `Programmed` does **not** mean traffic will succeed: the
gateway's own pod, and the backend's, may still be starting. The
integration test waits for both `Deployment`s to report `Available`
through `admissionlab_installer::KubeReadinessProbe` before sending a
single request — `admissionlab_gateway::probe` retries only *connection*
failures, within a 5-second window, and treats an HTTP 503 as the real
answer it is.

## Kubernetes certification

`compatibility/recipes.yaml`'s `istio-gateway` entry — not this file — is
the source of truth. It certifies **1.35.8, 1.36.4 (Tier-1 primary) and
1.37.0**: neither upstream states a Kubernetes support range for what
this recipe installs (Istio 1.30.4 declares no `kubeVersion`; Gateway
API v1.5.1's release notes state none, their only version remark being
"TLSRoute's CEL validation requires Kubernetes 1.31 or higher" — about a
kind this recipe never uses), so `documentedRange` is explicitly `null`
and nothing narrows the certified set below Admission Lab's own supported
matrix.

`crates/admissionlab-recipes/tests/istio_gateway_recipe.rs` reads that
list at test time and installs the stack once per listed version, each in
its own disposable cluster — so editing `certified` changes how many
clusters, at which versions, the next run creates.

There is deliberately no separate `gateway-api-crds` row: that component
installs CustomResourceDefinitions and nothing else, is never installed
alone, and has no behavior of its own to certify against a Kubernetes
version independently of the implementation that serves the API it adds.

## What this recipe does not do

- **It declares no `admission` capability.** `istiod` installed this way
  does serve the sidecar-injection webhook, and
  [`recipes/istio/`](../istio/README.md) is the recipe certified for
  exactly that. A capability is a claim about what a recipe has been
  certified to do; claiming `admission` here would make fixture selection
  run admission fixtures against a certification that never covered them.
- **It classifies nothing.** No severity, no pass/fail, no "this is a
  regression" — Global Constraint 6, enforced structurally by the recipe
  schema's `deny_unknown_fields` allow-list at every nesting level, and
  proven by mutation in
  `a_severity_field_cannot_be_added_to_this_recipe`.
- **It pins no container image digest.** The reproducible unit a Helm
  recipe pins is the chart version, not the `pilot`/`proxyv2` image tags
  its `values.yaml` selects — the same note
  [`recipes/istio/README.md`](../istio/README.md) and
  [`recipes/kyverno/README.md`](../kyverno/README.md) both carry.
