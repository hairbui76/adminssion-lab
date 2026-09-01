//! The resolved component/install/readiness model: how a
//! [`crate::ComponentSpec`] a user hand-writes turns into a concrete,
//! ready-to-execute install and readiness contract.
//!
//! Everything in this module is *resolved* data: every path is absolute
//! (joined against the configuration file's own directory — see
//! `resolve.rs`'s module documentation and
//! [`crate::resolve::config_directory`]), every optional YAML field that
//! has a meaningful default has been filled in, and a Helm chart version
//! is guaranteed to be an exact, reproducible pin rather than a range
//! that could resolve to a different chart release on a different day.
//! Nothing here is ever deserialized directly from a configuration file —
//! see [`crate::model`] for the raw, as-written shapes
//! [`resolve_component`] converts *from*.
//!
//! # Naming: two `HelmInstallSpec`s, one crate
//!
//! [`crate::model::HelmInstallSpec`] (the raw, user-facing YAML shape) and
//! this module's [`HelmInstallSpec`] (the resolved shape) are two
//! deliberately distinct types that happen to share a name — the same
//! relationship [`crate::model::InstallMethodSpec`] has to
//! [`InstallMethod`]. Because both live in this crate, only one of them
//! can be re-exported at the crate root under the bare name
//! `HelmInstallSpec` without a compile error ("defined multiple times");
//! [`crate`]'s own module documentation explains which one that is and
//! why. Reach the other via its module path
//! (`admissionlab_spec::model::HelmInstallSpec` or
//! `admissionlab_spec::component::HelmInstallSpec`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::error::SpecError;
use crate::model::{
    ComponentSpec, HelmInstallSpec as RawHelmInstallSpec, InstallMethodSpec,
    ManifestsInstallSpec as RawManifestsInstallSpec, ReadinessCheckSpec,
};
use crate::resolve::resolve_relative;
use crate::validate;

/// One resolved component within a [`crate::ResolvedEnvironment`]: a
/// name, a version, a concrete install method, its readiness contract,
/// and the (currently always empty) normalize/capability metadata a
/// recipe will eventually supply.
///
/// `readiness` is populated from [`crate::ComponentSpec::readiness`],
/// whose own documentation explains why a component that serves
/// admission needs one: neither `helm upgrade --install` nor
/// `kubectl apply` waits for a controller to actually be serving, so a
/// lab with no readiness contract replays its fixtures against a stack
/// that is not yet the stack under test.
///
/// `recipe_normalize_rules` and `capabilities` still have no YAML
/// surface — [`crate::ComponentSpec`] carries no fields for them, so
/// [`resolve_component`] always produces an empty collection for each,
/// exactly as [`crate::ResolvedLab::gateway`]/[`crate::ResolvedLab::migration`]
/// are always `None` until the phase that defines their source YAML
/// exists. Task 2.5 ("recipe metadata and capability model") is what
/// starts populating them, once a component can be driven by a recipe
/// that supplies this metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedComponent {
    /// The component's name, unique within its environment.
    pub name: String,
    /// The component's version. Equal to
    /// [`crate::ComponentSpec::version`] when the user set one
    /// explicitly (trimmed, non-empty); otherwise defaulted from the
    /// install method when — and only when — the install method itself
    /// carries an unambiguous version. Only a pinned Helm chart version
    /// qualifies; a manifests install has no implicit version, so
    /// [`resolve_component`] requires an explicit one in that case. See
    /// [`resolve_component`]'s documentation for the exact rule.
    pub version: String,
    /// How to install this component.
    pub install: InstallMethod,
    /// Readiness checks to wait on after installing, in the order they
    /// were written — see this type's documentation.
    pub readiness: Vec<ReadinessCheck>,
    /// Recipe-supplied response normalization rules. Always empty today
    /// — see this type's documentation.
    pub recipe_normalize_rules: Vec<RecipeNormalizeRule>,
    /// Which admission-related capabilities this component provides.
    /// Always empty today — see this type's documentation.
    pub capabilities: BTreeSet<Capability>,
}

/// How a resolved component gets installed onto a cluster.
///
/// The resolved counterpart of [`crate::InstallMethodSpec`] — see that
/// type's documentation for why the two are distinct by design rather
/// than merged or aliased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallMethod {
    /// Install via a Helm chart.
    Helm(HelmInstallSpec),
    /// Install a fixed set of raw Kubernetes manifests.
    Manifests(ManifestInstallSpec),
}

/// Install method: a Helm chart, fully resolved and ready to drive `helm`
/// argv construction (Task 2.2).
///
/// Every field is a concrete, non-optional value — there is nothing left
/// for the installer to default or guess:
///
/// - `repo_name`/`release_name`/`namespace` come from
///   [`crate::model::HelmInstallSpec`]'s matching optional field when the
///   user sets one, and otherwise default to the resolved component's
///   own name — see [`resolve_helm`]'s documentation for why that
///   default is a reasonable one to have, but not always a *correct*
///   one, and must be overridden explicitly whenever it isn't.
/// - `version` is guaranteed to be an exact pin, never a floating range —
///   see [`crate::validate::require_pinned_helm_version`]'s documentation
///   for exactly what counts as floating and why it is rejected.
/// - `values_files` is resolved against the configuration file's own
///   directory, never the process's current working directory — the same
///   invariant [`crate::resolve::config_directory`] documents for every
///   other path in the document.
/// - `set_values` is copied verbatim from
///   [`crate::model::HelmInstallSpec::set_values`] (empty by default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelmInstallSpec {
    /// The local name `helm repo add <repo_name> <repo_url>` registers
    /// the repository under.
    pub repo_name: String,
    /// The Helm repository URL.
    pub repo_url: String,
    /// The chart reference passed to `helm install`.
    pub chart: String,
    /// The exact, pinned chart version passed to `helm install --version`.
    pub version: String,
    /// The Helm release name.
    pub release_name: String,
    /// The namespace to install into.
    pub namespace: String,
    /// Values override files, resolved to absolute paths.
    pub values_files: Vec<PathBuf>,
    /// Literal `--set-string` overrides.
    pub set_values: BTreeMap<String, String>,
}

/// Install method: a fixed set of raw Kubernetes manifests, with every
/// path resolved to absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestInstallSpec {
    /// Manifest file or directory paths, resolved against the
    /// configuration file's own directory.
    pub paths: Vec<PathBuf>,
}

/// A condition a [`ResolvedComponent`] must satisfy before it counts as
/// installed. Has no YAML surface yet — see [`ResolvedComponent`]'s
/// documentation; Task 2.4 implements the probing behavior, Task 2.5 is
/// the eventual source of these values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadinessCheck {
    /// A `Deployment`'s `Available` condition must be `True`.
    DeploymentAvailable {
        /// The deployment's namespace.
        namespace: String,
        /// The deployment's name.
        name: String,
    },
    /// A `DaemonSet` must have every desired pod scheduled and ready.
    DaemonSetReady {
        /// The daemonset's namespace.
        namespace: String,
        /// The daemonset's name.
        name: String,
    },
    /// A `Job` must have completed successfully.
    JobComplete {
        /// The job's namespace.
        namespace: String,
        /// The job's name.
        name: String,
    },
    /// A `ValidatingWebhookConfiguration`/`MutatingWebhookConfiguration`
    /// with this name must exist.
    WebhookConfigurationPresent {
        /// The webhook configuration's name (cluster-scoped).
        name: String,
    },
    /// A custom resource's named condition must equal a given status.
    CustomResourceCondition {
        /// The custom resource's `apiVersion`.
        api_version: String,
        /// The custom resource's `kind`.
        kind: String,
        /// The custom resource's namespace, or `None` if cluster-scoped.
        namespace: Option<String>,
        /// The custom resource's name.
        name: String,
        /// The condition's `type`.
        condition_type: String,
        /// The condition's required `status` (typically `"True"` or
        /// `"False"`).
        status: String,
    },
}

/// A recipe-supplied rule for normalizing a captured response before
/// comparing baseline against candidate. Has no YAML surface yet — see
/// [`ResolvedComponent`]'s documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipeNormalizeRule {
    /// Remove the value at a JSON Pointer before comparison.
    RemovePointer(String),
    /// Remove a specific annotation before comparison.
    RemoveAnnotation(String),
    /// Sort an array of objects at a JSON Pointer by a named key before
    /// comparison, so element order differences don't register as
    /// regressions.
    SortNamedArray {
        /// The JSON Pointer to the array.
        pointer: String,
        /// The object key to sort by.
        key: String,
    },
}

/// An admission-related capability a resolved component provides. Has no
/// YAML surface yet — see [`ResolvedComponent`]'s documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// Provides `ValidatingWebhookConfiguration`/`MutatingWebhookConfiguration`
    /// admission behavior.
    Admission,
    /// Provides Gateway API conformance surface.
    GatewayApi,
    /// Provides legacy `Ingress` surface.
    LegacyIngress,
}

/// How to find the `Service` that fronts a Gateway's data plane, so a
/// local port-forward can be opened to it (ROADMAP Task 6.6).
///
/// # Why this lives here rather than in `admissionlab-gateway`
///
/// ROADMAP Task 6.6's file list names
/// `crates/admissionlab-gateway/src/endpoint.rs` as this type's home,
/// and that module is where its *resolution* lives. The type itself is
/// defined here for exactly the reason Controller Ruling R30 gives for
/// [`Capability`]: the value is produced by `admissionlab-recipes` (a
/// recipe declares it, next to the [`Capability::GatewayApi`] it
/// declares alongside it) and consumed by `admissionlab-gateway`, and
/// those two crates do not depend on one another. The only home that
/// both can name without inventing a new dependency edge — or forking
/// the type into two copies that have to be kept in step by hand — is
/// this leaf crate, which both already depend on.
/// `admissionlab_gateway::endpoint` re-exports it, exactly as
/// `admissionlab_gateway::model` re-exports [`crate::GatewaySuiteSpec`]
/// for the same reason.
///
/// # This is install metadata, never classification
///
/// PRODUCT.md §14 / Global Constraint 6: a recipe may supply
/// install/readiness/normalization/capability metadata, and never
/// regression-classification logic. "Which `Service` fronts this
/// Gateway" is squarely the former — it is the same kind of fact as
/// "which namespace does this chart install into". Nothing here says
/// what an observed difference *means*, and nothing here has a
/// severity, a `failOn`, or an expectation attached; it only says where
/// to send a request. See [`crate::GatewaySuiteSpec`]'s "Traffic
/// expectations are not regression policy" section for the same line
/// drawn on the user-facing side of Phase 6.
///
/// # Placeholders, and why they are unavoidable
///
/// One recipe serves every `Gateway` in a lab, but the `Service` that
/// fronts a Gateway is per-Gateway: Istio's Gateway API controller
/// provisions a `Deployment` and a `Service` named
/// `<Gateway name>-<GatewayClass name>` **in the Gateway's own
/// namespace**, labelled `gateway.networking.k8s.io/gateway-name:
/// <Gateway name>` (Istio's "Kubernetes Gateway API" task, "Automated
/// Deployment"; the label is the well-known one upstream Gateway API
/// documents under "Gateway infrastructure labels and annotations").
/// Both the namespace and the identifying label value therefore vary
/// with the Gateway, so a recipe that could only write literals could
/// serve exactly one Gateway.
///
/// The answer is a **closed, two-token** substitution vocabulary —
/// [`GATEWAY_NAME_PLACEHOLDER`] and [`GATEWAY_NAMESPACE_PLACEHOLDER`] —
/// applied by [`substitute_gateway_placeholders`], not a template
/// language. There are no conditionals, no defaults, no nesting and no
/// user-extensible variables: a recipe can say "the Gateway's name" and
/// "the Gateway's namespace" and nothing else. An unrecognized `{...}`
/// token is a loud error at recipe-load time, never a literal left in
/// place — a selector value of `"{gateway}"` would silently match no
/// `Service` at all and read as "this Gateway has no data plane", which
/// is Global Constraint 15's fabrication failure wearing a typo's
/// clothes.
///
/// Braces need no escape hatch, and deliberately have none: `{` and `}`
/// are not legal characters in a Kubernetes namespace name, object
/// name, label key, or label value (RFC 1123 label / the label-value
/// charset), so a brace in one of these fields is *always* a
/// placeholder delimiter and never data.
///
/// # Prefer the selector to the name
///
/// [`GatewayEndpointStrategy::ServiceByName`] can express Istio's
/// generated name as `"{gatewayName}-istio"`, but that hard-codes the
/// `GatewayClass` name into the recipe and silently breaks for any
/// Gateway using a differently named class. The label above is
/// upstream-documented, class-independent, and applied by Istio to
/// exactly the objects it generated for one Gateway, so
/// [`GatewayEndpointStrategy::ServiceBySelector`] is the form a recipe
/// should normally use. `ServiceByName` exists for the case a selector
/// cannot express: a hand-deployed, pre-existing data-plane `Service`
/// that carries no such label at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayEndpointStrategy {
    /// Find the `Service` by label selector — the form recipes should
    /// normally use (see this type's documentation).
    ServiceBySelector {
        /// The namespace to search. Usually
        /// [`GATEWAY_NAMESPACE_PLACEHOLDER`], since a Gateway's
        /// generated `Service` lives in the Gateway's own namespace.
        namespace: String,
        /// Label requirements every candidate `Service` must satisfy.
        /// Equality only — this is a map, so it can express nothing
        /// else — and *every* pair must match. Keys are literal (a
        /// label key names the vocabulary, not the instance); only
        /// values are substituted.
        selector: BTreeMap<String, String>,
        /// Which of the matched `Service`'s ports to use, by
        /// `spec.ports[].name`.
        port_name: Option<String>,
        /// Which of the matched `Service`'s ports to use, by
        /// `spec.ports[].port`.
        port: Option<u16>,
    },
    /// Find the `Service` by an exact name.
    ServiceByName {
        /// The `Service`'s namespace.
        namespace: String,
        /// The `Service`'s name.
        name: String,
        /// See [`GatewayEndpointStrategy::ServiceBySelector::port_name`].
        port_name: Option<String>,
        /// See [`GatewayEndpointStrategy::ServiceBySelector::port`].
        port: Option<u16>,
    },
}

/// The placeholder that stands for a `Gateway`'s own name in a
/// [`GatewayEndpointStrategy`]. See that type's documentation.
pub const GATEWAY_NAME_PLACEHOLDER: &str = "{gatewayName}";

/// The placeholder that stands for a `Gateway`'s own namespace in a
/// [`GatewayEndpointStrategy`]. See that type's documentation.
pub const GATEWAY_NAMESPACE_PLACEHOLDER: &str = "{gatewayNamespace}";

/// Every placeholder [`substitute_gateway_placeholders`] recognizes, in
/// the order error messages list them.
pub const GATEWAY_PLACEHOLDERS: [&str; 2] =
    [GATEWAY_NAME_PLACEHOLDER, GATEWAY_NAMESPACE_PLACEHOLDER];

/// Replaces every [`GATEWAY_PLACEHOLDERS`] token in `template` with the
/// named `Gateway`'s own namespace/name.
///
/// See [`GatewayEndpointStrategy`]'s documentation for why this
/// vocabulary is closed, why an unknown token is an error rather than a
/// literal, and why there is no escape sequence for a brace.
///
/// # Errors
///
/// Returns a human-readable message when `template` contains a `{` with
/// no matching `}`, a `}` that closes nothing, or a `{...}` token that
/// is not one of [`GATEWAY_PLACEHOLDERS`].
pub fn substitute_gateway_placeholders(
    template: &str,
    gateway_namespace: &str,
    gateway_name: &str,
) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    loop {
        let Some(open) = rest.find(['{', '}']) else {
            out.push_str(rest);
            return Ok(out);
        };
        if rest.as_bytes()[open] == b'}' {
            return Err(format!(
                "{template:?} contains a `}}` that closes no placeholder; `{{` and `}}` are not \
                 legal characters in a Kubernetes name or label value, so they are always \
                 placeholder delimiters here"
            ));
        }
        out.push_str(&rest[..open]);
        let after_open = &rest[open..];
        let Some(close) = after_open.find('}') else {
            return Err(format!(
                "{template:?} contains an unterminated placeholder: a `{{` with no matching `}}`"
            ));
        };
        let token = &after_open[..=close];
        match token {
            GATEWAY_NAME_PLACEHOLDER => out.push_str(gateway_name),
            GATEWAY_NAMESPACE_PLACEHOLDER => out.push_str(gateway_namespace),
            unknown => {
                return Err(format!(
                    "{template:?} contains unknown placeholder {unknown:?}; expected one of {}",
                    GATEWAY_PLACEHOLDERS
                        .iter()
                        .map(|placeholder| format!("{placeholder:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        rest = &after_open[close + 1..];
    }
}

/// Validates a hand-written [`crate::GatewayEndpointSpec`] and resolves
/// it into a [`GatewayEndpointStrategy`].
///
/// Every string field a [`GatewayEndpointStrategy`] substitutes into is
/// checked against [`GATEWAY_PLACEHOLDERS`] — so a typo such as
/// `{gateway}` fails when the document is read rather than silently
/// resolving to a `Service` selector that matches nothing at run time.
/// The check runs the *real* substitution against a placeholder identity
/// and discards the result, so this function and
/// `admissionlab-gateway`'s own resolution can never disagree about
/// which tokens are legal.
///
/// # Why this lives here rather than in `admissionlab-recipes`
///
/// It was in `admissionlab_recipes::capability` until ROADMAP Task 6.11
/// gave `admissionlab.yaml` a `gateway.gatewayEndpoint:` block of its
/// own. Two callers now read the same YAML shape into the same resolved
/// type, and leaving the validation in the crate only one of them
/// depends on would have forced the second to grow a copy — two
/// validators for one vocabulary, free to drift. It lives beside
/// [`GatewayEndpointStrategy`] and [`substitute_gateway_placeholders`],
/// which are the two things it is defined in terms of;
/// `admissionlab_recipes::capability::resolve_gateway_endpoint` is now a
/// one-line delegate to it.
///
/// # Errors
///
/// Returns `Err((locator, message))`, where `locator` is a dotted path
/// *relative to* the `gatewayEndpoint` block (for example `"namespace"`
/// or `"selector[\"app\"]"`), when a required field is empty, when
/// `selector` is empty, when `port` is zero, or when any substitutable
/// field contains an unrecognized placeholder.
pub fn resolve_gateway_endpoint(
    raw: &crate::model::GatewayEndpointSpec,
) -> Result<GatewayEndpointStrategy, (String, String)> {
    match raw {
        crate::model::GatewayEndpointSpec::ServiceBySelector {
            namespace,
            selector,
            port_name,
            port,
        } => {
            let namespace = endpoint_template("namespace", namespace)?;
            if selector.is_empty() {
                return Err((
                    "selector".to_owned(),
                    "must not be empty -- a selector matching every Service in the namespace \
                     could only ever be ambiguous"
                        .to_owned(),
                ));
            }
            let selector = selector
                .iter()
                .map(|(key, value)| {
                    let key = key.trim().to_owned();
                    if key.is_empty() {
                        return Err((
                            "selector".to_owned(),
                            "a label key must not be empty".to_owned(),
                        ));
                    }
                    // Only values are substituted: a label *key* names
                    // the vocabulary (`gateway.networking.k8s.io/gateway-name`),
                    // not the instance, so a placeholder in one would be
                    // meaningless. Checked rather than assumed, so a
                    // document writing one is told why instead of getting
                    // a key that silently matches nothing.
                    if key.contains(['{', '}']) {
                        return Err((
                            format!("selector[{key:?}]"),
                            "a label key must not contain a placeholder -- only label values are \
                             substituted"
                                .to_owned(),
                        ));
                    }
                    let value = endpoint_template(&format!("selector[{key:?}]"), value)?;
                    Ok((key, value))
                })
                .collect::<Result<_, _>>()?;
            Ok(GatewayEndpointStrategy::ServiceBySelector {
                namespace,
                selector,
                port_name: endpoint_port_name(port_name.as_deref())?,
                port: endpoint_port(*port)?,
            })
        }
        crate::model::GatewayEndpointSpec::ServiceByName {
            namespace,
            name,
            port_name,
            port,
        } => Ok(GatewayEndpointStrategy::ServiceByName {
            namespace: endpoint_template("namespace", namespace)?,
            name: endpoint_template("name", name)?,
            port_name: endpoint_port_name(port_name.as_deref())?,
            port: endpoint_port(*port)?,
        }),
    }
}

/// Trims `value`, rejects it if empty, and proves every placeholder it
/// contains is one [`substitute_gateway_placeholders`] recognizes.
fn endpoint_template(locator: &str, value: &str) -> Result<String, (String, String)> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err((locator.to_owned(), "must not be empty".to_owned()));
    }
    // The identity here is arbitrary: only the Err/Ok distinction is
    // used, and running the real substitution (rather than a private
    // second scanner) is what keeps this check and
    // `admissionlab-gateway`'s own resolution in step.
    substitute_gateway_placeholders(trimmed, "placeholder-namespace", "placeholder-name")
        .map_err(|message| (locator.to_owned(), message))?;
    Ok(trimmed.to_owned())
}

/// A `portName`, if present, must be a non-empty literal. Deliberately
/// **not** substitutable: a Service port name is part of the workload's
/// own contract (`http`, `https`, `status-port`), never derived from a
/// `Gateway`'s identity.
fn endpoint_port_name(raw: Option<&str>) -> Result<Option<String>, (String, String)> {
    let Some(raw) = raw else { return Ok(None) };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err((
            "portName".to_owned(),
            "must not be empty -- omit the field entirely to resolve the port some other way"
                .to_owned(),
        ));
    }
    Ok(Some(trimmed.to_owned()))
}

/// A `port`, if present, must be a real TCP port. Values above 65535 are
/// already rejected by `serde` (the field is a `u16`); zero is not.
fn endpoint_port(raw: Option<u16>) -> Result<Option<u16>, (String, String)> {
    if raw == Some(0) {
        return Err((
            "port".to_owned(),
            "must not be 0 -- omit the field entirely to resolve the port some other way"
                .to_owned(),
        ));
    }
    Ok(raw)
}

/// Converts one [`ComponentSpec`] into a [`ResolvedComponent`], resolving
/// every relative path inside its `install` block against `config_dir`
/// (see `resolve.rs`'s module documentation and
/// [`crate::resolve::config_directory`] for why it is always this
/// directory, never the process's current working directory) and
/// enforcing the rules this task closes:
///
/// - **Exactly one install method.** [`ComponentSpec::install`] must be
///   `Some` — a component with no `install` block cannot resolve to a
///   concrete [`InstallMethod`] yet, since recipe-driven installation
///   (Task 2.5) does not exist. The complementary "both Helm and
///   manifests at once" failure mode needs no runtime check here:
///   [`InstallMethodSpec`]'s internally tagged, `deny_unknown_fields`
///   representation already makes it a load-time parse error (a `type:
///   helm` block that also carries a manifests-only field such as
///   `paths` is an unknown field for [`crate::model::HelmInstallSpec`]).
/// - **A non-empty chart, an explicit repository, and a pinned version.**
///   See [`validate::require_helm_chart`], [`validate::require_helm_repo_url`],
///   and [`validate::require_pinned_helm_version`]. Local path and
///   `oci://` chart references remain syntactically valid in
///   [`crate::model::HelmInstallSpec::chart`], but resolution has no way
///   to act on them without a registered repository, so `repo` is
///   required for every Helm install today.
/// - **A meaningful component version.** See
///   [`validate::require_component_version`].
pub(crate) fn resolve_component(
    field: &str,
    index: usize,
    raw: &ComponentSpec,
    config_dir: &Path,
    source_path: &Path,
) -> Result<ResolvedComponent, SpecError> {
    let name = validate::require_component_name(field, index, raw.name.as_deref(), source_path)?;
    let locator = format!("{field}.components[{index}]");

    let install_spec =
        validate::require_install_method(&locator, raw.install.as_ref(), source_path)?;
    let install = match install_spec {
        InstallMethodSpec::Helm(helm) => InstallMethod::Helm(resolve_helm(
            &locator,
            helm,
            &name,
            config_dir,
            source_path,
        )?),
        InstallMethodSpec::Manifests(manifests) => InstallMethod::Manifests(resolve_manifests(
            &locator,
            manifests,
            config_dir,
            source_path,
        )?),
    };

    let install_version = match &install {
        InstallMethod::Helm(helm) => Some(helm.version.as_str()),
        InstallMethod::Manifests(_) => None,
    };
    let version = validate::require_component_version(
        &locator,
        raw.version.as_deref(),
        install_version,
        source_path,
    )?;

    Ok(ResolvedComponent {
        name,
        version,
        install,
        readiness: raw.readiness.iter().map(resolve_readiness).collect(),
        recipe_normalize_rules: Vec::new(),
        capabilities: BTreeSet::new(),
    })
}

/// Converts one user-written [`ReadinessCheckSpec`] into the
/// [`ReadinessCheck`] `admissionlab_installer::readiness` probes.
///
/// Infallible and total: every field the closed variant set needs is
/// already required by the parser (`serde` rejects a missing `name` or
/// `namespace` at load time), so there is nothing left here to validate
/// — an empty `name` would name no object, but that is a claim about a
/// cluster this crate never sees, and the readiness probe reports it as
/// "not ready" with the name it was given rather than this function
/// inventing a rule the recipe surface does not have either.
///
/// Matched exhaustively with no wildcard arm, so a sixth variant on
/// either side is a compile error rather than a silently dropped check.
///
/// `pub` since ROADMAP Task 6.11: a component's readiness is resolved
/// here by [`resolve_component`], and a Gateway suite's own
/// [`crate::GatewaySuiteSpec::readiness`] is resolved by its runner in
/// `admissionlab-cli`, which has to be able to call the *same*
/// conversion. Two conversions for one vocabulary is the drift this
/// crate's ownership rules exist to prevent.
#[must_use]
pub fn resolve_readiness(raw: &ReadinessCheckSpec) -> ReadinessCheck {
    match raw {
        ReadinessCheckSpec::DeploymentAvailable(object) => ReadinessCheck::DeploymentAvailable {
            namespace: object.namespace.clone(),
            name: object.name.clone(),
        },
        ReadinessCheckSpec::DaemonSetReady(object) => ReadinessCheck::DaemonSetReady {
            namespace: object.namespace.clone(),
            name: object.name.clone(),
        },
        ReadinessCheckSpec::JobComplete(object) => ReadinessCheck::JobComplete {
            namespace: object.namespace.clone(),
            name: object.name.clone(),
        },
        ReadinessCheckSpec::WebhookConfigurationPresent(object) => {
            ReadinessCheck::WebhookConfigurationPresent {
                name: object.name.clone(),
            }
        }
        ReadinessCheckSpec::CustomResourceCondition(condition) => {
            ReadinessCheck::CustomResourceCondition {
                api_version: condition.api_version.clone(),
                kind: condition.kind.clone(),
                namespace: condition.namespace.clone(),
                name: condition.name.clone(),
                condition_type: condition.condition_type.clone(),
                status: condition.status.clone(),
            }
        }
    }
}

/// Resolves a Helm install method.
///
/// `repo_name`/`release_name`/`namespace` each come from
/// [`crate::model::HelmInstallSpec`]'s matching optional field when the
/// user sets one (via [`nonempty_or`]), and otherwise default to
/// `component_name`. That default is a reasonable *starting point* —
/// component names are already required to be unique within their
/// environment (see [`crate::validate::unique_component_names`]), so it
/// is at least collision-free — but it is not always a *correct* one:
/// several real charts install into a namespace that does not match
/// their component's own name (`istio/istiod` and `istio/base` both
/// conventionally install into `istio-system`, not `istiod`/`base`).
/// This is exactly why the override exists rather than making the
/// default absolute; a user naming a component `istiod` must set
/// `namespace: istio-system` explicitly.
fn resolve_helm(
    locator: &str,
    raw: &RawHelmInstallSpec,
    component_name: &str,
    config_dir: &Path,
    source_path: &Path,
) -> Result<HelmInstallSpec, SpecError> {
    let chart = validate::require_helm_chart(locator, &raw.chart, source_path)?;
    let repo_url = validate::require_helm_repo_url(locator, raw.repo.as_deref(), source_path)?;
    let version =
        validate::require_pinned_helm_version(locator, raw.version.as_deref(), source_path)?;

    let values_files = raw
        .values_files
        .iter()
        .cloned()
        .map(|path| resolve_relative(config_dir, path))
        .collect();

    Ok(HelmInstallSpec {
        repo_name: nonempty_or(raw.repo_name.as_deref(), component_name),
        repo_url,
        chart,
        version,
        release_name: nonempty_or(raw.release_name.as_deref(), component_name),
        namespace: nonempty_or(raw.namespace.as_deref(), component_name),
        values_files,
        set_values: raw.set_values.clone(),
    })
}

/// Returns `override_value` trimmed, if present and non-empty; otherwise
/// returns `default_value` as an owned `String`. An explicitly blank
/// override (`namespace: ""`) is treated the same as an absent one,
/// consistent with every other optional string this crate resolves (see
/// [`crate::validate::require_component_version`], for example).
fn nonempty_or(override_value: Option<&str>, default_value: &str) -> String {
    match override_value.map(str::trim) {
        Some(value) if !value.is_empty() => value.to_owned(),
        _ => default_value.to_owned(),
    }
}

/// Resolves a manifests install method: rejects an empty path list (see
/// [`validate::manifests_paths_nonempty`]), then makes every remaining
/// path absolute against `config_dir`.
fn resolve_manifests(
    locator: &str,
    raw: &RawManifestsInstallSpec,
    config_dir: &Path,
    source_path: &Path,
) -> Result<ManifestInstallSpec, SpecError> {
    validate::manifests_paths_nonempty(locator, &raw.paths, source_path)?;
    Ok(ManifestInstallSpec {
        paths: raw
            .paths
            .iter()
            .cloned()
            .map(|path| resolve_relative(config_dir, path))
            .collect(),
    })
}

// ---------------------------------------------------------------------
// Tests: the Gateway endpoint placeholder vocabulary (Task 6.6).
//
// Unit tests here, rather than in `tests/`, because this is the one
// place the substitution rule is defined -- `admissionlab-recipes`
// validates a recipe's placeholders through it and
// `admissionlab-gateway` applies it at resolve time, so both crates'
// behavior follows from exactly these cases.
// ---------------------------------------------------------------------
#[cfg(test)]
mod gateway_placeholder_tests {
    use super::{GATEWAY_PLACEHOLDERS, substitute_gateway_placeholders};

    fn substitute(template: &str) -> Result<String, String> {
        substitute_gateway_placeholders(template, "gateway-lab", "lab-gateway")
    }

    #[test]
    fn a_template_with_no_placeholder_is_returned_unchanged() {
        assert_eq!(substitute("istio-system"), Ok("istio-system".to_owned()));
        assert_eq!(substitute(""), Ok(String::new()));
    }

    #[test]
    fn both_placeholders_substitute_the_gateways_own_identity() {
        assert_eq!(
            substitute("{gatewayNamespace}"),
            Ok("gateway-lab".to_owned())
        );
        assert_eq!(substitute("{gatewayName}"), Ok("lab-gateway".to_owned()));
    }

    /// The Istio-shaped case: a placeholder embedded in a larger literal
    /// (`<Gateway name>-<GatewayClass name>`), and more than one
    /// placeholder in one template.
    #[test]
    fn a_placeholder_substitutes_in_place_within_surrounding_literal_text() {
        assert_eq!(
            substitute("{gatewayName}-istio"),
            Ok("lab-gateway-istio".to_owned())
        );
        assert_eq!(
            substitute("{gatewayNamespace}/{gatewayName}"),
            Ok("gateway-lab/lab-gateway".to_owned())
        );
    }

    #[test]
    fn an_unknown_placeholder_is_an_error_naming_it_and_the_known_set() {
        // The exact typo the strict rule exists for: `{gateway}` left
        // literal would produce a selector matching no Service at all,
        // reported as "this Gateway has no data plane".
        let error = substitute("{gateway}").expect_err("{gateway} is not a known placeholder");
        assert!(error.contains("\"{gateway}\""), "got: {error}");
        for known in GATEWAY_PLACEHOLDERS {
            assert!(
                error.contains(known),
                "error must name {known}, got: {error}"
            );
        }
    }

    #[test]
    fn unbalanced_braces_are_rejected_rather_than_left_literal() {
        assert!(
            substitute("{gatewayName").is_err(),
            "an unterminated placeholder must not be passed through"
        );
        assert!(
            substitute("gatewayName}").is_err(),
            "a `}}` closing nothing must not be passed through"
        );
    }

    /// A substituted value is inserted verbatim: nothing re-scans it for
    /// placeholders, so a (impossible, but not this function's business)
    /// brace inside an identity cannot start a second substitution pass.
    #[test]
    fn substitution_is_not_recursive() {
        assert_eq!(
            substitute_gateway_placeholders("{gatewayName}", "ns", "{gatewayNamespace}"),
            Ok("{gatewayNamespace}".to_owned())
        );
    }
}
