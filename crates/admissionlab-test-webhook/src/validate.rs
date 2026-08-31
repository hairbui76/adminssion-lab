//! Validating behavior: the deny, delay and controlled-failure half of
//! PRODUCT.md §30's vocabulary, served on `/validate` by the single
//! `ValidatingWebhookConfiguration` this recipe installs
//! (`recipes/test-webhook/manifests/20-webhook-configuration.yaml`).
//!
//! # Why all three live only here, and never on a mutating route
//!
//! The mutating routes ([`crate::mutate`]) deliberately ignore
//! [`crate::behavior::DENY`], [`crate::behavior::DELAY_MS`] and
//! [`crate::behavior::FAIL`] even though they parse them like any other
//! annotation. Concentrating them on the one validating configuration
//! makes a fixture's observed behavior a function of the fixture alone:
//! `delay-ms: "250"` adds exactly 250 milliseconds to the request, not
//! 250 per mutating configuration installed, so adding a third mutating
//! configuration later cannot silently change what an existing latency
//! fixture measures. Global Constraint 7 wants deterministic results;
//! "deterministic" has to survive the manifests changing around the
//! fixture.
//!
//! # `fail` outranks `deny`
//!
//! [`Decision::Fail`] is not "deny with a different message": it is the
//! webhook *not answering at all* — an HTTP 500 that the API server
//! records as a webhook call failure and resolves through the webhook's
//! `failurePolicy`, not through anything in the response body. A
//! webhook that fails never got as far as forming a verdict, so a
//! fixture asking for both gets the failure: any other precedence would
//! have this webhook report a considered denial it never actually
//! reached.
//!
//! # The delay happens before every outcome, including allow
//!
//! [`evaluate`] sleeps first and decides second, on allow as much as on
//! deny. A slow webhook is slow whatever it eventually says, and the
//! per-webhook latency signal Task 3.8 collects (Global Constraint 19)
//! is only meaningful if a fixture can manufacture latency without also
//! manufacturing a rejection.

use std::time::Duration;

use crate::behavior::Behavior;

/// What the validating webhook does with a request, after any delay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Admit the object unchanged.
    Allow,
    /// Answer `allowed: false` with this message — a real admission
    /// verdict the API server surfaces to the client.
    Deny {
        /// The fixture's own [`crate::behavior::DENY`] message.
        message: String,
    },
    /// Answer HTTP 500 instead of an admission response, so the API
    /// server treats this as a webhook call failure and applies the
    /// webhook's `failurePolicy` — see this module's own documentation.
    Fail,
}

/// Applies `behavior`'s delay (if any) and then returns its decision.
///
/// The sleep is `tokio::time::sleep`, so a caller using Tokio's paused
/// clock (`tokio::time::pause`, or `#[tokio::test(start_paused = true)]`)
/// observes the full delay with no wall-clock cost — which is how this
/// module's own tests cover [`crate::behavior::MAX_DELAY_MS`]-scale
/// delays without a 60-second test suite.
pub async fn evaluate(behavior: &Behavior) -> Decision {
    if let Some(delay) = behavior.delay {
        delay_for(delay).await;
    }
    decide(behavior)
}

/// The delay half of [`evaluate`], split out so the decision half stays
/// synchronous and directly testable.
async fn delay_for(delay: Duration) {
    tracing::debug!(?delay, "delaying this admission response");
    tokio::time::sleep(delay).await;
}

/// The decision half of [`evaluate`]: pure, synchronous, and total —
/// every [`Behavior`] maps to exactly one [`Decision`].
#[must_use]
pub fn decide(behavior: &Behavior) -> Decision {
    if behavior.fail {
        return Decision::Fail;
    }
    match &behavior.deny {
        Some(message) => Decision::Deny {
            message: message.clone(),
        },
        None => Decision::Allow,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::time::Instant;

    use super::{Decision, decide, evaluate};
    use crate::behavior::{Behavior, parse};

    fn behavior(annotations: &Value) -> Behavior {
        parse(&json!({"metadata": {"annotations": annotations}}))
            .expect("test objects carry valid annotations")
    }

    #[test]
    fn an_object_asking_for_nothing_is_allowed() {
        assert_eq!(decide(&Behavior::default()), Decision::Allow);
    }

    #[test]
    fn deny_carries_the_fixtures_own_message() {
        let decision = decide(&behavior(
            &json!({"test.admissionlab.io/deny": "no privileged pods"}),
        ));
        assert_eq!(
            decision,
            Decision::Deny {
                message: "no privileged pods".to_owned()
            }
        );
    }

    #[test]
    fn fail_outranks_deny() {
        let decision = decide(&behavior(&json!({
            "test.admissionlab.io/deny": "would have been denied",
            "test.admissionlab.io/fail": "true",
        })));
        assert_eq!(
            decision,
            Decision::Fail,
            "a webhook that fails never reached a verdict; see this module's documentation"
        );
    }

    /// Tokio's paused clock: the sleep is observed in full (the elapsed
    /// `tokio::time::Instant` really does advance by the requested
    /// delay) while the test itself costs no wall-clock time, so this
    /// covers the largest delay the vocabulary permits
    /// (`behavior::MAX_DELAY_MS`, a full minute) without a minute-long
    /// test.
    #[tokio::test(start_paused = true)]
    async fn the_delay_is_applied_before_the_decision() {
        let started = Instant::now();
        let decision = evaluate(&behavior(
            &json!({"test.admissionlab.io/delay-ms": "60000"}),
        ))
        .await;
        assert_eq!(started.elapsed(), Duration::from_millis(60_000));
        assert_eq!(
            decision,
            Decision::Allow,
            "a delay on its own must not change the verdict"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_delay_and_a_deny_compose() {
        let started = Instant::now();
        let decision = evaluate(&behavior(&json!({
            "test.admissionlab.io/delay-ms": "250",
            "test.admissionlab.io/deny": "slow and denied",
        })))
        .await;
        assert_eq!(started.elapsed(), Duration::from_millis(250));
        assert_eq!(
            decision,
            Decision::Deny {
                message: "slow and denied".to_owned()
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn no_delay_annotation_means_no_sleep_at_all() {
        let started = Instant::now();
        let _ = evaluate(&Behavior::default()).await;
        assert_eq!(
            started.elapsed(),
            Duration::ZERO,
            "an absent delay must not become a zero-length sleep that still yields"
        );
    }
}
