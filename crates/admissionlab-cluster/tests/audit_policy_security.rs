//! The standing proof that Admission Lab's audit policy never writes a
//! credential body to disk (ROADMAP Task 9.3, Global Constraints 14 and
//! 18).
//!
//! `tests/kind_config.rs` already checks the policy's *shape*: that a
//! `level: None` rule for Secrets exists and precedes the general
//! `Request` rule. This file checks its *behavior*, by doing what
//! kube-apiserver does — resolving each request against the ordered rules
//! and taking the first match — and then asking the resolved level a
//! question the shape alone cannot answer: "would this request's body be
//! written to the log?"
//!
//! The Phase 3 exit gate observed once, on a real cluster, that no Secret
//! body appeared in an audit log. That observation is expensive to repeat
//! and covers only the requests that run happened to make. This is the
//! unit-level standing version of it, over the policy document itself,
//! and it covers requests no fixture makes.
//!
//! # The three things proved here
//!
//! 1. **[`the_rendered_policy_leaks_no_credential_body`]** — every
//!    request in [`CREDENTIAL_BEARING`] resolves to a level at or below
//!    the maximum that request may be recorded at. The table names *why*
//!    each entry is credential-bearing, which is the part a future
//!    reviewer needs and a shape assertion cannot carry.
//! 2. **[`no_rule_records_a_response_body`]** — nothing anywhere in the
//!    policy is `RequestResponse`. This is what makes
//!    `serviceaccounts/token` safe: its request body holds no token, its
//!    response holds the minted one, and no rule records a response.
//! 3. **The pins.** [`inserting_a_secret_logging_rule_before_the_exclusion_is_rejected`]
//!    and its siblings run the same checker over deliberately broken
//!    policies — a `Request` rule for Secrets inserted at each position,
//!    a rule promoted to `RequestResponse`, `authentication.k8s.io` added
//!    to the admission-relevant group list — so a future rule addition
//!    that logs a credential fails here rather than shipping.
//!
//! # The simulator is checked against the document it simulates
//!
//! [`parse_rules`] rejects any rule carrying a selector this file does not
//! model (`users`, `userGroups`, `namespaces`, `omitStages` per rule).
//! Without that, a future rule whose matching this simulator gets wrong
//! would be silently treated as never matching — which is the one way a
//! test like this can pass while the policy leaks.

use std::collections::BTreeSet;

use admissionlab_cluster::render_audit_policy;

// ---------------------------------------------------------------------
// A kube-apiserver audit policy, as this file models it
// ---------------------------------------------------------------------

/// An audit level, ordered by how much of a request it records.
///
/// `Ord` is the point: "at or below `Metadata`" is the whole safety
/// question for a credential-bearing request, and an ordering makes that
/// one comparison rather than a match arm per level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    /// No event at all.
    None,
    /// Metadata only: who, what, when. Never a body.
    Metadata,
    /// The request body as well.
    Request,
    /// The request body *and* the response body.
    RequestResponse,
}

impl Level {
    /// Parses the string Kubernetes writes in a policy document.
    fn parse(text: &str) -> Self {
        match text {
            "None" => Self::None,
            "Metadata" => Self::Metadata,
            "Request" => Self::Request,
            "RequestResponse" => Self::RequestResponse,
            other => panic!("unknown audit level {other:?}"),
        }
    }

    /// Whether an event at this level carries the request body.
    const fn records_request_body(self) -> bool {
        matches!(self, Self::Request | Self::RequestResponse)
    }
}

/// One `group`/`resources` entry of a rule's `resources` list.
#[derive(Debug, Clone)]
struct GroupResources {
    group: String,
    /// `None` means "every resource *and subresource* in this group",
    /// which is the semantics kube-apiserver gives an omitted list.
    resources: Option<Vec<String>>,
}

/// One rule of the policy.
#[derive(Debug, Clone)]
struct Rule {
    level: Level,
    resources: Option<Vec<GroupResources>>,
    non_resource_urls: Option<Vec<String>>,
    verbs: Option<Vec<String>>,
}

/// A request against a resource endpoint, as an audit rule sees it.
#[derive(Debug, Clone, Copy)]
struct ResourceRequest<'a> {
    group: &'a str,
    resource: &'a str,
    /// Empty for a request against the resource itself.
    subresource: &'a str,
    verb: &'a str,
}

/// The rule fields this file knows how to evaluate. A rule carrying
/// anything else is rejected by [`parse_rules`] rather than silently
/// mis-simulated.
const MODELLED_RULE_FIELDS: &[&str] = &["level", "resources", "nonResourceURLs", "verbs"];

/// Parses a rendered policy's `rules` into the model above.
///
/// Panics rather than returning an error: every caller here is a test,
/// and a policy this file cannot parse is a failure of exactly the kind
/// this file exists to report.
fn parse_rules(policy_yaml: &str) -> Vec<Rule> {
    let document: serde_norway::Value = serde_norway::from_str(policy_yaml)
        .unwrap_or_else(|error| panic!("the rendered policy must be YAML: {error}"));
    let rules = document["rules"]
        .as_sequence()
        .expect("a policy always has a `rules` sequence");

    rules
        .iter()
        .map(|rule| {
            let mapping = rule
                .as_mapping()
                .expect("every rule is a mapping of fields");
            for key in mapping.keys() {
                let key = key.as_str().expect("a rule's field names are strings");
                assert!(
                    MODELLED_RULE_FIELDS.contains(&key),
                    "this file's first-match-wins simulator does not model the rule selector \
                     {key:?}; teach it that selector before adding a rule that uses one, or \
                     every assertion below silently stops covering that rule"
                );
            }

            Rule {
                level: Level::parse(
                    rule["level"]
                        .as_str()
                        .expect("every rule has a string level"),
                ),
                resources: rule["resources"].as_sequence().map(|groups| {
                    groups
                        .iter()
                        .map(|entry| GroupResources {
                            group: entry["group"]
                                .as_str()
                                .expect("a resources entry names a group")
                                .to_owned(),
                            resources: entry["resources"].as_sequence().map(|list| {
                                list.iter()
                                    .map(|value| {
                                        value
                                            .as_str()
                                            .expect("a resource name is a string")
                                            .to_owned()
                                    })
                                    .collect()
                            }),
                        })
                        .collect()
                }),
                non_resource_urls: rule["nonResourceURLs"].as_sequence().map(|list| {
                    list.iter()
                        .map(|value| {
                            value
                                .as_str()
                                .expect("a URL pattern is a string")
                                .to_owned()
                        })
                        .collect()
                }),
                verbs: rule["verbs"].as_sequence().map(|list| {
                    list.iter()
                        .map(|value| value.as_str().expect("a verb is a string").to_owned())
                        .collect()
                }),
            }
        })
        .collect()
}

/// Whether `rule` matches `request`, following kube-apiserver's
/// `policy.Checker`.
///
/// The three parts, in the order they can each rule the request out:
///
/// - **Verbs.** An omitted list matches every verb; `"*"` does too.
/// - **Non-resource URLs.** A rule that names them matches *only*
///   non-resource requests, so it can never match a resource request no
///   matter what else it says. This is what keeps rule 2 (health and
///   discovery) from being mistaken for a rule that covers `/api/v1/...`.
/// - **Resources.** A group entry with no `resources` list matches every
///   resource *and every subresource* in that group. A list matches a
///   bare resource by name, and a subresource only through an explicit
///   `resource/subresource` or `*/subresource` entry — the boundary
///   `a_hypothetical_secret_subresource_is_not_covered_by_rule_one` pins.
/// - A rule with neither `resources` nor `nonResourceURLs` matches
///   everything: the catch-all.
fn matches(rule: &Rule, request: ResourceRequest<'_>) -> bool {
    if let Some(verbs) = &rule.verbs
        && !verbs.iter().any(|verb| verb == request.verb || verb == "*")
    {
        return false;
    }

    match (&rule.resources, &rule.non_resource_urls) {
        (Some(groups), _) => groups
            .iter()
            .any(|entry| group_resources_match(entry, request)),
        (None, Some(_)) => false,
        (None, None) => true,
    }
}

/// The resource half of [`matches`], for one `group`/`resources` entry.
fn group_resources_match(entry: &GroupResources, request: ResourceRequest<'_>) -> bool {
    if entry.group != request.group {
        return false;
    }
    let Some(resources) = &entry.resources else {
        return true;
    };
    resources.iter().any(|name| {
        if request.subresource.is_empty() {
            name == request.resource
        } else {
            name == &format!("{}/{}", request.resource, request.subresource)
                || name == &format!("*/{}", request.subresource)
        }
    })
}

/// The level `request` is recorded at: the first matching rule's level,
/// or [`Level::None`] when nothing matches.
///
/// First match wins. That single line is the whole reason rule order in
/// `audit.rs` is load-bearing, and the reason a "the exclusion rule
/// exists" assertion is not enough on its own.
fn effective_level(rules: &[Rule], request: ResourceRequest<'_>) -> Level {
    rules
        .iter()
        .find(|rule| matches(rule, request))
        .map_or(Level::None, |rule| rule.level)
}

/// The verbs a request can carry. Both the mutating ones the policy
/// names and the read verbs it does not, so a rule that accidentally
/// dropped its `verbs` filter is visible.
const ALL_VERBS: &[&str] = &[
    "create",
    "update",
    "patch",
    "delete",
    "deletecollection",
    "get",
    "list",
    "watch",
];

// ---------------------------------------------------------------------
// The credential table, and the checker over it
// ---------------------------------------------------------------------

/// One resource whose request body can hold a credential, the highest
/// level it may ever be recorded at, and why.
struct CredentialBearing {
    group: &'static str,
    resource: &'static str,
    subresource: &'static str,
    /// The highest level any verb on this resource may resolve to.
    max_level: Level,
    why: &'static str,
}

/// Every credential-bearing endpoint this project has identified, with
/// the level ceiling each one must stay under.
///
/// This table is the honest answer to "which resources carry credentials"
/// rather than only "Secrets": three of the five entries are not Secrets,
/// and two of them are safe today for a reason (`ADMISSION_RELEVANT_GROUPS`
/// is an allow-list; no rule is `RequestResponse`) that a future edit
/// could remove without touching Secrets at all.
const CREDENTIAL_BEARING: &[CredentialBearing] = &[
    CredentialBearing {
        group: "",
        resource: "secrets",
        subresource: "",
        max_level: Level::None,
        why: "a Secret's `data`/`stringData` is the credential. Not `Metadata` either: even the \
              name of a Secret being written is more than this tool needs, and `None` is the \
              stance PRODUCT.md §29.3 states.",
    },
    CredentialBearing {
        group: "authentication.k8s.io",
        resource: "tokenreviews",
        subresource: "",
        max_level: Level::Metadata,
        why: "a TokenReview's *request* body is `spec.token` — a bearer token in plain text. It \
              is safe only because `authentication.k8s.io` is absent from the \
              admission-relevant group allow-list and falls through to the Metadata catch-all.",
    },
    CredentialBearing {
        group: "",
        resource: "serviceaccounts",
        subresource: "token",
        max_level: Level::Request,
        why: "a TokenRequest's request body carries audiences and an expiry, never a token; the \
              minted token is in the *response*, so this endpoint is safe exactly as long as no \
              rule is RequestResponse.",
    },
    CredentialBearing {
        group: "certificates.k8s.io",
        resource: "certificatesigningrequests",
        subresource: "",
        max_level: Level::Metadata,
        why: "a CSR body is public material (a PEM certificate request), but its `status` carries \
              an issued certificate and the resource sits next to key material in every \
              operator's mental model. Metadata costs nothing here — nothing in this group is \
              admission-relevant to Admission Lab.",
    },
    CredentialBearing {
        group: "authentication.k8s.io",
        resource: "selfsubjectreviews",
        subresource: "",
        max_level: Level::Metadata,
        why: "same group as TokenReview, and the same allow-list argument: an entry here exists \
              so that widening the group list fails this test rather than only the TokenReview \
              row.",
    },
];

/// Every way `rules` would record a credential body, as human-readable
/// findings.
///
/// Returns findings rather than asserting, so the identical check runs
/// against the real policy and against the deliberately broken ones the
/// pins below build — the same construction `kind_config.rs`'s ordering
/// check uses, and for the same reason: a checker only ever exercised on
/// a passing input is a checker nobody has tested.
fn credential_leaks(rules: &[Rule]) -> Vec<String> {
    let mut findings = Vec::new();

    for entry in CREDENTIAL_BEARING {
        for verb in ALL_VERBS {
            let request = ResourceRequest {
                group: entry.group,
                resource: entry.resource,
                subresource: entry.subresource,
                verb,
            };
            let level = effective_level(rules, request);
            if level > entry.max_level {
                findings.push(format!(
                    "{verb} on {}/{}{}{} resolves to {level:?}, above its ceiling of {:?}: {}",
                    if entry.group.is_empty() {
                        "core"
                    } else {
                        entry.group
                    },
                    entry.resource,
                    if entry.subresource.is_empty() {
                        ""
                    } else {
                        "/"
                    },
                    entry.subresource,
                    entry.max_level,
                    entry.why,
                ));
            }
        }
    }

    for (index, rule) in rules.iter().enumerate() {
        if rule.level == Level::RequestResponse {
            findings.push(format!(
                "rule {index} is RequestResponse, so response bodies are recorded; a \
                 serviceaccounts/token response is a minted bearer token"
            ));
        }
    }

    findings
}

/// The rendered policy, parsed.
fn rendered_rules() -> Vec<Rule> {
    parse_rules(&render_audit_policy())
}

// ---------------------------------------------------------------------
// The proof
// ---------------------------------------------------------------------

#[test]
fn the_rendered_policy_leaks_no_credential_body() {
    let findings = credential_leaks(&rendered_rules());
    assert!(
        findings.is_empty(),
        "the rendered audit policy would record credential material:\n  {}",
        findings.join("\n  ")
    );
}

#[test]
fn no_secret_request_is_recorded_at_any_level_for_any_verb() {
    // The narrow, load-bearing case, asserted on its own so a failure
    // names Secrets rather than "some credential-bearing resource".
    let rules = rendered_rules();
    for verb in ALL_VERBS {
        let level = effective_level(
            &rules,
            ResourceRequest {
                group: "",
                resource: "secrets",
                subresource: "",
                verb,
            },
        );
        assert_eq!(
            level,
            Level::None,
            "{verb} on core/secrets resolves to {level:?}; kube-apiserver takes the first \
             matching rule, so the `level: None` exclusion must precede every rule that could \
             also match a Secret"
        );
    }
}

#[test]
fn no_rule_records_a_response_body() {
    let levels: BTreeSet<Level> = rendered_rules().iter().map(|rule| rule.level).collect();
    assert!(
        !levels.contains(&Level::RequestResponse),
        "no rule may be RequestResponse: a response body is where a minted ServiceAccount \
         token, a created Secret, and an issued certificate live, and none of them is evidence \
         this tool needs. Levels present: {levels:?}"
    );
}

#[test]
fn the_policys_stance_on_every_enumerated_resource_is_exactly_this() {
    // The whole document's behavior in one table. It is deliberately
    // exhaustive rather than illustrative: changing what the policy
    // records is then a visible edit to this list, with the reasons in
    // front of whoever makes it.
    let rules = rendered_rules();
    let stance = [
        // Excluded outright.
        ("", "secrets", "", "create", Level::None),
        ("", "secrets", "", "delete", Level::None),
        // Admission-relevant mutations: bodies recorded.
        ("", "pods", "", "create", Level::Request),
        ("", "configmaps", "", "create", Level::Request),
        ("", "configmaps", "", "patch", Level::Request),
        ("", "serviceaccounts", "token", "create", Level::Request),
        ("apps", "deployments", "", "update", Level::Request),
        ("batch", "jobs", "", "create", Level::Request),
        (
            "networking.k8s.io",
            "ingresses",
            "",
            "create",
            Level::Request,
        ),
        (
            "rbac.authorization.k8s.io",
            "roles",
            "",
            "create",
            Level::Request,
        ),
        (
            "admissionregistration.k8s.io",
            "mutatingwebhookconfigurations",
            "",
            "create",
            Level::Request,
        ),
        // Read verbs never reach admission control, so they are not
        // recorded above the catch-all even in those groups.
        ("", "pods", "", "get", Level::Metadata),
        ("", "configmaps", "", "list", Level::Metadata),
        // Groups outside the allow-list: metadata only, whatever the verb.
        (
            "authentication.k8s.io",
            "tokenreviews",
            "",
            "create",
            Level::Metadata,
        ),
        (
            "certificates.k8s.io",
            "certificatesigningrequests",
            "",
            "create",
            Level::Metadata,
        ),
        (
            "gateway.networking.k8s.io",
            "httproutes",
            "",
            "create",
            Level::Metadata,
        ),
    ];

    for (group, resource, subresource, verb, expected) in stance {
        let level = effective_level(
            &rules,
            ResourceRequest {
                group,
                resource,
                subresource,
                verb,
            },
        );
        assert_eq!(
            level,
            expected,
            "{verb} on {}/{resource}{}{subresource}",
            if group.is_empty() { "core" } else { group },
            if subresource.is_empty() { "" } else { "/" },
        );
    }
}

#[test]
fn configmap_bodies_are_recorded_and_that_is_the_documented_stance() {
    // Stated as its own test, with its own name, because it is the one
    // entry in the table above that a reader could mistake for an
    // oversight. It is not: see `audit.rs`'s "ConfigMaps are logged at
    // `Request`, on purpose" and `docs/security.md`. A ConfigMap is an
    // ordinary fixture object here (the shipped example's control fixture
    // is one), and demoting it to Metadata would drop the mutating
    // webhook patch annotations Global Constraint 18 requires `Request`
    // for — the evidence this tool exists to collect.
    //
    // If this ever becomes false, the two documents above are wrong and
    // must change with it.
    let rules = rendered_rules();
    for verb in ["create", "update", "patch", "delete"] {
        let level = effective_level(
            &rules,
            ResourceRequest {
                group: "",
                resource: "configmaps",
                subresource: "",
                verb,
            },
        );
        assert_eq!(level, Level::Request);
        assert!(
            level.records_request_body(),
            "and `Request` is precisely the level that writes the ConfigMap's `data` into the \
             run's audit log — which is the trade this stance makes"
        );
    }
}

#[test]
fn a_hypothetical_secret_subresource_is_not_covered_by_rule_one() {
    // A known boundary, asserted so it cannot become a surprise.
    //
    // Rule 1 names the resource `secrets`, and Kubernetes matches a
    // subresource only through an explicit `resource/subresource` (or
    // `*/subresource`) entry. Core Secrets have no subresources, so
    // nothing is missed today. If Kubernetes ever adds one, this test
    // starts describing a real leak — and the fix is to name it in rule
    // 1, not to relax this assertion.
    let rules = rendered_rules();
    let level = effective_level(
        &rules,
        ResourceRequest {
            group: "",
            resource: "secrets",
            subresource: "status",
            verb: "patch",
        },
    );
    assert_eq!(
        level,
        Level::Request,
        "if this changed, either rule 1 learned about subresources (good — update this test) or \
         the general Request rule stopped covering the core group (check what else that broke)"
    );
}

// ---------------------------------------------------------------------
// The pins: the checker must reject the ways this could break
// ---------------------------------------------------------------------

/// A `Request`-level rule that matches Secrets — the future addition
/// Task 9.3 step 1c requires to fail.
fn secret_logging_rule() -> Rule {
    Rule {
        level: Level::Request,
        resources: Some(vec![GroupResources {
            group: String::new(),
            resources: Some(vec!["secrets".to_owned()]),
        }]),
        non_resource_urls: None,
        verbs: Some(vec!["create".to_owned(), "update".to_owned()]),
    }
}

#[test]
fn inserting_a_secret_logging_rule_before_the_exclusion_is_rejected() {
    // First-match-wins, stated as an experiment rather than as a claim:
    // the same rule is inserted at every position, and the checker must
    // reject it exactly at the positions that precede the `None`
    // exclusion. A checker that ignored order would pass at every
    // position; one that rejected the rule outright would fail at the
    // later ones, where the exclusion still wins and the addition is
    // genuinely harmless.
    let rules = rendered_rules();
    let exclusion = rules
        .iter()
        .position(|rule| rule.level == Level::None && rule.resources.is_some())
        .expect("the policy opens with the Secret exclusion");

    for position in 0..=rules.len() {
        let mut broken = rules.clone();
        broken.insert(position, secret_logging_rule());
        let findings = credential_leaks(&broken);

        if position <= exclusion {
            assert!(
                !findings.is_empty(),
                "a Request-level rule matching Secrets inserted at index {position} (at or \
                 before the exclusion at {exclusion}) wins the first-match race and logs Secret \
                 bodies, but the checker found nothing"
            );
        } else {
            assert!(
                findings.is_empty(),
                "the same rule inserted at index {position}, after the exclusion, is shadowed \
                 and harmless; a checker that rejects it is rejecting rule *shape* rather than \
                 rule *effect*: {findings:?}"
            );
        }
    }
}

#[test]
fn promoting_a_rule_to_request_response_is_rejected() {
    let mut broken = rendered_rules();
    let general = broken
        .iter()
        .position(|rule| rule.level == Level::Request)
        .expect("the policy has a Request-level rule");
    broken[general].level = Level::RequestResponse;

    let findings = credential_leaks(&broken);
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("RequestResponse")),
        "promoting the admission-relevant rule to RequestResponse records response bodies, \
         including a minted serviceaccounts/token: {findings:?}"
    );
}

#[test]
fn adding_the_authentication_group_to_the_request_rule_is_rejected() {
    // The realistic way this policy grows a leak: someone widens the
    // admission-relevant group list to "cover more of the API", and
    // TokenReview request bodies — bearer tokens — start being written to
    // the log.
    let mut broken = rendered_rules();
    let general = broken
        .iter()
        .position(|rule| rule.level == Level::Request)
        .expect("the policy has a Request-level rule");
    broken[general]
        .resources
        .as_mut()
        .expect("the Request-level rule names resource groups")
        .push(GroupResources {
            group: "authentication.k8s.io".to_owned(),
            resources: None,
        });

    let findings = credential_leaks(&broken);
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("tokenreviews")),
        "adding authentication.k8s.io to the Request-level rule logs TokenReview bodies, which \
         are bearer tokens: {findings:?}"
    );
}

#[test]
fn removing_the_secret_exclusion_entirely_is_rejected() {
    let broken: Vec<Rule> = rendered_rules()
        .into_iter()
        .filter(|rule| rule.level != Level::None || rule.resources.is_none())
        .collect();

    let findings = credential_leaks(&broken);
    assert!(
        findings.iter().any(|finding| finding.contains("secrets")),
        "with the exclusion removed, Secret mutations fall through to the general Request rule: \
         {findings:?}"
    );
}

#[test]
fn a_rule_carrying_an_unmodelled_selector_is_rejected_by_the_parser() {
    // The simulator's own honesty check. A rule with a `namespaces` or
    // `users` selector matches conditionally in ways nothing here models,
    // and treating it as an ordinary rule would quietly weaken every
    // assertion above.
    let policy = r#"
rules:
  - level: Request
    users: ["system:serviceaccount:kube-system:generic-garbage-collector"]
    resources:
      - group: ""
"#;
    let result = std::panic::catch_unwind(|| parse_rules(policy));
    assert!(
        result.is_err(),
        "a rule selector this file cannot evaluate must fail loudly, not be ignored"
    );
}

#[test]
fn the_health_and_discovery_rule_cannot_match_a_resource_request() {
    // A `nonResourceURLs` rule matches only non-resource requests. If
    // this file modelled that wrongly — treating the rule as a catch-all
    // — rule 2 would shadow rule 3 and every level assertion above would
    // be measuring the wrong rule.
    let rules = rendered_rules();
    let health = rules
        .iter()
        .find(|rule| rule.non_resource_urls.is_some())
        .expect("the policy has a health/discovery rule");

    assert!(!matches(
        health,
        ResourceRequest {
            group: "",
            resource: "pods",
            subresource: "",
            verb: "create",
        }
    ));
    assert_eq!(health.level, Level::None);
}

#[test]
fn the_policy_is_exactly_these_four_rules_in_this_order() {
    // The blunt pin behind everything above: a rule added anywhere fails
    // here, so adding one is a deliberate act taken with `audit.rs`'s
    // ordering documentation and this file's credential table in front of
    // the author.
    let levels: Vec<Level> = rendered_rules().iter().map(|rule| rule.level).collect();
    assert_eq!(
        levels,
        vec![Level::None, Level::None, Level::Request, Level::Metadata],
        "the audit policy's rule sequence changed; re-read `audit.rs`'s rule documentation and \
         this file's `CREDENTIAL_BEARING` table before updating this assertion"
    );
}
