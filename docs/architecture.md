# Architecture

How Admission Lab is put together, as built.

This document describes the code that exists, not a plan. Where the as-built
shape differs from the layering `ROADMAP.md` §1.1 drew up front, the deviation
is named here together with the reason it was taken — an architecture document
that quietly redraws the map to match the code teaches nobody anything.

Everything below is verifiable from the crates it names. Where a rule is
enforced by a test or a comment rather than by the type system, the file that
enforces it is cited.

---

## Contents

- [1. Crate map](#1-crate-map)
- [2. Dependency direction, as built](#2-dependency-direction-as-built)
- [3. The run pipeline](#3-the-run-pipeline)
- [4. The evidence model](#4-the-evidence-model)
- [5. Audit correlation](#5-audit-correlation)
- [6. Fixture execution is serial, and why](#6-fixture-execution-is-serial-and-why)

---

## 1. Crate map

Fifteen workspace members under `crates/`. Two are stubs reserved for Phase 6
and beyond; one exists only to be run inside a disposable cluster.

| Crate | What it owns |
| --- | --- |
| `admissionlab-spec` | The `admissionlab.io/v1alpha1` `Lab` document: strict model, loader, `resolve_lab`, and the JSON Schema. Owns the *resolved* install/readiness/normalization/capability vocabulary every other crate names. |
| `admissionlab-core` | Run identity and workspace: `RunId`/`FixtureId`/`Side`/`Diagnostic`, `ArtifactStore`/`RunPaths`, `ProcessRunner`, `RunManifest`, `StageTimings`, `RunDisposition`, and `LabRunner` — the orchestration of the stages that happen *while clusters exist*. Declares the `ClusterManager`, `StackInstaller` and `FixtureCapture` traits. |
| `admissionlab-cluster` | `KindClusterManager`: the `kind` lifecycle, per-side kubeconfigs, node-image resolution against `compatibility/kubernetes.yaml`, and the rendered audit policy every cluster boots with. |
| `admissionlab-installer` | Installing one side's components: `HelmInstaller`, the raw-manifest backend, the readiness probes, and `install_stack`'s ordered component loop. |
| `admissionlab-recipes` | The recipe document model and the built-in Kyverno / Istio / test-webhook recipes. Carries install, readiness, normalization and capability metadata — and, by construction, no regression classification whatsoever. |
| `admissionlab-fixtures` | Fixture discovery and identity (`FixtureSource`, SHA-256 source hashing), matrix expansion, live `apiVersion`/`kind` resolution, and `dry_run_create` — the one place a fixture object is put on the wire. |
| `admissionlab-admission` | The observed admission model (`AdmissionOutcome`, `AdmissionTrace`, `WebhookInvocation`), the executor that issues a server-side dry-run CREATE, the audit-log reader and correlator, the optional `/metrics` scrape, and `KubeFixtureCapture` — the serial per-side replay loop. |
| `admissionlab-normalize` | Deterministic canonicalization of captured objects and traces, driven by tiered `NormalizeRule`s (built-in, recipe, user). |
| `admissionlab-diff` | Semantic difference: decision, workload mutation, and webhook-trace comparison, plus first-divergence attribution and the comparability predicates. |
| `admissionlab-policy` | Severity. The default severity table, `expectations.yaml` matching, and `evaluate_with_expectations` — the only code in the product that grades anything. |
| `admissionlab-report` | `LabResult`, the single redaction pass, and the three renderers (terminal, JSON, standalone HTML) plus the GitHub job summary. |
| `admissionlab-cli` | The `admissionlab` binary — `test`, `doctor`, `reproduce` — the exit-code mapping, and the compare-and-report assembly (`src/pipeline/`). A `[lib]` as well as a `[[bin]]`, so integration tests can drive the pipeline through fake backends. |
| `admissionlab-test-webhook` | The deterministic dogfood webhook (PRODUCT.md §30): a container image that denies, mutates, fails, or sleeps on command, used by the project's own integration tests. Never part of a user's lab. |
| `admissionlab-gateway` | Reserved for Phase 6. Currently one line: `#![forbid(unsafe_code)]`. |
| `admissionlab-echo` | Reserved test fixture binary. Currently `fn main() {}`. |

---

## 2. Dependency direction, as built

### 2.1 The graph

```text
spec                                    (leaf — depends on nothing in the workspace)
core        -> spec
cluster     -> core
installer   -> core, spec
recipes     -> spec
fixtures    -> core, spec
admission   -> core, fixtures
normalize   -> admission
diff        -> admission, core, normalize, spec
policy      -> core, diff, spec
report      -> admission, core, diff, policy
cli         -> every crate above
test-webhook, gateway, echo             (leaves)
```

`admissionlab-cli` is the designated sink: it depends on everything and nothing
depends on it, so its edges can never take part in a cycle. `admissionlab-spec`
is the designated source: it depends on nothing here, which is what makes
`core -> spec`, `installer -> spec`, `diff -> spec` and `policy -> spec` all
provably acyclic.

### 2.2 Deviations from ROADMAP §1.1

Four differences between the drawn graph and the built one. All four are
deliberate, and each is documented on the `Cargo.toml` entry that creates it.

**(a) `core` does not depend on the stage crates; the arrows are inverted into
traits.** §1.1 draws `core -> cluster`, `core -> installer`, `core -> fixtures`,
`core -> admission`, `core -> normalize`, `core -> diff`, `core -> policy`,
`core -> report`, `core -> recipes`. **None of those edges exists.**
`admissionlab-core`'s only workspace dependency is `admissionlab-spec`. Where
`LabRunner` needs a cluster backend or an installer, it names a trait declared
in `core` itself and implemented downstream.

The forcing argument is Controller Ruling R22, spelled out in
`crates/admissionlab-core/src/cluster.rs`: `ClusterManager::create` takes a
`&RunPaths` and a `ClusterSpec` holding a `Side`, so whichever crate declares the
trait must depend on `core`; but `LabRunner<C: ClusterManager>` lives *in* `core`,
so `core` must be able to name the trait. Together those are a cycle, which Cargo
rejects outright. The abstraction is therefore pulled *up* into `core` and the
implementation left downstream. `StackInstaller` and `FixtureCapture` follow the
same pattern for the same reason.

**(b) `normalize`, `diff` and `report` each depend on `admission`.** §1.1 reads
"normalize -> core domain types only", "diff -> normalize/core domain types",
"report -> policy/diff/core domain types". In the built graph all three also
reach `admissionlab-admission`, because the observed-evidence model lives there
and §1.2's frozen signatures are stated in terms of it:

- `normalize_trace(&AdmissionTrace) -> NormalizedTrace`;
- `diff_admission_decision(&AdmissionOutcome, &AdmissionOutcome, …)`;
- `AdmissionComparison { baseline: AdmissionOutcome, candidate: AdmissionOutcome, … }`.

There is no `core` counterpart to normalize or diff against, and minting a
parallel evidence type in `core` purely to satisfy an arrow would duplicate the
one model whose entire value is that exactly one type says what was observed.
The edges are one-way and stay one-way: `admission -> normalize`,
`admission -> diff` and `admission -> report` are all explicitly forbidden.

**(c) `diff -> spec` and `policy -> spec`.** Neither is drawn. Both are real:
`diff_admission_trace` takes a `&admissionlab_spec::LatencyPolicy`, and
`PolicySpec`/`PolicyOverrideSpec` are assigned to `admissionlab-spec` by §1.2
because they are user-authored configuration. Since `spec` is a leaf, both edges
are safe; the rule that keeps them safe is that `spec -> diff` and
`spec -> policy` must never be added.

**(d) The compare-and-report assembly lives in the CLI, not in `core`.** This is
the largest deviation, and the reason is again a cycle.

`admissionlab-core` owns `LabRunner`, which drives cluster creation, install,
capture and cleanup. It does *not* drive normalization, diff, grading or
reporting. Those stages need concrete types — `NormalizedObject`,
`SemanticChange`, `PolicyResult`, `LabResult` — that live in crates sitting
*above* `core`. A `core -> policy` edge would close

```text
core -> policy -> diff -> admission -> fixtures -> core
```

which Cargo rejects. R22's answer (pull the abstraction up into `core`) does not
apply here because the direction is reversed: the comparison stage needs
concrete types `core` can never name, so it has to settle *above* every crate
that owns one. `admissionlab-cli` is the only such place.

Two visible consequences, both documented at the seam:

- `admissionlab_core::CapturedFixture` deliberately carries no outcome. Outcomes
  reach the comparison through `OutcomeCapture::captured_outcomes`, a CLI-side
  trait extending `core`'s `FixtureCapture` — `outcome.json` is emit-only and
  does not round-trip.
- Nothing in `admissionlab-core` decides a lab's verdict. `RunDisposition` is
  defined there, but every mapping into it lives in
  `crates/admissionlab-cli/src/exit.rs`.

The same "the seam lands in the assembler" rule places two smaller conversions
in the CLI: `RecipeNormalizeRule -> NormalizeRule` (`pipeline/compare.rs`) and
`FixtureSource`/`NormalizationProfile` -> run-manifest provenance
(`pipeline/provenance.rs`).

This is not "the CLI duplicating orchestration logic", which §1.1 forbids. There
is exactly one orchestrator per half: `LabRunner` owns everything that happens
while clusters exist, and `pipeline::run_lab` owns the order of the whole run and
calls into it. Neither reimplements the other.

### 2.3 The edges that must never be added

Enforced by review and by the comment on each `Cargo.toml` entry, not by the
compiler — most of them would be caught by Cargo as a cycle, but not all.

| Forbidden | Why |
| --- | --- |
| `core -> cluster` / `installer` / `fixtures` / `admission` / `normalize` / `diff` / `policy` / `report` | Every one closes a cycle. The trait inversion in §2.2(a) is the substitute. |
| `admission -> normalize` / `diff` / `report` | Evidence is produced once and consumed downstream; a back-edge would let capture depend on how its output is judged. |
| `normalize -> diff` | Canonicalization must not know what the comparison will conclude. |
| `policy -> report` | §1.1: report rendering never decides severity. `report` reads grades; it never produces one. |
| `spec -> diff` / `policy` | `spec` stays a leaf beneath everything. |
| `recipes -> diff` / `policy` | Global Constraint 6 / PRODUCT.md §14: a recipe may describe a stack, never classify a regression. |
| `installer -> cluster` | The installer works against a `ClusterHandle`; it never provisions. |

---

## 3. The run pipeline

### 3.1 Stages

`admissionlab_core::run_manifest::RunStage` is the vocabulary. Every other
per-stage record — the run manifest's failure stage, the stage timings, the
console progress lines — uses these same names, so "failed at `fixture_capture`"
in `run.json` and "`fixtureCapture` took 11s" in `result.json` name the same
stretch of the run.

| `RunStage` | What completed | Performed by | Exit code if it fails |
| --- | --- | --- | ---: |
| `started` | Workspace created; configuration, fixtures, expectations, normalization, policy, host, tool versions and both sides' node images recorded. Nothing provisioned. | `spec`, `fixtures`, `policy`, `core` | `2` (workspace I/O: `3`) |
| `cluster_creation` | Both ephemeral clusters exist. | `cluster` (`KindClusterManager`), driven by `LabRunner` | `3` |
| `installation` | Both sides' components installed **and ready**. | `installer` (`install_stack`), driven by `LabRunner` | `4` |
| `fixture_capture` | Every fixture replayed through both sides and its evidence written. | `admission` (`KubeFixtureCapture`) + `fixtures` (resolution, dry-run) | `5` |
| `comparison` | Both sides' evidence normalized, compared and graded. | `normalize`, `diff`, `policy` | `2` or `6` |
| `reporting` | `result.json`, `report.html` and the job summary written. | `report` | `3` or `6` |
| `completed` | The run finished. Says nothing about the verdict. | — | `0` / `1` |

Cleanup happens after `completed` and therefore has no `RunStage` of its own. It
does have a timing (`TimedStage::Cleanup`) because it is a real and sometimes
slow part of a run, and because omitting it would leave a visible gap between the
stages' sum and the elapsed total.

The full exit-code table lives in
[`docs/troubleshooting.md`](troubleshooting.md#exit-code-quick-reference); the
mapping from each typed failure to a `RunDisposition` is
`crates/admissionlab-cli/src/exit.rs`, where every `match` is exhaustive with no
wildcard arm, so a new error variant fails to compile until somebody decides what
it means.

### 3.2 Three structural rules

**Every input check happens before any cluster is created.** Fixture discovery,
policy validation and expectations loading are all hoisted above cluster
creation. They are exit-2 categories, and discovering a fixture with no
`metadata.name` only after provisioning two `kind` clusters would charge a user
several minutes to learn something knowable in milliseconds. The one check that
genuinely cannot be hoisted — resolving a fixture's `apiVersion`/`kind` against
the live cluster's discovered API surface — happens inside capture and is exit 5.

**Everything after cluster creation funnels through cleanup.** `run_lab` creates
the clusters, hands off to a single inner function, and then always calls
`finish`. There is exactly one path from "the clusters exist" to "this function
returns", and cleanup is on it. That structure is what implements PRODUCT.md
§33's "no leaked cluster after normal failure paths"; the companion rule, in
`exit.rs`, is that a run whose clusters could not be deleted never exits `0` — a
passing run is downgraded to `3`, because `0` is a positive claim that the run
completed cleanly and a machine left with two clusters running has not.

**A run that fails late still writes what it knows, and never a verdict.** A
failure at or after installation writes `diagnostics.json` — the failed stage plus
every diagnostic collected so far — *before* cleanup, so the evidence outlives the
clusters. It deliberately writes no `result.json`: a run that never compared both
sides has not earned one, and manufacturing a `pass` (or a `fail`) there would be
exactly the fabrication Global Constraint 15 forbids.

### 3.3 Concurrency in the pipeline

There is exactly one axis of concurrency in a lab run: **the two sides**.
`LabRunner` drives baseline and candidate together with `tokio::join!` — never
`try_join!`, so one side failing does not cancel the other mid-`kind`-create and
leave a half-built cluster behind — for cluster creation, install, capture and
cleanup.

Everything inside a side is sequential, and in two places that is a correctness
requirement rather than a simplification:

- **Components install one at a time within a side.** Helm's repository config
  is isolated per side, not per component; two Helm components installing
  concurrently onto the same side would race on one `repositories.yaml`.
- **Fixtures replay one at a time within a cluster.** This is Global Constraint
  17, and §5 and §6 below are about it.

Because both sides run concurrently, a timer wrapped around `capture_fixtures`
measures the *pair*. Per-side numbers are taken by three transparent decorators
(`TimedClusterManager`, `TimedStackInstaller`, `TimedFixtureCapture`) at the only
seam where one side is distinguishable from the other: the trait call that names
a side.

---

## 4. The evidence model

Global Constraint 15: *missing observability data is represented as
unavailable/unknown; it must never be fabricated or presented as proven
causality.* This is not a convention applied by hand at each call site. It is
built into the types, in three repeated moves.

### 4.1 Three states, never two

Wherever the product can be uncertain, the type says so explicitly:

| Type | Crate | States |
| --- | --- | --- |
| `TraceEvidence` | `admission` | `Observed` — the full webhook chain was watched. `Partial` — some of it was; `invocations` may be missing entries. `Unavailable` — no usable evidence; treat `invocations` as empty regardless of its actual length. |
| `WebhookOutcome` | `admission` | `Allowed`, `Denied`, `Errored`, `Unknown`. Four, not three: `failurePolicy: Ignore` turns an error into an allow and `Fail` turns it into a deny, so `Errored` must stay distinguishable from `Denied`. |
| `DivergenceConfidence` | `diff` | `Observed` — the divergence itself was seen on both sides. `Inferred` — deduced from evidence incomplete on at least one side; the conclusion is deterministic, the observation was not complete. `Unknown` — a difference exists but the evidence does not locate it. |
| `TraceComparability` | `diff` | `Comparable`, `Partial { baseline, candidate }`, `Incomparable { baseline, candidate }`. Under `Partial`, an invocation missing from one side proves nothing and no added/removed claim is made about it. |
| `DecisionComparability` | `diff` | `Comparable`, `Incomparable { baseline, candidate }` — so an empty change list meaning "both sides agreed" is distinguishable from one meaning "there was nothing to compare". |
| `FixtureBucket` | `report` | `Identical`, `Expected`, `Warnings`, `Critical`, `Inconclusive`. The last is never folded into the first. |

### 4.2 No `Default`, anywhere in this family

None of the types above derives `Default`, and no field holding one may carry
`#[serde(default)]`. The reason is uniform: `Observed` is the variant a developer
writes first, so an accidental default would silently upgrade an unproven claim
to a proven one. A serialized `AdmissionTrace` that omits its `evidence` field
therefore fails to deserialize rather than defaulting —
`deserializing_admission_trace_without_evidence_fails` in
`crates/admissionlab-admission/tests/model.rs` pins that.

### 4.3 Absent is absent, never zero

The same rule stated numerically. A `0` is a measurement; an absence is not one,
and the two are never conflated:

- `WebhookInvocation::latency` is `Option<Duration>` and serializes absence as
  JSON `null`, never `0`. A zero would read as "instantaneous", which the latency
  comparison would then treat as a real — and false — improvement.
- `WebhookInvocation::mutated` is `Option<bool>`; `None` is never collapsed to
  `Some(false)`, which would be the positive claim that the webhook ran and
  changed nothing.
- Every stage in `StageTimings` is an `Option` and every absent stage is *omitted
  from the serialized document*. A run that failed during installation never
  captured a fixture, and a `fixtureCapture` of zero milliseconds would state that
  capturing a hundred fixtures was instantaneous.
- `AdmissionMetricSnapshot`'s families are `Option<BTreeMap<…>>`, because "the
  family was absent from the page" and "the family was exported with no
  observations" are different facts. `DurationSample`'s `sum` and `count` are
  each `Option`: a missing half makes the whole delta `Unavailable` rather than a
  delta computed against a fabricated zero.
- An `AdmissionTrace` is never `Observed` with an empty `invocations` list —
  that combination is the positive claim that no mutating webhook ran, and
  capture only makes it when the evidence supports it.

### 4.4 Optional signals never fail a run

Per-webhook latency is an optional observed signal (Global Constraint 19). It is
attributed only when the evidence is unambiguous: `attributable_latency()`
returns `Some` if and only if the metric delta is `Observed`, the webhook's
request-count delta is exactly `1`, and the `_sum` increase converts to a
`Duration`. A count delta of `0` (the webhook did not run) and a delta above `1`
(background traffic shared the measurement window) both yield `None`.

Every failure mode of the `/metrics` scrape — no client, a failed request, a
timeout — is a recoverable absence recorded as a `Diagnostic`, never a run
failure. A caller that propagated one as a fixture failure would have turned an
optional signal into a required one.

---

## 5. Audit correlation

A fixture is executed as a server-side dry-run CREATE against a real API server
(Global Constraint 16). The response object is the admitted object, but the
response says nothing about *which* mutating webhooks ran or what each of them
patched. That evidence exists only in the API server's audit log, as annotations
on the request's own audit event. Correlating a request to its audit event is
therefore the load-bearing step in the whole capture pipeline, and it is designed
to fail loudly rather than guess.

### 5.1 The object is never touched

`admissionlab_fixtures::execute::dry_run_create` serializes `FixtureSource::object`
exactly as discovered: no correlation label, no annotation, no injected field of
any kind. This is a correctness rule, not a style preference — anything stamped
onto the object would change what the webhooks under test actually see, and the
lab would end up comparing behavior on an object the user never wrote.

Correlation therefore has to work from the outside, which is what the next two
subsections are.

### 5.2 The checkpoint is a byte offset

Immediately before issuing the request, capture takes an `AuditCheckpoint`:

```rust
pub struct AuditCheckpoint {
    pub byte_offset: u64,
}
```

A plain file offset, not a line number and not a timestamp — the only one of the
three that is exact, cheap to take (a single `stat`), and monotonic under
concurrent appends. `checkpoint()` is deliberately *synchronous* even though the
surrounding code is `async`: it is one `stat` with no waiting, taken microseconds
before the request goes out, and an `.await` point there would let the runtime
interleave something else and widen the very window the checkpoint exists to pin
down. `AuditCheckpoint` derives no `Default`, because a defaulted
`byte_offset: 0` would silently mean "read the entire audit log from the
beginning".

After the response arrives, `events_since(&checkpoint, deadline)` polls the log
from that offset — reopening the file each pass, so a rotation shows up as a
shrink and is reported as `AuditError::Truncated` rather than misread — parsing
only complete lines, and stopping as soon as a `ResponseComplete` event has been
drained or the deadline passes. A line that will not parse becomes an
`audit.line_unparsable` diagnostic; it is never fatal and never silently dropped.

The default correlation deadline is 10 seconds per fixture
(`DEFAULT_AUDIT_CORRELATION_TIMEOUT`), polled at 25 ms. Both numbers are chosen
against serial execution: every fixture pays this wait one after another, so an
unbounded wait on one wedged event would stall an entire run, and a coarse poll
interval would show up directly as run duration.

### 5.3 The match, and the tie that is never broken

`select_fixture_event(events, key, started, finished)` picks the fixture's own
event out of the window. `key` is an `ObjectKey` — group, version, resource,
namespace, name — with the name taken from the API server's *response* object
where possible, falling back to the fixture's own.

Candidates must satisfy, in order:

1. the event targets that exact object (no subresource, matching group, version,
   resource, namespace and name);
2. `stage == "ResponseComplete"`;
3. `verb == "create"`;
4. the request URI's query string genuinely contains `dryRun=All` — parsed, not
   substring-matched;
5. a `requestReceivedTimestamp` is present;
6. that timestamp falls inside `[started - 1µs, finished]`.

`requestReceivedTimestamp` rather than `stageTimestamp`: on a `ResponseComplete`
event the latter is written *after* the response was flushed, so it can land
fractionally after the client's own `finished` and would intermittently reject
the correct event. The 1 µs of slack on the lower bound is truncation of Go's
`metav1.MicroTime` layout, not a clock-skew allowance; the upper bound gets none.

Notably, the request's `uid` and `User-Agent` are *not* part of the match. Both
are parsed and both are written into the `audit.json` artifact, but neither
selects. Identity comes entirely from the object reference plus the time window —
which is exactly why the window has to be unambiguous. See §6.

The outcome is one of three, and there is no fourth:

```rust
pub enum CorrelationError {
    NoMatch   { key: Box<ObjectKey>, near_misses: Vec<NearMiss> },
    Ambiguous { key: Box<ObjectKey>, audit_ids: Vec<String> },
}
```

- **Exactly one candidate** — that is the fixture's event; its annotations
  reconstruct the mutating-webhook chain.
- **None** — `NoMatch`, carrying every event that named the same object but
  failed a criterion, each labelled with which criterion it failed (`Stage`,
  `Verb`, `DryRun`, `MissingTimestamp`, `OutsideWindow`). When nothing in the
  window referred to the object at all, the error says that too.
- **More than one** — `Ambiguous`, listing every equally valid candidate's
  `auditID`. **Admission Lab does not break the tie.** Picking the nearest
  timestamp would attach one fixture's webhook chain to another fixture's report,
  in a document whose entire value is being trustworthy about exactly that.

An `Ambiguous` is never retried, either: more events can only add candidates,
never remove one, so waiting longer cannot resolve a tie.

Both errors degrade the same way, and neither fails the fixture: the outcome is
returned with `TraceEvidence::Unavailable`, no invocations, and an
`admission.audit_correlation_failed` diagnostic carrying the verbatim reason. The
`audit.json` artifact is written either way, with the checkpoint, every event in
the window, and either the selected event or the correlation error — never both,
never neither.

### 5.4 Why the audit policy is `Request` level

Global Constraint 18. Mutating-webhook *invocation* annotations appear at
`Metadata` level, but the *patch* annotations — what each webhook actually
changed — appear only at `Request`. Reconstructing a trace needs both, so
`Request` it is.

`admissionlab_cluster::render_audit_policy()` is the single source of truth. It
takes no argument: every cluster Admission Lab creates boots with byte-identical
policy, so there is nothing for a caller to vary. Four first-match-wins rules, in
an order that is itself load-bearing:

1. `level: None` for core-group **`secrets`** — first, so that raising the level
   in rule 3 can never capture a Secret body. A test
   (`secret_exclusion_rule_precedes_general_request_rule`) pins the ordering.
2. `level: None` for health and discovery URLs (`/healthz*`, `/readyz*`,
   `/livez*`, `/version`, `/metrics`) — pure noise in a correlation window.
3. `level: Request` for `create`/`update`/`patch`/`delete` on the
   admission-relevant API groups (core, `apps`, `batch`, `networking.k8s.io`,
   `rbac.authorization.k8s.io`, `admissionregistration.k8s.io`).
4. `level: Metadata` catch-all.

The Secret exclusion has a visible consequence users should know about, and it is
documented in [`docs/troubleshooting.md`](troubleshooting.md#first-divergence-says-unknown-or-partial):
a fixture involving Secrets has less trace evidence available by construction.

---

## 6. Fixture execution is serial, and why

**Admission Lab v1 replays fixtures one at a time within each cluster. There is
no bounded-concurrency mode, no concurrency setting, and no correlation-tag
mechanism in the shipped code. This section explains why that is the right answer
for v1, and states precisely what would have to change for it not to be.**

### 6.1 What "serial" means here, exactly

Global Constraint 17: *Alpha fixture execution is serial within each cluster.
This makes audit-log correlation deterministic. Parallel fixture execution is
allowed only after request-level correlation is implemented and tested.*

The implementation is one plain `for` loop —
`KubeFixtureCapture::capture_side` in
`crates/admissionlab-admission/src/capture.rs`, carrying the comment that says
why it must stay one:

> A plain sequential loop, and that is the point: Global Constraint 17 makes at
> most one fixture request per cluster in flight at a time a correctness
> requirement for audit correlation, not a throughput choice.

The contract is stated one level up as well, on
`admissionlab_core::FixtureCapture::capture_side`, which takes a whole *side*
rather than a single fixture precisely so that the ordering guarantee lives
inside the implementation that depends on it rather than in a caller that could
always violate it.

Serial is scoped to one cluster, not to the run. Baseline and candidate are
separate clusters with separate API servers and separate audit logs, so neither
side's requests can appear in the other's correlation window; `LabRunner` runs
the two sides concurrently, and GC17 explicitly allows it.

### 6.2 The measurement (ROADMAP Task 5.7)

Task 5.7 instrumented every stage with monotonic timers and ran
`scripts/benchmark-alpha.sh`: a real 100-fixture Pod lab, two real `kind`
clusters, one `manifests` component per side, release build, warm node image,
16-core Linux host, `kind` v0.33.0. Two consecutive runs:

| Stage | Run 1 (wall / baseline / candidate) | Run 2 |
| --- | --- | --- |
| cluster creation | 9.906s / 9.906s / 9.555s | 9.296s / 9.295s / 9.295s |
| installation | 0.137s / 0.134s / 0.136s | 0.127s / 0.126s / 0.126s |
| **fixture capture** | **10.880s** / 10.752s / 10.880s | **11.475s** / 11.475s / 11.300s |
| comparison | 0.002s | 0.002s |
| reporting | 0.01s | 0.01s |
| cleanup | 0.97s | 1.06s |
| total wall-clock | 22.06s | 22.12s |
| **per-fixture capture** | **0.109s** (slower side) | **0.115s** |

Both runs reported 100/100 identical fixtures and left no cluster behind.

### 6.3 Against PRODUCT.md §33

§33's initial goals, and what was measured against each:

| Target (PRODUCT.md §33) | Measured | Headroom |
| --- | --- | --- |
| "typical `kind` cluster creation under approximately 90 seconds per cluster on a healthy CI runner" | 9.3–9.9s per cluster | ~9x |
| "semantic comparison of 100 ordinary fixtures in under one second after artifacts are collected" | 0.002s in the real lab; 0.012–0.014s in the release-mode `diff_benchmark` over 100 synthesized diverging pairs | ~70x on the pessimistic number |
| "100-fixture admission suite completes within approximately five minutes excluding component installation under normal CI conditions" | ~11s of capture; ~22s end to end including both clusters and cleanup | **~25x** |

The suite target is the one the parallelism question is actually about, and
serial capture clears it by a factor of roughly twenty-five. Even allowing a
GitHub-hosted runner several times slower than this host, the measurement does
not come close to the budget.

§33 also closes with the sentence this decision turns on: *"Before v1,
repeated-run reliability is more important than optimizing a few seconds of
runtime."*

### 6.4 What serial buys

Concurrency here would not be a performance/complexity trade. It would be a
performance/**correctness** trade, because the correlation primitive described in
§5 is only sound while one request is in flight.

Recall what identifies a fixture's audit event: an object reference plus a time
window bounded by a byte checkpoint. With one request in flight, at most one
`ResponseComplete` event in that window can be a dry-run CREATE of that object,
so the match is exact and the ambiguity branch is a defensive check that fires
only on a broken cluster.

With two requests in flight against one API server, three things break at once:

1. **The checkpoint stops bounding anything useful.** The byte offset taken
   before request A also precedes request B's event; both land in A's window.
2. **The time windows overlap.** `[started, finished]` for A contains B's
   `requestReceivedTimestamp` whenever the two requests overlap at all — which,
   with concurrency, is the normal case rather than the exception.
3. **Nothing else distinguishes them.** The object reference is the only identity
   in the match, and every field of the audit event that could carry a
   per-request identity is either absent or unused: the object is sent
   byte-for-byte with nothing stamped on it (§5.1), and `User-Agent` and `uid`
   are parsed but not matched on.

Two fixtures that create *different* objects would still be separable by object
reference. Two fixtures that create the same object — a matrix expansion varying
only a `spec` field, a re-run of the same corpus — would not be. The failure mode
is not a slow run or a flaky one; it is `Ambiguous` at best, and at worst one
fixture's webhook chain silently attached to another fixture's report. That is
precisely the fabrication the whole evidence model (§4) exists to prevent.

Serial execution eliminates the class outright, with no runtime cost that the
measurement in §6.2 can detect against the targets in §6.3.

### 6.5 What would trigger revisiting this

ROADMAP Task 5.8's decision gate is explicit about the alternative, and it is
recorded here as the future path rather than as work in progress. If serial
fixture capture ever exceeds the product target materially — the measurement in
§6.2 would have to regress by well over an order of magnitude for that to happen
— the roadmap's own five steps apply:

1. **Implement request-level correlation with a per-request unique `User-Agent`
   value visible in Kubernetes audit events:**

   ```rust
   pub struct CorrelationTag(String); // e.g. admissionlab/<version> run/<short-id> fixture/<fixture-id>
   ```

   A `User-Agent` is the right carrier precisely because it is *not* part of the
   object: §5.1's rule that the fixture is sent byte-for-byte survives intact,
   while `AuditEvent::user_agent` — already parsed, already written into
   `audit.json`, today simply unused for matching — becomes the discriminator.
2. **Prove the tag reaches the audit log**, in a real `kind` integration test,
   before anything depends on it.
3. **Add a max-concurrency configuration setting defaulting to `1` until Beta**,
   so that adopting the mechanism and adopting the concurrency remain two
   separate, separately reversible decisions.
4. **Run 100 concurrent and mixed-noise cases and prove zero cross-correlation**
   — including the hard case above: several in-flight requests creating
   identically named objects.
5. **If that proof fails, or if the tag introduces any observable change in
   webhook behavior, revert to serial and document the decision.** A webhook that
   can see the tag can in principle route on it, and a lab whose fixtures are
   distinguishable from the traffic they are meant to model is measuring the
   wrong thing.

Steps 3 and 5 are the load-bearing ones. Until every one of them has been done
and tested, GC17 stands: parallel fixture execution is not permitted.

### 6.6 The conclusion, stated plainly

Parallel fixture execution is **not implemented in v1**. Serial capture was
measured, meets PRODUCT.md §33's targets with roughly 25x of headroom on the
binding one, and buys a determinism guarantee — byte-offset audit correlation
with zero ambiguity — that concurrency would spend. `execute.rs` and
`capture.rs` are unchanged by this decision; the decision is that they should be.
