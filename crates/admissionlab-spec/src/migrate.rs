//! Migrating an older lab document forward to the current
//! `admissionlab.io/v1` model (ROADMAP Task 7.1 Step 3, Task 9.1 Step 3).
//!
//! Two bridges, chained, and no other path between versions:
//!
//! ```text
//! v1alpha1 --migrate_v1alpha1_to_v1beta1--> v1beta1 --migrate_v1beta1_to_v1--> v1
//! ```
//!
//! Both are pure — they perform no I/O, consult no environment, and
//! resolve no path — so they can be tested exhaustively against
//! hand-built values, and so a migration can never depend on the machine
//! it runs on. [`crate::load_any_supported_lab`] calls whichever prefix
//! of the chain a document needs, immediately after parsing it and before
//! [`crate::resolve_lab`] ever sees the result; nothing else in the
//! workspace needs to know that three versions exist.
//!
//! **Chained rather than one function per pair.** There is no
//! `migrate_v1alpha1_to_v1`, and there deliberately never will be: with
//! `n` versions, per-pair migrations are `n^2` functions that can
//! disagree, and the one that gets exercised least is the one carrying an
//! Alpha document — the oldest input and the one with the least margin
//! for a silent mistake. One step per version boundary means an Alpha
//! document travels the *same* code a Beta document does for the second
//! half of its journey.
//!
//! # The stable freeze review (Task 9.1 Step 1)
//!
//! `v1beta1 -> v1` renames nothing and removes nothing. Every public Beta
//! field was walked once more for necessity and naming consistency —
//! including the `migration:` section, which landed after the Beta freeze
//! as an additive change and is therefore reviewed here for the first
//! time — and the audit's outcome is the table in
//! `docs/schema-migrations.md`'s "Configuration: `v1beta1` -> `v1`" note.
//! A rename is only cheap at a version boundary, and this is the last
//! boundary at which it was cheap; the answer was still "keep", because
//! the Beta freeze had already made every one of these calls deliberately
//! and re-litigating them would have cost every existing user a rewrite
//! to buy nothing.
//!
//! # The Beta freeze review: every v1alpha1 field, and what happened to it
//!
//! "Freezing" a schema means looking at every name once, on purpose,
//! while renaming one is still cheap. This is that record for the
//! `v1alpha1 -> v1beta1` boundary — the one that did change two names —
//! and every conclusion in it survived the stable review above unchanged.
//! Two names changed; every other name was examined and deliberately
//! kept.
//!
//! ## Renamed (2)
//!
//! | v1alpha1 | v1beta1 | why |
//! |---|---|---|
//! | `policy.latency.absoluteIncrease` | `policy.latency.absoluteIncreaseMillis` | The value is a bare YAML integer whose unit lived only in Rustdoc. `absoluteIncrease: 50` reads as "50 seconds" to most Kubernetes users; it means 50 milliseconds. The two readings differ by 1000x and neither one fails to parse, so the mistake surfaces as "the latency threshold is never crossed" rather than as an error. Kubernetes' own API puts the unit in the name for exactly this reason (`timeoutSeconds`, `periodSeconds`, `initialDelaySeconds`), and Beta follows it. |
//! | `gateway.reconciliationTimeout` | `gateway.reconciliationTimeoutMillis` | The same bare-integer-milliseconds field, one section over (`reconciliationTimeout: 120000`). Renaming one and not the other would leave the config with two conventions for one idea, which is worse than either convention alone. |
//!
//! Both renames are confined to the *wire* name. The Rust fields stay
//! [`crate::LatencyPolicy::absolute_increase`] and
//! [`crate::GatewaySuiteSpec::reconciliation_timeout`], because their
//! type is [`std::time::Duration`], which already carries the unit — see
//! [`crate::v1beta1::LatencyPolicy`] for that argument in full. Nothing
//! outside this crate changes.
//!
//! ## Reviewed and deliberately kept
//!
//! - **`baseline.images` / `candidate.images`.** `preloadImages` would
//!   say more (the list is side-loaded into the node's image store
//!   before anything is installed). It was rejected because this same
//!   list is called `images` in the resolved model, in the run manifest,
//!   and in the report: renaming only the configuration surface would
//!   put two spellings on one concept across the tool, which costs a
//!   reader more than the terser name does. The ambiguity is bounded and
//!   documented on the field itself.
//! - **`policy.overrides[].kind`.** `kind` is heavily overloaded in a
//!   Kubernetes-shaped document — it is also the root document's own
//!   discriminator — and this one means "regression category". It was
//!   kept because it is unambiguous in position (nested three levels
//!   under `policy`), because it pairs with `policy.failOn`'s entries,
//!   which are the same vocabulary, and because a rename here would
//!   force the same rename through `admissionlab-policy`'s public
//!   `PolicySpec` handling for no wire-level gain.
//! - **`install.type: manifests` + `paths` versus `gateway.manifests`.**
//!   Two names for "a list of manifest files", one field apart. Kept:
//!   `paths` is qualified by its enclosing `type: manifests`, and
//!   `gateway.manifests` is a top-level noun in its own section. Making
//!   them identical would require making one of them worse.
//! - **`expectationsFile` (singular `File`) versus `valuesFiles`
//!   (plural `Files`).** Both are correct for their arity; there is no
//!   inconsistency to fix.
//! - **Everything else** — `apiVersion`, `kind`, `baseline`,
//!   `candidate`, `fixtures.include`, `policy.failOn`,
//!   `policy.latency.relativeMultiplier`, every `install`/`readiness`
//!   key, every `gateway` route and probe key — is unchanged in name,
//!   type, default, and meaning.
//!
//! # Why this migration is total
//!
//! Once the two renames above are applied, **every v1alpha1 field maps
//! onto exactly one v1beta1 field of the same Rust type, with the same
//! meaning and the same default.** There is no Alpha value that Beta
//! cannot represent, and no Alpha value whose Beta representation is a
//! choice rather than a translation. That is not luck; it is what makes
//! this a *promotion* rather than a redesign, and it is the property
//! ROADMAP Task 7.1 Step 3 is really about: an ambiguous migration must
//! be rejected rather than guessed at, and the way to have no guesses is
//! to have no ambiguity.
//!
//! Concretely, three things could have made a field ambiguous, and none
//! of them is present:
//!
//! 1. **A narrowed type** (Beta accepts fewer values than Alpha) — a
//!    migration would then have to reject or clamp some inputs. Beta
//!    narrows nothing; the two renamed fields keep their exact
//!    `u64`-milliseconds domain.
//! 2. **A merged or split field** (two Alpha fields becoming one, or one
//!    becoming two) — a migration would then have to invent a
//!    combination or a division. Nothing was merged or split.
//! 3. **A new *required* field** — a migration would have to default it,
//!    which is precisely the "invent a default" failure Step 3 forbids.
//!    Beta adds no required field. It has since gained one *optional*
//!    section Alpha never had ([`crate::V1Beta1Lab::migration`], ROADMAP
//!    Task 8.3), and that is the addition-only rule the freeze committed
//!    to rather than an exception to it: the field's absence is a
//!    complete configuration in its own right, so an Alpha document
//!    migrates to `None` without anything being guessed. A *required*
//!    addition would still be forbidden, and would still need a new
//!    `apiVersion`.
//!
//! `tests/migrate_alpha_beta.rs` tests this as a property rather than
//! taking it on faith: it migrates a maximal Alpha document in which
//! every optional field is populated and asserts each Beta field equals
//! its Alpha counterpart, and [`migrate_v1alpha1_to_v1beta1`] itself
//! destructures [`crate::V1Alpha1Lab`] exhaustively, so adding a field to
//! the (frozen, so this should never happen) Alpha model is a compile
//! error here rather than a silently dropped value.
//!
//! # Then why does this return a `Result`?
//!
//! Because totality is a statement about *field* migration, and this
//! function's input is a whole document. [`migrate_v1alpha1_to_v1beta1`]
//! is public and takes a [`crate::V1Alpha1Lab`] by value, which a caller
//! may build by hand rather than obtain from
//! [`crate::load_lab`] — and `api_version`/`kind` are plain `String`s in
//! that model (parsing accepts any value; the loader is what checks
//! them). A caller can therefore hand this function a document that is
//! not an Alpha `Lab` at all, and translating one would mean asserting a
//! provenance that was never established. [`MigrationError`] is that
//! precondition, made explicit rather than assumed.
//!
//! This is deliberately *not* an invented failure mode standing in for
//! an ambiguity that does not exist: it is unreachable through
//! [`crate::load_any_supported_lab`], which checks `apiVersion` before
//! choosing a model, and `tests/migrate_alpha_beta.rs` asserts both
//! halves — that a hand-built mismatched document is rejected, and that
//! no document reachable through the loader ever is.

use crate::model::KIND;
use crate::v1::{self, V1Lab};
use crate::v1alpha1::{self, V1Alpha1Lab};
use crate::v1beta1::{self, V1Beta1Lab};

/// The only way a migration in this module can fail: it was handed
/// something that is not a `Lab` document of the version it migrates
/// *from*.
///
/// See this module's "Then why does this return a `Result`?" — every
/// *field* in a genuine document of the source version migrates
/// unambiguously, so there is no per-field variant here and no variant
/// that defaults anything.
#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
pub enum MigrationError {
    /// The document's `apiVersion` is not the one the migration reads, so
    /// there is no basis for reading it as a document of that version.
    #[error(
        "cannot migrate a document whose apiVersion is {found:?}: \
         migration from {expected:?} to {target:?} is defined only for {expected:?} documents"
    )]
    UnexpectedApiVersion {
        /// The `apiVersion` the document actually carried.
        found: String,
        /// The version this migration reads
        /// ([`v1alpha1::API_VERSION`] or [`v1beta1::API_VERSION`]).
        expected: &'static str,
        /// The version this migration produces
        /// ([`v1beta1::API_VERSION`] or [`v1::API_VERSION`]).
        target: &'static str,
    },

    /// The document's `kind` is not [`KIND`]. A `FixtureMatrix` or an
    /// `Expectations` document is a different contract with a different
    /// owner (see [`crate::model::FIXTURE_MATRIX_KIND`]), and neither is
    /// migrated by this function.
    #[error("cannot migrate a document whose kind is {found:?}: expected {expected:?}")]
    UnexpectedKind {
        /// The `kind` the document actually carried.
        found: String,
        /// [`KIND`].
        expected: &'static str,
    },
}

/// Translates a frozen `admissionlab.io/v1alpha1` lab document into the
/// `admissionlab.io/v1beta1` model — the first half of the chain to the
/// current model, whose second half is [`migrate_v1beta1_to_v1`].
///
/// Pure: no I/O, no environment, no path resolution. Every field maps
/// 1:1 (see this module's "Why this migration is total"); the only two
/// differences are wire *names*, which are a property of the two models'
/// `serde` derives rather than of this function, so the translation
/// itself is a field-for-field move.
///
/// # Errors
///
/// Returns [`MigrationError`] if `old` is not an
/// `admissionlab.io/v1alpha1` document whose `kind` is `Lab` — a
/// precondition a hand-built value can violate but
/// [`crate::load_any_supported_lab`] never can. See this module's "Then
/// why does this return a `Result`?".
pub fn migrate_v1alpha1_to_v1beta1(old: V1Alpha1Lab) -> Result<V1Beta1Lab, MigrationError> {
    // Exhaustive destructure, not field access: the whole point of a
    // frozen model is that it cannot gain a field, and this is what
    // turns "it did anyway" into a compile error here instead of a value
    // that migration silently drops.
    let V1Alpha1Lab {
        api_version,
        kind,
        baseline,
        candidate,
        fixtures,
        policy,
        expectations_file,
        gateway,
    } = old;

    if api_version != v1alpha1::API_VERSION {
        return Err(MigrationError::UnexpectedApiVersion {
            found: api_version,
            expected: v1alpha1::API_VERSION,
            target: v1beta1::API_VERSION,
        });
    }
    if kind != KIND {
        return Err(MigrationError::UnexpectedKind {
            found: kind,
            expected: KIND,
        });
    }

    Ok(V1Beta1Lab {
        // The one value migration *changes*: the document now declares
        // the version whose model it has become. Everything below is
        // carried across unchanged.
        api_version: v1beta1::API_VERSION.to_owned(),
        kind,
        // `baseline`, `candidate`, and `fixtures` are literally the same
        // types in both versions (they live in `crate::model`, which is
        // where a type stays for exactly as long as every supported
        // version spells it identically), so there is nothing to
        // translate — moving the value *is* the migration.
        baseline,
        candidate,
        fixtures,
        policy: migrate_policy(policy),
        expectations_file,
        gateway: gateway.map(migrate_gateway),
        // v1alpha1 has no `migration:` section at all (ROADMAP Task 8.3
        // is v1.0 work, and Alpha was frozen well before it), so there
        // is no Alpha value to translate and `None` is not a default
        // being invented -- it is the *only* thing an Alpha document can
        // have meant. See this module's "Why this migration is total",
        // clause 3: what Step 3 forbids is defaulting a new *required*
        // field, and this one is optional in Beta precisely because
        // omitting it is a complete, meaningful configuration.
        migration: None,
    })
}

/// Translates a `admissionlab.io/v1beta1` lab document into the stable
/// `admissionlab.io/v1` model (ROADMAP Task 9.1).
///
/// Pure, and totally trivial by design: the stable freeze renamed
/// nothing and removed nothing, and [`V1Lab`] shares every nested type
/// with [`V1Beta1Lab`] rather than declaring copies of them (see
/// [`crate::v1`]'s "Zero wire changes from `v1beta1`"), so the only value
/// that changes is the `apiVersion` the document now declares.
///
/// That triviality is the *result* being asserted, not a shortcut taken
/// on the way to it. A migration with nothing to do is what "we audited
/// every field and kept every one" looks like when it is written down as
/// code, and the exhaustive destructure below is what keeps it honest: a
/// field added to either model without an answer here is a compile error.
///
/// # Errors
///
/// Returns [`MigrationError`] if `old` is not an
/// `admissionlab.io/v1beta1` document whose `kind` is `Lab` — the same
/// caller precondition [`migrate_v1alpha1_to_v1beta1`] has, for the same
/// reason (see this module's "Then why does this return a `Result`?"),
/// and equally unreachable through [`crate::load_any_supported_lab`].
pub fn migrate_v1beta1_to_v1(old: V1Beta1Lab) -> Result<V1Lab, MigrationError> {
    let V1Beta1Lab {
        api_version,
        kind,
        baseline,
        candidate,
        fixtures,
        policy,
        expectations_file,
        gateway,
        migration,
    } = old;

    if api_version != v1beta1::API_VERSION {
        return Err(MigrationError::UnexpectedApiVersion {
            found: api_version,
            expected: v1beta1::API_VERSION,
            target: v1::API_VERSION,
        });
    }
    if kind != KIND {
        return Err(MigrationError::UnexpectedKind {
            found: kind,
            expected: KIND,
        });
    }

    Ok(V1Lab {
        // The one value migration changes. Everything below is the same
        // Rust type in both models, so moving the value *is* the
        // migration — there is nothing left to translate.
        api_version: v1::API_VERSION.to_owned(),
        kind,
        baseline,
        candidate,
        fixtures,
        policy,
        expectations_file,
        gateway,
        migration,
    })
}

/// Carries a v1alpha1 `policy` section across to v1beta1.
///
/// Structurally identical; the two types differ only in the `serde` name
/// of one leaf field (`absoluteIncrease` -> `absoluteIncreaseMillis`),
/// which never appears in Rust. Destructured exhaustively for the reason
/// [`migrate_v1alpha1_to_v1beta1`] destructures its own input.
fn migrate_policy(old: v1alpha1::PolicySpec) -> v1beta1::PolicySpec {
    let v1alpha1::PolicySpec {
        fail_on,
        overrides,
        latency,
    } = old;
    let v1alpha1::LatencyPolicy {
        absolute_increase,
        relative_multiplier,
    } = latency;
    v1beta1::PolicySpec {
        fail_on,
        overrides,
        latency: v1beta1::LatencyPolicy {
            // Same `Duration`, same milliseconds. Only the YAML key a
            // user types changed.
            absolute_increase,
            relative_multiplier,
        },
    }
}

/// Carries a v1alpha1 `gateway` section across to v1beta1.
///
/// As with [`migrate_policy`]: identical structure, one renamed wire key
/// (`reconciliationTimeout` -> `reconciliationTimeoutMillis`), and an
/// exhaustive destructure so a field cannot be forgotten.
///
/// Note what is *not* here: `manifests` is carried across exactly as the
/// user wrote it, still unresolved. Migration is a wire-format
/// translation and path resolution is [`crate::resolve_lab`]'s job, in
/// that order — a migration that resolved paths would need to know which
/// file the document came from, which is exactly the impurity this
/// module refuses.
fn migrate_gateway(old: v1alpha1::GatewaySuiteSpec) -> v1beta1::GatewaySuiteSpec {
    let v1alpha1::GatewaySuiteSpec {
        manifests,
        routes,
        reconciliation_timeout,
        gateway_endpoint,
        readiness,
    } = old;
    v1beta1::GatewaySuiteSpec {
        manifests,
        routes,
        reconciliation_timeout,
        gateway_endpoint,
        readiness,
    }
}
