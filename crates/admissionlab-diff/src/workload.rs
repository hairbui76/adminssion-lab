//! Comparing what the two API servers *did to the object*.
//!
//! [`diff_workload_objects`] takes two normalized objects -- the same
//! fixture as each side's admission chain finally left it -- and
//! classifies the differences a human reasons about in pod terms: a
//! sidecar that appeared, an init container that stopped being injected,
//! an image that moved, a mount or an environment entry that changed.
//! What the API server *decided* is [`crate::admission`]'s question; what
//! the webhook chain did on the way is Task 4.6's.
//!
//! # Everything is keyed by name, never by index
//!
//! `spec.containers` is an array, but it is a *set with a stable key*:
//! two stacks that inject the same sidecar in different array positions
//! produced the same pod. Comparing by index would report a whole-array
//! difference for a reordering that changed nothing, and -- far worse --
//! would silently misattribute a real change to the wrong container
//! (index 1 was `app` on one side and `istio-proxy` on the other). So
//! containers, init containers, and volumes are keyed by `name`;
//! `volumeMounts` by `mountPath` (the field that decides what the
//! container actually sees); environment entries by `name`.
//!
//! Normalization already sorts `/spec/containers` and `/spec/volumes` by
//! name for the same reason, but this module does not rely on that: a
//! recipe or user profile may drop those rules, and `initContainers` is
//! deliberately never sorted (order is behavior there), so name-keying
//! has to be this module's own property rather than an inherited one.
//!
//! An element with no usable key -- a container without a string `name`,
//! a mount without a string `mountPath` -- is skipped entirely rather
//! than matched positionally as a fallback. It is not a pod field this
//! module can honestly say anything about, so it stays visible through
//! [`crate::raw::raw_object_diff`] and nowhere else. The same goes for a
//! repeated key, which Kubernetes rejects: the first element wins and the
//! rest are left to the raw diff.
//!
//! # Subject naming
//!
//! [`SemanticChange::subject`] is the **bare Kubernetes name** of the
//! thing the change is about -- `nginx`, `istio-proxy`, `cache` -- and
//! never a decorated path such as `container/nginx`.
//!
//! That is not a stylistic preference; it is what the consumer expects.
//! `admissionlab_policy`'s `ChangeSelector::subject` matches this string
//! for **exact equality** against a value a user hand-writes in
//! `policy.overrides` or `expectations.yaml`, and both that type and the
//! configuration model document a subject as "a container name, a
//! webhook name". A user writing `subject: istio-proxy` is asking the
//! obvious question, and a decorated form would silently never match.
//!
//! Nothing is lost by dropping the category prefix, because two other
//! fields already carry it: `kind` says *what kind of thing* changed
//! (`container_added` versus `volume_added`), and `object_path` says
//! *where* (`/spec/initContainers/0` versus `/spec/containers/2`).
//! Kubernetes also forbids one pod from having a container and an init
//! container with the same name, so a container subject is unambiguous
//! within a fixture even before `kind` is consulted.
//!
//! A change that is not scoped to a named subject carries [`None`]:
//! `service_account_changed` and the *pod-level* `security_context_changed`
//! are properties of the pod itself. A container-level security context
//! change carries the container's name, so the two are distinguishable
//! by subject as well as by path.
//!
//! # Unmodeled fields never become guessed categories
//!
//! The seventeen [`SemanticChangeKind`] names are a frozen, public
//! vocabulary, and this module emits one only where the difference it
//! observed *is* that thing. Everything else about an object --
//! `nodeSelector`, `tolerations`, `hostNetwork`, a changed volume
//! *definition* (there is no `volume_changed` kind; only added and
//! removed), the deprecated `spec.serviceAccount` mirror of
//! `serviceAccountName`, every annotation and label, every custom
//! resource field -- produces no semantic change at all. It remains
//! fully visible in the raw RFC 6902 diff, which is exactly the division
//! [`crate::raw`] documents: raw output is evidence, a semantic change is
//! a claim, and inventing a category for a field nobody has modeled
//! would put a guess into a report and into `expectations.yaml` files
//! users grade themselves against.
//!
//! # Environment values are never rendered here
//!
//! Task 4.5 Step 3 requires environment comparison "without rendering
//! sensitive literal values in report-ready fields", and
//! [`SemanticChange::baseline`]/[`SemanticChange::candidate`] are
//! report-ready fields: a terminal renderer and an HTML renderer both
//! print them. So an environment entry reaches those fields as a
//! *descriptor* -- its `name`, and where its value comes from -- never as
//! its literal value:
//!
//! ```json
//! {"name": "DB_PASSWORD", "valueSource": "literal"}
//! {"name": "REGION", "valueSource": "valueFrom",
//!  "valueFrom": {"fieldRef": {"fieldPath": "metadata.name"}}}
//! ```
//!
//! A `valueFrom` block is carried verbatim because it holds *references*
//! (a `secretKeyRef` names a Secret and a key; it does not contain the
//! secret), and those references are precisely what a reader needs to see
//! when a webhook rewires where a value comes from. A literal value is
//! carried not at all. The consequence is deliberate and worth stating:
//! when only the literal changed, both payloads read
//! `{"name": …, "valueSource": "literal"}` and are *equal*. That is not a
//! bug -- the change exists because the entries differ, and its payloads
//! say "the literal value changed, and this tool will not print it".
//!
//! The same sanitization is applied wherever a whole container is
//! rendered (an added or removed container carries its `env` as
//! descriptors), so there is no side door.
//!
//! This is the one field-specific redaction this crate performs, and it
//! is not a substitute for Task 4.10's central redaction pass. Every
//! other channel still carries literals -- the raw diff, and both sides'
//! whole `final_object` under `AdmissionComparison` -- and `command`,
//! `args`, and annotations can hold credentials this module renders
//! verbatim. Global Constraint 14 is satisfied by
//! `admissionlab-report`'s `redact.rs`; what happens here only ensures
//! this crate does not *add* a new place a password can surface.
//!
//! # Where the pod spec is
//!
//! A fixture may be a `Pod`, or a workload that carries a pod template
//! (`Deployment`, `StatefulSet`, `DaemonSet`, `Job`, `ReplicaSet`), or a
//! `CronJob` whose template is one level deeper again. The pod spec is
//! located structurally, by trying the deepest template location first
//! (see [`POD_SPEC_LOCATIONS`]), rather than by switching on `kind`:
//! `kind` is a string a CRD can also carry, and every one of these
//! locations means the same thing when it is present.
//!
//! Two guards keep that honest. If the two sides resolve to *different*
//! locations, nothing is emitted -- that is a caller comparing a `Pod`
//! against a `Deployment`, and no field-level claim about it would be
//! meaningful. And an object with no pod spec at all (a `ConfigMap`, a
//! `ValidatingWebhookConfiguration`) yields no semantic changes, which is
//! the correct answer rather than a gap: nothing about it is modeled, so
//! all of it belongs to the raw diff.
//!
//! # Ordering
//!
//! The result is deterministic (Global Constraint 7) and in a fixed
//! reading order, not a sort applied afterwards:
//!
//! 1. `serviceAccountName`, then the pod-level `securityContext`;
//! 2. `initContainers`, by container name;
//! 3. `containers`, by container name;
//! 4. `volumes`, by volume name.
//!
//! Within one container: `image`, `securityContext`, `resources`,
//! `volumeMounts` (by mount path), `env` (by variable name). Every
//! iteration above is over a [`BTreeMap`]/[`BTreeSet`] of names, so the
//! order depends on the objects' content and never on their key order,
//! their array order, or a hash seed.
//!
//! # What `object_path` points into
//!
//! Every emitted pointer is a real RFC 6901 pointer that resolves in one
//! of the two compared documents, which means an array index has to name
//! a side. The rule: **a change about something the candidate has points
//! into the candidate object; a change about something only the baseline
//! has points into the baseline object.** So `container_removed` and a
//! removed mount address the baseline, and everything else addresses the
//! candidate. The candidate is the side a user is investigating, and the
//! `subject` field carries the name, which is what identity actually
//! rests on.

use std::collections::{BTreeMap, BTreeSet};

use admissionlab_normalize::NormalizedObject;
use serde_json::{Map, Value};

use crate::types::{SemanticChange, SemanticChangeKind, unattributed_fixture_id};

/// Where a pod spec can live inside a fixture object, deepest first.
///
/// Order matters: a `Deployment` has both `/spec/template/spec` and
/// `/spec`, and only the first is a pod spec. A `CronJob` nests one
/// level deeper again. The first location that resolves to a JSON object
/// wins.
const POD_SPEC_LOCATIONS: [&str; 3] = [
    "/spec/jobTemplate/spec/template/spec",
    "/spec/template/spec",
    "/spec",
];

/// Classifies the workload differences between two normalized objects.
///
/// Returns one [`SemanticChange`] per difference this module models, in
/// the fixed order this module's documentation lists. An empty result
/// means the two objects agree *on everything modeled here* -- it is not
/// a claim that the documents are identical, which is
/// [`crate::raw::raw_object_diff`]'s question and the only honest way to
/// ask it.
///
/// Every returned change carries
/// [`crate::types::unattributed_fixture_id`] as its `fixture_id`: a
/// [`NormalizedObject`] does not say which fixture it came from, and this
/// task's signature is frozen at two parameters. The caller that paired
/// the two sides stamps the identity with
/// [`SemanticChange::attributed_to`]; see that method for the full
/// argument. Changes also carry `origin: None` -- first-divergence
/// attribution needs the webhook traces this function never sees, and is
/// Task 4.7's job.
#[must_use]
pub fn diff_workload_objects(
    baseline: &NormalizedObject,
    candidate: &NormalizedObject,
) -> Vec<SemanticChange> {
    let (Some((baseline_prefix, baseline_spec)), Some((candidate_prefix, candidate_spec))) = (
        locate_pod_spec(&baseline.value),
        locate_pod_spec(&candidate.value),
    ) else {
        return Vec::new();
    };
    // Two different template locations mean two different kinds of
    // object. See this module's documentation.
    if baseline_prefix != candidate_prefix {
        return Vec::new();
    }
    let prefix = baseline_prefix;

    let mut changes = Vec::new();
    diff_scalar(
        SemanticChangeKind::ServiceAccountChanged,
        &format!("{prefix}/serviceAccountName"),
        None,
        baseline_spec.get("serviceAccountName"),
        candidate_spec.get("serviceAccountName"),
        &mut changes,
    );
    diff_scalar(
        SemanticChangeKind::SecurityContextChanged,
        &format!("{prefix}/securityContext"),
        None,
        baseline_spec.get("securityContext"),
        candidate_spec.get("securityContext"),
        &mut changes,
    );
    diff_container_list(
        &format!("{prefix}/initContainers"),
        SemanticChangeKind::InitContainerAdded,
        SemanticChangeKind::InitContainerRemoved,
        baseline_spec.get("initContainers"),
        candidate_spec.get("initContainers"),
        &mut changes,
    );
    diff_container_list(
        &format!("{prefix}/containers"),
        SemanticChangeKind::ContainerAdded,
        SemanticChangeKind::ContainerRemoved,
        baseline_spec.get("containers"),
        candidate_spec.get("containers"),
        &mut changes,
    );
    diff_volumes(
        &format!("{prefix}/volumes"),
        baseline_spec.get("volumes"),
        candidate_spec.get("volumes"),
        &mut changes,
    );
    changes
}

/// Finds the pod spec inside a fixture object, with the RFC 6901 prefix
/// that addresses it.
///
/// Returns [`None`] for an object that has no pod spec at any of
/// [`POD_SPEC_LOCATIONS`].
fn locate_pod_spec(value: &Value) -> Option<(&'static str, &Value)> {
    POD_SPEC_LOCATIONS.into_iter().find_map(|location| {
        let found = value.pointer(location)?;
        found.is_object().then_some((location, found))
    })
}

/// One element of a name-keyed array, with the index it was found at.
///
/// The index is kept because `object_path` has to be a pointer that
/// really resolves, and matching is by name regardless of it.
#[derive(Clone, Copy)]
struct Keyed<'value> {
    index: usize,
    value: &'value Value,
}

/// Indexes an array's object elements by their string value at `key`,
/// keeping the first element for a repeated key.
///
/// Elements that are not objects, and objects with no string value at
/// `key`, are dropped: they cannot be matched by name against the other
/// side, and guessing at a positional match is exactly what this module
/// refuses to do. A [`None`] or non-array input indexes to nothing,
/// which makes "the field is absent" and "the field is an empty array"
/// compare the same way here -- both mean the pod has no such elements,
/// and the difference between them stays in the raw diff.
fn index_by<'value>(
    array: Option<&'value Value>,
    key: &str,
) -> BTreeMap<&'value str, Keyed<'value>> {
    let mut indexed = BTreeMap::new();
    let Some(Value::Array(items)) = array else {
        return indexed;
    };
    for (index, value) in items.iter().enumerate() {
        if let Some(name) = value.get(key).and_then(Value::as_str) {
            indexed.entry(name).or_insert(Keyed { index, value });
        }
    }
    indexed
}

/// Every key in either side, in sorted order.
fn union_keys<'value>(
    baseline: &BTreeMap<&'value str, Keyed<'value>>,
    candidate: &BTreeMap<&'value str, Keyed<'value>>,
) -> BTreeSet<&'value str> {
    baseline.keys().chain(candidate.keys()).copied().collect()
}

/// Emits one change of `kind` if the two sides' values at one modeled
/// field differ.
///
/// A [`None`] on either side means the field is absent there, and is
/// carried into the payload as [`None`] -- "this side had no value here",
/// never a fabricated empty object.
fn diff_scalar(
    kind: SemanticChangeKind,
    object_path: &str,
    subject: Option<&str>,
    baseline: Option<&Value>,
    candidate: Option<&Value>,
    changes: &mut Vec<SemanticChange>,
) {
    if baseline == candidate {
        return;
    }
    changes.push(change(
        kind,
        object_path,
        subject,
        baseline.cloned(),
        candidate.cloned(),
    ));
}

/// Compares one container array (`containers` or `initContainers`),
/// keyed by container name.
fn diff_container_list(
    field_path: &str,
    added: SemanticChangeKind,
    removed: SemanticChangeKind,
    baseline: Option<&Value>,
    candidate: Option<&Value>,
    changes: &mut Vec<SemanticChange>,
) {
    let baseline_containers = index_by(baseline, "name");
    let candidate_containers = index_by(candidate, "name");
    for name in union_keys(&baseline_containers, &candidate_containers) {
        match (
            baseline_containers.get(name),
            candidate_containers.get(name),
        ) {
            (Some(only_baseline), None) => changes.push(change(
                removed,
                &format!("{field_path}/{}", only_baseline.index),
                Some(name),
                Some(sanitize_container(only_baseline.value)),
                None,
            )),
            (None, Some(only_candidate)) => changes.push(change(
                added,
                &format!("{field_path}/{}", only_candidate.index),
                Some(name),
                None,
                Some(sanitize_container(only_candidate.value)),
            )),
            (Some(in_baseline), Some(in_candidate)) => diff_container(
                &format!("{field_path}/{}", in_baseline.index),
                &format!("{field_path}/{}", in_candidate.index),
                name,
                in_baseline.value,
                in_candidate.value,
                changes,
            ),
            // Unreachable: `union_keys` only yields keys one of the two
            // maps holds. Written out rather than reached for with
            // `unreachable!`, so this function cannot panic.
            (None, None) => {}
        }
    }
}

/// Compares one container that both sides have, in the fixed field order
/// this module's documentation lists.
fn diff_container(
    baseline_path: &str,
    candidate_path: &str,
    name: &str,
    baseline: &Value,
    candidate: &Value,
    changes: &mut Vec<SemanticChange>,
) {
    diff_scalar(
        SemanticChangeKind::ImageChanged,
        &format!("{candidate_path}/image"),
        Some(name),
        baseline.get("image"),
        candidate.get("image"),
        changes,
    );
    diff_scalar(
        SemanticChangeKind::SecurityContextChanged,
        &format!("{candidate_path}/securityContext"),
        Some(name),
        baseline.get("securityContext"),
        candidate.get("securityContext"),
        changes,
    );
    diff_scalar(
        SemanticChangeKind::ResourceRequirementChanged,
        &format!("{candidate_path}/resources"),
        Some(name),
        baseline.get("resources"),
        candidate.get("resources"),
        changes,
    );
    diff_volume_mounts(
        baseline_path,
        candidate_path,
        name,
        baseline,
        candidate,
        changes,
    );
    diff_environment(
        baseline_path,
        candidate_path,
        name,
        baseline,
        candidate,
        changes,
    );
}

/// Compares one container's `volumeMounts`, keyed by `mountPath`.
///
/// The mount path is the key rather than the volume `name` because it is
/// what decides what the container actually sees: a mount that starts
/// pointing at a different volume, or that gains `readOnly`, is a change
/// at that path.
fn diff_volume_mounts(
    baseline_path: &str,
    candidate_path: &str,
    name: &str,
    baseline: &Value,
    candidate: &Value,
    changes: &mut Vec<SemanticChange>,
) {
    let baseline_mounts = index_by(baseline.get("volumeMounts"), "mountPath");
    let candidate_mounts = index_by(candidate.get("volumeMounts"), "mountPath");
    for mount_path in union_keys(&baseline_mounts, &candidate_mounts) {
        let in_baseline = baseline_mounts.get(mount_path);
        let in_candidate = candidate_mounts.get(mount_path);
        if in_baseline.map(|mount| mount.value) == in_candidate.map(|mount| mount.value) {
            continue;
        }
        let object_path = match (in_baseline, in_candidate) {
            (_, Some(mount)) => format!("{candidate_path}/volumeMounts/{}", mount.index),
            (Some(mount), None) => format!("{baseline_path}/volumeMounts/{}", mount.index),
            // Unreachable, for the reason `diff_container_list` gives.
            (None, None) => continue,
        };
        changes.push(change(
            SemanticChangeKind::VolumeMountChanged,
            &object_path,
            Some(name),
            in_baseline.map(|mount| mount.value.clone()),
            in_candidate.map(|mount| mount.value.clone()),
        ));
    }
}

/// Compares one container's `env`, keyed by variable name, rendering
/// descriptors rather than literal values.
///
/// The *comparison* is over the entries verbatim, so a changed literal is
/// detected; only the rendering is sanitized. See this module's
/// documentation for why, and for what it means when the two payloads
/// come out equal.
fn diff_environment(
    baseline_path: &str,
    candidate_path: &str,
    name: &str,
    baseline: &Value,
    candidate: &Value,
    changes: &mut Vec<SemanticChange>,
) {
    let baseline_env = index_by(baseline.get("env"), "name");
    let candidate_env = index_by(candidate.get("env"), "name");
    for variable in union_keys(&baseline_env, &candidate_env) {
        let in_baseline = baseline_env.get(variable);
        let in_candidate = candidate_env.get(variable);
        if in_baseline.map(|entry| entry.value) == in_candidate.map(|entry| entry.value) {
            continue;
        }
        let object_path = match (in_baseline, in_candidate) {
            (_, Some(entry)) => format!("{candidate_path}/env/{}", entry.index),
            (Some(entry), None) => format!("{baseline_path}/env/{}", entry.index),
            // Unreachable, for the reason `diff_container_list` gives.
            (None, None) => continue,
        };
        changes.push(change(
            SemanticChangeKind::EnvironmentChanged,
            &object_path,
            Some(name),
            in_baseline.map(|entry| environment_descriptor(entry.value)),
            in_candidate.map(|entry| environment_descriptor(entry.value)),
        ));
    }
}

/// Compares `spec.volumes`, keyed by volume name.
///
/// Only addition and removal are modeled, because those are the only two
/// volume kinds in the frozen vocabulary. A volume that exists on both
/// sides with a *different definition* -- a `configMap` volume now
/// pointing at another `ConfigMap` -- produces no semantic change here
/// and stays in the raw diff, rather than being reported under a
/// borrowed name that would mean something else to whoever reads it.
fn diff_volumes(
    field_path: &str,
    baseline: Option<&Value>,
    candidate: Option<&Value>,
    changes: &mut Vec<SemanticChange>,
) {
    let baseline_volumes = index_by(baseline, "name");
    let candidate_volumes = index_by(candidate, "name");
    for name in union_keys(&baseline_volumes, &candidate_volumes) {
        match (baseline_volumes.get(name), candidate_volumes.get(name)) {
            (Some(only_baseline), None) => changes.push(change(
                SemanticChangeKind::VolumeRemoved,
                &format!("{field_path}/{}", only_baseline.index),
                Some(name),
                Some(only_baseline.value.clone()),
                None,
            )),
            (None, Some(only_candidate)) => changes.push(change(
                SemanticChangeKind::VolumeAdded,
                &format!("{field_path}/{}", only_candidate.index),
                Some(name),
                None,
                Some(only_candidate.value.clone()),
            )),
            (Some(_), Some(_)) | (None, None) => {}
        }
    }
}

/// Renders a container for a report-ready payload, with its `env`
/// replaced by descriptors.
///
/// Every other field is carried verbatim: the point is that a reader can
/// see what was injected. See this module's documentation for why `env`
/// is the one exception and why it is not a substitute for Task 4.10's
/// central redaction.
fn sanitize_container(container: &Value) -> Value {
    let Some(fields) = container.as_object() else {
        return container.clone();
    };
    let mut sanitized = fields.clone();
    if let Some(Value::Array(entries)) = fields.get("env") {
        sanitized.insert(
            "env".to_owned(),
            Value::Array(entries.iter().map(environment_descriptor).collect()),
        );
    }
    Value::Object(sanitized)
}

/// Describes one environment entry without rendering a literal value.
///
/// `valueSource` is `"literal"` when the entry carries a `value`,
/// `"valueFrom"` when it carries a `valueFrom` (which is then included
/// verbatim, since it holds references rather than values), and
/// `"none"` when it carries neither. An entry that somehow carries both
/// -- which the API server rejects -- is described by its `valueFrom`,
/// the more informative of the two, and its literal is still not
/// rendered.
///
/// `name` is included only when the entry has one, rather than
/// substituted with a placeholder.
fn environment_descriptor(entry: &Value) -> Value {
    let mut descriptor = Map::new();
    if let Some(name) = entry.get("name") {
        descriptor.insert("name".to_owned(), name.clone());
    }
    match (entry.get("valueFrom"), entry.get("value")) {
        (Some(value_from), _) => {
            descriptor.insert(
                "valueSource".to_owned(),
                Value::String("valueFrom".to_owned()),
            );
            descriptor.insert("valueFrom".to_owned(), value_from.clone());
        }
        (None, Some(_)) => {
            descriptor.insert(
                "valueSource".to_owned(),
                Value::String("literal".to_owned()),
            );
        }
        (None, None) => {
            descriptor.insert("valueSource".to_owned(), Value::String("none".to_owned()));
        }
    }
    Value::Object(descriptor)
}

/// Builds one change, filling in the two fields every change from this
/// module shares.
fn change(
    kind: SemanticChangeKind,
    object_path: &str,
    subject: Option<&str>,
    baseline: Option<Value>,
    candidate: Option<Value>,
) -> SemanticChange {
    SemanticChange {
        kind,
        fixture_id: unattributed_fixture_id(),
        object_path: Some(object_path.to_owned()),
        subject: subject.map(ToOwned::to_owned),
        baseline,
        candidate,
        // Attribution needs the webhook traces this function never sees;
        // `None` means "not attributed", never "no divergence".
        origin: None,
    }
}
