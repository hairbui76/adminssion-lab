//! Kubernetes object identity: validating that a parsed fixture document
//! has the deterministic identity Alpha requires, and folding that
//! identity into a stable [`FixtureId`].
//!
//! # What Alpha requires (brief Step 2 / controller supplement §4)
//!
//! A document is only accepted as a fixture if it is a JSON/YAML object
//! (mapping) carrying a non-empty `apiVersion`, a non-empty `kind`, and a
//! deterministic name. "Deterministic name" means `metadata.name`
//! specifically: `metadata.generateName` is rejected even when present
//! and non-empty, because Kubernetes assigns the actual name for a
//! `generateName` object at admission time -- unpredictable and
//! non-reproducible across runs -- and no rewrite contract exists yet to
//! pin it to something stable (that is future work, not this task's).
//! [`FixtureError::GenerateNameUnsupported`]'s own message says this,
//! not merely that the field is unsupported, so a user who hits it with
//! a real manifest knows it is a deliberate Alpha limit.
//!
//! A field that is present but not a non-empty string (wrong JSON type,
//! or an empty string) is treated identically to that field being
//! absent: [`FixtureError::MissingField`] does not distinguish "you
//! forgot this" from "you wrote something that cannot be a name" because
//! neither is usable as part of a deterministic identity.
//!
//! # Fixture ID construction
//!
//! [`compute_fixture_id`] folds a document's normalized relative path,
//! its zero-based `document_index` within that file, and its
//! [`ObjectIdentity`] (`kind`, `namespace` when present, `name`) into one
//! [`FixtureId`]. See that function's own documentation for the
//! determinism argument and for why this module does not try to make
//! that construction collision-proof on its own.

use std::path::Path;

use admissionlab_core::FixtureId;
use serde_json::{Map, Value};

use crate::FixtureError;

/// The parts of a parsed fixture document's own Kubernetes identity that
/// [`compute_fixture_id`] folds into a [`FixtureId`]. Never built from
/// `metadata.generateName` -- see this module's documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjectIdentity {
    pub(crate) kind: String,
    pub(crate) namespace: Option<String>,
    pub(crate) name: String,
}

/// Validates that `document` (the `document_index`-th document parsed
/// from `path`) looks like a Kubernetes object Alpha can give a
/// deterministic identity to, and extracts that identity.
///
/// Never called with `Value::Null` in practice -- [`crate::discover`]
/// filters a null document (an empty, comment-only, or bare `---`
/// document) out before validating anything, since it is not a
/// candidate object at all, not a malformed one. This function still
/// handles `Null` consistently (as [`FixtureError::NotAnObject`]) rather
/// than assuming that filtering happened, in case a future caller is
/// added that does not filter first.
///
/// # Errors
///
/// Returns [`FixtureError::NotAnObject`] if `document` is not a JSON/YAML
/// mapping at all (a scalar, array, or null). Returns
/// [`FixtureError::MissingField`] if `apiVersion`, `kind`, or
/// `metadata.name` is absent, not a string, or an empty string --
/// including when `metadata` itself is absent or not a mapping. Returns
/// [`FixtureError::GenerateNameUnsupported`] if `metadata.name` is
/// missing but `metadata.generateName` is present as a non-empty string.
pub(crate) fn extract_object_identity(
    document: &Value,
    path: &Path,
    document_index: usize,
) -> Result<ObjectIdentity, FixtureError> {
    let object = document
        .as_object()
        .ok_or_else(|| FixtureError::NotAnObject {
            path: path.to_path_buf(),
            document_index,
            found: json_type_name(document),
        })?;

    if !has_nonempty_string(object, "apiVersion") {
        return Err(FixtureError::MissingField {
            path: path.to_path_buf(),
            document_index,
            field: "apiVersion",
        });
    }
    let kind = nonempty_string(object, "kind").ok_or_else(|| FixtureError::MissingField {
        path: path.to_path_buf(),
        document_index,
        field: "kind",
    })?;

    let metadata = object.get("metadata").and_then(Value::as_object);
    let name = metadata.and_then(|m| nonempty_string(m, "name"));
    let has_generate_name = metadata.is_some_and(|m| has_nonempty_string(m, "generateName"));

    let name = match (name, has_generate_name) {
        (Some(name), _) => name,
        (None, true) => {
            return Err(FixtureError::GenerateNameUnsupported {
                path: path.to_path_buf(),
                document_index,
            });
        }
        (None, false) => {
            return Err(FixtureError::MissingField {
                path: path.to_path_buf(),
                document_index,
                field: "metadata.name",
            });
        }
    };

    let namespace = metadata.and_then(|m| nonempty_string(m, "namespace"));

    Ok(ObjectIdentity {
        kind,
        namespace,
        name,
    })
}

/// A human-readable JSON/YAML type name for
/// [`FixtureError::NotAnObject::found`].
fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Returns `object[field]` as an owned `String` if it is present and a
/// non-empty JSON string; `None` for absent, wrong-typed, or empty-string
/// otherwise.
fn nonempty_string(object: &Map<String, Value>, field: &str) -> Option<String> {
    object
        .get(field)?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Whether `object[field]` is present and a non-empty JSON string.
fn has_nonempty_string(object: &Map<String, Value>, field: &str) -> bool {
    nonempty_string(object, field).is_some()
}

/// Converts `s` into a component suitable for embedding in a
/// [`FixtureId`]: lowercased, with every maximal run of one or more
/// characters outside `[a-z0-9]` collapsed to a single `-`, and any
/// leading or trailing `-` trimmed.
///
/// This is a lossy, best-effort transform for legibility, **not** a
/// guaranteed-injective encoding: two different inputs can slugify to
/// the same output (for example `"a.b"` and `"a-b"` both become
/// `"a-b"` -- pinned by this module's own
/// `slugify_is_lossy_by_design_a_dot_and_a_hyphen_collide` test).
/// [`compute_fixture_id`] does not rely on this function alone for
/// uniqueness -- see that function's documentation.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_separator = false;
    for c in s.chars() {
        let lower = c.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            out.push(lower);
            last_was_separator = false;
        } else if !last_was_separator {
            out.push('-');
            last_was_separator = true;
        }
    }
    out.trim_matches('-').to_owned()
}

/// Computes the deterministic [`FixtureId`] for one fixture document.
///
/// # Determinism
///
/// The only inputs are `relative_path` (already normalized by
/// [`crate::discover`] -- see that module's documentation for what
/// normalization means on this project's sole build target),
/// `document_index`, and `identity`'s own `kind`/`namespace`/`name`.
/// Nothing else reaches this function: no absolute path, no random
/// value, no wall-clock time, and no iteration order over any
/// collection. The same four inputs always produce the same
/// [`FixtureId`], on any machine.
///
/// # Not collision-proof by itself
///
/// [`slugify`] is lossy (see its own documentation). This function does
/// not attempt to make its output injective on its own; instead,
/// [`crate::discover::discover_fixtures`] checks the *complete*
/// discovered set for a repeated [`FixtureId`] afterward and fails
/// loudly if two documents ever land on the same one. A collision here
/// requires two distinct fixture documents -- necessarily at different
/// `relative_path`/`document_index` pairs, since a real filesystem walk
/// never yields the same file twice and one file's own document stream
/// never repeats an index -- to slugify to an identical string; possible,
/// but only when the inputs differ solely in characters this function
/// discards.
///
/// # Why this always succeeds
///
/// This returns a bare [`FixtureId`], not a `Result`: the assembled
/// candidate string can never fail [`FixtureId::parse`], which is a
/// property of this function's own construction, not an assumption
/// about it. `FixtureId::parse` rejects exactly two things (its own,
/// documented contract): an empty string, and any character outside
/// `[a-z0-9-]`. Neither is reachable here --
/// [`slugify`] only ever pushes an ASCII lowercase letter, an ASCII
/// digit, or `-` (by inspection of its own loop body: every branch is
/// one of those three), so every `parts` entry it produces already
/// satisfies the character-set rule, and joining them with `-` (itself
/// an allowed character) cannot introduce a new one. Non-emptiness is
/// guaranteed independently of `slugify` entirely: `parts`'s *last*
/// entry is always `document_index.to_string()`, which for a `usize`
/// is always one or more ASCII digits (never empty), so the joined
/// `candidate` can never be empty even when every other part slugifies
/// to `""`. This module's own
/// `compute_fixture_id_never_panics_even_when_every_slugifiable_part_is_degenerate`
/// test exercises exactly that worst case.
///
/// An earlier version of this function returned
/// `Result<FixtureId, FixtureError>` with an `InvalidFixtureId` variant
/// for this supposedly-fallible step. It was removed: no input could
/// ever construct it (confirmed by trying, in this module's own tests),
/// which made it exactly the kind of check that could never fail this
/// task was warned against shipping.
pub(crate) fn compute_fixture_id(
    relative_path: &str,
    document_index: usize,
    identity: &ObjectIdentity,
) -> FixtureId {
    let mut parts = vec![slugify(relative_path), slugify(&identity.kind)];
    if let Some(namespace) = &identity.namespace {
        parts.push(slugify(namespace));
    }
    parts.push(slugify(&identity.name));
    parts.push(document_index.to_string());

    let candidate = parts.join("-");
    FixtureId::parse(&candidate).expect(
        "candidate satisfies FixtureId::parse's documented rules by construction -- see this \
         function's own \"Why this always succeeds\" documentation",
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ObjectIdentity, compute_fixture_id, extract_object_identity, slugify};
    use crate::FixtureError;

    fn path() -> std::path::PathBuf {
        std::path::PathBuf::from("fixtures/example.yaml")
    }

    // -------------------------------------------------------------
    // extract_object_identity
    // -------------------------------------------------------------

    #[test]
    fn extracts_cluster_scoped_identity() {
        let doc = json!({
            "apiVersion": "v1",
            "kind": "Namespace",
            "metadata": {"name": "demo"},
        });
        let identity = extract_object_identity(&doc, &path(), 0).unwrap();
        assert_eq!(
            identity,
            ObjectIdentity {
                kind: "Namespace".to_owned(),
                namespace: None,
                name: "demo".to_owned(),
            }
        );
    }

    #[test]
    fn extracts_namespaced_identity() {
        let doc = json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {"name": "demo", "namespace": "default"},
        });
        let identity = extract_object_identity(&doc, &path(), 0).unwrap();
        assert_eq!(
            identity,
            ObjectIdentity {
                kind: "ConfigMap".to_owned(),
                namespace: Some("default".to_owned()),
                name: "demo".to_owned(),
            }
        );
    }

    #[test]
    fn missing_api_version_is_rejected() {
        let doc = json!({"kind": "ConfigMap", "metadata": {"name": "demo"}});
        let err = extract_object_identity(&doc, &path(), 3).unwrap_err();
        assert!(matches!(
            err,
            FixtureError::MissingField {
                field: "apiVersion",
                document_index: 3,
                ..
            }
        ));
    }

    #[test]
    fn missing_kind_is_rejected() {
        let doc = json!({"apiVersion": "v1", "metadata": {"name": "demo"}});
        let err = extract_object_identity(&doc, &path(), 0).unwrap_err();
        assert!(matches!(
            err,
            FixtureError::MissingField { field: "kind", .. }
        ));
    }

    #[test]
    fn missing_name_and_generate_name_is_rejected() {
        let doc = json!({"apiVersion": "v1", "kind": "ConfigMap", "metadata": {}});
        let err = extract_object_identity(&doc, &path(), 0).unwrap_err();
        assert!(matches!(
            err,
            FixtureError::MissingField {
                field: "metadata.name",
                ..
            }
        ));
    }

    #[test]
    fn missing_metadata_entirely_is_rejected_as_missing_name() {
        let doc = json!({"apiVersion": "v1", "kind": "ConfigMap"});
        let err = extract_object_identity(&doc, &path(), 0).unwrap_err();
        assert!(matches!(
            err,
            FixtureError::MissingField {
                field: "metadata.name",
                ..
            }
        ));
    }

    #[test]
    fn empty_string_name_is_treated_as_missing() {
        let doc = json!({"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": ""}});
        let err = extract_object_identity(&doc, &path(), 0).unwrap_err();
        assert!(matches!(
            err,
            FixtureError::MissingField {
                field: "metadata.name",
                ..
            }
        ));
    }

    #[test]
    fn generate_name_without_name_is_rejected_with_its_own_reason() {
        let doc = json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {"generateName": "demo-"},
        });
        let err = extract_object_identity(&doc, &path(), 5).unwrap_err();
        assert!(matches!(
            err,
            FixtureError::GenerateNameUnsupported {
                document_index: 5,
                ..
            }
        ));
        // The brief/supplement require the message to say *why*
        // (deterministic identity), not merely that the field is
        // unsupported -- pin that content, not only the variant.
        let message = err.to_string();
        assert!(
            message.contains("generateName") && message.contains("deterministic"),
            "message {message:?} must explain the deterministic-name requirement"
        );
    }

    #[test]
    fn name_takes_precedence_when_both_name_and_generate_name_are_present() {
        // Only *missing* `name` falls back to (and is rejected by)
        // `generateName`; a document naming both is unambiguous and
        // must succeed using `name`.
        let doc = json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {"name": "demo", "generateName": "demo-"},
        });
        let identity = extract_object_identity(&doc, &path(), 0).unwrap();
        assert_eq!(identity.name, "demo");
    }

    #[test]
    fn a_yaml_list_document_is_rejected_as_not_an_object() {
        let doc = json!([1, 2, 3]);
        let err = extract_object_identity(&doc, &path(), 0).unwrap_err();
        assert!(matches!(
            err,
            FixtureError::NotAnObject {
                found: "an array",
                ..
            }
        ));
    }

    #[test]
    fn a_scalar_document_is_rejected_as_not_an_object() {
        let doc = json!("just a string");
        let err = extract_object_identity(&doc, &path(), 0).unwrap_err();
        assert!(matches!(
            err,
            FixtureError::NotAnObject {
                found: "a string",
                ..
            }
        ));
    }

    // -------------------------------------------------------------
    // slugify
    // -------------------------------------------------------------

    #[test]
    fn slugify_lowercases_and_collapses_separators() {
        assert_eq!(slugify("My.Weird Name_2"), "my-weird-name-2");
    }

    #[test]
    fn slugify_trims_leading_and_trailing_separators() {
        assert_eq!(slugify("--Foo--"), "foo");
        assert_eq!(slugify(".hidden"), "hidden");
    }

    #[test]
    fn slugify_of_all_disallowed_characters_is_empty() {
        assert_eq!(slugify("..."), "");
    }

    /// Documents, with a runnable assertion, the lossiness
    /// [`compute_fixture_id`]'s own documentation warns about: two
    /// distinct, legal strings collapse to the same slug. This is why
    /// `discover_fixtures` cannot treat `slugify` alone as proof of
    /// uniqueness.
    #[test]
    fn slugify_is_lossy_by_design_a_dot_and_a_hyphen_collide() {
        assert_eq!(slugify("a.b"), slugify("a-b"));
        assert_eq!(slugify("a.b"), "a-b");
    }

    // -------------------------------------------------------------
    // compute_fixture_id
    // -------------------------------------------------------------

    fn identity(kind: &str, namespace: Option<&str>, name: &str) -> ObjectIdentity {
        ObjectIdentity {
            kind: kind.to_owned(),
            namespace: namespace.map(str::to_owned),
            name: name.to_owned(),
        }
    }

    #[test]
    fn computes_a_readable_id_for_a_flat_path() {
        let id = compute_fixture_id(
            "basic-mutation.yaml",
            0,
            &identity("ConfigMap", None, "basic-mutation"),
        );
        assert_eq!(
            id.as_str(),
            "basic-mutation-yaml-configmap-basic-mutation-0"
        );
    }

    #[test]
    fn same_inputs_produce_the_same_id_every_time() {
        let id_a = compute_fixture_id("a.yaml", 2, &identity("Secret", None, "s"));
        let id_b = compute_fixture_id("a.yaml", 2, &identity("Secret", None, "s"));
        assert_eq!(id_a, id_b);
    }

    #[test]
    fn document_index_is_part_of_the_id() {
        let id_0 = compute_fixture_id("a.yaml", 0, &identity("Secret", None, "s"));
        let id_1 = compute_fixture_id("a.yaml", 1, &identity("Secret", None, "s"));
        assert_ne!(id_0, id_1);
    }

    #[test]
    fn namespace_is_part_of_the_id_when_present() {
        let cluster_scoped = compute_fixture_id("a.yaml", 0, &identity("ConfigMap", None, "s"));
        let namespaced =
            compute_fixture_id("a.yaml", 0, &identity("ConfigMap", Some("default"), "s"));
        assert_ne!(
            cluster_scoped, namespaced,
            "a present namespace must change the id, not be silently dropped"
        );
    }

    #[test]
    fn different_namespaces_produce_different_ids() {
        let a = compute_fixture_id("a.yaml", 0, &identity("ConfigMap", Some("ns-a"), "s"));
        let b = compute_fixture_id("a.yaml", 0, &identity("ConfigMap", Some("ns-b"), "s"));
        assert_ne!(a, b);
    }

    #[test]
    fn dns_subdomain_names_containing_dots_still_produce_a_valid_id() {
        // Kubernetes object names may be DNS-1123 *subdomains*, which
        // permit `.` (for example a webhook configuration named
        // "my.example.com"). `FixtureId` itself forbids `.`, so this
        // must still succeed via `slugify` rather than panicking.
        let id = compute_fixture_id(
            "webhooks.yaml",
            0,
            &identity("ValidatingWebhookConfiguration", None, "my.example.com"),
        );
        assert_eq!(
            id.as_str(),
            "webhooks-yaml-validatingwebhookconfiguration-my-example-com-0"
        );
    }

    #[test]
    fn compute_fixture_id_never_panics_even_when_every_slugifiable_part_is_degenerate() {
        // `path`, `kind`, and `name` all slugify to "" (see
        // `slugify_of_all_disallowed_characters_is_empty` above); only
        // `document_index` keeps the candidate non-empty. This is the
        // worst case `compute_fixture_id`'s own "Why this always
        // succeeds" documentation argues about -- exercised here for
        // real, not merely asserted in prose. The mutation this test
        // exists to kill: an implementation that put `document_index`
        // anywhere but guaranteed-non-empty-and-always-present in
        // `parts` (or that reordered `parts.join` around it) could make
        // this candidate genuinely empty and panic the internal
        // `.expect()`.
        let id = compute_fixture_id("...", 0, &identity("...", None, "..."));
        assert_eq!(id.as_str(), "---0");
    }

    #[test]
    fn compute_fixture_id_never_panics_when_only_namespace_is_degenerate() {
        let id = compute_fixture_id("a.yaml", 0, &identity("ConfigMap", Some("..."), "demo"));
        assert_eq!(id.as_str(), "a-yaml-configmap--demo-0");
    }
}
