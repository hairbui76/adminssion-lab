# Dependencies and supply chain

Admission Lab is a tool people run against their own infrastructure, on their
own machines, to decide whether a change is safe to ship. That makes its
dependency graph part of its threat model: a compromised transitive crate does
not just break the tool, it gets executed by someone with a cluster in front of
them.

This document is the prose half of the policy. The machine-readable half is
[`deny.toml`](../deny.toml), enforced on every push and pull request by the
`quality-gates` job in
[`.github/workflows/ci.yml`](../.github/workflows/ci.yml). Neither half is
authoritative alone: `deny.toml` decides what fails, this document says what to
do about it and what may be waived.

**The one-line version:** crates.io only, no git dependencies, permissive
licenses only, one version of the HTTP/TLS stack, and no known advisory —
checked by a pinned `cargo-deny` on every commit.

---

## Contents

- [The gate](#the-gate)
- [Why cargo-deny and not also cargo-audit](#why-cargo-deny-and-not-also-cargo-audit)
- [What the graph looks like today](#what-the-graph-looks-like-today)
- [Update cadence](#update-cadence)
- [Emergency security updates](#emergency-security-updates)
- [The policy knobs](#the-policy-knobs)
- [Exception protocols](#exception-protocols)
- [Updating the pinned tooling](#updating-the-pinned-tooling)
- [Running the checks locally](#running-the-checks-locally)

---

## The gate

CI runs one command:

```bash
cargo deny check advisories bans licenses sources
```

Four checks, named explicitly rather than relying on bare `cargo deny check`
defaulting to all of them — so that a future cargo-deny release cannot quietly
change what the default set contains.

| Check | Asks | Fails the build when |
| --- | --- | --- |
| `advisories` | Is any crate in `Cargo.lock` subject to a published RustSec advisory, yanked, or unmaintained? | Any advisory matches, any dependency version has been yanked from crates.io, or any crate in the graph is flagged unmaintained |
| `bans` | Is the HTTP/TLS stack duplicated across major versions? | Two majors of `http`, `http-body`, `http-body-util`, `hyper`, `hyper-util`, `hyper-rustls`, `rustls`, `rustls-pki-types`, `rustls-webpki`, or `tokio-rustls` are in the graph. Every other duplicate warns |
| `licenses` | Is every dependency's license compatible with Apache-2.0? | A crate's license is outside the allowlist |
| `sources` | Where did this code come from? | Anything resolves to a git repository, or to a registry other than crates.io |

The tool is pinned to **cargo-deny 0.20.2**, installed by
`taiki-e/install-action@v2.87.1` (itself pinned). Both pins are deliberate:
cargo-deny changes its own lint defaults between releases, so an unpinned tool
would make this gate's strictness a function of *when the job ran* — the same
commit passing on Monday and failing on Friday with nothing in the repository
having changed.

---

## Why cargo-deny and not also cargo-audit

Because they read the same database.

`cargo audit` queries [RustSec `advisory-db`][advisory-db] and reports
advisories matching `Cargo.lock`. cargo-deny's `advisories` check queries
RustSec `advisory-db` — the same repository, configured explicitly as
`db-urls` in `deny.toml` — and reports advisories matching the same
`Cargo.lock`. A second pinned binary would produce the same findings over the
same graph, with a second version to keep current and a second config to keep
consistent with this one.

cargo-deny is the stricter of the two here, not the weaker: it also fails on
**yanked** versions and on **unmaintained** crates anywhere in the graph, which
`cargo audit` treats as warnings by default.

If that ever stops being true — cargo-audit grows a check cargo-deny lacks —
add it then, pinned the same way, and update this section rather than leaving
the two documents to disagree.

[advisory-db]: https://github.com/RustSec/advisory-db

---

## What the graph looks like today

Recorded at the commit that introduced this policy, so that later drift is
visible rather than assumed. Regenerate with the commands in
[Running the checks locally](#running-the-checks-locally).

| Finding | State |
| --- | --- |
| Total packages in `Cargo.lock` | 261 |
| Git dependencies | **None.** Zero `source = "git+…"` entries; every package resolves to the crates.io registry with a checksum |
| HTTP/TLS stack duplication | **None.** Every crate in the banned-duplicate list resolves to exactly one version |
| Live advisories | **None.** `advisories ok` |
| Non-allowlisted licenses | **None.** `licenses ok` |
| Duplicate-version warnings | Six, all outside the HTTP/TLS stack: `base64`, `getrandom`, `pem`, `r-efi`, `syn`, `windows-sys` |

Two of those rows deserve more than a checkmark.

**No git dependencies is a release requirement, not a preference.** A `branch`
or `tag` git ref is mutable: upstream can move it, and the same `Cargo.lock`
then resolves to different code tomorrow. Admission Lab's entire claim is that
a run's result is attributable to the stack under test rather than to the tool,
which a shifting dependency quietly falsifies. `allow-git = []` in `deny.toml`
is what keeps this true by construction instead of by a reviewer happening to
notice a `git = ` key in a diff.

**The HTTP/TLS stack is banned from duplicating because duplication there is a
correctness problem, not a size problem.** Two `rustls` majors means two
certificate verifiers, two cipher-suite policies, and a patched advisory that
lands on only one of them. Two `http` majors means two incompatible
`HeaderMap`/`Body` types that cannot cross the seam, so whichever crate bridges
them re-serializes headers by hand — which is where header-injection and
redaction bugs live. Admission Lab reads admission responses and Gateway probe
results off these types and then *classifies regressions from them*: a
divergence introduced by our own transport stack would be reported to the user
as a divergence in *their* admission stack. That is the specific failure this
ban exists to prevent.

The six remaining duplicates are leaf, proc-macro, and platform crates. None of
them carries protocol state across a boundary, and most are a transitive
dependency's release schedule rather than something this repository can fix, so
they warn instead of failing. (`Cargo.lock` also holds two `bitflags` majors,
but the 1.3.2 copy is reachable only through `defmt` on a target this workspace
does not build for, so it is not in the graph cargo-deny resolves.)

---

## Update cadence

Two clocks, and they exist for different failure modes.

### Monthly sweep

Once a month, someone runs the sweep in
[Running the checks locally](#running-the-checks-locally), reviews
`cargo update --dry-run`, and opens a single PR with the resulting `Cargo.lock`
changes. Scope, in order of preference:

1. **Patch and minor updates within existing requirements** — `cargo update`.
   The default. These need no `Cargo.toml` change.
2. **Major bumps of direct dependencies** — one crate per PR, with the
   changelog entry that justifies it in the PR body. Under §0.1 of the roadmap,
   adding or bumping a dependency is a deliberate commit, not a side effect:
   the reviewer needs to see what changed and why it was worth it.
3. **Nothing else.** A monthly sweep is not the place to introduce a new
   dependency.

The sweep is a floor, not a ceiling — a dependency change that a feature needs
happens when the feature happens.

### Advisory-driven

The `advisories` check reaches the network for the current advisory database on
every run, so a newly-published advisory against an already-pinned dependency
turns CI red **without any change on our side**. That is the intended
notification path, and it is why `advisories` is in the per-commit gate rather
than in a slower job.

Its limit is worth stating plainly: it only fires when CI runs. During a quiet
period with no commits, an advisory published on day 2 is not seen until the
next push or the monthly sweep, whichever comes first. The monthly sweep is
therefore also the backstop that bounds that window, and running it is not
optional just because CI is green.

---

## Emergency security updates

For an advisory or a compromised dependency affecting Admission Lab. For a
vulnerability *in Admission Lab itself*, [`SECURITY.md`](../SECURITY.md) is the
front door and takes precedence.

**Who.** The maintainers, through the normal PR process. Consistent with
`SECURITY.md`, this is a community-maintained project with no dedicated
security team and no bug bounty: the windows below are the targets work is
organized around, not a contractual SLA, and saying otherwise here would be a
promise no one is staffed to keep.

**How**, in order — the first step that works is the answer:

1. **Assess reachability before reacting.** Does Admission Lab actually call
   the vulnerable code path? An advisory against a crate we depend on but whose
   affected API we never invoke is real but not urgent, and treating it as
   urgent trains people to bypass the gate. Record the conclusion; it becomes
   the justification for whichever step follows.
2. **Update the dependency.** `cargo update -p <crate>` if a fixed version
   exists within the current requirement, or a minimal `Cargo.toml` bump if it
   does not. Smallest change that clears the advisory — an emergency is the
   worst possible moment for an unrelated refactor to ride along.
3. **Remove the dependency**, if no fix exists and the crate is not load
   bearing. Often faster than waiting for an upstream release.
4. **Replace the transitive owner.** If the vulnerable crate arrives through
   another dependency, `cargo tree -i <crate>` names who to chase; the fix is
   that crate's update, or its replacement.
5. **Only if none of the above is possible**, add a time-boxed
   `advisories.ignore` entry under the
   [advisory ignore protocol](#advisory-ignore), with the reachability finding
   from step 1 as its justification and an issue tracking removal. This is the
   last resort, not the quick fix, and it is the only step that leaves the
   product shipping known-vulnerable code.

**Timeline**, measured from the advisory becoming visible in CI:

| Severity, as it applies to Admission Lab | Target |
| --- | --- |
| Critical or high, and the vulnerable path is reachable from a normal run | Same day: fix, or a documented mitigation and a public issue |
| Reachable but low impact, or high severity in a path we never invoke | Within the week, in its own PR |
| Not reachable, or advisory against a dev-dependency only | Folded into the next monthly sweep, with the reachability finding recorded |

**Do not merge around a red supply-chain gate.** If the fix cannot land
immediately, the `advisories.ignore` protocol is how the exception is recorded,
reviewed, and later removed. Disabling the check, or landing with it failing,
converts a tracked exception into an untracked one.

---

## The policy knobs

Section by section through [`deny.toml`](../deny.toml). The file's own comments
carry the full reasoning; this is the map.

### `[graph]`

`all-features = true` — police the widest graph the workspace can build, not
the narrowest. CI's `cargo clippy --all-features` already lints code behind
optional features; without this, that same code's *dependencies* would sit
outside the supply-chain gate, and an optional dependency could carry a denied
license or a live advisory and still ship. No workspace crate declares a
`[features]` table today, which makes this a no-op right now and the correct
default the moment one does.

### `[advisories]`

| Knob | Value | Meaning |
| --- | --- | --- |
| `db-urls` | RustSec `advisory-db` | The feed. Stated explicitly rather than left to the default, because [this is the cargo-audit equivalence claim](#why-cargo-deny-and-not-also-cargo-audit) and it should be checkable by reading the file |
| `yanked` | `deny` | A yanked version is a supply-chain signal in its own right — the publisher withdrew it, usually for a defect or a compromise |
| `unmaintained` | `all` | Unmaintained crates anywhere in the graph, not only direct workspace dependencies. An abandoned transitive crate is exactly where an unpatched advisory lands |
| `ignore` | `[]` | Advisories deliberately not failing the build. Empty, and every future entry needs the [advisory ignore protocol](#advisory-ignore) |

### `[licenses]`

`allow` is the set of licenses compatible with Admission Lab's own Apache-2.0
(Global Constraint 1): Apache-2.0, MIT, BSD-2-Clause, BSD-3-Clause, ISC,
Unicode-3.0, Zlib, and CDLA-Permissive-2.0. Anything else fails.
CDLA-Permissive-2.0 is there for exactly one crate — `webpki-root-certs`, which
is Mozilla's CA root *dataset* rather than source code — under a ruling
recorded in full in `deny.toml`'s own comment, including the notice-retention
obligation that release packaging has to satisfy.

`unused-allowed-license = "allow"` silences cargo-deny's warning about
allowlist entries that match nothing in the current graph. The allowlist is a
policy statement — what this project is willing to accept — not an inventory of
what happens to be present, and warning on every transitive crate that comes
and goes trains people to skim the output. The failure direction is untouched:
a license *not* in the list still hard-fails.

### `[bans]`

`multiple-versions = "warn"` is the baseline for the graph at large.
Ten `[[bans.deny]]` entries carrying `deny-multiple-versions = true` override
it for the HTTP/TLS stack, turning any duplicate there into an error.

Cargo already unifies semver-compatible requirements, so any duplicate that
survives into `Cargo.lock` is by construction a *major* (or 0.x-minor, which is
semver-major) split — which is why these entries need no version ranges to mean
"fail on duplicate majors".

The list is enumerated from `Cargo.lock`, not guessed. Adding a crate to it is
cheap; removing one requires the same justification as an exception.

### `[sources]`

`unknown-registry = "deny"`, `unknown-git = "deny"`, crates.io as the only
allowed registry, and `allow-git = []`. Everything about why is in
[What the graph looks like today](#what-the-graph-looks-like-today).

---

## Exception protocols

Every exception below shares one shape: **it names what is being waived, why it
is safe in the interim, and the issue that removes it.** An exception with no
removal issue is a permanent hole with optimistic phrasing, and reviewers
should reject it on that basis alone.

### Duplicate-version ban

For a duplicate major in the HTTP/TLS stack that cannot be resolved now. It is
a build failure until it is either removed or granted an exception.

The exception is a `[[bans.skip]]` entry pinning the **exact** version being
tolerated — never a bare crate name, which would tolerate every future
duplicate of that crate too — immediately preceded by a comment naming all four
required facts:

1. the crate, and the exact duplicate versions in play;
2. the transitive owner(s) forcing the old version, from
   `cargo tree -i <crate>@<version>` — the crate to chase, by name;
3. why the duplication is safe in the interim: which side of the seam each
   version sits on, and what stops state crossing between them;
4. the removal issue, as a URL.

`deny.toml` carries a worked example of the required shape. The reviewer's job
is to check fact 3 specifically: "it builds fine" is not an answer to it.

### Advisory ignore

For an advisory that cannot be fixed by updating, removing, or replacing (steps
2–4 of [Emergency security updates](#emergency-security-updates)). An
`advisories.ignore` entry needs:

1. the advisory ID and the affected crate and version;
2. why the vulnerable code path is unreachable from Admission Lab, **or** why
   no fix is available — the reachability finding, not a restatement of the
   advisory;
3. the transitive owner pulling it in;
4. the removal issue, as a URL.

Every one of these is re-examined at the monthly sweep. An ignore entry that
has outlived its justification is removed; an ignore entry nobody can explain
is removed and the advisory dealt with properly.

### License allowlist addition

Adding a license to `allow` is a licensing decision about the whole project,
not a build fix. It needs the license text read directly — not recalled — and a
comment recording which crate needs it, how it enters the graph, what
obligations it imposes, and how those obligations are satisfied in shipped
artifacts. The CDLA-Permissive-2.0 entry in `deny.toml` is the reference for
the expected depth.

### Git source

Pin an immutable `rev = "<full 40-character sha>"` — never `branch` or `tag`,
both of which upstream can move — allow-list it in `[sources]` with the same
four facts the duplicate-ban protocol requires, and remove it before the next
stable release. A stable release must not depend on a mutable ref.

---

## Updating the pinned tooling

Bumping the cargo-deny pin in `.github/workflows/ci.yml` is a deliberate change
with its own PR, because a cargo-deny release can change lint defaults and
therefore change what the gate means:

1. Read cargo-deny's changelog for lint-default and config-schema changes
   between the pinned version and the target.
2. Install the target version locally
   (`cargo install cargo-deny --locked --version <target>`) and run the full
   check. New warnings or errors on an unchanged graph are the changelog
   entries you just read, showing up.
3. Bump `tool: cargo-deny@<version>` in the workflow, and the version named in
   `deny.toml`'s header comment and in [The gate](#the-gate) above.
4. If a config key changed shape, fix `deny.toml` in the same PR. A stale key
   is silently ignored by some cargo-deny versions, which means a check that
   appears to be running and is not.

The same applies to `taiki-e/install-action`: pinned to a tag, bumped
deliberately.

---

## Running the checks locally

The full gate, exactly as CI runs it:

```bash
cargo install cargo-deny --locked --version 0.20.2   # once
cargo deny check advisories bans licenses sources
```

The monthly sweep:

```bash
cargo deny check advisories bans licenses sources   # current state
cargo update --dry-run                              # what would move
cargo tree -d                                       # duplicate detail
```

To confirm the two structural properties by hand, without the tool:

```bash
grep -c 'source = "git' Cargo.lock   # git dependencies: must be 0
cargo tree -i <crate>@<version>      # who pulls in a specific version
```

`cargo deny check advisories` needs network access to fetch the advisory
database; the other three checks are offline once the graph resolves.
