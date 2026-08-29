//! Mapping supported Kubernetes minor versions to pinned, digest-locked
//! `kind` node images.
//!
//! The mapping's source of truth is the checked-in
//! `compatibility/kubernetes.yaml` file, embedded into the compiled
//! binary at build time ([`load_matrix`]) rather than read from disk or
//! fetched over the network at runtime. That is deliberate, not an
//! oversight: dropping a supported minor must require a deliberate,
//! reviewed change to that checked-in file, never something that can
//! silently drift the next time Admission Lab runs (Task 1.8 Step 3;
//! PRODUCT.md §32 "Kubernetes Compatibility Policy" — human review is
//! required before dropping a supported version in a stable release
//! line). This module makes no HTTP calls at all.
//!
//! [`resolve_node_image`] is pure: it takes an already-loaded
//! [`KubernetesImageMatrix`] and never touches the filesystem or network
//! itself, so it is fully testable against a small hand-built matrix
//! independent of the real checked-in file (see `tests/version.rs`).

use serde::Deserialize;
use thiserror::Error;

/// `compatibility/kubernetes.yaml`'s contents, embedded into the binary
/// at compile time. The path is relative to *this source file*
/// (`crates/admissionlab-cluster/src/version.rs`): three directories up
/// reaches the workspace root, then into `compatibility/`.
const MATRIX_YAML: &str = include_str!("../../../compatibility/kubernetes.yaml");

/// One Kubernetes minor version's pinned `kind` node image.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KubernetesImage {
    /// The minor version, for example `"1.36"`.
    pub minor: String,
    /// The exact patch version this minor is pinned to, for example
    /// `"1.36.4"`. Matches [`resolve_node_image`]'s `requested` argument
    /// when a caller asks for a full version rather than a bare minor.
    pub version: String,
    /// The node's image reference without a digest, for example
    /// `"kindest/node:v1.36.4"`.
    pub image: String,
    /// The image's content digest, for example
    /// `"sha256:099e0493..."`. Combined with [`Self::image`] by
    /// [`resolve_node_image`] into the fully pinned reference kind
    /// actually receives.
    pub digest: String,
    /// Whether Admission Lab currently supports provisioning this
    /// version. `false` marks a minor retained in the matrix so
    /// [`resolve_node_image`] can give a specific "no longer supported"
    /// error rather than a generic "unknown version" one — see
    /// `compatibility/kubernetes.yaml`'s own comments for why each such
    /// entry is retired.
    pub supported: bool,
}

/// The full set of Kubernetes minor versions Admission Lab knows about,
/// loaded from `compatibility/kubernetes.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KubernetesImageMatrix {
    /// Every known release, in the order written in the source file.
    pub releases: Vec<KubernetesImage>,
}

/// A Kubernetes version successfully resolved against a
/// [`KubernetesImageMatrix`]: the exact patch version and the fully
/// digest-pinned `kind` node image reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedKubernetes {
    /// The exact resolved patch version, for example `"1.36.4"`.
    pub version: String,
    /// The fully pinned image reference —
    /// [`KubernetesImage::image`]/[`KubernetesImage::digest`] combined
    /// as `"kindest/node:v1.36.4@sha256:..."` — ready to use as
    /// `crate::config::KindClusterConfigInput::node_image`.
    pub pinned_image: String,
}

/// `requested` did not resolve to a supported [`KubernetesImage`] in a
/// [`KubernetesImageMatrix`].
#[derive(Debug, Error)]
pub enum VersionError {
    /// `requested` matched neither any [`KubernetesImage::version`] nor
    /// any [`KubernetesImage::minor`] in the matrix: this version is
    /// entirely unknown, not merely retired.
    #[error(
        "Kubernetes version {requested:?} is not in Admission Lab's supported matrix \
         (compatibility/kubernetes.yaml); see that file for supported versions"
    )]
    UnknownVersion {
        /// The value [`resolve_node_image`] was called with.
        requested: String,
    },
    /// `requested` matched a known minor, but that minor's
    /// [`KubernetesImage::supported`] is `false`.
    #[error(
        "Kubernetes {minor} (pinned to {version}) is no longer supported by Admission Lab; \
         see compatibility/kubernetes.yaml for currently supported versions"
    )]
    UnsupportedMinor {
        /// The value [`resolve_node_image`] was called with.
        requested: String,
        /// The matched entry's minor version.
        minor: String,
        /// The matched entry's exact pinned patch version.
        version: String,
    },
    /// `compatibility/kubernetes.yaml` is not valid YAML matching
    /// [`KubernetesImageMatrix`]'s shape. In practice this can only
    /// follow a bad hand-edit to that checked-in file — the embedded
    /// copy [`load_matrix`] reads never changes at runtime — and
    /// `tests/version.rs` catches it directly against the real file.
    #[error("failed to parse compatibility/kubernetes.yaml: {source}")]
    Malformed {
        /// The underlying YAML parse failure.
        #[source]
        source: serde_norway::Error,
    },
}

/// Loads the checked-in [`KubernetesImageMatrix`] embedded into this
/// binary at compile time from `compatibility/kubernetes.yaml`.
///
/// This never touches the filesystem or the network at call time: the
/// file's contents are embedded into the compiled binary via
/// `include_str!`, so the set of supported Kubernetes versions can only
/// change by editing that checked-in file and rebuilding — never by
/// anything Admission Lab observes while running (see this module's
/// documentation).
///
/// # Errors
///
/// Returns [`VersionError::Malformed`] if the embedded
/// `compatibility/kubernetes.yaml` is not valid YAML matching
/// [`KubernetesImageMatrix`]'s shape.
pub fn load_matrix() -> Result<KubernetesImageMatrix, VersionError> {
    parse_matrix(MATRIX_YAML)
}

fn parse_matrix(text: &str) -> Result<KubernetesImageMatrix, VersionError> {
    serde_norway::from_str(text).map_err(|source| VersionError::Malformed { source })
}

/// Resolves `requested` — a full patch version (`"1.36.4"`) or a bare
/// minor (`"1.36"`) — against `matrix`, returning the exact pinned
/// version and image reference `kind` should use.
///
/// Matching is an exact string match against either
/// [`KubernetesImage::version`] or [`KubernetesImage::minor`] — never a
/// semver range, "closest available patch," or otherwise fuzzy match. A
/// full-version match is checked first, so a matrix that pins
/// `minor: "1.36"` to `version: "1.36.4"` resolves a request for the
/// literal string `"1.36"` to that same entry via the minor match.
///
/// # Errors
///
/// Returns [`VersionError::UnknownVersion`] if `requested` matches
/// neither field on any entry, or [`VersionError::UnsupportedMinor`] if
/// it matches an entry whose [`KubernetesImage::supported`] is `false`.
pub fn resolve_node_image(
    requested: &str,
    matrix: &KubernetesImageMatrix,
) -> Result<ResolvedKubernetes, VersionError> {
    let matched = matrix
        .releases
        .iter()
        .find(|release| release.version == requested)
        .or_else(|| {
            matrix
                .releases
                .iter()
                .find(|release| release.minor == requested)
        })
        .ok_or_else(|| VersionError::UnknownVersion {
            requested: requested.to_owned(),
        })?;

    if !matched.supported {
        return Err(VersionError::UnsupportedMinor {
            requested: requested.to_owned(),
            minor: matched.minor.clone(),
            version: matched.version.clone(),
        });
    }

    Ok(ResolvedKubernetes {
        version: matched.version.clone(),
        pinned_image: format!("{}@{}", matched.image, matched.digest),
    })
}
