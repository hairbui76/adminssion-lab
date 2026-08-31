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

There is no strict/offline mode in Alpha. A future one is desirable. Until then,
the mitigation is environmental: run in a CI environment with the network policy
you actually want, rather than assuming the tool imposes one.

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

### 2. Sensitive headers

```text
authorization, proxy-authorization, cookie, set-cookie, x-auth-token, x-api-key
```

Matched case-insensitively, in two forms:

- **In any string the result carries** — diagnostics, rejection messages,
  API-server warnings, divergence explanations, component names and versions,
  stale-expectation reasons, webhook names, subjects — a header name at a *word
  boundary*, followed by optional whitespace and a `:`, has its value replaced
  **through the end of that line**.
- **As an object key**, compared for *equality* rather than substring, and only
  when the value is a string. Equality is deliberate: a substring test would
  blank an unrelated field named `authorizationMode`.

The word-boundary rule is pinned by test. Given
`"x-authorization-mode: RBAC\nauthorizationMode: Node\nCookie: session=abc"`,
only the last line is redacted.

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

> **Alpha limitation.** This is a library capability with no YAML surface yet.
> `admissionlab.yaml` has no `redaction:` section, so configured pointers cannot
> be set from configuration today. Rules 1, 2, 3a, and 4 apply regardless.

### 4. Credential-like environment values

An object with a string `name` **and** a string `value` — the Kubernetes
`EnvVar` shape — has its `value` replaced when the `name` contains, case
insensitively, any of:

```text
password, passwd, pwd, passphrase, secret, token, key, credential,
auth, signature, session, private, salt
```

These are substrings, so `DB_PASSWORD`, `password_file`, and `PGPASSWORD` all
match. The list deliberately **over-approximates** — `key` matches
`MONKEY_HOST`, `auth` matches `AUTHOR_NAME` — because the two failure modes are
not symmetric. There is no way to remove an entry; narrowing redaction is not a
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

The proof is a whole-result test: eight distinct sentinel secrets, one per rule,
planted across a `final_object`, a webhook patch, a diagnostic message, and a
warning. The redacted result is serialized in full and asserted to contain none
of them — with a companion test asserting the *un*redacted result contains all
eight, so the first cannot pass vacuously.

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
