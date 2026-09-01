# Schema versions and migration policy

Admission Lab publishes three versioned document families. Two of them are
written by a run and read by whatever comes after it — a report viewer, a CI
job, `admissionlab reproduce` — and one is written by a user and read by
Admission Lab. All three carry their version *in the document*, and this file
is the single statement of what a version bump is allowed to change, what a
reader must do with a version it does not know, and where each family stands
today.

> **Stable.** The three families are frozen at v1 (ROADMAP Task 9.1). Section
> [The stable-schema rule](#the-stable-schema-rule) is the rule now in force
> and the one a consumer can rely on; [The pre-v1.0 rule](#the-pre-v10-rule)
> is kept below it because it is what the `v1alpha1` and `v1beta1` documents
> still in circulation were written under.

---

## Contents

- [The three families](#the-three-families)
- [The stable-schema rule](#the-stable-schema-rule)
- [The pre-v1.0 rule](#the-pre-v10-rule)
- [What counts as a silent semantics change](#what-counts-as-a-silent-semantics-change)
- [How a reader must behave on an unknown version](#how-a-reader-must-behave-on-an-unknown-version)
- [Adding a field](#adding-a-field)
- [Removing or renaming a field](#removing-or-renaming-a-field)
- [Migration notes](#migration-notes)
- [How the rule is enforced](#how-the-rule-is-enforced)
- [Where the older versions stand now](#where-the-older-versions-stand-now)

---

## The three families

| Family | Document | Version string | Written by | Read by |
| --- | --- | --- | --- | --- |
| Configuration | `admissionlab.yaml` (and `expectations.yaml`) | `apiVersion: admissionlab.io/<version>` | a user, by hand | `admissionlab_spec::load_any_supported_lab` |
| Result | `result.json` | `schemaVersion: admissionlab.io/result/<version>` | `admissionlab test` | reports, CI, downstream tooling |
| Run manifest | `run.json` | `schemaVersion: admissionlab.io/run/v1` (`admissionlab.io/run-manifest/<version>` before v1) | `admissionlab test` | `admissionlab reproduce`, bug reports |

The version string is a field of the document, never something a reader infers
from a filename or a directory layout. That is what lets a consumer holding one
arbitrary Admission Lab JSON file tell what it is holding, and it is why every
family's version carries the `admissionlab.io/` prefix and the family name.

### Where each family stands

| Family | Current version | Also readable | Schema files |
| --- | --- | --- | --- |
| Configuration | `admissionlab.io/v1` | `admissionlab.io/v1beta1` and `admissionlab.io/v1alpha1`, migrated on load | [`schemas/admissionlab-v1.json`](../schemas/admissionlab-v1.json), [`schemas/admissionlab-v1beta1.json`](../schemas/admissionlab-v1beta1.json), [`schemas/admissionlab-v1alpha1.json`](../schemas/admissionlab-v1alpha1.json) |
| Result | `admissionlab.io/result/v1` | — (emit-only) | [`schemas/result-v1.json`](../schemas/result-v1.json), [`schemas/result-v1beta1.json`](../schemas/result-v1beta1.json) |
| Run manifest | `admissionlab.io/run/v1` | `admissionlab.io/run-manifest/v1beta1` and `admissionlab.io/run-manifest/v1alpha1`, read as-is | [`schemas/run-manifest-v1.json`](../schemas/run-manifest-v1.json), [`schemas/run-manifest-v1beta1.json`](../schemas/run-manifest-v1beta1.json), [`schemas/run-manifest-v1alpha1.json`](../schemas/run-manifest-v1alpha1.json) |

Each family's row is maintained by the code that owns it and is authoritative
as of ROADMAP Task 9.1. When a family gains a version, the row above and the
[migration note](#migration-notes) for that step are part of the same change —
a version bump with no line here is an incomplete one.

**The run manifest's stable identifier drops the `-manifest` infix.** It is
`admissionlab.io/run/v1`, not `admissionlab.io/run-manifest/v1`. A version
identifier is a string consumers match on, so it can only change *at* a version
boundary, and this was the last such boundary before the name became permanent.
The two pre-stable identifiers keep their original spelling, because a document
already on disk carries the string it was written with.

Note that the three families version **independently**. A `v1` run manifest
recording a run driven by a `v1alpha1` configuration is an ordinary, correct
document, and the manifest's own `configApiVersion` field is what says so.

**Two documents inside the configuration family version independently of the
`Lab` document too**, and both are still `admissionlab.io/v1alpha1`: the
`Expectations` document (`expectations.yaml`) and the `FixtureMatrix` document
(`*.matrix.yaml`, owned by `admissionlab-fixtures`). Neither the Public Beta
promotion nor the stable freeze touched anything but the `Lab` document.
Setting either of those to `v1` to match a lab file beside it is a
configuration error, not a migration — and when either is promoted, it gets its
own note below.

---

## The stable-schema rule

Straight from the roadmap (ROADMAP Task 9.1, step 4). **Within `v1.x`:**

> - optional additive fields are allowed when old readers can ignore them;
> - existing field meaning cannot change;
> - required fields cannot be removed;
> - semantic change serialization strings cannot be renamed without a new
>   result schema version;
> - exit codes cannot be reassigned.

Read as five obligations, each with the test that fails the build when it is
broken:

1. **Additions are optional, and only additions are allowed.** A new field is
   an `Option` (or a `#[serde(default)]` collection) and is absent from the
   generated schema's `required` list, so every document written before it
   existed is still valid. Pinned by the superset tests in
   `crates/admissionlab-spec/tests/stable_schema.rs`,
   `crates/admissionlab-report/tests/stable_schema.rs`, and
   `crates/admissionlab-core/tests/run_manifest_beta.rs`, each of which
   compares the generated `v1` schema against every frozen predecessor file:
   no property dropped or renamed, no requirement dropped, no required
   addition — at the root *and* in every shared definition.
2. **Existing field meaning cannot change.** Not the unit, not what a digest
   is taken over, not what absence claims, not what an existing wire literal
   denotes, not an array's ordering contract — see
   [what counts as a silent semantics change](#what-counts-as-a-silent-semantics-change).
   This is the clause a schema diff cannot see, so what enforces it is that
   every wire literal is written out in a `#[serde(rename = ...)]` and every
   document has a checked-in golden: changing a meaning means editing a
   pinned string or moving a golden byte, in a diff a reviewer reads.
3. **Required fields cannot be removed.** The same superset tests as clause 1
   check the `required` lists specifically, because a dropped requirement
   reads as a relaxation and is in fact a semantics change: a consumer that
   was entitled to the field is not any more.
4. **Semantic-change serialization strings cannot be renamed** without a new
   *result* schema version. The closed set of `snake_case` strings
   (`newly_denied`, `container_removed`, `traffic_backend_changed`, ...) is a
   vocabulary a consumer's `match` and a user's `policy.failOn` are both
   written against. `crates/admissionlab-report/tests/stable_schema.rs` lists
   every one of them as a literal and compares that list against the published
   schema; `crates/admissionlab-diff/tests/types.rs` proves each Rust variant
   serializes to its pinned string, exhaustively. Adding a *new* case with a *new*
   literal stays allowed — that is clause 1 — and shows up in those lists as
   a deliberate edit.
5. **Exit codes cannot be reassigned.** That contract belongs to ROADMAP Task
   9.2 and is documented and pinned there (`docs/troubleshooting.md`,
   `crates/admissionlab-cli/tests/exit_codes.rs`). It is listed here because
   the exit code is part of the same promise to a CI job that the result
   document is: both answer "what happened", and a reassignment would be
   invisible to a consumer that only reads the number.

**What a breaking change now costs.** Removing or renaming a field, changing a
meaning, or renaming a wire literal requires `v2` — a new `apiVersion` or
`schemaVersion`, a new schema file, a migration note here, and a reader that
keeps accepting `v1`. That is deliberately expensive. It is the difference
between a schema that is *published* and one that is merely *current*.

## The pre-v1.0 rule

The rule the `v1alpha1` and `v1beta1` documents were written under, kept
because those documents still exist and this build still reads them (ROADMAP
Task 7.3, step 2):

> Before v1.0:
>
> - Beta readers may add optional fields.
> - Existing field semantics cannot change silently.
> - Removing/renaming fields requires a new schema version and migration note.
>
> At v1.0, stable schema rules become stricter in Phase 9.

Read as three obligations, all of which the stable rule above keeps and
tightens:

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
document's own version says which claim its absence is making. In a `v1` or
`v1beta1` run manifest, `gateway: null` means "this run had no Gateway suite".
In a `v1alpha1` one, it means "the build that recorded this run could not say".
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

No version bump is needed for an addition, at v1 as before it. A version bump
was previously also available when the *set* of additions was worth naming as a
generation — which is what `run-manifest/v1beta1` was: not a breaking change,
but a line drawn so that "this manifest records the Gateway suite and the
side-loaded images" is a claim a reader can make from the version string alone.
**Within `v1.x` that option is gone**: `v2` means a break, so a purely
cosmetic generation bump would spend a consumer's whole migration budget on
nothing. Additions inside `v1` are announced in a [migration
note](#migration-notes) and in the field's own documentation instead.

## Removing or renaming a field

At v1 this is a `v2`, and the steps below are what a `v2` costs. Read [the
stable-schema rule](#the-stable-schema-rule) first: the answer to "can I rename
this field" is no, and the answer to "can I add the new one and leave the old
one" is yes.

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
of the two keys the file uses, together — and note that the version to move it
*to* today is `admissionlab.io/v1`, which spells both keys the way `v1beta1`
does (see the [`v1beta1` → `v1` note](#configuration-v1beta1--v1-task-91)).

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
value its loader accepts. Setting it to the lab file's version to match is a
configuration error, and when it *is* promoted, that step gets its own note
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

### Configuration: `v1beta1` → `v1` (Task 9.1)

**Nothing was renamed, removed, added, or redefined. Change the `apiVersion`
line and you are done.**

Task 9.1 step 1 re-audited every public `v1beta1` field for necessity and
naming consistency before the names became permanent, including the two
sections that landed *after* the Beta freeze as additive changes and were
therefore being reviewed at a version boundary for the first time. The audit's
outcome, in full:

| Field or section | Outcome | Why |
| --- | --- | --- |
| `apiVersion`, `kind` | kept | The document discriminators. `kind: Lab` is unchanged and still the only accepted value. |
| `baseline`, `candidate` | kept | The two sides of every comparison, named for what they are. |
| `baseline.kubernetes`, `.images`, `.components` | kept | `images` was examined again (`preloadImages` says more) and kept again: it is spelled `images` in the resolved model, the run manifest and the report, and renaming only the configuration surface would put two spellings on one concept. |
| `components[].name`, `.recipe`, `.version`, `.install`, `.readiness` | kept | Unchanged in name, type, default and meaning since `v1alpha1`. |
| `install.type: helm` / `manifests` and every key under them | kept | Including `paths` beside `gateway.manifests`: each is qualified by its own enclosing key, and making them identical would require making one worse. |
| `fixtures.include` | kept | |
| `policy.failOn`, `.overrides[]`, `.overrides[].kind` | kept | `kind` here means "regression category" and pairs with `failOn`'s entries, which are the same vocabulary. Unambiguous in position; renaming it would ripple through `admissionlab-policy` for no wire-level gain. |
| `policy.latency.absoluteIncreaseMillis` | kept | The unit-carrying name the Beta freeze introduced. Renaming it again would cost every user a rewrite to buy nothing. |
| `policy.latency.relativeMultiplier` | kept | Dimensionless; there is no unit to name. |
| `expectationsFile` | kept | Singular because it takes one path, beside `valuesFiles` which takes several. Both are correct for their arity. |
| `gateway.*` (`manifests`, `routes[]`, `reconciliationTimeoutMillis`, `gatewayEndpoint`, `readiness`) | kept | Same reasoning as `absoluteIncreaseMillis` for the timeout; the rest were unchanged from `v1alpha1` and reviewed then. |
| `migration.*` (`cases[]`, `baseline`, `candidate`, and every key under them) | kept | **Audited here for the first time at a boundary.** The section landed inside `v1beta1` (Tasks 8.3 and 8.8) as optional additions, which is what the Beta rule allowed. It is reviewed on the same terms as everything above and is now part of the stable contract: `baselineIngressManifests` / `candidateGatewayManifests` say which side each list is applied to, which is the distinction the whole suite turns on; `expectedNonportable` names an input feature rather than a diff finding, which is why it is not `expectations.yaml`'s vocabulary. |
| **Removed** | none | No field was experimental, so none had to go before this task landed. |

**What a user must do:** nothing, to keep running. A `v1beta1` document — and a
`v1alpha1` one — loads unchanged, and resolves to a byte-identical lab. To move
a file forward, change the `apiVersion` line; there is no second edit, which is
the whole content of this step.

**What a `v1beta1` document still means:** exactly what it always did. It is
parsed against the frozen `v1beta1` model and carried to the `v1` model as pure
data — the two models share every nested type, so there is nothing to translate
— and then resolved by the one resolver all three versions share.
`crates/admissionlab-spec/tests/stable_schema.rs` asserts that as an equality
between resolved values, over hand-written twins in `testdata/configs/` and
over every checked-in `v1beta1` fixture with only its `apiVersion` line
rewritten.

**`examples/` moved; `testdata/configs/` did not.** Every example a user copies
from is now `admissionlab.io/v1`, because an example still written in a
superseded version is how a superseded spelling outlives its supersession. The
`v1beta1` and `v1alpha1` read-support proofs live in `testdata/configs/`
instead, which is where a compatibility fixture belongs: nobody copies it into
a repository by accident.

### Result: `v1beta1` → `v1` (Task 9.1)

**The identifier changed and the document did not.** `schemaVersion` is now
`admissionlab.io/result/v1`; every key, type, default and meaning is what
`v1beta1` published, which
`crates/admissionlab-report/tests/stable_schema.rs` asserts by comparing the
two published schemas for equality once documentation is stripped.

**What a consumer must do:** accept the new `schemaVersion` string. A consumer
that pinned an equality check on `admissionlab.io/result/v1beta1` needs one
edit; one that reads the document is unaffected.

**There is no result reader, by design.** A result is emit-only — Admission Lab
writes one and never reads one back — so there is no migration to run and no
"old result still loads" property to preserve. What is preserved instead is the
published schema: [`schemas/result-v1beta1.json`](../schemas/result-v1beta1.json)
stays checked in, with no generator behind it, as the contract the documents
already in artifact directories were written against.

### Run manifest: `v1beta1` → `v1` (Task 9.1)

**The identifier changed, including its shape, and the document did not.**
`schemaVersion` is now `admissionlab.io/run/v1` — note the dropped `-manifest`
infix, explained under [Where each family stands](#where-each-family-stands).
No field was added, removed, renamed, or redefined.

**What a consumer must do:** match on the new string as well as the old ones.
`admissionlab reproduce` already does: it reads `run/v1`,
`run-manifest/v1beta1` and `run-manifest/v1alpha1`, preserves whichever the
document carries, and plans a reproduction from any of them without a migration
step.

**What a `v1beta1` manifest still means:** exactly what it always did. Every
field has the same meaning at `v1`, and the reader keeps the document's own
`schemaVersion` rather than rewriting it — which is what keeps an absent
`gateway` on a `v1alpha1` manifest ("not recorded") distinguishable from an
absent `gateway` on a `v1beta1` or `v1` one ("there was no Gateway suite").

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
- **Superseded schema files are frozen, with no generator behind them** — for
  the result and run-manifest families, whose older shapes no Rust type writes
  any more. A generator can only describe the type that exists now, so a
  "v1beta1 generator" would silently start describing v1 the moment a field was
  added, which is how a frozen schema stops being frozen. The *configuration*
  family is the exception and not an inconsistent one: every supported
  `apiVersion` still has a model this build parses with, so all three
  configuration schemas are generated and compared, and that comparison is what
  keeps the two older models frozen.
- **The compatibility rule itself is a test**, once per family.
  `crates/admissionlab-core/tests/run_manifest_beta.rs`,
  `crates/admissionlab-spec/tests/stable_schema.rs` and
  `crates/admissionlab-report/tests/stable_schema.rs` each compare the
  generated `v1` schema against every frozen predecessor file — at the root and
  in every shared definition — and fail if a property was dropped or renamed,
  if a requirement was dropped, or if an addition is required rather than
  optional.
- **"Nothing changed" is a test too.** Each of the two `stable_schema.rs` files
  strips documentation from the generated `v1` schema and its frozen `v1beta1`
  predecessor and asserts the remainder is *equal*. That is the freeze's actual
  content — the promotion moved an identifier and nothing else — and it is what
  makes the shared Rust types behind the two configuration versions safe rather
  than merely convenient.
- **The semantic-change vocabulary is pinned as literals.**
  `crates/admissionlab-report/tests/stable_schema.rs` lists every
  `SemanticChangeKind` and `MigrationBehaviorKind` wire string and compares the
  list against the published schema, so renaming one fails the build in a file
  whose entire subject is that it must not be renamed.
- **Golden documents are checked in and compared**, so any change to the wire
  form of a document — including one made accidentally, by reordering fields or
  changing a serializer — shows up as a diff in a file a reviewer reads.
- **A top-level key set is frozen by test**, so a new field in a run manifest
  cannot appear without someone deliberately updating that list, at which point
  the "what this document may never contain" rules (Global Constraint 14: no
  paths, no environment, no captured output, no credential material) are in
  front of them.

---

## Where the older versions stand now

`v1alpha1` and `v1beta1` are **superseded, not withdrawn**. Neither carries a
stability promise going forward — that is what `v1` is for — but both remain
readable, and each family's reader is what makes that concrete rather than
aspirational:

| Family | `v1alpha1` | `v1beta1` |
| --- | --- | --- |
| Configuration | loads; migrated forward on load | loads; migrated forward on load |
| Result | never had a checked-in schema | published schema kept, frozen; documents are still valid against it |
| Run manifest | read as-is, version preserved | read as-is, version preserved |

Support for reading an older configuration is not promised forever, and the day
one is dropped it will be a `v2`-sized announcement with its own note here. It
is not being dropped now: a `v1alpha1` file written for Public Alpha still runs
against this release, unedited, which is the property Global Constraint 10's
"schema stability" is actually asking for.
