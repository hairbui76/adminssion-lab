# Release PR checklist

The manual acceptance pass a release candidate is judged against (ROADMAP Task
10.2). The rows are stated once here and **the results of the pass are recorded
here too**, filled in below for the candidate named in "The pass on record".
Copy the same table into the release PR body when one exists; this environment
has no GitHub remote, so this file is the record.

**Rule from Task 10.3:** no new feature enters the RC stabilization window. A
row that fails becomes a release blocker fixed by
`reproduce -> failing test -> minimal fix -> narrow test -> full phase gate ->
commit`. A non-blocking enhancement discovered along the way becomes a post-v1
issue instead.

Every row is a claim about the **packaged release binary** — the one unpacked
from `admissionlab-<version>-<target>.tar.gz` — never about `target/`. Record
for each row the command run, the exit code, and where the evidence lives
(artifact directory, job URL, or pasted output).

---

## The pass on record

| | |
| --- | --- |
| Candidate | `1.0.0-rc.1` |
| Commit | `6ed5147` — `release: prepare Admission Lab 1.0.0-rc.1`, 2026-09-01 21:15:46 +0900 |
| Pass run | 2026-09-01, 21:17–21:51 +0900, one Linux host (`x86_64-unknown-linux-gnu`), kind v0.33.0 / kubectl v1.32.11 / helm v3.20.0 / docker 29.4.1 |
| Binary under test | `admissionlab 1.0.0-rc.1`, unpacked from `admissionlab-1.0.0-rc.1-x86_64-unknown-linux-gnu.tar.gz` (sha256 `6a2494fdb4e8f404abf4e24523abaf50874cc7d97d3820e90b6833e7ffd3403f`), rebuilt at this commit by `./scripts/verify-release.sh --out-dir <staging>` |
| Verdict | **17 of 17 rows accounted for: 14 PASS with fresh evidence produced at this commit, 2 PASS by citation (rows 15 and 17), 1 OPERATOR (row 11).** No release blocker found. |

One commit landed mid-pass, from the concurrent maintenance-loop task:
`784e263` (21:30) adds `.github/workflows/maintenance.yml`,
`scripts/check-recipe-updates.sh`, three workflow edits, and
`docs/versioning.md` prose. The runs before it were made at `6ed5147` and the
two after it (rows 14's certification test and the migration demo) at `784e263`;
it touches no crate, recipe, example, or schema, so no row's evidence changes
meaning either way.

Two words are used precisely below:

- **fresh** — a command run during this pass, against this commit, with the
  packaged binary (or, where the row is about a certification test rather than
  about user experience, with `cargo test` at this commit).
- **cited** — a run made earlier the same day by the agent that owned the
  preceding task, at a stated commit, whose log is quoted. Every citation says
  which commit the run was made at and why the difference from `6ed5147` cannot
  affect the claim.

The two commits between the cited recipe runs and this one are
`3cabb57` (`CHANGELOG.md`, `compatibility/kubernetes.yaml`,
`compatibility/recipes.yaml`, `docs/compatibility.md` — additions only) and
`6ed5147` (the version bump: fifteen `Cargo.toml` version lines, `Cargo.lock`,
docs, and the six test builders that assert the version a run manifest records).
Neither touches a recipe, an installer, the diff engine, the report renderer, or
the cluster lifecycle, which is why a recipe run made at `f7e76af`/`3cabb57` is
still evidence about `6ed5147`.

---

## Prerequisite gates

The Phase 9 exit gate, on the RC tree. Run by Task 10.1 immediately before
`6ed5147` was committed (its commit message carries the same table); logs in
that pass's working directory.

| Gate | Result | Evidence |
| --- | --- | --- |
| `cargo fmt --all -- --check` | pass | Task 10.1, RC commit message |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass, 0 warnings | Task 10.1, RC commit message |
| `cargo test --workspace` | **1585 passed / 0 failed / 32 ignored**; no suite reported a failure | `test.log`, 21:02 |
| `cargo deny check` | advisories, bans, licenses, sources ok | Task 10.1, RC commit message |
| `./scripts/verify-cleanup.sh` | PASS — 10 cycles / 100 s, no `adlab-*` ever survived an iteration | `cleanup.log`, 21:04. The RC commit message records the 100-iteration tier green two hours earlier, with nothing since touching cluster lifecycle. |
| `./scripts/benchmark-alpha.sh` | PASS — 100 fixtures/side, fixture stage 10.951 s (budget 300 s), comparison 0.003 s (budget 1 s) | `bench-alpha.log`, 21:06 |
| `./scripts/benchmark-gateway.sh` | PASS — both sides converged, both probes contracted, comparison 0.000 s | `bench-gw.log`, 21:09 |
| `./scripts/verify-release.sh` | PASS — archive + 241-package SPDX-2.3 SBOM + `SHA256SUMS` verified, one-byte modification rejected, packaged binary reports `1.0.0-rc.1` | rerun fresh at 21:17 for this pass; identical result |

---

## The seventeen rows

| # | Row | Method | Verdict |
| ---: | --- | --- | --- |
| 1 | `doctor` tells a new user exactly which prerequisite is missing | fresh, packaged binary, twice (intact PATH, crippled PATH) | **PASS** |
| 2 | invalid config fails before creating clusters | fresh, packaged binary | **PASS** |
| 3 | `--keep-clusters` prints exact cleanup commands | fresh, packaged binary; printed commands then executed | **PASS** |
| 4 | normal failure cleans all clusters/port-forwards | fresh forced install failure + cited cancellation and sweep runs | **PASS** |
| 5 | semantic diff hides harmless Kubernetes metadata noise | fresh run, raw evidence compared side-to-side + cited 100-fixture run | **PASS** |
| 6 | expected changes remain visible and stale expectations are reported | fresh, two full `kyverno-istio-upgrade` runs | **PASS** |
| 7 | first divergence says unknown when evidence cannot prove it | fresh (`observed` half, live) + test-cited (`Unknown` half) | **PASS** |
| 8 | HTML opens offline | fresh, both of this pass's reports | **PASS** |
| 9 | JSON validates against stable schema | fresh, in-repo validator run against the live documents | **PASS** |
| 10 | `reproduce` rejects modified fixture hash | fresh, packaged binary, reject-then-accept | **PASS** |
| 11 | GitHub Action preserves exit code and artifacts | **OPERATOR** — no remote here; in-repo job design + assertions cited | **OPERATOR** |
| 12 | Kyverno recipe works on its declared combinations | fresh (two lab runs on 1.35.8) + cited certification test | **PASS** |
| 13 | Istio admission recipe works on its declared combinations | fresh certification test, all three certified versions | **PASS** |
| 14 | Istio Gateway recipe works on its declared combinations | fresh twice — packaged binary on 1.36.4, and the certification test over all three | **PASS** |
| 15 | NGINX Gateway Fabric recipe works on its declared combinations | cited certification test, all three certified versions | **PASS** |
| 16 | legacy ingress-nginx migration example is clearly labeled legacy | fresh inspection of recipe, docs, capability, example | **PASS** |
| 17 | all latest-three-Kubernetes core combinations pass | cited three-minor dogfood + cited install smoke | **PASS** |

### 1. `doctor` tells a new user exactly which prerequisite is missing — PASS

Fresh, both directions, packaged binary.

Intact host, `admissionlab doctor` → **exit 0**:

```text
  kind: found (v0.33.0)
  kubectl: found (v1.32.11) - kubectl client v1.32.11 is 3 Kubernetes minor
    versions away from Admission Lab's supported range (1.37, 1.36, 1.35); ...
  helm: found (v3.20.0)
  docker: found (29.4.1)
  docker daemon: reachable

All required prerequisites are met.
```

The kubectl line is the skew *warning* doctor is supposed to raise, and it does
not make the run fail — which is itself the row's other half: a warning is not
reported as a missing prerequisite.

Same binary, `env -i PATH=<dir with kubectl/helm/docker symlinks only>` so only
`kind` is unreachable → **exit 2**:

```text
  kind: NOT FOUND (unknown) - `kind` was not usable: failed to spawn
    `kind version`: No such file or directory (os error 2)
  kubectl: found (v1.32.11) - ...
  helm: found (v3.20.0)
  docker: found (29.4.1)
  docker daemon: reachable

Some required prerequisites are missing; see above.
```

The one missing tool is named, on its own line, marked `NOT FOUND`, with the
other four still reported found so the reader can see the check ran.

### 2. invalid config fails before creating clusters — PASS

Fresh. A copy of `examples/admission-basic` with `candidate:` misspelled
`candiate:`, run through the packaged binary → **exit 2** in under a second:

```text
admissionlab: failed to load lab configuration: failed to parse lab
configuration at <...>/admissionlab.yaml: unknown field `candiate`, expected
one of `apiVersion`, `kind`, `baseline`, `candidate`, `fixtures`, `policy`,
`expectationsFile`, `gateway`, `migration` at line 102 column 1
```

The error names the offending field, the line and column, and every field that
would have been legal. `kind get clusters` → `No kind clusters found.`, and
`docker ps -a --filter name=adlab` → 0 containers: nothing was created.

### 3. `--keep-clusters` prints exact cleanup commands — PASS

Fresh. `admissionlab test examples/admission-basic/admissionlab.yaml
--report-dir <dir> --keep-clusters` → exit 0, run `ddd01df6-7f14-…`:

```text
Clusters preserved (--keep-clusters was set); nothing was deleted.
  baseline cluster "adlab-baseline-ddd01df6-7f1"
    kubeconfig: /tmp/admissionlab-runs/ddd01df6-…/kubeconfigs/baseline.kubeconfig
    delete with: kind delete cluster --name adlab-baseline-ddd01df6-7f1
  candidate cluster "adlab-candidate-ddd01df6-7f1"
    kubeconfig: /tmp/admissionlab-runs/ddd01df6-…/kubeconfigs/candidate.kubeconfig
    delete with: kind delete cluster --name adlab-candidate-ddd01df6-7f1
```

`kind get clusters` afterwards listed both. The two printed lines were then run
verbatim, with no edit: both reported `Deleted nodes: [...]` and
`kind get clusters` returned `No kind clusters found.` The commands are exact,
not approximate — which is the whole claim.

### 4. normal failure cleans all clusters/port-forwards — PASS

Fresh failure, and cited evidence for the paths a cheap failure cannot reach.

*Fresh (install failure, exit 4).* A lab whose only component applies a manifest
the API server rejects. Run `5a460cb6-e9ac-…` → **exit 4** in 10.9 s:

```text
admissionlab: both stacks failed to install: baseline: component "broken"
  failed to install: ... `kubectl apply --server-side=false -f ...` exited with
  exit status: 1; candidate: ...
admissionlab: wrote <report-dir>/diagnostics.json.
admissionlab: baseline and candidate clusters deleted.
```

`diagnostics.json` written (before cleanup, with each side's cluster failure
bundle), **no `result.json`** — the run reached no verdict and does not pretend
to. Afterwards: `kind get clusters` → none; `docker ps -a --filter name=adlab`
→ none; `pgrep -af port-forward` → nothing but the checking shell itself.

*Cited (cancellation).* Task 9.6's real-signal suite, log `real2.log` (19:13
+0900, the working tree committed minutes later as `5225e09`, "cleanly cancel
and tear down lab runs"): three `cancellation.rs` tests that send genuine signals to
a genuine child — a single SIGINT during setup and after install (both tear
down and leave no cluster), and a double SIGINT (which exits at once, prints
both `kind delete cluster --name …` lines, and whose clusters the sweep then
deletes: `verify-cleanup: PASS — swept every adlab-* cluster left by the
interrupted run`). `3 passed; 0 failed`.

*Cited (leak sweep).* `./scripts/verify-cleanup.sh` at 21:04 — 10 create/delete
cycles, "no `adlab-*` cluster ever survived an iteration".

*Port-forwards.* The only stage that starts one is the Gateway suite. The cited
Task 10.1 Gateway smoke (row 14) ran one to completion and left nothing behind;
this pass ended with no `kubectl port-forward` process on the host.

### 5. semantic diff hides harmless Kubernetes metadata noise — PASS

Fresh, with the noise shown to be present rather than assumed absent. From the
run in row 3 (two bare 1.36.4 clusters, two fixtures), the raw per-side evidence
under `/tmp/admissionlab-runs/ddd01df6-…/raw/` differs between sides:

```text
baseline  metadata.uid 7a6711fb-3bba-400a-a74d-8ff272b1d9df
candidate metadata.uid ecc4bc56-a21d-4b19-bf9e-ef5e332766a7
(and per-side request timings: total_latency 5 ms vs 8 ms)
```

and the comparison of the same two fixtures reports:

```text
Summary  2 fixtures
  identical    2 ... critical 0 ... inconclusive 0
```

and in `result.json` both fixtures are `"bucket": "identical"` with `changes`
empty, `policy.changes` empty, and no `firstDivergence`. The noise was really
there and was really not reported.

The rule behind it is `admissionlab_normalize::built_in_rules` —
`/metadata/uid`, `/metadata/resourceVersion`, `/metadata/creationTimestamp`,
`/metadata/managedFields`, the `kubectl.kubernetes.io/last-applied-configuration`
annotation, plus name-keyed sorting of `/spec/containers` and `/spec/volumes` —
each documented as a field *the API server* populates, explicitly drawn so that
nothing a webhook could set is stripped.

Cited at scale: `benchmark-alpha.sh` at 21:06 replayed **100 fixtures per side**
across two independently created clusters and reported `identical 100`,
`critical 0` — 100 objects' worth of server-populated metadata, zero false
findings.

### 6. expected changes remain visible and stale expectations are reported — PASS

Two fresh full runs of `examples/kyverno-istio-upgrade` (Kyverno 3.9.0 + Istio
1.30.4 on both sides, Kubernetes 1.35.8, ten fixtures), packaged binary.

*Run A — as shipped.* Run `500d77fb-f7da-…` → **exit 1** in 99.9 s:

```text
Summary  10 fixtures
  identical 6   expected 3   warnings 0   critical 1   inconclusive 0

Critical  1
  fixtures-regression-pod-init-container-… [alpha-audit-init]
    init_container_removed at /spec/initContainers/0
    first divergence [observed]: Webhook `mutate.kyverno.svc-fail` … ran in
    round 0 at index 5 on both sides, and was observed to mutate the object on
    the baseline side but not on the candidate side.
```

The expected changes are still in the report, still graded, and still say what
they are. `result.json` carries six `"expected": true` entries — three
`image_changed` on container `app` (`registry.k8s.io/pause:3.9` →
`registry.k8s.io/pause:3.10`), each still `"severity": "critical"`, and three
`webhook_invocation_changed` warnings — plus two *unexpected* changes (a
critical `init_container_removed` and a `webhook_invocation_changed` warning).
The unexpected critical is what makes `policy.disposition` `"fail"` and the exit
1; the six expected ones are still printed, still graded, and no longer a reason
to fail. `report.html` renders each of them inline:

```text
critical expected image_changed · app · /spec/template/spec/containers/0/image
  baseline "registry.k8s.io/pause:3.9"  candidate "registry.k8s.io/pause:3.10"
```

`staleExpectations: []` on the shipped file. (The terminal renderer summarizes
them as the `expected 3` *fixture* bucket rather than listing the six changes
one by one; the HTML and JSON reports list every one. Noted, not a finding — the
row asks that they remain visible, and they do.)

*Run B — one deliberately unmatchable expectation.* A scratch copy of the same
example with one extra entry (`id: task-10-2-never-matches`, `fixtures:
"no-such-fixture-*"`, `kind: volume_added`) → **exit 1**, same verdict as run A
(the stale entry changes no severity), and:

```text
Stale expectations  1
  task-10-2-never-matches: no change of kind volume_added matched fixtures
  glob "no-such-fixture-*"
```

present in the terminal output, in `report.html`, and in `result.json`'s
`policy.staleExpectations`, with the reason stating exactly why it matched
nothing.

### 7. first divergence says unknown when evidence cannot prove it — PASS

Half fresh, half test-cited, and the split is stated rather than blurred.

*Fresh — that a proven divergence claims `observed`.* Every one of the four
fixtures in run A that has a first divergence records
`"confidence": "observed"`, and the sentence it prints names the webhook, the
configuration, the round, the index, and what was seen on each side ("was
observed to mutate the object on the baseline side but not on the candidate
side"). No claim is made beyond what the trace shows.

*Test-cited — that an unprovable one says `Unknown`.* Producing a live
`Unknown` needs a cluster whose audit evidence is deliberately unavailable, so
the property is pinned by tests rather than by a contrived lab run. In
`crates/admissionlab-diff/tests/divergence.rs`:

- `unavailable_evidence_yields_unknown_with_no_position` — no evidence at all
  yields `DivergenceConfidence::Unknown` **and no position**;
- `identical_chains_with_differing_objects_are_unknown` — two chains that look
  identical while the objects differ is exactly the case the enum exists for;
- `partial_evidence_caps_an_absence_at_inferred` and
  `a_mutated_flag_difference_is_only_inferred_under_partial_evidence` — partial
  evidence is capped at `Inferred`, never promoted to `observed`;
- `a_directly_observed_patch_difference_stays_observed_under_partial_evidence` —
  and it is not demoted either.

All green in the gate run (`cargo test --workspace`, 21:02).

### 8. HTML opens offline — PASS

Fresh, on both reports this pass produced:

| Report | Size | `http://` or `https://` | `src=`/`href=` attributes |
| --- | ---: | ---: | ---: |
| `report.html`, admission-basic (row 3) | 23,976 B | 0 | 0 |
| `report.html`, kyverno-istio-upgrade (row 6) | 129,100 B | 0 | 0 |

No external stylesheet, script, font, or image reference of any kind: the file
has no `src`/`href` attributes at all, no `<script>`, no `<img>`, no `@import` —
one inline `<style>` block and the document. There is nothing for a browser to
fetch, so opening it from a `file://` URL on a disconnected machine renders
exactly what a connected one renders.

### 9. JSON validates against stable schema — PASS

Fresh, and the method is stated exactly because "validates" deserves it. There
is no `jsonschema` package installable in this environment, so a second
validator was not invented. Instead the **repository's own** validator — the
`validate`/`resolve`/`type_matches` functions from
`crates/admissionlab-report/tests/result_schema.rs` — was copied verbatim into a
scratch binary so it could be pointed at a live file rather than only at the
golden.

Three checks, in order:

1. *The in-repo chain, unchanged.* `cargo test -p admissionlab-report --test
   result_schema --test stable_schema --test json` at this commit: 10 + 5 + 21
   tests green, including `schema_matches_checked_in_file` (the generated schema
   equals `schemas/result-v1.json`), `the_golden_validates_against_the_generated_schema`,
   `the_stable_schema_has_no_wire_change_from_v1beta1`, and
   `the_semantic_change_wire_strings_are_frozen`.
2. *The relocated validator agrees on the golden.*
   `schemacheck schemas/result-v1.json testdata/golden/result-v1.json` → `VALID`.
3. *The live documents.* Both `result.json` files this pass produced —
   admission-basic (row 3) and kyverno-istio-upgrade (row 6, ten fixtures, six
   expected changes, a first divergence, a stale expectation) — validate against
   `schemas/result-v1.json`: `VALID`, zero errors.

Negative control, so "VALID" is known to mean something: the same live document
with one property added and `summary` removed is rejected with exactly two
errors — `$.bogusProperty: not described by the schema` and
`$.summary: required by the schema but absent`.

### 10. `reproduce` rejects modified fixture hash — PASS

Fresh, reject-then-accept, packaged binary. A scratch copy of
`examples/admission-basic` was run normally (exit 0, run `725adc03-296e-…`), one
byte of `fixtures/configmap-settings.yaml` was flipped, and the recorded run was
reproduced:

```text
admissionlab: the source tree no longer matches the recorded run, so this would
not be a reproduction:
  changed  <...>/row10/fixtures/configmap-settings.yaml
    expected sha256 80c207b1b46232574b80fb64363e4cffdf6ec101e1e0afc28592ce4e8f537ced
    actual   sha256 4ce81bc7dedb6ecf2eaa74367dba0b4fee87240a25502bf829e3c7f2c1d2b2dd
admissionlab: restore the recorded revision of this lab (for example, check out
the commit the run was made from) and try again.
```

**Exit 2**, the file named, both hashes shown, and `kind get clusters` empty —
the refusal happens before anything is created. The byte was then restored (file
back to `80c207b1…`) and the same command reproduced the run to completion:
exit 0, `Result: pass`, both clusters deleted. The check rejects a modified
fixture and accepts an unmodified one.

(No repository file was modified for this row: the flipped byte was in a scratch
copy outside the work tree.)

### 11. GitHub Action preserves exit code and artifacts — OPERATOR

There is no GitHub remote in this environment, so the half of this row that only
a real run can prove — that artifacts are *retrievable through the GitHub API*
after a failed job, and that the summary renders on a real pull request —
remains an operator item (Task 10.1 Step 5). What is on record instead:

- `.github/workflows/integration.yml` contains a dedicated job that runs the
  action against `examples/kyverno-istio-upgrade` (a lab designed to fail) with
  `continue-on-error: true`, then asserts three things: `steps.lab.outcome ==
  'failure'`, `steps.lab.outputs.exit-code == '1'` (not merely non-zero — 2–6
  would mean the lab never reached a verdict), and that `result.json`,
  `report.html`, `github-summary.md`, and `run-manifests/<run-id>.json` all
  exist and that the summary carries `Admission Lab: FAIL`. A second job does
  the same for a passing lab, and both end by asserting no `adlab-*` cluster
  leaked.
- The action itself (`.github/actions/admissionlab/action.yml`) records
  `exit-code=${status}` from `admissionlab test`, carries `if: always()` on the
  summary, manifest-collection, and `actions/upload-artifact@v4.6.2` steps, and
  ends with an unconditional final step that re-exits with the recorded status
  (rejecting an empty or non-numeric one).
- The inputs those assertions read are true at this commit:
  `examples/kyverno-istio-upgrade` exits 1 and leaves `result.json`,
  `report.html`, and its `run.json` on disk (row 6).

The action's own behavior — exit code, files on disk, summary content — is
therefore covered; the platform behavior around it is not, and is not claimed.

### 12. Kyverno recipe works on its declared combinations — PASS

Declared: `kyverno 3.9.0`, certified on Kubernetes **1.35.8** only (`perCommit`;
`compatibility/recipes.yaml`, narrower than Admission Lab's own matrix because
the chart's documented range is 1.33–1.35).

Fresh: runs A and B of row 6 each installed Kyverno 3.9.0 from `kyverno/kyverno`
into namespace `kyverno` on **two** 1.35.8 clusters, gated on all four
Deployments, all four webhook configurations, and every `ClusterPolicy` that
side installs reaching `Ready` (three on the baseline, two on the candidate),
then replayed ten fixtures through the real mutating webhook and attributed a
divergence to `mutate.kyverno.svc-fail` — twice, through the packaged binary, at
this commit.

Cited: `kyverno_recipe_installs_and_enforces_fixture_policies` (the recipe's own
certification test, which drives the certified version set) green in 90.7 s at
20:31, tree `f7e76af`.

### 13. Istio admission recipe works on its declared combinations — PASS

Declared: `istio 1.30.4`, certified on **1.35.8** (`nightly`), **1.36.4**
(`perCommit`), **1.37.0** (`nightly`).

Fresh: `cargo test -p admissionlab-recipes --test istio_recipe -- --ignored` at
this commit — `istio_recipe_installs_and_injects_sidecar_for_every_certified_kubernetes_version`
**green in 121.4 s** (`1 passed; 0 failed`). It installs `istio/istiod` 1.30.4
in a disposable cluster **per certified version**, gated on `istiod` becoming
Available, and asserts a real sidecar injection on each; the loop attempts every
listed version even after a failure, so a green result covers all three (no
`ADMISSIONLAB_CERTIFY_KUBERNETES` narrowing was in the environment). Faster than
the ~222 s the test's own documentation records because the node and `istiod`
images were already warm here. `kind get clusters` empty afterwards, no
`adlab-*` container left.

This is the one row for which no fresh evidence existed at the start of the
pass: the recipe certification tests are `#[ignore]`d, so the gate run
(`cargo test --workspace`) skips them, and no earlier log in this release cycle
covered it. It was run rather than cited for that reason.

Corroborating: the two `kyverno-istio-upgrade` runs in row 6 installed the same
chart at the same pin into `istio-system` on four 1.35.8 clusters, gated on
`istiod` availability and the `istio-sidecar-injector` webhook configuration
being present, with Istio in the observed admission chain for every fixture.

### 14. Istio Gateway recipe works on its declared combinations — PASS

Declared: `istio-gateway 1.30.4`, certified on **1.35.8** (`weeklyRelease`),
**1.36.4** (`perCommit`), **1.37.0** (`weeklyRelease`).

Fresh (1.36.4, the per-commit tier): Task 10.1's install smoke ran
`examples/gateway-istio` from the **unpacked RC tarball** at 21:14 —
gateway-api-crds 1.5.1 + istio-gateway 1.30.4 on both sides, both sides
converged (263 ms / 264 ms), a real HTTP probe answered `200 from echo-b` on the
baseline, and the seeded `ReferenceGrant` regression caught on the candidate:
`backend_resolution_changed`, `resolved_refs_condition_changed`
(`ResolvedRefs=True (ResolvedRefs)` → `ResolvedRefs=False (RefNotPermitted)`),
`traffic_status_changed`, plus a `gateway.probe_skipped` diagnostic explaining
why no probe was sent rather than inventing a result. Exit 1 in 172 s, both
clusters deleted.

Fresh (all three versions): `cargo test -p admissionlab-recipes --test
istio_gateway_recipe -- --ignored` at this commit —
`istio_gateway_recipe_routes_real_traffic_for_every_certified_kubernetes_version`
**green in 197.1 s**, three iterations of the two scenarios
(`istio-same-namespace`, `istio-cross-namespace`), each reconciling in ~270 ms
and each answering a real probe: six probes, all `status 200`, three from
`echo-a` and three from `echo-b`. No cluster left behind.

This test was re-run rather than cited: the earlier green run of it
(`recipe.log`, 210.7 s, 12:53) predates three later commits to
`crates/admissionlab-gateway/src` (`dbfa76b`, `915deac`, `4c7b25f`), so citing
it would have been a claim about code this candidate no longer ships.

### 15. NGINX Gateway Fabric recipe works on its declared combinations — PASS

Declared: `nginx-gateway-fabric 2.6.7`, certified on **1.35.8** (`nightly`),
**1.36.4** (`perCommit`), **1.37.0** (`nightly`).

Cited: `nginx_gateway_recipe_routes_real_traffic_for_every_certified_kubernetes_version`
green in 314.7 s at 20:26 (tree `f7e76af`), and the log shows the full
three-version loop — three iterations of the three scenarios
(`nginx-same-namespace`, `nginx-cross-namespace`,
`nginx-infrastructure-override`), each converging in ~270 ms and each answering
a real probe (`status 200 backend echo-a` / `echo-b`, including the
`ClusterIP`-typed infrastructure override). A second, version-filtered run of
the same test on 1.35.8 alone was green in 107.8 s at 20:21.

Not re-run in this pass, and the citation is safe to make: `git diff
f7e76af..6ed5147` over `recipes/nginx-gateway-fabric/`,
`crates/admissionlab-gateway/`, `crates/admissionlab-echo/`, and
`crates/admissionlab-recipes/` is **three `Cargo.toml` version lines and nothing
else**. That run is therefore about the same code this candidate ships, and the
row's cost (six clusters, five minutes) bought no new information.

### 16. legacy ingress-nginx migration example is clearly labeled legacy — PASS

Fresh inspection. The label is not in one place that could be missed:

- **The recipe's name.** `ingress-nginx-legacy` — the directory, the
  `Recipe::name`, and every reference to it.
- **`recipes/ingress-nginx-legacy/README.md`**, first heading after the title:
  `> ## ⚠️ THE UPSTREAM PROJECT IS RETIRED AND ITS REPOSITORY IS ARCHIVED`,
  quoting upstream's own "no further releases, no bugfixes, no updates to
  resolve any security vulnerabilities" and "if you are not already using
  ingress-nginx, you should not be deploying it", then stating Admission Lab's
  position: the recipe exists so a team can migrate *away*, and "nothing
  installs this recipe unless a lab file names it."
- **`recipes/ingress-nginx-legacy/recipe.yaml`**, first line:
  `# LEGACY / ARCHIVED UPSTREAM.`
- **Machine-readable.** It claims the `legacyIngress` capability and
  deliberately **not** `admission`; `metadata_tests::the_recipe_is_marked_legacy_in_a_way_a_program_can_read`
  and `the_legacy_recipe_is_deliberately_not_a_builtin` pin both.
- **`docs/recipes.md`**, the recipe table: "**The upstream project is retired
  and its repository archived.** … its presence here is not a recommendation to
  run it", and the certification column reads "**migration testing only**".
- **`README.md`**: "`ingress-nginx-legacy` is an **archived** upstream, admitted
  only so migrations …", and the matrix row is annotated
  `weeklyRelease (migration only)`.
- **The example** (`examples/ingress-to-gateway/admissionlab.yaml`) points at
  that README for the retirement dates before it installs anything.

Also cited, for the recipe still doing its job:
`ingress_nginx_legacy_recipe_routes_and_denies_for_every_certified_kubernetes_version`
green in 54.0 s at 20:27 — real traffic (`status 200 backend echo-a`) and a real
admission denial quoted verbatim from the API server.

### 17. all latest-three-Kubernetes core combinations pass — PASS

Cited, twice over.

*Task 9.7's three-minor dogfood* (20:32, tree `f7e76af`): `examples/admission-basic`
run with **both** sides pinned to each supported minor in turn — the Tier-2
pattern — using the release-profile binary built from that tree:

| Kubernetes | Verdict | Fixtures | Elapsed | Clusters after |
| --- | --- | ---: | ---: | --- |
| 1.35.8 | pass (exit 0) | 2 identical, 0 critical | 10.55 s | none |
| 1.36.4 | pass (exit 0) | 2 identical, 0 critical | 10.91 s | none |
| 1.37.0 | pass (exit 0) | 2 identical, 0 critical | 11.27 s | none |

*Task 10.1's install smoke* (21:11–21:14, this commit, unpacked RC tarball):
`examples/admission-basic` exit 0 on 1.36.4 with `admissionlabVersion
1.0.0-rc.1` in its run manifest, and `examples/gateway-istio` exit 1 catching
its seeded regression on 1.36.4.

*And the suite itself:* `cargo test --workspace` at 21:02 — 1585 passed, 0
failed — with the per-version certification tests covering 1.35.8/1.36.4/1.37.0
recorded in rows 12–15.

---

## Release blockers

Any of these, observed at any point, blocks the release regardless of how the
rows above scored (ROADMAP, Phase 9 exit gate):

- any secret sentinel appears in a user-facing report;
- any known false first-divergence claim;
- leaked cluster/port-forward child in normal/cancellation paths;
- stable schema golden mismatch without explicit migration;
- red core suite on any of the latest three supported Kubernetes minors;
- canonical admission/Gateway/migration demos fail to catch their seeded
  regressions;
- unexplained flaky weighted-routing or Gateway convergence test.

**Observed in this pass: none.** Each was looked for rather than assumed absent:

- *Secret sentinels.* `admissionlab-report`'s `security_sentinels.rs` and the
  redaction suites are green in the gate run (21:02), and the live capture check
  at 20:33 reports `audit policy check: 1300 audit events, 481 carrying a
  requestObject, none for a secrets resource, none carrying a responseObject,
  and no trace of the probe Secret's canary value`.
- *False first divergence.* Every divergence this pass produced (four, row 6)
  is `observed`, each naming the webhook, configuration, round, and index it was
  read from; the `Unknown`/`Inferred` boundaries are pinned by the tests in
  row 7.
- *Leaked cluster or port-forward.* Every run in this pass — pass, policy
  failure, install failure, refused reproduce, four certification runs — ended
  with `kind get clusters` empty and no `adlab-*` container; no
  `kubectl port-forward` process survived; cited cancellation and sweep evidence
  in row 4.
- *Schema.* Goldens and the frozen-surface tests green (row 9), no migration
  pending.
- *Core suite on the latest three minors.* Green (row 17).
- *Seeded regressions, all three canonical demos, fresh.* Admission —
  `examples/kyverno-istio-upgrade` exit 1 twice (row 6). Gateway —
  `examples/gateway-istio` exit 1 through the packaged binary, plus the
  certification test over all three versions (row 14). Migration —
  `cargo test -p admissionlab-cli --test migration_demo -- --ignored` at this
  commit: **3 passed / 0 failed in 702.5 s**, covering
  `the_migration_demo_reports_all_three_behaviors_and_fails_the_run` (exit 1 in
  172.1 s), `the_same_configuration_produces_the_same_migration_finding_twice`
  (exit 1 in 171.9 s, identical finding), and
  `the_demos_artifacts_carry_no_key_material` (exit 1 in 172.7 s). No cluster
  left behind by any of them.
- *Flakiness.* No test was retried to turn it green and no run in this pass
  needed a second attempt; the determinism test above ran the same migration
  configuration twice and got the same finding both times, and the weighted
  routing/Gateway convergence figures held at ~270 ms across every reconciliation
  in rows 14 and 15.

### Non-blocking observation (post-v1 issue candidate)

Not a blocker, not one of the seventeen rows, and **not fixed here** — recorded
so it is not rediscovered as a surprise:

- **An install failure does not show the tool's own error text.**
  `InstallError::CommandFailed`'s `Display` renders the component, the argv, and
  the exit status, but not the captured `stderr` (which the variant does carry),
  and it has no `#[source]`, so `pipeline::install::render_chain` has nothing
  further to append. In the row 4 run the user is told
  `` `kubectl apply … -f bad.yaml …` exited with exit status: 1 `` and never
  learns *why* Kubernetes rejected the manifest; the text is not in
  `diagnostics.json` (`"diagnostics": []`) or in the run workspace either.
  `docs/troubleshooting.md`'s "Exit 4 — `helm` failed" section says "the whole
  error chain is rendered, so the `helm` exit status **or the Kubernetes
  validation message** reaches you", which overstates what the code does today.
  Repro: a `manifests` component whose file the API server rejects (for example
  a Deployment with `replicas: "not-a-number"`), any Kubernetes version.
  Either surfacing a bounded `output_tail(&stderr)` — the rule
  `admissionlab_core::process` already documents for exactly this — or
  narrowing the doc sentence would settle it; that is a Task 10.3 judgement, not
  this pass's.

  **Task 10.3 disposition:** the doc sentence was narrowed to describe what the
  code does today (re-run the printed command, or read the spilled log), and
  surfacing a bounded stderr tail inline is a **post-v1 patch-release
  candidate** — a `Display`/`#[source]` change in the RC stabilization window
  would alter error text several installer tests assert on, which is exactly
  the class of non-blocker churn Task 10.3's rule exists to keep out. No other
  blocker was found, so no code changed and no gate rerun was required beyond
  the standing green results above.

---

## What only an operator can sign off

These need infrastructure a local machine does not have. They are listed here
so a PR that cannot tick them says so explicitly rather than leaving the row
blank:

- **Fresh Linux CI runner and macOS runner install** of the RC artifact, then
  `doctor`, the basic admission example, and the Istio Gateway example on each
  (Task 10.1 Step 4). A local unpack-and-run of the same tarball is a stand-in
  for the Linux half only, and proves nothing about macOS. *Status: the Linux
  stand-in was done (rows 1, 14, 17 — unpacked tarball, empty directory, all
  three commands); macOS is untested.*
- **A real repository PR driven by the RC GitHub Action**, verifying the
  policy failure surfaces and the artifacts upload (Task 10.1 Step 5).
  *Status: row 11 — the action's own behavior is asserted in-repo; the platform
  behavior around it is unverified here.*
- **Sigstore signature verification** (`cosign verify-blob`) — keyless signing
  needs the release workflow's GitHub OIDC identity, so neither the signature
  nor its certificate exists before the tag is pushed.
- **The other three release targets.** `scripts/verify-release.sh` builds and
  checks the host target only, by design; the workflow's native runners cover
  the rest. *Status: `x86_64-unknown-linux-gnu` built, packaged, checksummed,
  and smoke-tested at this commit; the other three untested.*
