# Security Policy

Admission Lab creates disposable local Kubernetes clusters, installs
third-party charts into them, replays fixtures through them, and writes reports
about what it observed. That shape decides what a vulnerability *is* here: the
charts and workloads are untrusted by design, so "a component did something bad
inside its own ephemeral cluster" is the tool working — while anything that
escapes that cluster, leaks a credential into a report, or turns an input into a
command on your machine is not.

---

## Reporting a vulnerability

**Please do not open a public GitHub issue for a suspected security
vulnerability.**

Report it privately, by either of:

- GitHub's [private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing/privately-reporting-a-security-vulnerability)
  on this repository's **Security** tab, or
- opening a [GitHub Security Advisory](https://github.com/hairbui76/admission-lab/security/advisories/new)
  directly.

Both route to the same place. There is no security mailing list and no other
channel — a report sent anywhere else may not be seen.

Please include:

- what the vulnerability is and what an attacker gets from it;
- steps to reproduce, with the Admission Lab version or commit
  (`admissionlab --version`), the Kubernetes version, and the configuration,
  fixtures, or recipe involved;
- any known mitigation or workaround.

**Redact your own secrets before attaching anything.** A `result.json`, a
`run.json`, or a run workspace may carry material from your cluster. Admission
Lab redacts what its own rendering controls (see
[`docs/security.md`](docs/security.md#report-redaction)), and that is not a
guarantee about a file you assembled by hand.

### Response expectations

This is a community-maintained open-source project with **no dedicated security
team, no on-call rotation, and no bug bounty**. Saying so plainly is more useful
than publishing a service level nobody is staffed to meet. What you can expect:

| Stage | Expectation |
| --- | --- |
| **Acknowledgement** | As soon as a maintainer sees the advisory. Best effort; realistically days, not hours. |
| **Triage** | An in-scope/out-of-scope decision with reasons, and a severity assessment. |
| **Fix** | Prioritized by real impact. A credential-leak or host-execution issue comes before everything else on the roadmap. |
| **Disclosure** | Coordinated with the reporter, case by case. Nothing is published before you are ready unless the issue is already public. |
| **Credit** | In the advisory and in the `CHANGELOG.md` entry, under whatever name you prefer, unless you ask us not to. |

If a report goes unacknowledged for two weeks, please comment on the advisory —
it is far more likely to have been missed than ignored.

---

## Supported release lines

| Line | Status |
| --- | --- |
| Latest `v1.x` release | **Supported.** Security fixes land here. |
| Older `v1.x` releases | **No backport promise.** Upgrading within `v1.x` is designed to be safe — the document schemas, the CLI surface, and the exit codes are frozen and additive-only — so "upgrade to the latest `v1`" is a real answer rather than a deflection. See [`docs/versioning.md`](docs/versioning.md). |
| `v0.1.0-alpha.1`, `v0.2.0-beta.1`, and pre-1.0 default-branch builds | **Not supported.** No fixes, no advisories, no backports. Their *documents* keep loading in `v1.x` — a `v1alpha1` configuration still runs unedited — but their **builds** receive no security updates. |

Until `v1.0.0` is tagged, fixes land on the default branch and moving to it is
the remedy.

---

## What counts as a vulnerability here

### In scope

- **Report leakage.** Any path by which a `Secret`'s data, an `Authorization`
  or other credential header, a private key, a token, a kubeconfig, or a value
  at a configured sensitive path reaches `result.json`, `report.html`, the
  terminal report, the GitHub job summary, or `diagnostics.json` unredacted.
  Redaction is applied once, to one value, from which every rendering is drawn;
  a rendering that bypasses it is a security bug and not a cosmetic one.
- **Escape from the ephemeral cluster.** Anything that lets a fixture, a
  recipe, a chart, or an installed component reach outside the disposable
  cluster it was applied to — the operator's real `~/.kube/config` or
  `~/.kube/cache`, their Helm repository configuration or cache, the *other*
  side's cluster, or the host filesystem outside the run workspace. A recipe's
  relative `install.paths` escaping the recipe's own directory tree with `../`
  is this class.
- **Argv injection and command construction.** External tools are invoked with
  an argv vector and never by building a shell command string. Any input —
  configuration value, fixture content, recipe field, environment variable,
  cluster response — that reaches a shell, or that can inject an argument into
  `kind`, `kubectl`, `helm`, or `docker`, is in scope.
- **Credential and kubeconfig handling.** A kubeconfig written with wrong
  permissions, a credential logged or placed in an environment a subprocess can
  read when it should not, or a run workspace created without the `0700` it
  documents.
- **Audit-log exposure.** Admission Lab configures API-server audit logging to
  capture its own fixture requests at `Request` level. A change that causes
  `Secret` bodies, or requests from outside the fixture set, to be captured and
  retained is in scope.
- **Supply chain.** A dependency pulled from a mutable git ref or an unreviewed
  source, a vendored artifact whose checksum is not verified, or a release
  archive whose published `SHA256SUMS` and Sigstore signature do not cover what
  the archive actually contains.
- **Process lifecycle.** A subprocess that outlives the run that spawned it, or
  a cluster a failed or canceled run leaks — a leaked cluster keeps running
  whatever was installed in it.
- **Privilege escalation on the host** through any Admission Lab code path,
  beyond what the invoked external tools were explicitly asked to do.

### Out of scope

- **A vulnerability in a third-party chart or controller you asked Admission
  Lab to install.** Everything Admission Lab installs is an *untrusted test
  workload* by design — see
  [`docs/security.md`](docs/security.md#trust-model-for-third-party-charts-and-controllers).
  Report those to their own upstream. A vulnerability in *Admission Lab's own
  handling* of such a component — trusting its output where it should not, or
  letting it reach outside its cluster — is in scope, and that distinction is
  the whole point of this section.
- **Anything requiring an operator to hand a lab something the default flow
  never asks for**, such as pointing it at a production kubeconfig. The default
  v1 flow requires no production kubeconfig and copies no production secrets. A
  report assuming a mode outside that flow should say so explicitly and describe
  the scenario; it may still be worth reading, but it is a different argument.
- **The `kind`, `docker`, `kubectl`, and `helm` binaries themselves**, and the
  container runtime's own isolation properties. Report those upstream.
- **Denial of service from a lab you configured** — a fixture corpus that
  exhausts disk, a chart that never converges, a timeout that is too generous.
  Those are resource-management issues; file them as ordinary bugs.
- **Missing hardening with no demonstrated impact**, and automated-scanner
  output with no reproducible finding.

---

## What the design already guarantees

Read [`docs/security.md`](docs/security.md) before reporting. It is the full
threat model and states in detail what is redacted, what is deliberately *not*
redacted and why, how the audit policy excludes `Secret` bodies, what network
egress happens and when, the subprocess discipline, and the filesystem
permissions. Several things that look like findings are documented, deliberate
positions there.

The load-bearing invariants, in one list:

- Baseline and candidate are separate ephemeral clusters that never share
  mutable state, and both are deleted on success *and* on failure — a run that
  leaks a cluster never exits `0`.
- External commands are argv vectors, never shell strings, each with a timeout,
  separate stdout/stderr capture, and recorded provenance.
- `helm` and `kubectl` are given their own isolated configuration and cache
  directories; a lab run never reads or writes the operator's `~/.kube/config`,
  `~/.kube/cache`, or `~/.config/helm`.
- Report redaction is applied once, to one value, from which the terminal, JSON,
  and HTML renderings are all produced.
- Missing evidence is reported as unavailable or unknown. It is never
  fabricated.

---

## Code of conduct

Participation in this project, security correspondence included, is governed by
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).
