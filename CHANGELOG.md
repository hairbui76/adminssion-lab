# Changelog

All notable changes to Admission Lab are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
as [`docs/versioning.md`](docs/versioning.md) defines it for this tool: the
three document schemas, the CLI surface, and the exit codes are the promises a
version number is about, and the Rust crate APIs are not.

**Document schema versions are independent of release versions.** A release
listed below may write `admissionlab.io/result/v1` while carrying any `1.x`
number of its own; the two were never meant to be synchronized.

---

## [Unreleased]

Nothing yet.

---

## [1.0.0] — 2026-09-01

The first stable release: `1.0.0-rc.1` finalized with no code change. The
acceptance checklist recorded sixteen locally-verified PASS rows, one
operator-only row, and zero blockers ([`docs/release-checklist.md`](docs/release-checklist.md));
the single Task 10.3 disposition was a documentation correction.

**Supported Kubernetes minors** (kind v0.33.0 node images, digest-pinned in
`compatibility/kubernetes.yaml`): **1.37.0, 1.36.4, 1.35.8** — the core
dogfood suite passes on all three.

**Certified recipe versions** (`compatibility/recipes.yaml`):

- `kyverno` 3.9.0 — Kubernetes 1.35.8 (Kyverno documents 1.33–1.35; a recipe
  limitation, not a core one)
- `istio` 1.30.4 — 1.35.8 / 1.36.4 / 1.37.0
- `istio-gateway` 1.30.4 (Gateway API v1.5.1) — 1.35.8 / 1.36.4 / 1.37.0
- `nginx-gateway-fabric` 2.6.7 (Gateway API v1.5.1) — 1.35.8 / 1.36.4 / 1.37.0
- `ingress-nginx-legacy` 4.15.1 (upstream archived; migration testing only) —
  1.36.4

Everything the release contains is described by the `1.0.0-rc.1` entry below;
nothing was added between the candidate and this release.

---

## [1.0.0-rc.1] — 2026-09-01

Everything since `v0.2.0-beta.1`, cut as the **v1 release candidate**. The
workspace crates now carry `1.0.0-rc.1`, which is what `admissionlab --version`
prints and what the release archive is named after
(`admissionlab-1.0.0-rc.1-<target>.tar.gz`). **The `v1.0.0` tag has not been
cut**: this candidate is what gets finalized into `1.0.0`, and no new feature
enters the window between the two. The contracts described below are frozen and
test-enforced today.

The manual acceptance pass a candidate is judged against — seventeen rows, the
release blockers, and the rows only an operator with CI runners can sign off —
is [`docs/release-checklist.md`](docs/release-checklist.md).

### Added

- **NGINX Gateway Fabric as a second certified Gateway API implementation**
  (chart `2.6.7`, Gateway API `v1.5.1`), installed and traffic-tested on all
  three supported Kubernetes minors — same namespace, cross-namespace with a
  `ReferenceGrant`, and through its own `NginxProxy` data-plane override.
- **A portable Gateway traffic contract corpus**, run against *both* certified
  implementations, so a behavior claim is about the Gateway API rather than
  about one controller.
- **Ingress-to-Gateway migration comparison.** A `migration:` suite captures
  legacy `Ingress` traffic behavior on the baseline and Gateway API behavior on
  the candidate, sends the same probes through both data planes, and classifies
  the difference — `backend_changed`, `traffic_status_changed`, and an explicit
  `non_portable_feature` finding for a legacy annotation Gateway API v1 cannot
  express. Findings appear in the terminal report, `result.json`, the HTML
  artifact, and the GitHub job summary.
- **A pinned legacy `ingress-nginx` recipe** (chart `4.15.1`, the archived
  project's final release) certified on the primary Kubernetes version **for
  migration compatibility testing only**. Its presence is not a recommendation
  to run it; see [`recipes/ingress-nginx-legacy/README.md`](recipes/ingress-nginx-legacy/README.md).
- **[`examples/ingress-to-gateway/`](examples/ingress-to-gateway/)** — the
  third canonical demo, and the only one whose two sides run different stacks.
  One behavior preserved, one non-portable feature accepted in writing, one
  unintended backend flip that no status, condition, or manifest diff reports.
- **Cooperative cancellation.** `SIGINT`/`SIGTERM` stops scheduling new work,
  tears down both clusters, and exits `130`/`143` — deliberately outside the
  frozen `0`–`6` table, because an interrupted run reached none of the seven
  conclusions that table assigns. A second signal exits immediately.
- **Verifiable release artifacts**: prebuilt archives for Linux x86_64/aarch64
  and macOS Apple Silicon/Intel, a `SHA256SUMS` file, a keyless Sigstore
  signature over it, an SPDX SBOM, and `scripts/verify-release.sh` for
  reproducing an artifact locally. Documented in
  [`docs/install.md`](docs/install.md).
- **A dependency supply-chain gate.** `cargo deny check advisories bans
  licenses sources` runs in CI against a pinned tool version, duplicate major
  versions of the HTTP/TLS stack fail unless an exception names the crates and
  their removal issue, and no dependency may come from a mutable git ref.
  Cadence and the emergency-update process are in
  [`docs/dependencies.md`](docs/dependencies.md).
- **Actionable cluster-failure diagnostics**: a `kind` or Docker failure now
  carries the stage, the command, and the captured output that explain it,
  instead of an opaque non-zero status.
- **[`docs/versioning.md`](docs/versioning.md)** and this changelog.

### Changed

- **Crate versions moved to `[workspace.package]`.** All fifteen workspace
  crates inherit one `version` field rather than each declaring its own, so a
  release bump is a single line. The crate versions still promise nothing —
  see [`docs/versioning.md`](docs/versioning.md).
- **The three document families are frozen at `v1`** (from `v1beta1`):
  `admissionlab.io/v1`, `admissionlab.io/result/v1`, and
  `admissionlab.io/run/v1`. Within `v1.x` no field meaning changes, no required
  field is removed, no semantic-change wire string is renamed, and no exit code
  is reassigned; only optional additive fields are allowed. Each clause is
  pinned by a test. `v1beta1` and `v1alpha1` configurations still load
  unchanged. See [`docs/schema-migrations.md`](docs/schema-migrations.md).
- **The run manifest's stable identifier drops the `-manifest` infix** —
  `admissionlab.io/run/v1`, not `admissionlab.io/run-manifest/v1`. A version
  string is matched on, so it could only change at a version boundary, and this
  was the last one. Manifests already on disk keep the spelling they were
  written with.
- **The CLI command surface is frozen.** Three commands, their positional
  arguments, and their long flags are a public contract; `--help`/`--version`
  exit `0` on every subcommand, and a bare `admissionlab` prints root help to
  stderr and exits `2`. A flag added, renamed, or dropped fails a test rather
  than reaching a release.
- **Documentation now states support boundaries explicitly** — core Kubernetes
  support, the certified recipe table, and user-supplied stacks (first-class,
  warned about, never refused) — in `README.md` and
  [`docs/compatibility.md`](docs/compatibility.md).
- **Certification schedules were rebalanced**: NGINX Gateway Fabric's two
  non-primary minors moved from the weekly tier to nightly, and the legacy
  `ingress-nginx` row is scheduled only in migration-specific weekly jobs. A
  tier is a statement about schedule and never about confidence; no row was
  added, removed, or re-certified.
- **The v1 Kubernetes compatibility matrix is final.** Admission Lab `1.0.0`
  provisions the latest three upstream-supported Kubernetes minors — **1.37,
  1.36 and 1.35** — each pinned to an exact patch *and* to the `kindest/node`
  digest that the `kind` v0.33.0 release published for it. Nothing floats, and
  no digest was resolved from a tag:

  ```text
  1.37.0  kindest/node:v1.37.0@sha256:a1ed56cfb0e7b93589bdf97c8cd566405a265939e3620fc4f5de89adff580ae5
  1.36.4  kindest/node:v1.36.4@sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed  (Tier 1 primary)
  1.35.8  kindest/node:v1.35.8@sha256:07b2536e30b803ed61d1677a79df6115f798ce64c80f9e22f6ed45afd09323c0
  ```

  The pins were re-checked against endoflife.date, the `kind` v0.33.0 release
  notes and `dl.k8s.io/release/stable.txt` immediately before this release
  candidate (`scripts/update-kubernetes-matrix.sh`) and needed **no change**:
  those are still upstream's newest three minors at their newest patches, and
  no 1.38 exists. `1.34.11` stays checked in as `supported: false`, so a
  configuration still asking for it is refused by name rather than by a lookup
  failure. All three supported minors were then re-run for real — the core
  admission dogfood lab on each, and every certified recipe row in
  [`compatibility/recipes.yaml`](compatibility/recipes.yaml) on the Kubernetes
  versions it names.
- **One vendor limitation is carried into v1, and it is a *recipe* limitation.**
  The `kyverno` recipe (chart `3.9.0` / appVersion `v1.19.0`) is certified on
  Kubernetes `1.35.8` and nowhere else, because Kyverno's own documentation for
  that chart line states support for v1.33–v1.35 and stops there — certifying
  it on `1.36.4` or `1.37.0` would mean claiming a window the vendor does not.
  Core Kubernetes support is unaffected and is proven separately on all three
  minors; see
  [`docs/compatibility.md`](docs/compatibility.md#the-certified-table).

### Security

- **External process lifecycle hardening.** Every subprocess is killed and
  reaped on timeout, cancellation, and panic; no child outlives the run that
  started it.
- **Audit-log and report data hardening.** Secret bodies are excluded at the
  audit-policy level rather than filtered afterwards, and the redaction pass
  that produces the terminal, JSON, and HTML views is applied once to one value
  so the three can never disagree.
- **[`SECURITY.md`](SECURITY.md)** now names the reporting channel, response
  expectations, in-scope classes, and the supported release lines.

### Fixed

- The `repository` URL in workspace package metadata and in `SECURITY.md`
  pointed at a misspelled repository (`adminssion-lab`). Corrected to
  `admission-lab`.

---

## [0.2.0-beta.1] — 2026-09-01

**Public Beta: versioned contracts and the Gateway engine.** Beta added a
second engine, made a run reproducible from its own recorded manifest, and
published the first versioned document contracts.

### Added

- **The Gateway engine**, observing Gateway API behavior as three separate
  kinds of evidence that never stand in for one another: admission (what the
  API server decided), reconciliation (what the implementation published in
  `status`, with a convergence rule that requires settled conditions at a
  current `observedGeneration` across two consecutive polls), and traffic (what
  a real HTTP request through the real data plane got back, and which backend
  answered). A probe that could not be sent is recorded as a skip naming the
  condition, state, and controller reason that stopped it — never as silence,
  and never as a fabricated status.
- **Istio Gateway API certified** across all three supported Kubernetes
  minors, with a vendored, checksum-verified Gateway API CRD bundle installed
  as a first component.
- **[`examples/gateway-istio/`](examples/gateway-istio/)** — two identical real
  Istio installs told apart by one line of a `ReferenceGrant`.
- **Reproducible run manifests.** `run.json` records the configuration, source
  digests, Kubernetes versions, node-image digests, component versions, and
  tool provenance of a run; `admissionlab reproduce` re-runs it against the
  same inputs. Provenance is preserved even when a run fails.
- **A GitHub Action wrapper** with pinned, checksummed installation, artifact
  upload on failure, and a job summary that never states a verdict the run did
  not reach.
- **Deterministic parameterized fixture matrices** (`*.matrix.yaml`).
- **Stage timing instrumentation** and a nightly reliability suite.
- **A certified-compatibility model** (`compatibility/recipes.yaml`) with CI
  tiers, plus a warning — never a refusal — when a lab requests a combination
  nobody certified.

### Changed

- The three document families were promoted to `v1beta1` with a published
  compatibility rule, and a lab configuration gained an explicit `readiness`
  section so a run can prove a component is *serving* rather than merely
  applied.
- `admissionlab reproduce` accepts any supported configuration `apiVersion`,
  not only the newest.

---

## [0.1.0-alpha.1] — 2026-09-01

**Public Alpha: the admission regression engine.** Alpha established the whole
spine — two ephemeral clusters, real server-side dry-run, deterministic
comparison, three renderings of one result — for admission only. Gateway work
was deliberately kept out of the critical path until this gate passed.

### Added

- **Two-cluster lab orchestration.** `admissionlab test` creates isolated
  baseline and candidate `kind` clusters at pinned, digest-verified node
  images, installs a stack into each through Helm and raw-manifest backends
  with deterministic readiness probes, and deletes both on success *and* on
  failure — a run that leaks a cluster never exits `0`.
- **Real server-side dry-run fixture execution.** Every fixture is replayed as
  a `?dryRun=All` CREATE against a real API server; the response object is the
  authoritative admitted, mutated object. There is no in-process simulator
  anywhere in the result path. A fixture that cannot be evaluated this way
  fails explicitly rather than silently switching semantics.
- **Request-scoped audit correlation and webhook trace reconstruction**, with
  serial fixture execution inside each cluster to make correlation
  deterministic, plus optional per-webhook latency deltas from isolated
  kube-apiserver metrics.
- **Deterministic comparison**: object normalization, trace canonicalization,
  admission-decision and workload-mutation classification, a frozen default
  severity table, explicit `expectations.yaml` matching with stale-entry
  reporting, and first-divergence attribution graded `observed`, `partial`, or
  `unknown` — never invented.
- **Three renderings of one redacted result**: a terminal report, `result.json`,
  a standalone `report.html`, and a GitHub job summary.
- **Curated recipes** — a vendor-neutral format carrying pinned installs,
  readiness checks, normalization rules, and capabilities, structurally unable
  to classify a regression — with certified Kyverno and Istio recipes and a
  first-party dogfood webhook.
- **[`examples/kyverno-istio-upgrade/`](examples/kyverno-istio-upgrade/)** —
  the canonical worked example, and a checked-in regression corpus.
- **`admissionlab doctor`** (and `--deep`), the frozen `0`–`6` exit-code table,
  and a bounded, argv-only external process runner with timeouts, separate
  stream capture, and credential redaction.

[Unreleased]: https://github.com/hairbui76/admission-lab/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/hairbui76/admission-lab/compare/v1.0.0-rc.1...v1.0.0
[1.0.0-rc.1]: https://github.com/hairbui76/admission-lab/compare/v0.2.0-beta.1...v1.0.0-rc.1
[0.2.0-beta.1]: https://github.com/hairbui76/admission-lab/compare/v0.1.0-alpha.1...v0.2.0-beta.1
[0.1.0-alpha.1]: https://github.com/hairbui76/admission-lab/releases/tag/v0.1.0-alpha.1
