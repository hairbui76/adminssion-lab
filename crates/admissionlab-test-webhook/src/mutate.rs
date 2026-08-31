//! Mutating patch construction: turning a parsed [`Behavior`] plus the
//! object it came from into the RFC 6902 JSON Patch this webhook returns.
//!
//! # Two scopes, because two mutating webhook configurations
//!
//! This recipe installs *two* `MutatingWebhookConfiguration`s
//! (`recipes/test-webhook/manifests/21-mutating-webhook-configurations.yaml`),
//! which is the only way to exercise Kubernetes' own reinvocation
//! machinery — Task 3.9 Step 4. Both point at this same server, so the
//! server distinguishes them the way every real multi-webhook controller
//! does: by the path each configuration's `clientConfig.service.path`
//! names. [`MutationScope`] is that distinction, and it is why
//! [`build_patch`] takes one:
//!
//! - [`MutationScope::Labels`] (`/mutate-labels`) performs only
//!   [`Behavior::add_label`].
//! - [`MutationScope::Workload`] (`/mutate-containers`) performs only
//!   the `spec`-level actions: containers, init containers, volumes.
//!
//! The split is what makes the chain possible. The workload
//! configuration carries an `objectSelector` matching a label that the
//! *labels* configuration is able to add, so a fixture that requests
//! that label via [`crate::behavior::ADD_LABEL`] cannot be mutated by
//! the workload webhook until the labels webhook has run — which is
//! exactly the "one webhook adds a field that makes the second mutate"
//! shape reinvocation exists for. That manifest file documents the
//! chain end to end; this module only needs to know that the two scopes
//! are disjoint.
//!
//! Neither scope ever performs deny/delay/fail. Those are
//! [`crate::validate`]'s, and belong to the single validating
//! configuration alone, so a fixture's `delay-ms` costs exactly one
//! delay per request no matter how many mutating configurations are
//! installed.
//!
//! # Idempotency is a correctness requirement here, not politeness
//!
//! Task 3.9 Step 3: an add whose target is already present returns *no*
//! patch. That is not an optimization — it is what makes this webhook
//! safe to invoke more than once on the same object, which Kubernetes
//! will do the moment `reinvocationPolicy: IfNeeded` is set and some
//! other webhook changes the object. Without it, a reinvoked
//! `add-container` would append a second copy of the same sidecar and
//! the API server would reject the pod for duplicate container names —
//! a failure whose cause is invocation *count*, i.e. exactly the kind of
//! order-dependence Step 4 forbids product correctness from resting on.
//!
//! Every action here is therefore idempotent in the strict sense: the
//! patch is a pure function of the object, and applying the patch and
//! re-running produces an empty patch. Removals get this for free (a
//! name that is not there is not removed), adds check by container /
//! volume `name` and by label key.
//!
//! # Operation order inside one patch
//!
//! RFC 6902 operations apply in sequence, so array indices in a `remove`
//! are indices into the state *at that point in the patch*. This module
//! emits, per array, the `remove` first and the append second, and
//! computes the `remove` index against the incoming object — which is
//! sound because the append that follows uses `/-` (append to the end)
//! and so cannot shift any index the `remove` already used. The
//! combination `remove-container: x` plus `add-container: x=image` is
//! therefore a well-defined replacement, not a conflict, and the
//! idempotency check for the add is evaluated *after* the remove, as a
//! reader of the fixture would expect.

use serde_json::{Value, json};

use crate::behavior::{Behavior, NamedImage};

/// JSON Pointer to a pod's containers.
const CONTAINERS: &str = "/spec/containers";
/// JSON Pointer to a pod's init containers.
const INIT_CONTAINERS: &str = "/spec/initContainers";
/// JSON Pointer to a pod's volumes.
const VOLUMES: &str = "/spec/volumes";
/// JSON Pointer to a pod's labels.
const LABELS: &str = "/metadata/labels";

/// Which of this recipe's two mutating webhook configurations is asking
/// — see this module's own documentation for why there are two and why
/// their action sets are disjoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationScope {
    /// `/mutate-labels`: `metadata.labels` only.
    Labels,
    /// `/mutate-containers`: `spec.containers`, `spec.initContainers`,
    /// `spec.volumes`.
    Workload,
}

/// Builds the RFC 6902 operations `scope` asks for, given `behavior`
/// (already parsed from `object`'s own annotations) and the current
/// `object`.
///
/// An empty result means "no mutation": [`crate::serve`] then answers
/// `allowed: true` with no `patch`/`patchType` fields at all, rather
/// than an empty patch — the two are equivalent to the API server, but
/// only the former is distinguishable in an audit log from "this
/// webhook patched the object with nothing", which is the signal Task
/// 3.10's capture pipeline reads.
#[must_use]
pub fn build_patch(scope: MutationScope, behavior: &Behavior, object: &Value) -> Vec<Value> {
    let mut ops = Vec::new();
    match scope {
        MutationScope::Labels => label_ops(behavior, object, &mut ops),
        MutationScope::Workload => {
            array_ops(
                CONTAINERS,
                behavior.remove_container.as_deref(),
                behavior.add_container.as_ref(),
                object,
                &mut ops,
            );
            array_ops(
                INIT_CONTAINERS,
                behavior.remove_init_container.as_deref(),
                behavior.add_init_container.as_ref(),
                object,
                &mut ops,
            );
            volume_ops(behavior, object, &mut ops);
        }
    }
    ops
}

/// The `remove`-then-append pair for one named-entry array (containers
/// or init containers) — see this module's own documentation on
/// operation order.
fn array_ops(
    pointer: &str,
    remove: Option<&str>,
    add: Option<&NamedImage>,
    object: &Value,
    ops: &mut Vec<Value>,
) {
    // `None` (the array is absent) and `Some(vec![])` (present and
    // empty) are genuinely different: appending to an absent array with
    // `add <pointer>/-` is not a valid RFC 6902 operation, so the absent
    // case must create the array instead. `spec.initContainers` and
    // `spec.volumes` are absent on most real pods, so this is the common
    // path, not an edge case.
    let mut names = entry_names(object, pointer);

    if let (Some(name), Some(existing)) = (remove, names.as_mut())
        && let Some(index) = existing
            .iter()
            .position(|entry| entry.as_deref() == Some(name))
    {
        ops.push(json!({"op": "remove", "path": format!("{pointer}/{index}")}));
        existing.remove(index);
    }

    if let Some(NamedImage { name, image }) = add {
        append_by_name(
            pointer,
            names.as_deref(),
            name,
            &json!({"name": name, "image": image}),
            ops,
        );
    }
}

/// [`crate::behavior::ADD_VOLUME`]: an `emptyDir` volume, because a
/// volume with no source is not a valid pod and an `emptyDir` needs no
/// cluster state of its own (no `PersistentVolumeClaim` to provision, no
/// `Secret`/`ConfigMap` to exist first) — the only volume kind whose
/// admission outcome is a function of this webhook alone.
fn volume_ops(behavior: &Behavior, object: &Value, ops: &mut Vec<Value>) {
    let Some(name) = behavior.add_volume.as_deref() else {
        return;
    };
    let names = entry_names(object, VOLUMES);
    append_by_name(
        VOLUMES,
        names.as_deref(),
        name,
        &json!({"name": name, "emptyDir": {}}),
        ops,
    );
}

/// Appends `entry` to the array at `pointer` unless `name` is already
/// among `names` — the idempotency rule this module's own documentation
/// describes. `names` is `None` when the array is absent entirely, in
/// which case the array is created holding just `entry`.
fn append_by_name(
    pointer: &str,
    names: Option<&[Option<String>]>,
    name: &str,
    entry: &Value,
    ops: &mut Vec<Value>,
) {
    match names {
        Some(existing) => {
            if existing.iter().any(|found| found.as_deref() == Some(name)) {
                return;
            }
            ops.push(json!({"op": "add", "path": format!("{pointer}/-"), "value": entry}));
        }
        None => ops.push(json!({"op": "add", "path": pointer, "value": [entry]})),
    }
}

/// [`crate::behavior::ADD_LABEL`].
///
/// RFC 6902's `add` on an object member replaces an existing member, so
/// one operation covers both "the key is new" and "the key is there with
/// a different value"; only the already-equal case is skipped, which is
/// what makes a reinvocation of this webhook a no-op. When
/// `metadata.labels` is absent entirely the map itself is created, for
/// the same reason [`array_ops`] creates an absent array.
fn label_ops(behavior: &Behavior, object: &Value, ops: &mut Vec<Value>) {
    let Some((key, value)) = behavior.add_label.as_ref() else {
        return;
    };
    match object.pointer(LABELS) {
        Some(Value::Object(labels)) => {
            if labels.get(key).and_then(Value::as_str) == Some(value.as_str()) {
                return;
            }
            ops.push(json!({
                "op": "add",
                "path": format!("{LABELS}/{}", escape_pointer_token(key)),
                "value": value,
            }));
        }
        _ => ops.push(json!({"op": "add", "path": LABELS, "value": {key.as_str(): value}})),
    }
}

/// The `name` of every entry of the array at `pointer`, positionally —
/// `None` for the whole array when it is absent (or is not an array),
/// and `None` for an individual entry that has no string `name`.
///
/// Positional, rather than a filtered list of the names that do exist,
/// because the index of a name in this vector is used verbatim as an
/// RFC 6902 `remove` path: silently dropping a nameless entry would
/// shift every later index and make the emitted patch remove the wrong
/// container.
fn entry_names(object: &Value, pointer: &str) -> Option<Vec<Option<String>>> {
    let entries = object.pointer(pointer)?.as_array()?;
    Some(
        entries
            .iter()
            .map(|entry| entry.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect(),
    )
}

/// RFC 6901 §3 escaping for one JSON Pointer reference token: `~` before
/// `/`, always, since escaping `/` introduces a `~` of its own.
///
/// Load-bearing, not defensive: Kubernetes label keys are routinely
/// prefixed (`app.kubernetes.io/name`, and this recipe's own
/// `test.admissionlab.io/...` gate label), and an unescaped `/` in a
/// pointer token would address a nested member that does not exist
/// instead of the label itself — a patch the API server would reject.
fn escape_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{MutationScope, build_patch, escape_pointer_token};
    use crate::behavior::parse;

    /// Parses `annotations` through the real [`crate::behavior::parse`]
    /// and builds the patch for `scope` — never a hand-built
    /// [`crate::behavior::Behavior`], so these tests exercise the same
    /// path a live request takes.
    fn patch(scope: MutationScope, object: &Value) -> Vec<Value> {
        let behavior = parse(object).expect("test objects carry valid annotations");
        build_patch(scope, &behavior, object)
    }

    fn pod(annotations: &Value, spec: &Value) -> Value {
        json!({"metadata": {"name": "fixture", "annotations": annotations}, "spec": spec})
    }

    #[test]
    fn label_keys_are_escaped_per_rfc_6901() {
        assert_eq!(
            escape_pointer_token("test.admissionlab.io/containers"),
            "test.admissionlab.io~1containers"
        );
        // `~` first, then `/`: escaping in the other order would turn
        // the `~1` this produces into `~01`.
        assert_eq!(escape_pointer_token("a~b/c"), "a~0b~1c");
    }

    #[test]
    fn an_absent_labels_map_is_created_rather_than_appended_to() {
        let object = pod(
            &json!({"test.admissionlab.io/add-label": "team=platform"}),
            &json!({"containers": []}),
        );
        assert_eq!(
            patch(MutationScope::Labels, &object),
            vec![json!({"op": "add", "path": "/metadata/labels", "value": {"team": "platform"}})]
        );
    }

    #[test]
    fn an_existing_label_with_a_different_value_is_overwritten() {
        let mut object = pod(
            &json!({"test.admissionlab.io/add-label": "team=platform"}),
            &json!({"containers": []}),
        );
        object["metadata"]["labels"] = json!({"team": "storage"});
        assert_eq!(
            patch(MutationScope::Labels, &object),
            vec![json!({"op": "add", "path": "/metadata/labels/team", "value": "platform"})]
        );
    }

    #[test]
    fn a_nameless_container_entry_does_not_shift_the_remove_index() {
        let object = pod(
            &json!({"test.admissionlab.io/remove-container": "app"}),
            &json!({"containers": [{"image": "no-name-field"}, {"name": "app"}]}),
        );
        assert_eq!(
            patch(MutationScope::Workload, &object),
            vec![json!({"op": "remove", "path": "/spec/containers/1"})],
            "index 1 is the position in the real array, not in a filtered list of named entries"
        );
    }

    #[test]
    fn removing_and_adding_the_same_name_is_a_replacement() {
        let object = pod(
            &json!({
                "test.admissionlab.io/remove-container": "app",
                "test.admissionlab.io/add-container": "app=registry.k8s.io/pause:3.10",
            }),
            &json!({"containers": [{"name": "app", "image": "old"}]}),
        );
        assert_eq!(
            patch(MutationScope::Workload, &object),
            vec![
                json!({"op": "remove", "path": "/spec/containers/0"}),
                json!({
                    "op": "add",
                    "path": "/spec/containers/-",
                    "value": {"name": "app", "image": "registry.k8s.io/pause:3.10"},
                }),
            ],
            "the remove must precede the add, and the add's idempotency check must see the \
             post-remove state"
        );
    }

    #[test]
    fn the_two_scopes_are_disjoint() {
        let object = pod(
            &json!({
                "test.admissionlab.io/add-label": "team=platform",
                "test.admissionlab.io/add-container": "sidecar=registry.k8s.io/pause:3.10",
                "test.admissionlab.io/add-volume": "scratch",
            }),
            &json!({"containers": []}),
        );

        let labels = patch(MutationScope::Labels, &object);
        assert_eq!(labels.len(), 1, "the labels scope touches only the label");
        assert_eq!(labels[0]["path"], json!("/metadata/labels"));

        let workload = patch(MutationScope::Workload, &object);
        assert!(
            workload
                .iter()
                .all(|op| op["path"].as_str().is_some_and(|p| p.starts_with("/spec/"))),
            "the workload scope touches only spec: {workload:?}"
        );
    }

    /// The property Step 3 exists for, stated as a property rather than
    /// a special case: applying the patch and asking again produces
    /// nothing. Uses `json_patch`-free manual application of the exact
    /// operations this module emits, so a bug in the emitted `path`
    /// would surface here as a failed re-check rather than being papered
    /// over by a library's leniency.
    #[test]
    fn a_second_invocation_on_the_already_mutated_object_patches_nothing() {
        let object = pod(
            &json!({
                "test.admissionlab.io/add-label": "team=platform",
                "test.admissionlab.io/add-container": "sidecar=registry.k8s.io/pause:3.10",
                "test.admissionlab.io/add-init-container": "setup=registry.k8s.io/busybox:1.36",
                "test.admissionlab.io/add-volume": "scratch",
            }),
            &json!({"containers": [{"name": "app", "image": "app:1"}]}),
        );

        let mut mutated = object.clone();
        mutated["metadata"]["labels"] = json!({"team": "platform"});
        mutated["spec"]["containers"]
            .as_array_mut()
            .expect("containers is an array")
            .push(json!({"name": "sidecar", "image": "registry.k8s.io/pause:3.10"}));
        mutated["spec"]["initContainers"] =
            json!([{"name": "setup", "image": "registry.k8s.io/busybox:1.36"}]);
        mutated["spec"]["volumes"] = json!([{"name": "scratch", "emptyDir": {}}]);

        assert!(
            patch(MutationScope::Labels, &mutated).is_empty(),
            "the label is already present"
        );
        assert!(
            patch(MutationScope::Workload, &mutated).is_empty(),
            "the sidecar, init container and volume are all already present"
        );
    }

    #[test]
    fn removing_an_absent_container_patches_nothing() {
        let object = pod(
            &json!({"test.admissionlab.io/remove-container": "never-there"}),
            &json!({"containers": [{"name": "app"}]}),
        );
        assert!(patch(MutationScope::Workload, &object).is_empty());
    }

    #[test]
    fn removing_from_an_absent_init_container_array_patches_nothing() {
        let object = pod(
            &json!({"test.admissionlab.io/remove-init-container": "setup"}),
            &json!({"containers": [{"name": "app"}]}),
        );
        assert!(patch(MutationScope::Workload, &object).is_empty());
    }
}
