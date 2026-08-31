//! `bootstrap` mode: the init container step that makes this component
//! self-bootstrapping. Generates this pod's CA/serving certificate
//! ([`crate::cert::generate`]), writes the serving certificate/key where
//! `serve` mode's main container will read them from, and updates every
//! webhook configuration this recipe already applied — the one
//! `ValidatingWebhookConfiguration` and both
//! `MutatingWebhookConfiguration`s (Task 3.9;
//! `recipes/test-webhook/manifests/`) — so their `caBundle`s actually
//! validate that serving certificate. All three are patched through one
//! generic [`patch_ca_bundle`], via [`CaBundleTarget`]; see that trait's
//! own documentation for why the two Kubernetes kinds need a trait at
//! all.
//!
//! # Where the private key goes — and where it never goes
//!
//! [`write_cert_files`] writes exactly three files under [`CERT_DIR`]:
//! `ca.crt`, `tls.crt` (both public), and `tls.key` (the private key,
//! written `0600`). [`CERT_DIR`] is a Kubernetes `emptyDir` volume
//! (`medium: Memory` — see `recipes/test-webhook/manifests/30-deployment.yaml`)
//! shared between this init container and `serve` mode's main container
//! in the *same* pod — nothing else. This is deliberately **not** a
//! Kubernetes `Secret` object:
//!
//! - A `Secret`, even one never checked into git, is still persisted
//!   into the cluster's etcd store (base64-encoded, not encrypted at
//!   rest on a default `kind` cluster) for as long as the object exists.
//!   A `Memory`-backed `emptyDir` is never written to durable storage at
//!   all, and is reclaimed the moment the pod that owns it is deleted —
//!   there is no separate object outliving the pod for anything to leak
//!   from.
//! - It needs no RBAC of its own: this init container's `ServiceAccount`
//!   is granted exactly `get`/`update` on the three named webhook
//!   configuration objects this recipe installs, by `resourceNames` (see
//!   the RBAC manifest and this crate's own top-level report) — nothing
//!   that would let it create, read, or list `Secret` objects anywhere in
//!   the cluster.
//! - Kubernetes guarantees init containers run to completion, in order,
//!   *before* any regular container in the same pod starts — so there is
//!   no window in which `serve` mode's main container could start before
//!   these files exist; no ordering coordination beyond that guarantee
//!   is needed. A container *restart* (kubelet restarting a crashed
//!   container in place, `restartPolicy: Always`) does not re-run this
//!   init container and does not wipe the `emptyDir` — only the pod's
//!   own deletion/recreation does either, and a fresh pod always means a
//!   fresh call to [`crate::cert::generate`] regardless (see that
//!   module's own documentation on the "per cluster" property).
//!
//! Nothing in this module — or anywhere else in this crate — ever opens
//! a path inside this repository's own working tree for writing. Every
//! path this module touches ([`CERT_DIR`], [`SERVICE_ACCOUNT_NAMESPACE_FILE`])
//! is fixed, absolute, and only ever meaningful inside a running pod's
//! own filesystem.
//!
//! # Every guarantee above is per-pod; none of it composes across pods
//!
//! Read the section above again with this in mind: every safety
//! argument in it — the ordering guarantee, the "restart doesn't
//! re-run this" guarantee, "a fresh pod always means a fresh call to
//! [`crate::cert::generate`]" — is reasoned entirely about *one* pod in
//! isolation. None of it says anything about what happens if *two*
//! pods of this Deployment ever run at once, and the honest answer is:
//! nothing good. [`patch_ca_bundle`] writes to single, cluster-wide
//! `webhooks[].clientConfig.caBundle` fields — there is exactly one of
//! each per cluster, not one per pod — via
//! fetch-mutate-[`Api::replace`], with no lease, no leader election, no
//! compare-and-swap beyond what `resourceVersion` already gives a
//! single writer racing nothing. Two pods each generate their own
//! independent CA ([`crate::cert::generate`] takes no cluster-wide
//! input at all — that is the entire point of "per cluster," but it
//! also means "per pod" the moment more than one pod exists) and race
//! to overwrite the same field with two different CAs. Whichever
//! `replace` lands last wins `caBundle`; the `Service` in front of both
//! pods keeps round-robining traffic to *both*, including the pod whose
//! own serving certificate no longer matches the CA a caller now
//! trusts.
//!
//! Neither of this recipe's declared readiness checks would notice:
//! `deploymentAvailable` only reads the Deployment's own `Available`
//! condition, and `webhookConfigurationPresent` only checks that the
//! `ValidatingWebhookConfiguration` object exists
//! (`admissionlab_installer::readiness::evaluate`) — neither compares
//! the live `caBundle` against what either pod is actually serving. A
//! scaled-up version of this recipe would report itself ready while
//! genuinely broken.
//!
//! This is why `recipes/test-webhook/manifests/30-deployment.yaml` pins
//! `replicas: 1` with a comment pointing back at this exact paragraph,
//! and why `tests/recipe_loads.rs`'s
//! `deployment_replicas_is_pinned_to_one` fails the moment that number
//! changes. Raising it is not a one-line edit: it requires an actual
//! redesign of how the CA and `caBundle` are produced and agreed on
//! (electing a single bootstrapping pod, or moving CA generation out of
//! the pod entirely) — not just a bigger number.
//!
//! # `caBundle`: retried, never assumed present on the first attempt
//!
//! `recipes/test-webhook/manifests/20-webhook-configuration.yaml` and
//! `21-mutating-webhook-configurations.yaml` both apply with no
//! `caBundle` set at all (omitted, not a placeholder value — Kubernetes
//! accepts that; nothing trusts this webhook's TLS server until
//! [`patch_ca_bundle`] fills it in). This crate's own recipe applies
//! both files *before* the Deployment's manifest file (`install.paths`,
//! in `recipes/test-webhook/recipe.yaml` —
//! `admissionlab-installer`'s `ManifestsInstaller` applies each file in
//! that declared order, one `kubectl apply` at a time), so in the common
//! case every webhook configuration object already exists by the time
//! this init container starts. [`patch_ca_bundle`] does not
//! *assume* that ordering held, though: a "not found" `get` is retried
//! with a short fixed interval up to [`GET_RETRY_DEADLINE`], the same
//! "poll, do not assume immediate consistency" discipline
//! `admissionlab-installer::readiness` already uses for exactly this
//! shape of problem (a Kubernetes object that may not exist the instant
//! after something else applied it).

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use k8s_openapi::ByteString;
use k8s_openapi::Resource as _;
use k8s_openapi::api::admissionregistration::v1::{
    MutatingWebhookConfiguration, ValidatingWebhookConfiguration,
};
use kube::api::{Api, PostParams};

use crate::cert::{self, GeneratedCerts};
use crate::config::{
    self, MUTATING_WEBHOOK_CONFIGURATION_NAMES_ENV, SERVICE_NAME_ENV,
    WEBHOOK_CONFIGURATION_NAME_ENV,
};

/// Where `serve` mode's main container expects `tls.crt`/`tls.key`
/// (`ca.crt` is written alongside them for operator debugging — nothing
/// in this crate itself reads `ca.crt` back). A `Memory`-backed
/// `emptyDir` mount, shared between this init container and the main
/// container in the same pod — see this module's own documentation.
pub(crate) const CERT_DIR: &str = "/certs";

/// The standard in-cluster path Kubernetes projects a pod's own
/// namespace into, via its (default-mounted) `ServiceAccount` token
/// volume — the same file `kube::Config::incluster` itself reads to
/// build a client's default namespace. Reading it directly here needs no
/// extra manifest wiring (no Downward API field) beyond the standard
/// `ServiceAccount` token mount every pod already gets.
const SERVICE_ACCOUNT_NAMESPACE_FILE: &str =
    "/var/run/secrets/kubernetes.io/serviceaccount/namespace";

/// How long [`patch_ca_bundle`] retries a "not found"
/// `ValidatingWebhookConfiguration` lookup before giving up — generous
/// for a slow/loaded CI runner, bounded so a genuinely missing object
/// (a real misconfiguration, not a timing race) fails loudly rather than
/// hanging the init container — and therefore the pod, and therefore
/// this component's readiness — forever.
const GET_RETRY_DEADLINE: Duration = Duration::from_secs(60);

/// The fixed delay between retry attempts. Deliberately not exponential
/// backoff (unlike `admissionlab-installer::readiness::BackoffPolicy`):
/// this retries exactly one specific, already-named object against a
/// cluster this pod's own Deployment is itself part of installing, not a
/// long-running poll against components that may take minutes to
/// stabilize — a short fixed interval keeps the common, fast-converging
/// case fast without the added complexity of a capped-backoff schedule
/// this narrower use case does not need.
const GET_RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// Everything that can go wrong running `bootstrap` mode end to end.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// This pod's own namespace could not be read from
    /// [`SERVICE_ACCOUNT_NAMESPACE_FILE`].
    #[error("failed to read this pod's namespace from {SERVICE_ACCOUNT_NAMESPACE_FILE}: {source}")]
    ReadNamespace {
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// A required environment variable was missing or invalid — see
    /// [`crate::config`].
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    /// Certificate generation itself failed — see [`crate::cert`].
    #[error(transparent)]
    Cert(#[from] cert::CertError),
    /// A certificate/key file could not be written under [`CERT_DIR`].
    #[error("failed to write {}: {source}", path.display())]
    WriteFile {
        /// The file that could not be written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// A Kubernetes client could not be built from this pod's in-cluster
    /// configuration.
    #[error("failed to build an in-cluster Kubernetes client: {0}")]
    Client(#[source] Box<kube::Error>),
    /// The named webhook configuration never appeared within
    /// [`GET_RETRY_DEADLINE`].
    #[error("{kind} {name:?} was not found within {GET_RETRY_DEADLINE:?}: {source}")]
    WebhookConfigurationNotFound {
        /// Which webhook configuration kind this was — that type's own
        /// `k8s_openapi::Resource::KIND` constant.
        kind: &'static str,
        /// The webhook configuration name that was never found.
        name: String,
        /// The last "not found" (or other) error observed while
        /// retrying. Boxed (mirroring `admissionlab_installer::InstallError`'s
        /// own `Box<ReadinessCheck>`/`Box<InstallError>` fields, for the
        /// same reason): `kube::Error` is large enough that clippy's
        /// `result_large_err` flags every `Result<_, BootstrapError>`-returning
        /// function in this module once it is inlined directly.
        #[source]
        source: Box<kube::Error>,
    },
    /// The named webhook configuration exists but declares no
    /// `webhooks` entries — this recipe's own manifests always declare
    /// at least one per configuration, so this can only mean something
    /// else already replaced the object with an unexpected shape.
    #[error("{kind} {name:?} has no `webhooks` entries; expected at least one")]
    NoWebhookEntries {
        /// Which webhook configuration kind this was — that type's own
        /// `k8s_openapi::Resource::KIND` constant.
        kind: &'static str,
        /// The webhook configuration name with no entries.
        name: String,
    },
    /// The updated webhook configuration could not be written back.
    #[error("failed to update {kind} {name:?}'s caBundle: {source}")]
    Replace {
        /// Which webhook configuration kind this was — that type's own
        /// `k8s_openapi::Resource::KIND` constant.
        kind: &'static str,
        /// The webhook configuration name that could not be updated.
        name: String,
        /// The underlying Kubernetes API failure. Boxed — see
        /// [`BootstrapError::WebhookConfigurationNotFound`]'s own `source`
        /// field documentation for why.
        #[source]
        source: Box<kube::Error>,
    },
}

/// Runs `bootstrap` mode end to end: generate this pod's CA/serving
/// certificate, write the serving certificate/key for `serve` mode to
/// pick up, and update every webhook configuration this recipe installs
/// — the one `ValidatingWebhookConfiguration` *and* each
/// `MutatingWebhookConfiguration` named by
/// [`MUTATING_WEBHOOK_CONFIGURATION_NAMES_ENV`] — so their `caBundle`s
/// match. Idempotent in effect (a re-run generates a *different* CA/cert
/// pair and simply overwrites every destination), but never re-run in
/// practice: Kubernetes runs an init container exactly once per pod (see
/// this module's own documentation).
///
/// The configurations are patched one at a time, and the first failure
/// aborts the rest: a partially-trusted install (some webhooks trusting
/// this pod's CA, others still trusting nothing) is not a state worth
/// continuing into, and failing the init container instead means the pod
/// never becomes Ready and the recipe's own `deploymentAvailable`
/// readiness check surfaces it (`recipes/test-webhook/recipe.yaml`).
///
/// # Errors
///
/// Returns [`BootstrapError`] for any failure along that path — see each
/// variant's own documentation for exactly which step it corresponds to.
pub async fn run() -> Result<(), BootstrapError> {
    let namespace = read_namespace()?;
    let service_name = config::read_required(SERVICE_NAME_ENV)?;
    let validating_name = config::read_required(WEBHOOK_CONFIGURATION_NAME_ENV)?;
    let mutating_names = config::read_required_list(MUTATING_WEBHOOK_CONFIGURATION_NAMES_ENV)?;

    tracing::info!(
        namespace = %namespace,
        service_name = %service_name,
        validating_webhook_configuration = %validating_name,
        mutating_webhook_configurations = %mutating_names.join(","),
        "generating a deterministic, test-only CA and serving certificate for this cluster"
    );
    let certs = cert::generate(&service_name, &namespace)?;

    write_cert_files(Path::new(CERT_DIR), &certs)?;
    tracing::info!(cert_dir = CERT_DIR, "wrote serving certificate and key");

    let client = kube::Client::try_default()
        .await
        .map_err(|source| BootstrapError::Client(Box::new(source)))?;

    patch_ca_bundle::<ValidatingWebhookConfiguration>(
        &client,
        &validating_name,
        &certs.ca_cert_pem,
    )
    .await?;
    tracing::info!(
        kind = ValidatingWebhookConfiguration::KIND,
        name = %validating_name,
        "updated caBundle"
    );
    for name in &mutating_names {
        patch_ca_bundle::<MutatingWebhookConfiguration>(&client, name, &certs.ca_cert_pem).await?;
        tracing::info!(
            kind = MutatingWebhookConfiguration::KIND,
            name = %name,
            "updated caBundle"
        );
    }

    Ok(())
}

/// The one thing [`patch_ca_bundle`] needs from a webhook configuration
/// object that `k8s-openapi`'s generated types do not already express
/// uniformly: `ValidatingWebhookConfiguration` and
/// `MutatingWebhookConfiguration` are structurally identical for this
/// purpose but have entirely separate generated entry types
/// (`ValidatingWebhook` / `MutatingWebhook`), so there is no shared
/// trait upstream to reach `clientConfig.caBundle` through.
///
/// Deliberately narrow: a `&mut` borrow of each entry's own `caBundle`
/// slot, nothing else. [`patch_ca_bundle`] then contains exactly one
/// copy of the fetch-retry/mutate/`replace` logic rather than one copy
/// per kind — two copies that would then have to be kept in sync by
/// hand, which is precisely the drift this crate elsewhere writes
/// dedicated tests to prevent (see [`crate::serve`]'s own
/// `cert_dir_matches_bootstrap`).
/// The `kind` string used in this module's error messages and log
/// fields comes from `k8s_openapi::Resource::KIND`, that type's own
/// generated constant, rather than a hand-spelled literal — one fewer
/// pair of strings that could drift apart.
trait CaBundleTarget:
    kube::Resource<DynamicType = ()>
    + k8s_openapi::Resource
    + Clone
    + std::fmt::Debug
    + serde::de::DeserializeOwned
    + serde::Serialize
{
    /// Every `webhooks[].clientConfig.caBundle` slot in this object, in
    /// declaration order. Empty exactly when the object declares no
    /// `webhooks` entries at all.
    fn ca_bundle_slots(&mut self) -> Vec<&mut Option<ByteString>>;
}

impl CaBundleTarget for ValidatingWebhookConfiguration {
    fn ca_bundle_slots(&mut self) -> Vec<&mut Option<ByteString>> {
        self.webhooks
            .iter_mut()
            .flatten()
            .map(|webhook| &mut webhook.client_config.ca_bundle)
            .collect()
    }
}

impl CaBundleTarget for MutatingWebhookConfiguration {
    fn ca_bundle_slots(&mut self) -> Vec<&mut Option<ByteString>> {
        self.webhooks
            .iter_mut()
            .flatten()
            .map(|webhook| &mut webhook.client_config.ca_bundle)
            .collect()
    }
}

/// Reads this pod's own namespace from [`SERVICE_ACCOUNT_NAMESPACE_FILE`].
fn read_namespace() -> Result<String, BootstrapError> {
    std::fs::read_to_string(SERVICE_ACCOUNT_NAMESPACE_FILE)
        .map(|contents| contents.trim().to_owned())
        .map_err(|source| BootstrapError::ReadNamespace { source })
}

/// Writes `certs`' three files under `dir`: `ca.crt`/`tls.crt` world
/// readable, `tls.key` owner-only (`0600`).
fn write_cert_files(dir: &Path, certs: &GeneratedCerts) -> Result<(), BootstrapError> {
    write_file(&dir.join("ca.crt"), certs.ca_cert_pem.as_bytes(), 0o644)?;
    write_file(
        &dir.join("tls.crt"),
        certs.server_cert_pem.as_bytes(),
        0o644,
    )?;
    write_file(&dir.join("tls.key"), certs.server_key_pem.as_bytes(), 0o600)?;
    Ok(())
}

/// Writes `contents` to `path` and sets its Unix permission bits to
/// `mode`.
fn write_file(path: &Path, contents: &[u8], mode: u32) -> Result<(), BootstrapError> {
    std::fs::write(path, contents).map_err(|source| BootstrapError::WriteFile {
        path: path.to_path_buf(),
        source,
    })?;
    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions).map_err(|source| BootstrapError::WriteFile {
        path: path.to_path_buf(),
        source,
    })
}

/// Fetches the named webhook configuration of kind `K` (retrying "not
/// found" up to [`GET_RETRY_DEADLINE`] — see this module's own
/// documentation), sets every one of its `webhooks[].clientConfig.caBundle`
/// entries to `ca_cert_pem`, and writes the object back with
/// [`Api::replace`].
///
/// Deliberately fetch-mutate-`replace` rather than a JSON Merge Patch:
/// this preserves every other field of the live object exactly as
/// `kubectl apply` last set it (including `metadata.resourceVersion`,
/// which `replace` needs for the API server's own optimistic-concurrency
/// check) — a merge patch targeting the whole `webhooks` array would
/// instead need to reconstruct every other field of that array's one
/// entry (`rules`, `namespaceSelector`, `failurePolicy`, and so on) from
/// this Rust code too, or risk silently clobbering them back to
/// whatever this code happened to send.
async fn patch_ca_bundle<K: CaBundleTarget>(
    client: &kube::Client,
    name: &str,
    ca_cert_pem: &str,
) -> Result<(), BootstrapError> {
    let api: Api<K> = Api::all(client.clone());

    let mut config = get_with_retry::<K>(&api, name).await?;

    let slots = config.ca_bundle_slots();
    if slots.is_empty() {
        return Err(BootstrapError::NoWebhookEntries {
            kind: K::KIND,
            name: name.to_owned(),
        });
    }
    for slot in slots {
        *slot = Some(ByteString(ca_cert_pem.as_bytes().to_vec()));
    }

    api.replace(name, &PostParams::default(), &config)
        .await
        .map(|_| ())
        .map_err(|source| BootstrapError::Replace {
            kind: K::KIND,
            name: name.to_owned(),
            source: Box::new(source),
        })
}

/// Retries `api.get(name)` at [`GET_RETRY_INTERVAL`] until it succeeds
/// or [`GET_RETRY_DEADLINE`] elapses. Always attempts at least once, even
/// if the deadline were somehow already past.
async fn get_with_retry<K: CaBundleTarget>(api: &Api<K>, name: &str) -> Result<K, BootstrapError> {
    let deadline = Instant::now() + GET_RETRY_DEADLINE;
    loop {
        match api.get(name).await {
            Ok(found) => return Ok(found),
            Err(source) => {
                if Instant::now() >= deadline {
                    return Err(BootstrapError::WebhookConfigurationNotFound {
                        kind: K::KIND,
                        name: name.to_owned(),
                        source: Box::new(source),
                    });
                }
                tracing::debug!(
                    kind = K::KIND,
                    name,
                    error = %source,
                    "webhook configuration not found yet; retrying"
                );
                tokio::time::sleep(GET_RETRY_INTERVAL).await;
            }
        }
    }
}
