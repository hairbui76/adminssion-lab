//! Finding the `Service` that fronts a Gateway's data plane (ROADMAP
//! Task 6.6).
//!
//! Everything downstream of this module -- the port-forward (Task 6.7)
//! and the HTTP probes (Task 6.8) -- needs one concrete answer:
//! *namespace, Service, port*. [`GatewayEndpoint`] is that answer, and
//! [`GatewayEndpointResolver`] is how it is obtained from a
//! [`GatewayEndpointStrategy`] a recipe declared.
//!
//! # Why a recipe has to say this at all
//!
//! Gateway API deliberately does not specify how an implementation
//! realizes a `Gateway`. Istio provisions a `Deployment` and a `Service`
//! named `<Gateway name>-<GatewayClass name>` in the `Gateway`'s own
//! namespace; other implementations use a shared, cluster-wide data
//! plane, an in-cluster `LoadBalancer` a fixture cannot reach, or a
//! `Service` a user deployed by hand. `Gateway.status.addresses` is the
//! only in-band answer the API itself offers, and in a `kind` cluster it
//! is routinely an address nothing on the host can dial.
//!
//! So the mapping is *install metadata* -- exactly the category
//! PRODUCT.md §14 puts in a recipe -- and it is declared next to the
//! [`admissionlab_spec::Capability::GatewayApi`] it belongs to. It is
//! **not** classification: nothing here or in a recipe says what an
//! observed difference means, only where to send a request. Global
//! Constraint 6 stays intact, and the `admissionlab-recipes` schema
//! enforces the rest of it structurally (see that crate's `model`
//! module).
//!
//! [`GatewayEndpointStrategy`]'s own documentation -- in
//! `admissionlab-spec`, where the type is defined so that both
//! `admissionlab-recipes` and this crate can name it -- carries the
//! upstream provenance for Istio's naming and for the well-known
//! `gateway.networking.k8s.io/gateway-name` label, and explains why the
//! two-token placeholder vocabulary exists and why it is closed.
//!
//! # How a selector is matched, and why here rather than server-side
//!
//! A [`GatewayEndpointStrategy::ServiceBySelector`] selector is a
//! `BTreeMap<String, String>`, so the only requirement it can express is
//! equality, and the only sensible reading of several entries is "all of
//! them". This module therefore lists every `Service` in the namespace
//! once and applies that rule itself, rather than pushing a
//! `labelSelector` to the API server.
//!
//! The trade is deliberate. A server-side selector would return only the
//! matches, which is exactly what makes it the wrong tool here: Task
//! 6.6's own requirement is that a zero-match or a multi-match failure
//! name *every candidate*, and a filtered list cannot say what it
//! filtered out. Listing once and matching locally makes
//! [`GatewayError::EndpointNotFound::considered`] and
//! [`GatewayError::EndpointAmbiguous::candidates`] honest at no extra
//! request, and there is no semantic gap to worry about: for
//! equality-only requirements, "every pair matches" is precisely what
//! `labelSelector` means.
//!
//! A lookup by exact name does the opposite -- one `GET` for one object
//! -- because there is nothing to enumerate. That is why
//! [`GatewayError::EndpointNotFound::considered`] is an `Option`: it is
//! `None` there, meaning "nothing was enumerated", never a misleading
//! empty list.
//!
//! # Choosing the port
//!
//! The rule, in order, given the `Service`'s own `spec.ports`:
//!
//! | Strategy names | Result |
//! | --- | --- |
//! | `portName` only | the port with that `name`; absent name is an error listing every exposed port |
//! | `port` only | that port, **checked against the Service** |
//! | both | each is resolved, and they must name the *same* port |
//! | neither, and the Service exposes exactly one port | that port |
//! | neither, and the Service exposes several | an error listing every one of them |
//!
//! Two of these lines are decisions rather than deductions.
//!
//! **A bare `port` is validated, not passed through.** Task 6.7 runs
//! `kubectl port-forward service/<name> :<remote-port>`, which requires
//! `<remote-port>` to be a port the `Service` actually exposes; a
//! strategy naming one it does not would fail later, inside `kubectl`,
//! with a message about a `Service` the user never mentioned. Checking
//! it here costs nothing (the `Service` has already been read to find
//! it) and turns that into an error naming every port available.
//!
//! **Neither field, with exactly one port, resolves rather than
//! erroring.** A single-port `Service` has no ambiguity to resolve:
//! there is one answer, and choosing it is not a guess. Requiring a
//! recipe to name it anyway would make the common case -- a data plane
//! `Service` exposing only `http` -- carry a redundant field whose only
//! failure mode is drifting out of step with the workload. The moment
//! there is more than one port, that reasoning stops applying and the
//! ambiguity is reported with every candidate (Global Constraint 15:
//! "the first one" is a fabrication, not an observation).
//!
//! # The offline seam
//!
//! [`KubeGatewayEndpointResolver`] is the production
//! [`GatewayEndpointResolver`]: it builds a `kube::Client` from the
//! cluster's own isolated kubeconfig and delegates to
//! [`resolve_gateway_endpoint_with_client`], which is where all of the
//! logic above lives. The same split -- and for the same reason -- as
//! [`crate::apply::apply_gateway_plan_with_client`] and
//! [`crate::reconcile::wait_for_route_reconciliation_with_client`]:
//! turning an on-disk kubeconfig into a network-connecting client has
//! nowhere to insert a fake; everything after it does, and
//! `tests/endpoint.rs` drives exactly that against a
//! `tower_test::mock`-backed client.

use std::collections::BTreeMap;

use admissionlab_core::ClusterHandle;
use async_trait::async_trait;
use k8s_openapi::api::core::v1::{Service, ServicePort};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Api, Client, Config};

use crate::error::{EndpointLookup, GatewayError};
use crate::model::GatewayIdentity;

/// How to find the `Service` fronting a Gateway's data plane.
///
/// Re-exported from `admissionlab-spec`, not declared here: the value is
/// produced by `admissionlab-recipes` and consumed by this crate, and
/// neither depends on the other. See the type's own documentation for
/// the full ownership argument (it is the same one
/// [`crate::model`] records for [`crate::model::RouteContract`]), for
/// the upstream provenance of the well-known gateway-name label, and
/// for the placeholder vocabulary.
pub use admissionlab_spec::GatewayEndpointStrategy;

/// Where a Gateway's data plane can actually be reached inside the
/// cluster: one `Service`, and one of its ports.
///
/// `port` is the **`Service`** port (`spec.ports[].port`), not the
/// backing pod's `targetPort`: `kubectl port-forward service/<name>
/// :<port>` names the former, and Task 6.7 builds exactly that argv.
///
/// `Serialize` (but not `Deserialize`), the same one-way asymmetry
/// [`GatewayIdentity`] documents: this is something Admission Lab
/// observed about a cluster, and it only ever travels outward into a
/// run's report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayEndpoint {
    /// The `Service`'s namespace.
    pub namespace: String,
    /// The `Service`'s name.
    pub service: String,
    /// The `Service` port to forward to.
    pub port: u16,
}

impl std::fmt::Display for GatewayEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}/{}:{}",
            self.namespace, self.service, self.port
        )
    }
}

/// Turns a recipe-declared [`GatewayEndpointStrategy`] into a concrete
/// [`GatewayEndpoint`] against a live cluster.
///
/// A trait rather than a free function because Task 6.11 assembles a run
/// from a recipe's metadata and a cluster handle, and a test of that
/// assembly needs to substitute an answer without a cluster -- the same
/// mock-seam shape [`crate::reconcile::GatewayStatusSource`] already
/// uses. [`KubeGatewayEndpointResolver`] is the one production
/// implementation.
#[async_trait]
pub trait GatewayEndpointResolver: Send + Sync {
    /// Resolves `strategy` for `gateway` against `cluster`.
    ///
    /// # Errors
    ///
    /// [`GatewayError::EndpointStrategyInvalid`] if `strategy` contains
    /// an unrecognized placeholder,
    /// [`GatewayError::ObservationUnavailable`] if the cluster could not
    /// be queried at all, [`GatewayError::EndpointNotFound`] /
    /// [`GatewayError::EndpointAmbiguous`] if zero or several `Service`s
    /// answered, and [`GatewayError::EndpointPortUnresolved`] if the
    /// matched `Service`'s port could not be determined.
    async fn resolve(
        &self,
        cluster: &ClusterHandle,
        gateway: &GatewayIdentity,
        strategy: &GatewayEndpointStrategy,
    ) -> Result<GatewayEndpoint, GatewayError>;
}

/// The production [`GatewayEndpointResolver`]: reads `Service` objects
/// through a `kube::Client` built from the cluster's own isolated
/// kubeconfig.
///
/// Holds no state; one value is reusable across clusters and Gateways.
#[derive(Debug, Clone, Copy, Default)]
pub struct KubeGatewayEndpointResolver;

impl KubeGatewayEndpointResolver {
    /// Creates a resolver.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl GatewayEndpointResolver for KubeGatewayEndpointResolver {
    async fn resolve(
        &self,
        cluster: &ClusterHandle,
        gateway: &GatewayIdentity,
        strategy: &GatewayEndpointStrategy,
    ) -> Result<GatewayEndpoint, GatewayError> {
        let client =
            client_for(cluster)
                .await
                .map_err(|source| GatewayError::ObservationUnavailable {
                    cluster: cluster.spec.name.clone(),
                    object: format!("the data-plane Service for Gateway {gateway}"),
                    reason: source.to_string(),
                })?;
        resolve_gateway_endpoint_with_client(client, &cluster.spec.name, gateway, strategy).await
    }
}

/// [`KubeGatewayEndpointResolver`]'s offline-testable core: the same
/// resolution, driven by an already-built `client`.
///
/// See this module's "The offline seam" section for why the split exists
/// and what each half is exercised by.
///
/// # Errors
///
/// See [`GatewayEndpointResolver::resolve`]; this function raises every
/// error except the client-construction one.
pub async fn resolve_gateway_endpoint_with_client(
    client: Client,
    cluster_name: &str,
    gateway: &GatewayIdentity,
    strategy: &GatewayEndpointStrategy,
) -> Result<GatewayEndpoint, GatewayError> {
    let (namespace, service) = match strategy {
        GatewayEndpointStrategy::ServiceBySelector {
            namespace,
            selector,
            ..
        } => {
            let namespace = substitute(namespace, gateway)?;
            let selector = substitute_selector(selector, gateway)?;
            let api: Api<Service> = Api::namespaced(client, &namespace);
            let service = find_by_selector(
                &api,
                cluster_name,
                gateway,
                &namespace,
                &describe_selector(&selector),
                &selector,
            )
            .await?;
            (namespace, service)
        }
        GatewayEndpointStrategy::ServiceByName {
            namespace, name, ..
        } => {
            let namespace = substitute(namespace, gateway)?;
            let name = substitute(name, gateway)?;
            let api: Api<Service> = Api::namespaced(client, &namespace);
            let service = find_by_name(&api, cluster_name, &namespace, &name)
                .await?
                .ok_or_else(|| GatewayError::EndpointNotFound {
                    lookup: Box::new(EndpointLookup {
                        cluster: cluster_name.to_owned(),
                        gateway: gateway.to_string(),
                        namespace: namespace.clone(),
                        criteria: format!("is named {name:?}"),
                    }),
                    // Nothing was enumerated -- see this module's "How a
                    // selector is matched" section.
                    considered: None,
                })?;
            (namespace, service)
        }
    };

    let name = service_name(&service, &namespace)?;
    let ports = service
        .spec
        .as_ref()
        .and_then(|spec| spec.ports.as_deref())
        .unwrap_or_default();
    let port = choose_port(
        cluster_name,
        &format!("{namespace}/{name}"),
        ports,
        strategy_port_name(strategy),
        strategy_port(strategy),
    )?;

    Ok(GatewayEndpoint {
        namespace,
        service: name,
        port,
    })
}

// =========================================================================
// Finding the Service
// =========================================================================

/// Lists every `Service` in `namespace` and returns the single one whose
/// labels satisfy every entry of `selector`.
async fn find_by_selector(
    api: &Api<Service>,
    cluster_name: &str,
    gateway: &GatewayIdentity,
    namespace: &str,
    criteria: &str,
    selector: &BTreeMap<String, String>,
) -> Result<Service, GatewayError> {
    let listed = api
        .list(&kube::api::ListParams::default())
        .await
        .map_err(|source| GatewayError::ObservationUnavailable {
            cluster: cluster_name.to_owned(),
            object: format!("the Services in namespace {namespace:?}"),
            reason: source.to_string(),
        })?;

    let mut considered: Vec<String> = Vec::with_capacity(listed.items.len());
    let mut matched: Vec<(String, Service)> = Vec::new();
    for service in listed.items {
        let name = service_name(&service, namespace)?;
        considered.push(name.clone());
        if matches_selector(&service, selector) {
            matched.push((name, service));
        }
    }
    considered.sort();
    matched.sort_by(|(left, _), (right, _)| left.cmp(right));

    let lookup = || {
        Box::new(EndpointLookup {
            cluster: cluster_name.to_owned(),
            gateway: gateway.to_string(),
            namespace: namespace.to_owned(),
            criteria: criteria.to_owned(),
        })
    };
    match matched.len() {
        1 => Ok(matched.remove(0).1),
        0 => Err(GatewayError::EndpointNotFound {
            lookup: lookup(),
            considered: Some(considered),
        }),
        _ => Err(GatewayError::EndpointAmbiguous {
            lookup: lookup(),
            candidates: matched.into_iter().map(|(name, _)| name).collect(),
        }),
    }
}

/// Reads one `Service` by name, mapping a 404 to `Ok(None)`.
async fn find_by_name(
    api: &Api<Service>,
    cluster_name: &str,
    namespace: &str,
    name: &str,
) -> Result<Option<Service>, GatewayError> {
    // `get_opt` is `kube`'s own "404 is not an error" read, so an absent
    // Service never has to be recognized by parsing an error message --
    // the same call, for the same reason,
    // `crate::reconcile::KubeGatewayStatusSource` uses.
    api.get_opt(name)
        .await
        .map_err(|source| GatewayError::ObservationUnavailable {
            cluster: cluster_name.to_owned(),
            object: format!("Service {namespace}/{name}"),
            reason: source.to_string(),
        })
}

/// Whether `service`'s labels satisfy every entry of `selector`.
///
/// Equality on every pair -- see this module's "How a selector is
/// matched" section. A `Service` with no labels at all satisfies only an
/// empty selector, which `admissionlab-recipes` rejects at load time.
fn matches_selector(service: &Service, selector: &BTreeMap<String, String>) -> bool {
    let labels = service.metadata.labels.as_ref();
    selector.iter().all(|(key, value)| {
        labels.is_some_and(|labels| labels.get(key).is_some_and(|actual| actual == value))
    })
}

/// A `Service`'s `metadata.name`.
///
/// An object the API server returned always has one; a value that does
/// not is not the object this code believes it is, and inventing a name
/// for it would put a fabricated identity into a report (Global
/// Constraint 15) -- the same argument
/// [`GatewayError::MalformedStatus`] already carries for Gateway API
/// objects.
fn service_name(service: &Service, namespace: &str) -> Result<String, GatewayError> {
    service
        .metadata
        .name
        .clone()
        .ok_or_else(|| GatewayError::MalformedStatus {
            object: format!("a Service in namespace {namespace}"),
            reason: "it has no metadata.name".to_owned(),
        })
}

// =========================================================================
// Choosing the port
// =========================================================================

/// Applies this module's documented port rule to one `Service`'s ports.
fn choose_port(
    cluster_name: &str,
    service: &str,
    ports: &[ServicePort],
    wanted_name: Option<&str>,
    wanted_port: Option<u16>,
) -> Result<u16, GatewayError> {
    let rendered = ports.iter().map(render_port).collect::<Vec<_>>();
    let unresolved = |reason: String| GatewayError::EndpointPortUnresolved {
        cluster: cluster_name.to_owned(),
        service: service.to_owned(),
        reason,
        ports: rendered.clone(),
    };

    if ports.is_empty() {
        return Err(unresolved("the Service exposes no ports".to_owned()));
    }

    let by_name = match wanted_name {
        None => None,
        Some(wanted) => {
            let mut hits = ports
                .iter()
                .filter(|port| port.name.as_deref() == Some(wanted));
            let first = hits.next().ok_or_else(|| {
                unresolved(format!("the Service exposes no port named {wanted:?}"))
            })?;
            if hits.next().is_some() {
                // Kubernetes requires port names to be unique within a
                // Service, so this is unreachable against a real API
                // server -- reported rather than resolved by taking the
                // first, for the same reason every other tie here is.
                return Err(unresolved(format!(
                    "the Service exposes more than one port named {wanted:?}"
                )));
            }
            Some(port_number(first, &unresolved)?)
        }
    };

    let by_number =
        match wanted_port {
            None => None,
            Some(wanted) => {
                let found = ports
                    .iter()
                    .map(|port| port_number(port, &unresolved))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .find(|port| *port == wanted);
                Some(found.ok_or_else(|| {
                    unresolved(format!("the Service does not expose port {wanted}"))
                })?)
            }
        };

    match (by_name, by_number) {
        (Some(named), Some(numbered)) if named != numbered => Err(unresolved(format!(
            "port name {:?} resolves to {named}, but the strategy also names port {numbered}",
            wanted_name.unwrap_or_default()
        ))),
        (Some(port), _) | (None, Some(port)) => Ok(port),
        (None, None) => match ports {
            [only] => port_number(only, &unresolved),
            several => Err(unresolved(format!(
                "the Service exposes {} ports and the strategy names neither a portName nor a \
                 port",
                several.len()
            ))),
        },
    }
}

/// One `Service` port's number, as a `u16`.
///
/// `ServicePort::port` is an `i32` because that is what the Kubernetes
/// `OpenAPI` schema declares; the API server's own validation confines it
/// to `1..=65535`. A value outside that range means the object is not
/// what this code believes it is, so it is reported rather than
/// truncated into a plausible-looking port.
fn port_number(
    port: &ServicePort,
    unresolved: &impl Fn(String) -> GatewayError,
) -> Result<u16, GatewayError> {
    u16::try_from(port.port)
        .ok()
        .filter(|number| *number != 0)
        .ok_or_else(|| {
            unresolved(format!(
                "the Service declares port {}, which is not a TCP port number",
                port.port
            ))
        })
}

/// `name=port`, or just `port` for an unnamed port.
fn render_port(port: &ServicePort) -> String {
    match &port.name {
        Some(name) => format!("{name}={}", port.port),
        None => port.port.to_string(),
    }
}

// =========================================================================
// Placeholder substitution
// =========================================================================

/// Substitutes `gateway`'s identity into one of a strategy's templated
/// fields.
fn substitute(template: &str, gateway: &GatewayIdentity) -> Result<String, GatewayError> {
    admissionlab_spec::substitute_gateway_placeholders(template, &gateway.namespace, &gateway.name)
        .map_err(|reason| GatewayError::EndpointStrategyInvalid {
            gateway: gateway.to_string(),
            reason,
        })
}

/// Substitutes into every selector *value*. Keys are literal -- see
/// [`GatewayEndpointStrategy`]'s own documentation.
fn substitute_selector(
    selector: &BTreeMap<String, String>,
    gateway: &GatewayIdentity,
) -> Result<BTreeMap<String, String>, GatewayError> {
    selector
        .iter()
        .map(|(key, value)| Ok((key.clone(), substitute(value, gateway)?)))
        .collect()
}

/// Renders a substituted selector the way `kubectl -l` would, for an
/// error message.
fn describe_selector(selector: &BTreeMap<String, String>) -> String {
    format!(
        "matches the selector `{}`",
        selector
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// A strategy's `portName`, whichever variant it is.
fn strategy_port_name(strategy: &GatewayEndpointStrategy) -> Option<&str> {
    match strategy {
        GatewayEndpointStrategy::ServiceBySelector { port_name, .. }
        | GatewayEndpointStrategy::ServiceByName { port_name, .. } => port_name.as_deref(),
    }
}

/// A strategy's `port`, whichever variant it is.
const fn strategy_port(strategy: &GatewayEndpointStrategy) -> Option<u16> {
    match strategy {
        GatewayEndpointStrategy::ServiceBySelector { port, .. }
        | GatewayEndpointStrategy::ServiceByName { port, .. } => *port,
    }
}

/// Builds a `kube::Client` for `cluster` from its own isolated
/// kubeconfig -- never the operator's ambient `~/.kube/config`.
///
/// A second copy of [`crate::apply`]'s private function of the same
/// name, which that module's own documentation already explains this
/// workspace has accepted three times over rather than adding a
/// dependency edge for four lines. Kept as its own function, never
/// inlined, so its error path is exercisable offline against a
/// deliberately missing kubeconfig.
async fn client_for(cluster: &ClusterHandle) -> Result<Client, kube::Error> {
    let kubeconfig = Kubeconfig::read_from(&cluster.kubeconfig)?;
    let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default()).await?;
    Client::try_from(config)
}
