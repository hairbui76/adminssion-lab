//! ROADMAP Task 6.2: the persisted Gateway manifest installer.
//!
//! Two halves, tested two ways.
//!
//! [`admissionlab_gateway::plan_gateway_apply`] touches no cluster --
//! it reads, hashes, parses, validates and orders -- so it is tested
//! directly against real files under a temp directory. That covers Steps
//! 1 and 2 in full, including the fail-fast property (a malformed file
//! anywhere means nothing is applied) and the exact category order.
//!
//! [`admissionlab_gateway::apply_gateway_plan_with_client`] is tested
//! against a `tower_test::mock`-backed `kube::Client` -- the same
//! technique `admissionlab-fixtures`'s `resources.rs` and
//! `admissionlab-admission`'s `tests/execute_unit.rs` already use -- so
//! the assertions are about the *requests that actually go out*: their
//! order, their method, their URI, their `fieldManager`/`force` query
//! parameters, their `Content-Type`, and their bodies. Resource
//! resolution is supplied by a fake [`ResourceResolver`] rather than by
//! mocking `kube::discovery::Discovery`'s four-request handshake, which
//! keeps each test's scripted exchange about the applies it is
//! asserting; `admissionlab-fixtures`'s own test suite is what covers
//! the real discovery path.
//!
//! Not covered here, deliberately, and left to a live-cluster exit gate:
//! whether a forced server-side apply behaves as documented against a
//! real kube-apiserver, and whether `client_for` connects using a real
//! `kind`-produced kubeconfig. Both are the same gaps
//! `admissionlab-fixtures`'s own module documentation scopes out.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::pin::pin;

use admissionlab_core::{ClusterHandle, ClusterSpec, RunId, Side};
use admissionlab_fixtures::{FixtureError, ResolvedResource, ResourceResolver};
use admissionlab_gateway::{
    ApplyCategory, GatewayError, apply_gateway_plan_with_client, plan_gateway_apply,
};
use async_trait::async_trait;
use http::{Request, Response};
use http_body_util::BodyExt;
use kube::client::Body;
use kube::core::ApiResource;
use tower_test::mock;

// =========================================================================
// Fixture helpers
// =========================================================================

/// A temporary directory that removes itself when dropped.
///
/// A test holds one for as long as it uses paths underneath it. `Drop`
/// runs on a panicking assertion too, which an explicit delete at the
/// end of a test does not — that is what keeps a `cargo test` run from
/// leaving a directory per test behind in the system temp directory.
struct TempDir(PathBuf);

impl TempDir {
    /// The directory's path, valid for as long as this guard lives.
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fresh, empty directory under the OS temp dir for one test's
/// manifest files.
fn temp_manifest_dir(label: &str) -> TempDir {
    let directory = std::env::temp_dir().join(format!(
        "admissionlab-gateway-apply-{label}-{}",
        RunId::generate().as_str()
    ));
    std::fs::create_dir_all(&directory).expect("create temp manifest directory");
    TempDir(directory)
}

/// Writes `contents` to `<directory>/<name>` and returns the path.
fn write_manifest(directory: &Path, name: &str, contents: &str) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, contents).expect("write manifest");
    path
}

/// A minimal object document of `kind`, with `metadata.name` = `name`.
fn document(api_version: &str, kind: &str, namespace: Option<&str>, name: &str) -> String {
    let namespace_line = namespace.map_or_else(String::new, |namespace| {
        format!("\n  namespace: {namespace}")
    });
    format!("apiVersion: {api_version}\nkind: {kind}\nmetadata:\n  name: {name}{namespace_line}\n")
}

/// A minimal, otherwise-inert [`ClusterHandle`]. Only
/// [`ResourceResolver::resolve`] ever sees it in these tests, and the
/// fake resolver below never reads its kubeconfig -- which is why the
/// path is allowed not to exist.
fn cluster_handle() -> ClusterHandle {
    ClusterHandle {
        spec: ClusterSpec {
            side: Side::Baseline,
            name: "gateway-apply-test-cluster".to_string(),
            kubernetes_version: "1.36.0".to_string(),
            node_image: "kindest/node:v1.36.0".to_string(),
            images: Vec::new(),
        },
        kubeconfig: std::env::temp_dir().join("admissionlab-gateway-apply-test.kubeconfig"),
        audit_log: std::env::temp_dir().join("admissionlab-gateway-apply-test-audit.log"),
    }
}

/// A [`ResourceResolver`] that answers from a fixed table keyed by
/// `(apiVersion, kind)`, and reports anything else as unsupported --
/// exactly what a real cluster's discovery would do for a kind it does
/// not serve.
///
/// Stands in for `kube::discovery::Discovery` so the mocked HTTP
/// exchange in each test is only the applies under assertion. Every
/// entry's plural is the real upstream one (`gateways`, `httproutes`,
/// `referencegrants`, `gatewayclasses`), which is the point of resolving
/// against a cluster rather than guessing: `HTTPRoute` -> `httproutes`
/// is not something `ApiResource::from_gvk`'s heuristic can be trusted
/// to produce.
struct FakeResolver {
    resources: BTreeMap<(String, String), ResolvedResource>,
}

impl FakeResolver {
    fn gateway_api() -> Self {
        let mut resources = BTreeMap::new();
        let mut insert = |api_version: &str, kind: &str, plural: &str, namespaced: bool| {
            let (group, version) = api_version.split_once('/').unwrap_or(("", api_version));
            resources.insert(
                (api_version.to_string(), kind.to_string()),
                ResolvedResource {
                    api_resource: ApiResource {
                        group: group.to_string(),
                        version: version.to_string(),
                        api_version: api_version.to_string(),
                        kind: kind.to_string(),
                        plural: plural.to_string(),
                    },
                    namespaced,
                },
            );
        };

        insert("v1", "Namespace", "namespaces", false);
        insert("v1", "ConfigMap", "configmaps", true);
        insert("v1", "Secret", "secrets", true);
        insert("v1", "Service", "services", true);
        insert("v1", "Pod", "pods", true);
        insert("apps/v1", "Deployment", "deployments", true);
        insert(
            "gateway.networking.k8s.io/v1",
            "GatewayClass",
            "gatewayclasses",
            false,
        );
        insert("gateway.networking.k8s.io/v1", "Gateway", "gateways", true);
        insert(
            "gateway.networking.k8s.io/v1beta1",
            "ReferenceGrant",
            "referencegrants",
            true,
        );
        insert(
            "gateway.networking.k8s.io/v1",
            "HTTPRoute",
            "httproutes",
            true,
        );
        insert("telemetry.istio.io/v1", "Telemetry", "telemetries", true);

        Self { resources }
    }
}

#[async_trait]
impl ResourceResolver for FakeResolver {
    async fn resolve(
        &self,
        cluster: &ClusterHandle,
        api_version: &str,
        kind: &str,
    ) -> Result<ResolvedResource, FixtureError> {
        self.resources
            .get(&(api_version.to_string(), kind.to_string()))
            .cloned()
            .ok_or_else(|| FixtureError::UnsupportedResource {
                cluster: cluster.spec.name.clone(),
                api_version: api_version.to_string(),
                kind: kind.to_string(),
            })
    }
}

/// One request the mock apiserver observed.
#[derive(Debug)]
struct ObservedRequest {
    method: String,
    uri: String,
    content_type: Option<String>,
    body: serde_json::Value,
}

// =========================================================================
// Step 1: parse and hash everything before applying anything
// =========================================================================

#[test]
fn every_file_is_hashed_and_parsed_before_anything_is_applied() {
    let directory = temp_manifest_dir("plan-hashes");
    let namespace = write_manifest(
        directory.path(),
        "00-namespace.yaml",
        &document("v1", "Namespace", None, "gateway-lab"),
    );
    let gateway = write_manifest(
        directory.path(),
        "10-gateway.yaml",
        &document(
            "gateway.networking.k8s.io/v1",
            "Gateway",
            Some("gateway-lab"),
            "lab-gateway",
        ),
    );

    let plan = plan_gateway_apply(&[namespace.clone(), gateway.clone()]).expect("plan");

    assert_eq!(plan.source_hashes.len(), 2);
    // Real SHA-256 of the real bytes, and the same digest
    // `admissionlab_core::sha256_hex` computes -- not a placeholder and
    // not a hash of a re-serialization of the parsed document.
    assert_eq!(
        plan.source_hashes[&namespace],
        admissionlab_core::sha256_hex(&std::fs::read(&namespace).expect("read"))
    );
    assert_eq!(
        plan.source_hashes[&gateway],
        admissionlab_core::sha256_hex(&std::fs::read(&gateway).expect("read"))
    );
    assert_ne!(
        plan.source_hashes[&namespace], plan.source_hashes[&gateway],
        "two different files must not hash the same"
    );
}

#[test]
fn a_malformed_later_file_fails_the_whole_plan() {
    // The fail-fast property Step 1 exists for: nothing is applied when
    // *any* file is bad, so a cluster is never left holding half a
    // fixture. The malformed file is deliberately last, so a loader that
    // applied as it parsed would already have sent the first two.
    let directory = temp_manifest_dir("plan-malformed");
    let good_one = write_manifest(
        directory.path(),
        "00-namespace.yaml",
        &document("v1", "Namespace", None, "gateway-lab"),
    );
    let good_two = write_manifest(
        directory.path(),
        "10-service.yaml",
        &document("v1", "Service", Some("gateway-lab"), "echo-a"),
    );
    let bad = write_manifest(
        directory.path(),
        "20-routes.yaml",
        "apiVersion: gateway.networking.k8s.io/v1\nkind: HTTPRoute\nmetadata:\n  name: [unclosed\n",
    );

    let error = plan_gateway_apply(&[good_one, good_two, bad.clone()])
        .expect_err("a malformed document must fail the plan");

    match error {
        GatewayError::ManifestParse {
            path,
            document_index,
            format,
            ..
        } => {
            assert_eq!(path, bad);
            assert_eq!(document_index, 0);
            assert_eq!(format, "YAML");
        }
        other => panic!("expected ManifestParse, got {other:?}"),
    }
}

#[test]
fn documents_without_a_usable_identity_are_rejected() {
    let directory = temp_manifest_dir("plan-identity");

    for (name, contents, check) in [
        (
            "no-api-version.yaml",
            "kind: Gateway\nmetadata:\n  name: lab\n",
            "apiVersion",
        ),
        (
            "no-kind.yaml",
            "apiVersion: gateway.networking.k8s.io/v1\nmetadata:\n  name: lab\n",
            "kind",
        ),
        (
            "no-name.yaml",
            "apiVersion: gateway.networking.k8s.io/v1\nkind: Gateway\nmetadata: {}\n",
            "metadata.name",
        ),
        (
            "empty-name.yaml",
            "apiVersion: gateway.networking.k8s.io/v1\nkind: Gateway\nmetadata:\n  name: \"  \"\n",
            "metadata.name",
        ),
    ] {
        let path = write_manifest(directory.path(), name, contents);
        let error = plan_gateway_apply(std::slice::from_ref(&path))
            .expect_err(&format!("{name} must be rejected"));
        match error {
            GatewayError::ManifestMissingField { field, .. } => assert_eq!(field, check),
            other => panic!("{name}: expected ManifestMissingField, got {other:?}"),
        }
    }

    let scalar = write_manifest(directory.path(), "scalar.yaml", "just-a-string\n");
    match plan_gateway_apply(&[scalar]).expect_err("a scalar document must be rejected") {
        GatewayError::ManifestNotAnObject { found, .. } => assert_eq!(found, "a string"),
        other => panic!("expected ManifestNotAnObject, got {other:?}"),
    }

    let generated = write_manifest(
        directory.path(),
        "generate-name.yaml",
        "apiVersion: gateway.networking.k8s.io/v1\nkind: HTTPRoute\nmetadata:\n  \
         generateName: route-\n",
    );
    match plan_gateway_apply(&[generated]).expect_err("generateName must be rejected") {
        GatewayError::ManifestGenerateNameUnsupported { .. } => {}
        other => panic!("expected ManifestGenerateNameUnsupported, got {other:?}"),
    }
}

#[test]
fn a_missing_file_is_reported_by_name() {
    let directory = temp_manifest_dir("plan-missing");
    let missing = directory.path().join("does-not-exist.yaml");

    match plan_gateway_apply(std::slice::from_ref(&missing))
        .expect_err("a missing file must fail the plan")
    {
        GatewayError::ManifestRead { path, .. } => assert_eq!(path, missing),
        other => panic!("expected ManifestRead, got {other:?}"),
    }
}

#[test]
fn duplicate_paths_are_read_and_planned_once() {
    // Mirrors `admissionlab_installer::manifests`'s own deduplication
    // rule. Here it additionally keeps `source_hashes` from depending on
    // how many times a path was repeated.
    let directory = temp_manifest_dir("plan-duplicates");
    let path = write_manifest(
        directory.path(),
        "namespace.yaml",
        &document("v1", "Namespace", None, "gateway-lab"),
    );

    let plan = plan_gateway_apply(&[path.clone(), path.clone(), path]).expect("plan");

    assert_eq!(plan.documents.len(), 1);
    assert_eq!(plan.source_hashes.len(), 1);
}

#[test]
fn a_trailing_empty_yaml_document_is_dropped_without_renumbering() {
    // A file ending in a bare `---` must not produce a spurious null
    // entry, and the surviving documents must keep the indices a user
    // would count to in their editor.
    let directory = temp_manifest_dir("plan-trailing");
    let path = write_manifest(
        directory.path(),
        "two.yaml",
        &format!(
            "{}---\n{}---\n",
            document("v1", "Namespace", None, "gateway-lab"),
            document("v1", "Service", Some("gateway-lab"), "echo-a"),
        ),
    );

    let plan = plan_gateway_apply(&[path]).expect("plan");

    assert_eq!(plan.documents.len(), 2);
    assert_eq!(plan.documents[0].document_index, 0);
    assert_eq!(plan.documents[1].document_index, 1);
}

// =========================================================================
// Step 2: deterministic category ordering
// =========================================================================

#[test]
fn the_category_order_is_the_roadmap_table() {
    // Pins `ApplyCategory::rank` directly, so reordering the enum's
    // variants for readability cannot silently reorder every apply.
    let ordered = [
        ApplyCategory::Namespace,
        ApplyCategory::Configuration,
        ApplyCategory::Service,
        ApplyCategory::Workload,
        ApplyCategory::GatewayClass,
        // Task 8.4 added `IngressClass` and `Ingress`, each beside its
        // Gateway API counterpart. See `apply.rs`'s own "`IngressClass`
        // and `Ingress` are rows in that table too" for why both were
        // added at once and why adding only `Ingress` would have been a
        // regression.
        ApplyCategory::IngressClass,
        ApplyCategory::Gateway,
        ApplyCategory::ReferenceGrant,
        ApplyCategory::HttpRoute,
        ApplyCategory::Ingress,
        ApplyCategory::Unknown,
    ];
    for (position, category) in ordered.iter().enumerate() {
        assert_eq!(
            usize::from(category.rank()),
            position,
            "{category:?} is out of position"
        );
    }

    for (kind, expected) in [
        ("Namespace", ApplyCategory::Namespace),
        ("Secret", ApplyCategory::Configuration),
        ("ConfigMap", ApplyCategory::Configuration),
        ("Service", ApplyCategory::Service),
        ("Deployment", ApplyCategory::Workload),
        ("Pod", ApplyCategory::Workload),
        ("GatewayClass", ApplyCategory::GatewayClass),
        ("IngressClass", ApplyCategory::IngressClass),
        ("Gateway", ApplyCategory::Gateway),
        ("ReferenceGrant", ApplyCategory::ReferenceGrant),
        ("HTTPRoute", ApplyCategory::HttpRoute),
        ("Ingress", ApplyCategory::Ingress),
        ("Telemetry", ApplyCategory::Unknown),
        // Case-sensitive: a Kubernetes `kind` is, so `httproute` is not
        // a spelling of `HTTPRoute` and must not sort as one.
        ("httproute", ApplyCategory::Unknown),
        ("ingress", ApplyCategory::Unknown),
    ] {
        assert_eq!(ApplyCategory::for_kind(kind), expected, "kind {kind}");
    }
}

#[test]
fn documents_are_reordered_into_category_order_regardless_of_source_order() {
    // The file is written in *exactly reverse* dependency order, which
    // is the shape that makes an unordered installer produce a
    // route with `Accepted: False` / `NoMatchingParent` on one side and
    // not the other.
    let directory = temp_manifest_dir("order-reverse");
    let path = write_manifest(
        directory.path(),
        "all.yaml",
        &[
            document(
                "gateway.networking.k8s.io/v1",
                "HTTPRoute",
                Some("gateway-lab"),
                "echo-a",
            ),
            document(
                "gateway.networking.k8s.io/v1beta1",
                "ReferenceGrant",
                Some("backends"),
                "allow-lab",
            ),
            document(
                "gateway.networking.k8s.io/v1",
                "Gateway",
                Some("gateway-lab"),
                "lab-gateway",
            ),
            document(
                "gateway.networking.k8s.io/v1",
                "GatewayClass",
                None,
                "istio",
            ),
            document("apps/v1", "Deployment", Some("gateway-lab"), "echo-a"),
            document("v1", "Service", Some("gateway-lab"), "echo-a"),
            document("v1", "ConfigMap", Some("gateway-lab"), "echo-a-config"),
            document("v1", "Namespace", None, "gateway-lab"),
        ]
        .join("---\n"),
    );

    let plan = plan_gateway_apply(&[path]).expect("plan");

    assert_eq!(
        plan.documents
            .iter()
            .map(|planned| planned.kind.as_str())
            .collect::<Vec<_>>(),
        [
            "Namespace",
            "ConfigMap",
            "Service",
            "Deployment",
            "GatewayClass",
            "Gateway",
            "ReferenceGrant",
            "HTTPRoute",
        ],
        "documents must be sorted into the roadmap's category order, not left in source order"
    );
}

#[test]
fn unknown_kinds_are_applied_last_in_source_order() {
    // Pins the reading of "unknown kinds preserve source order after
    // known prerequisites" chosen in `apply.rs`'s documentation: *after
    // every known category*, and among themselves in source order.
    let directory = temp_manifest_dir("order-unknown");
    let path = write_manifest(
        directory.path(),
        "all.yaml",
        &[
            document(
                "telemetry.istio.io/v1",
                "Telemetry",
                Some("gateway-lab"),
                "second-written-first",
            ),
            document(
                "gateway.networking.k8s.io/v1",
                "HTTPRoute",
                Some("gateway-lab"),
                "echo-a",
            ),
            document(
                "telemetry.istio.io/v1",
                "Telemetry",
                Some("gateway-lab"),
                "written-later",
            ),
            document("v1", "Namespace", None, "gateway-lab"),
        ]
        .join("---\n"),
    );

    let plan = plan_gateway_apply(&[path]).expect("plan");

    assert_eq!(
        plan.documents
            .iter()
            .map(|planned| (planned.kind.as_str(), planned.name.as_str()))
            .collect::<Vec<_>>(),
        [
            ("Namespace", "gateway-lab"),
            ("HTTPRoute", "echo-a"),
            ("Telemetry", "second-written-first"),
            ("Telemetry", "written-later"),
        ],
        "unknown kinds go after every known category, keeping their own source order"
    );
}

#[test]
fn ties_within_a_category_keep_file_then_document_order() {
    // The stability half of the sort: three `Service`s split across two
    // files must come out in file order, then in-file order.
    let directory = temp_manifest_dir("order-stable");
    let first = write_manifest(
        directory.path(),
        "00-first.yaml",
        &format!(
            "{}---\n{}",
            document("v1", "Service", Some("gateway-lab"), "a"),
            document("v1", "Service", Some("gateway-lab"), "b"),
        ),
    );
    let second = write_manifest(
        directory.path(),
        "10-second.yaml",
        &document("v1", "Service", Some("gateway-lab"), "c"),
    );

    let plan = plan_gateway_apply(&[first, second]).expect("plan");

    assert_eq!(
        plan.documents
            .iter()
            .map(|planned| planned.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
}

// =========================================================================
// Step 3: the requests that actually go out
// =========================================================================

/// Runs `plan` through [`apply_gateway_plan_with_client`] against a
/// mocked apiserver that answers every request with `response_for`,
/// and returns the applied fixture (or its error) alongside every
/// request the mock observed, in order.
async fn apply_against_mock(
    plan: &admissionlab_gateway::GatewayApplyPlan,
    mut response_for: impl FnMut(usize, &ObservedRequest) -> Response<Body> + Send + 'static,
) -> (
    Result<admissionlab_gateway::AppliedGatewayFixture, GatewayError>,
    Vec<ObservedRequest>,
) {
    let (mock_service, handle) = mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(mock_service, "default");
    let expected = plan.documents.len();

    let responder = tokio::spawn(async move {
        let mut handle = pin!(handle);
        let mut observed = Vec::new();
        for index in 0..expected {
            let Some((request, send)) = handle.next_request().await else {
                break;
            };
            let (parts, body) = request.into_parts();
            let bytes = body
                .collect()
                .await
                .expect("collect request body")
                .to_bytes();
            let request = ObservedRequest {
                method: parts.method.to_string(),
                uri: parts.uri.to_string(),
                content_type: parts
                    .headers
                    .get(http::header::CONTENT_TYPE)
                    .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned()),
                body: if bytes.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::from_slice(&bytes).expect("request body is JSON")
                },
            };
            send.send_response(response_for(index, &request));
            observed.push(request);
        }
        observed
    });

    let cluster = cluster_handle();
    let resolver = FakeResolver::gateway_api();
    let applied = apply_gateway_plan_with_client(&cluster, &resolver, client, plan).await;
    let observed = responder.await.expect("mock responder task must not panic");
    (applied, observed)
}

/// Asserts one observed request is a forced server-side apply carrying
/// this project's own field manager, and is not a dry run.
///
/// Server-side apply is identified on the wire by its content type, not
/// by the method alone (a JSON-merge or strategic-merge patch is also a
/// `PATCH`), which is why all four properties are checked together.
fn assert_is_forced_server_side_apply(request: &ObservedRequest) {
    assert_eq!(request.method, "PATCH");
    assert_eq!(
        request.content_type.as_deref(),
        Some("application/apply-patch+yaml"),
        "a server-side apply is identified by its content type"
    );
    assert!(
        request.uri.contains(&format!(
            "fieldManager={}",
            admissionlab_gateway::FIELD_MANAGER
        )),
        "every apply must carry the project's own field manager, got {}",
        request.uri
    );
    assert!(
        request.uri.contains("force=true"),
        "conflicts are forced in a disposable cluster, got {}",
        request.uri
    );
    assert!(
        !request.uri.contains("dryRun"),
        "Gateway fixtures are persisted, never dry-run, got {}",
        request.uri
    );
}

/// A 200 response echoing back an object with `name`, which is all
/// `Api::patch` needs to deserialize a `DynamicObject`.
fn ok_object(api_version: &str, kind: &str, name: &str) -> Response<Body> {
    let body = serde_json::json!({
        "apiVersion": api_version,
        "kind": kind,
        "metadata": {"name": name},
    });
    Response::new(Body::from(
        serde_json::to_vec(&body).expect("serialize response"),
    ))
}

#[tokio::test]
async fn objects_are_applied_in_category_order_as_forced_server_side_applies() {
    let directory = temp_manifest_dir("apply-order");
    let path = write_manifest(
        directory.path(),
        "all.yaml",
        &[
            document(
                "gateway.networking.k8s.io/v1",
                "HTTPRoute",
                Some("gateway-lab"),
                "echo-a",
            ),
            document(
                "gateway.networking.k8s.io/v1",
                "Gateway",
                Some("gateway-lab"),
                "lab-gateway",
            ),
            document(
                "gateway.networking.k8s.io/v1",
                "GatewayClass",
                None,
                "istio",
            ),
            document("v1", "Namespace", None, "gateway-lab"),
        ]
        .join("---\n"),
    );
    let plan = plan_gateway_apply(&[path]).expect("plan");

    let (applied, observed) = apply_against_mock(&plan, |index, _| match index {
        0 => ok_object("v1", "Namespace", "gateway-lab"),
        1 => ok_object("gateway.networking.k8s.io/v1", "GatewayClass", "istio"),
        2 => ok_object("gateway.networking.k8s.io/v1", "Gateway", "lab-gateway"),
        _ => ok_object("gateway.networking.k8s.io/v1", "HTTPRoute", "echo-a"),
    })
    .await;
    let applied = applied.expect("every apply succeeded");

    // Request order is the category order, not the source order.
    let paths: Vec<&str> = observed
        .iter()
        .map(|request| {
            request
                .uri
                .split('?')
                .next()
                .expect("split always yields one element")
        })
        .collect();
    assert_eq!(
        paths,
        [
            "/api/v1/namespaces/gateway-lab",
            "/apis/gateway.networking.k8s.io/v1/gatewayclasses/istio",
            "/apis/gateway.networking.k8s.io/v1/namespaces/gateway-lab/gateways/lab-gateway",
            "/apis/gateway.networking.k8s.io/v1/namespaces/gateway-lab/httproutes/echo-a",
        ],
        "cluster-scoped objects must not carry a /namespaces/ segment, and the plural must be \
         the one the resolver reported"
    );

    for request in &observed {
        assert_is_forced_server_side_apply(request);
    }

    // Bodies are the fixture documents verbatim -- no injected label,
    // annotation, or namespace field.
    assert_eq!(
        observed[0].body,
        serde_json::json!({
            "apiVersion": "v1",
            "kind": "Namespace",
            "metadata": {"name": "gateway-lab"},
        }),
        "the applied body must be exactly the document the user wrote"
    );

    // And the recorded identities use the resolver's plurals and the
    // request's own namespace (absent for cluster-scoped objects).
    assert_eq!(
        applied
            .objects
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        [
            "v1 namespaces gateway-lab",
            "gateway.networking.k8s.io/v1 gatewayclasses istio",
            "gateway.networking.k8s.io/v1 gateways gateway-lab/lab-gateway",
            "gateway.networking.k8s.io/v1 httproutes gateway-lab/echo-a",
        ]
    );
    assert_eq!(applied.source_hashes, plan.source_hashes);
}

#[tokio::test]
async fn a_namespaced_object_without_a_namespace_targets_default() {
    // The same fallback `admissionlab_fixtures::execute::namespace_of`
    // documents, and it must be a *read*: the body must not gain a
    // `metadata.namespace` the user never wrote.
    let directory = temp_manifest_dir("apply-default-ns");
    let path = write_manifest(
        directory.path(),
        "service.yaml",
        &document("v1", "Service", None, "echo-a"),
    );
    let plan = plan_gateway_apply(&[path]).expect("plan");

    let (applied, observed) =
        apply_against_mock(&plan, |_, _| ok_object("v1", "Service", "echo-a")).await;
    let applied = applied.expect("apply succeeded");

    assert!(
        observed[0]
            .uri
            .starts_with("/api/v1/namespaces/default/services/echo-a"),
        "got {}",
        observed[0].uri
    );
    assert_eq!(
        observed[0].body,
        serde_json::json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {"name": "echo-a"},
        }),
        "the default namespace is read, never written into the object"
    );
    assert_eq!(
        applied.objects[0].namespace.as_deref(),
        Some("default"),
        "the recorded identity names the namespace the request actually targeted"
    );
}

#[tokio::test]
async fn a_refused_object_stops_the_suite_and_carries_the_api_servers_own_words() {
    // The line `error.rs` documents: a refusal is an error here (there
    // is no fixture to reconcile), but the API server's own
    // code/reason/message survive so a later comparator has the real
    // answer rather than a boolean.
    let directory = temp_manifest_dir("apply-refused");
    let path = write_manifest(
        directory.path(),
        "all.yaml",
        &format!(
            "{}---\n{}",
            document("v1", "Namespace", None, "gateway-lab"),
            document(
                "gateway.networking.k8s.io/v1",
                "Gateway",
                Some("gateway-lab"),
                "lab-gateway",
            ),
        ),
    );
    let plan = plan_gateway_apply(&[path]).expect("plan");

    let (applied, observed) = apply_against_mock(&plan, |index, _| {
        if index == 0 {
            return ok_object("v1", "Namespace", "gateway-lab");
        }
        let status = serde_json::json!({
            "kind": "Status",
            "apiVersion": "v1",
            "status": "Failure",
            "message": "admission webhook \"policy.example\" denied the request: \
                        gateways must set spec.gatewayClassName",
            "reason": "Forbidden",
            "code": 403,
        });
        Response::builder()
            .status(403)
            .body(Body::from(
                serde_json::to_vec(&status).expect("serialize status"),
            ))
            .expect("build response")
    })
    .await;

    match applied.expect_err("a refused apply must fail the suite") {
        GatewayError::ApplyRejected {
            cluster,
            object,
            code,
            reason,
            message,
        } => {
            assert_eq!(cluster, "gateway-apply-test-cluster");
            assert_eq!(
                object,
                "gateway.networking.k8s.io/v1 gateways gateway-lab/lab-gateway"
            );
            assert_eq!(code, Some(403));
            assert_eq!(reason.as_deref(), Some("Forbidden"));
            assert!(
                message.contains("gateways must set spec.gatewayClassName"),
                "the API server's own message must survive verbatim, got {message:?}"
            );
        }
        other => panic!("expected ApplyRejected, got {other:?}"),
    }

    assert_eq!(
        observed.len(),
        2,
        "the suite stops at the refusal; no later object is sent"
    );
    // And nothing is deleted: no DELETE was ever issued for the
    // namespace that *did* apply. Cluster teardown is the cleanup.
    assert!(
        observed.iter().all(|request| request.method != "DELETE"),
        "a partial failure must not delete what was already applied"
    );
}

#[tokio::test]
async fn a_kind_the_cluster_does_not_serve_fails_before_any_request() {
    let directory = temp_manifest_dir("apply-unknown-kind");
    let path = write_manifest(
        directory.path(),
        "custom.yaml",
        &document("example.com/v1", "NotServedHere", Some("gateway-lab"), "x"),
    );
    let plan = plan_gateway_apply(&[path]).expect("plan");

    let (applied, observed) = apply_against_mock(&plan, |_, _| {
        panic!("no request should be issued for an unresolvable kind")
    })
    .await;

    match applied.expect_err("an unresolvable kind must fail") {
        GatewayError::ResourceResolution(FixtureError::UnsupportedResource {
            api_version,
            kind,
            ..
        }) => {
            assert_eq!(api_version, "example.com/v1");
            assert_eq!(kind, "NotServedHere");
        }
        other => panic!("expected ResourceResolution, got {other:?}"),
    }
    assert!(observed.is_empty());
}
