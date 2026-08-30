//! Unit tests for `admissionlab_installer`'s Kubernetes readiness probes
//! (Task 2.4).
//!
//! No test here creates a cluster or talks to a real Kubernetes API
//! server. [`admissionlab_installer::readiness::evaluate`] is a pure
//! function over captured (`serde_json::json!`-built) objects — the
//! design the Task 2.4 brief's Step 1 asks for, so "does this object
//! satisfy the check" is testable with no cluster at all.
//! [`admissionlab_installer::readiness::poll_readiness`] is the
//! backoff/deadline orchestration, exercised here against
//! [`ScriptedFetch`]/[`AlwaysReturns`] — fake
//! [`admissionlab_installer::readiness::ReadinessFetch`] implementations
//! that never perform I/O.
//!
//! `readiness.rs`'s own internal `tests` module (not this file — those
//! are private items, so an external test cannot reach them) separately
//! covers `client_for`/`resolve_target`'s error paths offline (a real
//! temp-file kubeconfig, no cluster) and `ResolvedTarget::fetch`'s
//! validating-then-mutating fallback against a `tower_test::mock` fake
//! service — `Client::try_from`/`ClientBuilder::build` are synchronous,
//! local `tower`-stack construction with no network I/O, so those are
//! genuinely offline tests too, not live-cluster ones. What remains
//! untested anywhere, left for the Phase 2 exit gate: whether
//! `client_for` actually *connects* using a real `kind`-produced
//! kubeconfig, whether `ApiResource::from_gvk`'s plural guesser resolves
//! correctly for real project-defined CRDs, and end-to-end behavior
//! against a real Kyverno install.
//!
//! Covers Task 2.4 brief Step 1 and this task's minimum coverage list:
//! - Each of the five [`admissionlab_spec::ReadinessCheck`] variants'
//!   predicate against a satisfying and a non-satisfying captured
//!   object.
//! - `DaemonSetReady`'s counter comparison specifically, including the
//!   zero-desired case (`daemonset_ready_true_when_desired_is_zero_and_generation_observed`
//!   /
//!   `daemonset_ready_false_when_desired_is_zero_but_generation_not_yet_observed`).
//! - Backoff growth being capped.
//! - The absolute deadline being respected.
//! - A deadline failure carrying `last_observed` rather than losing it.
//! - Redaction of `Secret`-shaped observed objects, and of a literal
//!   credential-shaped `env[].value` in a `Deployment`/`DaemonSet`/`Job`
//!   pod template (`redact_masks_a_hardcoded_credential_in_a_deployment_env_value`
//!   and neighbors) — found in code review as the more likely of the two
//!   vectors.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use admissionlab_installer::readiness::{
    BackoffPolicy, ReadinessFetch, evaluate, poll_readiness, redact_for_evidence,
};
use admissionlab_spec::ReadinessCheck;
use async_trait::async_trait;
use serde_json::{Value, json};

// ---------------------------------------------------------------------
// Predicates (Brief Step 1): DeploymentAvailable
// ---------------------------------------------------------------------

fn deployment_available_check() -> ReadinessCheck {
    ReadinessCheck::DeploymentAvailable {
        namespace: "kyverno".to_string(),
        name: "kyverno-admission-controller".to_string(),
    }
}

#[test]
fn deployment_available_true_when_condition_status_true() {
    let observed = json!({
        "status": {
            "conditions": [
                {"type": "Progressing", "status": "True"},
                {"type": "Available", "status": "True"},
            ]
        }
    });
    assert!(evaluate(&deployment_available_check(), Some(&observed)));
}

#[test]
fn deployment_available_false_when_condition_status_false() {
    let observed = json!({
        "status": {
            "conditions": [
                {"type": "Available", "status": "False", "reason": "ProgressDeadlineExceeded"},
            ]
        }
    });
    assert!(!evaluate(&deployment_available_check(), Some(&observed)));
}

#[test]
fn deployment_available_false_when_conditions_array_missing() {
    let observed = json!({"status": {"replicas": 1}});
    assert!(!evaluate(&deployment_available_check(), Some(&observed)));
}

#[test]
fn deployment_available_false_when_no_object_observed() {
    assert!(!evaluate(&deployment_available_check(), None));
}

// ---------------------------------------------------------------------
// Predicates: DaemonSetReady
// ---------------------------------------------------------------------

fn daemonset_ready_check() -> ReadinessCheck {
    ReadinessCheck::DaemonSetReady {
        namespace: "kyverno".to_string(),
        name: "kyverno-admission-controller".to_string(),
    }
}

fn daemonset_json(
    generation: i64,
    observed_generation: Option<i64>,
    desired: i64,
    updated: i64,
    available: i64,
) -> Value {
    let mut status = json!({
        "desiredNumberScheduled": desired,
        "updatedNumberScheduled": updated,
        "numberAvailable": available,
    });
    if let Some(og) = observed_generation {
        status["observedGeneration"] = json!(og);
    }
    json!({
        "metadata": {"generation": generation},
        "status": status,
    })
}

#[test]
fn daemonset_ready_true_when_updated_and_available_meet_desired_and_generation_observed() {
    let observed = daemonset_json(1, Some(1), 3, 3, 3);
    assert!(evaluate(&daemonset_ready_check(), Some(&observed)));
}

#[test]
fn daemonset_ready_false_when_updated_number_scheduled_below_desired() {
    let observed = daemonset_json(1, Some(1), 3, 1, 1);
    assert!(!evaluate(&daemonset_ready_check(), Some(&observed)));
}

#[test]
fn daemonset_ready_false_when_number_available_below_desired() {
    // updated caught up, but available pods (post-readiness-probe) have not.
    let observed = daemonset_json(1, Some(1), 3, 3, 2);
    assert!(!evaluate(&daemonset_ready_check(), Some(&observed)));
}

/// The case the brief explicitly calls out: a `DaemonSet` that
/// legitimately targets zero nodes (its `nodeSelector` matches nothing
/// in this cluster) is ready the moment the controller has reconciled
/// at least once -- `0 == 0` is a real "nothing to do here" state, not a
/// hang.
#[test]
fn daemonset_ready_true_when_desired_is_zero_and_generation_observed() {
    let observed = daemonset_json(1, Some(1), 0, 0, 0);
    assert!(evaluate(&daemonset_ready_check(), Some(&observed)));
}

/// The trap a naive `desired == ready`-style comparison falls into: a
/// freshly created `DaemonSet` the controller has not reconciled even
/// once also reports every counter as `0` (Kubernetes defaults), which
/// is indistinguishable from "legitimately targets zero nodes" by the
/// counters alone. Requiring `status.observedGeneration >=
/// metadata.generation` first (the same check `kubectl rollout status
/// daemonset` itself makes) is what keeps this from being a false
/// "ready" reported before the controller has looked at the object at
/// all.
#[test]
fn daemonset_ready_false_when_desired_is_zero_but_generation_not_yet_observed() {
    let observed = daemonset_json(1, None, 0, 0, 0);
    assert!(!evaluate(&daemonset_ready_check(), Some(&observed)));
}

#[test]
fn daemonset_ready_false_when_observed_generation_is_stale() {
    // The controller reconciled generation 1, but the object has since
    // moved to generation 2 (e.g. a spec update) and has not been
    // reconciled again yet -- counters from the stale reconcile must not
    // be trusted.
    let observed = daemonset_json(2, Some(1), 3, 3, 3);
    assert!(!evaluate(&daemonset_ready_check(), Some(&observed)));
}

#[test]
fn daemonset_ready_false_when_no_object_observed() {
    assert!(!evaluate(&daemonset_ready_check(), None));
}

// ---------------------------------------------------------------------
// Predicates: JobComplete
// ---------------------------------------------------------------------

fn job_complete_check() -> ReadinessCheck {
    ReadinessCheck::JobComplete {
        namespace: "kyverno".to_string(),
        name: "kyverno-policy-install".to_string(),
    }
}

#[test]
fn job_complete_true_when_condition_status_true() {
    let observed = json!({"status": {"conditions": [{"type": "Complete", "status": "True"}]}});
    assert!(evaluate(&job_complete_check(), Some(&observed)));
}

#[test]
fn job_complete_false_when_condition_status_false() {
    let observed = json!({"status": {"conditions": [{"type": "Complete", "status": "False"}]}});
    assert!(!evaluate(&job_complete_check(), Some(&observed)));
}

#[test]
fn job_complete_false_when_condition_type_is_failed_instead() {
    // A Job that failed carries a `Failed: True` condition, not
    // `Complete: True` -- this must not be mistaken for readiness.
    let observed = json!({"status": {"conditions": [{"type": "Failed", "status": "True"}]}});
    assert!(!evaluate(&job_complete_check(), Some(&observed)));
}

#[test]
fn job_complete_false_when_no_object_observed() {
    assert!(!evaluate(&job_complete_check(), None));
}

// ---------------------------------------------------------------------
// Predicates: WebhookConfigurationPresent
// ---------------------------------------------------------------------

fn webhook_configuration_present_check() -> ReadinessCheck {
    ReadinessCheck::WebhookConfigurationPresent {
        name: "kyverno-policy-validating-webhook-cfg".to_string(),
    }
}

#[test]
fn webhook_configuration_present_true_when_object_observed() {
    // Kyverno creates its webhook configurations at runtime, not via its
    // Helm chart (research-kube-api.md finding #4) -- so a real observed
    // object here looks exactly like this: it exists once the admission
    // controller has started and registered itself, content aside.
    let observed = json!({
        "apiVersion": "admissionregistration.k8s.io/v1",
        "kind": "ValidatingWebhookConfiguration",
        "metadata": {"name": "kyverno-policy-validating-webhook-cfg"},
        "webhooks": [],
    });
    assert!(evaluate(
        &webhook_configuration_present_check(),
        Some(&observed)
    ));
}

#[test]
fn webhook_configuration_present_false_when_no_object_observed() {
    assert!(!evaluate(&webhook_configuration_present_check(), None));
}

// ---------------------------------------------------------------------
// Predicates: CustomResourceCondition
// ---------------------------------------------------------------------

fn custom_resource_condition_check() -> ReadinessCheck {
    ReadinessCheck::CustomResourceCondition {
        api_version: "kyverno.io/v2".to_string(),
        kind: "ClusterPolicy".to_string(),
        namespace: None,
        name: "disallow-latest-tag".to_string(),
        condition_type: "Ready".to_string(),
        status: "True".to_string(),
    }
}

#[test]
fn custom_resource_condition_true_when_condition_matches() {
    let observed = json!({"status": {"conditions": [{"type": "Ready", "status": "True"}]}});
    assert!(evaluate(
        &custom_resource_condition_check(),
        Some(&observed)
    ));
}

#[test]
fn custom_resource_condition_false_when_status_mismatches() {
    let observed = json!({"status": {"conditions": [{"type": "Ready", "status": "False"}]}});
    assert!(!evaluate(
        &custom_resource_condition_check(),
        Some(&observed)
    ));
}

#[test]
fn custom_resource_condition_false_when_condition_type_missing() {
    let observed = json!({"status": {"conditions": [{"type": "Validating", "status": "True"}]}});
    assert!(!evaluate(
        &custom_resource_condition_check(),
        Some(&observed)
    ));
}

#[test]
fn custom_resource_condition_matches_an_arbitrary_non_true_false_status_value() {
    // `CustomResourceCondition::status` is a caller-supplied string, not
    // hardcoded to "True"/"False" the way the other four checks are --
    // this proves the comparison is a literal match against whatever
    // string the check names, not a boolean coercion.
    let check = ReadinessCheck::CustomResourceCondition {
        api_version: "example.io/v1".to_string(),
        kind: "Widget".to_string(),
        namespace: Some("default".to_string()),
        name: "w".to_string(),
        condition_type: "Phase".to_string(),
        status: "Provisioning".to_string(),
    };
    let observed = json!({"status": {"conditions": [{"type": "Phase", "status": "Provisioning"}]}});
    assert!(evaluate(&check, Some(&observed)));
    let other = json!({"status": {"conditions": [{"type": "Phase", "status": "Ready"}]}});
    assert!(!evaluate(&check, Some(&other)));
}

#[test]
fn custom_resource_condition_false_when_no_object_observed() {
    assert!(!evaluate(&custom_resource_condition_check(), None));
}

// ---------------------------------------------------------------------
// Backoff: growth is capped
// ---------------------------------------------------------------------

#[test]
fn backoff_doubles_until_capped() {
    let policy = BackoffPolicy {
        initial: Duration::from_millis(100),
        max: Duration::from_secs(1),
    };
    let mut delay = policy.initial;
    let sequence: Vec<Duration> = std::iter::from_fn(|| {
        delay = policy.advance(delay);
        Some(delay)
    })
    .take(6)
    .collect();
    assert_eq!(
        sequence,
        vec![
            Duration::from_millis(200),
            Duration::from_millis(400),
            Duration::from_millis(800),
            Duration::from_secs(1), // capped, not 1.6s
            Duration::from_secs(1),
            Duration::from_secs(1),
        ]
    );
}

#[test]
fn backoff_never_exceeds_max_even_from_a_current_delay_already_past_it() {
    let policy = BackoffPolicy {
        initial: Duration::from_millis(100),
        max: Duration::from_secs(1),
    };
    // A delay that somehow already exceeds `max` (defensive: must still
    // clamp down, never grow further).
    let next = policy.advance(Duration::from_secs(10));
    assert_eq!(next, Duration::from_secs(1));
}

#[test]
fn default_backoff_policy_starts_small_and_caps_in_the_tens_of_seconds() {
    let policy = BackoffPolicy::default();
    assert!(policy.initial <= Duration::from_secs(1));
    assert!(policy.max <= Duration::from_secs(30));
    assert!(policy.initial < policy.max);
}

// ---------------------------------------------------------------------
// poll_readiness: deadline + backoff orchestration, and redaction
// ---------------------------------------------------------------------

/// A [`ReadinessFetch`] that always returns the same scripted result and
/// counts how many times it was called.
struct AlwaysReturns {
    result_template: Value,
    satisfied_shape: bool,
    calls: AtomicUsize,
}

impl AlwaysReturns {
    fn present(value: Value) -> Self {
        Self {
            result_template: value,
            satisfied_shape: true,
            calls: AtomicUsize::new(0),
        }
    }

    fn absent() -> Self {
        Self {
            result_template: Value::Null,
            satisfied_shape: false,
            calls: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ReadinessFetch for AlwaysReturns {
    async fn fetch(&self) -> Result<Option<Value>, kube::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.satisfied_shape {
            Ok(Some(self.result_template.clone()))
        } else {
            Ok(None)
        }
    }
}

/// A [`ReadinessFetch`] that replays a fixed sequence of results, one
/// per call, holding on the last entry once exhausted.
struct ScriptedFetch {
    responses: Mutex<VecDeque<Result<Option<Value>, kube::Error>>>,
    calls: AtomicUsize,
}

impl ScriptedFetch {
    fn new(responses: Vec<Result<Option<Value>, kube::Error>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            calls: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn fake_error() -> kube::Error {
        let bad_json = serde_json::from_str::<Value>("{not json").unwrap_err();
        kube::Error::SerdeError(bad_json)
    }
}

#[async_trait]
impl ReadinessFetch for ScriptedFetch {
    async fn fetch(&self) -> Result<Option<Value>, kube::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut guard = self.responses.lock().expect("mutex not poisoned");
        match guard.pop_front() {
            Some(result) => result,
            None => Ok(None),
        }
    }
}

fn fast_backoff() -> BackoffPolicy {
    BackoffPolicy {
        initial: Duration::from_millis(20),
        max: Duration::from_millis(40),
    }
}

#[tokio::test]
async fn poll_returns_immediately_once_satisfied_without_waiting_out_the_deadline() {
    let satisfying = json!({"status": {"conditions": [{"type": "Available", "status": "True"}]}});
    let fetch = AlwaysReturns::present(satisfying.clone());
    let deadline = Instant::now() + Duration::from_secs(60);

    let evidence = poll_readiness(
        &deployment_available_check(),
        deadline,
        &fast_backoff(),
        &fetch,
    )
    .await;

    assert!(evidence.satisfied);
    assert_eq!(evidence.last_observed, Some(satisfying));
    assert_eq!(fetch.call_count(), 1, "must not poll again once satisfied");
    assert!(
        evidence.elapsed < Duration::from_secs(5),
        "must not wait anywhere near the 60s deadline once satisfied, got {:?}",
        evidence.elapsed
    );
}

#[tokio::test]
async fn poll_respects_absolute_deadline_when_never_satisfied() {
    let fetch = AlwaysReturns::absent();
    let deadline_duration = Duration::from_millis(200);
    let deadline = Instant::now() + deadline_duration;

    let evidence = poll_readiness(
        &deployment_available_check(),
        deadline,
        &fast_backoff(),
        &fetch,
    )
    .await;

    assert!(!evidence.satisfied);
    assert!(
        fetch.call_count() >= 2,
        "expected multiple poll attempts within a 200ms deadline at a 20-40ms backoff, got {}",
        fetch.call_count()
    );
    assert!(
        evidence.elapsed <= deadline_duration + Duration::from_secs(2),
        "must not overshoot the deadline by much, got {:?}",
        evidence.elapsed
    );
}

#[tokio::test]
async fn poll_does_not_sleep_past_an_already_elapsed_deadline() {
    let fetch = AlwaysReturns::absent();
    // Already in the past.
    let deadline = Instant::now();

    let evidence = poll_readiness(
        &deployment_available_check(),
        deadline,
        &fast_backoff(),
        &fetch,
    )
    .await;

    assert!(!evidence.satisfied);
    assert_eq!(
        fetch.call_count(),
        1,
        "must still attempt once even with an already-expired deadline, but never retry"
    );
    assert!(
        evidence.elapsed < Duration::from_secs(1),
        "must return promptly rather than sleeping, got {:?}",
        evidence.elapsed
    );
}

#[tokio::test]
async fn poll_deadline_failure_carries_last_observed_object_rather_than_losing_it() {
    // A Deployment that is genuinely unhealthy: observed every attempt,
    // but never becomes Available before the deadline.
    let unsatisfying = json!({
        "status": {
            "replicas": 1,
            "unavailableReplicas": 1,
            "conditions": [
                {"type": "Available", "status": "False", "reason": "ProgressDeadlineExceeded"},
            ]
        }
    });
    let fetch = AlwaysReturns::present(unsatisfying.clone());
    let deadline = Instant::now() + Duration::from_millis(150);

    let evidence = poll_readiness(
        &deployment_available_check(),
        deadline,
        &fast_backoff(),
        &fetch,
    )
    .await;

    assert!(!evidence.satisfied);
    assert_eq!(
        evidence.last_observed,
        Some(unsatisfying),
        "a deadline failure must carry the last observed object, not lose it"
    );
    assert_eq!(evidence.check, deployment_available_check());
}

#[tokio::test]
async fn poll_treats_fetch_errors_as_retryable_and_still_succeeds() {
    let satisfying = json!({"status": {"conditions": [{"type": "Complete", "status": "True"}]}});
    let fetch = ScriptedFetch::new(vec![
        Err(ScriptedFetch::fake_error()),
        Err(ScriptedFetch::fake_error()),
        Ok(Some(satisfying.clone())),
    ]);
    let deadline = Instant::now() + Duration::from_secs(30);

    let evidence = poll_readiness(&job_complete_check(), deadline, &fast_backoff(), &fetch).await;

    assert!(evidence.satisfied);
    assert_eq!(evidence.last_observed, Some(satisfying));
    assert_eq!(fetch.call_count(), 3);
}

#[tokio::test]
async fn poll_never_satisfied_when_object_never_appears() {
    // `WebhookConfigurationPresent`: Kyverno creates these at runtime, so
    // a poll that only ever sees "not found" (never installed, or crashed
    // before registering) must report unsatisfied with no observed
    // object at all -- not fabricate one.
    let fetch = AlwaysReturns::absent();
    let deadline = Instant::now() + Duration::from_millis(100);

    let evidence = poll_readiness(
        &webhook_configuration_present_check(),
        deadline,
        &fast_backoff(),
        &fetch,
    )
    .await;

    assert!(!evidence.satisfied);
    assert_eq!(evidence.last_observed, None);
}

// ---------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------

#[test]
fn redact_masks_secret_data_and_string_data_values_but_keeps_keys() {
    let secret = json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {"name": "webhook-ca", "namespace": "kyverno"},
        "type": "kubernetes.io/tls",
        "data": {
            "tls.crt": "bGV0c2VuY3J5cHQ=",
            "tls.key": "c3VwZXJzZWNyZXQ=",
        },
        "stringData": {
            "token": "hunter2",
        },
    });

    let redacted = redact_for_evidence(secret);

    assert_eq!(redacted["kind"], "Secret");
    assert_eq!(redacted["metadata"]["name"], "webhook-ca");
    assert_eq!(redacted["type"], "kubernetes.io/tls");
    assert_eq!(redacted["data"]["tls.crt"], "[REDACTED]");
    assert_eq!(redacted["data"]["tls.key"], "[REDACTED]");
    assert_eq!(redacted["stringData"]["token"], "[REDACTED]");
}

#[test]
fn redact_leaves_non_secret_objects_untouched() {
    let deployment = json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "status": {"conditions": [{"type": "Available", "status": "True"}]},
    });
    let redacted = redact_for_evidence(deployment.clone());
    assert_eq!(redacted, deployment);
}

#[test]
fn redact_leaves_a_secret_with_no_data_fields_untouched() {
    let secret = json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {"name": "empty"},
    });
    let redacted = redact_for_evidence(secret.clone());
    assert_eq!(redacted, secret);
}

#[test]
fn redact_handles_a_secret_looked_up_via_custom_resource_condition() {
    // Nothing in `ReadinessCheck::CustomResourceCondition`'s type
    // prevents a caller from naming `apiVersion: v1, kind: Secret` --
    // it is the one check that can observe an object of genuinely
    // arbitrary kind. Confirm redaction still applies along that path's
    // output shape (a plain `serde_json::Value`, same as every other
    // check).
    let secret = json!({
        "kind": "Secret",
        "data": {"password": "hunter2"},
    });
    let redacted = redact_for_evidence(secret);
    assert_eq!(redacted["data"]["password"], "[REDACTED]");
}

#[test]
fn redact_masks_a_hardcoded_credential_in_a_deployment_env_value() {
    // The vector found in code review: a literal credential in a pod
    // template's `env[].value` is a common real-world anti-pattern, and
    // three of this module's five actual check targets (`Deployment`,
    // `DaemonSet`, `Job`) always carry this shape in full once fetched.
    const SENTINEL: &str = "hunter2-sentinel-must-not-leak";
    let deployment = json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {"name": "kyverno-admission-controller"},
        "spec": {
            "template": {
                "spec": {
                    "containers": [{
                        "name": "kyverno",
                        "env": [
                            {"name": "DB_PASSWORD", "value": SENTINEL},
                            {"name": "LOG_LEVEL", "value": "debug"},
                        ],
                    }],
                },
            },
        },
        "status": {"conditions": [{"type": "Available", "status": "True"}]},
    });

    let redacted = redact_for_evidence(deployment);

    let env = redacted["spec"]["template"]["spec"]["containers"][0]["env"]
        .as_array()
        .expect("env array present");
    assert_eq!(env[0]["name"], "DB_PASSWORD");
    assert_eq!(env[0]["value"], "[REDACTED]");
    assert_eq!(env[1]["name"], "LOG_LEVEL");
    assert_eq!(
        env[1]["value"], "debug",
        "a non-sensitive value must survive untouched"
    );

    let rendered = redacted.to_string();
    assert!(
        !rendered.contains(SENTINEL),
        "the sentinel credential must not appear anywhere in the redacted evidence"
    );
}

#[test]
fn redact_covers_init_containers_as_well_as_containers() {
    const SENTINEL: &str = "init-container-secret-sentinel";
    let job = json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "spec": {
            "template": {
                "spec": {
                    "initContainers": [{
                        "name": "wait-for-db",
                        "env": [{"name": "DB_PASSWORD", "value": SENTINEL}],
                    }],
                    "containers": [{"name": "main", "env": []}],
                },
            },
        },
    });

    let redacted = redact_for_evidence(job);

    assert_eq!(
        redacted["spec"]["template"]["spec"]["initContainers"][0]["env"][0]["value"],
        "[REDACTED]"
    );
    assert!(!redacted.to_string().contains(SENTINEL));
}

#[test]
fn redact_leaves_value_from_references_untouched_even_when_the_name_looks_sensitive() {
    // `valueFrom` (a `secretKeyRef`/`configMapKeyRef`/... reference) is
    // the *safe*, indirect way to wire a Secret into a pod: there is no
    // literal value inline to redact, and the reference itself (which
    // Secret name/key it draws from) is useful, non-sensitive
    // information a diagnostic should keep.
    let deployment = json!({
        "kind": "Deployment",
        "spec": {
            "template": {
                "spec": {
                    "containers": [{
                        "name": "kyverno",
                        "env": [{
                            "name": "DB_PASSWORD",
                            "valueFrom": {
                                "secretKeyRef": {"name": "db-credentials", "key": "password"},
                            },
                        }],
                    }],
                },
            },
        },
    });

    let redacted = redact_for_evidence(deployment.clone());

    assert_eq!(
        redacted, deployment,
        "no literal value exists here to redact"
    );
}

#[test]
fn redact_is_a_no_op_for_objects_with_no_pod_template() {
    // `WebhookConfigurationPresent`'s targets and most custom resources
    // have nothing at `.spec.template.spec` at all -- confirms the pass
    // degrades to a safe no-op rather than panicking on a missing path.
    let webhook = json!({
        "apiVersion": "admissionregistration.k8s.io/v1",
        "kind": "ValidatingWebhookConfiguration",
        "metadata": {"name": "kyverno-policy-validating-webhook-cfg"},
        "webhooks": [{"name": "validate.kyverno.svc"}],
    });
    let redacted = redact_for_evidence(webhook.clone());
    assert_eq!(redacted, webhook);
}
