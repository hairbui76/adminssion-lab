//! Behavioral tests for `admissionlab_installer::stack::install_stack`
//! (Task 2.6): the orchestrator that installs one side's whole ordered
//! component stack — calling [`ComponentInstaller::install`] for each
//! [`ResolvedComponent`] in turn, then awaiting every one of that
//! component's [`ReadinessCheck`]s before moving on to the next
//! component.
//!
//! No test here creates a cluster or contacts a real Kubernetes API
//! server: every test drives [`install_stack`] against [`Fake`], a
//! whole-trait test double implementing both [`ComponentInstaller`] and
//! [`ReadinessProbe`] that never spawns a process or performs network
//! I/O — mirroring how `tests/helm_unit.rs`/`tests/manifests_unit.rs`
//! drive their installers through a fake `ProcessRunner` rather than a
//! real `helm`/`kubectl`. Real stack installation against a real
//! cluster belongs to the Phase 2 exit gate, not this suite.
//!
//! [`Fake`] deliberately implements *both* traits on one type, sharing
//! one event log between them: `install_stack` takes `installer` and
//! `readiness` as two independent trait-object parameters, but nothing
//! stops a caller from handing the same underlying value as both,
//! borrowed twice. Two independently-logging fakes could only prove
//! "installer was called N times" and "readiness was called M times"
//! separately, never that they *interleaved* correctly — exactly the
//! property Task 2.6's Kyverno constraint (a component's webhooks may
//! not exist until well after its `helm install` returns) depends on.
//!
//! Covers Task 2.6's requirements:
//! - Step 1 (component order preserved exactly) —
//!   `install_stack_preserves_component_order_even_against_a_sort_defeating_order`.
//! - Step 2 (stop on first failure, diagnosable) —
//!   `install_stack_stops_at_the_first_installation_failure_and_never_attempts_later_components`,
//!   `install_stack_reports_a_readiness_error_with_the_failing_components_name_and_stops`,
//!   `install_stack_treats_an_unsatisfied_readiness_deadline_as_a_stack_failure_and_stops`.
//! - Step 3 (baseline/candidate install concurrently; each side's own
//!   order stays deterministic) —
//!   `install_stack_allows_two_sides_to_install_concurrently_while_each_sides_order_stays_deterministic`.
//! - Readiness awaited per component, not batched after every install —
//!   `install_stack_awaits_a_components_readiness_before_installing_the_next_component`.
//! - `component_timeout`: one shared deadline per component, covering
//!   install-plus-readiness together, not reset per check —
//!   `install_stack_shares_one_deadline_across_a_components_multiple_readiness_checks`
//!   — and computed *before* `install` is called, not fresh once it
//!   returns —
//!   `install_stack_computes_the_deadline_before_install_not_after_it_returns`.
//! - Installer dispatch (`lib.rs`/`helm.rs`/`manifests.rs`: two concrete
//!   installers, `InstallMethod` has two variants) —
//!   `composite_installer_dispatches_helm_and_manifests_components_to_their_own_installer`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use admissionlab_core::{ClusterHandle, ClusterSpec, Side};
use admissionlab_installer::stack::{CompositeInstaller, install_stack};
use admissionlab_installer::{
    ComponentInstaller, InstallError, InstallRecord, ReadinessEvidence, ReadinessProbe,
};
use admissionlab_spec::component::HelmInstallSpec;
use admissionlab_spec::{InstallMethod, ManifestInstallSpec, ReadinessCheck, ResolvedComponent};
use async_trait::async_trait;

// ---------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------

/// A [`ClusterHandle`] for `side`, distinctive enough that a test can
/// tell baseline and candidate calls apart.
fn cluster_handle(side: Side) -> ClusterHandle {
    ClusterHandle {
        spec: ClusterSpec {
            side,
            name: format!("adlab-{}-teststack", side.as_str()),
            kubernetes_version: "1.36.4".to_owned(),
            node_image: "kindest/node:v1.36.4@sha256:0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
            images: Vec::new(),
        },
        kubeconfig: PathBuf::from(format!("/fake/{}/kubeconfig", side.as_str())),
        audit_log: PathBuf::from(format!("/fake/{}/audit.log", side.as_str())),
    }
}

/// A minimal, arbitrary resolved component named `name`, waiting on
/// `readiness` (often empty). The install method's own content never
/// matters to [`Fake`] — only [`ResolvedComponent::name`] does — so
/// every test other than the dispatch test below uses `Manifests` with
/// a single placeholder path.
fn component_named(name: &str, readiness: Vec<ReadinessCheck>) -> ResolvedComponent {
    ResolvedComponent {
        name: name.to_owned(),
        version: "1.0.0".to_owned(),
        install: InstallMethod::Manifests(ManifestInstallSpec {
            paths: vec![PathBuf::from(format!("/fake/{name}.yaml"))],
        }),
        readiness,
        recipe_normalize_rules: Vec::new(),
        capabilities: BTreeSet::new(),
    }
}

/// Like [`component_named`], but resolved to install via Helm — used
/// only by the dispatch test.
fn helm_component_named(name: &str) -> ResolvedComponent {
    ResolvedComponent {
        name: name.to_owned(),
        version: "1.0.0".to_owned(),
        install: InstallMethod::Helm(HelmInstallSpec {
            repo_name: "r".to_owned(),
            repo_url: "https://example.invalid/charts".to_owned(),
            chart: "r/chart".to_owned(),
            version: "1.0.0".to_owned(),
            release_name: name.to_owned(),
            namespace: name.to_owned(),
            values_files: Vec::new(),
            set_values: BTreeMap::new(),
        }),
        readiness: Vec::new(),
        recipe_normalize_rules: Vec::new(),
        capabilities: BTreeSet::new(),
    }
}

/// A [`ReadinessCheck::WebhookConfigurationPresent`] labeled `label` —
/// the one check variant every test here uses, since `install_stack`
/// treats every variant opaquely (see `readiness.rs`'s own `evaluate`
/// for the variant-specific logic, which is Task 2.4's tested concern,
/// not this one's).
fn check_named(label: &str) -> ReadinessCheck {
    ReadinessCheck::WebhookConfigurationPresent {
        name: label.to_owned(),
    }
}

/// A minimal, successful [`InstallRecord`] for `component`.
fn fake_record(component: &str) -> InstallRecord {
    InstallRecord {
        component: component.to_owned(),
        method: "fake".to_owned(),
        resolved_version: "1.0.0".to_owned(),
        started_at: SystemTime::now(),
        elapsed: Duration::ZERO,
        diagnostics: Vec::new(),
    }
}

fn fake_evidence(check: &ReadinessCheck, satisfied: bool) -> ReadinessEvidence {
    ReadinessEvidence {
        check: check.clone(),
        satisfied,
        last_observed: None,
        elapsed: Duration::ZERO,
    }
}

/// How [`Fake`]'s [`ComponentInstaller::install`] behaves for one
/// component, keyed by [`ResolvedComponent::name`].
#[derive(Clone)]
enum InstallPlan {
    Succeed,
    /// Sleeps for the given duration, then succeeds — used to force real
    /// interleaving between two concurrent [`install_stack`] calls.
    SucceedAfter(Duration),
    Fail,
}

/// How [`Fake`]'s [`ReadinessProbe::wait`] behaves for one labeled check
/// (see [`check_named`]).
#[derive(Clone)]
enum ReadinessPlan {
    /// Satisfied on the first (only) attempt.
    Satisfied,
    /// Never satisfied — returns `Ok(ReadinessEvidence { satisfied:
    /// false, .. })` immediately, standing in for a real
    /// `KubeReadinessProbe` that polled all the way to its deadline
    /// without ever observing a satisfied check (Task 2.4's own
    /// `poll_readiness` already covers that backoff/deadline loop; this
    /// fake stands in for the whole `wait` call, not that loop).
    NeverSatisfied,
    /// `wait` itself fails, mirroring
    /// [`InstallError::ReadinessUnavailable`].
    Errors,
    /// Sleeps for the given duration, then reports satisfied — used to
    /// prove a component's later checks share one already-ticking
    /// deadline rather than each getting a fresh `component_timeout`
    /// window.
    SleepThenSatisfied(Duration),
}

/// A single, whole-trait test double implementing both
/// [`ComponentInstaller`] and [`ReadinessProbe`] — see this file's
/// module documentation for why one shared type, not two independent
/// ones. Every call (install or wait) appends one labeled entry to one
/// shared `events` log, so a test can assert the exact interleaved
/// sequence, not just how many times each trait method ran.
struct Fake {
    install_plans: BTreeMap<String, InstallPlan>,
    readiness_plans: BTreeMap<String, ReadinessPlan>,
    events: Mutex<Vec<String>>,
    remaining_at_call: Mutex<BTreeMap<String, Duration>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            install_plans: BTreeMap::new(),
            readiness_plans: BTreeMap::new(),
            events: Mutex::new(Vec::new()),
            remaining_at_call: Mutex::new(BTreeMap::new()),
        }
    }

    /// Configures `install` to fail for the component named `name`.
    fn failing_install(mut self, name: &str) -> Self {
        self.install_plans
            .insert(name.to_owned(), InstallPlan::Fail);
        self
    }

    /// Configures `install` to sleep for `delay` before succeeding, for
    /// the component named `name`.
    fn delayed_install(mut self, name: &str, delay: Duration) -> Self {
        self.install_plans
            .insert(name.to_owned(), InstallPlan::SucceedAfter(delay));
        self
    }

    /// Configures `wait` for the check labeled `label` (see
    /// [`check_named`]).
    fn readiness(mut self, label: &str, plan: ReadinessPlan) -> Self {
        self.readiness_plans.insert(label.to_owned(), plan);
        self
    }

    /// The full, interleaved event log: `"install:<side>:<name>"` for
    /// every [`ComponentInstaller::install`] call and
    /// `"ready:<label>"` for every [`ReadinessProbe::wait`] call, in
    /// call order.
    fn events(&self) -> Vec<String> {
        self.events.lock().expect("events mutex poisoned").clone()
    }

    /// Just the installed component names for `side`, in call order —
    /// for tests that only care about one side's own sequence.
    fn installed_for(&self, side: Side) -> Vec<String> {
        let prefix = format!("install:{}:", side.as_str());
        self.events()
            .into_iter()
            .filter_map(|event| event.strip_prefix(&prefix).map(str::to_owned))
            .collect()
    }

    /// How much of its `deadline` argument remained, at the moment
    /// `wait` was called, for the check labeled `label`.
    fn remaining_at_call(&self, label: &str) -> Duration {
        *self
            .remaining_at_call
            .lock()
            .expect("remaining_at_call mutex poisoned")
            .get(label)
            .unwrap_or_else(|| panic!("wait was never called for check {label:?}"))
    }
}

#[async_trait]
impl ComponentInstaller for Fake {
    async fn install(
        &self,
        cluster: &ClusterHandle,
        component: &ResolvedComponent,
    ) -> Result<InstallRecord, InstallError> {
        self.events
            .lock()
            .expect("events mutex poisoned")
            .push(format!(
                "install:{}:{}",
                cluster.spec.side.as_str(),
                component.name
            ));

        match self
            .install_plans
            .get(&component.name)
            .cloned()
            .unwrap_or(InstallPlan::Succeed)
        {
            InstallPlan::Succeed => Ok(fake_record(&component.name)),
            InstallPlan::SucceedAfter(delay) => {
                tokio::time::sleep(delay).await;
                Ok(fake_record(&component.name))
            }
            InstallPlan::Fail => Err(InstallError::UnsupportedMethod {
                component: component.name.clone(),
                expected: "fake-expected",
                actual: "fake-actual",
            }),
        }
    }
}

#[async_trait]
impl ReadinessProbe for Fake {
    async fn wait(
        &self,
        _cluster: &ClusterHandle,
        check: &ReadinessCheck,
        deadline: Instant,
    ) -> Result<ReadinessEvidence, InstallError> {
        let label = match check {
            ReadinessCheck::WebhookConfigurationPresent { name } => name.clone(),
            other => format!("{other:?}"),
        };
        self.events
            .lock()
            .expect("events mutex poisoned")
            .push(format!("ready:{label}"));
        self.remaining_at_call
            .lock()
            .expect("remaining_at_call mutex poisoned")
            .insert(
                label.clone(),
                deadline.saturating_duration_since(Instant::now()),
            );

        match self
            .readiness_plans
            .get(&label)
            .cloned()
            .unwrap_or(ReadinessPlan::Satisfied)
        {
            ReadinessPlan::Satisfied => Ok(fake_evidence(check, true)),
            ReadinessPlan::NeverSatisfied => Ok(fake_evidence(check, false)),
            ReadinessPlan::Errors => Err(InstallError::ReadinessUnavailable {
                check: Box::new(check.clone()),
                reason: "fake: forced readiness failure".to_owned(),
            }),
            ReadinessPlan::SleepThenSatisfied(delay) => {
                tokio::time::sleep(delay).await;
                Ok(fake_evidence(check, true))
            }
        }
    }
}

// ---------------------------------------------------------------------
// Step 1: component order is preserved exactly.
// ---------------------------------------------------------------------

#[tokio::test]
async fn install_stack_preserves_component_order_even_against_a_sort_defeating_order() {
    // "zeta", "alpha", "mu" is an order any lexicographic (or most
    // other) sort would reorder to "alpha", "mu", "zeta" — this test
    // would fail if `install_stack` ever sorted `components` before
    // installing them.
    let components = vec![
        component_named("zeta", Vec::new()),
        component_named("alpha", Vec::new()),
        component_named("mu", Vec::new()),
    ];
    let cluster = cluster_handle(Side::Baseline);
    let fake = Fake::new();

    let result = install_stack(&cluster, &components, &fake, &fake, Duration::from_secs(5))
        .await
        .expect("every component succeeds");

    let expected = vec!["zeta".to_owned(), "alpha".to_owned(), "mu".to_owned()];
    assert_eq!(
        fake.installed_for(Side::Baseline),
        expected,
        "install must be called in exactly the given order, never sorted"
    );
    assert_eq!(
        result
            .components
            .iter()
            .map(|record| record.component.clone())
            .collect::<Vec<_>>(),
        expected,
        "InstalledStack::components must preserve the same order"
    );
    assert_eq!(result.side, Side::Baseline);
}

// ---------------------------------------------------------------------
// Step 2: stop on first failure, diagnosable.
// ---------------------------------------------------------------------

#[tokio::test]
async fn install_stack_stops_at_the_first_installation_failure_and_never_attempts_later_components()
{
    let components = vec![
        component_named("c1", Vec::new()),
        component_named("c2", Vec::new()),
        component_named("c3", Vec::new()),
        component_named("c4", Vec::new()),
    ];
    let cluster = cluster_handle(Side::Candidate);
    let fake = Fake::new().failing_install("c2");

    let result = install_stack(&cluster, &components, &fake, &fake, Duration::from_secs(5)).await;

    assert!(result.is_err(), "c2 failing must fail the whole stack");
    assert_eq!(
        fake.installed_for(Side::Candidate),
        vec!["c1".to_owned(), "c2".to_owned()],
        "c3 and c4 must never be attempted once c2 fails"
    );
    match result.unwrap_err() {
        InstallError::UnsupportedMethod { component, .. } => {
            assert_eq!(
                component, "c2",
                "the error must name the component that actually failed"
            );
        }
        other => panic!("expected InstallError::UnsupportedMethod, got {other:?}"),
    }
}

#[tokio::test]
async fn install_stack_reports_a_readiness_error_with_the_failing_components_name_and_stops() {
    let components = vec![
        component_named("c1", vec![check_named("c1-check")]),
        component_named("c2", Vec::new()),
    ];
    let cluster = cluster_handle(Side::Baseline);
    let fake = Fake::new().readiness("c1-check", ReadinessPlan::Errors);

    let result = install_stack(&cluster, &components, &fake, &fake, Duration::from_secs(5)).await;

    match result {
        Err(InstallError::ComponentReadinessUnavailable { component, source }) => {
            assert_eq!(component, "c1");
            assert!(
                matches!(*source, InstallError::ReadinessUnavailable { .. }),
                "must carry the underlying readiness failure, got {source:?}"
            );
        }
        other => panic!("expected Err(ComponentReadinessUnavailable), got {other:?}"),
    }
    assert_eq!(
        fake.installed_for(Side::Baseline),
        vec!["c1".to_owned()],
        "c2 must never be attempted once c1's readiness cannot be confirmed"
    );
}

#[tokio::test]
async fn install_stack_treats_an_unsatisfied_readiness_deadline_as_a_stack_failure_and_stops() {
    let components = vec![
        component_named("c1", vec![check_named("c1-check")]),
        component_named("c2", Vec::new()),
    ];
    let cluster = cluster_handle(Side::Baseline);
    let fake = Fake::new().readiness("c1-check", ReadinessPlan::NeverSatisfied);

    let result = install_stack(
        &cluster,
        &components,
        &fake,
        &fake,
        Duration::from_millis(50),
    )
    .await;

    match result {
        Err(InstallError::ComponentNotReady {
            component,
            evidence,
        }) => {
            assert_eq!(component, "c1");
            assert!(!evidence.satisfied);
        }
        other => panic!("expected Err(ComponentNotReady), got {other:?}"),
    }
    assert_eq!(
        fake.installed_for(Side::Baseline),
        vec!["c1".to_owned()],
        "c2 must never be attempted once c1 never becomes ready"
    );
}

// ---------------------------------------------------------------------
// Readiness is awaited per component, not batched after every install.
// ---------------------------------------------------------------------

#[tokio::test]
async fn install_stack_awaits_a_components_readiness_before_installing_the_next_component() {
    let components = vec![
        component_named("c1", vec![check_named("c1-check")]),
        component_named("c2", vec![check_named("c2-check")]),
    ];
    let cluster = cluster_handle(Side::Baseline);
    let fake = Fake::new();

    install_stack(&cluster, &components, &fake, &fake, Duration::from_secs(5))
        .await
        .expect("everything succeeds");

    // If `install_stack` installed every component first and only
    // awaited readiness afterward, this would read
    // `["install:baseline:c1", "install:baseline:c2", "ready:c1-check",
    // "ready:c2-check"]` instead — this asserts the two interleave: c1's
    // readiness is awaited before c2 is ever installed.
    assert_eq!(
        fake.events(),
        vec![
            "install:baseline:c1".to_owned(),
            "ready:c1-check".to_owned(),
            "install:baseline:c2".to_owned(),
            "ready:c2-check".to_owned(),
        ]
    );
}

// ---------------------------------------------------------------------
// component_timeout: one shared deadline per component.
// ---------------------------------------------------------------------

#[tokio::test]
async fn install_stack_shares_one_deadline_across_a_components_multiple_readiness_checks() {
    let components = vec![component_named(
        "c1",
        vec![check_named("check-a"), check_named("check-b")],
    )];
    let cluster = cluster_handle(Side::Baseline);
    let component_timeout = Duration::from_millis(400);
    let fake = Fake::new().readiness(
        "check-a",
        ReadinessPlan::SleepThenSatisfied(Duration::from_millis(250)),
    );

    install_stack(&cluster, &components, &fake, &fake, component_timeout)
        .await
        .expect("both checks eventually report satisfied");

    let remaining_for_b = fake.remaining_at_call("check-b");
    assert!(
        remaining_for_b < Duration::from_millis(250),
        "check-b must see a deadline that already reflects check-a's ~250ms sleep, not a fresh \
         400ms window; got {remaining_for_b:?} remaining"
    );
}

#[tokio::test]
async fn install_stack_computes_the_deadline_before_install_not_after_it_returns() {
    // The other half of the "one shared deadline" property: not only
    // must it stay fixed across a component's own checks (proved above),
    // it must be *computed* before `install` is even called, not fresh
    // once `install` returns. A slow install eating most of a tight
    // budget must leave correspondingly little for the readiness check
    // that follows it -- if the deadline were instead computed only
    // after `install` returned, this component's check would see nearly
    // the whole, untouched `component_timeout` remaining regardless of
    // how long `install` took.
    let components = vec![component_named("c1", vec![check_named("c1-check")])];
    let cluster = cluster_handle(Side::Baseline);
    let component_timeout = Duration::from_millis(150);
    let fake = Fake::new().delayed_install("c1", Duration::from_millis(120));

    install_stack(&cluster, &components, &fake, &fake, component_timeout)
        .await
        .expect(
            "install succeeds (within its own installer-internal bound) and the readiness \
                 check is satisfied on its first attempt",
        );

    let remaining = fake.remaining_at_call("c1-check");
    assert!(
        remaining < Duration::from_millis(70),
        "c1's 120ms install against a 150ms component_timeout must leave well under the full \
         150ms remaining for its readiness check (expected roughly 30ms) -- a deadline computed \
         only after install() returned would instead show close to the full 150ms here; got \
         {remaining:?} remaining"
    );
}

// ---------------------------------------------------------------------
// Edge case: an empty stack trivially succeeds.
// ---------------------------------------------------------------------

#[tokio::test]
async fn install_stack_with_no_components_succeeds_with_an_empty_result() {
    let cluster = cluster_handle(Side::Baseline);
    let fake = Fake::new();

    let result = install_stack(&cluster, &[], &fake, &fake, Duration::from_secs(5))
        .await
        .expect("an empty stack trivially succeeds");

    assert_eq!(result.side, Side::Baseline);
    assert!(result.components.is_empty());
    assert!(
        fake.events().is_empty(),
        "nothing should be attempted for an empty component list"
    );
}

// ---------------------------------------------------------------------
// Step 3: baseline and candidate install concurrently; each side's own
// order stays deterministic.
// ---------------------------------------------------------------------

#[tokio::test]
async fn install_stack_allows_two_sides_to_install_concurrently_while_each_sides_order_stays_deterministic()
 {
    let baseline_components = vec![
        component_named("b1", Vec::new()),
        component_named("b2", Vec::new()),
    ];
    let candidate_components = vec![
        component_named("c1", Vec::new()),
        component_named("c2", Vec::new()),
    ];
    let baseline_cluster = cluster_handle(Side::Baseline);
    let candidate_cluster = cluster_handle(Side::Candidate);
    // b1 sleeps long enough that, on this single-threaded test runtime,
    // candidate's whole (instant) stack can run to completion while
    // baseline's first component is still "installing" — proving
    // install_stack does not serialize the two sides against each
    // other.
    let fake = Fake::new().delayed_install("b1", Duration::from_millis(60));

    let (baseline_result, candidate_result) = tokio::join!(
        install_stack(
            &baseline_cluster,
            &baseline_components,
            &fake,
            &fake,
            Duration::from_secs(5)
        ),
        install_stack(
            &candidate_cluster,
            &candidate_components,
            &fake,
            &fake,
            Duration::from_secs(5)
        ),
    );

    baseline_result.expect("baseline succeeds");
    candidate_result.expect("candidate succeeds");

    assert_eq!(
        fake.installed_for(Side::Baseline),
        vec!["b1".to_owned(), "b2".to_owned()]
    );
    assert_eq!(
        fake.installed_for(Side::Candidate),
        vec!["c1".to_owned(), "c2".to_owned()]
    );

    let events = fake.events();
    let index_of = |needle: &str| {
        events
            .iter()
            .position(|event| event == needle)
            .unwrap_or_else(|| panic!("{needle:?} missing from events: {events:?}"))
    };
    assert!(
        index_of("install:candidate:c1") < index_of("install:baseline:b2"),
        "candidate's stack must make progress while baseline's slow first component is still \
         installing, not wait for the whole baseline side first; events: {events:?}"
    );
    assert!(
        index_of("install:candidate:c2") < index_of("install:baseline:b2"),
        "events: {events:?}"
    );
}

// ---------------------------------------------------------------------
// Installer dispatch: InstallMethod has two variants, and there are two
// concrete installers (HelmInstaller, ManifestsInstaller); CompositeInstaller
// routes each component to whichever matches its own resolved method.
// ---------------------------------------------------------------------

#[tokio::test]
async fn composite_installer_dispatches_helm_and_manifests_components_to_their_own_installer() {
    let helm_fake = Arc::new(Fake::new());
    let manifests_fake = Arc::new(Fake::new());
    let composite = CompositeInstaller::new(
        Arc::clone(&helm_fake) as Arc<dyn ComponentInstaller>,
        Arc::clone(&manifests_fake) as Arc<dyn ComponentInstaller>,
    );
    let cluster = cluster_handle(Side::Baseline);
    let helm_component = helm_component_named("helm-one");
    let manifests_component = component_named("manifests-one", Vec::new());

    composite
        .install(&cluster, &helm_component)
        .await
        .expect("helm component succeeds");
    composite
        .install(&cluster, &manifests_component)
        .await
        .expect("manifests component succeeds");

    assert_eq!(
        helm_fake.installed_for(Side::Baseline),
        vec!["helm-one".to_owned()],
        "a Helm-method component must be dispatched to the Helm installer, and only it"
    );
    assert_eq!(
        manifests_fake.installed_for(Side::Baseline),
        vec!["manifests-one".to_owned()],
        "a Manifests-method component must be dispatched to the manifests installer, and only it"
    );
}
