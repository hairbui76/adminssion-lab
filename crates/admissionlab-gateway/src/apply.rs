//! Installing a Gateway fixture into a lab cluster (ROADMAP Task 6.2).
//!
//! # These objects are PERSISTED, and the ephemeral cluster is what makes that safe
//!
//! Global Constraint 16 makes Kubernetes server-side dry-run the
//! authoritative fixture execution mode for Alpha admission testing, and
//! `admissionlab_fixtures::execute` implements exactly that: every
//! admission fixture is sent with `dryRun=All` and nothing is ever
//! written. **This module is the roadmap's own, explicit exception**,
//! stated in ROADMAP Phase 6's opening "Execution distinction":
//!
//! > Gateway fixtures are persisted in the disposable cluster because
//! > controller reconciliation and data-plane programming require
//! > durable resources. Persisted Gateway fixtures are isolated by the
//! > ephemeral cluster; Admission Lab never applies them to production.
//!
//! That is not a relaxation of the safety property, it is a restatement
//! of where the property comes from. A dry-run `Gateway` is never seen
//! by a controller, never gets a `Programmed` condition, and never
//! programs a listener, so the whole of Phase 6 would be unobservable
//! under dry-run. What keeps a persisted write safe is that there is
//! nothing durable to damage: the target is a `kind` cluster this run
//! created and this run will delete, and the *only* way this module can
//! reach an API server at all is through a
//! [`ClusterHandle::kubeconfig`], which `admissionlab-cluster` documents
//! as never the operator's ambient `~/.kube/config` and never shared
//! between a run's two sides. There is no code path here that reads an
//! environment variable, a default kubeconfig location, or a
//! current-context: [`apply_gateway_manifests`] takes a
//! [`ClusterHandle`] and builds its client from that handle's own file,
//! full stop.
//!
//! # Nothing is deleted on completion
//!
//! [`apply_gateway_manifests`] never deletes an object it created, not
//! even on a partial failure (ROADMAP Task 6.2 Step 4). Cluster teardown
//! is authoritative cleanup, and it is already guaranteed by
//! `admissionlab_core::run`'s own cleanup path. Per-object deletion here
//! would be strictly worse in both directions: on the success path it
//! would tear down exactly the state Tasks 6.3-6.8 exist to observe, and
//! on the failure path it would destroy the evidence a user needs to
//! understand *why* the fixture failed to install, in the one situation
//! (`--preserve-cluster`) where they explicitly asked to keep the
//! cluster around to look at.
//!
//! [`AppliedGatewayFixture::objects`] therefore is not a cleanup list.
//! It is a record of what this run put into the cluster -- provenance,
//! and the input a later task uses to know which objects a suite's
//! evidence should be about.
//!
//! # Parse and hash everything, then apply (Step 1)
//!
//! [`plan_gateway_apply`] reads, hashes, parses and validates *every*
//! manifest file before a single object is sent. A syntax error in the
//! last file therefore fails the whole suite with nothing half-applied,
//! rather than leaving a namespace and two backends behind in a cluster
//! whose Gateway never arrived. This is the same "Step 2 before Step 3,
//! always" discipline `admissionlab_installer::manifests` documents for
//! the component installer, applied to a different loader.
//!
//! ## Why this does not call `admissionlab_installer::load_manifest_bundle`
//!
//! That function exists, is public, and parses the same YAML -- but its
//! output shape cannot answer Task 6.2's questions. It returns one
//! aggregate `source_hash` for the whole bundle, while
//! [`AppliedGatewayFixture::source_hashes`] is frozen as a *per-file*
//! `BTreeMap<PathBuf, String>`; and it returns a flat `Vec<Value>` with
//! no record of which document came from which file, while every error
//! and every ordering decision here has to name a specific file and a
//! specific document within it. Reusing it would mean parsing twice (once
//! for the bundle, once to recover provenance) or reconstructing
//! provenance by re-splitting the files, which is not reuse. The
//! ~40 lines that are genuinely shared -- extension-based format
//! selection, multi-document YAML splitting, dropping a trailing null
//! document -- are re-derived here with that module named as their
//! source, and the parts that are *not* shared (per-file hashing;
//! `apiVersion`/`kind`/`metadata.name` validation, which the installer
//! deliberately leaves to `kubectl apply`) are what makes this a
//! different loader rather than a copy. What *is* reused wholesale is
//! `admissionlab_core::sha256_hex`, the one implementation of "a digest
//! over bytes" in this workspace, and
//! `admissionlab_fixtures::ResourceResolver`, the one implementation of
//! "what does this `apiVersion`/`kind` mean on this cluster".
//!
//! # Apply order (Step 2)
//!
//! ROADMAP Task 6.2 fixes the order by *category*, not by file:
//!
//! ```text
//! Namespace
//! Secret/ConfigMap
//! Service
//! Deployment/Pod
//! GatewayClass
//! Gateway
//! ReferenceGrant
//! HTTPRoute
//! ```
//!
//! [`ApplyCategory`] is that table, and [`plan_gateway_apply`] sorts by
//! it with a **stable** sort, so within one category the documents keep
//! the order they were written in (file order first, then position
//! within the file).
//!
//! ## `IngressClass` and `Ingress` are rows in that table too (Task 8.4)
//!
//! Task 6.2's table is written in Gateway API vocabulary because Phase 6
//! only ever applied Gateway API objects. ROADMAP Task 8.4 gave this
//! module a second kind of caller -- [`crate::ingress`], the runner that
//! *owns* applying a migration case's legacy `Ingress` side -- and two
//! rows were added for it, each placed beside its Gateway API
//! counterpart: [`ApplyCategory::IngressClass`] beside
//! [`ApplyCategory::GatewayClass`] (both are the cluster-scoped class
//! object a routing object names by string), and
//! [`ApplyCategory::Ingress`] beside [`ApplyCategory::HttpRoute`] (both
//! are the routing object every other category is a prerequisite for).
//! [`ApplyCategory::rank`] is the full order, and `tests/apply_unit.rs`
//! pins it.
//!
//! **Why add them at all**, when an unrecognized `Ingress` already sorted
//! into `Unknown` and `Unknown` is already applied last?
//! `fixtures/migration/ingress-nginx/basic-routing.yaml` relies on
//! exactly that today, says so in its own header, deliberately leaves the
//! decision to "the tasks that build the migration runner", and states
//! the one constraint it has: it "keeps working unchanged as long as the
//! row sits after `Workload`". The answer is that an `Ingress` landing
//! after its backends was an *accident of not being recognized* rather
//! than a decision, and a fixture whose correctness rests on a kind
//! staying unknown is one vendor CRD away from silently meaning something
//! else. Naming the row turns the property into a contract; it changes
//! nothing about where that fixture's `Ingress` is applied.
//!
//! **Why the two rows had to be added together.** Adding `Ingress` alone
//! would have been a regression. An `IngressClass` is also an
//! unrecognized kind today, so a fixture that declares its own class and
//! then an `Ingress` applies them in source order and works. Give
//! `Ingress` a rank while leaving `IngressClass` in `Unknown`, and the
//! class sorts *after* the `Ingress` that names it. Neither fixture in
//! this repository declares an `IngressClass` -- the controller's chart
//! installs one -- so this row exists to avoid creating a hazard that did
//! not exist before, not to serve a fixture that wants it.
//!
//! **This ordering overrides `admissionlab_installer::manifests`'s
//! rule, deliberately, and the two do not conflict.** That module
//! preserves the caller's declared `paths` order exactly and explicitly
//! refuses to sort, on the grounds that a user's ordering is deliberate
//! information. That is right for a *component install*, where the user
//! is describing a third-party stack whose internal dependencies only
//! they know. It is wrong for a Gateway fixture, where the dependency
//! graph is fixed by Gateway API itself and known here in full: an
//! `HTTPRoute` whose `parentRefs` name a `Gateway` that does not exist
//! yet gets `Accepted: False` / `reason: NoMatchingParent` from the
//! controller, and a fixture author who happened to list their routes
//! file first would see a spurious, timing-dependent reconciliation
//! failure on one side and not the other -- exactly the nondeterminism
//! Global Constraint 7 rules out. Sorting here removes a way to be
//! wrong; sorting there would remove information.
//!
//! **Unknown kinds go last, in source order.** Task 6.2's table ends
//! with "Unknown kinds preserve source order after known prerequisites",
//! and "after" is read here as *after every known category*, not as a
//! slot in the middle. An unknown kind in a Gateway fixture is
//! overwhelmingly something attached to the Gateway API objects (an
//! Istio `Telemetry`, an `EnvoyFilter`, a policy targeting a `Gateway`)
//! rather than something they depend on; CRDs, the one class of object
//! the Gateway API objects genuinely depend on, are installed by the
//! *stack* under test, not by a fixture. Both readings are defensible
//! from the table alone, so the choice is pinned by
//! `tests/apply_unit.rs`'s `unknown_kinds_are_applied_last_in_source_order`
//! rather than left to be rediscovered.
//!
//! # How objects are applied (Step 3): server-side apply, forced
//!
//! Each object is sent as a single server-side apply -- a `PATCH` with
//! `Content-Type: application/apply-patch+yaml`, `fieldManager`
//! [`FIELD_MANAGER`], and `force=true` -- through the dynamic
//! (`DynamicObject`) API, so an arbitrary Gateway API or vendor CRD
//! needs no generated Rust type. Three reasons, in order of weight:
//!
//! - **Idempotence.** A `CREATE` fails with `409 AlreadyExists` the
//!   second time. Re-running a suite against a cluster where part of it
//!   already exists is a real situation (a retried step, a fixture that
//!   also creates its own namespace) and it should converge, not fail.
//! - **Conflict semantics that are actually correct here.** Without
//!   `force`, a field already owned by another field manager makes the
//!   apply fail with `409 Conflict`. In a disposable, single-purpose lab
//!   cluster there is no other manager whose ownership deserves
//!   deference -- the only plausible other owner is a previous
//!   `admissionlab` apply of the same fixture. Forcing makes "the
//!   cluster now matches the fixture" the invariant. This is a
//!   deliberately different answer from the one a production `GitOps`
//!   controller should give, and it is safe only because of the
//!   ephemeral-cluster property above.
//! - **No read-modify-write.** The applied object is exactly the
//!   document the user wrote; there is no `GET`, no merge performed
//!   here, and no field this module adds. The same rule
//!   `admissionlab_fixtures::execute` states for admission fixtures
//!   holds here for the same reason: anything injected would change what
//!   the stack under test actually sees.
//!
//! A refusal from the API server is [`GatewayError::ApplyRejected`] and
//! stops the suite -- see [`crate::error`]'s own documentation for why
//! that is an error here even though the equivalent is an observation in
//! `admissionlab_fixtures::execute`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use admissionlab_admission::ObjectKey;
use admissionlab_core::{ClusterHandle, sha256_hex};
use admissionlab_fixtures::{KubeResourceResolver, ResolvedResource, ResourceResolver};
use kube::api::{Patch, PatchParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::core::DynamicObject;
use kube::{Api, Client, Config};
use serde::Deserialize;

use crate::error::GatewayError;

/// The `fieldManager` every server-side apply this module issues
/// identifies itself with.
///
/// Unlike `admissionlab_fixtures::execute`'s own field manager (which
/// has no lasting effect, because every request there is `dryRun=All`),
/// this one is recorded in each applied object's
/// `metadata.managedFields` and is what a forced re-apply takes
/// ownership *from*. A fixed, project-identifying name is therefore
/// load-bearing: it makes a second apply of the same fixture a no-op
/// conflict-wise rather than a fight between two anonymous managers.
pub const FIELD_MANAGER: &str = "admissionlab-gateway";

/// What one Gateway fixture installation put into a cluster.
///
/// Not a cleanup list -- see this module's "Nothing is deleted on
/// completion" section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedGatewayFixture {
    /// Every object applied, in the exact order it was applied (the
    /// [`ApplyCategory`] order, ties broken by source order). Each is
    /// identified by the group/version/plural the *cluster's own
    /// discovery* reported for it, plus the namespace the request
    /// actually targeted -- never a plural guessed from the kind.
    pub objects: Vec<ObjectKey>,
    /// SHA-256 of each manifest file's canonical source bytes, lowercase
    /// hex, keyed by the path it was read from.
    ///
    /// Per-file rather than one aggregate digest (the shape
    /// `admissionlab_installer::ManifestBundle::source_hash` uses),
    /// because this is what a run manifest needs to say *which* fixture
    /// file changed between two runs, not merely that something did.
    /// The bytes hashed are exactly as read from disk -- never a
    /// re-serialization of the parsed documents -- so comments and
    /// formatting are part of the identity of the file, matching
    /// `admissionlab_core::file_sha256`'s own rule.
    pub source_hashes: BTreeMap<PathBuf, String>,
}

/// Which apply-order category a document's `kind` falls into.
///
/// The variants *are* ROADMAP Task 6.2's ordering table, in its order;
/// [`ApplyCategory::for_kind`] is the mapping and
/// [`ApplyCategory::rank`] is the position. Derived `Ord` follows
/// declaration order, so reordering these variants would silently
/// reorder every apply -- which is why [`ApplyCategory::rank`] is
/// written out explicitly and pinned by a test rather than left implicit
/// in the derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ApplyCategory {
    /// `Namespace`. Everything namespaced below needs its namespace to
    /// exist first.
    Namespace,
    /// `Secret` and `ConfigMap`: data a workload mounts or reads at
    /// startup.
    Configuration,
    /// `Service`. Applied before the workloads behind it so an
    /// `HTTPRoute`'s `backendRefs` resolve as soon as the route lands.
    Service,
    /// `Deployment` and `Pod`: the backends themselves.
    Workload,
    /// `GatewayClass`, which a `Gateway` names in
    /// `spec.gatewayClassName`.
    GatewayClass,
    /// `IngressClass`, which an `Ingress` names in
    /// `spec.ingressClassName` -- the legacy counterpart of
    /// [`Self::GatewayClass`], and beside it for that reason. See this
    /// module's "`IngressClass` and `Ingress` are rows in that table
    /// too".
    IngressClass,
    /// `Gateway`, which an `HTTPRoute` names in `parentRefs`.
    Gateway,
    /// `ReferenceGrant`, which must exist before a cross-namespace
    /// `backendRef` can resolve.
    ReferenceGrant,
    /// `HTTPRoute`, the last Gateway API object and the one every other
    /// category is a prerequisite for.
    HttpRoute,
    /// `Ingress`, the legacy counterpart of [`Self::HttpRoute`]: the
    /// routing object a migration case's baseline side is *about*, and
    /// the one every other category is a prerequisite for. Applied after
    /// `HTTPRoute` only because something has to be last among the two
    /// routing objects; no fixture mixes them, and neither depends on
    /// the other.
    Ingress,
    /// Any other kind. Applied after every known category, preserving
    /// source order -- see this module's documentation for why "after
    /// known prerequisites" is read as "last".
    Unknown,
}

impl ApplyCategory {
    /// This category's position in the apply order, lowest first.
    ///
    /// Written out rather than derived from variant order so that the
    /// ordering contract survives someone reordering the enum for
    /// readability. `tests/apply_unit.rs` asserts the full sequence.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Namespace => 0,
            Self::Configuration => 1,
            Self::Service => 2,
            Self::Workload => 3,
            Self::GatewayClass => 4,
            Self::IngressClass => 5,
            Self::Gateway => 6,
            Self::ReferenceGrant => 7,
            Self::HttpRoute => 8,
            Self::Ingress => 9,
            Self::Unknown => 10,
        }
    }

    /// The category `kind` belongs to.
    ///
    /// Matched on `kind` alone, not on `apiVersion`, exactly as ROADMAP
    /// Task 6.2's table is written. That is a real (if small) risk --
    /// an unrelated CRD whose kind is literally `Gateway` (Istio's own
    /// `networking.istio.io/v1` `Gateway`, the most likely instance in
    /// this project's own domain) would sort with Gateway API's -- and
    /// it is accepted rather than silently "fixed": both are gateways
    /// that routes attach to, so the position the table gives them is
    /// the right one anyway, and narrowing the match to
    /// `gateway.networking.k8s.io` would push the Istio one into
    /// `Unknown` (applied last, *after* the routes that reference it),
    /// which is worse. Every kind here is matched case-sensitively,
    /// because a Kubernetes `kind` is case-sensitive.
    #[must_use]
    pub fn for_kind(kind: &str) -> Self {
        match kind {
            "Namespace" => Self::Namespace,
            "Secret" | "ConfigMap" => Self::Configuration,
            "Service" => Self::Service,
            "Deployment" | "Pod" => Self::Workload,
            "GatewayClass" => Self::GatewayClass,
            "IngressClass" => Self::IngressClass,
            "Gateway" => Self::Gateway,
            "ReferenceGrant" => Self::ReferenceGrant,
            "HTTPRoute" => Self::HttpRoute,
            "Ingress" => Self::Ingress,
            _ => Self::Unknown,
        }
    }
}

/// One manifest document, validated and placed in the apply order, but
/// not yet resolved against any cluster.
///
/// Everything here comes from the document itself and from where it was
/// read; nothing has been asked of a cluster yet, which is exactly what
/// makes [`plan_gateway_apply`] a pure, offline-testable function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedObject {
    /// The manifest file this document was read from.
    pub source: PathBuf,
    /// This document's zero-based position within `source`, counting
    /// every `---`-separated document including one later dropped for
    /// being null.
    pub document_index: usize,
    /// The document's `apiVersion`, verbatim.
    pub api_version: String,
    /// The document's `kind`, verbatim.
    pub kind: String,
    /// The document's `metadata.name`, verbatim. Never a
    /// `generateName` prefix -- see
    /// [`GatewayError::ManifestGenerateNameUnsupported`].
    pub name: String,
    /// The document's own `metadata.namespace`, exactly as written, or
    /// `None` if it did not carry one.
    ///
    /// This is what the *document* said, not the namespace the request
    /// will target: whether a namespace is needed at all depends on the
    /// cluster's own scope for the resource, which is not known until
    /// [`apply_gateway_plan_with_client`] resolves it. See
    /// [`effective_namespace`].
    pub namespace: Option<String>,
    /// Which apply-order category `kind` fell into.
    pub category: ApplyCategory,
    /// The document itself, parsed and otherwise untouched. This is the
    /// exact value sent to the API server.
    pub object: serde_json::Value,
}

/// Every document a Gateway fixture's manifests contain, in apply order,
/// plus each source file's content digest.
///
/// Produced entirely locally by [`plan_gateway_apply`] -- no cluster is
/// contacted -- so the whole of Task 6.2 Steps 1 and 2 is unit-testable
/// without any client at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayApplyPlan {
    /// Every document, sorted into [`ApplyCategory`] order with ties
    /// broken by source order (file order, then position within file).
    pub documents: Vec<PlannedObject>,
    /// Per-file content digests, as
    /// [`AppliedGatewayFixture::source_hashes`] carries them.
    pub source_hashes: BTreeMap<PathBuf, String>,
}

/// Applies a Gateway fixture's manifests to `cluster` and reports what
/// was installed.
///
/// Reads, hashes, parses and validates every file in `manifests` before
/// sending anything (see this module's "Parse and hash everything, then
/// apply" section), then applies each document in the fixed category
/// order, one server-side apply at a time, waiting for each to complete
/// before the next is sent -- so a later object never reaches the API
/// server before the prerequisite it depends on is confirmed persisted.
///
/// Objects are **left in the cluster**; nothing here deletes them. See
/// this module's "Nothing is deleted on completion" section.
///
/// # Errors
///
/// Returns a [`GatewayError`] manifest variant if any file cannot be
/// read or any document within it is malformed, incomplete, or uses
/// `metadata.generateName` -- in every case before a single object is
/// applied. Returns [`GatewayError::ResourceResolution`] if a document's
/// `apiVersion`/`kind` is not served by `cluster`,
/// [`GatewayError::ApplyUnavailable`] if `cluster`'s kubeconfig could
/// not be turned into a usable client or an exchange failed at the
/// transport level, and [`GatewayError::ApplyRejected`] if the API
/// server refused an object. A failure part-way through leaves every
/// object applied before it in the cluster, deliberately.
pub async fn apply_gateway_manifests(
    cluster: &ClusterHandle,
    manifests: &[PathBuf],
) -> Result<AppliedGatewayFixture, GatewayError> {
    let plan = plan_gateway_apply(manifests)?;
    let client = client_for(cluster)
        .await
        .map_err(|source| GatewayError::ApplyUnavailable {
            cluster: cluster.spec.name.clone(),
            object: "(none -- no client could be built)".to_string(),
            reason: source.to_string(),
        })?;
    let resolver = KubeResourceResolver::new();
    apply_gateway_plan_with_client(cluster, &resolver, client, &plan).await
}

/// [`apply_gateway_manifests`]'s offline-testable core: given an
/// already-built `client` and an already-planned `plan`, resolves and
/// applies each document in order.
///
/// The split mirrors `admissionlab_fixtures::execute::dry_run_create` /
/// `dry_run_create_with_client` and
/// `admissionlab_installer::readiness`'s own: turning a
/// [`ClusterHandle`]'s on-disk kubeconfig into a network-connecting
/// client has no seam to swap a fake into, so it stays in the thin
/// wrapper above and is exercised live by an end-to-end gate; everything
/// downstream of an existing `Client` is here, where
/// `tests/apply_unit.rs` drives it against a `tower_test::mock`-backed
/// one.
///
/// `cluster` is still required because
/// [`ResourceResolver::resolve`] takes one (it caches discovery per
/// cluster and labels its errors with the cluster's name); it is never
/// used to build or look up `client`, which is `client`'s own job. A
/// test therefore pairs a mock `client` with a fabricated
/// [`ClusterHandle`] whose kubeconfig need not exist, and a fake
/// [`ResourceResolver`] that never reads it.
///
/// # Errors
///
/// See [`apply_gateway_manifests`]; this function raises every error
/// except the manifest-loading ones (already done) and the
/// client-construction one.
pub async fn apply_gateway_plan_with_client(
    cluster: &ClusterHandle,
    resolver: &dyn ResourceResolver,
    client: Client,
    plan: &GatewayApplyPlan,
) -> Result<AppliedGatewayFixture, GatewayError> {
    // `PatchParams::apply(..).force()` -- see this module's "How objects
    // are applied" section for why forcing is the correct answer in a
    // disposable cluster and would not be in a durable one.
    let patch_params = PatchParams::apply(FIELD_MANAGER).force();

    let mut objects = Vec::with_capacity(plan.documents.len());
    for planned in &plan.documents {
        let resource = resolver
            .resolve(cluster, &planned.api_version, &planned.kind)
            .await?;
        let key = object_key(planned, &resource);

        let api: Api<DynamicObject> = if resource.namespaced {
            Api::namespaced_with(
                client.clone(),
                &effective_namespace(planned),
                &resource.api_resource,
            )
        } else {
            Api::all_with(client.clone(), &resource.api_resource)
        };

        api.patch(&planned.name, &patch_params, &Patch::Apply(&planned.object))
            .await
            .map_err(|source| apply_failure(&cluster.spec.name, &key, source))?;

        objects.push(key);
    }

    Ok(AppliedGatewayFixture {
        objects,
        source_hashes: plan.source_hashes.clone(),
    })
}

/// Reads, hashes, parses and validates every manifest file in
/// `manifests`, and returns their documents in apply order (ROADMAP Task
/// 6.2 Steps 1-2).
///
/// Touches no cluster at all. Exact-duplicate paths are read once, at
/// their first position -- the same rule (and the same reasoning)
/// `admissionlab_installer::manifests` documents for its own `paths`
/// list, and here it additionally keeps
/// [`AppliedGatewayFixture::source_hashes`] from depending on how many
/// times a path happened to be repeated.
///
/// # Errors
///
/// Returns [`GatewayError::ManifestRead`] if a file cannot be read;
/// [`GatewayError::ManifestParse`] if a document is not syntactically
/// valid; [`GatewayError::ManifestNotAnObject`] if a document is a
/// scalar or a sequence; [`GatewayError::ManifestMissingField`] if a
/// document lacks `apiVersion`, `kind`, or `metadata.name`; and
/// [`GatewayError::ManifestGenerateNameUnsupported`] for a
/// `generateName`-only document. Files are read in order and the first
/// failure stops the walk, so a later file is never even opened.
pub fn plan_gateway_apply(manifests: &[PathBuf]) -> Result<GatewayApplyPlan, GatewayError> {
    let mut source_hashes = BTreeMap::new();
    let mut documents = Vec::new();

    for path in deduplicate_paths(manifests) {
        let bytes = std::fs::read(&path).map_err(|source| GatewayError::ManifestRead {
            path: path.clone(),
            source,
        })?;
        source_hashes.insert(path.clone(), sha256_hex(&bytes));

        for (document_index, value) in parse_manifest_file(&path, &bytes)? {
            documents.push(plan_document(&path, document_index, value)?);
        }
    }

    // `sort_by_key`, which is guaranteed stable, so documents within one
    // category keep their source order (file order, then position within
    // the file) -- the "unknown kinds preserve source order" half of
    // Step 2, and equally the reason two `Service`s in one file stay in
    // the order they were written.
    documents.sort_by_key(|planned| planned.category.rank());

    Ok(GatewayApplyPlan {
        documents,
        source_hashes,
    })
}

/// The namespace a request for `planned` targets, for a resource the
/// cluster reports as namespaced: the document's own
/// `metadata.namespace`, or `"default"`.
///
/// The same fallback (and the same rule that this only ever *reads* the
/// field -- the object sent to the API server is never rewritten to add
/// one) as `admissionlab_fixtures::execute::namespace_of`, which is
/// where the reasoning lives: it is what a plain `kubectl apply -f`
/// with no `-n` flag would resolve to. Not called through to that
/// function because it takes an `admissionlab_fixtures::FixtureSource`,
/// a type this module has no reason to construct.
fn effective_namespace(planned: &PlannedObject) -> String {
    planned
        .namespace
        .clone()
        .unwrap_or_else(|| "default".to_string())
}

/// The [`ObjectKey`] identifying `planned` as applied against
/// `resource`.
///
/// Group, version and plural come from the *cluster's own discovery*
/// (via [`ResolvedResource`]), never from guessing a plural off the
/// kind -- see `admissionlab_fixtures::resources`'s documentation for
/// why that guess is observably wrong for real CRDs (Global Constraint
/// 15). `namespace` is `None` for a cluster-scoped resource even if the
/// document carried one, because it records which namespace the
/// *request* named, and a cluster-scoped request names none.
fn object_key(planned: &PlannedObject, resource: &ResolvedResource) -> ObjectKey {
    ObjectKey {
        group: resource.api_resource.group.clone(),
        version: resource.api_resource.version.clone(),
        resource: resource.api_resource.plural.clone(),
        namespace: resource
            .namespaced
            .then(|| effective_namespace(planned))
            .clone(),
        name: planned.name.clone(),
    }
}

/// Splits a failed apply into "the API server gave a real, structured
/// refusal" ([`GatewayError::ApplyRejected`]) and "no answer could be
/// obtained at all" ([`GatewayError::ApplyUnavailable`]).
///
/// `kube::Error::Api` is exactly the first case: `kube` produces it only
/// after decoding a Kubernetes `Status` object out of a non-2xx
/// response, so its `code`/`reason`/`message` are the API server's own
/// words rather than this crate's interpretation. Every other
/// `kube::Error` (transport, TLS, serialization, an undecodable body) is
/// the second. `code: 0` is mapped to `None` rather than reported as a
/// status code, because `ErrorResponse::code` is a plain `u16` with no
/// "absent" representation and `0` is not an HTTP status -- fabricating
/// a plausible `403` there is exactly what Global Constraint 15 forbids.
fn apply_failure(cluster_name: &str, key: &ObjectKey, source: kube::Error) -> GatewayError {
    match source {
        kube::Error::Api(response) => GatewayError::ApplyRejected {
            cluster: cluster_name.to_string(),
            object: key.to_string(),
            code: (response.code != 0).then_some(response.code),
            reason: (!response.reason.is_empty()).then(|| response.reason.clone()),
            message: response.message,
        },
        other => GatewayError::ApplyUnavailable {
            cluster: cluster_name.to_string(),
            object: key.to_string(),
            reason: other.to_string(),
        },
    }
}

/// Validates one parsed document and places it in the apply order.
fn plan_document(
    path: &Path,
    document_index: usize,
    object: serde_json::Value,
) -> Result<PlannedObject, GatewayError> {
    let map = match &object {
        serde_json::Value::Object(map) => map,
        other => {
            return Err(GatewayError::ManifestNotAnObject {
                path: path.to_path_buf(),
                document_index,
                found: json_shape(other),
            });
        }
    };

    let required = |field: &'static str, value: Option<&serde_json::Value>| {
        value
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
            .ok_or(GatewayError::ManifestMissingField {
                path: path.to_path_buf(),
                document_index,
                field,
            })
    };

    let api_version = required("apiVersion", map.get("apiVersion"))?;
    let kind = required("kind", map.get("kind"))?;
    let metadata = map.get("metadata");

    // `generateName` is checked before `name` is required, so a document
    // that has only `generateName` gets the specific explanation rather
    // than a bare "missing metadata.name".
    let has_generate_name = metadata
        .and_then(|metadata| metadata.get("generateName"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    let name_field = metadata.and_then(|metadata| metadata.get("name"));
    let has_name = name_field
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if has_generate_name && !has_name {
        return Err(GatewayError::ManifestGenerateNameUnsupported {
            path: path.to_path_buf(),
            document_index,
        });
    }
    let name = required("metadata.name", name_field)?;

    let namespace = metadata
        .and_then(|metadata| metadata.get("namespace"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|namespace| !namespace.is_empty())
        .map(str::to_owned);

    Ok(PlannedObject {
        source: path.to_path_buf(),
        document_index,
        category: ApplyCategory::for_kind(&kind),
        api_version,
        kind,
        name,
        namespace,
        object,
    })
}

/// Removes exact-duplicate paths, keeping each distinct path's first
/// occurrence and otherwise preserving the given order. See
/// [`plan_gateway_apply`].
fn deduplicate_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen: BTreeSet<&Path> = BTreeSet::new();
    paths
        .iter()
        .filter(|path| seen.insert(path.as_path()))
        .cloned()
        .collect()
}

/// Parses `bytes` (the on-disk contents of `path`) into
/// `(document_index, value)` pairs.
///
/// Format is chosen from the extension -- `.json` (case-insensitively)
/// is JSON, everything else is YAML -- and a YAML document that parses
/// to null (a file ending in a bare `---`) is dropped rather than
/// becoming a spurious entry. Both rules, and the reasoning behind them,
/// are `admissionlab_installer::manifests`'s; see this module's "Why
/// this does not call `load_manifest_bundle`" section for why they are
/// re-derived here instead of called. `document_index` counts every
/// `---`-separated block including a dropped null one, so it always
/// names the block a user would count to in their editor.
fn parse_manifest_file(
    path: &Path,
    bytes: &[u8],
) -> Result<Vec<(usize, serde_json::Value)>, GatewayError> {
    let is_json = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));

    if is_json {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|error| GatewayError::ManifestParse {
                path: path.to_path_buf(),
                document_index: 0,
                format: "JSON",
                reason: error.to_string(),
            })?;
        return Ok(vec![(0, value)]);
    }

    let mut documents = Vec::new();
    for (document_index, document) in serde_norway::Deserializer::from_slice(bytes).enumerate() {
        let value = serde_json::Value::deserialize(document).map_err(|error| {
            GatewayError::ManifestParse {
                path: path.to_path_buf(),
                document_index,
                format: "YAML",
                reason: error.to_string(),
            }
        })?;
        if !value.is_null() {
            documents.push((document_index, value));
        }
    }
    Ok(documents)
}

/// A human-readable name for what a document parsed to, for
/// [`GatewayError::ManifestNotAnObject`]'s message.
const fn json_shape(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// Builds a `kube::Client` for `cluster` from its own isolated
/// kubeconfig -- never the operator's ambient `~/.kube/config`.
///
/// The third copy of this four-line function in this workspace, after
/// `admissionlab_installer::readiness::client_for` and
/// `admissionlab_fixtures::resources::client_for`. The second one's own
/// documentation records why it did not reach across a crate boundary
/// for the first ("adding that edge for four lines is not worth the
/// extra dependency-graph surface"); the same applies here, with the
/// additional point that `admissionlab-fixtures`'s copy is `pub(crate)`
/// and so is not reachable from this crate even though the edge already
/// exists. Kept as its own function, never inlined into
/// [`apply_gateway_manifests`], so its error path is exercisable
/// offline against a deliberately missing kubeconfig.
async fn client_for(cluster: &ClusterHandle) -> Result<Client, kube::Error> {
    let kubeconfig = Kubeconfig::read_from(&cluster.kubeconfig)?;
    let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default()).await?;
    Client::try_from(config)
}
