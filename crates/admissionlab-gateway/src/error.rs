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

    /// An object read back from a cluster does not have the shape a
    /// Gateway API object must have, so its status could not be
    /// normalized: no `metadata.name`, no integer
    /// `metadata.generation`, a `status.conditions` that is not a list,
    /// a condition with no `type`, a condition whose `status` is not one
    /// of `"True"`/`"False"`/`"Unknown"`, or a duplicated condition
    /// type.
    ///
    /// Deliberately an error rather than a partial or best-effort
    /// reading: every one of these cases is *impossible* against a real
    /// API server serving Gateway API's own CRDs (their schemas require
    /// each field and enumerate the legal condition statuses), so
    /// reaching this variant means the value is not the object this code
    /// believes it is. Filling in a plausible default there --
    /// generation `0`, status `Unknown` -- would put words in a
    /// controller's mouth, which Global Constraint 15 forbids. See
    /// `crate::conditions`'s own documentation for that argument in
    /// full.
    #[error("could not read the status of {object}: {reason}")]
    MalformedStatus {
        /// Which object, described as specifically as could be
        /// determined (for example `"Gateway gateway-lab/lab-gateway"`,
        /// or `"a Gateway object"` when even its name was unreadable).
        object: String,
        /// What about its shape could not be read.
        reason: String,
    },

    /// A cluster could not be queried while waiting for a route to
    /// reconcile: its kubeconfig could not be turned into a usable
    /// client, or a read failed at the transport level.
    ///
    /// Never the same thing as "the object is not there", which
    /// `crate::reconcile::GatewayStatusSource` reports as `Ok(None)` and
    /// the waiter simply retries.
    #[error("could not observe {object} on cluster {cluster:?}: {reason}")]
    ObservationUnavailable {
        /// The cluster's own name, for the reason
        /// [`GatewayError::ApplyUnavailable::cluster`] gives.
        cluster: String,
        /// Which object was being read (for example
        /// `"HTTPRoute gateway-lab/echo-a"`).
        object: String,
        /// A human-readable explanation, from the underlying failure's
        /// own `Display`.
        reason: String,
    },

    /// The `Gateway` or `HTTPRoute` a route contract names was never
    /// observed to exist, up to the reconciliation deadline.
    ///
    /// An error rather than `converged: false` evidence, and the reason
    /// is structural: §1.2 freezes
    /// `crate::reconcile::ReconciliationEvidence`'s `gateway` and
    /// `route` as non-optional, so an object that never existed has
    /// nothing to put there -- and filling the field with a fabricated
    /// empty evidence value carrying a made-up generation is exactly
    /// what Global Constraint 15 forbids. This is also a genuinely
    /// different failure from a timeout: a timeout means the
    /// implementation did not finish reconciling something that exists,
    /// while this means the fixture (or the contract) names an object
    /// that is not there at all.
    ///
    /// A *transient* 404 during polling is not this: it is retried, and
    /// only an absence that persists to the deadline reaches here.
    #[error("{object} does not exist on cluster {cluster:?}")]
    ObjectAbsent {
        /// The cluster's own name.
        cluster: String,
        /// Which object was expected (for example
        /// `"Gateway gateway-lab/lab-gateway"`).
        object: String,
    },

    /// A [`crate::endpoint::GatewayEndpointStrategy`] could not be
    /// turned into a concrete lookup: one of its substitutable fields
    /// contains a placeholder
    /// [`admissionlab_spec::substitute_gateway_placeholders`] does not
    /// recognize.
    ///
    /// Unreachable for a strategy that came from a recipe --
    /// `admissionlab-recipes` performs exactly this check at recipe-load
    /// time, through the same function, so a typo fails there with the
    /// recipe's own file named. This variant covers a strategy built
    /// programmatically, and exists so that path fails loudly too
    /// instead of silently searching for a `Service` labelled with a
    /// literal `"{gateway}"`.
    #[error("cannot resolve the data-plane endpoint for Gateway {gateway}: {reason}")]
    EndpointStrategyInvalid {
        /// The Gateway the strategy was being resolved for, in
        /// [`crate::GatewayIdentity`]'s `Display` form.
        gateway: String,
        /// What about the strategy could not be resolved.
        reason: String,
    },

    /// No `Service` matched a Gateway's endpoint strategy.
    ///
    /// Distinct from [`GatewayError::ObjectAbsent`], which is about an
    /// object a *contract* named directly: this is about an object
    /// nothing named, which a recipe's strategy said how to *find*.
    /// Which of the two failed is the difference between "the fixture
    /// names a Gateway that was never applied" and "the implementation
    /// never provisioned this Gateway's data plane", and they have
    /// different fixes.
    #[error("no Service {lookup}{}", describe_considered(.considered.as_deref()))]
    EndpointNotFound {
        /// What was looked for, and where.
        lookup: Box<EndpointLookup>,
        /// Every `Service` that was actually considered, in name order.
        ///
        /// `None` means no enumeration took place at all (a lookup by
        /// exact name asks the API server for one object rather than
        /// listing the namespace), which is deliberately *not* the same
        /// value as `Some(vec![])` ("the namespace was listed and holds
        /// no Service") -- Global Constraint 15's distinction between
        /// unknown and empty, in the one place a reader would otherwise
        /// have to guess which happened.
        considered: Option<Vec<String>>,
    },

    /// More than one `Service` matched a Gateway's endpoint strategy
    /// equally well.
    ///
    /// Never resolved by picking the first, the alphabetically smallest,
    /// or the most recently created: which `Service` fronts a Gateway
    /// determines what every probe in Task 6.8 measures, so breaking the
    /// tie would silently attribute one data plane's behavior to
    /// another. The same rule, for the same reason,
    /// `admissionlab_admission::CorrelationError::Ambiguous` follows for
    /// audit events.
    #[error(
        "{} Services {lookup} ({}); Admission Lab does not break such a tie -- narrow the \
         recipe's selector",
        .candidates.len(), .candidates.join(", ")
    )]
    EndpointAmbiguous {
        /// What was looked for, and where.
        lookup: Box<EndpointLookup>,
        /// Every equally valid `Service` name, in name order.
        candidates: Vec<String>,
    },

    /// A `Service` was found, but which of its ports to forward could
    /// not be determined.
    ///
    /// Carries every port the `Service` actually exposes, rendered as
    /// `name=port` (or just `port` for an unnamed one), so the fix is
    /// visible without a second `kubectl get svc`.
    #[error(
        "cannot choose a port on Service {service} on cluster {cluster:?}: {reason} (exposed: {})",
        if .ports.is_empty() { "none".to_owned() } else { .ports.join(", ") }
    )]
    EndpointPortUnresolved {
        /// The cluster's own name.
        cluster: String,
        /// The `Service`, as `namespace/name`.
        service: String,
        /// Why no single port could be chosen.
        reason: String,
        /// Every port the `Service` exposes, in declaration order.
        ports: Vec<String>,
    },

    /// A local port-forward to a Gateway's data plane could not be
    /// started or stopped because no answer could be obtained at all:
    /// `kubectl` could not be spawned, or a running forward could not be
    /// killed.
    ///
    /// The `Unavailable`/`Failed` split is the same one
    /// [`GatewayError::ApplyUnavailable`] and
    /// [`GatewayError::ApplyRejected`] already draw: this variant is
    /// "the machinery did not work", never "the forward was refused".
    #[error("could not manage a port-forward to {endpoint} on cluster {cluster:?}: {reason}")]
    PortForwardUnavailable {
        /// The cluster's own name, for the reason
        /// [`GatewayError::ApplyUnavailable::cluster`] gives.
        cluster: String,
        /// The endpoint being forwarded to, in
        /// [`crate::endpoint::GatewayEndpoint`]'s `Display` form.
        endpoint: String,
        /// A human-readable explanation, from the underlying failure's
        /// own `Display`.
        reason: String,
    },

    /// A `kubectl port-forward` started but never became usable: it
    /// exited before announcing a local port, announced nothing within
    /// the readiness window, or closed its stdout without ever printing
    /// a line this crate could parse.
    ///
    /// Carries `stderr` verbatim (lossily decoded, and bounded by
    /// `admissionlab_core::MAX_CAPTURED_STREAM_BYTES`) rather than
    /// summarizing it: `kubectl`'s own message is the actual diagnosis
    /// — "Service does not have any active Endpoint", "unable to listen
    /// on any of the requested ports", "the server could not find the
    /// requested resource" — and rewording it would only ever lose
    /// information.
    #[error("port-forward to {endpoint} on cluster {cluster:?} never became usable: {reason}{}",
        if .stderr.is_empty() {
            String::new()
        } else {
            format!("; kubectl stderr: {}", .stderr.trim_end())
        }
    )]
    PortForwardFailed {
        /// The cluster's own name.
        cluster: String,
        /// The endpoint being forwarded to.
        endpoint: String,
        /// What went wrong, in this crate's own words (the child exited,
        /// the readiness window elapsed, stdout ended).
        reason: String,
        /// Everything `kubectl` wrote to stderr, verbatim. Empty when it
        /// wrote nothing, which is itself worth seeing.
        stderr: String,
    },
}

/// What a Gateway data-plane `Service` lookup was looking for, and
/// where.
///
/// Shared by [`GatewayError::EndpointNotFound`] and
/// [`GatewayError::EndpointAmbiguous`] -- the two outcomes of the same
/// search -- so the two can never describe the same lookup differently,
/// and boxed in both so neither variant dominates
/// [`GatewayError`]'s size (the same reason
/// `admissionlab_admission::CorrelationError` boxes its own `ObjectKey`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointLookup {
    /// The cluster's own name, for the reason
    /// [`GatewayError::ApplyUnavailable::cluster`] gives.
    pub cluster: String,
    /// The Gateway whose data plane was being located, in
    /// [`crate::GatewayIdentity`]'s `Display` form.
    pub gateway: String,
    /// The namespace that was searched, after placeholder substitution.
    pub namespace: String,
    /// How the search was expressed, already substituted -- for example
    /// `matches the selector gateway.networking.k8s.io/gateway-name=lab-gateway`,
    /// or `is named "lab-gateway-istio"`.
    pub criteria: String,
}

impl std::fmt::Display for EndpointLookup {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "in namespace {:?} on cluster {:?} {} for Gateway {}",
            self.namespace, self.cluster, self.criteria, self.gateway
        )
    }
}

/// Renders [`GatewayError::EndpointNotFound::considered`] as a trailing
/// clause, keeping "not enumerated" and "enumerated, and empty" visibly
/// different in the message itself rather than only in the data.
fn describe_considered(considered: Option<&[String]>) -> String {
    match considered {
        None => String::new(),
        Some([]) => " (the namespace holds no Services at all)".to_owned(),
        Some(names) => format!(" (considered: {})", names.join(", ")),
    }
}
