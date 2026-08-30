//! Replaying a fixture through a real API server and classifying what it
//! decided (Task 3.4's high-level half).
//!
//! [`admissionlab_fixtures::execute::dry_run_create`] (this crate's new
//! `admission -> fixtures` dependency, Controller supplement §2) does
//! the actual server-side dry-run CREATE and hands back a real,
//! unclassified answer -- an admitted object, or the `kube::core::Status`
//! the API server rejected the request with. [`AdmissionExecutor`] is
//! this crate's own contract for the next step: turning that raw answer
//! into [`crate::outcome::AdmissionDecision`], this project's reporting
//! vocabulary. [`KubeAdmissionExecutor`] is the one production
//! implementation.
//!
//! # Global Constraint 16, and the one rule this module must not bend
//!
//! > A fixture that cannot be safely evaluated with server-side dry-run
//! > must fail explicitly as unsupported for that mode rather than
//! > silently switch semantics.
//!
//! This module never falls back to a persisted (non-dry-run) CREATE --
//! `dry_run: true` is unconditional in
//! [`admissionlab_fixtures::execute::dry_run_create`], and nothing here
//! ever asks for anything else. What GC16 leaves to this module is
//! *classification*: [`classify_rejection`] below is the one place that
//! decides whether a non-2xx response is an ordinary
//! [`crate::outcome::AdmissionDecision::Rejected`] (valid comparison
//! data) or a
//! [`crate::outcome::AdmissionDecision::UnsupportedDryRun`] (a lab
//! capability limit). See that function's own documentation for why, as
//! of this task, it never returns the latter.
//!
//! # `UnsupportedDryRun`: what was checked, and why this task does not assert it
//!
//! Controller supplement §5 describes the mechanism precisely: "the API
//! server refuses a `dryRun` request when a matching webhook declares
//! `sideEffects` of `Unknown` or `Some`", and explicitly declines to
//! guess the resulting error's exact shape, asking this task to confirm
//! it against a real API server instead.
//!
//! That was attempted, against a real `kind` v1.37.0 cluster, and it
//! could not be completed -- not because observing the error is hard,
//! but because **the precondition itself cannot be created** on any
//! Kubernetes version this project targets (1.35-1.37; see
//! `research-kube-api.md` §1). Confirmed three ways, live:
//!
//! 1. `kubectl api-versions` on a fresh `kind` v1.37.0 cluster serves
//!    only `admissionregistration.k8s.io/v1` -- `v1beta1` was removed
//!    from Kubernetes entirely in 1.22 and is not reachable here.
//! 2. `kubectl explain validatingwebhookconfiguration.webhooks.sideEffects`
//!    against that same cluster states: "Acceptable values are: `None`,
//!    `NoneOnDryRun` (webhooks created via v1beta1 may also specify
//!    `Some` or `Unknown`)" -- `Some`/`Unknown` are a `v1beta1`-only
//!    allowance.
//! 3. Applying a `ValidatingWebhookConfiguration` with
//!    `sideEffects: Some` (and separately `sideEffects: Unknown`)
//!    through the `v1` API on that cluster was rejected outright by the
//!    API server's own object validation, *before* any dry-run request
//!    could ever reach it:
//!    ```text
//!    The ValidatingWebhookConfiguration "..." is invalid:
//!    webhooks[0].sideEffects: Unsupported value: "Some":
//!    supported values: "None", "NoneOnDryRun"
//!    ```
//!    (`reason: "Invalid"`, `code: 422` -- a webhook-*configuration*
//!    validation failure, not the dry-run-CREATE rejection this task
//!    needs to observe.)
//!
//! So on every Kubernetes version this project targets, no webhook that
//! could ever trigger the mechanism supplement §5 describes can exist in
//! the first place -- not "rare", but structurally unreachable through
//! the only admission-registration API these clusters serve. Kyverno's
//! and Istio's own webhooks are registered the same way (`v1`), so this
//! is not a gap specific to this project's own test fixtures either.
//!
//! Two failure shapes that *are* real and were captured live were
//! deliberately **not** used as a substitute signal, because neither one
//! is what supplement §5 describes:
//!
//! - A `sideEffects: None` webhook whose backing service does not exist
//!   yields `reason: "InternalError"`, `code: 500`, message `"failed
//!   calling webhook ... failed to call webhook: ..."` -- a webhook
//!   *connectivity* failure (the equivalent failure would happen on a
//!   real, non-dry-run request too), not a dry-run-safety refusal.
//!   [`classify_rejection`] does not treat this as `UnsupportedDryRun`
//!   either -- see its own documentation.
//! - A genuine policy denial (tested via the built-in `PodSecurity`
//!   admission plugin) yields `reason: "Forbidden"`, `code: 403` --
//!   confirming the shape [`classify_rejection`] does map to `Rejected`.
//!
//! Given all of that, asserting `UnsupportedDryRun` here from a guessed
//! message pattern is exactly the shipped-wrong-guess GC15 exists to
//! prevent, and the supplement is explicit that the honest-unknown
//! choice is `Rejected`, not a fabricated third bucket. So
//! [`classify_rejection`] classifies every non-2xx dry-run CREATE
//! response as `Rejected` today.
//! [`crate::outcome::AdmissionDecision::UnsupportedDryRun`] itself is
//! left fully intact (Task 3.3's type, this task does not touch it) so
//! a later task can wire in real detection the moment a genuine,
//! confirmed trigger exists -- on a version where `v1beta1` webhooks
//! still applied, on a future Kubernetes release that changes this
//! validation, or via evidence this task's live probe did not
//! anticipate.

use std::time::{Duration, SystemTime};

use admissionlab_core::ClusterHandle;
use admissionlab_fixtures::{FixtureSource, ResolvedResource};
use async_trait::async_trait;
use kube::core::Status;
use thiserror::Error;

use crate::outcome::AdmissionDecision;

/// The contract that replays one fixture through one cluster via a real
/// server-side dry-run CREATE and reports what the API server decided.
/// See this module's documentation for the one production implementation
/// ([`KubeAdmissionExecutor`]) and for why `UnsupportedDryRun` detection
/// is not yet wired in.
///
/// `Send + Sync` for the same reason every other async trait in this
/// workspace is (see `admissionlab_core::ClusterManager`'s
/// documentation): a later task replays fixtures against baseline and
/// candidate clusters concurrently, the same way clusters are created
/// and components installed concurrently today.
///
/// Deliberately **not** named or shaped so `admissionlab-core`'s
/// `run.rs` could call it directly: its own parameters
/// (`&FixtureSource`, `&ResolvedResource`) are `admissionlab-fixtures`
/// types, and calling this trait from `core` would need a `core ->
/// admission` edge, which -- combined with the `admission -> fixtures ->
/// core` edges that already exist by this task -- Cargo would reject as
/// a cycle outright. Task 3.10 integrates this crate's pipeline into
/// `run.rs` through a *separate*, coarser trait declared in `core` using
/// only core-visible types (controller supplement §2), not this one.
#[async_trait]
pub trait AdmissionExecutor: Send + Sync {
    /// Replays `fixture` (already resolved to `resource` on `cluster` --
    /// Task 3.2's job) as one server-side dry-run CREATE, and classifies
    /// the API server's response.
    ///
    /// # Errors
    ///
    /// Returns [`FixtureExecutionError`] if no real response could be
    /// obtained at all -- see that type's own documentation. Never
    /// returned merely because the API server rejected the object: that
    /// is [`crate::outcome::AdmissionDecision::Rejected`], a successful
    /// `Ok` result.
    async fn execute_create(
        &self,
        cluster: &ClusterHandle,
        fixture: &FixtureSource,
        resource: &ResolvedResource,
    ) -> Result<RawAdmissionResponse, FixtureExecutionError>;
}

/// What [`AdmissionExecutor::execute_create`] observed from one real
/// server-side dry-run CREATE: the classified decision, the object as
/// the API server would have persisted it (when admitted), any
/// `Warning` response headers, and request timing.
///
/// A lower-level sibling of [`crate::outcome::AdmissionOutcome`]: this
/// type is what one raw replay attempt produced, before Task 3.7 folds
/// it (and a webhook trace reconstructed from audit-log evidence) into
/// that richer, per-fixture-per-side report type.
#[derive(Debug, Clone)]
pub struct RawAdmissionResponse {
    /// The classified admission decision. See [`classify_rejection`]'s
    /// documentation for exactly how a non-2xx response maps to this.
    pub decision: AdmissionDecision,
    /// The object the API server reports it would have persisted, when
    /// [`RawAdmissionResponse::decision`] is
    /// [`crate::outcome::AdmissionDecision::Accepted`]. `None` for a
    /// rejection of any kind -- a rejection's response body is a
    /// `Status` object describing the failure, not the fixture object,
    /// so there is no persisted-form object to report (Global Constraint
    /// 15: never substituted with the *fixture's own* input object,
    /// which is not what was actually observed coming back from the API
    /// server).
    pub response_object: Option<serde_json::Value>,
    /// `Warning` HTTP response header values, verbatim and in the order
    /// the API server sent them -- see
    /// `admissionlab_fixtures::execute`'s own module documentation for
    /// how these were captured and confirmation this was checked live,
    /// not merely assumed reachable.
    pub warnings: Vec<String>,
    /// Wall-clock time the dry-run CREATE request took.
    pub elapsed: Duration,
    /// Wall-clock time just before the request was sent.
    pub request_started_at: SystemTime,
    /// Wall-clock time just after the response finished arriving.
    pub request_finished_at: SystemTime,
}

/// [`AdmissionExecutor::execute_create`]'s failure mode: no real
/// response could be obtained from the API server at all. Never
/// constructed for an ordinary admission denial -- that is
/// [`crate::outcome::AdmissionDecision::Rejected`], a successful `Ok`.
#[derive(Debug, Error)]
pub enum FixtureExecutionError {
    /// The dry-run CREATE itself could not be carried out. Wraps
    /// [`admissionlab_fixtures::FixtureError`] (in practice always its
    /// own [`admissionlab_fixtures::FixtureError::ReplayUnavailable`]
    /// variant, since that is the only one
    /// `admissionlab_fixtures::execute::dry_run_create` returns) rather
    /// than re-deriving a second copy of the same reasons -- see that
    /// variant's own documentation for exactly what it covers.
    #[error(transparent)]
    Replay(#[from] admissionlab_fixtures::FixtureError),
}

/// The one production [`AdmissionExecutor`]: replays a fixture against a
/// real Kubernetes API server via
/// [`admissionlab_fixtures::execute::dry_run_create`].
#[derive(Debug, Clone, Copy, Default)]
pub struct KubeAdmissionExecutor;

impl KubeAdmissionExecutor {
    /// Creates an executor. Carries no state of its own -- every call
    /// resolves a fresh `kube::Client` from `cluster`'s own kubeconfig
    /// (mirroring `admissionlab_installer::readiness::client_for` and
    /// `admissionlab_fixtures::resources::client_for`, neither of which
    /// this crate's own dry-run path bypasses).
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AdmissionExecutor for KubeAdmissionExecutor {
    async fn execute_create(
        &self,
        cluster: &ClusterHandle,
        fixture: &FixtureSource,
        resource: &ResolvedResource,
    ) -> Result<RawAdmissionResponse, FixtureExecutionError> {
        let raw = admissionlab_fixtures::execute::dry_run_create(cluster, resource, fixture)
            .await
            .map_err(FixtureExecutionError::from)?;
        Ok(classify(raw))
    }
}

/// [`KubeAdmissionExecutor::execute_create`]'s offline-testable core:
/// given an already-built `client`, issues the dry-run CREATE (via
/// [`admissionlab_fixtures::execute::dry_run_create_with_client`]) and
/// classifies the result. `cluster_name` only labels a
/// [`FixtureExecutionError`] if this fails; it is never used to build or
/// look up `client` itself, so this function never touches a kubeconfig
/// or the filesystem -- exactly the seam
/// `admissionlab-admission`'s own `tests/execute_unit.rs` drives against
/// a `tower_test::mock`-backed `Client` (Task 3.4 brief Step 1).
///
/// # Errors
///
/// See [`admissionlab_fixtures::execute::dry_run_create_with_client`]'s
/// own documentation for this function's error cases -- classification
/// itself never fails.
pub async fn execute_create_with_client(
    client: kube::Client,
    cluster_name: &str,
    fixture: &FixtureSource,
    resource: &ResolvedResource,
) -> Result<RawAdmissionResponse, FixtureExecutionError> {
    let raw = admissionlab_fixtures::execute::dry_run_create_with_client(
        client,
        cluster_name,
        resource,
        fixture,
    )
    .await
    .map_err(FixtureExecutionError::from)?;
    Ok(classify(raw))
}

/// Converts a low-level
/// [`admissionlab_fixtures::execute::DryRunCreateResponse`] into this
/// crate's own [`RawAdmissionResponse`], classifying any rejection via
/// [`classify_rejection`]. Shared by [`KubeAdmissionExecutor::execute_create`]
/// and [`execute_create_with_client`] so classification exists in
/// exactly one place regardless of which one built the `Client`.
fn classify(raw: admissionlab_fixtures::DryRunCreateResponse) -> RawAdmissionResponse {
    let (decision, response_object) = match raw.result {
        Ok(admitted) => (AdmissionDecision::Accepted, Some(admitted)),
        Err(status) => (classify_rejection(&status), None),
    };
    RawAdmissionResponse {
        decision,
        response_object,
        warnings: raw.warnings,
        elapsed: raw.elapsed,
        request_started_at: raw.request_started_at,
        request_finished_at: raw.request_finished_at,
    }
}

/// Classifies one non-2xx dry-run CREATE response's `Status` into an
/// [`AdmissionDecision`].
///
/// Always returns [`AdmissionDecision::Rejected`] today. See this
/// module's own documentation ("`UnsupportedDryRun`: what was checked,
/// and why this task does not assert it") for the live investigation
/// this reflects: the specific mechanism Controller supplement §5
/// describes (a matching webhook declaring `sideEffects: Some`/
/// `Unknown`) is structurally unreachable on every Kubernetes version
/// this project targets, so there is no confirmed, narrow signal to
/// match against, and Global Constraint 15 says to report that honest
/// unknown as `Rejected` rather than assert `UnsupportedDryRun` from a
/// guess.
///
/// `status.code` maps to [`AdmissionDecision::Rejected`]'s own `code`
/// field only when non-zero: `kube::core::Status::code` defaults to `0`
/// when the
/// response body did not carry one (`#[serde(default)]`), and `0` is
/// never a real HTTP status code, so it is treated the same as "not
/// observed" rather than reported as a fabricated code (Global
/// Constraint 15) -- pinned by
/// `zero_status_code_is_reported_as_no_code_not_a_fabricated_zero`
/// below.
fn classify_rejection(status: &Status) -> AdmissionDecision {
    AdmissionDecision::Rejected {
        code: (status.code != 0).then_some(status.code),
        message: status.message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use kube::core::Status;

    use super::classify_rejection;
    use crate::outcome::AdmissionDecision;

    /// The mutation this test exists to kill: an implementation that
    /// reported `Some(0)` (or any other fabricated default) instead of
    /// `None` when the API server's `Status` body carried no `code` at
    /// all.
    #[test]
    fn zero_status_code_is_reported_as_no_code_not_a_fabricated_zero() {
        let status = Status {
            message: "boom".to_string(),
            ..Status::default()
        };
        let decision = classify_rejection(&status);
        assert_eq!(
            decision,
            AdmissionDecision::Rejected {
                code: None,
                message: "boom".to_string(),
            }
        );
    }

    /// The mutation this test exists to kill: an implementation that
    /// dropped `status.code` on the floor entirely, or hardcoded some
    /// other value regardless of what the `Status` actually carried.
    #[test]
    fn a_nonzero_status_code_is_carried_through() {
        let status = Status {
            code: 403,
            message: "denied".to_string(),
            ..Status::default()
        };
        let decision = classify_rejection(&status);
        assert_eq!(
            decision,
            AdmissionDecision::Rejected {
                code: Some(403),
                message: "denied".to_string(),
            }
        );
    }
}
