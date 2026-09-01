//! The Gateway suite port: running one lab's configured Gateway route
//! contracts against one already-installed side, and writing what they
//! did (ROADMAP Task 6.11).
//!
//! # Why this port is declared here and not in `admissionlab-core`
//!
//! Every other external system a run drives reaches it through a trait
//! `admissionlab-core` declares — [`admissionlab_core::ClusterManager`],
//! [`admissionlab_core::StackInstaller`],
//! [`admissionlab_core::FixtureCapture`] — and `core`'s own `run.rs`
//! explains why: `LabRunner` has to drive them, and a trait it can name
//! is the only way to do that without depending on the crates that
//! implement them.
//!
//! That construction works because each of those traits can be *spelled*
//! without naming a type from the crate below it.
//! `FixtureCapture::capture_side` returns a `SideCapture`, which
//! deliberately carries paths on disk and no `AdmissionOutcome` at all —
//! and `capture.rs` in this same directory documents the consequence:
//! the outcomes themselves are taken from the implementation through
//! [`crate::pipeline::OutcomeCapture`], a trait declared *here*, because
//! `admissionlab-core` cannot name `admissionlab_admission`'s types.
//!
//! A Gateway suite cannot be split that way, and the reason is the shape
//! of the evidence rather than a preference. What one side produces is a
//! [`GatewayCaseResult`] per route contract, and every one of its three
//! fields is an `admissionlab-gateway` type
//! (`ReconciliationEvidence`, `HttpProbeResult`, and the contract id
//! that ties them together). There is no path-only summary a `core`
//! trait could return that the comparison stage could then work from: a
//! condition state is not a file location, and re-reading it from the
//! `reconciliation.json` this module writes is impossible for the same
//! reason `outcome.json` cannot be read back — the evidence types are
//! `Serialize`-only, because the `Diagnostic`s inside them render
//! redacted values as a literal with no faithful inverse.
//!
//! So the port is declared at the altitude that can name both halves,
//! which is this crate: the one that already depends on everything and
//! that nothing depends on. This is the *same* resolution §1.1's
//! `core -> gateway` arrow received when Task 6.1 landed
//! (`admissionlab-gateway/Cargo.toml` records it in full: the arrow
//! cannot coexist with `gateway -> core`, Cargo rejects the cycle, and
//! the workspace had already resolved that exact shape once for the
//! compare-and-report half of the pipeline), and the same one Controller
//! Ruling R22 reached for `ClusterManager` from the other direction —
//! there, the trait moved *down* into `core` because `LabRunner` had to
//! name it; here it stays *up* in the CLI because `LabRunner` cannot.
//!
//! The consequence is visible and deliberate: [`crate::pipeline::run_lab`]
//! drives this port itself, rather than through a `LabRunner` method,
//! and the two sides' concurrency is expressed here (`tokio::join!`, the
//! same discipline `LabRunner::capture_fixtures` uses and for the same
//! reason — one side failing must never abandon the other mid-flight,
//! least of all with a `kubectl port-forward` child still running).
//!
//! # Stage order within one side
//!
//! ```text
//! apply every manifest (server-side apply, fixed category order)
//! for each route contract, in the order the suite declares them:
//!   wait for reconciliation, to the suite's own deadline   <- evidence
//!   write raw/<side>/gateway/<contract-id>/reconciliation.json
//!   if the route is not carrying traffic, skip its probes  <- evidence
//!   otherwise: resolve the data-plane endpoint, port-forward,
//!              send each probe, close the forward
//!   write raw/<side>/gateway/<contract-id>/probes.json
//! ```
//!
//! **Reconciliation evidence is captured before any probe** (step 2),
//! and it is captured for every contract whether or not that contract
//! declares a probe: the status a controller published is the thing this
//! phase exists to compare, and a traffic result is an additional
//! observation on top of it, never a substitute.
//!
//! **A skipped probe is evidence, not silence** (step 3). See
//! [`probe_skip_reason`] for the exact rule and for why it is stated as
//! a reason rather than as an empty list.
//!
//! # What this module deliberately does not do
//!
//! It does not wait for anything the suite did not ask it to wait for.
//! `admissionlab-recipes`' own Task 6.10 certification test waits for
//! Istio's provisioned data-plane `Deployment` before asserting on
//! `Programmed`, because a *certification* is entitled to know what a
//! healthy Istio looks like. A lab run is not: a candidate whose Gateway
//! is `Programmed: False` because its data plane never came up is a
//! behavior difference the comparator must see, and a hidden extra wait
//! here would be this tool quietly waiting for the regression to go
//! away. [`admissionlab_spec::GatewaySuiteSpec::reconciliation_timeout`]
//! is the user's knob, and it is the only one.
//!
//! It also grades nothing. Whether a route's conditions changed for the
//! worse is `admissionlab-gateway`'s `diff_gateway` to claim and
//! `admissionlab-policy`'s to grade (Global Constraint 6).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use admissionlab_core::{
    ArtifactStore, ClusterHandle, Diagnostic, ProcessSpawner, RedactedValue, RunPaths, Side,
};
use admissionlab_gateway::{
    CONDITION_ACCEPTED, CONDITION_PROGRAMMED, CONDITION_RESOLVED_REFS, ConditionState,
    GatewayCaseResult, GatewayEndpoint, GatewayEndpointResolver, GatewayEndpointStrategy,
    GatewayError, HttpProbeResult, KubeGatewayEndpointResolver, ObservedCondition, ParentLookup,
    ReconciliationEvidence, RouteContract, apply_gateway_manifests, contract_gateway_identity,
    describe_probe_request, execute_http_probe, start_service_port_forward,
    wait_for_route_reconciliation,
};
use admissionlab_installer::{KubeReadinessProbe, ReadinessProbe};
use admissionlab_spec::{GatewaySuiteSpec, resolve_gateway_endpoint, resolve_readiness};
use async_trait::async_trait;
use serde::Serialize;

/// The subdirectory of `raw/<side>/` this suite's evidence lands in.
///
/// A directory of its own, beside the per-fixture admission bundles
/// rather than among them: a route contract id and a fixture id live in
/// the same namespace (see
/// [`crate::pipeline::compare::gateway_fixture_id`]) but their bundles
/// have entirely different contents, and a reader listing `raw/baseline/`
/// should be able to tell which is which without opening one.
pub const GATEWAY_RAW_DIR: &str = "gateway";

/// The applied-manifest provenance file, written once per side.
pub const APPLIED_ARTIFACT: &str = "applied.json";

/// One contract's reconciliation evidence, verbatim.
pub const RECONCILIATION_ARTIFACT: &str = "reconciliation.json";

/// One contract's traffic evidence: what was sent, and what was skipped.
pub const PROBES_ARTIFACT: &str = "probes.json";

/// The diagnostic code for a traffic probe that was not sent.
///
/// A run-level [`Diagnostic`] rather than a field on
/// [`GatewayCaseResult`], whose three fields §1.2 freezes. It is the
/// only place the *reason* for a skip is recorded in prose, so it is
/// recorded where every renderer already shows it — the terminal
/// report's Diagnostics section and `result.json`'s `diagnostics` array
/// — rather than in a fourth field this crate cannot add.
pub const DIAGNOSTIC_PROBE_SKIPPED: &str = "gateway.probe_skipped";

/// How long the suite's own readiness gate gets, per check.
///
/// Ten minutes, matching [`crate::pipeline::DEFAULT_COMPONENT_TIMEOUT`]
/// exactly, and for the same reason: the objects being waited on are
/// ordinary workloads whose slowest step is an image pull, and a bound
/// that fires on a slow pull turns a working suite into a spurious
/// failure. It is a separate constant rather than a reuse of that one
/// because the two are independent knobs that happen to agree today.
pub const READINESS_TIMEOUT: Duration = Duration::from_secs(600);

/// How long to wait before re-running the convergence rule on a route
/// that settled into a state it is not carrying traffic in.
///
/// Short enough that the common case (already correct on the first
/// observation) is unaffected, long enough that a route that will never
/// become healthy is not polled in a tight loop for the whole
/// reconciliation budget. The same value, chosen for the same reasons,
/// as `admissionlab-recipes`' own certification test.
pub const REOBSERVE_INTERVAL: Duration = Duration::from_millis(500);

/// The Gateway suite could not be run against one side.
///
/// Rendered down to a `String` in the same shape, and for the same
/// reason, as [`admissionlab_core::FixtureCaptureError`]: the caller
/// reports it and maps it to an exit code, and nothing downstream
/// matches on a [`GatewayError`] variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewaySuiteError {
    /// The [`RouteContract::id`] being run when this failed, or `None`
    /// for a failure that happened before any contract was reached (a
    /// manifest that would not apply).
    pub contract: Option<String>,
    /// A human-readable, safe-to-print explanation.
    pub message: String,
}

impl std::fmt::Display for GatewaySuiteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.contract {
            Some(contract) => write!(
                formatter,
                "Gateway route contract {contract:?} could not be observed: {}",
                self.message
            ),
            None => write!(formatter, "the Gateway suite failed: {}", self.message),
        }
    }
}

impl std::error::Error for GatewaySuiteError {}

/// What one side's Gateway suite produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideGatewayOutcome {
    /// Which side this is.
    pub side: Side,
    /// One result per route contract, in the order the suite declares
    /// them.
    pub cases: Vec<GatewayCaseResult>,
    /// Run-level findings this side produced — today, one per skipped
    /// probe. Already tagged with the side and the contract.
    pub diagnostics: Vec<Diagnostic>,
}

/// Running a lab's Gateway suite against one side.
///
/// One call per side, mirroring
/// [`admissionlab_core::FixtureCapture::capture_side`] — and, like it,
/// the *same implementation* is used for both sides, which is what makes
/// the two sides' results comparable at all. The suite itself is handed
/// to the implementation at construction rather than passed here, for
/// the reason that trait gives for doing the same with its fixtures.
#[async_trait]
pub trait GatewaySuiteRunner: Send + Sync {
    /// Applies the suite to `cluster` and observes every route contract.
    ///
    /// # Errors
    ///
    /// Returns [`GatewaySuiteError`] if the manifests could not be
    /// applied, if a route's status could not be read at all, if the
    /// data-plane endpoint of a route that *was* carrying traffic could
    /// not be resolved or forwarded to, or if evidence could not be
    /// written. A route that reconciled to a `False` condition is **not**
    /// a failure: that is an ordinary observation, and one of the two
    /// things this phase exists to see.
    async fn run_side(
        &self,
        cluster: &ClusterHandle,
        side: Side,
        paths: &RunPaths,
    ) -> Result<SideGatewayOutcome, GatewaySuiteError>;
}

/// The production [`GatewaySuiteRunner`]: real server-side applies, real
/// controller status, a real `kubectl port-forward`, and a real HTTP
/// request through the resulting data plane.
pub struct KubeGatewaySuite {
    /// The resolved suite, exactly as the lab configuration declared it.
    suite: GatewaySuiteSpec,
    /// Where evidence bundles are written.
    store: ArtifactStore,
    /// Spawns the long-lived `kubectl port-forward` child. A
    /// [`ProcessSpawner`] rather than a `ProcessRunner`: a forward is a
    /// process whose stdout is read line by line while it keeps running,
    /// not a command run to completion.
    spawner: Arc<dyn ProcessSpawner>,
    /// Turns the suite's endpoint strategy into a concrete
    /// namespace/Service/port against the live cluster.
    endpoints: KubeGatewayEndpointResolver,
    /// Waits out the suite's own `readiness:` gate. The same probe the
    /// installer uses for a component's readiness, rather than a second
    /// implementation that could disagree with it about what
    /// `Available` means.
    readiness: KubeReadinessProbe,
}

impl KubeGatewaySuite {
    /// Builds the production runner for `suite`.
    #[must_use]
    pub fn new(
        suite: GatewaySuiteSpec,
        store: ArtifactStore,
        spawner: Arc<dyn ProcessSpawner>,
    ) -> Self {
        Self {
            suite,
            store,
            spawner,
            endpoints: KubeGatewayEndpointResolver::new(),
            readiness: KubeReadinessProbe::new(),
        }
    }

    /// The suite's endpoint strategy, already validated by
    /// `admissionlab_spec::resolve_lab`, or `None` when the suite
    /// declares none.
    ///
    /// Resolving it here rather than at configuration load time keeps
    /// [`GatewaySuiteSpec`] the single type for both stages (see its own
    /// "One type for both the raw and the resolved stage" section). The
    /// error arm is unreachable — `resolve_lab` refuses a document whose
    /// strategy does not resolve — and is handled rather than unwrapped
    /// so this module encodes no assumption about another crate's
    /// internals that a refactor there could quietly break.
    fn endpoint_strategy(&self) -> Result<Option<GatewayEndpointStrategy>, GatewaySuiteError> {
        self.suite
            .gateway_endpoint
            .as_ref()
            .map(|spec| {
                resolve_gateway_endpoint(spec).map_err(|(locator, message)| GatewaySuiteError {
                    contract: None,
                    message: format!("the suite's gatewayEndpoint.{locator} is invalid: {message}"),
                })
            })
            .transpose()
    }

    /// Observes one route contract, writes its two evidence files, and
    /// returns what it saw plus any skip diagnostic.
    async fn run_contract(
        &self,
        cluster: &ClusterHandle,
        side: Side,
        paths: &RunPaths,
        contract: &RouteContract,
        strategy: Option<&GatewayEndpointStrategy>,
    ) -> Result<(GatewayCaseResult, Vec<Diagnostic>), GatewaySuiteError> {
        let fail = |message: String| GatewaySuiteError {
            contract: Some(contract.id.clone()),
            message,
        };

        // The suite's own knob, applied from *now* rather than from the
        // start of the side: each contract gets the full budget the user
        // configured, which is what makes a two-route suite's second
        // route as observable as its first.
        let deadline = Instant::now() + self.suite.reconciliation_timeout;
        let evidence = self
            .observe_route(cluster, contract, deadline)
            .await
            .map_err(&fail)?;

        let directory = contract_dir(paths, side, &contract.id);
        self.write_artifact(&directory, RECONCILIATION_ARTIFACT, &evidence)
            .await
            .map_err(&fail)?;

        let (probes, skipped, diagnostics) = self
            .run_probes(cluster, side, contract, strategy, &evidence)
            .await?;
        self.write_artifact(
            &directory,
            PROBES_ARTIFACT,
            &ProbeArtifact {
                contract_id: contract.id.clone(),
                sent: &probes,
                skipped: &skipped,
            },
        )
        .await
        .map_err(&fail)?;

        Ok((
            GatewayCaseResult {
                contract_id: contract.id.clone(),
                reconciliation: evidence,
                probes,
            },
            diagnostics,
        ))
    }

    /// Sends this contract's probes, or records why it sent none.
    ///
    /// Returns the results, the skip records, and the run-level
    /// diagnostics — three values rather than a struct because two of
    /// them go to different destinations (the artifact and the run) and
    /// the third is the case result itself.
    async fn run_probes(
        &self,
        cluster: &ClusterHandle,
        side: Side,
        contract: &RouteContract,
        strategy: Option<&GatewayEndpointStrategy>,
        evidence: &ReconciliationEvidence,
    ) -> Result<(Vec<HttpProbeResult>, Vec<SkippedProbe>, Vec<Diagnostic>), GatewaySuiteError> {
        if contract.probes.is_empty() {
            // Nothing was contracted, so nothing was skipped. A
            // reconciliation-only contract is explicitly a meaningful
            // one (`RouteContract::probes`' own documentation), and
            // reporting a skip for a probe that does not exist would be
            // noise.
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        }
        let reason = match strategy {
            None => Some(
                "this lab's gateway suite declares no gatewayEndpoint, so there is no \
                 data-plane Service to send a request through"
                    .to_owned(),
            ),
            Some(_) => probe_skip_reason(contract, evidence),
        };
        if let Some(reason) = reason {
            let skipped = contract
                .probes
                .iter()
                .enumerate()
                .map(|(index, probe)| SkippedProbe {
                    probe_index: index,
                    request: describe_probe_request(probe),
                    reason: reason.clone(),
                })
                .collect();
            return Ok((
                Vec::new(),
                skipped,
                vec![skip_diagnostic(side, &contract.id, &reason)],
            ));
        }

        let strategy = strategy.expect("a probe is only sent when a strategy resolved");
        let identity = contract_gateway_identity(contract);
        let endpoint = self
            .endpoints
            .resolve(cluster, &identity, strategy)
            .await
            .map_err(|error| GatewaySuiteError {
                contract: Some(contract.id.clone()),
                message: format!(
                    "the data-plane endpoint for Gateway {identity} could not be resolved: {error}"
                ),
            })?;
        let probes = self.probe_all(cluster, contract, &endpoint).await?;
        Ok((probes, Vec::new(), Vec::new()))
    }

    /// Opens one port-forward, sends every probe through it, and closes
    /// it on **every** path — including the one where a probe failed.
    ///
    /// One forward for all of a contract's probes rather than one each:
    /// they all target the same data-plane Service, and a fresh
    /// `kubectl` per request would add a process spawn and a readiness
    /// wait to every measured elapsed time.
    ///
    /// The close is not `?`-guarded anywhere. `PortForwardHandle::close`
    /// consumes the handle, so the only way to hold one across a
    /// fallible call and still close it is to keep the result and
    /// combine the two afterwards — exactly what
    /// `admissionlab-recipes`' own certification test does, and the
    /// reason a failed probe cannot leak a `kubectl` child here.
    async fn probe_all(
        &self,
        cluster: &ClusterHandle,
        contract: &RouteContract,
        endpoint: &GatewayEndpoint,
    ) -> Result<Vec<HttpProbeResult>, GatewaySuiteError> {
        let forward = start_service_port_forward(self.spawner.as_ref(), cluster, endpoint)
            .await
            .map_err(|error| GatewaySuiteError {
                contract: Some(contract.id.clone()),
                message: format!("a port-forward to {endpoint} could not be started: {error}"),
            })?;

        let mut results = Vec::with_capacity(contract.probes.len());
        let mut failure: Option<GatewayError> = None;
        for probe in &contract.probes {
            match execute_http_probe(forward.local_addr, probe).await {
                Ok(result) => results.push(result),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }

        let closed = forward.close().await;
        match (failure, closed) {
            (None, Ok(())) => Ok(results),
            (Some(error), Ok(())) => Err(GatewaySuiteError {
                contract: Some(contract.id.clone()),
                message: format!("a traffic probe through {endpoint} failed: {error}"),
            }),
            (None, Err(close)) => Err(GatewaySuiteError {
                contract: Some(contract.id.clone()),
                message: format!("the port-forward to {endpoint} could not be closed: {close}"),
            }),
            (Some(error), Err(close)) => Err(GatewaySuiteError {
                contract: Some(contract.id.clone()),
                message: format!(
                    "a traffic probe through {endpoint} failed: {error}; additionally, the \
                     port-forward could not be closed: {close}"
                ),
            }),
        }
    }

    /// Observes one route until it is carrying traffic, or until
    /// `deadline`.
    ///
    /// # Why this is a loop and not one call
    ///
    /// [`wait_for_route_reconciliation`] answers "has this route's
    /// status stopped changing?". That is the right question for an
    /// evidence engine and it is *not* the same question as "has the
    /// implementation finished?" -- `reconcile.rs` says so in as many
    /// words, and Task 6.10 measured the gap on real clusters: Istio
    /// publishes a stable, current, settled `Programmed: False
    /// (AddressNotAssigned)` within ~270ms of a `Gateway` being applied,
    /// every single time, because the data plane it is describing has
    /// not been provisioned yet. It clears on its own a second or two
    /// later.
    ///
    /// Taking that first answer would report a snapshot of the *middle*
    /// of reconciliation as this route's behavior -- identically on both
    /// sides, so it would not manufacture a difference, but it would
    /// make every traffic probe a lab ever sends be skipped, and would
    /// compare two mid-flight statuses instead of two settled ones.
    ///
    /// So this spends the budget the user gave: it re-runs the *whole*
    /// convergence rule (never a hand-rolled poll of its parts) until
    /// the route is carrying traffic or
    /// [`GatewaySuiteSpec::reconciliation_timeout`] elapses, and returns
    /// the last evidence either way.
    ///
    /// **This is not a hidden wait, and it cannot hide a regression.**
    /// It is the only wait, its bound is the user's own knob, and a
    /// route that never becomes healthy returns exactly the evidence it
    /// did reach -- its `False` conditions intact, its `converged` flag
    /// whatever the rule says -- for `diff_gateway` to claim and
    /// `admissionlab-policy` to grade. What it costs is real and is
    /// stated here rather than hidden: a genuinely broken route spends
    /// the full `reconciliationTimeout` before the run moves on, which
    /// is what a timeout is for.
    async fn observe_route(
        &self,
        cluster: &ClusterHandle,
        contract: &RouteContract,
        deadline: Instant,
    ) -> Result<ReconciliationEvidence, String> {
        observe_route(cluster, contract, deadline).await
    }

    /// Writes one JSON artifact into `directory`, creating it first.
    ///
    /// [`ArtifactStore`] never creates directories (its own
    /// documentation is explicit), so the `create_dir_all` here is the
    /// same one `admissionlab_admission::capture::write_evidence`
    /// performs for the admission bundle beside this one.
    async fn write_artifact<T: Serialize + Sync>(
        &self,
        directory: &std::path::Path,
        name: &str,
        value: &T,
    ) -> Result<(), String> {
        write_artifact(&self.store, directory, name, value).await
    }
}

/// Observes one route until it is carrying traffic, or until `deadline`
/// — the loop [`KubeGatewaySuite::observe_route`] documents, as a free
/// function so ROADMAP Task 8.8's migration runner drives the *same*
/// one.
///
/// A free function rather than a second copy for the reason §1.2 gives
/// for refusing competing synonyms: the argument for this loop's
/// existence (Istio publishes a stable, current, settled
/// `Programmed: False (AddressNotAssigned)` within ~270 ms of a
/// `Gateway` being applied, and taking that first answer would report
/// the middle of reconciliation as the route's behavior) is the same
/// argument on either side of a migration comparison, and two
/// implementations of it would be free to answer differently about the
/// same cluster.
///
/// # Errors
///
/// Returns the rendered [`GatewayError`] if the route's status could not
/// be read at all. A route that reconciled to a `False` condition is
/// **not** an error — that is the evidence.
pub async fn observe_route(
    cluster: &ClusterHandle,
    contract: &RouteContract,
    deadline: Instant,
) -> Result<ReconciliationEvidence, String> {
    // The freshest observation that actually satisfied the
    // convergence rule. Kept because the *last* observation need not
    // have: once the deadline is close, an inner wait has no room
    // left for the two-poll stability window, so it returns
    // `converged: false` about a status that had already settled
    // several observations ago. Reporting that would turn "the route
    // settled on rejecting this backend" into "we could not tell",
    // and would make the two sides incomparable for a reason that is
    // an artefact of this loop rather than a fact about the cluster.
    let mut settled: Option<ReconciliationEvidence> = None;
    loop {
        let evidence = wait_for_route_reconciliation(cluster, contract, deadline)
            .await
            .map_err(|error| error.to_string())?;
        if probe_skip_reason(contract, &evidence).is_none() {
            return Ok(evidence);
        }
        let out_of_time = Instant::now() + REOBSERVE_INTERVAL >= deadline;
        match (evidence.converged, out_of_time) {
            // Settled, but not on a state that carries traffic. Keep
            // it as the best answer so far and look again: Istio's
            // first settled status routinely is a transient one.
            (true, false) => settled = Some(evidence),
            (true, true) => return Ok(evidence),
            // Never settled, and the budget is gone. The honest
            // answer is the freshest settled observation when there
            // was one, and this unconverged one otherwise -- which
            // the comparator is the only thing allowed to interpret.
            (false, true) => return Ok(settled.unwrap_or(evidence)),
            (false, false) => {}
        }
        tokio::time::sleep(REOBSERVE_INTERVAL).await;
    }
}

impl KubeGatewaySuite {
    /// Waits out the suite's own `readiness:` gate, if it declared one.
    ///
    /// After the manifests are applied and before any route is observed:
    /// applying a manifest returns when its objects exist, and a backend
    /// with no ready pod answers a request with the data plane's own
    /// `503` (see [`GatewaySuiteSpec::readiness`] for the full argument,
    /// including why the entries should name the *suite's own* objects).
    ///
    /// An unsatisfied check is a failure rather than a diagnostic,
    /// unlike a route that does not converge: the user asserted this
    /// condition holds before anything is observed, and continuing past
    /// it would produce evidence about a cluster that is not the one
    /// they described.
    async fn wait_for_readiness(&self, cluster: &ClusterHandle) -> Result<(), GatewaySuiteError> {
        for check in &self.suite.readiness {
            let resolved = resolve_readiness(check);
            let evidence = self
                .readiness
                .wait(cluster, &resolved, Instant::now() + READINESS_TIMEOUT)
                .await
                .map_err(|error| GatewaySuiteError {
                    contract: None,
                    message: format!("a readiness check could not be attempted: {error}"),
                })?;
            if !evidence.satisfied {
                return Err(GatewaySuiteError {
                    contract: None,
                    message: format!(
                        "the suite's readiness check {resolved:?} was not satisfied within \
                         {READINESS_TIMEOUT:?}; nothing was observed, because the cluster is not \
                         yet the one this suite describes"
                    ),
                });
            }
        }
        Ok(())
    }
}

/// Writes one JSON artifact into `directory`, creating it first.
///
/// [`ArtifactStore`] never creates directories (its own documentation is
/// explicit), so the `create_dir_all` here is the same one
/// `admissionlab_admission::capture::write_evidence` performs for the
/// admission bundle beside this one.
///
/// A free function taking the store for the reason [`observe_route`]
/// gives for being one: ROADMAP Task 8.8's migration runner writes its
/// evidence into the same `raw/<side>/` tree with the same atomicity and
/// the same "create the directory, then write" order, and two copies of
/// four lines is two places for that order to change.
///
/// # Errors
///
/// Returns a rendered message naming either the directory that could not
/// be created or the file that could not be written.
pub async fn write_artifact<T: Serialize + Sync>(
    store: &ArtifactStore,
    directory: &std::path::Path,
    name: &str,
    value: &T,
) -> Result<(), String> {
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(|error| {
            format!(
                "its evidence directory {} could not be created: {error}",
                directory.display()
            )
        })?;
    store
        .write_json_atomic(&directory.join(name), value)
        .await
        .map_err(|error| format!("{name} could not be written: {error}"))
}

#[async_trait]
impl GatewaySuiteRunner for KubeGatewaySuite {
    async fn run_side(
        &self,
        cluster: &ClusterHandle,
        side: Side,
        paths: &RunPaths,
    ) -> Result<SideGatewayOutcome, GatewaySuiteError> {
        let strategy = self.endpoint_strategy()?;

        let applied = apply_gateway_manifests(cluster, &self.suite.manifests)
            .await
            .map_err(|error| GatewaySuiteError {
                contract: None,
                message: error.to_string(),
            })?;
        // Provenance for what this side now holds, written before the
        // first observation so a run that dies mid-suite still says what
        // it put in the cluster.
        self.write_artifact(
            &side_dir(paths, side),
            APPLIED_ARTIFACT,
            &AppliedArtifact {
                objects: applied.objects.iter().map(ToString::to_string).collect(),
                source_hashes: applied
                    .source_hashes
                    .iter()
                    .map(|(path, digest)| (path.display().to_string(), digest.clone()))
                    .collect(),
            },
        )
        .await
        .map_err(|message| GatewaySuiteError {
            contract: None,
            message,
        })?;

        self.wait_for_readiness(cluster).await?;

        let mut cases = Vec::with_capacity(self.suite.routes.len());
        let mut diagnostics = Vec::new();
        for contract in &self.suite.routes {
            let (case, contract_diagnostics) = self
                .run_contract(cluster, side, paths, contract, strategy.as_ref())
                .await?;
            cases.push(case);
            diagnostics.extend(contract_diagnostics);
        }
        Ok(SideGatewayOutcome {
            side,
            cases,
            diagnostics,
        })
    }
}

/// Why this contract's probes must not be sent, or [`None`] when they
/// must.
///
/// # The rule
///
/// A probe is sent only when the route is actually carrying traffic for
/// this contract, which takes three published `True` conditions:
///
/// - the `Gateway`'s [`CONDITION_PROGRAMMED`] — its data plane exists
///   and is configured;
/// - the contract's own parent entry's [`CONDITION_ACCEPTED`] — the
///   route attached to the listener the contract names;
/// - that same parent's [`CONDITION_RESOLVED_REFS`] — the route's
///   backends resolve.
///
/// Anything else is a skip, and the returned reason names the specific
/// condition, its state, and the controller's own reason for it.
///
/// # Why a skip rather than a probe
///
/// Not to spare the run a request: a request *would* get an answer, and
/// that answer would be the data plane's own error page. Gateway API
/// specifies a `503` for a route whose parent did not accept it and a
/// `500` for a rule whose backend does not resolve, so probing anyway
/// would record a status that says nothing about this route's own
/// behavior and everything about the implementation's chosen failure
/// code — and would then be compared, as a traffic difference, against a
/// baseline that answered from a real backend. The condition change is
/// the finding; a status code invented by the same broken state is not a
/// second, independent one.
///
/// # Why the skip is not silence
///
/// ROADMAP Task 6.11 step 3, in as many words: the traffic probe is
/// skipped *with an explicit reason*, and the status regression stays
/// visible. Three things carry that here, and all three are needed —
/// the reason is written into `probes.json` beside the request that was
/// not sent, it is raised as a [`DIAGNOSTIC_PROBE_SKIPPED`] run
/// diagnostic that every renderer shows, and the empty `probes` list
/// itself becomes a `traffic_status_changed` claim in `diff_gateway`
/// whenever the *other* side answered (that comparator's "a probe only
/// one side answered" rule). The condition change is claimed
/// independently and is not affected by any of this.
#[must_use]
pub fn probe_skip_reason(
    contract: &RouteContract,
    evidence: &ReconciliationEvidence,
) -> Option<String> {
    let programmed = evidence.gateway.condition(CONDITION_PROGRAMMED);
    if programmed.state != ConditionState::True {
        return Some(format!(
            "Gateway {} published {}, so its data plane is not carrying traffic for this route",
            evidence.gateway.identity,
            condition_text(&programmed),
        ));
    }

    let parent = match evidence.route.parent_for(contract) {
        ParentLookup::Found(parent) => parent,
        ParentLookup::Absent => {
            return Some(format!(
                "HTTPRoute {}/{} published no status entry for the Gateway and listener this \
                 contract names, so it is not attached to anything a request could arrive \
                 through",
                evidence.route.namespace, evidence.route.name,
            ));
        }
        ParentLookup::Ambiguous(count) => {
            return Some(format!(
                "HTTPRoute {}/{} published {count} status entries matching this contract's \
                 parent; which one describes the listener a request would arrive on cannot be \
                 determined",
                evidence.route.namespace, evidence.route.name,
            ));
        }
    };
    for type_name in [CONDITION_ACCEPTED, CONDITION_RESOLVED_REFS] {
        let condition = parent.condition(type_name);
        if condition.state != ConditionState::True {
            return Some(format!(
                "HTTPRoute {}/{} published {} for this contract's parent, so a request through \
                 it would be answered by the data plane's own error handling rather than by a \
                 backend",
                evidence.route.namespace,
                evidence.route.name,
                condition_text(&condition),
            ));
        }
    }
    None
}

/// One condition as a skip reason names it: `Programmed=False
/// (AddressNotAssigned)`.
fn condition_text(condition: &ObservedCondition) -> String {
    let state = match condition.state {
        ConditionState::True => "True",
        ConditionState::False => "False",
        ConditionState::Unknown => "Unknown",
        ConditionState::Missing => "Missing",
    };
    match &condition.reason {
        Some(reason) => format!("{}={state} ({reason})", condition.type_name),
        None => format!("{}={state}", condition.type_name),
    }
}

/// The run-level diagnostic for one side's skipped probes.
fn skip_diagnostic(side: Side, contract_id: &str, reason: &str) -> Diagnostic {
    let mut context = BTreeMap::new();
    context.insert("side".to_owned(), RedactedValue::Public(side.to_string()));
    context.insert(
        "contract".to_owned(),
        RedactedValue::Public(contract_id.to_owned()),
    );
    Diagnostic {
        code: DIAGNOSTIC_PROBE_SKIPPED.to_owned(),
        message: format!(
            "no traffic probe was sent for route contract {contract_id:?} on the {side} side: \
             {reason}"
        ),
        context,
    }
}

/// `raw/<side>/gateway/`.
fn side_dir(paths: &RunPaths, side: Side) -> PathBuf {
    paths.raw().join(side.as_str()).join(GATEWAY_RAW_DIR)
}

/// `raw/<side>/gateway/<contract-id>/`.
///
/// The contract id is safe as a path segment for the same reason a
/// fixture id is: [`crate::pipeline::compare::gateway_fixture_id`]
/// requires every contract id to parse as an
/// [`admissionlab_core::FixtureId`] before the run starts, and that
/// grammar (lowercase ASCII, digits, `-`) contains no separator and no
/// `..`.
fn contract_dir(paths: &RunPaths, side: Side, contract_id: &str) -> PathBuf {
    side_dir(paths, side).join(contract_id)
}

/// `applied.json`: what this side's suite put into the cluster.
///
/// Object keys and source digests as strings, so the file is readable
/// without this crate. `AppliedGatewayFixture`'s own types are not
/// serialized directly because its `objects` are `ObjectKey`s whose
/// `Display` is the form a reader wants (`namespace/name` with the
/// resource plural) and its `source_hashes` is keyed by `PathBuf`, which
/// JSON has no object-key representation for.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppliedArtifact {
    /// Every object applied, in apply order.
    objects: Vec<String>,
    /// Each manifest file's SHA-256, keyed by path.
    source_hashes: BTreeMap<String, String>,
}

/// `probes.json`: what was sent, and what was not.
///
/// Both halves in one file rather than an empty file plus a diagnostic:
/// a reader looking for "what did this route's traffic do" opens one
/// path, and finds either results or the reason there are none.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeArtifact<'a> {
    /// The contract these probes belong to.
    contract_id: String,
    /// Every probe that was answered, in contract order.
    sent: &'a [HttpProbeResult],
    /// Every probe that was not sent, with the request it would have
    /// been and why it was skipped.
    skipped: &'a [SkippedProbe],
}

/// One probe that was contracted but not sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkippedProbe {
    /// Which of the contract's probes this is, by index — the same key
    /// `admissionlab_gateway::diff` pairs probes on.
    probe_index: usize,
    /// The request that was not sent, rendered by
    /// [`describe_probe_request`] so it reads exactly as it does in a
    /// probe error.
    request: String,
    /// Why it was not sent. See [`probe_skip_reason`].
    reason: String,
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsString;
    use std::path::PathBuf;

    use admissionlab_core::{
        ClusterSpec, CommandSpec, ManagedChild, ProcessError, TokioProcessRunner,
    };
    use admissionlab_gateway::{GatewayEndpoint, HttpProbeContract};

    use super::*;

    /// A [`ProcessSpawner`] that ignores the argv it is given and spawns
    /// a stand-in that behaves like a healthy `kubectl port-forward`:
    /// it announces a local address on stdout and then stays alive.
    ///
    /// Substituting the *command* rather than the child is what makes
    /// this a real test of the runner's cleanup: everything after the
    /// spawn -- the readiness parse, the handle, the probe, the close --
    /// is the production code path over a real OS process, and the only
    /// thing faked is which binary is on the other end of it.
    struct AnnouncingSpawner {
        /// A unique token in the child's argv, so the test can ask the
        /// OS whether that specific process still exists.
        marker: String,
        /// The port the stand-in announces. Chosen closed, so the probe
        /// that follows genuinely fails.
        port: u16,
    }

    #[async_trait]
    impl ProcessSpawner for AnnouncingSpawner {
        async fn spawn(&self, _spec: CommandSpec) -> Result<ManagedChild, ProcessError> {
            let script = format!(
                "echo 'Forwarding from 127.0.0.1:{} -> 80'; while true; do sleep 1; done",
                self.port
            );
            TokioProcessRunner::new()
                .spawn(CommandSpec {
                    program: "bash".into(),
                    args: vec![
                        OsString::from("-c"),
                        OsString::from(script),
                        // `$0`, which is what shows up in the process
                        // table and is what `pgrep -f` matches on.
                        OsString::from(self.marker.clone()),
                    ],
                    cwd: None,
                    env: BTreeMap::new(),
                    sensitive_env_keys: BTreeSet::new(),
                    timeout: Duration::from_secs(60),
                })
                .await
        }
    }

    /// Whether any process still has `marker` in its command line.
    fn process_exists(marker: &str) -> bool {
        std::process::Command::new("pgrep")
            .arg("-f")
            .arg(marker)
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn cluster() -> ClusterHandle {
        ClusterHandle {
            spec: ClusterSpec {
                side: Side::Baseline,
                name: "adlab-baseline-test".to_owned(),
                kubernetes_version: "1.36.4".to_owned(),
                node_image: "kindest/node:v1.36.4".to_owned(),
                images: Vec::new(),
            },
            kubeconfig: PathBuf::from("/tmp/admissionlab-gateway-unit.kubeconfig"),
            audit_log: PathBuf::from("/tmp/admissionlab-gateway-unit-audit.log"),
        }
    }

    fn contract() -> RouteContract {
        RouteContract {
            id: "echo-route".to_owned(),
            gateway_namespace: "demo".to_owned(),
            gateway_name: "lab-gateway".to_owned(),
            route_namespace: "demo".to_owned(),
            route_name: "echo-route".to_owned(),
            listener_name: Some("http".to_owned()),
            probes: vec![HttpProbeContract {
                host: "echo.example.test".to_owned(),
                path: "/".to_owned(),
                method: "GET".to_owned(),
                headers: BTreeMap::new(),
                expected_status: 200,
                expected_backend: None,
            }],
        }
    }

    /// A port nothing is listening on, so the probe below fails for the
    /// reason the test is about rather than by accident.
    ///
    /// Bound and immediately dropped: the kernel will not hand the same
    /// ephemeral port to anything else within this test's lifetime, and
    /// asking it for a free one is more reliable than picking a literal
    /// and hoping.
    fn closed_port() -> u16 {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port must be bindable");
        listener
            .local_addr()
            .expect("a bound listener has an address")
            .port()
    }

    /// The exit gate's "port-forward processes are cleaned on all paths"
    /// bullet, on the path that is easiest to get wrong: the forward
    /// started, and then the *probe* failed.
    ///
    /// `admissionlab-gateway`'s own `tests/port_forward_unit.rs` already
    /// covers the failures that happen before a handle exists (a
    /// `kubectl` that will not spawn, one that exits early, one that
    /// never announces a port -- each of which terminates the child
    /// before returning). This covers the one it cannot: once
    /// `start_service_port_forward` has returned a live handle, the
    /// child's lifetime belongs to *this* module, and the only thing
    /// standing between a failed probe and a leaked `kubectl` is that
    /// `close()` is not behind a `?`.
    #[tokio::test]
    async fn a_failed_probe_still_closes_the_port_forward() {
        let marker = format!("admissionlab-forward-marker-{}", std::process::id());
        let suite = KubeGatewaySuite::new(
            GatewaySuiteSpec {
                manifests: vec![PathBuf::from("/dev/null")],
                routes: vec![contract()],
                reconciliation_timeout: Duration::from_secs(1),
                gateway_endpoint: None,
                readiness: Vec::new(),
            },
            ArtifactStore::new(std::path::Path::new("/tmp")),
            Arc::new(AnnouncingSpawner {
                marker: marker.clone(),
                port: closed_port(),
            }),
        );
        let endpoint = GatewayEndpoint {
            namespace: "demo".to_owned(),
            service: "lab-gateway-istio".to_owned(),
            port: 80,
        };

        let error = suite
            .probe_all(&cluster(), &contract(), &endpoint)
            .await
            .expect_err("a probe against a closed port must fail");

        assert_eq!(error.contract.as_deref(), Some("echo-route"));
        assert!(
            error.message.contains("traffic probe"),
            "the probe failure must be what is reported, not the close: {}",
            error.message
        );
        assert!(
            !process_exists(&marker),
            "the port-forward child survived a failed probe -- `close()` must not be reachable \
             only on the success path"
        );
    }

    /// The same discipline on the ordinary path: a probe that succeeded
    /// still closes the forward.
    ///
    /// Driven through the same stand-in, against a port that *is*
    /// listening and answers one minimal HTTP response, so the probe
    /// returns a result rather than an error.
    #[tokio::test]
    async fn a_successful_probe_also_closes_the_port_forward() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port must be bindable");
        let port = listener
            .local_addr()
            .expect("a bound listener has an address")
            .port();
        // One connection, one fixed response, then done. Not an echo
        // backend: what is under test here is the forward's lifetime,
        // and `backend: None` is the honest answer for a response that
        // did not identify itself.
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::AsyncWriteExt as _;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                    .await;
                let _ = stream.flush().await;
            }
        });

        let marker = format!("admissionlab-forward-ok-marker-{}", std::process::id());
        let suite = KubeGatewaySuite::new(
            GatewaySuiteSpec {
                manifests: vec![PathBuf::from("/dev/null")],
                routes: vec![contract()],
                reconciliation_timeout: Duration::from_secs(1),
                gateway_endpoint: None,
                readiness: Vec::new(),
            },
            ArtifactStore::new(std::path::Path::new("/tmp")),
            Arc::new(AnnouncingSpawner {
                marker: marker.clone(),
                port,
            }),
        );
        let endpoint = GatewayEndpoint {
            namespace: "demo".to_owned(),
            service: "lab-gateway-istio".to_owned(),
            port: 80,
        };

        let probes = suite
            .probe_all(&cluster(), &contract(), &endpoint)
            .await
            .expect("a probe that got an answer must succeed");

        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].status, 200);
        assert!(
            !process_exists(&marker),
            "the port-forward child survived a successful probe"
        );
    }
}
