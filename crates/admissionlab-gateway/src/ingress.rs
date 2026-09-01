//! What a legacy `Ingress` stack actually did with a migration case
//! (ROADMAP Task 8.4).
//!
//! [`run_ingress_case`] is the baseline half of an Ingress-to-Gateway
//! comparison: it persists one [`MigrationCaseSpec`]'s
//! `baselineIngressManifests` into a lab cluster, finds out whether the
//! legacy stack's own admission webhooks even accepted them, waits for
//! the controller to actually serve the case's traffic, and replays the
//! case's probes through a local port-forward. What comes back is an
//! [`IngressCaseResult`], which Task 8.5's comparator pairs with the
//! candidate side's [`crate::case::GatewayCaseResult`].
//!
//! It observes; it never grades. `admitted: false` is not a verdict and
//! neither is `ready: false` -- both are facts about a cluster, and
//! exactly one place in this project decides what a fact is worth
//! (Global Constraint 6, the same line [`crate::case`] draws for the
//! Gateway side).
//!
//! # THE FINDING: an `Ingress` has no status worth waiting on
//!
//! The Gateway side of this comparison can wait for a controller to
//! publish `Accepted`/`ResolvedRefs`/`Programmed`
//! ([`crate::reconcile`]), and that wait is what makes its probes
//! deterministic. The `Ingress` API offers no equivalent. Its only
//! status field is `status.loadBalancer`, which an `Ingress` controller
//! populates from the address of its *own* `Service` -- and on `kind`
//! that `LoadBalancer` Service never gets an address, so the field stays
//! empty **forever**, on a cluster where routing demonstrably works.
//! Measured, on a real cluster running
//! `recipes/ingress-nginx-legacy/`, by
//! `admissionlab-recipes`' own `tests/ingress_nginx_legacy.rs`: waiting
//! on it would hang, and asserting on it would fail.
//!
//! ROADMAP Task 8.4 Step 2 says the same thing prescriptively ("using
//! recipe-specific endpoint, not cloud `LoadBalancer` status"), and this
//! module implements the only remaining honest definition of readiness:
//! **traffic**. The controller's data plane is located through the
//! recipe's own [`GatewayEndpointStrategy`], and the case is `ready`
//! when every one of its probes is answered as its
//! [`HttpProbeContract`] describes, within a caller-supplied deadline.
//!
//! That definition deliberately folds "the data plane is up" and "it is
//! serving *this* case" into one bit, because for an `Ingress` there is
//! nothing else to read. A consequence worth stating: a case whose
//! probes never match is `ready: false` even if the controller is
//! perfectly healthy and answering `404`. That is the honest report --
//! the legacy stack did not serve what the case says it serves -- and
//! the reason, including the last real response, is in
//! [`IngressCaseResult::diagnostics`].
//!
//! # Why the loop, and what it does and does not retry
//!
//! [`crate::probe::execute_http_probe`] retries only a connection that
//! is not ready yet, and returns a `404` as the observation it is --
//! deliberately, and that rule is right for a Gateway whose readiness
//! was established from its status. Here there is no status, and "nginx
//! has not reloaded its configuration yet" presents as a perfectly good
//! connection answering `404`. So this module runs its own outer loop,
//! bounded by the caller's deadline:
//!
//! - **A response that does not match the contract** is retried. The
//!   `attempts` count inside each [`crate::probe::HttpProbeResult`] is
//!   still that probe's own, unmodified; the outer rounds are not folded
//!   into it.
//! - **A [`GatewayError::ProbeUnavailable`]** -- no answer at all,
//!   after `execute_http_probe`'s own five-second window -- is also
//!   retried, because a controller `Pod` that has not started yet
//!   refuses connections through a port-forward that is already bound.
//! - **Every other probe error is returned immediately.**
//!   [`GatewayError::ProbeRequestInvalid`] is a contract that cannot be
//!   turned into a request and [`GatewayError::ProbeBodyTooLarge`] is a
//!   response too big to hash; neither becomes true by waiting.
//!
//! Each round re-sends *every* probe, and the recorded results are the
//! round in which all of them matched -- one result per contract probe,
//! in the order the case declares them, all measured within one round.
//! Task 8.5 pairs the two sides' probes by index, and a `probes` vector
//! assembled from different rounds would silently mix observations from
//! different states of the same cluster.
//!
//! # `probes` is either complete or empty
//!
//! There are exactly two shapes an [`IngressCaseResult`] can carry:
//! one result per contract probe, or none at all. A round that did not
//! fully match is *not* recorded, and neither is a partial round.
//!
//! That is a deliberate trade against keeping the last failing response
//! as evidence, and the reason is index pairing: Task 8.5 pairs
//! `probes[i]` with the candidate's `probes[i]`, so a shorter or
//! differently-assembled vector would compare probe 1 against probe 2
//! and report a routing difference that no cluster produced. The
//! evidence is not lost -- the last response's status and backend, or
//! the connection failure's own words, are in the
//! [`DIAGNOSTIC_INGRESS_NOT_SERVING`] diagnostic, where every renderer
//! already shows it. The same shape [`crate::case::GatewayCaseResult`]
//! documents for its own empty `probes`.
//!
//! # A webhook denial is evidence, not a failure (Step 4)
//!
//! The pinned legacy `ingress-nginx` release ships a validating
//! admission webhook, and
//! `fixtures/migration/ingress-nginx/webhook-deny.yaml` exists to be
//! rejected by it. When the API server refuses one of a case's baseline
//! objects, this module returns `Ok` with `admitted: false`,
//! `ready: false`, no probes, and a [`DIAGNOSTIC_INGRESS_DENIED`]
//! diagnostic carrying the API server's own `code`, `reason` and
//! `message` -- the same honesty `admissionlab_admission`'s
//! `AdmissionDecision::Rejected` applies to a dry-run fixture, and
//! exactly what [`crate::error`]'s own "what is, and is not, an error
//! here" reserved [`GatewayError::ApplyRejected`]'s verbatim fields for.
//!
//! **Only that one variant.** A transport failure, an unresolvable
//! `apiVersion`, an unreadable manifest file: all still errors. The
//! distinction is not "the apply failed" but "the API server answered,
//! with a decision" -- which is precisely the line
//! [`crate::apply`]'s `apply_failure` already draws by decoding a
//! Kubernetes `Status` before producing [`GatewayError::ApplyRejected`].
//!
//! **Any rejected object counts, not only the `Ingress`.** A case's
//! baseline manifests are applied as a unit and stop at the first
//! refusal, so a refused `Namespace` means the `Ingress` was never even
//! attempted; reporting that as "admitted" would be false, and reporting
//! it as an error would throw away a decision a real API server made.
//! The diagnostic names the object, so which one was refused is never in
//! doubt.
//!
//! # This module is CLI-independent
//!
//! [`run_ingress_case`] is the per-case primitive, in this crate, taking
//! a [`ClusterHandle`] and a [`admissionlab_core::ProcessSpawner`] and
//! returning evidence -- the same altitude
//! [`crate::reconcile::wait_for_route_reconciliation`] and
//! [`crate::probe::execute_http_probe`] sit at, and deliberately *not*
//! the altitude of `admissionlab-cli`'s `pipeline::gateway`, which owns
//! artifact writing, side bookkeeping and run diagnostics. Task 8.8
//! assembles a run out of this; nothing here knows a run exists.
//!
//! [`run_ingress_case_with_resolver`] is the seam, in the shape this
//! crate already uses everywhere (`apply_gateway_plan_with_client`,
//! `wait_for_route_reconciliation_with_source`,
//! `resolve_gateway_endpoint_with_client`): the thin production wrapper
//! builds the one implementation that must touch a network, and
//! everything downstream of it is a parameter.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use admissionlab_core::{ClusterHandle, Diagnostic, ProcessSpawner, RedactedValue};

use crate::apply::{AppliedGatewayFixture, apply_gateway_manifests};
use crate::endpoint::{
    GatewayEndpoint, GatewayEndpointResolver, GatewayEndpointStrategy, KubeGatewayEndpointResolver,
};
use crate::error::GatewayError;
use crate::migration::MigrationCaseSpec;
use crate::model::{GatewayIdentity, HttpProbeContract};
use crate::port_forward::start_service_port_forward;
use crate::probe::{HttpProbeResult, describe_probe_request, execute_http_probe};

/// The diagnostic code for a migration case whose baseline the API
/// server refused. See this module's "A webhook denial is evidence".
pub const DIAGNOSTIC_INGRESS_DENIED: &str = "ingress.admission_denied";

/// The diagnostic code for a migration case whose baseline was admitted
/// but never served the case's own traffic within the deadline. See this
/// module's "THE FINDING".
pub const DIAGNOSTIC_INGRESS_NOT_SERVING: &str = "ingress.not_serving";

/// How long to wait between two rounds of probing an `Ingress` that is
/// not serving the case's traffic yet.
///
/// The same value, chosen for the same reason, as
/// `admissionlab-cli`'s `pipeline::gateway::REOBSERVE_INTERVAL` and
/// `admissionlab-recipes`' own certification test: short enough that the
/// common case (already serving on the first round) is unaffected, long
/// enough that a case that will never be served is not probed in a tight
/// loop for the whole budget.
pub const INGRESS_REPROBE_INTERVAL: Duration = Duration::from_millis(500);

/// The API group an `Ingress` this module recognizes belongs to.
///
/// `networking.k8s.io` only. The long-removed `extensions/v1beta1`
/// spelling is not accepted: it is not served by any Kubernetes release
/// in `compatibility/kubernetes.yaml`, so a manifest using it fails at
/// [`crate::apply::plan_gateway_apply`]'s resource resolution long
/// before reaching here.
pub const INGRESS_GROUP: &str = "networking.k8s.io";

/// The plural an `Ingress` resource is served under, as it appears in an
/// `admissionlab_admission::ObjectKey` built from the cluster's *own*
/// discovery (never guessed from the kind -- see
/// `admissionlab_fixtures::resources`).
pub const INGRESS_RESOURCE: &str = "ingresses";

/// What one legacy `Ingress` migration case did on the baseline side.
///
/// §1.2's canonical shape, frozen at four fields by ROADMAP Task 8.4.
/// The `Ingress` counterpart of [`crate::case::GatewayCaseResult`], and
/// deliberately not the same type: an `Ingress` publishes no conditions,
/// so there is no `ReconciliationEvidence` to carry, and what stands in
/// its place is the pair of booleans this stack *can* answer.
///
/// `Serialize` but not `Deserialize`, like every other evidence type in
/// this crate: it is captured once from a live cluster and only ever
/// travels outward into a run's report.
///
/// No `Default`, for the reason [`crate::case::GatewayCaseResult`] gives
/// for refusing one: `admitted: false, ready: false` is a *statement*
/// about a cluster -- that a webhook refused the object and no traffic
/// was served -- and a value that could be fabricated by accident would
/// make that statement about a cluster nobody looked at.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngressCaseResult {
    /// Whether the API server accepted every one of the case's baseline
    /// objects.
    ///
    /// `false` means one of them was *refused, with a decision* -- a
    /// validating webhook said no, or the API server itself did. The
    /// refusal is preserved verbatim in `diagnostics`. It never means
    /// "the apply could not be attempted"; that is an error, not a
    /// result.
    pub admitted: bool,
    /// Whether the legacy stack served every one of the case's probes as
    /// contracted, within the deadline.
    ///
    /// Never read from `status.loadBalancer` -- see this module's "THE
    /// FINDING" for why that field is unusable and why traffic is the
    /// definition here.
    pub ready: bool,
    /// One result per [`MigrationCaseSpec::probes`] entry, in the order
    /// the case declares them, all from the single round in which every
    /// one of them matched -- or empty. See this module's "`probes` is
    /// either complete or empty".
    pub probes: Vec<HttpProbeResult>,
    /// What this case could not do, in the vocabulary every renderer
    /// already shows. Empty for a case that was admitted and served.
    pub diagnostics: Vec<Diagnostic>,
}

/// Runs one migration case's baseline (`Ingress`) side against
/// `cluster`.
///
/// Applies `case.baseline_ingress_manifests`, resolves the legacy
/// controller's data plane through `strategy`, opens a
/// `kubectl port-forward` to it, and probes until the case is served or
/// `deadline` passes. The port-forward is closed on every path,
/// including the one where a probe failed.
///
/// `deadline` bounds the *serving* wait only; the apply and the endpoint
/// resolution have their own bounds inside the calls that perform them
/// (Global Constraint 13).
///
/// # Errors
///
/// Returns whatever [`apply_gateway_manifests`] raised **except**
/// [`GatewayError::ApplyRejected`], which is reported as
/// `admitted: false` instead (this module's "A webhook denial is
/// evidence"); [`GatewayError::IngressCaseWithoutIngress`] if the
/// baseline applied no `Ingress`; the endpoint-resolution and
/// port-forward variants if the controller's data plane could not be
/// located or forwarded to; and
/// [`GatewayError::ProbeRequestInvalid`]/[`GatewayError::ProbeBodyTooLarge`]
/// from a probe. A probe that simply did not answer, or answered
/// something else, is never an error -- see this module's "Why the loop".
pub async fn run_ingress_case(
    cluster: &ClusterHandle,
    spawner: &dyn ProcessSpawner,
    case: &MigrationCaseSpec,
    strategy: &GatewayEndpointStrategy,
    deadline: Instant,
) -> Result<IngressCaseResult, GatewayError> {
    run_ingress_case_with_resolver(
        cluster,
        spawner,
        &KubeGatewayEndpointResolver::new(),
        case,
        strategy,
        deadline,
    )
    .await
}

/// [`run_ingress_case`] with the endpoint resolver supplied.
///
/// The seam this crate uses everywhere for the one step that must reach
/// a cluster to answer a question a test can answer offline -- see this
/// module's "This module is CLI-independent".
///
/// # Errors
///
/// See [`run_ingress_case`].
pub async fn run_ingress_case_with_resolver(
    cluster: &ClusterHandle,
    spawner: &dyn ProcessSpawner,
    endpoints: &dyn GatewayEndpointResolver,
    case: &MigrationCaseSpec,
    strategy: &GatewayEndpointStrategy,
    deadline: Instant,
) -> Result<IngressCaseResult, GatewayError> {
    let applied = match apply_gateway_manifests(cluster, &case.baseline_ingress_manifests).await {
        Ok(applied) => applied,
        Err(error) => {
            // The one variant that is a decision rather than a failure
            // to obtain one.
            return match admission_denial(&case.id, &error) {
                Some(diagnostic) => Ok(IngressCaseResult {
                    admitted: false,
                    ready: false,
                    probes: Vec::new(),
                    diagnostics: vec![diagnostic],
                }),
                None => Err(error),
            };
        }
    };

    let identity =
        applied_ingress_identity(&applied).ok_or(GatewayError::IngressCaseWithoutIngress {
            case: case.id.clone(),
        })?;
    let endpoint = endpoints.resolve(cluster, &identity, strategy).await?;
    probe_case(cluster, spawner, case, &endpoint, deadline).await
}

/// Reads a refused apply back as admission evidence, or [`None`] when
/// the error was not a decision at all.
///
/// Public because it *is* the rule this module's "A webhook denial is
/// evidence" states, and a rule stated in prose and implemented in a
/// `match` arm is a rule that can only be checked by reading both.
/// `tests/ingress_e2e.rs` drives it directly, with no cluster.
///
/// The API server's `message` reaches the diagnostic verbatim and as
/// [`RedactedValue::Public`]. That is deliberate and bounded: it is the
/// text a `kubectl apply` prints on the operator's own terminal, it is
/// already what [`GatewayError::ApplyRejected`]'s `Display` renders, and
/// the material PRODUCT.md §29.3 / Global Constraint 14 require redacted
/// (Secret data, authorization headers, private keys) cannot appear in a
/// refusal about an object this crate applied -- the object itself is
/// never echoed into it.
#[must_use]
pub fn admission_denial(case_id: &str, error: &GatewayError) -> Option<Diagnostic> {
    let GatewayError::ApplyRejected {
        cluster,
        object,
        code,
        reason,
        message,
    } = error
    else {
        return None;
    };

    let mut context = BTreeMap::new();
    context.insert("case".to_owned(), RedactedValue::Public(case_id.to_owned()));
    context.insert("cluster".to_owned(), RedactedValue::Public(cluster.clone()));
    context.insert("object".to_owned(), RedactedValue::Public(object.clone()));
    // `code` and `reason` are `Option` on the variant because the API
    // server genuinely may not have supplied them (see
    // `crate::apply::apply_failure` for why a `0` code becomes `None`),
    // and an absent key is how that is said here -- never a `"0"` or an
    // `"unknown"` standing in for something nobody reported (Global
    // Constraint 15).
    if let Some(code) = code {
        context.insert("code".to_owned(), RedactedValue::Public(code.to_string()));
    }
    if let Some(reason) = reason {
        context.insert("reason".to_owned(), RedactedValue::Public(reason.clone()));
    }
    context.insert("message".to_owned(), RedactedValue::Public(message.clone()));

    Some(Diagnostic {
        code: DIAGNOSTIC_INGRESS_DENIED.to_owned(),
        message: format!(
            "the API server refused {object} while persisting the baseline of migration case \
             {case_id:?}, so this case has no legacy routing behavior to observe: {message}"
        ),
        context,
    })
}

/// The `Ingress` a case's baseline applied, as a [`GatewayIdentity`], or
/// [`None`] if it applied none.
///
/// The **first** one in apply order when a case applies several: the
/// identity is used only to label the endpoint resolution and any error
/// it raises, because an `Ingress` controller is one shared data plane
/// with no per-object `Service` to select (see
/// `recipes/ingress-nginx-legacy/recipe.yaml`, which is why its
/// `gatewayEndpoint` carries no `{gatewayName}` placeholder). Picking
/// the first is therefore a naming choice, not a routing one -- but it
/// is a *stable* one, since `AppliedGatewayFixture::objects` is in apply
/// order and apply order is deterministic.
///
/// [`GatewayIdentity`] rather than a new type: it is §1.2's canonical
/// "which object's data plane are we asking about", and
/// [`GatewayEndpointResolver::resolve`] takes exactly that. Naming an
/// `Ingress` through it is what
/// `admissionlab-recipes`' own certification test already does by hand.
#[must_use]
pub fn applied_ingress_identity(applied: &AppliedGatewayFixture) -> Option<GatewayIdentity> {
    applied
        .objects
        .iter()
        .find(|key| key.group == INGRESS_GROUP && key.resource == INGRESS_RESOURCE)
        .map(|key| GatewayIdentity {
            // An `Ingress` is namespaced, so discovery-derived keys for
            // one always carry a namespace; `unwrap_or_default` is the
            // total form of that rather than a claim about a case that
            // cannot arise.
            namespace: key.namespace.clone().unwrap_or_default(),
            name: key.name.clone(),
        })
}

/// Whether `result` is what `contract` says the request should return.
///
/// Status always; backend only when the contract constrains it. A
/// contract with `expected_backend: None` "does not constrain *which*
/// backend answered, only the status" ([`HttpProbeContract`]'s own
/// words), so requiring the response to have identified one would reject
/// exactly the `404`/`403` probes that field exists to allow.
///
/// A response whose backend is [`None`] against a contract that names
/// one does **not** match: `None` means "which workload answered is
/// unknown", and a contract asking for `echo-a` has not been satisfied
/// by an unidentifiable answer.
#[must_use]
pub fn probe_matches_contract(result: &HttpProbeResult, contract: &HttpProbeContract) -> bool {
    result.status == contract.expected_status
        && match &contract.expected_backend {
            None => true,
            Some(expected) => result.backend.as_deref() == Some(expected.as_str()),
        }
}

/// Opens one port-forward, probes until the case is served or `deadline`
/// passes, and closes the forward on every path.
///
/// One forward for every round rather than one per round: they all
/// target the same data-plane `Service`, and re-spawning `kubectl`
/// between rounds would add a process spawn and a readiness wait to
/// every measured elapsed time (the same reason
/// `admissionlab-cli`'s `pipeline::gateway::probe_all` opens one).
///
/// The close is deliberately not behind a `?`. `PortForwardHandle::close`
/// consumes the handle, so the only way to hold one across a fallible
/// call and still close it is to keep both results and combine them
/// afterwards.
async fn probe_case(
    cluster: &ClusterHandle,
    spawner: &dyn ProcessSpawner,
    case: &MigrationCaseSpec,
    endpoint: &GatewayEndpoint,
    deadline: Instant,
) -> Result<IngressCaseResult, GatewayError> {
    let forward = start_service_port_forward(spawner, cluster, endpoint).await?;
    let started = Instant::now();
    let served = probe_until_served(forward.local_addr, case, deadline).await;
    let waited = started.elapsed();
    let closed = forward.close().await;

    // A probe failure is reported in preference to a close failure: it
    // is what the caller was trying to find out, and a `kubectl` that
    // could not be killed is a warning `ManagedChild`'s own drop path
    // already prints. Only one `GatewayError` can be returned, and
    // burying the diagnosis under a cleanup problem would be the wrong
    // choice of the two.
    let served = match (served, closed) {
        (Ok(served), Ok(())) => served,
        (Err(error), _) => return Err(error),
        (Ok(_), Err(close)) => return Err(close),
    };

    Ok(match served {
        Served::Yes(probes) => IngressCaseResult {
            admitted: true,
            ready: true,
            probes,
            diagnostics: Vec::new(),
        },
        Served::No(reason) => IngressCaseResult {
            admitted: true,
            ready: false,
            probes: Vec::new(),
            diagnostics: vec![not_serving_diagnostic(case, endpoint, waited, &reason)],
        },
    })
}

/// The outcome of the serving wait: every probe answered as contracted
/// in one round, or the reason the deadline passed without that
/// happening.
///
/// A two-variant enum rather than `Option<Vec<_>>` because the failing
/// side carries the diagnosis, and a `None` would throw away the one
/// thing a user needs.
enum Served {
    Yes(Vec<HttpProbeResult>),
    No(String),
}

/// Sends every one of `case`'s probes, in order, until they all match or
/// `deadline` passes.
///
/// See this module's "Why the loop" for exactly what is retried, and
/// "`probes` is either complete or empty" for why a partially matching
/// round is discarded rather than returned.
async fn probe_until_served(
    local_addr: std::net::SocketAddr,
    case: &MigrationCaseSpec,
    deadline: Instant,
) -> Result<Served, GatewayError> {
    let mut rounds: u32 = 0;
    loop {
        rounds = rounds.saturating_add(1);
        let round = probe_round(local_addr, &case.probes).await?;
        let mismatch = match round {
            Round::Complete(results) => return Ok(Served::Yes(results)),
            Round::Mismatched(reason) => reason,
        };
        if Instant::now() + INGRESS_REPROBE_INTERVAL >= deadline {
            return Ok(Served::No(format!(
                "after {rounds} round(s) of probing, the last one said: {mismatch}"
            )));
        }
        tokio::time::sleep(INGRESS_REPROBE_INTERVAL).await;
    }
}

/// One round's outcome.
enum Round {
    Complete(Vec<HttpProbeResult>),
    Mismatched(String),
}

/// Sends every probe once, stopping at the first that did not match.
///
/// Stopping early rather than completing the round: a round is only kept
/// when *all* of its probes matched, so once one has not, the remaining
/// requests cannot change the outcome and sending them would only slow
/// the retry down.
async fn probe_round(
    local_addr: std::net::SocketAddr,
    contracts: &[HttpProbeContract],
) -> Result<Round, GatewayError> {
    let mut results = Vec::with_capacity(contracts.len());
    for (index, contract) in contracts.iter().enumerate() {
        let result = match execute_http_probe(local_addr, contract).await {
            Ok(result) => result,
            // No answer at all is "not serving yet" at this altitude --
            // see this module's "Why the loop". Every other probe error
            // is returned.
            Err(error @ GatewayError::ProbeUnavailable { .. }) => {
                return Ok(Round::Mismatched(format!(
                    "probe {index} ({}) got no answer: {error}",
                    describe_probe_request(contract)
                )));
            }
            Err(other) => return Err(other),
        };
        if !probe_matches_contract(&result, contract) {
            return Ok(Round::Mismatched(describe_mismatch(
                index, contract, &result,
            )));
        }
        results.push(result);
    }
    Ok(Round::Complete(results))
}

/// Why one probe's response is not what its contract described.
///
/// Names the request through [`describe_probe_request`], so a header
/// value that is a credential is rendered redacted here exactly as it is
/// in a probe error.
fn describe_mismatch(
    index: usize,
    contract: &HttpProbeContract,
    result: &HttpProbeResult,
) -> String {
    let request = describe_probe_request(contract);
    if result.status != contract.expected_status {
        return format!(
            "probe {index} ({request}) expected HTTP {} and got {}",
            contract.expected_status, result.status
        );
    }
    format!(
        "probe {index} ({request}) reached backend {:?}, expected {:?}",
        result.backend, contract.expected_backend
    )
}

/// The diagnostic for a case that was admitted but never served.
fn not_serving_diagnostic(
    case: &MigrationCaseSpec,
    endpoint: &GatewayEndpoint,
    waited: Duration,
    reason: &str,
) -> Diagnostic {
    let mut context = BTreeMap::new();
    context.insert("case".to_owned(), RedactedValue::Public(case.id.clone()));
    context.insert(
        "endpoint".to_owned(),
        RedactedValue::Public(endpoint.to_string()),
    );
    context.insert(
        "probes".to_owned(),
        RedactedValue::Public(case.probes.len().to_string()),
    );
    // How long was actually spent waiting, measured with `Instant` so it
    // is monotonic -- not the budget the caller configured, which this
    // module never sees (it is handed a deadline, and a deadline that
    // had already passed produces a single round and a near-zero wait,
    // which is the truth about that call).
    context.insert(
        "waited".to_owned(),
        RedactedValue::Public(format!("{waited:?}")),
    );
    Diagnostic {
        code: DIAGNOSTIC_INGRESS_NOT_SERVING.to_owned(),
        message: format!(
            "the legacy Ingress stack never served migration case {:?} as its probes describe, so \
             no baseline traffic evidence was recorded for it; {reason}",
            case.id
        ),
        context,
    }
}
