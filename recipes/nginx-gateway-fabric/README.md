# NGINX Gateway Fabric (certified recipe)

ROADMAP Task 8.1. The **second** Gateway API implementation Admission Lab
certifies, after `recipes/istio-gateway/` (Task 6.10), and the first
half of Phase 8's real question: can this project compare two
implementations of one API rather than compare two YAML files?

Everything here is *install/readiness/endpoint metadata*. Global
Constraint 6 and PRODUCT.md §14: a recipe may say how to install a stack,
how to know it is ready, how to find its data plane, and which
nondeterminism it stamps on responses. It may not say which behavioral
difference counts as a regression. That is `admissionlab-diff` /
`admissionlab-policy`'s job, and the recipe schema enforces the boundary
by construction — see "Global Constraint 6, enforced not asserted" below.

Certified against Kubernetes **1.35.8, 1.36.4 (primary) and 1.37.0**, by
`crates/admissionlab-recipes/tests/nginx_gateway_recipe.rs`, on real
`kind` clusters, with real HTTP traffic. See "What was measured".

---

## This directory is a two-component stack

| File | Recipe name | Installs | Version |
| ---- | ----------- | -------- | ------- |
| `gateway-api-crds.yaml` | `gateway-api-crds-nginx` | The vendored Gateway API CRD bundle, as raw manifests | Gateway API `1.5.1` |
| `recipe.yaml` | `nginx-gateway-fabric` | The NGF Helm chart, and the `gatewayApi` capability | NGF `2.6.7` |

Two recipe documents rather than one, for the reason
`recipes/istio-gateway/` already established: the recipe schema has
exactly one `install:` block per recipe and no shape for "install more
than one thing". `admissionlab_installer::install_stack` takes an ordered
component list and fully waits out each component's readiness before the
next one's install begins, so "install the CRDs, then NGF" is expressed
as ordering, and
`nginx_gateway_recipe.rs`'s `the_two_components_install_the_crds_before_the_implementation`
is what stops that order being accidentally reversed.

The order is required by NGF, not chosen for tidiness: its own
documentation states that "the Gateway API resources from the standard
channel must be installed before deploying NGINX Gateway Fabric".

NGF's **own** CRDs — `NginxProxy`, `NginxGateway`, `SnippetsFilter` and
eight more in the `gateway.nginx.org` group — are deliberately *not* a
third component. The Helm chart carries them in its `crds/` directory,
and Helm installs those directly. That is not merely convenient; see
"Why Helm, and why that forced a change to the installer".

---

## Upstream provenance: every pin, and where it came from

| What | Pin | Source, fetched 2026-09-01 |
| ---- | --- | -------------------------- |
| NGF release | `2.6.7` | `api.github.com/repos/nginx/nginx-gateway-fabric/releases/latest` → `tag_name: v2.6.7`, published 2026-07-15 |
| Helm chart | `oci://ghcr.io/nginx/charts/nginx-gateway-fabric` @ `2.6.7` | NGF's own Helm installation page; chart digest `sha256:5fd07b74794d4d21d3e5c14ec9cbf932384397a9f6c9af19cbcb49e0d6c7f06f` as reported by `helm pull` |
| Gateway API | `v1.5.1`, standard channel | `raw.githubusercontent.com/nginx/nginx-gateway-fabric/v2.6.7/go.mod` → `sigs.k8s.io/gateway-api v1.5.1`; corroborated by NGF's technical-specifications page ("Gateway API: 1.5.1") |
| Kubernetes support | "1.31+" | NGF technical-specifications page, 2.6.7 row |
| Control-plane image | `ghcr.io/nginx/nginx-gateway-fabric:2.6.7` | chart default, observed on a live cluster |
| Data-plane image | `ghcr.io/nginx/nginx-gateway-fabric/nginx:2.6.7` | chart's default `NginxProxy`, observed on a live cluster |

The Kubernetes floor is recorded as prose in
`compatibility/recipes.yaml` rather than as a `documentedRange`, because
`documentedRange` requires both a `min` and a `max` and "1.31+" publishes
no maximum — inventing one would be exactly the fabrication Global
Constraint 15 rules out. That entry's own comment says so in full.

---

## Why Helm, and why that forced a change to the installer

This recipe is the first in the project whose `install.chart` is an
`oci://` reference, and getting there took a change to
`admissionlab_installer::helm`. Both halves of that were forced by
measurement, not preference.

### NGF publishes its chart *only* to an OCI registry

Its installation documentation gives exactly one command shape,
`helm install ngf oci://ghcr.io/nginx/charts/nginx-gateway-fabric …`, and
no `helm repo add` at all. F5's classic Helm repository at
`https://helm.nginx.com/stable` does not carry the chart — its own
`index.yaml` lists `nginx-appprotect-dos-arbitrator`, `nginx-devportal`,
`nginx-ingress`, `nginx-service-mesh`, `nim`, `nms`, `nms-acm`,
`nms-adm` and `nms-hybrid`, and no `nginx-gateway-fabric`. There is no
HTTP repository to point `install.repo` at.

`admissionlab_installer::helm` ran `helm repo add <name> <url>`
unconditionally before every install, and that subcommand speaks the
classic HTTP `index.yaml` protocol. Measured against `helm` v3.20.0:

```text
$ helm repo add ngf oci://ghcr.io/nginx/charts/nginx-gateway-fabric --force-update
Error: looks like "oci://ghcr.io/nginx/charts/nginx-gateway-fabric" is not a valid
chart repository or cannot be reached: failed to perform "FetchReference" on source:
invalid reference
```

The same `helm`, with no repository registered at all, installed the
chart in 12.5 s. So the installer now skips step 1 for a chart reference
beginning with `oci://`, and sets `HELM_REGISTRY_CONFIG` alongside the
two `HELM_REPOSITORY_*` variables it already isolated — an OCI install
resolves through Helm's *registry* client, whose credential store is a
third file (`~/.config/helm/registry/config.json`) that this module could
previously claim never to touch only because it never took this path.

`install.repo` stays **required**, and this recipe sets it to
`oci://ghcr.io/nginx/charts`: the registry path the chart reference is
rooted at. Nothing runs `helm repo add` with it. Making the field
optional would have turned its emptiness into a sentinel encoding which
of two install paths a component takes; keeping it means `repo` always
answers "where does this chart come from".

### The alternative install path is closed, and not for a stylistic reason

NGF also publishes plain manifests at each release tag
(`deploy/crds.yaml`, 909,895 bytes, and `deploy/default/deploy.yaml`),
and vendoring those would have matched `gateway-api-crds.yaml`'s own
local-first shape exactly. It does not work.

`admissionlab_installer::manifests` applies with
`kubectl apply --server-side=false`, which stores the entire applied
object in a `kubectl.kubernetes.io/last-applied-configuration`
annotation. Kubernetes caps annotations at 262,144 bytes. Serialized
sizes of the largest object in each bundle this project touches:

| Object | Serialized | Against the 262,144-byte cap |
| ------ | ---------- | ---------------------------- |
| `httproutes.gateway.networking.k8s.io` (Gateway API v1.5.1) | 243,898 bytes | fits, 18,246 bytes of headroom |
| `nginxproxies.gateway.nginx.org` (NGF 2.6.7) | **320,611 bytes** | **58,467 bytes over** |

On a real Kubernetes 1.36.4 cluster, `kubectl apply --server-side=false
-f deploy/crds.yaml` created ten of NGF's eleven CRDs and then:

```text
The CustomResourceDefinition "nginxproxies.gateway.nginx.org" is invalid:
metadata.annotations: Too long: may not be more than 262144 bytes
```

Helm has no such problem: it records a release's manifest in its own
Secret rather than in a per-object annotation, and a chart's `crds/`
directory is installed with a plain create. So the install method here is
not a choice between two working options — it is the one that can install
this chart's CRDs at all.

(The Gateway API bundle's own margin is 7.0%, which is why *that*
component can still use manifests. `recipes/istio-gateway/README.md`
records the same measurement from the other side.)

---

## The Gateway API CRD bundle: vendored, and vendored *twice*

```text
url:    https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.5.1/standard-install.yaml
sha256: 751002b3b91a87f7ae3bd2517c79a47a8d7ed6702901808a1cf9bd97d284f9b8
size:   1024333 bytes
```

`gateway-api/standard-install-v1.5.1.yaml` here is byte-identical to
`recipes/istio-gateway/gateway-api/standard-install-v1.5.1.yaml`. That is
a second copy of a megabyte already in the repository, and it is
deliberate.

**Why a copy and not a reference.** `install.paths` resolves relative
paths against the recipe's *own* directory and confines them to it
(`admissionlab_recipes::model::resolve_manifest_path` → `join_confined`).
A path like `../istio-gateway/gateway-api/standard-install-v1.5.1.yaml`
is a validation error by design: PRODUCT.md §29.1 treats everything a
recipe causes to be installed as an untrusted test workload, and a recipe
that could reach outside its own directory is a recipe that could install
anything on the machine. An absolute path would resolve, but only on the
machine it was written on. So a recipe that installs a vendored artifact
must vendor it inside itself. Widening that confinement to save a
megabyte of disk would be trading a real security property for nothing.

**Why the duplication is safe.**
`nginx_gateway_recipe.rs`'s
`the_vendored_gateway_api_bundle_is_byte_identical_to_the_istio_gateway_copy`
hashes both files and fails if they differ, and each is separately
checked against the upstream digest above. Both run under plain
`cargo test --workspace`, in milliseconds. Duplication a machine checks
is a different thing from duplication a reviewer is asked to remember.

**Why v1.5.1, arrived at independently.** Not "the newest", and not
"whatever the other recipe had": NGF 2.6.7's own `go.mod` declares
`sigs.k8s.io/gateway-api v1.5.1`, so that is the API version its
controller was compiled and tested against. Istio 1.30.4 happens to build
against the same release, which is *why* the two bundles are identical
rather than merely similar. If a future NGF release moves to a different
Gateway API version, this recipe's bundle moves with it and the equality
test above starts failing — which is exactly the moment a human should be
looking at it, and exactly what that failure message says.

### Coexistence: two Gateway recipes, one cluster

CustomResourceDefinitions are cluster-scoped, so two components that both
install the Gateway API bundle would collide if composed into one stack.
Today they cannot be, and nothing tries: each recipe is installed into
its own lab's own ephemeral cluster (Global Constraint 4 — baseline and
candidate never share mutable cluster state), and
`nginx_gateway_recipe.rs` and `istio_gateway_recipe.rs` each create their
own clusters.

Two things make a future collision loud rather than silent:

- the recipe names differ (`gateway-api-crds-nginx` vs
  `gateway-api-crds`), so both can be loaded into one set without a
  duplicate-name error hiding one of them; and
- the bundles are byte-identical *and tested to be*, so applying both to
  one cluster today is idempotent rather than a conflict.

What is **not** claimed: that installing both implementations on one
cluster works. Two Gateway API controllers on one cluster is a supported
upstream configuration (each owns its own `GatewayClass`), but this
project has not certified it, and a later task that wants it should
compose a *single* CRD component ahead of both implementations rather
than letting two components race to own the same cluster-scoped objects.

---

## Readiness: four checks, four different questions

`recipe.yaml`'s four checks are not four ways of asking "is it up".

1. **`Deployment nginx-gateway/nginx-gateway-fabric` is `Available`.**
   The control plane's own process is running. The chart also runs a
   `cert-generator` Job that creates the `server-tls` Secret the
   Deployment mounts, so `Available` implicitly waits that out too.

   The *name* is worth a sentence, because it is an easy and silent way
   to get this wrong: `nginx-gateway` is the NAMESPACE, and it is also
   the Deployment name in NGF's plain-manifest install. Under Helm every
   object is named after the **release**, which this recipe leaves unset
   and which therefore defaults to the recipe's own name —
   `nginx-gateway-fabric`. Setting `install.releaseName` would rename the
   Deployment out from under this check, so `check_helm_install` asserts
   the two are equal rather than trusting the default to stay put.

2 & 3. **`gateways` and `httproutes` CRDs are `Established`.** Restated
   here even though `gateway-api-crds-nginx` already asserts them, so
   that this component's readiness describes its own preconditions.
   Composed without the CRD component, it now fails naming the missing
   CRD instead of installing "successfully" and failing much later when a
   fixture's `Gateway` is rejected as an unknown kind.

4. **`GatewayClass/nginx` is `Accepted=True`.** The strongest of the
   four, and the reason the first three are not the whole story: it
   proves NGF's controller is *running and reconciling Gateway API
   objects on this cluster*, which no CRD existence check and no
   Deployment condition can establish. The chart creates this class
   (controller `gateway.nginx.org/nginx-gateway-controller`); `nginx` is
   NGF's own default and is what the controller is started with
   (`--gatewayclass=nginx`).

   Observed on a live cluster five seconds after the Helm install
   returned, the class carried three conditions:

   ```text
   Accepted=True         "The GatewayClass is accepted"
   SupportedVersion=True "The Gateway API CRD versions are supported"
   ResolvedRefs=True     "The ParametersRef resource is resolved"
   ```

   Only `Accepted` is gated on. The other two are NGF-specific status
   this project has no certified meaning for; `Accepted` is the condition
   Gateway API itself defines.

---

## Endpoint resolution: by label, and with no port named

For `Gateway/lab-gateway` in namespace `admissionlab-nginx-gateway-same`,
NGF provisioned (read off a live cluster, not from documentation):

```text
Service/lab-gateway-nginx           (= <gateway name>-nginx)
Deployment/lab-gateway-nginx
  labels on both:
    gateway.networking.k8s.io/gateway-name: lab-gateway
    app.kubernetes.io/name:       lab-gateway-nginx
    app.kubernetes.io/instance:   nginx-gateway-fabric        <- the Helm release
    app.kubernetes.io/managed-by: nginx-gateway-fabric-nginx
  ports on the Service:
    - name: port-80  port: 80  targetPort: 80
```

The strategy selects on `gateway.networking.k8s.io/gateway-name` alone.
Not on the name, even though `<gateway>-nginx` is perfectly predictable:
that label is Gateway API's own documented gateway infrastructure label,
which NGF applies because upstream specifies it, while the `-nginx`
suffix is NGF's convention. And deliberately not on
`app.kubernetes.io/instance`, which carries the *Helm release* name —
selecting on that would make this strategy silently wrong for anyone who
installed the chart under a different release name.

That much is identical to `recipes/istio-gateway/recipe.yaml`'s. The
difference is the port:

| | Istio 1.30.4 | NGF 2.6.7 |
| --- | --- | --- |
| Ports on the provisioned Service | the listener's, **plus** `status-port` 15021 | exactly the listener ports |
| Port naming | vendor-chosen (`http`, `status-port`) | derived (`port-<number>`) |
| What the recipe must say | `portName: http` | nothing |

Istio's Service has two candidates, so the recipe must name one, and
Istio gives it a stable vendor-owned *name* to name. NGF's has exactly
one (its readiness port is added only when an `NginxProxy` sets
`nginxReadinessProbe.expose`, which nothing here does), and its port
names are derived from the port numbers — so there is no vendor-stable
name to select on, and pinning a *number* would push the fixture's
listener-port choice into the recipe, making a fixture that binds 8080
unresolvable for no reason.

With both fields unset, `admissionlab_gateway::endpoint`'s documented
rule resolves a single-port Service unambiguously, and a two-listener
Gateway becomes a loud "the port is unresolvable, here are the ports"
error — which is the honest answer, because at that point the recipe
genuinely does not know which port a probe means.

---

## No normalization rules, and the evidence for that

`recipe.yaml` carries no `normalizeRules:`. That is a finding, not an
omission — the same question `recipes/istio-gateway/` asked of a
different vendor, re-asked here rather than assumed to carry over.

Every object this recipe's fixtures create was read back from a live
cluster after NGF had fully reconciled them. The `Gateway` and the
`HTTPRoute` came back with `metadata.labels`, `metadata.annotations` and
`metadata.finalizers` all **absent**: NGF stamps nothing onto the objects
a fixture declares. The only additions to any fixture object were Gateway
API's own CRD schema defaults (`parentRefs[].group`/`kind`,
`backendRefs[].group`/`kind`/`weight: 1`) — written by the API server
from the CRD's schema, identical on every cluster serving the same bundle
version, and therefore not nondeterminism to normalize away.

NGF's genuinely nondeterministic output lives on objects it *creates*
(the per-Gateway Deployment and Service, their pod names and resource
versions), which no fixture declares and no comparison reads.

One difference is worth naming precisely because it is *not* normalized.
NGF adds request headers on the way to a backend that Istio does not, and
the echo backend reflects them:

```json
"headers": { "host": "...", "user-agent": "...",
             "x-forwarded-for": "127.0.0.1", "x-forwarded-host": "...",
             "x-forwarded-port": "80", "x-forwarded-proto": "http",
             "x-real-ip": "127.0.0.1" }
```

Those are deterministic, and they are a real behavioral difference
between two implementations rather than noise. Deleting them with a
recipe-level rule would delete exactly the evidence an Ingress-to-Gateway
or Istio-to-NGF comparison exists to surface. PRODUCT.md §14: a
normalization rule deletes evidence, so an unnecessary one silently
narrows what a comparison can ever detect.

---

## Fixtures: `fixtures/gateway/nginx/`

Three files, and the split between them *is* Task 8.1 Steps 3 and 4.

| File | Portable? | Namespaces | Backend | Data-plane Service |
| ---- | --------- | ---------- | ------- | ------------------ |
| `same-namespace.yaml` | yes | `…-same` | `echo-a` | `LoadBalancer` (NGF default) |
| `cross-namespace.yaml` | yes | `…-cross-route` + `…-cross-backend`, `ReferenceGrant` | `echo-b` | `LoadBalancer` (NGF default) |
| `nginx-infrastructure-override.yaml` | **no**, and labeled so | `…-override` | `echo-a` | `ClusterIP`, via an `NginxProxy` |

"Portable" is not a comment anyone has to trust:
`only_the_labeled_nginx_specific_fixture_carries_a_vendor_object` parses
all three and fails if either portable fixture contains an object from
any API group outside core/`apps`/`gateway.networking.k8s.io`, or any
`infrastructure.parametersRef` at all. Without that test, the natural
next change — adding one small NGF-only knob to `same-namespace.yaml`
because it is convenient — would quietly turn the portable pack into a
second vendor pack.

### How portable it actually turned out to be

Diff `same-namespace.yaml` against
`fixtures/gateway/istio/same-namespace.yaml`, ignoring comments and the
namespaces' own names, and what is left is:

1. `gatewayClassName: istio` → `nginx`;
2. the Istio `ConfigMap` and the `Gateway.spec.infrastructure
   .parametersRef` pointing at it are **gone**.

Nothing else. Same listener, same route, same path match, same backend,
same probe. (2) is the next section.

### THE FINDING: NGF programs a Gateway on `kind` with no override at all

`recipes/istio-gateway/README.md` records that on a bare `kind` cluster
Istio's data-plane `Service` (`type: LoadBalancer` by default) never gets
an address, and Istio therefore reports — correctly, and permanently —
`Programmed=False (AddressNotAssigned)`. Every Istio Gateway fixture
carries a `ConfigMap` forcing `ClusterIP` purely to get out of that.

NGF provisions its data-plane Service as `LoadBalancer` too, and on
`kind` its external address stays `<pending>` exactly the same way.
Measured on a real Kubernetes 1.36.4 cluster with the portable fixture
applied as written:

```text
service/lab-gateway-nginx   LoadBalancer   10.96.74.62   <pending>   80:30793/TCP

Gateway/lab-gateway
  Accepted=True    (Accepted:   "The Gateway is accepted")
  Programmed=True  (Programmed: "The Gateway is programmed")

Gateway.status.addresses: (empty)
```

NGF's `Programmed` is a statement about the data plane it provisioned and
started, not about an external address having been assigned to it. So the
environment problem that forced a vendor object into the Istio fixtures
simply does not arise, and adding an NGF equivalent "for symmetry" would
be adding vendor configuration the fixture has no need of.

The port-forward the probe runs through is unaffected either way: it
targets the Service's own port 80 on its ClusterIP, which a `LoadBalancer`
Service has just as much as a `ClusterIP` one does.

This is exactly the kind of difference Phase 8 exists to find: two
conformant implementations, the same `Gateway`, and a `Programmed`
condition that means measurably different things.

### The NGF-specific fixture, and what it proves

`nginx-infrastructure-override.yaml` attaches an `NginxProxy`
(`gateway.nginx.org/v1alpha2`) through the `Gateway`'s standard
`spec.infrastructure.parametersRef`. The *reference* is Gateway API's own
extension point; the group, the kind and the entire body of the thing
referenced are NGF's.

It earns its own cluster scenario by proving two things the portable pack
cannot:

**1. NGF's per-Gateway provisioning is real and takes effect.** The same
`Gateway`, with and without the reference, produced a `ClusterIP` and a
`LoadBalancer` Service respectively. The certification test reads
`spec.type` back off the cluster and asserts it per fixture — because
both types serve traffic identically, so a test that only probed traffic
could not tell them apart.

Only `spec.kubernetes.service.type` is set. NGF's CRD documents that a
Gateway-level `NginxProxy` is *merged* with the GatewayClass-level one
("Settings specified on the Gateway NginxProxy override those set on the
GatewayClass NginxProxy"), so the data-plane image, tag, pull policy and
replica count still come from the chart's own
`nginx-gateway-fabric-proxy-config`. Writing only the field the fixture is
about is therefore complete, and it means the fixture holds no copy of
the NGF image reference to rot at the next chart bump.

A related trap that did *not* fire: the chart's class-level `NginxProxy`
sets `service.externalTrafficPolicy: Local`, which Kubernetes rejects on
a `ClusterIP` Service. Verified on a live cluster — NGF omits the field
when it provisions a `ClusterIP` Service, and the resulting Service has no
`externalTrafficPolicy` at all.

**2. A parameters reference survives being applied after its Gateway.**
`admissionlab_gateway::apply` sorts documents into a fixed category order
and applies every *unknown* kind last — and `NginxProxy` is, correctly,
unknown to it. So this fixture always creates the `Gateway` before the
`NginxProxy` it points at, which is the worst available ordering and
therefore the one worth measuring:

```text
# immediately after the Gateway, NginxProxy absent:
Accepted=True      (InvalidParameters: "The Gateway is accepted, but ParametersRef
                    is ignored due to an error: Spec.infrastructure.parametersRef.name:
                    Not found: \"gateway-infrastructure\"")
Programmed=True    (Programmed)
ResolvedRefs=False (ParametersRefInvalid)
service/lab-gateway-nginx   LoadBalancer   <pending>

# ~1s after the NginxProxy is created:
Accepted=True      (Accepted)
Programmed=True    (Programmed)
ResolvedRefs=True  (ResolvedRefs)
service/lab-gateway-nginx   ClusterIP
```

NGF watches `NginxProxy` objects and re-reconciles, converting the
already-provisioned Service from `LoadBalancer` to `ClusterIP` in place.
The test's `observe_until_reconciled` re-runs the whole convergence rule
until every certified condition is `True`, so the transient state is
observed and ridden out rather than latched onto. Note which condition
moves: `ResolvedRefs` on the **Gateway**, an NGF-reported statement about
the parameters reference. The route's own `Accepted`/`ResolvedRefs` are
`True` throughout, because nothing about the backend reference changed.

### The echo backends are copies, and the copies are checked

Each fixture inlines `fixtures/gateway/backends/echo-{a,b}.yaml`'s
`Service` and `Deployment` with a `metadata.namespace` added and nothing
else changed, because `apply_gateway_manifests` applies each document to
the namespace the document itself names and a namespaced object naming
none would land in `default`.
`fixture_backends_match_the_shared_echo_backend_definition` parses the
shared definition and every copy and fails on any other difference. See
`fixtures/gateway/istio/same-namespace.yaml`'s header for the full
argument; this pack inherits it unchanged.

---

## What was measured

Reference machine, warm `kind` node-image and Docker layer cache, real
clusters, one cluster per certified Kubernetes version.

| Step | Measurement |
| ---- | ----------- |
| `kubectl apply` of the whole 1 MB Gateway API CRD bundle (client-side) | 0.49 s |
| `helm upgrade --install` of the OCI chart → returns | 12.5 s (includes the OCI pull and the `cert-generator` Job) |
| `GatewayClass/nginx` → `Accepted=True` | ~5 s after the install returned |
| `Gateway` → `Programmed=True`, default `LoadBalancer` on `kind` | reached; address never assigned, and NGF does not wait for one |
| `Gateway` → `ResolvedRefs=True` after a late `NginxProxy` | ~1 s after the `NginxProxy` was created |
| Data-plane `Service` type, with / without the `NginxProxy` | `ClusterIP` / `LoadBalancer` |
| HTTP probe through the port-forward | 200, correct backend, first attempt, all three fixtures |
| `kubectl apply --server-side=false` of NGF's own CRD bundle | **fails** — `nginxproxies` exceeds the 262,144-byte annotation cap |

### The certification run

`cargo test -p admissionlab-recipes --test nginx_gateway_recipe --
--ignored --nocapture`, all three certified Kubernetes versions, one
disposable cluster each: **303.34 s** total, `1 passed; 0 failed`, and
`kind get clusters` empty afterwards.

Nine fixture runs (three fixtures × three Kubernetes versions), and every
one of them converged on the **first** observation with `converged=true`,
resolved its endpoint to `<namespace>/lab-gateway-nginx:80`, found the
data-plane `Service` in the expected type on the first read, and got a
200 from the right backend on the first attempt:

| Kubernetes | Fixture | Reconciliation | Service type | Probe |
| ---------- | ------- | -------------- | ------------ | ----- |
| 1.35.8 | `same-namespace` | 268.4 ms, 1 obs. | `LoadBalancer` | 200, `echo-a`, 3.2 ms |
| 1.35.8 | `cross-namespace` | 268.9 ms, 1 obs. | `LoadBalancer` | 200, `echo-b`, 3.5 ms |
| 1.35.8 | `nginx-infrastructure-override` | 267.0 ms, 1 obs. | `ClusterIP` | 200, `echo-a`, 3.1 ms |
| 1.36.4 | `same-namespace` | 267.9 ms, 1 obs. | `LoadBalancer` | 200, `echo-a`, 3.3 ms |
| 1.36.4 | `cross-namespace` | 267.1 ms, 1 obs. | `LoadBalancer` | 200, `echo-b`, 3.0 ms |
| 1.36.4 | `nginx-infrastructure-override` | 265.1 ms, 1 obs. | `ClusterIP` | 200, `echo-a`, 3.5 ms |
| 1.37.0 | `same-namespace` | 268.8 ms, 1 obs. | `LoadBalancer` | 200, `echo-a`, 2.8 ms |
| 1.37.0 | `cross-namespace` | 267.7 ms, 1 obs. | `LoadBalancer` | 200, `echo-b`, 3.3 ms |
| 1.37.0 | `nginx-infrastructure-override` | 267.7 ms, 1 obs. | `ClusterIP` | 200, `echo-a`, 3.0 ms |

NGF's reconciliation is strikingly uniform: 265–269 ms on every fixture
on every Kubernetes minor, with the cross-namespace `ReferenceGrant`
resolution costing nothing measurable over the same-namespace case.

### THE THIRD FINDING: the Service type needed polling, and the first version of this test proved it

The `Service`-type assertion above was written as a single read, and it
passed on 1.35.8 and 1.36.4 before failing on 1.37.0 in the very first
full certification run:

```text
[kubernetes 1.37.0]
[fixture nginx-infrastructure-override]
expected NGINX Gateway Fabric to provision Service
admissionlab-nginx-gateway-override/lab-gateway-nginx with type "ClusterIP",
got "LoadBalancer"
```

That is not a flaw in the fixture or in NGF — it is the same
stability-is-not-finality lesson `recipes/istio-gateway/README.md`
records for `Programmed`, arriving through a different field. Every
condition this recipe certifies was already `True` and current at that
moment: `Accepted` stays `True` in NGF's `InvalidParameters` state (only
the *reason* changes), and the Gateway's `ResolvedRefs` — the condition
that actually moves when the `NginxProxy` lands — is deliberately not
certified here, being NGF-specific status. So the route was legitimately
"reconciled" while the Service NGF had provisioned for it was still the
default.

`check_data_plane_service_type` now polls to the same 120-second bound
`observe_until_reconciled` uses, and reports how many reads it took. In
the passing run above it took exactly one read on all nine, which is the
point: the common case is unchanged, and the race that fires once in nine
is gone rather than left to reappear on a slower runner.

---

## Kubernetes certification

`compatibility/recipes.yaml`'s `nginx-gateway-fabric` entry certifies
1.35.8, 1.36.4 and 1.37.0 — the full Admission Lab supported set. 1.36.4
is `perCommit` (Admission Lab's Tier-1 primary); the other two are
`nightly` (Tier 2).

Those two were `weeklyRelease` when Task 8.1 wrote this recipe, mirroring
`istio-gateway`. **ROADMAP Task 8.9 moved them to Tier 2, and only this
recipe** — `istio-gateway`'s two non-primary minors stay at Tier 3.
Task 8.9 step 1 asks in as many words for NGINX Gateway Fabric in
"Tier 2/Tier 3", and a Tier-2 claim nothing schedules daily is a claim
rather than evidence; the Phase 8 exit gate rests on this
implementation specifically (the portable HTTPRoute corpus must run
against Istio *and* NGF); and this is the cheaper of the two stacks to
install — 12.5 s for the Helm release against istiod's 22.5 s, with no
per-Gateway `ConfigMap` override needed to reach `Programmed=True` on
`kind` at all. Nothing about the certification itself changed: a tier is
a statement about schedule, never about confidence.

`.github/workflows/integration.yml` runs the test as its own matrix
entry; `scripts/recipe-matrix.py` turns the rows above into the tiered
job matrix. Tier 2 additionally runs `recipe-matrix.yml`'s
`portable-contracts` job, which drives
`fixtures/gateway/portable/` through this recipe *and* `istio-gateway`
in one run.

---

## Global Constraint 6, enforced not asserted

`a_severity_field_cannot_be_added_to_this_recipe` appends
`severity: critical` to a copy of `recipe.yaml` and asserts the load
fails naming the field. The mechanism is the recipe schema's
`deny_unknown_fields` allow-list, not a keyword blocklist — see
`crates/admissionlab-recipes/src/model.rs`'s module documentation for why
an allow-list is the only version of that rule with no gap.

---

## What this recipe does not do

- **It does not compare anything.** It installs, reports readiness, and
  says where the data plane is. Which differences matter is
  `admissionlab-diff`/`admissionlab-policy`'s decision.
- **It does not claim NGF's admission behavior.** NGF 2.6.7 registers no
  admission webhook of its own (Gateway API validation lives in CEL rules
  inside the CRDs), and in any case this recipe's fixtures and test
  exercise Gateway API behavior only, so it declares `gatewayApi` and not
  `admission`.
- **It does not certify NGINX Plus.** The chart's `nginx.plus=true` path
  needs a private registry credential; everything here is the
  open-source data plane.
- **It does not certify running NGF and Istio on one cluster.** See
  "Coexistence" above for what is and is not claimed.
- **It does not exercise NGF's own policy CRDs** (`ClientSettingsPolicy`,
  `SnippetsFilter`, `WAFPolicy` and the rest). The chart installs them;
  no fixture creates one, and no readiness check gates on one, because
  nothing here has certified their behavior.
