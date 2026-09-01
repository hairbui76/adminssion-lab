#![forbid(unsafe_code)]

pub mod artifact;
pub mod cache;
pub mod cluster;
pub mod diagnostic;
pub mod error;
pub mod ids;
pub mod process;
pub mod reproduce;
pub mod run;
pub mod run_manifest;
pub mod side;
pub mod timing;
pub mod tool;

pub use artifact::{ArtifactError, ArtifactStore, RunDisposition, RunPaths};
pub use cache::{
    CACHE_DIR_ENV, CacheError, CacheLookup, CacheMiss, CachePaths, ContentKey, default_cache_root,
};
pub use cluster::{
    ClusterDiagnostics, ClusterError, ClusterHandle, ClusterManager, ClusterSpec, RollbackOutcome,
};
pub use diagnostic::{Diagnostic, DiagnosticLevel, RedactedValue};
pub use error::IdParseError;
pub use ids::{FixtureId, RunId};
pub use process::{
    CommandContext, CommandResult, CommandSpec, MAX_CAPTURED_STREAM_BYTES, MAX_LINE_BYTES,
    ManagedChild, ProcessError, ProcessRunner, ProcessSpawner, TokioProcessRunner,
    env_key_looks_sensitive,
};
pub use reproduce::{
    DEFAULT_LAB_FILE_NAME, DiscoveredFixture, EffectiveMismatch, FixtureVerification,
    ReproduceError, ReproducePlan, ReproductionPin, SidePin, UNCONFIRMED_COMPONENT_VERSION,
    VerifiedInput, incomplete_run_warning, plan_reproduction, plan_reproduction_from_config,
    verify_effective_digests, verify_fixtures,
};
pub use run::{
    CapturedFixture, CapturedLab, ClusterCreationFailure, FixtureCapture, FixtureCaptureError,
    FixtureCaptureFailure, InstalledComponent, InstalledLab, LabRunner, PreparedLab,
    ResolvedNodeImages, RunError, RunOptions, SideCapture, SideInstall, StackInstallError,
    StackInstallFailure, StackInstaller, preserved_cluster_report,
};
pub use run_manifest::{
    ComponentProvenance, EffectiveNormalization, EnvironmentProvenance, GatewayProvenance,
    HostProvenance, ManifestReadError, NormalizationRuleRecord, RunManifest, RunManifestWriter,
    RunStage, RunStatus, SUPPORTED_SCHEMA_VERSIONS, ToolProvenance, canonical_sha256, file_sha256,
    normalization_sha256, policy_sha256, read_run_manifest, run_manifest_v1beta1_json_schema,
    sha256_hex, split_node_image_reference,
};
pub use side::Side;
pub use timing::{
    CaptureStage, ComponentTiming, InstallStage, SideInstallTiming, SideStage, StageScope,
    StageTimings, TimedClusterManager, TimedFixtureCapture, TimedSideStage, TimedStackInstaller,
    TimedStage, TimingRecorder,
};
pub use tool::{
    DISK_WARNING_THRESHOLD_BYTES, DoctorReport, ToolName, ToolStatus, collect_doctor_report,
    disk_space_warning, probe_tool,
};

/// Returns this crate's package identity.
///
/// Used by the workspace smoke test to prove that `admissionlab-core`
/// builds, links, and is callable from another crate in the workspace.
#[must_use]
pub const fn crate_identity() -> &'static str {
    "admissionlab-core"
}
