//! The Ingress-to-Gateway migration suite's configuration surface
//! (ROADMAP Task 8.3): which `Ingress` manifests define today's
//! behavior, which Gateway API manifests are meant to replace them, and
//! what has to keep answering the same way through both.
//!
//! # Admission Lab converts nothing, and that is the design
//!
//! **v1 contains no Ingress-to-Gateway converter.** Both halves of every
//! [`MigrationCaseSpec`] are written by a person, or produced by some
//! other tool (`ingress2gateway`, a vendor's migration guide, a hand
//! edit), and this suite exists to check the conversion *that user has
//! already performed*.
//!
//! This is worth saying loudly because the opposite is the obvious first
//! guess, and it is unsound. A suite that generated its own candidate
//! manifests would compare a converter against itself: every rule the
//! converter got wrong would be applied identically on both sides, and
//! the run would report "no behavior change" for exactly the mistakes it
//! was built to catch. The pairing is explicit for the same reason
//! [`admissionlab_spec::RouteContract`] names its `Gateway` explicitly
//! -- a contract that reads its expectation out of the artifact under
//! test can never contradict that artifact.
//!
//! # Where these types are defined, and why not here
//!
//! Task 8.3's file list names this module as [`MigrationSuiteSpec`]'s
//! home, while §1.2's registry independently freezes
//! [`admissionlab_spec::ResolvedLab::migration`] as an
//! `Option<MigrationSuiteSpec>`. Exactly the situation
//! [`crate::model`] already resolved for
//! [`crate::model::GatewaySuiteSpec`], with the
//! same forced answer: this crate depends (through
//! `admissionlab-core`) on `admissionlab-spec`, so a `spec -> gateway`
//! edge would be a dependency cycle Cargo rejects. The hand-written
//! configuration types are defined in [`admissionlab_spec::v1beta1`],
//! and this module **re-exports those exact types** rather than
//! declaring parallel ones -- §1.2's "these names are canonical" rule
//! forbids a synonym.
//!
//! [`NonPortableFeatureExpectation`] is a registry-frozen name that Task
//! 8.5 will also read, and it is defined on the same side for the same
//! reason: it is nested inside a [`MigrationCaseSpec`], which is nested
//! inside the resolved lab. `MigrationBehaviorChange` -- the *observed*
//! half of the vocabulary -- is Task 8.5's, and belongs in this crate
//! when that task lands, next to everything else Admission Lab observed
//! rather than read.
//!
//! # What is validated, and where
//!
//! Every Task 8.3 rule -- a non-empty case list, unique non-empty case
//! ids, a non-empty manifest list on *both* sides of each pairing,
//! probes that are valid by the same rules a Gateway suite's probes are
//! (`admissionlab_spec::validate`'s one shared probe validator), at
//! least one probe per case, and non-portability entries with a feature
//! name, a human reason, and no duplicate feature -- is enforced by
//! [`admissionlab_spec::load_any_supported_lab`] at configuration-load
//! time, before any cluster is created. Same placement, same reasoning
//! as [`crate::model`]: the rules are about the document, the document
//! is parsed there, and a second pass here could only ever disagree with
//! the first. `tests/migration_model.rs` drives them through this
//! module's re-exports, so the surface Tasks 8.4-8.5 consume is the one
//! under test.
//!
//! # `migration:` is a v1beta1-only section
//!
//! `admissionlab.io/v1alpha1` has no `migration:` key, so an Alpha
//! document always resolves to `migration: None`. See
//! [`admissionlab_spec::V1Beta1Lab::migration`] for why that is a
//! translation rather than an invented default.

use std::collections::BTreeSet;

pub use admissionlab_spec::{MigrationCaseSpec, MigrationSuiteSpec, NonPortableFeatureExpectation};

/// The set of feature names a [`MigrationCaseSpec`] declares
/// non-portable.
///
/// A free function rather than an inherent method for the reason
/// [`crate::model::contract_gateway_identity`] gives: the type is
/// defined in `admissionlab-spec` and Rust does not allow inherent impls
/// on a foreign type.
///
/// A [`BTreeSet`] of borrowed names, which is the shape the question is
/// actually asked in: Task 8.5 turns each observed behavioral difference
/// into a `MigrationBehaviorChange` and has to decide whether it was
/// *expected*, which is one membership test per difference rather than a
/// linear scan per difference. Building the set is safe because
/// `admissionlab_spec::load_any_supported_lab` has already rejected a
/// duplicated feature -- so the set's size always equals
/// [`MigrationCaseSpec::expected_nonportable`]'s length, and nothing is
/// silently collapsed here.
///
/// Deliberately does *not* return the reasons: this answers "was this
/// declared?", and a caller that needs to show a user *why* reads the
/// entry itself, where the reason sits next to the feature it explains.
#[must_use]
pub fn expected_nonportable_features(case: &MigrationCaseSpec) -> BTreeSet<&str> {
    case.expected_nonportable
        .iter()
        .map(|expectation| expectation.feature.as_str())
        .collect()
}
