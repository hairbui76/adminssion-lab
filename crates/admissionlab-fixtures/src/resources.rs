//! Resolving a fixture document's `apiVersion`/`kind` to a real
//! Kubernetes API resource on a specific cluster (Task 3.2).
//!
//! [`discover::discover_fixtures`](crate::discover::discover_fixtures)
//! (Task 3.1) only reads a fixture document's raw JSON/YAML; it never
//! asks a cluster what that document's `apiVersion`/`kind` actually
//! *means* there. This module is that next step: [`ResourceResolver::resolve`]
//! turns `(api_version, kind)` into a [`ResolvedResource`] -- the
//! `kube::core::ApiResource` a dynamic `Api<DynamicObject>` needs, plus
//! whether the resource is namespaced -- so a later task can replay an
//! arbitrary user fixture (including a CRD Kyverno or Istio installs)
//! without asking `kubectl`/`kube`'s own plural-guessing heuristic
//! ([`kube::core::ApiResource::from_gvk`]) to guess. Guessing is
//! exactly what Global Constraint 15 forbids for missing data, and a
//! guessed plural is observably wrong for plenty of real CRDs (see that
//! function's own doc comment: "for CRDs with complex pluralisations it
//! can fail").
//!
//! # Client construction is untestable without a real cluster; discovery is not
//!
//! Mirrors the split `admissionlab_installer::readiness` already
//! documents and uses, for the identical reason: [`client_for`] turns a
//! [`admissionlab_core::ClusterHandle`]'s on-disk kubeconfig into a real,
//! network-connecting `kube::Client` -- there is no seam to swap in a
//! fake backend there, so it is exercised only via its own error paths
//! (`tests/resources.rs`, against an intentionally missing kubeconfig)
//! and, live, by whatever end-to-end exit gate later covers fixture
//! replay. Everything downstream of an already-built `Client` --
//! running [`kube::discovery::Discovery`] and matching a `(group,
//! version, kind)` against what it found -- is ordinary async code with
//! no direct I/O of its own, so [`resolve_against`] takes a `&Discovery`
//! directly and is unit-tested (this module's own `tests`) against a
//! `Discovery` built from a `tower_test::mock`-backed `Client`, not a
//! live cluster.
//!
//! # Caching (Step 3) and its invalidation (controller supplement §4)
//!
//! [`KubeResourceResolver`] runs [`kube::discovery::Discovery`] at most
//! once per cluster: [`KubeResourceResolver::resolve`] keys a cache on
//! [`admissionlab_core::ClusterHandle::kubeconfig`] -- a plain `PathBuf`,
//! so it is a usable cache key without needing `ClusterHandle` itself to
//! implement `Ord`/`Hash` -- and only runs discovery on a cache miss. A
//! `BTreeMap` is used for that cache, not a `HashMap`, matching this
//! crate's blanket rule (see the crate root's documentation) rather than
//! arguing this particular cache is exempt from it.
//!
//! **What that key actually guarantees, precisely (found in review):**
//! [`admissionlab_core::ClusterHandle::kubeconfig`]'s own documentation
//! only promises two things -- never the operator's ambient
//! `~/.kube/config`, and never shared between a baseline and a candidate
//! cluster from the same run. It does not promise global uniqueness or
//! stability of a cluster's *identity* across a run. Tracing the real
//! derivation (`admissionlab_cluster::kubeconfig::kubeconfig_path`:
//! `<run>/kubeconfigs/<side>.kubeconfig`) shows the path is a function
//! of `(RunId, Side)` alone, with no incarnation counter. That is
//! sufficient today because [`admissionlab_core::cluster::ClusterManager::create`]
//! is called at most once per side per run (confirmed by grep across
//! this workspace) -- so within one run, one `(RunId, Side)` pair names
//! at most one cluster, and the cache key is unique for as long as this
//! resolver is used. It would **not** survive a same-side cluster being
//! torn down and recreated within one run while the same
//! `KubeResourceResolver` instance stays alive: the new cluster would
//! reuse the old one's kubeconfig path and silently inherit its stale
//! cached discovery -- a wrong answer (a collision), not merely a
//! missing one. No such recreate-within-a-run path exists yet, so this
//! is not a live bug; it is a constraint for whoever adds one (plausibly
//! alongside Task 3.10's caller, the same task that first exercises
//! [`KubeResourceResolver::invalidate`]) to know about rather than
//! discover the hard way.
//!
//! A cache populated before a component installs a CRD (Kyverno's
//! `ClusterPolicy`, Istio's `VirtualService`, and so on) will not
//! contain that CRD's resource, so [`KubeResourceResolver::invalidate`]
//! exists to drop one cluster's cached entry, forcing the next
//! [`ResourceResolver::resolve`] call for that cluster to run discovery
//! again. **Nothing in this crate calls it yet.** "Cache discovery per
//! cluster and invalidate once after CRD installs finish" (Task 3.2
//! brief Step 3) describes a caller that knows when a cluster's CRD
//! installs are done -- that is `admissionlab-core`'s `run.rs`
//! integration, Task 3.10, which does not exist yet. Inventing a
//! cross-crate callback here (for example, having this crate reach into
//! `admissionlab-installer` to observe when a stack install finishes)
//! would recreate exactly the dependency-direction risk the crate root's
//! documentation and controller supplement §2 warn about, for a caller
//! this task cannot yet write correctly. So [`KubeResourceResolver::invalidate`]
//! is a public capability with no caller inside this crate: a documented
//! hook, not a silently-never-invoked one.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use admissionlab_core::ClusterHandle;
use async_trait::async_trait;
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::core::{ApiResource, GroupVersion};
use kube::discovery::{Discovery, Scope};
use kube::{Client, Config};
use tokio::sync::Mutex;

use crate::FixtureError;

/// What [`ResourceResolver::resolve`] resolved a fixture document's
/// `apiVersion`/`kind` to: the descriptor a dynamic `kube::Api` needs to
/// query it, and whether it is namespace-scoped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedResource {
    /// The `kube` resource descriptor (group, version, kind, plural)
    /// discovery reported for this `apiVersion`/`kind`, from the live
    /// cluster's own served API -- never guessed from `kind` alone
    /// (Global Constraint 15; see this module's documentation).
    pub api_resource: ApiResource,
    /// Whether the cluster's discovery reported this resource as
    /// namespace-scoped (`true`) or cluster-scoped (`false`).
    pub namespaced: bool,
}

/// Resolves a fixture document's `apiVersion`/`kind` to a real
/// Kubernetes API resource on a specific cluster. See this module's
/// documentation for the one production implementation
/// ([`KubeResourceResolver`]) and its caching/invalidation design.
///
/// `Send + Sync` for the same reason every other async trait in this
/// workspace is (see `admissionlab_core::ClusterManager`'s
/// documentation): a later task resolves fixtures against baseline and
/// candidate clusters concurrently, the same way clusters are created
/// and components installed concurrently today.
#[async_trait]
pub trait ResourceResolver: Send + Sync {
    /// Resolves `api_version`/`kind` (taken verbatim from a fixture
    /// document's own fields) against `cluster`'s live API surface.
    ///
    /// # Errors
    ///
    /// Returns [`FixtureError::ResourceDiscoveryUnavailable`] if
    /// `cluster` could not even be queried (its kubeconfig could not be
    /// turned into a usable client, or the discovery request(s)
    /// themselves failed). Returns [`FixtureError::UnsupportedResource`]
    /// if `cluster`'s discovered API surface has no resource matching
    /// `api_version`/`kind` -- see that variant's own documentation for
    /// why this does not claim the resource is definitely absent.
    async fn resolve(
        &self,
        cluster: &ClusterHandle,
        api_version: &str,
        kind: &str,
    ) -> Result<ResolvedResource, FixtureError>;
}

/// The one production [`ResourceResolver`]: backed by
/// `kube::discovery::Discovery`, caching one discovery snapshot per
/// cluster. See this module's documentation for the caching and
/// invalidation design.
pub struct KubeResourceResolver {
    /// One cluster's most recently run discovery snapshot, keyed by
    /// that cluster's kubeconfig path. `Arc` so a cache hit can be
    /// cloned out and used without holding the cache lock for the
    /// duration of the (possibly slow) resource lookup that follows.
    cache: Mutex<BTreeMap<PathBuf, Arc<Discovery>>>,
}

impl KubeResourceResolver {
    /// Creates a resolver with an empty cache -- every cluster's first
    /// [`ResourceResolver::resolve`] call runs discovery fresh.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    /// Drops `cluster`'s cached discovery snapshot, if any, so the next
    /// [`ResourceResolver::resolve`] call for `cluster` runs discovery
    /// again rather than reusing a snapshot that may predate a CRD
    /// install. See this module's documentation for who is meant to
    /// call this and why nothing inside this crate does yet.
    ///
    /// A no-op (not an error) if `cluster` has no cached entry.
    pub async fn invalidate(&self, cluster: &ClusterHandle) {
        self.cache.lock().await.remove(&cluster.kubeconfig);
    }

    /// Returns `cluster`'s cached discovery snapshot, running
    /// [`kube::discovery::Discovery`] and populating the cache first if
    /// there is no entry for it yet.
    async fn discovery_for(&self, cluster: &ClusterHandle) -> Result<Arc<Discovery>, FixtureError> {
        if let Some(cached) = self.cache.lock().await.get(&cluster.kubeconfig) {
            return Ok(Arc::clone(cached));
        }

        let client = client_for(cluster).await.map_err(|source| {
            FixtureError::ResourceDiscoveryUnavailable {
                cluster: cluster.spec.name.clone(),
                reason: source.to_string(),
            }
        })?;
        let discovery = Discovery::new(client).run().await.map_err(|source| {
            FixtureError::ResourceDiscoveryUnavailable {
                cluster: cluster.spec.name.clone(),
                reason: source.to_string(),
            }
        })?;
        let discovery = Arc::new(discovery);

        self.cache
            .lock()
            .await
            .insert(cluster.kubeconfig.clone(), Arc::clone(&discovery));
        Ok(discovery)
    }
}

impl Default for KubeResourceResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ResourceResolver for KubeResourceResolver {
    async fn resolve(
        &self,
        cluster: &ClusterHandle,
        api_version: &str,
        kind: &str,
    ) -> Result<ResolvedResource, FixtureError> {
        let discovery = self.discovery_for(cluster).await?;
        resolve_against(&discovery, &cluster.spec.name, api_version, kind)
    }
}

/// Builds a `kube::Client` for `cluster` from its own isolated
/// kubeconfig -- never the operator's ambient `~/.kube/config`. Kept as
/// its own function (never inlined into [`KubeResourceResolver::discovery_for`])
/// so its error paths can be exercised in `tests/resources.rs` without a
/// live cluster, the same "narrow, offline-testable boundary" role
/// `admissionlab_installer::readiness::client_for` plays there.
///
/// Duplicated from that function (four lines) rather than pulled in
/// through a new `admissionlab-fixtures -> admissionlab-installer`
/// dependency edge: `admissionlab-installer` is a sibling crate here,
/// not something this crate otherwise needs, and adding that edge for
/// four lines is not worth the extra dependency-graph surface.
async fn client_for(cluster: &ClusterHandle) -> Result<Client, kube::Error> {
    let kubeconfig = Kubeconfig::read_from(&cluster.kubeconfig)?;
    let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default()).await?;
    Client::try_from(config)
}

/// Resolves `api_version`/`kind` against an already-run `discovery`
/// snapshot, translating a miss into [`FixtureError::UnsupportedResource`].
/// A free function, not a method, specifically so `tests` below can
/// drive it directly against a `Discovery` built from a mocked `Client`
/// -- see this module's documentation.
fn resolve_against(
    discovery: &Discovery,
    cluster_name: &str,
    api_version: &str,
    kind: &str,
) -> Result<ResolvedResource, FixtureError> {
    // `GroupVersion::from_str` splits on the first `/`; reading
    // `kube-core-4.2.0/src/gvk.rs` directly shows it has no reachable
    // rejecting arm for any `&str` input (`splitn(2, '/')` always
    // yields one or two elements). Still handled as a `Result` -- never
    // `.unwrap()`/`.expect()` -- both because that guarantee belongs to
    // `kube-core`, not to this function, and because folding it into
    // this same error keeps this function honest about the type
    // `kube-core` actually gives it, mirroring
    // `admissionlab_installer::readiness::parse_group_version`'s own
    // documented reasoning for the identical near-infallible parse.
    let group_version: GroupVersion =
        api_version
            .parse()
            .map_err(|error: kube::core::gvk::ParseGroupVersionError| {
                FixtureError::ResourceDiscoveryUnavailable {
                    cluster: cluster_name.to_string(),
                    reason: format!("invalid apiVersion {api_version:?}: {error}"),
                }
            })?;
    let gvk = group_version.with_kind(kind);

    discovery
        .resolve_gvk(&gvk)
        .map(|(api_resource, capabilities)| ResolvedResource {
            api_resource,
            namespaced: capabilities.scope == Scope::Namespaced,
        })
        .ok_or_else(|| FixtureError::UnsupportedResource {
            cluster: cluster_name.to_string(),
            api_version: api_version.to_string(),
            kind: kind.to_string(),
        })
}

// =========================================================================
// What is, and is not, covered without a live cluster
//
// Covered here, offline: `resolve_against`'s matching logic (core
// resource, namespaced CRD, cluster-scoped resource, and an
// apiVersion/kind absent from discovery), driven against a `Discovery`
// built from a `tower_test::mock`-backed `Client` -- never a real
// kube-apiserver, the same technique `admissionlab-installer/src/readiness.rs`'s
// own internal `tests` module uses (confirmed by reading
// `kube-client-4.2.0/src/client/mod.rs`'s `list_api_groups`/
// `list_api_group_resources`/`list_core_api_versions`/
// `list_core_api_resources` directly: each is one plain HTTP `GET`, with
// no retry/backoff of its own, so a fixed four-request mock exchange is
// exactly what one `Discovery::run()` call issues for one non-core group
// with one served version). Also covered: `KubeResourceResolver`'s cache
// actually being consulted, and `invalidate` actually clearing it (see
// `cache_is_reused_until_invalidated` below).
//
// NOT covered here, deliberately, and left for a live-cluster exit gate
// the same way `admissionlab-installer`'s own module documentation
// scopes its equivalent gap: whether `client_for` genuinely connects
// using a real `kind`-produced kubeconfig, and whether
// `kube::discovery::Discovery` resolves correctly against a real
// Kyverno/Istio-installed CRD.
// =========================================================================
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::pin::pin;

    use admissionlab_core::{ClusterSpec, RunId, Side};
    use http::{Request, Response};
    use kube::client::Body;
    use tower_test::mock;

    use super::{
        ClusterHandle, Discovery, KubeResourceResolver, ResourceResolver, resolve_against,
    };
    use crate::FixtureError;

    /// A fresh, guaranteed-unique path under the OS temp dir. Mirrors
    /// `admissionlab_installer::readiness`'s own `unique_kubeconfig_path`
    /// test helper -- no file is ever actually created at this path in
    /// this module's own tests, only named as a distinct cache key or a
    /// deliberately-missing kubeconfig.
    fn unique_path(label: &str) -> PathBuf {
        let unique = RunId::generate();
        std::env::temp_dir().join(format!(
            "admissionlab-fixtures-resources-test-{label}-{}.yaml",
            unique.as_str()
        ))
    }

    /// A minimal, otherwise-valid [`ClusterHandle`] pointing at
    /// `kubeconfig`. Only `kubeconfig` varies per test; every other
    /// field is a fixed, inert placeholder nothing in this module
    /// inspects.
    fn cluster_handle_with_kubeconfig(kubeconfig: PathBuf) -> ClusterHandle {
        ClusterHandle {
            spec: ClusterSpec {
                side: Side::Baseline,
                name: "resources-test-cluster".to_string(),
                kubernetes_version: "1.36.0".to_string(),
                node_image: "kindest/node:v1.36.0".to_string(),
            },
            kubeconfig,
            audit_log: std::env::temp_dir().join("admissionlab-fixtures-resources-test-audit.log"),
        }
    }

    fn json_body(value: &serde_json::Value) -> Body {
        Body::from(serde_json::to_vec(value).expect("serialize discovery response"))
    }

    /// Runs `kube::discovery::Discovery` to completion against a
    /// `tower_test` mock service that answers the fixed four-request
    /// sequence `Discovery::run()` issues for one non-core group
    /// ("kyverno.io/v2", carrying `Policy` -- namespaced -- and
    /// `ClusterPolicy` -- cluster-scoped) plus the always-queried core
    /// group ("v1", carrying `ConfigMap` -- namespaced). See this
    /// module's "What is, and is not, covered" note above for exactly
    /// which real HTTP calls this stands in for.
    async fn discovery_with_fake_data() -> Discovery {
        let (mock_service, handle) = mock::pair::<Request<Body>, Response<Body>>();
        let client = kube::Client::new(mock_service, "default");

        let responder = tokio::spawn(async move {
            let mut handle = pin!(handle);

            let (request, send) = handle.next_request().await.expect("GET /apis");
            assert_eq!(request.uri().path(), "/apis");
            send.send_response(Response::new(json_body(&serde_json::json!({
                "groups": [{
                    "name": "kyverno.io",
                    "versions": [{"groupVersion": "kyverno.io/v2", "version": "v2"}],
                    "preferredVersion": {"groupVersion": "kyverno.io/v2", "version": "v2"},
                }],
            }))));

            let (request, send) = handle
                .next_request()
                .await
                .expect("GET /apis/kyverno.io/v2");
            assert_eq!(request.uri().path(), "/apis/kyverno.io/v2");
            send.send_response(Response::new(json_body(&serde_json::json!({
                "groupVersion": "kyverno.io/v2",
                "resources": [
                    {
                        "name": "policies",
                        "singularName": "policy",
                        "namespaced": true,
                        "kind": "Policy",
                        "verbs": ["get", "list"],
                    },
                    {
                        "name": "clusterpolicies",
                        "singularName": "clusterpolicy",
                        "namespaced": false,
                        "kind": "ClusterPolicy",
                        "verbs": ["get", "list"],
                    },
                ],
            }))));

            let (request, send) = handle.next_request().await.expect("GET /api");
            assert_eq!(request.uri().path(), "/api");
            send.send_response(Response::new(json_body(&serde_json::json!({
                "versions": ["v1"],
            }))));

            let (request, send) = handle.next_request().await.expect("GET /api/v1");
            assert_eq!(request.uri().path(), "/api/v1");
            send.send_response(Response::new(json_body(&serde_json::json!({
                "groupVersion": "v1",
                "resources": [{
                    "name": "configmaps",
                    "singularName": "configmap",
                    "namespaced": true,
                    "kind": "ConfigMap",
                    "verbs": ["get", "list"],
                }],
            }))));
        });

        let discovery = Discovery::new(client)
            .run()
            .await
            .expect("discovery against the mocked apiserver must succeed");
        responder.await.expect("mock responder task must not panic");
        discovery
    }

    #[tokio::test]
    async fn resolve_against_finds_a_core_namespaced_resource() {
        // Fails if the core (empty-group) branch of discovery is not
        // read at all, or if `ConfigMap`'s group/plural were mixed up
        // with the CRD data also present in this same `Discovery`.
        let discovery = discovery_with_fake_data().await;

        let resolved = resolve_against(&discovery, "cluster", "v1", "ConfigMap")
            .expect("ConfigMap is present in the fake core discovery data");

        assert_eq!(resolved.api_resource.group, "");
        assert_eq!(resolved.api_resource.version, "v1");
        assert_eq!(resolved.api_resource.plural, "configmaps");
        assert!(
            resolved.namespaced,
            "ConfigMap is namespaced in the fake discovery data"
        );
    }

    #[tokio::test]
    async fn resolve_against_finds_a_namespaced_crd() {
        // Fails if `namespaced` were read from the wrong resource entry
        // (there are two `kyverno.io/v2` kinds in the fake data, one
        // namespaced and one not) or hardcoded `false`.
        let discovery = discovery_with_fake_data().await;

        let resolved = resolve_against(&discovery, "cluster", "kyverno.io/v2", "Policy")
            .expect("Policy is present in the fake kyverno.io/v2 discovery data");

        assert_eq!(resolved.api_resource.group, "kyverno.io");
        assert_eq!(resolved.api_resource.plural, "policies");
        assert!(
            resolved.namespaced,
            "Policy is namespaced in the fake discovery data"
        );
    }

    #[tokio::test]
    async fn resolve_against_finds_a_cluster_scoped_resource() {
        // The vacuous-assertion trap this test exists to catch: if
        // `namespaced` were hardcoded `true` (or read from the wrong
        // `ApiCapabilities`), this specific assertion -- not just some
        // assertion somewhere -- fails, because `ClusterPolicy` is the
        // one resource in the fake data whose `namespaced` is `false`.
        let discovery = discovery_with_fake_data().await;

        let resolved = resolve_against(&discovery, "cluster", "kyverno.io/v2", "ClusterPolicy")
            .expect("ClusterPolicy is present in the fake kyverno.io/v2 discovery data");

        assert_eq!(resolved.api_resource.plural, "clusterpolicies");
        assert!(
            !resolved.namespaced,
            "ClusterPolicy is cluster-scoped in the fake discovery data"
        );
    }

    #[tokio::test]
    async fn resolve_against_reports_unsupported_resource_when_discovery_has_no_match() {
        // Fails if a discovery miss were silently coerced into some
        // default `ResolvedResource` (a fabricated plural/scope -- the
        // exact thing Global Constraint 15 forbids) instead of an error,
        // or if the error's own fields did not name what was actually
        // requested.
        let discovery = discovery_with_fake_data().await;

        let error = resolve_against(&discovery, "cluster", "kyverno.io/v2", "NoSuchKind")
            .expect_err("NoSuchKind is not present in the fake discovery data");

        match error {
            FixtureError::UnsupportedResource {
                cluster,
                api_version,
                kind,
            } => {
                assert_eq!(cluster, "cluster");
                assert_eq!(api_version, "kyverno.io/v2");
                assert_eq!(kind, "NoSuchKind");
            }
            other => panic!("expected UnsupportedResource, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cache_is_reused_until_invalidated() {
        // Proves both halves of Step 3 at once:
        //
        // 1. Caching: `cluster`'s handle points at a kubeconfig that
        //    does not exist on disk, so any `resolve` call that
        //    actually attempts `client_for` fails with
        //    `ResourceDiscoveryUnavailable`. Seeding the cache directly
        //    (bypassing `client_for`, the same offline-vs-live split
        //    this module's documentation describes) and then calling
        //    `resolve` proves the cache was consulted *first*: if
        //    `resolve` ignored the cache and always rebuilt a client,
        //    every assertion below would see
        //    `ResourceDiscoveryUnavailable` instead of the results
        //    asserted.
        // 2. Invalidation: after `invalidate`, the same cluster's next
        //    `resolve` call has no cached entry left, so it *must* fall
        //    through to `client_for` -- which fails against the
        //    nonexistent kubeconfig. Seeing that failure appear only
        //    after `invalidate` (never before) is the proof
        //    `invalidate` actually removed the entry, rather than being
        //    a no-op.
        let discovery = discovery_with_fake_data().await;
        let cluster = cluster_handle_with_kubeconfig(unique_path("cache"));

        let resolver = KubeResourceResolver::new();
        resolver
            .cache
            .lock()
            .await
            .insert(cluster.kubeconfig.clone(), std::sync::Arc::new(discovery));

        // Cache hit: resolves correctly without ever touching
        // `client_for`'s nonexistent kubeconfig.
        let outcome = resolver
            .resolve(&cluster, "kyverno.io/v2", "Policy")
            .await
            .expect("a cached discovery snapshot must be used before falling back to client_for");
        assert_eq!(outcome.api_resource.plural, "policies");

        // Still a cache hit, still stale: `ClusterPolicy` was present in
        // the seeded snapshot, so this is not itself proof of caching --
        // covered by `resolve_against`'s own tests above. What matters
        // here is that this call succeeds at all (via the same cached
        // entry), setting up the contrast with the post-invalidate
        // failure below.
        resolver
            .resolve(&cluster, "kyverno.io/v2", "ClusterPolicy")
            .await
            .expect("ClusterPolicy is present in the seeded snapshot");

        resolver.invalidate(&cluster).await;

        let error = resolver
            .resolve(&cluster, "kyverno.io/v2", "ClusterPolicy")
            .await
            .expect_err(
                "invalidate must have dropped the cached snapshot, forcing a fresh client_for \
                 attempt against a kubeconfig that does not exist on disk",
            );
        assert!(
            matches!(error, FixtureError::ResourceDiscoveryUnavailable { .. }),
            "expected ResourceDiscoveryUnavailable once the cache is empty and client_for is \
             attempted, got {error:?}"
        );
    }
}
