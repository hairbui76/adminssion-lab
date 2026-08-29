# Admission Lab — Product Specification

**Status:** Approved product design baseline  
**License:** Apache-2.0  
**Primary audience:** Platform and SRE teams operating Kubernetes in production  
**Implementation direction:** Rust-first, local-first, deterministic, real-cluster authoritative  
**Initial delivery model:** Fully free and open source; no hosted SaaS dependency

## 1. Product Summary

Admission Lab is an open-source compatibility regression testing lab for Kubernetes control-plane extensions.

It answers one operational question:

> If we change our admission stack or Gateway stack, how will the behavior of our real workloads change before that change reaches production?

Admission Lab creates isolated baseline and candidate Kubernetes environments, installs the requested control-plane components, replays the same workload corpus through both environments, captures the observable behavior produced by the Kubernetes API server and relevant controllers, normalizes nondeterministic data, computes semantic behavioral differences, attributes the first meaningful divergence where possible, and produces a deterministic pass/warn/fail result suitable for local use and CI.

The product is intentionally not a Kubernetes management platform, policy authoring environment, observability product, GitOps controller, API management suite, or AI SRE agent.

## 2. Product Thesis

Kubernetes platform changes are routinely validated one component at a time, while production behavior emerges from interactions between components.

A Platform/SRE team may independently validate:

- a Kyverno upgrade;
- an Istio upgrade;
- a custom admission webhook;
- an NGINX Gateway Fabric upgrade;
- a Gateway API migration;
- a change to a validating or mutating policy.

Each component can be correct in isolation while the combined stack changes the behavior of a workload. Examples include:

- a workload newly being rejected;
- an injected init container disappearing;
- a sidecar being added twice;
- an admission webhook no longer being reinvoked after a later mutation;
- a security context becoming weaker or stricter;
- a Gateway route remaining syntactically valid but no longer attaching to a listener;
- a backend reference no longer resolving;
- traffic reaching a different backend after a Gateway implementation upgrade;
- admission latency increasing beyond an acceptable limit.

The product hypothesis is that platform teams need a reproducible equivalent of regression testing for these behaviors, not another raw YAML diff.

## 3. Positioning

### 3.1 Primary positioning

> **Regression testing for your Kubernetes control-plane extensions.**

### 3.2 Secondary positioning

> **Know what breaks before upgrading your admission and Gateway stack.**

### 3.3 What makes Admission Lab distinct

Admission Lab combines four ideas in one workflow:

1. **Real baseline environment versus real candidate environment.**
2. **The same workload corpus replayed through both.**
3. **Semantic behavioral diff instead of only object diff.**
4. **First-divergence attribution and CI gating.**

The core comparison is therefore:

```text
CURRENT STACK                       CANDIDATE STACK
     |                                    |
     +----------- same fixtures ----------+
                       |
                       v
             observed behavior
                       |
                       v
             semantic differences
                       |
                       v
      expected / informational / regression
```

## 4. Target User

### 4.1 Primary persona

The primary user is a Platform Engineer or SRE responsible for production Kubernetes clusters.

Typical responsibilities include:

- maintaining admission controllers and policies;
- maintaining service mesh and ingress/Gateway infrastructure;
- approving platform upgrades;
- creating cluster upgrade playbooks;
- managing shared cluster add-ons;
- maintaining golden paths and platform standards;
- preventing platform changes from breaking application teams.

### 4.2 Primary jobs to be done

The product must help the user answer:

- Can we safely upgrade Kyverno?
- Can we safely upgrade Istio?
- Can we safely upgrade NGINX Gateway Fabric?
- What behavior changes if we upgrade multiple admission components together?
- Did a policy change start rejecting workloads that were previously admitted?
- Did a mutation change remove or alter a container, init container, volume, environment variable, service account, security context, or resource requirement?
- Which webhook or reconciliation stage first caused the baseline and candidate to diverge?
- Will our Gateway API resources still attach, resolve, become programmed, and route traffic as expected?
- Does an Ingress-to-Gateway migration preserve relevant request-routing behavior?
- Should a CI job block this platform change?

### 4.3 Secondary users

Secondary users may include:

- maintainers of custom admission webhooks;
- maintainers of Kubernetes operators that register admission webhooks;
- platform vendors that want compatibility fixture suites;
- security platform teams validating policy changes.

Admission Lab should remain usable by these groups, but product decisions for v1 are made for Platform/SRE teams first.

## 5. Core Product Principles

Admission Lab must remain:

1. **Local-first.** A user must be able to run the important workflows without an Admission Lab server or account.
2. **Fully open source.** The project is Apache-2.0 and must not gate essential functionality behind a proprietary service.
3. **Deterministic by default.** A result must be reproducible from recorded inputs except for explicitly identified external nondeterminism.
4. **Real-cluster authoritative.** The authoritative result comes from a real Kubernetes API server and real component installation, not an in-process simulation.
5. **Vendor-neutral at the core.** Vendor-specific recipes simplify installation and normalization but must not own the regression engine.
6. **Behavioral rather than textual.** The core question is whether behavior changed, not whether YAML changed.
7. **CI-friendly.** Exit codes, JSON results, artifacts, and failure modes must be predictable.
8. **Safe by default.** Admission Lab must not require production secrets or production write access for the default test flow.
9. **Explainable without AI.** Classification and pass/fail decisions must be deterministic. AI is not required for v1 and is explicitly out of scope.
10. **YAGNI-driven.** Every core feature must answer: which meaningful regression does this enable us to catch that we could not catch before?

## 6. Product Scope

### 6.1 v1 domains

Admission Lab v1 has two related domains.

#### Domain A — Admission regression

The engine must compare:

- admitted versus rejected;
- final mutated object;
- webhook invocation information available through Kubernetes audit data;
- mutation patches when available;
- webhook ordering and rounds where reconstructable;
- reinvocation effects;
- webhook failures;
- total and per-webhook admission latency where observable;
- warnings and errors relevant to admission.

#### Domain B — Gateway behavior regression

The Gateway engine must compare, initially for Kubernetes Gateway API:

- `GatewayClass` acceptance;
- `Gateway` acceptance/readiness-relevant conditions;
- `HTTPRoute` acceptance;
- `ReferenceGrant` effects;
- `Accepted` condition;
- `ResolvedRefs` condition;
- `Programmed` condition;
- route-to-parent attachment;
- backend resolution;
- observed generation convergence;
- basic HTTP traffic status;
- expected backend identity.

### 6.2 Explicit v1 non-goals

The following are not part of v1:

- generic Kubernetes observability;
- GitOps reconciliation management;
- policy authoring or visual policy editing;
- secret management;
- production agent deployment;
- multi-cluster production monitoring;
- API management features such as developer portals, billing, API keys, WAF configuration, or OAuth management;
- general service-mesh traffic debugging;
- generic controller behavioral replay;
- Terraform or cloud infrastructure testing;
- hosted SaaS;
- user accounts, billing, or proprietary cloud services;
- LLM explanation or AI-generated pass/fail decisions;
- a full chaos-testing platform;
- a generic Kubernetes dashboard;
- a VS Code extension;
- a Slack bot.

## 7. Release Boundaries

### 7.1 Public Alpha

Public Alpha proves the admission-regression thesis.

It includes:

- baseline and candidate `kind` clusters;
- Helm and raw-manifest installation;
- user-provided fixtures;
- admission allow/deny capture;
- final-object capture;
- audit-based webhook tracing where supported;
- normalization;
- semantic admission diff;
- severity and regression policy;
- explicit expected changes;
- first-divergence attribution where the captured trace allows it;
- terminal, JSON, and static HTML reports;
- certified Kyverno and Istio recipes;
- a deterministic test webhook maintained by Admission Lab.

Gateway behavior is explicitly outside Alpha.

### 7.2 Public Beta

Public Beta adds Gateway behavior and CI integration.

It includes:

- GitHub Action integration;
- reproducible run manifest;
- stable beta result schema;
- Istio Gateway API recipe;
- `GatewayClass`, `Gateway`, `HTTPRoute`, and `ReferenceGrant` fixtures;
- reconciliation condition comparison;
- basic HTTP echo backend;
- traffic probes;
- route/backend behavioral regression classification.

### 7.3 v1.0

v1.0 hardens the product and adds the second Gateway implementation.

It includes:

- stable CLI contracts for documented core commands;
- stable config schema;
- stable JSON result schema;
- compatibility with the latest three Kubernetes minor releases still supported upstream;
- certified Kyverno recipe;
- certified Istio admission recipe;
- certified Istio Gateway API recipe;
- certified NGINX Gateway Fabric recipe;
- legacy community `ingress-nginx` compatibility recipe where applicable;
- Ingress-to-Gateway behavior migration suite;
- GitHub Action;
- terminal, JSON, and HTML reporting;
- security and diagnostics hardening;
- documented schema migration policy.

## 8. Primary User Workflow

The primary command is:

```bash
admissionlab test admissionlab.yaml
```

A typical repository layout is:

```text
admission-lab/
├── admissionlab.yaml
├── expectations.yaml
├── fixtures/
│   ├── workloads/
│   └── gateway/
├── values/
└── local-recipes/
```

The top-level flow is:

```text
                 admissionlab test
                        |
           +------------+------------+
           |                         |
           v                         v
 baseline kind cluster       candidate kind cluster
           |                         |
 install baseline stack      install candidate stack
           |                         |
 verify readiness            verify readiness
           |                         |
           +------------+------------+
                        |
                 same fixtures
                        |
             real API server replay
                        |
             +----------+----------+
             |                     |
             v                     v
      baseline result        candidate result
             |                     |
             +----------+----------+
                        |
                   normalize
                        |
                  semantic diff
                        |
                  policy engine
                        |
             PASS / WARN / FAIL
```

## 9. Configuration Model

The configuration format must be declarative, deterministic, and versioned.

Illustrative shape:

```yaml
apiVersion: admissionlab.io/v1beta1
kind: Lab

baseline:
  kubernetes: "<supported-version>"
  components:
    - recipe: kyverno
      version: "<baseline-version>"
    - recipe: istio
      version: "<baseline-version>"

candidate:
  kubernetes: "<supported-version>"
  components:
    - recipe: kyverno
      version: "<candidate-version>"
    - recipe: istio
      version: "<candidate-version>"

fixtures:
  include:
    - fixtures/**/*.yaml

policy:
  failOn:
    - newly_denied
    - removed_container
    - removed_init_container
    - removed_volume
    - webhook_failure
    - security_regression

expectationsFile: expectations.yaml
```

The exact schema names are implementation details to be finalized before the first schema freeze. The requirements are:

- explicit schema version;
- no silent acceptance of unknown critical fields;
- deterministic component order where order matters;
- explicit baseline and candidate;
- no implicit access to production clusters;
- paths resolved relative to the configuration file;
- human-readable validation failures;
- JSON Schema published for editor/tooling support.

## 10. Execution Model

### 10.1 Real Kubernetes API server

The authoritative mode uses real ephemeral Kubernetes clusters created with `kind`.

Simulation is not authoritative because admission behavior can depend on:

- API server defaults;
- admission ordering;
- selectors;
- match conditions;
- reinvocation;
- failure policy;
- timeout behavior;
- JSON patches;
- version conversion;
- Kubernetes release behavior.

A future fast/static mode may exist, but it must be labeled as advisory and cannot replace real-cluster CI checks.

### 10.2 Baseline and candidate isolation

Each test run uses separate clusters by default:

```text
adlab-baseline-<run-id>
adlab-candidate-<run-id>
```

The clusters must not share mutable cluster state.

### 10.3 Workspace

Each run receives a private workspace under the configured cache/run root containing:

- resolved config;
- component installation metadata;
- kubeconfigs;
- raw fixture hashes;
- raw captured artifacts;
- normalized results;
- diff results;
- reports;
- diagnostic logs;
- run manifest.

Secrets captured inadvertently must be redacted before report generation and must not be copied into long-lived artifacts when avoidable.

### 10.4 Cleanup

Clusters are deleted on success and failure by default.

A debugging option may preserve them:

```bash
admissionlab test --keep-clusters admissionlab.yaml
```

Preserved-cluster mode must clearly print cluster names, kubeconfig locations, and cleanup commands.

### 10.5 Admission fixture execution semantics

The default v1 admission-fixture path uses Kubernetes server-side dry-run CREATE requests against the real API server. This invokes the real admission chain while avoiding persistence/controller side effects and returns the final admitted/mutated object for comparison.

Requirements:

- a fixture that cannot be evaluated because a webhook declares dry-run-unsafe side effects is reported explicitly as unsupported/inconclusive for this execution mode; Admission Lab must not silently switch to different semantics;
- Alpha executes fixture requests serially within each cluster so audit-log correlation remains deterministic; baseline and candidate clusters may process the corresponding fixture concurrently because they are isolated;
- future bounded parallel fixture execution requires a proven request-correlation mechanism and must not weaken evidence quality;
- Gateway behavior fixtures are different: Gateway resources must be persisted inside the disposable cluster because reconciliation and data-plane programming require durable objects.

### 10.6 Audit evidence semantics

Admission Lab configures the ephemeral API server audit log to capture the request-level evidence needed for mutating-webhook invocation and patch annotations. Secret request bodies must not be logged at Request/RequestResponse level.

Trace reporting distinguishes observed, partial, and unavailable evidence. Kubernetes mutating-webhook audit annotations may support reconstruction of invocation rounds and patches; validating-webhook allow invocations must not be invented when equivalent evidence is unavailable.

Per-webhook latency is optional evidence. When collected, it may be derived from isolated kube-apiserver admission webhook metric deltas around serial fixture requests; ambiguous or missing latency data remains unknown rather than becoming zero or a failure by itself.

## 11. Rust Architecture

Admission Lab is Rust-first.

Initial workspace boundaries:

```text
crates/
├── admissionlab-cli/
├── admissionlab-core/
├── admissionlab-spec/
├── admissionlab-cluster/
├── admissionlab-installer/
├── admissionlab-fixtures/
├── admissionlab-admission/
├── admissionlab-gateway/
├── admissionlab-normalize/
├── admissionlab-diff/
├── admissionlab-policy/
├── admissionlab-report/
└── admissionlab-recipes/
```

Responsibilities:

- `admissionlab-cli`: user commands, argument parsing, top-level exit-code mapping.
- `admissionlab-core`: run orchestration interfaces and shared domain types that genuinely span modules.
- `admissionlab-spec`: versioned configuration parsing, validation, JSON Schema generation, path resolution.
- `admissionlab-cluster`: `kind` lifecycle, kubeconfig handling, health checks, diagnostics.
- `admissionlab-installer`: Helm/raw-manifest/Kustomize installation abstractions and readiness checks.
- `admissionlab-fixtures`: fixture discovery, identity, hashing, parameterization, apply/delete lifecycle.
- `admissionlab-admission`: admission execution, outcome capture, audit correlation, webhook trace reconstruction.
- `admissionlab-gateway`: Gateway reconciliation observation and traffic probes.
- `admissionlab-normalize`: deterministic Kubernetes-object and trace normalization.
- `admissionlab-diff`: raw and semantic behavioral difference generation.
- `admissionlab-policy`: severity mapping, expected changes, pass/warn/fail decision.
- `admissionlab-report`: terminal, JSON, static HTML, and CI-oriented summaries.
- `admissionlab-recipes`: recipe loading, validation, capability metadata, and built-in recipe catalog.

No generic `engine` crate should accumulate unrelated responsibilities.

## 12. External Tool Strategy

v1 intentionally delegates established functionality instead of reimplementing it.

External binaries:

- `kind` for local Kubernetes cluster creation;
- `kubectl` for carefully bounded operations when a Rust Kubernetes client is not the better abstraction;
- `helm` for Helm installs and upgrades.

Kubernetes API reads and structured interactions should use the Rust Kubernetes ecosystem where doing so improves correctness and typed data handling.

External commands must:

- be invoked using argv rather than shell interpolation;
- capture stdout/stderr separately;
- have explicit timeouts;
- produce structured diagnostic context;
- report discovered tool versions in `admissionlab doctor` and run manifests.

## 13. Installation Abstraction

The generic installer supports, in planned order:

### Alpha

1. Helm.
2. Raw Kubernetes manifests.

### Beta or later

3. Kustomize.

A component declaration must be able to express:

- source/install method;
- version or digest when available;
- values/overrides;
- namespace;
- readiness conditions;
- install timeout;
- capability metadata;
- recipe-provided normalization rules.

A recipe is convenience and certification metadata above this generic model, not a separate execution engine.

## 14. Recipe Model

Recipes must not contain regression-classification business logic.

A recipe may contain:

- installation defaults;
- supported versions/ranges;
- readiness checks;
- known harmless normalization rules;
- optional fixture packs;
- capability metadata;
- diagnostic hints;
- compatibility-test metadata.

Initial certification plan:

### Alpha

- Kyverno.
- Istio admission behavior.

### Beta

- Istio Gateway API.

### v1.0

- NGINX Gateway Fabric.
- Community `ingress-nginx` legacy/migration compatibility where feasible.

Potential post-v1 recipes include:

- Envoy Gateway;
- Kong;
- Traefik;
- Cilium Gateway API;
- Vault Agent Injector;
- OpenTelemetry Operator;
- cert-manager interaction suites.

## 15. Fixture System

### 15.1 Static fixtures

A standard Kubernetes manifest is the initial fixture unit.

### 15.2 Parameterized fixtures

The framework may expand a declarative matrix into multiple deterministic fixture cases. Parameterization must be explicit and produce stable fixture IDs.

### 15.3 Generated edge cases

Automated fixture generation is post-v1 unless needed earlier to reproduce a critical known regression.

Potential generated cases include:

- pre-existing init container;
- pre-existing sidecar;
- projected volume;
- host networking;
- custom service account;
- multiple injection annotations;
- security-context variants;
- Job;
- CronJob;
- StatefulSet.

### 15.4 Production workload capture

Automatic production capture is post-v1. The default v1 flow must not require production cluster access.

## 16. Admission Capture Model

For every admission fixture, the engine should record a result with the following conceptual model:

```text
FixtureResult
├── fixture_identity
├── admission_outcome
│   ├── accepted
│   ├── rejection
│   ├── warnings
│   └── total_latency
├── final_object
├── webhook_trace[]
│   ├── webhook_identity
│   ├── round
│   ├── latency
│   ├── mutation_patch
│   ├── outcome
│   └── correlation_metadata
└── diagnostics[]
```

Exact fields may vary based on information Kubernetes exposes for a given release and admission type.

### 16.1 Audit-based tracing

Where supported, Admission Lab should use Kubernetes audit events and admission-controller audit annotations to reconstruct webhook behavior without requiring vendor-specific instrumentation.

The implementation must explicitly distinguish:

- observed fact;
- inferred ordering/correlation;
- unavailable information.

A missing trace field must never be fabricated.

### 16.2 Fixture correlation

Concurrent fixtures must not cross-correlate audit events.

The engine should attach a unique, temporary correlation marker where safe, then ensure it does not appear as a semantic regression. The raw artifact may preserve it for diagnostic correlation if it cannot safely be removed earlier.

## 17. First-Divergence Attribution

Final object diff is insufficient for the intended user experience.

When trace information allows it, Admission Lab should identify the earliest stage at which baseline and candidate behavior differ.

Example:

```text
Deployment/payments-api

BASELINE TRACE
Kyverno -> Vault Injector -> Istio -> final object

CANDIDATE TRACE
Kyverno -> Vault Injector -> Istio -> final object

FIRST DIVERGENCE
istio-sidecar-injector

Observed candidate patch:
  remove /spec/initContainers

Semantic effect:
  removed initContainer/vault-agent-init
```

If the first divergence cannot be proven, the report must say so and fall back to the strongest observable statement rather than guessing.

## 18. Normalization

Raw Kubernetes objects contain expected nondeterminism and generated state.

Built-in normalization should address fields such as:

- `metadata.uid`;
- `metadata.resourceVersion`;
- `metadata.creationTimestamp`;
- `metadata.managedFields`;
- selected server-generated fields;
- irrelevant status for admission-only tests;
- semantically irrelevant ordering;
- known generated checksums where justified.

Normalization hierarchy:

```text
built-in normalization
        -> recipe normalization
        -> user normalization
```

User ignores must be explicit. The product should warn when normalization is so broad that it risks masking meaningful behavior.

Every report should record the normalization rules that materially affected comparison.

## 19. Semantic Diff

Admission Lab stores raw object differences for diagnostics but presents semantic changes as the primary model.

Initial admission change categories include:

- `ObjectNewlyDenied`;
- `ObjectNewlyAllowed`;
- `ContainerAdded`;
- `ContainerRemoved`;
- `InitContainerAdded`;
- `InitContainerRemoved`;
- `VolumeAdded`;
- `VolumeRemoved`;
- `VolumeMountChanged`;
- `EnvironmentChanged`;
- `ImageChanged`;
- `ServiceAccountChanged`;
- `SecurityContextChanged`;
- `ResourceRequirementChanged`;
- `WebhookFailed`;
- `WebhookInvocationChanged`;
- `WebhookLatencyChanged`.

Initial Gateway change categories include:

- `RouteAttached`;
- `RouteDetached`;
- `BackendResolutionChanged`;
- `ListenerBindingChanged`;
- `AcceptedConditionChanged`;
- `ResolvedRefsConditionChanged`;
- `ProgrammedConditionChanged`;
- `TrafficStatusChanged`;
- `TrafficBackendChanged`.

Semantic categories must be versioned as part of the machine-readable result contract once stabilized.

## 20. Severity and Regression Policy

Default severity exists to make the tool immediately useful but must be configurable.

### Critical defaults

Examples:

- previously accepted workload becomes rejected;
- application container removed;
- init container removed;
- required volume removed;
- unexpected service-account change;
- security posture weakened;
- webhook failure introduced;
- previously programmed route becomes unprogrammed;
- HTTP route stops reaching an expected backend.

### Warning defaults

Examples:

- resource requirements changed;
- environment configuration changed;
- behavior-relevant annotation changed;
- meaningful admission-latency regression;
- webhook invocation/order changed without final critical effect;
- new mutation introduced.

### Informational defaults

Examples:

- expected component image update;
- non-behavioral metadata;
- known generated version annotation;
- semantically irrelevant ordering.

The policy file may override severity by semantic change type and scoped selectors.

## 21. Expected Changes

Admission Lab must support explicit, reviewable expected changes rather than a single blanket snapshot approval.

An expectation contains at least:

- fixture scope;
- semantic change type;
- optional object/path/component selector;
- human reason.

Example shape:

```yaml
expectedChanges:
  - fixtures: "*"
    type: image_changed
    selector:
      container: istio-proxy
    reason: Planned Istio upgrade
```

Expected-change matching must be deterministic.

If an expectation no longer matches any observed change, the report should surface it as a stale expectation so ignore rules do not accumulate forever.

## 22. Gateway Behavior Engine

Gateway testing has three layers.

### 22.1 Admission layer

The normal admission engine handles:

- schema validation;
- CEL validation effects exposed by the API server;
- admission webhook allow/deny;
- mutation.

### 22.2 Reconciliation layer

The Gateway engine waits for controller reconciliation and observes normalized conditions and relationships, including:

- `GatewayClass` acceptance;
- `Gateway` relevant conditions;
- `HTTPRoute` parent status;
- `Accepted`;
- `ResolvedRefs`;
- `Programmed`;
- attachment to expected Gateway/listener;
- backend reference resolution;
- `observedGeneration` convergence.

Timeout must produce an explicit inconclusive/setup or regression result according to the observed state, not an arbitrary pass.

### 22.3 Data-plane layer

Admission Lab deploys deterministic echo backends and performs HTTP probes through the candidate and baseline Gateways.

An initial traffic contract includes:

- host;
- path;
- method;
- optional request headers;
- expected status;
- expected backend identity.

v1 may add portable Gateway API behaviors such as header modification, redirect/rewrite, weighted backend behavior, TLS termination, and portable timeouts after basic routing is stable.

## 23. Gateway Scope by Release

### Beta

- `GatewayClass`;
- `Gateway`;
- `HTTPRoute`;
- `ReferenceGrant`;
- HTTP traffic only;
- Istio Gateway API as reference implementation.

### v1

- NGINX Gateway Fabric;
- portable TLS termination tests;
- header modification;
- redirects/rewrites where portable and deterministic;
- weighted backend checks with statistically defensible bounds or deterministic controller-config inspection where traffic sampling would be flaky;
- portable timeout behavior where feasible.

### Post-v1

- `GRPCRoute`;
- `BackendTLSPolicy`;
- `TCPRoute`;
- `TLSRoute`;
- `UDPRoute`;
- service-mesh Gateway API use cases;
- implementation-specific extensions when maintained as explicit recipe capabilities.

## 24. Ingress-to-Gateway Migration Suite

The migration suite tests behavior preservation, not only syntax conversion.

Potential comparisons include:

- host matching;
- path matching;
- TLS behavior;
- backend selection;
- rewrite behavior;
- redirects;
- canary or implementation-specific behavior with no portable Gateway equivalent.

The product must identify unsupported or non-portable behavior rather than pretending a perfect conversion exists.

## 25. Reports

Three report formats are required.

### 25.1 Terminal

Optimized for interactive local runs and CI logs.

Minimum summary:

```text
245 fixtures tested
232 identical
10 expected
2 warnings
1 critical regression

CRITICAL
Deployment/payments-api
  removed initContainer: vault-agent-init
  first divergence: istio-sidecar-injector

Upgrade safety: FAIL
```

### 25.2 JSON

Machine-readable result for CI and integrations.

The beta schema must be versioned. v1 schema changes follow documented compatibility/migration rules.

### 25.3 Static HTML

A self-contained or artifact-friendly report containing:

- run summary;
- environment/component matrix;
- regression counts;
- fixture drill-down;
- semantic differences;
- first-divergence information;
- webhook trace;
- Gateway reconciliation/traffic results;
- diagnostics.

No server is required to view the report.

## 26. GitHub Integration

The GitHub Action is a thin wrapper around the same CLI and engine.

It must not contain a second implementation of regression logic.

A typical workflow should:

1. install a pinned Admission Lab release;
2. validate prerequisites;
3. run the lab;
4. expose a concise job summary;
5. upload JSON/HTML/run-manifest artifacts;
6. fail the job only according to Admission Lab's documented exit codes and policy result.

A server-side GitHub App is explicitly unnecessary for v1.

## 27. Error and Exit-Code Model

Admission Lab must distinguish product regressions from lab failures.

Conceptual exit codes:

- `0`: completed, policy passed;
- `1`: completed, regression policy failed;
- `2`: invalid user configuration or invalid fixture definition;
- `3`: lab infrastructure failure such as `kind` failure;
- `4`: component installation or readiness failure;
- `5`: fixture execution/capture failure that prevents a valid comparison;
- `6`: internal Admission Lab error.

Exact codes may be expanded only before v1 contract freeze.

Reports must preserve partial diagnostics even when a run cannot produce a valid behavioral comparison.

## 28. Reproducibility

Every completed run should produce a versioned run manifest recording sufficient provenance to reproduce the environment as closely as possible:

- Admission Lab version;
- host metadata relevant to execution;
- Kubernetes versions;
- `kind` node image identifiers/digests;
- external tool versions;
- component versions;
- Helm chart versions/digests when obtainable;
- fixture hashes;
- resolved configuration hash;
- normalization configuration;
- policy configuration;
- expected-change configuration;
- timestamps and run IDs.

A later beta milestone adds:

```bash
admissionlab reproduce <run-manifest>
```

The command must fail clearly when an input artifact or remote version can no longer be resolved.

## 29. Security Model

### 29.1 Default trust boundary

Admission Lab executes third-party Helm charts, manifests, admission webhooks, and controllers in disposable Kubernetes clusters. They must be treated as untrusted test workloads.

### 29.2 Production access

v1 must not require production cluster credentials.

### 29.3 Secrets

Admission Lab must not automatically copy production secrets into the lab.

Reports and logs must redact at least:

- `Secret.data`;
- `Secret.stringData`;
- common credential/token environment-variable values where Admission Lab controls rendering;
- authorization headers in traffic diagnostics;
- private keys;
- values explicitly marked sensitive by configuration.

### 29.4 Process execution

External binaries are executed without shell string interpolation.

### 29.5 Network considerations

A future strict/offline mode is desirable. Before v1, documentation must at minimum state that installed charts/controllers can perform network access from the disposable cluster and explain how users should run Admission Lab in an appropriately isolated CI environment.

## 30. Deterministic Dogfood Admission Webhook

The repository should contain a minimal deterministic admission webhook used only for Admission Lab's own integration/E2E tests.

It should be able to produce controlled behaviors such as:

- allow;
- deny;
- add label;
- add/remove container;
- add/remove init container;
- add/remove volume;
- controlled delay;
- controlled failure;
- behavior dependent on an explicit fixture annotation;
- multi-webhook scenarios needed to test ordering/reinvocation.

This prevents core tests from depending entirely on external vendor behavior.

## 31. Testing Strategy

### 31.1 Unit tests

Required for:

- spec parsing and validation;
- path resolution;
- normalization;
- semantic diff;
- severity mapping;
- expected-change matching;
- policy evaluation;
- report generation;
- exit-code mapping.

### 31.2 Golden tests

Golden fixtures should verify:

```text
raw baseline object + raw candidate object
                 -> normalization
                 -> semantic diff
                 -> expected golden result
```

Golden files must be reviewed carefully and not refreshed blindly.

### 31.3 Integration tests

Use a single `kind` cluster and the deterministic webhook to verify:

- allow;
- deny;
- mutation;
- failure;
- timeout;
- trace correlation;
- ordering/reinvocation cases that Kubernetes exposes.

### 31.4 End-to-end tests

Use two clusters with intentionally different deterministic webhook behavior and assert that Admission Lab catches known regressions.

### 31.5 Certified recipe tests

Scheduled and release CI validates supported recipe combinations for:

- Kyverno;
- Istio admission;
- Istio Gateway API;
- NGINX Gateway Fabric by v1.

## 32. Kubernetes Compatibility Policy

Admission Lab targets the latest three Kubernetes minor versions still supported upstream at the time of each Admission Lab release.

The matrix is tiered to control CI cost.

### Tier 1 — per commit

- primary supported Kubernetes version;
- current certified recipe versions;
- core dogfood test matrix.

### Tier 2 — nightly

- all three supported Kubernetes minors;
- selected recipe-version combinations.

### Tier 3 — weekly/release

- expanded certified compatibility matrix;
- Gateway implementations;
- migration suites;
- slow reliability/repetition tests.

Automation should update compatibility metadata when Kubernetes support windows move, but human review is required before dropping a supported version in a stable Admission Lab release line.

## 33. Performance and Reliability Targets

Targets are engineering goals, not absolute guarantees across every CI provider.

Initial goals:

- typical `kind` cluster creation under approximately 90 seconds per cluster on a healthy CI runner;
- semantic comparison of 100 ordinary fixtures in under one second after artifacts are collected;
- 100-fixture admission suite completes within approximately five minutes excluding component installation under normal CI conditions;
- no leaked cluster after normal failure paths;
- sufficient diagnostics on first failure so users do not need to rerun solely to discover which setup stage failed.

Before v1, repeated-run reliability is more important than optimizing a few seconds of runtime.

## 34. Diagnostics and `doctor`

The command:

```bash
admissionlab doctor
```

must inspect at least:

- host platform support;
- Docker/container runtime reachability required by `kind`;
- `kind` availability/version;
- `kubectl` availability/version;
- `helm` availability/version;
- disk-space warning threshold;
- ability to create a temporary test cluster in an optional deep-check mode.

The doctor command is diagnostic only and must not mutate production contexts.

## 35. Self-Hosted Server Policy

A self-hosted server is post-v1 and optional.

Admission Lab v1 must be complete and useful if no server is ever built.

Potential future server capabilities include:

- run history;
- compatibility matrix storage;
- scheduled test runs;
- static report browsing;
- PostgreSQL or SQLite persistence;
- worker coordination.

These capabilities must use the same core result/config contracts and must not fork the execution semantics.

## 36. Success Metrics

### 36.1 Technical north star

> Percentage of known admission/Gateway behavioral regressions in the maintained regression corpus that Admission Lab catches before deployment.

### 36.2 OSS adoption indicators

Useful secondary indicators include:

- weekly CLI runs when telemetry is available only through opt-in/community reporting, never mandatory tracking;
- GitHub Action adoption;
- repositories containing `admissionlab` configuration;
- external recipe contributions;
- external fixture/regression contributions;
- real-world issue reports describing a regression caught before upgrade.

The most credible social proof is a maintained list of real regressions the project can reproduce and catch.

## 37. Demo Contract

The canonical demo must communicate value in under 30 seconds.

A successful demo shows:

```text
1. Baseline platform stack.
2. Candidate platform upgrade.
3. Same fixture corpus.
4. One command.
5. A concrete behavior regression.
6. The first responsible divergence when observable.
7. CI-safe FAIL result.
```

Illustrative output:

```text
245 fixtures tested

232 identical
10 expected
2 warnings
1 critical regression

CRITICAL
Deployment/payments-api

Removed initContainer:
  vault-agent-init

First divergence:
  istio-sidecar-injector

Candidate mutation:
  remove /spec/initContainers

Upgrade safety: FAIL
```

## 38. Repository Governance Guardrails

Contributor guidance should state:

> Admission Lab must remain local-first, deterministic, vendor-neutral at the core, real-cluster authoritative, CI-friendly, safe by default, fully open source, and useful without a server.

Every significant feature proposal should answer:

1. Which concrete admission or Gateway regression does this enable Admission Lab to detect, explain, or gate?
2. Why cannot the existing core model express it?
3. Does it preserve deterministic behavior?
4. Does it introduce a vendor-specific dependency into the generic engine?
5. Can it remain useful in local/CI workflows without a central service?

Features that cannot answer these questions should normally remain out of scope.

## 39. Key Product Risks

### 39.1 Audit trace limitations

Kubernetes versions and audit configuration may expose different levels of webhook detail. First-divergence attribution must degrade gracefully and never overclaim causality.

### 39.2 Test fidelity versus complexity

A disposable cluster cannot reproduce every production dependency. Admission Lab should focus on deterministic control-plane compatibility and explicit traffic contracts rather than pretending to emulate production completely.

### 39.3 Fixture quality

The engine cannot catch behavior that the fixture corpus never exercises. The product should make fixture coverage visible without prematurely becoming a fuzzing platform.

### 39.4 Matrix explosion

Kubernetes versions multiplied by component versions can make certification expensive. Tiered matrices and a small set of certified combinations are required.

### 39.5 Noisy semantic diff

Poor normalization or overly broad severity rules would make the tool easy to ignore. Regression quality is therefore a product feature, not merely an implementation detail.

### 39.6 Gateway nondeterminism

Traffic-based tests such as weighted routing can be probabilistic. v1 should prefer deterministic contracts and carefully bounded statistical assertions only when required.

## 40. Post-v1 Exploration

Post-v1 candidates, ordered by likely product fit rather than commitment:

1. sanitized workload capture from a user-selected cluster;
2. generated edge-case fixture packs;
3. Kustomize installation support if not already delivered in Beta;
4. `GRPCRoute` and `BackendTLSPolicy`;
5. Envoy Gateway recipe;
6. Kong recipe;
7. Traefik recipe;
8. Cilium Gateway API recipe;
9. Vault Agent Injector recipe;
10. OpenTelemetry Operator recipe;
11. cert-manager interaction suites;
12. optional `admissionlab serve` for self-hosted history/scheduling;
13. advisory fast/static analysis only if it materially shortens iteration without being confused with authoritative results.

None of these are prerequisites for v1.

## 41. Final Product Boundary

Admission Lab exists to detect, explain, and gate behavior changes caused by Kubernetes admission and Gateway-stack changes before production.

If a feature does not materially improve one of those three capabilities, it should not be part of the core product without a compelling new product decision.
