//! Task 4.1: deterministic Kubernetes object normalization.
//!
//! The golden files live at the workspace root under
//! `testdata/objects/normalization/`, mirroring where every other
//! cross-crate corpus in this project lives (`testdata/audit/`,
//! `testdata/manifests/`), and are reached the same way
//! `admissionlab-admission/tests/audit_reader.rs` reaches its own.
//!
//! # Provenance of the golden inputs
//!
//! `pod-add-label-final.input.json` is a hand-written *representative*
//! capture, not a recorded one, and this file says so rather than
//! implying otherwise. Every field in it is either copied from
//! `fixtures/core/admission/pod-add-label.yaml` (the real fixture) or is
//! a field kube-apiserver is documented to populate on a `Pod` `CREATE`
//! -- `uid`, `resourceVersion`, `creationTimestamp`, `managedFields`,
//! the `serviceAccount`/`serviceAccountName` defaults and the
//! `kube-api-access-*` projected volume the in-tree `ServiceAccount`
//! admission plugin adds, and the `admissionlab.dev/mutated` label
//! `recipes/test-webhook`'s mutating webhook adds (that fixture's own
//! comments quote the exact patch). Recording a real one needs a live
//! `kind` cluster, which is what `admissionlab-admission`'s
//! `tests/kind_capture.rs` exit gate is for; this file's job is to pin
//! the *transformation*, which is pure and needs no cluster.

use std::path::{Path, PathBuf};

use admissionlab_normalize::{
    NormalizationProfile, NormalizeError, NormalizeRule, NormalizedObject, PointerError, RuleTier,
    normalize_object,
};
use serde_json::{Value, json};

// ---------------------------------------------------------------------
// Golden-file plumbing
// ---------------------------------------------------------------------

fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/objects/normalization")
}

fn read_input(case: &str) -> Value {
    let path = testdata_dir().join(format!("{case}.input.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// Renders `normalized` exactly as the checked-in `.expected.json` files
/// are written: pretty JSON plus a trailing newline.
fn render(normalized: &NormalizedObject) -> String {
    let mut text = serde_json::to_string_pretty(normalized).expect("serialize NormalizedObject");
    text.push('\n');
    text
}

fn assert_golden(case: &str, normalized: &NormalizedObject) {
    let path = testdata_dir().join(format!("{case}.expected.json"));
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    assert_eq!(
        render(normalized),
        expected,
        "normalized output drifted from {}",
        path.display()
    );
}

fn normalize(value: &Value, profile: &NormalizationProfile) -> NormalizedObject {
    normalize_object(value, profile).expect("normalization succeeds")
}

// ---------------------------------------------------------------------
// Step 1: built-in removals, golden-tested
// ---------------------------------------------------------------------

/// The built-in profile strips exactly the server-generated metadata it
/// claims to and leaves everything else -- including the mutation the
/// webhook under test performed -- untouched.
///
/// The assertions after the golden comparison are the ones worth stating
/// out loud, because a golden file alone would let a future edit relax
/// them without anyone noticing what was given up.
#[test]
fn built_in_profile_strips_server_generated_pod_metadata() {
    let input = read_input("pod-add-label-final");
    let normalized = normalize(&input, &NormalizationProfile::built_in());
    assert_golden("pod-add-label-final", &normalized);

    let metadata = &normalized.value["metadata"];
    for stripped in [
        "uid",
        "resourceVersion",
        "creationTimestamp",
        "managedFields",
    ] {
        assert!(
            metadata.get(stripped).is_none(),
            "built-in profile must remove metadata.{stripped}"
        );
    }
    assert_eq!(
        metadata["generation"], 1,
        "metadata.generation must survive: Task 4.1 forbids removing it globally, and it is a \
         real signal about whether the spec was rewritten"
    );
}

/// The `admissionlab.dev/mutated` label is the *behavior* under test, not
/// lab noise, and normalization must never touch it. Same for the
/// `test.admissionlab.io/*` annotation that made the webhook act: it is
/// fixture input, identical on both sides, and a stack that rewrote it
/// would be a genuine regression this profile must not hide.
#[test]
fn built_in_profile_preserves_webhook_behavior_and_fixture_input() {
    let input = read_input("pod-add-label-final");
    let normalized = normalize(&input, &NormalizationProfile::built_in());

    assert_eq!(
        normalized.value["metadata"]["labels"]["admissionlab.dev/mutated"],
        json!("true")
    );
    assert_eq!(
        normalized.value["metadata"]["annotations"]["test.admissionlab.io/add-label"],
        json!("admissionlab.dev/mutated=true")
    );
    assert_eq!(
        normalized.value["spec"]["serviceAccountName"],
        json!("default"),
        "API-server defaulting is observable behavior, not noise"
    );
    assert_eq!(
        normalized.value["status"]["qosClass"],
        json!("BestEffort"),
        "status is computed behavior; no built-in rule removes it"
    );
}

/// The one built-in rule that exercises RFC 6901 key escaping: the
/// `kubectl.kubernetes.io/last-applied-configuration` key contains a `/`,
/// so reaching it needs a `~1` escape the rule builds for itself.
#[test]
fn built_in_profile_removes_the_last_applied_configuration_annotation() {
    let input = read_input("pod-add-label-final");
    let normalized = normalize(&input, &NormalizationProfile::built_in());

    let annotations = &normalized.value["metadata"]["annotations"];
    assert!(
        annotations
            .get("kubectl.kubernetes.io/last-applied-configuration")
            .is_none()
    );
    assert!(
        normalized.evidence.applied_rules.iter().any(|rule| {
            rule == "built_in: remove-annotation kubectl.kubernetes.io/last-applied-configuration"
        }),
        "applied_rules: {:?}",
        normalized.evidence.applied_rules
    );
}

// ---------------------------------------------------------------------
// Step 2: named-list sorting
// ---------------------------------------------------------------------

/// Containers and volumes are sorted by `name`; `initContainers`,
/// `env`, `command`, and `args` are not.
///
/// `initContainers` is the load-bearing half: init containers run
/// sequentially in array order, so reordering them would erase a real
/// behavioral difference. See `rules::built_in_rules` for the full
/// argument.
#[test]
fn built_in_profile_sorts_containers_and_volumes_only() {
    let input = read_input("unsorted-workload");
    let normalized = normalize(&input, &NormalizationProfile::built_in());
    assert_golden("unsorted-workload", &normalized);

    let spec = &normalized.value["spec"];
    assert_eq!(
        names(&spec["containers"]),
        vec![Some("app"), Some("proxy"), None],
        "keyed containers sort by name; the one without a name keeps its place at the end"
    );
    assert_eq!(
        names(&spec["volumes"]),
        vec![Some("config"), Some("scratch")]
    );
    assert_eq!(
        names(&spec["initContainers"]),
        vec![Some("migrate-schema"), Some("await-database")],
        "init containers run in order: the built-in profile must leave that order alone"
    );

    let proxy = &spec["containers"][1];
    assert_eq!(
        names(&proxy["env"]),
        vec![Some("PROXY_MODE"), Some("ADDRESS")],
        "env is not sorted by the built-in profile -- RFC 6901 has no wildcard for the \
         container index, so there is no honest default form of the rule"
    );
    assert_eq!(
        proxy["args"],
        json!(["--listen", "0.0.0.0:15000", "--log-level", "warn"]),
        "args order is meaning"
    );
}

/// An element with no usable sort key is never dropped and never
/// interleaved: keyed elements come first in key order, unkeyed ones
/// follow in their original relative order.
///
/// "No usable key" covers all four shapes at once here -- a missing key,
/// a non-string key, and an element that is not an object at all.
#[test]
fn sorting_keeps_unkeyed_elements_after_keyed_ones_in_original_order() {
    let input = json!({
        "spec": {"containers": [
            {"name": "zulu"},
            {"note": "no name at all"},
            {"name": 7},
            "not an object",
            {"name": "alpha"}
        ]}
    });
    let normalized = normalize(&input, &NormalizationProfile::built_in());

    assert_eq!(
        normalized.value["spec"]["containers"],
        json!([
            {"name": "alpha"},
            {"name": "zulu"},
            {"note": "no name at all"},
            {"name": 7},
            "not an object"
        ])
    );
    assert_eq!(
        normalized.value["spec"]["containers"]
            .as_array()
            .map(Vec::len),
        Some(5),
        "sorting never drops an element"
    );
}

/// Two elements with the same key keep their original relative order:
/// the sort is stable, so normalization of an already-normalized object
/// is a no-op.
#[test]
fn sorting_is_stable_for_duplicate_keys() {
    let input = json!({
        "spec": {"containers": [
            {"name": "app", "marker": "second"},
            {"name": "app", "marker": "first"}
        ]}
    });
    let normalized = normalize(&input, &NormalizationProfile::built_in());
    assert_eq!(
        normalized.value["spec"]["containers"],
        json!([
            {"name": "app", "marker": "second"},
            {"name": "app", "marker": "first"}
        ])
    );
    assert!(normalized.evidence.applied_rules.is_empty());
}

/// A recipe or user rule may point `SortNamedArray` at an array the
/// built-in profile deliberately refuses to sort. The engine is
/// mechanical and obeys; the evidence names the tier, so a report can
/// attribute the suppression back to whoever asked for it.
#[test]
fn a_user_rule_may_sort_an_array_the_built_in_profile_will_not() {
    let input = read_input("unsorted-workload");
    let mut profile = NormalizationProfile::built_in();
    profile.user.push(NormalizeRule::SortNamedArray {
        pointer: "/spec/initContainers".to_owned(),
        key: "name".to_owned(),
    });
    let normalized = normalize(&input, &profile);

    assert_eq!(
        names(&normalized.value["spec"]["initContainers"]),
        vec![Some("await-database"), Some("migrate-schema")]
    );
    assert!(
        normalized
            .evidence
            .applied_rules
            .contains(&"user: sort-named-array /spec/initContainers by name".to_owned()),
        "applied_rules: {:?}",
        normalized.evidence.applied_rules
    );
}

/// Pointing a sort rule at something that is not an array changes
/// nothing and warns. Unlike a pointer that matches nothing, this rule
/// asserts a shape the object does not have, which is worth surfacing.
#[test]
fn sorting_a_non_array_warns_and_changes_nothing() {
    let input = json!({"spec": {"containers": {"app": {"image": "pause"}}}});
    let normalized = normalize(&input, &NormalizationProfile::built_in());

    assert_eq!(normalized.value, input);
    assert!(normalized.evidence.applied_rules.is_empty());
    assert_eq!(
        normalized.evidence.warnings,
        vec![
            "built_in rule sort-named-array /spec/containers by name was skipped: the value at \
             that pointer is not an array"
        ]
    );
}

// ---------------------------------------------------------------------
// Step 3: what `applied_rules` records
// ---------------------------------------------------------------------

/// `applied_rules` records rules that matched *and changed something*,
/// never rules that were merely configured.
///
/// Both halves are pinned here: the profile below has seven built-in
/// rules, only three of which have anything to do on this object, and
/// the two sort rules find their arrays already in normal form.
#[test]
fn applied_rules_lists_only_rules_that_changed_the_object() {
    let input = json!({
        "metadata": {"name": "already-tidy", "uid": "9e8d-7c6b", "labels": {"a": "b"}},
        "spec": {"containers": [{"name": "app"}, {"name": "sidecar"}]}
    });
    let normalized = normalize(&input, &NormalizationProfile::built_in());

    assert_eq!(
        normalized.evidence.applied_rules,
        vec!["built_in: remove-pointer /metadata/uid"],
        "the other six built-in rules matched nothing or found nothing to change"
    );
    assert!(normalized.evidence.warnings.is_empty());
}

/// A rule that matches nothing is a silent no-op, not a warning and not
/// an error -- including a user rule, whose possible typo is a whole-run
/// question a per-object function cannot answer. See `object.rs`'s own
/// module documentation for the reasoning and the seam it leaves open.
#[test]
fn a_user_rule_that_matches_nothing_is_silent() {
    let input = json!({"metadata": {"name": "pod"}});
    let mut profile = NormalizationProfile::empty();
    profile
        .user
        .push(NormalizeRule::RemovePointer("/spec/nodeNam".to_owned()));
    profile
        .user
        .push(NormalizeRule::RemoveAnnotation("no.such/key".to_owned()));
    let normalized = normalize(&input, &profile);

    assert_eq!(normalized.value, input);
    assert!(normalized.evidence.applied_rules.is_empty());
    assert!(normalized.evidence.warnings.is_empty());
}

/// Evidence entries carry the tier that produced them, in application
/// order: `built_in`, then `recipe`, then `user`.
#[test]
fn applied_rules_are_recorded_in_tier_order_with_their_tier() {
    let input = json!({
        "metadata": {"name": "pod", "uid": "u"},
        "spec": {"nodeName": "node-1", "schedulerName": "default-scheduler"}
    });
    let profile = NormalizationProfile {
        built_in: vec![NormalizeRule::RemovePointer("/metadata/uid".to_owned())],
        recipe: vec![NormalizeRule::RemovePointer(
            "/spec/schedulerName".to_owned(),
        )],
        user: vec![NormalizeRule::RemovePointer("/spec/nodeName".to_owned())],
    };
    let normalized = normalize(&input, &profile);

    assert_eq!(
        normalized.evidence.applied_rules,
        vec![
            "built_in: remove-pointer /metadata/uid",
            "recipe: remove-pointer /spec/schedulerName",
            "user: remove-pointer /spec/nodeName",
        ]
    );
}

/// The tier order is observable, not just cosmetic: a user rule sees
/// what a recipe rule left behind. Here the recipe sorts the containers
/// and the user rule removes index 0 of the *sorted* array.
#[test]
fn user_rules_apply_after_recipe_rules() {
    let input = json!({"spec": {"containers": [{"name": "proxy"}, {"name": "app"}]}});
    let profile = NormalizationProfile {
        built_in: Vec::new(),
        recipe: vec![NormalizeRule::SortNamedArray {
            pointer: "/spec/containers".to_owned(),
            key: "name".to_owned(),
        }],
        user: vec![NormalizeRule::RemovePointer(
            "/spec/containers/0".to_owned(),
        )],
    };
    let normalized = normalize(&input, &profile);

    assert_eq!(
        normalized.value["spec"]["containers"],
        json!([{"name": "proxy"}]),
        "the user rule removed `app`, which only sits at index 0 after the recipe sort"
    );
}

// ---------------------------------------------------------------------
// Step 4: broad-parent warnings
// ---------------------------------------------------------------------

/// A user rule that removes a whole top-level section, every annotation,
/// or every label is warned about -- the suppression it causes cannot be
/// seen by reading the resulting diff, because the diff no longer
/// contains it.
#[test]
fn removing_a_broad_parent_warns_and_names_the_tier() {
    let input = json!({
        "metadata": {"annotations": {"a": "1"}, "labels": {"b": "2"}},
        "spec": {"nodeName": "node-1"}
    });
    for (tier, rule) in [
        ("user", "/spec"),
        ("user", "/metadata/annotations"),
        ("user", "/metadata/labels"),
    ] {
        let mut profile = NormalizationProfile::empty();
        profile
            .user
            .push(NormalizeRule::RemovePointer(rule.to_owned()));
        let normalized = normalize(&input, &profile);
        assert_eq!(
            normalized.evidence.warnings,
            vec![format!(
                "{tier} rule removed {rule}, a broad parent: no difference beneath it can be \
                 observed in this comparison"
            )]
        );
    }
}

/// A recipe rule gets the same warning as a user rule. A recipe is
/// vendor-supplied data (Global Constraint 6) and blinds a comparison
/// exactly as effectively; the tier in the text still says whose it was.
#[test]
fn a_recipe_rule_removing_a_broad_parent_warns_too() {
    let input = json!({"status": {"phase": "Pending"}});
    let mut profile = NormalizationProfile::empty();
    profile
        .recipe
        .push(NormalizeRule::RemovePointer("/status".to_owned()));
    let normalized = normalize(&input, &profile);

    assert_eq!(
        normalized.evidence.warnings,
        vec![
            "recipe rule removed /status, a broad parent: no difference beneath it can be \
             observed in this comparison"
        ]
    );
}

/// No warning for a narrow user rule, and none for a broad rule that
/// found nothing to remove: nothing was suppressed, so there is nothing
/// for a reader to go looking for. The built-in profile never warns
/// either -- it contains no broad rule by construction, and a warning on
/// every object would train users to ignore the ones that matter.
#[test]
fn narrow_unmatched_and_built_in_removals_do_not_warn() {
    let input = json!({"metadata": {"uid": "u"}, "spec": {"nodeName": "node-1"}});
    let mut profile = NormalizationProfile::built_in();
    profile
        .user
        .push(NormalizeRule::RemovePointer("/spec/nodeName".to_owned()));
    profile
        .user
        .push(NormalizeRule::RemovePointer("/status".to_owned()));
    let normalized = normalize(&input, &profile);

    assert_eq!(
        normalized.evidence.applied_rules,
        vec![
            "built_in: remove-pointer /metadata/uid",
            "user: remove-pointer /spec/nodeName",
        ]
    );
    assert!(
        normalized.evidence.warnings.is_empty(),
        "warnings: {:?}",
        normalized.evidence.warnings
    );
}

// ---------------------------------------------------------------------
// RFC 6901 pointer handling
// ---------------------------------------------------------------------

/// `~0`/`~1` escapes, an annotation key that needs them, and the pruning
/// of an annotations map that a `RemoveAnnotation` emptied.
#[test]
fn annotation_removal_handles_escaping_and_prunes_the_emptied_map() {
    let input = read_input("escaped-annotations");
    let mut profile = NormalizationProfile::built_in();
    profile.user.push(NormalizeRule::RemoveAnnotation(
        "example.com/a~b".to_owned(),
    ));
    let normalized = normalize(&input, &profile);
    assert_golden("escaped-annotations", &normalized);

    assert!(
        normalized.value["metadata"].get("annotations").is_none(),
        "the map normalization emptied is removed, so an object whose only annotations were \
         noise compares equal to one that never had any"
    );
    assert_eq!(
        normalized.evidence.applied_rules,
        vec![
            "built_in: remove-pointer /metadata/uid",
            "built_in: remove-pointer /metadata/resourceVersion",
            "built_in: remove-pointer /metadata/creationTimestamp",
            "built_in: remove-annotation kubectl.kubernetes.io/last-applied-configuration",
            "user: remove-annotation example.com/a~b",
        ]
    );
}

/// An annotations map that was *already* empty in the input is left
/// exactly as the object had it. Pruning only ever removes a map this
/// crate emptied.
#[test]
fn an_already_empty_annotations_map_is_left_alone() {
    let input = json!({"metadata": {"annotations": {}, "name": "pod"}});
    let normalized = normalize(&input, &NormalizationProfile::built_in());

    assert_eq!(normalized.value, input);
    assert!(normalized.evidence.applied_rules.is_empty());
}

/// `RemovePointer` reaches an escaped key too, and -- unlike
/// `RemoveAnnotation` -- leaves the emptied map behind, because its
/// contract is that it removes exactly what it names and nothing else.
#[test]
fn remove_pointer_honors_escapes_and_does_not_prune() {
    let input = json!({"metadata": {"annotations": {"example.com/x": "1"}}});
    let mut profile = NormalizationProfile::empty();
    profile.user.push(NormalizeRule::RemovePointer(
        "/metadata/annotations/example.com~1x".to_owned(),
    ));
    let normalized = normalize(&input, &profile);

    assert_eq!(
        normalized.value,
        json!({"metadata": {"annotations": {}}}),
        "remove-pointer is literal: the map it emptied stays"
    );
    assert_eq!(
        normalized.evidence.applied_rules,
        vec!["user: remove-pointer /metadata/annotations/example.com~1x"]
    );
}

/// `~0` unescapes to a literal `~`, and the two escapes compose: `~01`
/// is `~1` the two-character string, not the `/` that `~1` alone means.
#[test]
fn tilde_escapes_address_the_literal_key() {
    let input = json!({"metadata": {"annotations": {"a~b": "1", "a~1b": "2", "a/b": "3"}}});
    let mut profile = NormalizationProfile::empty();
    for pointer in [
        "/metadata/annotations/a~0b",
        "/metadata/annotations/a~01b",
        "/metadata/annotations/a~1b",
    ] {
        profile
            .user
            .push(NormalizeRule::RemovePointer(pointer.to_owned()));
    }
    let normalized = normalize(&input, &profile);

    assert_eq!(normalized.value, json!({"metadata": {"annotations": {}}}));
    assert_eq!(normalized.evidence.applied_rules.len(), 3);
}

/// Pointers into arrays address elements by index, and RFC 6901 §4's
/// index syntax is enforced: `01` (leading zero), `-` (the
/// after-the-last-element token), and an out-of-range index all address
/// nothing, so all three are no-ops rather than coerced positions.
#[test]
fn pointers_into_arrays_address_elements_by_index() {
    let input = json!({"spec": {"initContainers": [{"name": "a"}, {"name": "b"}, {"name": "c"}]}});
    let mut profile = NormalizationProfile::empty();
    for pointer in [
        "/spec/initContainers/01",
        "/spec/initContainers/-",
        "/spec/initContainers/9",
        "/spec/initContainers/1",
    ] {
        profile
            .user
            .push(NormalizeRule::RemovePointer(pointer.to_owned()));
    }
    let normalized = normalize(&input, &profile);

    assert_eq!(
        normalized.value["spec"]["initContainers"],
        json!([{"name": "a"}, {"name": "c"}])
    );
    assert_eq!(
        normalized.evidence.applied_rules,
        vec!["user: remove-pointer /spec/initContainers/1"]
    );
}

/// A pointer that is not valid RFC 6901 is a configuration error, kept
/// distinct from "this pointer matched nothing" -- otherwise a typo in a
/// profile would look exactly like a rule that correctly had nothing to
/// do.
#[test]
fn a_malformed_pointer_is_an_error_naming_its_tier() {
    let input = json!({"spec": {}});

    let mut missing_slash = NormalizationProfile::empty();
    missing_slash
        .user
        .push(NormalizeRule::RemovePointer("spec".to_owned()));
    assert_eq!(
        normalize_object(&input, &missing_slash),
        Err(NormalizeError::InvalidPointer {
            tier: RuleTier::User,
            source: PointerError::MissingLeadingSlash {
                pointer: "spec".to_owned()
            },
        })
    );

    let mut bad_escape = NormalizationProfile::empty();
    bad_escape.recipe.push(NormalizeRule::SortNamedArray {
        pointer: "/spec/a~2b".to_owned(),
        key: "name".to_owned(),
    });
    assert_eq!(
        normalize_object(&input, &bad_escape),
        Err(NormalizeError::InvalidPointer {
            tier: RuleTier::Recipe,
            source: PointerError::InvalidEscape {
                pointer: "/spec/a~2b".to_owned()
            },
        })
    );
}

/// Removing the empty pointer -- the whole document -- is rejected
/// rather than honored: whatever it left behind would make every
/// comparison of the object trivially equal, which is a total,
/// silent suppression of the product's own output.
#[test]
fn removing_the_document_root_is_an_error() {
    let mut profile = NormalizationProfile::empty();
    profile
        .user
        .push(NormalizeRule::RemovePointer(String::new()));
    assert_eq!(
        normalize_object(&json!({"spec": {}}), &profile),
        Err(NormalizeError::RemovesDocumentRoot {
            tier: RuleTier::User
        })
    );
}

/// A profile containing one bad rule never yields a half-normalized
/// object: every pointer is validated before anything is modified.
#[test]
fn a_bad_rule_prevents_any_normalization_at_all() {
    let input = read_input("pod-add-label-final");
    let mut profile = NormalizationProfile::built_in();
    profile
        .user
        .push(NormalizeRule::RemovePointer("metadata".to_owned()));

    assert!(normalize_object(&input, &profile).is_err());
    assert!(
        input["metadata"]["uid"].is_string(),
        "the caller's own value is never mutated, error or not"
    );
}

// ---------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------

/// The same input and the same profile produce byte-identical output,
/// every time, and normalizing an already-normalized object is a no-op.
#[test]
fn normalization_is_deterministic_and_idempotent() {
    let input = read_input("unsorted-workload");
    let profile = NormalizationProfile::built_in();

    let first = normalize(&input, &profile);
    let second = normalize(&input, &profile);
    assert_eq!(render(&first), render(&second));

    let again = normalize(&first.value, &profile);
    assert_eq!(
        again.value, first.value,
        "normalization of a normalized object changes nothing further"
    );
    assert!(
        again.evidence.applied_rules.is_empty(),
        "and therefore records no applied rules: {:?}",
        again.evidence.applied_rules
    );
}

/// `normalize_object` never mutates its input; the raw captured object
/// stays available for a report that wants to show what was actually
/// observed.
#[test]
fn the_input_value_is_never_mutated() {
    let input = read_input("pod-add-label-final");
    let before = input.clone();
    let _ = normalize(&input, &NormalizationProfile::built_in());
    assert_eq!(input, before);
}

/// An empty profile is the identity transformation with empty evidence.
#[test]
fn an_empty_profile_changes_nothing() {
    let input = read_input("pod-add-label-final");
    let normalized = normalize(&input, &NormalizationProfile::empty());

    assert_eq!(normalized.value, input);
    assert!(normalized.evidence.applied_rules.is_empty());
    assert!(normalized.evidence.warnings.is_empty());
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

/// Each element's `name`, or `None` for an element that has no string
/// `name`.
fn names(array: &Value) -> Vec<Option<&str>> {
    array
        .as_array()
        .expect("an array")
        .iter()
        .map(|item| item.get("name").and_then(Value::as_str))
        .collect()
}
