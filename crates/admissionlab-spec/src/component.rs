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
    ManifestsInstallSpec as RawManifestsInstallSpec,
};
use crate::resolve::resolve_relative;
use crate::validate;

/// One resolved component within a [`crate::ResolvedEnvironment`]: a
/// name, a version, a concrete install method, and the (currently always
/// empty) readiness/normalize/capability metadata a recipe will
/// eventually supply.
///
/// `readiness`, `recipe_normalize_rules`, and `capabilities` have no YAML
/// surface yet — [`crate::ComponentSpec`] carries no fields for them, so
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
    /// Readiness checks to wait on after installing. Always empty today
    /// — see this type's documentation.
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
/// - `repo_name`/`release_name`/`namespace` default to the resolved
///   component's own name when [`crate::model::HelmInstallSpec`] does not
///   (yet) expose a way to override them; see [`resolve_helm`]'s
///   documentation.
/// - `version` is guaranteed to be an exact pin, never a floating range —
///   see [`crate::validate::require_pinned_helm_version`]'s documentation
///   for exactly what counts as floating and why it is rejected.
/// - `values_files` is resolved against the configuration file's own
///   directory, never the process's current working directory — the same
///   invariant [`crate::resolve::config_directory`] documents for every
///   other path in the document.
/// - `set_values` has no YAML surface yet, so it is always empty; a
///   future task can add one without changing this shape.
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
    /// Literal `--set-string` overrides. Always empty today — see this
    /// type's documentation.
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
/// - **An explicit, pinned Helm chart version and an explicit repository.**
///   See [`validate::require_pinned_helm_version`] and
///   [`validate::require_helm_repo_url`]. Local path and `oci://` chart
///   references remain syntactically valid in
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
        InstallMethodSpec::Manifests(manifests) => {
            InstallMethod::Manifests(resolve_manifests(manifests, config_dir))
        }
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
        readiness: Vec::new(),
        recipe_normalize_rules: Vec::new(),
        capabilities: BTreeSet::new(),
    })
}

/// Resolves a Helm install method.
///
/// Defaults `repo_name`/`release_name`/`namespace` to `component_name`: a
/// predictable, collision-free choice, since component names are already
/// required to be unique within their environment (see
/// [`crate::validate::unique_component_names`]), and
/// [`crate::model::HelmInstallSpec`] does not expose a way to override
/// any of the three today (there is deliberately no YAML key for this
/// yet — see this module's [`HelmInstallSpec`] documentation; a future
/// task can add one without changing the resolved shape).
fn resolve_helm(
    locator: &str,
    raw: &RawHelmInstallSpec,
    component_name: &str,
    config_dir: &Path,
    source_path: &Path,
) -> Result<HelmInstallSpec, SpecError> {
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
        repo_name: component_name.to_owned(),
        repo_url,
        chart: raw.chart.clone(),
        version,
        release_name: component_name.to_owned(),
        namespace: component_name.to_owned(),
        values_files,
        set_values: BTreeMap::new(),
    })
}

/// Resolves a manifests install method: every path made absolute against
/// `config_dir`.
fn resolve_manifests(raw: &RawManifestsInstallSpec, config_dir: &Path) -> ManifestInstallSpec {
    ManifestInstallSpec {
        paths: raw
            .paths
            .iter()
            .cloned()
            .map(|path| resolve_relative(config_dir, path))
            .collect(),
    }
}
