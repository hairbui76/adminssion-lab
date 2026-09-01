# Compatibility: certified, supported, and merely configurable

Three different words, and Admission Lab means three different things by them.
Getting them mixed up is how a tool ends up implying it has tested something it
has not.

| Word | What it claims |
| --- | --- |
| **Provisionable** | Admission Lab can create a `kind` cluster at this Kubernetes version. Pinned, with a digest, in [`compatibility/kubernetes.yaml`](../compatibility/kubernetes.yaml). |
| **Certified** | This *recipe at this version* has been installed on this *exact Kubernetes patch version*, in a disposable cluster, by a test in this repository that CI runs on a schedule. Listed in [`compatibility/recipes.yaml`](../compatibility/recipes.yaml). |
| **Configurable** | Everything else. Any chart, any manifest, any private webhook, on any provisionable Kubernetes version. Admission Lab runs it, and its comparison is exactly as real — but nothing here has proven that combination. |

**The third row is the normal case, and it is fully supported.** A lab that
compares your own webhook against your own next release is what this tool is
for. Certification is not a permission system; it is a record of what this
repository has actually run.

---

## Contents

- [The certified table](#the-certified-table)
- [What a certification asserts](#what-a-certification-asserts)
- [Tiers: how often, never how confident](#tiers-how-often-never-how-confident)
- [Kubernetes versions Admission Lab provisions](#kubernetes-versions-admission-lab-provisions)
- [What happens on an uncertified combination](#what-happens-on-an-uncertified-combination)
- [The three-minors rule, and its one exception](#the-three-minors-rule-and-its-one-exception)
- [How the matrix changes](#how-the-matrix-changes)

---

## The certified table

Transcribed from [`compatibility/recipes.yaml`](../compatibility/recipes.yaml),
which is the authority. Seven rows.

| Recipe | Recipe version | Kubernetes | Tier | Proven by |
| --- | --- | --- | --- | --- |
| `kyverno` | `3.9.0` | `1.35.8` | `perCommit` | `crates/admissionlab-recipes/tests/kyverno_recipe.rs` |
| `istio` | `1.30.4` | `1.35.8` | `nightly` | `crates/admissionlab-recipes/tests/istio_recipe.rs` |
| `istio` | `1.30.4` | `1.36.4` | `perCommit` | `crates/admissionlab-recipes/tests/istio_recipe.rs` |
| `istio` | `1.30.4` | `1.37.0` | `nightly` | `crates/admissionlab-recipes/tests/istio_recipe.rs` |
| `istio-gateway` | `1.30.4` | `1.35.8` | `weeklyRelease` | `crates/admissionlab-recipes/tests/istio_gateway_recipe.rs` |
| `istio-gateway` | `1.30.4` | `1.36.4` | `perCommit` | `crates/admissionlab-recipes/tests/istio_gateway_recipe.rs` |
| `istio-gateway` | `1.30.4` | `1.37.0` | `weeklyRelease` | `crates/admissionlab-recipes/tests/istio_gateway_recipe.rs` |

**That is the complete list.** Nothing else in this repository is certified —
not `test-webhook` (Admission Lab's own dogfood webhook, which is a test
instrument rather than a stack anyone compares), not `gateway-api-crds` (it
installs CustomResourceDefinitions and has no behavior of its own to certify
against a Kubernetes version independently of the implementation serving them),
and no other chart, version or vendor at all.

Two entries deserve calling out, because both are places where the honest
answer is narrower than the convenient one:

- **`kyverno` is certified on `1.35.8` and nowhere else** — deliberately *not*
  on `1.36.4`, which is Admission Lab's own primary Kubernetes version. Kyverno's
  documentation for this chart line (chart `3.9.0` / appVersion `v1.19.0`) states
  support for Kubernetes v1.33–v1.35, and 1.36 is outside it. Certifying it on
  1.36 would mean this project claiming a window the vendor does not.
- **`istio` and `istio-gateway` carry `documentedRange: null`** — neither
  upstream states a Kubernetes support range for what these recipes install.
  That is recorded as *unknown*, never as *supported*. The certified set for
  those two is the full provisionable set because every row in it was actually
  installed and verified, not because an absent constraint was read as
  permission.

The list is machine-checked against itself: `certified_combinations()` must
equal exactly the seven rows above
(`crates/admissionlab-recipes/tests/compatibility.rs`,
`the_certified_combinations_are_exactly_the_reviewed_ones`), every certified
Kubernetes version must be one `compatibility/kubernetes.yaml` marks
`supported: true`, and every row must name a recipe this repository really pins
at that version. A row that certifies install metadata which does not exist is a
test failure, not a footnote.

## What a certification asserts

For an **admission** recipe (`kyverno`, `istio`): a disposable `kind` cluster at
that exact Kubernetes patch version was created, the recipe's pinned chart was
installed through its own recipe metadata, its readiness checks were waited out,
and the component was then observed *doing its job* — Kyverno enforcing this
repository's fixture policies, Istio actually injecting a sidecar. Not "the
chart installed".

For the **Gateway** recipe (`istio-gateway`): the same, plus the Gateway API CRD
bundle, plus a `Gateway` and an `HTTPRoute` reconciled to
`Accepted`/`ResolvedRefs`/`Programmed`, plus a real HTTP request answered with
`200` **by the expected backend** — in both the same-namespace and the
cross-namespace/`ReferenceGrant` configuration.

What a certification does **not** assert: that the combination is bug-free, that
the vendor supports it, or that your own values, policies and fixtures behave the
same way on it. It asserts that this repository ran it and it worked.

## Tiers: how often, never how confident

Every certified row carries the tier that runs it. A tier is a statement about
**schedule** and nothing else — a `nightly` row is exactly as certified as a
`perCommit` one, because both were proven the same way by the same test.

| Tier | Wire spelling | Runs on | Rows |
| --- | --- | --- | ---: |
| Tier 1 | `perCommit` | Every push to `main`, and pull requests touching the code or metadata it covers | 3 |
| Tier 2 | `nightly` | The nightly workflow | 5 |
| Tier 3 | `weeklyRelease` | Weekly cron, manual dispatch, and every release candidate | 7 |

**Tiers are cumulative downward**: each tier runs its own rows *and* every more
frequent tier's, so the counts grow 3 → 5 → 7 and each row appears exactly once,
at its cheapest schedule. Tier 2 is deliberately arranged to cover every
supported Kubernetes minor at least once
(`tier_2_covers_every_supported_kubernetes_minor`), and Tier 1 is capped at one
Kubernetes version per recipe (`tier_1_certifies_at_most_one_kubernetes_version_per_recipe`) —
a per-commit job installing a full Istio on three clusters costs more than the
signal it adds.

There is no hardcoded list of jobs anywhere in CI.
[`scripts/recipe-matrix.py`](../scripts/recipe-matrix.py) joins the rows above to
`compatibility/kubernetes.yaml`'s digest-pinned node images and prints the job
matrix as JSON, which `.github/workflows/recipe-matrix.yml` turns into its job
list with `fromJSON`. A combination nobody scheduled cannot exist, and a tier
that would select zero rows is a hard error rather than a green run that
certified nothing.

Each certification test derives the Kubernetes versions it runs from the same
file, and CI narrows a single job to one version through the
`ADMISSIONLAB_CERTIFY_KUBERNETES` environment variable. That variable is read by
the **certification tests only** — never by `admissionlab test`, and never by any
production code path. Setting it to a version the recipe does not certify fails
the test rather than silently certifying nothing.

## Kubernetes versions Admission Lab provisions

From [`compatibility/kubernetes.yaml`](../compatibility/kubernetes.yaml).
Every entry pins an exact patch version *and* a `kindest/node` digest, taken
from the `kind` v0.33.0 release that publishes them. Nothing floats.

| Minor | Version | `supported` | Notes |
| --- | --- | --- | --- |
| `1.37` | `1.37.0` | yes | Exercised by the nightly tier. |
| `1.36` | `1.36.4` | yes | Tier 1's primary version. |
| `1.35` | `1.35.8` | yes | The only version Kyverno is certified on. |
| `1.34` | `1.34.11` | **no** | Retired, and kept in the file rather than deleted. |

A retired minor stays checked in on purpose: it lets a request for `1.34` be
refused with *"no longer supported by Admission Lab"* rather than *"never heard
of it"*, which are different problems with different fixes. Version resolution is
an exact string match on the version, then on the minor — never a semver range,
and never "the closest patch we have".

"Primary" is prose, not a field: nothing in the file marks it, and no code reads
it. It names which version Tier 1 spends its per-commit budget on.

## What happens on an uncertified combination

**A warning. Never a refusal.** Global Constraint 6 makes the core
vendor-neutral and a generic, user-defined stack a first-class input, so nothing
in this workspace declines to run a lab over a certification question.

Before anything is provisioned, `admissionlab test` compares each side's
components against the certified table and writes one line per uncertified
combination to stderr — and the same text into `result.json`'s `diagnostics`,
under the code `compatibility.uncertified_combination`, so an archived report
still says which combination it was:

```text
admissionlab: warning: baseline, candidate requests kyverno 3.9.0 on Kubernetes
1.36.4, which Admission Lab does not certify. The run continues and its
comparison is as real as any other — user-defined stacks are supported — but
this combination is not covered by Admission Lab's own certification tests.
Certified: kyverno 3.9.0 on Kubernetes 1.35.8.
```

Three cases, and only the middle one warns:

1. **The component name is not in the matrix at all** — `my-webhook`,
   `some-fork`, your own chart. **Silent.** This is the generic user-defined
   stack, and warning about every component Admission Lab ships no recipe for
   would make the warning worthless within a week: the signal would be "you are
   using this tool as designed".
2. **The name is one Admission Lab certifies, but not at this version or on this
   Kubernetes version.** **Warns.** This is the actionable case, and the one a
   user is most likely to have reached by accident — running the certified
   Kyverno chart on Admission Lab's primary Kubernetes version, say.
3. **The combination is certified.** Silent.

The check is additionally skipped when the requested Kubernetes version is not
`supported: true` at all: that is refused outright when the cluster is created,
with a message about *that*, and a second quieter warning about certification
would only compete with it.

Baseline and candidate usually name the same component at the same version, so
combinations are deduplicated — one warning, with every side that asked for it
named in it.

## The three-minors rule, and its one exception

Admission Lab supports **the latest three upstream-supported Kubernetes minor
versions** at release time (Global Constraint 10; PRODUCT.md §32). The number is
a Rust constant (`RELEASE_SUPPORTED_MINORS`), not a YAML field or a flag —
precisely so that "we support three minors" cannot be edited away in passing
while somebody is changing something else. Validation rejects a
`compatibility/kubernetes.yaml` with any other number of `supported: true`
minors.

Note that upstream's own window is frequently *wider* than three. "Still
supported upstream" and "in Admission Lab's matrix" are therefore not the same
set, and the newest three win.

The roadmap allows one exception — *"unless the upstream support window
temporarily differs and release notes explain it"* — and it is expressed as a
reviewable diff to a checked-in file that names its own justification:

```yaml
supportWindowException:
  expectedSupportedMinors: 2
  reason: "<what upstream did, in one sentence>"
  releaseNotes: "<path or URL to the release notes that explain it>"
```

All three fields are required whenever the key is present; `reason` and
`releaseNotes` must be non-blank; and `expectedSupportedMinors` must actually
differ from three — an exception that restates the ordinary rule grants nothing,
and validation calls it stale and tells you to delete it. There is no flag, no
environment variable, and no runtime derivation from whatever upstream published
that morning.

**No exception is declared today.** Upstream supports 1.35, 1.36 and 1.37, which
is exactly three, and a test fails if an exception is left standing while the
count is ordinary.

## How the matrix changes

[`scripts/update-kubernetes-matrix.sh`](../scripts/update-kubernetes-matrix.sh)
**proposes** an update. It never applies one.

It is the only part of this project that talks to the network on purpose, and it
is not part of any lab run. It fetches three things: which Kubernetes minors are
still alive from endoflife.date (a cycle counts only if its EOL date is strictly
in the future — an unknown EOL never reads as supported), the `kindest/node`
images and digests that the pinned `kind` release actually published, and
`dl.k8s.io/release/stable.txt` for context, which it deliberately does not use to
select anything. It then applies the three-newest rule, prints a unified diff
against the checked-in `releases:` block, writes the proposal to a temporary file,
and exits `10` — a difference being its normal, successful outcome rather than an
error.

Three properties are worth knowing:

- **It refuses to write over `compatibility/kubernetes.yaml`.** Passing
  `--output` pointing at that file — including by way of `../` — fails with
  *"this script proposes a change for review, it never applies one"*. Applying a
  proposal is a human editing the file, keeping the comments that explain why
  each entry is what it is.
- **It never deletes a minor.** A minor that falls out of the newest three is
  proposed as `supported: false`, retaining its checked-in digest, for the same
  reason 1.34.11 is still in the file.
- **It flags the dangerous direction loudly.** A minor that would become
  `supported: false` gets a reviewer note saying so in as many words: *this
  DROPS a supported Kubernetes version and needs human review*. If fewer than
  three minors are available at all, it tells the reader to declare a
  `supportWindowException` rather than quietly proposing a two-minor matrix.

Adding a **certified recipe row** is a separate, entirely manual change: edit
`compatibility/recipes.yaml`, with the tier that will run it, and make sure a
certification test actually covers it. `scripts/recipe-matrix.py` refuses to
build a matrix for a recipe it has no registered test for — *"no certification
test is registered for recipe X. Add it to TESTS in this script, or the row would
be certified by nothing"* — and the Rust validation refuses a row that names
install metadata nothing pins. A certification nobody schedules is a claim rather
than evidence, and both halves of the pipeline are built to say so.

---

## See also

- [`docs/recipes.md`](recipes.md) — what a recipe is, the pins each certified
  recipe carries, and why a recipe may never classify a regression.
- [`compatibility/recipes.yaml`](../compatibility/recipes.yaml) — the authority
  for the table above, with per-entry rationale and the measured cost of each
  certification run.
- [`compatibility/kubernetes.yaml`](../compatibility/kubernetes.yaml) — the
  provisionable versions and their pinned node-image digests.
