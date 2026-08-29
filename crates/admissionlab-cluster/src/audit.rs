//! Rendering the Kubernetes API server audit policy Admission Lab mounts
//! into every `kind` cluster it creates (see [`crate::config`] for how
//! the rendered text reaches kube-apiserver).
//!
//! [`render_audit_policy`] takes no input: every cluster Admission Lab
//! creates uses the exact same policy, so there is nothing for a caller
//! to vary. What follows documents each rule and — this is the
//! load-bearing part — *why the order is exactly what it is*, since
//! Kubernetes audit policies use first-match-wins semantics: reordering
//! these rules changes what gets audited, not just how it reads.
//!
//! Exactly four rules, in this fixed order:
//!
//! 1. **Secrets: `None`.** No audit event at all is recorded for any
//!    request touching a core `secrets` resource — not even `Metadata`
//!    level. Must come before rule 3, which otherwise matches secret
//!    mutations at `Request` level and would defeat this exclusion
//!    (PRODUCT.md §29.3 "Secrets", §10.6 "Audit evidence semantics").
//!    `tests/kind_config.rs`'s `secret_exclusion_rule_precedes_general_request_rule`
//!    specifically guards this ordering.
//! 2. **Health/discovery noise: `None`.** Liveness/readiness/health
//!    probes, the version endpoint, and the Prometheus metrics scrape
//!    are polled continuously and carry no admission-relevant
//!    information; recording them even at `Metadata` level would dwarf
//!    genuine admission traffic in volume (brief Step 2).
//! 3. **Admission-relevant mutations: `Request`.** Records
//!    `create`/`update`/`patch`/`delete` on [`ADMISSION_RELEVANT_GROUPS`]
//!    at `Request` level — the level needed to capture mutating-webhook
//!    patch annotations (PRODUCT.md §10.6; this crate's dispatch point
//!    4). `get`/`list`/`watch` are deliberately excluded: those verbs
//!    never pass through Kubernetes admission control at all, so
//!    recording them would add pure volume with zero admission
//!    relevance.
//! 4. **Everything else: `Metadata`.** A low-cost fallback so an
//!    admission-relevant resource this policy did not anticipate is
//!    never silently invisible; still never includes a request/response
//!    body.
//!
//! `omitStages: [RequestReceived]` on the whole policy drops the
//! duplicate "request received" event Kubernetes would otherwise emit
//! alongside every rule's own stage, halving audit volume without
//! losing any information (brief Step 2).

use serde::Serialize;

/// `apiVersion` of the rendered policy document.
const API_VERSION: &str = "audit.k8s.io/v1";

/// Non-resource URL patterns treated as pure health/discovery noise: see
/// this module's rule 2 documentation.
const HEALTH_AND_DISCOVERY_URLS: &[&str] =
    &["/healthz*", "/readyz*", "/livez*", "/version", "/metrics"];

/// API groups whose mutating requests are admission-relevant: workload
/// resources fixtures create/update/delete (`""` core, `apps`, `batch`),
/// resources policy engines like Kyverno/Gatekeeper commonly gate
/// (`networking.k8s.io`, `rbac.authorization.k8s.io`), and the
/// webhook/policy configuration objects that are this tool's own
/// subject matter (`admissionregistration.k8s.io`).
const ADMISSION_RELEVANT_GROUPS: &[&str] = &[
    "",
    "apps",
    "batch",
    "networking.k8s.io",
    "rbac.authorization.k8s.io",
    "admissionregistration.k8s.io",
];

/// Verbs that pass through Kubernetes admission control. `get`/`list`/
/// `watch` never do (see rule 3's documentation).
const MUTATING_VERBS: &[&str] = &["create", "update", "patch", "delete"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum AuditLevel {
    None,
    Metadata,
    Request,
}

#[derive(Debug, Serialize)]
struct AuditPolicyDocument {
    #[serde(rename = "apiVersion")]
    api_version: &'static str,
    kind: &'static str,
    #[serde(rename = "omitStages")]
    omit_stages: Vec<&'static str>,
    rules: Vec<AuditPolicyRule>,
}

#[derive(Debug, Serialize)]
struct AuditPolicyRule {
    level: AuditLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    resources: Option<Vec<GroupResources>>,
    #[serde(rename = "nonResourceURLs", skip_serializing_if = "Option::is_none")]
    non_resource_urls: Option<Vec<&'static str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verbs: Option<Vec<&'static str>>,
}

#[derive(Debug, Serialize)]
struct GroupResources {
    group: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    resources: Option<Vec<&'static str>>,
}

/// Renders Admission Lab's kube-apiserver audit policy. See this
/// module's documentation for each rule's content and — the
/// load-bearing part — why the order is exactly what it is.
///
/// # Panics
///
/// Does not panic: every value serialized here is a fixed constant, so
/// the internal YAML rendering step cannot fail.
#[must_use]
pub fn render_audit_policy() -> String {
    let document = AuditPolicyDocument {
        api_version: API_VERSION,
        kind: "Policy",
        omit_stages: vec!["RequestReceived"],
        rules: vec![
            // Rule 1: exclude Secrets entirely. Must precede rule 3.
            AuditPolicyRule {
                level: AuditLevel::None,
                resources: Some(vec![GroupResources {
                    group: "",
                    resources: Some(vec!["secrets"]),
                }]),
                non_resource_urls: None,
                verbs: None,
            },
            // Rule 2: health/discovery noise.
            AuditPolicyRule {
                level: AuditLevel::None,
                resources: None,
                non_resource_urls: Some(HEALTH_AND_DISCOVERY_URLS.to_vec()),
                verbs: None,
            },
            // Rule 3: admission-relevant mutations, Request level.
            AuditPolicyRule {
                level: AuditLevel::Request,
                resources: Some(
                    ADMISSION_RELEVANT_GROUPS
                        .iter()
                        .map(|&group| GroupResources {
                            group,
                            resources: None,
                        })
                        .collect(),
                ),
                non_resource_urls: None,
                verbs: Some(MUTATING_VERBS.to_vec()),
            },
            // Rule 4: catch-all fallback, Metadata level. No filters:
            // matches whatever the first three rules did not.
            AuditPolicyRule {
                level: AuditLevel::Metadata,
                resources: None,
                non_resource_urls: None,
                verbs: None,
            },
        ],
    };

    serde_norway::to_string(&document)
        .expect("AuditPolicyDocument holds only fixed constants, which always serialize")
}
