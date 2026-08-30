# Kyverno (certified recipe)

[Kyverno](https://kyverno.io/) is a Kubernetes-native policy engine: an
admission controller that validates, mutates, and generates resources
via `ClusterPolicy`/`Policy` custom resources, enforced through
`ValidatingWebhookConfiguration`/`MutatingWebhookConfiguration` objects
it manages itself. Licensed Apache-2.0. Source:
<https://github.com/kyverno/kyverno>. This is the first third-party
admission stack Admission Lab installs (Task 2.8) — everything before it
(`recipes/test-webhook`) was Admission Lab's own deterministic dogfood
component.

## What this recipe pins

- **Chart:** `kyverno/kyverno` version `3.9.0`, from the official chart
  repository `https://kyverno.github.io/kyverno/`.
- **appVersion:** `v1.19.0` (the controller image version the chart
  installs).

`3.9.0` was chosen over the immediately prior line, `3.8.2`
(appVersion `v1.18.2`), because it is a strict superset of `3.8.2`'s
changes (identical CRD set, identical default controller flags) plus a
real fix for CVE-2026-32280 (an upstream Go `crypto/x509` DoS
reachable through `imageVerify` rules — not used by this recipe's
fixtures, but fixed regardless). Full comparison, with every claim
traced to a command against the real chart or upstream source:
`.superpowers/sdd/ROADMAP/research-kyverno.md` §1.

**Maintenance note for whoever next bumps this pin:** chart `3.9.0` is
the last release line whose `NOTES.txt` does not yet warn about it, but
its own upstream announcement states plainly that **v1.19 is the final
release supporting the legacy `kyverno.io` policy API** — `ClusterPolicy`,
`Policy`, `ClusterCleanupPolicy` — that this recipe's fixture pack (and
Kyverno's docs generally, as of this writing) still use. The next chart
minor (`3.10.x`, appVersion `v1.20.x`) is expected to remove those CRDs
in favor of the newer CEL-based `policies.kyverno.io` API
(`ValidatingPolicy`, `MutatingPolicy`, and so on — a different set of
kinds under a same-named-but-different API group; see
`research-kyverno.md` §2.4). A routine version bump past `3.9.0` will
need `fixtures/kyverno/smoke/*.yaml` migrated to the new API, not just a
version-string edit here.

## Kubernetes certification: 1.35.8, not Admission Lab's Tier-1 primary

`compatibility/recipes.yaml`'s own `kyverno` entry — not this file — is
the source of truth for which Kubernetes version(s) this recipe is
certified against, and it is **deliberately narrower** than Admission
Lab's Tier-1 primary (`1.36.4`, per `compatibility/kubernetes.yaml`):

- Kyverno's current docs for this chart/appVersion line state plainly:
  **"Kubernetes Versions Supported: v1.33 - v1.35."**
- `Chart.yaml` itself declares `kubeVersion: '>=1.25.0-0'` — an open
  lower bound with **no upper bound**. That is Helm's own install-time
  gate (it will not stop `helm install` on 1.36 or 1.37), **not** a
  compatibility guarantee, and is not treated as one here (Global
  Constraint 15: an absent or unbounded constraint means "unknown,"
  never "supported").

So `compatibility/recipes.yaml` records `certified: ["1.35.8"]` for
`kyverno`, with the full `documentedRange`/reasoning inline in that
file. `crates/admissionlab-recipes/tests/kyverno_recipe.rs` — this
recipe's own certification test — reads that `certified` list at test
time to decide which Kubernetes version to provision its `kind` cluster
at, rather than hardcoding a copy of `"1.35.8"` here or there: an edit to
`certified` in `compatibility/recipes.yaml` changes what that test
installs against on its next run.

## Why readiness gates on `kyverno-admission-controller` only

The chart renders four Deployments in the `kyverno` namespace:
`kyverno-admission-controller`, `kyverno-background-controller`,
`kyverno-cleanup-controller`, `kyverno-reports-controller`. Only
**`kyverno-admission-controller`** owns any resource-facing webhook —
confirmed by extracting every rendered `ClusterRole`'s RBAC rules: the
other three Deployments' service accounts have no permission touching a
`*webhookconfigurations` resource at all (`research-kyverno.md` §2.1).
`recipe.yaml`'s `readiness:` therefore names only that one Deployment,
plus the two webhook-configuration objects it owns
(`kyverno-resource-validating-webhook-cfg`,
`kyverno-resource-mutating-webhook-cfg`).

**This recipe does not (and, as of this writing, cannot) disable the
other three Deployments.** `crates/admissionlab-recipes/src/model.rs`'s
`RawHelmInstall` has no `setValues`/`valuesFiles` field yet — deliberate
YAGNI, not an oversight; see that struct's own documentation — so this
recipe installs the chart entirely at default values. All four
Deployments run; readiness simply never waits on the three that are not
on the admission path.

## The webhook-configuration race, and how fixtures must handle it

Kyverno creates `kyverno-resource-validating-webhook-cfg` and
`kyverno-resource-mutating-webhook-cfg` itself, at **runtime**, almost
immediately after `kyverno-admission-controller` starts — gated by a
fast internal self-check, not by any `ClusterPolicy` existing. But both
objects start with an **empty `webhooks: []` list**; a per-policy rule is
appended only after that policy is created and the controller's
workqueue processes the resulting event
(`research-kyverno.md` §2.2/§5.3).

Consequences:

- `recipe.yaml`'s `webhookConfigurationPresent` readiness checks confirm
  the two objects **exist**. That is necessary but not sufficient for
  "my policy is enforced" — it can (and in practice does) go true before
  a policy applied immediately afterward has been folded into either
  object.
- The race-free signal is the `ClusterPolicy`'s own `status.conditions[]`
  `Ready` condition (also surfaced as the CRD's `READY`
  `additionalPrinterColumn`), populated once the policy's rules are
  actually live in the webhook configuration. **Any caller that applies
  a `ClusterPolicy` after this recipe installs must wait for that
  policy's own `Ready` condition — via a `customResourceCondition`
  readiness check against `kyverno.io/v1`/`ClusterPolicy` — before
  sending a resource it expects to be denied or mutated.**
  `crates/admissionlab-recipes/tests/kyverno_recipe.rs` does exactly
  this for every fixture policy in `fixtures/kyverno/smoke/`; a test (or
  a future caller) that skips this step and only waits on
  `webhookConfigurationPresent` will be flaky at best and silently green
  at worst.

## The rule-level `failureAction` field, exactly as named in the CRD

`spec.validationFailureAction` (spec-level) is **deprecated**, defaults
to `Audit`, and never blocks a request by itself. The CRD's own
description for it says "Deprecated, use validationFailureAction under
the validate rule instead" — but that message names the *old* field;
verified directly against the real `clusterpolicies.kyverno.io` v1 CRD
schema pulled from chart 3.9.0, the actual rule-level field is named
**`failureAction`** (`spec.rules[].validate.failureAction`, enum
`[Audit, Enforce]`, no default — see
`fixtures/kyverno/smoke/10-validate-policy.yaml` and
`research-kyverno.md` §4 for the exact commands used to confirm this).
Every validating fixture in this recipe's fixture pack sets
`failureAction: Enforce` at that exact path, and
`crates/admissionlab-recipes/tests/kyverno_recipe.rs` proves the denial
actually happens (applies a violating resource and asserts it is
rejected, attributably to the policy by name) rather than merely
asserting "applying a policy didn't error."

## Fixture pack: `fixtures/kyverno/smoke/`

Two independent scenarios, each in its own namespace so neither
`ClusterPolicy` ever sees the other scenario's objects:

- **Validating** (`admissionlab-kyverno-smoke-validate`):
  `10-validate-policy.yaml` denies any Pod missing the
  `admissionlab.io/team` label. `11-validate-allowed-pod.yaml` (carries
  the label) must be admitted; `12-validate-denied-pod.yaml` (does not)
  must be rejected, attributably to this policy.
- **Mutating** (`admissionlab-kyverno-smoke-mutate`):
  `20-mutate-policy.yaml` adds
  `app.kubernetes.io/managed-by: admissionlab` to every Pod.
  `21-mutate-input-pod.yaml` (carries no such label) must be admitted
  *and* the created object must actually carry the added label — not
  merely "the apply succeeded," which a no-op policy would also satisfy.

Both policies set `background: false` (admission-time evaluation only —
this fixture pack does not depend on the background-controller
Deployment, which this recipe does not gate readiness on) and are
fully deterministic: every outcome depends only on the incoming object's
own metadata, never on cluster state, ordering, or a variable/context
lookup.

## What this recipe does not pin

No container image digest is pinned anywhere in this recipe: the recipe
schema's Helm install method has no field for one, and the reproducible
unit this recipe pins is the Helm **chart** version (`3.9.0`), not the
controller image tag/digest the chart's own `values.yaml` selects.
