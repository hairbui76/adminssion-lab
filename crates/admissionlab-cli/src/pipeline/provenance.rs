//! Assembling this run's [`RunManifest`] (ROADMAP Tasks 5.1/5.2).
//!
//! `admissionlab-core` owns the manifest *model*, its digest definitions,
//! and the incremental writer. This module owns the one thing that model
//! cannot: turning the values a real `admissionlab test` run has in hand —
//! a resolved lab, discovered fixtures, a doctor report, resolved node
//! images, an installed stack — into that model.
//!
//! # Why the conversion lands here
//!
//! Two of the manifest's inputs live in crates `admissionlab-core` can
//! never name. `admissionlab_fixtures::FixtureSource` (which carries each
//! fixture's own SHA-256) and
//! `admissionlab_normalize::NormalizationProfile` both sit *above* core
//! in the dependency graph — `normalize -> admission -> fixtures -> core`
//! — so core defines a mirror ([`EffectiveNormalization`]) and leaves the
//! conversion to "the crate that already depends on both". That is this
//! crate, and it is the same argument, in the same words, that
//! [`super::compare`]'s module documentation makes for the
//! `RecipeNormalizeRule` → `NormalizeRule` conversion living there.
//!
//! [`normalization_rule_record`] is written as a total, wildcard-free
//! match for the same reason that conversion is: a fourth
//! `NormalizeRule` variant must be a compile error here, not a rule
//! silently dropped from the digest that claims to describe the profile.
//!
//! # What is hashed, and when
//!
//! [`input_digests`] hashes everything a run knows before it provisions
//! anything: the configuration file's own bytes, the expectations file's
//! own bytes, each fixture's already-computed content hash, and the
//! canonical encodings of the effective normalization profile and the
//! effective regression policy. See `admissionlab_core::run_manifest`'s
//! "Canonical serialization" section for the exact rules; nothing here
//! defines a hash, it only supplies the bytes.
//!
//! The configuration and expectations files are read a second time here,
//! *after* `load_lab`/`load_expectations` already parsed them. That is
//! deliberate rather than wasteful: the manifest must record the hash of
//! the bytes on disk, and neither loader hands back the bytes it read.
//! Re-reading also means a file that changed between parse and hash
//! produces a hash that does not match what ran — which is why a read
//! failure here fails the run rather than being recorded as an absent
//! digest.
//!
//! # Components are recorded twice, from two different sources
//!
//! Before installation, a side's [`ComponentProvenance`] entries come
//! from the *resolved configuration*: the name and the pinned version the
//! user asked for. After installation they are re-recorded from
//! `InstalledComponent::resolved_version`, which the installer confirms
//! against the cluster where it can. A manifest left behind by a run that
//! failed during install therefore still names what that side was
//! *trying* to install, which is exactly the question a failed install
//! raises.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use admissionlab_core::{
    ComponentProvenance, DoctorReport, EffectiveNormalization, EnvironmentProvenance,
    HostProvenance, NormalizationRuleRecord, ResolvedNodeImages, RunId, RunManifest, RunStage,
    RunStatus, SideInstall, ToolProvenance, file_sha256, normalization_sha256, policy_sha256,
    split_node_image_reference,
};
use admissionlab_core::{FixtureId, run_manifest::SCHEMA_VERSION};
use admissionlab_fixtures::FixtureSource;
use admissionlab_normalize::{NormalizationProfile, NormalizeRule};
use admissionlab_spec::{ResolvedComponent, ResolvedLab};

/// The Admission Lab version recorded in every manifest this binary
/// writes.
///
/// The **CLI** crate's version, not `admissionlab-core`'s: the manifest
/// field means "which Admission Lab produced this run", and what a user
/// installs and invokes is this binary.
const ADMISSIONLAB_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Every digest a run computes from its own inputs, before it provisions
/// anything.
///
/// Grouped rather than passed as five loose strings so a future digest
/// is added in one place and cannot be forgotten at a call site.
#[derive(Debug, Clone)]
pub struct InputDigests {
    /// SHA-256 of the lab configuration file's own bytes.
    pub config_sha256: String,
    /// One SHA-256 per discovered fixture, keyed by fixture identifier.
    pub fixture_hashes: BTreeMap<FixtureId, String>,
    /// SHA-256 of the expectations file's own bytes, or `None` when the
    /// configuration declared none.
    pub expectations_sha256: Option<String>,
    /// SHA-256 of the effective normalization profile's canonical
    /// encoding.
    pub normalization_sha256: String,
    /// SHA-256 of the effective regression policy's canonical encoding.
    pub policy_sha256: String,
}

/// Computes every input digest for a run whose configuration is at
/// `config`.
///
/// # Errors
///
/// Returns [`io::Error`] if the configuration file or the declared
/// expectations file could not be read. See this module's "What is
/// hashed, and when" section for why that fails the run rather than
/// degrading to an absent digest: a manifest that silently omitted the
/// hash of a file the run actually used would make reproduction unable to
/// tell "there was no such file" from "we could not read it".
pub fn input_digests(
    config: &Path,
    lab: &ResolvedLab,
    fixtures: &[FixtureSource],
) -> io::Result<InputDigests> {
    // `file_sha256`, not a local `sha256_hex(&fs::read(..))`: it is the
    // one implementation of "the digest of a file's bytes" in the
    // workspace, and `admissionlab_core::reproduce` checks these same two
    // digests back through it. Two spellings of one rule is exactly the
    // pair that would eventually stop agreeing.
    let config_sha256 = file_sha256(config)?;
    let expectations_sha256 = match &lab.expectations_file {
        Some(path) => Some(file_sha256(path)?),
        None => None,
    };

    // `FixtureSource::sha256` is already the hash of that fixture's
    // content (whole-file bytes for a static fixture, a documented
    // domain-separated digest for a matrix-expanded one), so this
    // re-keys rather than re-hashes: computing it again here could only
    // ever disagree with what discovery recorded.
    let fixture_hashes = fixtures
        .iter()
        .map(|fixture| (fixture.id.clone(), fixture.sha256.clone()))
        .collect();

    Ok(InputDigests {
        config_sha256,
        fixture_hashes,
        expectations_sha256,
        normalization_sha256: normalization_sha256(&effective_normalization(
            &super::compare::normalization_profile(lab),
        )),
        policy_sha256: policy_sha256(&lab.policy),
    })
}

/// Converts the engine's normalization profile into the core-owned
/// mirror the manifest hashes. See this module's documentation for why
/// this conversion lives in this crate.
#[must_use]
pub fn effective_normalization(profile: &NormalizationProfile) -> EffectiveNormalization {
    let tier = |rules: &[NormalizeRule]| rules.iter().map(normalization_rule_record).collect();
    EffectiveNormalization {
        built_in: tier(&profile.built_in),
        recipe: tier(&profile.recipe),
        user: tier(&profile.user),
    }
}

/// Maps one engine rule onto its manifest record.
///
/// Total and wildcard-free: a new [`NormalizeRule`] variant must fail to
/// compile here rather than vanish from the digest that claims to
/// describe the profile.
fn normalization_rule_record(rule: &NormalizeRule) -> NormalizationRuleRecord {
    match rule {
        NormalizeRule::RemovePointer(pointer) => NormalizationRuleRecord::RemovePointer {
            pointer: pointer.clone(),
        },
        NormalizeRule::SortNamedArray { pointer, key } => NormalizationRuleRecord::SortNamedArray {
            pointer: pointer.clone(),
            key: key.clone(),
        },
        NormalizeRule::RemoveAnnotation(annotation) => NormalizationRuleRecord::RemoveAnnotation {
            annotation: annotation.clone(),
        },
    }
}

/// Builds the manifest written before any cluster is created.
///
/// Complete rather than skeletal: by this point the run has validated
/// every input, hashed them, probed the host, and resolved both sides'
/// node images, so the only thing a later write revises is each side's
/// component versions (see this module's "Components are recorded twice"
/// section) — plus the stage and status, which advance.
///
/// Stamped [`RunStatus::InProgress`] at [`RunStage::Started`] here rather
/// than by the writer, which deliberately decides nothing on its own.
#[must_use]
pub fn initial_manifest(
    run_id: &RunId,
    doctor: &DoctorReport,
    lab: &ResolvedLab,
    images: &ResolvedNodeImages,
    digests: InputDigests,
    started_at: SystemTime,
) -> RunManifest {
    RunManifest {
        schema_version: SCHEMA_VERSION.to_owned(),
        run_id: run_id.clone(),
        admissionlab_version: ADMISSIONLAB_VERSION.to_owned(),
        status: RunStatus::InProgress,
        stage: RunStage::Started,
        host: HostProvenance::detect(),
        tools: ToolProvenance::from_doctor_report(doctor),
        baseline: configured_environment(
            &lab.baseline.kubernetes,
            &images.baseline,
            &lab.baseline.components,
        ),
        candidate: configured_environment(
            &lab.candidate.kubernetes,
            &images.candidate,
            &lab.candidate.components,
        ),
        config_sha256: digests.config_sha256,
        fixture_hashes: digests.fixture_hashes,
        expectations_sha256: digests.expectations_sha256,
        normalization_sha256: digests.normalization_sha256,
        policy_sha256: digests.policy_sha256,
        started_at,
        completed_at: None,
    }
}

/// One side's environment as *configured*: the resolved node image, plus
/// the components and pinned versions this side is about to install.
fn configured_environment(
    kubernetes: &str,
    node_image: &str,
    components: &[ResolvedComponent],
) -> EnvironmentProvenance {
    let (node_image, node_image_digest) = split_node_image_reference(node_image);
    EnvironmentProvenance {
        kubernetes_version: kubernetes.to_owned(),
        node_image,
        node_image_digest,
        components: components
            .iter()
            .map(|component| ComponentProvenance {
                name: component.name.clone(),
                version: component.version.clone(),
                // See `ComponentProvenance::source_sha256`: honestly
                // absent today, with the seam that would fill it named
                // there.
                source_sha256: None,
            })
            .collect(),
    }
}

/// One side's components as *installed*, in install order.
///
/// Replaces the configured list once the install stage succeeds:
/// `InstalledComponent::resolved_version` is the version actually
/// installed, confirmed against the cluster wherever the installer could
/// confirm it.
#[must_use]
pub fn installed_components(install: &SideInstall) -> Vec<ComponentProvenance> {
    install
        .components
        .iter()
        .map(|component| ComponentProvenance {
            name: component.name.clone(),
            version: component.resolved_version.clone(),
            source_sha256: None,
        })
        .collect()
}
