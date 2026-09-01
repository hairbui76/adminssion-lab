# Portable Gateway behavior contracts (ROADMAP Task 8.7)

One corpus of Gateway API fixtures, run against **both** certified
implementations — Istio 1.30.4 and NGINX Gateway Fabric 2.6.7, both
pinned to Gateway API v1.5.1 — by
`crates/admissionlab-gateway/tests/portable_contracts.rs`.

The point is not that the fixtures *look* portable. It is that the same
files are applied to a real cluster running each implementation and the
same behaviors are probed through both, so "portable" is a measurement
this directory keeps making rather than a claim someone made once.

## The seven contracts

| # | Contract | Where the evidence comes from |
| --- | --- | --- |
| 1 | Basic host/path routing | `200` from `echo-a` on a matched path; `404` on an unmatched one |
| 2 | `ReferenceGrant` cross-namespace backend | the backend identity `echo-b-remote`, which only the remote workload answers with |
| 3 | TLS termination | a real TLS handshake against the generated CA, with the contract's hostname as SNI, then `200` from `echo-a` |
| 4 | `RequestHeaderModifier` + `ResponseHeaderModifier` | the echo body's `headers` map (what the backend received) **and** the probe's own response headers |
| 5 | HTTP redirect | `301` plus a normalized `Location` (scheme/host/path) |
| 6 | URL rewrite | the echo body's `path` field — what the backend saw, which is the only place a rewrite is visible |
| 7 | Two-backend weighted routing | 1000 probes tallied by echoed backend, against the roadmap's statistical bound |

## The overlay convention

Four files, split by one rule: **a file is shared unless an
implementation forces it apart.**

| File | Shared? |
| --- | --- |
| `backends.yaml` | shared — namespaces, three echo backends, one `ReferenceGrant` |
| `routes.yaml` | shared — all seven `HTTPRoute`s |
| `gateway-istio.yaml` | Istio only |
| `gateway-nginx.yaml` | NGF only |

A run applies `backends.yaml`, exactly one `gateway-*.yaml`, the
generated TLS Secret, and `routes.yaml`. Order does not matter:
`admissionlab_gateway::apply::apply_gateway_manifests` sorts every
document from every file into one fixed category order before applying
any of them.

**There is no templating engine, and there will not be one.** Two thin,
complete, readable Gateway objects beat one templated object plus a
substitution language: a reader can `diff gateway-istio.yaml
gateway-nginx.yaml` and see the entire cost of an implementation change
in a few lines, and a reviewer can tell that the two files differ in
exactly the ways they claim to. That diff is:

1. `gatewayClassName: istio` vs `nginx`;
2. Istio's `ConfigMap` + `Gateway.spec.infrastructure.parametersRef`,
   which forces the provisioned data-plane `Service` to `ClusterIP`
   because Istio will otherwise never report `Programmed` on a `kind`
   cluster. NGF needs no equivalent — it programs a Gateway whose
   `LoadBalancer` address never arrives, because its `Programmed` is
   about the data plane it started.

Both halves of that were measured for Tasks 6.10 and 8.1 and are written
out in `fixtures/gateway/istio/same-namespace.yaml` and
`fixtures/gateway/nginx/same-namespace.yaml`.

## Resolving the data plane

The portable suite finds the data-plane `Service` by the standard label
`gateway.networking.k8s.io/gateway-name` and by **port number** (80 and
443), not by port name.

Port names are the one part of a provisioned `Service` that is genuinely
vendor-specific: Istio names the listener port `http` (and adds
`status-port` 15021), while NGF derives `port-<number>` from the
listener port. Each recipe's own `gatewayEndpoint` strategy encodes its
vendor's answer, and the per-implementation certification suites
exercise exactly that. A corpus whose subject is portability selects on
the thing the Gateway API itself fixes — the listener's port — instead.

## The TLS Secret is generated, never checked in

`gateway-*.yaml` names a Secret called `portable-tls`. It is **not** in
this directory and must never be.

Each run calls `admissionlab_gateway::tls::generate_test_certificate`
for `tls.portable.gateway.admissionlab.test`, which mints a fresh CA and
leaf valid for 24 hours, writes a `kubernetes.io/tls` Secret manifest
into the run workspace with mode `0600`, applies it, and deletes the
file. The private key is an `admissionlab_core::SensitiveBytes`
throughout: it renders as `[REDACTED]` in `Debug`, `Display` and
`Serialize`, and reaches plaintext only through the one greppable
`expose_key_pem` call at the manifest-writing site. It is never logged,
never printed by the suite, and never reaches a report.

The probe side trusts *only* that generated CA
(`test_certificate_client_config`) and verifies the certificate against
the contract's own hostname while dialling `127.0.0.1:<forwarded port>`
— the asymmetry `admissionlab_gateway::tls` documents as the seam and
`admissionlab_gateway::probe` implements as `ProbeTransport::Tls`.

## Where the contract model lives, and why not in the spec

Each contract here needs more than
`admissionlab_spec::HttpProbeContract` carries. That type has `host`,
`path`, `method`, `headers`, `expectedStatus` and `expectedBackend`; a
TLS contract also needs "over TLS", a redirect contract an expected
`Location`, a rewrite contract an expected backend-observed path, a
header contract expected request/response headers, and a weighted
contract two weights and a sample count.

**Those live in `tests/portable_contracts.rs`, as a repo-internal
model, and deliberately not in `admissionlab-spec`.** Three reasons:

1. `admissionlab.io/v1` is frozen. Optional additive fields would
   be permitted before v1.0 under the migration policy, so this is a
   judgement rather than a prohibition — but a frozen schema should
   grow for users, not for the project's own test tooling.
2. This corpus **is** that tooling. It is certification infrastructure
   that proves two vendored implementations behave alike; it is not a
   thing a user writes in their `admissionlab.yaml`. Nothing outside
   this repository consumes it.
3. The roadmap's Phase 8 gate asks that the corpus *run against both
   implementations*. It does not ask users to be able to express these
   contracts, and shipping six user-facing fields to satisfy an
   internal test would be paying a permanent API cost for a temporary
   convenience.

If a user ever does need them, the extension is additive and obvious:
optional `tls: bool`, `expectedLocation: {scheme, host, path}`,
`expectedObservedPath: string`, `expectedResponseHeaders: {..}`, and a
weighted variant carrying `{backend, weight}` pairs plus `samples` —
each `#[serde(default)]` on `HttpProbeContract`, each absent by default,
none changing the meaning of an existing document. The engine work is
already done and already public: `ProbeTransport`, `ProbeObservation`,
`normalize_location`, `probe_many` and
`weighted_routing_within_tolerance` are all in
`admissionlab_gateway::probe`, so such an extension would be wiring
rather than new behavior.

## Measured portability (Gateway API v1.5.1)

Support levels are from the v1.5.1 API source
(`apis/v1/httproute_types.go`, `apis/v1/gateway_types.go`). The
per-implementation columns are from each project's published
conformance report against Gateway API v1.5.1 — Istio 1.30.1
(`conformance/reports/v1.5/istio-istio/`) and NGF 2.6.0
(`conformance/reports/v1.5/nginx-nginx-gateway-fabric/`), the nearest
published reports to the pinned 1.30.4 / 2.6.7 — plus, for NGF, its own
per-field compatibility page.

| Feature | Level | Istio | NGF | In this corpus |
| --- | --- | --- | --- | --- |
| Listener TLS `Terminate` + `certificateRefs` | Core | supported | supported | yes (3) |
| `RequestHeaderModifier` | Core | supported | supported | yes (4) |
| `ResponseHeaderModifier` | Extended | supported | supported | yes (4) |
| `RequestRedirect` `hostname`/`statusCode` | Core | supported | supported | yes (5) |
| `URLRewrite` path `ReplacePrefixMatch` | Extended | supported | supported | yes (6) |
| `backendRefs[].weight` | Core | supported | supported | yes (7) |
| `rules[].timeouts` | Extended | supported | **not supported** | **no — deferred** |

### What the corpus actually observed

Every claim above that this corpus covers has now been measured rather
than only read. On Kubernetes 1.36.4, all seven contracts held on both
implementations — 14 of 14 — with each route reconciling to
`Accepted`/`ResolvedRefs`/`Programmed` in under 300 ms on the first
observation. The weighted contract, at `n = 1000` and `p = 0.8/0.2`,
has a tolerance of `0.0506` (the statistical term, which at these values
just exceeds the 0.05 floor):

| Implementation | `echo-a` | `echo-b` | observed | \|delta\| | margin |
| --- | --- | --- | --- | --- | --- |
| Istio 1.30.4 | 795/1000 | 205/1000 | 0.7950 / 0.2050 | 0.0050 | 0.0456 |
| NGF 2.6.7 | 788/1000 | 212/1000 | 0.7880 / 0.2120 | 0.0120 | 0.0386 |

Both are comfortably inside the bound, and neither is suspiciously
exact — an implementation that ignored `weight` and round-robined would
land near 0.5 and miss it by an order of magnitude.

### Timeout: deferred, with evidence (Step 6)

ROADMAP Task 8.7 Step 6 asks for a portable timeout contract *if it
proves stable on both certified implementations*, and prefers a
documented deferral to a flaky v1 test. It is not stable on both, and
the reason is not flakiness — NGF does not implement the field:

- NGF's own Gateway API compatibility page, for 2.6.7, lists
  `spec.rules.timeouts` as **"Not supported"**, and states outright:
  *"If `name`, `timeouts`, or `retry` are defined for a HTTPRoute rule,
  they will be ignored and rule still will be created."*
- NGF 2.6.0's conformance report lists both `HTTPRouteRequestTimeout`
  and `HTTPRouteBackendTimeout` under `unsupportedFeatures`.
- In NGF 2.6.7's source, `rules[].timeouts` is collected as an
  unsupported field and surfaces as `Accepted: True` with
  `reason: UnsupportedField` — an *accepted* route that ignores the
  timeout, which is the worst shape a test could be built on: the
  reconciliation assertion passes and the traffic assertion fails.
- Istio 1.30.4, by contrast, supports both features and documents
  `timeouts.request` on an `HTTPRoute`.

So a timeout contract in this corpus would be a test that asserts a
Gateway behavior one certified implementation is documented not to have.
That is a per-implementation contract, not a portable one, and Task 8.7
is explicitly about the portable set. **Timeout is deferred**, and this
paragraph is the deferral.

Two things follow for whoever revisits it:

- The machinery is ready. `admissionlab-echo` already implements both a
  per-pod delay (`ADMISSIONLAB_ECHO_DELAY_MS`) and a per-request one
  (`x-admissionlab-delay-ms`), with `/healthz` deliberately never
  delayed so a slow backend still becomes a Service endpoint. Nothing
  new needs building; what is missing is a second implementation that
  honors the field.
- The right home when NGF gains support is this directory. Until then,
  an Istio-only timeout fixture belongs in `fixtures/gateway/istio/`,
  labeled non-portable, exactly as
  `fixtures/gateway/nginx/nginx-infrastructure-override.yaml` is
  labeled NGF-only.

### Redirect `Location`: normalized, because the two disagree

The one place where two conforming implementations produce different
bytes for the same fixture. Predicted from each project's source at its
pinned tag, then **measured** on real clusters by this suite, which
prints the raw header on every run:

```text
[istio]                 raw Location: http://redirected.portable.gateway.admissionlab.test/redirect-probe
[nginx-gateway-fabric]  raw Location: http://redirected.portable.gateway.admissionlab.test:80/redirect-probe
```

Istio 1.30.4 strips the default port (`pilot/pkg/networking/core/route/
route.go`, `ApplyRedirect`); NGF 2.6.7 keeps it, because its
port-stripping branch is reached only when the filter sets a `scheme`
(`internal/controller/nginx/config/servers.go`).

Gateway API says an implementation SHOULD NOT include the port for
HTTP-on-80, but its own conformance helper accepts both, so neither is
wrong. On top of that, a probe reaches the data plane through a
`kubectl port-forward`, so the origin port a client observes is an
ephemeral local one and is not a property of the route at all.

`admissionlab_gateway::probe::normalize_location` therefore compares
**scheme, host and path**, and drops the port and the query. Forcing
byte-equality instead — by setting `scheme: http` on the filter — would
buy agreement by moving the contract onto an Extended field, which is a
bad trade for a corpus whose subject is portability.
