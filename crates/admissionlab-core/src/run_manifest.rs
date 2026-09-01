//! The versioned run manifest: the single document that says exactly
//! which inputs, tools, and environments produced one completed run
//! (Task 5.1; Global Constraint 13's "version/provenance recording";
//! PRODUCT.md §28).
//!
//! [`RunManifest`] is written to a run's `run.json` (see
//! [`crate::RunPaths::run_json`]) and is the input Task 5.3's
//! `admissionlab reproduce` reads back. Everything here therefore has two
//! audiences at once — a human diagnosing "why did this run differ from
//! that one", and a machine re-deriving a run from the record — and the
//! design rules below follow from serving both.
//!
//! # What this document may never contain (Global Constraint 14)
//!
//! A run manifest is an artifact users attach to bug reports, upload as a
//! CI artifact, and paste into issues. It must therefore never carry a
//! secret value or a kubeconfig, and the strongest way to guarantee that
//! is to make it *unable* to: **no type in this module holds a
//! [`std::path::PathBuf`], an environment map, a captured stdout/stderr,
//! or any cluster-connection material.** Every field is one of a version
//! string, an image reference, an identifier, a SHA-256 hex digest, or a
//! timestamp.
//!
//! That is a structural guarantee rather than a filtering one, which
//! matters because filtering is the kind of thing that silently stops
//! working. A kubeconfig path cannot be "accidentally left in" a field
//! that cannot hold a path; certificate data cannot leak through a digest
//! field that is validated to be hex. `tests/run_manifest.rs`'s
//! `manifest_from_realistic_inputs_leaks_no_credential_material` and
//! `top_level_keys_are_exactly_the_frozen_set` are the regression tests
//! for both halves: the first serializes a manifest built from inputs
//! that *do* carry kubeconfig paths and certificate data nearby and
//! asserts none of it reaches the JSON, the second fails the moment a
//! field is added, so adding one is a deliberate act with this section
//! re-read rather than an incidental one.
//!
//! # Honest absence (Global Constraint 15)
//!
//! Three fields are `Option` where ROADMAP §1.2's type registry sketches
//! a bare `String`, and each one is a deliberate refinement rather than a
//! drift from the frozen shape (the field *names* and their meaning are
//! unchanged; only the ability to say "not observed" is added):
//!
//! - [`ToolProvenance`]'s four version fields. A tool can be present and
//!   runnable while its version probe exits non-zero or prints something
//!   unparseable — [`crate::ToolStatus::version`] is already `Option` for
//!   exactly that reason, and a run is not blocked by it
//!   ([`crate::DoctorReport::meets_prerequisites`] gates on `found`, not
//!   on `version`). Copying a `None` through as `""` or `"unknown"` would
//!   turn "we could not read this" into a value a reproduce step could
//!   try to match against.
//! - [`EnvironmentProvenance::node_image_digest`]. Admission Lab's own
//!   `kind` backend always digest-pins (`compatibility/kubernetes.yaml`
//!   stores a digest per release), but [`crate::ClusterManager`] is a
//!   trait any backend may implement and its
//!   [`resolve_node_image`](crate::ClusterManager::resolve_node_image)
//!   contract only says "ideally already digest-pinned". A reference that
//!   carries no `@sha256:...` has no digest to record, and inventing one
//!   is the fabrication GC15 forbids.
//! - [`ComponentProvenance::source_sha256`]. See that field's own
//!   documentation for what is and is not obtainable today.
//!
//! # Canonical serialization, and what each `*_sha256` field hashes
//!
//! Four fields are digests, and a digest is only reproducible if the
//! bytes fed to it are pinned. Two rules cover all four:
//!
//! 1. **A digest over a file hashes that file's own bytes, exactly as
//!    read from disk.** [`RunManifest::config_sha256`] and
//!    [`RunManifest::expectations_sha256`] are computed with
//!    [`sha256_hex`] over the raw file contents — never over a
//!    re-serialization of the parsed document, which would silently
//!    change whenever a parser's round-trip did. This is the same
//!    convention `admissionlab_fixtures`'s own fixture hashing already
//!    follows, so `fixture_hashes`, `config_sha256`, and
//!    `expectations_sha256` are all the same function of the same kind of
//!    bytes.
//! 2. **A digest over an in-memory value hashes its canonical JSON
//!    encoding**, as defined by [`canonical_sha256`]:
//!    `serde_json::to_value` followed by `serde_json::to_vec` — compact
//!    (no whitespace), UTF-8, no trailing newline. Object keys come out
//!    in lexicographic order because `serde_json::Map` is `BTreeMap`
//!    -backed workspace-wide (neither `serde_json`'s nor `schemars`'s
//!    `preserve_order` feature is enabled anywhere; `admissionlab-spec`'s
//!    `schema.rs` documents and depends on the same property). Array
//!    order is preserved, because array order is *meaning* here: a
//!    normalization profile's rules apply in order, and reordering them
//!    can change the normalized result.
//!
//!    [`RunManifest::normalization_sha256`] and
//!    [`RunManifest::policy_sha256`] are the two digests of this second
//!    kind, over [`EffectiveNormalization`] and
//!    [`admissionlab_spec::PolicySpec`] respectively — the *effective*
//!    values a run actually used, not the text a user typed, which
//!    `config_sha256` already covers. That distinction is the point of
//!    having both: two configurations that differ only in comments have
//!    different `config_sha256` and identical `policy_sha256`, and a
//!    recipe that contributes normalization rules changes
//!    `normalization_sha256` without touching `config_sha256` at all.
//!
//! # Timestamps are RFC 3339 with fixed nanosecond precision
//!
//! [`RunManifest::started_at`] and [`RunManifest::completed_at`] are
//! [`SystemTime`] in Rust and, on the wire, RFC 3339 in UTC with exactly
//! nine fractional digits: `2026-09-01T12:00:00.000000000Z`. The choice
//! (ROADMAP Task 5.1 leaves it to the implementation, requiring only that
//! it be picked and pinned) is RFC 3339 rather than epoch seconds because
//! a manifest is read by humans at least as often as by machines, and the
//! precision is *fixed* rather than variable so that a manifest's byte
//! width does not depend on whether an instant happened to land on a
//! whole second — a golden file that only sometimes has a fractional part
//! is a golden file that only sometimes matches.
//!
//! Rendering goes through `jiff::Timestamp`, the same library
//! `admissionlab-admission` already parses Kubernetes audit timestamps
//! with. An instant outside the representable range fails the write
//! rather than being clamped.
//!
//! # Incremental writes and partial provenance (Task 5.2)
//!
//! A manifest is not written once at the end. [`RunManifestWriter`]
//! creates `run.json` **before any cluster exists** and atomically
//! rewrites it after every major stage, so a run that dies at install
//! still leaves a valid, parseable document saying what it was, what it
//! ran on, and where it stopped. The alternative — writing only on
//! success — throws the provenance away in exactly the situation a user
//! most needs it (PRODUCT.md §33: enough on first failure that a rerun is
//! not required to learn what happened).
//!
//! Two fields carry that state, and they answer different questions:
//! [`RunManifest::status`] says whether the run is still going, finished,
//! or failed, and [`RunManifest::stage`] says *where* — the last stage
//! that completed while a run is in progress, or the stage that failed
//! once it has. [`RunManifest::completed_at`] stays `None` for anything
//! but a completed run, so `completedAt: null` alone already
//! distinguishes a partial manifest from a whole one without a reader
//! needing to understand the stage vocabulary at all.
//!
//! ## Where the first write actually happens, and why
//!
//! `admissionlab-cli`'s pipeline validates every user input, hashes the
//! fixtures, probes host prerequisites, and resolves both sides' node
//! images *before* it creates a cluster — see that module's own "every
//! input check happens before any cluster is created" section for why
//! that order exists independently of this task. The first manifest write
//! therefore lands after all of those and before
//! [`crate::ClusterManager::create`] is ever called, which means it is
//! already a *complete* pre-cluster record: host, tool versions, every
//! input digest, and both environments' resolved node images. Later
//! writes revise it (component versions become the versions actually
//! installed) and advance the stage; they do not fill in blanks the first
//! write left behind, except for a side's `components` list, which is
//! populated from the resolved configuration up front and re-recorded
//! from the installer afterwards.
//!
//! # Schema
//!
//! [`run_manifest_v1alpha1_json_schema`] generates
//! `schemas/run-manifest-v1alpha1.json` from the same derives that govern
//! serialization, so the published schema can never describe a shape
//! Admission Lab does not actually write. `tests/run_manifest.rs`
//! regenerates it and compares byte-for-byte, exactly as
//! `admissionlab-spec`'s `tests/schema.rs` does for the configuration
//! schema; see that crate's `schema.rs` for the determinism argument,
//! which applies here unchanged.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use admissionlab_spec::PolicySpec;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::artifact::{ArtifactError, ArtifactStore, RunPaths};
use crate::ids::{FixtureId, RunId};
use crate::tool::{DoctorReport, ToolName};

/// The `schemaVersion` value every manifest this crate writes carries.
///
/// Namespaced and versioned the same way `admissionlab-report`'s own
/// `SCHEMA_VERSION` is (`admissionlab.io/result/v1alpha1`), so a consumer
/// holding an arbitrary Admission Lab JSON document can tell the two
/// apart from this field alone. Like that one, `v1alpha1` is
/// **experimental**: Alpha makes no compatibility promise across a
/// version bump.
pub const SCHEMA_VERSION: &str = "admissionlab.io/run-manifest/v1alpha1";

/// Everything needed to say what produced one run, and (Task 5.3) to
/// attempt reproducing it.
///
/// See this module's documentation for what this document may never
/// contain, for where each `*_sha256` digest's bytes come from, and for
/// the pinned timestamp encoding.
///
/// # No `Default`
///
/// Deliberately absent, and not an oversight: every field here is
/// *evidence*, and a defaulted manifest would be a document full of
/// empty strings asserting things nothing observed. There is no
/// constructor either — a caller fills the struct literally, so adding a
/// field is a compile error at every construction site rather than a
/// silently-defaulted blank.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunManifest {
    /// Always [`SCHEMA_VERSION`] for documents this crate writes; kept as
    /// a field rather than implied so a reader that only has the file can
    /// tell what it is holding.
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,
    /// The run this manifest describes.
    #[serde(rename = "runId")]
    #[schemars(with = "String")]
    pub run_id: RunId,
    /// The Admission Lab version that produced this run, as its binary
    /// reports it.
    #[serde(rename = "admissionlabVersion")]
    pub admissionlab_version: String,
    /// Whether this run is still going, finished, or failed (Task 5.2).
    #[serde(rename = "status")]
    pub status: RunStatus,
    /// Where this run got to — the last stage that completed while it is
    /// in progress, or the stage that failed once it has (Task 5.2). See
    /// [`RunStage`].
    #[serde(rename = "stage")]
    pub stage: RunStage,
    /// The machine the run executed on.
    #[serde(rename = "host")]
    pub host: HostProvenance,
    /// The external tool versions this run actually used.
    #[serde(rename = "tools")]
    pub tools: ToolProvenance,
    /// The baseline environment as provisioned.
    #[serde(rename = "baseline")]
    pub baseline: EnvironmentProvenance,
    /// The candidate environment as provisioned.
    #[serde(rename = "candidate")]
    pub candidate: EnvironmentProvenance,
    /// SHA-256 (lowercase hex) of the lab configuration file's own bytes.
    /// See this module's "Canonical serialization" section.
    #[serde(rename = "configSha256")]
    pub config_sha256: String,
    /// One SHA-256 (lowercase hex) per replayed fixture, keyed by
    /// fixture identifier and therefore written in identifier order (see
    /// [`FixtureId`]'s own `Ord` note) rather than discovery order.
    #[serde(rename = "fixtureHashes")]
    #[schemars(with = "BTreeMap<String, String>")]
    pub fixture_hashes: BTreeMap<FixtureId, String>,
    /// SHA-256 (lowercase hex) of the expectations file's own bytes, or
    /// `None` when the configuration declared no `expectationsFile`.
    /// `None` here means "there was no such file", which is a different
    /// claim from "there was one and we could not read it" — the latter
    /// fails the run before a manifest is ever completed.
    #[serde(rename = "expectationsSha256")]
    pub expectations_sha256: Option<String>,
    /// SHA-256 (lowercase hex) of the canonical encoding of the
    /// *effective* normalization profile ([`EffectiveNormalization`]) —
    /// built-in rules plus whatever the run's recipes and the user
    /// contributed. See this module's "Canonical serialization" section.
    #[serde(rename = "normalizationSha256")]
    pub normalization_sha256: String,
    /// SHA-256 (lowercase hex) of the canonical encoding of the effective
    /// regression policy. See this module's "Canonical serialization"
    /// section, and [`policy_sha256`] for the exact encoding.
    #[serde(rename = "policySha256")]
    pub policy_sha256: String,
    /// When the run began.
    #[serde(rename = "startedAt", with = "rfc3339")]
    #[schemars(with = "String")]
    pub started_at: SystemTime,
    /// When the run finished, or `None` while it is still running or if
    /// it failed before finishing. Task 5.2 makes this the load-bearing
    /// signal that a manifest describes an *incomplete* run, so it is
    /// never filled speculatively.
    #[serde(rename = "completedAt", with = "rfc3339_option")]
    #[schemars(with = "Option<String>")]
    pub completed_at: Option<SystemTime>,
}

/// Whether the run a manifest describes is still going, finished, or
/// failed (Task 5.2).
///
/// Deliberately separate from [`RunStage`]: "we are at the install stage"
/// and "we failed at the install stage" are different claims, and a
/// single enum trying to encode both would either lose one of them or
/// double the variant count. Wire values are pinned literals so renaming
/// a Rust variant cannot change what a written manifest says.
///
/// This carries no verdict. Whether the regression *policy* passed is
/// `admissionlab-report`'s `result.json`, not this document:
/// [`RunStatus::Completed`] means "the run finished", never "the run
/// passed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RunStatus {
    /// The run is still executing (or was interrupted so abruptly that
    /// nothing got to record an outcome — a `SIGKILL`, a lost machine).
    #[serde(rename = "in_progress")]
    InProgress,
    /// Every stage finished. [`RunManifest::completed_at`] is `Some` for
    /// exactly this variant and no other.
    #[serde(rename = "completed")]
    Completed,
    /// A stage failed; [`RunManifest::stage`] names which.
    #[serde(rename = "failed")]
    Failed,
}

/// The stages of a run, in execution order (Task 5.2).
///
/// Read together with [`RunManifest::status`]:
///
/// - [`RunStatus::InProgress`] — the last stage that **completed**. A
///   manifest at [`RunStage::Started`] has recorded its inputs, tools,
///   and node images and has not yet created a cluster.
/// - [`RunStatus::Failed`] — the stage that **failed**.
/// - [`RunStatus::Completed`] — always [`RunStage::Completed`].
///
/// Wire values are pinned snake-case literals, matching how
/// `admissionlab-normalize`'s `RuleTier` pins its own text form and for
/// the same reason: a variant rename must never silently change a
/// checked-in golden file or a consumer's parser.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum RunStage {
    /// The run workspace exists and every pre-cluster input has been
    /// recorded: configuration, fixtures, expectations, normalization,
    /// policy, host, tool versions, and both sides' resolved node images.
    /// Nothing has been provisioned.
    #[serde(rename = "started")]
    Started,
    /// Both ephemeral clusters were created.
    #[serde(rename = "cluster_creation")]
    ClusterCreation,
    /// Both sides' component stacks were installed and became ready.
    #[serde(rename = "installation")]
    Installation,
    /// Every fixture was replayed through both sides and its evidence
    /// written.
    #[serde(rename = "fixture_capture")]
    FixtureCapture,
    /// The configured Gateway suite was applied to both sides, every
    /// route's reconciliation was observed, and every traffic probe was
    /// sent or explicitly skipped. Absent from a run whose configuration
    /// has no `gateway:` section, which never enters this stage at all.
    #[serde(rename = "gateway_suite")]
    GatewaySuite,
    /// Both sides' evidence was normalized, compared, and graded.
    #[serde(rename = "comparison")]
    Comparison,
    /// The run's reports were rendered and written.
    #[serde(rename = "reporting")]
    Reporting,
    /// The run finished. Paired only with [`RunStatus::Completed`].
    #[serde(rename = "completed")]
    Completed,
}

impl RunStage {
    /// This stage's stable text form, identical to its serialized value.
    ///
    /// Exposed so a caller can name a stage in a console message or a
    /// diagnostic without re-deriving the spelling (and without going
    /// through `serde`), the same way `RuleTier::as_str` does upstream.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::ClusterCreation => "cluster_creation",
            Self::Installation => "installation",
            Self::FixtureCapture => "fixture_capture",
            Self::GatewaySuite => "gateway_suite",
            Self::Comparison => "comparison",
            Self::Reporting => "reporting",
            Self::Completed => "completed",
        }
    }
}

impl std::fmt::Display for RunStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The machine a run executed on.
///
/// Exactly the two values Rust can state without probing anything: the
/// target OS and architecture this binary was compiled for. Deliberately
/// not a hostname, a username, a kernel build string, or an IP address —
/// none of those help reproduce a run, and every one of them is a small
/// privacy leak in a document users attach to public bug reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HostProvenance {
    /// The target operating system, from [`std::env::consts::OS`] (for
    /// example `"linux"`, `"macos"`).
    #[serde(rename = "os")]
    pub os: String,
    /// The target architecture, from [`std::env::consts::ARCH`] (for
    /// example `"x86_64"`, `"aarch64"`).
    #[serde(rename = "arch")]
    pub arch: String,
}

impl HostProvenance {
    /// The host this binary is running on.
    ///
    /// Compile-time constants rather than a runtime probe: what matters
    /// for reproduction is which target Admission Lab was built for,
    /// which is also the only thing that can be reported without running
    /// anything.
    #[must_use]
    pub fn detect() -> Self {
        Self {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
        }
    }
}

/// The external tools this run used, by self-reported version.
///
/// Every field is `Option` — see this module's "Honest absence" section
/// for why, and [`ToolProvenance::from_doctor_report`] for where the
/// values come from. The versions are recorded *verbatim* as each tool
/// printed them (`kind`'s `v0.33.0`, `helm`'s `v3.16.2`, `kubectl`'s
/// `v1.36.4`), leading `v` and all: normalizing them here would make the
/// manifest disagree with what a user sees when they run the same probe
/// by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolProvenance {
    /// `kind`'s version.
    #[serde(rename = "kind")]
    pub kind: Option<String>,
    /// `kubectl`'s client version.
    #[serde(rename = "kubectl")]
    pub kubectl: Option<String>,
    /// `helm`'s version.
    #[serde(rename = "helm")]
    pub helm: Option<String>,
    /// The Docker *server* (daemon) version — what `docker version
    /// --format {{json .Server.Version}}` reports, which is the version
    /// that actually runs the ephemeral nodes, not the client binary's.
    #[serde(rename = "docker")]
    pub docker: Option<String>,
}

impl ToolProvenance {
    /// Reads the four tool versions out of the host probe a run already
    /// performed.
    ///
    /// Deliberately sourced from [`DoctorReport`] rather than re-probing:
    /// `admissionlab test` runs the prerequisite check before it creates
    /// anything, so re-running four subprocesses to fill the manifest
    /// would both cost time and risk recording a *different* version than
    /// the one the run was gated on.
    ///
    /// A tool absent from `report` (which [`crate::collect_doctor_report`]
    /// never produces, but a hand-built report could) yields `None`,
    /// identically to a tool that was found but whose version could not
    /// be read.
    #[must_use]
    pub fn from_doctor_report(report: &DoctorReport) -> Self {
        let version = |name: ToolName| {
            report
                .tool(name)
                .and_then(|status| status.version.as_ref())
                .cloned()
        };
        Self {
            kind: version(ToolName::Kind),
            kubectl: version(ToolName::Kubectl),
            helm: version(ToolName::Helm),
            docker: version(ToolName::Docker),
        }
    }
}

/// One side's environment, as provisioned rather than as requested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentProvenance {
    /// The Kubernetes version this side ran, as configured and resolved
    /// (for example `"1.36.4"`).
    #[serde(rename = "kubernetesVersion")]
    pub kubernetes_version: String,
    /// The node image reference **without** its digest (for example
    /// `"kindest/node:v1.36.4"`), as split out by
    /// [`split_node_image_reference`].
    #[serde(rename = "nodeImage")]
    pub node_image: String,
    /// The node image's content digest (for example `"sha256:099e04..."`),
    /// or `None` when the backend did not digest-pin the reference. See
    /// this module's "Honest absence" section.
    #[serde(rename = "nodeImageDigest")]
    pub node_image_digest: Option<String>,
    /// This side's components, in install order. Empty before the install
    /// stage has run — Task 5.2's `stage` field is what distinguishes
    /// "nothing installed yet" from "this side genuinely has no
    /// components".
    #[serde(rename = "components")]
    pub components: Vec<ComponentProvenance>,
}

/// One installed component's identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentProvenance {
    /// The component's name, as written in the lab configuration.
    #[serde(rename = "name")]
    pub name: String,
    /// The version actually installed. For a Helm component this is the
    /// pinned chart version (`admissionlab-spec` guarantees a Helm
    /// version is an exact pin, never a floating range); after the
    /// install stage it is `InstalledComponent::resolved_version`, which
    /// the installer confirms against the cluster where it can.
    #[serde(rename = "version")]
    pub version: String,
    /// A content hash of the component's *source*, when one is
    /// obtainable, and `None` otherwise — never `""`.
    ///
    /// `None` for every component today, and that is honest rather than
    /// unimplemented. A pinned Helm chart's content hash is not knowable
    /// without fetching and hashing the chart, which no stage of a run
    /// currently does; a raw-manifests component *does* have one —
    /// `admissionlab_installer::ManifestBundle::source_hash`, computed at
    /// install time — but that value does not cross the
    /// [`crate::StackInstaller`] boundary into
    /// [`crate::InstalledComponent`], and widening that trait is
    /// `admissionlab-installer`'s change to make, not this module's.
    /// That is the seam to fill this field through when it is made.
    #[serde(rename = "sourceSha256")]
    pub source_sha256: Option<String>,
}

/// Splits a node image reference into its tag part and its digest part.
///
/// A `kind` node image resolved from `compatibility/kubernetes.yaml`
/// arrives as `"kindest/node:v1.36.4@sha256:099e04..."` — one string,
/// because that is what `kind --image` takes. A manifest records the two
/// halves separately so a reader can see at a glance whether the run was
/// digest-pinned at all, and so Task 5.3 can compare digests without
/// re-parsing.
///
/// Splits at the **first** `@`, since a digest is always the final
/// component of an OCI reference and a repository name may not contain
/// `@` at all. A reference with no `@`, or one whose digest half is
/// empty, yields `None` for the digest rather than an empty string (see
/// this module's "Honest absence" section).
#[must_use]
pub fn split_node_image_reference(reference: &str) -> (String, Option<String>) {
    match reference.split_once('@') {
        Some((image, digest)) if !digest.is_empty() => (image.to_owned(), Some(digest.to_owned())),
        Some((image, _)) => (image.to_owned(), None),
        None => (reference.to_owned(), None),
    }
}

/// Returns the SHA-256 digest of `bytes` as lowercase hex.
///
/// The manifest's file-derived digests (`config_sha256`,
/// `expectations_sha256`) are this function over the file's own bytes.
/// Identical in behavior to `admissionlab-fixtures`'s own private
/// `sha256_hex`, deliberately: a fixture hash recorded in
/// `fixture_hashes` and a configuration hash recorded next to it must be
/// the same function, or comparing them across crates means nothing.
///
/// **Provenance, never authentication.** This is a content-change
/// detector for a document that sits next to the content it describes; it
/// is unkeyed and gives no integrity guarantee against anyone who can
/// also rewrite the manifest.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Returns the SHA-256 digest of `path`'s bytes as lowercase hex.
///
/// **The one implementation of "a digest over a file" in this
/// workspace.** Rule 1 of this module's "Canonical serialization" section
/// says a file-derived digest hashes that file's own bytes exactly as
/// read from disk; this is that rule as a function, so the code that
/// *writes* `config_sha256`/`expectations_sha256` into a manifest
/// (`admissionlab-cli`'s `pipeline::provenance::input_digests`) and the
/// code that *checks* them back on a reproduction
/// ([`crate::reproduce::plan_reproduction`]) cannot drift apart. A second
/// `sha256_hex(&fs::read(..)?)` anywhere would be two implementations of
/// one rule, and the pair that must agree is exactly the pair that would
/// stop agreeing.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] if `path` could not be
/// read. Never degraded to an absent digest: a caller that could not read
/// an input has not verified it, and reporting "no digest" would let a
/// vanished file read as an honest absence.
pub fn file_sha256(path: &Path) -> std::io::Result<String> {
    Ok(sha256_hex(&std::fs::read(path)?))
}

/// Returns the SHA-256 digest, as lowercase hex, of `value`'s canonical
/// JSON encoding.
///
/// The canonical encoding is defined in this module's "Canonical
/// serialization" section and is exactly: `serde_json::to_value` (which
/// puts every object's keys in lexicographic order, because
/// `serde_json::Map` is `BTreeMap`-backed in this workspace) followed by
/// `serde_json::to_vec` (compact, UTF-8, no trailing newline). Going
/// through `Value` first is what makes key order independent of Rust
/// struct field declaration order; serializing the value directly would
/// hash declaration order instead, and a field reordering would then
/// silently change every digest.
///
/// # Errors
///
/// Returns the underlying [`serde_json::Error`] if `value` cannot be
/// represented as JSON — for a non-string map key, or a `Serialize`
/// implementation that fails. Propagated rather than swallowed: a digest
/// that could not be computed must never be reported as some other
/// digest.
pub fn canonical_sha256<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let canonical = serde_json::to_value(value)?;
    Ok(sha256_hex(&serde_json::to_vec(&canonical)?))
}

/// The effective normalization profile a run applied, in the shape this
/// crate hashes it in.
///
/// This mirrors `admissionlab_normalize::NormalizationProfile` rather
/// than reusing it, for the reason `run.rs`'s module documentation gives
/// at length for [`crate::StackInstaller`] and [`crate::FixtureCapture`]:
/// `admissionlab-normalize` sits *above* this crate
/// (`normalize -> admission -> fixtures -> core`), so naming its types
/// here would close a dependency cycle Cargo rejects. The conversion
/// therefore lives in the assembler that already depends on both —
/// `admissionlab-cli`'s `pipeline::compare` — which is the same place the
/// recipe-rule → engine-rule conversion already lives, and for the same
/// documented reason.
///
/// The three tiers are hashed in the order they are applied
/// (`built_in`, then `recipe`, then `user`) and each tier's rules keep
/// their `Vec` order, because both orders change the normalized result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectiveNormalization {
    /// Admission Lab's own built-in rules.
    #[serde(rename = "builtIn")]
    pub built_in: Vec<NormalizationRuleRecord>,
    /// Rules contributed by the recipes backing the stacks under test.
    #[serde(rename = "recipe")]
    pub recipe: Vec<NormalizationRuleRecord>,
    /// Rules the user wrote themselves.
    #[serde(rename = "user")]
    pub user: Vec<NormalizationRuleRecord>,
}

/// One normalization rule, as recorded for hashing.
///
/// Externally tagged on a pinned `rule` discriminator whose values are
/// written out literally (`"removePointer"`, and so on) rather than
/// derived from the Rust variant names: a variant rename must never
/// silently change `normalization_sha256` for an unchanged profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "rule", deny_unknown_fields)]
pub enum NormalizationRuleRecord {
    /// Remove the value at an RFC 6901 JSON Pointer.
    #[serde(rename = "removePointer")]
    RemovePointer {
        /// The pointer whose value is removed.
        #[serde(rename = "pointer")]
        pointer: String,
    },
    /// Stably sort the array of objects at a pointer by each element's
    /// `key`.
    #[serde(rename = "sortNamedArray")]
    SortNamedArray {
        /// The pointer addressing the array.
        #[serde(rename = "pointer")]
        pointer: String,
        /// The element key sorted on.
        #[serde(rename = "key")]
        key: String,
    },
    /// Remove one top-level `metadata.annotations` entry by its literal,
    /// unescaped key.
    #[serde(rename = "removeAnnotation")]
    RemoveAnnotation {
        /// The annotation key removed.
        #[serde(rename = "annotation")]
        annotation: String,
    },
}

/// Computes [`RunManifest::normalization_sha256`] for `normalization`.
///
/// Infallible, unlike the general [`canonical_sha256`]: every field of
/// [`EffectiveNormalization`] is a `String` or a `Vec` of them, so its
/// JSON encoding cannot fail. The `expect` below is therefore unreachable
/// rather than optimistic, and is written that way (with this note) so a
/// future field whose serialization *can* fail forces this function's
/// signature to change rather than panicking in production.
///
/// # Panics
///
/// Never, for any [`EffectiveNormalization`] that can be constructed:
/// see above.
#[must_use]
pub fn normalization_sha256(normalization: &EffectiveNormalization) -> String {
    canonical_sha256(normalization)
        .expect("EffectiveNormalization contains only strings and cannot fail to serialize")
}

/// Computes [`RunManifest::policy_sha256`] for `policy`.
///
/// [`admissionlab_spec::PolicySpec`] implements `Deserialize` but not
/// `Serialize` (it is parsed from YAML and never written back), so this
/// builds the canonical value explicitly instead of deriving it. That is
/// not merely a workaround: writing the encoding out by hand is what lets
/// every wire key be pinned here, in the one place the digest is defined,
/// so a future rename in `admissionlab-spec`'s YAML vocabulary cannot
/// silently change the digest of a policy that did not change.
///
/// The encoding, field by field:
///
/// - `failOn`: the fail-on set, already lexicographically ordered by
///   being a [`std::collections::BTreeSet`].
/// - `overrides`: each override as an object, **in declaration order** —
///   overrides are evaluated in order, so reordering them is a real
///   change. Absent restrictions are `null`, not omitted, so
///   "unrestricted" and "restricted to nothing" can never collide.
/// - `latency`: the absolute threshold as whole milliseconds (the unit
///   users write it in) and the relative multiplier as a JSON number.
///
/// A multiplier that is not finite cannot be encoded as JSON; a
/// configuration carrying one is rejected long before this point by
/// `admissionlab-spec`'s own validation, and the fallback below encodes
/// it as `null` rather than panicking, so a hypothetical
/// hand-constructed `PolicySpec` still produces *a* digest rather than
/// aborting a run.
///
/// # Panics
///
/// Never: the value hashed is a [`serde_json::Value`] this function just
/// built, and re-encoding an already-valid JSON value cannot fail.
#[must_use]
pub fn policy_sha256(policy: &PolicySpec) -> String {
    let overrides: Vec<serde_json::Value> = policy
        .overrides
        .iter()
        .map(|entry| {
            serde_json::json!({
                "kind": entry.kind,
                "fixtures": entry.fixtures,
                "subject": entry.subject,
                "path": entry.path,
                "severity": entry.severity,
            })
        })
        .collect();

    let canonical = serde_json::json!({
        "failOn": policy.fail_on,
        "overrides": overrides,
        "latency": {
            "absoluteIncreaseMillis": u64::try_from(
                policy.latency.absolute_increase.as_millis()
            ).unwrap_or(u64::MAX),
            "relativeMultiplier": serde_json::Number::from_f64(
                policy.latency.relative_multiplier
            ),
        },
    });

    canonical_sha256(&canonical)
        .expect("a serde_json::Value built here is already valid JSON and cannot fail to re-encode")
}

/// Owns one run's `run.json` and keeps it truthful as the run proceeds
/// (Task 5.2).
///
/// See this module's "Incremental writes and partial provenance" section
/// for the contract. In short: [`RunManifestWriter::create`] writes a
/// complete pre-cluster manifest before anything is provisioned, and
/// every later method mutates the in-memory manifest and rewrites the
/// file in one call, so the file can never lag the value the run is
/// actually working from.
///
/// # Atomicity, and what a crash leaves behind
///
/// Every write goes through [`ArtifactStore::write_json_atomic`], so a
/// reader (or a crash) always observes either the previous manifest in
/// full or the new one in full — never a truncated document. A process
/// killed between two writes therefore leaves the *previous* stage's
/// manifest intact, with `status: in_progress`, which is exactly the
/// honest statement: something was still running and never reported an
/// outcome.
///
/// # This type never decides anything
///
/// It advances no stage on its own and never infers a status from what it
/// was told. The caller — which is the only thing that knows whether a
/// stage succeeded — names the stage and the outcome explicitly. That is
/// deliberate: a writer that guessed "the next stage must be
/// installation" would silently record a stage that never ran the moment
/// the pipeline's order changed.
#[derive(Debug)]
pub struct RunManifestWriter {
    /// The store every write is routed through, for its atomicity and
    /// path-containment guarantees.
    store: ArtifactStore,
    /// Where this run's manifest lives: [`RunPaths::run_json`].
    path: PathBuf,
    /// The current manifest, always equal to what was last written.
    manifest: RunManifest,
}

impl RunManifestWriter {
    /// Writes `manifest` to `paths`' [`RunPaths::run_json`] and returns a
    /// writer that keeps it up to date.
    ///
    /// `manifest` is written exactly as given — this does not stamp a
    /// stage or a status onto it, for the reason this type's "never
    /// decides anything" note gives. A caller creating a pre-cluster
    /// manifest sets [`RunStatus::InProgress`] and [`RunStage::Started`]
    /// itself.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError`] if the manifest could not be serialized
    /// or written. A caller should treat this as fatal *before* creating
    /// any cluster: it means the run's own artifact directory is not
    /// writable, so nothing later in the run would be recoverable either
    /// — and at that point nothing has been provisioned, so failing costs
    /// nothing.
    pub async fn create(
        store: ArtifactStore,
        paths: &RunPaths,
        manifest: RunManifest,
    ) -> Result<Self, ArtifactError> {
        let writer = Self {
            store,
            path: paths.run_json().to_path_buf(),
            manifest,
        };
        writer.write().await?;
        Ok(writer)
    }

    /// The manifest as last written.
    #[must_use]
    pub fn manifest(&self) -> &RunManifest {
        &self.manifest
    }

    /// Where this run's manifest is written.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records that `stage` completed, applying `update` to the manifest
    /// first, and rewrites the file.
    ///
    /// Mutation and write are one call rather than a `manifest_mut()`
    /// accessor plus a separate `flush()`, so there is no window in which
    /// the in-memory manifest and the file disagree, and no way for a
    /// caller to update one and forget the other.
    ///
    /// Leaves [`RunManifest::status`] at [`RunStatus::InProgress`] and
    /// [`RunManifest::completed_at`] at `None`: only
    /// [`RunManifestWriter::complete`] may set those, because only the
    /// end of the run knows the run ended.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError`] if the rewrite failed. A caller should
    /// treat this as non-fatal and report it: a manifest that stopped
    /// being updated is a lesser problem than abandoning a run whose
    /// clusters are already up and whose comparison may still succeed.
    pub async fn record<F>(&mut self, stage: RunStage, update: F) -> Result<(), ArtifactError>
    where
        F: FnOnce(&mut RunManifest),
    {
        update(&mut self.manifest);
        self.manifest.status = RunStatus::InProgress;
        self.manifest.stage = stage;
        self.write().await
    }

    /// Records that `stage` **failed**: [`RunStatus::Failed`], the stage
    /// named, and [`RunManifest::completed_at`] left `None`.
    ///
    /// `completed_at` is explicitly cleared rather than merely left
    /// alone, so this is safe to call from a path that might follow a
    /// [`RunManifestWriter::complete`] — a failed run must never carry a
    /// completion time (Global Constraint 15).
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError`] if the rewrite failed. Report it; never
    /// let it replace the failure being recorded.
    pub async fn fail(&mut self, stage: RunStage) -> Result<(), ArtifactError> {
        self.manifest.status = RunStatus::Failed;
        self.manifest.stage = stage;
        self.manifest.completed_at = None;
        self.write().await
    }

    /// Records that the run finished at `completed_at`:
    /// [`RunStatus::Completed`], [`RunStage::Completed`], and a real
    /// completion timestamp.
    ///
    /// "Finished" is not "passed" — see [`RunStatus`]. A run whose policy
    /// failed still completed, and its manifest says so; the verdict
    /// lives in `result.json`.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError`] if the rewrite failed.
    pub async fn complete(&mut self, completed_at: SystemTime) -> Result<(), ArtifactError> {
        self.manifest.status = RunStatus::Completed;
        self.manifest.stage = RunStage::Completed;
        self.manifest.completed_at = Some(completed_at);
        self.write().await
    }

    /// Atomically writes the current manifest to [`Self::path`].
    async fn write(&self) -> Result<(), ArtifactError> {
        self.store
            .write_json_atomic(&self.path, &self.manifest)
            .await
    }
}

/// Generates the JSON Schema for [`RunManifest`], derived from the same
/// derives that govern its serialization.
///
/// See this module's "Schema" section, and `admissionlab-spec`'s
/// `schema.rs` for why generating this twice always produces
/// byte-for-byte identical output.
#[must_use]
pub fn run_manifest_v1alpha1_json_schema() -> schemars::Schema {
    schemars::schema_for!(RunManifest)
}

/// RFC 3339 encoding for a required [`SystemTime`] field. See this
/// module's "Timestamps" section for the exact form and why it is fixed
/// at nine fractional digits.
mod rfc3339 {
    use std::time::SystemTime;

    use serde::{Deserialize as _, Deserializer, Serializer, de::Error as _};

    /// Renders `time` as `YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ`.
    ///
    /// # Errors
    ///
    /// Fails if `time` lies outside the range `jiff::Timestamp` can
    /// represent (roughly years -9999 to 9999). Reported rather than
    /// clamped: a manifest that silently recorded a different instant
    /// than the one observed would be worse than one that failed to
    /// write.
    pub(super) fn serialize<S: Serializer>(
        time: &SystemTime,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let timestamp = jiff::Timestamp::try_from(*time).map_err(serde::ser::Error::custom)?;
        // `{:.9}` pins the fractional precision; jiff's own `Display`
        // otherwise trims trailing zeroes, which would make a manifest's
        // width depend on the instant it happened to record.
        serializer.serialize_str(&format!("{timestamp:.9}"))
    }

    /// Parses an RFC 3339 timestamp back into a [`SystemTime`].
    ///
    /// Accepts any offset and any fractional precision `jiff` accepts,
    /// not only the exact form [`serialize`] writes: a manifest a human
    /// hand-edited to `...T12:00:00Z` still round-trips to the same
    /// instant.
    ///
    /// # Errors
    ///
    /// Fails if the value is not a string, or is not a parseable RFC 3339
    /// timestamp.
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SystemTime, D::Error> {
        let text = String::deserialize(deserializer)?;
        let timestamp: jiff::Timestamp = text.parse().map_err(D::Error::custom)?;
        Ok(SystemTime::from(timestamp))
    }
}

/// RFC 3339 encoding for an optional [`SystemTime`] field, where `None`
/// is JSON `null`.
///
/// A separate module rather than a `#[serde(with)]` on the inner type,
/// because `serde`'s `with` attribute applies to the field's *whole*
/// type: `Option<SystemTime>` needs its own pair of functions that handle
/// the `null` case before delegating.
mod rfc3339_option {
    use std::time::SystemTime;

    use serde::{Deserialize as _, Deserializer, Serializer};

    /// Renders `Some(time)` exactly as [`super::rfc3339::serialize`]
    /// does, and `None` as JSON `null`.
    ///
    /// # Errors
    ///
    /// Fails for the same reason [`super::rfc3339::serialize`] does.
    // `&Option<T>` rather than clippy's preferred `Option<&T>`: `serde`'s
    // `#[serde(with = "...")]` attribute calls this with a reference to
    // the field itself, so the signature is fixed by serde's contract and
    // not free to be made more idiomatic.
    #[allow(clippy::ref_option)]
    pub(super) fn serialize<S: Serializer>(
        time: &Option<SystemTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match time {
            Some(time) => super::rfc3339::serialize(time, serializer),
            None => serializer.serialize_none(),
        }
    }

    /// Parses `null` as `None` and a string as `Some`.
    ///
    /// # Errors
    ///
    /// Fails if a present value is not a parseable RFC 3339 timestamp.
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<SystemTime>, D::Error> {
        let text = Option::<String>::deserialize(deserializer)?;
        match text {
            Some(text) => {
                let timestamp: jiff::Timestamp = text
                    .parse()
                    .map_err(<D::Error as serde::de::Error>::custom)?;
                Ok(Some(SystemTime::from(timestamp)))
            }
            None => Ok(None),
        }
    }
}
