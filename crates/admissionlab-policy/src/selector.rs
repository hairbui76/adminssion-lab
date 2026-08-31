//! Which changes a policy override (or, from Task 4.9, an expectation)
//! applies to.
//!
//! [`ChangeSelector`] is the §1.2 registry's canonical, policy-owned
//! narrowing vocabulary: a fixture glob, a subject, and an object path,
//! each optional, each an independent restriction. All three are
//! *conjunctive* -- a change must satisfy every dimension that is set --
//! and an unset dimension restricts nothing. A selector with nothing set
//! therefore matches every change of the kind it is attached to, which
//! is the correct reading of `policy.overrides` entries that name only a
//! `kind`.
//!
//! # Absent is not wildcard
//!
//! A [`SemanticChange`] may itself have no `subject` or no `object_path`
//! (`admissionlab_diff` documents `None` there as "this comparison
//! genuinely had nothing to put here"). Such a change does **not** match
//! a selector that names one. The alternative -- treating the change's
//! own `None` as "matches anything" -- would make a narrowly scoped
//! override silently apply to whole-request decision flips that have no
//! path at all, which is the opposite of what someone writing `path:
//! /spec/containers/0/image` is asking for.
//!
//! # Why only `fixtures` is a glob
//!
//! Fixture identifiers are enumerable and stable, so a glob over them is
//! a statement about a set the user can see (`web-*`). `subject` and
//! `object_path` are compared for exact equality instead. A glob (or a
//! path prefix rule) over `object_path` is a plausible extension, but it
//! needs a decided answer to "does `/spec/containers` cover
//! `/spec/containers/0/image`?" -- an RFC 6901 segment-boundary rule
//! that must be specified, documented, and tested rather than inherited
//! by accident from whichever matcher was reached for first. Alpha
//! answers the narrow question exactly and leaves the broad one open;
//! see `ROADMAP.md` Task 4.8, which asks only for "optional
//! subject/object path".

use admissionlab_diff::SemanticChange;
use admissionlab_spec::PolicyOverrideSpec;
use globset::{Glob, GlobMatcher};
use serde::Deserialize;

use crate::error::PolicyValidationError;

/// Narrows which [`SemanticChange`]s a policy override or expectation
/// applies to.
///
/// Every field is the value exactly as written in configuration; nothing
/// here is compiled or validated. [`CompiledSelector`] is the checked
/// form matching actually uses, and building one is what rejects an
/// unparsable glob or an empty-string restriction.
///
/// Derives no `Default`: an all-[`None`] selector means "match every
/// change of this kind", and that is a decision a caller should have to
/// write out rather than reach by calling `default()`.
///
/// [`Deserialize`] is derived (with the same `camelCase` +
/// `deny_unknown_fields` strictness `admissionlab_spec`'s own model
/// uses, for the same reason) because Task 4.9's `ExpectedChange`
/// carries one straight out of a hand-written `expectations.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeSelector {
    /// Restrict to fixtures whose identifier matches this glob.
    #[serde(default)]
    pub fixture_glob: Option<String>,
    /// Restrict to changes whose [`SemanticChange::subject`] is exactly
    /// this -- a container name, a webhook name.
    #[serde(default)]
    pub subject: Option<String>,
    /// Restrict to changes whose [`SemanticChange::object_path`] is
    /// exactly this RFC 6901 pointer, for example
    /// `/spec/containers/0/image`. A location *inside the compared
    /// object*, never a filesystem path.
    #[serde(default)]
    pub object_path: Option<String>,
}

impl ChangeSelector {
    /// A selector that restricts nothing.
    #[must_use]
    pub fn unrestricted() -> Self {
        Self {
            fixture_glob: None,
            subject: None,
            object_path: None,
        }
    }

    /// Reads the three narrowing dimensions out of a
    /// [`PolicyOverrideSpec`].
    ///
    /// The configuration model spells them `fixtures`/`subject`/`path`
    /// (flat, alongside `kind` and `severity`) while the registry's
    /// policy-owned type spells them `fixture_glob`/`subject`/
    /// `object_path` (nested). Both names are frozen by §1.2, so this
    /// crate translates rather than renaming either -- and translating
    /// in one named place means matching has exactly one input shape to
    /// reason about, whether it came from `policy.overrides` or from an
    /// expectations file.
    #[must_use]
    pub fn from_override(spec: &PolicyOverrideSpec) -> Self {
        Self {
            fixture_glob: spec.fixtures.clone(),
            subject: spec.subject.clone(),
            object_path: spec.path.clone(),
        }
    }
}

/// A [`ChangeSelector`] whose glob is compiled and whose restrictions
/// are known to be meaningful.
///
/// Built once per override or expectation at load time, then reused for
/// every change in the run: compiling a glob per change would be both
/// wasteful and a place for a late failure to hide, and the whole point
/// of Task 4.8 step 3 is that an unusable selector is rejected before a
/// cluster exists.
#[derive(Debug, Clone)]
pub struct CompiledSelector {
    /// The compiled `fixture_glob`, matched against
    /// [`admissionlab_core::FixtureId::as_str`].
    fixture: Option<GlobMatcher>,
    /// The required subject, compared for exact equality.
    subject: Option<String>,
    /// The required object path, compared for exact equality.
    object_path: Option<String>,
    /// How many of the three dimensions are set. Cached because
    /// `crate::evaluate` consults it for every (override, change) pair
    /// it has to break a tie between.
    specificity: u8,
}

impl CompiledSelector {
    /// Compiles and checks `selector`, reporting **every** problem it
    /// has rather than the first.
    ///
    /// `locator` is the dotted locator of the thing carrying the
    /// selector (for example `policy.overrides[1]`); the returned errors
    /// extend it with the offending field's own name.
    ///
    /// Rejects, as "impossible" selectors in the sense of Task 4.8
    /// step 3:
    ///
    /// - a `fixtureGlob` that `globset` cannot parse, and
    /// - any dimension that is present but empty (or all whitespace).
    ///
    /// The second rule matters because an empty restriction is not a
    /// harmless no-op: a `subject: ""` can never equal a real subject,
    /// so the override it belongs to would match nothing, forever,
    /// silently. That is exactly the failure mode a user cannot debug
    /// from a report, so it is refused at the door instead. An
    /// *omitted* dimension remains the way to say "unrestricted".
    ///
    /// # Errors
    ///
    /// Returns every [`PolicyValidationError`] the selector produced, in
    /// field order, if it produced any.
    pub fn compile(
        selector: &ChangeSelector,
        locator: &str,
    ) -> Result<Self, Vec<PolicyValidationError>> {
        let mut errors = Vec::new();

        let fixture = match nonempty(selector.fixture_glob.as_deref()) {
            Restriction::Absent => None,
            Restriction::Empty => {
                errors.push(PolicyValidationError::new(
                    format_args!("{locator}.fixtures"),
                    "must not be empty (omit it to match every fixture)",
                ));
                None
            }
            Restriction::Present(pattern) => match Glob::new(pattern) {
                Ok(glob) => Some(glob.compile_matcher()),
                Err(source) => {
                    errors.push(PolicyValidationError::new(
                        format_args!("{locator}.fixtures"),
                        format_args!("invalid glob pattern {pattern:?}: {source}"),
                    ));
                    None
                }
            },
        };

        let subject = checked_exact(
            selector.subject.as_deref(),
            &format!("{locator}.subject"),
            "must not be empty (omit it to match every subject)",
            &mut errors,
        );
        let object_path = checked_exact(
            selector.object_path.as_deref(),
            &format!("{locator}.path"),
            "must not be empty (omit it to match every object path)",
            &mut errors,
        );

        if errors.is_empty() {
            let specificity = u8::from(fixture.is_some())
                + u8::from(subject.is_some())
                + u8::from(object_path.is_some());
            Ok(Self {
                fixture,
                subject,
                object_path,
                specificity,
            })
        } else {
            Err(errors)
        }
    }

    /// A compiled selector that restricts nothing.
    ///
    /// Infallible, unlike [`CompiledSelector::compile`]: there is
    /// nothing in it to reject. Used for a `policy.overrides` entry or
    /// an expectation that narrows by kind alone.
    #[must_use]
    pub fn unrestricted() -> Self {
        Self {
            fixture: None,
            subject: None,
            object_path: None,
            specificity: 0,
        }
    }

    /// How many of the three dimensions this selector restricts, `0..=3`.
    ///
    /// `crate::evaluate` uses this as the primary key when several
    /// overrides match one change -- see that module's documentation for
    /// why a *count* (rather than a partial order over which dimensions
    /// are set) is the rule, and how equal counts are resolved.
    #[must_use]
    pub fn specificity(&self) -> u8 {
        self.specificity
    }

    /// Whether `change` satisfies every dimension this selector
    /// restricts.
    ///
    /// See this module's documentation for the two rules that are easy
    /// to get wrong: an unset dimension restricts nothing, and a change
    /// whose own `subject`/`object_path` is [`None`] never satisfies a
    /// selector that names one.
    #[must_use]
    pub fn matches(&self, change: &SemanticChange) -> bool {
        if let Some(fixture) = &self.fixture
            && !fixture.is_match(change.fixture_id.as_str())
        {
            return false;
        }
        if let Some(subject) = &self.subject
            && change.subject.as_deref() != Some(subject.as_str())
        {
            return false;
        }
        if let Some(object_path) = &self.object_path
            && change.object_path.as_deref() != Some(object_path.as_str())
        {
            return false;
        }
        true
    }
}

/// What a configured restriction actually said.
enum Restriction<'a> {
    /// The field was omitted: no restriction.
    Absent,
    /// The field was present but empty or all whitespace: rejected.
    Empty,
    /// The field carried a usable value, trimmed.
    Present(&'a str),
}

/// Classifies an optional configured restriction.
///
/// Trims before deciding, matching `admissionlab_spec::validate`'s
/// habit of trimming user-written scalars, so ` web-* ` and `web-*` are
/// the same pattern and `"   "` is recognized as empty rather than
/// compiled into a glob that matches only three spaces.
fn nonempty(value: Option<&str>) -> Restriction<'_> {
    match value.map(str::trim) {
        None => Restriction::Absent,
        Some("") => Restriction::Empty,
        Some(trimmed) => Restriction::Present(trimmed),
    }
}

/// Validates one exact-match dimension, pushing an error and yielding
/// [`None`] if it is present but empty.
fn checked_exact(
    value: Option<&str>,
    locator: &str,
    message: &str,
    errors: &mut Vec<PolicyValidationError>,
) -> Option<String> {
    match nonempty(value) {
        Restriction::Absent => None,
        Restriction::Empty => {
            errors.push(PolicyValidationError::new(locator, message));
            None
        }
        Restriction::Present(trimmed) => Some(trimmed.to_owned()),
    }
}
