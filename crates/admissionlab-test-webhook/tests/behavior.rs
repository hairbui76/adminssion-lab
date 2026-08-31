//! Task 3.9's regression test: every controlled behavior PRODUCT.md §30
//! names, driven through the *real* request handler
//! (`admissionlab_test_webhook::serve::handle`) with a real
//! `AdmissionReview` body, and every webhook configuration that routes
//! to it, read from the *real* checked-in manifests.
//!
//! No cluster, no Docker, no `kind` — this runs under a plain `cargo
//! test --workspace`, unlike `tests/kind_smoke.rs`. That is deliberate:
//! the things asserted here are properties of this repository's own
//! code and manifests, and a property that can be checked without a
//! cluster should never need one.
//!
//! # Two halves that only mean something together
//!
//! - *Behavior*: an `AdmissionReview` goes in, an `AdmissionReview`
//!   comes out, and the base64 `patch` inside it is decoded and compared
//!   operation by operation. Not "a patch was returned" — the exact
//!   operations, and for one case the exact base64 bytes on the wire.
//! - *Contract*: each `clientConfig.service.path` in the manifests
//!   equals the route constant the server actually serves, each
//!   webhook's scoping/`failurePolicy`/`sideEffects`/`admissionReviewVersions`
//!   are what this task decided they should be, and the object-selector
//!   label the reinvocation chain turns on is one the labels webhook can
//!   really produce.
//!
//! Either half alone would pass while the component was broken: a
//! perfect handler on a path nothing calls admits everything silently,
//! and perfectly wired manifests in front of a handler that patches the
//! wrong pointer produce an API server rejection at fixture time. The
//! cross-checks between them are the point of this file.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use admissionlab_installer::load_manifest_bundle;
use admissionlab_test_webhook::serve::{
    self, MUTATE_CONTAINERS_PATH, MUTATE_LABELS_PATH, VALIDATE_PATH,
};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::{Method, Request, StatusCode};
use k8s_openapi::ByteString;
use serde_json::{Value, json};

/// The namespace label every one of this recipe's webhooks requires
/// before it will look at a pod at all. Spelled here independently of
/// the manifests so that a change to either side fails this test —
/// which is the whole point: this is the single property that keeps a
/// `failurePolicy: Fail` webhook away from `kube-system` and from this
/// recipe's own namespace.
const NAMESPACE_OPT_IN_LABEL: &str = "admissionlab.dev/test-webhook";

/// The object label `admissionlab-test-webhook-mutate-containers`
/// selects on — the gate the reinvocation chain turns on. Also spelled
/// independently of the manifests, for the same reason.
const CONTAINERS_GATE_LABEL: &str = "test.admissionlab.io/containers";
/// That gate label's required value.
const CONTAINERS_GATE_VALUE: &str = "enabled";

/// The two `MutatingWebhookConfiguration` names, in the order the API
/// server evaluates them (lexicographic by configuration name).
const MUTATING_CONFIGURATION_NAMES: [&str; 2] = [
    "admissionlab-test-webhook-mutate-containers",
    "admissionlab-test-webhook-mutate-labels",
];

/// The `ValidatingWebhookConfiguration`'s name.
const VALIDATING_CONFIGURATION_NAME: &str = "admissionlab-test-webhook";

// ---------------------------------------------------------------------
// Driving the real handler.
// ---------------------------------------------------------------------

/// The response to one admission request: the HTTP status, plus the
/// parsed body (`None` when the body is not JSON, which is how the
/// controlled-failure and bad-request shapes answer).
struct Answer {
    status: StatusCode,
    body: Option<Value>,
}

impl Answer {
    /// The `response` half of a well-formed `AdmissionReview` answer.
    fn response(&self) -> &Value {
        assert_eq!(self.status, StatusCode::OK, "expected an admission answer");
        &self.body.as_ref().expect("a JSON body")["response"]
    }

    fn allowed(&self) -> bool {
        self.response()["allowed"]
            .as_bool()
            .expect("`allowed` is always present and boolean")
    }

    /// The `patch` field decoded from base64 and parsed — `None` when
    /// the field is absent entirely, which is how "no mutation" is
    /// expressed (see `serve::mutated`).
    fn patch(&self) -> Option<Value> {
        let raw = self.response().get("patch")?;
        assert!(
            raw.is_string(),
            "`patch` must be a base64 string on the wire, not {raw}: a JSON array of numbers is \
             what a plain `Vec<u8>` would serialize to, and the API server would reject it"
        );
        assert_eq!(
            self.response()["patchType"],
            json!("JSONPatch"),
            "a response carrying a patch must declare its type"
        );
        // Decoded through `k8s_openapi::ByteString`'s own base64 codec
        // -- the same one the server encoded with, and the same one
        // every `caBundle` in this recipe round-trips through.
        let bytes: ByteString =
            serde_json::from_value(raw.clone()).expect("`patch` must decode as base64");
        Some(serde_json::from_slice(&bytes.0).expect("the decoded patch must be JSON"))
    }

    /// The denial message, which is only ever set when `allowed` is
    /// false.
    fn message(&self) -> String {
        assert!(!self.allowed(), "only a denial carries a message");
        self.response()["status"]["message"]
            .as_str()
            .expect("a denial always carries a message")
            .to_owned()
    }
}

/// `POST`s an `AdmissionReview` wrapping `object` to `path`, through the
/// real handler.
async fn post(path: &str, object: &Value) -> Answer {
    let review = json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "11111111-2222-3333-4444-555555555555",
            "kind": {"group": "", "version": "v1", "kind": "Pod"},
            "resource": {"group": "", "version": "v1", "resource": "pods"},
            "name": "fixture",
            "namespace": "admissionlab-fixtures",
            "operation": "CREATE",
            "userInfo": {"username": "admissionlab"},
            "dryRun": true,
            "object": object,
        },
    });

    let request = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(
            serde_json::to_vec(&review).expect("the test review serializes"),
        )))
        .expect("well-formed test request");

    let response = serve::handle(request).await.expect("handle is infallible");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collecting a Full body cannot fail")
        .to_bytes();
    Answer {
        status,
        body: serde_json::from_slice(&bytes).ok(),
    }
}

/// A minimal pod carrying `annotations` and, optionally, `labels`.
fn pod(annotations: &Value, labels: &Value, spec: &Value) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": "fixture",
            "namespace": "admissionlab-fixtures",
            "annotations": annotations,
            "labels": labels,
        },
        "spec": spec,
    })
}

/// A pod with one ordinary container and no labels.
fn simple_pod(annotations: &Value) -> Value {
    pod(
        annotations,
        &json!({}),
        &json!({"containers": [{"name": "app", "image": "registry.k8s.io/pause:3.10"}]}),
    )
}

// ---------------------------------------------------------------------
// Step 1: the JSON Patch each mutating action produces.
// ---------------------------------------------------------------------

/// The one case that pins the bytes actually put on the wire, not only
/// the decoded operations: `AdmissionResponse.patch` is a Go `[]byte` on
/// the API server's side, i.e. a base64 **string** in JSON. A
/// `Vec<u8>` serialized with plain `serde` would be a JSON array of
/// numbers instead, which the API server rejects — and no test that
/// only decodes the field with the same codec that encoded it would
/// notice. This literal does.
#[tokio::test]
async fn the_patch_is_standard_base64_on_the_wire() {
    let object = pod(
        &json!({"test.admissionlab.io/add-label": "team=platform"}),
        &json!(null),
        &json!({"containers": [{"name": "app", "image": "registry.k8s.io/pause:3.10"}]}),
    );
    let answer = post(MUTATE_LABELS_PATH, &object).await;

    assert_eq!(
        answer.response()["patch"],
        json!(
            "W3sib3AiOiJhZGQiLCJwYXRoIjoiL21ldGFkYXRhL2xhYmVscyIsInZhbHVlIjp7InRlYW0iOiJwbGF0Zm9ybSJ9fV0="
        ),
        "base64 of [{{\"op\":\"add\",\"path\":\"/metadata/labels\",\"value\":{{\"team\":\"platform\"}}}}]"
    );
}

#[tokio::test]
async fn add_label_patches_metadata_labels() {
    let object = simple_pod(&json!({"test.admissionlab.io/add-label": "team=platform"}));
    let answer = post(MUTATE_LABELS_PATH, &object).await;

    assert!(answer.allowed());
    assert_eq!(
        answer.patch(),
        Some(json!([
            {"op": "add", "path": "/metadata/labels/team", "value": "platform"}
        ]))
    );
}

/// A label key with a `/` in it — every Kubernetes-prefixed label, and
/// the reinvocation gate itself — has to be RFC 6901 escaped in the
/// pointer or the patch addresses a nested member that does not exist.
#[tokio::test]
async fn a_prefixed_label_key_is_escaped_in_the_pointer() {
    let object = simple_pod(&json!({
        "test.admissionlab.io/add-label": format!("{CONTAINERS_GATE_LABEL}={CONTAINERS_GATE_VALUE}"),
    }));
    let answer = post(MUTATE_LABELS_PATH, &object).await;

    assert_eq!(
        answer.patch(),
        Some(json!([{
            "op": "add",
            "path": "/metadata/labels/test.admissionlab.io~1containers",
            "value": CONTAINERS_GATE_VALUE,
        }]))
    );
}

#[tokio::test]
async fn add_container_appends_to_spec_containers() {
    let object = simple_pod(&json!({
        "test.admissionlab.io/add-container": "sidecar=registry.k8s.io/pause:3.10",
    }));
    let answer = post(MUTATE_CONTAINERS_PATH, &object).await;

    assert_eq!(
        answer.patch(),
        Some(json!([{
            "op": "add",
            "path": "/spec/containers/-",
            "value": {"name": "sidecar", "image": "registry.k8s.io/pause:3.10"},
        }]))
    );
}

/// `spec.initContainers` is absent on an ordinary pod, so this is the
/// create-the-array path, not the append path — a distinction RFC 6902
/// cares about (`add /spec/initContainers/-` on an absent array is not a
/// valid operation).
#[tokio::test]
async fn add_init_container_creates_the_array_when_it_is_absent() {
    let object = simple_pod(&json!({
        "test.admissionlab.io/add-init-container": "setup=registry.k8s.io/busybox:1.36",
    }));
    let answer = post(MUTATE_CONTAINERS_PATH, &object).await;

    assert_eq!(
        answer.patch(),
        Some(json!([{
            "op": "add",
            "path": "/spec/initContainers",
            "value": [{"name": "setup", "image": "registry.k8s.io/busybox:1.36"}],
        }]))
    );
}

#[tokio::test]
async fn add_init_container_appends_when_the_array_already_exists() {
    let object = pod(
        &json!({"test.admissionlab.io/add-init-container": "setup=registry.k8s.io/busybox:1.36"}),
        &json!({}),
        &json!({
            "containers": [{"name": "app", "image": "registry.k8s.io/pause:3.10"}],
            "initContainers": [{"name": "other", "image": "registry.k8s.io/busybox:1.36"}],
        }),
    );
    let answer = post(MUTATE_CONTAINERS_PATH, &object).await;

    assert_eq!(
        answer.patch(),
        Some(json!([{
            "op": "add",
            "path": "/spec/initContainers/-",
            "value": {"name": "setup", "image": "registry.k8s.io/busybox:1.36"},
        }]))
    );
}

#[tokio::test]
async fn add_volume_adds_an_empty_dir() {
    let object = simple_pod(&json!({"test.admissionlab.io/add-volume": "scratch"}));
    let answer = post(MUTATE_CONTAINERS_PATH, &object).await;

    assert_eq!(
        answer.patch(),
        Some(json!([{
            "op": "add",
            "path": "/spec/volumes",
            "value": [{"name": "scratch", "emptyDir": {}}],
        }]))
    );
}

#[tokio::test]
async fn remove_container_removes_by_index() {
    let object = pod(
        &json!({"test.admissionlab.io/remove-container": "sidecar"}),
        &json!({}),
        &json!({"containers": [
            {"name": "app", "image": "registry.k8s.io/pause:3.10"},
            {"name": "sidecar", "image": "registry.k8s.io/pause:3.10"},
        ]}),
    );
    let answer = post(MUTATE_CONTAINERS_PATH, &object).await;

    assert_eq!(
        answer.patch(),
        Some(json!([{"op": "remove", "path": "/spec/containers/1"}]))
    );
}

#[tokio::test]
async fn remove_init_container_removes_by_index() {
    let object = pod(
        &json!({"test.admissionlab.io/remove-init-container": "setup"}),
        &json!({}),
        &json!({
            "containers": [{"name": "app", "image": "registry.k8s.io/pause:3.10"}],
            "initContainers": [{"name": "setup", "image": "registry.k8s.io/busybox:1.36"}],
        }),
    );
    let answer = post(MUTATE_CONTAINERS_PATH, &object).await;

    assert_eq!(
        answer.patch(),
        Some(json!([{"op": "remove", "path": "/spec/initContainers/0"}]))
    );
}

/// Every response must echo the request's `uid`; the API server
/// discards one that does not.
#[tokio::test]
async fn the_request_uid_is_echoed() {
    let answer = post(VALIDATE_PATH, &simple_pod(&json!({}))).await;
    assert_eq!(
        answer.response()["uid"],
        json!("11111111-2222-3333-4444-555555555555")
    );
}

// ---------------------------------------------------------------------
// Step 3: idempotency.
// ---------------------------------------------------------------------

/// The property `reinvocationPolicy: IfNeeded` makes load-bearing: a
/// webhook the API server calls twice must not append the sidecar
/// twice. Asserted as "no `patch` field at all", not "an empty patch" —
/// the two look the same to the API server but only the former is
/// distinguishable in an audit log from a webhook that patched nothing.
#[tokio::test]
async fn an_already_present_target_produces_no_mutation_at_all() {
    let object = pod(
        &json!({
            "test.admissionlab.io/add-label": "team=platform",
            "test.admissionlab.io/add-container": "sidecar=registry.k8s.io/pause:3.10",
            "test.admissionlab.io/add-init-container": "setup=registry.k8s.io/busybox:1.36",
            "test.admissionlab.io/add-volume": "scratch",
        }),
        &json!({"team": "platform"}),
        &json!({
            "containers": [
                {"name": "app", "image": "registry.k8s.io/pause:3.10"},
                {"name": "sidecar", "image": "registry.k8s.io/pause:3.10"},
            ],
            "initContainers": [{"name": "setup", "image": "registry.k8s.io/busybox:1.36"}],
            "volumes": [{"name": "scratch", "emptyDir": {}}],
        }),
    );

    for path in [MUTATE_LABELS_PATH, MUTATE_CONTAINERS_PATH] {
        let answer = post(path, &object).await;
        assert!(answer.allowed(), "an idempotent no-op still allows");
        assert_eq!(answer.patch(), None, "for {path}");
        assert!(
            answer.response().get("patchType").is_none(),
            "`patchType` must be absent whenever `patch` is, for {path}"
        );
    }
}

/// A container whose name matches but whose image differs is still "the
/// requested container is already there": this webhook adds sidecars, it
/// does not reconcile their images, and a fixture that wanted a
/// different image would produce a duplicate-name pod the API server
/// rejects.
#[tokio::test]
async fn idempotency_is_keyed_on_the_container_name() {
    let object = pod(
        &json!({"test.admissionlab.io/add-container": "sidecar=registry.k8s.io/pause:3.10"}),
        &json!({}),
        &json!({"containers": [{"name": "sidecar", "image": "something-else:1"}]}),
    );
    assert_eq!(post(MUTATE_CONTAINERS_PATH, &object).await.patch(), None);
}

// ---------------------------------------------------------------------
// Step 2: deny, delay, controlled failure.
// ---------------------------------------------------------------------

#[tokio::test]
async fn deny_is_a_real_admission_denial_with_the_fixtures_message() {
    let object = simple_pod(&json!({"test.admissionlab.io/deny": "denied by fixture"}));
    let answer = post(VALIDATE_PATH, &object).await;

    assert!(!answer.allowed());
    assert_eq!(answer.message(), "denied by fixture");
    assert_eq!(answer.response()["status"]["code"], json!(403));
}

/// The mutating routes never deny, delay or fail — those belong to the
/// single validating configuration, so a fixture's delay is applied
/// exactly once however many mutating configurations are installed.
#[tokio::test]
async fn the_mutating_routes_ignore_the_validating_vocabulary() {
    let object = simple_pod(&json!({
        "test.admissionlab.io/deny": "denied by fixture",
        "test.admissionlab.io/fail": "true",
    }));

    for path in [MUTATE_LABELS_PATH, MUTATE_CONTAINERS_PATH] {
        let answer = post(path, &object).await;
        assert_eq!(answer.status, StatusCode::OK, "for {path}");
        assert!(answer.allowed(), "for {path}");
    }
}

/// A controlled failure is an HTTP 500, not a denial: the API server
/// records it as a webhook *call* failure and resolves it through
/// `failurePolicy`, which is a different observable outcome from a
/// verdict of "denied".
#[tokio::test]
async fn fail_answers_with_a_server_error_rather_than_a_verdict() {
    let object = simple_pod(&json!({"test.admissionlab.io/fail": "true"}));
    let answer = post(VALIDATE_PATH, &object).await;

    assert_eq!(answer.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        answer.body.is_none(),
        "a failed call must not look like a well-formed admission answer"
    );
}

/// Tokio's paused clock: the full delay elapses on the runtime's clock
/// while the test costs no wall-clock time.
#[tokio::test(start_paused = true)]
async fn delay_holds_the_response_without_changing_it() {
    let started = tokio::time::Instant::now();
    let object = simple_pod(&json!({"test.admissionlab.io/delay-ms": "250"}));
    let answer = post(VALIDATE_PATH, &object).await;

    assert_eq!(started.elapsed(), std::time::Duration::from_millis(250));
    assert!(
        answer.allowed(),
        "a delay alone must not change the verdict"
    );
}

// ---------------------------------------------------------------------
// Annotation errors.
// ---------------------------------------------------------------------

/// The decision `crates/admissionlab-test-webhook/src/behavior.rs`
/// documents at length: an unusable annotation denies, naming itself.
/// The alternative — ignore it and admit the object unchanged — is
/// indistinguishable in a captured result from the regression Admission
/// Lab exists to catch.
#[tokio::test]
async fn an_unusable_annotation_denies_and_names_itself_on_every_route() {
    let object = simple_pod(&json!({"test.admissionlab.io/add-labels": "team=platform"}));

    for path in [MUTATE_LABELS_PATH, MUTATE_CONTAINERS_PATH, VALIDATE_PATH] {
        let answer = post(path, &object).await;
        assert_eq!(answer.status, StatusCode::OK, "for {path}");
        assert!(!answer.allowed(), "for {path}");
        let message = answer.message();
        assert!(
            message.contains("test.admissionlab.io/add-labels"),
            "the denial must name the offending annotation, got {message:?} for {path}"
        );
        assert!(
            answer.response().get("patch").is_none(),
            "a denial never carries a patch, for {path}"
        );
    }
}

// ---------------------------------------------------------------------
// The contract between the routes and the manifests that call them.
// ---------------------------------------------------------------------

/// This checkout's own `recipes/test-webhook/manifests` directory.
fn manifests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../recipes/test-webhook/manifests")
}

/// Every document in one checked-in manifest file, parsed through the
/// same loader the installer itself uses (Task 2.3) rather than a
/// hand-rolled YAML read.
fn documents(file: &str) -> Vec<Value> {
    let path = manifests_dir().join(file);
    load_manifest_bundle(std::slice::from_ref(&path))
        .unwrap_or_else(|error| panic!("{file} must parse as Kubernetes manifests: {error}"))
        .documents
}

/// The `webhooks` entries of one configuration document.
fn webhooks(document: &Value) -> &Vec<Value> {
    document["webhooks"]
        .as_array()
        .expect("a webhook configuration always declares `webhooks`")
}

/// Everything every webhook in this recipe must declare identically,
/// whichever kind it is — checked once, here, so a new configuration
/// added later cannot quietly omit one of them.
fn assert_common_webhook_contract(webhook: &Value, label: &str) {
    assert_eq!(
        webhook["admissionReviewVersions"],
        json!(["v1"]),
        "{label}: this server speaks exactly one AdmissionReview version"
    );
    assert_eq!(
        webhook["sideEffects"],
        json!("None"),
        "{label}: a webhook declaring side effects may be skipped during the server-side dry-run \
         that Global Constraint 16 makes Alpha's authoritative fixture execution mode -- which \
         would make it invisible to every fixture Admission Lab runs"
    );
    assert_eq!(
        webhook["matchPolicy"],
        json!("Equivalent"),
        "{label}: a request arriving through another API version must still be intercepted"
    );
    assert_eq!(
        webhook["failurePolicy"],
        json!("Fail"),
        "{label}: `Ignore` would turn every webhook outage into a silent allow, which Global \
         Constraint 15 rules out -- unavailable is not the same as allowed"
    );
    assert_eq!(
        webhook["namespaceSelector"]["matchLabels"][NAMESPACE_OPT_IN_LABEL],
        json!("enabled"),
        "{label}: the namespace opt-in is the one property that structurally keeps a \
         failurePolicy: Fail webhook away from kube-system and from this recipe's own namespace"
    );
    assert_eq!(
        webhook["rules"],
        json!([{
            "apiGroups": [""],
            "apiVersions": ["v1"],
            "operations": ["CREATE", "UPDATE"],
            "resources": ["pods"],
            "scope": "Namespaced",
        }]),
        "{label}: fixtures submit namespaced v1 Pods and nothing else"
    );
}

/// The route constants the server actually serves must be the paths the
/// manifests call. A drift here is a 404 at fixture time — which, with
/// `failurePolicy: Fail`, rejects every fixture, and without it would
/// silently admit every fixture.
#[test]
fn every_webhook_calls_a_path_this_server_serves() {
    let validating = documents("20-webhook-configuration.yaml");
    assert_eq!(validating.len(), 1, "one validating configuration");
    assert_eq!(
        validating[0]["kind"],
        json!("ValidatingWebhookConfiguration")
    );
    assert_eq!(
        validating[0]["metadata"]["name"],
        json!(VALIDATING_CONFIGURATION_NAME)
    );

    let validating_webhooks = webhooks(&validating[0]);
    assert_eq!(validating_webhooks.len(), 1);
    assert_common_webhook_contract(&validating_webhooks[0], "validating webhook");
    assert_eq!(
        validating_webhooks[0]["clientConfig"]["service"]["path"],
        json!(VALIDATE_PATH)
    );
    assert!(
        validating_webhooks[0].get("reinvocationPolicy").is_none(),
        "reinvocationPolicy is a mutating-only field; the API server rejects it here"
    );

    let mutating = documents("21-mutating-webhook-configurations.yaml");
    assert_eq!(mutating.len(), 2, "Task 3.9 Step 4 needs exactly two");

    let by_name: Vec<&Value> = MUTATING_CONFIGURATION_NAMES
        .iter()
        .map(|name| {
            mutating
                .iter()
                .find(|document| document["metadata"]["name"] == json!(name))
                .unwrap_or_else(|| panic!("no MutatingWebhookConfiguration named {name}"))
        })
        .collect();

    let expected_paths = [MUTATE_CONTAINERS_PATH, MUTATE_LABELS_PATH];
    for (document, expected_path) in by_name.iter().zip(expected_paths) {
        assert_eq!(document["kind"], json!("MutatingWebhookConfiguration"));
        let entries = webhooks(document);
        assert_eq!(entries.len(), 1);
        assert_common_webhook_contract(&entries[0], expected_path);
        assert_eq!(
            entries[0]["clientConfig"]["service"]["path"],
            json!(expected_path)
        );
        assert_eq!(
            entries[0]["reinvocationPolicy"],
            json!("IfNeeded"),
            "{expected_path}: set on both webhooks, not only the one the chain needs, so \
             correctness never rests on which of them the API server happens to call first"
        );
    }
}

/// The reinvocation chain, checked end to end without a cluster: the
/// object selector that gates the containers webhook must be a label
/// the labels webhook can actually produce, and the labels webhook must
/// really produce it. If either side drifted, the dedicated
/// reinvocation fixture would simply never be mutated — and, since the
/// containers webhook would not have matched, nothing would report an
/// error.
#[tokio::test]
async fn the_reinvocation_gate_label_is_one_the_labels_webhook_can_add() {
    let mutating = documents("21-mutating-webhook-configurations.yaml");
    let containers = mutating
        .iter()
        .find(|document| document["metadata"]["name"] == json!(MUTATING_CONFIGURATION_NAMES[0]))
        .expect("the containers configuration");
    let containers_webhook = &webhooks(containers)[0];

    assert_eq!(
        containers_webhook["objectSelector"]["matchLabels"][CONTAINERS_GATE_LABEL],
        json!(CONTAINERS_GATE_VALUE),
        "the containers webhook must be gated on exactly the label the chain turns on"
    );

    let labels = mutating
        .iter()
        .find(|document| document["metadata"]["name"] == json!(MUTATING_CONFIGURATION_NAMES[1]))
        .expect("the labels configuration");
    assert!(
        webhooks(labels)[0].get("objectSelector").is_none(),
        "the labels webhook must match a pod that does not yet carry the gate label -- gating it \
         on any label of its own would break the chain it exists to start"
    );

    // And the other half: asking for that exact label really does
    // produce a patch that sets it.
    let fixture = simple_pod(&json!({
        "test.admissionlab.io/add-label": format!("{CONTAINERS_GATE_LABEL}={CONTAINERS_GATE_VALUE}"),
    }));
    let answer = post(MUTATE_LABELS_PATH, &fixture).await;
    assert_eq!(
        answer.patch(),
        Some(json!([{
            "op": "add",
            "path": "/metadata/labels/test.admissionlab.io~1containers",
            "value": CONTAINERS_GATE_VALUE,
        }])),
        "the labels webhook must be able to produce the gate the containers webhook waits for"
    );

    // Once the gate label is there, the containers webhook's own work
    // is a plain first-round mutation -- reinvocation changes when it
    // runs, never what it produces.
    let gated = pod(
        &json!({"test.admissionlab.io/add-container": "sidecar=registry.k8s.io/pause:3.10"}),
        &json!({CONTAINERS_GATE_LABEL: CONTAINERS_GATE_VALUE}),
        &json!({"containers": [{"name": "app", "image": "registry.k8s.io/pause:3.10"}]}),
    );
    assert_eq!(
        post(MUTATE_CONTAINERS_PATH, &gated).await.patch(),
        Some(json!([{
            "op": "add",
            "path": "/spec/containers/-",
            "value": {"name": "sidecar", "image": "registry.k8s.io/pause:3.10"},
        }]))
    );
}

/// The three places the mutating configuration names are written by
/// hand — the manifests themselves, the init container's environment
/// variable, and the `ClusterRole`'s `resourceNames` — must agree. A name
/// missing from the environment variable leaves that configuration's
/// `caBundle` empty (so every call to it fails); a name missing from the
/// `ClusterRole` makes the init container, and therefore the whole pod,
/// fail to start.
#[test]
fn the_mutating_configuration_names_agree_across_every_manifest() {
    let declared: BTreeSet<String> = documents("21-mutating-webhook-configurations.yaml")
        .iter()
        .map(|document| {
            document["metadata"]["name"]
                .as_str()
                .expect("every configuration is named")
                .to_owned()
        })
        .collect();
    let expected: BTreeSet<String> = MUTATING_CONFIGURATION_NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(declared, expected);

    let deployment = documents("30-deployment.yaml");
    let env = deployment
        .iter()
        .find(|document| document["kind"] == json!("Deployment"))
        .expect("a Deployment")["spec"]["template"]["spec"]["initContainers"][0]["env"]
        .as_array()
        .expect("the init container declares env")
        .iter()
        .find(|entry| {
            entry["name"] == json!("ADMISSIONLAB_TEST_WEBHOOK_MUTATING_WEBHOOK_CONFIGURATION_NAMES")
        })
        .expect("the init container is told which mutating configurations to patch")["value"]
        .as_str()
        .expect("the value is a string")
        .to_owned();
    let from_env: BTreeSet<String> = env.split(',').map(|name| name.trim().to_owned()).collect();
    assert_eq!(
        from_env, expected,
        "the init container's environment must name every mutating configuration, and no other"
    );

    let rules = documents("10-rbac.yaml")
        .into_iter()
        .find(|document| document["kind"] == json!("ClusterRole"))
        .expect("a ClusterRole")["rules"]
        .as_array()
        .expect("the ClusterRole declares rules")
        .clone();
    let granted: BTreeSet<String> = rules
        .iter()
        .filter(|rule| rule["resources"] == json!(["mutatingwebhookconfigurations"]))
        .flat_map(|rule| {
            rule["resourceNames"]
                .as_array()
                .expect("every rule is restricted by resourceNames")
                .iter()
                .map(|name| {
                    name.as_str()
                        .expect("a resource name is a string")
                        .to_owned()
                })
        })
        .collect();
    assert_eq!(
        granted, expected,
        "the ClusterRole must grant get/update on exactly the mutating configurations this \
         recipe installs -- no more (it shares a cluster with a vendor's own webhooks) and no \
         fewer (the init container fails the pod if it cannot patch one)"
    );
    for rule in &rules {
        assert_eq!(
            rule["verbs"],
            json!(["get", "update"]),
            "the bootstrap container fetches and replaces; it never lists, creates or deletes"
        );
    }
}
