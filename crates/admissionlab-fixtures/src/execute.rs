//! Issuing one server-side dry-run CREATE for a fixture against a real
//! cluster (Task 3.4's low-level half).
//!
//! [`dry_run_create`] is this module's entry point: given a
//! [`ClusterHandle`], a [`ResolvedResource`] (Task 3.2's own real-cluster
//! resolution of the fixture's `apiVersion`/`kind`), and a
//! [`FixtureSource`], it issues exactly one Kubernetes `CREATE` with
//! `dryRun=All` and reports back what the API server actually said --
//! the admitted/mutated object on success, or the `Status` object the
//! API server returned on failure -- plus any `Warning` response headers
//! and how long the request took.
//!
//! # Client construction is untestable without a real cluster; the request/response exchange is not
//!
//! Mirrors the split `admissionlab_installer::readiness` and this
//! crate's own `resources.rs` already document and use, for the
//! identical reason: [`dry_run_create`] itself turns `cluster`'s own
//! on-disk kubeconfig into a real, network-connecting `kube::Client` via
//! [`crate::resources::client_for`], so it has no seam to swap in a fake
//! backend and is exercised live only by an end-to-end exit gate.
//! Everything downstream of an already-built `Client` -- building the
//! request, sending it, reading `Warning` headers, decoding the response
//! -- is [`dry_run_create_with_client`], which takes a `Client` directly
//! and is what `admissionlab-admission`'s `tests/execute_unit.rs`
//! drives against a `tower_test::mock`-backed one (Task 3.4 brief Step
//! 1's "verify the request the mock actually receives"). `dry_run_create`
//! is a thin wrapper: build the client, delegate.
//!
//! # This module never decides what a rejection *means*
//!
//! A non-2xx response is still a **successful observation**: the API
//! server was reached, and it returned a real, structured answer. This
//! module reports that answer (via [`DryRunCreateResponse::result`])
//! without interpreting it -- `admissionlab_admission::execute`'s job,
//! not this crate's (Controller supplement §2, Task 3.4: the trait that
//! classifies belongs in `admissionlab-admission`, which is *why*
//! `admission -> fixtures` is the dependency edge this task adds, not
//! the reverse). This module's own [`FixtureError::ReplayUnavailable`]
//! is reserved for the different case where no such answer could be
//! obtained at all -- see that variant's own documentation.
//!
//! # The fixture object is sent byte-for-byte, never annotated
//!
//! [`dry_run_create`] serializes [`FixtureSource::object`] exactly as
//! discovered -- no correlation label, no annotation, no injected field
//! of any kind. Controller supplement §3 (Task 3.4) explains why this is
//! a correctness rule, not a style preference: anything this module
//! added to the object would change what the webhooks under test
//! actually see, so admission-lab would end up comparing behavior on an
//! object the user never wrote. A later task (3.7) correlates a request
//! to its audit-log evidence without touching the object at all (serial
//! execution plus audit-log fields).
//!
//! # Namespace: the fixture's own `metadata.namespace`, or `"default"`
//!
//! [`ResolvedResource::namespaced`] says *whether* a namespace is
//! needed, not *which* one. This module reads
//! `FixtureSource::object`'s own `metadata.namespace` when present and
//! falls back to `"default"` otherwise -- the same default a plain
//! `kubectl apply -f fixture.yaml` (no `-n` flag, no namespace in the
//! manifest) would resolve to against an ordinary kubeconfig context.
//! This reads the field, it never writes one: the object sent to the API
//! server is untouched (see above), so a fixture that omits
//! `metadata.namespace` is still admitted into a real namespace, exactly
//! as a user replaying it by hand would see.
//!
//! # Warning headers: captured via `Client::send`, not guessed
//!
//! `Api::create`/`Client::request` throw away the HTTP response (and
//! therefore its headers) as soon as they have a deserialized body --
//! confirmed by tracing `kube-client-4.2.0/src/client/mod.rs`'s own
//! `Api::create` -> `Client::request` -> `Client::request_text` ->
//! `Client::send` chain (`research-kube-api.md` §6). `Client::send` is
//! the public escape hatch that returns the raw `http::Response<Body>`,
//! so this module builds the request the same way `Api::create` does
//! internally (`kube::core::Request::create`) and calls `Client::send`
//! directly, reading `Warning` header values off the real response
//! before consuming its body. This needs no `unsafe` code and
//! duplicates no private `kube` internals beyond the same ~3-line
//! non-2xx-body-to-`Status` decode `handle_api_errors` (a private
//! function) performs, using only `kube::core::Status`'s own public
//! `Deserialize` impl and fallback constructor
//! (`Status::failure(..).with_code(..)`) -- confirmed live against a
//! real `kind` cluster: a deprecated CRD version's dry-run CREATE
//! returned exactly one `Warning: 299 - "<message>"` header, captured
//! by this same `Client::send`-based approach (see this task's report
//! for the transcript). [`DryRunCreateResponse::warnings`] holds each
//! header's raw value text, decoded with [`String::from_utf8_lossy`]
//! rather than [`http::HeaderValue::to_str`] -- see that field's own
//! documentation for why this distinction matters (found in review: an
//! earlier version used `to_str`, which silently dropped any header
//! that was not valid UTF-8, making an empty `Vec` ambiguous between
//! "observed zero" and "one arrived malformed and was discarded"). No
//! RFC 7234 `warn-code`/`warn-agent` unwrapping either way.

use std::time::{Duration, Instant, SystemTime};

use admissionlab_core::ClusterHandle;
use http::header::WARNING;
use http_body_util::BodyExt;
use kube::Api;
use kube::Client;
use kube::api::PostParams;
use kube::client::Body;
use kube::core::{DynamicObject, Request as KubeRequest, Status};

use crate::FixtureError;
use crate::discover::FixtureSource;
use crate::resources::{ResolvedResource, client_for};

/// The `PostParams::field_manager` every dry-run CREATE this module
/// issues identifies itself with. Never persisted (every request is
/// `dryRun=All`), so this has no lasting field-ownership consequence --
/// it exists only because `PostParams` requires *some* manager name, and
/// a fixed, project-identifying one is more useful in apiserver-side
/// diagnostics than `kube`'s own default.
const FIELD_MANAGER: &str = "admissionlab";

/// What one server-side dry-run CREATE attempt produced: a real answer
/// from the API server (admitted object, or the `Status` it rejected the
/// request with), plus the HTTP-level evidence
/// [`admissionlab_admission::execute`] and later tasks need but this
/// module does not itself interpret.
///
/// Deliberately not `admissionlab_admission::execute::RawAdmissionResponse`:
/// that type's `decision: AdmissionDecision` requires classifying
/// `result` below, which is this crate's downstream dependent's job, not
/// this crate's own (see this module's documentation).
#[derive(Debug, Clone)]
pub struct DryRunCreateResponse {
    /// `Ok` with the object the API server reports it would have
    /// persisted (a 2xx dry-run CREATE response), or `Err` with the
    /// `Status` object the API server returned for a non-2xx response.
    /// Both are real, successfully-obtained answers -- see this
    /// module's documentation for why a rejection is not itself a
    /// [`FixtureError`].
    pub result: Result<serde_json::Value, Status>,
    /// `Warning` HTTP response header values, in the order the API
    /// server sent them, decoded with [`String::from_utf8_lossy`] --
    /// verbatim for a well-formed (UTF-8) header value, with U+FFFD
    /// substituted only for a byte sequence that was not valid UTF-8
    /// (RFC 7234 `warning-value` technically permits arbitrary
    /// `obs-text`, though a real Kubernetes apiserver has not been
    /// observed to send one). No RFC 7234 `warn-code`/`warn-agent`
    /// unwrapping either way. Every header this response carried is
    /// represented by exactly one entry here -- none is ever dropped,
    /// so an empty `Vec` here means "observed zero", not "not
    /// captured" or "one was malformed and silently discarded" (found
    /// in review: an earlier version used `HeaderValue::to_str`, which
    /// returns `None` -- and was then filtered out -- for exactly the
    /// non-UTF-8 case this lossy decode now keeps).
    pub warnings: Vec<String>,
    /// Wall-clock time from just before the request was sent to just
    /// after its response finished arriving. Measured with
    /// [`std::time::Instant`] internally and converted to a
    /// [`Duration`] here; never derived by subtracting
    /// [`DryRunCreateResponse::request_finished_at`] from
    /// [`DryRunCreateResponse::request_started_at`], which (being wall
    /// clock) is not guaranteed monotonic.
    pub elapsed: Duration,
    /// Wall-clock time just before the request was sent.
    pub request_started_at: SystemTime,
    /// Wall-clock time just after the response finished arriving
    /// (headers and body both read).
    pub request_finished_at: SystemTime,
}

/// Issues one server-side dry-run CREATE for `fixture` against
/// `resource` on `cluster` and reports the API server's real response.
/// Resolves `cluster`'s own kubeconfig into a real `kube::Client` (via
/// [`crate::resources::client_for`]) and delegates to
/// [`dry_run_create_with_client`] for everything else -- see this
/// module's documentation ("Client construction is untestable...") for
/// why this split exists, and that function's own documentation for the
/// request's exact shape.
///
/// # Errors
///
/// Returns [`FixtureError::ReplayUnavailable`] if `cluster`'s kubeconfig
/// could not be turned into a usable client, or (via
/// [`dry_run_create_with_client`]) for any of that function's own error
/// cases.
pub async fn dry_run_create(
    cluster: &ClusterHandle,
    resource: &ResolvedResource,
    fixture: &FixtureSource,
) -> Result<DryRunCreateResponse, FixtureError> {
    let client = client_for(cluster)
        .await
        .map_err(|source| FixtureError::ReplayUnavailable {
            cluster: cluster.spec.name.clone(),
            reason: source.to_string(),
        })?;
    dry_run_create_with_client(client, &cluster.spec.name, resource, fixture).await
}

/// [`dry_run_create`]'s offline-testable core: given an already-built
/// `client`, issues the dry-run CREATE and reports the API server's real
/// response. `cluster_name` is used only to label a
/// [`FixtureError::ReplayUnavailable`] if this fails -- it is never used
/// to build or look up the client itself, which is `client`'s job
/// entirely (this function never touches a kubeconfig or the
/// filesystem).
///
/// See this module's documentation for the request's exact shape (no
/// annotation/label added, namespace fallback, warning capture) and for
/// why a rejection is not itself an `Err` here.
///
/// # Errors
///
/// Returns [`FixtureError::ReplayUnavailable`] if this could not obtain
/// *any* real response from the API server: the request could not be
/// built or `fixture.object` could not be serialized, the
/// request/response exchange failed at the transport level, or a
/// successful (2xx) response body did not decode as JSON at all
/// (something no real kube-apiserver produces). See
/// [`FixtureError::ReplayUnavailable`]'s own documentation for why this
/// is never a decision about the fixture.
pub async fn dry_run_create_with_client(
    client: Client,
    cluster_name: &str,
    resource: &ResolvedResource,
    fixture: &FixtureSource,
) -> Result<DryRunCreateResponse, FixtureError> {
    let unavailable = |reason: String| FixtureError::ReplayUnavailable {
        cluster: cluster_name.to_string(),
        reason,
    };

    let api: Api<DynamicObject> = if resource.namespaced {
        Api::namespaced_with(
            client.clone(),
            &namespace_of(fixture),
            &resource.api_resource,
        )
    } else {
        Api::all_with(client.clone(), &resource.api_resource)
    };

    let post_params = PostParams {
        dry_run: true,
        field_manager: Some(FIELD_MANAGER.to_string()),
    };
    let body_bytes =
        serde_json::to_vec(&fixture.object).map_err(|source| unavailable(source.to_string()))?;
    let request = KubeRequest::new(api.resource_url())
        .create(&post_params, body_bytes)
        .map_err(|source| unavailable(source.to_string()))?
        .map(Body::from);

    let request_started_at = SystemTime::now();
    let started = Instant::now();
    let response = client
        .send(request)
        .await
        .map_err(|source| unavailable(source.to_string()))?;
    let elapsed = started.elapsed();
    let request_finished_at = SystemTime::now();

    let warnings = response
        .headers()
        .get_all(WARNING)
        .iter()
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .collect();

    let status_code = response.status();
    let response_body = response
        .into_body()
        .collect()
        .await
        .map_err(|source| unavailable(source.to_string()))?
        .to_bytes();

    let result = if status_code.is_success() {
        let admitted: serde_json::Value = serde_json::from_slice(&response_body)
            .map_err(|source| unavailable(source.to_string()))?;
        Ok(admitted)
    } else {
        // Mirrors `kube-client-4.2.0/src/client/mod.rs`'s private
        // `handle_api_errors`: parse the body as a Kubernetes `Status`
        // object, falling back to a reconstructed `Status` (using the
        // real HTTP status code) if the body was not one -- something a
        // real kube-apiserver is not expected to send, but not assumed
        // impossible. Either way this is `Ok` from this function's own
        // point of view: a real response was obtained.
        let status = serde_json::from_slice::<Status>(&response_body).unwrap_or_else(|_| {
            let text = String::from_utf8_lossy(&response_body);
            Status::failure(&text, "Failed to parse error data").with_code(status_code.as_u16())
        });
        Err(status)
    };

    Ok(DryRunCreateResponse {
        result,
        warnings,
        elapsed,
        request_started_at,
        request_finished_at,
    })
}

/// The namespace `fixture.object`'s own `metadata.namespace` names, or
/// `"default"` if absent/empty/not-a-string. See this module's
/// documentation ("Namespace") for why this is a *read*, not a write --
/// `fixture.object` itself is never mutated to add one.
///
/// `pub` as of Task 3.10, for one reason: `admissionlab_admission::capture`
/// has to build the audit `objectRef` key it correlates this request
/// against, and an audit event's `objectRef.namespace` is whatever
/// namespace the request *URL* named -- which is exactly what this
/// function decides, `"default"` fallback included. Re-deriving that
/// rule at the correlation site would put the same fallback in two
/// places, where a change to one silently stops every fixture that omits
/// `metadata.namespace` from correlating at all.
#[must_use]
pub fn namespace_of(fixture: &FixtureSource) -> String {
    fixture
        .object
        .get("metadata")
        .and_then(|metadata| metadata.get("namespace"))
        .and_then(serde_json::Value::as_str)
        .filter(|namespace| !namespace.is_empty())
        .unwrap_or("default")
        .to_string()
}

// =========================================================================
// What is, and is not, covered without a live cluster
//
// `namespace_of` is pure and synchronous -- no cluster access -- and is
// covered directly below. `dry_run_create_with_client` -- everything
// this module does once a `Client` already exists -- is covered by
// `admissionlab-admission`'s `tests/execute_unit.rs`, against a
// `tower_test::mock`-backed `Client` (the same technique
// `admissionlab-installer/src/readiness.rs` and this crate's own
// `resources.rs` already use). It lives in that other crate's test
// suite, not this crate's, because Task 3.4 brief Step 1 asks for the
// request-shape assertion ("the outgoing request is a CREATE carrying
// dryRun=All") to prove out the behavior `admissionlab_admission::execute::KubeAdmissionExecutor`
// actually depends on, and one shared mock exchange there covers both
// this function's request-building and that crate's own
// classification, rather than two separate, drifting copies.
//
// NOT covered anywhere without a live cluster, and left for a live exit
// gate: whether `client_for` genuinely connects using a real
// `kind`-produced kubeconfig (see `resources.rs`'s own identical scope
// note, and `dry_run_create`'s own thin wrapper around it, which is
// exercised only via its error path -- a missing kubeconfig -- in this
// module's own `tests` below). A real kube-apiserver's `Warning` header
// formatting was, however, checked live once against a real `kind`
// cluster, not merely assumed reachable -- see this module's
// documentation and this task's report for the transcript.
// =========================================================================
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use admissionlab_core::{ClusterSpec, FixtureId, RunId, Side};
    use kube::core::{ApiResource, GroupVersion};
    use serde_json::json;

    use super::{ClusterHandle, dry_run_create, namespace_of};
    use crate::FixtureError;
    use crate::discover::FixtureSource;
    use crate::resources::ResolvedResource;

    fn fixture_with_object(object: serde_json::Value) -> FixtureSource {
        FixtureSource {
            id: FixtureId::parse("test-fixture-0").expect("valid FixtureId"),
            path: std::path::PathBuf::from("fixture.yaml"),
            document_index: 0,
            sha256: "0".repeat(64),
            object,
        }
    }

    /// A fresh, guaranteed-unique path under the OS temp dir, for one
    /// test's kubeconfig -- nothing is ever actually written there;
    /// mirrors `resources.rs`'s own identical helper.
    fn unique_path(label: &str) -> PathBuf {
        let unique = RunId::generate();
        std::env::temp_dir().join(format!(
            "admissionlab-fixtures-execute-test-{label}-{}.yaml",
            unique.as_str()
        ))
    }

    /// A minimal, otherwise-valid [`ClusterHandle`] pointing at
    /// `kubeconfig`. Mirrors `resources.rs`'s own identical helper.
    fn cluster_handle_with_kubeconfig(kubeconfig: PathBuf) -> ClusterHandle {
        ClusterHandle {
            spec: ClusterSpec {
                side: Side::Baseline,
                name: "execute-test-cluster".to_string(),
                kubernetes_version: "1.36.0".to_string(),
                node_image: "kindest/node:v1.36.0".to_string(),
            },
            kubeconfig,
            audit_log: std::env::temp_dir().join("admissionlab-fixtures-execute-test-audit.log"),
        }
    }

    #[tokio::test]
    async fn dry_run_create_wraps_a_client_for_failure_as_replay_unavailable() {
        // Fails if `dry_run_create` swallowed `client_for`'s error,
        // reported a different `FixtureError` variant, or attempted the
        // request anyway against a client that could not be built.
        let cluster = cluster_handle_with_kubeconfig(unique_path("missing"));
        let resource = ResolvedResource {
            api_resource: ApiResource::from_gvk(
                &"v1".parse::<GroupVersion>().unwrap().with_kind("ConfigMap"),
            ),
            namespaced: true,
        };
        let fixture = fixture_with_object(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {"name": "demo"},
        }));

        let error = dry_run_create(&cluster, &resource, &fixture)
            .await
            .expect_err("a nonexistent kubeconfig must not succeed");

        assert!(
            matches!(error, FixtureError::ReplayUnavailable { .. }),
            "expected ReplayUnavailable, got {error:?}"
        );
    }

    #[test]
    fn namespace_of_reads_the_fixtures_own_namespace() {
        // Fails if this read the resolver's cluster-default namespace,
        // or any other source, instead of the fixture object's own
        // `metadata.namespace`.
        let fixture = fixture_with_object(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {"name": "demo", "namespace": "kyverno-test"},
        }));
        assert_eq!(namespace_of(&fixture), "kyverno-test");
    }

    #[test]
    fn namespace_of_falls_back_to_default_when_absent() {
        let fixture = fixture_with_object(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {"name": "demo"},
        }));
        assert_eq!(namespace_of(&fixture), "default");
    }

    #[test]
    fn namespace_of_falls_back_to_default_when_empty() {
        // Distinguishes "read the field and it happened to be empty"
        // from "read the field correctly" -- an implementation that
        // used an empty string verbatim (rather than falling back)
        // would build an `Api` against an invalid empty namespace path.
        let fixture = fixture_with_object(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {"name": "demo", "namespace": ""},
        }));
        assert_eq!(namespace_of(&fixture), "default");
    }
}
