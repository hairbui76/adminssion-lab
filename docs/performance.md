# Performance and reliability budgets

[PRODUCT.md §33](../PRODUCT.md) states five targets and one closing sentence.
This document says which of them are **enforced** (a number a script checks, and
fails on), which are **reported** (a number a script measures and prints,
because asserting it would be asserting the weather), what was actually
measured, and how the repeated-run flake budget is enforced in CI.

The closing sentence is the one everything below is organized around:

> Before v1, repeated-run reliability is more important than optimizing a few
> seconds of runtime.

---

## Contents

- [Enforced, reported, and why the line is where it is](#enforced-reported-and-why-the-line-is-where-it-is)
- [Measured](#measured)
- [Running the benchmarks](#running-the-benchmarks)
- [The budget knobs](#the-budget-knobs)
- [The flake budget](#the-flake-budget)
- [The reference-runner caveat](#the-reference-runner-caveat)

---

## Enforced, reported, and why the line is where it is

| PRODUCT.md §33 target | Status | Where |
| --- | --- | --- |
| 100-fixture admission suite within ~5 minutes, excluding component installation | **Enforced** | [`scripts/benchmark-alpha.sh`](../scripts/benchmark-alpha.sh) |
| Semantic comparison of 100 fixtures under 1 second | **Enforced** | [`scripts/benchmark-alpha.sh`](../scripts/benchmark-alpha.sh), [`scripts/benchmark-gateway.sh`](../scripts/benchmark-gateway.sh) |
| `kind` cluster creation under ~90 seconds per cluster | Reported (median and p95) | both scripts |
| No leaked cluster after normal failure paths | **Enforced** | both scripts, [`scripts/verify-cleanup.sh`](../scripts/verify-cleanup.sh), and every job in [`.github/workflows/nightly.yml`](../.github/workflows/nightly.yml) |
| Sufficient diagnostics on first failure | Enforced elsewhere | [`.github/workflows/nightly.yml`](../.github/workflows/nightly.yml)'s four forced-failure jobs |
| Gateway reconcile and probe timings | Reported | [`scripts/benchmark-gateway.sh`](../scripts/benchmark-gateway.sh) |

Two things are worth being explicit about.

**Cluster creation is reported and never asserted.** ROADMAP Task 9.8 step 2
says so directly — "approximately 90 seconds per cluster is a target, not a PR
hard fail due to hosted-runner variance" — and Task 5.7 step 4 said it first.
Cluster creation is the stage most exposed to runner variance and the one this
project controls least: it is `kind` starting a container, a kubelet, and a
control plane, on whatever disk and however many cores the runner has. An
assertion there would fail on a busy afternoon and teach everyone to re-run it.

What the scripts do instead is keep the samples. Every cluster either benchmark
creates appends its own creation time to
`target/benchmark/kind-create-seconds.tsv`, and every run prints the median and
p95 across everything in that file — so the number grows into a statistic
instead of being two numbers with a percentile's name on them. The file is under
`target/`, which `.gitignore` excludes: these are local measurements, not
committed evidence.

**There is no Gateway budget, because §33 states none.** The Gateway
benchmark reports the reconcile and probe timings ROADMAP Task 9.8 step 3 asks
for and asserts only the one §33 number in its scope (comparison) plus the
suite's own determinism. Inventing a "routes must reconcile in under N seconds"
target in a shell script would be inventing a product promise in the wrong file.

---

## Measured

### The reference host

Everything below was measured on one machine, twice per script, with a warm
`kind` node-image cache and a warm Docker layer cache:

| | |
| --- | --- |
| CPU | 28 cores, x86_64 |
| OS | Linux 5.14 (RHEL 9) |
| Docker | 29.4.1 |
| `kind` | v0.33.0 |
| Kubernetes | 1.36.4 on both sides (Tier 1 primary) |
| Build | `cargo build --release` |

This is **not** a GitHub-hosted runner. See
[the reference-runner caveat](#the-reference-runner-caveat) before reading any
headroom figure as a promise.

### Admission: `scripts/benchmark-alpha.sh`

100 Pod fixtures generated as one `FixtureMatrix`, two `kind` clusters, one
`manifests` component per side. Two consecutive runs:

| Stage | Run 1 (wall / baseline / candidate) | Run 2 |
| --- | --- | --- |
| cluster creation | 9.875s / 9.370s / 9.875s | 10.138s / 10.055s / 10.138s |
| installation | 0.125s / 0.120s / 0.124s | 0.135s / 0.133s / 0.134s |
| **fixture capture** | **10.977s** / 10.977s / 10.872s | **10.548s** / 10.497s / 10.548s |
| **comparison** | **0.002s** | **0.002s** |
| reporting | 0.01s | 0.01s |
| cleanup | 1.21s | 0.96s |
| `result.json` elapsed | 21.080s | 20.937s |
| total wall-clock | 22.33s | 21.92s |
| per-fixture capture | 0.110s (slower side) | 0.105s |

Both runs reported 100/100 identical fixtures, passed both asserted budgets, and
left no cluster behind. The figures reproduce
[`docs/architecture.md` §6.2](architecture.md)'s Task 5.7 measurement (10.880s /
11.475s of capture, 22.06s / 22.12s total) closely enough that the two are the
same measurement taken a phase apart.

### Gateway: `scripts/benchmark-gateway.sh`

One route contract, two probes, two `kind` clusters each running NGINX Gateway
Fabric 2.6.7 against Gateway API v1.5.1, plus one admission fixture. Two
consecutive runs:

| Stage | Run 1 (wall / baseline / candidate) | Run 2 |
| --- | --- | --- |
| cluster creation | 11.400s / 11.231s / 11.400s | 11.986s / 11.974s / 11.986s |
| installation | 37.328s / 37.328s / 37.092s | 35.942s / 35.751s / 35.927s |
| fixture capture | 0.187s / 0.187s / 0.186s | 0.166s / 0.163s / 0.166s |
| **gateway suite** | **27.961s** / 23.988s / 27.960s | **24.011s** / 24.009s / 24.010s |
| **comparison** | **0.000s** | **0.000s** |
| reporting | 0.00s | 0.01s |
| cleanup | 1.09s | 1.23s |
| `result.json` elapsed | 76.976s | 72.250s |
| total wall-clock | 78.10s | 73.57s |

Inside the gateway stage, the two numbers ROADMAP Task 9.8 step 3 asks for by
name — both of them the observed evidence's own monotonic measurements, not the
script's:

| | Run 1 (baseline / candidate) | Run 2 |
| --- | --- | --- |
| route reconciliation (first poll to last) | 0.263s / 0.266s | 0.266s / 0.265s |
| probe 0 (`/bench/probe`, HTTP 200 from `echo-a`) | 0.004s / 0.002s, 1 attempt each | 0.003s / 0.003s |
| probe 1 (`/not-bench/probe`, HTTP 404) | 0.001s / 0.001s, 1 attempt each | 0.001s / 0.001s |

And the per-component install breakdown, which is where the stage's 36-37
seconds actually goes:

| Component | Run 1 (baseline / candidate) | Run 2 |
| --- | --- | --- |
| `gateway-api-crds` (vendored v1.5.1 bundle) | 0.630s / 0.636s | 0.667s / 0.667s |
| `nginx-gateway-fabric` (OCI Helm chart) | 20.590s / 20.342s | 18.990s / 19.143s |

The gap between the per-side install sums (~21s) and the stage wall (~36s) is
the OCI chart pull and the two sides contending for one Docker daemon and one
network.

Both runs exited 0, both sides' routes converged, every probe returned its
contracted response on both sides, and no cluster survived.

**Why NGF and not Istio.** The Gateway benchmark had a choice of two certified
implementations and took the cheaper one, measured rather than assumed:
`recipes/nginx-gateway-fabric/recipe.yaml` records NGF's Helm install returning
in 12.5s on a real cluster, while `.github/workflows/nightly.yml`'s
`dogfood-gateway` job measures the Istio-based `examples/gateway-istio` at
204.5s per run. The measurement above — 73–78s per run end to end, two clusters
included — is the same order of saving. The vendor is not the subject: what is
being timed is how long Admission Lab takes to apply a suite, observe a route
converge, and probe a data plane, and NGF exercises every one of those paths.

### `kind` cluster creation

Across the ten clusters created while measuring the above — the four runs
tabulated here plus one confirmation run of the Gateway benchmark — on the
reference host:

| | |
| --- | --- |
| samples | 10 |
| fastest | 9.370s |
| median | 10.325s |
| p95 (nearest rank) | 11.986s |
| slowest | 11.986s |

With fewer than 20 samples, nearest-rank p95 *is* the slowest sample, and both
scripts say so on the line where they print it rather than letting the label
imply more resolution than the file holds. Keep running the benchmarks and the
same file answers the same question with more of a distribution behind it.

### Against PRODUCT.md §33

| Target | Measured | Headroom |
| --- | --- | --- |
| 100-fixture suite within ~5 minutes excluding installation | 10.5–11.0s of fixture stage (0.13s of installation, excluded by construction) | **~27x** |
| Comparison of 100 fixtures under 1 second | 0.002s | ~500x |
| `kind` cluster creation under ~90s per cluster | 10.325s median, 11.986s p95 | ~8x |

The suite budget is the binding one, and it is the one
[`docs/architecture.md` §6](architecture.md)'s serial-versus-parallel decision
turns on. It has not moved.

---

## Running the benchmarks

Both scripts need `docker`, `kind`, `jq` and a Rust toolchain on `PATH`;
`benchmark-gateway.sh` additionally needs `helm`. Both create and delete real
`kind` clusters, and both verify at the end that they left none behind.

```bash
# 100 real admission fixtures through two real API servers.
# ~25 seconds on the reference host; several minutes on a cold one.
./scripts/benchmark-alpha.sh

# One deterministic Gateway route suite through two real NGF stacks.
# ~80 seconds on the reference host with warm caches.
./scripts/benchmark-gateway.sh
```

Both are Phase 9 / v1 RC exit-gate commands: the gate runs them and they must
pass.

Each builds `admissionlab` in release mode first, or measures the binary named
by `ADMISSIONLAB_BIN` — which must itself be a release build, because a debug
build measures a program nobody ships. `benchmark-gateway.sh` also runs
`scripts/build-test-images.sh` to put the echo backend in the local Docker
store; that is a cached-layer no-op when nothing changed, and is what stops the
benchmark measuring a backend built from some other commit.

Each script prints a stable table, appends its cluster-creation samples, checks
its budgets, and exits `0` only if everything held. Exit `1` means the run, the
suite's contracted behavior, or a budget failed — and the message says which.

---

## The budget knobs

Every knob is an environment variable, every one is optional, and none of them
turns enforcement off. They exist to move a ceiling for a runner class whose
real behavior you know.

| Variable | Applies to | Default |
| --- | --- | --- |
| `ADMISSIONLAB_BENCH_MAX_CAPTURE_SECONDS` | `benchmark-alpha.sh`, the fixture stage's wall-clock | §33's 300s for a corpus of 100 or fewer; 3s per fixture above that |
| `ADMISSIONLAB_BENCH_MAX_COMPARISON_SECONDS` | both scripts, the comparison stage | §33's 1s for a corpus of 100 or fewer; 0.01s per fixture above that |
| `ADMISSIONLAB_BENCH_FIXTURES` | `benchmark-alpha.sh` | 100, the size §33 states its targets at |
| `ADMISSIONLAB_BENCH_SAMPLE_FILE` | both scripts | `target/benchmark/kind-create-seconds.tsv` |
| `ADMISSIONLAB_BENCH_SKIP_IMAGE_BUILD` | `benchmark-gateway.sh` | unset; the echo image is rebuilt every run |
| `ADMISSIONLAB_BIN` | both scripts | unset; a release build is made |

Two rules are worth stating outside the table.

**The ceilings scale up from 100 fixtures and never down.** §33 states its
numbers at 100 fixtures. A 10-fixture exploration is therefore held to the same
300-second and 1-second ceilings, not to a tenth of them: a smaller corpus was
never measured or promised at a smaller budget, and a proportional ceiling would
invent one.

**The ceilings are sized to catch a tenfold regression, not a ten-percent
one.** 300s against a measured 10.5s is roughly 27x. That is deliberate. A
budget tight enough to catch a small regression on this host is a budget that
fails on a slower one, and a benchmark everyone has learned to re-run enforces
nothing at all. Small regressions are what the tables above are for; the
assertion is for the day something becomes quadratic.

---

## The flake budget

ROADMAP Task 9.8 step 4: *"Run canonical dogfood admission demo 100 times and
Gateway demos 50 times in scheduled/sharded CI. Any unexplained false regression
or cross-correlation is release-blocking. Infrastructure failures are tracked
separately but repeated environment-induced flakes require diagnosis."*

### What runs, how often, and in how many pieces

[`.github/workflows/nightly.yml`](../.github/workflows/nightly.yml), on a
schedule (03:17 UTC) and on demand:

| Demo | Expected exit | Runs | Shape |
| --- | --- | --- | --- |
| `examples/admission-basic` | 0 | 100 | `dogfood-basic`, 4 shards × 25 |
| `examples/gateway-istio` | 1 | 25 | `dogfood-gateway`, 5 shards × 5 |
| `examples/ingress-to-gateway` | 1 | 25 | `dogfood-migration`, 5 shards × 5 |
| `examples/kyverno-istio-upgrade` | 1 | 5 | `dogfood-regression`, unsharded |
| `kind` create/delete cycles | — | 100 | `cluster-cycles`, unsharded on purpose |

"Gateway demos 50 times" is split evenly across the project's two Gateway demos.
They ask different questions of the same asynchronous machinery — `gateway-istio`
whether a route's reconciliation and traffic behavior are stable across runs,
`ingress-to-gateway` whether an Ingress-to-Gateway migration comparison is — and
neither is more likely to flake, so neither earns the larger half.
`kyverno-istio-upgrade` is not a Gateway demo and stays at 5; it asks a binary
product question ("is the known regression still detected?") that a sixth run
does not answer better.

`cluster-cycles` is deliberately **not** sharded. Sharding is right for a flake
budget and wrong for a leak budget: 100 independent trials are 100 trials on any
number of machines, but "does a long unattended sequence on *one* machine
accumulate anything?" is a question four fresh runner VMs cannot be asked.

### Flake versus regression

Every loop asserts its *expected* exit code, never "success":

- `dogfood-basic` runs a demo built to pass. Any non-zero exit is a flake or a
  regression **in Admission Lab itself**. Its failure means *the tool is not
  reliable*.
- `dogfood-gateway`, `dogfood-migration` and `dogfood-regression` run demos
  built to find a real seeded regression. Exit 1 is the pass condition; exit 0
  means Admission Lab stopped detecting a known regression, and any other code
  means the run never reached a verdict. Their failure means *the tool stopped
  being correct*.

Keeping them apart is the whole point: folded together they would produce one
red X that answers neither question.

Every shard collects each failed iteration's whole run workspace, strips the
per-cluster kubeconfigs from it, and uploads it as a shard-named artifact
(`nightly-dogfood-<demo>-diagnostics-shard-<n>`, 14-day retention). Successful
iterations' workspaces are deleted as they go — a job that dies of ENOSPC on run
11 answers nothing about runs 12–25.

### Release-blocking

- **An unexplained unexpected exit code blocks the release.** Explained means a
  named root cause in Admission Lab, in a vendor, or in the runner environment.
  "It passed on the retry" is not an explanation.
- **Cross-correlation blocks the release outright, explained or not.** A finding
  attributed to the wrong fixture or route contract is the fabrication Global
  Constraint 15 and the whole evidence model exist to prevent;
  [`docs/architecture.md` §6.4](architecture.md) is why fixture capture is
  serial in the first place.
- **Infrastructure failures are tracked separately** and do not block on their
  own — a runner that lost its Docker daemon, a registry that timed out. But
  repeated environment-induced flakes require diagnosis: the second occurrence of
  the same environmental failure is a pattern, and an unexplained pattern is
  indistinguishable from a product bug that only appears under load.

The Phase 9 exit gate lists "unexplained flaky weighted-routing or Gateway
convergence test" among its release blockers, and this suite plus
`.github/workflows/recipe-matrix.yml`'s `portable-contracts` job are what would
surface one.

---

## The reference-runner caveat

Every number in [Measured](#measured) came from one unloaded 28-core Linux
machine with warm `kind` node-image and Docker layer caches. A GitHub-hosted
`ubuntu-latest` runner has four cores, less disk bandwidth, and — the part that
usually dominates — a cold container-image cache, so it pays for every vendor
image and every Helm chart on first use.

So:

- **The headroom figures are not portable.** ~27x on this host might be ~5x on a
  hosted runner. That is still headroom, which is the point of sizing the
  ceilings for a tenfold regression rather than a tenth.
- **The Gateway numbers move the most.** 36 seconds of installation on a warm
  host is mostly an OCI chart pull that was already local. Cold, it is a
  download.
- **The cluster-creation samples are the honest ones to compare across hosts,**
  because that is exactly what the median-and-p95 file is for: run the
  benchmarks on the machine you care about and read its own distribution rather
  than this document's.
- **§33 is a set of engineering goals, not a guarantee across every CI
  provider.** It says so itself, in its first line.

To refresh these tables, run each script twice on a quiet machine and replace
the numbers along with the host description. Do not merge measurements from two
different hosts into one table.
