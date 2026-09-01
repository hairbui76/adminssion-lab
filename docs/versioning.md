# Versioning: what a version number promises

Admission Lab publishes several surfaces that outlive any one run — a
configuration file somebody checked into their repository, a `result.json` a CI
job parses, a `run.json` `admissionlab reproduce` reads a year later, an exit
code a shell script branches on. This document is the single statement of which
of those are **promises**, what each kind of version bump is allowed to do to
them, and what is deliberately left unpromised.

It is the governance companion to
[`docs/schema-migrations.md`](schema-migrations.md), which owns the *mechanics*
of a schema version — what a bump may change, how a reader must behave on a
version it does not know, and the migration note for every step taken so far.
This file owns the *release* question: given a version number on a release,
what may have changed since the previous one.

> **Status: v1 release candidate.** The contracts below are frozen and enforced
> by tests today (ROADMAP Tasks 9.1 and 9.2). The workspace crates carry
> `1.0.0-rc.1`, which is what `admissionlab --version` prints. The `v1.0.0` tag
> itself has not been cut — the release candidate is what Phase 10 finalizes
> into `1.0.0`. Nothing in this document is aspirational about the *contracts*;
> the only thing not yet done is the final tag.

---

## Contents

- [The four versioned surfaces](#the-four-versioned-surfaces)
- [The three document schemas](#the-three-document-schemas)
- [The CLI surface](#the-cli-surface)
- [Exit codes](#exit-codes)
- [What patch, minor, and major mean here](#what-patch-minor-and-major-mean-here)
- [Cutting a release](#cutting-a-release)
- [What is deliberately not promised](#what-is-deliberately-not-promised)
- [Deprecation policy](#deprecation-policy)
- [Supported release lines](#supported-release-lines)

---

## The four versioned surfaces

| Surface | Versioned by | Promise |
| --- | --- | --- |
| Configuration, result, and run-manifest documents | their own `apiVersion` / `schemaVersion` field, **independently of the release** | frozen at `v1`; additive only — [below](#the-three-document-schemas) |
| The CLI: commands, positional arguments, long flags | the release version | frozen for `v1.x`; additive only — [below](#the-cli-surface) |
| Exit codes | the release version | frozen; never reassigned — [below](#exit-codes) |
| The Rust crates under `crates/` | each crate's own `Cargo.toml` | **no promise.** See [what is not promised](#what-is-deliberately-not-promised) |

The first row versions **independently** of the other three, and that is the
single most important thing on this page. A document's `apiVersion` says what
the document is; the release version says which build read it. A `v1.4.0`
Admission Lab still writes `admissionlab.io/result/v1`, and a `v1` schema will
outlive many releases. The two numbers are not synchronized and were never
meant to be.

## The three document schemas

Three identifiers, all frozen at `v1`, all with checked-in schemas:

| Family | Identifier | Field | Schema |
| --- | --- | --- | --- |
| Lab configuration | `admissionlab.io/v1` | `apiVersion` | [`schemas/admissionlab-v1.json`](../schemas/admissionlab-v1.json) |
| Result (`result.json`) | `admissionlab.io/result/v1` | `schemaVersion` | [`schemas/result-v1.json`](../schemas/result-v1.json) |
| Run manifest (`run.json`) | `admissionlab.io/run/v1` | `schemaVersion` | [`schemas/run-manifest-v1.json`](../schemas/run-manifest-v1.json) |

**Within `v1.x`, in any release**, these five clauses hold — quoted from the
roadmap and each pinned by a test that fails the build when it is broken. The
full argument for each, and the test that enforces it, is
[`docs/schema-migrations.md` § The stable-schema rule](schema-migrations.md#the-stable-schema-rule):

1. Optional additive fields are allowed, when an old reader can ignore them.
2. An existing field's *meaning* cannot change — not its unit, not what a
   digest is taken over, not what absence claims.
3. A required field cannot be removed.
4. A semantic-change serialization string (`newly_denied`,
   `container_removed`, `traffic_backend_changed`, …) cannot be renamed. These
   are the vocabulary a consumer's `match` and a user's `policy.failOn` are
   both written against.
5. An exit code cannot be reassigned.

Breaking any of them requires **`v2`**: a new identifier, a new schema file, a
migration note in `docs/schema-migrations.md`, and a reader that keeps
accepting `v1`. That is deliberately expensive, and it is the difference
between a schema that is published and one that is merely current.

**Older configurations keep loading.** `admissionlab.io/v1beta1` and
`admissionlab.io/v1alpha1` `Lab` documents are migrated on load and run
unchanged; `admissionlab.io/run-manifest/v1beta1` and
`.../v1alpha1` manifests are read as-is. Dropping that would be a `v2`-scale
break, so it is a major-release decision and nothing less.

**Two documents inside the configuration family version separately** and are
still `admissionlab.io/v1alpha1` on purpose: `expectations.yaml`
(`kind: Expectations`) and `*.matrix.yaml` (`kind: FixtureMatrix`). Setting
either to `v1` to match the lab file beside it is a configuration error, not a
migration. When either is promoted it gets its own migration note.

## The CLI surface

Frozen as of ROADMAP Task 9.2. Three commands, and exactly these flags:

```text
admissionlab [-v|--verbose] doctor    [--deep]
admissionlab [-v|--verbose] test      <CONFIG>
                                      [--keep-clusters]
                                      [--report-dir <DIR>]
                                      [--github-summary <FILE>]
admissionlab [-v|--verbose] reproduce <MANIFEST>
                                      [--source-root <DIR>]
                                      [--config <FILE>]
                                      [--keep-clusters]
                                      [--report-dir <DIR>]
```

Within `v1.x`, no command, positional argument, or long flag in that list may
be renamed or removed, and none may change from taking a value to not taking
one (or the reverse). A user's shell script and a CI workflow step are both
written against exactly these spellings.

**Adding a new optional flag with a backwards-compatible default is the only
change that stays inside the contract**, and it is a minor release.

Three further rules are part of the same freeze:

- `--help`/`-h` and `--version`/`-V` **always exit `0`**, on the root and on
  every subcommand.
- `admissionlab` with no arguments prints root help to **stderr** and exits
  `2` — a bare invocation is a usage mistake, and `2` is already this tool's
  invalid-input code.
- `-v`/`--verbose` is global: accepted before and after the subcommand, with
  identical meaning.

`crates/admissionlab-cli/tests/exit_codes.rs` pins the list mechanically. It
parses `--help` down to command names, positional value names, and option
spellings, and compares that against the table above; a flag added, renamed, or
dropped fails that test rather than reaching a release. Rewording a *help
description* is deliberately free — the golden is trimmed to the surface a
script can depend on, and nothing else.

## Exit codes

`0`–`6` are frozen and are documented, with their causes, in
[`README.md`](../README.md#exit-codes) and
[`docs/troubleshooting.md`](troubleshooting.md#exit-code-quick-reference).
**A number's meaning is never reassigned within `v1.x`**, and no eighth meaning
is added to that table.

Two further codes sit deliberately *outside* the table and are frozen in
exactly the same sense:

| Code | Meaning |
| --- | --- |
| `130` | Canceled by `SIGINT` (Ctrl-C). **No verdict.** |
| `143` | Canceled by `SIGTERM` (`kill`, a canceled CI job, a stopped container). **No verdict.** |

They are `128 + signal`, which is what every Unix shell already reports for a
process that died of a signal — so a script that reads `130` as "the operator
stopped this" is reading a convention it already knows rather than an Admission
Lab invention, and a gate written the ordinary way (non-zero fails) keeps
working untouched. They are outside the frozen seven because an interrupted run
reached none of the conclusions those seven assign: answering `3` would report
an infrastructure failure that did not happen, `6` a bug that does not exist,
and `0` a pass that was never computed. `130` and `143` now mean "canceled, no
verdict" and will not be reassigned either.

A run that *did* reach a verdict reports the verdict even if a signal arrived
afterwards — the comparison happened and the reports are on disk, and telling a
CI gate to discard them would be the wrong claim.

## What patch, minor, and major mean here

Admission Lab follows semantic versioning, read against the surfaces above
rather than against a library API.

| Bump | What it may contain | What it may never contain |
| --- | --- | --- |
| **Patch** (`v1.2.3` → `v1.2.4`) | Bug fixes, dependency updates, documentation, performance work, and **corrections to a classification that was wrong**. Also: a recipe pin bump for an already-certified recipe, with the certification run that proves it — [below](#the-patch-release-rule). | Any new field, flag, command, or wire string. Any change to what a correct run reports. |
| **Minor** (`v1.2.x` → `v1.3.0`) | New optional configuration fields, new optional CLI flags, new semantic-change kinds with new wire literals, new optional result/manifest fields, new recipes, a changed Kubernetes support window (see below). | A rename, a removal, a meaning change, or an exit-code reassignment. |
| **Major** (`v1.x` → `v2.0.0`) | Everything a minor cannot do — and it must arrive with a new schema identifier, a migration note, and a reader that keeps accepting `v1`. | Silence. A major release that does not say what it broke is a failed release. |

Three consequences worth stating outright, because each is a place where the
obvious reading is the wrong one:

- **A severity change is a minor, not a patch.** Regrading a semantic-change
  kind — `image_changed` from `info` to `warning`, say — changes which runs
  exit `1`, and a CI job that was green becomes red without anybody editing a
  configuration. A *bug* fix (a change classified as the wrong *kind*) is a
  patch; a *policy* change to the default severity table is a minor, and it is
  named in the changelog.
- **A new semantic-change kind is a minor.** It is an addition under clause 1,
  and consumers whose `match` has a default arm are unaffected — but a
  previously silent difference now appears in reports, and a `policy.failOn`
  written as a wildcard would newly fire.
- **The Kubernetes support window moves in minors.** Admission Lab supports the
  latest three upstream-supported Kubernetes minors at release time (Global
  Constraint 10). A minor that adds `1.38` and retires `1.35` is a supported
  change, and a request for the retired version is then refused with *"no
  longer supported by Admission Lab"* rather than *"never heard of it"* —
  retired minors stay checked into `compatibility/kubernetes.yaml` precisely so
  that distinction survives. See
  [`docs/compatibility.md`](compatibility.md#the-three-minors-rule-and-its-one-exception).

## Cutting a release

The section above says what a version *number* promises. This one says what a
*release* is allowed to be, who cuts it, and what they run — the rules the
post-v1 maintenance loop
([`.github/workflows/maintenance.yml`](../.github/workflows/maintenance.yml))
feeds proposals into.

Nothing here loosens the table above. It answers the question the table does
not: given a pile of merged commits, which number comes next, and what evidence
has to exist before the tag is pushed.

### The patch-release rule

**A patch release carries security and reliability fixes, and nothing else.**

`v1.2.3` → `v1.2.4` may contain:

- **Security fixes.** Advisories against a dependency, and vulnerabilities in
  Admission Lab itself. The handling is [`SECURITY.md`](../SECURITY.md) and
  [`docs/dependencies.md` § Emergency security updates](dependencies.md#emergency-security-updates);
  this is the release that ships the result.
- **Reliability fixes.** A flake, a hang, a leaked cluster, a race, a wrong
  verdict. Anything the nightly reliability suite exists to find.
- **Correctness fixes that restore the documented behavior** — including a
  semantic change classified as the wrong *kind*, which is a bug rather than a
  policy decision (the table above draws that line).
- **Dependency updates, documentation, and performance work** that change no
  observable output.
- **A recipe pin bump for a recipe that is already certified**, under the
  evidence rule below.

A patch release may **not** contain:

- **Any change to a versioned surface.** No new configuration field, no new
  result or manifest field, no new CLI command or flag, no new semantic-change
  wire literal, no exit code — not even additive ones. Additive is what a
  *minor* is for. A user upgrading a patch must not have to read anything.
- **A default severity change**, or any other change to what a correct run
  reports. The table above already calls a regrade a minor; a patch is the
  release where that rule is most tempting to break and most damaging to break,
  because a patch is what people apply without reading.
- **A Kubernetes support-window move.** That is a minor, always.
- **A new recipe**, or a first certification of an existing recipe against a
  Kubernetes minor it was not certified against before.

**Certified pins may bump in a patch, but only with re-certification
evidence.** This is the one clause where a patch touches
`compatibility/recipes.yaml`, and it is narrow on purpose:

1. The recipe is already certified, and the bump moves its pinned chart version
   within the same set of Kubernetes versions its rows already claim.
2. The recipe certification matrix has actually run on the new pin — Tier 3
   (`weeklyRelease`), which is every certified row — and the release PR names
   that run. A green run is the evidence; the version existing upstream is not.
   `compatibility/recipes.yaml`'s own header sets this standard: "every row
   below has actually been installed and verified by a test in this
   repository."
3. The new entry is **appended**. That file's append-don't-mutate rule holds in
   a patch exactly as it does anywhere else, so the release that bumped a pin
   stays readable a year later.
4. If the recipe pairs a vendored Gateway API bundle, that bundle's version is
   **re-derived from the new release's own `go.mod`**, never bumped
   independently — both `gateway-api-crds` components say so in their headers.
   A re-derivation that lands on a different bundle is still a patch; a bundle
   bumped on its own is not a change this project makes at all.

The weekly issue that
[`.github/workflows/maintenance.yml`](../.github/workflows/maintenance.yml)
opens when an upstream ships a new chart is a *prompt* for that work, never a
substitute for it. **A version existing upstream certifies nothing.**

### The minor-release rule

**A minor release carries backward-compatible additions, under the frozen `v1`
rules.**

`v1.2.x` → `v1.3.0` may contain everything a patch may, plus:

- **Additive schema fields** — optional configuration, result, and manifest
  fields, under clause 1 of [the stable-schema rule](#the-three-document-schemas):
  an old reader ignores them and keeps working. The `apiVersion` /
  `schemaVersion` identifiers do **not** move; they are versioned
  independently of the release and stay at `v1`.
- **New optional CLI flags and new commands.** Never a rename, never a removal,
  never a changed default that changes a verdict.
- **New semantic-change kinds**, with new wire literals — and the changelog
  entry the table above requires, because a previously silent difference now
  appears in reports.
- **A default severity change**, named in the changelog.
- **New certified recipes, and new certified Kubernetes rows for existing
  recipes** — a recipe certified where it was not certified before. Each row
  carries the tier that runs it and is proven by a real run, exactly like every
  row already there.
- **A Kubernetes support-window move.** Adding `1.38` and retiring `1.35` is a
  minor. The retired minor stays checked in as `supported: false` so a request
  for it is refused with *"no longer supported"* rather than *"never heard of
  it"*, and the changelog names both halves.
- **A deprecation announcement**, under [the deprecation
  policy](#deprecation-policy).

A minor release may **not** contain a rename, a removal, a meaning change, or
an exit-code reassignment. Those are `v2`, with a new schema identifier and a
migration note.

Two clarifications of the certified-matrix rows, because the boundary between
patch and minor runs straight through that file:

| Change to `compatibility/recipes.yaml` | Release |
| --- | --- |
| A newer chart pin for a recipe already certified on those Kubernetes versions | **Patch**, with the certification run named |
| A recipe certified against a Kubernetes version it was not certified against before | **Minor** |
| A new recipe | **Minor** |
| A row's `tier` changed (schedule only, no new claim) | **Patch** |
| A row dropped because its upstream is archived or its schedule stopped earning its cost | **Minor**, named in the changelog; not a deprecation |

### Who cuts a release

**The operator.** Admission Lab is a community-maintained project with no
release engineer and no automated publish: a release is a human decision by
someone with push access to a protected tag and the ability to publish a draft
GitHub Release.

Two consequences worth stating, because both are places automation stops on
purpose:

- **Nothing is published until a person publishes it.**
  [`.github/workflows/release.yml`](../.github/workflows/release.yml) attaches
  every artifact to a **draft** release. Nothing it produces is public until a
  maintainer reviews the assets and clicks publish.
- **Nothing merges itself.** The maintenance loop opens pull requests and
  issues; it has no auto-merge, and a Kubernetes support-window proposal it
  writes is explicitly a starting point rather than a mergeable change —
  the prose in `compatibility/kubernetes.yaml` that explains each entry is
  something only a person writes.

### The mechanical steps

1. **Decide the number** by the rules above, and make sure `CHANGELOG.md` says
   what changed — including which Kubernetes minors are supported and which
   recipe versions are certified, if either moved.
2. **Run the release checklist.**
   [`docs/release-checklist.md`](release-checklist.md) is the gate: its
   prerequisite gates, its rows, and the sign-offs only an operator can give.
3. **Rehearse the packaging locally.** `./scripts/verify-release.sh` runs the
   buildable half of the release workflow on one host with no tag — same
   `--locked` build, same tarball layout, same pinned SBOM generator, same
   checksum file, same smoke test.
4. **Bump the version** in the workspace manifests and `Cargo.lock`, and commit
   it as its own commit.
5. **Tag it.** A signed, protected `vX.Y.Z` tag on `main`. Pushing that tag is
   the only thing that starts the release workflow; nothing in it can run
   outside a tag push.
6. **Watch the workflow.** It builds the four supported targets, smoke-tests
   each packaged archive on the runner that produced it, generates the SPDX
   SBOM, checksums all five artifacts, signs `SHA256SUMS` with a keyless
   Sigstore certificate, and attaches the set to a draft release.
7. **Verify the published artifacts as a downloader would**, from
   [`docs/install.md`](install.md): check `SHA256SUMS`, verify the signature
   with `cosign verify-blob` against the workflow identity, unpack, and run
   `admissionlab --version`. Verifying the thing you published, rather than the
   binary in your workspace, is the point of the step.
8. **Run one smoke lab from the published binary** — not the workspace one —
   and confirm the GitHub Action install path resolves the new version.
9. **Publish the draft**, and announce only what the release metadata actually
   certifies.

The nightly reliability suite and the recipe certification matrix keep running
on `main` throughout, on their own schedules and with no release coupling —
that is what makes "the latest `v1.x`" a defensible answer to a bug report.

## What is deliberately not promised

Naming these is as much a part of the contract as the promises. Everything
below may change in any release, including a patch:

- **The Rust crate APIs.** The `crates/` workspace is an implementation, not a
  published library. Nothing is on crates.io, the crate versions
  (`1.0.0-rc.1` today) track nothing a user reads, and a type may be renamed,
  merged, or deleted between any two releases. The supported ways to consume
  Admission Lab are the binary, `result.json`, and `run.json`.
- **Terminal rendering.** Wording, column layout, ordering within a section,
  and color are presentation. A CI job that greps the terminal output is
  reading something with no promise attached to it; parse `result.json`
  instead, which is a schema.
- **`report.html`.** The HTML artifact's markup, styling, and structure. It
  renders the same redacted value `result.json` carries, and the JSON is the
  contract.
- **The GitHub job summary's Markdown.** Same reasoning.
- **Log lines and diagnostic prose.** Diagnostic *codes*
  (`metrics.unavailable`, `compatibility.uncertified_combination`, …) appear in
  `result.json` and are covered by the schema promise; the human-readable
  message beside a code is not.
- **The run workspace layout** under `${TMPDIR}/admissionlab-runs/<run-id>/`.
  The reports point at the evidence; the directory shape is not a published
  path scheme.
- **The contents of the certified matrix.** Which recipe × Kubernetes rows are
  certified is a record of what CI has actually run, and it moves as vendors
  and Kubernetes move. What *is* promised is the meaning of the word —
  [`docs/compatibility.md`](compatibility.md) — and that an uncertified
  combination is warned about, never refused.
- **Recipe pins.** A recipe's chart version is a curated fact about an
  upstream, not an interface. It changes when the upstream does.

## Deprecation policy

Nothing in the frozen surfaces above can be *removed* inside `v1.x` at all, so
deprecation here means one thing: announcing, ahead of a future major, that
something will not survive it. The rule is:

1. **A deprecation is announced in a minor release**, in `CHANGELOG.md` under
   its own `Deprecated` heading, and in the reference documentation for the
   thing being deprecated.
2. **The deprecated thing keeps working, unchanged, for the rest of `v1.x`.**
   Deprecation is a statement about the future, never a behavior change. A
   deprecated flag still takes its value and still does its job; a deprecated
   field is still read and still means what it meant.
3. **A runtime warning is allowed but never required**, and never on stderr in
   a way that could be mistaken for a finding. Where one is emitted for a
   configuration field, it is also a `result.json` diagnostic, so an archived
   report still carries it.
4. **Removal happens only in the next major**, and the major's migration note
   says what replaced it and how to migrate.
5. **A deprecation may be withdrawn.** If the replacement does not materialize,
   saying so in a later minor is better than carrying a threat nobody acts on.

Two things are explicitly *not* deprecations and need no announcement:
retiring a Kubernetes minor from the support window (that is the three-minors
rule doing its job, and it is a changelog entry rather than a deprecation), and
dropping a certified row when an upstream is archived or a test stops being
worth its schedule.

## Supported release lines

- **`v1.x`** — the current line. Fixes land on the latest `v1.x` release.
- **Older `v1` minors** — no backport promise. Upgrading within `v1.x` is
  designed to be safe precisely so that "upgrade to the latest `v1`" is a
  reasonable answer to a bug report.
- **Pre-1.0 history** — the `v1alpha1` and `v1beta1` *documents* keep loading,
  forever, inside `v1.x`. The pre-1.0 *builds* are not supported and receive no
  fixes.

Security-specific handling — the reporting channel, what counts as in scope,
and what response to expect — is [`SECURITY.md`](../SECURITY.md).

---

## See also

- [`docs/schema-migrations.md`](schema-migrations.md) — the mechanics of a
  schema version, and the migration note for every step taken.
- [`docs/compatibility.md`](compatibility.md) — certified vs supported vs
  merely configurable, and the three-minors rule.
- [`CHANGELOG.md`](../CHANGELOG.md) — what each release actually changed.
- [`SECURITY.md`](../SECURITY.md) — the security reporting channel and the
  supported release lines for security fixes.
- [`docs/release-checklist.md`](release-checklist.md) — the gate every release
  passes, and the sign-offs only an operator can give.
- [`docs/install.md`](install.md) — the downloader's side of a release:
  checksum, signature, unpack, PATH.
- [`docs/dependencies.md`](dependencies.md) — the monthly dependency sweep and
  the advisory-driven path, which is where most patch releases start.
