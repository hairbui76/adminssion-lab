# Istio (certified recipe)

[Istio](https://istio.io/) is a service mesh; the piece this recipe
cares about is its **sidecar injector** — a `MutatingWebhookConfiguration`
served by `istiod` that adds an Envoy proxy to a Pod at admission time.
Licensed Apache-2.0. Source: <https://github.com/istio/istio>. This is
the second third-party admission stack Admission Lab installs (Task
2.9), after Kyverno (Task 2.8) — the last task in Phase 2.

## What this recipe pins, and why it is `istiod` alone

- **Chart:** `istio/istiod` version `1.30.4`, from the official chart
  repository `https://istio-release.storage.googleapis.com/charts`.
- **appVersion:** `1.30.4`.

Almost every Istio installation guide — including this project's own
prior research notes — installs `istio/base` (cluster-wide
CustomResourceDefinitions: `VirtualService`, `Gateway`, `DestinationRule`,
and eleven others, plus a `ValidatingWebhookConfiguration` for them)
*before* `istio/istiod`. **This recipe deliberately installs `istiod`
alone.** This is a verified finding from working on this task, not an
oversight or a shortcut taken to save effort:

1. This recipe's whole job (brief Interfaces line) is "the minimum Istio
   components needed for admission/sidecar injection tests" — and
   `istio/base`'s CRDs (`VirtualService`, `Gateway`, ...) are never
   created, read, or referenced by anything in this recipe's own fixture
   pack.
2. Installing `istiod` **alone** on a real `kind` cluster (no
   `istio/base`, ever) was verified directly:
   - `Deployment/istiod` reaches `Available: True` and its single Pod
     reaches `1/1 Running`, with **zero restarts**.
   - Sidecar injection **works identically** to an installation with
     `base` present — the same `istio-init`/`istio-proxy`
     `initContainers` pair, confirmed by reading back a created Pod (see
     "Sidecar injection" below).
   - `istiod`'s own logs contain **zero `error`-level lines** and
     exactly two benign `warn`-level lines across the entire startup:
     `"discovery is not ready"` (transient, self-resolving) and
     `"Missing Gateway CRD, cannot perform validation check. Assuming
     validation is ready"` (`istiod`'s *own* CRD-validation webhook —
     `failurePolicy: Ignore`, and irrelevant to this recipe, which never
     creates a Gateway) — istiod's `crdclient`/`krt` layer is explicitly
     built to tolerate an absent CRD (every Istio CRD informer logs a
     clean `"sync complete"` even though the type does not exist on this
     cluster at all).
3. `crates/admissionlab-installer/src/helm.rs`'s own module documentation
   (written before this task, for `--create-namespace`) observes that
   "neither `istio/base` nor `istio/istiod` creates their own target
   namespace object" — true of both charts regardless of whether one or
   both are installed, and not itself a claim that both must be.

**What this means, concretely, and its one real limitation:** this
recipe cannot be used to install or validate anything that needs an
Istio custom resource (`VirtualService`, `Gateway`, `PeerAuthentication`,
and so on) — attempting to `kubectl apply` one against a cluster this
recipe installed onto fails outright ("no matches for kind"), because
the CRD was never created. That is out of scope for this recipe's own
job (sidecar-injection admission behavior); a future task that needs
Istio custom resources should add `istio/base` as its own, separate
built-in recipe (`recipes/istio-base/recipe.yaml`) and compose it ahead
of this one in a stack, rather than this recipe silently growing a
second Helm install it does not otherwise need — see
`crates/admissionlab-installer/src/stack.rs`'s own module documentation
for why installing two charts through this crate's `Recipe`/
`ResolvedComponent` model already means two separate, ordered
components, not one recipe with two installs (the schema has no
"install more than one chart" shape at all).

This is a direct deviation from the (unverified) advice in this
project's own prior Istio research and from the controller's initial
task supplement, which both recommended installing `base` first,
following the general Istio convention. Per that supplement's own §10
("this document is evidence, not authority... if the real chart... or a
real cluster contradicts it, report that rather than working around it
silently"), this is reported here rather than followed silently against
what a live cluster actually showed.

## Kubernetes certification: all three supported versions

`compatibility/recipes.yaml`'s own `istio` entry — not this file — is
the source of truth for which Kubernetes version(s) this recipe is
certified against, and unlike Kyverno's entry (deliberately narrower
than Admission Lab's own matrix), Istio's is the **full** matrix:

- Neither `istio/base` nor `istio/istiod` 1.30.4 declares a
  `kubeVersion` constraint in `Chart.yaml`, and no "Kubernetes versions
  supported" statement from Istio's own documentation was found.
  `documentedRange` is therefore explicitly recorded as `null` — Global
  Constraint 15: an absent or unbounded constraint means "unknown,"
  never "supported," and is not restated as a support claim here.
- With no documented constraint narrowing it, `certified` is Admission
  Lab's entire supported set: `1.35.8`, `1.36.4` (Tier-1 primary per
  `compatibility/kubernetes.yaml`), `1.37.0`.

`crates/admissionlab-recipes/tests/istio_recipe.rs` — this recipe's own
certification test — reads that `certified` list at test time and
installs this recipe **once per listed Kubernetes version**, in its own
disposable cluster, rather than hardcoding a copy of any one of them
here or there: an edit to `certified` in `compatibility/recipes.yaml`
changes how many clusters, and at which versions, that test creates on
its next run. (Compare Kyverno's own certification test, which certifies
against a single version because its own `certified` list has exactly
one entry — the mechanism is shared; the list length is not.)

## Sidecar injection: `initContainers`, not `containers`

The single easiest way to write a false-negative "injection didn't
happen" assertion against this recipe: nearly every Istio tutorial shows
the injected Envoy proxy landing in `spec.containers`. On **every**
Kubernetes version this project supports (1.35.8, 1.36.4, 1.37.0), it
does not.

Traced to Istio source at tag `1.30.4`
(`pkg/kube/inject/webhook.go`, `DetectNativeSidecar`): the gate is
Kubernetes **1.33**, the minor where native sidecar containers went
*stable* (not the widely quoted ~1.29 "went beta" milestone).
`ENABLE_NATIVE_SIDECARS` defaults to `"auto"`, which reads each node's
kubelet version; every node image this project pins is >= 1.33, so
`auto` resolves to **enabled** uniformly across the whole supported
matrix — deterministic, just not what most tutorials show.

After injection, a Pod gains, in `spec.initContainers`:

1. `istio-init` — ordinary init container (no `restartPolicy`), runs
   `istio-iptables`.
2. `istio-proxy` — the Envoy sidecar, **`restartPolicy: Always`** (a
   native/"sidecar" container, not a regular one).

`spec.containers` is unchanged — only the fixture's own `app` container.
`metadata.annotations["sidecar.istio.io/status"]` is present once
injected. All of the above was read back from a real created Pod, not
assumed: `fixtures/istio/smoke/10-inject-pod.yaml`'s own comment and
`crates/admissionlab-recipes/tests/istio_recipe.rs` show the exact
assertions.

## The injection namespace label, and why `istio-injection: enabled`

Read from the **rendered** `MutatingWebhookConfiguration/istio-sidecar-injector`
(`helm template istio/istiod` at chart 1.30.4, inspected directly), not
assumed from memory. The chart renders **four** webhook entries, every
one `failurePolicy: Fail`; the one this recipe's fixtures rely on is
`namespace.sidecar-injector.istio.io`, whose `namespaceSelector` is
exactly `istio-injection In [enabled]` — the "legacy", non-revisioned
opt-in label. (The other three entries additionally support a newer
`istio.io/rev: default`-based opt-in this recipe does not use, and one
of them requires *absence* of the `istio-injection` label — the two
schemes are mutually exclusive by design, not redundant.)

`fixtures/istio/smoke/00-namespaces.yaml` labels
`admissionlab-istio-smoke-inject` with exactly this label, and gives the
Istio fixture pack its own dedicated namespace pair — **never** shared
with the test-webhook or Kyverno smoke fixtures' namespaces. This
matters specifically because `failurePolicy: Fail` here is scoped by
namespace selector, not by resource: *any* Pod created in an
`istio-injection: enabled` namespace is subject to fail-closed behavior
if `istiod` becomes unavailable, so an Istio-specific outage must not be
able to collaterally block an unrelated fixture.

## Sequencing: `failurePolicy: Fail` before the `caBundle` is patched — and whether that is a race

The injector's `MutatingWebhookConfiguration` renders with
`failurePolicy: Fail` **hard-coded** on every one of its four webhook
entries, and with **no `caBundle` at all** (confirmed by rendering the
chart: the string `caBundle` appears nowhere in 147,510 bytes of
output). Istio's own webhook-cert controller, running inside `istiod`,
patches the `caBundle` in later, once it is ready. So the object is
fail-closed from the moment it exists, before it can work — this is a
real, documented Istio failure mode (the "connection refused" error
Istio's own troubleshooting docs describe).

**Settled, on a live cluster, empirically (not by reasoning about the
code alone):** whether `Deployment/istiod` reaching `Available: True` is
itself sufficient to guarantee the `caBundle` patch has already landed,
or whether a narrow race exists. Method: a fresh `istiod` install,
polled every 200ms from before `helm upgrade --install` even started,
recording the first tick at which (a) the webhook's `caBundle` becomes
non-empty and (b) `Deployment/istiod`'s `Available` condition becomes
`"True"`. Run twice, independently, on two separate installs:

| Trial | `caBundle` first non-empty | `Available` first `True` | Margin |
| ----- | --------------------------- | -------------------------- | ------ |
| 1     | ~4.16s                       | ~7.43s                      | ~3.3s  |
| 2     | ~3.77s                       | ~7.14s                      | ~3.4s  |

**Answer: no race was observed.** In both trials `caBundle` was patched
in ~3.3s before `Available` went `True`, never the other way around.
Stated precisely, because the distinction matters: this is two samples
on one development machine, so it is evidence that the ordering holds
comfortably here, not proof that it holds on every cluster. A slower or
heavily loaded node could in principle narrow or invert the margin. No
counter-evidence was found, and the mechanism below explains why the
observed ordering is the expected one -- but a caller that must not
tolerate the inverted case should check `caBundle` directly rather than
rely on `deploymentAvailable` alone. This is consistent with
`istiod`'s own architecture: the in-process webhook-cert controller
completes its first reconcile early in startup, well before the
container's own HTTP readiness endpoint (which ultimately drives
`Available`) begins passing. `recipe.yaml`'s readiness therefore gates
only on `DeploymentAvailable{istio-system, istiod}` — as the controller
supplement instructed doing regardless — with no additional `caBundle`
poll layered on top, because the measured margin gives no reason to
believe one is needed. `crates/admissionlab-recipes/tests/istio_recipe.rs`
still asserts the `caBundle` is non-empty once, immediately after
`install_stack` returns — a single documented sanity check of this
finding, not a retry loop that could paper over a future regression.

Same *shape* of finding as Kyverno's own webhook-configuration race
(Ruling R33: object existence there is not proof a specific policy's
rule is live) — a different mechanism, the same lesson: an object's
mere existence is not proof of readiness. `readiness.rs`'s
`WebhookConfigurationPresent` checks existence only, which is why this
recipe's own use of it (below) is deliberately narrow.

## Why readiness does not also gate on `istio-validator-istio-system`

The `istiod` chart itself (not `base`) additionally renders a *second*
`ValidatingWebhookConfiguration`, `istio-validator-istio-system`, scoped
to Istio custom-resource API groups only (`security.istio.io`,
`networking.istio.io`, `telemetry.istio.io`, `extensions.istio.io`) and
set to `failurePolicy: Ignore` — fail-*open*, and irrelevant to a Pod
admission path this recipe's fixtures exercise. `recipe.yaml`'s
readiness therefore names only `istio-sidecar-injector` (the object this
recipe's own fixtures actually depend on), not this second one.

## Fixture pack: `fixtures/istio/smoke/`

Two Pods, identical apart from which namespace each lands in
(`00-namespaces.yaml` creates both, one labelled, one not):

- `10-inject-pod.yaml`, in `admissionlab-istio-smoke-inject`
  (`istio-injection: enabled`): must come back from the API server
  carrying `istio-init`/`istio-proxy` in `spec.initContainers` (the
  latter with `restartPolicy: Always`) and the
  `sidecar.istio.io/status` annotation.
- `11-noinject-pod.yaml`, in `admissionlab-istio-smoke-noinject` (no
  injection label at all): the negative counterweight — must come back
  with **no** `spec.initContainers` and no `sidecar.istio.io/status`
  annotation. Without this fixture, a webhook that happened to inject
  every Pod regardless of namespace would make the first fixture's
  assertion pass for the wrong reason.

Both Pods use `registry.k8s.io/pause:3.10`, the same minimal image
Kyverno's own fixture pack uses, and neither depends on cluster state,
ordering, or a variable/context lookup — fully deterministic.

**Scope boundary (Controller supplement §8):** this fixture pack and its
test prove sidecar injection *happened*, correctly placed, in the right
namespace only. They do not compare baseline against candidate, do not
normalize the injected output, and do not decide whether any particular
difference is a regression — that is Phase 4's job
(`admissionlab-diff`/`admissionlab-policy`), not this recipe's.

## What this recipe does not pin

No container image digest is pinned anywhere in this recipe: the recipe
schema's Helm install method has no field for one, and the reproducible
unit this recipe pins is the Helm **chart** version (`1.30.4`), not the
`pilot`/`proxyv2` image tags/digests the chart's own `values.yaml`
selects (mirroring `recipes/kyverno/README.md`'s identical note).
