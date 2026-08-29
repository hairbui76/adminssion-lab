//! Behavioral tests for `kind` cluster configuration rendering
//! ([`admissionlab_cluster::config`]) and the audit policy it mounts
//! ([`admissionlab_cluster::audit`]).
//!
//! Three properties are load-bearing here and are each covered below:
//!
//! - **Structural fidelity to the checked-in golden file** —
//!   `rendered_config_matches_golden_structure`. It compares *parsed*
//!   YAML rather than raw text (see that test's own documentation for
//!   why), including a second-level parse of the kubeadm
//!   `ClusterConfiguration` patch embedded as a string inside
//!   `nodes[0].kubeadmConfigPatches[0]`.
//! - **The rendered config actually reflects its input** —
//!   `rendered_config_reflects_input_fields` — so the golden test above
//!   cannot be satisfied by a renderer that ignores its argument and
//!   returns fixed text.
//! - **Secret exclusion, and specifically its *ordering*** —
//!   `secret_exclusion_rule_precedes_general_request_rule`. Kubernetes
//!   audit policies use first-match-wins semantics, so a `None` rule for
//!   Secrets that exists but is *shadowed* by an earlier, broader
//!   `Request`-level rule would still leak Secret request bodies. This
//!   test fails if the rules are ever reordered; two more tests
//!   (`ordering_check_rejects_*`) prove the check itself is not
//!   vacuously true by feeding it deliberately broken policies.

use std::path::{Path, PathBuf};

use admissionlab_cluster::{
    ClusterConfigError, KindClusterConfigInput, render_audit_policy, render_kind_config,
};

// ---------------------------------------------------------------------
// Test support
// ---------------------------------------------------------------------

/// Path to `testdata/golden/kind-config-audit.yaml`, which lives at the
/// workspace root (two levels above this crate's own
/// `CARGO_MANIFEST_DIR`) — mirrors the pattern already established by
/// `admissionlab-spec`'s `tests/schema.rs` and `tests/load.rs` for
/// workspace-rooted checked-in fixtures.
fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/golden/kind-config-audit.yaml")
}

/// The exact input `testdata/golden/kind-config-audit.yaml` was written
/// to match. Keep in sync with that file if either changes.
fn golden_input() -> KindClusterConfigInput {
    KindClusterConfigInput {
        name: "adlab-baseline-golden".to_string(),
        node_image: "kindest/node:v1.36.4@sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed".to_string(),
        audit_policy_host_path: PathBuf::from(
            "/var/lib/admissionlab/runs/golden/audit-policy.yaml",
        ),
        audit_log_host_dir: PathBuf::from("/var/lib/admissionlab/runs/golden/audit-logs"),
    }
}

fn parse_yaml(text: &str) -> serde_norway::Value {
    serde_norway::from_str(text).unwrap_or_else(|e| panic!("invalid YAML: {e}\n---\n{text}"))
}

/// Pulls out the kubeadm `ClusterConfiguration` patch text embedded as
/// `nodes[0].kubeadmConfigPatches[0]` in a parsed `kind` config document.
fn patch_text(doc: &serde_norway::Value) -> String {
    doc["nodes"][0]["kubeadmConfigPatches"][0]
        .as_str()
        .unwrap_or_else(|| {
            panic!("nodes[0].kubeadmConfigPatches[0] must be a string; doc = {doc:?}")
        })
        .to_owned()
}

/// Returns `doc` with its embedded kubeadm `ClusterConfiguration` patch
/// (`nodes[0].kubeadmConfigPatches[0]`, itself a YAML document embedded
/// as a string scalar) replaced by *its own parsed structure*.
///
/// Without this, a single top-level `assert_eq!` would treat that
/// string as an opaque leaf value and compare it byte-for-byte —
/// defeating the point of a structural comparison, since two
/// serializers can legitimately choose different indentation for the
/// *nested* YAML (for example list-item indent depth) while parsing to
/// identical structure. Normalizing first means one structural
/// comparison of the whole, normalized document catches a difference at
/// either nesting level, and formatting-only differences at either
/// level are not one.
fn normalize_embedded_patch(mut doc: serde_norway::Value) -> serde_norway::Value {
    let parsed_patch = parse_yaml(&patch_text(&doc));
    doc["nodes"][0]["kubeadmConfigPatches"][0] = parsed_patch;
    doc
}

// ---------------------------------------------------------------------
// Step 1: golden test
// ---------------------------------------------------------------------

#[test]
fn rendered_config_matches_golden_structure() {
    let golden_text = std::fs::read_to_string(golden_path())
        .unwrap_or_else(|e| panic!("checked-in golden file missing at {:?}: {e}", golden_path()));
    let rendered_text = render_kind_config(&golden_input()).expect("golden_input must render");

    // Parse the embedded kubeadm patch separately first, for
    // specific-field assertions below with a clearer failure message
    // than the whole-document comparison would give on its own.
    let golden_patch = parse_yaml(&patch_text(&parse_yaml(&golden_text)));
    assert_eq!(golden_patch["kind"].as_str(), Some("ClusterConfiguration"));
    assert_eq!(
        golden_patch["apiServer"]["extraArgs"]["audit-log-path"].as_str(),
        Some("/var/log/kubernetes/kube-apiserver-audit.log")
    );
    assert_eq!(
        golden_patch["apiServer"]["extraArgs"]["audit-policy-file"].as_str(),
        Some("/etc/kubernetes/policies/admissionlab-audit-policy.yaml")
    );

    // Structural equality of the whole document, at both nesting levels
    // (see `normalize_embedded_patch`). `serde_norway::Mapping`'s
    // `PartialEq` compares key/value pairs independent of key order
    // (backed by `IndexMap`, whose `PartialEq` is order-insensitive), so
    // this assertion is tolerant of formatting/quoting/key-order
    // differences at every level and only fails on a genuine structural
    // difference. Sequence order (for example `extraMounts`' two
    // entries) still matters, as it should.
    let golden_value = normalize_embedded_patch(parse_yaml(&golden_text));
    let rendered_value = normalize_embedded_patch(parse_yaml(&rendered_text));
    assert_eq!(
        rendered_value, golden_value,
        "rendered kind config no longer matches testdata/golden/kind-config-audit.yaml \
         (structural comparison, including the embedded kubeadm patch — see this test's \
         doc comment)"
    );
}

#[test]
fn rendered_config_reflects_input_fields() {
    // Guards against a renderer that ignores its argument and always
    // returns fixed (golden-matching) text: a *different* input must
    // produce correspondingly different output.
    let input = KindClusterConfigInput {
        name: "adlab-candidate-other".to_string(),
        node_image: "kindest/node:v1.37.0@sha256:a1ed56cfb0e7b93589bdf97c8cd566405a265939e3620fc4f5de89adff580ae5".to_string(),
        audit_policy_host_path: PathBuf::from("/tmp/other-run/policy.yaml"),
        audit_log_host_dir: PathBuf::from("/tmp/other-run/logs"),
    };
    let rendered = parse_yaml(&render_kind_config(&input).expect("must render"));

    assert_eq!(rendered["name"].as_str(), Some("adlab-candidate-other"));
    assert_eq!(
        rendered["nodes"][0]["image"].as_str(),
        Some(
            "kindest/node:v1.37.0@sha256:a1ed56cfb0e7b93589bdf97c8cd566405a265939e3620fc4f5de89adff580ae5"
        )
    );

    let extra_mounts = rendered["nodes"][0]["extraMounts"]
        .as_sequence()
        .expect("extraMounts must be a sequence");
    let host_paths: Vec<&str> = extra_mounts
        .iter()
        .map(|mount| {
            mount["hostPath"]
                .as_str()
                .expect("hostPath must be a string")
        })
        .collect();
    assert!(host_paths.contains(&"/tmp/other-run/policy.yaml"));
    assert!(host_paths.contains(&"/tmp/other-run/logs"));
}

#[test]
fn kind_config_rendering_is_deterministic() {
    let input = golden_input();
    assert_eq!(
        render_kind_config(&input).unwrap(),
        render_kind_config(&input).unwrap()
    );
}

// ---------------------------------------------------------------------
// Node image / extraMounts wiring: the two mount hops brief Step 1
// describes (host -> node via kind's extraMounts; node -> apiserver
// container via kubeadm's extraVolumes) must agree on the node-internal
// paths.
// ---------------------------------------------------------------------

#[test]
fn policy_mount_container_path_matches_apiserver_audit_policy_file_arg() {
    let rendered = parse_yaml(&render_kind_config(&golden_input()).unwrap());
    let patch = parse_yaml(&patch_text(&rendered));

    let policy_container_path = rendered["nodes"][0]["extraMounts"][0]["containerPath"]
        .as_str()
        .expect("extraMounts[0].containerPath must be a string");
    assert_eq!(
        Some(policy_container_path),
        patch["apiServer"]["extraArgs"]["audit-policy-file"].as_str(),
        "the node-internal path kind mounts the policy file at must be the exact path \
         kube-apiserver is told to read its policy from"
    );
}

#[test]
fn log_mount_container_path_is_the_parent_of_the_apiserver_audit_log_path_arg() {
    let rendered = parse_yaml(&render_kind_config(&golden_input()).unwrap());
    let patch = parse_yaml(&patch_text(&rendered));

    let log_dir_container_path = rendered["nodes"][0]["extraMounts"][1]["containerPath"]
        .as_str()
        .expect("extraMounts[1].containerPath must be a string");
    let audit_log_path = patch["apiServer"]["extraArgs"]["audit-log-path"]
        .as_str()
        .expect("audit-log-path must be a string");
    assert!(
        audit_log_path.starts_with(log_dir_container_path),
        "kube-apiserver's audit-log-path ({audit_log_path:?}) must live inside the directory \
         kind mounts the host's audit_log_host_dir at ({log_dir_container_path:?}), or the log \
         it writes never reaches the host"
    );
    assert_eq!(
        rendered["nodes"][0]["extraMounts"][1]["readOnly"].as_bool(),
        Some(false)
    );
    assert_eq!(
        rendered["nodes"][0]["extraMounts"][0]["readOnly"].as_bool(),
        Some(true)
    );
}

// ---------------------------------------------------------------------
// Input validation
// ---------------------------------------------------------------------

fn input_with_name(name: &str) -> KindClusterConfigInput {
    let mut input = golden_input();
    input.name = name.to_string();
    input
}

#[test]
fn empty_name_is_rejected() {
    let err = render_kind_config(&input_with_name("")).unwrap_err();
    assert!(matches!(err, ClusterConfigError::EmptyName));
}

#[test]
fn name_with_uppercase_is_rejected() {
    let err = render_kind_config(&input_with_name("Adlab-Baseline")).unwrap_err();
    assert!(matches!(err, ClusterConfigError::InvalidName { .. }));
}

#[test]
fn name_with_underscore_is_rejected() {
    let err = render_kind_config(&input_with_name("adlab_baseline")).unwrap_err();
    assert!(matches!(err, ClusterConfigError::InvalidName { .. }));
}

#[test]
fn name_starting_with_hyphen_is_rejected() {
    let err = render_kind_config(&input_with_name("-adlab")).unwrap_err();
    assert!(matches!(err, ClusterConfigError::InvalidName { .. }));
}

#[test]
fn name_ending_with_hyphen_is_rejected() {
    let err = render_kind_config(&input_with_name("adlab-")).unwrap_err();
    assert!(matches!(err, ClusterConfigError::InvalidName { .. }));
}

#[test]
fn name_that_is_a_single_lowercase_letter_is_accepted() {
    // Boundary case: shortest possible name that still starts and ends
    // (the same character) on an alphanumeric.
    render_kind_config(&input_with_name("a")).expect("single-letter name must be accepted");
}

#[test]
fn empty_node_image_is_rejected() {
    let mut input = golden_input();
    input.node_image = String::new();
    let err = render_kind_config(&input).unwrap_err();
    assert!(matches!(err, ClusterConfigError::EmptyNodeImage));
}

#[cfg(unix)]
#[test]
fn non_utf8_audit_policy_host_path_is_rejected() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    // 0xFF is never valid as the start of a UTF-8 sequence.
    let invalid = OsStr::from_bytes(&[0x66, 0xFF, 0x66]);
    let mut input = golden_input();
    input.audit_policy_host_path = PathBuf::from(invalid);

    let err = render_kind_config(&input).unwrap_err();
    assert!(matches!(err, ClusterConfigError::NonUtf8Path { .. }));
}

// ---------------------------------------------------------------------
// Step 2: audit policy content
// ---------------------------------------------------------------------

#[test]
fn audit_policy_is_a_v1_policy_document() {
    let policy = parse_yaml(&render_audit_policy());
    assert_eq!(policy["apiVersion"].as_str(), Some("audit.k8s.io/v1"));
    assert_eq!(policy["kind"].as_str(), Some("Policy"));
}

#[test]
fn audit_policy_omits_request_received_stage() {
    let policy = parse_yaml(&render_audit_policy());
    let omit_stages: Vec<&str> = policy["omitStages"]
        .as_sequence()
        .expect("omitStages must be a sequence")
        .iter()
        .map(|v| v.as_str().expect("omitStages entries must be strings"))
        .collect();
    assert!(
        omit_stages.contains(&"RequestReceived"),
        "omitStages must include RequestReceived (brief Step 2); got {omit_stages:?}"
    );
}

#[test]
fn audit_policy_health_and_discovery_urls_are_not_request_level() {
    let policy = parse_yaml(&render_audit_policy());
    let rules = policy["rules"]
        .as_sequence()
        .expect("rules must be a sequence");

    let health_urls = ["/healthz", "/readyz", "/livez", "/version", "/metrics"];
    let mut covered = std::collections::BTreeSet::new();

    for rule in rules {
        let Some(urls) = rule["nonResourceURLs"].as_sequence() else {
            continue;
        };
        let level = rule["level"].as_str().unwrap_or_default();
        assert_ne!(
            level, "Request",
            "a nonResourceURLs rule must not be at Request level (health/discovery noise); \
             rule = {rule:?}"
        );
        assert_ne!(level, "RequestResponse", "rule = {rule:?}");
        for pattern in urls {
            let pattern = pattern.as_str().unwrap_or_default();
            for &url in &health_urls {
                if pattern == url
                    || (pattern.ends_with('*') && url.starts_with(pattern.trim_end_matches('*')))
                {
                    covered.insert(url);
                }
            }
        }
    }

    assert_eq!(
        covered,
        health_urls.iter().copied().collect(),
        "every health/discovery URL must be covered by a None/Metadata rule; covered = {covered:?}"
    );
}

#[test]
fn audit_policy_records_admission_relevant_mutations_at_request_level() {
    let policy = parse_yaml(&render_audit_policy());
    let rules = policy["rules"]
        .as_sequence()
        .expect("rules must be a sequence");

    let matches_core_mutations = rules.iter().any(|rule| {
        let level_is_request = rule["level"].as_str() == Some("Request");
        let verbs_include_mutations = rule["verbs"].as_sequence().is_some_and(|verbs| {
            ["create", "update", "delete"]
                .iter()
                .all(|needed| verbs.iter().any(|v| v.as_str() == Some(*needed)))
        });
        let resources_include_core = rule["resources"]
            .as_sequence()
            .is_some_and(|groups| groups.iter().any(|g| g["group"].as_str() == Some("")));
        level_is_request && verbs_include_mutations && resources_include_core
    });

    assert!(
        matches_core_mutations,
        "policy must record create/update/delete on the core resource group at Request \
         level (brief Step 2); rules = {rules:?}"
    );
}

#[test]
fn audit_policy_rendering_is_deterministic() {
    assert_eq!(render_audit_policy(), render_audit_policy());
}

// ---------------------------------------------------------------------
// Step 3: the security test — Secret exclusion, and its ordering
// ---------------------------------------------------------------------

fn rule_level(rule: &serde_norway::Value) -> Option<&str> {
    rule["level"].as_str()
}

/// Whether `rule`'s `resources` filter would match a `secrets` request in
/// the core API group: either an entry for the core group (`group: ""`)
/// with no `resources` sub-filter (matches every core resource,
/// including secrets) or one that explicitly names `"secrets"`.
fn rule_matches_secrets(rule: &serde_norway::Value) -> bool {
    let Some(resources) = rule["resources"].as_sequence() else {
        return false;
    };
    resources.iter().any(|group_resources| {
        if group_resources["group"].as_str() != Some("") {
            return false;
        }
        match group_resources["resources"].as_sequence() {
            None => true,
            Some(list) => list.iter().any(|r| r.as_str() == Some("secrets")),
        }
    })
}

/// Checks that a `level: None` rule matching Secrets appears *before*
/// any `Request`-level-or-higher rule that would also match Secrets.
///
/// Returns `Err` naming the problem instead of asserting directly, so
/// the exact same check can run against both the real rendered policy
/// (`secret_exclusion_rule_precedes_general_request_rule`) and
/// deliberately broken synthetic policies
/// (`ordering_check_rejects_*`), proving this check is not vacuously
/// true — see this file's module documentation.
fn check_secrets_exclusion_ordering(rules: &[serde_norway::Value]) -> Result<(), String> {
    let none_index = rules
        .iter()
        .position(|rule| rule_level(rule) == Some("None") && rule_matches_secrets(rule))
        .ok_or("no `level: None` rule matches Secrets")?;

    let conflicting_indices: Vec<usize> = rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| {
            matches!(rule_level(rule), Some("Request" | "RequestResponse"))
                && rule_matches_secrets(rule)
        })
        .map(|(index, _)| index)
        .collect();

    if conflicting_indices.is_empty() {
        return Err(
            "no Request-level-or-higher rule matches Secrets; the ordering check is vacuous \
             against this policy"
                .to_string(),
        );
    }

    if let Some(&earlier) = conflicting_indices
        .iter()
        .find(|&&index| index < none_index)
    {
        return Err(format!(
            "a Request-level-or-higher rule at index {earlier} matches Secrets and comes \
             before the `None` exclusion rule at index {none_index}; Kubernetes audit \
             policies are first-match-wins, so this rule wins and Secret bodies are logged"
        ));
    }

    Ok(())
}

#[test]
fn secret_exclusion_rule_precedes_general_request_rule() {
    let policy = parse_yaml(&render_audit_policy());
    let rules = policy["rules"]
        .as_sequence()
        .expect("rules must be a sequence");
    check_secrets_exclusion_ordering(rules).expect(
        "rendered audit policy must exclude Secrets (level: None) strictly before any \
         Request-level-or-higher rule that would otherwise match them",
    );
}

#[test]
fn ordering_check_rejects_a_reordered_secrets_rule() {
    // Proves the check above is not vacuous: a policy where the
    // Request-level rule matching Secrets comes *before* the None
    // exclusion rule must be rejected.
    let broken = parse_yaml(
        r#"
rules:
  - level: Request
    verbs: ["create"]
    resources:
      - group: ""
  - level: None
    resources:
      - group: ""
        resources: ["secrets"]
"#,
    );
    let rules = broken["rules"].as_sequence().unwrap();
    let result = check_secrets_exclusion_ordering(rules);
    assert!(
        result.is_err(),
        "check must reject a policy where the Request-level rule precedes the None exclusion"
    );
}

#[test]
fn ordering_check_rejects_a_policy_with_no_secrets_exclusion() {
    let broken = parse_yaml(
        r#"
rules:
  - level: Request
    verbs: ["create"]
    resources:
      - group: ""
"#,
    );
    let rules = broken["rules"].as_sequence().unwrap();
    assert!(check_secrets_exclusion_ordering(rules).is_err());
}

#[test]
fn ordering_check_accepts_a_correctly_ordered_synthetic_policy() {
    // The mirror image of the two tests above: proves the check does
    // *not* reject a policy just because it contains both rule shapes,
    // only because of their order.
    let correct = parse_yaml(
        r#"
rules:
  - level: None
    resources:
      - group: ""
        resources: ["secrets"]
  - level: Request
    verbs: ["create"]
    resources:
      - group: ""
"#,
    );
    let rules = correct["rules"].as_sequence().unwrap();
    assert!(check_secrets_exclusion_ordering(rules).is_ok());
}
