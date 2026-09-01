//! The Ingress-to-Gateway migration suite port: running one lab's
//! `migration:` cases across **both** clusters and reporting what the
//! two stacks did with the same requests (ROADMAP Task 8.8).
//!
//! # Why this exists at all, and why here
//!
//! ROADMAP Phase 8 builds the migration suite in four tasks — 8.3's
//! configuration surface, 8.4's legacy `Ingress` runner, 8.5's
//! comparator, 8.8's canonical demo — and gives none of them a lab run
//! to happen inside. There is no roadmap task named "wire the migration
//! suite into `admissionlab test`", and yet Task 8.8's demo is an
//! `admissionlab.yaml` executed by the real binary. So this module is
//! Task 8.8's: without it the demo cannot exist.
//!
//! It is declared beside [`crate::pipeline::gateway`] and for exactly
//! the reason that module sets out at length: what a migration case
//! produces is an `admissionlab_gateway::IngressCaseResult` and an
//! `admissionlab_gateway::GatewayCaseResult`, both `Serialize`-only
//! evidence types owned by `admissionlab-gateway`, so a trait
//! `admissionlab-core` could name would have to return either paths on
//! disk (which cannot be read back — the `Diagnostic`s inside them
//! render redacted values with no faithful inverse) or a type `core`
//! cannot see. The port therefore stays at the altitude that can name
//! both halves, which is this crate.
//!
//! # One call, two clusters — unlike the Gateway port
//!
//! [`GatewaySuiteRunner::run_side`](crate::pipeline::GatewaySuiteRunner::run_side)
//! is called once per side, with the same implementation, because a
//! Gateway suite applies the *same manifests* to two clusters: "the same
//! suite, run twice" is literally what makes the two results comparable.
//!
//! A migration suite is the opposite by construction. Its baseline half
//! is a set of `Ingress` manifests served by an `Ingress` controller and
//! its candidate half is a set of Gateway API manifests served by a
//! Gateway API implementation — different objects, different
//! controllers, different readiness vocabularies, different data-plane
//! discovery. There is no "run this side" that both sides could share,
//! so [`MigrationSuiteRunner::run`] takes both handles and drives the
//! two halves itself, `tokio::join!`-style, one case at a time.
//!
//! # The two readiness vocabularies, kept apart
//!
//! **Baseline.** `admissionlab_gateway::run_ingress_case`, unchanged.
//! Its "THE FINDING" section is the argument: an `Ingress` publishes no
//! status worth waiting on (`status.loadBalancer` stays empty forever on
//! `kind`), so readiness is defined as *traffic* — the case is ready
//! when every one of its probes answers as its contract describes.
//!
//! **Candidate.** [`crate::pipeline::gateway::observe_route`], the same
//! loop a Gateway suite uses, because a Gateway API implementation
//! *does* publish `Accepted`/`ResolvedRefs`/`Programmed` and waiting on
//! those is what makes its probes deterministic.
//!
//! # THE ONE DELIBERATE WAIT, and why it cannot hide a regression
//!
//! After the candidate's route is carrying traffic, this module probes
//! it in **rounds**, re-sending every one of the case's probes until
//! either they all match the case's contracts or
//! [`MIGRATION_SERVING_TIMEOUT`] passes — and then records the last
//! complete round, whatever it says. That is a real decision with a real
//! cost, so both halves are stated:
//!
//! - **Why.** A candidate's echo backends are applied in the same batch
//!   as its `Gateway` and `HTTPRoute`, and a route can be fully
//!   `Programmed` seconds before a backend `Pod` is `Ready`. A single
//!   request sent into that window is answered by the data plane's own
//!   `502`/`503` — a status invented by this run's timing — and would be
//!   compared against the baseline's real `200` as though it were a
//!   migration regression. The alternative to waiting is a flaky
//!   comparator, and the Gateway suite avoids the same race only because
//!   `GatewaySuiteSpec::readiness` lets a user declare the backends by
//!   name. A migration suite has no such list, and this is the honest
//!   substitute.
//! - **What it cannot do.** It cannot wait a regression away. The loop
//!   exits early only when the candidate reproduces the baseline's
//!   contracted behavior — which is precisely "there is no regression" —
//!   and otherwise spends the whole budget and reports what it finally
//!   saw. So a difference this module reports is one that *persisted for
//!   the whole budget*, which is strictly stronger evidence than a
//!   single early request.
//! - **What it does cost.** A case with a genuine regression spends the
//!   full [`MIGRATION_SERVING_TIMEOUT`] before the run moves on, and a
//!   difference that is genuinely transient (present at second 1, gone
//!   by second 90) is not reported. That second one is a real blind
//!   spot, stated rather than hidden; it is the same trade
//!   `crate::pipeline::gateway`'s own re-observation loop already makes
//!   for reconciliation.
//!
//! # Grading: a parallel, minimal scale — and `admissionlab-policy` is
//! # untouched
//!
//! `admissionlab_gateway::migration` is explicit that a
//! `MigrationBehaviorChange` is **not** an
//! `admissionlab_diff::SemanticChange` and must never be turned into
//! one: the two vocabularies answer different questions, and collapsing
//! six routing behaviors into `traffic_status_changed` would destroy the
//! classification Task 8.5 exists to produce. It is equally explicit
//! that grading is *not* the Gateway crate's job (Global Constraint 6).
//!
//! So the grade is decided here, by [`grade`], in nine lines over
//! `(kind, expected)`. It reuses `admissionlab_policy::Severity`'s three
//! words and nothing else — no `SemanticChangeKind`, no
//! `expectations.yaml`, no `policy.overrides`, no call into the policy
//! engine, and not one line changed in `admissionlab-policy`. Reusing
//! the *scale* without the *engine* is what lets a reader compare a
//! migration row against a fixture row on one scale, while keeping the
//! two claim vocabularies apart. The rule and the argument for each arm
//! are on [`grade`] itself.
//!
//! # No stage timing, and no run-manifest stage
//!
//! `admissionlab_core::TimedStage` and
//! `admissionlab_core::RunStage` are the run manifest's own frozen stage
//! vocabulary (`admissionlab.io/run-manifest/v1beta1`). Adding a
//! `MigrationSuite` variant would be a change to a *different* frozen
//! document than the one Task 8.8 owns, made in passing, so it is not
//! made: the migration phase is not separately timed and records no
//! manifest stage. What it costs is one line in `result.json`'s
//! `timings` block; what it buys is that a schema this task has no
//! mandate over is left alone. The phase's wall time is still inside the
//! run's own `elapsed`, and the console reports each case as it
//! finishes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use admissionlab_core::{
    ArtifactStore, ClusterHandle, Diagnostic, ProcessSpawner, RedactedValue, RunPaths, Side,
};
use admissionlab_gateway::{
    GatewayCaseResult, GatewayEndpoint, GatewayEndpointResolver, GatewayEndpointStrategy,
    GatewayError, GatewayIdentity, HttpProbeResult, IngressCaseResult, KubeGatewayEndpointResolver,
    MigrationBehaviorChange, MigrationBehaviorKind, MigrationCaseSpec, MigrationSuiteSpec,
    PlannedObject, RouteContract, apply_gateway_manifests, compare_migration_case,
    describe_probe_request, execute_http_probe, migration_comparability, plan_gateway_apply,
    probe_matches_contract, run_ingress_case, start_service_port_forward,
    unmatched_nonportable_expectations,
};
use admissionlab_policy::{PolicyDisposition, Severity};
use admissionlab_report::{GradedMigrationChange, MigrationCaseComparison};
use admissionlab_spec::{DEFAULT_RECONCILIATION_TIMEOUT, resolve_gateway_endpoint};
use async_trait::async_trait;
use serde::Serialize;

use crate::pipeline::gateway::{observe_route, probe_skip_reason, write_artifact};

/// The subdirectory of `raw/<side>/` a migration case's evidence lands
/// in.
///
/// Beside `raw/<side>/gateway/` rather than inside it, for the reason
/// that directory's own constant gives: a reader listing `raw/baseline/`
/// should be able to tell what kind of evidence a bundle holds without
/// opening one, and a migration case's baseline bundle is an
/// `IngressCaseResult` while a Gateway contract's is a
/// `ReconciliationEvidence`.
pub const MIGRATION_RAW_DIR: &str = "migration";

/// One case's baseline evidence: what the legacy `Ingress` stack did.
pub const INGRESS_ARTIFACT: &str = "ingress.json";

/// One case's candidate evidence: what the Gateway stack did.
pub const GATEWAY_ARTIFACT: &str = "gateway.json";

/// What one side of one case applied, for provenance.
pub const APPLIED_ARTIFACT: &str = "applied.json";

/// The diagnostic code for a migration case whose candidate route never
/// reached a state a request could be sent through.
///
/// The migration twin of `crate::pipeline::gateway`'s
/// `gateway.probe_skipped`, and a separate code rather than a reuse of
/// it: a consumer filtering on `gateway.probe_skipped` is asking about a
/// route contract, and answering with a migration case would be a
/// different subject under the same name.
pub const DIAGNOSTIC_MIGRATION_PROBE_SKIPPED: &str = "migration.probe_skipped";

/// The diagnostic code for a migration case whose candidate route
/// reconciled, and was probed, and answered nothing.
///
/// Distinct from [`DIAGNOSTIC_MIGRATION_PROBE_SKIPPED`] on purpose --
/// see [`not_serving_diagnostic`] for why the two are different facts.
pub const DIAGNOSTIC_MIGRATION_NOT_SERVING: &str = "migration.not_serving";

/// How long the candidate side's route gets to reconcile.
///
/// `admissionlab_spec::DEFAULT_RECONCILIATION_TIMEOUT` — the same two
/// minutes a `gateway:` suite gets by default, reused rather than
/// re-chosen so the two halves of Phase 8 wait the same amount for the
/// same thing. A migration suite has no knob of its own for it, and
/// deliberately: `MigrationSuiteSpec` is §1.2-frozen at `cases` plus
/// Task 8.8's two per-side endpoint blocks, and a third field nobody has
/// needed yet would be surface invented ahead of a use.
pub const MIGRATION_RECONCILIATION_TIMEOUT: Duration = DEFAULT_RECONCILIATION_TIMEOUT;

/// How long each side gets to actually serve a case's probes as the case
/// contracts them.
///
/// Two minutes, matching [`MIGRATION_RECONCILIATION_TIMEOUT`] because
/// the two bound the same kind of thing (a controller and its data plane
/// converging on a configuration) and a run that waited longer for one
/// than the other would report the shorter one's timeout as a behavior
/// difference.
///
/// Read this module's "THE ONE DELIBERATE WAIT" before shortening it:
/// this is the budget a candidate regression spends in full, and it is
/// also the budget that keeps a slow backend `Pod` from being reported
/// as a migration regression.
pub const MIGRATION_SERVING_TIMEOUT: Duration = Duration::from_secs(120);

/// How long to wait between two rounds of probing a candidate that has
/// not reproduced the baseline's contracted behavior yet.
///
/// The same 500 ms, chosen for the same reason, as
/// `admissionlab_gateway::INGRESS_REPROBE_INTERVAL` and
/// `crate::pipeline::gateway::REOBSERVE_INTERVAL`: short enough that the
/// common case (already correct on the first round) is unaffected, long
/// enough that a case that will never match is not probed in a tight
/// loop for the whole budget.
pub const MIGRATION_REPROBE_INTERVAL: Duration = Duration::from_millis(500);

/// The migration suite could not be run.
///
/// The same shape, and for the same reason, as
/// [`crate::pipeline::GatewaySuiteError`]: the caller reports it and
/// maps it to an exit code, and nothing downstream matches on a
/// [`GatewayError`] variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationSuiteError {
    /// The [`MigrationCaseSpec::id`] being run when this failed, or
    /// `None` for a failure before any case was reached (an endpoint
    /// block that does not resolve).
    pub case: Option<String>,
    /// A human-readable, safe-to-print explanation.
    pub message: String,
}

impl std::fmt::Display for MigrationSuiteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.case {
            Some(case) => write!(
                formatter,
                "migration case {case:?} could not be observed: {}",
                self.message
            ),
            None => write!(formatter, "the migration suite failed: {}", self.message),
        }
    }
}

impl std::error::Error for MigrationSuiteError {}

/// What the migration suite produced across both clusters.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationRunOutcome {
    /// One comparison per case, in the order the suite declares them.
    pub cases: Vec<MigrationCaseComparison>,
    /// Run-level findings, already tagged with the case and the side.
    pub diagnostics: Vec<Diagnostic>,
}

/// Running a lab's `migration:` suite across both clusters.
///
/// One call, not one per side — see this module's "One call, two
/// clusters".
#[async_trait]
pub trait MigrationSuiteRunner: Send + Sync {
    /// Runs every case and compares what the two stacks did.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationSuiteError`] if a side's endpoint strategy
    /// does not resolve, if a case's manifests could not be applied or
    /// planned, if a candidate case applies no unambiguous
    /// `Gateway`/`HTTPRoute` pair to observe, if a data plane could not
    /// be located or forwarded to, or if evidence could not be written.
    ///
    /// A case whose baseline was **refused by a webhook**, whose
    /// baseline never served, or whose candidate answered differently is
    /// none of those: each is an ordinary observation, and observing
    /// them is the entire point.
    async fn run(
        &self,
        baseline: &ClusterHandle,
        candidate: &ClusterHandle,
        paths: &RunPaths,
    ) -> Result<MigrationRunOutcome, MigrationSuiteError>;
}

/// The production [`MigrationSuiteRunner`]: real applies on two real
/// clusters, a real `kubectl port-forward` into each data plane, and
/// real HTTP requests through both.
pub struct KubeMigrationSuite {
    /// The resolved suite, exactly as the lab configuration declared it.
    suite: MigrationSuiteSpec,
    /// Where evidence bundles are written.
    store: ArtifactStore,
    /// Spawns the long-lived `kubectl port-forward` children.
    spawner: Arc<dyn ProcessSpawner>,
    /// Turns each side's endpoint strategy into a concrete
    /// namespace/Service/port against that side's live cluster.
    endpoints: KubeGatewayEndpointResolver,
}

impl KubeMigrationSuite {
    /// Builds the production runner for `suite`.
    #[must_use]
    pub fn new(
        suite: MigrationSuiteSpec,
        store: ArtifactStore,
        spawner: Arc<dyn ProcessSpawner>,
    ) -> Self {
        Self {
            suite,
            store,
            spawner,
            endpoints: KubeGatewayEndpointResolver::new(),
        }
    }

    /// Both sides' endpoint strategies, resolved once before any cluster
    /// is touched.
    ///
    /// A missing block is a failure here rather than a skipped probe: a
    /// migration case's probes are the *only* thing its two sides can be
    /// compared on ([`MigrationCaseSpec::probes`]), so a suite with
    /// nowhere to send them would install two stacks and then claim
    /// nothing. `crate::pipeline::validate_migration_suite` refuses the
    /// same configuration before any cluster exists; this is the
    /// belt-and-braces half, and it is what keeps this type usable on
    /// its own.
    fn strategies(
        &self,
    ) -> Result<(GatewayEndpointStrategy, GatewayEndpointStrategy), MigrationSuiteError> {
        Ok((
            side_strategy(self.suite.baseline.as_ref(), "baseline")?,
            side_strategy(self.suite.candidate.as_ref(), "candidate")?,
        ))
    }

    /// Runs one case's baseline half: the legacy `Ingress` stack.
    ///
    /// A thin wrapper over `admissionlab_gateway::run_ingress_case`,
    /// which owns every decision about how a legacy stack is waited on.
    async fn run_baseline(
        &self,
        cluster: &ClusterHandle,
        paths: &RunPaths,
        case: &MigrationCaseSpec,
        strategy: &GatewayEndpointStrategy,
    ) -> Result<IngressCaseResult, MigrationSuiteError> {
        let fail = |message: String| MigrationSuiteError {
            case: Some(case.id.clone()),
            message,
        };
        let result = run_ingress_case(
            cluster,
            self.spawner.as_ref(),
            case,
            strategy,
            Instant::now() + MIGRATION_SERVING_TIMEOUT,
        )
        .await
        .map_err(|error| fail(format!("the legacy Ingress side failed: {error}")))?;

        let directory = case_dir(paths, Side::Baseline, &case.id);
        write_artifact(&self.store, &directory, INGRESS_ARTIFACT, &result)
            .await
            .map_err(fail)?;
        Ok(result)
    }

    /// Runs one case's candidate half: the Gateway API stack.
    ///
    /// The stage order, and each step's own reason, is this module's
    /// "The two readiness vocabularies" and "THE ONE DELIBERATE WAIT".
    async fn run_candidate(
        &self,
        cluster: &ClusterHandle,
        paths: &RunPaths,
        case: &MigrationCaseSpec,
        strategy: &GatewayEndpointStrategy,
    ) -> Result<(GatewayCaseResult, Vec<Diagnostic>), MigrationSuiteError> {
        let fail = |message: String| MigrationSuiteError {
            case: Some(case.id.clone()),
            message,
        };

        // Planned first, and offline: the plan is what says which
        // `Gateway` and which `HTTPRoute` this case is about, and a
        // case that names an ambiguous pair must be refused before
        // anything is persisted.
        let plan = plan_gateway_apply(&case.candidate_gateway_manifests)
            .map_err(|error| fail(format!("its Gateway manifests could not be read: {error}")))?;
        let contract = route_contract(case, &plan.documents).map_err(&fail)?;

        let applied = apply_gateway_manifests(cluster, &case.candidate_gateway_manifests)
            .await
            .map_err(|error| {
                fail(format!(
                    "its Gateway manifests could not be applied: {error}"
                ))
            })?;
        let directory = case_dir(paths, Side::Candidate, &case.id);
        write_artifact(
            &self.store,
            &directory,
            APPLIED_ARTIFACT,
            &AppliedArtifact {
                case_id: case.id.clone(),
                objects: applied.objects.iter().map(ToString::to_string).collect(),
                source_hashes: applied
                    .source_hashes
                    .iter()
                    .map(|(path, digest)| (path.display().to_string(), digest.clone()))
                    .collect(),
            },
        )
        .await
        .map_err(&fail)?;

        let evidence = observe_route(
            cluster,
            &contract,
            Instant::now() + MIGRATION_RECONCILIATION_TIMEOUT,
        )
        .await
        .map_err(|message| {
            fail(format!(
                "its HTTPRoute's status could not be read: {message}"
            ))
        })?;

        // A route that is not carrying traffic gets no request, for the
        // reason `crate::pipeline::gateway::probe_skip_reason` states in
        // full: a request would be answered by the data plane's own
        // error handling, and comparing that against the baseline's real
        // answer would report an invented status as a migration
        // regression. `migration_comparability` then reports the case as
        // `candidate_not_serving`, which is the honest claim.
        let (probes, diagnostics) = if let Some(reason) = probe_skip_reason(&contract, &evidence) {
            (
                Vec::new(),
                vec![skip_diagnostic(&case.id, &contract, &reason)],
            )
        } else {
            let identity = GatewayIdentity {
                namespace: contract.gateway_namespace.clone(),
                name: contract.gateway_name.clone(),
            };
            let endpoint = self
                .endpoints
                .resolve(cluster, &identity, strategy)
                .await
                .map_err(|error| {
                    fail(format!(
                        "the data-plane endpoint for Gateway {identity} could not be resolved: \
                         {error}"
                    ))
                })?;
            match self
                .probe_candidate(cluster, case, &endpoint)
                .await
                .map_err(&fail)?
            {
                Settled::Round(probes) => (probes, Vec::new()),
                // The route reconciled, a forward opened, and the data
                // plane still answered nothing within the budget. That
                // is not a migration finding -- `migration_comparability`
                // reports it as `candidate_not_serving` and the case
                // becomes inconclusive -- so what it needs is a reason,
                // in the one place every renderer already shows one.
                Settled::Nothing(reason) => (
                    Vec::new(),
                    vec![not_serving_diagnostic(
                        &case.id,
                        &endpoint.to_string(),
                        &reason,
                    )],
                ),
            }
        };

        let result = GatewayCaseResult {
            contract_id: case.id.clone(),
            reconciliation: evidence,
            probes,
        };
        write_artifact(&self.store, &directory, GATEWAY_ARTIFACT, &result)
            .await
            .map_err(&fail)?;
        Ok((result, diagnostics))
    }

    /// Probes the candidate's data plane in rounds until the case's
    /// contracts are met or the budget is gone, and reports the last
    /// complete round.
    ///
    /// Read this module's "THE ONE DELIBERATE WAIT" for what this loop
    /// can and cannot do. What is decided *here* is one thing, and it is
    /// the one this function exists for:
    ///
    /// # THE FINDING: a fresh port-forward per round, not one for the
    /// # whole wait
    ///
    /// `crate::pipeline::gateway::KubeGatewaySuite::probe_all` opens one
    /// forward for all of a contract's probes, and
    /// `admissionlab_gateway::ingress::probe_case` opens one for all of
    /// its rounds. Both are right for what they forward to: a Gateway
    /// suite probes immediately after a readiness gate the user
    /// declared, and an `Ingress` controller's data plane is one stable
    /// `Deployment` the component install already waited on.
    ///
    /// A migration's candidate has neither. Its data plane is
    /// *provisioned by the implementation* in response to the `Gateway`
    /// this very case just applied, so it is coming up while this loop
    /// runs, and it can be replaced while it settles. Two failures were
    /// measured on real Kubernetes 1.36.4 `kind` clusters running NGINX
    /// Gateway Fabric 2.6.7, and both are what this shape answers:
    ///
    /// 1. **The forward cannot be opened yet.** NGF publishes
    ///    `Programmed=True` when it has *started* the data plane it
    ///    provisioned, not when the resulting `Pod` is running, so
    ///    `kubectl` exits `1` with `unable to forward port because pod
    ///    is not running. Current status=Pending` — **before announcing
    ///    a local port**, so a round loop inside the forward never gets
    ///    to run at all.
    /// 2. **The forward is opened and then dies.** A `kubectl
    ///    port-forward` is attached to one `Pod`; when that `Pod` is
    ///    replaced, `kubectl` exits and the local socket stops
    ///    listening. Every subsequent probe then gets `Connection
    ///    refused` from `127.0.0.1`, for the whole budget, and the run
    ///    reports "the Gateway stack answered none of this case's
    ///    probes" about a data plane that is serving perfectly. Measured
    ///    exactly that way: 22 rounds, 110 seconds, every probe refused,
    ///    against a cluster that answered `curl` on the first try
    ///    afterwards.
    ///
    /// Re-opening per round costs one `kubectl` spawn per round, which
    /// is real and is the price of the loop being able to recover from
    /// (2) at all. It buys the property that matters: **no round's
    /// result depends on a process started for an earlier round.**
    ///
    /// # Errors
    ///
    /// A probe that cannot be turned into a request, or whose response
    /// is too large to hash, is returned immediately: neither becomes
    /// true by waiting. So is a forward that could not be *closed*,
    /// because a leaked `kubectl` per round is exactly what this shape
    /// must not produce. A forward that could not be *opened*, and a
    /// probe that simply got no answer, are ordinary "not serving yet"
    /// observations and are retried.
    async fn probe_candidate(
        &self,
        cluster: &ClusterHandle,
        case: &MigrationCaseSpec,
        endpoint: &GatewayEndpoint,
    ) -> Result<Settled, String> {
        let deadline = Instant::now() + MIGRATION_SERVING_TIMEOUT;
        let mut last_complete: Option<Vec<HttpProbeResult>> = None;
        let mut unanswered: Option<String> = None;
        let mut rounds: u32 = 0;

        loop {
            rounds = rounds.saturating_add(1);
            match self.probe_round(cluster, case, endpoint).await? {
                Round::Complete(round) => {
                    if round
                        .iter()
                        .zip(case.probes.iter())
                        .all(|(result, contract)| probe_matches_contract(result, contract))
                    {
                        return Ok(Settled::Round(round));
                    }
                    last_complete = Some(round);
                }
                Round::Unanswered(reason) => unanswered = Some(reason),
            }

            if Instant::now() + MIGRATION_REPROBE_INTERVAL >= deadline {
                return Ok(match last_complete {
                    Some(round) => Settled::Round(round),
                    None => Settled::Nothing(format!(
                        "after {rounds} round(s) of probing within \
                         {MIGRATION_SERVING_TIMEOUT:?}, no round was answered in full; the last \
                         one said: {}",
                        unanswered.unwrap_or_else(|| "(nothing was attempted)".to_owned())
                    )),
                });
            }
            tokio::time::sleep(MIGRATION_REPROBE_INTERVAL).await;
        }
    }

    /// One round: open a forward, send every probe once, close the
    /// forward.
    ///
    /// The close is deliberately not behind a `?`, for the reason
    /// `crate::pipeline::gateway::KubeGatewaySuite::probe_all`
    /// documents: `PortForwardHandle::close` consumes the handle, so the
    /// only way to hold one across a fallible call and still close it is
    /// to keep both results and combine them afterwards.
    ///
    /// A round is kept only when it is **complete** -- every probe
    /// answered. That is the same "complete or empty" rule
    /// `admissionlab_gateway::ingress` documents for its own results, and
    /// for the same reason: `compare_migration_case` pairs `probes[i]`
    /// with the baseline's `probes[i]`, so a short round would compare
    /// probe 1 against probe 2 and report a routing difference no
    /// cluster produced.
    async fn probe_round(
        &self,
        cluster: &ClusterHandle,
        case: &MigrationCaseSpec,
        endpoint: &GatewayEndpoint,
    ) -> Result<Round, String> {
        let forward =
            match start_service_port_forward(self.spawner.as_ref(), cluster, endpoint).await {
                Ok(forward) => forward,
                Err(error) => {
                    return Ok(Round::Unanswered(format!(
                        "no port-forward to {endpoint} could be started: {error}"
                    )));
                }
            };

        let mut round = Vec::with_capacity(case.probes.len());
        let mut unanswered: Option<Result<String, GatewayError>> = None;
        for (index, contract) in case.probes.iter().enumerate() {
            match execute_http_probe(forward.local_addr, contract).await {
                Ok(result) => round.push(result),
                // No answer at all is "not serving yet" at this
                // altitude, exactly as `admissionlab_gateway::ingress`'s
                // own loop treats it -- but *why* is kept, because a
                // candidate that never answered has to be able to say so
                // (Global Constraint 15).
                Err(error @ GatewayError::ProbeUnavailable { .. }) => {
                    unanswered = Some(Ok(format!(
                        "probe {index} ({}) got no answer: {error}",
                        describe_probe_request(contract)
                    )));
                    break;
                }
                Err(other) => {
                    unanswered = Some(Err(other));
                    break;
                }
            }
        }

        let closed = forward.close().await;
        if let Err(close) = closed {
            return Err(format!(
                "the port-forward to {endpoint} could not be closed: {close}"
            ));
        }
        match unanswered {
            None => Ok(Round::Complete(round)),
            Some(Ok(reason)) => Ok(Round::Unanswered(reason)),
            Some(Err(error)) => Err(format!(
                "a traffic probe through {endpoint} failed: {error}"
            )),
        }
    }
}

/// What one round of [`KubeMigrationSuite::probe_round`] produced.
enum Round {
    /// Every probe answered, in contract order.
    Complete(Vec<HttpProbeResult>),
    /// At least one probe was not answered -- or no forward could be
    /// opened at all -- with the reason.
    Unanswered(String),
}

#[async_trait]
impl MigrationSuiteRunner for KubeMigrationSuite {
    async fn run(
        &self,
        baseline: &ClusterHandle,
        candidate: &ClusterHandle,
        paths: &RunPaths,
    ) -> Result<MigrationRunOutcome, MigrationSuiteError> {
        let (baseline_strategy, candidate_strategy) = self.strategies()?;
        let mut outcome = MigrationRunOutcome::default();

        for case in &self.suite.cases {
            // `join!`, never `try_join!` — the same discipline, and the
            // same argument, as `crate::pipeline::run_gateway_suite`:
            // abandoning the other side's in-flight future the instant
            // one fails would drop it while a `kubectl port-forward`
            // child is still running.
            let (baseline_result, candidate_result) = tokio::join!(
                self.run_baseline(baseline, paths, case, &baseline_strategy),
                self.run_candidate(candidate, paths, case, &candidate_strategy),
            );
            let (ingress, (gateway, mut diagnostics)) = match (baseline_result, candidate_result) {
                (Ok(ingress), Ok(gateway)) => (ingress, gateway),
                (baseline_result, candidate_result) => {
                    // Both sides' failures, not just the first: a case
                    // that broke on both usually broke for one reason.
                    let message = [
                        baseline_result
                            .err()
                            .map(|error| format!("baseline: {}", error.message)),
                        candidate_result
                            .err()
                            .map(|error| format!("candidate: {}", error.message)),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("; ");
                    return Err(MigrationSuiteError {
                        case: Some(case.id.clone()),
                        message,
                    });
                }
            };

            // The baseline's own diagnostics -- a webhook refusal, or a
            // stack that never served the case -- are run-level findings
            // once they are tagged with the case and the side they came
            // from, exactly as `install_diagnostics` tags a component's.
            diagnostics.extend(
                ingress
                    .diagnostics
                    .iter()
                    .map(|diagnostic| tagged(diagnostic, &case.id, Side::Baseline)),
            );

            // Planned rather than re-read from the cluster: this is the
            // *only* thing `compare_migration_case` reads from a
            // manifest, and reading it from what was applied keeps the
            // comparator's output a function of its inputs (Global
            // Constraint 7).
            let baseline_plan =
                plan_gateway_apply(&case.baseline_ingress_manifests).map_err(|error| {
                    MigrationSuiteError {
                        case: Some(case.id.clone()),
                        message: format!("its Ingress manifests could not be re-read: {error}"),
                    }
                })?;
            let comparison =
                compare_migration_case(case, &ingress, &gateway, &baseline_plan.documents);

            outcome.cases.push(MigrationCaseComparison {
                case_id: case.id.clone(),
                comparability: migration_comparability(&ingress, &gateway),
                changes: comparison
                    .changes
                    .into_iter()
                    .map(|change| GradedMigrationChange {
                        severity: grade(&change),
                        change,
                    })
                    .collect(),
                probes: comparison.probes,
                unmatched_expectations: unmatched_nonportable_expectations(
                    case,
                    &baseline_plan.documents,
                )
                .into_iter()
                .cloned()
                .collect(),
            });
            outcome.diagnostics.extend(diagnostics);
        }

        Ok(outcome)
    }
}

/// How much one observed migration difference matters (ROADMAP Task 8.8).
///
/// The whole grading rule, and the argument for each of its three arms:
///
/// - **A declared non-portable feature is [`Severity::Info`].** The
///   author wrote down what the feature did and what the migration does
///   instead ([`admissionlab_gateway::NonPortableFeatureExpectation`]
///   requires both), so it is a real difference that has been accounted
///   for. `Info` rather than silence, because the finding still belongs
///   in the report: `admissionlab_report::FixtureBucket::Expected`
///   already makes exactly this distinction for an `info`-graded
///   admission change.
/// - **An undeclared non-portable feature is [`Severity::Warning`].**
///   ROADMAP Task 8.5 step 3, in as many words: "an unexpected
///   nonportable feature is warning by default". Not `Critical`: the
///   catalog is a statement about the *baseline manifests*, not about
///   observed traffic, and a feature this run never exercised may or may
///   not matter to this migration. The person who knows is the author,
///   and a warning is what asks them.
/// - **Every traffic-derived difference is [`Severity::Critical`].**
///   These are the regressions the suite exists for: the stack serving
///   production today and the stack meant to replace it answered the
///   *same request* differently, and the change's `detail` names the two
///   observed answers. `MigrationBehaviorChange::expected` is always
///   `false` for these by construction (that field's own documentation
///   explains why `expectedNonportable` cannot honestly absolve a status
///   code), so there is no "accounted for" arm to write — a team that
///   wants to accept a traffic difference does it by changing the case's
///   probes, which is a reviewable edit that says what they accepted.
///
/// Deterministic and total: a pure function of the change, with no
/// clock, no configuration and no policy engine (Global Constraint 7).
#[must_use]
pub fn grade(change: &MigrationBehaviorChange) -> Severity {
    match change.kind {
        MigrationBehaviorKind::NonPortableFeature => {
            if change.expected {
                Severity::Info
            } else {
                Severity::Warning
            }
        }
        MigrationBehaviorKind::HostBehaviorChanged
        | MigrationBehaviorKind::PathBehaviorChanged
        | MigrationBehaviorKind::TlsBehaviorChanged
        | MigrationBehaviorKind::BackendChanged
        | MigrationBehaviorKind::RewriteBehaviorChanged
        | MigrationBehaviorKind::RedirectBehaviorChanged => Severity::Critical,
    }
}

/// One case's contribution to the run's verdict.
///
/// The worst grade wins, exactly as
/// `admissionlab_report::FixtureComparison::bucket` lets the worst
/// unexpected finding decide a fixture's bucket, and for the same
/// reason: a row has to answer "how bad is the worst thing here".
///
/// A case whose two sides were **not comparable** is at least
/// [`PolicyDisposition::Warn`], whatever its changes say. That is the
/// migration analog of `FixtureBucket::Inconclusive` outranking
/// everything: a baseline the API server refused, or a legacy stack that
/// never served the case, means the run established nothing about this
/// migration — and a run that quietly passes having compared nothing is
/// the exact failure Global Constraint 15 forbids. It is `Warn` rather
/// than `Fail` because "we could not tell" is not "the candidate
/// regressed"; the reason is in
/// `MigrationComparability::reason` and in the case's own diagnostics.
///
/// [`MigrationCaseComparison::unmatched_expectations`] does **not** move
/// this: an expectation that matched nothing is a statement about the
/// configuration rather than something the run observed, which is
/// precisely how `admissionlab-policy` already treats a stale
/// `expectations.yaml` entry.
#[must_use]
pub fn case_disposition(case: &MigrationCaseComparison) -> PolicyDisposition {
    let observed = case
        .changes
        .iter()
        .map(|graded| match graded.severity {
            Severity::Critical => PolicyDisposition::Fail,
            Severity::Warning => PolicyDisposition::Warn,
            Severity::Info => PolicyDisposition::Pass,
        })
        .max()
        .unwrap_or(PolicyDisposition::Pass);
    if case.comparability.is_comparable() {
        observed
    } else {
        observed.max(PolicyDisposition::Warn)
    }
}

/// The whole migration suite's contribution to the run's verdict: the
/// worst case's.
///
/// [`PolicyDisposition::Pass`] for an empty slice, which is the honest
/// answer for a lab with no migration suite: nothing was compared, so
/// nothing about a migration can make this run worse.
#[must_use]
pub fn migration_disposition(cases: &[MigrationCaseComparison]) -> PolicyDisposition {
    cases
        .iter()
        .map(case_disposition)
        .max()
        .unwrap_or(PolicyDisposition::Pass)
}

/// The `RouteContract` a case's own candidate manifests describe.
///
/// # Why this is derived rather than configured
///
/// `admissionlab_gateway::wait_for_route_reconciliation` needs to know
/// *which* `Gateway` and *which* `HTTPRoute` to read a status from, and
/// a [`MigrationCaseSpec`] names neither: §1.2 freezes it at an id, two
/// manifest lists, probes and expectations. Adding a route identity to
/// the configuration would ask a user to write down, a second time, two
/// names their own manifests already carry — and would let the two
/// disagree.
///
/// So it is read from the plan, which is the exact symmetric twin of
/// what the baseline side already does:
/// `admissionlab_gateway::applied_ingress_identity` finds the `Ingress`
/// a case applied, and this finds the `Gateway` and `HTTPRoute` it
/// applied.
///
/// The one place it differs is ambiguity. The baseline picks the
/// *first* `Ingress` because an `Ingress` controller is one shared data
/// plane and the identity is only a label. Here the identity decides
/// which status is read and which data-plane `Service` is probed, so
/// **more than one of either kind is refused** rather than resolved by
/// position: a case with two `HTTPRoute`s has no single answer to "did
/// the route reconcile", and guessing would produce evidence about an
/// object the user did not mean.
///
/// `listener_name` comes from the route's own first `parentRefs` entry's
/// `sectionName` when it has one, which is how a contract stops a route
/// with two parents from making its parent lookup ambiguous. `None`
/// where the route names none, which is Gateway API's own default and is
/// carried rather than substituted.
///
/// # Errors
///
/// Returns a message naming what was found when the case applies no
/// `Gateway`, no `HTTPRoute`, or more than one of either.
fn route_contract(
    case: &MigrationCaseSpec,
    documents: &[PlannedObject],
) -> Result<RouteContract, String> {
    let gateway = single(documents, "Gateway", &case.id)?;
    let route = single(documents, "HTTPRoute", &case.id)?;

    Ok(RouteContract {
        id: case.id.clone(),
        gateway_namespace: gateway.namespace.clone().unwrap_or_default(),
        gateway_name: gateway.name.clone(),
        route_namespace: route.namespace.clone().unwrap_or_default(),
        route_name: route.name.clone(),
        listener_name: route
            .object
            .get("spec")
            .and_then(|spec| spec.get("parentRefs"))
            .and_then(|parents| parents.get(0))
            .and_then(|parent| parent.get("sectionName"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        // The case's own probes, so the candidate is asked exactly the
        // questions the baseline was asked. This is what makes
        // `compare_migration_case`'s index pairing meaningful.
        probes: case.probes.clone(),
    })
}

/// The one planned document of `kind`, or a message saying what was
/// there instead.
fn single<'a>(
    documents: &'a [PlannedObject],
    kind: &str,
    case_id: &str,
) -> Result<&'a PlannedObject, String> {
    let matching: Vec<&PlannedObject> = documents
        .iter()
        .filter(|document| document.kind == kind)
        .collect();
    match matching.as_slice() {
        [only] => Ok(only),
        [] => Err(format!(
            "its candidateGatewayManifests apply no {kind}, so there is no object to observe the \
             migration of case {case_id:?} on"
        )),
        several => Err(format!(
            "its candidateGatewayManifests apply {} {kind} objects ({}); a migration case must \
             apply exactly one, because which one a probe and a status reading are about would \
             otherwise be a guess",
            several.len(),
            several
                .iter()
                .map(|document| document.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        )),
    }
}

/// One side's endpoint strategy, resolved from the configuration block.
fn side_strategy(
    side: Option<&admissionlab_spec::MigrationSideSpec>,
    name: &str,
) -> Result<GatewayEndpointStrategy, MigrationSuiteError> {
    let side = side.ok_or_else(|| MigrationSuiteError {
        case: None,
        message: format!(
            "this lab's migration suite declares no migration.{name}.gatewayEndpoint, so there is \
             no data-plane Service to send the {name} side's probes through -- and a migration \
             case's probes are the only thing its two sides can be compared on"
        ),
    })?;
    resolve_gateway_endpoint(&side.gateway_endpoint).map_err(|(locator, message)| {
        MigrationSuiteError {
            case: None,
            message: format!("migration.{name}.gatewayEndpoint.{locator} is invalid: {message}"),
        }
    })
}

/// What [`KubeMigrationSuite::probe_candidate`] concluded.
///
/// Two variants rather than a possibly-empty `Vec`, because the failing
/// side carries the diagnosis and a `None` would throw away the one
/// thing a user needs -- the same shape, for the same reason,
/// `admissionlab_gateway::ingress`'s own `Served` enum takes.
enum Settled {
    /// One complete round, every probe answered. It may or may not
    /// match the case's contracts; a round that did not match, after
    /// the whole budget, **is** the finding.
    Round(Vec<HttpProbeResult>),
    /// No round was ever answered in full, with the reason the last
    /// attempt gave.
    Nothing(String),
}

/// The run-level diagnostic for a candidate whose data plane answered
/// none of the case's probes.
///
/// The migration twin of `admissionlab_gateway::ingress`'s
/// [`DIAGNOSTIC_INGRESS_NOT_SERVING`], and separate from
/// [`skip_diagnostic`] because the two are different facts: a *skip* is
/// "the route never reached a state a request could be sent through, and
/// here is the condition", while this is "the route did reach that state
/// and nothing answered anyway". Reporting either as the other would
/// point a reader at the wrong half of the stack.
///
/// [`DIAGNOSTIC_INGRESS_NOT_SERVING`]: admissionlab_gateway::DIAGNOSTIC_INGRESS_NOT_SERVING
fn not_serving_diagnostic(case_id: &str, endpoint: &str, reason: &str) -> Diagnostic {
    let mut context = BTreeMap::new();
    context.insert("case".to_owned(), RedactedValue::Public(case_id.to_owned()));
    context.insert(
        "side".to_owned(),
        RedactedValue::Public(Side::Candidate.to_string()),
    );
    context.insert(
        "endpoint".to_owned(),
        RedactedValue::Public(endpoint.to_owned()),
    );
    Diagnostic {
        code: DIAGNOSTIC_MIGRATION_NOT_SERVING.to_owned(),
        message: format!(
            "the Gateway stack never answered migration case {case_id:?}'s probes, so no \
             candidate traffic evidence was recorded for it; {reason}"
        ),
        context,
    }
}

/// The run-level diagnostic for a candidate route that was never probed.
fn skip_diagnostic(case_id: &str, contract: &RouteContract, reason: &str) -> Diagnostic {
    let mut context = BTreeMap::new();
    context.insert("case".to_owned(), RedactedValue::Public(case_id.to_owned()));
    context.insert(
        "side".to_owned(),
        RedactedValue::Public(Side::Candidate.to_string()),
    );
    context.insert(
        "probes".to_owned(),
        RedactedValue::Public(contract.probes.len().to_string()),
    );
    context.insert(
        "request".to_owned(),
        RedactedValue::Public(
            contract
                .probes
                .first()
                .map_or_else(|| "(none)".to_owned(), describe_probe_request),
        ),
    );
    Diagnostic {
        code: DIAGNOSTIC_MIGRATION_PROBE_SKIPPED.to_owned(),
        message: format!(
            "no traffic probe was sent for migration case {case_id:?} on the candidate side: \
             {reason}"
        ),
        context,
    }
}

/// One diagnostic, tagged with the case and side it came from.
fn tagged(diagnostic: &Diagnostic, case_id: &str, side: Side) -> Diagnostic {
    let mut tagged = diagnostic.clone();
    tagged
        .context
        .insert("case".to_owned(), RedactedValue::Public(case_id.to_owned()));
    tagged
        .context
        .insert("side".to_owned(), RedactedValue::Public(side.to_string()));
    tagged
}

/// `raw/<side>/migration/<case-id>/`.
///
/// The case id is safe as a path segment for the same reason a Gateway
/// route contract id is: `crate::pipeline::validate_migration_suite`
/// requires every case id to parse as an
/// `admissionlab_core::FixtureId` before the run starts, and that
/// grammar (lowercase ASCII, digits, `-`) contains no separator and no
/// `..`.
fn case_dir(paths: &RunPaths, side: Side, case_id: &str) -> PathBuf {
    paths
        .raw()
        .join(side.as_str())
        .join(MIGRATION_RAW_DIR)
        .join(case_id)
}

/// `applied.json`: what one case's candidate side put into the cluster.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppliedArtifact {
    /// The case these objects belong to.
    case_id: String,
    /// Every object applied, in apply order.
    objects: Vec<String>,
    /// Each manifest file's SHA-256, keyed by path.
    source_hashes: BTreeMap<String, String>,
}
