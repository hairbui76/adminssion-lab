//! [`ManifestsInstaller`]: the raw-Kubernetes-manifest-backed
//! [`ComponentInstaller`] (Task 2.3). Installs one
//! [`admissionlab_spec::ManifestInstallSpec`] onto a cluster by shelling
//! out to `kubectl` through [`admissionlab_core::ProcessRunner`] — never
//! `std::process::Command`/`tokio::process::Command` directly, and never
//! a shell (Global Constraint 12) — and reports what happened as an
//! [`InstallRecord`].
//!
//! [`load_manifest_bundle`] is this module's other public surface: it
//! reads and parses every manifest file a component's install method
//! names, entirely locally, with no cluster interaction at all. It is
//! used both by [`ManifestsInstaller::install`] itself (Task 2.3 brief
//! Step 2: a malformed manifest must fail before anything touches a
//! cluster) and is exported for a later task (run-manifest/fixture
//! provenance, PRODUCT.md §28) to call independently.
//!
//! # Why every `kubectl` invocation is safe by construction
//!
//! Task 2.2's Helm installer found, in review, that passing an empty
//! environment to `helm` does not stop it from reaching for the
//! operator's real, ambient `~/.config/helm/repositories.yaml` — the
//! child still inherits this process's own environment regardless of
//! what [`admissionlab_core::CommandSpec::env`] adds — and Phase 1 found
//! the same shape of bug for `kind delete` reaching for
//! `~/.kube/config`. `kubectl` is a third instance of exactly this
//! exposure, and for `kubectl` the stakes are the highest of the three:
//! with no `--kubeconfig` and no `KUBECONFIG` set, `kubectl` reads the
//! operator's real `~/.kube/config` and acts against their *current
//! context* — which on a real operator's machine is a real,
//! non-Admission-Lab cluster. PRODUCT.md §29.2 requires v1 to never need
//! production cluster credentials, and §8's whole premise is that the
//! lab is disposable and isolated; a `kubectl apply` that silently
//! reached a real cluster would violate both.
//!
//! This module closes that off structurally rather than by convention:
//! [`ManifestsInstaller::kubectl_command`] is the *only* place in this
//! module — and, since [`apply_args`] never accepts a kubeconfig
//! parameter at all, the only place in this crate — that assembles
//! `kubectl` argv into a runnable [`CommandSpec`]. It unconditionally
//! appends `--kubeconfig <cluster's own kubeconfig>` itself, every
//! single time, with no parameter to opt out. Unlike the Helm
//! installer's own chokepoint (which must leave `--kubeconfig` to each
//! call site, because `helm repo add` is a local, cluster-independent
//! operation that must never receive it), *every* `kubectl` invocation
//! this module ever makes operates on a live cluster, so there is no
//! such exception to carve out here. `--kubeconfig` selection also never
//! goes through the `KUBECONFIG` environment variable:
//! [`ManifestsInstaller::kubectl_command`] sets no environment overrides
//! at all. A future call site cannot silently reintroduce the original
//! class of defect: doing so would require hand-building a
//! [`CommandSpec`] directly rather than calling this method, a
//! deliberate departure from how the one existing call site
//! ([`ManifestsInstaller::apply_file`]) works, not an easy oversight.
//! `tests/manifests_unit.rs`'s
//! `every_kubectl_invocation_carries_kubeconfig_pointing_at_the_clusters_own_path`
//! makes this a regression-proof property rather than merely an
//! inspection of the code.
//!
//! # Manifest loading: order, duplicates, and hashing
//!
//! [`load_manifest_bundle`] treats each entry of
//! [`admissionlab_spec::ManifestInstallSpec::paths`] as one manifest
//! *file* (never a directory — see the "Scope" section below), reads
//! it, and parses it as either YAML or JSON by its extension (`.json`,
//! case-insensitively, selects JSON; anything else — including `.yaml`,
//! `.yml`, or no extension at all — selects YAML, since YAML is the
//! predominant Kubernetes manifest format and a syntactic superset of
//! JSON for a single document). A YAML file may contain multiple
//! `---`-separated documents, each becoming its own entry in
//! [`ManifestBundle::documents`]; a trailing empty document (a file
//! ending in a bare `---` with nothing after it) is silently dropped
//! rather than surfacing as a spurious null entry, matching how
//! Kubernetes's own YAML-splitting tooling treats it. A JSON file always
//! contributes exactly one document, whatever shape it parses to
//! (including a `kind: List`, which this module does not itself unpack
//! — `kubectl apply` already does that at apply time).
//!
//! **Order: the caller's declared order, never sorted.** Applying
//! manifests is not commutative — a `Namespace` must exist before a
//! `Deployment` inside it, and a `CustomResourceDefinition` must exist
//! before a custom resource of that kind — so the sequence a user wrote
//! in their configuration's `paths` list is meaningful, deliberate
//! ordering information, not an arbitrary set. [`load_manifest_bundle`]
//! therefore preserves `paths`'s given order exactly (after the
//! deduplication described below) — the same choice the Helm installer
//! already makes for its own `valuesFiles` — and
//! [`ManifestsInstaller::install`] applies files in that same order, one
//! `kubectl apply` invocation at a time, so an earlier file's objects
//! are fully persisted before a later file that may depend on them is
//! ever sent.
//!
//! **Duplicates: exact repeats are read and applied once, at their
//! first position.** If the same resolved absolute path appears more
//! than once in `paths` — in practice, a copy-paste mistake in the
//! configuration file — [`deduplicate_paths`] keeps only its first
//! occurrence. Re-reading and reapplying byte-identical content a second
//! time adds no information, and would make [`ManifestBundle::source_hash`]
//! sensitive to how many times a path happens to be repeated — which
//! would make two configurations with identical *effective* manifest
//! sets hash differently for a reason with no semantic meaning.
//! "Duplicate" here means an exact match of the resolved path string;
//! two different paths whose *content* happens to be identical (for
//! example, one a symlink to the other) are not deduplicated —
//! detecting that would need a `canonicalize`/inode comparison this
//! task's brief does not ask for, and a real configuration is unlikely
//! to produce that shape by accident the way a literal repeated line in
//! a `paths:` list can happen.
//!
//! **`source_hash`: SHA-256 of canonical source bytes, lowercase hex,
//! never including the paths themselves.** For each deduplicated file,
//! in order, its raw on-disk bytes (exactly as read — never a
//! re-serialization of the parsed documents, so YAML formatting choices
//! like comments, key order, or indentation style never change the
//! hash) are fed to the hasher, each length-prefixed so that, for
//! example, hashing the two files `("ab", "c")` can never collide with
//! hashing `("a", "bc")`. Deliberately **not** mixed in: the paths'
//! own text. Two machines (or two runs on the same machine at a
//! different checkout location) can resolve byte-identical
//! configuration checkouts to different absolute paths, and this hash
//! must stay stable "across runs and machines for the same inputs" for
//! run-manifest provenance (PRODUCT.md §28) — hashing path strings would
//! break that for no benefit, since the hash's job is to prove *which
//! manifest content* was applied, not *which filesystem layout* happened
//! to hold it. This is the same SHA-256/lowercase-hex convention Task
//! 3.1 uses for fixtures; the hash is for provenance and matching
//! content across runs, never a security authentication token (Global
//! Constraint 15's "unavailable data is unknown, never fabricated" is
//! why this is a real hash of real bytes rather than any kind of
//! placeholder).
//!
//! # Parse-then-apply: Step 2 before Step 3, always
//!
//! [`ManifestsInstaller::install`] calls [`load_manifest_bundle`] (via
//! the private, single-filesystem-pass [`load_manifests`], which returns
//! both the validated bundle and the same deduplicated path list the
//! apply loop below it uses) to completion *before* issuing a single
//! `kubectl` invocation. If any file cannot be read or any document
//! within it fails to parse, [`InstallError::ManifestRead`] or
//! [`InstallError::ManifestParse`] — naming the offending file and (for
//! a parse failure) which document within it — is returned immediately,
//! without reading any path later in the list, and `install` returns
//! without ever touching the cluster.
//!
//! # Apply, one file per invocation
//!
//! Each deduplicated, ordered path becomes its own `kubectl apply
//! --server-side=false -f <path> --kubeconfig <cluster's kubeconfig>`
//! invocation (Task 2.3 brief Step 3), run to completion before the next
//! one starts — never multiple `-f` flags folded into one invocation —
//! so that a failure names exactly one file and one command
//! ([`InstallError::CommandFailed`] carries a single
//! [`admissionlab_core::CommandContext`]), and so a later file that
//! depends on an earlier one is never sent until the earlier one is
//! confirmed applied. `kubectl apply` itself still splits a single
//! file's own multi-document YAML into its constituent objects; this
//! module does not re-split what [`load_manifest_bundle`] already parsed
//! back out for the actual apply call — that parse exists for local
//! validation and hashing, not to hand `kubectl` anything other than the
//! original file.
//!
//! # `--server-side=false`'s known failure mode, made legible
//!
//! Client-side apply (`--server-side=false`) stores the entire applied
//! object in the `kubectl.kubernetes.io/last-applied-configuration`
//! annotation, and Kubernetes caps total `metadata.annotations` size at
//! a hard-coded 262144 bytes — a limit a large `CustomResourceDefinition`
//! can exceed (charts that ship such CRDs, such as Istio's, are
//! installed via Helm rather than raw manifests, which avoids this, but
//! a user's own raw-manifest CRD can still hit it). This task's brief
//! says to implement `--server-side=false` "initially," so that is what
//! this module does — but [`ManifestsInstaller::apply_file`] recognizes
//! Kubernetes's own validation message for this specific failure (see
//! [`looks_like_annotation_size_limit_failure`]) and reports it as
//! [`InstallError::ManifestExceedsAnnotationLimit`] instead of a plain
//! [`InstallError::CommandFailed`], so the cause and the remedy are
//! legible without a reader needing to already know about this
//! Kubernetes limit and go research a raw "Too long" message themselves.
//! This module never silently retries with `--server-side=true` on this
//! (or any) failure: Global Constraint 16 requires Admission Lab to
//! never silently change apply semantics, so the choice of remedy
//! (install this component a different way, or shrink the manifest) is
//! always the user's, made with the full, unmodified `stderr` still
//! attached to the error.
//!
//! # Scope
//!
//! An entry of [`admissionlab_spec::ManifestInstallSpec::paths`] naming
//! a directory rather than a file is out of scope for this task: neither
//! the interface this task implements
//! (`load_manifest_bundle(paths: &[PathBuf])`) nor its brief describe a
//! file-discovery or intra-directory ordering policy, so
//! [`load_manifest_bundle`] simply attempts to read every entry as a
//! file — passing it a directory surfaces as an ordinary
//! [`InstallError::ManifestRead`] (reading a directory as a file fails
//! at the OS level) rather than being silently skipped or expanded.
//!
//! [`ManifestsInstaller`] also does not confirm what actually landed on
//! the cluster the way the Helm installer confirms an installed chart's
//! version via `helm get metadata`: a raw manifest has no independent,
//! authoritative version source to confirm against the way a Helm
//! chart's registry does, so [`InstallRecord::resolved_version`] is
//! simply the resolved component's own declared
//! [`admissionlab_spec::ResolvedComponent::version`] — not a guess
//! standing in for an unconfirmed value (Global Constraint 15), but the
//! only version that concept has for a manifests install: what the user
//! declared *is* what was applied, verbatim, with nothing else to
//! independently corroborate it against.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use admissionlab_core::{ClusterHandle, CommandResult, CommandSpec, ProcessRunner};
use admissionlab_spec::{InstallMethod, ResolvedComponent};
use async_trait::async_trait;
use serde::Deserialize as _;
use sha2::{Digest, Sha256};

use crate::{ComponentInstaller, InstallError, InstallRecord};

/// The `kubectl` program name, resolved via `PATH` — never an absolute
/// path, matching this crate's `helm` module's own convention for
/// external tools.
const KUBECTL_PROGRAM: &str = "kubectl";

/// How long one `kubectl apply --server-side=false -f <file>` invocation
/// may run before it is killed and reported as timed out.
///
/// Applying a raw manifest file is a small, fixed number of synchronous
/// API-server round trips (one per object the file contains) through
/// whatever admission/validating webhook chain is already installed on
/// the cluster — it never waits for an applied object to become ready
/// (Task 2.4 probes readiness separately, once every file in this
/// component has already been applied), so this only needs to cover
/// object persistence, not workload startup. Sized generously for a
/// slow/loaded CI runner and a handful of objects per file rather than
/// tuned to the common near-instant case.
const APPLY_TIMEOUT: Duration = Duration::from_secs(120);

/// Kubernetes's hard-coded total `metadata.annotations` size limit, in
/// bytes (`k8s.io/apimachinery`'s `ValidateAnnotations`). Client-side
/// `kubectl apply` (`--server-side=false`) stores the whole applied
/// object in the `kubectl.kubernetes.io/last-applied-configuration`
/// annotation, so a sufficiently large object (in practice, a large
/// `CustomResourceDefinition`) pushes its total annotations past this
/// limit and the API server rejects it. This exact number is expected to
/// appear verbatim in the API server's own rejection message (it is
/// formatted directly into the message by the validation error
/// builder), which is why [`looks_like_annotation_size_limit_failure`]
/// matches on it rather than on wording that could vary across
/// Kubernetes versions.
const ANNOTATION_SIZE_LIMIT_BYTES: &str = "262144";

/// A fully parsed, locally validated set of Kubernetes manifest
/// documents loaded from a component's
/// [`admissionlab_spec::ManifestInstallSpec::paths`], plus a stable
/// content hash for provenance. See the module documentation's "Manifest
/// loading" section for exactly what is included, in what order, and how
/// [`ManifestBundle::source_hash`] is computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestBundle {
    /// Every parsed manifest document, across every (deduplicated) file
    /// in the source `paths`, in that same deduplicated, caller-declared
    /// order.
    pub documents: Vec<serde_json::Value>,
    /// SHA-256 of every (deduplicated) file's canonical source bytes,
    /// lowercase hex.
    pub source_hash: String,
}

/// The result of [`load_manifests`]: a [`ManifestBundle`] plus the exact
/// deduplicated, ordered path list it was built from — the same list
/// [`ManifestsInstaller::install`] applies, in the same order, without
/// needing to recompute (and thereby risk disagreeing with) the
/// deduplication [`ManifestBundle`] was already validated and hashed
/// against.
struct LoadedManifests {
    bundle: ManifestBundle,
    /// Deduplicated, ordered source file paths.
    paths: Vec<PathBuf>,
}

/// Reads and locally parses every manifest file named in
/// `paths` (Task 2.3 brief Steps 1-2), producing every parsed document
/// plus a stable content hash — entirely without touching a cluster. See
/// the module documentation for the exact ordering, deduplication, and
/// hashing rules.
///
/// # Errors
///
/// Returns [`InstallError::ManifestRead`] if any (deduplicated) path
/// cannot be read, or [`InstallError::ManifestParse`] if any document
/// within a file is not syntactically valid YAML/JSON — in either case,
/// naming the offending file (and, for a parse failure, which document
/// within it), without reading any path later in the list.
pub fn load_manifest_bundle(paths: &[PathBuf]) -> Result<ManifestBundle, InstallError> {
    Ok(load_manifests(paths)?.bundle)
}

/// The single-filesystem-pass implementation behind both
/// [`load_manifest_bundle`] and [`ManifestsInstaller::install`]: reads
/// and hashes each deduplicated path's raw bytes exactly once, and
/// parses those same bytes into [`ManifestBundle::documents`].
fn load_manifests(paths: &[PathBuf]) -> Result<LoadedManifests, InstallError> {
    let ordered_paths = deduplicate_paths(paths);

    let mut documents = Vec::new();
    let mut hasher = Sha256::new();
    for path in &ordered_paths {
        let bytes = fs::read(path).map_err(|source| InstallError::ManifestRead {
            path: path.clone(),
            source,
        })?;

        // Length-prefixed so that, for example, hashing the two files
        // ("ab", "c") can never produce the same digest as hashing
        // ("a", "bc") — see the module documentation's `source_hash`
        // section. `usize -> u64` cannot truncate on any platform this
        // crate targets, but this avoids an `as` cast rather than
        // relying on that (mirroring `admissionlab_core::tool`'s own
        // `available_bytes`).
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        hasher.update(length.to_le_bytes());
        hasher.update(&bytes);

        documents.extend(parse_manifest_file(path, &bytes)?);
    }

    Ok(LoadedManifests {
        bundle: ManifestBundle {
            documents,
            source_hash: format!("{:x}", hasher.finalize()),
        },
        paths: ordered_paths,
    })
}

/// Removes exact-duplicate resolved paths from `paths`, keeping each
/// distinct path's first occurrence and otherwise preserving `paths`'s
/// given order exactly. See the module documentation's "Duplicates"
/// section for what counts as a duplicate and why repeats are dropped
/// rather than rejected.
fn deduplicate_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen: BTreeSet<&Path> = BTreeSet::new();
    paths
        .iter()
        .filter(|path| seen.insert(path.as_path()))
        .cloned()
        .collect()
}

/// Which serialization format one manifest file is parsed as, chosen
/// from its extension by [`manifest_format`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestFormat {
    Yaml,
    Json,
}

impl ManifestFormat {
    /// This format's name, used in [`InstallError::ManifestParse::format`].
    const fn label(self) -> &'static str {
        match self {
            Self::Yaml => "YAML",
            Self::Json => "JSON",
        }
    }
}

/// Chooses [`ManifestFormat`] for `path` by its extension: `.json`
/// (case-insensitively) selects JSON; anything else — `.yaml`, `.yml`,
/// no extension, or any other extension — selects YAML. YAML is the
/// default because it is the predominant Kubernetes manifest format and
/// a syntactic superset of JSON for a single document, not because every
/// other extension is expected to actually be YAML.
fn manifest_format(path: &Path) -> ManifestFormat {
    match path.extension().and_then(std::ffi::OsStr::to_str) {
        Some(extension) if extension.eq_ignore_ascii_case("json") => ManifestFormat::Json,
        _ => ManifestFormat::Yaml,
    }
}

/// Parses `bytes` (the on-disk contents of `path`) into zero or more
/// documents, per [`manifest_format`].
///
/// # Errors
///
/// Returns [`InstallError::ManifestParse`] naming `path` and the
/// offending document's 1-based position if any document is not
/// syntactically valid.
fn parse_manifest_file(path: &Path, bytes: &[u8]) -> Result<Vec<serde_json::Value>, InstallError> {
    match manifest_format(path) {
        ManifestFormat::Json => {
            let value: serde_json::Value =
                serde_json::from_slice(bytes).map_err(|error| InstallError::ManifestParse {
                    path: path.to_path_buf(),
                    document_number: 1,
                    format: ManifestFormat::Json.label(),
                    reason: error.to_string(),
                })?;
            Ok(vec![value])
        }
        ManifestFormat::Yaml => parse_yaml_documents(path, bytes),
    }
}

/// Parses `bytes` as a (possibly multi-document, `---`-separated) YAML
/// stream. A document that parses to YAML's null (for example, a
/// trailing empty document from a file ending in a bare `---`) is
/// silently omitted rather than becoming a spurious entry — see the
/// module documentation.
///
/// # Errors
///
/// Returns [`InstallError::ManifestParse`] naming `path` and the
/// offending document's 1-based position (counting every
/// `---`-separated block, including one later omitted for being null)
/// if any document is not syntactically valid YAML.
fn parse_yaml_documents(path: &Path, bytes: &[u8]) -> Result<Vec<serde_json::Value>, InstallError> {
    let mut documents = Vec::new();
    for (index, document) in serde_norway::Deserializer::from_slice(bytes).enumerate() {
        let value: serde_json::Value =
            serde_json::Value::deserialize(document).map_err(|error| {
                InstallError::ManifestParse {
                    path: path.to_path_buf(),
                    document_number: index + 1,
                    format: ManifestFormat::Yaml.label(),
                    reason: error.to_string(),
                }
            })?;
        if !value.is_null() {
            documents.push(value);
        }
    }
    Ok(documents)
}

/// Returns whether `stderr` from a failed `kubectl apply` looks like
/// Kubernetes's own rejection for exceeding the total
/// `metadata.annotations` size limit (see [`ANNOTATION_SIZE_LIMIT_BYTES`]),
/// rather than some other failure.
///
/// Deliberately a narrow, conservative heuristic — both the exact byte
/// limit and the word "annotations" must appear — so an unrelated
/// failure whose message happens to mention one but not the other is
/// never misclassified. A real annotation-size rejection reliably
/// contains both, since the API server formats the limit directly into
/// its message rather than summarizing it.
fn looks_like_annotation_size_limit_failure(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr);
    text.contains("annotations") && text.contains(ANNOTATION_SIZE_LIMIT_BYTES)
}

/// Drives `kubectl` (via a shared [`ProcessRunner`]) to install a single
/// resolved raw-manifests component. Holds only the runner: unlike the
/// Helm installer, this installer has no backend-owned client state to
/// isolate (`kubectl` needs nothing beyond `--kubeconfig`; see the
/// module documentation), so there is no per-run/per-side workspace
/// directory to capture at construction.
pub struct ManifestsInstaller {
    runner: Arc<dyn ProcessRunner>,
}

impl ManifestsInstaller {
    /// Creates an installer that drives `kubectl` through `runner`.
    #[must_use]
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner }
    }

    /// The one place every `kubectl`-targeting [`CommandSpec`] in this
    /// module is built. See the module documentation's "Why every
    /// `kubectl` invocation is safe by construction" section: this
    /// function unconditionally appends `--kubeconfig <kubeconfig>` to
    /// whatever subcommand-specific `args` it is given, with no way to
    /// opt out, and sets no environment overrides at all — kubeconfig
    /// selection always goes through this flag, never a `KUBECONFIG`
    /// environment variable. Takes no `&self`: unlike the Helm
    /// installer's own chokepoint, nothing here depends on
    /// per-instance state, so `clippy::unused_self` correctly flags
    /// `&self` as unnecessary if added back.
    fn kubectl_command(
        kubeconfig: &Path,
        mut args: Vec<OsString>,
        timeout: Duration,
    ) -> CommandSpec {
        args.push("--kubeconfig".into());
        args.push(kubeconfig.as_os_str().to_owned());
        CommandSpec {
            program: KUBECTL_PROGRAM.into(),
            args,
            cwd: None,
            env: BTreeMap::new(),
            sensitive_env_keys: BTreeSet::new(),
            timeout,
        }
    }

    /// Runs `spec` (one `kubectl` invocation for `component`) and maps
    /// its outcome to `Result<CommandResult, InstallError>`: a
    /// [`admissionlab_core::ProcessError`] becomes
    /// [`InstallError::Process`], and a non-zero exit becomes
    /// [`InstallError::CommandFailed`]. Mirrors the Helm installer's own
    /// `run_and_check` exactly.
    async fn run_and_check(
        &self,
        component: &str,
        spec: CommandSpec,
    ) -> Result<CommandResult, InstallError> {
        let context = Box::new(spec.context());
        let result = self
            .runner
            .run(spec)
            .await
            .map_err(|source| InstallError::Process {
                component: component.to_owned(),
                source,
            })?;
        if result.status.success() {
            Ok(result)
        } else {
            Err(InstallError::CommandFailed {
                component: component.to_owned(),
                context,
                status: result.status,
                stdout: result.stdout,
                stderr: result.stderr,
            })
        }
    }

    /// Applies one manifest `path` via `kubectl apply --server-side=false`
    /// (see [`apply_args`]) and, on a non-zero exit whose `stderr`
    /// matches Kubernetes's own annotation-size-limit rejection,
    /// re-reports it as [`InstallError::ManifestExceedsAnnotationLimit`]
    /// instead of a plain [`InstallError::CommandFailed`] — see the
    /// module documentation's "`--server-side=false`'s known failure
    /// mode" section.
    async fn apply_file(
        &self,
        component: &str,
        kubeconfig: &Path,
        path: &Path,
    ) -> Result<(), InstallError> {
        let spec = Self::kubectl_command(kubeconfig, apply_args(path), APPLY_TIMEOUT);
        match self.run_and_check(component, spec).await {
            Err(InstallError::CommandFailed {
                component: failed_component,
                context,
                status,
                stdout,
                stderr,
            }) if looks_like_annotation_size_limit_failure(&stderr) => {
                Err(InstallError::ManifestExceedsAnnotationLimit {
                    component: failed_component,
                    path: path.to_path_buf(),
                    context,
                    status,
                    stdout,
                    stderr,
                })
            }
            other => other.map(|_| ()),
        }
    }
}

#[async_trait]
impl ComponentInstaller for ManifestsInstaller {
    async fn install(
        &self,
        cluster: &ClusterHandle,
        component: &ResolvedComponent,
    ) -> Result<InstallRecord, InstallError> {
        let manifests = match &component.install {
            InstallMethod::Manifests(manifests) => manifests,
            InstallMethod::Helm(_) => {
                return Err(InstallError::UnsupportedMethod {
                    component: component.name.clone(),
                    expected: "Manifests",
                    actual: "Helm",
                });
            }
        };

        let started_at = SystemTime::now();
        let start = Instant::now();

        // Step 2: every manifest in this component must parse before
        // any of them is applied to the cluster.
        let loaded = load_manifests(&manifests.paths)?;

        // Step 3: apply, one file per invocation, in the same
        // deduplicated order `loaded.bundle` was hashed from.
        for path in &loaded.paths {
            self.apply_file(&component.name, &cluster.kubeconfig, path)
                .await?;
        }

        Ok(InstallRecord {
            component: component.name.clone(),
            method: "manifests".to_owned(),
            resolved_version: component.version.clone(),
            started_at,
            elapsed: start.elapsed(),
            diagnostics: Vec::new(),
        })
    }
}

/// Builds the argv (excluding the program name and `--kubeconfig`) for
/// `kubectl apply --server-side=false -f <path>` (Task 2.3 brief Step
/// 3). Pure argv construction only — [`ManifestsInstaller::kubectl_command`]
/// is what turns this into a runnable [`CommandSpec`], and it alone adds
/// `--kubeconfig`; this function must never add it itself, or a future
/// reader could mistake it for a second, competing place that decides
/// kubeconfig selection.
fn apply_args(path: &Path) -> Vec<OsString> {
    vec![
        "apply".into(),
        "--server-side=false".into(),
        "-f".into(),
        path.as_os_str().to_owned(),
    ]
}
