//! Task 4.9 contract tests: what an `expectations.yaml` must contain to
//! be accepted, the exact matching rule, and the effect of a match on
//! the run's disposition.
//!
//! The load-time rules (unique `id`, non-empty `reason`) are asserted on
//! the rendered message text, not just on "an error happened": the
//! message is the entire user experience of a rejected file.
//!
//! Each test's doc comment names what would make it fail.

use std::path::{Path, PathBuf};

use admissionlab_core::FixtureId;
use admissionlab_diff::{SemanticChange, SemanticChangeKind};
use admissionlab_policy::{
    ExpectationMatch, ExpectationsError, PolicyDisposition, ResolvedExpectations, ResolvedPolicy,
    Severity, evaluate_with_expectations, load_expectations, match_expectations,
    parse_expectations, resolve_policy,
};
use admissionlab_spec::PolicySpec;

/// Path to `testdata/configs/expectations.yaml`, three levels above this
/// crate's own manifest directory.
fn testdata_expectations() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/configs/expectations.yaml")
}

/// A stand-in path for documents parsed from a literal; only ever used
/// to build error messages.
fn literal_path() -> &'static Path {
    Path::new("expectations.yaml")
}

/// A minimal [`SemanticChange`], as in `tests/evaluate.rs`.
fn change(
    kind: SemanticChangeKind,
    fixture_id: &str,
    object_path: Option<&str>,
    subject: Option<&str>,
) -> SemanticChange {
    SemanticChange {
        kind,
        fixture_id: FixtureId::parse(fixture_id).expect("test fixture id is valid"),
        object_path: object_path.map(str::to_owned),
        subject: subject.map(str::to_owned),
        baseline: None,
        candidate: None,
        origin: None,
    }
}

/// Parses an expectations document, panicking with the rejection message
/// if it was supposed to be valid.
fn parse(text: &str) -> ResolvedExpectations {
    parse_expectations(text, literal_path()).unwrap_or_else(|e| panic!("{e}"))
}

/// Returns the rendered validation problems of a document that must be
/// rejected, or panics if it was accepted.
fn problems(text: &str) -> Vec<String> {
    match parse_expectations(text, literal_path()) {
        Ok(_) => panic!("document was expected to be rejected"),
        Err(ExpectationsError::Validation { problems, .. }) => {
            problems.iter().map(ToString::to_string).collect()
        }
        Err(other) => panic!("expected a validation failure, got: {other}"),
    }
}

/// A document header plus `expectations:`, so each test writes only the
/// entries it cares about.
fn document(entries: &str) -> String {
    format!("apiVersion: admissionlab.io/v1alpha1\nkind: Expectations\nexpectations:\n{entries}")
}

/// Fails if the checked-in example file stops loading, stops exercising
/// both a plain glob and a selector, or loses an entry.
#[test]
fn the_checked_in_example_file_loads() {
    let expectations =
        load_expectations(&testdata_expectations()).expect("testdata expectations file loads");
    let described = expectations.descriptions();
    assert_eq!(
        described.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![
            "istio-sidecar-injection",
            "istio-proxy-run-as-user",
            "nightly-image-tag-drift"
        ]
    );
    assert!(
        described
            .iter()
            .all(|(_, reason)| reason.len() > 40 && !reason.trim().is_empty()),
        "every entry carries a real human reason: {described:#?}"
    );

    // Glob plus selector, and glob alone, both actually match something.
    let policy = ResolvedPolicy::permissive();
    let result = evaluate_with_expectations(
        &policy,
        &expectations,
        &[
            change(
                SemanticChangeKind::ContainerAdded,
                "web-frontend",
                None,
                Some("istio-proxy"),
            ),
            change(
                SemanticChangeKind::SecurityContextChanged,
                "web-frontend",
                Some("/spec/containers/1/securityContext/runAsUser"),
                Some("istio-proxy"),
            ),
            change(SemanticChangeKind::ImageChanged, "nightly-api", None, None),
        ],
    );
    assert!(result.changes.iter().all(|graded| graded.expected));
    assert!(result.stale_expectations.is_empty());
    assert_eq!(result.disposition, PolicyDisposition::Pass);
}

/// Fails if a missing or blank `reason` stops being rejected -- the rule
/// that keeps an expectations file reviewable.
#[test]
fn a_blank_reason_is_rejected() {
    let rendered = problems(&document(
        "  - id: a\n    fixtures: '*'\n    kind: image_changed\n    reason: '   '\n",
    ));
    assert_eq!(rendered.len(), 1, "{rendered:#?}");
    assert!(
        rendered[0].starts_with("expectations[0].reason: must not be empty"),
        "{rendered:#?}"
    );

    // An omitted `reason` is a parse error, not a validation one: the
    // field is required at the type level.
    let error = parse_expectations(
        &document("  - id: a\n    fixtures: '*'\n    kind: image_changed\n"),
        literal_path(),
    )
    .expect_err("a missing reason is rejected");
    assert!(
        matches!(error, ExpectationsError::Parse { .. }),
        "{error:#?}"
    );
    assert!(error.to_string().contains("reason"), "{error}");
}

/// Fails if a blank or duplicated `id` stops being rejected, or if the
/// duplicate's message stops naming the entry it collides with.
#[test]
fn ids_must_be_present_and_unique() {
    let rendered = problems(&document(
        "  - id: '  '\n    fixtures: '*'\n    kind: image_changed\n    reason: why\n\
         \x20 - id: shared\n    fixtures: 'a-*'\n    kind: image_changed\n    reason: why\n\
         \x20 - id: shared\n    fixtures: 'b-*'\n    kind: image_changed\n    reason: why\n",
    ));
    assert_eq!(rendered.len(), 2, "{rendered:#?}");
    assert!(
        rendered[0].starts_with("expectations[0].id: must not be empty"),
        "{rendered:#?}"
    );
    assert_eq!(
        rendered[1],
        "expectations[2].id: duplicate id \"shared\", already used by expectations[1]"
    );
}

/// Fails if every problem in a file stops being reported at once, or if
/// the `apiVersion`/`kind` header stops being checked.
#[test]
fn every_problem_is_reported_at_once() {
    let rendered = problems(
        "apiVersion: admissionlab.io/v1beta9\n\
         kind: Expectation\n\
         expectations:\n  \
         - id: bad-glob\n    \
           fixtures: 'web-['\n    \
           kind: image_changed\n    \
           reason: why\n  \
         - id: impossible-selector\n    \
           fixtures: ''\n    \
           kind: image_changed\n    \
           selector:\n      \
             subject: ''\n    \
           reason: why\n",
    );
    assert_eq!(
        rendered,
        vec![
            "apiVersion: must be \"admissionlab.io/v1alpha1\", found \"admissionlab.io/v1beta9\"",
            "kind: must be \"Expectations\", found \"Expectation\"",
            "expectations[0].fixtures: invalid glob pattern \"web-[\": error parsing glob \
             'web-[': unclosed character class; missing ']'",
            "expectations[1].fixtures: must not be empty (use \"*\" to expect this change on \
             any fixture)",
            "expectations[1].selector.subject: must not be empty (omit it to match every subject)",
        ]
    );
}

/// Fails if an unknown semantic kind stops being rejected at the line it
/// appears on, or if `serde` stops listing the valid names.
#[test]
fn an_unknown_kind_is_rejected_by_name() {
    let error = parse_expectations(
        &document("  - id: a\n    fixtures: '*'\n    kind: image_change\n    reason: why\n"),
        literal_path(),
    )
    .expect_err("an unknown kind is rejected");
    let rendered = error.to_string();
    assert!(rendered.contains("image_change"), "{rendered}");
    assert!(rendered.contains("image_changed"), "{rendered}");
}

/// Fails if a misspelled field stops being a loud error -- the same
/// strictness `admissionlab.yaml` has.
#[test]
fn an_unknown_field_is_rejected() {
    let error = parse_expectations(
        &document("  - id: a\n    fixtues: '*'\n    kind: image_changed\n    reason: why\n"),
        literal_path(),
    )
    .expect_err("an unknown field is rejected");
    assert!(error.to_string().contains("fixtues"), "{error}");
}

/// Fails if a missing expectations file stops producing an I/O error
/// naming the path.
#[test]
fn a_missing_file_names_the_path() {
    let error =
        load_expectations(Path::new("/nonexistent/expectations.yaml")).expect_err("no such file");
    assert!(matches!(error, ExpectationsError::Io { .. }), "{error:#?}");
    assert!(
        error.to_string().contains("/nonexistent/expectations.yaml"),
        "{error}"
    );
}

/// Fails if an expected critical change starts failing the run, or stops
/// being reported -- Task 4.9 step 4's exact requirement.
#[test]
fn an_expected_critical_change_passes_and_stays_visible() {
    let expectations = parse(&document(
        "  - id: legacy-init-removal\n    \
           fixtures: 'legacy-*'\n    \
           kind: init_container_removed\n    \
           reason: The candidate drops the deprecated migration init container.\n",
    ));
    let result = evaluate_with_expectations(
        &ResolvedPolicy::permissive(),
        &expectations,
        &[change(
            SemanticChangeKind::InitContainerRemoved,
            "legacy-api",
            None,
            Some("db-migrate"),
        )],
    );

    assert_eq!(result.disposition, PolicyDisposition::Pass);
    // Still listed, still critical: expected means "not counted", never
    // "hidden" or "downgraded".
    assert_eq!(result.changes.len(), 1);
    assert_eq!(result.changes[0].severity, Severity::Critical);
    assert!(result.changes[0].expected);
    assert!(result.stale_expectations.is_empty());
}

/// Fails if an unexpected critical change alongside an expected one
/// stops failing the run.
#[test]
fn an_unexpected_critical_change_still_fails() {
    let expectations = parse(&document(
        "  - id: legacy-init-removal\n    \
           fixtures: 'legacy-*'\n    \
           kind: init_container_removed\n    \
           reason: The candidate drops the deprecated migration init container.\n",
    ));
    let result = evaluate_with_expectations(
        &ResolvedPolicy::permissive(),
        &expectations,
        &[
            change(
                SemanticChangeKind::InitContainerRemoved,
                "legacy-api",
                None,
                Some("db-migrate"),
            ),
            // Same fixture, a different kind the file says nothing
            // about.
            change(
                SemanticChangeKind::ContainerRemoved,
                "legacy-api",
                None,
                Some("app"),
            ),
        ],
    );

    assert_eq!(result.disposition, PolicyDisposition::Fail);
    let flags: Vec<bool> = result
        .changes
        .iter()
        .map(|graded| graded.expected)
        .collect();
    // Sorted by kind wire name: `container_removed` before
    // `init_container_removed`.
    assert_eq!(flags, vec![false, true]);
}

/// Fails if an expectation stops covering every instance of the class it
/// describes -- which would force users to enumerate what they cannot
/// predict.
#[test]
fn one_expectation_accounts_for_every_change_it_matches() {
    let expectations = parse(&document(
        "  - id: sidecar\n    \
           fixtures: 'web-*'\n    \
           kind: container_added\n    \
           selector:\n      \
             subject: istio-proxy\n    \
           reason: Sidecar injection is enabled on the candidate.\n",
    ));
    let changes = [
        change(
            SemanticChangeKind::ContainerAdded,
            "web-api",
            None,
            Some("istio-proxy"),
        ),
        change(
            SemanticChangeKind::ContainerAdded,
            "web-frontend",
            None,
            Some("istio-proxy"),
        ),
    ];
    let result = evaluate_with_expectations(&ResolvedPolicy::permissive(), &expectations, &changes);
    assert!(result.changes.iter().all(|graded| graded.expected));
    assert!(result.stale_expectations.is_empty());
    assert_eq!(result.disposition, PolicyDisposition::Pass);

    let matching = match_expectations(&expectations, &result.changes);
    assert_eq!(
        matching.matches,
        vec![
            ExpectationMatch {
                expectation_id: "sidecar".to_owned(),
                change_index: 0
            },
            ExpectationMatch {
                expectation_id: "sidecar".to_owned(),
                change_index: 1
            },
        ]
    );
}

/// Fails if a contested change stops going to the earlier declaration,
/// or if the loser stops being reported as stale with an honest reason.
///
/// This is the "one change cannot satisfy two expectations" rule: both
/// entries match the single change, the first declared claims it, and
/// the second must not silently disappear.
#[test]
fn a_contested_change_goes_to_the_earlier_expectation() {
    let entries = [
        "  - id: broad\n    fixtures: '*'\n    kind: container_added\n    reason: broad reason\n",
        "  - id: narrow\n    fixtures: 'web-*'\n    kind: container_added\n    \
         selector:\n      subject: istio-proxy\n    reason: narrow reason\n",
    ];
    let single = [change(
        SemanticChangeKind::ContainerAdded,
        "web-api",
        None,
        Some("istio-proxy"),
    )];

    // Declaration order decides, not specificity: unlike policy
    // overrides, expectations are claims about *instances*, so the
    // narrower entry does not outrank the earlier one.
    let broad_first = parse(&document(&format!("{}{}", entries[0], entries[1])));
    let result = evaluate_with_expectations(&ResolvedPolicy::permissive(), &broad_first, &single);
    assert!(result.changes[0].expected);
    assert_eq!(result.stale_expectations.len(), 1);
    assert_eq!(result.stale_expectations[0].id, "narrow");
    assert_eq!(
        result.stale_expectations[0].reason,
        "every matching change (1) was already accounted for by an earlier expectation \
         (broad); one change cannot satisfy two expectations"
    );

    let narrow_first = parse(&document(&format!("{}{}", entries[1], entries[0])));
    let result = evaluate_with_expectations(&ResolvedPolicy::permissive(), &narrow_first, &single);
    assert_eq!(result.stale_expectations.len(), 1);
    assert_eq!(result.stale_expectations[0].id, "broad");

    // Either way the change is accounted for exactly once and the run
    // passes: a contested change is not a failure, it is a tidiness
    // signal.
    assert!(result.changes[0].expected);
    assert_eq!(result.disposition, PolicyDisposition::Pass);
}

/// Fails if an expectation that matched nothing at all stops naming what
/// it was looking for.
#[test]
fn an_unmatched_expectation_names_what_did_not_happen() {
    let expectations = parse(&document(
        "  - id: no-such-change\n    \
           fixtures: 'legacy-*'\n    \
           kind: volume_removed\n    \
           selector:\n      \
             subject: data\n      \
             objectPath: /spec/volumes/0\n    \
           reason: The candidate was supposed to drop the legacy data volume.\n",
    ));
    let result = evaluate_with_expectations(
        &ResolvedPolicy::permissive(),
        &expectations,
        &[change(
            SemanticChangeKind::ImageChanged,
            "legacy-api",
            None,
            None,
        )],
    );

    assert_eq!(result.stale_expectations.len(), 1);
    assert_eq!(result.stale_expectations[0].id, "no-such-change");
    assert_eq!(
        result.stale_expectations[0].reason,
        "no change of kind volume_removed matched fixtures glob \"legacy-*\" with subject \
         \"data\" at object path \"/spec/volumes/0\""
    );

    // A stale expectation is a configuration signal, never a verdict on
    // the compared stacks: the run's only change is informational, so it
    // still passes.
    assert_eq!(result.disposition, PolicyDisposition::Pass);
}

/// Fails if `fixtures` and `selector.fixtureGlob` stop both applying,
/// which would let one silently replace the other.
#[test]
fn both_fixture_globs_must_match() {
    let expectations = parse(&document(
        "  - id: both\n    \
           fixtures: 'web-*'\n    \
           kind: image_changed\n    \
           selector:\n      \
             fixtureGlob: '*-canary'\n    \
           reason: Only the canary web fixtures move their image tag.\n",
    ));
    let result = evaluate_with_expectations(
        &ResolvedPolicy::permissive(),
        &expectations,
        &[
            change(
                SemanticChangeKind::ImageChanged,
                "web-api-canary",
                None,
                None,
            ),
            change(
                SemanticChangeKind::ImageChanged,
                "web-api-stable",
                None,
                None,
            ),
            change(SemanticChangeKind::ImageChanged, "batch-canary", None, None),
        ],
    );
    let flags: Vec<(&str, bool)> = result
        .changes
        .iter()
        .map(|graded| (graded.change.fixture_id.as_str(), graded.expected))
        .collect();
    assert_eq!(
        flags,
        vec![
            ("batch-canary", false),
            ("web-api-canary", true),
            ("web-api-stable", false),
        ]
    );
}

/// Fails if expectations stop interacting correctly with a policy
/// override or `failOn`: severity is decided first, and expectation
/// matching never consults it.
#[test]
fn expectations_do_not_change_how_a_change_is_graded() {
    let spec: PolicySpec = serde_norway::from_str("failOn: [image_changed]\n").unwrap();
    let policy = resolve_policy(&spec).expect("policy resolves");
    let expectations = parse(&document(
        "  - id: nightly-drift\n    \
           fixtures: 'nightly-*'\n    \
           kind: image_changed\n    \
           reason: Nightly fixtures pin a floating tag by design.\n",
    ));
    let result = evaluate_with_expectations(
        &policy,
        &expectations,
        &[change(
            SemanticChangeKind::ImageChanged,
            "nightly-api",
            None,
            None,
        )],
    );

    // Escalated to critical by `failOn`, expected by the file: graded
    // critical, reported, and not a failure.
    assert_eq!(result.changes[0].severity, Severity::Critical);
    assert!(result.changes[0].expected);
    assert_eq!(result.disposition, PolicyDisposition::Pass);
}

/// Fails if evaluating against no expectations stops being equivalent to
/// evaluating with an empty file.
#[test]
fn an_empty_expectations_file_matches_nothing() {
    let empty = parse(&document(""));
    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);

    let changes = [change(
        SemanticChangeKind::ContainerRemoved,
        "web",
        None,
        None,
    )];
    let with_empty_file =
        evaluate_with_expectations(&ResolvedPolicy::permissive(), &empty, &changes);
    let with_none = evaluate_with_expectations(
        &ResolvedPolicy::permissive(),
        &ResolvedExpectations::none(),
        &changes,
    );
    assert_eq!(with_empty_file, with_none);
    assert_eq!(with_empty_file.disposition, PolicyDisposition::Fail);
}
