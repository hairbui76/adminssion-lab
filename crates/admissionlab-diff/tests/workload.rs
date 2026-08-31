//! Task 4.5 workload-mutation classification tests.
//!
//! Three groups of test matter here, and only the first is the obvious
//! one:
//!
//! - **The claims.** A sidecar appeared, an init container stopped being
//!   injected, an image moved. The golden case pins the exact list, in
//!   order, for a realistic injection regression.
//! - **The silences.** A change to a field nobody modeled must produce
//!   *no* semantic change while remaining fully visible in the raw diff,
//!   and a reordered container array must produce none at all. A false
//!   category is worse than a missing one: it reaches
//!   `expectations.yaml` files users grade themselves against.
//! - **The redaction.** No test here may ever find an environment
//!   literal inside a `SemanticChange`, whatever else it asserts.
//!
//! The golden files live at the workspace root under
//! `testdata/golden/semantic-workloads/`, reached the way every other
//! cross-crate corpus in this project is. They are hand-written
//! *representative* objects, not recorded captures: the pair encodes the
//! canonical Istio-style sidecar injection (a proxy container, its
//! `emptyDir` volume and mount) together with an init container the
//! candidate stack no longer injects, which is exactly the regression
//! shape Task 4.5 Step 6 asks for. Recording a real pair needs a live
//! cluster; what is pinned here is the pure classification.

use std::path::{Path, PathBuf};

use admissionlab_core::FixtureId;
use admissionlab_diff::{
    SemanticChange, SemanticChangeKind, UNATTRIBUTED_FIXTURE, diff_workload_objects,
    raw_object_diff,
};
use admissionlab_normalize::{NormalizationEvidence, NormalizedObject};
use serde_json::{Value, json};

// ---------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------

/// Wraps a value as a normalized object with empty evidence.
///
/// These tests compare *already normalized* objects, so nothing was
/// applied and nothing was warned about; the evidence record is not what
/// is under test here.
fn normalized(value: Value) -> NormalizedObject {
    NormalizedObject {
        value,
        evidence: NormalizationEvidence {
            applied_rules: Vec::new(),
            warnings: Vec::new(),
        },
    }
}

fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/golden/semantic-workloads")
}

fn read_golden(name: &str) -> Value {
    let path = testdata_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// Renders a JSON value canonically, so an assertion failure prints a
/// readable diff and object key order never decides the outcome.
fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("serialize JSON value")
}

/// A minimal pod with one container named `app`.
fn pod(container: &Value) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": "app", "namespace": "default"},
        "spec": {"containers": [container]}
    })
}

// ---------------------------------------------------------------------
// Step 6: the golden regression
// ---------------------------------------------------------------------

/// Fails if the canonical sidecar-injection-plus-init-container-removal
/// pair stops producing exactly the checked-in change list, in order --
/// the single most important assertion in this file, and the one that
/// would catch a reordering, a renamed subject, a shifted `object_path`,
/// or a newly leaked environment literal all at once.
#[test]
fn sidecar_injection_regression_matches_golden() {
    let baseline = normalized(read_golden("sidecar-injection.baseline.json"));
    let candidate = normalized(read_golden("sidecar-injection.candidate.json"));
    let fixture_id = FixtureId::parse("sidecar-injection-pod-checkout-0").unwrap();

    let changes: Vec<SemanticChange> = diff_workload_objects(&baseline, &candidate)
        .into_iter()
        .map(|change| change.attributed_to(&fixture_id))
        .collect();

    let actual = serde_json::to_value(&changes).expect("serialize changes");
    assert_eq!(
        pretty(&actual),
        pretty(&read_golden("sidecar-injection.expected.json")),
        "classification drifted from testdata/golden/semantic-workloads/sidecar-injection.expected.json"
    );
}

/// Fails if the injected proxy's bearer-token-shaped environment literal
/// reaches a report-ready field anywhere in the golden case's output.
///
/// The golden file above already pins this by equality; this test states
/// the property directly, so a future edit that regenerates the golden
/// cannot quietly bless a leak.
#[test]
fn golden_output_contains_no_environment_literal() {
    let baseline = normalized(read_golden("sidecar-injection.baseline.json"));
    let candidate = normalized(read_golden("sidecar-injection.candidate.json"));

    let changes = diff_workload_objects(&baseline, &candidate);
    let rendered = serde_json::to_string(&changes).expect("serialize changes");

    for literal in [
        "s3cr3t-checkout-password",
        "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.injected",
    ] {
        assert!(
            !rendered.contains(literal),
            "environment literal {literal} reached a report-ready field: {rendered}"
        );
    }
    // The *reference* a `valueFrom` names is not a value, and is exactly
    // what a reader needs when a webhook rewires where a value comes
    // from, so it must still be there.
    assert!(
        rendered.contains("checkout-db"),
        "the secretKeyRef the baseline read from must stay visible: {rendered}"
    );
}

// ---------------------------------------------------------------------
// Steps 1 and 2: containers, init containers, images, volumes, mounts
// ---------------------------------------------------------------------

/// Fails if a container that exists only in the candidate is not
/// reported as added, keyed by its name.
#[test]
fn container_only_in_candidate_is_added() {
    let baseline = normalized(pod(&json!({"name": "app", "image": "app:1"})));
    let candidate = normalized(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": "app"},
        "spec": {"containers": [
            {"name": "app", "image": "app:1"},
            {"name": "proxy", "image": "proxy:2"}
        ]}
    }));

    let changes = diff_workload_objects(&baseline, &candidate);

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(changes[0].kind, SemanticChangeKind::ContainerAdded);
    assert_eq!(changes[0].subject.as_deref(), Some("proxy"));
    assert_eq!(
        changes[0].object_path.as_deref(),
        Some("/spec/containers/1")
    );
    assert_eq!(
        changes[0].baseline, None,
        "an added container has no baseline value"
    );
}

/// Fails if init containers are classified with the container kinds, or
/// if a removed one is not reported against the baseline's own path.
#[test]
fn init_container_only_in_baseline_is_removed() {
    let baseline = normalized(json!({
        "kind": "Pod",
        "spec": {
            "initContainers": [{"name": "setup", "image": "setup:1"}],
            "containers": [{"name": "app", "image": "app:1"}]
        }
    }));
    let candidate = normalized(pod(&json!({"name": "app", "image": "app:1"})));

    let changes = diff_workload_objects(&baseline, &candidate);

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(changes[0].kind, SemanticChangeKind::InitContainerRemoved);
    assert_eq!(changes[0].subject.as_deref(), Some("setup"));
    assert_eq!(
        changes[0].object_path.as_deref(),
        Some("/spec/initContainers/0"),
        "a removal must address the object that still has the element"
    );
}

/// Fails if a container's image change is missed, misattributed, or
/// keyed by array position instead of by name.
///
/// The two sides list the same two containers in opposite order, so an
/// index-keyed comparison would report two image changes on the wrong
/// containers instead of one on the right one.
#[test]
fn image_change_is_keyed_by_container_name_not_index() {
    let baseline = normalized(json!({
        "kind": "Pod",
        "spec": {"containers": [
            {"name": "app", "image": "app:1"},
            {"name": "proxy", "image": "proxy:2"}
        ]}
    }));
    let candidate = normalized(json!({
        "kind": "Pod",
        "spec": {"containers": [
            {"name": "proxy", "image": "proxy:2"},
            {"name": "app", "image": "app:9"}
        ]}
    }));

    let changes = diff_workload_objects(&baseline, &candidate);

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(changes[0].kind, SemanticChangeKind::ImageChanged);
    assert_eq!(changes[0].subject.as_deref(), Some("app"));
    assert_eq!(
        changes[0].object_path.as_deref(),
        Some("/spec/containers/1/image"),
        "the path must address the candidate object, where `app` sits at index 1"
    );
    assert_eq!(changes[0].baseline, Some(json!("app:1")));
    assert_eq!(changes[0].candidate, Some(json!("app:9")));
}

/// Fails if a pure reordering of the container array is reported as any
/// change at all.
#[test]
fn reordering_containers_alone_is_not_a_change() {
    let baseline = normalized(json!({
        "kind": "Pod",
        "spec": {"containers": [{"name": "app", "image": "app:1"}, {"name": "proxy", "image": "proxy:2"}]}
    }));
    let candidate = normalized(json!({
        "kind": "Pod",
        "spec": {"containers": [{"name": "proxy", "image": "proxy:2"}, {"name": "app", "image": "app:1"}]}
    }));

    assert_eq!(
        diff_workload_objects(&baseline, &candidate),
        Vec::new(),
        "the same containers in a different order are the same pod"
    );
}

/// Fails if volume mounts are keyed by anything other than the mount
/// path, or if a mount whose backing volume changed at the same path is
/// missed.
#[test]
fn volume_mounts_are_keyed_by_mount_path() {
    let baseline = normalized(pod(&json!({
        "name": "app",
        "image": "app:1",
        "volumeMounts": [{"name": "config", "mountPath": "/etc/config"}]
    })));
    let candidate = normalized(pod(&json!({
        "name": "app",
        "image": "app:1",
        "volumeMounts": [{"name": "overrides", "mountPath": "/etc/config"}]
    })));

    let changes = diff_workload_objects(&baseline, &candidate);

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(changes[0].kind, SemanticChangeKind::VolumeMountChanged);
    assert_eq!(changes[0].subject.as_deref(), Some("app"));
    assert_eq!(
        changes[0].object_path.as_deref(),
        Some("/spec/containers/0/volumeMounts/0")
    );
}

/// Fails if a volume added on the candidate side is missed, or if a
/// volume whose *definition* changed acquires a semantic category the
/// frozen vocabulary does not have.
#[test]
fn volumes_report_addition_but_never_a_redefinition() {
    let baseline = normalized(json!({
        "kind": "Pod",
        "spec": {
            "containers": [{"name": "app", "image": "app:1"}],
            "volumes": [{"name": "config", "configMap": {"name": "old"}}]
        }
    }));
    let candidate = normalized(json!({
        "kind": "Pod",
        "spec": {
            "containers": [{"name": "app", "image": "app:1"}],
            "volumes": [
                {"name": "config", "configMap": {"name": "new"}},
                {"name": "cache", "emptyDir": {}}
            ]
        }
    }));

    let changes = diff_workload_objects(&baseline, &candidate);

    assert_eq!(
        changes.len(),
        1,
        "the redefined `config` volume has no kind in the frozen vocabulary: {changes:?}"
    );
    assert_eq!(changes[0].kind, SemanticChangeKind::VolumeAdded);
    assert_eq!(changes[0].subject.as_deref(), Some("cache"));
    assert!(
        !raw_object_diff(&baseline.value, &candidate.value).is_empty(),
        "the redefinition must still be visible in the raw diff"
    );
}

// ---------------------------------------------------------------------
// Step 3: environment, without literals
// ---------------------------------------------------------------------

/// Fails if a changed environment *literal* is missed, or if either
/// literal is rendered into a report-ready field.
///
/// The two payloads are deliberately equal here: the descriptors say
/// "this entry's value is a literal" on both sides, and the change
/// exists because the entries themselves differ. See `workload.rs`'s
/// module documentation.
#[test]
fn changed_environment_literal_is_reported_without_either_value() {
    let baseline = normalized(pod(&json!({
        "name": "app",
        "image": "app:1",
        "env": [{"name": "TOKEN", "value": "old-secret"}]
    })));
    let candidate = normalized(pod(&json!({
        "name": "app",
        "image": "app:1",
        "env": [{"name": "TOKEN", "value": "new-secret"}]
    })));

    let changes = diff_workload_objects(&baseline, &candidate);

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(changes[0].kind, SemanticChangeKind::EnvironmentChanged);
    assert_eq!(changes[0].subject.as_deref(), Some("app"));
    assert_eq!(
        changes[0].object_path.as_deref(),
        Some("/spec/containers/0/env/0")
    );
    assert_eq!(
        changes[0].baseline,
        Some(json!({"name": "TOKEN", "valueSource": "literal"}))
    );
    assert_eq!(changes[0].candidate, changes[0].baseline);
    let rendered = serde_json::to_string(&changes).expect("serialize changes");
    assert!(
        !rendered.contains("old-secret") && !rendered.contains("new-secret"),
        "no environment literal may reach a report-ready field: {rendered}"
    );
}

/// Fails if a rewired environment source is flattened into the same
/// opaque descriptor a literal gets: moving a value out of a Secret and
/// into the pod spec is precisely the regression a reader must be able
/// to see.
#[test]
fn environment_source_change_is_visible_in_full() {
    let baseline = normalized(pod(&json!({
        "name": "app",
        "image": "app:1",
        "env": [{"name": "TOKEN", "valueFrom": {"secretKeyRef": {"name": "api", "key": "token"}}}]
    })));
    let candidate = normalized(pod(&json!({
        "name": "app",
        "image": "app:1",
        "env": [{"name": "TOKEN", "value": "inlined"}]
    })));

    let changes = diff_workload_objects(&baseline, &candidate);

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(
        changes[0].baseline,
        Some(json!({
            "name": "TOKEN",
            "valueSource": "valueFrom",
            "valueFrom": {"secretKeyRef": {"name": "api", "key": "token"}}
        }))
    );
    assert_eq!(
        changes[0].candidate,
        Some(json!({"name": "TOKEN", "valueSource": "literal"}))
    );
}

// ---------------------------------------------------------------------
// Step 4: service account, security contexts, resources
// ---------------------------------------------------------------------

/// Fails if the pod-level and container-level security contexts are not
/// distinguishable, or if the service account or resource changes are
/// missed -- and pins the documented emission order while it is at it.
#[test]
fn pod_level_and_container_level_changes_are_distinguishable() {
    let baseline = normalized(json!({
        "kind": "Pod",
        "spec": {
            "serviceAccountName": "default",
            "securityContext": {"runAsNonRoot": true},
            "containers": [{
                "name": "app",
                "image": "app:1",
                "securityContext": {"allowPrivilegeEscalation": false},
                "resources": {"limits": {"cpu": "500m"}}
            }]
        }
    }));
    let candidate = normalized(json!({
        "kind": "Pod",
        "spec": {
            "serviceAccountName": "workload",
            "securityContext": {"runAsNonRoot": false},
            "containers": [{
                "name": "app",
                "image": "app:1",
                "securityContext": {"allowPrivilegeEscalation": true},
                "resources": {"limits": {"cpu": "2"}}
            }]
        }
    }));

    let changes = diff_workload_objects(&baseline, &candidate);

    let observed: Vec<(SemanticChangeKind, Option<&str>, Option<&str>)> = changes
        .iter()
        .map(|change| {
            (
                change.kind,
                change.subject.as_deref(),
                change.object_path.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        observed,
        vec![
            (
                SemanticChangeKind::ServiceAccountChanged,
                None,
                Some("/spec/serviceAccountName")
            ),
            (
                SemanticChangeKind::SecurityContextChanged,
                None,
                Some("/spec/securityContext")
            ),
            (
                SemanticChangeKind::SecurityContextChanged,
                Some("app"),
                Some("/spec/containers/0/securityContext")
            ),
            (
                SemanticChangeKind::ResourceRequirementChanged,
                Some("app"),
                Some("/spec/containers/0/resources")
            ),
        ],
        "emission order, subjects, or paths drifted"
    );
}

// ---------------------------------------------------------------------
// Step 5: unmodeled fields stay in the raw diff
// ---------------------------------------------------------------------

/// Fails if a change to a field this module does not model produces any
/// semantic change, or if that change is not visible in the raw diff --
/// the two halves of Task 4.5 Step 5, which are only meaningful
/// together.
#[test]
fn unmodeled_field_change_is_raw_only() {
    let baseline = normalized(json!({
        "kind": "Pod",
        "spec": {
            "nodeSelector": {"disktype": "ssd"},
            "containers": [{"name": "app", "image": "app:1"}]
        }
    }));
    let candidate = normalized(json!({
        "kind": "Pod",
        "spec": {
            "nodeSelector": {"disktype": "nvme"},
            "containers": [{"name": "app", "image": "app:1"}]
        }
    }));

    assert_eq!(
        diff_workload_objects(&baseline, &candidate),
        Vec::new(),
        "`nodeSelector` has no kind in the frozen vocabulary and must not borrow one"
    );
    assert_eq!(
        raw_object_diff(&baseline.value, &candidate.value).len(),
        1,
        "the difference must remain visible as evidence"
    );
}

// ---------------------------------------------------------------------
// Locating the pod spec
// ---------------------------------------------------------------------

/// Fails if a pod template inside a `Deployment` is not compared, or if
/// its paths are not rooted at the template.
#[test]
fn deployment_pod_template_is_compared_at_its_own_path() {
    let baseline = normalized(json!({
        "kind": "Deployment",
        "spec": {"replicas": 2, "template": {"spec": {"containers": [{"name": "app", "image": "app:1"}]}}}
    }));
    let candidate = normalized(json!({
        "kind": "Deployment",
        "spec": {"replicas": 2, "template": {"spec": {"containers": [{"name": "app", "image": "app:2"}]}}}
    }));

    let changes = diff_workload_objects(&baseline, &candidate);

    assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
    assert_eq!(
        changes[0].object_path.as_deref(),
        Some("/spec/template/spec/containers/0/image")
    );
}

/// Fails if two objects whose pod specs live at different locations are
/// compared field by field: a `Pod` and a `Deployment` are not two
/// versions of the same thing, and no claim about them would mean
/// anything.
#[test]
fn objects_with_different_template_locations_are_not_compared() {
    let baseline = normalized(pod(&json!({"name": "app", "image": "app:1"})));
    let candidate = normalized(json!({
        "kind": "Deployment",
        "spec": {"template": {"spec": {"containers": [{"name": "app", "image": "app:2"}]}}}
    }));

    assert_eq!(diff_workload_objects(&baseline, &candidate), Vec::new());
}

/// Fails if an object with no pod spec at all produces a semantic claim.
#[test]
fn object_without_a_pod_spec_produces_nothing() {
    let baseline = normalized(json!({"kind": "ConfigMap", "data": {"a": "1"}}));
    let candidate = normalized(json!({"kind": "ConfigMap", "data": {"a": "2"}}));

    assert_eq!(diff_workload_objects(&baseline, &candidate), Vec::new());
    assert!(!raw_object_diff(&baseline.value, &candidate.value).is_empty());
}

// ---------------------------------------------------------------------
// Fixture attribution and determinism
// ---------------------------------------------------------------------

/// Fails if a change arrives already claiming to belong to some fixture,
/// or if stamping the real identity does not replace the sentinel.
///
/// A `NormalizedObject` does not say which fixture produced it, so this
/// function cannot know; the sentinel is what makes a caller that forgot
/// to attribute visible instead of silently wrong.
#[test]
fn changes_are_unattributed_until_a_caller_stamps_them() {
    let baseline = normalized(pod(&json!({"name": "app", "image": "app:1"})));
    let candidate = normalized(pod(&json!({"name": "app", "image": "app:2"})));
    let fixture_id = FixtureId::parse("pod-basic-0").unwrap();

    let changes = diff_workload_objects(&baseline, &candidate);
    assert_eq!(changes[0].fixture_id.as_str(), UNATTRIBUTED_FIXTURE);

    let attributed = changes[0].clone().attributed_to(&fixture_id);
    assert_eq!(attributed.fixture_id, fixture_id);
    assert_eq!(
        attributed.kind, changes[0].kind,
        "attribution changes nothing else about a claim"
    );
}

/// Fails if the result depends on the order the two documents' keys and
/// arrays happened to be written in, which a report's stability and
/// Global Constraint 7 both rest on.
#[test]
fn output_is_deterministic_across_source_orderings() {
    let baseline = normalized(json!({
        "kind": "Pod",
        "spec": {"containers": [
            {"image": "app:1", "name": "app", "env": [{"name": "B", "value": "1"}, {"name": "A", "value": "1"}]},
            {"name": "proxy", "image": "proxy:1"}
        ]}
    }));
    let candidate = normalized(json!({
        "kind": "Pod",
        "spec": {"containers": [
            {"name": "proxy", "image": "proxy:2"},
            {"env": [{"name": "A", "value": "2"}, {"name": "B", "value": "2"}], "name": "app", "image": "app:1"}
        ]}
    }));

    let first = diff_workload_objects(&baseline, &candidate);
    let second = diff_workload_objects(&baseline, &candidate);

    assert_eq!(first, second);
    let observed: Vec<(SemanticChangeKind, Option<&str>)> = first
        .iter()
        .map(|change| (change.kind, change.subject.as_deref()))
        .collect();
    assert_eq!(
        observed,
        vec![
            (SemanticChangeKind::EnvironmentChanged, Some("app")),
            (SemanticChangeKind::EnvironmentChanged, Some("app")),
            (SemanticChangeKind::ImageChanged, Some("proxy")),
        ],
        "containers sort by name and environment entries by variable name"
    );
}
