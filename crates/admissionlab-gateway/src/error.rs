//! Failure modes of the Gateway behavior engine.
//!
//! One error type for the whole crate, the same shape
//! `admissionlab_fixtures::FixtureError` uses: every variant that names a
//! specific manifest document carries both `path` (the file it came
//! from) and `document_index` (its zero-based position, counting every
//! `---`-separated document in file order) so a user can find the exact
//! offending document without re-deriving position from a renumbered
//! list.
//!
//! # What is, and is not, an error here
//!
//! `admissionlab_fixtures::execute` draws a hard line: a Kubernetes API
//! server *rejecting* a dry-run fixture is a successful observation, not
//! an error, because the rejection is the thing being measured. Task
//! 6.2's apply step sits on the other side of that line, and
//! [`GatewayError::ApplyRejected`] is an error: a Gateway fixture that
//! could not be persisted leaves no `Gateway` and no `HTTPRoute` for a
//! controller to reconcile, so there is nothing for Tasks 6.3-6.9 to
//! observe and no [`crate::apply::AppliedGatewayFixture`] to return. The
//! variant still carries the API server's own `reason`/`code` verbatim
//! rather than collapsing to a boolean, so a later task that wants to
//! compare *admission* behavior on Gateway fixtures across sides has the
//! real answer to compare and does not have to re-apply anything to get
//! it.

use std::path::PathBuf;

use thiserror::Error;

/// Something went wrong installing or observing a Gateway fixture.
#[derive(Debug, Error)]
pub enum GatewayError {
    /// A Gateway fixture manifest file could not be read from disk.
    #[error("failed to read Gateway manifest {}: {source}", .path.display())]
    ManifestRead {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// One document inside a Gateway fixture manifest is not
    /// syntactically valid.
    #[error(
        "Gateway manifest {} document {}: not valid {format}: {reason}",
        .path.display(), .document_index + 1
    )]
    ManifestParse {
        /// The file containing the malformed document.
        path: PathBuf,
        /// Zero-based position of the malformed document within `path`.
        document_index: usize,
        /// `"YAML"` or `"JSON"`, chosen from the file's extension.
        format: &'static str,
        /// A human-readable explanation from the underlying parser.
        reason: String,
    },

    /// A non-empty document parsed to something other than a JSON/YAML
    /// mapping (a scalar or a sequence), so it has no
    /// `apiVersion`/`kind`/`metadata` to even look for.
    #[error(
        "Gateway manifest {} document {}: expected a Kubernetes object (a YAML/JSON mapping), \
         found {found}",
        .path.display(), .document_index + 1
    )]
    ManifestNotAnObject {
        /// The file containing the malformed document.
        path: PathBuf,
        /// Zero-based position of the malformed document within `path`.
        document_index: usize,
        /// What the document actually parsed to, for the message.
        found: &'static str,
    },

    /// A document is missing a field this crate requires to apply it and
    /// to record what it applied (`apiVersion`, `kind`, or
    /// `metadata.name`), or has that field present but not as a
    /// non-empty string.
    #[error(
        "Gateway manifest {} document {}: missing required field {field:?}",
        .path.display(), .document_index + 1
    )]
    ManifestMissingField {
        /// The file containing the incomplete document.
        path: PathBuf,
        /// Zero-based position of the incomplete document within `path`.
        document_index: usize,
        /// The dotted field path that was missing (for example
        /// `"metadata.name"`).
        field: &'static str,
    },

    /// A document has `metadata.generateName` but no `metadata.name`.
    ///
    /// Rejected for the same reason
    /// `admissionlab_fixtures::FixtureError::GenerateNameUnsupported`
    /// rejects it, plus one that is specific to this crate: a
    /// [`crate::model::RouteContract`] names its `Gateway` and its
    /// `HTTPRoute` by an exact name, so an object whose real name the
    /// API server invents at admission time could never be the object a
    /// contract is about.
    #[error(
        "Gateway manifest {} document {}: `metadata.generateName` is not supported -- a route \
         contract names its Gateway and HTTPRoute by exact name, so a server-generated name \
         could never be the object under contract",
        .path.display(), .document_index + 1
    )]
    ManifestGenerateNameUnsupported {
        /// The file containing the `generateName`-only document.
        path: PathBuf,
        /// Zero-based position of that document within `path`.
        document_index: usize,
    },

    /// A manifest document's `apiVersion`/`kind` could not be resolved
    /// against the cluster's own served API surface, or the cluster's
    /// discovery could not be run at all.
    ///
    /// `#[error(transparent)]`: `admissionlab_fixtures::FixtureError`'s
    /// own `ResourceDiscoveryUnavailable`/`UnsupportedResource` messages
    /// already name the cluster, the `apiVersion`, the `kind`, and (for
    /// the second) the two indistinguishable causes Global Constraint 15
    /// forbids guessing between. A wrapping prefix here would only
    /// repeat what follows it.
    #[error(transparent)]
    ResourceResolution(#[from] admissionlab_fixtures::FixtureError),

    /// A Gateway fixture object could not be applied because no answer
    /// could be obtained from the API server at all: the cluster's
    /// kubeconfig could not be turned into a usable client, the request
    /// could not be built or serialized, or the exchange failed at the
    /// transport level.
    ///
    /// Never an admission decision about the object -- see
    /// [`GatewayError::ApplyRejected`] for that.
    #[error("could not apply Gateway fixture object {object} on cluster {cluster:?}: {reason}")]
    ApplyUnavailable {
        /// The cluster's own name
        /// (`admissionlab_core::ClusterSpec::name`), not its kubeconfig
        /// path -- the same choice
        /// `admissionlab_fixtures::FixtureError`'s cluster-scoped
        /// variants make, so a local filesystem path never reaches a
        /// user-visible report.
        cluster: String,
        /// The object being applied, in
        /// `admissionlab_admission::ObjectKey`'s `Display` form, or a
        /// best-effort `apiVersion kind namespace/name` when the object
        /// could not even be resolved to one.
        object: String,
        /// A human-readable explanation, from the underlying failure's
        /// own `Display`.
        reason: String,
    },

    /// The API server returned a real, structured refusal for a Gateway
    /// fixture object -- an admission webhook denied it, its schema
    /// validation rejected it, a field-ownership conflict was reported,
    /// and so on.
    ///
    /// See this module's documentation for why this is an error here
    /// even though the equivalent is an *observation* in
    /// `admissionlab_fixtures::execute`, and for why `code`/`reason` are
    /// carried through verbatim rather than collapsed.
    #[error(
        "cluster {cluster:?} refused Gateway fixture object {object}{}: {message}",
        .code.map(|code| format!(" (HTTP {code})")).unwrap_or_default()
    )]
    ApplyRejected {
        /// The cluster's own name, for the reason
        /// [`GatewayError::ApplyUnavailable::cluster`] gives.
        cluster: String,
        /// The object being applied, in
        /// `admissionlab_admission::ObjectKey`'s `Display` form.
        object: String,
        /// The HTTP status code the API server reported, when its
        /// response carried one. `None` means no code was observed --
        /// never fabricated as a plausible `403` (Global Constraint 15),
        /// the same rule
        /// `admissionlab_admission::AdmissionDecision::Rejected` follows.
        code: Option<u16>,
        /// The API server's own `reason` (for example `"Forbidden"`,
        /// `"Conflict"`, `"Invalid"`), when its response carried one.
        reason: Option<String>,
        /// The API server's own message, verbatim.
        message: String,
    },
}
