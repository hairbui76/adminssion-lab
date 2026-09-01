//! Reading a [`RunManifest`] back and re-deriving the run it describes
//! (ROADMAP Task 5.3; PRODUCT.md §28).
//!
//! `admissionlab test` writes a run manifest; `admissionlab reproduce`
//! reads one and runs the *same* lab again — same source files, same
//! Kubernetes versions, same node images, same component versions. This
//! module owns the mechanics of that: locating and re-resolving the
//! configuration, checking the current source against the digests the
//! manifest recorded, and turning the manifest's recorded environments
//! into pins the run applies instead of resolving them afresh.
//!
//! # The one rule this module exists to enforce
//!
//! **Never silently fall forward to a newer dependency** (ROADMAP Task
//! 5.3 step 4). A reproduction that quietly picked up today's `kindest/node`
//! digest, or today's chart release, would produce a run that *looks*
//! like the recorded one and is not — which is worse than no reproduction
//! at all, because it would be believed. Every substitution this module
//! is capable of making is therefore either impossible (a digest-pinned
//! node image is passed through verbatim) or loud (a [`Diagnostic`] is
//! emitted naming exactly what could not be pinned and why).
//!
//! # Where verification is split, and why it is split there
//!
//! A manifest records five digests. Two of them are digests of *files*
//! ([`RunManifest::config_sha256`], [`RunManifest::expectations_sha256`]);
//! three are not reachable from this crate at all:
//!
//! - `fixture_hashes` are computed by `admissionlab-fixtures`, and
//! - `normalization_sha256` comes from `admissionlab-normalize`'s profile,
//!
//! both of which sit *above* `admissionlab-core`
//! (`normalize -> admission -> fixtures -> core`), so naming either here
//! would close the dependency cycle `crate::run`'s module documentation
//! describes at length. `policy_sha256` *is* computable here
//! ([`crate::policy_sha256`]), but it is produced alongside the other
//! four by one function in `admissionlab-cli`
//! (`pipeline::provenance::input_digests`) — the single place in the
//! product where a run's input digests are computed.
//!
//! The split follows from that, and it is deliberately *not* "core
//! recomputes what it can and the CLI recomputes the rest":
//!
//! - [`plan_reproduction`] verifies the two file digests, because it has
//!   to read those two files anyway to load the lab, and because it
//!   hashes them through [`crate::file_sha256`] — the *same* function
//!   `input_digests` calls for the same two files. There is one
//!   implementation of "the SHA-256 of a file's bytes" in this workspace
//!   and both the writer and the verifier go through it.
//! - [`verify_fixtures`] and [`verify_effective_digests`] **hash
//!   nothing**. They are pure comparisons over digests the caller
//!   already computed, so the CLI hands them `input_digests`' own output
//!   rather than a second derivation of it. A digest this module cannot
//!   produce is a digest this module does not try to produce.
//!
//! [`ReproducePlan::verified_inputs`] is therefore *incomplete* when
//! [`plan_reproduction`] returns: it holds the file-backed inputs, and
//! the caller extends it with the fixture entries before deciding
//! whether the reproduction may proceed. That is stated on the function
//! itself, because a caller that forgot would silently reproduce against
//! tampered fixtures.
//!
//! # Plan-time refusal versus run-time unavailability
//!
//! ROADMAP Task 5.3 steps 1 and 3 are two different failures and this
//! module keeps them apart:
//!
//! - **Plan time** — before anything is provisioned. The configuration is
//!   missing or no longer parses; a source file's digest no longer matches
//!   the recorded run; the set of components changed. Every one of these
//!   is knowable in milliseconds from files on disk, is the user's own
//!   input, and is a [`ReproduceError`] (which `admissionlab-cli` maps to
//!   exit `2`, the invalid-input class — a manifest and a source tree that
//!   disagree are not a valid input pair for reproduction).
//! - **Run time** — only a real registry can answer it. A recorded node
//!   image digest that is no longer pullable fails at `kind create`; a
//!   chart version yanked from its repository fails at `helm install`.
//!   Nothing here can detect either without network access it deliberately
//!   does not take, so this module's contribution is to make the failure
//!   *legible*: [`ReproductionPin::pinned_summary`] renders exactly which
//!   recorded artifacts the run is about to demand, so a run-time failure
//!   names the pinned thing rather than leaving a user to guess whether
//!   the reproduction or the registry moved.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use admissionlab_spec::{InstallMethod, ResolvedLab, SpecError, load_any_supported_lab};
use thiserror::Error;

use crate::diagnostic::{Diagnostic, RedactedValue};
use crate::ids::FixtureId;
use crate::run::ResolvedNodeImages;
use crate::run_manifest::{
    EnvironmentProvenance, RunManifest, RunStatus, SUPPORTED_SCHEMA_VERSIONS, file_sha256,
};
use crate::side::Side;

/// The configuration file name [`plan_reproduction`] looks for inside a
/// `--source-root`.
///
/// A run manifest **cannot** record where its configuration lived:
/// `admissionlab_core::run_manifest`'s module documentation makes it a
/// structural guarantee that no type in that module holds a
/// [`PathBuf`], precisely so a document users attach to public bug
/// reports cannot leak a filesystem layout. That guarantee is worth more
/// than the convenience it costs here, so reproduction is told where the
/// source is instead of remembering — and this is the conventional name
/// it assumes when told only a directory. Every example and every test
/// in this repository uses it; a lab file named anything else is reached
/// through [`plan_reproduction_from_config`].
pub const DEFAULT_LAB_FILE_NAME: &str = "admissionlab.yaml";

/// The version literal `admissionlab-installer` records for a Helm
/// component whose installed chart version it could **not** confirm.
///
/// Duplicated rather than imported: `admissionlab-installer` sits above
/// this crate, so naming it here would close a dependency cycle — the
/// same situation, and the same resolution, as [`crate::run`]'s
/// `SHORT_RUN_ID_LEN` (which duplicates a constant from
/// `admissionlab-cluster` for the same reason and documents the same
/// hand-maintenance). `admissionlab-cli`'s `tests/reproduce_command.rs`
/// asserts the two are equal, which is what keeps the duplication from
/// drifting silently.
///
/// [`ReproductionPin::apply`] treats a component version equal to this
/// as **no recorded pin at all** rather than as a version to install.
/// See that method's documentation for why that is the honest reading
/// and not a fall-forward.
pub const UNCONFIRMED_COMPONENT_VERSION: &str = "unknown";

/// One source file checked against the digest a run manifest recorded
/// for it (ROADMAP §1.2's type registry).
///
/// Holds both digests rather than a boolean, because the failure message
/// a user needs is "expected *this*, found *that*, for *this file*" —
/// a `bool` would force every caller to re-read the manifest to say
/// anything useful.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedInput {
    /// The file that was hashed, exactly as the reproduction resolved it
    /// (never canonicalized — the same convention
    /// `admissionlab_spec::resolve` documents for every other path).
    pub path: PathBuf,
    /// The digest the run manifest recorded for this input.
    pub expected_sha256: String,
    /// The digest this file has now.
    pub actual_sha256: String,
}

impl VerifiedInput {
    /// Whether this input still has the content the recorded run used.
    #[must_use]
    pub fn matches(&self) -> bool {
        self.expected_sha256 == self.actual_sha256
    }
}

/// A digest a manifest records that belongs to no single file.
///
/// [`RunManifest::normalization_sha256`] and
/// [`RunManifest::policy_sha256`] are digests of *effective values* — the
/// normalization profile a run actually applied, the regression policy it
/// actually evaluated — assembled from the configuration, the recipes,
/// and Admission Lab's own built-in rules. There is no path to report for
/// either, so they get their own type rather than a [`VerifiedInput`]
/// with a fabricated one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveMismatch {
    /// Which effective value disagreed: `"normalization"` or `"policy"`.
    pub what: &'static str,
    /// The digest the run manifest recorded.
    pub expected_sha256: String,
    /// The digest the current source produces.
    pub actual_sha256: String,
}

/// Everything [`plan_reproduction`] established before anything is
/// provisioned (ROADMAP Task 5.3's frozen interface).
///
/// `verified_inputs` is deliberately **not complete** when this is
/// returned — see this module's "Where verification is split" section and
/// [`plan_reproduction`]'s own documentation. Both fields are public so
/// the caller that owns the rest of the verification can extend the list
/// in place rather than carrying two.
#[derive(Debug, Clone)]
pub struct ReproducePlan {
    /// The lab configuration, re-loaded and re-resolved from the source
    /// tree. Not yet pinned: [`ReproductionPin::apply`] is what imposes
    /// the recorded versions on it.
    pub resolved_lab: ResolvedLab,
    /// Every file-backed input checked so far, in the order it was
    /// checked: the configuration first, then the expectations file when
    /// the lab declares one.
    pub verified_inputs: Vec<VerifiedInput>,
}

impl ReproducePlan {
    /// Every input whose content no longer matches the recorded run.
    ///
    /// Returned as an iterator over *all* of them rather than the first,
    /// because ROADMAP Task 5.3 step 1 requires the failure to list every
    /// mismatched path with its expected and actual digest: a user who
    /// changed three fixtures should learn that in one run, not three.
    pub fn mismatches(&self) -> impl Iterator<Item = &VerifiedInput> {
        self.verified_inputs.iter().filter(|input| !input.matches())
    }
}

/// Why a reproduction could not even be planned.
///
/// Every variant is a *plan-time* failure — see this module's "Plan-time
/// refusal versus run-time unavailability" section. None of them can
/// occur after a cluster exists, because none of them needs one to be
/// detected.
#[derive(Debug, Error)]
pub enum ReproduceError {
    /// The document is not a run manifest this build understands.
    ///
    /// A manifest carrying an unknown schema is refused rather than read
    /// hopefully: reproducing from a document whose field meanings may
    /// have changed is exactly the silent wrongness this command exists
    /// to avoid. Which versions *are* understood is
    /// [`SUPPORTED_SCHEMA_VERSIONS`] — as of ROADMAP Task 7.3 that is
    /// both `v1beta1` and `v1alpha1`, because a manifest records
    /// something that already happened and a run recorded before the
    /// promotion is exactly as reproducible as one recorded after it.
    ///
    /// Reached only by a [`RunManifest`] built in memory: a manifest read
    /// from bytes has already been through
    /// [`crate::read_run_manifest`], which rejects an unsupported version
    /// with its own richer error. Kept anyway, because
    /// [`plan_reproduction`] takes a value rather than bytes and must not
    /// depend on its caller having checked.
    #[error(
        "run manifest declares schemaVersion {found:?}, but this build of Admission Lab \
         reproduces {} manifests only",
        supported.join(", ")
    )]
    UnsupportedSchema {
        /// The `schemaVersion` the document carries.
        found: String,
        /// Every schema this build reads, newest first.
        supported: &'static [&'static str],
    },
    /// A file the reproduction had to read could not be read.
    #[error("failed to read {}: {source}", .path.display())]
    Unreadable {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
    },
    /// The lab configuration did not load or resolve.
    ///
    /// Carries the configuration's own [`VerifiedInput`] alongside the
    /// parse failure: when the file has *also* changed since the recorded
    /// run, that is almost certainly the explanation, and reporting the
    /// parse error alone would send a user hunting for a syntax mistake
    /// they did not make.
    #[error(
        "{}{source}",
        if config.matches() {
            String::new()
        } else {
            format!(
                "the lab configuration at {} no longer matches the recorded run \
                 (expected {}, found {}), which is the likely cause: ",
                config.path.display(), config.expected_sha256, config.actual_sha256,
            )
        }
    )]
    Config {
        /// The load or resolve failure.
        #[source]
        source: Box<SpecError>,
        /// The configuration file's digest check.
        config: Box<VerifiedInput>,
    },
    /// The recorded run had an expectations file and the current source
    /// does not, or the reverse.
    ///
    /// Cannot happen while the configuration's own digest matches — the
    /// `expectationsFile` field lives in that file — so this exists to
    /// catch the one thing that *could* still change it: a change in how
    /// `admissionlab-spec` resolves the field. Guarded rather than
    /// assumed away, because "this cannot happen" is not a thing to
    /// discover from a confusing report later.
    #[error(
        "the recorded run {} an expectations file, but the current source {}",
        if *recorded { "used" } else { "used no" },
        match source_file {
            Some(path) => format!("declares {}", path.display()),
            None => "declares none".to_owned(),
        }
    )]
    ExpectationsPresenceChanged {
        /// Whether the manifest recorded an expectations digest.
        recorded: bool,
        /// The expectations file the current source resolves to, if any.
        source_file: Option<PathBuf>,
    },
    /// One side's component *set* is no longer what the manifest
    /// recorded.
    ///
    /// A version difference is pinned and warned about
    /// ([`ReproductionPin::apply`]); a differing set of component *names*
    /// is refused, because there is nothing to pin a recorded version
    /// onto and nothing to install a recorded component from. Like
    /// [`ReproduceError::ExpectationsPresenceChanged`], this is
    /// unreachable while the configuration's digest matches and is
    /// guarded anyway.
    #[error(
        "the {side} environment's components changed since the recorded run: recorded {recorded:?}, \
         current source has {current:?}"
    )]
    ComponentSetChanged {
        /// Which side disagreed.
        side: Side,
        /// The component names the manifest recorded, in install order.
        recorded: Vec<String>,
        /// The component names the current source resolves, in install
        /// order.
        current: Vec<String>,
    },
}

/// Plans a reproduction of `manifest` from the lab configuration at
/// `<source_root>/`[`DEFAULT_LAB_FILE_NAME`].
///
/// This is ROADMAP Task 5.3's frozen entry point.
/// [`plan_reproduction_from_config`] is the same function for a lab file
/// that is not conventionally named; this one exists in exactly this
/// shape because the roadmap freezes it, and because
/// `admissionlab reproduce ./artifacts/run.json --source-root .` is the
/// invocation the product documents.
///
/// **Nothing is provisioned and nothing is fetched.** Every check here
/// reads files that are already on disk.
///
/// # What this verifies, and what it leaves to the caller
///
/// Returns the re-resolved lab plus a [`VerifiedInput`] for the
/// configuration and (when the lab declares one) the expectations file.
/// It does **not** verify fixture hashes or the effective
/// normalization/policy digests — those are not reachable from this crate
/// (see this module's "Where verification is split" section). A caller
/// must extend [`ReproducePlan::verified_inputs`] with
/// [`verify_fixtures`]' output and check [`verify_effective_digests`]
/// before creating anything.
///
/// A digest that does not match is **reported, not raised**: it lands in
/// `verified_inputs` and [`ReproducePlan::mismatches`] finds it. That is
/// what lets the caller list every mismatched input — the configuration,
/// the expectations file, and each fixture — in one message, which
/// ROADMAP Task 5.3 step 1 requires.
///
/// # Errors
///
/// Returns [`ReproduceError::UnsupportedSchema`] if `manifest`'s schema
/// version is not one of [`SUPPORTED_SCHEMA_VERSIONS`],
/// [`ReproduceError::Unreadable`] if the
/// configuration or the expectations file could not be read,
/// [`ReproduceError::Config`] if the configuration did not load or
/// resolve, and [`ReproduceError::ExpectationsPresenceChanged`] if the
/// source and the manifest disagree about whether there is an
/// expectations file at all.
pub fn plan_reproduction(
    manifest: &RunManifest,
    source_root: &Path,
) -> Result<ReproducePlan, ReproduceError> {
    plan_reproduction_from_config(manifest, &source_root.join(DEFAULT_LAB_FILE_NAME))
}

/// [`plan_reproduction`], for a lab configuration at an explicit path.
///
/// See [`plan_reproduction`] for the whole contract; this differs only in
/// how the configuration is located.
///
/// # Errors
///
/// Identical to [`plan_reproduction`]'s.
pub fn plan_reproduction_from_config(
    manifest: &RunManifest,
    config: &Path,
) -> Result<ReproducePlan, ReproduceError> {
    if !SUPPORTED_SCHEMA_VERSIONS.contains(&manifest.schema_version.as_str()) {
        return Err(ReproduceError::UnsupportedSchema {
            found: manifest.schema_version.clone(),
            supported: SUPPORTED_SCHEMA_VERSIONS,
        });
    }

    let config_input = VerifiedInput {
        path: config.to_path_buf(),
        expected_sha256: manifest.config_sha256.clone(),
        actual_sha256: read_digest(config)?,
    };

    // `load_any_supported_lab`, not the Alpha-only `load_lab`: a run
    // recorded against a v1beta1 configuration must be reproducible from
    // that same file (Task 7.1 made more than one `apiVersion` loadable;
    // the hash above already verified the exact bytes, so which
    // vocabulary reads them is provenance, not drift).
    let resolved_lab = load_any_supported_lab(config).map_err(|source| ReproduceError::Config {
        source: Box::new(source),
        config: Box::new(config_input.clone()),
    })?;

    let mut verified_inputs = vec![config_input];
    match (
        resolved_lab.expectations_file.as_deref(),
        manifest.expectations_sha256.as_deref(),
    ) {
        (Some(path), Some(expected)) => verified_inputs.push(VerifiedInput {
            path: path.to_path_buf(),
            expected_sha256: expected.to_owned(),
            actual_sha256: read_digest(path)?,
        }),
        (None, None) => {}
        (source_file, recorded) => {
            return Err(ReproduceError::ExpectationsPresenceChanged {
                recorded: recorded.is_some(),
                source_file: source_file.map(Path::to_path_buf),
            });
        }
    }

    Ok(ReproducePlan {
        resolved_lab,
        verified_inputs,
    })
}

/// Hashes `path`'s bytes, reporting the path in any failure.
///
/// Goes through [`file_sha256`], which is the one implementation of this
/// digest in the workspace — see this module's "Where verification is
/// split" section.
fn read_digest(path: &Path) -> Result<String, ReproduceError> {
    file_sha256(path).map_err(|source| ReproduceError::Unreadable {
        path: path.to_path_buf(),
        source,
    })
}

/// One fixture the current source discovers, as [`verify_fixtures`]
/// needs it.
///
/// A core-owned mirror of the two fields of
/// `admissionlab_fixtures::FixtureSource` this comparison uses, for the
/// reason this module's "Where verification is split" section gives:
/// `admissionlab-fixtures` sits above this crate and cannot be named
/// here. The caller copies `sha256` across verbatim — discovery already
/// computed it, and recomputing it here could only ever disagree with
/// what the run itself will use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFixture {
    /// The fixture's identifier, the key `fixture_hashes` uses.
    pub id: FixtureId,
    /// The file this fixture's content came from, for the failure
    /// message. Several fixtures may share one path (a multi-document
    /// file, or one matrix's cases), which is why the identifier is
    /// reported alongside it.
    pub path: PathBuf,
    /// The fixture's content digest, exactly as discovery computed it.
    pub sha256: String,
}

/// What [`verify_fixtures`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureVerification {
    /// One entry per fixture the manifest recorded *and* the current
    /// source still discovers, in identifier order.
    pub verified: Vec<VerifiedInput>,
    /// Identifiers the manifest recorded that the current source no
    /// longer discovers, in identifier order.
    pub missing: Vec<FixtureId>,
    /// Identifiers the current source discovers that the recorded run
    /// never replayed, in identifier order.
    pub unexpected: Vec<FixtureId>,
}

impl FixtureVerification {
    /// Whether the corpus is exactly the recorded one, content included.
    #[must_use]
    pub fn is_faithful(&self) -> bool {
        self.missing.is_empty()
            && self.unexpected.is_empty()
            && self.verified.iter().all(VerifiedInput::matches)
    }
}

/// Compares the fixtures the current source discovers against the hashes
/// `manifest` recorded.
///
/// **Hashes nothing.** `discovered` carries digests the caller's own
/// fixture discovery already computed, so this is a set comparison and a
/// string comparison — see this module's "Where verification is split"
/// section for why that division is the one that keeps a single digest
/// implementation in the product.
///
/// A fixture that appears or disappears is reported separately from one
/// whose content changed, because they are different mistakes: a missing
/// identifier usually means a renamed file or a narrowed glob, while a
/// mismatched digest means the fixture itself was edited.
#[must_use]
pub fn verify_fixtures(
    manifest: &RunManifest,
    discovered: &[DiscoveredFixture],
) -> FixtureVerification {
    let by_id: BTreeMap<&FixtureId, &DiscoveredFixture> = discovered
        .iter()
        .map(|fixture| (&fixture.id, fixture))
        .collect();

    let mut verified = Vec::new();
    let mut missing = Vec::new();
    for (id, expected) in &manifest.fixture_hashes {
        match by_id.get(id) {
            Some(fixture) => verified.push(VerifiedInput {
                path: fixture.path.clone(),
                expected_sha256: expected.clone(),
                actual_sha256: fixture.sha256.clone(),
            }),
            None => missing.push(id.clone()),
        }
    }

    let recorded: BTreeSet<&FixtureId> = manifest.fixture_hashes.keys().collect();
    let unexpected = by_id
        .keys()
        .filter(|id| !recorded.contains(**id))
        .map(|id| (*id).clone())
        .collect();

    FixtureVerification {
        verified,
        missing,
        unexpected,
    }
}

/// Compares the effective normalization and policy digests the caller
/// recomputed against the ones `manifest` recorded.
///
/// **Hashes nothing**, for the same reason [`verify_fixtures`] does not:
/// the normalization profile is assembled by `admissionlab-normalize`,
/// which this crate cannot name, and both digests come out of the one
/// function that produces every input digest in the product.
///
/// Returns an empty vector when both agree.
#[must_use]
pub fn verify_effective_digests(
    manifest: &RunManifest,
    normalization_sha256: &str,
    policy_sha256: &str,
) -> Vec<EffectiveMismatch> {
    [
        (
            "normalization",
            &manifest.normalization_sha256,
            normalization_sha256,
        ),
        ("policy", &manifest.policy_sha256, policy_sha256),
    ]
    .into_iter()
    .filter(|(_, expected, actual)| expected.as_str() != *actual)
    .map(|(what, expected, actual)| EffectiveMismatch {
        what,
        expected_sha256: expected.clone(),
        actual_sha256: actual.to_owned(),
    })
    .collect()
}

/// One side's recorded environment, as a set of pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidePin {
    /// The Kubernetes version the recorded run provisioned.
    pub kubernetes_version: String,
    /// The node image reference to create the cluster from, digest and
    /// all when the recorded run had one.
    pub node_image: String,
    /// Whether `node_image` carries an `@sha256:...` digest. `false`
    /// means the reproduction can pin the *tag* but not the content —
    /// see [`ReproductionPin::apply`] for what is emitted then.
    pub node_image_digest_pinned: bool,
    /// The version each recorded component was installed at, keyed by
    /// component name.
    pub component_versions: BTreeMap<String, String>,
}

/// Everything a reproduction imposes on a run instead of resolving it
/// afresh (ROADMAP Task 5.3 step 2).
///
/// Built from a manifest alone — it reads no files and contacts no
/// registry — and applied to a resolved lab by [`ReproductionPin::apply`]
/// and to cluster creation by [`ReproductionPin::node_images`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReproductionPin {
    /// The baseline side's recorded environment.
    pub baseline: SidePin,
    /// The candidate side's recorded environment.
    pub candidate: SidePin,
}

impl ReproductionPin {
    /// Reads both sides' pins out of `manifest`.
    #[must_use]
    pub fn from_manifest(manifest: &RunManifest) -> Self {
        Self {
            baseline: side_pin(&manifest.baseline),
            candidate: side_pin(&manifest.candidate),
        }
    }

    /// Both sides' node image references, in the shape
    /// [`crate::LabRunner::create_clusters`] takes.
    ///
    /// This is what makes ROADMAP Task 5.3 step 4 structural rather than
    /// remembered: a caller holding one of these never calls
    /// [`crate::LabRunner::resolve_node_images`], so the compatibility
    /// matrix — and whatever newer digest it has learned since — is never
    /// consulted at all. The reference reaching `kind --image` is the
    /// recorded one, byte for byte.
    #[must_use]
    pub fn node_images(&self) -> ResolvedNodeImages {
        ResolvedNodeImages {
            baseline: self.baseline.node_image.clone(),
            candidate: self.candidate.node_image.clone(),
        }
    }

    /// Imposes both sides' recorded versions on `lab`, returning one
    /// [`Diagnostic`] for every substitution a reader must know about.
    ///
    /// # What is pinned
    ///
    /// Each side's Kubernetes version and each component's version — and,
    /// for a Helm component, the chart version passed to
    /// `helm install --version`, not merely the component's display
    /// version. Pinning only the latter would leave the actual chart
    /// resolution untouched and make this whole step decorative.
    ///
    /// # Why anything is ever different, given the configuration matched
    ///
    /// It should not be. The configuration file's own digest is checked
    /// before this runs, and every version in a resolved lab comes from
    /// that file. So a difference here can only come from *resolution
    /// behavior* changing between the recorded run's build of Admission
    /// Lab and this one — a defaulting rule that changed, a field that
    /// started being inherited. That is exactly the case ROADMAP Task 5.3
    /// step 2 means by "not current recipe defaults", and the rule is:
    /// **the manifest wins for versions, the source must hash-match for
    /// content.** Every such substitution is reported, never silent.
    ///
    /// # The one thing that is not pinned
    ///
    /// A component whose recorded version is
    /// [`UNCONFIRMED_COMPONENT_VERSION`] is left at the source's own
    /// pinned version, with a diagnostic. That value is not a version:
    /// it is what the recorded run wrote down when `helm get metadata`
    /// could not confirm what had been installed (Global Constraint 15 —
    /// it refuses to echo the requested version back as a guess).
    /// Installing `--version unknown` would fail the reproduction for a
    /// reason that has nothing to do with the lab, and inventing a
    /// version would be the fabrication that constraint forbids. The
    /// source's pin is used instead, and it is *not* a fall-forward: the
    /// configuration's digest matched, so it is the identical pin, from
    /// the identical bytes, that the recorded run itself requested.
    ///
    /// # Errors
    ///
    /// Returns [`ReproduceError::ComponentSetChanged`] if either side's
    /// component names are no longer the recorded ones. There is nothing
    /// to pin in that case, and proceeding would install a stack the
    /// manifest does not describe.
    pub fn apply(&self, lab: &mut ResolvedLab) -> Result<Vec<Diagnostic>, ReproduceError> {
        let mut notes = Vec::new();
        for (side, pin, environment) in [
            (Side::Baseline, &self.baseline, &mut lab.baseline),
            (Side::Candidate, &self.candidate, &mut lab.candidate),
        ] {
            let current: Vec<String> = environment
                .components
                .iter()
                .map(|component| component.name.clone())
                .collect();
            let recorded: Vec<String> = pin.component_versions.keys().cloned().collect();
            if current.iter().cloned().collect::<BTreeSet<_>>()
                != recorded.iter().cloned().collect::<BTreeSet<_>>()
            {
                return Err(ReproduceError::ComponentSetChanged {
                    side,
                    recorded,
                    current,
                });
            }

            if environment.kubernetes != pin.kubernetes_version {
                notes.push(pin_note(
                    "reproduce.kubernetes_version_pinned",
                    side,
                    format!(
                        "the {side} environment now resolves Kubernetes {current:?}, but the \
                         recorded run provisioned {recorded:?}; reproducing at the recorded \
                         version",
                        current = environment.kubernetes,
                        recorded = pin.kubernetes_version,
                    ),
                    None,
                ));
                environment.kubernetes.clone_from(&pin.kubernetes_version);
            }

            if !pin.node_image_digest_pinned {
                notes.push(pin_note(
                    "reproduce.node_image_not_digest_pinned",
                    side,
                    format!(
                        "the recorded run did not pin a content digest for the {side} node image \
                         {image:?}; this reproduction pins the same tag, but the image behind it \
                         may have been republished since",
                        image = pin.node_image,
                    ),
                    None,
                ));
            }

            for component in &mut environment.components {
                let Some(recorded) = pin.component_versions.get(&component.name) else {
                    // Unreachable: the name sets were just proved equal.
                    continue;
                };
                if recorded == UNCONFIRMED_COMPONENT_VERSION {
                    notes.push(pin_note(
                        "reproduce.component_version_unconfirmed",
                        side,
                        format!(
                            "the recorded run could not confirm which version of {name:?} it \
                             installed; reproducing at the source configuration's own pin \
                             {version:?}, which is the version that run requested",
                            name = component.name,
                            version = component.version,
                        ),
                        Some(&component.name),
                    ));
                    continue;
                }
                if &component.version != recorded {
                    notes.push(pin_note(
                        "reproduce.component_version_pinned",
                        side,
                        format!(
                            "the current source resolves {name:?} to {current:?}, but the \
                             recorded run installed {recorded:?}; reproducing at the recorded \
                             version",
                            name = component.name,
                            current = component.version,
                        ),
                        Some(&component.name),
                    ));
                }
                component.version.clone_from(recorded);
                if let InstallMethod::Helm(helm) = &mut component.install {
                    helm.version.clone_from(recorded);
                }
            }
        }
        Ok(notes)
    }

    /// A human-readable block naming every recorded artifact this
    /// reproduction is about to demand.
    ///
    /// Printed before any cluster is created, so a *run-time*
    /// unavailability — a node image digest that no longer pulls, a chart
    /// version yanked from its repository — can be read against an
    /// explicit list of what was asked for. See this module's "Plan-time
    /// refusal versus run-time unavailability" section for why that is
    /// the only thing this crate can honestly contribute to a failure it
    /// cannot detect without a network.
    ///
    /// Ends with a newline, so it can be written straight to a stream.
    #[must_use]
    pub fn pinned_summary(&self) -> String {
        use std::fmt::Write as _;

        let mut summary = String::from("Reproducing with the recorded environment:\n");
        for (side, pin) in [
            (Side::Baseline, &self.baseline),
            (Side::Candidate, &self.candidate),
        ] {
            let _: std::fmt::Result = writeln!(
                summary,
                "  {side}: Kubernetes {version}, node image {image}",
                version = pin.kubernetes_version,
                image = pin.node_image,
            );
            for (name, version) in &pin.component_versions {
                let _: std::fmt::Result = writeln!(summary, "    {name} {version}");
            }
        }
        summary
    }
}

/// Reads one side's pins out of its recorded environment.
///
/// Reassembles the node image reference from the two halves
/// [`crate::split_node_image_reference`] wrote, which is what makes the
/// reference `kind` receives byte-identical to the recorded run's.
fn side_pin(environment: &EnvironmentProvenance) -> SidePin {
    let node_image = match &environment.node_image_digest {
        Some(digest) => format!("{}@{digest}", environment.node_image),
        None => environment.node_image.clone(),
    };
    SidePin {
        kubernetes_version: environment.kubernetes_version.clone(),
        node_image,
        node_image_digest_pinned: environment.node_image_digest.is_some(),
        component_versions: environment
            .components
            .iter()
            .map(|component| (component.name.clone(), component.version.clone()))
            .collect(),
    }
}

/// Builds one substitution diagnostic, tagged with the side (and, where
/// there is one, the component) it concerns.
fn pin_note(code: &str, side: Side, message: String, component: Option<&str>) -> Diagnostic {
    let mut context = BTreeMap::new();
    context.insert("side".to_owned(), RedactedValue::Public(side.to_string()));
    if let Some(component) = component {
        context.insert(
            "component".to_owned(),
            RedactedValue::Public(component.to_owned()),
        );
    }
    Diagnostic {
        code: code.to_owned(),
        message,
        context,
    }
}

/// A warning about reproducing from a manifest whose run never finished,
/// or `None` when the manifest describes a completed run.
///
/// Reproduction is **not** refused for an unfinished run, and that is a
/// deliberate reading of ROADMAP Task 5.2's status/stage pair rather than
/// a gap. A manifest's *first* write is already a complete pre-cluster
/// record — host, tools, both node images, every input digest (see
/// `admissionlab_core::run_manifest`'s "Incremental writes" section) — so
/// everything reproduction needs is present the moment the file exists.
/// And reproducing a run that *failed* is one of the most useful things
/// this command does: it is how a user re-creates the environment a
/// twenty-minute install died in.
///
/// What is degraded is narrow and worth saying out loud: a run that never
/// reached the install stage recorded each side's components as
/// *configured*, not as *installed*, so those versions are the
/// configuration's own pins rather than versions confirmed against a
/// cluster. Everything else is identical.
#[must_use]
pub fn incomplete_run_warning(manifest: &RunManifest) -> Option<String> {
    match manifest.status {
        RunStatus::Completed => None,
        RunStatus::InProgress => Some(format!(
            "this manifest describes a run that never reported an outcome (last completed stage: \
             {}); its recorded component versions may be the configured ones rather than the \
             installed ones",
            manifest.stage,
        )),
        RunStatus::Failed => Some(format!(
            "this manifest describes a run that failed at the {} stage; its recorded component \
             versions may be the configured ones rather than the installed ones",
            manifest.stage,
        )),
    }
}
