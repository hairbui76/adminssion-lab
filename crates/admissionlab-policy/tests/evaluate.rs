//! Task 4.8 contract tests: the frozen default-severity table, selector
//! -scoped overrides and their precedence, load-time rejection of
//! unknown names, and the pass/warn/fail rule.
//!
//! Two things here are a *public* contract and are asserted on
//! serialized text rather than on Rust values: the seventeen default
//! severities (people read them out of the product documentation and
//! write `policy.overrides` against them) and the `severity`/
//! `disposition` wire names (people branch CI jobs on them).
//!
//! Each test's doc comment names what would make it fail.

use admissionlab_core::FixtureId;
use admissionlab_diff::{SemanticChange, SemanticChangeKind};
use admissionlab_policy::{
    ALL_KINDS, ChangeSelector, CompiledSelector, PolicyDisposition, ResolvedPolicy, Severity,
    default_severity, evaluate, kind_from_name, kind_index, resolve_policy, validate_policy_spec,
};
use admissionlab_spec::PolicySpec;

/// The exact table `ROADMAP.md` Task 4.8 step 1 specifies, transcribed
/// from the roadmap's own seventeen rows by wire name.
///
/// Written as `(&str, Severity)` rather than `(SemanticChangeKind,
/// Severity)` on purpose: transcribing the roadmap's *strings* means a
/// variant renamed on either side is caught here, where a transcription
/// of Rust identifiers would silently follow the rename.
const ROADMAP_TABLE: [(&str, Severity); 17] = [
    ("newly_denied", Severity::Critical),
    ("newly_allowed", Severity::Critical),
    ("container_added", Severity::Warning),
    ("container_removed", Severity::Critical),
    ("init_container_added", Severity::Warning),
    ("init_container_removed", Severity::Critical),
    ("volume_added", Severity::Warning),
    ("volume_removed", Severity::Critical),
    ("volume_mount_changed", Severity::Warning),
    ("environment_changed", Severity::Warning),
    ("image_changed", Severity::Info),
    ("service_account_changed", Severity::Critical),
    ("security_context_changed", Severity::Critical),
    ("resource_requirement_changed", Severity::Warning),
    ("webhook_failed", Severity::Critical),
    ("webhook_invocation_changed", Severity::Warning),
    ("webhook_latency_changed", Severity::Warning),
];

/// Builds a fixture identifier, panicking on an invalid one (a test
/// typo, never a runtime condition).
fn fixture(id: &str) -> FixtureId {
    FixtureId::parse(id).expect("test fixture id is valid")
}

/// A minimal [`SemanticChange`] with no values and no attribution --
/// enough to grade, which is all these tests need.
fn change(
    kind: SemanticChangeKind,
    fixture_id: &str,
    object_path: Option<&str>,
    subject: Option<&str>,
) -> SemanticChange {
    SemanticChange {
        kind,
        fixture_id: fixture(fixture_id),
        object_path: object_path.map(str::to_owned),
        subject: subject.map(str::to_owned),
        baseline: None,
        candidate: None,
        origin: None,
    }
}

/// Parses a `policy` section exactly as it would appear inside an
/// `admissionlab.yaml`, so these tests exercise the same strict,
/// `deny_unknown_fields` deserialization a real configuration file goes
/// through rather than a hand-built struct.
fn policy_spec(yaml: &str) -> PolicySpec {
    serde_norway::from_str(yaml).expect("test policy section parses")
}

/// Fails if any of the seventeen Alpha default severities drifts from
/// the roadmap's table, or if a kind's name changes.
#[test]
fn default_severity_matches_the_roadmap_table() {
    for (name, expected) in ROADMAP_TABLE {
        let kind = kind_from_name(name).unwrap_or_else(|| panic!("{name:?} is a known kind"));
        assert_eq!(
            default_severity(kind),
            expected,
            "default severity for {name:?}"
        );
    }
}

/// Fails if the table above stops covering every kind -- for example if
/// an eighteenth `SemanticChangeKind` is added and graded but never
/// transcribed here.
#[test]
fn the_roadmap_table_covers_every_kind_exactly_once() {
    assert_eq!(ROADMAP_TABLE.len(), ALL_KINDS.len());
    for (index, kind) in ALL_KINDS.into_iter().enumerate() {
        assert_eq!(
            kind.as_str(),
            ROADMAP_TABLE[index].0,
            "ALL_KINDS[{index}] and the roadmap table disagree"
        );
    }
}

/// Fails if `ALL_KINDS` stops being complete or stops being in the order
/// the compiler-checked `classify` match assigns.
///
/// `kind_index` comes from an exhaustive `match`, so every variant has
/// an index; asserting the indices are exactly `0..17` in array order
/// makes "the array lists every variant, once, in this order" a checked
/// claim rather than a reviewed one.
#[test]
fn all_kinds_agrees_with_the_compiler_checked_index() {
    for (index, kind) in ALL_KINDS.into_iter().enumerate() {
        assert_eq!(kind_index(kind), index, "index of {}", kind.as_str());
    }
}

/// Fails if a severity's or disposition's JSON name changes -- both are
/// consumed by report readers and CI jobs outside this repository.
#[test]
fn wire_names_are_pinned() {
    assert_eq!(serde_json::to_string(&Severity::Info).unwrap(), "\"info\"");
    assert_eq!(
        serde_json::to_string(&Severity::Warning).unwrap(),
        "\"warning\""
    );
    assert_eq!(
        serde_json::to_string(&Severity::Critical).unwrap(),
        "\"critical\""
    );
    for severity in Severity::ALL {
        assert_eq!(
            serde_json::to_string(&severity).unwrap(),
            format!("\"{}\"", severity.as_str())
        );
    }

    for disposition in [
        PolicyDisposition::Pass,
        PolicyDisposition::Warn,
        PolicyDisposition::Fail,
    ] {
        assert_eq!(
            serde_json::to_string(&disposition).unwrap(),
            format!("\"{}\"", disposition.as_str())
        );
    }
    assert_eq!(PolicyDisposition::Fail.as_str(), "fail");
}

/// Fails if severity stops ordering weakest-first, which
/// `disposition_of`'s `max()` depends on.
#[test]
fn severity_orders_weakest_first() {
    assert!(Severity::Info < Severity::Warning);
    assert!(Severity::Warning < Severity::Critical);
}

/// Fails if a near-miss or differently-cased name is silently accepted,
/// which would make a typo'd `policy.failOn` entry match nothing forever
/// instead of being rejected.
#[test]
fn name_lookups_are_exact() {
    assert_eq!(
        kind_from_name("image_changed"),
        Some(SemanticChangeKind::ImageChanged)
    );
    assert_eq!(
        kind_from_name("  image_changed  "),
        kind_from_name("image_changed")
    );
    assert_eq!(kind_from_name("image_change"), None);
    assert_eq!(kind_from_name("ImageChanged"), None);
    assert_eq!(kind_from_name(""), None);

    assert_eq!(Severity::from_name("warning"), Some(Severity::Warning));
    assert_eq!(Severity::from_name(" warning "), Some(Severity::Warning));
    assert_eq!(Severity::from_name("Warning"), None);
    assert_eq!(Severity::from_name("fatal"), None);
}

/// Fails if a run with no policy stops grading purely by the default
/// table, or if the pass/warn/fail rule changes.
#[test]
fn disposition_follows_the_worst_unexpected_change() {
    let policy = ResolvedPolicy::permissive();

    let info_only = evaluate(
        &policy,
        &[change(
            SemanticChangeKind::ImageChanged,
            "web",
            Some("/spec/containers/0/image"),
            Some("app"),
        )],
    );
    assert_eq!(info_only.disposition, PolicyDisposition::Pass);
    assert_eq!(info_only.changes.len(), 1);
    assert_eq!(info_only.changes[0].severity, Severity::Info);

    let warning = evaluate(
        &policy,
        &[
            change(SemanticChangeKind::ImageChanged, "web", None, None),
            change(SemanticChangeKind::ContainerAdded, "web", None, None),
        ],
    );
    assert_eq!(warning.disposition, PolicyDisposition::Warn);

    let critical = evaluate(
        &policy,
        &[
            change(SemanticChangeKind::ContainerAdded, "web", None, None),
            change(SemanticChangeKind::ContainerRemoved, "web", None, None),
        ],
    );
    assert_eq!(critical.disposition, PolicyDisposition::Fail);

    let empty = evaluate(&policy, &[]);
    assert_eq!(empty.disposition, PolicyDisposition::Pass);
    assert!(empty.changes.is_empty());
    assert!(empty.stale_expectations.is_empty());
}

/// Fails if Task 4.8's expectation seam stops being wired: every change
/// must be reported as unexpected until Task 4.9 matches them.
#[test]
fn every_change_is_unexpected_before_expectations_exist() {
    let result = evaluate(
        &ResolvedPolicy::permissive(),
        &[change(
            SemanticChangeKind::ContainerRemoved,
            "web",
            None,
            None,
        )],
    );
    assert!(result.changes.iter().all(|graded| !graded.expected));
    assert!(result.stale_expectations.is_empty());
}

/// Fails if `failOn` stops escalating a kind to critical, or starts
/// *lowering* the severity of kinds it does not name.
#[test]
fn fail_on_escalates_and_never_downgrades() {
    let spec = policy_spec("failOn: [image_changed]\n");
    let policy = resolve_policy(&spec).expect("policy resolves");

    let escalated = evaluate(
        &policy,
        &[change(SemanticChangeKind::ImageChanged, "web", None, None)],
    );
    assert_eq!(escalated.changes[0].severity, Severity::Critical);
    assert_eq!(escalated.disposition, PolicyDisposition::Fail);

    // A kind absent from `failOn` keeps its default severity; naming one
    // kind must not turn the policy into an allow-list for the rest.
    let untouched = evaluate(
        &policy,
        &[change(
            SemanticChangeKind::ContainerRemoved,
            "web",
            None,
            None,
        )],
    );
    assert_eq!(untouched.changes[0].severity, Severity::Critical);
}

/// Fails if an override stops narrowing by fixture glob, or starts
/// applying to fixtures outside it.
#[test]
fn overrides_narrow_by_fixture_glob() {
    let spec = policy_spec(
        "overrides:\n  \
         - kind: container_added\n    \
           fixtures: 'web-*'\n    \
           severity: info\n",
    );
    let policy = resolve_policy(&spec).expect("policy resolves");

    let result = evaluate(
        &policy,
        &[
            change(
                SemanticChangeKind::ContainerAdded,
                "web-frontend",
                None,
                None,
            ),
            change(SemanticChangeKind::ContainerAdded, "batch-job", None, None),
        ],
    );
    let by_fixture: Vec<(&str, Severity)> = result
        .changes
        .iter()
        .map(|graded| (graded.change.fixture_id.as_str(), graded.severity))
        .collect();
    assert_eq!(
        by_fixture,
        vec![
            ("batch-job", Severity::Warning),
            ("web-frontend", Severity::Info)
        ]
    );
    assert_eq!(result.disposition, PolicyDisposition::Warn);
}

/// Fails if an override's `subject`/`path` stop being exact matches, or
/// if a change with no subject/path of its own starts matching a
/// selector that names one.
#[test]
fn overrides_match_subject_and_path_exactly() {
    let spec = policy_spec(
        "overrides:\n  \
         - kind: security_context_changed\n    \
           subject: sidecar\n    \
           path: /spec/containers/1/securityContext/runAsNonRoot\n    \
           severity: info\n",
    );
    let policy = resolve_policy(&spec).expect("policy resolves");

    let matching = change(
        SemanticChangeKind::SecurityContextChanged,
        "web",
        Some("/spec/containers/1/securityContext/runAsNonRoot"),
        Some("sidecar"),
    );
    // Same subject, a path one segment deeper: exact matching means this
    // is a different location, not a covered one.
    let deeper = change(
        SemanticChangeKind::SecurityContextChanged,
        "web",
        Some("/spec/containers/1/securityContext/runAsNonRoot/extra"),
        Some("sidecar"),
    );
    // No subject and no path at all: never matches a selector naming
    // either.
    let unscoped = change(
        SemanticChangeKind::SecurityContextChanged,
        "web",
        None,
        None,
    );

    let result = evaluate(&policy, &[matching, deeper, unscoped]);
    let severities: Vec<Severity> = result
        .changes
        .iter()
        .map(|graded| graded.severity)
        .collect();
    // Sorted: `None` path first, then the two `Some` paths
    // lexicographically.
    assert_eq!(
        severities,
        vec![Severity::Critical, Severity::Info, Severity::Critical]
    );
    assert_eq!(result.disposition, PolicyDisposition::Fail);
}

/// Fails if a broader override starts shadowing a narrower one, which
/// would make the narrower entry dead configuration.
#[test]
fn the_most_specific_override_wins() {
    let spec = policy_spec(
        "overrides:\n  \
         - kind: container_added\n    \
           fixtures: '*'\n    \
           subject: istio-proxy\n    \
           severity: info\n  \
         - kind: container_added\n    \
           severity: critical\n",
    );
    let policy = resolve_policy(&spec).expect("policy resolves");

    // The two-dimension override wins over the later, unrestricted one.
    let scoped = evaluate(
        &policy,
        &[change(
            SemanticChangeKind::ContainerAdded,
            "web",
            None,
            Some("istio-proxy"),
        )],
    );
    assert_eq!(scoped.changes[0].severity, Severity::Info);

    // A change the narrow override does not match still gets the broad
    // one.
    let unscoped = evaluate(
        &policy,
        &[change(
            SemanticChangeKind::ContainerAdded,
            "web",
            None,
            Some("linkerd-proxy"),
        )],
    );
    assert_eq!(unscoped.changes[0].severity, Severity::Critical);
}

/// Fails if equally specific overrides stop resolving to the
/// last-declared one -- the documented tiebreaker, and the only thing
/// making the rule total.
#[test]
fn equally_specific_overrides_tie_break_on_declaration_order() {
    // Both restrict exactly one dimension, and both match the change, so
    // specificity cannot separate them.
    let spec = policy_spec(
        "overrides:\n  \
         - kind: container_added\n    \
           fixtures: 'web-*'\n    \
           severity: info\n  \
         - kind: container_added\n    \
           subject: istio-proxy\n    \
           severity: critical\n",
    );
    let policy = resolve_policy(&spec).expect("policy resolves");
    let result = evaluate(
        &policy,
        &[change(
            SemanticChangeKind::ContainerAdded,
            "web-frontend",
            None,
            Some("istio-proxy"),
        )],
    );
    assert_eq!(result.changes[0].severity, Severity::Critical);

    // Reversing the file's order reverses the winner, and nothing else.
    let reversed = policy_spec(
        "overrides:\n  \
         - kind: container_added\n    \
           subject: istio-proxy\n    \
           severity: critical\n  \
         - kind: container_added\n    \
           fixtures: 'web-*'\n    \
           severity: info\n",
    );
    let reversed = resolve_policy(&reversed).expect("policy resolves");
    let result = evaluate(
        &reversed,
        &[change(
            SemanticChangeKind::ContainerAdded,
            "web-frontend",
            None,
            Some("istio-proxy"),
        )],
    );
    assert_eq!(result.changes[0].severity, Severity::Info);
}

/// Fails if `failOn` starts beating a matching override, which would
/// leave a user with no way to make a targeted exception.
#[test]
fn an_override_beats_fail_on_for_the_same_kind() {
    let spec = policy_spec(
        "failOn: [image_changed]\n\
         overrides:\n  \
         - kind: image_changed\n    \
           fixtures: 'nightly-*'\n    \
           severity: info\n",
    );
    let policy = resolve_policy(&spec).expect("policy resolves");
    let result = evaluate(
        &policy,
        &[
            change(
                SemanticChangeKind::ImageChanged,
                "nightly-build",
                None,
                None,
            ),
            change(SemanticChangeKind::ImageChanged, "release", None, None),
        ],
    );
    let graded: Vec<(&str, Severity)> = result
        .changes
        .iter()
        .map(|entry| (entry.change.fixture_id.as_str(), entry.severity))
        .collect();
    assert_eq!(
        graded,
        vec![
            ("nightly-build", Severity::Info),
            ("release", Severity::Critical)
        ]
    );
    assert_eq!(result.disposition, PolicyDisposition::Fail);
}

/// Fails if the documented output ordering changes: fixture id, then
/// kind wire name, then object path (`None` first), then subject, with
/// input order preserved for full ties.
#[test]
fn changes_are_ordered_deterministically() {
    let input = vec![
        change(
            SemanticChangeKind::VolumeAdded,
            "web",
            Some("/spec/volumes/1"),
            None,
        ),
        change(
            SemanticChangeKind::ImageChanged,
            "web",
            Some("/a"),
            Some("b"),
        ),
        change(SemanticChangeKind::ImageChanged, "web", None, None),
        change(SemanticChangeKind::ImageChanged, "api", Some("/z"), None),
        // Two entries with an identical sort key: input order must hold.
        change(
            SemanticChangeKind::ImageChanged,
            "web",
            Some("/a"),
            Some("b"),
        ),
    ];
    let result = evaluate(&ResolvedPolicy::permissive(), &input);
    let order: Vec<(&str, &str, Option<&str>)> = result
        .changes
        .iter()
        .map(|graded| {
            (
                graded.change.fixture_id.as_str(),
                graded.change.kind.as_str(),
                graded.change.object_path.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        order,
        vec![
            ("api", "image_changed", Some("/z")),
            ("web", "image_changed", None),
            ("web", "image_changed", Some("/a")),
            ("web", "image_changed", Some("/a")),
            ("web", "volume_added", Some("/spec/volumes/1")),
        ]
    );

    // Evaluating the same input twice is byte-identical (Global
    // Constraint 7).
    let again = evaluate(&ResolvedPolicy::permissive(), &input);
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        serde_json::to_string(&again).unwrap()
    );
}

/// Fails if unknown or impossible names stop being rejected, or stop
/// being reported all at once.
///
/// The whole check runs against a parsed configuration section and
/// touches no cluster, kubeconfig, or subprocess -- which is what makes
/// it usable at load time, before `admissionlab-core` creates anything.
#[test]
fn unknown_and_impossible_names_are_rejected_at_load_time() {
    let spec = policy_spec(
        "failOn: [image_change, newly_denied]\n\
         overrides:\n  \
         - kind: containre_added\n    \
           severity: Critical\n  \
         - kind: container_added\n    \
           fixtures: 'web-['\n    \
           severity: info\n  \
         - kind: container_added\n    \
           subject: '   '\n    \
           path: ''\n    \
           severity: info\n",
    );
    let errors = validate_policy_spec(&spec);
    let rendered: Vec<String> = errors.iter().map(ToString::to_string).collect();

    // Every problem at once, never just the first -- including both
    // impossible dimensions of the same override.
    assert_eq!(errors.len(), 6, "expected six problems, got {rendered:#?}");

    // `failOn` is a `BTreeSet`, so `image_change` sorts first; the valid
    // `newly_denied` produces nothing.
    assert!(
        rendered[0].starts_with("policy.failOn[0]: unknown semantic change kind \"image_change\";"),
        "{rendered:#?}"
    );
    assert!(
        rendered[0].contains("image_changed"),
        "the message lists the valid names: {rendered:#?}"
    );

    assert!(
        rendered[1].starts_with(
            "policy.overrides[0].kind: unknown semantic change kind \"containre_added\";"
        ),
        "{rendered:#?}"
    );
    // Case-sensitive: `Critical` is not the wire name.
    assert_eq!(
        rendered[2],
        "policy.overrides[0].severity: unknown severity \"Critical\"; \
         expected one of: info, warning, critical"
    );
    assert!(
        rendered[3].starts_with("policy.overrides[1].fixtures: invalid glob pattern \"web-[\":"),
        "{rendered:#?}"
    );
    assert_eq!(
        rendered[4],
        "policy.overrides[2].subject: must not be empty (omit it to match every subject)"
    );
    assert_eq!(
        rendered[5],
        "policy.overrides[2].path: must not be empty (omit it to match every object path)"
    );

    // And the compiling entry point refuses the same policy rather than
    // silently dropping the bad entries.
    let error = resolve_policy(&spec).expect_err("invalid policy must not resolve");
    assert_eq!(error.as_slice(), errors.as_slice());
    assert!(
        error
            .to_string()
            .starts_with("invalid policy: policy.failOn[0]:")
    );
}

/// Fails if an empty `path` stops being the *only* complaint about an
/// override whose other dimensions are fine -- the case above proves
/// each impossible dimension is reported separately; this one proves the
/// rule does not depend on another dimension also being broken.
#[test]
fn an_empty_object_path_restriction_is_rejected() {
    let spec = policy_spec(
        "overrides:\n  \
         - kind: container_added\n    \
           path: ''\n    \
           severity: info\n",
    );
    let rendered: Vec<String> = validate_policy_spec(&spec)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        rendered,
        vec![
            "policy.overrides[0].path: must not be empty (omit it to match every object path)"
                .to_owned()
        ]
    );
}

/// Fails if a policy section with nothing in it stops resolving, or
/// stops being equivalent to the permissive policy.
#[test]
fn an_omitted_policy_section_is_valid_and_permissive() {
    let spec = PolicySpec::default();
    assert!(validate_policy_spec(&spec).is_empty());
    let policy = resolve_policy(&spec).expect("the default policy resolves");

    let changes = [
        change(SemanticChangeKind::ImageChanged, "web", None, None),
        change(SemanticChangeKind::ContainerAdded, "web", None, None),
    ];
    assert_eq!(
        evaluate(&policy, &changes),
        evaluate(&ResolvedPolicy::permissive(), &changes)
    );
}

/// Fails if selector matching stops being conjunctive, or if an
/// unrestricted selector stops matching everything.
#[test]
fn selector_dimensions_are_conjunctive() {
    let selector = ChangeSelector {
        fixture_glob: Some("web-*".to_owned()),
        subject: Some("app".to_owned()),
        object_path: None,
    };
    let compiled = CompiledSelector::compile(&selector, "policy.overrides[0]").expect("compiles");
    assert_eq!(compiled.specificity(), 2);

    assert!(compiled.matches(&change(
        SemanticChangeKind::ImageChanged,
        "web-frontend",
        Some("/anything"),
        Some("app")
    )));
    // Right fixture, wrong subject.
    assert!(!compiled.matches(&change(
        SemanticChangeKind::ImageChanged,
        "web-frontend",
        None,
        Some("sidecar")
    )));
    // Right subject, wrong fixture.
    assert!(!compiled.matches(&change(
        SemanticChangeKind::ImageChanged,
        "batch",
        None,
        Some("app")
    )));

    let unrestricted = CompiledSelector::unrestricted();
    assert_eq!(unrestricted.specificity(), 0);
    assert!(unrestricted.matches(&change(SemanticChangeKind::ImageChanged, "web", None, None)));
    assert_eq!(
        CompiledSelector::compile(&ChangeSelector::unrestricted(), "policy.overrides[0]")
            .expect("compiles")
            .specificity(),
        0
    );
}
