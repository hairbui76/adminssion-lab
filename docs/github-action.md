# The GitHub Action

Admission Lab ships a composite action at
`.github/actions/admissionlab`. It installs a pinned `admissionlab`
binary and pinned tooling, runs **one** `admissionlab test`, publishes
what that run wrote, and exits with that run's exit code.

That list is the whole of it. The action contains no regression logic:
nothing in it reads `result.json`, counts a finding, compares a version,
or decides what a run meant. Every one of those decisions is made by the
binary, is recorded in the uploaded artifacts, and is summarized in the
job summary by a renderer inside the binary
(`admissionlab_report::render_github_summary`). A workflow that wanted a
different verdict would change its `admissionlab.yaml`, not this action.

---

## Quick start

```yaml
name: Admission Lab

on:
  pull_request:
    paths:
      - "admissionlab.yaml"
      - "expectations.yaml"
      - "fixtures/**"
      - "stacks/**"

permissions:
  contents: read

jobs:
  admission-lab:
    runs-on: ubuntu-latest
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@v7.0.1
      - uses: OWNER/admission-lab/.github/actions/admissionlab@v1
        with:
          config: admissionlab.yaml
          version: "0.1.0"
          sha256: "<the release's SHA256SUMS entry for the linux x86_64 tarball>"
```

A complete, copyable version of this file — with the reasoning in it —
is [`examples/admission-basic/.github/workflows/admissionlab.yml`](../examples/admission-basic/.github/workflows/admissionlab.yml),
next to the smallest lab configuration this project ships.

**Pin both `version` and `sha256`.** `version` on its own is refused: the
action never downloads a binary it cannot verify. See
[Security](#security) for where the checksum comes from and what signs
it.

---

## Requirements

- **A Linux x86_64 runner.** `ubuntu-latest` is the expected one. The
  action checks this first and stops with an explanation otherwise:
  `kind` needs a working Docker daemon, which GitHub's macOS runners do
  not have, and the pinned checksums are for `linux-amd64` artifacts.
- **Docker running on the runner**, which GitHub's `ubuntu-*` images
  provide.
- **`kubectl` and `helm` on `PATH`**, which those images also provide.
  The action does not download either and has no input to pin them: on a
  self-hosted runner, install them yourself. It prints both versions
  before the run, and `admissionlab test` records what it actually found
  in the run manifest.
- **No secrets.** The action needs no token, no registry credentials,
  and no `packages: read`. `contents: read` — what `actions/checkout`
  needs — is enough for the whole workflow.
- **Time and two clusters.** Every run creates two disposable `kind`
  clusters and deletes them. A lab with no components takes a couple of
  minutes; one that installs real vendor charts on both sides takes well
  over ten. Set `timeout-minutes` accordingly.

---

## Inputs

Eight, and the set is frozen for Public Beta. They cover exactly four
concerns: **which lab** to run, **which Admission Lab** to run it with,
**what to do with the artifacts**, and **whether to keep the clusters**.

| Input | Required | Default | What it does |
| --- | --- | --- | --- |
| `config` | yes | — | Path to the lab configuration (`admissionlab.yaml`), relative to the workspace or absolute. |
| `version` | no | *(empty)* | Admission Lab release to install, e.g. `0.1.0` (a leading `v` is accepted). Requires `sha256`. Empty selects the from-source mode below. |
| `sha256` | no | *(empty)* | SHA-256 of that release's `admissionlab-<version>-x86_64-unknown-linux-gnu.tar.gz`. **Required whenever `version` is set.** |
| `repository` | no | *(the action's own repository)* | `owner/repo` to download the release from. The default is right whenever the action is referenced as `owner/admission-lab/.github/actions/admissionlab@vX`; set it when you have vendored the action into a repository that does not itself publish Admission Lab releases. |
| `artifact-name` | no | `admissionlab-artifacts` | Name of the uploaded workflow artifact **and** of the directory under `$GITHUB_WORKSPACE` the reports are written into. Must be a single path segment. |
| `artifact-retention-days` | no | `14` | Retention for the uploaded artifact, in days. A positive whole number; GitHub enforces your repository's own upper limit. |
| `upload-artifacts` | no | `true` | Set `false` to skip the upload step and collect the report directory yourself — its absolute path is the `report-dir` output. |
| `keep-clusters` | no | `false` | Pass `--keep-clusters`, preserving both `kind` clusters. **Refused on GitHub-hosted runners** — see below. |

### Outputs

| Output | What it is |
| --- | --- |
| `exit-code` | `admissionlab test`'s own exit code, as a string. Available even though the action itself fails on a non-zero code, so a workflow can branch on *which* failure without parsing anything. |
| `report-dir` | Absolute path of the directory the artifacts were written to (`$GITHUB_WORKSPACE/<artifact-name>`). Resolved before anything is installed, so it is set even on a run that never reached `admissionlab test`. |

### What is deliberately not an input

The action takes **no input that becomes part of a command line** beyond
the config path: no extra flags, no `args`, no `env`, no pre- or
post-script. A wrapper that accepted arbitrary arguments would be a way
to run something other than the run it reports on.

Four things that used to be inputs, or could plausibly be, are not:

- **`report-dir`.** The report directory is `$GITHUB_WORKSPACE/<artifact-name>`.
  Two inputs that had to agree — and that a job running the action twice
  had to remember to vary *together* — are now one. The absolute path is
  still available, as the `report-dir` output.
- **The `kind` version and its checksum.** Pinned in the action as a
  constant. `compatibility/kubernetes.yaml`'s node-image digests were
  captured from that exact `kind` release, so a caller who changed it
  would be running node images this project never validated while still
  getting a report that names those Kubernetes versions. Moving the pin
  is an Admission Lab release, reviewed alongside the compatibility
  matrix it belongs to.
- **`kubectl` and `helm` versions.** Not downloaded and not pinnable; see
  [Requirements](#requirements).
- **Anything about the fixtures, the policy, or the verdict.** Those live
  in your `admissionlab.yaml` and `expectations.yaml`, which are
  reviewable files in your repository — not in a workflow input a job can
  set differently on one branch.

### `keep-clusters` on hosted runners

`admissionlab test --keep-clusters` preserves both `kind` clusters so
that an operator can `kubectl` into them afterwards. On a GitHub-hosted
runner there is nobody to do that: the runner VM — clusters, Docker
daemon, disk and all — is destroyed when the job ends, so nothing is
preserved, and the only observable effect is that a long job runs out of
disk sooner.

So the action refuses `keep-clusters: true` unless `RUNNER_ENVIRONMENT`
is `self-hosted`, and it fails *closed*: a runner that does not set that
variable is treated as hosted. On a hosted runner, the evidence you keep
is the uploaded artifacts, and reproducing the run locally is what
`admissionlab reproduce` and the uploaded run manifest are for.

---

## Artifacts

One workflow artifact, uploaded **with `if: always()`** so it exists on a
failing run as well as a passing one. Its contents:

| File | When | What it is |
| --- | --- | --- |
| `result.json` | the run reached a verdict | The machine-readable result: every fixture, every graded change, both sides' captured admission outcomes, and the environments they ran in. Schema `admissionlab.io/result/v1` — frozen and additive-only, checked in as [`schemas/result-v1.json`](../schemas/result-v1.json) and described in [`docs/schema-migrations.md`](schema-migrations.md). |
| `report.html` | the run reached a verdict | The standalone report page — per-fixture drill-down with the full webhook trace and every patch. No external scripts, no network. |
| `diagnostics.json` | the run failed at or after installation | The stage that failed, the failure, and every diagnostic collected up to that point. Written *before* cleanup runs. |
| `github-summary.md` | always, unless the process died before writing anything | The same Markdown the action appended to the job summary. |
| `run-manifests/<run-id>.json` | a run workspace existed | The run manifest: tool versions, node images, and configuration digests — what `admissionlab reproduce` needs. Schema `admissionlab.io/run/v1` ([`schemas/run-manifest-v1.json`](../schemas/run-manifest-v1.json)). |

`result.json` and `diagnostics.json` are mutually exclusive by design. A
run that never compared both sides has not earned a verdict, and writing
a `pass` — or a `fail` — for it would be a fabrication; so the failure
path writes diagnostics instead of a half-filled result. The same rule
governs the job summary (below).

The run manifest is copied into the report directory by the action,
because `admissionlab test` writes it into the run *workspace*
(`$TMPDIR/admissionlab-runs/<run-id>/run.json`) rather than the report
directory, and the workspace location is not configurable yet. That copy
is the one place this action knows a path the CLI chose; it reads a path,
never a result. If a future release adds a `--run-root` flag, this is the
step to simplify.

---

## Exit codes

The action's conclusion **is** `admissionlab test`'s exit code. The
numbering is frozen and identical to the CLI's (see the README's own
table):

| Code | Job result | Meaning |
| ---: | --- | --- |
| `0` | success | Passed. Warnings also exit `0` — they are visible in the summary and in `result.json`, and they do not fail your pull request. |
| `1` | failure | The regression policy failed: an unexpected critical change, or a `policy.failOn` category was observed. **This is the case the artifacts exist for.** |
| `2` | failure | Invalid configuration, invalid fixture, or a missing host prerequisite. Nothing was provisioned. |
| `3` | failure | Lab infrastructure failure — a cluster could not be created, the report directory was not writable, or cleanup failed. |
| `4` | failure | A component would not install or never became ready. |
| `5` | failure | A fixture could not be replayed or its evidence could not be written. |
| `6` | failure | An internal Admission Lab error. Please report it. |

### How the exit code survives the artifact upload

Worth knowing before editing the action, because the two requirements
pull against each other: a composite step that exits non-zero ends the
action there, which would skip exactly the summary and upload steps that
a failing run most needs.

So the run step never fails. It captures the CLI's status into a step
output, exits `0`, and the action's **last** step re-exits with that
saved code. The steps in between carry `if: always()`, so they also
survive a failure *before* the run step (a checksum mismatch, a missing
tool) — in which case the last step reports that Admission Lab never ran
and fails, rather than passing a job that tested nothing.

`continue-on-error` would say the same thing in one line and is
deliberately not used: it is a *workflow* step key and not part of the
composite-action step schema, and an action that depended on it would
fail to parse rather than degrade.

---

## The job summary

The action appends one file to `$GITHUB_STEP_SUMMARY`. It does not
compose it — the binary renders it, capped at 128 KiB against GitHub's
1 MiB limit, so appending can never truncate the rest of your job's
summary.

What a reader sees at the top of the pull request's checks page, in
words:

> **## Admission Lab: FAIL**
>
> At least one unexpected critical difference. `admissionlab test` exits 1.
>
> Run `01K...` — result schema `admissionlab.io/result/v1`
> (frozen; additive changes only).
>
> **### Fixtures** — a six-row table: `identical`, `expected`,
> `warnings`, `critical`, `inconclusive`, and the bold **total**. All
> five are always listed, zeroes included, so "no warnings" and
> "warnings were not counted" cannot look alike.
>
> **### Critical findings (1)** — a table of at most ten rows: fixture,
> subject, the change and the object path it happened at, and the first
> divergence with its confidence and the webhook on each side. The
> heading carries the *complete* count, so a capped table cannot be
> mistaken for a short one; anything omitted is stated ("and 490 more
> critical findings — the complete list is in the `result.json`
> artifact").
>
> **### Warnings (0)** — `None.`
>
> **### Full evidence** — what this summary deliberately omits (webhook
> traces, patches, object bodies) and which uploaded artifact has it.

The verdict word is `PASS`, `WARN`, or `FAIL` in plain letters — no
emoji and no color chip, because a summary is also read in email
notifications and by screen readers, and a green circle that renders as
nothing is not a verdict.

A run that never reached a verdict writes a different summary, headed
**`## Admission Lab: NO RESULT`**, which names the stage that failed
(`configuration`, `prerequisites`, `workspace`, `node-image`,
`manifest`, `cluster-creation`, `install`, `capture`, `normalize`, or
`reporting`), quotes the failure, and says where the diagnostics are. It
contains no verdict word at all. If even that file is missing — the
process died before it could write anything — the action appends a short
note saying so and nothing else. **No summary this action produces ever
states an outcome the run did not reach.**

---

## Security

- **Nothing is downloaded unverified.** The Admission Lab release
  requires `sha256`; `kind` is pinned in the action with the checksum
  published in that `kind` release, and neither is a caller input.
  `kubectl` and `helm` are not downloaded at all. Every download is
  `curl --fail` followed by `sha256sum --check --strict`.
- **The checksums have a source.** Admission Lab releases publish a
  `SHA256SUMS` file covering every archive, signed with a keyless
  Sigstore certificate bound to the release workflow's GitHub OIDC
  identity and recorded in the public Rekor transparency log. The
  release notes carry the exact `cosign verify-blob` command. Verify
  `SHA256SUMS` once, then copy the line for the Linux x86_64 tarball
  into `sha256`.
- **`kind` comes from its own release**
  (`github.com/kubernetes-sigs/kind/releases/download/<version>/kind-linux-amd64`),
  checksummed against the value published in that release's
  `kind-linux-amd64.sha256sum`. It is the only thing the action fetches
  other than the Admission Lab release itself.
- **The action needs no secrets** and requests no permissions of its
  own. It reads public downloads and writes the job summary, the report
  directory, and one workflow artifact.
- **Reports are redacted before they are written.** Secret data,
  authorization headers, private keys, and credential-like environment
  variable names never reach `result.json`, `report.html`, or the job
  summary. `docs/security.md` is the full statement of what is redacted
  and what is not — read it before uploading artifacts from a cluster
  whose stack you do not control.
- **Third-party charts run in the disposable clusters**, not on the
  runner's host beyond Docker itself. `docs/security.md` describes that
  trust boundary.

---

## From-source mode (this repository's own CI)

Omitting `version` selects a different first step: instead of
downloading a release, the action runs `cargo build --release --locked
-p admissionlab-cli` in the checked-out workspace and uses that binary.

That mode exists so this repository can test the action *in the pull
request that changes it* — a pinned release download would test a
previously published binary instead — and it is what
`.github/workflows/integration.yml`'s `action` job uses against
[`examples/admission-basic`](../examples/admission-basic/). It is not
what a downstream repository should do: it needs the Admission Lab
source checked out, needs a Rust toolchain, and takes minutes longer.
The two modes never fall back to one another; a pinned download that
fails to verify stops the run.

---

## Running more than one lab in a job

Use the action twice, with different `config` and `artifact-name`
values. `artifact-name` is also the report directory, so varying it is
enough to keep the two runs' artifacts apart — there is no second path
input to remember. Each invocation is one `admissionlab test`, and the
*first* non-zero exit ends the job unless the step carries its own
`continue-on-error` — which is a workflow-level key and works normally
there, unlike inside a composite action.

---

## How this action is tested

`.github/workflows/integration.yml` runs it on every pull request that
touches `.github/actions/**`, twice, against two real two-cluster labs:

- **`action`** runs [`examples/admission-basic`](../examples/admission-basic/),
  which is designed to pass, and asserts that `result.json`,
  `report.html`, `github-summary.md` and a run manifest all exist.
- **`action-failure`** runs [`examples/kyverno-istio-upgrade`](../examples/kyverno-istio-upgrade/),
  which finds a real regression and exits `1`. The step carries
  `continue-on-error: true` so the job survives it, and the next step
  asserts that the step's `outcome` was `failure`, that `exit-code` was
  `1`, that every artifact above exists **anyway**, and that
  `github-summary.md` carries the `FAIL` verdict rather than a cheerful
  one. That is the `if: always()` guarantee, observed on the only kind of
  run that can observe it.

Two things those jobs cannot prove, and that are checked by hand before a
release rather than claimed here: that the pinned-**release** download
branch works (it needs a published release to download, and it is built
so that a checksum mismatch stops the run rather than falling back to
anything), and that the uploaded artifacts and rendered job summary look
right *on a real pull request* in a downstream repository.
