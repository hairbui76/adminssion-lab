# Admission Lab Implementation Roadmap

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Do not skip review gates, tests, or phase exit gates.

**Goal:** Build Admission Lab from an empty Rust repository into a stable v1.0 open-source regression-testing tool that compares baseline and candidate Kubernetes admission/Gateway stacks on real ephemeral clusters and deterministically reports behavior changes before production.

**Architecture:** Admission Lab is a Rust-first, local-first CLI. `admissionlab test` creates isolated baseline and candidate `kind` clusters, installs stacks through generic installers/curated recipes, replays the same fixtures through real API servers, captures admission/Gateway behavior, normalizes nondeterminism, computes semantic differences, evaluates explicit policy/expectations, and renders terminal/JSON/HTML reports. Gateway behavior is added only after the admission engine reaches Public Alpha; there is no hosted service in the v1 critical path.

**Tech Stack:** Rust 1.98.0 / Edition 2024 workspace; Tokio; Clap; Serde; `serde_yaml`; `serde_json`; `schemars`; `kube` + `k8s-openapi`; `tokio::process`; `tracing`; `thiserror`; SHA-256; Prometheus text parsing; JSON Patch; `reqwest`; static HTML rendering; `kind`; `kubectl`; `helm`; GitHub Actions.

**Spec:** `PRODUCT.md` and `docs/superpowers/specs/2026-08-29-admission-lab-design.md`

## Global Constraints

These constraints apply to every task unless a later approved spec changes them.

1. License is Apache-2.0. Admission Lab is fully free and open source; there are no paid tiers, proprietary core features, hosted SaaS requirements, accounts, billing, or commercial-cloud dependencies.
2. Rust-first. Core behavior lives in Rust crates. Existing tools such as `kind`, `kubectl`, and `helm` are invoked as bounded subprocesses rather than reimplemented.
3. Real Kubernetes API servers are authoritative. An in-process simulator may never replace authoritative CI results.
4. Baseline and candidate use separate ephemeral clusters by default and must not share mutable cluster state.
5. Default v1 flow requires no production kubeconfig and copies no production secrets.
6. Core is vendor-neutral. Recipes may provide install/readiness/normalization/capability metadata but may not contain regression classification logic.
7. Classification, first-divergence claims, and pass/fail decisions are deterministic. No LLM/AI is used for correctness in v1.
8. Public Alpha contains admission regression only. Gateway work cannot enter the critical path until the Alpha gate passes.
9. Public Beta adds Istio Gateway API, HTTP probes, GitHub Action integration, reproducible run manifests, and a versioned beta result schema.
10. v1.0 adds NGINX Gateway Fabric, a pinned legacy community `ingress-nginx` migration recipe when that archived release passes the primary supported Kubernetes integration job, Ingress-to-Gateway behavior comparison, schema stability, hardening, and support for the latest three upstream-supported Kubernetes minor versions at release time.
11. Kustomize, generated fuzz fixtures, production workload capture, `GRPCRoute`, server/history UI, AI explanation, Terraform, Argo CD integration, Slack bots, editor extensions, and generic Kubernetes dashboards are not prerequisites for v1.
12. External commands must use argv directly; never build shell command strings from user input.
13. Every external command has a timeout, separate stdout/stderr capture, version/provenance recording, and structured error context.
14. Reports redact Secret data, authorization headers, private keys, configured sensitive paths, and credential-like values controlled by Admission Lab rendering.
15. Missing observability data is represented as unavailable/unknown; it must never be fabricated or presented as proven causality.
16. The authoritative admission fixture execution mode for Alpha is Kubernetes server-side dry-run against a real API server. The response object is the final admitted/mutated object. A fixture that cannot be safely evaluated with server-side dry-run must fail explicitly as unsupported for that mode rather than silently switch semantics. Persisted fixture mode is a post-Alpha extension unless a certified recipe requires it to reproduce a known regression.
17. Alpha fixture execution is serial within each cluster. This makes audit-log correlation deterministic. Parallel fixture execution is allowed only after request-level correlation is implemented and tested.
18. Audit logging is configured at `Request` level for Admission Lab fixture requests because Kubernetes exposes mutating-webhook invocation annotations at `Metadata` or higher and patch annotations at `Request` or higher.
19. Per-webhook latency is treated as an optional observed signal. When collected, it comes from isolated kube-apiserver admission webhook metric deltas around serial fixture requests; absent or ambiguous metrics never fail a run by themselves.
20. A phase is not complete because code exists. Its exit gate and verification commands must pass in a clean clone/CI environment.

---

## 0. How Agents Must Execute This Roadmap

### 0.1 Task discipline

- Implement tasks in dependency order.
- Each task is a reviewable unit with its own failing test, minimal implementation, verification, and commit.
- Do not combine tasks merely because they touch nearby files.
- Do not start a later phase while the current phase exit gate is red.
- If implementation discovers a spec ambiguity that changes public behavior, stop and amend `PRODUCT.md` before coding the behavioral change.
- If a dependency or Kubernetes release has changed since this roadmap was written, use the latest stable compatible release, pin it in the lockfile/config, and record the decision in the task commit. Do not silently change architecture.

### 0.2 Mandatory verification after every task

Run the narrowest test first, then the workspace gate when the task is ready to commit:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Tasks that require Docker/kind add their stated integration command. Never require `kind` for pure unit-test crates.

### 0.3 Commit convention

Use one focused commit per task:

```text
build: bootstrap Rust workspace policy
feat(spec): add strict v1alpha1 lab configuration
feat(cluster): add isolated kind lifecycle
feat(installer): add Helm installation backend
feat(fixtures): discover and hash deterministic fixtures
feat(admission): capture real admission behavior per fixture
feat(diff): classify workload mutation semantics
feat(policy): evaluate deterministic regression severity
feat(report): generate standalone HTML artifact
feat(gateway): classify Gateway behavior regressions
test: add canonical admission regression corpus
docs: publish Public Beta usage and contracts
ci: add tiered Kubernetes recipe certification matrix
fix: preserve cleanup after candidate install failure
```

### 0.4 Definition of a lab result

All code must keep these outcomes separate:

```rust
pub enum RunDisposition {
    Passed,
    PolicyFailed,
    InvalidInput,
    InfrastructureFailed,
    InstallationFailed,
    FixtureFailed,
    InternalError,
}
```

CLI exit-code mapping is frozen only at the v1 contract task, but the intended values are:

```text
0 = completed, policy passed
1 = completed, regression policy failed
2 = invalid user configuration / fixture definition
3 = lab infrastructure failure
4 = installation/readiness failure
5 = fixture execution/capture failure
6 = internal Admission Lab error
```

---

# 1. Final Repository Map

The roadmap targets this structure. Agents should create directories only when the first task that owns them begins.

```text
admission-lab/
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
├── LICENSE
├── README.md
├── PRODUCT.md
├── ROADMAP.md
├── CONTRIBUTING.md
├── SECURITY.md
├── CODE_OF_CONDUCT.md
├── deny.toml
├── .gitignore
├── .github/
│   ├── workflows/
│   │   ├── ci.yml
│   │   ├── integration.yml
│   │   ├── nightly.yml
│   │   ├── release.yml
│   │   └── recipe-matrix.yml
│   └── actions/admissionlab/
│       └── action.yml
├── crates/
│   ├── admissionlab-cli/
│   │   └── src/{main.rs,commands/,exit.rs,output.rs}
│   ├── admissionlab-core/
│   │   └── src/{lib.rs,run.rs,side.rs,ids.rs,error.rs,artifact.rs}
│   ├── admissionlab-spec/
│   │   └── src/{lib.rs,model.rs,load.rs,validate.rs,resolve.rs,schema.rs}
│   ├── admissionlab-cluster/
│   │   └── src/{lib.rs,kind.rs,config.rs,lifecycle.rs,kubeconfig.rs,audit.rs,diagnostics.rs}
│   ├── admissionlab-installer/
│   │   └── src/{lib.rs,model.rs,helm.rs,manifests.rs,readiness.rs}
│   ├── admissionlab-fixtures/
│   │   └── src/{lib.rs,discover.rs,identity.rs,load.rs,execute.rs,hash.rs}
│   ├── admissionlab-admission/
│   │   └── src/{lib.rs,execute.rs,outcome.rs,audit_reader.rs,trace.rs,metrics.rs,correlate.rs}
│   ├── admissionlab-normalize/
│   │   └── src/{lib.rs,object.rs,trace.rs,rules.rs,pointer.rs}
│   ├── admissionlab-diff/
│   │   └── src/{lib.rs,raw.rs,semantic.rs,admission.rs,divergence.rs,types.rs}
│   ├── admissionlab-policy/
│   │   └── src/{lib.rs,severity.rs,expectation.rs,evaluate.rs,selector.rs}
│   ├── admissionlab-report/
│   │   └── src/{lib.rs,model.rs,terminal.rs,json.rs,html.rs,redact.rs,templates/}
│   ├── admissionlab-recipes/
│   │   └── src/{lib.rs,model.rs,load.rs,catalog.rs,capability.rs}
│   ├── admissionlab-gateway/
│   │   └── src/{lib.rs,model.rs,reconcile.rs,conditions.rs,endpoint.rs,port_forward.rs,probe.rs,diff.rs}
│   ├── admissionlab-test-webhook/
│   │   └── src/{main.rs,server.rs,mutate.rs,validate.rs,behavior.rs}
│   └── admissionlab-echo/
│       └── src/main.rs
├── schemas/
│   ├── admissionlab-v1alpha1.json
│   ├── admissionlab-v1beta1.json
│   ├── result-v1beta1.json
│   └── run-manifest-v1beta1.json
├── recipes/
│   ├── kyverno/
│   ├── istio/
│   ├── istio-gateway/
│   ├── nginx-gateway-fabric/
│   └── ingress-nginx-legacy/
├── fixtures/
│   ├── core/
│   ├── kyverno/
│   ├── istio/
│   ├── gateway/
│   └── migration/
├── testdata/
│   ├── configs/
│   ├── manifests/
│   ├── audit/
│   ├── metrics/
│   ├── objects/
│   └── golden/
├── compatibility/
│   ├── kubernetes.yaml
│   └── recipes.yaml
├── examples/
│   ├── admission-basic/
│   ├── kyverno-istio-upgrade/
│   ├── gateway-istio/
│   ├── gateway-nginx/
│   └── ingress-to-gateway/
├── docs/
│   ├── architecture.md
│   ├── config.md
│   ├── fixtures.md
│   ├── recipes.md
│   ├── security.md
│   ├── github-action.md
│   ├── compatibility.md
│   ├── troubleshooting.md
│   ├── schema-migrations.md
│   └── superpowers/
│       ├── specs/
│       └── plans/
└── scripts/
    ├── verify-cleanup.sh
    ├── update-kubernetes-matrix.sh
    ├── build-test-images.sh
    └── verify-release.sh
```

## 1.1 Dependency direction

Keep dependency arrows one-way:

```text
cli -> core
cli -> spec
cli -> report

core -> spec
core -> cluster
core -> installer
core -> fixtures
core -> admission
core -> normalize
core -> diff
core -> policy
core -> report
core -> recipes
core -> gateway   # only used after Gateway feature is enabled

admission -> fixtures
admission -> cluster
normalize -> core domain types only
diff -> normalize/core domain types
policy -> diff
report -> policy/diff/core domain types
recipes -> spec/installer model only
gateway -> cluster/fixtures/core domain types
```

Do not let `recipes` depend on `diff` or `policy`. Do not let report rendering decide severity. Do not let CLI duplicate orchestration logic.

## 1.2 Cross-task type registry

The following names are canonical. Later tasks may add fields, but they must not rename these types or change existing field meaning without updating every dependent task and the product spec when behavior is public.

```rust
// admissionlab-spec ownership
pub struct LoadedLab {
    pub source_path: PathBuf,
    pub raw: LabSpec,
}

pub struct ResolvedLab {
    pub source_path: PathBuf,
    pub baseline: ResolvedEnvironment,
    pub candidate: ResolvedEnvironment,
    pub fixtures: ResolvedFixtureSelection,
    pub policy: PolicySpec,
    pub expectations_file: Option<PathBuf>,
    pub gateway: Option<GatewaySuiteSpec>,
    pub migration: Option<MigrationSuiteSpec>,
}

pub struct ResolvedEnvironment {
    pub kubernetes: String,
    pub components: Vec<ResolvedComponent>,
}

pub struct ComponentSpec {
    pub name: Option<String>,
    pub recipe: Option<String>,
    pub version: Option<String>,
    pub install: Option<InstallMethodSpec>,
}

pub struct ResolvedComponent {
    pub name: String,
    pub version: String,
    pub install: InstallMethod,
    pub readiness: Vec<ReadinessCheck>,
    pub recipe_normalize_rules: Vec<RecipeNormalizeRule>,
    pub capabilities: BTreeSet<Capability>,
}

pub struct FixtureSelectionSpec {
    pub include: Vec<String>,
}

pub struct ResolvedFixtureSelection {
    pub include: Vec<globset::Glob>,
    pub root: PathBuf,
}

pub struct PolicySpec {
    pub fail_on: BTreeSet<String>,
    pub overrides: Vec<PolicyOverrideSpec>,
    pub latency: LatencyPolicy,
}

pub struct PolicyOverrideSpec {
    pub kind: String,
    pub fixtures: Option<String>,
    pub subject: Option<String>,
    pub path: Option<String>,
    pub severity: String,
}

// admissionlab-installer / recipe ownership
pub struct ReadinessEvidence {
    pub check: ReadinessCheck,
    pub satisfied: bool,
    pub last_observed: Option<serde_json::Value>,
    pub elapsed: Duration,
}

pub enum RecipeNormalizeRule {
    RemovePointer(String),
    RemoveAnnotation(String),
    SortNamedArray { pointer: String, key: String },
}

// admissionlab-normalize ownership
pub struct NormalizedObject {
    pub value: serde_json::Value,
    pub evidence: NormalizationEvidence,
}

pub struct NormalizationEvidence {
    pub applied_rules: Vec<String>,
    pub warnings: Vec<String>,
}

pub struct NormalizedWebhookInvocation {
    pub configuration: String,
    pub webhook: String,
    pub round: u32,
    pub index: u32,
    pub mutated: Option<bool>,
    pub patch: Option<Vec<json_patch::PatchOperation>>,
    pub latency: Option<Duration>,
    pub outcome: WebhookOutcome,
}

pub struct LatencyPolicy {
    pub absolute_increase: Duration,
    pub relative_multiplier: f64,
}

// admissionlab-policy ownership
pub struct ChangeSelector {
    pub fixture_glob: Option<String>,
    pub subject: Option<String>,
    pub object_path: Option<String>,
}

pub struct StaleExpectation {
    pub id: String,
    pub reason: String,
}

// admissionlab-report ownership
pub struct RunSummary {
    pub fixtures_total: usize,
    pub identical: usize,
    pub expected: usize,
    pub warnings: usize,
    pub critical: usize,
    pub inconclusive: usize,
}

pub struct EnvironmentSummary {
    pub baseline: EnvironmentReport,
    pub candidate: EnvironmentReport,
}

pub struct EnvironmentReport {
    pub kubernetes: String,
    pub components: Vec<ComponentReport>,
}

pub struct ComponentReport {
    pub name: String,
    pub version: String,
}

pub struct FixtureComparison {
    pub fixture_id: FixtureId,
    pub admission: Option<AdmissionComparison>,
    pub gateway: Option<GatewayCaseComparison>,
    pub changes: Vec<ClassifiedChange>,
}

pub struct AdmissionComparison {
    pub baseline: AdmissionOutcome,
    pub candidate: AdmissionOutcome,
    pub first_divergence: Option<DivergenceEvidence>,
}

// provenance ownership
pub struct HostProvenance {
    pub os: String,
    pub arch: String,
}

pub struct ToolProvenance {
    pub kind: String,
    pub kubectl: String,
    pub helm: String,
    pub docker: String,
}

pub struct EnvironmentProvenance {
    pub kubernetes_version: String,
    pub node_image: String,
    pub node_image_digest: String,
    pub components: Vec<ComponentProvenance>,
}

pub struct ComponentProvenance {
    pub name: String,
    pub version: String,
    pub source_sha256: Option<String>,
}

pub struct VerifiedInput {
    pub path: PathBuf,
    pub expected_sha256: String,
    pub actual_sha256: String,
}

// process spawning ownership used by long-lived port-forward
#[async_trait]
pub trait ProcessSpawner: Send + Sync {
    async fn spawn(&self, spec: CommandSpec) -> Result<ManagedChild, ProcessError>;
}

pub struct ManagedChild {
    pub id: u32,
    // private child handle and bounded stdout/stderr readers
}

// Gateway ownership
pub struct GatewayIdentity {
    pub namespace: String,
    pub name: String,
}

pub struct ParentIdentity {
    pub namespace: Option<String>,
    pub name: String,
    pub section_name: Option<String>,
}

pub struct GatewayClassEvidence {
    pub name: String,
    pub accepted: ObservedCondition,
}

pub struct GatewayEvidence {
    pub identity: GatewayIdentity,
    pub conditions: BTreeMap<String, ObservedCondition>,
    pub generation: i64,
}

pub struct RouteEvidence {
    pub namespace: String,
    pub name: String,
    pub generation: i64,
    pub parents: Vec<RouteParentStatus>,
}

pub struct GatewayCaseResult {
    pub contract_id: String,
    pub reconciliation: ReconciliationEvidence,
    pub probes: Vec<HttpProbeResult>,
}

pub struct GatewayCaseComparison {
    pub baseline: GatewayCaseResult,
    pub candidate: GatewayCaseResult,
}

pub struct ProbePair {
    pub contract_id: String,
    pub baseline: HttpProbeResult,
    pub candidate: HttpProbeResult,
}

pub struct NonPortableFeatureExpectation {
    pub feature: String,
    pub reason: String,
}

pub struct MigrationBehaviorChange {
    pub kind: MigrationBehaviorKind,
    pub detail: String,
    pub expected: bool,
}

pub struct SensitiveBytes(Vec<u8>);
```

Ownership rule: if a type is owned by a later-phase crate (for example `GatewaySuiteSpec`), pre-Gateway builds may keep the field behind a Cargo feature or schema enum representation, but the canonical name is reserved. Do not create a competing synonym such as `GatewayTestSpec` and migrate later.

---

# 2. Critical-Path Milestones

| Phase | Deliverable | Public status | Hard gate |
|---|---|---|---|
| 0 | Reproducible Rust repo + CI + domain skeleton | internal | clean workspace gates |
| 1 | Config, doctor, process runner, two-cluster lifecycle | internal | 100 create/delete loops without leak |
| 2 | Generic installation + readiness + recipes foundation | internal | deterministic install on both sides |
| 3 | Fixture + admission capture + dogfood webhook | internal | known allow/deny/mutate/timeout captured |
| 4 | Normalize + semantic diff + policy + first divergence + reports | **Public Alpha** | canonical admission regression demo passes |
| 5 | CI integration + run manifest + reproducibility + performance | Alpha hardening | GitHub workflow artifact/repro contract |
| 6 | Gateway reconciliation + Istio Gateway + HTTP data-plane probes | **Public Beta** | canonical Gateway regression demo passes |
| 7 | Beta schema freeze + compatibility matrix + docs | Public Beta | beta compatibility/release gate |
| 8 | NGINX Gateway Fabric + legacy ingress + migration behavior suite | v1 RC feature-complete | two Gateway implementations certified |
| 9 | Security/reliability/performance/schema hardening | v1 RC | release verification passes |
| 10 | v1.0 release | **v1.0** | stable contracts + latest 3 K8s minors |
| Post-v1 | optional expansion | no commitment | adoption-driven |

Critical path:

```text
0 -> 1 -> 2 -> 3 -> 4 -> 5 -> 6 -> 7 -> 8 -> 9 -> 10
```

Within a phase, tasks marked **[PARALLEL]** may be assigned to different agents only after their shared dependency task lands.

---

# PHASE 0 — Repository Foundation and Contract Skeleton

**Goal:** Produce a clean Rust workspace whose crate boundaries, error model, IDs, logging, CI, licensing, and contributor guardrails are stable enough for all later agents.

**Exit artifact:** `cargo test --workspace` works in a clean clone; the CLI prints a version/help page; all listed crates compile with intentionally minimal APIs.

## Task 0.1 — Bootstrap workspace, toolchain, licensing, and dependency policy

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `LICENSE`
- Create: `deny.toml`
- Create: `.gitignore`
- Create: `CONTRIBUTING.md`
- Create: `SECURITY.md`
- Create: `CODE_OF_CONDUCT.md`

**Interfaces:**
- Produces workspace membership and repository-wide lint/profile settings consumed by every later task.

- [ ] **Step 1: Write the workspace manifest with the approved crate list**

```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
edition = "2024"
license = "Apache-2.0"

[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
all = "warn"
pedantic = "warn"

[profile.release]
lto = "thin"
codegen-units = 1
strip = "symbols"
```

The repository URL may be replaced once the real GitHub owner is known; this is package metadata only and must not block compilation.

- [ ] **Step 2: Pin a stable Rust toolchain in `rust-toolchain.toml`**

Use the stable toolchain available when implementation starts and commit the exact resolved channel string rather than `stable` after bootstrap validation:

```toml
[toolchain]
channel = "1.98.0"
components = ["rustfmt", "clippy"]
profile = "minimal"
```

This roadmap pins Rust `1.98.0`, the stable release current when the plan was authored. A later deliberate toolchain upgrade requires its own reviewed dependency/toolchain commit and full workspace verification; implementation must not silently float to a newer compiler.

- [ ] **Step 3: Add Apache-2.0 license and dependency-license allowlist**

`deny.toml` must allow permissive licenses compatible with Apache-2.0 and deny unknown git sources by default. Add exceptions only with a comment explaining why.

- [ ] **Step 4: Add contributor guardrails copied from `PRODUCT.md`**

`CONTRIBUTING.md` must explicitly state: local-first, real-cluster authoritative, vendor-neutral core, deterministic decisions, no proprietary gating, and “what regression does this catch?” as the feature test.

- [ ] **Step 5: Verify repository metadata**

Run:

```bash
cargo metadata --no-deps --format-version 1
```

Expected: valid workspace metadata and zero missing members after Task 0.2 creates crate skeletons.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml rust-toolchain.toml LICENSE deny.toml .gitignore CONTRIBUTING.md SECURITY.md CODE_OF_CONDUCT.md
git commit -m "build: bootstrap Rust workspace policy"
```

**Acceptance criteria:** Apache-2.0 is explicit; unsafe Rust is forbidden workspace-wide; exact Rust toolchain is committed; contributor docs reject SaaS/proprietary scope and unrelated platform features.

## Task 0.2 — Create crate skeletons with one-responsibility boundaries

**Files:**
- Create each `crates/*/Cargo.toml`
- Create each `crates/*/src/lib.rs` or `main.rs`

**Interfaces:**
- Produces the crate names and dependency graph defined in Section 1.1.

- [ ] **Step 1: Create library crates**

Create these package names exactly:

```text
admissionlab-core
admissionlab-spec
admissionlab-cluster
admissionlab-installer
admissionlab-fixtures
admissionlab-admission
admissionlab-normalize
admissionlab-diff
admissionlab-policy
admissionlab-report
admissionlab-recipes
admissionlab-gateway
```

Each starts with:

```rust
#![forbid(unsafe_code)]
```

- [ ] **Step 2: Create binary crates**

Create:

```text
admissionlab-cli       -> binary name `admissionlab`
admissionlab-test-webhook -> binary name `admissionlab-test-webhook`
admissionlab-echo      -> binary name `admissionlab-echo`
```

- [ ] **Step 3: Enforce allowed initial dependencies**

At this task, `admissionlab-cli` may depend on `admissionlab-core`; other core crates must not depend on the CLI. Add only dependencies actually needed by skeleton code.

- [ ] **Step 4: Add a crate-boundary smoke test**

Create `crates/admissionlab-core/tests/workspace_smoke.rs`:

```rust
#[test]
fn core_crate_is_linkable() {
    assert_eq!(admissionlab_core::crate_identity(), "admissionlab-core");
}
```

Implement:

```rust
pub const fn crate_identity() -> &'static str {
    "admissionlab-core"
}
```

- [ ] **Step 5: Verify**

```bash
cargo check --workspace --all-targets
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates Cargo.toml Cargo.lock
git commit -m "build: create Admission Lab crate boundaries"
```

**Acceptance criteria:** every planned crate exists; no cyclic dependency; no vendor-specific logic appears in generic crates.

## Task 0.3 — Define core IDs, side, run disposition, and artifact paths

**Files:**
- Create: `crates/admissionlab-core/src/side.rs`
- Create: `crates/admissionlab-core/src/ids.rs`
- Create: `crates/admissionlab-core/src/error.rs`
- Create: `crates/admissionlab-core/src/artifact.rs`
- Modify: `crates/admissionlab-core/src/lib.rs`
- Test: `crates/admissionlab-core/tests/domain.rs`

**Interfaces:**
- Produces:

```rust
pub enum Side { Baseline, Candidate }
pub struct RunId(String);
pub struct FixtureId(String);
pub enum RunDisposition { Passed, PolicyFailed, InvalidInput, InfrastructureFailed, InstallationFailed, FixtureFailed, InternalError }
pub struct RunPaths { /* typed paths */ }
```

- [ ] **Step 1: Write failing tests for stable display/parse behavior**

```rust
#[test]
fn side_names_are_stable() {
    assert_eq!(Side::Baseline.as_str(), "baseline");
    assert_eq!(Side::Candidate.as_str(), "candidate");
}

#[test]
fn run_id_rejects_path_separators() {
    assert!(RunId::parse("abc/def").is_err());
}
```

- [ ] **Step 2: Run tests and confirm failure**

```bash
cargo test -p admissionlab-core --test domain
```

Expected: compile/test failure because types do not exist.

- [ ] **Step 3: Implement minimal domain types**

IDs must allow ASCII lowercase letters, digits, and `-`; generation uses a random UUID/ULID rendered lowercase, but parsing never accepts `/`, `\\`, `..`, or whitespace.

`RunPaths::new(root, run_id)` returns canonical children for `raw/`, `normalized/`, `reports/`, `logs/`, `kubeconfigs/`, and `run.json` without touching the filesystem.

- [ ] **Step 4: Run tests**

```bash
cargo test -p admissionlab-core --test domain
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/admissionlab-core
git commit -m "feat(core): define run identity and disposition types"
```

**Acceptance criteria:** IDs are safe for filenames/cluster suffixes; side names are stable serialization values; no IO is hidden in path constructors.

## Task 0.4 — Add structured logging and secret-safe diagnostic fields

**Files:**
- Create: `crates/admissionlab-core/src/diagnostic.rs`
- Modify: `crates/admissionlab-core/src/lib.rs`
- Create: `crates/admissionlab-cli/src/output.rs`
- Test: `crates/admissionlab-core/tests/diagnostic.rs`

**Interfaces:**

```rust
pub enum DiagnosticLevel { Info, Warning, Error }
pub struct Diagnostic { pub code: String, pub message: String, pub context: BTreeMap<String, RedactedValue> }
pub enum RedactedValue { Public(String), Sensitive }
```

- [ ] **Step 1: Write a failing serialization test proving sensitive values never serialize**

```rust
#[test]
fn sensitive_context_serializes_as_redacted() {
    let value = RedactedValue::Sensitive;
    assert_eq!(serde_json::to_string(&value).unwrap(), r#""[REDACTED]""#);
}
```

- [ ] **Step 2: Implement diagnostic types and `tracing` initialization**

CLI logging supports `--verbose` and `RUST_LOG`; default output must not print debug-level raw Kubernetes objects.

- [ ] **Step 3: Verify**

```bash
cargo test -p admissionlab-core --test diagnostic
```

- [ ] **Step 4: Commit**

```bash
git add crates/admissionlab-core crates/admissionlab-cli
git commit -m "feat(core): add structured secret-safe diagnostics"
```

## Task 0.5 — Add CLI skeleton and version contract

**Files:**
- Create: `crates/admissionlab-cli/src/main.rs`
- Create: `crates/admissionlab-cli/src/commands/mod.rs`
- Create: `crates/admissionlab-cli/src/commands/doctor.rs`
- Create: `crates/admissionlab-cli/src/commands/test.rs`
- Create: `crates/admissionlab-cli/src/exit.rs`
- Test: `crates/admissionlab-cli/tests/cli.rs`

**Interfaces:**

```text
admissionlab --version
admissionlab doctor [--deep]
admissionlab test <CONFIG> [--keep-clusters]
```

At this stage `doctor` and `test` may return a typed “not implemented in current phase” internal status only in tests; public execution of `test` must not pretend to run a lab.

- [ ] **Step 1: Write CLI parsing tests using `assert_cmd`**

```rust
#[test]
fn help_lists_core_commands() {
    let mut cmd = assert_cmd::Command::cargo_bin("admissionlab").unwrap();
    cmd.arg("--help").assert().success()
        .stdout(predicates::str::contains("doctor"))
        .stdout(predicates::str::contains("test"));
}
```

- [ ] **Step 2: Implement Clap structs and version output**

`--version` uses package version; commands are kebab-case; config paths are `PathBuf`.

- [ ] **Step 3: Verify**

```bash
cargo test -p admissionlab-cli --test cli
cargo run -p admissionlab-cli -- --help
```

- [ ] **Step 4: Commit**

```bash
git add crates/admissionlab-cli
git commit -m "feat(cli): add Admission Lab command skeleton"
```

## Task 0.6 — Establish CI quality gates

**Files:**
- Create: `.github/workflows/ci.yml`
- Create: `.github/workflows/integration.yml`

**Interfaces:** Pull requests receive fast Rust checks; kind-dependent checks are separate and can be retried without hiding unit failures.

- [ ] **Step 1: Add fast CI job**

The job runs:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check
```

Cache Cargo registry/git/target safely keyed by lockfile and toolchain.

- [ ] **Step 2: Add empty-but-valid integration workflow trigger**

It should run on PRs touching `crates/admissionlab-{cluster,installer,admission,gateway}/**`, `recipes/**`, `fixtures/**`, or `testdata/**`; the first real integration task fills its steps.

- [ ] **Step 3: Validate workflow syntax with GitHub's parser or `actionlint` locally if installed**

```bash
actionlint .github/workflows/*.yml
```

If `actionlint` is unavailable locally, CI validation is authoritative; do not add it as a required runtime dependency.

- [ ] **Step 4: Commit**

```bash
git add .github
git commit -m "ci: establish Rust quality gates"
```

## Phase 0 Exit Gate

Run from a clean clone:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check
cargo run -p admissionlab-cli -- --version
cargo run -p admissionlab-cli -- --help
```

**Must be true:**
- no unresolved placeholders or deferred public-contract decisions;
- all crates compile;
- license and contributor guardrails are present;
- CLI/help works;
- no kind/Docker is required for Phase 0 tests.

---

# PHASE 1 — Spec, Process Runner, Doctor, and Two-Cluster Lifecycle

**Goal:** Turn the skeleton into a deterministic lab launcher that validates config, checks prerequisites, creates baseline/candidate kind clusters with audit logging, captures diagnostics, and always cleans up.

## Task 1.1 — Define `v1alpha1` configuration model and strict loader

**Files:**
- Create: `crates/admissionlab-spec/src/model.rs`
- Create: `crates/admissionlab-spec/src/load.rs`
- Create: `crates/admissionlab-spec/src/validate.rs`
- Create: `crates/admissionlab-spec/src/resolve.rs`
- Modify: `crates/admissionlab-spec/src/lib.rs`
- Test: `crates/admissionlab-spec/tests/load.rs`
- Testdata: `testdata/configs/{minimal-valid,unknown-field,missing-candidate}.yaml`

**Interfaces:**

```rust
pub struct LabSpec {
    pub api_version: String,
    pub kind: String,
    pub baseline: EnvironmentSpec,
    pub candidate: EnvironmentSpec,
    pub fixtures: FixtureSelectionSpec,
    pub policy: PolicySpec,
    pub expectations_file: Option<PathBuf>,
}

pub struct EnvironmentSpec {
    pub kubernetes: String,
    pub components: Vec<ComponentSpec>,
}

pub fn load_lab(path: &Path) -> Result<LoadedLab, SpecError>;
pub fn resolve_lab(loaded: LoadedLab) -> Result<ResolvedLab, SpecError>;
```

- [ ] **Step 1: Write failing parse tests**

Require `apiVersion: admissionlab.io/v1alpha1`, `kind: Lab`, baseline, candidate, fixtures. Use `#[serde(deny_unknown_fields)]` on user-facing structs where forward compatibility does not require extension maps.

- [ ] **Step 2: Verify failures**

```bash
cargo test -p admissionlab-spec --test load
```

Expected: missing implementation or assertions fail.

- [ ] **Step 3: Implement strict YAML loading**

Errors include file path and serde path; never silently ignore misspelled keys such as `candiate`.

- [ ] **Step 4: Implement path resolution**

All relative paths resolve from the config file directory, not current working directory. Preserve both original and resolved paths for diagnostics.

- [ ] **Step 5: Add validation**

Reject:
- baseline/candidate Kubernetes version empty;
- duplicate component names after resolution;
- fixture include list empty;
- absolute output paths that escape an explicitly configured run root only when such restriction is enabled later; do not invent production access.

- [ ] **Step 6: Verify and commit**

```bash
cargo test -p admissionlab-spec --test load
git add crates/admissionlab-spec testdata/configs
git commit -m "feat(spec): add strict v1alpha1 lab configuration"
```

## Task 1.2 — Generate JSON Schema from Rust model

**Files:**
- Create: `crates/admissionlab-spec/src/schema.rs`
- Create: `schemas/admissionlab-v1alpha1.json`
- Test: `crates/admissionlab-spec/tests/schema.rs`

**Interfaces:**

```rust
pub fn v1alpha1_json_schema() -> schemars::Schema;
```

- [ ] **Step 1: Write a test that regenerates the schema and compares it byte-for-byte after canonical pretty serialization**

- [ ] **Step 2: Implement `JsonSchema` derivations and deterministic schema generation**

- [ ] **Step 3: Add a small generator command in tests or `xtask`-style test utility**

Do not add a separate `xtask` crate unless needed by more than one generation task.

- [ ] **Step 4: Verify**

```bash
cargo test -p admissionlab-spec --test schema
```

Expected: checked-in schema equals generated schema.

- [ ] **Step 5: Commit**

```bash
git add crates/admissionlab-spec schemas/admissionlab-v1alpha1.json
git commit -m "feat(spec): publish v1alpha1 JSON schema"
```

## Task 1.3 — Build safe external process runner

**Files:**
- Create: `crates/admissionlab-core/src/process.rs`
- Modify: `crates/admissionlab-core/src/lib.rs`
- Test: `crates/admissionlab-core/tests/process.rs`

**Interfaces:**

```rust
pub struct CommandSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<OsString, OsString>,
    pub timeout: Duration,
}

pub struct CommandResult {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub elapsed: Duration,
}

#[async_trait]
pub trait ProcessRunner: Send + Sync {
    async fn run(&self, spec: CommandSpec) -> Result<CommandResult, ProcessError>;
}
```

- [ ] **Step 1: Write tests for argv preservation, timeout, cwd, and separate streams**

Use the current test binary/helper mode rather than shell commands so tests work cross-platform.

- [ ] **Step 2: Implement with `tokio::process::Command`**

Never invoke `sh -c`, `bash -c`, PowerShell command strings, or string concatenation for arguments.

- [ ] **Step 3: Add redacted diagnostic rendering**

Environment values marked sensitive may be passed to the child but must render as `[REDACTED]` in diagnostics.

- [ ] **Step 4: Verify and commit**

```bash
cargo test -p admissionlab-core --test process
git add crates/admissionlab-core
git commit -m "feat(core): add bounded external process runner"
```

## Task 1.4 — Implement tool discovery and `admissionlab doctor`

**Files:**
- Create: `crates/admissionlab-core/src/tool.rs`
- Modify: `crates/admissionlab-cli/src/commands/doctor.rs`
- Test: `crates/admissionlab-cli/tests/doctor.rs`

**Interfaces:**

```rust
pub enum ToolName { Kind, Kubectl, Helm, Docker }
pub struct ToolStatus { pub name: ToolName, pub found: bool, pub version: Option<String>, pub diagnostic: Option<String> }
pub struct DoctorReport { pub tools: Vec<ToolStatus>, pub docker_reachable: bool, pub disk_warning: Option<String> }
```

- [ ] **Step 1: Write tests using a fake `ProcessRunner`**

Cases: all present; missing kind; malformed version output; Docker daemon unreachable.

- [ ] **Step 2: Implement version probes**

Use argv calls equivalent to:

```text
kind version
kubectl version --client=true --output=json
helm version --template {{.Version}}
docker version --format {{json .Server.Version}}
```

- [ ] **Step 3: Implement shallow doctor output**

Missing required tool => doctor exits non-zero only when requested as a prerequisite check by `test`; interactive `doctor` prints a summary and returns code 2 for invalid host prerequisites.

- [ ] **Step 4: Implement deferred `--deep` behavior as a real cluster create/delete probe only after Task 1.9; until then hide the flag from release builds via feature/test wiring rather than pretending it works.**

- [ ] **Step 5: Verify and commit**

```bash
cargo test -p admissionlab-cli --test doctor
git add crates/admissionlab-core crates/admissionlab-cli
git commit -m "feat(cli): add prerequisite doctor checks"
```

## Task 1.5 — Build run workspace creation and atomic artifact writes

**Files:**
- Modify: `crates/admissionlab-core/src/artifact.rs`
- Test: `crates/admissionlab-core/tests/artifact.rs`

**Interfaces:**

```rust
pub struct ArtifactStore { root: PathBuf }
impl ArtifactStore {
    pub async fn create_run(&self, id: &RunId) -> Result<RunPaths, ArtifactError>;
    pub async fn write_json_atomic<T: Serialize>(&self, path: &Path, value: &T) -> Result<(), ArtifactError>;
    pub async fn write_bytes_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), ArtifactError>;
}
```

- [ ] **Step 1: Test path safety and atomic rename behavior**

- [ ] **Step 2: Implement directories with owner-only permissions where supported**

On Unix, kubeconfig/raw sensitive workspace directories should be `0700`; files containing kubeconfig should be `0600`.

- [ ] **Step 3: Verify no partial JSON file after simulated write failure**

- [ ] **Step 4: Commit**

```bash
git add crates/admissionlab-core
git commit -m "feat(core): add safe run artifact store"
```

## Task 1.6 — Generate kind config with audit logging

**Files:**
- Create: `crates/admissionlab-cluster/src/config.rs`
- Create: `crates/admissionlab-cluster/src/audit.rs`
- Test: `crates/admissionlab-cluster/tests/kind_config.rs`
- Golden: `testdata/golden/kind-config-audit.yaml`

**Interfaces:**

```rust
pub struct KindClusterConfigInput {
    pub name: String,
    pub node_image: String,
    pub audit_policy_host_path: PathBuf,
    pub audit_log_host_dir: PathBuf,
}

pub fn render_kind_config(input: &KindClusterConfigInput) -> Result<String, ClusterConfigError>;
pub fn render_audit_policy() -> String;
```

- [ ] **Step 1: Write golden test**

The rendered kind config must mount an audit policy and configure kube-apiserver with:

```yaml
apiServer:
  extraArgs:
    audit-log-path: /var/log/kubernetes/kube-apiserver-audit.log
    audit-policy-file: /etc/kubernetes/policies/admissionlab-audit-policy.yaml
  extraVolumes:
    - name: audit-policies
      hostPath: /etc/kubernetes/policies
      mountPath: /etc/kubernetes/policies
      readOnly: true
      pathType: DirectoryOrCreate
    - name: audit-logs
      hostPath: /var/log/kubernetes
      mountPath: /var/log/kubernetes
      readOnly: false
      pathType: DirectoryOrCreate
```

The kind node also gets `extraMounts` from the host audit-policy path into `/etc/kubernetes/policies/admissionlab-audit-policy.yaml`.

- [ ] **Step 2: Render an audit policy at `Request` level**

Omit `RequestReceived` stage to reduce duplicate volume. At minimum record create/update/delete admission-relevant resource requests. Keep health/discovery noise at `None` or `Metadata`; do not log Secrets at `Request` level.

- [ ] **Step 3: Add a security test ensuring Secret resources are excluded from request-body logging**

- [ ] **Step 4: Verify and commit**

```bash
cargo test -p admissionlab-cluster --test kind_config
git add crates/admissionlab-cluster testdata/golden/kind-config-audit.yaml
git commit -m "feat(cluster): render kind audit configuration"
```

**Reference behavior:** kind supports kubeadm config patches and host mounts for kube-apiserver audit policy/logs; mutating webhook patch annotations require `Request` audit level.

## Task 1.7 — Implement kind lifecycle and kubeconfig isolation

**Files:**
- Create: `crates/admissionlab-cluster/src/kind.rs`
- Create: `crates/admissionlab-cluster/src/lifecycle.rs`
- Create: `crates/admissionlab-cluster/src/kubeconfig.rs`
- Modify: `crates/admissionlab-cluster/src/lib.rs`
- Test: `crates/admissionlab-cluster/tests/lifecycle_unit.rs`

**Interfaces:**

```rust
pub struct ClusterSpec { pub side: Side, pub name: String, pub kubernetes_version: String, pub node_image: String }
pub struct ClusterHandle { pub spec: ClusterSpec, pub kubeconfig: PathBuf, pub audit_log: PathBuf }

#[async_trait]
pub trait ClusterManager {
    async fn create(&self, spec: &ClusterSpec, paths: &RunPaths) -> Result<ClusterHandle, ClusterError>;
    async fn delete(&self, handle: &ClusterHandle) -> Result<(), ClusterError>;
    async fn diagnostics(&self, handle: &ClusterHandle) -> ClusterDiagnostics;
}
```

- [ ] **Step 1: Unit-test exact argv generated for create/delete**

Create uses a generated kind config and explicit kubeconfig path; delete uses the exact cluster name.

- [ ] **Step 2: Implement `KindClusterManager` through `ProcessRunner`**

- [ ] **Step 3: Verify cluster names**

Names must be `adlab-baseline-<short-run-id>` and `adlab-candidate-<short-run-id>` and fit kind/Docker naming constraints.

- [ ] **Step 4: Add rollback on partial create failure**

If kind reports a created node but kubeconfig export/health check fails, attempt deletion and preserve diagnostics regardless of cleanup outcome.

- [ ] **Step 5: Verify unit tests and commit**

```bash
cargo test -p admissionlab-cluster --test lifecycle_unit
git add crates/admissionlab-cluster
git commit -m "feat(cluster): add isolated kind lifecycle"
```

## Task 1.8 — Map Kubernetes minor versions to pinned kind node images

**Files:**
- Create: `compatibility/kubernetes.yaml`
- Create: `crates/admissionlab-cluster/src/version.rs`
- Test: `crates/admissionlab-cluster/tests/version.rs`

**Interfaces:**

```rust
pub struct KubernetesImageMatrix { pub releases: Vec<KubernetesImage> }
pub struct KubernetesImage { pub minor: String, pub version: String, pub image: String, pub digest: String, pub supported: bool }
pub fn resolve_node_image(requested: &str, matrix: &KubernetesImageMatrix) -> Result<ResolvedKubernetes, VersionError>;
```

- [ ] **Step 1: Populate the file with the latest three upstream-supported minors at implementation time**

Use official kind node image digests for exact patch releases. Commit exact versions/digests; no floating `latest` tags.

- [ ] **Step 2: Test unsupported minor and exact-match behavior**

- [ ] **Step 3: Make dropping a minor require a deliberate file change**

Do not compute support silently from network data during normal runs.

- [ ] **Step 4: Verify and commit**

```bash
cargo test -p admissionlab-cluster --test version
git add compatibility/kubernetes.yaml crates/admissionlab-cluster
git commit -m "feat(cluster): pin supported Kubernetes node images"
```

## Task 1.9 — Add real two-cluster integration smoke test and deep doctor

**Files:**
- Create: `crates/admissionlab-cluster/tests/kind_smoke.rs`
- Modify: `crates/admissionlab-cli/src/commands/doctor.rs`
- Modify: `.github/workflows/integration.yml`

**Interfaces:** `doctor --deep` creates one temporary cluster, verifies API health/audit log existence, then deletes it. The integration smoke test creates baseline and candidate concurrently, verifies distinct kubeconfigs/cluster UIDs, and deletes both.

- [ ] **Step 1: Write ignored integration test**

```rust
#[tokio::test]
#[ignore = "requires Docker and kind"]
async fn baseline_and_candidate_are_isolated() {
    // create both; assert API server UIDs differ; cleanup in finally-style guard
}
```

- [ ] **Step 2: Implement cleanup guard**

Dropping the guard cannot perform async cleanup reliably, so explicit orchestration must call cleanup; Drop may only emit a warning with an exact cleanup command if a handle survives.

- [ ] **Step 3: Implement deep doctor**

Run only when `--deep` is explicitly requested.

- [ ] **Step 4: Add CI integration job**

```bash
cargo test -p admissionlab-cluster --test kind_smoke -- --ignored --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add crates/admissionlab-cluster crates/admissionlab-cli .github/workflows/integration.yml
git commit -m "test(cluster): verify real kind isolation and deep doctor"
```

## Task 1.10 — Implement top-level cluster orchestration and guaranteed cleanup

**Files:**
- Create: `crates/admissionlab-core/src/run.rs`
- Modify: `crates/admissionlab-core/src/lib.rs`
- Modify: `crates/admissionlab-cli/src/commands/test.rs`
- Test: `crates/admissionlab-core/tests/run_lifecycle.rs`

**Interfaces:**

```rust
pub struct RunOptions { pub keep_clusters: bool, pub run_root: PathBuf }
pub struct LabRunner<C: ClusterManager> {
    pub cluster_manager: Arc<C>,
    pub artifact_store: ArtifactStore,
}

impl<C: ClusterManager> LabRunner<C> {
    pub async fn prepare_clusters(&self, lab: &ResolvedLab, options: &RunOptions) -> Result<PreparedLab, RunError>;
    pub async fn cleanup(&self, prepared: &PreparedLab) -> Vec<Diagnostic>;
}
```

- [ ] **Step 1: Test cleanup on baseline create failure, candidate create failure, and later simulated run failure**

- [ ] **Step 2: Implement orchestration**

Baseline/candidate cluster creation may run concurrently because they are isolated; later fixture execution remains serial per cluster.

- [ ] **Step 3: Implement `--keep-clusters`**

On preserve mode, print cluster names, kubeconfig paths, and exact `kind delete cluster --name adlab-baseline-<run>` / `adlab-candidate-<run>` commands.

- [ ] **Step 4: Verify and commit**

```bash
cargo test -p admissionlab-core --test run_lifecycle
git add crates/admissionlab-core crates/admissionlab-cli
git commit -m "feat(core): orchestrate two-cluster lab lifecycle"
```

## Phase 1 Exit Gate — 100-loop leak test

Create `scripts/verify-cleanup.sh` that runs a small supported node image and loops create/delete. For normal CI cost, run 10 iterations on PR/release candidates and 100 iterations manually/nightly before Public Alpha.

```bash
./scripts/verify-cleanup.sh 100
kind get clusters | grep '^adlab-' && exit 1 || true
```

Also run:

```bash
cargo run -p admissionlab-cli -- doctor --deep
```

**Must be true:** no leaked `adlab-*` cluster; each cluster has audit log file; kubeconfigs are isolated; failure diagnostics survive cleanup; invalid config exits before cluster creation.

---

# PHASE 2 — Generic Installation, Readiness, and Recipe Foundation

**Goal:** Install baseline/candidate component stacks deterministically through generic installers and curated metadata without embedding regression logic in recipes.

## Task 2.1 — Define generic component/install/readiness model

**Files:**
- Create: `crates/admissionlab-installer/src/model.rs`
- Modify: `crates/admissionlab-installer/src/lib.rs`
- Modify: `crates/admissionlab-spec/src/model.rs`
- Test: `crates/admissionlab-installer/tests/model.rs`

**Interfaces:**

```rust
pub enum InstallMethod {
    Helm(HelmInstallSpec),
    Manifests(ManifestInstallSpec),
}

pub struct HelmInstallSpec {
    pub repo_name: String,
    pub repo_url: String,
    pub chart: String,
    pub version: String,
    pub release_name: String,
    pub namespace: String,
    pub values_files: Vec<PathBuf>,
    pub set_values: BTreeMap<String, String>,
}

pub struct ManifestInstallSpec { pub paths: Vec<PathBuf> }

pub enum ReadinessCheck {
    DeploymentAvailable { namespace: String, name: String },
    DaemonSetReady { namespace: String, name: String },
    JobComplete { namespace: String, name: String },
    WebhookConfigurationPresent { name: String },
    CustomResourceCondition { api_version: String, kind: String, namespace: Option<String>, name: String, condition_type: String, status: String },
}
```

- [ ] **Step 1: Add strict spec tests for exactly-one install method and explicit versioned Helm chart**
- [ ] **Step 2: Implement model conversion from `admissionlab-spec` to installer model**
- [ ] **Step 3: Reject floating Helm versions in certified recipes**
- [ ] **Step 4: Verify and commit**

```bash
cargo test -p admissionlab-installer --test model
git add crates/admissionlab-installer crates/admissionlab-spec
git commit -m "feat(installer): define generic install and readiness contracts"
```

## Task 2.2 — Implement Helm installer

**Files:**
- Create: `crates/admissionlab-installer/src/helm.rs`
- Test: `crates/admissionlab-installer/tests/helm_unit.rs`

**Interfaces:**

```rust
#[async_trait]
pub trait ComponentInstaller {
    async fn install(&self, cluster: &ClusterHandle, component: &ResolvedComponent) -> Result<InstallRecord, InstallError>;
}

pub struct InstallRecord { pub component: String, pub method: String, pub resolved_version: String, pub started_at: SystemTime, pub elapsed: Duration, pub diagnostics: Vec<Diagnostic> }
```

- [ ] **Step 1: Test Helm argv generation**

Expected flow:

```text
helm repo add <name> <url> --force-update
helm upgrade --install "$RELEASE" "$CHART" --version "$VERSION" --namespace "$NAMESPACE" --create-namespace --kubeconfig "$KUBECONFIG" --values "$VALUES_FILE"
```

Values files are separate argv entries; `--set-string` is used for literal string overrides.

- [ ] **Step 2: Implement with explicit install timeout**
- [ ] **Step 3: Capture `helm get metadata`/release version when available**
- [ ] **Step 4: Verify and commit**

```bash
cargo test -p admissionlab-installer --test helm_unit
git add crates/admissionlab-installer
git commit -m "feat(installer): add Helm installation backend"
```

## Task 2.3 — Implement raw-manifest installer using structured discovery

**Files:**
- Create: `crates/admissionlab-installer/src/manifests.rs`
- Test: `crates/admissionlab-installer/tests/manifests_unit.rs`

**Interfaces:**

```rust
pub struct ManifestBundle { pub documents: Vec<serde_json::Value>, pub source_hash: String }
pub fn load_manifest_bundle(paths: &[PathBuf]) -> Result<ManifestBundle, InstallError>;
```

- [ ] **Step 1: Test multi-document YAML, JSON, deterministic path order, and duplicate source files**
- [ ] **Step 2: Parse locally before invoking cluster operations**
- [ ] **Step 3: Apply through `kubectl apply --server-side=false -f <resolved-file>` initially**

Use the bounded process runner. Store source hashes. Do not use shell pipes/stdin interpolation from user text.

- [ ] **Step 4: Verify and commit**

```bash
cargo test -p admissionlab-installer --test manifests_unit
git add crates/admissionlab-installer
git commit -m "feat(installer): add raw manifest backend"
```

## Task 2.4 — Implement Kubernetes readiness checks

**Files:**
- Create: `crates/admissionlab-installer/src/readiness.rs`
- Test: `crates/admissionlab-installer/tests/readiness_unit.rs`

**Interfaces:**

```rust
#[async_trait]
pub trait ReadinessProbe {
    async fn wait(&self, cluster: &ClusterHandle, check: &ReadinessCheck, deadline: Instant) -> Result<ReadinessEvidence, InstallError>;
}
```

- [ ] **Step 1: Unit-test condition predicates using captured Kubernetes objects**
- [ ] **Step 2: Implement typed/dynamic Kubernetes reads with `kube`**
- [ ] **Step 3: Poll with capped exponential backoff and absolute deadline**

Do not `sleep 60` blindly. A failed deadline includes last observed object/condition after redaction.

- [ ] **Step 4: Verify and commit**

```bash
cargo test -p admissionlab-installer --test readiness_unit
git add crates/admissionlab-installer
git commit -m "feat(installer): add deterministic readiness probes"
```

## Task 2.5 — Define recipe metadata and capability model

**Files:**
- Create: `crates/admissionlab-recipes/src/model.rs`
- Create: `crates/admissionlab-recipes/src/capability.rs`
- Create: `crates/admissionlab-recipes/src/load.rs`
- Test: `crates/admissionlab-recipes/tests/load.rs`
- Create: `compatibility/recipes.yaml`

**Interfaces:**

```rust
pub enum Capability {
    Admission,
    GatewayApi,
    LegacyIngress,
}

pub struct Recipe {
    pub name: String,
    pub version: String,
    pub install: InstallMethod,
    pub readiness: Vec<ReadinessCheck>,
    pub normalize_rules: Vec<RecipeNormalizeRule>,
    pub capabilities: BTreeSet<Capability>,
}
```

- [ ] **Step 1: Test that recipe schema rejects policy/severity keys**

A recipe containing `failOn`, `severity`, or semantic regression classification must fail validation.

- [ ] **Step 2: Implement built-in recipe loading from repository/embedded assets**
- [ ] **Step 3: Implement local recipe override directory with explicit opt-in path**
- [ ] **Step 4: Verify and commit**

```bash
cargo test -p admissionlab-recipes --test load
git add crates/admissionlab-recipes compatibility/recipes.yaml
git commit -m "feat(recipes): define vendor-neutral recipe metadata"
```

## Task 2.6 — Implement stack installation orchestration

**Files:**
- Modify: `crates/admissionlab-core/src/run.rs`
- Create: `crates/admissionlab-installer/src/stack.rs`
- Test: `crates/admissionlab-installer/tests/stack.rs`

**Interfaces:**

```rust
pub struct InstalledStack { pub side: Side, pub components: Vec<InstallRecord> }
pub async fn install_stack(
    cluster: &ClusterHandle,
    components: &[ResolvedComponent],
    installer: &dyn ComponentInstaller,
    readiness: &dyn ReadinessProbe,
    component_timeout: Duration,
) -> Result<InstalledStack, InstallError>;
```

- [ ] **Step 1: Test component order is preserved exactly**
- [ ] **Step 2: Stop candidate/baseline stack on first installation failure for that side and collect diagnostics**
- [ ] **Step 3: Allow baseline and candidate stacks to install concurrently, but component order within a side remains deterministic**
- [ ] **Step 4: Commit**

```bash
cargo test -p admissionlab-installer --test stack
git add crates/admissionlab-installer crates/admissionlab-core
git commit -m "feat(installer): orchestrate ordered component stacks"
```

## Task 2.7 — Add deterministic test component recipe

**Files:**
- Create: `recipes/test-webhook/recipe.yaml`
- Create: `recipes/test-webhook/manifests/`
- Modify: `crates/admissionlab-test-webhook/*` only enough to build a health endpoint; mutation behavior lands Phase 3.
- Create: `scripts/build-test-images.sh`

**Interfaces:** recipe installs the Admission Lab test webhook image into a kind cluster and waits for Deployment + webhook configuration readiness.

- [ ] **Step 1: Build minimal HTTPS webhook container with `/healthz`**

Certificate bootstrapping may use a deterministic test-only CA/Secret generated per cluster; never check a private key into git.

- [ ] **Step 2: Add image build/load helper for kind integration tests**
- [ ] **Step 3: Add recipe install smoke test**
- [ ] **Step 4: Commit**

```bash
git add recipes/test-webhook crates/admissionlab-test-webhook scripts/build-test-images.sh
git commit -m "test: add deterministic admission test component"
```

## Task 2.8 — Add Kyverno certified recipe

**Files:**
- Create: `recipes/kyverno/recipe.yaml`
- Create: `recipes/kyverno/README.md`
- Create: `fixtures/kyverno/smoke/`
- Test: `crates/admissionlab-recipes/tests/kyverno_recipe.rs`

**Interfaces:** recipe resolves a pinned Kyverno Helm chart version and readiness checks; fixture pack includes at least one validating and one mutating policy scenario maintained by Admission Lab.

- [ ] **Step 1: Pin a known-good Kyverno release compatible with the current primary Kubernetes minor**
- [ ] **Step 2: Define Helm install and controller/webhook readiness checks**
- [ ] **Step 3: Add smoke policy/manifests with deterministic behavior**
- [ ] **Step 4: Install in kind integration CI and verify webhook configurations become ready**
- [ ] **Step 5: Commit**

```bash
git add recipes/kyverno fixtures/kyverno crates/admissionlab-recipes
git commit -m "feat(recipes): add certified Kyverno install recipe"
```

## Task 2.9 — Add Istio admission certified recipe

**Files:**
- Create: `recipes/istio/recipe.yaml`
- Create: `recipes/istio/README.md`
- Create: `fixtures/istio/smoke/`
- Test: `crates/admissionlab-recipes/tests/istio_recipe.rs`

**Interfaces:** recipe installs the minimum Istio components needed for admission/sidecar injection tests and exposes readiness of the sidecar injector configuration.

- [ ] **Step 1: Pin a known-good Istio release compatible with the current primary Kubernetes minor**
- [ ] **Step 2: Define minimal Helm install set and namespace labels/fixture prerequisites explicitly**
- [ ] **Step 3: Add a Pod fixture that receives deterministic sidecar injection**
- [ ] **Step 4: Integration-test installation only; semantic assertions wait for Phase 4**
- [ ] **Step 5: Commit**

```bash
git add recipes/istio fixtures/istio crates/admissionlab-recipes
git commit -m "feat(recipes): add certified Istio admission recipe"
```

## Phase 2 Exit Gate

Run:

```bash
cargo test --workspace
cargo test -p admissionlab-recipes -- --ignored --nocapture
```

Then execute a sample lab that creates two clusters and installs the test webhook, Kyverno, and Istio on both sides without fixtures.

**Must be true:**
- generic Helm/raw installers work without vendor branches;
- recipe parser rejects regression-policy logic;
- readiness timeouts return last-observed evidence;
- component install provenance is written to run artifacts;
- both sides clean up after any install failure.

---
# PHASE 3 — Fixture Engine and Admission Capture

**Goal:** Replay deterministic user fixtures through real API servers and capture enough observed evidence to distinguish allow/deny, final mutation, mutating-webhook trace/patches, request latency, and optional per-webhook metric deltas.

**Alpha execution contract:** fixture requests are server-side dry-run CREATE operations and execute serially per cluster. This intentionally avoids controller side effects and gives a final mutated response object while still exercising the real API server/admission chain. A webhook stack that declares itself unsafe for dry-run is reported as an unsupported fixture execution condition; Admission Lab does not silently change to persisted semantics.

## Task 3.1 — Implement fixture discovery, identity, and hashing

**Files:**
- Create: `crates/admissionlab-fixtures/src/discover.rs`
- Create: `crates/admissionlab-fixtures/src/identity.rs`
- Create: `crates/admissionlab-fixtures/src/hash.rs`
- Modify: `crates/admissionlab-fixtures/src/lib.rs`
- Test: `crates/admissionlab-fixtures/tests/discovery.rs`
- Testdata: `testdata/manifests/discovery/`

**Interfaces:**

```rust
pub struct FixtureSource {
    pub id: FixtureId,
    pub path: PathBuf,
    pub document_index: usize,
    pub sha256: String,
    pub object: serde_json::Value,
}

pub fn discover_fixtures(selection: &ResolvedFixtureSelection) -> Result<Vec<FixtureSource>, FixtureError>;
```

- [ ] **Step 1: Write failing tests for glob ordering and multi-document YAML**

Fixture IDs are stable across machines and derive from normalized relative path + document index + Kubernetes object identity, not from random values.

- [ ] **Step 2: Reject YAML documents that are empty or lack `apiVersion`, `kind`, and `metadata.name`/`generateName`**

Alpha requires a deterministic name. `generateName` is rejected until a deterministic name-rewrite contract exists.

- [ ] **Step 3: Hash canonical source bytes and preserve original bytes separately**

SHA-256 is lowercase hex. Hash is for provenance, not security authentication.

- [ ] **Step 4: Verify deterministic ordering**

```bash
cargo test -p admissionlab-fixtures --test discovery
```

- [ ] **Step 5: Commit**

```bash
git add crates/admissionlab-fixtures testdata/manifests/discovery
git commit -m "feat(fixtures): discover and hash deterministic fixtures"
```

## Task 3.2 — Add Kubernetes discovery for arbitrary fixture resources

**Files:**
- Create: `crates/admissionlab-fixtures/src/discovery.rs`
- Test: `crates/admissionlab-fixtures/tests/discovery_unit.rs`

**Interfaces:**

```rust
pub struct ResolvedResource {
    pub api_resource: kube::core::ApiResource,
    pub namespaced: bool,
}

#[async_trait]
pub trait ResourceResolver {
    async fn resolve(&self, cluster: &ClusterHandle, api_version: &str, kind: &str) -> Result<ResolvedResource, FixtureError>;
}
```

- [ ] **Step 1: Test resolution against fake discovery data for core, namespaced CRD, and cluster-scoped resources**
- [ ] **Step 2: Implement using `kube::discovery::Discovery`**
- [ ] **Step 3: Cache discovery per cluster and invalidate once after CRD installs finish**
- [ ] **Step 4: Return an explicit unsupported-resource error instead of asking kubectl to guess**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-fixtures --test discovery_unit
git add crates/admissionlab-fixtures
git commit -m "feat(fixtures): resolve dynamic Kubernetes resources"
```

## Task 3.3 — Define admission outcome and trace domain model

**Files:**
- Create: `crates/admissionlab-admission/src/outcome.rs`
- Create: `crates/admissionlab-admission/src/trace.rs`
- Modify: `crates/admissionlab-admission/src/lib.rs`
- Test: `crates/admissionlab-admission/tests/model.rs`

**Interfaces:**

```rust
pub enum AdmissionDecision {
    Accepted,
    Rejected { code: Option<u16>, message: String },
    UnsupportedDryRun { message: String },
}

pub struct AdmissionOutcome {
    pub fixture_id: FixtureId,
    pub side: Side,
    pub decision: AdmissionDecision,
    pub warnings: Vec<String>,
    pub total_latency: Duration,
    pub final_object: Option<serde_json::Value>,
    pub trace: AdmissionTrace,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct AdmissionTrace {
    pub evidence: TraceEvidence,
    pub invocations: Vec<WebhookInvocation>,
}

pub enum TraceEvidence { Observed, Partial, Unavailable }

pub struct WebhookInvocation {
    pub configuration: String,
    pub webhook: String,
    pub round: u32,
    pub index: u32,
    pub mutated: Option<bool>,
    pub patch: Option<Vec<json_patch::PatchOperation>>,
    pub latency: Option<Duration>,
    pub outcome: WebhookOutcome,
}
```

- [ ] **Step 1: Write serialization round-trip tests**
- [ ] **Step 2: Make evidence level explicit and non-defaultable**
- [ ] **Step 3: Ensure unavailable latency serializes as `null`, not zero**
- [ ] **Step 4: Commit**

```bash
cargo test -p admissionlab-admission --test model
git add crates/admissionlab-admission
git commit -m "feat(admission): define observed admission result model"
```

## Task 3.4 — Implement server-side dry-run fixture executor

**Files:**
- Create: `crates/admissionlab-fixtures/src/execute.rs`
- Create: `crates/admissionlab-admission/src/execute.rs`
- Test: `crates/admissionlab-admission/tests/execute_unit.rs`

**Interfaces:**

```rust
#[async_trait]
pub trait AdmissionExecutor {
    async fn execute_create(
        &self,
        cluster: &ClusterHandle,
        fixture: &FixtureSource,
        resource: &ResolvedResource,
    ) -> Result<RawAdmissionResponse, FixtureExecutionError>;
}

pub struct RawAdmissionResponse {
    pub decision: AdmissionDecision,
    pub response_object: Option<serde_json::Value>,
    pub warnings: Vec<String>,
    pub elapsed: Duration,
    pub request_started_at: SystemTime,
    pub request_finished_at: SystemTime,
}
```

- [ ] **Step 1: Test that the Kubernetes request uses CREATE with `dryRun=All`**

Use a fake service/client layer; do not require kind for the unit test.

- [ ] **Step 2: Implement dynamic API CREATE**

The request must preserve the fixture object except normal API serialization. Do not add correlation annotations or labels in Alpha.

- [ ] **Step 3: Classify rejections**

A normal admission denial is `Rejected` and is valid comparison data. A server-side dry-run rejection specifically caused by unsafe webhook side effects is `UnsupportedDryRun` and is a fixture/lab capability issue, not automatically a candidate regression.

- [ ] **Step 4: Capture Kubernetes Warning headers where the client stack exposes them**

If Warning headers cannot be captured through the chosen client API without unsafe duplication, leave warnings unavailable and record that limitation; do not scrape CLI text.

- [ ] **Step 5: Verify and commit**

```bash
cargo test -p admissionlab-admission --test execute_unit
git add crates/admissionlab-fixtures crates/admissionlab-admission
git commit -m "feat(admission): execute real server-side dry-run fixtures"
```

## Task 3.5 — Implement append-only audit-log reader with offset checkpoints

**Files:**
- Create: `crates/admissionlab-admission/src/audit_reader.rs`
- Test: `crates/admissionlab-admission/tests/audit_reader.rs`
- Testdata: `testdata/audit/basic.jsonl`

**Interfaces:**

```rust
pub struct AuditCheckpoint { pub byte_offset: u64 }
pub struct AuditEvent { /* typed subset + raw annotations */ }

pub trait AuditLogReader {
    fn checkpoint(&self) -> Result<AuditCheckpoint, AuditError>;
    async fn events_since(&self, checkpoint: &AuditCheckpoint, deadline: Instant) -> Result<Vec<AuditEvent>, AuditError>;
}
```

- [ ] **Step 1: Test partial trailing JSON line handling**

The reader must wait for the line to complete rather than classify truncated JSON as corruption.

- [ ] **Step 2: Parse only fields Admission Lab needs**

Keep raw annotation map and typed fields: `auditID`, `stage`, `verb`, `requestURI`, `userAgent`, `objectRef`, `responseStatus`, timestamps.

- [ ] **Step 3: Stop waiting once a matching `ResponseComplete` event exists or deadline expires**

- [ ] **Step 4: Preserve malformed unrelated lines as diagnostics, not fatal fixture errors unless the target event cannot be reconstructed**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-admission --test audit_reader
git add crates/admissionlab-admission testdata/audit/basic.jsonl
git commit -m "feat(admission): read request-scoped audit log windows"
```

## Task 3.6 — Parse Kubernetes mutating-webhook audit annotations

**Files:**
- Create: `crates/admissionlab-admission/src/correlate.rs`
- Test: `crates/admissionlab-admission/tests/correlate.rs`
- Testdata: `testdata/audit/mutation-rounds.jsonl`

**Interfaces:**

```rust
pub fn reconstruct_mutating_trace(event: &AuditEvent) -> Result<AdmissionTrace, TraceError>;
```

Recognize annotation keys exactly matching:

```text
mutation.webhook.admission.k8s.io/round_<round>_index_<index>
patch.webhook.admission.k8s.io/round_<round>_index_<index>
```

- [ ] **Step 1: Add fixtures for one round, reinvocation, mutated=false, and missing patch annotation**
- [ ] **Step 2: Parse the JSON payload inside each annotation**

Invocation payload supplies configuration/webhook/mutated. Patch payload supplies configuration/webhook/patchType/patch.

- [ ] **Step 3: Merge invocation and patch evidence by `(round,index,configuration,webhook)`**
- [ ] **Step 4: Mark partial evidence when a patch is absent even though `mutated=true`**
- [ ] **Step 5: Never infer validating-webhook allow invocations from these mutating annotations**
- [ ] **Step 6: Verify and commit**

```bash
cargo test -p admissionlab-admission --test correlate
git add crates/admissionlab-admission testdata/audit/mutation-rounds.jsonl
git commit -m "feat(admission): reconstruct mutating webhook audit traces"
```

## Task 3.7 — Correlate a serial fixture request to its audit event

**Files:**
- Modify: `crates/admissionlab-admission/src/correlate.rs`
- Test: `crates/admissionlab-admission/tests/request_correlation.rs`
- Testdata: `testdata/audit/background-noise.jsonl`

**Interfaces:**

```rust
pub struct ObjectKey { pub group: String, pub version: String, pub resource: String, pub namespace: Option<String>, pub name: String }
pub fn select_fixture_event(events: &[AuditEvent], key: &ObjectKey, started: SystemTime, finished: SystemTime) -> Result<&AuditEvent, CorrelationError>;
```

- [ ] **Step 1: Test background controller/audit noise around the request**
- [ ] **Step 2: Match CREATE + objectRef + dry-run request URI + request time window**
- [ ] **Step 3: If zero or multiple equally valid target events remain, return correlation failure with candidate event IDs**
- [ ] **Step 4: Do not guess using nearest timestamp alone**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-admission --test request_correlation
git add crates/admissionlab-admission testdata/audit/background-noise.jsonl
git commit -m "feat(admission): correlate serial fixtures to audit events"
```

## Task 3.8 — Capture optional per-webhook latency and rejection deltas from API server metrics

**Files:**
- Create: `crates/admissionlab-admission/src/metrics.rs`
- Test: `crates/admissionlab-admission/tests/metrics.rs`
- Testdata: `testdata/metrics/{before,after}.prom`

**Interfaces:**

```rust
pub struct AdmissionMetricSnapshot { /* histogram sums/counts and rejection counters keyed by labels */ }
pub struct WebhookMetricDelta { pub webhook: String, pub request_count_delta: u64, pub duration_sum_delta: f64, pub rejection_delta: u64 }

pub fn diff_metrics(before: &AdmissionMetricSnapshot, after: &AdmissionMetricSnapshot) -> Vec<WebhookMetricDelta>;
```

- [ ] **Step 1: Parse `apiserver_admission_webhook_admission_duration_seconds_{sum,count}`**

Key on `name`, `operation`, `rejected`, and `type` labels.

- [ ] **Step 2: Parse `apiserver_admission_webhook_rejection_count` where present**
- [ ] **Step 3: Derive a per-fixture latency only when count delta is exactly one for a matching webhook/operation bucket**

When count delta is zero or greater than one, leave per-fixture latency unknown and preserve aggregate evidence.

- [ ] **Step 4: Add scraper using authenticated `kubectl get --raw /metrics` or kube client raw request**

Prefer kube client raw HTTP if headers/auth are already available; otherwise bounded `kubectl --kubeconfig "$KUBECONFIG" get --raw /metrics` is acceptable in v1.

- [ ] **Step 5: Verify and commit**

```bash
cargo test -p admissionlab-admission --test metrics
git add crates/admissionlab-admission testdata/metrics
git commit -m "feat(admission): capture optional webhook metric deltas"
```

## Task 3.9 — Complete deterministic dogfood webhook behaviors

**Files:**
- Create/Modify: `crates/admissionlab-test-webhook/src/{server.rs,mutate.rs,validate.rs,behavior.rs}`
- Modify: `recipes/test-webhook/manifests/`
- Test: `crates/admissionlab-test-webhook/tests/behavior.rs`

**Interfaces:**

Dogfood behavior is controlled only by fixture annotations under `test.admissionlab.io/*`:

```text
test.admissionlab.io/add-label: "key=value"
test.admissionlab.io/add-container: "name=image"
test.admissionlab.io/add-init-container: "name=image"
test.admissionlab.io/remove-container: "name"
test.admissionlab.io/remove-init-container: "name"
test.admissionlab.io/add-volume: "name"
test.admissionlab.io/deny: "message"
test.admissionlab.io/delay-ms: "250"
test.admissionlab.io/fail: "true"
```

- [ ] **Step 1: Unit-test JSONPatch output for each mutating action**
- [ ] **Step 2: Unit-test deny/failure/delay validation behavior with a Tokio paused clock where possible**
- [ ] **Step 3: Keep webhook responses idempotent for add operations**

If an object already contains the requested sidecar/label/volume, return no mutation rather than duplicate it.

- [ ] **Step 4: Deploy two mutating webhook configurations for reinvocation tests**

One webhook can add an annotation/field that makes the second mutate; the first uses `reinvocationPolicy: IfNeeded` in the dedicated integration fixture. Do not make product correctness depend on a fixed global invocation order.

- [ ] **Step 5: Commit**

```bash
git add crates/admissionlab-test-webhook recipes/test-webhook
git commit -m "test: add deterministic admission webhook behaviors"
```

## Task 3.10 — Integrate complete fixture admission capture pipeline

**Files:**
- Modify: `crates/admissionlab-admission/src/execute.rs`
- Modify: `crates/admissionlab-core/src/run.rs`
- Create: `crates/admissionlab-admission/tests/kind_capture.rs`
- Create: `fixtures/core/admission/`

**Interfaces:**

```rust
pub async fn capture_fixture(
    cluster: &ClusterHandle,
    side: Side,
    fixture: &FixtureSource,
    resolver: &dyn ResourceResolver,
    executor: &dyn AdmissionExecutor,
    audit: &dyn AuditLogReader,
    metrics: Option<&dyn AdmissionMetricsSource>,
) -> Result<AdmissionOutcome, FixtureExecutionError>;
```

- [ ] **Step 1: Before request, record audit offset and optional metrics snapshot**
- [ ] **Step 2: Execute exactly one fixture request**
- [ ] **Step 3: Wait for target ResponseComplete audit evidence**
- [ ] **Step 4: Reconstruct mutating trace and merge optional latency/rejection metrics**
- [ ] **Step 5: Write raw artifact bundle**

Per side/fixture:

```text
raw/<side>/<fixture-id>/request.json
raw/<side>/<fixture-id>/response.json
raw/<side>/<fixture-id>/audit.json
raw/<side>/<fixture-id>/metrics-before.prom   # if enabled
raw/<side>/<fixture-id>/metrics-after.prom    # if enabled
raw/<side>/<fixture-id>/outcome.json
```

- [ ] **Step 6: Run fixtures serially per side**

Baseline and candidate may process corresponding fixtures concurrently with each other because they are separate clusters; within each cluster, run one fixture request at a time.

- [ ] **Step 7: Verify kind capture suite**

```bash
cargo test -p admissionlab-admission --test kind_capture -- --ignored --nocapture
```

Cases must cover allow, deny, label mutation, container mutation, init-container mutation, delay, webhook failure, and reinvocation evidence.

- [ ] **Step 8: Commit**

```bash
git add crates/admissionlab-admission crates/admissionlab-core fixtures/core
git commit -m "feat(admission): capture real admission behavior per fixture"
```

## Phase 3 Exit Gate

Create a baseline dogfood stack and candidate dogfood stack with intentionally different behaviors. Run at least these fixtures:

```text
allow
new deny
add label
remove init container
webhook failure
250ms delay
reinvocation case
```

Verification:

```bash
cargo test -p admissionlab-admission --test kind_capture -- --ignored --nocapture
```

Inspect raw artifacts manually once before freezing Alpha semantics.

**Must be true:**
- accepted/rejected is correct;
- final dry-run response object is captured;
- audit evidence shows mutating webhook rounds/patches when Kubernetes exposes them;
- first-level raw evidence contains no fabricated validating-webhook invocation list;
- unsupported dry-run is distinct from regression;
- serial requests never cross-correlate;
- secret request bodies are not logged at Request level by the audit policy.

---

# PHASE 4 — Normalization, Semantic Diff, Policy, First Divergence, and Public Alpha

**Goal:** Convert raw evidence into low-noise, explainable regressions and ship the first useful public Admission Lab release.

## Task 4.1 — Implement deterministic Kubernetes object normalization

**Files:**
- Create: `crates/admissionlab-normalize/src/object.rs`
- Create: `crates/admissionlab-normalize/src/pointer.rs`
- Create: `crates/admissionlab-normalize/src/rules.rs`
- Modify: `crates/admissionlab-normalize/src/lib.rs`
- Test: `crates/admissionlab-normalize/tests/object.rs`
- Golden: `testdata/objects/normalization/`

**Interfaces:**

```rust
pub enum NormalizeRule {
    RemovePointer(String),
    SortNamedArray { pointer: String, key: String },
    RemoveAnnotation(String),
}

pub struct NormalizationProfile {
    pub built_in: Vec<NormalizeRule>,
    pub recipe: Vec<NormalizeRule>,
    pub user: Vec<NormalizeRule>,
}

pub fn normalize_object(value: &serde_json::Value, profile: &NormalizationProfile) -> Result<NormalizedObject, NormalizeError>;
```

- [ ] **Step 1: Golden-test built-in removals**

Remove at least `metadata.uid`, `resourceVersion`, `creationTimestamp`, `managedFields`, and the Admission Lab correlation/user-generated test-only metadata when applicable. Do not remove `metadata.generation` globally unless a domain-specific comparison proves it irrelevant.

- [ ] **Step 2: Implement semantically safe named-list sorting only for known Kubernetes lists**

Examples that can be keyed by `name`: containers, initContainers, volumes, env entries when valueFrom/value semantics are preserved. Do not sort `command`, `args`, or arbitrary arrays.

- [ ] **Step 3: Record effective rules in `NormalizationEvidence`**
- [ ] **Step 4: Warn when user rules remove broad parents such as `/spec` or all annotations**
- [ ] **Step 5: Verify and commit**

```bash
cargo test -p admissionlab-normalize --test object
git add crates/admissionlab-normalize testdata/objects/normalization
git commit -m "feat(normalize): normalize Kubernetes objects deterministically"
```

## Task 4.2 — Normalize admission traces and patches

**Files:**
- Create: `crates/admissionlab-normalize/src/trace.rs`
- Test: `crates/admissionlab-normalize/tests/trace.rs`

**Interfaces:**

```rust
pub struct NormalizedTrace { pub evidence: TraceEvidence, pub invocations: Vec<NormalizedWebhookInvocation> }
pub fn normalize_trace(trace: &AdmissionTrace) -> NormalizedTrace;
```

- [ ] **Step 1: Preserve round/index and webhook identity**
- [ ] **Step 2: Canonicalize JSONPatch value objects recursively without reordering patch operations**
- [ ] **Step 3: Do not strip a changed patch merely because final objects happen to match**
- [ ] **Step 4: Verify and commit**

```bash
cargo test -p admissionlab-normalize --test trace
git add crates/admissionlab-normalize
git commit -m "feat(normalize): canonicalize webhook trace evidence"
```

## Task 4.3 — Define raw and semantic change model with stable Alpha names

**Files:**
- Create: `crates/admissionlab-diff/src/types.rs`
- Create: `crates/admissionlab-diff/src/raw.rs`
- Modify: `crates/admissionlab-diff/src/lib.rs`
- Test: `crates/admissionlab-diff/tests/types.rs`

**Interfaces:**

```rust
pub enum SemanticChangeKind {
    ObjectNewlyDenied,
    ObjectNewlyAllowed,
    ContainerAdded,
    ContainerRemoved,
    InitContainerAdded,
    InitContainerRemoved,
    VolumeAdded,
    VolumeRemoved,
    VolumeMountChanged,
    EnvironmentChanged,
    ImageChanged,
    ServiceAccountChanged,
    SecurityContextChanged,
    ResourceRequirementChanged,
    WebhookFailed,
    WebhookInvocationChanged,
    WebhookLatencyChanged,
}

pub struct SemanticChange {
    pub kind: SemanticChangeKind,
    pub fixture_id: FixtureId,
    pub object_path: Option<String>,
    pub subject: Option<String>,
    pub baseline: Option<serde_json::Value>,
    pub candidate: Option<serde_json::Value>,
    pub origin: Option<DivergenceEvidence>,
}
```

Serialization names are explicit and human-oriented:

```text
newly_denied
newly_allowed
container_added
container_removed
init_container_added
init_container_removed
volume_added
volume_removed
volume_mount_changed
environment_changed
image_changed
service_account_changed
security_context_changed
resource_requirement_changed
webhook_failed
webhook_invocation_changed
webhook_latency_changed
```

- [ ] **Step 1: Add a serialization snapshot test for every public kind**
- [ ] **Step 2: Implement raw RFC-6902-compatible object diff for diagnostics**
- [ ] **Step 3: Commit**

```bash
cargo test -p admissionlab-diff --test types
git add crates/admissionlab-diff
git commit -m "feat(diff): define admission semantic change contract"
```

## Task 4.4 — Implement admission decision semantic diff

**Files:**
- Create: `crates/admissionlab-diff/src/admission.rs`
- Test: `crates/admissionlab-diff/tests/admission_decision.rs`

**Interfaces:**

```rust
pub fn diff_admission_decision(baseline: &AdmissionOutcome, candidate: &AdmissionOutcome) -> Vec<SemanticChange>;
```

- [ ] **Step 1: Test accepted->rejected => `ObjectNewlyDenied`**
- [ ] **Step 2: Test rejected->accepted => `ObjectNewlyAllowed`**
- [ ] **Step 3: Test rejected->rejected with message change produces diagnostic raw difference but no newly-denied change**
- [ ] **Step 4: Treat `UnsupportedDryRun` as incomparable fixture capability, not a semantic regression change**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-diff --test admission_decision
git add crates/admissionlab-diff
git commit -m "feat(diff): classify admission decision changes"
```

## Task 4.5 — Implement Pod/workload semantic field diff

**Files:**
- Modify: `crates/admissionlab-diff/src/admission.rs`
- Create: `crates/admissionlab-diff/src/workload.rs`
- Test: `crates/admissionlab-diff/tests/workload.rs`
- Golden: `testdata/golden/semantic-workloads/`

**Interfaces:**

```rust
pub fn diff_workload_objects(baseline: &NormalizedObject, candidate: &NormalizedObject) -> Vec<SemanticChange>;
```

- [ ] **Step 1: Implement container/init-container add/remove/image changes keyed by name**
- [ ] **Step 2: Implement volumes and volumeMount changes keyed by name/mountPath**
- [ ] **Step 3: Implement env changes keyed by container + env name without rendering sensitive literal values in report-ready fields**
- [ ] **Step 4: Implement service account, securityContext, and resource requirements changes**
- [ ] **Step 5: Unknown object fields remain available in raw diff but do not become guessed semantic categories**
- [ ] **Step 6: Golden-test known sidecar/init-container regression**
- [ ] **Step 7: Commit**

```bash
cargo test -p admissionlab-diff --test workload
git add crates/admissionlab-diff testdata/golden/semantic-workloads
git commit -m "feat(diff): classify workload mutation semantics"
```

## Task 4.6 — Implement webhook invocation/failure/latency semantic diff

**Files:**
- Create: `crates/admissionlab-diff/src/trace.rs`
- Test: `crates/admissionlab-diff/tests/trace.rs`

**Interfaces:**

```rust
pub fn diff_admission_trace(baseline: &NormalizedTrace, candidate: &NormalizedTrace, latency_policy: &LatencyPolicy) -> Vec<SemanticChange>;
```

- [ ] **Step 1: Detect webhook sequence/round/patch changes as `WebhookInvocationChanged`**
- [ ] **Step 2: Detect candidate-only calling errors/rejection metric evidence as `WebhookFailed` only when evidence identifies a webhook failure**
- [ ] **Step 3: Detect latency regression only when both sides have unambiguous observed durations**

Default Alpha latency policy: warning when candidate duration is at least 100ms slower **and** at least 2x baseline for the same webhook. Make thresholds configurable before Beta schema freeze.

- [ ] **Step 4: Never convert missing latency into zero**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-diff --test trace
git add crates/admissionlab-diff
git commit -m "feat(diff): classify webhook trace regressions"
```

## Task 4.7 — Implement first-divergence attribution without overclaiming

**Files:**
- Create: `crates/admissionlab-diff/src/divergence.rs`
- Test: `crates/admissionlab-diff/tests/divergence.rs`

**Interfaces:**

```rust
pub enum DivergenceConfidence { Observed, Inferred, Unknown }

pub struct DivergenceEvidence {
    pub confidence: DivergenceConfidence,
    pub baseline_position: Option<(u32, u32)>,
    pub candidate_position: Option<(u32, u32)>,
    pub baseline_webhook: Option<String>,
    pub candidate_webhook: Option<String>,
    pub explanation: String,
}

pub fn first_divergence(baseline: &NormalizedTrace, candidate: &NormalizedTrace) -> Option<DivergenceEvidence>;
```

- [ ] **Step 1: Observed divergence when same trace position has different webhook identity/mutated flag/patch**
- [ ] **Step 2: Observed divergence when one trace adds/removes an invocation at a position**
- [ ] **Step 3: If traces are identical but final objects differ, return `Unknown` explanation: difference occurred outside captured mutating-webhook evidence or evidence is incomplete**
- [ ] **Step 4: If either trace evidence is partial, confidence cannot be stronger than `Inferred` unless the differing patch itself is directly observed**
- [ ] **Step 5: Test the canonical “remove `/spec/initContainers`” case**
- [ ] **Step 6: Commit**

```bash
cargo test -p admissionlab-diff --test divergence
git add crates/admissionlab-diff
git commit -m "feat(diff): attribute first observable admission divergence"
```

## Task 4.8 — Implement severity defaults and policy overrides

**Files:**
- Create: `crates/admissionlab-policy/src/severity.rs`
- Create: `crates/admissionlab-policy/src/selector.rs`
- Create: `crates/admissionlab-policy/src/evaluate.rs`
- Modify: `crates/admissionlab-policy/src/lib.rs`
- Modify: `crates/admissionlab-spec/src/model.rs`
- Test: `crates/admissionlab-policy/tests/evaluate.rs`

**Interfaces:**

```rust
pub enum Severity { Info, Warning, Critical }
pub struct ClassifiedChange { pub change: SemanticChange, pub severity: Severity, pub expected: bool }
pub struct PolicyResult { pub disposition: PolicyDisposition, pub changes: Vec<ClassifiedChange>, pub stale_expectations: Vec<StaleExpectation> }
pub enum PolicyDisposition { Pass, Warn, Fail }
```

- [ ] **Step 1: Encode the exact Alpha default mapping**

| Semantic kind | Default severity |
|---|---|
| `newly_denied` | Critical |
| `newly_allowed` | Critical |
| `container_added` | Warning |
| `container_removed` | Critical |
| `init_container_added` | Warning |
| `init_container_removed` | Critical |
| `volume_added` | Warning |
| `volume_removed` | Critical |
| `volume_mount_changed` | Warning |
| `environment_changed` | Warning |
| `image_changed` | Info |
| `service_account_changed` | Critical |
| `security_context_changed` | Critical |
| `resource_requirement_changed` | Warning |
| `webhook_failed` | Critical |
| `webhook_invocation_changed` | Warning |
| `webhook_latency_changed` | Warning |

The intentionally conservative `security_context_changed` and `newly_allowed` defaults can be overridden explicitly by policy; Alpha does not attempt an incomplete security partial-order classifier.
- [ ] **Step 2: Add selector-scoped overrides by semantic kind + fixture glob + optional subject/object path**
- [ ] **Step 3: Validate impossible/unknown semantic names at config load time**
- [ ] **Step 4: `Critical` unmatched change => Fail; warnings alone => Warn; otherwise Pass**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-policy --test evaluate
git add crates/admissionlab-policy crates/admissionlab-spec
git commit -m "feat(policy): evaluate deterministic regression severity"
```

## Task 4.9 — Implement explicit expected-change matching and stale expectations

**Files:**
- Create: `crates/admissionlab-policy/src/expectation.rs`
- Test: `crates/admissionlab-policy/tests/expectations.rs`
- Testdata: `testdata/configs/expectations.yaml`

**Interfaces:**

```rust
pub struct ExpectedChange {
    pub id: String,
    pub fixtures: String,
    pub kind: SemanticChangeKind,
    pub selector: Option<ChangeSelector>,
    pub reason: String,
}

pub struct ExpectationMatch { pub expectation_id: String, pub change_index: usize }
pub struct StaleExpectation { pub id: String, pub reason: String }
```

- [ ] **Step 1: Require non-empty human reason and stable expectation ID**
- [ ] **Step 2: Match deterministically; one change cannot satisfy two expectations unless both explicitly allow shared matching (not supported in v1 Alpha)**
- [ ] **Step 3: Surface unmatched expectations as stale warnings**
- [ ] **Step 4: Expected critical changes remain visible but do not fail policy**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-policy --test expectations
git add crates/admissionlab-policy testdata/configs/expectations.yaml
git commit -m "feat(policy): match explicit expected changes"
```

## Task 4.10 — Build report-ready result model and central redaction pass

**Files:**
- Create: `crates/admissionlab-report/src/model.rs`
- Create: `crates/admissionlab-report/src/redact.rs`
- Modify: `crates/admissionlab-report/src/lib.rs`
- Test: `crates/admissionlab-report/tests/redact.rs`

**Interfaces:**

```rust
pub struct LabResult {
    pub schema_version: String,
    pub run_id: RunId,
    pub summary: RunSummary,
    pub environments: EnvironmentSummary,
    pub fixtures: Vec<FixtureComparison>,
    pub policy: PolicyResult,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn redact_result(result: &LabResult, rules: &RedactionRules) -> LabResult;
```

- [ ] **Step 1: Redact Kubernetes Secret objects recursively**

`data` and `stringData` values become `[REDACTED]`; key names may remain.

- [ ] **Step 2: Redact Authorization/Cookie/proxy-auth headers in traffic diagnostics**
- [ ] **Step 3: Redact private-key PEM blocks and user-configured JSON pointers**
- [ ] **Step 4: Redact env literal values when names match configured/common credential patterns, but preserve change existence**
- [ ] **Step 5: Add a test that serializes the entire result and asserts known secret sentinel strings are absent**
- [ ] **Step 6: Commit**

```bash
cargo test -p admissionlab-report --test redact
git add crates/admissionlab-report
git commit -m "feat(report): centralize result redaction"
```

## Task 4.11 — Implement terminal report

**Files:**
- Create: `crates/admissionlab-report/src/terminal.rs`
- Test: `crates/admissionlab-report/tests/terminal.rs`

**Interfaces:**

```rust
pub fn render_terminal(result: &LabResult, options: &TerminalOptions) -> String;
```

- [ ] **Step 1: Golden-test canonical summary**

Must contain counts for identical/expected/warning/critical/inconclusive; critical details include object, semantic change, and first divergence when known.

- [ ] **Step 2: Use color only when stdout is a TTY and `NO_COLOR` is not set**
- [ ] **Step 3: Never hide warnings/critical entries behind interactive UI**
- [ ] **Step 4: Commit**

```bash
cargo test -p admissionlab-report --test terminal
git add crates/admissionlab-report
git commit -m "feat(report): render concise terminal regressions"
```

## Task 4.12 — Implement Alpha JSON report

**Files:**
- Create: `crates/admissionlab-report/src/json.rs`
- Test: `crates/admissionlab-report/tests/json.rs`
- Golden: `testdata/golden/result-alpha.json`

**Interfaces:**

```rust
pub fn write_json_report(path: &Path, result: &LabResult) -> Result<(), ReportError>;
```

- [ ] **Step 1: Serialize explicit `schemaVersion: admissionlab.io/result/v1alpha1`**
- [ ] **Step 2: Golden-test field names and semantic-kind strings**
- [ ] **Step 3: Do not promise stable schema until Beta; mark Alpha schema experimental in docs**
- [ ] **Step 4: Commit**

```bash
cargo test -p admissionlab-report --test json
git add crates/admissionlab-report testdata/golden/result-alpha.json
git commit -m "feat(report): emit machine-readable Alpha result"
```

## Task 4.13 — Implement self-contained static HTML report

**Files:**
- Create: `crates/admissionlab-report/src/html.rs`
- Create: `crates/admissionlab-report/src/templates/report.html`
- Test: `crates/admissionlab-report/tests/html.rs`

**Interfaces:**

```rust
pub fn write_html_report(path: &Path, result: &LabResult) -> Result<(), ReportError>;
```

- [ ] **Step 1: Render summary + fixture drill-down + raw/semantic diff + trace**
- [ ] **Step 2: Embed CSS/JS locally; report must open without a server or network**
- [ ] **Step 3: Escape every user/vendor string before HTML insertion**
- [ ] **Step 4: Test report contains no external `<script src>` or stylesheet URL**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-report --test html
git add crates/admissionlab-report
git commit -m "feat(report): generate standalone HTML artifact"
```

## Task 4.14 — Wire full `admissionlab test` command and exit codes

**Files:**
- Modify: `crates/admissionlab-core/src/run.rs`
- Modify: `crates/admissionlab-cli/src/commands/test.rs`
- Modify: `crates/admissionlab-cli/src/exit.rs`
- Test: `crates/admissionlab-cli/tests/test_command.rs`

**Interfaces:**

```text
admissionlab test admissionlab.yaml
admissionlab test admissionlab.yaml --keep-clusters
admissionlab test admissionlab.yaml --report-dir ./artifacts
```

- [ ] **Step 1: Implement pipeline**

```text
load/validate config
-> doctor prerequisite check
-> create run workspace
-> create clusters
-> install stacks
-> discover fixtures
-> capture baseline/candidate outcomes
-> normalize
-> semantic diff
-> first divergence
-> policy/expectations
-> redact report-ready model
-> write terminal/json/html/raw artifacts
-> cleanup
-> exit
```

- [ ] **Step 2: Map typed failures to conceptual exit codes 0-6**
- [ ] **Step 3: Always attempt report/diagnostic artifact write before cleanup on a later-stage failure**
- [ ] **Step 4: Always attempt cleanup unless `--keep-clusters`**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-cli --test test_command
git add crates/admissionlab-core crates/admissionlab-cli
git commit -m "feat(cli): run end-to-end admission regression lab"
```

## Task 4.15 — Create canonical Kyverno + Istio Alpha regression corpus

**Files:**
- Create: `examples/kyverno-istio-upgrade/admissionlab.yaml`
- Create: `examples/kyverno-istio-upgrade/expectations.yaml`
- Create: `examples/kyverno-istio-upgrade/fixtures/`
- Create: `fixtures/core/alpha-corpus/`
- Test: `crates/admissionlab-cli/tests/alpha_e2e.rs`

**Interfaces:** canonical demo must produce at least one known critical regression and one expected image/version change.

- [ ] **Step 1: Add fixtures for Pod, Job, Deployment policy target, sidecar injection, existing init container, security context, volume, env, and service account**
- [ ] **Step 2: Configure baseline/candidate stack or local dogfood overlay to reproduce a deterministic init-container removal/behavior change**

Do not rely on a vendor bug that may disappear from current versions; preserve a dogfood reproducer as the authoritative test and optionally document equivalent historical vendor regressions.

- [ ] **Step 3: Assert result contains the critical semantic change and observed first divergence**
- [ ] **Step 4: Assert expected Istio image version changes are marked expected, not hidden**
- [ ] **Step 5: Commit**

```bash
git add examples/kyverno-istio-upgrade fixtures/core/alpha-corpus crates/admissionlab-cli/tests/alpha_e2e.rs
git commit -m "test: add canonical admission regression corpus"
```

## Task 4.16 — Public Alpha documentation and release workflow

**Files:**
- Rewrite/Create: `README.md`
- Create: `docs/config.md`
- Create: `docs/fixtures.md`
- Create: `docs/recipes.md`
- Create: `docs/security.md`
- Create: `docs/troubleshooting.md`
- Modify: `.github/workflows/release.yml`

**Interfaces:** docs expose only working Alpha commands/features; Gateway sections are clearly “planned Beta,” not presented as available.

- [ ] **Step 1: README 30-second quickstart**

Show install, example config, `admissionlab doctor`, `admissionlab test`, sample critical output, and cleanup behavior.

- [ ] **Step 2: Document dry-run semantic limitation explicitly**
- [ ] **Step 3: Document trust model for third-party Helm charts/controllers**
- [ ] **Step 4: Add release workflow building signed/checksummed binaries for Linux amd64/arm64 and macOS amd64/arm64**

Windows native support is not a v1 commitment because kind/Docker behavior differs; Windows users may use WSL2 and this must be stated rather than implied.

- [ ] **Step 5: Tag Public Alpha only after the Alpha gate below passes**

## Phase 4 / Public Alpha Exit Gate

Mandatory:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p admissionlab-cli --test alpha_e2e -- --ignored --nocapture
./scripts/verify-cleanup.sh 100
```

Manual release review:
- canonical demo communicates a real regression in under 30 seconds;
- no report contains test sentinel secrets;
- known raw nondeterminism does not create warnings;
- missing trace evidence says unknown/partial rather than inventing first cause;
- Kyverno and Istio recipes pass on the primary Kubernetes version;
- Gateway code may compile as an empty crate but no Gateway behavior is advertised.

**Public Alpha definition:** a platform engineer can compare two real admission stacks using fixtures and receive a deterministic PASS/WARN/FAIL with semantic regression and first observable divergence.

---

# PHASE 5 — CI Integration, Provenance, Reproduction, and Alpha Hardening

**Goal:** Make Admission Lab dependable in pull-request workflows and make every completed run explain exactly what inputs produced it.

## Task 5.1 — Define versioned run-manifest model

**Files:**
- Create: `crates/admissionlab-core/src/run_manifest.rs`
- Create: `schemas/run-manifest-v1alpha1.json`
- Test: `crates/admissionlab-core/tests/run_manifest.rs`

**Interfaces:**

```rust
pub struct RunManifest {
    pub schema_version: String,
    pub run_id: RunId,
    pub admissionlab_version: String,
    pub host: HostProvenance,
    pub tools: ToolProvenance,
    pub baseline: EnvironmentProvenance,
    pub candidate: EnvironmentProvenance,
    pub config_sha256: String,
    pub fixture_hashes: BTreeMap<FixtureId, String>,
    pub expectations_sha256: Option<String>,
    pub normalization_sha256: String,
    pub policy_sha256: String,
    pub started_at: SystemTime,
    pub completed_at: Option<SystemTime>,
}
```

- [ ] **Step 1: Record exact kind node image names/digests and external tool versions**
- [ ] **Step 2: Record component/chart versions and source hashes where obtainable**
- [ ] **Step 3: Never record secret values or full kubeconfigs**
- [ ] **Step 4: Generate schema and golden example**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-core --test run_manifest
git add crates/admissionlab-core schemas/run-manifest-v1alpha1.json
git commit -m "feat(core): record reproducible run provenance"
```

## Task 5.2 — Write run manifest incrementally and preserve partial provenance on failure

**Files:**
- Modify: `crates/admissionlab-core/src/run.rs`
- Test: `crates/admissionlab-core/tests/run_manifest_failure.rs`

**Interfaces:** run manifest is created before cluster creation and atomically updated after tool discovery, install resolution, fixture hashing, and completion.

- [ ] **Step 1: Test candidate install failure still leaves a valid manifest with `completed_at: null` and failure stage**
- [ ] **Step 2: Add stage/status enum to manifest**
- [ ] **Step 3: Atomic-write after each major stage**
- [ ] **Step 4: Commit**

```bash
cargo test -p admissionlab-core --test run_manifest_failure
git add crates/admissionlab-core
git commit -m "feat(core): preserve provenance across failed runs"
```

## Task 5.3 — Implement `admissionlab reproduce`

**Files:**
- Create: `crates/admissionlab-cli/src/commands/reproduce.rs`
- Modify: `crates/admissionlab-cli/src/commands/mod.rs`
- Create: `crates/admissionlab-core/src/reproduce.rs`
- Test: `crates/admissionlab-core/tests/reproduce.rs`

**Interfaces:**

```text
admissionlab reproduce ./artifacts/run.json --source-root .
```

```rust
pub struct ReproducePlan { pub resolved_lab: ResolvedLab, pub verified_inputs: Vec<VerifiedInput> }
pub fn plan_reproduction(manifest: &RunManifest, source_root: &Path) -> Result<ReproducePlan, ReproduceError>;
```

- [ ] **Step 1: Verify current source fixture/config hashes against manifest before cluster creation**
- [ ] **Step 2: Reuse recorded Kubernetes/component versions, not current recipe defaults**
- [ ] **Step 3: Fail clearly listing unavailable chart/image/source version**
- [ ] **Step 4: Do not silently fall forward to a newer dependency**
- [ ] **Step 5: E2E-test reproducing the dogfood run twice produces equal semantic results after timestamps/run IDs are normalized**
- [ ] **Step 6: Commit**

```bash
git add crates/admissionlab-cli crates/admissionlab-core
git commit -m "feat(cli): reproduce recorded Admission Lab runs"
```

## Task 5.4 — Add GitHub composite action as a thin CLI wrapper

**Files:**
- Create: `.github/actions/admissionlab/action.yml`
- Create: `docs/github-action.md`
- Create: `examples/admission-basic/.github/workflows/admissionlab.yml`
- Test: `.github/workflows/integration.yml`

**Interfaces:**

```yaml
- uses: <owner>/admission-lab/.github/actions/admissionlab@v1
  with:
    config: admissionlab.yaml
```

Action responsibilities only:
1. install/download pinned Admission Lab binary;
2. verify/install pinned kind/kubectl/helm as documented;
3. run `admissionlab test`;
4. write GitHub job summary from CLI-produced summary file;
5. upload JSON/HTML/run manifest artifacts.

- [ ] **Step 1: Do not implement regression logic in YAML/shell**
- [ ] **Step 2: Pin downloaded release by version and verify SHA-256**
- [ ] **Step 3: Preserve Admission Lab exit code**
- [ ] **Step 4: Upload artifacts even when exit code is 1-5**
- [ ] **Step 5: Test the action against repository example on pull requests**
- [ ] **Step 6: Commit**

```bash
git add .github/actions docs/github-action.md examples/admission-basic/.github
git commit -m "feat(ci): add GitHub Action wrapper"
```

## Task 5.5 — Add GitHub job-summary renderer

**Files:**
- Create: `crates/admissionlab-report/src/github.rs`
- Test: `crates/admissionlab-report/tests/github.rs`

**Interfaces:**

```rust
pub fn render_github_summary(result: &LabResult) -> String;
```

- [ ] **Step 1: Limit summary to counts + top critical/warnings + artifact pointers**
- [ ] **Step 2: Escape Markdown table/control characters from object/vendor names**
- [ ] **Step 3: Keep full traces in HTML artifact, not job summary**
- [ ] **Step 4: Commit**

```bash
cargo test -p admissionlab-report --test github
git add crates/admissionlab-report
git commit -m "feat(report): render GitHub job summary"
```

## Task 5.6 — Add explicit cache root and safe reusable downloads

**Files:**
- Modify: `crates/admissionlab-core/src/artifact.rs`
- Create: `crates/admissionlab-core/src/cache.rs`
- Test: `crates/admissionlab-core/tests/cache.rs`

**Interfaces:**

```rust
pub struct CachePaths { pub root: PathBuf, pub downloads: PathBuf, pub helm: PathBuf }
```

- [ ] **Step 1: Add `ADMISSIONLAB_CACHE_DIR` override and platform default**
- [ ] **Step 2: Cache immutable downloads by content hash/version only**
- [ ] **Step 3: Never reuse run-specific kubeconfig/audit/raw artifacts as cache**
- [ ] **Step 4: Add cache corruption test: mismatched hash causes redownload/failure, never trust stale bytes**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-core --test cache
git add crates/admissionlab-core
git commit -m "feat(core): add safe immutable cache layout"
```

## Task 5.7 — Add performance instrumentation and regression benchmark harness

**Files:**
- Create: `crates/admissionlab-core/src/timing.rs`
- Create: `scripts/benchmark-alpha.sh`
- Modify: `crates/admissionlab-report/src/model.rs`

**Interfaces:** result summary records durations for cluster creation, install per side/component, fixture capture, normalization/diff, reporting, cleanup.

- [ ] **Step 1: Add monotonic timers around each stage**
- [ ] **Step 2: Benchmark 100 dry-run Pod fixtures on a healthy CI runner**
- [ ] **Step 3: Assert semantic diff of pre-captured 100 fixtures stays below one second in a release-mode benchmark job**
- [ ] **Step 4: Do not make wall-clock kind target a flaky PR assertion; report trend and enforce only egregious regressions in scheduled CI**
- [ ] **Step 5: Commit**

```bash
git add crates/admissionlab-core crates/admissionlab-report scripts/benchmark-alpha.sh
git commit -m "perf: instrument Admission Lab stage timings"
```

## Task 5.8 — Decide fixture parallelism from evidence; default remains serial

**Files:**
- Create: `docs/architecture.md`
- Conditional modify: `crates/admissionlab-admission/src/execute.rs` only when the measured decision in this task selects parallel correlation; otherwise the task must commit the documented serial-execution decision and leave code unchanged.

**Decision gate:** Measure Phase 5.7. If 100 fixtures already meet the product target serially, keep serial execution for v1 and document why. If serial fixture capture exceeds the target materially, implement correlation tags using a per-request unique `User-Agent` value visible in Kubernetes audit events and add bounded concurrency.

If parallelism is required:

```rust
pub struct CorrelationTag(String); // e.g. admissionlab/<version> run/<short-id> fixture/<fixture-id>
```

- [ ] **Step 1: Prove audit events contain the unique user-agent tag in a kind integration test**
- [ ] **Step 2: Add max concurrency config defaulting to 1 until Beta**
- [ ] **Step 3: Run 100 concurrent/mixed-noise cases and prove zero cross-correlation**
- [ ] **Step 4: If the proof fails or introduces observable webhook behavior changes, revert to serial and document the decision**
- [ ] **Step 5: Commit the measured decision**

Commit either:

```text
perf: add audit-safe bounded fixture concurrency
```

or:

```text
docs: retain serial fixtures for deterministic v1 execution
```

## Task 5.9 — Alpha hardening reliability matrix

**Files:**
- Create: `.github/workflows/nightly.yml`
- Create: `scripts/verify-cleanup.sh` enhancements
- Create: `docs/troubleshooting.md` failure catalog additions

Nightly suite:

```text
100 create/delete cycles (or sharded equivalent)
50 full dogfood admission lab runs
supported primary Kubernetes minor
forced install timeout
forced webhook timeout
forced kind failure
a deterministic artifact-store write failure injected through the test filesystem abstraction
```

- [ ] **Step 1: Ensure no test leaves `adlab-*` clusters**
- [ ] **Step 2: Track flake separately from known candidate regression**
- [ ] **Step 3: Save diagnostics from failed nightly runs as artifacts**
- [ ] **Step 4: Commit**

```bash
git add .github/workflows/nightly.yml scripts/verify-cleanup.sh docs/troubleshooting.md
git commit -m "test: add Admission Lab nightly reliability suite"
```

## Task 5.10 — Add explicit parameterized fixture matrices

**Files:**
- Create: `crates/admissionlab-fixtures/src/matrix.rs`
- Modify: `crates/admissionlab-fixtures/src/discover.rs`
- Modify: `crates/admissionlab-spec/src/v1alpha1.rs`
- Test: `crates/admissionlab-fixtures/tests/matrix.rs`
- Create: `fixtures/core/matrix/`

**Interfaces:** parameterization is patch-based, not text templating.

```rust
pub struct FixtureMatrixSpec {
    pub id: String,
    pub base: PathBuf,
    pub cases: Vec<FixtureMatrixCase>,
}

pub struct FixtureMatrixCase {
    pub id: String,
    pub patches: Vec<json_patch::PatchOperation>,
}

pub fn expand_matrix(spec: &FixtureMatrixSpec, root: &Path) -> Result<Vec<FixtureSource>, FixtureError>;
```

- [ ] **Step 1: Reject duplicate matrix/case IDs and patch paths that make the resulting object invalid JSON**
- [ ] **Step 2: Apply RFC 6902 patches to the parsed base object, never interpolate arbitrary YAML text**
- [ ] **Step 3: Stable fixture ID is `<matrix-id>/<case-id>` and source hash covers base hash + canonical patch list**
- [ ] **Step 4: Validate every expanded object with the same `apiVersion`/`kind`/metadata requirements as static fixtures**
- [ ] **Step 5: Add examples for pre-existing init container, hostNetwork, and custom service account variants without introducing an automatic generator**
- [ ] **Step 6: Commit**

```bash
cargo test -p admissionlab-fixtures --test matrix
git add crates/admissionlab-fixtures crates/admissionlab-spec fixtures/core/matrix
git commit -m "feat(fixtures): add deterministic parameterized fixture matrices"
```

## Phase 5 Exit Gate

```bash
cargo test --workspace
cargo test -p admissionlab-cli --test alpha_e2e -- --ignored --nocapture
./scripts/benchmark-alpha.sh
```

Run the GitHub Action in a real PR and download artifacts.

**Must be true:**
- JSON, HTML, and run manifest upload even on a policy fail;
- `reproduce` verifies hashes and versions before executing;
- serial execution meets target or bounded parallelism has a proven audit-correlation mechanism;
- partial failed runs preserve provenance and diagnostics;
- action logic is a wrapper, not a second engine.

---
# PHASE 6 — Gateway Behavior Engine and Istio Gateway API

**Goal:** Extend the proven admission lab into a three-layer Gateway test: admission, reconciliation, and HTTP data-plane behavior. Istio Gateway API is the reference implementation. This phase does not add NGINX yet.

**Execution distinction:** Gateway fixtures are persisted in the disposable cluster because controller reconciliation and data-plane programming require durable resources. Persisted Gateway fixtures are isolated by the ephemeral cluster; Admission Lab never applies them to production.

## Task 6.1 — Define Gateway fixture and traffic-contract model

**Files:**
- Create: `crates/admissionlab-gateway/src/model.rs`
- Modify: `crates/admissionlab-spec/src/model.rs`
- Test: `crates/admissionlab-gateway/tests/model.rs`
- Testdata: `testdata/configs/gateway-valid.yaml`

**Interfaces:**

```rust
pub struct GatewaySuiteSpec {
    pub manifests: Vec<PathBuf>,
    pub routes: Vec<RouteContract>,
    pub reconciliation_timeout: Duration,
}

pub struct RouteContract {
    pub id: String,
    pub gateway_namespace: String,
    pub gateway_name: String,
    pub route_namespace: String,
    pub route_name: String,
    pub listener_name: Option<String>,
    pub probes: Vec<HttpProbeContract>,
}

pub struct HttpProbeContract {
    pub host: String,
    pub path: String,
    pub method: String,
    pub headers: BTreeMap<String, String>,
    pub expected_status: u16,
    pub expected_backend: Option<String>,
}
```

- [ ] **Step 1: Add strict config tests for duplicate contract IDs and invalid HTTP methods/statuses**
- [ ] **Step 2: Require Gateway resource identity to be explicit; do not guess from first route in a directory**
- [ ] **Step 3: Keep traffic expectations separate from regression policy**
- [ ] **Step 4: Commit**

```bash
cargo test -p admissionlab-gateway --test model
git add crates/admissionlab-gateway crates/admissionlab-spec testdata/configs/gateway-valid.yaml
git commit -m "feat(gateway): define Gateway reconciliation and traffic contracts"
```

## Task 6.2 — Implement persisted Gateway manifest installer

**Files:**
- Create: `crates/admissionlab-gateway/src/apply.rs`
- Test: `crates/admissionlab-gateway/tests/apply_unit.rs`

**Interfaces:**

```rust
pub struct AppliedGatewayFixture {
    pub objects: Vec<ObjectKey>,
    pub source_hashes: BTreeMap<PathBuf, String>,
}

pub async fn apply_gateway_manifests(cluster: &ClusterHandle, manifests: &[PathBuf]) -> Result<AppliedGatewayFixture, GatewayError>;
```

- [ ] **Step 1: Parse and hash all manifests before applying anything**
- [ ] **Step 2: Apply namespaces/backends before Gateway API objects using explicit deterministic kind ordering**

Ordering categories:

```text
Namespace
Secret/ConfigMap
Service
Deployment/Pod
GatewayClass
Gateway
ReferenceGrant
HTTPRoute
```

Unknown kinds preserve source order after known prerequisites.

- [ ] **Step 3: Apply through kube dynamic API or bounded kubectl; capture created object identities**
- [ ] **Step 4: Do not individually delete on normal completion; ephemeral cluster cleanup is authoritative**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-gateway --test apply_unit
git add crates/admissionlab-gateway
git commit -m "feat(gateway): persist Gateway fixtures in lab clusters"
```

## Task 6.3 — Define normalized Gateway condition evidence

**Files:**
- Create: `crates/admissionlab-gateway/src/conditions.rs`
- Test: `crates/admissionlab-gateway/tests/conditions.rs`
- Testdata: `testdata/objects/gateway-status/`

**Interfaces:**

```rust
pub enum ConditionState { True, False, Unknown, Missing }

pub struct ObservedCondition {
    pub type_name: String,
    pub state: ConditionState,
    pub reason: Option<String>,
    pub observed_generation: Option<i64>,
}

pub struct RouteParentStatus {
    pub parent: ParentIdentity,
    pub controller_name: Option<String>,
    pub conditions: BTreeMap<String, ObservedCondition>,
}
```

- [ ] **Step 1: Parse `Accepted`, `ResolvedRefs`, and `Programmed` without assuming list order**
- [ ] **Step 2: Preserve `reason` but do not make free-form message text a pass/fail contract**
- [ ] **Step 3: Treat `observedGeneration < metadata.generation` as stale status**
- [ ] **Step 4: Golden-test missing/false/unknown/stale conditions**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-gateway --test conditions
git add crates/admissionlab-gateway testdata/objects/gateway-status
git commit -m "feat(gateway): normalize Gateway API status conditions"
```

## Task 6.4 — Implement reconciliation waiter

**Files:**
- Create: `crates/admissionlab-gateway/src/reconcile.rs`
- Test: `crates/admissionlab-gateway/tests/reconcile_unit.rs`

**Interfaces:**

```rust
pub struct ReconciliationEvidence {
    pub gateway_class: Option<GatewayClassEvidence>,
    pub gateway: GatewayEvidence,
    pub route: RouteEvidence,
    pub elapsed: Duration,
    pub converged: bool,
    pub diagnostics: Vec<Diagnostic>,
}

pub async fn wait_for_route_reconciliation(
    cluster: &ClusterHandle,
    contract: &RouteContract,
    deadline: Instant,
) -> Result<ReconciliationEvidence, GatewayError>;
```

- [ ] **Step 1: Poll Route and Gateway status with capped backoff**
- [ ] **Step 2: Require observedGeneration convergence before declaring stable success/failure**
- [ ] **Step 3: Convergence rule**

For the target parent, a route is converged when status has current observedGeneration and the required positive conditions are present with stable True/False values for two consecutive polls at least 250ms apart. This dampens one transient status update without imposing long sleeps.

- [ ] **Step 4: Timeout produces explicit `converged=false` evidence**

Do not automatically call timeout a route regression until baseline/candidate comparison interprets it.

- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-gateway --test reconcile_unit
git add crates/admissionlab-gateway
git commit -m "feat(gateway): observe reconciled Gateway API state"
```

## Task 6.5 — Build deterministic echo backend

**Files:**
- Implement: `crates/admissionlab-echo/src/main.rs`
- Create: `crates/admissionlab-echo/Dockerfile`
- Create: `fixtures/gateway/backends/echo-a.yaml`
- Create: `fixtures/gateway/backends/echo-b.yaml`
- Test: `crates/admissionlab-echo/tests/http.rs`

**Interfaces:**

Echo JSON response:

```json
{
  "backend": "echo-a",
  "method": "GET",
  "path": "/payments",
  "host": "api.example.test",
  "headers": {"x-test": "value"}
}
```

- [ ] **Step 1: Implement HTTP server with backend ID from `ADMISSIONLAB_BACKEND_ID`**
- [ ] **Step 2: Sort/normalize echoed headers and exclude hop-by-hop headers**
- [ ] **Step 3: Add optional response delay endpoint/config for later timeout tests, default 0ms**
- [ ] **Step 4: Build a minimal non-root container image**
- [ ] **Step 5: Pin image digest in test fixture/release metadata after publishing; local development may `kind load docker-image`**
- [ ] **Step 6: Commit**

```bash
cargo test -p admissionlab-echo
git add crates/admissionlab-echo fixtures/gateway/backends
git commit -m "test(gateway): add deterministic HTTP echo backend"
```

## Task 6.6 — Define recipe-driven Gateway endpoint resolution capability

**Files:**
- Create: `crates/admissionlab-gateway/src/endpoint.rs`
- Modify: `crates/admissionlab-recipes/src/capability.rs`
- Test: `crates/admissionlab-gateway/tests/endpoint.rs`

**Interfaces:**

```rust
pub enum GatewayEndpointStrategy {
    ServiceBySelector { namespace: String, selector: BTreeMap<String, String>, port_name: Option<String>, port: Option<u16> },
    ServiceByName { namespace: String, name: String, port_name: Option<String>, port: Option<u16> },
}

pub struct GatewayEndpoint { pub namespace: String, pub service: String, pub port: u16 }

#[async_trait]
pub trait GatewayEndpointResolver {
    async fn resolve(&self, cluster: &ClusterHandle, gateway: &GatewayIdentity, strategy: &GatewayEndpointStrategy) -> Result<GatewayEndpoint, GatewayError>;
}
```

- [ ] **Step 1: Keep strategy in capability/install metadata, not semantic classification**
- [ ] **Step 2: If selector matches zero or multiple Services, return diagnostic with candidate names**
- [ ] **Step 3: Resolve named port to concrete port**
- [ ] **Step 4: Commit**

```bash
cargo test -p admissionlab-gateway --test endpoint
git add crates/admissionlab-gateway crates/admissionlab-recipes
git commit -m "feat(gateway): resolve recipe-declared Gateway endpoints"
```

## Task 6.7 — Implement managed `kubectl port-forward` process

**Files:**
- Create: `crates/admissionlab-gateway/src/port_forward.rs`
- Test: `crates/admissionlab-gateway/tests/port_forward_unit.rs`

**Interfaces:**

```rust
pub struct PortForwardHandle { pub local_addr: SocketAddr, child: ManagedChild }

pub async fn start_service_port_forward(
    runner: &dyn ProcessSpawner,
    cluster: &ClusterHandle,
    endpoint: &GatewayEndpoint,
) -> Result<PortForwardHandle, GatewayError>;
```

- [ ] **Step 1: Start argv equivalent to**

```text
kubectl --kubeconfig <path> -n <namespace> port-forward service/<name> :<remote-port> --address 127.0.0.1
```

- [ ] **Step 2: Parse the selected local port from stdout with timeout**
- [ ] **Step 3: Treat premature child exit as Gateway lab failure and include stderr**
- [ ] **Step 4: Ensure child process terminates on handle close/run cleanup**
- [ ] **Step 5: Unit-test parser with IPv4/IPv6 output variants but bind only 127.0.0.1 in v1**
- [ ] **Step 6: Commit**

```bash
cargo test -p admissionlab-gateway --test port_forward_unit
git add crates/admissionlab-gateway
git commit -m "feat(gateway): manage local Gateway port forwards"
```

## Task 6.8 — Implement HTTP probe engine

**Files:**
- Create: `crates/admissionlab-gateway/src/probe.rs`
- Test: `crates/admissionlab-gateway/tests/probe.rs`

**Interfaces:**

```rust
pub struct HttpProbeResult {
    pub status: u16,
    pub backend: Option<String>,
    pub response_headers: BTreeMap<String, String>,
    pub response_body_sha256: String,
    pub elapsed: Duration,
    pub attempts: u32,
}

pub async fn execute_http_probe(endpoint: SocketAddr, contract: &HttpProbeContract) -> Result<HttpProbeResult, GatewayError>;
```

- [ ] **Step 1: Build URL to local port but send requested Host header**
- [ ] **Step 2: Disable automatic redirects by default so redirect behavior can be tested later**
- [ ] **Step 3: Retry only connection-not-ready failures within a short readiness window; do not retry application 4xx/5xx into success**
- [ ] **Step 4: Parse Admission Lab echo JSON to backend ID when content type/schema match**
- [ ] **Step 5: Redact request Authorization/Cookie headers before persistence**
- [ ] **Step 6: Commit**

```bash
cargo test -p admissionlab-gateway --test probe
git add crates/admissionlab-gateway
git commit -m "feat(gateway): probe HTTP behavior through Gateway dataplane"
```

## Task 6.9 — Add Gateway semantic change categories and comparator

**Files:**
- Create: `crates/admissionlab-gateway/src/diff.rs`
- Modify: `crates/admissionlab-diff/src/types.rs`
- Test: `crates/admissionlab-gateway/tests/diff.rs`

**Interfaces:** new serialized semantic kinds:

```text
route_attached
route_detached
backend_resolution_changed
listener_binding_changed
accepted_condition_changed
resolved_refs_condition_changed
programmed_condition_changed
traffic_status_changed
traffic_backend_changed
```

```rust
pub fn diff_gateway(baseline: &GatewayCaseResult, candidate: &GatewayCaseResult) -> Vec<SemanticChange>;
```

- [ ] **Step 1: `Accepted True -> False` yields condition change; route parent disappearance yields `route_detached`**
- [ ] **Step 2: `ResolvedRefs True -> False` yields backend-resolution change plus condition evidence**
- [ ] **Step 3: `Programmed True -> False` is critical by default**
- [ ] **Step 4: same 200 status but different echo backend => `traffic_backend_changed`**
- [ ] **Step 5: baseline converged and candidate timeout/inconclusive becomes critical only when candidate lacks a previously stable required condition or traffic contract; otherwise surface inconclusive lab evidence**
- [ ] **Step 6: Add exact Gateway severity defaults**

| Gateway semantic kind | Default severity |
|---|---|
| `route_attached` | Info |
| `route_detached` | Critical |
| `backend_resolution_changed` | Critical |
| `listener_binding_changed` | Critical |
| `accepted_condition_changed` | Critical |
| `resolved_refs_condition_changed` | Critical |
| `programmed_condition_changed` | Critical |
| `traffic_status_changed` | Critical |
| `traffic_backend_changed` | Critical |

A condition change that moves from False/Unknown to True may be downgraded to Info by the comparator because it is an improvement; True to False is Critical. The comparator must encode direction rather than relying on free-form reason strings.

- [ ] **Step 7: Commit**

```bash
cargo test -p admissionlab-gateway --test diff
git add crates/admissionlab-gateway crates/admissionlab-diff
git commit -m "feat(gateway): classify Gateway behavior regressions"
```

## Task 6.10 — Add Istio Gateway API certified recipe

**Files:**
- Create: `recipes/istio-gateway/recipe.yaml`
- Create: `recipes/istio-gateway/README.md`
- Create: `fixtures/gateway/istio/`
- Test: `crates/admissionlab-recipes/tests/istio_gateway_recipe.rs`

**Interfaces:** recipe installs Istio Gateway API support, declares Gateway capability and endpoint resolution strategy, and includes no severity logic.

- [ ] **Step 1: Reuse common Istio install metadata without duplicating source-of-truth versions**
- [ ] **Step 2: Pin supported Gateway API CRD/version bundle required by the chosen Istio release**
- [ ] **Step 3: Add fixture with Gateway + HTTPRoute + same-namespace backend**
- [ ] **Step 4: Add fixture with cross-namespace backend + ReferenceGrant**
- [ ] **Step 5: Integration-test Accepted/ResolvedRefs/Programmed and 200-to-expected-backend**
- [ ] **Step 6: Commit**

```bash
git add recipes/istio-gateway fixtures/gateway/istio crates/admissionlab-recipes
git commit -m "feat(recipes): certify Istio Gateway API"
```

## Task 6.11 — Integrate Gateway suite into top-level runner and reports

**Files:**
- Modify: `crates/admissionlab-core/src/run.rs`
- Modify: `crates/admissionlab-report/src/{model.rs,terminal.rs,json.rs,html.rs}`
- Test: `crates/admissionlab-cli/tests/gateway_e2e.rs`

**Interfaces:** `admissionlab test` detects configured Gateway suite and, after stacks are installed, runs persisted Gateway cases on both sides.

- [ ] **Step 1: Run Gateway admission apply separately from admission-only dry-run fixtures**
- [ ] **Step 2: Capture reconciliation evidence before traffic probes**
- [ ] **Step 3: Skip traffic probe with explicit reason when route is not programmed/resolved; do not hide the status regression**
- [ ] **Step 4: Add Gateway sections to terminal/JSON/HTML**
- [ ] **Step 5: Ensure policy engine receives Gateway semantic changes through the same generic `SemanticChange` channel**
- [ ] **Step 6: Commit**

```bash
cargo test -p admissionlab-cli --test gateway_e2e -- --ignored --nocapture
git add crates/admissionlab-core crates/admissionlab-report crates/admissionlab-cli
git commit -m "feat(gateway): integrate Gateway behavior into lab runs"
```

## Task 6.12 — Canonical Istio Gateway regression demo

**Files:**
- Create: `examples/gateway-istio/admissionlab.yaml`
- Create: `examples/gateway-istio/fixtures/`
- Test: `crates/admissionlab-cli/tests/gateway_demo.rs`

**Scenario:** baseline route is Accepted/ResolvedRefs/Programmed and reaches `echo-a`; candidate intentionally removes/changes ReferenceGrant or listener namespace permission so the same route no longer resolves/attaches.

- [ ] **Step 1: Keep candidate difference deterministic and owned by example manifests, not an unstable upstream bug**
- [ ] **Step 2: Assert first user-facing failure includes route name, changed condition, reason, and skipped/failed traffic behavior**
- [ ] **Step 3: Assert raw controller messages are preserved but pass/fail does not depend on exact free-form message text**
- [ ] **Step 4: Commit**

```bash
git add examples/gateway-istio crates/admissionlab-cli/tests/gateway_demo.rs
git commit -m "test(gateway): add canonical Istio Gateway regression demo"
```

## Phase 6 Gateway Engine Exit Gate

```bash
cargo test -p admissionlab-gateway
cargo test -p admissionlab-cli --test gateway_demo -- --ignored --nocapture
```

**Must be true:**
- admission, reconciliation, and traffic evidence are distinct in result model;
- current observedGeneration is required before a route is called converged;
- baseline/candidate route conditions are normalized independent of list order;
- port-forward processes are cleaned on all paths;
- a route that changes backend while remaining HTTP 200 is caught;
- Istio Gateway recipe passes on primary Kubernetes version.

---

# PHASE 7 — Public Beta Contract Freeze and Compatibility Matrix

**Goal:** Freeze the first versioned public config/result/run contracts, complete GitHub CI UX, and certify Public Beta across the supported Kubernetes window.

## Task 7.1 — Promote config to `admissionlab.io/v1beta1`

**Files:**
- Modify: `crates/admissionlab-spec/src/model.rs`
- Create: `crates/admissionlab-spec/src/v1alpha1.rs`
- Create: `crates/admissionlab-spec/src/v1beta1.rs`
- Create: `crates/admissionlab-spec/src/migrate.rs`
- Create: `schemas/admissionlab-v1beta1.json`
- Test: `crates/admissionlab-spec/tests/migrate_alpha_beta.rs`

**Interfaces:**

```rust
pub fn load_any_supported_lab(path: &Path) -> Result<ResolvedLab, SpecError>;
pub fn migrate_v1alpha1_to_v1beta1(old: V1Alpha1Lab) -> Result<V1Beta1Lab, MigrationError>;
```

- [ ] **Step 1: Freeze explicit Beta fields for admission fixtures, Gateway suite, policy, expectations, output, and component install metadata**
- [ ] **Step 2: Maintain read support for Public Alpha configs through at least v1.0**
- [ ] **Step 3: Reject ambiguous migrations rather than invent defaults**
- [ ] **Step 4: Publish generated Beta JSON Schema**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-spec --test migrate_alpha_beta
git add crates/admissionlab-spec schemas/admissionlab-v1beta1.json
git commit -m "feat(spec): freeze v1beta1 lab schema"
```

## Task 7.2 — Freeze Beta result schema

**Files:**
- Modify: `crates/admissionlab-report/src/model.rs`
- Create: `schemas/result-v1beta1.json`
- Test: `crates/admissionlab-report/tests/result_schema.rs`
- Golden: `testdata/golden/result-v1beta1.json`

**Interfaces:** JSON reports now use `schemaVersion: admissionlab.io/result/v1beta1`.

- [ ] **Step 1: Include evidence/confidence fields explicitly for first divergence and trace availability**
- [ ] **Step 2: Include separate `admission`, `gatewayReconciliation`, and `traffic` evidence sections**
- [ ] **Step 3: Include semantic change IDs stable within a run**
- [ ] **Step 4: Generate the Beta result JSON Schema from the Rust result model with `schemars`, check it in, and validate the golden output against that schema**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-report --test result_schema
git add crates/admissionlab-report schemas/result-v1beta1.json testdata/golden/result-v1beta1.json
git commit -m "feat(report): freeze v1beta1 result schema"
```

## Task 7.3 — Promote run manifest to v1beta1 and migration policy

**Files:**
- Modify: `crates/admissionlab-core/src/run_manifest.rs`
- Create: `schemas/run-manifest-v1beta1.json`
- Create: `docs/schema-migrations.md`
- Test: `crates/admissionlab-core/tests/run_manifest_beta.rs`

- [ ] **Step 1: Add feature/capability versions needed for reproduction**
- [ ] **Step 2: Document compatibility rule**

Before v1.0:
- Beta readers may add optional fields.
- Existing field semantics cannot change silently.
- Removing/renaming fields requires a new schema version and migration note.

At v1.0, stable schema rules become stricter in Phase 9.

- [ ] **Step 3: Commit**

```bash
git add crates/admissionlab-core schemas/run-manifest-v1beta1.json docs/schema-migrations.md
git commit -m "feat(core): freeze v1beta1 run manifest"
```

## Task 7.4 — Build compatibility-matrix loader and validation

**Files:**
- Modify: `compatibility/kubernetes.yaml`
- Modify: `compatibility/recipes.yaml`
- Create: `crates/admissionlab-recipes/src/compatibility.rs`
- Test: `crates/admissionlab-recipes/tests/compatibility.rs`

**Interfaces:**

```rust
pub struct CertifiedCombination {
    pub kubernetes: String,
    pub recipe: String,
    pub recipe_version: String,
    pub tier: CertificationTier,
}

pub enum CertificationTier { PerCommit, Nightly, WeeklyRelease }
```

- [ ] **Step 1: Validate exactly three Kubernetes minors are marked supported for a release candidate unless upstream support window temporarily differs and release notes explain it**
- [ ] **Step 2: Validate recipe versions reference known pinned install metadata**
- [ ] **Step 3: CLI warns when a requested combination is supported Kubernetes but not certified recipe matrix; do not refuse generic user-defined stacks**
- [ ] **Step 4: Commit**

```bash
cargo test -p admissionlab-recipes --test compatibility
git add compatibility crates/admissionlab-recipes
git commit -m "feat(recipes): model certified compatibility combinations"
```

## Task 7.5 — Implement tiered recipe matrix workflows

**Files:**
- Create: `.github/workflows/recipe-matrix.yml`
- Modify: `.github/workflows/nightly.yml`
- Create: `scripts/update-kubernetes-matrix.sh`

**Interfaces:**

Tier 1 per commit:
```text
primary K8s + dogfood + current Kyverno + current Istio + current Istio Gateway
```

Tier 2 nightly:
```text
latest 3 K8s minors x selected recipe versions
```

Tier 3 weekly/release:
```text
expanded supported combinations + reliability repetition + Gateway demos
```

- [ ] **Step 1: Generate CI matrix from checked-in compatibility YAML, not duplicated hardcoded arrays**
- [ ] **Step 2: `update-kubernetes-matrix.sh` fetches/proposes latest supported data but never edits release support without a reviewable diff**
- [ ] **Step 3: Cache container pulls where GitHub supports it without sharing mutable run state**
- [ ] **Step 4: Upload failed lab reports/matrices**
- [ ] **Step 5: Commit**

```bash
git add .github scripts/update-kubernetes-matrix.sh
git commit -m "ci: add tiered Kubernetes recipe certification matrix"
```

## Task 7.6 — Complete Beta GitHub Action UX

**Files:**
- Modify: `.github/actions/admissionlab/action.yml`
- Modify: `docs/github-action.md`
- Test: `.github/workflows/integration.yml`

- [ ] **Step 1: Expose inputs only for config path, Admission Lab version, artifact retention/name, and optional keep-clusters disabled in hosted CI**
- [ ] **Step 2: Do not expose arbitrary shell command inputs**
- [ ] **Step 3: Write `GITHUB_STEP_SUMMARY` from CLI-rendered Markdown file**
- [ ] **Step 4: Verify exit 1 marks the job failed while artifacts still upload using `if: always()`**
- [ ] **Step 5: Commit**

```bash
git add .github/actions docs/github-action.md .github/workflows/integration.yml
git commit -m "feat(ci): harden Admission Lab GitHub Action UX"
```

## Task 7.7 — Public Beta docs and architecture references

**Files:**
- Modify: `README.md`
- Modify/Create: `docs/architecture.md`
- Modify: `docs/config.md`
- Modify: `docs/fixtures.md`
- Modify: `docs/recipes.md`
- Modify: `docs/troubleshooting.md`
- Create: `docs/compatibility.md`

- [ ] **Step 1: Document three-layer Gateway model**
- [ ] **Step 2: Document status convergence and observedGeneration semantics**
- [ ] **Step 3: Document why Programmed does not itself prove traffic success; traffic probe is separate**
- [ ] **Step 4: Document certified vs merely user-configurable combinations**
- [ ] **Step 5: Add exact beta JSON/config schema links inside repo docs**
- [ ] **Step 6: Commit**

```bash
git add README.md docs
git commit -m "docs: publish Public Beta usage and contracts"
```

## Phase 7 / Public Beta Exit Gate

Mandatory:

```bash
cargo test --workspace
cargo test -p admissionlab-cli --test alpha_e2e -- --ignored --nocapture
cargo test -p admissionlab-cli --test gateway_demo -- --ignored --nocapture
```

Run Tier 2 across all three Kubernetes minors. Public Beta cannot ship with a red current certified recipe unless the recipe is removed from certified metadata and release notes explain why.

**Must be true:**
- Alpha config migration test passes;
- Beta result schema is checked in and golden-tested;
- GitHub Action works on an actual PR;
- Istio admission and Istio Gateway are certified;
- all three supported Kubernetes minors pass core dogfood suite;
- docs never call a non-certified user combination “supported/certified.”

---

# PHASE 8 — NGINX Gateway Fabric, Legacy ingress-nginx, and Ingress-to-Gateway Behavior Migration

**Goal:** Add the second Gateway implementation and prove Admission Lab can compare legacy Ingress behavior with Gateway API behavior rather than only compare YAML syntax.

## Task 8.1 — Add NGINX Gateway Fabric certified recipe

**Files:**
- Create: `recipes/nginx-gateway-fabric/recipe.yaml`
- Create: `recipes/nginx-gateway-fabric/README.md`
- Create: `fixtures/gateway/nginx/`
- Test: `crates/admissionlab-recipes/tests/nginx_gateway_recipe.rs`

**Interfaces:** same generic Gateway capability model; NGINX-specific endpoint resolution stays recipe metadata.

- [ ] **Step 1: Pin a current stable NGINX Gateway Fabric version compatible with the supported Gateway API version**
- [ ] **Step 2: Define install/readiness/endpoint resolver**
- [ ] **Step 3: Run the same core HTTPRoute/ReferenceGrant contracts used for Istio wherever portable**
- [ ] **Step 4: Keep implementation-specific fixture pack separate and labeled**
- [ ] **Step 5: Commit**

```bash
git add recipes/nginx-gateway-fabric fixtures/gateway/nginx crates/admissionlab-recipes
git commit -m "feat(recipes): certify NGINX Gateway Fabric"
```

## Task 8.2 — Add legacy community ingress-nginx compatibility recipe

**Files:**
- Create: `recipes/ingress-nginx-legacy/recipe.yaml`
- Create: `recipes/ingress-nginx-legacy/README.md`
- Create: `fixtures/migration/ingress-nginx/`
- Test: `crates/admissionlab-recipes/tests/ingress_nginx_legacy.rs`

**Interfaces:** capability is `LegacyIngress`; certification metadata clearly marks the upstream project as legacy/archived and pins the exact tested chart/controller release.

- [ ] **Step 1: Do not use floating or “latest” legacy chart versions**
- [ ] **Step 2: Install only for migration compatibility tests, not as the product’s strategic ingress recommendation**
- [ ] **Step 3: Add endpoint resolver for its controller Service**
- [ ] **Step 4: Add a validating-webhook smoke fixture if the pinned release exposes it**
- [ ] **Step 5: Commit**

```bash
git add recipes/ingress-nginx-legacy fixtures/migration/ingress-nginx crates/admissionlab-recipes
git commit -m "feat(recipes): add legacy ingress-nginx migration recipe"
```

## Task 8.3 — Define migration-suite pairing model

**Files:**
- Create: `crates/admissionlab-gateway/src/migration.rs`
- Modify: `crates/admissionlab-spec/src/v1beta1.rs`
- Test: `crates/admissionlab-gateway/tests/migration_model.rs`

**Interfaces:**

```rust
pub struct MigrationSuiteSpec {
    pub cases: Vec<MigrationCaseSpec>,
}

pub struct MigrationCaseSpec {
    pub id: String,
    pub baseline_ingress_manifests: Vec<PathBuf>,
    pub candidate_gateway_manifests: Vec<PathBuf>,
    pub probes: Vec<HttpProbeContract>,
    pub expected_nonportable: Vec<NonPortableFeatureExpectation>,
}
```

- [ ] **Step 1: Require explicit baseline/candidate manifest pairing**

Admission Lab v1 does not auto-convert Ingress to Gateway. This suite validates user/tool-produced conversions.

- [ ] **Step 2: Allow explicit nonportable expectations with human reason**
- [ ] **Step 3: Keep migration expectations distinct from generic regression expectations**
- [ ] **Step 4: Commit**

```bash
cargo test -p admissionlab-gateway --test migration_model
git add crates/admissionlab-gateway crates/admissionlab-spec
git commit -m "feat(gateway): define Ingress-to-Gateway migration suites"
```

## Task 8.4 — Implement legacy Ingress behavior runner

**Files:**
- Create: `crates/admissionlab-gateway/src/ingress.rs`
- Test: `crates/admissionlab-gateway/tests/ingress_e2e.rs`

**Interfaces:**

```rust
pub struct IngressCaseResult {
    pub admitted: bool,
    pub ready: bool,
    pub probes: Vec<HttpProbeResult>,
    pub diagnostics: Vec<Diagnostic>,
}
```

- [ ] **Step 1: Persist Ingress + Service + echo backend in baseline cluster**
- [ ] **Step 2: Wait for ingress controller readiness using recipe-specific endpoint, not cloud LoadBalancer status**
- [ ] **Step 3: Probe through local port-forward using same `HttpProbeContract` model**
- [ ] **Step 4: Preserve validating-webhook denial as admission evidence when it occurs**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-gateway --test ingress_e2e -- --ignored --nocapture
git add crates/admissionlab-gateway
git commit -m "feat(gateway): capture legacy Ingress traffic behavior"
```

## Task 8.5 — Implement migration behavior comparator

**Files:**
- Modify: `crates/admissionlab-gateway/src/migration.rs`
- Test: `crates/admissionlab-gateway/tests/migration_diff.rs`

**Interfaces:**

```rust
pub enum MigrationBehaviorKind {
    HostBehaviorChanged,
    PathBehaviorChanged,
    TlsBehaviorChanged,
    BackendChanged,
    RewriteBehaviorChanged,
    RedirectBehaviorChanged,
    NonPortableFeature,
}

pub struct MigrationComparison { pub changes: Vec<MigrationBehaviorChange>, pub probes: Vec<ProbePair> }
```

- [ ] **Step 1: Compare observed probe status/backend/path/redirect location rather than manifest syntax**
- [ ] **Step 2: Add a small reviewed catalog of ingress-nginx annotations with no portable Gateway API equivalent only for warnings such as canary/configuration-snippet**

This catalog lives in migration code/data, not recipe severity logic.

- [ ] **Step 3: An explicitly expected nonportable feature is visible/expected; an unexpected one is warning by default**
- [ ] **Step 4: Commit**

```bash
cargo test -p admissionlab-gateway --test migration_diff
git add crates/admissionlab-gateway
git commit -m "feat(gateway): compare Ingress and Gateway traffic behavior"
```

## Task 8.6 — Add local test certificate generator for TLS contracts

**Files:**
- Create: `crates/admissionlab-gateway/src/tls.rs`
- Test: `crates/admissionlab-gateway/tests/tls.rs`

**Interfaces:**

```rust
pub struct TestCertificate { pub host: String, pub cert_pem: Vec<u8>, pub key_pem: SensitiveBytes, pub ca_pem: Vec<u8> }
pub fn generate_test_certificate(host: &str) -> Result<TestCertificate, GatewayError>;
```

- [ ] **Step 1: Generate ephemeral CA + leaf cert for `.test` hostname using Rust crypto/cert library**
- [ ] **Step 2: Store key only in run workspace/Kubernetes Secret and ensure report redaction removes it**
- [ ] **Step 3: Configure reqwest probe trust with generated CA and host resolution to `127.0.0.1:<forwarded-port>`**
- [ ] **Step 4: Test certificate expires after a short test-safe lifetime and never uses production trust material**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-gateway --test tls
git add crates/admissionlab-gateway
git commit -m "feat(gateway): generate isolated TLS test certificates"
```

## Task 8.7 — Add portable Gateway behavior contracts for v1

**Files:**
- Extend: `fixtures/gateway/portable/`
- Modify: `crates/admissionlab-gateway/src/probe.rs`
- Test: `crates/admissionlab-gateway/tests/portable_contracts.rs`

Required contracts, run against both Istio and NGINX Gateway Fabric where supported:

```text
basic host/path routing
ReferenceGrant cross-namespace backend
TLS termination
RequestHeaderModifier/ResponseHeaderModifier where portable
HTTP redirect
URL rewrite where portable
two-backend weighted routing
```

- [ ] **Step 1: Header contracts assert echoed request headers and returned response headers**
- [ ] **Step 2: Redirect contract disables auto-follow and compares status + normalized Location**
- [ ] **Step 3: Rewrite contract compares backend-observed path**
- [ ] **Step 4: TLS contract connects with generated CA/SNI host**
- [ ] **Step 5: Weighted routing uses a bounded statistical contract**

For expected probability `p` and sample count `n`, accept observed proportion when:

```text
abs(observed - p) <= max(0.05, 4 * sqrt(p * (1-p) / n))
```

Use at least `n=1000` requests for 20/80 or 50/50 fixtures. Record counts and tolerance. Do not classify a single request as weighted-routing correctness.

- [ ] **Step 6: If a portable timeout contract proves stable on both certified implementations, add delayed echo + timeout expectation; otherwise document timeout as deferred rather than shipping a flaky v1 test**
- [ ] **Step 7: Commit**

```bash
git add fixtures/gateway/portable crates/admissionlab-gateway
git commit -m "feat(gateway): add portable v1 Gateway traffic contracts"
```

## Task 8.8 — Add canonical ingress-to-NGINX-Gateway migration example

**Files:**
- Create: `examples/ingress-to-gateway/admissionlab.yaml`
- Create: `examples/ingress-to-gateway/fixtures/`
- Test: `crates/admissionlab-cli/tests/migration_demo.rs`

**Scenario:** baseline ingress-nginx routes host/path to echo backend with rewrite; candidate NGINX Gateway Fabric route intentionally misses/changes one behavior, producing a clear behavior regression.

- [ ] **Step 1: Include one preserved behavior, one expected nonportable behavior, and one unintended regression**
- [ ] **Step 2: Report must explain observed traffic difference, not merely annotation mismatch**
- [ ] **Step 3: Commit**

```bash
git add examples/ingress-to-gateway crates/admissionlab-cli/tests/migration_demo.rs
git commit -m "test(gateway): add Ingress-to-Gateway behavior migration demo"
```

## Task 8.9 — Extend compatibility matrix to NGINX tracks

**Files:**
- Modify: `compatibility/recipes.yaml`
- Modify: `.github/workflows/recipe-matrix.yml`

- [ ] **Step 1: Add NGINX Gateway Fabric to Tier 2/Tier 3**
- [ ] **Step 2: Add legacy ingress-nginx only to migration-specific Tier 3 jobs; do not multiply every general matrix by legacy versions**
- [ ] **Step 3: Run portable Gateway contracts against both certified implementations**
- [ ] **Step 4: Commit**

```bash
git add compatibility/recipes.yaml .github/workflows/recipe-matrix.yml
git commit -m "ci: certify NGINX Gateway and migration suites"
```

## Phase 8 Feature-Complete/v1 RC Gate

```bash
cargo test -p admissionlab-gateway
cargo test -p admissionlab-cli --test gateway_demo -- --ignored --nocapture
cargo test -p admissionlab-cli --test migration_demo -- --ignored --nocapture
```

Run Tier 3 matrix.

**Must be true:**
- same portable HTTPRoute corpus runs against Istio and NGINX Gateway Fabric;
- legacy ingress-nginx is explicitly marked legacy;
- migration suite compares observed behavior and supports expected nonportable differences;
- TLS test secrets never appear in JSON/HTML/CI logs;
- weighted routing test includes sample/tolerance evidence and is not flaky in repeated scheduled runs.

---
# PHASE 9 — v1 Hardening: Security, Stability, Performance, and Stable Contracts

**Goal:** Stop adding product breadth and make the feature-complete system safe, diagnosable, reproducible, and boring enough to call v1.0.

## Task 9.1 — Freeze stable v1 config/result/run schemas

**Files:**
- Create: `schemas/admissionlab-v1.json`
- Create: `schemas/result-v1.json`
- Create: `schemas/run-manifest-v1.json`
- Modify: `crates/admissionlab-spec/src/`
- Modify: `crates/admissionlab-report/src/model.rs`
- Modify: `crates/admissionlab-core/src/run_manifest.rs`
- Modify: `docs/schema-migrations.md`
- Test: `crates/admissionlab-spec/tests/stable_schema.rs`
- Test: `crates/admissionlab-report/tests/stable_schema.rs`

**Interfaces:** stable identifiers:

```text
admissionlab.io/v1
admissionlab.io/result/v1
admissionlab.io/run/v1
```

- [ ] **Step 1: Audit every public Beta field for necessity and naming consistency**
- [ ] **Step 2: Remove experimental fields only before this task lands; document migration**
- [ ] **Step 3: Preserve readers for supported Alpha/Beta input schemas and convert internally to stable domain model**
- [ ] **Step 4: Stable-schema rule**

Within v1.x:
- optional additive fields are allowed when old readers can ignore them;
- existing field meaning cannot change;
- required fields cannot be removed;
- semantic change serialization strings cannot be renamed without a new result schema version;
- exit codes cannot be reassigned.

- [ ] **Step 5: Golden-test stable JSON output and migration fixtures**
- [ ] **Step 6: Commit**

```bash
cargo test -p admissionlab-spec --test stable_schema
cargo test -p admissionlab-report --test stable_schema
git add schemas crates/admissionlab-spec crates/admissionlab-report crates/admissionlab-core docs/schema-migrations.md
git commit -m "feat: freeze Admission Lab v1 schemas"
```

## Task 9.2 — Freeze CLI and exit-code contract

**Files:**
- Modify: `crates/admissionlab-cli/src/exit.rs`
- Modify: `crates/admissionlab-cli/src/main.rs`
- Test: `crates/admissionlab-cli/tests/exit_codes.rs`
- Modify: `docs/troubleshooting.md`

**Stable commands:**

```text
admissionlab doctor
admissionlab test <config>
admissionlab reproduce <run-manifest>
```

Stable exit codes:

```text
0 pass
1 regression policy failed
2 invalid config/fixture
3 lab infrastructure failure
4 install/readiness failure
5 fixture/capture failure
6 internal Admission Lab error
```

- [ ] **Step 1: Table-driven test every typed error family to exact exit code**
- [ ] **Step 2: Ensure `--help`/`--version` always exit 0**
- [ ] **Step 3: Ensure `--keep-clusters` never changes exit meaning**
- [ ] **Step 4: Commit**

```bash
cargo test -p admissionlab-cli --test exit_codes
git add crates/admissionlab-cli docs/troubleshooting.md
git commit -m "feat(cli): freeze v1 command and exit contracts"
```

## Task 9.3 — Security harden audit policy and artifacts

**Files:**
- Modify: `crates/admissionlab-cluster/src/audit.rs`
- Modify: `crates/admissionlab-report/src/redact.rs`
- Modify: `docs/security.md`
- Test: `crates/admissionlab-report/tests/security_sentinels.rs`
- Test: `crates/admissionlab-cluster/tests/audit_policy_security.rs`

- [ ] **Step 1: Enumerate audit policy resource rules and prove Secrets are never logged at Request/RequestResponse body level**
- [ ] **Step 2: Add sentinel corpus containing tokens, password env values, private keys, Secret data/stringData, Authorization/Cookie headers**
- [ ] **Step 3: Run all terminal/JSON/HTML/GitHub renderers and assert sentinel bytes do not occur**
- [ ] **Step 4: Verify run-manifest contains hashes/metadata only, not raw kubeconfig/private test CA key**
- [ ] **Step 5: Document that installed test charts/controllers can make outbound network calls unless the user isolates CI networking**
- [ ] **Step 6: Commit**

```bash
cargo test -p admissionlab-report --test security_sentinels
cargo test -p admissionlab-cluster --test audit_policy_security
git add crates/admissionlab-cluster crates/admissionlab-report docs/security.md
git commit -m "security: harden audit and report data handling"
```

## Task 9.4 — Harden subprocess and child-process lifecycle

**Files:**
- Modify: `crates/admissionlab-core/src/process.rs`
- Modify: `crates/admissionlab-gateway/src/port_forward.rs`
- Test: `crates/admissionlab-core/tests/process_hardening.rs`

- [ ] **Step 1: Add process-group/job control so timed-out kind/helm/kubectl and port-forward children do not remain orphaned**

On Unix, terminate child then force-kill after grace period. On unsupported platforms, document behavior and verify no shell interpolation.

- [ ] **Step 2: Cap stdout/stderr kept in memory and spill larger output to run log files**
- [ ] **Step 3: Include tail excerpts in errors, never megabytes of command output**
- [ ] **Step 4: Add adversarial argv test containing spaces, quotes, semicolons, `$()`, and newline characters; confirm they remain literal argv and are never executed by a shell**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-core --test process_hardening
git add crates/admissionlab-core crates/admissionlab-gateway
git commit -m "security: harden external process lifecycle"
```

## Task 9.5 — Add complete diagnostics bundles for infrastructure/install failures

**Files:**
- Create/Modify: `crates/admissionlab-cluster/src/diagnostics.rs`
- Modify: `crates/admissionlab-installer/src/stack.rs`
- Modify: `crates/admissionlab-report/src/model.rs`
- Test: `crates/admissionlab-cluster/tests/diagnostics_unit.rs`

**Interfaces:**

```rust
pub struct ClusterDiagnostics {
    pub nodes: Vec<RedactedObjectSummary>,
    pub pods: Vec<RedactedObjectSummary>,
    pub events: Vec<RedactedEvent>,
    pub webhook_configurations: Vec<RedactedObjectSummary>,
    pub kind_logs_path: Option<PathBuf>,
}
```

- [ ] **Step 1: On cluster/setup failure, collect `kind export logs` when a cluster exists**
- [ ] **Step 2: Collect relevant namespace Pods/events and webhook configuration summaries**
- [ ] **Step 3: On readiness timeout, include last object conditions and pod failure reasons**
- [ ] **Step 4: Redact before embedding diagnostics in reports; raw kind logs remain local artifact with warning that third-party components may log secrets**
- [ ] **Step 5: Commit**

```bash
cargo test -p admissionlab-cluster --test diagnostics_unit
git add crates/admissionlab-cluster crates/admissionlab-installer crates/admissionlab-report
git commit -m "feat(cluster): preserve actionable failure diagnostics"
```

## Task 9.6 — Reliability-test cancellation, Ctrl-C, and partial failures

**Files:**
- Modify: `crates/admissionlab-core/src/run.rs`
- Modify: `crates/admissionlab-cli/src/main.rs`
- Test: `crates/admissionlab-cli/tests/cancellation.rs`
- Modify: `scripts/verify-cleanup.sh`

- [ ] **Step 1: Handle SIGINT/SIGTERM with cooperative cancellation**
- [ ] **Step 2: Stop starting new work after cancellation**
- [ ] **Step 3: Attempt port-forward termination, report flush, then cluster cleanup unless keep-clusters**
- [ ] **Step 4: Second interrupt may force immediate exit but prints remaining cluster cleanup commands if possible**
- [ ] **Step 5: Integration-test interrupt during install and fixture phases**
- [ ] **Step 6: Commit**

```bash
git add crates/admissionlab-core crates/admissionlab-cli scripts/verify-cleanup.sh
git commit -m "feat(core): cleanly cancel and tear down lab runs"
```

## Task 9.7 — Validate latest-three-Kubernetes support at v1 RC

**Files:**
- Modify: `compatibility/kubernetes.yaml`
- Modify: `compatibility/recipes.yaml`
- Modify: `.github/workflows/recipe-matrix.yml`
- Modify: `docs/compatibility.md`

At the time this roadmap was authored, upstream maintained branches are Kubernetes 1.37, 1.36, and 1.35. The release task must re-check upstream support immediately before v1.0 and update the checked-in exact patch/image digests through review.

- [ ] **Step 1: Resolve exact latest patch for each supported minor and official kind node image digest**
- [ ] **Step 2: Run core dogfood admission suite on all three**
- [ ] **Step 3: Run certified Kyverno/Istio/Istio Gateway/NGINX Gateway combinations declared for each minor**
- [ ] **Step 4: If a vendor recipe cannot support one upstream-supported Kubernetes minor, document that recipe limitation explicitly; core Kubernetes support must still pass**
- [ ] **Step 5: Commit exact matrix and release notes**

```bash
git add compatibility .github/workflows/recipe-matrix.yml docs/compatibility.md
git commit -m "ci: finalize v1 Kubernetes compatibility matrix"
```

## Task 9.8 — Enforce performance and flake budgets

**Files:**
- Modify: `scripts/benchmark-alpha.sh`
- Create: `scripts/benchmark-gateway.sh`
- Modify: `.github/workflows/nightly.yml`
- Create: `docs/performance.md`

- [ ] **Step 1: Admission target**

On project reference CI runner class, excluding component install:
- 100 ordinary admission fixtures <= approximately 5 minutes total fixture stage;
- semantic comparison of pre-captured 100 fixtures < 1 second.

- [ ] **Step 2: Cluster target**

Record median and p95 kind create time; approximately 90 seconds per cluster is a target, not a PR hard fail due to hosted-runner variance.

- [ ] **Step 3: Gateway target**

Basic deterministic route suite must avoid arbitrary sleeps; report reconcile and probe timings.

- [ ] **Step 4: Flake budget**

Run canonical dogfood admission demo 100 times and Gateway demos 50 times in scheduled/sharded CI. Any unexplained false regression or cross-correlation is release-blocking. Infrastructure failures are tracked separately but repeated environment-induced flakes require diagnosis.

- [ ] **Step 5: Commit**

```bash
git add scripts .github/workflows/nightly.yml docs/performance.md
git commit -m "perf: enforce v1 reliability and performance budgets"
```

## Task 9.9 — Packaging, checksums, SBOM, and release provenance

**Files:**
- Modify: `.github/workflows/release.yml`
- Create: `scripts/verify-release.sh`
- Create: `docs/install.md`

**Release artifacts:**

```text
admissionlab-<version>-x86_64-unknown-linux-gnu.tar.gz
admissionlab-<version>-aarch64-unknown-linux-gnu.tar.gz
admissionlab-<version>-x86_64-apple-darwin.tar.gz
admissionlab-<version>-aarch64-apple-darwin.tar.gz
SHA256SUMS
SBOM.spdx.json
```

- [ ] **Step 1: Build release binaries with locked Cargo dependencies**
- [ ] **Step 2: Generate SHA-256 checksums and SPDX/CycloneDX SBOM using a pinned release tool/action**
- [ ] **Step 3: Ensure GitHub Action installer verifies checksums**
- [ ] **Step 4: Run binary smoke tests on Linux and macOS artifacts**
- [ ] **Step 5: Document WSL2 path for Windows rather than claiming unsupported native parity**
- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release.yml scripts/verify-release.sh docs/install.md
git commit -m "build: add verifiable v1 release artifacts"
```

## Task 9.10 — Dependency and supply-chain policy

**Files:**
- Modify: `deny.toml`
- Modify: `.github/workflows/ci.yml`
- Create: `docs/dependencies.md`

- [ ] **Step 1: Run `cargo deny check advisories bans licenses sources` in CI**
- [ ] **Step 2: Run RustSec/cargo-audit equivalent through a pinned CI tool**
- [ ] **Step 3: Fail `cargo deny bans` on duplicate major versions of the selected HTTP/TLS stack unless an explicit `deny.toml` exception names the crates, versions, transitive owners, and removal issue**
- [ ] **Step 4: Review all git dependencies; stable release must not depend on mutable branch refs**
- [ ] **Step 5: Document dependency update cadence and emergency security update process**
- [ ] **Step 6: Commit**

```bash
git add deny.toml .github/workflows/ci.yml docs/dependencies.md
git commit -m "security: enforce dependency supply-chain policy"
```

## Task 9.11 — Finalize docs, governance, and support boundaries

**Files:**
- Modify: `README.md`
- Modify: `CONTRIBUTING.md`
- Modify: `SECURITY.md`
- Modify: `docs/*`
- Create: `CHANGELOG.md`
- Create: `docs/versioning.md`

- [ ] **Step 1: Document core vs certified recipe vs user-supplied stack support**
- [ ] **Step 2: Document schema/CLI semantic-version promises**
- [ ] **Step 3: Document security-report channel and supported release lines**
- [ ] **Step 4: Add “Known regressions Admission Lab catches” section backed only by deterministic repository fixtures or externally reproducible issues**
- [ ] **Step 5: Remove stale Alpha/Beta warnings from stable features while keeping historical migration docs**
- [ ] **Step 6: Commit**

```bash
git add README.md CONTRIBUTING.md SECURITY.md CHANGELOG.md docs
git commit -m "docs: finalize Admission Lab v1 project contracts"
```

## Phase 9 / v1 RC Exit Gate

Run all of:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check
./scripts/verify-cleanup.sh 100
./scripts/benchmark-alpha.sh
./scripts/benchmark-gateway.sh
./scripts/verify-release.sh
```

Then run Tier 3 compatibility matrix and scheduled repetition suites.

**Release blockers:**
- any secret sentinel appears in a user-facing report;
- any known false first-divergence claim;
- leaked cluster/port-forward child in normal/cancellation paths;
- stable schema golden mismatch without explicit migration;
- red core suite on any of the latest three supported Kubernetes minors;
- canonical admission/Gateway/migration demos fail to catch their seeded regressions;
- unexplained flaky weighted-routing or Gateway convergence test.

---

# PHASE 10 — v1.0 Release and Maintenance Handoff

**Goal:** Cut a reproducible v1.0 release, prove installation/CI docs from scratch, and establish a maintenance loop without adding new product scope.

## Task 10.1 — Create v1.0 release candidate from a clean clone

**Files:**
- Modify: `CHANGELOG.md`
- Modify: version fields in workspace crates
- Generate release artifacts through CI only

- [ ] **Step 1: Bump all public crates/binary to `1.0.0-rc.1` in one reviewed commit**
- [ ] **Step 2: Regenerate schemas and verify no uncommitted diff**
- [ ] **Step 3: Run Phase 9 full gate**
- [ ] **Step 4: Install RC artifact into a fresh Linux CI runner and macOS runner; run `doctor`, basic admission example, and Istio Gateway example**
- [ ] **Step 5: Open a real repository PR using the RC GitHub Action and verify policy failure + artifact upload behavior**
- [ ] **Step 6: Commit changelog/version bump**

```bash
git add Cargo.toml Cargo.lock crates CHANGELOG.md schemas
git commit -m "release: prepare Admission Lab 1.0.0-rc.1"
```

## Task 10.2 — Conduct manual v1 acceptance checklist

No code is written unless an issue is discovered.

- [ ] `admissionlab doctor` tells a new user exactly which prerequisite is missing.
- [ ] invalid config fails before creating clusters.
- [ ] `--keep-clusters` prints exact cleanup commands.
- [ ] normal failure cleans all clusters/port-forwards.
- [ ] semantic diff hides harmless Kubernetes metadata noise.
- [ ] expected changes remain visible and stale expectations are reported.
- [ ] first divergence says unknown when evidence cannot prove it.
- [ ] HTML opens offline.
- [ ] JSON validates against stable schema.
- [ ] `reproduce` rejects modified fixture hash.
- [ ] GitHub Action preserves exit code and artifacts.
- [ ] Kyverno recipe works on its declared combinations.
- [ ] Istio admission recipe works on its declared combinations.
- [ ] Istio Gateway recipe works on its declared combinations.
- [ ] NGINX Gateway Fabric recipe works on its declared combinations.
- [ ] legacy ingress-nginx migration example is clearly labeled legacy.
- [ ] all latest-three-Kubernetes core combinations pass.

Document results in the release PR checklist.

## Task 10.3 — Fix only release blockers and rerun complete gates

**Rule:** no new feature enters the RC stabilization window. A discovered non-blocking enhancement becomes a post-v1 issue.

For each blocker:

```text
reproduce -> failing test -> minimal fix -> narrow test -> full phase gate -> commit
```

Use normal TDD/review; do not batch unrelated RC fixes.

## Task 10.4 — Cut `v1.0.0`

- [ ] **Step 1: Change version from RC to `1.0.0`**
- [ ] **Step 2: Finalize `CHANGELOG.md` with supported K8s minors and certified recipe versions**
- [ ] **Step 3: Run release workflow from signed/protected tag `v1.0.0`**
- [ ] **Step 4: Verify checksums/SBOM/binaries and GitHub Action install path**
- [ ] **Step 5: Run one post-release smoke using the published artifact, not workspace binary**
- [ ] **Step 6: Announce only features actually certified in release metadata**

Commit/tag:

```bash
git add Cargo.toml Cargo.lock crates CHANGELOG.md
git commit -m "release: Admission Lab 1.0.0"
git tag v1.0.0
```

## Task 10.5 — Establish post-release maintenance automation

**Files:**
- Modify: `.github/workflows/nightly.yml`
- Modify: `.github/workflows/recipe-matrix.yml`
- Modify: `docs/versioning.md`

- [ ] **Step 1: Keep nightly core + recipe certification running on main**
- [ ] **Step 2: Open reviewed PRs when Kubernetes support window changes**
- [ ] **Step 3: Open reviewed recipe-version update PRs; never auto-certify merely because a chart version exists**
- [ ] **Step 4: Define patch-release rule for security/reliability bugs and minor-release rule for backward-compatible features**
- [ ] **Step 5: Commit**

```bash
git add .github docs/versioning.md
git commit -m "ci: establish Admission Lab post-v1 maintenance loop"
```

## v1.0 Definition of Done

A user with Docker, kind, kubectl, and helm can install the published binary and perform these supported workflows without any Admission Lab server/account:

```text
admission regression: baseline vs candidate
Kyverno + Istio certified examples
semantic policy gate + expectations
terminal/JSON/HTML artifacts
GitHub Action
run manifest + reproduce
Istio Gateway API behavior
NGINX Gateway Fabric behavior
legacy Ingress -> Gateway migration behavior
latest-three-Kubernetes core compatibility
```

The project is still Apache-2.0, local-first, deterministic, vendor-neutral at core, and fully useful without any hosted service.

---

# POST-v1 — Gated Exploration Backlog

> **DO NOT EXECUTE THESE TRACKS AUTOMATICALLY.** They are product candidates from `PRODUCT.md`, not approved v1 commitments. Each track requires fresh problem validation and a new Superpowers brainstorming/spec approval before implementation. The purpose of this section is to preserve sequencing and research questions, not to authorize scope expansion.

## Track P1 — Sanitized production workload capture

**Problem to validate:** users may want representative fixtures without manually curating manifests, but production access and sensitive data materially change the trust model.

Required research gate before design:
- which object kinds users actually need captured;
- whether GitOps/manifests already provide sufficient fixture sources;
- secret/config redaction expectations;
- namespace/RBAC read-only permissions;
- how to strip status, UIDs, ownerReferences, live replica counts, and environment-specific identities without masking admission-relevant content;
- whether capture should run as CLI read-only against an explicitly selected kubecontext;
- whether captured data can be made safe enough to commit to git.

No implementation until this is separately approved.

## Track P2 — Generated edge-case fixture packs

**Problem to validate:** fixture corpus misses interactions such as pre-existing sidecars/init containers, projected volumes, hostNetwork, unusual security contexts, Job/CronJob/StatefulSet.

Research/design gate:
- mutation operators must be deterministic and bounded;
- generated cases need stable IDs and reproducible seed;
- generator must not become a general Kubernetes fuzzer;
- reports must identify the parent fixture + transformation;
- measure whether generated cases catch regressions real users missed.

## Track P3 — Kustomize installation backend

**Problem to validate:** enough users require Kustomize input that `kubectl apply -k`/build support is worth the dependency surface.

Possible implementation direction after approval:
- prefer external `kustomize build` or `kubectl kustomize` with pinned provenance;
- feed rendered manifests into the existing raw-manifest installer;
- do not create a second readiness/install engine.

## Track P4 — Additional Gateway API kinds

Candidates:

```text
GRPCRoute
BackendTLSPolicy
TCPRoute
TLSRoute
UDPRoute
service-mesh Gateway API cases
```

Each kind requires its own deterministic data-plane contract before being advertised. Do not add CRD parsing merely to claim support.

## Track P5 — Additional certified recipes

Candidate order from the approved product spec:

```text
Envoy Gateway
Kong
Traefik
Cilium Gateway API
Vault Agent Injector
OpenTelemetry Operator
cert-manager interaction suites
```

Rule: a new recipe is accepted only with:
- pinned install metadata;
- readiness checks;
- capability declaration;
- deterministic regression fixture(s);
- scheduled certification job;
- maintainer commitment or explicit best-effort label.

Recipes still may not contain semantic severity logic.

## Track P6 — Optional `admissionlab serve`

This is the most scope-sensitive post-v1 candidate and must receive a separate architecture spec.

Questions to answer before any code:
- Is local static HTML + CI artifact history insufficient?
- Who needs scheduled runs and why cannot existing CI schedule them?
- Is SQLite enough or is PostgreSQL genuinely necessary?
- Is there a need for distributed workers?
- Authentication model?
- Threat model for stored raw artifacts?
- Can server consume the exact stable v1 result/run contracts without forking execution semantics?

Hard guardrail: Admission Lab CLI remains complete even if `serve` is never built.

## Track P7 — Advisory fast/static mode

Only pursue if measurements show real-cluster feedback is too slow for editing loops.

Requirements before approval:
- output must be labeled advisory/non-authoritative;
- must not claim webhook ordering/reinvocation fidelity it cannot provide;
- real `kind` mode remains CI/release truth;
- demonstrate material iteration-time benefit.

---

# Agent Parallelization Map

Use this map only after the phase dependency has landed.

## Phase 0

Sequential: 0.1 -> 0.2 -> 0.3. After 0.2, 0.4 and 0.5 can run in parallel. 0.6 after crate names stabilize.

## Phase 1

After 1.3 process runner:
- [PARALLEL] 1.1/1.2 spec work;
- [PARALLEL] 1.4 doctor/tool discovery;
- [PARALLEL] 1.5 artifact store;
- [PARALLEL] 1.6 kind audit config.

Then 1.7 -> 1.9 -> 1.10. 1.8 version matrix can run parallel with 1.7.

## Phase 2

2.1 first. Then:
- [PARALLEL] 2.2 Helm installer;
- [PARALLEL] 2.3 raw manifests;
- [PARALLEL] 2.4 readiness;
- [PARALLEL] 2.5 recipe model.

2.6 joins them. 2.7 dogfood recipe follows 2.6. 2.8 Kyverno and 2.9 Istio may proceed in parallel after generic recipe/install flow works.

## Phase 3

3.1 + 3.2 fixture work may run parallel with 3.3 model. 3.4 depends on both. 3.5/3.6 audit parsing and 3.8 metrics can run parallel. 3.7 joins 3.5/3.6. 3.9 dogfood behaviors can run parallel with parsers. 3.10 joins all.

## Phase 4

After domain types exist:
- [PARALLEL] 4.1 object normalization and 4.2 trace normalization;
- [PARALLEL] 4.10 redaction/report model can begin once semantic model shape is agreed.

4.3 -> 4.4/4.5/4.6 (parallel) -> 4.7. Policy 4.8/4.9 follows semantic type stability. Reports 4.11/4.12/4.13 can run in parallel after 4.10. 4.14 joins all. 4.15/4.16 last.

## Phase 5

5.1 -> 5.2 -> 5.3. 5.4 GitHub action and 5.5 summary can run parallel. 5.6 cache and 5.7 performance can run parallel. 5.8 is a measured decision after 5.7. 5.9 after all.

## Phase 6

6.1 first. Then:
- [PARALLEL] 6.2 apply;
- [PARALLEL] 6.3 conditions;
- [PARALLEL] 6.5 echo backend;
- [PARALLEL] 6.6 endpoint model.

6.3 -> 6.4. 6.6 -> 6.7 -> 6.8. 6.9 after model/result evidence. 6.10 recipe can progress with 6.5-6.8. 6.11 joins; 6.12 last.

## Phase 7

7.1/7.2/7.3 are parallel only after public Beta field inventory is frozen in a single review. 7.4 -> 7.5. 7.6 Action UX and 7.7 docs may run parallel after schemas land.

## Phase 8

8.1 NGINX Gateway recipe and 8.2 legacy recipe can run parallel. 8.3 -> 8.4/8.5. 8.6 TLS can run parallel with migration work. 8.7 joins Gateway implementations + TLS. 8.8 migration demo after 8.4/8.5. 8.9 last.

## Phase 9

9.1/9.2 stable contracts first. Then security 9.3/9.4, diagnostics 9.5/9.6, compatibility/perf 9.7/9.8, and packaging/supply chain 9.9/9.10 may run as parallel review lanes. 9.11 after public behavior stabilizes.

## Phase 10

Strictly sequential release process. Do not parallelize version/tag/publish state transitions.

---

# Spec Coverage Matrix

| Product requirement | Roadmap implementation |
|---|---|
| local-first/open source/Apache-2.0 | 0.1, 9.11, 10 |
| strict versioned config | 1.1-1.2, 7.1, 9.1 |
| real baseline/candidate kind clusters | 1.6-1.10 |
| safe external tools | 1.3-1.4, 9.4 |
| Helm + raw manifests | 2.1-2.3 |
| deterministic readiness | 2.4, 2.6 |
| vendor-neutral recipes | 2.5, 2.8-2.9 |
| static user fixtures | 3.1-3.4 |
| parameterized fixture matrices | 5.10 |
| allow/deny/final mutation | 3.4, 3.10 |
| audit webhook trace/patch | 3.5-3.7 |
| latency where observable | 3.8, 4.6 |
| dogfood webhook | 2.7, 3.9 |
| normalization | 4.1-4.2 |
| semantic admission diff | 4.3-4.6 |
| first divergence | 4.7 |
| severity/policy | 4.8 |
| explicit/stale expectations | 4.9 |
| terminal/JSON/HTML | 4.10-4.13 |
| canonical Alpha workflow | 4.14-4.16 |
| run provenance/reproduce | 5.1-5.3 |
| GitHub Action | 5.4-5.5, 7.6 |
| performance/reliability | 5.7-5.9, 9.8 |
| Gateway admission/reconcile/traffic | 6.1-6.11 |
| GatewayClass/Gateway/HTTPRoute/ReferenceGrant | 6.1-6.4, 6.10 |
| Istio Gateway API | 6.10-6.12 |
| versioned Beta schemas | 7.1-7.3 |
| latest 3 K8s minors | 1.8, 7.4-7.5, 9.7 |
| NGINX Gateway Fabric | 8.1, 8.7, 8.9 |
| legacy ingress-nginx | 8.2, 8.4 |
| Ingress -> Gateway behavior migration | 8.3-8.5, 8.8 |
| TLS/header/redirect/rewrite/weighted contracts | 8.6-8.7 |
| security/redaction | 1.6, 4.10, 8.6, 9.3-9.4 |
| diagnostics/doctor | 1.4, 1.9, 9.5 |
| stable v1 contracts | 9.1-9.2 |
| packaging/supply chain | 9.9-9.10 |
| no mandatory server | all v1 phases; P6 separately gated |

---

# Non-Goals Agents Must Not Pull Into v1

If an implementation task appears easier by introducing one of the following, stop and find a solution within the approved boundaries instead:

```text
hosted SaaS
accounts/billing
central control plane
generic production agent
production secret capture
LLM/AI root-cause classification
policy editor
Argo CD/GitOps controller
Terraform/cloud drift
full service-mesh debugger
full API management/WAF/OAuth platform
generic Kubernetes dashboard
VS Code extension
Slack bot
controller replay framework
chaos platform
generic fuzzing platform
```

---

# Final Execution Handoff

This `ROADMAP.md` is the canonical implementation plan requested for Admission Lab. An execution agent must read both `PRODUCT.md` and this file before beginning.

Recommended execution mode:

```text
superpowers:subagent-driven-development
```

Use a fresh implementation subagent per task and review task output before moving to the next dependency. For long-running sequential execution in one session, use `superpowers:executing-plans` with phase checkpoints.

Do not begin Post-v1 tracks without a new approved design/spec.

# Authoritative Technical References for Implementers

Use upstream documentation as the source of truth when Kubernetes/kind/Gateway behavior changes. Do not copy blog behavior into code without an integration test.

1. Kubernetes dynamic admission control and mutating-webhook audit annotations: `https://kubernetes.io/docs/reference/access-authn-authz/extensible-admission-controllers/`
   - invocation annotation: `mutation.webhook.admission.k8s.io/round_<round>_index_<index>` at Metadata audit level or higher;
   - patch annotation: `patch.webhook.admission.k8s.io/round_<round>_index_<index>` at Request audit level or higher;
   - reinvocation ordering/count is not guaranteed.
2. kind auditing configuration: `https://kind.sigs.k8s.io/docs/user/auditing/`
   - use kubeadm config patches plus mounted audit policy/log directories.
3. Kubernetes API server metrics reference: `https://kubernetes.io/docs/reference/instrumentation/metrics/`
   - `apiserver_admission_webhook_admission_duration_seconds` is the stable webhook latency histogram used for optional metric deltas.
4. Kubernetes audit API/config: `https://kubernetes.io/docs/tasks/debug/debug-cluster/audit/`
   - `Request` includes request body; `RequestResponse` also includes response body; Admission Lab deliberately obtains final response objects from the API client rather than enabling broad response-body audit logging.
5. Gateway API HTTPRoute status: `https://gateway-api.sigs.k8s.io/reference/api-types/httproute/`
6. Gateway API troubleshooting/status conditions: `https://gateway-api.sigs.k8s.io/docs/concepts/troubleshooting/`
   - interpret `Accepted`, `ResolvedRefs`, and `Programmed` separately;
   - use `observedGeneration` to reject stale status.
7. Gateway API conformance: `https://gateway-api.sigs.k8s.io/concepts/conformance/`
   - Admission Lab tests user-stack baseline-vs-candidate behavior; it does not replace upstream implementation conformance.
8. Kubernetes release/support window: `https://kubernetes.io/releases/`
   - the project maintains the latest three minor release branches; re-check immediately before each stable Admission Lab release.
9. Rust stable release announcements: `https://blog.rust-lang.org/releases/`
   - this roadmap was authored with Rust 1.98.0 as the pinned bootstrap toolchain.
