//! Explicitly expected changes, and the expectations that no longer
//! apply.
//!
//! An `expectations.yaml` is how a team says "yes, we know: the
//! candidate stack removes that init container on the `legacy-*`
//! fixtures, here is why, do not fail the build for it". Matching one
//! marks a change [`ClassifiedChange::expected`], which removes it from
//! the disposition calculation *without* changing its severity and
//! without hiding it from the report (Task 4.9 step 4). An expectation
//! that matches nothing becomes a [`StaleExpectation`].
//!
//! # The file
//!
//! ```yaml
//! apiVersion: admissionlab.io/v1alpha1
//! kind: Expectations
//! expectations:
//!   - id: istio-sidecar-injection
//!     fixtures: "web-*"
//!     kind: container_added
//!     selector:
//!       subject: istio-proxy
//!     reason: >-
//!       The candidate stack enables Istio sidecar injection ...
//! ```
//!
//! It carries `apiVersion`/`kind` for the same reason
//! `admissionlab.yaml` does, and pins `apiVersion` to
//! [`admissionlab_spec::model::API_VERSION`] rather than declaring a
//! second version line: the two files are written together, reviewed
//! together, and describe one contract, so they version together. Every
//! struct is `deny_unknown_fields` and `camelCase`, matching
//! `admissionlab_spec::model` exactly -- a misspelled `fixtues` is a
//! loud parse error naming the line, not a silently ignored key.
//!
//! # Two required human fields
//!
//! `id` must be non-empty and unique within the file; `reason` must be
//! non-empty. Neither is decoration. The `id` is the only handle a
//! [`StaleExpectation`] has back to the entry that produced it (and the
//! only stable name a report or a review comment can use), and
//! duplicates would make that handle ambiguous, so they are rejected
//! rather than silently first-wins. The `reason` is what makes an
//! expectations file reviewable at all: an entry suppressing a critical
//! change with no written justification is indistinguishable from
//! someone silencing a real regression.
//!
//! # Matching, exactly
//!
//! Changes are walked in [`PolicyResult::changes`] order -- the sorted
//! order `crate::evaluate` documents, so `ExpectationMatch::change_index`
//! indexes the list a reader is actually looking at. For each change,
//! expectations are scanned in **file declaration order** and the first
//! one that matches claims it; the scan then stops.
//!
//! Three consequences, all deliberate:
//!
//! - **One change satisfies at most one expectation.** Shared matching
//!   is not supported in Alpha (roadmap Task 4.9 step 2), and a claimed
//!   change is not offered to any later expectation.
//! - **One expectation may account for many changes.** An expectation
//!   is a statement about a *class* of change ("sidecar injection on
//!   every `web-*` fixture"), and requiring one entry per observed
//!   instance would force users to enumerate what they cannot predict --
//!   five fixtures injecting a sidecar would leave four unexplained
//!   critical changes and fail the build for a reason nobody wrote down.
//! - **Contested changes go to the earlier declaration, and the loser
//!   is not silent.** When two expectations both match one change, file
//!   order decides -- it is the one ordering the author controls
//!   exactly, and it is the same tiebreaker `crate::evaluate` uses for
//!   equally specific overrides, so there is one rule to learn rather
//!   than two. If the losing expectation then matches nothing else it
//!   becomes stale, with a reason that says its changes were already
//!   accounted for and names the expectation that took them, rather than
//!   the misleading "nothing matched".

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use admissionlab_diff::{SemanticChange, SemanticChangeKind};
use admissionlab_spec::model::API_VERSION;
use globset::{Glob, GlobMatcher};
use serde::Deserialize;

use crate::error::{ExpectationsError, PolicyValidationError};
use crate::evaluate::{ClassifiedChange, StaleExpectation};
use crate::selector::{ChangeSelector, CompiledSelector};

/// The only `kind` value [`load_expectations`] accepts.
pub const EXPECTATIONS_KIND: &str = "Expectations";

/// The root of an `expectations.yaml` file, exactly as written.
///
/// `api_version` and `kind` are plain `String`s for the same reason
/// `admissionlab_spec::LabSpec`'s are: rejecting the wrong value is a
/// semantic check, not a syntactic one, and the resulting message can
/// then name the expected value.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectationsSpec {
    /// Must equal [`admissionlab_spec::model::API_VERSION`].
    pub api_version: String,
    /// Must equal [`EXPECTATIONS_KIND`].
    pub kind: String,
    /// The expectations, in the order they were written -- which is
    /// load-bearing for matching (see this module's documentation).
    #[serde(default)]
    pub expectations: Vec<ExpectedChange>,
}

/// One change a team has explicitly accounted for.
///
/// `kind` is a real [`SemanticChangeKind`] rather than a `String`
/// (unlike `PolicyOverrideSpec::kind`, which is a `String` because
/// `admissionlab-spec` cannot see the kind names -- see that field's
/// documentation): this crate *can* see them, so an unknown kind is
/// rejected by `serde` itself, at the offending line, with the valid
/// names listed.
///
/// Derives no `Default`: every field but `selector` is required, and an
/// expectation with a blank `id` and `reason` is exactly what the
/// validation rules exist to refuse.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectedChange {
    /// A stable, file-unique handle for this entry. Appears in
    /// [`ExpectationMatch::expectation_id`] and in
    /// [`StaleExpectation::id`], so changing one renames it everywhere
    /// it has been referenced.
    pub id: String,
    /// A glob over fixture identifiers, required. `"*"` is the way to
    /// say "any fixture" -- spelled out rather than implied by omission,
    /// because an expectation that silently spans every fixture in the
    /// repository is not something to arrive at by leaving a line out.
    pub fixtures: String,
    /// The semantic kind this expectation accounts for.
    pub kind: SemanticChangeKind,
    /// Optional further narrowing (subject, object path, and a second
    /// fixture glob).
    ///
    /// Every dimension it sets is `AND`ed with `fixtures`, which always
    /// applies: setting `selector.fixtureGlob` as well narrows further
    /// rather than replacing `fixtures`, so the two must both match.
    #[serde(default)]
    pub selector: Option<ChangeSelector>,
    /// Why this change is expected -- required, non-empty, and written
    /// for the human reviewing the file. See this module's
    /// documentation.
    pub reason: String,
}

/// One expectation accounting for one change.
///
/// `change_index` indexes [`crate::PolicyResult::changes`] -- the
/// sorted, graded list -- not the caller's input slice, so a reader can
/// go straight from a match to the row it explains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectationMatch {
    /// The [`ExpectedChange::id`] that claimed the change.
    pub expectation_id: String,
    /// Which entry of [`crate::PolicyResult::changes`] it claimed.
    pub change_index: usize,
}

/// One expectation, checked and compiled.
#[derive(Debug, Clone)]
struct ResolvedExpectation {
    /// The entry's `id`, trimmed.
    id: String,
    /// The entry's `reason`, trimmed -- carried so a report can show
    /// why a change was expected without re-reading the file.
    reason: String,
    /// The kind this expectation accounts for.
    kind: SemanticChangeKind,
    /// The compiled `fixtures` glob, always present.
    fixtures: GlobMatcher,
    /// The compiled `selector`, or an unrestricted one when omitted.
    selector: CompiledSelector,
    /// A human phrase naming what this expectation was looking for, for
    /// [`StaleExpectation::reason`] -- built once at load time so the
    /// stale message and the matching rule cannot describe two different
    /// things.
    description: String,
}

impl ResolvedExpectation {
    /// Whether `change` is one this expectation accounts for.
    fn matches(&self, change: &SemanticChange) -> bool {
        change.kind == self.kind
            && self.fixtures.is_match(change.fixture_id.as_str())
            && self.selector.matches(change)
    }
}

/// A checked, compiled `expectations.yaml`.
///
/// Producing one is the only way to reach
/// [`crate::evaluate_with_expectations`], so a file with a duplicate id
/// or an unusable glob can never quietly account for nothing during a
/// real run.
#[derive(Debug, Clone)]
pub struct ResolvedExpectations {
    /// The expectations, in file declaration order.
    entries: Vec<ResolvedExpectation>,
}

impl ResolvedExpectations {
    /// No expectations at all -- what a lab with no `expectationsFile`
    /// evaluates against.
    ///
    /// Infallible, unlike [`load_expectations`], so the common case
    /// (no expectations file) needs no error handling.
    #[must_use]
    pub fn none() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// How many expectations this file declared.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the file declared no expectations at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The `(id, reason)` of every expectation, in declaration order --
    /// what a report needs to show alongside a matched change.
    #[must_use]
    pub fn descriptions(&self) -> Vec<(&str, &str)> {
        self.entries
            .iter()
            .map(|entry| (entry.id.as_str(), entry.reason.as_str()))
            .collect()
    }
}

/// What matching a run's changes against a file's expectations produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectationMatching {
    /// Every (expectation, change) pairing, ordered by `change_index`.
    pub matches: Vec<ExpectationMatch>,
    /// Every expectation that accounted for nothing, in declaration
    /// order.
    pub stale: Vec<StaleExpectation>,
}

/// Reads and checks an `expectations.yaml`.
///
/// # Errors
///
/// Returns [`ExpectationsError::Io`] if `path` cannot be read,
/// [`ExpectationsError::Parse`] if its contents are not a valid
/// expectations document (an unknown field, or an unknown semantic kind
/// -- `serde` names the line and lists the valid kinds itself), or
/// [`ExpectationsError::Validation`] listing every semantic problem: a
/// wrong `apiVersion`/`kind`, an empty or duplicated `id`, an empty
/// `reason`, an unusable `fixtures` glob, or an impossible selector
/// dimension.
pub fn load_expectations(path: &Path) -> Result<ResolvedExpectations, ExpectationsError> {
    let text = fs::read_to_string(path).map_err(|source| ExpectationsError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_expectations(&text, path)
}

/// Parses and checks an expectations document already in memory.
///
/// Split from [`load_expectations`] so the rules can be tested against
/// literal documents without a temporary file, and so a caller that
/// obtained the text some other way is not forced through the
/// filesystem. `path` is used only to build error messages.
///
/// # Errors
///
/// See [`load_expectations`]; this performs everything but the read.
pub fn parse_expectations(
    text: &str,
    path: &Path,
) -> Result<ResolvedExpectations, ExpectationsError> {
    let spec: ExpectationsSpec =
        serde_norway::from_str(text).map_err(|source| ExpectationsError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    resolve_expectations(&spec, path)
}

/// Checks and compiles a parsed expectations document.
///
/// Reports **every** problem at once, for the same reason
/// [`crate::validate_policy_spec`] does: a user who left three `reason`
/// fields blank should learn that in one run.
///
/// # Errors
///
/// Returns [`ExpectationsError::Validation`] listing every problem.
pub fn resolve_expectations(
    spec: &ExpectationsSpec,
    path: &Path,
) -> Result<ResolvedExpectations, ExpectationsError> {
    let mut problems = Vec::new();

    if spec.api_version != API_VERSION {
        problems.push(PolicyValidationError::new(
            "apiVersion",
            format_args!("must be {API_VERSION:?}, found {:?}", spec.api_version),
        ));
    }
    if spec.kind != EXPECTATIONS_KIND {
        problems.push(PolicyValidationError::new(
            "kind",
            format_args!("must be {EXPECTATIONS_KIND:?}, found {:?}", spec.kind),
        ));
    }

    // `BTreeMap` rather than a `HashSet`: it remembers where the first
    // occurrence was, so the duplicate's message can point at it.
    let mut seen_ids: BTreeMap<&str, usize> = BTreeMap::new();
    let mut entries = Vec::with_capacity(spec.expectations.len());

    for (index, entry) in spec.expectations.iter().enumerate() {
        let locator = format!("expectations[{index}]");

        let id = entry.id.trim();
        if id.is_empty() {
            problems.push(PolicyValidationError::new(
                format_args!("{locator}.id"),
                "must not be empty (a stale expectation is reported by id)",
            ));
        } else if let Some(first) = seen_ids.insert(id, index) {
            problems.push(PolicyValidationError::new(
                format_args!("{locator}.id"),
                format_args!("duplicate id {id:?}, already used by expectations[{first}]"),
            ));
        }

        let reason = entry.reason.trim();
        if reason.is_empty() {
            problems.push(PolicyValidationError::new(
                format_args!("{locator}.reason"),
                "must not be empty (an unexplained expectation is indistinguishable from \
                 silencing a regression)",
            ));
        }

        let fixtures_pattern = entry.fixtures.trim();
        let fixtures = if fixtures_pattern.is_empty() {
            problems.push(PolicyValidationError::new(
                format_args!("{locator}.fixtures"),
                "must not be empty (use \"*\" to expect this change on any fixture)",
            ));
            None
        } else {
            match Glob::new(fixtures_pattern) {
                Ok(glob) => Some(glob.compile_matcher()),
                Err(source) => {
                    problems.push(PolicyValidationError::new(
                        format_args!("{locator}.fixtures"),
                        format_args!("invalid glob pattern {fixtures_pattern:?}: {source}"),
                    ));
                    None
                }
            }
        };

        // The selector's own locator names the nested block, unlike a
        // policy override's (whose narrowing fields are flat alongside
        // `kind`), so an error points at the line the user wrote.
        let selector = match &entry.selector {
            None => Ok(CompiledSelector::unrestricted()),
            Some(selector) => CompiledSelector::compile(selector, &format!("{locator}.selector")),
        };

        match (fixtures, selector) {
            (Some(fixtures), Ok(selector)) if !id.is_empty() && !reason.is_empty() => {
                entries.push(ResolvedExpectation {
                    id: id.to_owned(),
                    reason: reason.to_owned(),
                    kind: entry.kind,
                    description: describe(entry.kind, fixtures_pattern, entry.selector.as_ref()),
                    fixtures,
                    selector,
                });
            }
            (_, selector) => {
                if let Err(selector_problems) = selector {
                    problems.extend(selector_problems);
                }
            }
        }
    }

    if problems.is_empty() {
        Ok(ResolvedExpectations { entries })
    } else {
        Err(ExpectationsError::Validation {
            path: path.to_path_buf(),
            problems,
        })
    }
}

/// Builds the phrase a [`StaleExpectation`] uses to name what the
/// expectation was looking for, in the same vocabulary the file used.
fn describe(kind: SemanticChangeKind, fixtures: &str, selector: Option<&ChangeSelector>) -> String {
    use std::fmt::Write as _;

    let mut description = format!(
        "no change of kind {} matched fixtures glob {fixtures:?}",
        kind.as_str()
    );
    if let Some(selector) = selector {
        // Writing into a `String` never fails, so the `Result` these
        // return carries no information; `let _ =` rather than
        // `.expect(...)` because there is no failure to describe.
        if let Some(glob) = selector.fixture_glob.as_deref() {
            let _ = write!(description, " and fixtures glob {:?}", glob.trim());
        }
        if let Some(subject) = selector.subject.as_deref() {
            let _ = write!(description, " with subject {:?}", subject.trim());
        }
        if let Some(object_path) = selector.object_path.as_deref() {
            let _ = write!(description, " at object path {:?}", object_path.trim());
        }
    }
    description
}

/// Matches `changes` against `expectations`, following the exact rule
/// this module documents.
///
/// `changes` must already be in [`crate::PolicyResult::changes`] order;
/// [`crate::evaluate_with_expectations`] is what guarantees that, and is
/// how callers should normally reach this.
#[must_use]
pub fn match_expectations(
    expectations: &ResolvedExpectations,
    changes: &[ClassifiedChange],
) -> ExpectationMatching {
    let mut matches = Vec::new();
    let mut claimed_by: Vec<Option<usize>> = vec![None; changes.len()];
    let mut claim_count = vec![0usize; expectations.entries.len()];

    for (change_index, classified) in changes.iter().enumerate() {
        // First declared match wins, and the scan stops: a change is
        // never offered to a second expectation.
        if let Some((entry_index, entry)) = expectations
            .entries
            .iter()
            .enumerate()
            .find(|(_, entry)| entry.matches(&classified.change))
        {
            claimed_by[change_index] = Some(entry_index);
            claim_count[entry_index] += 1;
            matches.push(ExpectationMatch {
                expectation_id: entry.id.clone(),
                change_index,
            });
        }
    }

    let stale = expectations
        .entries
        .iter()
        .enumerate()
        .filter(|(entry_index, _)| claim_count[*entry_index] == 0)
        .map(|(_, entry)| {
            // An expectation that claimed nothing may still have
            // *matched* something an earlier expectation took first.
            // Reporting that as "nothing matched" would send the reader
            // looking for a behavior change that did occur.
            let contested: Vec<&str> = changes
                .iter()
                .enumerate()
                .filter(|(_, classified)| entry.matches(&classified.change))
                .filter_map(|(change_index, _)| claimed_by[change_index])
                .map(|winner| expectations.entries[winner].id.as_str())
                .collect();

            let reason = if contested.is_empty() {
                entry.description.clone()
            } else {
                let mut winners: Vec<&str> = contested.clone();
                winners.dedup();
                format!(
                    "every matching change ({}) was already accounted for by an earlier \
                     expectation ({}); one change cannot satisfy two expectations",
                    contested.len(),
                    winners.join(", ")
                )
            };
            StaleExpectation {
                id: entry.id.clone(),
                reason,
            }
        })
        .collect();

    ExpectationMatching { matches, stale }
}
