# Security model

Admission Lab's job is to install third-party admission controllers and run
untrusted-by-construction workloads through them. This document states what it
protects, what it deliberately does not, and where the sharp edges are.

To report a vulnerability, see [`SECURITY.md`](../SECURITY.md).

---

## Contents

- [Threat model in one page](#threat-model-in-one-page)
- [No production access](#no-production-access)
- [Trust model for third-party charts and controllers](#trust-model-for-third-party-charts-and-controllers)
- [Network egress](#network-egress)
- [Report redaction](#report-redaction)
- [What redaction does not cover](#what-redaction-does-not-cover)
- [Audit-log Secret exclusion](#audit-log-secret-exclusion)
- [Subprocess discipline](#subprocess-discipline)
- [Filesystem permissions](#filesystem-permissions)

---

## Threat model in one page

| Property | Status |
| --- | --- |
| Runs entirely on your machine, no service or account | **Guaranteed.** There is no hosted component in the v1 critical path. |
| Requires no production kubeconfig | **Guaranteed.** The default flow never reads one. |
| Copies no production secrets into the lab | **Guaranteed.** Nothing does this automatically. |
| Clusters are ephemeral and isolated from each other | **Guaranteed.** Separate `kind` clusters, separate kubeconfigs, deleted by default. |
| Secret material is kept out of rendered reports | **Guaranteed for the documented rules**, with the stated limits below. |
| Charts and controllers you install are safe | **Not a guarantee, and not something Admission Lab can offer.** See below. |
| Lab clusters cannot reach the network | **Not true.** See below. |

The primary asset Admission Lab protects is your **production environment**,
and it protects it by never touching it. The secondary asset is **secret
material that ends up in a report** — a report is the artifact people paste
into pull requests and chat.

The primary residual risk is that **you are executing third-party code on your
machine by design**.

---

## No production access

The default v1 flow requires no production cluster credentials and copies no
production secrets (Global Constraint 5, PRODUCT.md §29.2–29.3).

Concretely:

- Both clusters are created fresh by `kind` and receive their own kubeconfigs
  written into the run workspace. Your ambient `~/.kube/config` is not read for
  the run's own cluster access.
- Nothing walks a production cluster to seed fixtures. Fixtures are files you
  wrote. Production workload capture is an explicit v1 non-goal.
- The only credentials in play are the ones `kind` mints for its own throwaway
  clusters, and whatever a chart creates inside them.

If you deliberately point a fixture or a Helm values file at production
material, you have left the default flow. That is your decision to make, and
nothing here protects it.

---

## Trust model for third-party charts and controllers

**Read this before pointing a lab at an unfamiliar chart.**

Admission Lab installs Helm charts, raw manifests, admission webhooks, and
controllers into disposable Kubernetes clusters, and it treats all of them as
**untrusted test workloads** (PRODUCT.md §29.1). What that phrase actually
means in practice:

- **A chart you install runs with effectively cluster-admin authority inside the
  lab cluster.** `helm install` applies whatever the chart renders — including
  its own RBAC. Admission Lab does not sandbox, scan, or restrict it, and the
  same is true of a `type: manifests` component.
- **A `kind` cluster is not a security boundary against the host.** It is Docker
  containers running a Kubernetes control plane. A container escape, a
  privileged pod, or a hostPath mount reaches your machine. `kind` is an
  isolation *convenience* against cluster-state contamination, not a sandbox
  against hostile code.
- **A recipe is code you are choosing to execute.** A recipe cannot classify
  regressions (see [`docs/recipes.md`](recipes.md)), but it absolutely can name
  a chart and a repository URL, and installing it runs whatever is at that URL.
  Reviewing a third-party recipe means reviewing the chart it points at.
- **Recipe manifest paths are confined to the recipe's own directory tree**, so
  a relative `install.paths` entry cannot walk out with `../`. That is a guard
  against accidents, not against a recipe that simply points at a hostile chart
  repository.

The practical guidance:

- Treat `admissionlab test <config>` with the same care as `helm install` from
  the same source, because that is what it does.
- Review a chart, a chart repository, and a recipe before running them, exactly
  as you would before installing them anywhere else.
- In CI, run Admission Lab in an **isolated, disposable runner** — an ephemeral
  container or VM you throw away — not on a build host with credentials or
  shared state. This is the environment recommendation PRODUCT.md §29.5 requires
  this documentation to make.
- Prefer the certified recipes' exact pins, and pin your own charts exactly. The
  configuration loader already refuses floating Helm versions
  ([`docs/config.md`](config.md#what-pinned-means)); that rule exists for
  reproducibility, and it happens to also mean you are installing the artifact
  you reviewed.

---

## Network egress

**Lab clusters have network access, and Admission Lab does not restrict it.**

An installed chart, a controller, or an admission webhook running inside a lab
cluster can reach the network: pull images, phone home, call an external policy
service. Admission Lab itself also reaches the network to add Helm repositories
and pull charts and node images.

Stated as plainly as it deserves:

> **Every third-party chart, controller, and admission webhook a lab installs
> can make outbound network calls — to any host, at any time, for the whole life
> of the run — unless you isolate the environment yourself. Admission Lab
> applies no egress restriction of any kind, and a `kind` cluster imposes none
> on your behalf.** A webhook that receives your fixtures is a program with your
> fixtures and a socket.

There is no strict/offline mode. A future one is desirable. Until then, the
mitigation is environmental, and these are the approaches that actually work,
strongest first:

- **Run in a disposable, network-restricted VM or container.** An ephemeral CI
  runner whose egress is filtered at the hypervisor or host firewall is the only
  boundary here that a container escape does not defeat. This is the same
  recommendation as the one above for untrusted charts, for the same reason.
- **Deny egress by default at the host firewall and allow-list the registries
  you actually need** — your image registry, the chart repositories your
  configuration pins, and nothing else. A lab needs to pull images; it does not
  need to reach the open internet.
- **Pre-pull everything and cut the network for the run itself.** Node images,
  chart archives, and `images:` side-loads can all be fetched before the run;
  `kind load docker-image` puts a locally built image into a cluster without any
  registry at all. A run whose inputs are already on the machine is a run that
  can be executed with egress off.
- **Do not rely on Kubernetes `NetworkPolicy` inside the lab cluster.** `kind`'s
  default CNI does not enforce it, so a policy you apply there is likely to be
  accepted and ignored — the worst of both worlds, because it *looks* like a
  control.
- **Do not rely on the cluster being ephemeral.** Deleting a `kind` cluster
  afterwards removes the state, not the calls that were made while it was up.

If you cannot isolate the environment, treat the run as though the chart's
author were executing code on that machine with your network access — because
they are.

---

## Report redaction

Global Constraint 14. Every rendered report — terminal, `result.json`, and
`report.html` — is drawn from **one** redacted value. Redaction happens once, at
a single chokepoint, before any renderer sees the result; no renderer redacts on
its own, so none can be forgotten.

Five rules, applied unconditionally, with or without any configuration:

### 1. Kubernetes Secret payloads

Any JSON object anywhere in the result whose `kind` is `"Secret"` has every
value under `data` and `stringData` replaced with `[REDACTED]`. This reaches a
Secret nested inside a `List`'s `items` and one embedded in a webhook's JSON
Patch, not only a top-level object.

**Key names are kept.** Which keys a Secret carries, and whether that set changed
between the two sides, is exactly the kind of behavior difference this tool
exists to report. A `data` block that is not an object at all is replaced
wholesale.

### 2. Sensitive headers and credential fields

```text
authorization, proxy-authorization, cookie, set-cookie, x-auth-token, x-api-key
client-key-data, token, id-token, refresh-token, access-token, password
```

The first line is HTTP headers. The second is the credential half of a
**kubeconfig** and of the `client.authentication.k8s.io` credential a `user.exec`
block yields — the material Global Constraint 5 says never enters the lab, and
which reaches a report only through someone else's text: a controller quoting a
kubeconfig into an error, an installer's captured output arriving in a
diagnostic, a webhook echoing its own service-account token.

`certificate-authority-data` and `client-certificate-data` are deliberately
**not** on the list, for the same reason a `CERTIFICATE` PEM block is left alone
below: a certificate is public material a reader may need.

`token` and `password` over-approximate — they match a `token:` or `password:`
line in any prose, not only in a kubeconfig. That is the same asymmetry rule 4
documents: the cost is a message reading `token: [REDACTED]` where the value was
"expired"; the benefit is that a bearer token quoted into a controller's error
message does not reach a pull request.

Matched case-insensitively, in two forms:

- **In any string the result carries** — diagnostics, rejection messages,
  API-server warnings, divergence explanations, component names and versions,
  stale-expectation reasons, webhook names, subjects — a name at a *word
  boundary*, followed by optional whitespace and a `:`, has its value replaced
  **through the end of that line**.
- **As an object key**, compared for *equality* rather than substring, and only
  when the value is a string. Equality is deliberate: a substring test would
  blank an unrelated field named `authorizationMode` or `tokenReviewEnabled`.

The word-boundary rule is pinned by test. Given
`"x-authorization-mode: RBAC\nauthorizationMode: Node\nCookie: session=abc"`,
only the last line is redacted. It is also why `set-cookie`, `id-token`, and
`refresh-token` are listed separately from `cookie` and `token`: `-` is not a
word boundary.

### 3a. PEM private keys

A PEM block whose label (uppercased) contains `PRIVATE KEY` is replaced from
`-----BEGIN` through `-----END` with `[REDACTED PRIVATE KEY]`. A **truncated**
block — an opening marker with no matching close — is redacted through the end
of the string, because half a private key is still key material.

A block labelled `CERTIFICATE` or `PUBLIC KEY` is left alone: a certificate is
public material a reader may need to see.

### 3b. Configured sensitive paths

RFC 6901 JSON pointers, resolved against **each embedded payload's own root** —
a `final_object`, a semantic change's `baseline`/`candidate` value, or the value
inside a JSON Patch operation. That is the level at which a pointer is writable
by a human: `/data/licence` names something in *your* object.

A pointer that does not resolve in a given payload is a no-op for that payload,
not an error. The empty pointer `""` addresses a payload's root and replaces the
whole thing. Pointers run *after* the recursive rules, so a pointer's
replacement is the last word for its location.

> **Current limitation.** This is a library capability with no YAML surface yet.
> `admissionlab.yaml` has no `redaction:` section, so configured pointers cannot
> be set from configuration today. Rules 1, 2, 3a, and 4 apply regardless.

### 4. Credential-like environment values

An object with a string `name` **and** a string `value` — the Kubernetes
`EnvVar` shape — has its `value` replaced when the `name` contains, case
insensitively, any of:

```text
pass, password, passwd, pwd, passphrase, secret, token, key, credential,
auth, signature, session, private, salt
```

These are substrings, so `DB_PASSWORD`, `password_file`, `PGPASSWORD`, and
`SMTP_PASS` all match. The list deliberately **over-approximates** — `key`
matches `MONKEY_HOST`, `auth` matches `AUTHOR_NAME`, `pass` matches
`BYPASS_CACHE` — because the two failure modes are not symmetric. There is no way to remove an entry; narrowing redaction is not a
knob this tool offers. Extra patterns can be added through the library API.

Requiring both a string `name` and a string `value` means a `{name, valueFrom}`
entry — a *reference*, holding no literal — is never touched, and neither is an
unrelated object that happens to have a `name`.

### 5. Nothing else

That is the complete list.

### What redaction never does

- **It never removes structure.** Every rule replaces a value in place. No rule
  drops a field, an array element, a change, or a fixture. A reader of a
  redacted result can still see *that* an environment variable's literal
  changed, *which* variable it was, and *where* — only the two literals are
  gone. This is Global Constraint 15 applied to secrets: hiding a value is not a
  licence to hide the finding.
- **It never changes a verdict.** Severities, gradings, the disposition, and the
  summary counts are identical before and after. Pinned by test.
- **It is idempotent**, and it never mutates its input.

### How this is proved

Two test files, and the second is the one an operator depends on.

`tests/redact.rs` plants eight sentinel secrets, one per rule, across a
`final_object`, a webhook patch, a diagnostic message, and a warning, then
serializes the redacted result in full and asserts none of them survives — with
a companion test asserting the *un*redacted result contains all eight, so the
first cannot pass vacuously.

`tests/security_sentinels.rs` does the same thing for **every renderer**, with a
29-string corpus covering each credential *shape* rather than each rule: bearer
tokens in a header line and in a header map; `Cookie`, `Set-Cookie`,
`Proxy-Authorization`, `X-Api-Key` and `X-Auth-Token` in free text and as object
keys; credential-named environment literals in eight naming shapes, reached
through a final object, a change payload, and a webhook patch; PEM private keys
in four encodings **plus a real key generated by the same call the Gateway TLS
suite makes**; a Secret's `data` and `stringData`, including one a webhook patch
creates; and kubeconfig `client-key-data`/`token` in both line and object form.
The corpus is rendered through `redact_result` → `serde_json`,
`write_json_report`, `render_terminal`, `write_html_report`, and
`render_github_summary`, and every output is searched for every sentinel.

Because two of those renderers deliberately show less than the whole document —
the terminal summarizes, the GitHub summary truncates each cell — each renderer
is also run on the *unredacted* result first, to establish what it can show at
all. `result.json` must reach all 29; each of the others must reach at least one.
A renderer that stopped rendering payloads fails that check rather than passing
the absence check for the wrong reason.

---

## What redaction does not cover

Stated plainly, because a partial list read as complete is worse than no list.

### The `EnvVar`-shape gap

Rule 4 recognizes a *shape*. A credential parked under an arbitrary map key —
`{"dbPassword": "hunter2"}` in an annotation, a ConfigMap, or a CRD's spec — is
not auto-detected.

This is a deliberate limit, not an oversight. The obvious generalization —
redact any string whose key looks credential-like — destroys information that is
not secret and that a reader needs. A `secretKeyRef`'s `key` field names a key
*inside* a Secret; `secretName` names the Secret. Both match a naive "contains
`key` or `secret`" test, neither holds a credential, and blanking them would
hide exactly the rewiring the diff engine carries `valueFrom` blocks verbatim in
order to show.

Configured JSON pointers are the intended escape hatch — and, as noted above,
they have no YAML surface yet.

### Raw evidence on disk is not redacted

The per-fixture evidence bundle under
`${TMPDIR}/admissionlab-runs/<run-id>/raw/<side>/<fixture-id>/` holds the
request sent and the API response received, verbatim. Its protection is
**filesystem permissions, not redaction**: `raw/` and `kubeconfigs/` are created
mode `0700`, and kubeconfig files `0600`, set before the file is moved into
place so there is no loose-permission window. That enforcement is Unix-only.

Do not attach a raw evidence bundle to a public issue without reading it first.
The rendered reports are the artifacts intended for sharing.

### The failure-path `diagnostics.json` is not run through redaction

When a run fails at or after installation, it writes a `diagnostics.json` naming
the failed stage and the diagnostics collected so far. That artifact is
serialized directly. Diagnostic context values explicitly marked sensitive carry
no payload at all and are therefore safe, but the redaction pass described above
is not applied to this file. Treat it as evidence, not as a report.

### CLI arguments are never redacted

The subprocess layer classifies **environment variable values only**. A
program's path, its arguments, and its working directory are recorded verbatim.
A secret passed as a command-line flag — `helm install --set password=...` —
gets no protection from any layer. Use a values file, and remember that a values
file's *path* is what gets logged, not its contents.

### The run manifest is safe by construction, not by filtering

`run.json` — the reproducible run manifest, and the artifact most likely to be
attached to a bug report — is the one document here that needs no redaction
pass, because **no type in it can hold a secret**: not a `PathBuf`, not an
environment map, not captured output, not any cluster-connection material. Every
field is a version string, an image reference, an identifier, a SHA-256 digest,
or a timestamp.

That is a structural guarantee rather than a filtering one, which matters
because filtering silently stops working. A kubeconfig path cannot be
accidentally left in a field that cannot hold a path.

Three tests keep it that way: a manifest built from inputs that genuinely carry
secrets (a config file with a password, a fixture that *is* a Secret, a host
probe quoting paths under `$HOME`) is written through the real writer into a
home-shaped workspace and the bytes on disk are searched — and, so the check
cannot pass for an empty document, the digest of each of those inputs must be
present. A fourth walks the manifest's generated JSON Schema and rejects any
field, at any nesting depth, whose *name* looks like a path or a credential.

### Other layers exist, and none substitutes for another

Redaction is defence in depth, with separate implementations at separate
boundaries: the report pass described here; a subprocess-logging pass with its
own key list; a readiness-evidence pass in the installer; and one field-specific
sanitization in the diff engine. Each documents its own limits. The report pass
is the one that satisfies Global Constraint 14 for reports, and it cannot
substitute for the others.

---

## Audit-log Secret exclusion

Admission Lab configures the ephemeral API server's audit log at `Request`
level, because Kubernetes only exposes mutating-webhook patch annotations at
that level or higher (Global Constraint 18). A `Request`-level log contains
request **bodies**.

The audit policy therefore opens with an exclusion, and **its position is
load-bearing**:

1. **`secrets` → `level: None`.** No audit event of any kind — not even
   `Metadata` — is recorded for any request touching a core `secrets` resource.
   This rule must precede the general `Request`-level rule, which would
   otherwise match Secret mutations and defeat the exclusion. A dedicated test
   guards that ordering.
2. Health and discovery URLs (`/healthz*`, `/readyz*`, `/livez*`, `/version`,
   `/metrics`) → `level: None`.
3. Mutating verbs (`create`, `update`, `patch`, `delete`) on the
   admission-relevant API groups → `level: Request`.
4. Everything else → `level: Metadata`, which never includes a body.

### The policy is proved, not just inspected

`crates/admissionlab-cluster/tests/audit_policy_security.rs` parses the rendered
policy and resolves requests against it **the way kube-apiserver does** — first
match wins — rather than asserting on rule shapes. Four results:

1. **No `secrets` request is recorded at any level, for any verb.** The Phase 3
   exit gate observed this once on a real cluster; this is the standing
   unit-level version, and it covers requests no fixture makes.
2. **No rule anywhere is `RequestResponse`**, so no *response* body is ever
   written. That is what makes `serviceaccounts/token` safe: a `TokenRequest`
   create is matched by the `Request`-level rule (a group entry with no resource
   list matches subresources too), but a `Request` event records only the
   submitted body — audiences and an expiry — while the minted bearer token
   exists only in the response. One rule promoted to `RequestResponse` would
   turn every token request in the cluster into a logged credential.
3. **The admission-relevant group list is an allow-list, and that is
   load-bearing.** `authentication.k8s.io` is absent from it, so a `TokenReview`
   — whose *request* body is a bearer token in plain text — falls through to the
   `Metadata` catch-all. Widening that list to "cover more of the API" would
   start logging bearer tokens; a test rejects exactly that edit.
4. **One known boundary.** A Kubernetes audit rule matches a subresource only
   through an explicit `resource/subresource` entry, and the exclusion names
   `secrets`. Core Secrets have no subresources today, so nothing is missed; if
   one is ever added, a test says so rather than the policy quietly logging it.

The suite also inserts a `Request`-level Secret rule at *every* position and
asserts it is rejected exactly at the positions preceding the exclusion — which
is what first-match-wins means, stated as an experiment rather than a claim.

### ConfigMap bodies are recorded, deliberately

**A ConfigMap's `data` appears in the run's audit log.** The `Request`-level rule
covers the whole core API group, and ConfigMaps are in it.

This is a stance, not an oversight, and it was re-examined for this document. A
ConfigMap is an *admission-relevant workload input* here in a way a Secret is
not: the shipped example's control fixture is a ConfigMap, the fixture corpora
use them as ordinary objects, and policy engines routinely mutate them.
Demoting them to `Metadata` would drop the `patch.webhook.admission.k8s.io/*`
annotations for exactly those fixtures — Kubernetes attaches a patch annotation
only at `Request` or higher (Global Constraint 18) — which is the evidence this
tool exists to collect.

The trade, stated so you can act on it: **a fixture ConfigMap's contents are
visible in that run's audit log; a Secret's are not.** Fixtures are files you
wrote. If one needs a credential, put it in a Secret, which this policy never
records at all.

Two further defences around the audit log:

- **An unparsable audit line's own text is never reported.** The diagnostic
  records *that* a line at a given byte offset failed and how, with the line
  itself stored as a sensitive value that holds no payload. The underlying
  parser error's message is deliberately omitted too — for a type error it
  embeds the offending value.
- **Only a subset of each event is parsed at all.** The upstream audit event
  type carries `requestObject`, `responseObject`, and a `user` block; none of
  that is needed, and all of it is precisely what Global Constraint 14 keeps out
  of reports. Because the parsed subset carries no bodies, the preserved
  `audit.json` window in the raw evidence bundle is safe to keep in full.

---

## Subprocess discipline

Global Constraints 12 and 13.

- **Argv, never a shell string.** Every external command (`kind`, `kubectl`,
  `helm`, `docker`) is executed with an explicit argument vector. No shell is
  spawned, and no command string is ever assembled from user input, so there is
  nothing for a fixture name, a chart reference, or a path to be interpolated
  into.
- **Every command has a timeout**, separate stdout and stderr capture, version
  and provenance recording, and structured error context. A hung `helm` is a
  reported failure, not a hung run.
- **Environment values are classified before logging.** A key containing any of
  `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `CREDENTIAL`, `KEY`, `AUTH`, `CERT`,
  or `PRIVATE` (case-insensitive substring), or any key the caller marked
  sensitive, has its *value* withheld from the recorded command context. As
  noted above, this covers `env` only — never argv.

---

## Filesystem permissions

Each run gets a private workspace under `${TMPDIR}/admissionlab-runs/<run-id>/`:

```text
raw/           per-fixture evidence bundles      mode 0700
normalized/    normalized objects
reports/       result.json, report.html, diagnostics.json
logs/          diagnostic logs
kubeconfigs/   per-cluster kubeconfigs           mode 0700, files 0600
run.json       run metadata
```

`raw/` and `kubeconfigs/` are the two directories that hold genuinely sensitive
content, and both are restricted at creation. Permissions are set before a file
is renamed into place, so there is no window in which a kubeconfig is
world-readable. This is Unix-only; it is not enforceable on other platforms.

`--report-dir` relocates `result.json` and `report.html` only. Raw evidence
always stays in the run workspace, which is where every path inside the reports
points.

Workspaces are not garbage-collected. They live under your temporary directory
and are subject to whatever policy your system applies to it.
