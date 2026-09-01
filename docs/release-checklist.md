# Release PR checklist

The manual acceptance pass a release candidate is judged against (ROADMAP Task
10.2). Copy this file's checklist into the release PR body and fill it in
there; the PR is where the results are recorded, not this file.

**Rule from Task 10.3:** no new feature enters the RC stabilization window. A
row that fails becomes a release blocker fixed by
`reproduce -> failing test -> minimal fix -> narrow test -> full phase gate ->
commit`. A non-blocking enhancement discovered along the way becomes a post-v1
issue instead.

Every row is a claim about the **packaged release binary** — the one unpacked
from `admissionlab-<version>-<target>.tar.gz` — never about `target/`. Record
for each row the command run, the exit code, and where the evidence lives
(artifact directory, job URL, or pasted output).

## Prerequisite gates

Before starting the rows below, the Phase 9 exit gate must be green on the
tagged tree:

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

## The seventeen rows

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

## What only an operator can sign off

These need infrastructure a local machine does not have. They are listed here
so a PR that cannot tick them says so explicitly rather than leaving the row
blank:

- **Fresh Linux CI runner and macOS runner install** of the RC artifact, then
  `doctor`, the basic admission example, and the Istio Gateway example on each
  (Task 10.1 Step 4). A local unpack-and-run of the same tarball is a stand-in
  for the Linux half only, and proves nothing about macOS.
- **A real repository PR driven by the RC GitHub Action**, verifying the
  policy failure surfaces and the artifacts upload (Task 10.1 Step 5).
- **Sigstore signature verification** (`cosign verify-blob`) — keyless signing
  needs the release workflow's GitHub OIDC identity, so neither the signature
  nor its certificate exists before the tag is pushed.
- **The other three release targets.** `scripts/verify-release.sh` builds and
  checks the host target only, by design; the workflow's native runners cover
  the rest.
