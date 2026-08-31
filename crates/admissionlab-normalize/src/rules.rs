//! The normalization vocabulary: what a rule can say, how rules are
//! layered into a profile, and which rules Admission Lab itself applies
//! by default (Task 4.1).
//!
//! # Three rule kinds, all mechanical
//!
//! [`NormalizeRule`] has exactly the three variants Task 4.1 freezes,
//! and every one of them is a pure, mechanical transformation of a
//! `serde_json::Value` — remove a value, remove an annotation, reorder
//! an array. None of them expresses a *judgment* about whether a
//! difference matters. That separation is Global Constraint 6 and
//! PRODUCT.md §14 in code: deciding that a difference is expected is
//! `admissionlab-diff`/`admissionlab-policy`'s job, and a rule that
//! could say "treat these as equivalent" would move classification into
//! normalization (and, via recipes, into vendor-supplied data). See
//! `admissionlab-recipes/src/model.rs`'s own module documentation, which
//! enforces the same closed set from the YAML side.
//!
//! # Relationship to `admissionlab_spec::RecipeNormalizeRule`
//!
//! `admissionlab_spec::RecipeNormalizeRule` is the *configuration*
//! vocabulary (Controller Ruling R30 puts it in `admissionlab-spec`, and
//! `admissionlab-recipes` parses recipe YAML into it).
//! [`NormalizeRule`] is the *engine* vocabulary. They have the same three
//! variants today, which is exactly why this crate does **not** depend on
//! `admissionlab-spec` or `admissionlab-recipes` to reuse the type: the
//! normalization engine must stay usable without any configuration crate
//! in the graph, and the two types are free to diverge (a later
//! engine-only rule kind must not automatically become recipe-authorable
//! surface, which would be a Global Constraint 6 hole opened by
//! accident).
//!
//! The conversion between them is therefore a genuine seam, and it
//! deliberately lives in neither crate yet. Whichever later task first
//! assembles a [`NormalizationProfile`] from a resolved lab file — Task
//! 4.6/4.7's comparison wiring is the current candidate — owns it, and
//! should place it in the crate that already depends on both (the
//! assembler), so that neither `admissionlab-normalize` nor
//! `admissionlab-recipes` grows an edge to the other for a three-arm
//! `match`.

use std::fmt;

/// One mechanical normalization step.
///
/// Every variant's pointer is RFC 6901 (see [`crate::pointer`]).
/// A pointer that does not match the object being normalized is an
/// ordinary no-op — profiles are written once and applied to every kind
/// of object a fixture corpus contains, so most rules miss most objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeRule {
    /// Remove the value at a JSON Pointer.
    ///
    /// Literal and unembellished: it removes exactly what the pointer
    /// addresses and does nothing else. In particular it does **not**
    /// clean up a container that it empties — contrast
    /// [`NormalizeRule::RemoveAnnotation`], which does, and see
    /// `object.rs` for why that asymmetry is deliberate.
    RemovePointer(String),
    /// Stably sort the array of objects at `pointer` by each element's
    /// `key`.
    ///
    /// Safe only where element order carries no meaning. The built-in
    /// profile ([`built_in_rules`]) applies this to a hand-audited list
    /// and nothing else; a recipe or user rule may point it anywhere,
    /// and owns the consequences of doing so. See [`built_in_rules`] for
    /// what "no meaning" was judged against, and `object.rs`'s
    /// `sort_named_array` for how elements without a usable `key` are
    /// ordered.
    SortNamedArray {
        /// RFC 6901 pointer to the array.
        pointer: String,
        /// The object key whose (string) value each element is sorted
        /// by.
        key: String,
    },
    /// Remove one annotation from the object's own
    /// `metadata.annotations` map, by its literal key.
    ///
    /// The key is given unescaped — `RemoveAnnotation` builds the RFC
    /// 6901 pointer itself, so a key containing `/` or `~` needs no
    /// hand-escaping by the profile author. Top-level metadata only: a
    /// pod-template's annotations inside a `Deployment` are addressed by
    /// an explicit [`NormalizeRule::RemovePointer`]. See `object.rs` for
    /// why this rule is not recursive.
    RemoveAnnotation(String),
}

/// Which layer of a [`NormalizationProfile`] a rule came from.
///
/// Carried into `NormalizationEvidence` so a Phase 4 diff explanation
/// can say *whose* rule suppressed a difference — "a built-in rule
/// removed `metadata.uid`" and "your own rule removed `/spec`" are very
/// different things to tell a user, and the second is the one worth
/// warning about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuleTier {
    /// Admission Lab's own default rules ([`built_in_rules`]).
    BuiltIn,
    /// Rules contributed by a recipe.
    Recipe,
    /// Rules the user wrote in their own lab configuration.
    User,
}

impl RuleTier {
    /// The tier's stable text form, as it appears in evidence strings
    /// and error messages. Pinned here rather than derived from the Rust
    /// identifier so that renaming a variant cannot silently change
    /// checked-in golden files.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RuleTier::BuiltIn => "built_in",
            RuleTier::Recipe => "recipe",
            RuleTier::User => "user",
        }
    }
}

impl fmt::Display for RuleTier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The three layers of normalization rules that apply to one comparison,
/// in the order they are applied.
///
/// `built_in` runs first, then `recipe`, then `user`. That order is part
/// of the contract, not an implementation detail: a user rule is the
/// last word, so it can always reach and reorder something a recipe rule
/// produced, and never the other way round. Within a tier, rules are
/// applied in `Vec` order. Nothing else about the profile influences the
/// result — there is no set, map, or hash iteration anywhere in
/// `normalize_object`, so the same input and the same profile always
/// produce byte-identical output.
///
/// # No `Default`
///
/// Deliberately absent. `NormalizationProfile::default()` would have to
/// silently pick between "no rules at all" and "Admission Lab's built-in
/// rules", and both readings are plausible enough that a caller would
/// eventually get the one they did not mean — an empty profile would
/// quietly leave `metadata.uid` in a comparison, and a built-in one
/// would quietly strip fields a caller wanted to see. [`built_in`] and
/// [`empty`] name the choice at the call site instead.
///
/// [`built_in`]: NormalizationProfile::built_in
/// [`empty`]: NormalizationProfile::empty
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizationProfile {
    /// Admission Lab's own rules. [`NormalizationProfile::built_in`]
    /// fills this with [`built_in_rules`].
    pub built_in: Vec<NormalizeRule>,
    /// Rules contributed by the recipes backing the stack under test.
    pub recipe: Vec<NormalizeRule>,
    /// Rules the user wrote themselves.
    pub user: Vec<NormalizeRule>,
}

impl NormalizationProfile {
    /// A profile with Admission Lab's [`built_in_rules`] and no recipe
    /// or user rules.
    #[must_use]
    pub fn built_in() -> Self {
        Self {
            built_in: built_in_rules(),
            recipe: Vec::new(),
            user: Vec::new(),
        }
    }

    /// A profile with no rules at all: `normalize_object` under it
    /// returns the input value unchanged with empty evidence.
    ///
    /// Useful for tests that isolate a single rule, and for a caller
    /// that genuinely wants raw comparison.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            built_in: Vec::new(),
            recipe: Vec::new(),
            user: Vec::new(),
        }
    }
}

/// Admission Lab's built-in normalization rules.
///
/// # What is removed, and why each entry earns its place
///
/// Every entry below is a field the **API server** populates, whose value
/// is either genuinely nondeterministic between two runs or a redundant
/// echo of data compared elsewhere. Nothing here is a field a webhook
/// under test could set, which is the line this list is drawn along:
/// stripping something a webhook writes would delete the exact behavior
/// this product exists to observe.
///
/// - `metadata.uid` — a fresh UUID per object, per cluster. Baseline and
///   candidate run on two separate ephemeral clusters (Global Constraint
///   4), so these can never match and their difference means nothing.
/// - `metadata.resourceVersion` — an opaque, cluster-local storage
///   token. Kubernetes documents it as meaningless to compare across
///   clusters, and even a server-side dry-run `CREATE` gets one.
/// - `metadata.creationTimestamp` — wall-clock time of the request.
/// - `metadata.managedFields` — server-side-apply bookkeeping that
///   records which field manager touched which field and *when*
///   (`time:` entries again put wall clock into the object). It is
///   derived from the object's own contents, so a real change shows up
///   in the field it happened to, and again in this block — the same
///   double-reporting argument as the annotation below.
/// - The `kubectl.kubernetes.io/last-applied-configuration` annotation —
///   a verbatim JSON copy of a previously applied object. It is not
///   nondeterministic, but it makes every real spec difference show up
///   twice: once in the field that changed and once inside this
///   annotation's embedded blob, where it is unreadable. Removing it is
///   pure signal-to-noise, and it is the one built-in rule that
///   exercises RFC 6901 key escaping (the key contains a `/`).
///
/// # Admission Lab's own metadata: there is none to strip
///
/// Task 4.1's brief asks for "the Admission Lab correlation/test-only
/// metadata when applicable". Read against this repository, it is not
/// applicable, and that is worth stating rather than quietly omitting:
///
/// - **The capture pipeline stamps nothing.**
///   `admissionlab_fixtures::execute::dry_run_create` sends the fixture
///   object byte-for-byte, with no correlation label, annotation, or
///   injected field of any kind — see that module's own
///   "The fixture object is sent byte-for-byte, never annotated"
///   section, and `admissionlab-admission/tests/execute_unit.rs`, which
///   asserts it against the body a mock transport actually received.
///   Correlation is done through serial execution plus audit-log fields
///   (Global Constraint 17, Task 3.7), precisely so that nothing has to
///   be written onto the object under test. So there is no lab-generated
///   field on a captured object for a rule here to remove.
/// - **`admissionlab.dev/mutated` is behavior, not noise.** It is the
///   label `recipes/test-webhook`'s mutating webhook *adds*
///   (`fixtures/core/admission/pod-add-label.yaml` exists to produce
///   exactly that patch). A rule removing it would delete the mutation
///   this project is trying to detect. The same goes for
///   `admissionlab.dev/reinvoked`.
/// - **`test.admissionlab.io/*` on a fixture is input, not noise.**
///   Those labels/annotations are what a fixture author writes to make
///   the test webhook act, and the webhook's `objectSelector` matches on
///   them. They are identical on both sides by construction, so they
///   contribute no difference to remove — and if a stack under test ever
///   *did* rewrite one, that would be a real regression this list must
///   not hide.
/// - **`admissionlab.dev/test-webhook` lives on the namespace**
///   (`fixtures/core/admission/00-namespace.yaml`), not on any fixture
///   object, and it is the `namespaceSelector` opt-in that makes the
///   webhooks run at all. Nothing to strip on the object, and a very bad
///   thing to strip if a namespace ever were normalized.
///
/// # A known gap this list cannot close: the `kube-api-access-*` volume
///
/// The in-tree `ServiceAccount` admission plugin mounts a projected
/// token volume into every pod, and names it `kube-api-access-` plus a
/// **random five-character suffix**. That name is genuinely
/// nondeterministic between two runs, it appears twice (in
/// `spec.volumes` and in each container's `volumeMounts`), and no rule
/// expressible with the three frozen [`NormalizeRule`] variants can
/// normalize it: removing it would need a pointer that matches a
/// prefix, and there is no such thing in RFC 6901. It is therefore left
/// in, visibly, rather than approximated — `testdata/objects/normalization/`
/// keeps a real one in its golden input for exactly that reason. Closing
/// it needs a new rule kind (a pattern-matched removal, or a rename
/// canonicalization), which is a deliberate design decision for a later
/// task and not something to smuggle in here.
///
/// # `metadata.generation` is deliberately absent
///
/// Task 4.1 forbids removing it globally, and the reason holds here:
/// `generation` is bumped by the API server when an object's *spec*
/// changes, so on a mutating-admission path it is a real, deterministic
/// signal about whether the spec was rewritten. A domain-specific
/// comparison may later prove it irrelevant for some resource; that is a
/// recipe or user rule, not a default.
///
/// # What is sorted, and the two lists that are deliberately not
///
/// - `/spec/containers` by `name` — a pod's containers all start
///   together; the array is a set with a stable key. A webhook that
///   prepends a sidecar and one that appends the same sidecar produce
///   the same running pod, and without this rule they would produce a
///   whole-array diff.
/// - `/spec/volumes` by `name` — same argument, and volumes are
///   referenced by name from `volumeMounts`, never by index.
///
/// Not sorted by default, and each for a specific reason:
///
/// - **`/spec/initContainers`.** Init containers run *sequentially, in
///   array order*, and a native sidecar is an init container with
///   `restartPolicy: Always`, so its position decides what has started
///   before the main containers do. Order is behavior here, not
///   presentation, and Task 4.1 Step 2's governing rule is "semantically
///   safe" sorting — which this fails, notwithstanding its appearance in
///   that step's list of examples. The corpus makes the stakes concrete:
///   `fixtures/core/admission/pod-add-init-container.yaml` and
///   `pod-remove-init-container.yaml` exist to observe init-container
///   changes, and a default that reordered them would blunt exactly
///   those fixtures. A recipe or user that has established the ordering
///   is irrelevant for their stack can still say so explicitly — see
///   below.
/// - **`env` entries.** Semantically these are a name-keyed set worth
///   sorting, but `env` lives inside *each* container
///   (`/spec/containers/0/env`), and RFC 6901 has no wildcard token. The
///   only default expressible with the frozen
///   [`NormalizeRule::SortNamedArray`] shape is a hard-coded list of
///   container indices — which is index-dependent, unbounded, and would
///   be evaluated against an array the `containers` rule above has just
///   reordered. So there is no honest built-in form of it; an explicit
///   `/spec/containers/0/env` rule from a user still works. A later
///   task that wants this properly needs a new, deliberately designed
///   rule kind, not a wildcard bolted onto an RFC 6901 pointer.
/// - **`command`, `args`, and arbitrary arrays.** Order is meaning.
///   Task 4.1 forbids it, and it stays forbidden by simply never
///   appearing in this list.
///
/// # A recipe or user may sort anything, and owns that choice
///
/// [`NormalizeRule::SortNamedArray`] is mechanically total: pointed at
/// `/spec/initContainers`, `/spec/containers/0/env`, or any other array
/// of objects, it sorts it. The safety judgment above governs what
/// Admission Lab does *by default*; it is not a lock. A recipe author
/// who knows their stack emits containers in a nondeterministic order,
/// or a user who has decided init-container ordering is irrelevant for
/// their comparison, can say so — and the resulting suppression is
/// recorded in `NormalizationEvidence::applied_rules` with its tier, so
/// a report can always attribute it back to them.
#[must_use]
pub fn built_in_rules() -> Vec<NormalizeRule> {
    vec![
        NormalizeRule::RemovePointer("/metadata/uid".to_owned()),
        NormalizeRule::RemovePointer("/metadata/resourceVersion".to_owned()),
        NormalizeRule::RemovePointer("/metadata/creationTimestamp".to_owned()),
        NormalizeRule::RemovePointer("/metadata/managedFields".to_owned()),
        NormalizeRule::RemoveAnnotation(
            "kubectl.kubernetes.io/last-applied-configuration".to_owned(),
        ),
        NormalizeRule::SortNamedArray {
            pointer: "/spec/containers".to_owned(),
            key: "name".to_owned(),
        },
        NormalizeRule::SortNamedArray {
            pointer: "/spec/volumes".to_owned(),
            key: "name".to_owned(),
        },
    ]
}
