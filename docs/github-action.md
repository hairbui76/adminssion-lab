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
  Pin them with `kubectl-version`/`helm-version` (each with its
  checksum) if you need exact versions or are on a self-hosted runner
  that lacks them.
- **No secrets.** The action needs no token, no registry credentials,
  and no `packages: read`. `contents: read` — what `actions/checkout`
  needs — is enough for the whole workflow.
- **Time and two clusters.** Every run creates two disposable `kind`
  clusters and deletes them. A lab with no components takes a couple of
  minutes; one that installs real vendor charts on both sides takes well
  over ten. Set `timeout-minutes` accordingly.

---

## Inputs

| Input | Required | Default | What it does |
| --- | --- | --- | --- |
| `config` | yes | — | Path to the lab configuration (`admissionlab.yaml`), relative to the workspace or absolute. |
| `version` | no | *(empty)* | Admission Lab release to install, e.g. `0.1.0` (a leading `v` is accepted). Requires `sha256`. Empty selects the from-source mode below. |
| `sha256` | no | *(empty)* | SHA-256 of that release's `admissionlab-<version>-x86_64-unknown-linux-gnu.tar.gz`. **Required whenever `version` is set.** |
| `repository` | no | *(the action's own repository)* | `owner/repo` to download the release from. The default is right whenever the action is referenced as `owner/admission-lab/.github/actions/admissionlab@vX`. |
| `report-dir` | no | `./admissionlab-artifacts` | Where the run's artifacts are written. Created if absent. |
| `artifact-name` | no | `admissionlab-artifacts` | Name of the uploaded workflow artifact. |
| `upload-artifacts` | no | `true` | Set `false` to skip the upload step and collect `report-dir` yourself. |
| `artifact-retention-days` | no | `14` | Retention for the uploaded artifact. |
| `kind-version` | no | `v0.33.0` | The `kind` version to install — the one this project validated its cluster lifecycle against, and the one `compatibility/kubernetes.yaml`'s node-image digests were captured from. |
| `kind-sha256` | no | *(checksum of the default version)* | Required if you change `kind-version`: the default checksum is for the default version and nothing else, so a mismatched pair fails the download rather than installing something unverified. |
| `kubectl-version` | no | *(empty — use the runner's)* | `kubectl` to install, e.g. `v1.36.4`. Requires `kubectl-sha256`. |
| `kubectl-sha256` | no | *(empty)* | From `https://dl.k8s.io/release/<version>/bin/linux/amd64/kubectl.sha256`. |
| `helm-version` | no | *(empty — use the runner's)* | `helm` to install, e.g. `v3.19.0`. Requires `helm-sha256`. |
| `helm-sha256` | no | *(empty)* | From `https://get.helm.sh/helm-<version>-linux-amd64.tar.gz.sha256sum`. |

### Outputs

| Output | What it is |
| --- | --- |
| `exit-code` | `admissionlab test`'s own exit code, as a string. Available even though the action itself fails on a non-zero code, so a workflow can branch on *which* failure without parsing anything. |
| `report-dir` | Absolute path of the directory the artifacts were written to. |

---

## Artifacts

One workflow artifact, uploaded **with `if: always()`** so it exists on a
failing run as well as a passing one. Its contents:

| File | When | What it is |
| --- | --- | --- |
| `result.json` | the run reached a verdict | The machine-readable result: every fixture, every graded change, both sides' captured admission outcomes, and the environments they ran in. Schema `admissionlab.io/result/v1alpha1` (experimental until Beta). |
| `report.html` | the run reached a verdict | The standalone report page — per-fixture drill-down with the full webhook trace and every patch. No external scripts, no network. |
| `diagnostics.json` | the run failed at or after installation | The stage that failed, the failure, and every diagnostic collected up to that point. Written *before* cleanup runs. |
| `github-summary.md` | always, unless the process died before writing anything | The same Markdown the action appended to the job summary. |
| `run-manifests/<run-id>.json` | a run workspace existed | The run manifest: tool versions, node images, and configuration digests — what `admissionlab reproduce` needs. |

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
> Run `01K...` — result schema `admissionlab.io/result/v1alpha1`
> (experimental; stable at Beta).
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
  requires `sha256`; `kind` carries a pinned checksum in the action and
  requires a new one if you change its version; `kubectl` and `helm` are
  only downloaded when you supply both a version and a checksum. Every
  download is `curl --fail` followed by `sha256sum --check --strict`.
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
  `kind-linux-amd64.sha256sum`. `kubectl` comes from `dl.k8s.io` and
  `helm` from `get.helm.sh`; for both, you supply the checksum, so the
  action is not trusting the download origin to vouch for itself.
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

Use the action twice, with different `config`, `report-dir`, and
`artifact-name` values. Each invocation is one `admissionlab test`, and
the *first* non-zero exit ends the job unless the step carries its own
`continue-on-error` — which is a workflow-level key and works normally
there, unlike inside a composite action.
