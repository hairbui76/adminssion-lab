# Schema versions and migration policy

Admission Lab publishes three versioned document families. Two of them are
written by a run and read by whatever comes after it — a report viewer, a CI
job, `admissionlab reproduce` — and one is written by a user and read by
Admission Lab. All three carry their version *in the document*, and this file
is the single statement of what a version bump is allowed to change, what a
reader must do with a version it does not know, and where each family stands
today.

> **Pre-v1.0.** Everything below describes the rule in force through Public
> Beta. Section [At v1.0](#at-v10) says what changes when the schemas become
> stable in Phase 9.

---

## Contents

- [The three families](#the-three-families)
- [The compatibility rule](#the-compatibility-rule)
- [What counts as a silent semantics change](#what-counts-as-a-silent-semantics-change)
- [How a reader must behave on an unknown version](#how-a-reader-must-behave-on-an-unknown-version)
- [Adding a field](#adding-a-field)
- [Removing or renaming a field](#removing-or-renaming-a-field)
- [Migration notes](#migration-notes)
- [How the rule is enforced](#how-the-rule-is-enforced)
- [At v1.0](#at-v10)

---

## The three families

| Family | Document | Version string | Written by | Read by |
| --- | --- | --- | --- | --- |
| Configuration | `admissionlab.yaml` (and `expectations.yaml`) | `apiVersion: admissionlab.io/<version>` | a user, by hand | `admissionlab_spec::load_any_supported_lab` |
| Result | `result.json` | `schemaVersion: admissionlab.io/result/<version>` | `admissionlab test` | reports, CI, downstream tooling |
| Run manifest | `run.json` | `schemaVersion: admissionlab.io/run-manifest/<version>` | `admissionlab test` | `admissionlab reproduce`, bug reports |

The version string is a field of the document, never something a reader infers
from a filename or a directory layout. That is what lets a consumer holding one
arbitrary Admission Lab JSON file tell what it is holding, and it is why every
family's version carries the `admissionlab.io/` prefix and the family name.

### Where each family stands

| Family | Current version | Also readable | Schema files |
| --- | --- | --- | --- |
| Configuration | `admissionlab.io/v1beta1` | `admissionlab.io/v1alpha1`, migrated on load | [`schemas/admissionlab-v1beta1.json`](../schemas/admissionlab-v1beta1.json), [`schemas/admissionlab-v1alpha1.json`](../schemas/admissionlab-v1alpha1.json) |
| Result | `admissionlab.io/result/v1beta1` | — | [`schemas/result-v1beta1.json`](../schemas/result-v1beta1.json) |
| Run manifest | `admissionlab.io/run-manifest/v1beta1` | `admissionlab.io/run-manifest/v1alpha1`, read as-is | [`schemas/run-manifest-v1beta1.json`](../schemas/run-manifest-v1beta1.json), [`schemas/run-manifest-v1alpha1.json`](../schemas/run-manifest-v1alpha1.json) |

Each family's row is maintained by the code that owns it; the run manifest row
is authoritative as of ROADMAP Task 7.3, and the configuration and result rows
are the state Tasks 7.1 and 7.2 establish. When a family gains a version, the
row above and the [migration note](#migration-notes) for that step are part of
the same change — a version bump with no line here is an incomplete one.

Note that the three families version **independently**. A `v1beta1` run manifest
recording a run driven by a `v1alpha1` configuration is an ordinary, correct
document, and the manifest's own `configApiVersion` field is what says so.

**Two documents inside the configuration family version independently of the
`Lab` document too**, and both are still `admissionlab.io/v1alpha1`: the
`Expectations` document (`expectations.yaml`) and the `FixtureMatrix` document
(`*.matrix.yaml`, owned by `admissionlab-fixtures`). Public Beta promoted the
`Lab` document only. Setting either of those to `v1beta1` to match a lab file
beside it is a configuration error, not a migration — and when either is
promoted, it gets its own note below.

---

## The compatibility rule

Straight from the roadmap (ROADMAP Task 7.3, step 2):

> Before v1.0:
>
> - Beta readers may add optional fields.
> - Existing field semantics cannot change silently.
> - Removing/renaming fields requires a new schema version and migration note.
>
> At v1.0, stable schema rules become stricter in Phase 9.

Read as three obligations:

1. **Additions are optional.** A field added within a version — or across one —
   must be optional, so every document written before it existed is still a
   valid document. In Rust that means an `Option` (or a `#[serde(default)]`
   collection), and in the generated JSON Schema it means the field is *not* in
   `required`.
2. **Meanings are frozen.** A field that exists keeps meaning exactly what it
   meant. Changing what a value denotes, what unit it is in, or which of two
   claims its absence makes is a breaking change even when the type and the name
   are untouched — see [below](#what-counts-as-a-silent-semantics-change).
3. **Removals and renames need a version and a note.** They cannot be done
   quietly at any point, because a consumer that was reading the field has no way
   to notice.

### Absence is a value

An optional field's absence is itself a claim, and the rule above applies to it
as much as to any value it can hold. Global Constraint 15 forbids fabricating
observations, so a field this build could not fill is `null` (or absent), never
an empty string, a zero, or a plausible-looking default.

That makes "absent" ambiguous in exactly one place — a document written before
the field existed also lacks it — and the resolution is always the same: the
document's own version says which claim its absence is making. In a `v1beta1`
run manifest, `gateway: null` means "this run had no Gateway suite". In a
`v1alpha1` one, it means "the build that recorded this run could not say".
A reader that normalizes an old document's version away has destroyed the only
evidence of which one it is holding, so readers do not do that
(see [below](#how-a-reader-must-behave-on-an-unknown-version)).

---

## What counts as a silent semantics change

All of these keep a field's name and JSON type and are therefore breaking
changes that no schema diff will catch. They require a new version and a
migration note exactly as a rename would:

- **Changing a unit.** `reconciliationTimeoutMillis` becoming seconds.
- **Changing what a value is a digest of.** A `*Sha256` field that hashed a
  file's own bytes and starts hashing a re-serialization of the parsed
  document — same field, same shape, different answer for unchanged input.
- **Changing what absence means.** A field whose `null` meant "there was no
  such thing" starting to mean "we could not determine it", or the reverse.
- **Narrowing or widening an enumeration's meaning.** Reusing an existing wire
  literal for a new case, so an old reader silently mis-classifies it. New
  cases get new literals.
- **Changing an array's ordering contract.** An array whose order was
  meaningful (a normalization profile's rules, a suite's routes) becoming
  arbitrary, or vice versa.

The mitigation for all five is the same and is structural rather than a matter
of care: wire literals are pinned in `#[serde(rename = ...)]` attributes rather
than derived from Rust identifiers, so a Rust-side rename cannot reach a
document by accident, and a deliberate wire change has to be typed out in the
place this policy applies.

---

## How a reader must behave on an unknown version

1. **Read the version first, before the body.** A reader parses the version
   field on its own, decides what it is holding, and only then deserializes.
   Deserializing first and checking the version afterwards means a document
   from the future has already been interpreted under this build's rules.
2. **Refuse what it does not know, by name.** An unrecognized version is an
   error, never a best-effort read. The error names the version found *and
   every version this build supports*, because the actionable question is
   "which Admission Lab do I need".
3. **Never partially read.** Unknown fields are rejected
   (`#[serde(deny_unknown_fields)]`), not ignored. For a provenance document
   this matters more than usual: silently dropping a field means reproducing
   from a record the build only partly understood.
4. **Keep reading records; migrate inputs.** The two kinds of document call for
   different answers to an *old* version:
   - A **record** of something that already happened — a run manifest, a result
     — is read at its own version, and its version field is preserved rather
     than rewritten. Refusing to read a run's provenance because Admission Lab
     has since promoted its own schema would throw away exactly the artifact
     the document exists to preserve.
   - A **configuration** is a user's input, and there is one current vocabulary
     to validate against, so an older version is migrated forward on load (or
     refused with a message naming the version to write). What must never
     happen is that it is read as though it were current.
5. **Write only the newest version.** A build reads every version it supports
   and emits exactly one. There is no "write the version we read" mode: a
   document written today describes today's run with today's fields.

For the run manifest, `admissionlab_core::read_run_manifest` is the single entry
point that does all five, and every path that turns `run.json` bytes into a
value goes through it.

---

## Adding a field

The routine change, and the only one this policy makes cheap:

1. Add it as `Option<T>` (or a defaulted collection), never as a required
   field, with `#[serde(rename = "camelCaseName", default)]`.
2. Document *why the field exists* on the field itself — what question it
   answers that no existing field answers, and what its absence means at each
   version that can produce the document.
3. Regenerate the schema file and the golden document, and check both in.
4. If the addition changes what a run must record to be reproducible, say so in
   the field's own documentation, so the next reader knows whether an older
   document is merely terser or actually less useful.

No version bump is needed for an addition. A version bump is needed when the
*set* of additions is worth naming as a generation — which is what
`run-manifest/v1beta1` is: not a breaking change, but a line drawn so that
"this manifest records the Gateway suite and the side-loaded images" is a claim
a reader can make from the version string alone.

## Removing or renaming a field

1. Pick the next version.
2. Keep the previous version's schema file checked in, unchanged, forever. It
   is the published contract for documents that already exist, and it is the
   reference the next promotion is measured against.
3. Write a [migration note](#migration-notes).
4. Decide, explicitly, whether the reader still accepts the old version. For a
   record it usually should; when it does, the old version's fields are mapped
   onto the current model at read time and everything the new version added is
   honestly absent.

---

## Migration notes

One entry per version step, per family. A note says what changed, what a
consumer must do, and what a document from the previous version still means.

### Configuration: `v1alpha1` → `v1beta1` (Task 7.1)

**Two fields were renamed. Nothing else changed, in either direction.**

| `v1alpha1` | `v1beta1` | Type and meaning |
| --- | --- | --- |
| `policy.latency.absoluteIncrease` | `policy.latency.absoluteIncreaseMillis` | Unchanged: a plain integer, milliseconds, same default (`100`). |
| `gateway.reconciliationTimeout` | `gateway.reconciliationTimeoutMillis` | Unchanged: a plain integer, milliseconds, same default (`120000`). |

**Why:** both values were always milliseconds, and neither name said so. A
reader looking at `absoluteIncrease: 50` had to open the documentation to learn
whether that was fifty milliseconds or fifty seconds. Putting the unit in the
name is the kind of change that is only cheap once, at a version boundary — which
is precisely what a version boundary is for. Every other name was examined at
the same time and deliberately kept, including `images`, `expectationsFile` and
`failOn`.

**What a user must do:** nothing, to keep running. A `v1alpha1` document loads
unchanged. To move a file forward, change the `apiVersion` line **and** whichever
of the two keys the file uses, together.

**The two spellings do not mix, on purpose.** A `v1beta1` document containing
`absoluteIncrease`, or a `v1alpha1` document containing `absoluteIncreaseMillis`,
is a named parse error rather than a tolerated alias. An alias would let a file
mean something it does not say, and would make "we renamed it" a claim in a
changelog rather than a fact about the parser.

**What a `v1alpha1` document still means:** exactly what it always did. It is
parsed against the frozen `v1alpha1` model, migrated to the `v1beta1` model as
pure data — no I/O, no path resolution, no defaulting — and resolved by the one
resolver both versions share, so it produces a byte-for-byte equal resolved lab.
Migration is total: no document the `v1alpha1` loader accepts can fail to
migrate, which `crates/admissionlab-spec/tests/migrate_alpha_beta.rs` asserts
over every checked-in Alpha document rather than over one hand-picked example.
`testdata/configs/renamed-fields-v1alpha1.yaml` is kept in the repository, with
every optional section populated, for exactly that purpose.

**`expectations.yaml` was not promoted.** The `Expectations` document versions
independently of the `Lab` one and is still `admissionlab.io/v1alpha1` — the only
value its loader accepts. Setting it to `v1beta1` to match the lab file beside it
is a configuration error, and when it *is* promoted, that step gets its own note
here.

### Result: → `v1beta1` (Task 7.2)

The result document's first published version is `admissionlab.io/result/v1beta1`,
so there is no migration step and no earlier schema file to keep valid. Alpha
builds emitted `admissionlab.io/result/v1alpha1`, which was explicitly labelled
experimental and never had a checked-in schema; a consumer holding one should
read it as the pre-Beta document it is and not expect the `v1beta1` shape.

What `v1beta1` freezes, and what a consumer can therefore rely on: three sibling
evidence sections per fixture (`admission`, `gatewayReconciliation`, `traffic`),
each always present — as `null` where it does not apply, never omitted — and
stable `sc-<16hex>` semantic-change identifiers. From here the document grows by
**addition** only. A consumer must tolerate fields it does not know; that is the
whole of what the freeze asks of it.

### Result: additions within `v1beta1` (Task 8.8)

**No version bump. Two optional additions, and one existing document shape left
byte-identical.**

| Where | Addition | Absent means |
| --- | --- | --- |
| result, top level | `migration` — an array of Ingress-to-Gateway migration cases, each with its `caseId`, its `comparability` (and that answer in prose), its graded `changes`, its paired `probes`, and any `unmatchedExpectations` | the lab declared no `migration:` section |
| configuration, `migration:` | `baseline` / `candidate`, each a `gatewayEndpoint:` block saying where that side's data plane is | the suite cannot be run; see below |

**Why `migration` is a top-level array and not a `fixtures` entry.** A fixture's
findings are `SemanticChange`s, graded by `admissionlab-policy` and counted into
the five `summary` buckets. A migration case's findings are
`MigrationBehaviorChange`s, which are a deliberately separate vocabulary
(`admissionlab_gateway::migration` states why at length). Putting them in
`fixtures` would have meant either inventing `SemanticChangeKind` variants for
six routing behaviors or flattening all six into one — so they sit in their own
list, with their own `severity` on each change. The consequence is stated rather
than hidden: **`summary`'s five counts still partition `fixtures` exactly and do
not count migration cases**, and no migration change appears in
`policy.changes`. What a migration case *does* reach is `policy.disposition`,
which is the run's verdict and what the exit code is derived from.

**The key is omitted, not `null`, when there is no suite.** That is what keeps
an admission-only or Gateway-only `result.json` byte-identical to what earlier
builds wrote, which is the property "additive" is supposed to name. It is the
same treatment `timings` already gets, and it does not weaken Global Constraint
15: where a claim could be misread, the section states it *inside* itself
(`comparability`, `comparabilityReason`), which is where the ambiguity actually
lives.

**The two configuration fields are optional in the schema and required to
run.** Making them required would invalidate any document that already used the
`migration:` section, which the compatibility rule forbids. Making them absent
and harmless would let a lab install two stacks, probe nothing, and report
success — a migration case's probes are the *only* thing its two sides can be
compared on. So they are optional in
[`schemas/admissionlab-v1beta1.json`](../schemas/admissionlab-v1beta1.json) and
refused by `admissionlab test`'s own pre-flight validation, before any cluster is
created, with a message naming the missing field. A document written before Task
8.8 still parses; it simply cannot run a migration, which was already true.

### Run manifest: `v1alpha1` → `v1beta1` (Task 7.3)

**Nothing was removed, renamed, or redefined.** Three optional fields were
added, each recording something a run gained a dependency on in Phase 6 that a
`v1alpha1` manifest could not express:

| Field | What it records | Why a reproduction needs it |
| --- | --- | --- |
| `configApiVersion` | the `apiVersion` of the lab configuration the run was driven by | `configSha256` pins which *bytes* were read, not which vocabulary they were read under; a build that no longer loads that version can now say so from the manifest alone |
| `baseline.images` / `candidate.images` | the local container images side-loaded into that side's cluster | side-loaded images are by construction the ones no registry can supply, so they are the one input a reproduction on another machine cannot obtain by asking |
| `gateway` | the Gateway suite's route ids, its reconciliation timeout, and which data-plane endpoint strategy was in effect (if any) | route contracts are compared and reported like fixtures but appear in no `fixtureHashes`; and a suite with no endpoint sends no traffic probe at all, which changes what evidence the run produced |

**What a consumer must do:** nothing, if it reads fields it knows and tolerates
new ones. A consumer that validates against the schema should validate against
[`schemas/run-manifest-v1beta1.json`](../schemas/run-manifest-v1beta1.json);
`v1alpha1` documents remain valid against the frozen
[`schemas/run-manifest-v1alpha1.json`](../schemas/run-manifest-v1alpha1.json).

**What a `v1alpha1` document still means:** exactly what it always did. Every
field it carries has the same meaning in `v1beta1`, `admissionlab reproduce`
reads and plans from it with no migration step, and the three fields above are
absent rather than defaulted — "the build that recorded this run did not record
it", not "there were none".

**Known gap, deliberately not papered over.** A Gateway suite's manifest files
are referenced from the configuration by path, so their *content* is covered by
no digest in the manifest — `configSha256` covers the lab file, not the files it
points at. `gateway` records what the suite was, not what its manifests said.
Closing that needs a digest the run computes over those files, which is a change
to what `admissionlab test` hashes rather than a change to this document alone.

---

## How the rule is enforced

Not by review alone. Each mechanism below fails the build rather than producing
a warning:

- **Schemas are generated from the same derives that govern serialization**, so
  a published schema can never describe a shape Admission Lab does not write. A
  test regenerates each current schema and compares it byte-for-byte with the
  checked-in file (`cargo test -p admissionlab-core --test run_manifest`), with
  an `#[ignore]`d regenerator alongside it so the generator and the checker
  cannot drift.
- **Superseded schema files are frozen, with no generator behind them.** A
  generator can only describe the type that exists now, so a "v1alpha1
  generator" would silently start describing v1beta1 the moment a field was
  added — which is how a frozen schema stops being frozen.
- **The compatibility rule itself is a test.**
  `crates/admissionlab-core/tests/run_manifest_beta.rs` compares the generated
  `v1beta1` schema against the frozen `v1alpha1` file and fails if a property
  was dropped or renamed, if a requirement was dropped, or if an addition is
  required rather than optional.
- **Golden documents are checked in and compared**, so any change to the wire
  form of a document — including one made accidentally, by reordering fields or
  changing a serializer — shows up as a diff in a file a reviewer reads.
- **A top-level key set is frozen by test**, so a new field in a run manifest
  cannot appear without someone deliberately updating that list, at which point
  the "what this document may never contain" rules (Global Constraint 14: no
  paths, no environment, no captured output, no credential material) are in
  front of them.

---

## At v1.0

Phase 9 makes these rules stricter, and this file is where that change will be
written down. The direction is already fixed by Global Constraint 10 ("schema
stability"): at v1.0 a `v1` schema stops being a moving target, additions stop
being a routine change made without discussion, and the guarantee offered to
consumers becomes a compatibility promise rather than a working practice.

Until then, every version here is explicitly pre-stable: `v1alpha1` is
experimental, `v1beta1` is the Public Beta contract under the rule above, and
neither carries a v1.0 stability promise.
