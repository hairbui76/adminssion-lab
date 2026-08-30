//! [`ReadinessProbe`]: deciding when an installed component is actually
//! *ready* (Task 2.4) — the piece that makes comparing baseline against
//! candidate a meaningful comparison rather than a race against
//! components still starting up.
//!
//! # Predicate/fetch separation (Brief Step 1)
//!
//! [`evaluate`] is a pure, synchronous function from a
//! [`ReadinessCheck`] and an already-observed `serde_json::Value` (or
//! `None`, if nothing has been observed yet) to `bool`. It never talks
//! to a cluster, so it is unit-tested directly against captured objects
//! in `tests/readiness_unit.rs` with no live cluster at all. Everything
//! that *does* talk to a cluster — [`client_for`], [`resolve_target`],
//! [`ResolvedTarget`]'s [`ReadinessFetch`] implementation — is kept
//! separate, reached only through [`KubeReadinessProbe::wait`], which
//! is consequently the one part of this module that cannot be
//! exercised without a real kube-apiserver (left for the Phase 2 exit
//! gate). [`poll_readiness`] (Step 3) sits in between: it is generic
//! over any [`ReadinessFetch`], so its backoff/deadline orchestration is
//! *also* unit-tested, against a fake implementation.
//!
//! # Why poll, not watch (Brief Step 3)
//!
//! `kube_runtime::wait::await_condition` (a watch-based primitive) was
//! considered and rejected for this module, for reasons confirmed by
//! reading `kube-runtime`'s own source (`research-kube-api.md` §7)
//! rather than assumed:
//!
//! 1. **The brief itself says "poll".** Step 3 is titled "Poll with
//!    capped exponential backoff and an absolute deadline" — not
//!    "watch".
//! 2. **`await_condition` has no reconnect backoff of its own.** Its
//!    underlying `watcher()` stream reconnects immediately after an
//!    error unless separately wrapped in `.default_backoff()`, which
//!    `await_condition`/`watch_object` do not do. Getting capped
//!    backoff on the watch-reconnect path itself would mean
//!    re-implementing `watch_object`'s internals — extra surface for no
//!    benefit over polling directly.
//! 3. **One poll loop serves all five [`ReadinessCheck`] variants.**
//!    `await_condition` needs a distinct `Condition<K>` per typed `K`
//!    (`Deployment`, `DaemonSet`, `Job`, …), and has no story at all for
//!    [`ReadinessCheck::WebhookConfigurationPresent`] (existence, not a
//!    condition, and across *two* possible kinds — see
//!    [`resolve_target`]) or [`ReadinessCheck::CustomResourceCondition`]
//!    (an arbitrary, only-known-at-runtime kind). [`poll_readiness`]
//!    handles all five through one mechanism: a [`ReadinessFetch`] that
//!    returns `Option<serde_json::Value>`, plus [`evaluate`].
//!
//! # `DaemonSetReady`: no `.status.conditions`, and what zero desired means
//!
//! `k8s-openapi`'s generated `DaemonSetStatus` has no `conditions` field
//! at all (verified by reading
//! `k8s-openapi-0.28.0/src/v1_36/api/apps/v1/daemon_set_status.rs`
//! directly) — only scalar counters. [`daemonset_ready`] follows the
//! same algorithm `kubectl rollout status daemonset` itself uses
//! (`k8s.io/kubectl`'s `polymorphichelpers/rollout_status.go`): first
//! require `status.observedGeneration >= metadata.generation` — the
//! controller has reconciled at least the current spec — and only then
//! trust `updatedNumberScheduled`/`numberAvailable` against
//! `desiredNumberScheduled`. This is what a naive `desired ==
//! numberReady` comparison misses: a freshly created `DaemonSet` the
//! controller has not reconciled even once also reports every counter
//! as `0` (Kubernetes' own zero-value defaults), which is
//! indistinguishable from "legitimately targets zero nodes" by the
//! counters alone. Requiring the generation check first means a
//! not-yet-reconciled `DaemonSet` is correctly `false` regardless of
//! what its not-yet-meaningful counters say, while a `DaemonSet` that
//! genuinely has no eligible nodes (a real reconcile observed, desired
//! legitimately `0`) is correctly `true` immediately — see
//! `tests/readiness_unit.rs`'s
//! `daemonset_ready_true_when_desired_is_zero_and_generation_observed`
//! and
//! `daemonset_ready_false_when_desired_is_zero_but_generation_not_yet_observed`.
//!
//! # `WebhookConfigurationPresent` may not exist until well after install
//!
//! Kyverno creates its `ValidatingWebhookConfiguration`/
//! `MutatingWebhookConfiguration` objects at runtime, not via its Helm
//! chart (confirmed by rendering the chart: zero webhook-configuration
//! objects appear in it) — the admission controller creates them itself
//! after it starts. [`resolve_target`] therefore looks up *both*
//! kinds by name (the check itself does not say which to expect), and
//! [`poll_readiness`] treats "not found yet" as an ordinary
//! not-satisfied poll result, retried like any other, never as an
//! error — a readiness model that only waited on chart-installed
//! objects would declare Kyverno ready before it can intercept
//! anything.
//!
//! # Errors during polling are retried, not surfaced
//!
//! [`poll_readiness`] never fails: a fetch that returns an error (a
//! transient network blip, the apiserver briefly unreachable, RBAC not
//! yet propagated) is treated exactly like "not observed this attempt"
//! and retried until `deadline`, the same as an object that simply does
//! not exist yet. The only failures [`KubeReadinessProbe::wait`]
//! surfaces as [`InstallError::ReadinessUnavailable`] are the two
//! *permanent* setup failures that make the whole attempt pointless —
//! an unbuildable Kubernetes client, or (only for
//! [`ReadinessCheck::CustomResourceCondition`]) an unparseable
//! `api_version` — both discovered once, before polling begins, so a
//! probe never spends its whole deadline retrying something that could
//! never succeed.
//!
//! # Redaction of `last_observed`, today
//!
//! `admissionlab-core` has [`admissionlab_core::RedactedValue`], and
//! Task 4.10 owns this project's eventual central redaction pass across
//! every report surface — but [`ReadinessEvidence::last_observed`]'s
//! type is fixed by the cross-task struct registry as a plain
//! `serde_json::Value` (not `RedactedValue`), and is populated here,
//! long before that central pass exists. [`redact_for_evidence`] is a
//! deliberately narrow stand-in, not that general pass: it recognizes
//! only `kind == "Secret"` and masks `.data`/`.stringData` map *values*
//! (never their keys — field names like `"tls.crt"` are useful
//! diagnostics on their own) with the literal `"[REDACTED]"`, the same
//! text [`admissionlab_core::RedactedValue::Sensitive`] renders as, so
//! a report reads consistently regardless of which layer produced it.
//! This matters even though none of the five checks *target* a Secret:
//! nothing in [`ReadinessCheck::CustomResourceCondition`]'s own type
//! stops a caller from naming `api_version: "v1", kind: "Secret"` — it
//! is the one check that reads a resource of genuinely arbitrary kind
//! (via `kube`'s dynamic API; see [`resolve_target`]) — so it is the
//! one path that can actually observe a real Secret. This pass does
//! *not* attempt to find secret material nested inside some other
//! kind's fields (for example a `kubectl.kubernetes.io/last-applied-configuration`
//! annotation that happens to embed a prior Secret) — that is Task
//! 4.10's job, not this one's.

use std::time::{Duration, Instant};

use admissionlab_core::ClusterHandle;
use admissionlab_spec::ReadinessCheck;
use async_trait::async_trait;
use k8s_openapi::api::admissionregistration::v1::{
    MutatingWebhookConfiguration, ValidatingWebhookConfiguration,
};
use k8s_openapi::api::apps::v1::{DaemonSet, Deployment};
use k8s_openapi::api::batch::v1::Job;
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::core::{ApiResource, DynamicObject, GroupVersion};
use kube::{Api, Client, Config};
use serde::Serialize;

use crate::InstallError;

/// The contract that decides when an installed component is actually
/// ready. See this module's documentation for the design behind
/// [`KubeReadinessProbe`], the one production implementation.
///
/// `Send + Sync` for the same reason every other async trait in this
/// workspace is (see `admissionlab_core::ClusterManager`'s
/// documentation): a later task waits on baseline and candidate
/// readiness concurrently, the same way clusters are created and
/// components installed concurrently today.
#[async_trait]
pub trait ReadinessProbe: Send + Sync {
    /// Waits for `check` to become satisfied on `cluster`, polling
    /// until either it is or `deadline` (an absolute point in time,
    /// never a bare duration) passes.
    ///
    /// # Errors
    ///
    /// Returns [`InstallError::ReadinessUnavailable`] if `check` could
    /// not even be attempted. Waiting out the full `deadline` without
    /// `check` becoming satisfied is *not* an error: it is reported as
    /// `Ok(ReadinessEvidence { satisfied: false, .. })`, carrying
    /// whatever was last observed.
    async fn wait(
        &self,
        cluster: &ClusterHandle,
        check: &ReadinessCheck,
        deadline: Instant,
    ) -> Result<ReadinessEvidence, InstallError>;
}

/// What [`ReadinessProbe::wait`] found: whether `check` became
/// satisfied, what was last observed (redacted — see
/// [`redact_for_evidence`]), and how long waiting took.
///
/// These field names are canonical (the cross-task struct registry):
/// later tasks may add fields but must not rename these.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadinessEvidence {
    /// The check this evidence is for.
    pub check: ReadinessCheck,
    /// Whether `check` was satisfied before `deadline`.
    pub satisfied: bool,
    /// The last object observed while waiting, if any was ever
    /// successfully fetched — redacted (see [`redact_for_evidence`])
    /// before being stored here. `None` when the target object was
    /// never observed at all (for example
    /// [`ReadinessCheck::WebhookConfigurationPresent`] before the
    /// component has created it), never fabricated (Global Constraint
    /// 15).
    pub last_observed: Option<serde_json::Value>,
    /// Wall-clock time [`ReadinessProbe::wait`] spent waiting.
    pub elapsed: Duration,
}

/// Capped exponential backoff between poll attempts: doubles each time,
/// starting from `initial`, never exceeding `max`.
///
/// `initial: 250ms` / `max: 10s` (see [`Default`]) is deliberately much
/// shorter than `kube_runtime`'s own watch-reconnect
/// `DefaultBackoff` (800ms / 30s, "inspired by client-go", tuned to
/// protect a *shared* apiserver from many independent long-lived
/// watchers reconnecting in lockstep after an outage). Neither concern
/// applies here: this is one process polling one specific, already-named
/// object, deadline-bounded, not a fleet of watchers — so a shorter
/// initial delay is safe (catches a fast-starting component sooner) and
/// a lower cap keeps the last few samples before a deadline meaningfully
/// spaced rather than coarse. No jitter, unlike `DefaultBackoff`: jitter
/// exists to desynchronize *many* independent clients, which does not
/// apply to a single poll loop, and this project's own point is
/// deterministic behavior, which an unseeded random jitter would work
/// against for no offsetting benefit here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffPolicy {
    /// The delay before the second poll attempt (the first always
    /// happens immediately).
    pub initial: Duration,
    /// The delay this backoff never grows past.
    pub max: Duration,
}

impl BackoffPolicy {
    /// Returns the next delay after `current`: doubled, capped at
    /// [`BackoffPolicy::max`]. Pure integer `Duration` arithmetic
    /// (`saturating_mul`/`min`), not floating point, so the sequence
    /// this produces is exactly reproducible.
    #[must_use]
    pub fn advance(&self, current: Duration) -> Duration {
        current.saturating_mul(2).min(self.max)
    }
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(250),
            max: Duration::from_secs(10),
        }
    }
}

/// The I/O boundary [`poll_readiness`] polls through: fetch the current
/// state of whatever object a [`ReadinessCheck`] names, or `Ok(None)`
/// if it does not exist (yet).
///
/// Kept as a small trait — implemented by [`ResolvedTarget`] for the
/// real `kube`-backed probe — specifically so [`poll_readiness`]'s
/// backoff/deadline orchestration can be unit-tested against a fake
/// implementation with no cluster at all. This is the same
/// "separate the fetch from the predicate" split [`evaluate`] gives
/// Brief Step 1, applied to Step 3.
#[async_trait]
pub trait ReadinessFetch: Send + Sync {
    /// Fetches the current object, or `None` if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns any transport/apiserver error other than "not found".
    /// [`poll_readiness`] treats this the same as `Ok(None)` — retryable,
    /// never fatal — see this module's documentation for why.
    async fn fetch(&self) -> Result<Option<serde_json::Value>, kube::Error>;
}

/// Polls `fetch` for `check`'s target object, applying [`evaluate`] to
/// each observed object, until either `check` is satisfied or
/// `deadline` passes. Waits between attempts using `backoff`, clamped
/// so it never sleeps past `deadline` itself.
///
/// Always attempts at least once, even if `deadline` has already passed
/// by the time this is called — a caller handing in an already-expired
/// deadline still gets one real observation rather than an instantly
/// fabricated "not ready".
///
/// Never returns an error — see this module's documentation's "Errors
/// during polling are retried, not surfaced" section.
#[must_use]
pub async fn poll_readiness(
    check: &ReadinessCheck,
    deadline: Instant,
    backoff: &BackoffPolicy,
    fetch: &dyn ReadinessFetch,
) -> ReadinessEvidence {
    let start = Instant::now();
    let mut last_observed: Option<serde_json::Value> = None;
    let mut delay = backoff.initial;

    loop {
        match fetch.fetch().await {
            Ok(Some(observed)) => {
                let satisfied = evaluate(check, Some(&observed));
                last_observed = Some(redact_for_evidence(observed));
                if satisfied {
                    return ReadinessEvidence {
                        check: check.clone(),
                        satisfied: true,
                        last_observed,
                        elapsed: start.elapsed(),
                    };
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(
                    ?check,
                    %error,
                    "readiness probe attempt failed; treating as not-yet-ready and retrying until deadline"
                );
            }
        }

        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let sleep_for = delay.min(deadline.saturating_duration_since(now));
        tokio::time::sleep(sleep_for).await;
        delay = backoff.advance(delay);
    }

    ReadinessEvidence {
        check: check.clone(),
        satisfied: false,
        last_observed,
        elapsed: start.elapsed(),
    }
}

/// Evaluates whether `observed` (the target object last read off the
/// cluster, or `None` if it does not exist / has not been observed yet)
/// satisfies `check`.
///
/// Pure and synchronous — no cluster access, no I/O — precisely so it
/// can be unit-tested against captured objects with no live cluster at
/// all (Task 2.4 brief Step 1).
#[must_use]
pub fn evaluate(check: &ReadinessCheck, observed: Option<&serde_json::Value>) -> bool {
    match check {
        ReadinessCheck::WebhookConfigurationPresent { .. } => observed.is_some(),
        ReadinessCheck::DeploymentAvailable { .. } => {
            observed.is_some_and(|value| condition_status(value, "Available") == Some("True"))
        }
        ReadinessCheck::JobComplete { .. } => {
            observed.is_some_and(|value| condition_status(value, "Complete") == Some("True"))
        }
        ReadinessCheck::DaemonSetReady { .. } => observed.is_some_and(daemonset_ready),
        ReadinessCheck::CustomResourceCondition {
            condition_type,
            status,
            ..
        } => observed
            .is_some_and(|value| condition_status(value, condition_type) == Some(status.as_str())),
    }
}

/// Finds a condition of type `condition_type` inside `observed`'s
/// `.status.conditions[]` array — the shape `Deployment`, `Job`, and
/// any custom resource that follows the conventional Kubernetes
/// conditions pattern share (`DaemonSet` does not; see
/// [`daemonset_ready`]) — and returns its `status` field, if present.
fn condition_status<'a>(observed: &'a serde_json::Value, condition_type: &str) -> Option<&'a str> {
    observed
        .get("status")?
        .get("conditions")?
        .as_array()?
        .iter()
        .find(|condition| {
            condition.get("type").and_then(serde_json::Value::as_str) == Some(condition_type)
        })?
        .get("status")?
        .as_str()
}

/// `DaemonSet` readiness from its scalar status counters. See this
/// module's documentation ("`DaemonSetReady`: no `.status.conditions`,
/// and what zero desired means") for the algorithm and why the
/// generation check comes first.
fn daemonset_ready(observed: &serde_json::Value) -> bool {
    let generation = observed
        .get("metadata")
        .and_then(|metadata| metadata.get("generation"))
        .and_then(serde_json::Value::as_i64);
    let Some(status) = observed.get("status") else {
        return false;
    };
    let observed_generation = status
        .get("observedGeneration")
        .and_then(serde_json::Value::as_i64);

    let reconciled = matches!(
        (generation, observed_generation),
        (Some(generation), Some(observed_generation)) if observed_generation >= generation
    );
    if !reconciled {
        return false;
    }

    let counter = |field: &str| {
        status
            .get(field)
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
    };
    let desired = counter("desiredNumberScheduled");
    counter("updatedNumberScheduled") >= desired && counter("numberAvailable") >= desired
}

/// Applied to every object about to be stored in
/// [`ReadinessEvidence::last_observed`]. See this module's
/// documentation ("Redaction of `last_observed`, today") for what this
/// does and does not cover, and why.
#[must_use]
pub fn redact_for_evidence(mut value: serde_json::Value) -> serde_json::Value {
    const REDACTED_MARKER: &str = "[REDACTED]";

    let is_secret = value.get("kind").and_then(serde_json::Value::as_str) == Some("Secret");
    if is_secret {
        for field in ["data", "stringData"] {
            if let Some(serde_json::Value::Object(map)) = value.get_mut(field) {
                for entry in map.values_mut() {
                    *entry = serde_json::Value::String(REDACTED_MARKER.to_string());
                }
            }
        }
    }
    value
}

/// Builds a `kube::Client` from `cluster`'s own kubeconfig — never the
/// operator's ambient `~/.kube/config` / `$KUBECONFIG`. This module's
/// one chokepoint for turning a [`ClusterHandle`] into a `kube::Client`,
/// mirroring `manifests.rs`'s `kubectl_command`: the only place a
/// client is ever built, so isolation cannot be silently dropped by a
/// future call site reaching for `Client::try_default()`/
/// `Config::infer()` (which read `$KUBECONFIG`/`~/.kube/config`)
/// instead.
async fn client_for(cluster: &ClusterHandle) -> Result<Client, kube::Error> {
    let kubeconfig = Kubeconfig::read_from(&cluster.kubeconfig)?;
    let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default()).await?;
    Client::try_from(config)
}

/// A cluster-bound, ready-to-poll handle for one [`ReadinessCheck`]:
/// whatever `Api<K>` (or pair of them, for
/// [`ReadinessCheck::WebhookConfigurationPresent`] — see this module's
/// documentation for why two) the check needs, built once before
/// polling begins rather than re-built on every attempt.
enum ResolvedTarget {
    /// [`ReadinessCheck::DeploymentAvailable`].
    Deployment(Api<Deployment>, String),
    /// [`ReadinessCheck::DaemonSetReady`].
    DaemonSet(Api<DaemonSet>, String),
    /// [`ReadinessCheck::JobComplete`].
    Job(Api<Job>, String),
    /// [`ReadinessCheck::WebhookConfigurationPresent`]: validating,
    /// then mutating, tried in that order.
    Webhook(
        Api<ValidatingWebhookConfiguration>,
        Api<MutatingWebhookConfiguration>,
        String,
    ),
    /// [`ReadinessCheck::CustomResourceCondition`].
    CustomResource(Api<DynamicObject>, String),
}

#[async_trait]
impl ReadinessFetch for ResolvedTarget {
    async fn fetch(&self) -> Result<Option<serde_json::Value>, kube::Error> {
        match self {
            Self::Deployment(api, name) => to_value_opt(api.get_opt(name).await?),
            Self::DaemonSet(api, name) => to_value_opt(api.get_opt(name).await?),
            Self::Job(api, name) => to_value_opt(api.get_opt(name).await?),
            Self::CustomResource(api, name) => to_value_opt(api.get_opt(name).await?),
            Self::Webhook(validating, mutating, name) => {
                if let Some(found) = validating.get_opt(name).await? {
                    return to_value_opt(Some(found));
                }
                to_value_opt(mutating.get_opt(name).await?)
            }
        }
    }
}

/// Converts a fetched object (if any) to `serde_json::Value`, mapping a
/// serialization failure — expected to be unreachable for any of these
/// well-formed generated/dynamic types, but never assumed infallible —
/// to `kube::Error::SerdeError`, the same variant `kube` itself uses for
/// a response deserialization failure. [`poll_readiness`] then treats it
/// like any other fetch error: retryable, not fatal.
fn to_value_opt<T: Serialize>(object: Option<T>) -> Result<Option<serde_json::Value>, kube::Error> {
    object
        .map(|value| serde_json::to_value(&value).map_err(kube::Error::SerdeError))
        .transpose()
}

/// Builds the [`ResolvedTarget`] for `check` against `cluster`. Both of
/// this function's failure modes are permanent, discovered once, before
/// any poll attempt begins — see [`InstallError::ReadinessUnavailable`]:
/// the client itself ([`client_for`]), or — only for
/// [`ReadinessCheck::CustomResourceCondition`] — parsing `api_version`
/// into a group/version.
async fn resolve_target(
    cluster: &ClusterHandle,
    check: &ReadinessCheck,
) -> Result<ResolvedTarget, String> {
    let client = client_for(cluster)
        .await
        .map_err(|error| error.to_string())?;
    Ok(match check {
        ReadinessCheck::DeploymentAvailable { namespace, name } => {
            ResolvedTarget::Deployment(Api::namespaced(client, namespace), name.clone())
        }
        ReadinessCheck::DaemonSetReady { namespace, name } => {
            ResolvedTarget::DaemonSet(Api::namespaced(client, namespace), name.clone())
        }
        ReadinessCheck::JobComplete { namespace, name } => {
            ResolvedTarget::Job(Api::namespaced(client, namespace), name.clone())
        }
        ReadinessCheck::WebhookConfigurationPresent { name } => {
            ResolvedTarget::Webhook(Api::all(client.clone()), Api::all(client), name.clone())
        }
        ReadinessCheck::CustomResourceCondition {
            api_version,
            kind,
            namespace,
            name,
            ..
        } => {
            let group_version: GroupVersion = api_version
                .parse()
                .map_err(|error| format!("invalid api_version {api_version:?}: {error}"))?;
            let resource = ApiResource::from_gvk(&group_version.with_kind(kind));
            let api = match namespace {
                Some(ns) => Api::namespaced_with(client, ns, &resource),
                None => Api::all_with(client, &resource),
            };
            ResolvedTarget::CustomResource(api, name.clone())
        }
    })
}

/// The production [`ReadinessProbe`]: polls the real Kubernetes API of
/// whichever cluster [`ReadinessProbe::wait`] is called with, through
/// `kube`, with [`BackoffPolicy::default`].
#[derive(Debug, Clone, Copy, Default)]
pub struct KubeReadinessProbe;

impl KubeReadinessProbe {
    /// Creates a probe using [`BackoffPolicy::default`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ReadinessProbe for KubeReadinessProbe {
    async fn wait(
        &self,
        cluster: &ClusterHandle,
        check: &ReadinessCheck,
        deadline: Instant,
    ) -> Result<ReadinessEvidence, InstallError> {
        let target = resolve_target(cluster, check).await.map_err(|reason| {
            InstallError::ReadinessUnavailable {
                check: Box::new(check.clone()),
                reason,
            }
        })?;
        Ok(poll_readiness(check, deadline, &BackoffPolicy::default(), &target).await)
    }
}
