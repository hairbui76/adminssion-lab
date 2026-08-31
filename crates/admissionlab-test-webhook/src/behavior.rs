//! Behavior selection: turning a fixture object's own
//! `test.admissionlab.io/*` annotations into the [`Behavior`] this
//! webhook then executes ([`crate::mutate`] for the mutating actions,
//! [`crate::validate`] for deny/delay/fail).
//!
//! This module is the *only* place a behavior is decided. Nothing else
//! in this crate reads an environment variable, a `ConfigMap`, a command
//! line flag, or the cluster's own state to decide what to do with an
//! admission request: the request's own object carries every input, so
//! two clusters running the same fixture through the same image produce
//! the same answer by construction. That is the entire reason this
//! component exists (PRODUCT.md §30: "This prevents core tests from
//! depending entirely on external vendor behavior") — a dogfood webhook
//! whose behavior could drift with ambient configuration would be no
//! more trustworthy as a fixture than a real vendor's controller.
//!
//! # The vocabulary
//!
//! Exactly nine annotations, all under [`ANNOTATION_PREFIX`]:
//!
//! | Annotation | Value | Effect |
//! | --- | --- | --- |
//! | [`ADD_LABEL`] | `key=value` | add/overwrite `metadata.labels[key]` |
//! | [`ADD_CONTAINER`] | `name=image` | append to `spec.containers` |
//! | [`ADD_INIT_CONTAINER`] | `name=image` | append to `spec.initContainers` |
//! | [`REMOVE_CONTAINER`] | `name` | remove that entry of `spec.containers` |
//! | [`REMOVE_INIT_CONTAINER`] | `name` | remove that entry of `spec.initContainers` |
//! | [`ADD_VOLUME`] | `name` | append an `emptyDir` volume to `spec.volumes` |
//! | [`DENY`] | `message` | validating webhook denies with `message` |
//! | [`DELAY_MS`] | `250` | validating webhook sleeps that long first |
//! | [`FAIL`] | `true`/`false` | validating webhook answers HTTP 500 |
//!
//! A Kubernetes annotation map has one value per key, so every field of
//! [`Behavior`] is at most one action — there is no "add two containers"
//! form, deliberately: a fixture that needs two mutations of the same
//! kind is two fixtures, and keeping the vocabulary single-valued keeps
//! the emitted JSON Patch (and therefore every test asserting it) small
//! enough to read exactly.
//!
//! # Why an unparseable annotation denies rather than being ignored
//!
//! PRODUCT.md §30 says only that behavior is "dependent on an explicit
//! fixture annotation"; it does not say what an *invalid* one does, so
//! this crate picks — and pins here — the honest option. Every parse
//! failure, including an unrecognized key under [`ANNOTATION_PREFIX`],
//! becomes a [`BehaviorError`] that [`crate::serve`] turns into a
//! *denial naming the offending annotation and value*. It never becomes
//! a silent allow.
//!
//! The alternative (ignore what cannot be parsed, admit the object
//! unchanged) fails in exactly the way this component exists to prevent:
//! a fixture with a typo'd annotation would be admitted unmutated, the
//! run would record "no mutation observed", and that is
//! indistinguishable from the *regression* Admission Lab is supposed to
//! catch — a stack that stopped mutating. Global Constraint 15 draws
//! this same line ("missing observability data is unavailable/unknown;
//! it must never be fabricated"): a behavior this webhook could not
//! determine is not the same as a behavior that did not happen, and a
//! deny is the only outcome a fixture author cannot mistake for either.
//!
//! Unknown keys under the reserved prefix are included in that rule for
//! the same reason: `test.admissionlab.io/add-labels` (plural, a typo)
//! must not quietly do nothing. Keys *outside* the prefix are ignored
//! entirely — real objects carry unrelated annotations, and this webhook
//! has no business having an opinion about them.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;

/// The reserved annotation prefix this webhook reads, and the only one
/// it reads. An annotation whose key starts with this and is not one of
/// the nine below is a [`BehaviorError`], not a no-op — see this
/// module's own documentation.
pub const ANNOTATION_PREFIX: &str = "test.admissionlab.io/";

/// `key=value`: add (or overwrite) one `metadata.labels` entry.
pub const ADD_LABEL: &str = "test.admissionlab.io/add-label";
/// `name=image`: append one container to `spec.containers`.
pub const ADD_CONTAINER: &str = "test.admissionlab.io/add-container";
/// `name=image`: append one container to `spec.initContainers`.
pub const ADD_INIT_CONTAINER: &str = "test.admissionlab.io/add-init-container";
/// `name`: remove that named entry from `spec.containers`.
pub const REMOVE_CONTAINER: &str = "test.admissionlab.io/remove-container";
/// `name`: remove that named entry from `spec.initContainers`.
pub const REMOVE_INIT_CONTAINER: &str = "test.admissionlab.io/remove-init-container";
/// `name`: append one `emptyDir` volume to `spec.volumes`.
pub const ADD_VOLUME: &str = "test.admissionlab.io/add-volume";
/// `message`: the validating webhook denies the request with `message`.
pub const DENY: &str = "test.admissionlab.io/deny";
/// Whole milliseconds: how long the validating webhook sleeps before
/// answering. Bounded by [`MAX_DELAY_MS`].
pub const DELAY_MS: &str = "test.admissionlab.io/delay-ms";
/// `true` or `false` (exactly, lowercase): whether the validating
/// webhook answers with an HTTP 500 instead of an admission response.
pub const FAIL: &str = "test.admissionlab.io/fail";

/// The largest [`DELAY_MS`] this webhook accepts, in milliseconds.
///
/// Comfortably above the 30-second ceiling Kubernetes itself puts on a
/// webhook's `timeoutSeconds`, on purpose: a fixture must be able to
/// manufacture a *timeout* regression by asking for a delay longer than
/// any legal `timeoutSeconds`. Bounded at all so that a typo'd
/// `delay-ms: "250000000"` fails the fixture immediately and explicitly
/// rather than parking a connection (and, with `failurePolicy: Fail`,
/// the API request behind it) for days.
pub const MAX_DELAY_MS: u64 = 60_000;

/// One `name=image` pair, from [`ADD_CONTAINER`]/[`ADD_INIT_CONTAINER`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedImage {
    /// The container's `name`.
    pub name: String,
    /// The container's `image`.
    pub image: String,
}

/// Everything one object's `test.admissionlab.io/*` annotations ask this
/// webhook to do. [`Default`] — every field absent — is the complete,
/// valid description of an object carrying none of them: allow, no
/// mutation, no delay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Behavior {
    /// [`ADD_LABEL`], already split into key and value.
    pub add_label: Option<(String, String)>,
    /// [`ADD_CONTAINER`].
    pub add_container: Option<NamedImage>,
    /// [`ADD_INIT_CONTAINER`].
    pub add_init_container: Option<NamedImage>,
    /// [`REMOVE_CONTAINER`].
    pub remove_container: Option<String>,
    /// [`REMOVE_INIT_CONTAINER`].
    pub remove_init_container: Option<String>,
    /// [`ADD_VOLUME`].
    pub add_volume: Option<String>,
    /// [`DENY`]'s message.
    pub deny: Option<String>,
    /// [`DELAY_MS`], already converted.
    pub delay: Option<Duration>,
    /// [`FAIL`]. `false` both when absent and when explicitly `"false"`
    /// — the two are the same request, so they are the same value.
    pub fail: bool,
}

/// One annotation under [`ANNOTATION_PREFIX`] that could not be
/// understood. Carries the annotation key *and* the value verbatim so
/// the resulting denial message points a fixture author at the exact
/// text to fix, rather than at "a bad annotation somewhere".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("annotation {key} has an unusable value {value:?}: {reason}")]
pub struct BehaviorError {
    /// The offending annotation's full key.
    pub key: String,
    /// Its value, verbatim (a non-string JSON value is rendered by
    /// [`serde_json`], so the message stays useful for a hand-built
    /// object too).
    pub value: String,
    /// Why it could not be used, as a fixed sentence fragment — fixed,
    /// not formatted, so the same mistake always produces the same
    /// bytes on the wire (Global Constraint 7: deterministic results).
    pub reason: &'static str,
}

impl BehaviorError {
    fn new(key: &str, value: &str, reason: &'static str) -> Self {
        Self {
            key: key.to_owned(),
            value: value.to_owned(),
            reason,
        }
    }
}

/// Parses every `test.admissionlab.io/*` annotation on `object` into a
/// [`Behavior`].
///
/// `object` is the raw admission-request object as JSON — a `Pod` in
/// every fixture this recipe ships, but nothing here is Pod-specific
/// beyond reading `metadata.annotations`. An object with no `metadata`,
/// no `annotations`, or no annotation under the prefix parses to
/// [`Behavior::default`].
///
/// # Errors
///
/// Returns the first [`BehaviorError`] in ascending key order — "first"
/// is defined by sorted key order, never by the object's own field
/// order, so the same object always produces the same error however its
/// JSON happened to be written (Global Constraint 7).
pub fn parse(object: &Value) -> Result<Behavior, BehaviorError> {
    let mut behavior = Behavior::default();

    let Some(Value::Object(annotations)) = object.pointer("/metadata/annotations") else {
        return Ok(behavior);
    };

    // Collected into a `BTreeMap` before parsing rather than iterated in
    // place: `serde_json::Map` is only sorted when `serde_json`'s
    // `preserve_order` feature is *off*, and that feature can be turned
    // on by any other crate in the build graph (feature unification).
    // Sorting here makes "which invalid annotation is reported" a
    // property of this function, not of the workspace's feature
    // resolution.
    let relevant: BTreeMap<&str, &Value> = annotations
        .iter()
        .filter(|(key, _)| key.starts_with(ANNOTATION_PREFIX))
        .map(|(key, value)| (key.as_str(), value))
        .collect();

    for (key, raw) in relevant {
        let value = match raw {
            Value::String(text) => text.trim(),
            other => {
                return Err(BehaviorError::new(
                    key,
                    &other.to_string(),
                    "Kubernetes annotation values are strings",
                ));
            }
        };

        match key {
            ADD_LABEL => behavior.add_label = Some(parse_label(key, value)?),
            ADD_CONTAINER => behavior.add_container = Some(parse_named_image(key, value)?),
            ADD_INIT_CONTAINER => {
                behavior.add_init_container = Some(parse_named_image(key, value)?);
            }
            REMOVE_CONTAINER => behavior.remove_container = Some(parse_name(key, value)?),
            REMOVE_INIT_CONTAINER => {
                behavior.remove_init_container = Some(parse_name(key, value)?);
            }
            ADD_VOLUME => behavior.add_volume = Some(parse_name(key, value)?),
            DENY => behavior.deny = Some(parse_message(key, value)?),
            DELAY_MS => behavior.delay = Some(parse_delay(key, value)?),
            FAIL => behavior.fail = parse_bool(key, value)?,
            unknown => {
                return Err(BehaviorError::new(
                    unknown,
                    value,
                    "not one of this webhook's behavior annotations",
                ));
            }
        }
    }

    Ok(behavior)
}

/// `key=value`. The value half may be empty (Kubernetes allows an empty
/// label value); the key half may not, and neither half may contain a
/// second `=` — a label value cannot contain one, so a second `=` is
/// always a mistake rather than a value this webhook should pass
/// through.
fn parse_label(annotation: &str, value: &str) -> Result<(String, String), BehaviorError> {
    let (label_key, label_value) = value
        .split_once('=')
        .ok_or_else(|| BehaviorError::new(annotation, value, "expected key=value"))?;
    let label_key = label_key.trim();
    let label_value = label_value.trim();
    if label_key.is_empty() {
        return Err(BehaviorError::new(
            annotation,
            value,
            "the label key half of key=value is empty",
        ));
    }
    if label_value.contains('=') {
        return Err(BehaviorError::new(
            annotation,
            value,
            "expected exactly one = separating key from value",
        ));
    }
    Ok((label_key.to_owned(), label_value.to_owned()))
}

/// `name=image`, both halves required.
fn parse_named_image(annotation: &str, value: &str) -> Result<NamedImage, BehaviorError> {
    let (name, image) = value
        .split_once('=')
        .ok_or_else(|| BehaviorError::new(annotation, value, "expected name=image"))?;
    let name = name.trim();
    let image = image.trim();
    if name.is_empty() {
        return Err(BehaviorError::new(
            annotation,
            value,
            "the name half of name=image is empty",
        ));
    }
    if image.is_empty() {
        return Err(BehaviorError::new(
            annotation,
            value,
            "the image half of name=image is empty",
        ));
    }
    if image.contains('=') {
        return Err(BehaviorError::new(
            annotation,
            value,
            "expected exactly one = separating name from image",
        ));
    }
    Ok(NamedImage {
        name: name.to_owned(),
        image: image.to_owned(),
    })
}

/// A bare Kubernetes object name — non-empty, and with no `=`, which
/// would mean the author wrote a `name=value` form for an annotation
/// that takes only a name.
fn parse_name(annotation: &str, value: &str) -> Result<String, BehaviorError> {
    if value.is_empty() {
        return Err(BehaviorError::new(annotation, value, "expected a name"));
    }
    if value.contains('=') {
        return Err(BehaviorError::new(
            annotation,
            value,
            "expected a bare name, not name=value",
        ));
    }
    Ok(value.to_owned())
}

/// A denial message: any text, as long as there is some. An empty
/// message would produce a denial a fixture author cannot act on.
fn parse_message(annotation: &str, value: &str) -> Result<String, BehaviorError> {
    if value.is_empty() {
        return Err(BehaviorError::new(
            annotation,
            value,
            "expected a denial message",
        ));
    }
    Ok(value.to_owned())
}

/// Whole milliseconds, bounded by [`MAX_DELAY_MS`].
fn parse_delay(annotation: &str, value: &str) -> Result<Duration, BehaviorError> {
    let millis: u64 = value.parse().map_err(|_| {
        BehaviorError::new(
            annotation,
            value,
            "expected whole milliseconds as a non-negative integer",
        )
    })?;
    if millis > MAX_DELAY_MS {
        return Err(BehaviorError::new(
            annotation,
            value,
            "exceeds this webhook's maximum delay of 60000 milliseconds",
        ));
    }
    Ok(Duration::from_millis(millis))
}

/// Exactly `"true"` or `"false"`. Deliberately not YAML's wider set
/// (`yes`/`on`/`True`/...): the value reaching this function is already
/// a Kubernetes annotation *string*, so there is no YAML boolean left to
/// be liberal about, and accepting spellings the manifest author did not
/// write is how a fixture ends up meaning something other than it says.
fn parse_bool(annotation: &str, value: &str) -> Result<bool, BehaviorError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(BehaviorError::new(
            annotation,
            value,
            "expected exactly \"true\" or \"false\"",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{Behavior, NamedImage, parse};

    /// A minimal object carrying exactly `annotations`.
    fn object(annotations: &serde_json::Value) -> serde_json::Value {
        json!({ "metadata": { "annotations": annotations } })
    }

    #[test]
    fn an_object_with_no_annotations_asks_for_nothing() {
        let parsed = parse(&json!({"metadata": {"name": "fixture"}}))
            .expect("an object with no annotations must parse");
        assert_eq!(parsed, Behavior::default());
    }

    #[test]
    fn annotations_outside_the_reserved_prefix_are_ignored() {
        let parsed = parse(&object(&json!({
            "kubectl.kubernetes.io/last-applied-configuration": "{}",
            "example.com/test.admissionlab.io/deny": "not ours",
        })))
        .expect("unrelated annotations must not be an error");
        assert_eq!(parsed, Behavior::default());
    }

    #[test]
    fn every_annotation_parses_into_its_own_field() {
        let parsed = parse(&object(&json!({
            "test.admissionlab.io/add-label": "team=platform",
            "test.admissionlab.io/add-container": "sidecar=registry.k8s.io/pause:3.10",
            "test.admissionlab.io/add-init-container": "setup=registry.k8s.io/busybox:1.36",
            "test.admissionlab.io/remove-container": "legacy",
            "test.admissionlab.io/remove-init-container": "legacy-init",
            "test.admissionlab.io/add-volume": "scratch",
            "test.admissionlab.io/deny": "denied by fixture",
            "test.admissionlab.io/delay-ms": "250",
            "test.admissionlab.io/fail": "true",
        })))
        .expect("the full vocabulary must parse");

        assert_eq!(
            parsed,
            Behavior {
                add_label: Some(("team".to_owned(), "platform".to_owned())),
                add_container: Some(NamedImage {
                    name: "sidecar".to_owned(),
                    image: "registry.k8s.io/pause:3.10".to_owned(),
                }),
                add_init_container: Some(NamedImage {
                    name: "setup".to_owned(),
                    image: "registry.k8s.io/busybox:1.36".to_owned(),
                }),
                remove_container: Some("legacy".to_owned()),
                remove_init_container: Some("legacy-init".to_owned()),
                add_volume: Some("scratch".to_owned()),
                deny: Some("denied by fixture".to_owned()),
                delay: Some(Duration::from_millis(250)),
                fail: true,
            }
        );
    }

    #[test]
    fn a_label_value_may_be_empty_but_its_key_may_not() {
        let parsed = parse(&object(
            &json!({"test.admissionlab.io/add-label": "marker="}),
        ))
        .expect("Kubernetes allows an empty label value");
        assert_eq!(parsed.add_label, Some(("marker".to_owned(), String::new())));

        let error = parse(&object(
            &json!({"test.admissionlab.io/add-label": "=value"}),
        ))
        .expect_err("an empty label key must be rejected");
        assert_eq!(error.key, super::ADD_LABEL);
    }

    #[test]
    fn fail_false_is_the_same_request_as_no_fail_annotation() {
        let parsed = parse(&object(&json!({"test.admissionlab.io/fail": "false"})))
            .expect("an explicit false must parse");
        assert_eq!(parsed, Behavior::default());
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        let parsed = parse(&object(&json!({
            "test.admissionlab.io/add-container": "  sidecar = registry.k8s.io/pause:3.10  ",
        })))
        .expect("a manifest author's stray whitespace must not change the meaning");
        assert_eq!(
            parsed.add_container,
            Some(NamedImage {
                name: "sidecar".to_owned(),
                image: "registry.k8s.io/pause:3.10".to_owned(),
            })
        );
    }

    /// The heart of this module's documented contract: an unrecognized
    /// key under the reserved prefix is a typo, and a typo must never
    /// look like "this stack performed no mutation" — see this module's
    /// own documentation.
    #[test]
    fn an_unknown_annotation_under_the_prefix_is_an_error_not_a_no_op() {
        let error = parse(&object(&json!({"test.admissionlab.io/add-labels": "a=b"})))
            .expect_err("a typo'd annotation must not be silently ignored");
        assert_eq!(error.key, "test.admissionlab.io/add-labels");
        assert_eq!(error.value, "a=b");
    }

    #[test]
    fn a_non_string_annotation_value_is_an_error() {
        let error = parse(&object(&json!({"test.admissionlab.io/delay-ms": 250})))
            .expect_err("a JSON number is not a Kubernetes annotation value");
        assert_eq!(error.key, super::DELAY_MS);
    }

    #[test]
    fn the_reported_error_is_the_lowest_offending_key_not_the_first_written() {
        // Two invalid annotations at once: whichever way `serde_json`
        // happens to order its map, the *same* one must be reported.
        let error = parse(&object(&json!({
            "test.admissionlab.io/fail": "yes",
            "test.admissionlab.io/deny": "",
        })))
        .expect_err("both annotations are invalid");
        assert_eq!(
            error.key,
            super::DENY,
            "\"deny\" sorts before \"fail\", so it is always the reported one"
        );
    }

    #[test]
    fn delay_is_bounded() {
        let ok = parse(&object(&json!({"test.admissionlab.io/delay-ms": "60000"})))
            .expect("the maximum itself must be accepted");
        assert_eq!(ok.delay, Some(Duration::from_millis(super::MAX_DELAY_MS)));

        let error = parse(&object(&json!({"test.admissionlab.io/delay-ms": "60001"})))
            .expect_err("one millisecond over the maximum must be rejected");
        assert_eq!(error.key, super::DELAY_MS);
    }

    #[test]
    fn delay_rejects_anything_that_is_not_whole_milliseconds() {
        for bad in ["250ms", "-1", "2.5", "", "250 000"] {
            let error = parse(&object(&json!({"test.admissionlab.io/delay-ms": bad})))
                .expect_err("only whole milliseconds are a valid delay");
            assert_eq!(error.key, super::DELAY_MS, "for value {bad:?}");
        }
    }

    #[test]
    fn a_name_only_annotation_rejects_a_pair() {
        let error = parse(&object(
            &json!({"test.admissionlab.io/remove-container": "sidecar=image"}),
        ))
        .expect_err("remove-container takes a bare name");
        assert_eq!(error.key, super::REMOVE_CONTAINER);
    }

    #[test]
    fn a_pair_annotation_rejects_a_bare_name() {
        let error = parse(&object(
            &json!({"test.admissionlab.io/add-container": "sidecar"}),
        ))
        .expect_err("add-container takes name=image");
        assert_eq!(error.key, super::ADD_CONTAINER);
    }
}
