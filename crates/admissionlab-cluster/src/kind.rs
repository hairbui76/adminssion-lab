//! Facts about talking to the `kind` CLI: its program name, the argv
//! `lifecycle.rs` builds for `create`/`delete`, the timeouts each
//! invocation gets, and the naming rules a `kind` cluster name must
//! satisfy.
//!
//! Everything here is pure and side-effect-free: no function in this
//! module touches the filesystem, spawns a process, or otherwise
//! performs I/O. `lifecycle.rs` is what actually drives `kind` through
//! [`admissionlab_core::ProcessRunner`].
//!
//! # Cluster naming
//!
//! A `kind` cluster name becomes, verbatim, part of the Docker container
//! name `kind` creates for its single node (`<name>-control-plane`),
//! which is in turn that node's Kubernetes `Node` object name — so it
//! must be a valid DNS-1123 label, and short enough that appending
//! `-control-plane` still is too.
//!
//! [`cluster_name`] assembles `adlab-<side>-<short-run-id>` and
//! [`validate_cluster_name`] enforces both rules on the result.
//! Critically, [`cluster_name`] does not just concatenate and trust the
//! result: `admissionlab_core::RunId::parse` (unlike
//! `RunId::generate`, which always produces a `UUIDv4`) accepts a value
//! with a leading or trailing `-`, which would otherwise silently
//! produce an invalid name such as `adlab-baseline-abcdefg-`. Feeding
//! the assembled name through [`validate_cluster_name`] closes that gap
//! for every caller, not only this one.
//!
//! ## Why 12 characters
//!
//! [`SHORT_RUN_ID_LEN`] is 12: enough characters of a
//! [`admissionlab_core::RunId`] to make an accidental collision between
//! two runs on the same machine implausible, while mirroring a
//! convention already familiar from this exact domain — Docker's own
//! "short ID" truncates a container/image ID to 12 characters. For the
//! common case, `RunId::generate()`'s `UUIDv4` (`xxxxxxxx-xxxx-...`,
//! hyphens fixed at positions 9/14/19/24) never has a hyphen anywhere in
//! its first 12 characters, so [`cluster_name`] never needs to fall back
//! to a shorter prefix or reject a generated id. The length budget below
//! shows *why* 12 comfortably fits even for the one side name that is
//! longer, and [`validate_cluster_name`] is what catches the rare
//! non-UUID id (see above) that this reasoning does not cover.
//!
//! ## Why the limit is 49, not 63
//!
//! DNS-1123 caps a label at 63 characters, but the cluster name is not
//! the longest label actually derived from it: `kind` appends
//! `-control-plane` (14 characters) to form the Docker container name,
//! which becomes the Kubernetes `Node` name — so *that* 63-character
//! label is the one that must not be exceeded. `"adlab-candidate-"` (the
//! longer of the two side prefixes) is 16 characters, leaving
//! `63 - 14 - 16 = 33` characters for `<short-run-id>`. [`SHORT_RUN_ID_LEN`]
//! (12) fits with 21 characters to spare, and [`validate_cluster_name`]
//! enforces the precise bound (`name.len() + 14 <= 63`, i.e. a bare name
//! of at most 49 characters) rather than this specific worked example,
//! so it also protects a future caller that assembles a name some other
//! way.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use admissionlab_core::{ClusterError, RunId, Side};

/// The `kind` program name, passed to
/// [`admissionlab_core::process::CommandSpec::program`] and resolved via
/// `PATH` — never an absolute path, matching
/// `admissionlab_core::tool::ToolName::program`'s own convention for
/// external tools.
pub(crate) const KIND_PROGRAM: &str = "kind";

/// How long `kind create cluster` may run before it is killed and
/// reported as timed out.
///
/// Sized for a cold node-image pull, not the common warm case: measured
/// directly against kind v0.33.0 + Kubernetes 1.36.4 on this machine,
/// `kind create cluster` took approximately 31 seconds warm (image
/// already pulled) and approximately 105 seconds on a cold first pull.
/// Five minutes is roughly 3x the measured cold figure, giving headroom
/// for a slower disk/network/CI runner than the measurement machine
/// without making a genuine hang wait unreasonably long to be caught —
/// the same "generous for the slow case, not tuned to the common one"
/// reasoning `admissionlab_core::tool::PROBE_TIMEOUT` already documents,
/// applied to a much larger operation.
pub(crate) const CREATE_TIMEOUT: Duration = Duration::from_secs(300);

/// How long `kind delete cluster` may run before it is killed and
/// reported as timed out.
///
/// Measured steady-state delete time on the same machine was
/// approximately 1 second. 60 seconds is generous headroom (60x) for a
/// slower teardown under contention (for example many concurrent
/// deletes during the Phase 1 exit gate's 100-loop leak test) while
/// still catching a genuine hang well within a normal CI budget.
pub(crate) const DELETE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long `kind get clusters` (used by `diagnostics`) may run before
/// it is killed and reported as timed out.
///
/// This is a simple, offline, local Docker-state query — not expected to
/// approach this — but sized the same as
/// `admissionlab_core::tool::PROBE_TIMEOUT` for the same reason: generous
/// for a slow/loaded CI runner rather than tuned to the common case.
pub(crate) const DIAGNOSTICS_TIMEOUT: Duration = Duration::from_secs(10);

/// Number of leading characters of a [`RunId`] used as the
/// `<short-run-id>` suffix in [`cluster_name`]. See the module
/// documentation's "Why 12 characters" section.
const SHORT_RUN_ID_LEN: usize = 12;

/// The DNS-1123 label length limit.
const DNS1123_LABEL_MAX_LEN: usize = 63;

/// The suffix `kind` appends to a cluster name to form its single node's
/// Docker container name and Kubernetes `Node` name. See the module
/// documentation's "Why the limit is 49, not 63" section.
const CONTROL_PLANE_SUFFIX: &str = "-control-plane";

/// Basename of the audit log file `kube-apiserver` writes inside the
/// node, once mounted through to the host directory a
/// [`ClusterHandle`](admissionlab_core::ClusterHandle)'s caller provides.
///
/// This must match the basename of `crate::config`'s private
/// `AUDIT_LOG_NODE_FILE` constant (`/var/log/kubernetes/kube-apiserver-audit.log`).
/// That constant is deliberately not exported (Controller Ruling R22
/// leaves `config.rs` untouched), so this is a second, independent
/// literal kept in sync by hand — guarded by
/// `tests/lifecycle_unit.rs`'s `audit_log_file_name_matches_what_render_kind_config_actually_configures`,
/// which parses a real `render_kind_config` output and asserts its
/// `audit-log-path` basename equals this constant.
pub(crate) const AUDIT_LOG_FILE_NAME: &str = "kube-apiserver-audit.log";

/// Assembles and validates a `kind`-safe cluster name for `side` using
/// `run_id`: `adlab-<side>-<short-run-id>`.
///
/// See the module documentation for what `<short-run-id>` is and why,
/// and for why the assembled name is always validated (never just
/// concatenated and trusted) before being returned.
///
/// # Errors
///
/// Returns [`ClusterError::InvalidName`] if the assembled name is not a
/// valid `kind` cluster name — in practice, only when `run_id` is a
/// parsed (not generated) id whose first [`SHORT_RUN_ID_LEN`] characters
/// end in `-` (the assembled name can never be empty:
/// `admissionlab_core::RunId::parse` already rejects an empty `run_id`,
/// and the fixed `adlab-<side>-` prefix alone is non-empty).
pub fn cluster_name(side: Side, run_id: &RunId) -> Result<String, ClusterError> {
    let short_run_id: String = run_id.as_str().chars().take(SHORT_RUN_ID_LEN).collect();
    let name = format!("adlab-{}-{short_run_id}", side.as_str());
    validate_cluster_name(&name)?;
    Ok(name)
}

/// Validates that `name` is safe to use as a `kind` cluster name.
///
/// Two independent rules, both required:
///
/// - **DNS-1123 label charset**: non-empty, only ASCII lowercase
///   letters, digits, and `-`, and it must start and end with a letter
///   or digit. This mirrors `admissionlab_cluster::config`'s own
///   (private) `validate_name` exactly, since both must agree on what
///   `kind` itself will accept — duplicated rather than shared because
///   that function is a private implementation detail of rendering, not
///   part of this crate's public naming contract.
/// - **Length budget**: `name`'s length plus `"-control-plane"`'s must
///   not exceed the DNS-1123 label limit of 63 characters — see the
///   module documentation's "Why the limit is 49, not 63" section for
///   why 63 alone is not the right bound to check.
///
/// # Errors
///
/// Returns [`ClusterError::InvalidName`] describing which rule `name`
/// failed.
pub fn validate_cluster_name(name: &str) -> Result<(), ClusterError> {
    let is_label_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let valid_charset = !name.is_empty()
        && name.chars().all(|c| is_label_char(c) || c == '-')
        && name.starts_with(is_label_char)
        && name.ends_with(is_label_char);
    if !valid_charset {
        return Err(ClusterError::InvalidName {
            name: name.to_owned(),
            reason: "must be non-empty and a valid DNS-1123 label: only ASCII lowercase \
                     letters, digits, and '-', starting and ending with a letter or digit"
                .to_owned(),
        });
    }

    let derived_len = name.len() + CONTROL_PLANE_SUFFIX.len();
    if derived_len > DNS1123_LABEL_MAX_LEN {
        let max_name_len = DNS1123_LABEL_MAX_LEN - CONTROL_PLANE_SUFFIX.len();
        return Err(ClusterError::InvalidName {
            name: name.to_owned(),
            reason: format!(
                "must be at most {max_name_len} characters so that kind's derived \
                 \"{name}{CONTROL_PLANE_SUFFIX}\" Docker container/Kubernetes node name \
                 ({derived_len} characters) still fits the DNS-1123 label limit of \
                 {DNS1123_LABEL_MAX_LEN} characters"
            ),
        });
    }

    Ok(())
}

/// Converts a filesystem path to an owned [`OsString`], without
/// depending on which `From`/`Into` conversions happen to exist for
/// `&Path` (there is no direct `impl From<&Path> for OsString` in
/// `std`): `Path::as_os_str` plus `OsStr`'s `ToOwned` implementation
/// always works.
fn path_to_os_string(path: &Path) -> OsString {
    path.as_os_str().to_owned()
}

/// Builds the exact argv (excluding the program name) for
/// `kind create cluster`: an explicit `--name`, a generated `--config`
/// file, and an explicit `--kubeconfig` path (Task 1.7 brief Step 1;
/// PRODUCT.md §29.2, so `kind` never falls back to `$KUBECONFIG` or
/// `~/.kube/config`).
pub(crate) fn create_argv(name: &str, config_path: &Path, kubeconfig_path: &Path) -> Vec<OsString> {
    vec![
        "create".into(),
        "cluster".into(),
        "--name".into(),
        name.into(),
        "--config".into(),
        path_to_os_string(config_path),
        "--kubeconfig".into(),
        path_to_os_string(kubeconfig_path),
    ]
}

/// Builds the exact argv (excluding the program name) for
/// `kind delete cluster`: the cluster name only (Task 1.7 brief Step 1).
/// `kind` identifies a cluster by name alone; no kubeconfig path is
/// needed to delete one.
pub(crate) fn delete_argv(name: &str) -> Vec<OsString> {
    vec![
        "delete".into(),
        "cluster".into(),
        "--name".into(),
        name.into(),
    ]
}

/// Builds the exact argv (excluding the program name) for
/// `kind get clusters`, used by `diagnostics` to check whether a cluster
/// by this name still exists.
pub(crate) fn get_clusters_argv() -> Vec<OsString> {
    vec!["get".into(), "clusters".into()]
}

/// This crate's only inline test module: every other test lives in
/// `tests/*.rs` (see `tests/lifecycle_unit.rs`'s own module
/// documentation), but the one check here genuinely needs to be inline.
#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::AUDIT_LOG_FILE_NAME;
    use crate::config::{KindClusterConfigInput, render_kind_config};

    /// Proves [`AUDIT_LOG_FILE_NAME`] stays in sync with what
    /// `render_kind_config` actually configures kube-apiserver to write,
    /// by referencing the real `pub(crate)` constant directly rather than
    /// a second hardcoded copy of the same literal. This has to live
    /// here, inline, rather than in `tests/lifecycle_unit.rs`: an
    /// external test crate cannot see a `pub(crate)` item, so a copy of
    /// this check living there could only ever hardcode the literal a
    /// second time -- which would keep passing even if
    /// `AUDIT_LOG_FILE_NAME` alone drifted out of sync, detecting
    /// nothing. Only an inline test can name both
    /// `AUDIT_LOG_FILE_NAME` and `render_kind_config`'s actual output at
    /// once and so genuinely fail on drift.
    #[test]
    fn audit_log_file_name_matches_what_render_kind_config_actually_configures() {
        let rendered = render_kind_config(&KindClusterConfigInput {
            name: "adlab-baseline-couplingtest".to_owned(),
            node_image: "kindest/node:v1.36.4@sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed".to_owned(),
            audit_policy_host_path: PathBuf::from("/tmp/adlab-coupling/audit-policy.yaml"),
            audit_log_host_dir: PathBuf::from("/tmp/adlab-coupling/audit"),
        })
        .expect("render_kind_config should succeed for valid input");

        let doc: serde_norway::Value =
            serde_norway::from_str(&rendered).expect("rendered config must be valid YAML");
        let patch_text = doc["nodes"][0]["kubeadmConfigPatches"][0]
            .as_str()
            .expect("kubeadmConfigPatches[0] must be a string");
        let patch: serde_norway::Value =
            serde_norway::from_str(patch_text).expect("embedded patch must be valid YAML");
        let audit_log_path = patch["apiServer"]["extraArgs"]["audit-log-path"]
            .as_str()
            .expect("apiServer.extraArgs.audit-log-path must be a string");

        let basename = Path::new(audit_log_path)
            .file_name()
            .and_then(|name| name.to_str())
            .expect("audit-log-path must have a file name");

        assert_eq!(basename, AUDIT_LOG_FILE_NAME);
    }
}
