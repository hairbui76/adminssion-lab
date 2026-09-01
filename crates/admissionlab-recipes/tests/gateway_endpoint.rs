//! ROADMAP Task 6.6, recipes half: the `gatewayEndpoint:` block a recipe
//! writes, driven through this crate's *public* loader
//! (`load_recipe_overrides`) so what is asserted is the YAML surface a
//! recipe author actually writes rather than an internal helper's
//! signature.
//!
//! The three properties this file exists to pin:
//!
//! - **The Istio-shaped strategy round-trips.** A recipe declaring
//!   `gatewayApi` plus a `serviceBySelector` block keyed on the
//!   upstream well-known `gateway.networking.k8s.io/gateway-name` label
//!   loads into exactly the [`GatewayEndpointStrategy`] value
//!   `admissionlab-gateway` resolves against a cluster. See
//!   [`admissionlab_spec::GatewayEndpointStrategy`]'s own documentation
//!   for the upstream provenance of that label and of Istio's
//!   `<Gateway name>-<GatewayClass name>` naming.
//! - **A placeholder typo fails at load time, loudly.** `{gateway}` is
//!   not `{gatewayName}`; left literal it would produce a selector that
//!   matches no `Service`, which Admission Lab would then have to report
//!   as "this Gateway has no data plane" -- a fabricated observation
//!   (Global Constraint 15) standing in for a configuration error.
//! - **No classification can enter through this block.** The
//!   `deny_unknown_fields` allow-list that already governs every other
//!   recipe field governs this one too, at its own nesting level: a
//!   `severity:` inside `gatewayEndpoint:` is a parse error for exactly
//!   the same structural reason a top-level `failOn:` is (PRODUCT.md
//!   §14 / Global Constraint 6; see `src/model.rs`'s module
//!   documentation).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use admissionlab_recipes::{
    Capability, GATEWAY_NAME_LABEL, GatewayEndpointStrategy, Recipe, load_recipe_overrides,
};

// ---------------------------------------------------------------------
// Test support (mirrors `tests/load.rs`'s helpers of the same shape)
// ---------------------------------------------------------------------

/// A temporary directory that removes itself when dropped.
///
/// [`load_one`] holds one for as long as the loader is reading out of
/// it. `Drop` runs on a panicking assertion too, which an explicit
/// delete at the end of a test does not — that is what keeps a `cargo
/// test` run from leaving a directory per test behind in the system temp
/// directory.
struct TempDir(PathBuf);

impl TempDir {
    /// The directory's path, valid for as long as this guard lives.
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_temp_dir(label: &str) -> TempDir {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-recipes-gateway-endpoint-{}-{label}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create unique temp dir");
    TempDir(dir)
}

fn dedent(text: &str) -> String {
    let indent = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    text.lines()
        .map(|line| line.get(indent..).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Loads one recipe document through the public override loader.
fn load_one(label: &str, yaml: &str) -> Result<Recipe, String> {
    let dir = unique_temp_dir(label);
    std::fs::write(dir.path().join("recipe.yaml"), dedent(yaml)).expect("write temp recipe file");
    match load_recipe_overrides(dir.path()) {
        Ok(mut recipes) => {
            assert_eq!(recipes.len(), 1, "each test writes exactly one recipe");
            Ok(recipes.remove(0))
        }
        Err(error) => Err(error.to_string()),
    }
}

/// A Helm-installing recipe body, with `capabilities`/`gatewayEndpoint`
/// supplied by each test.
fn recipe_yaml(tail: &str) -> String {
    format!(
        "{}\n{}",
        dedent(
            r#"
            name: istio-gateway
            version: "1.30.4"
            install:
              type: helm
              chart: istio/istiod
              repo: https://istio-release.storage.googleapis.com/charts
              version: "1.30.4"
              namespace: istio-system
            "#
        )
        .trim_start_matches('\n'),
        dedent(tail).trim_start_matches('\n')
    )
}

fn selector(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

// ---------------------------------------------------------------------
// The shape Task 6.10's istio-gateway recipe is expected to write
// ---------------------------------------------------------------------

#[test]
fn a_selector_strategy_keyed_on_the_well_known_gateway_name_label_loads() {
    let recipe = load_one(
        "istio-shaped",
        &recipe_yaml(
            r#"
            capabilities:
              - gatewayApi
            gatewayEndpoint:
              type: serviceBySelector
              namespace: "{gatewayNamespace}"
              selector:
                gateway.networking.k8s.io/gateway-name: "{gatewayName}"
              portName: http
            "#,
        ),
    )
    .expect("the Istio-shaped recipe must load");

    assert!(recipe.capabilities.contains(&Capability::GatewayApi));
    assert_eq!(
        recipe.gateway_endpoint,
        Some(GatewayEndpointStrategy::ServiceBySelector {
            namespace: "{gatewayNamespace}".to_owned(),
            selector: selector(&[(GATEWAY_NAME_LABEL, "{gatewayName}")]),
            port_name: Some("http".to_owned()),
            port: None,
        }),
        "the loaded strategy must be exactly what admissionlab-gateway resolves"
    );
}

#[test]
fn a_service_by_name_strategy_loads_with_an_explicit_port() {
    let recipe = load_one(
        "by-name",
        &recipe_yaml(
            r#"
            capabilities:
              - gatewayApi
            gatewayEndpoint:
              type: serviceByName
              namespace: gateway-lab
              name: "{gatewayName}-istio"
              port: 80
            "#,
        ),
    )
    .expect("a serviceByName recipe must load");

    assert_eq!(
        recipe.gateway_endpoint,
        Some(GatewayEndpointStrategy::ServiceByName {
            namespace: "gateway-lab".to_owned(),
            // Expressible, but class-coupled -- see
            // `GatewayEndpointStrategy`'s "Prefer the selector to the
            // name" section for why a recipe should not normally write
            // this.
            name: "{gatewayName}-istio".to_owned(),
            port_name: None,
            port: Some(80),
        })
    );
}

#[test]
fn a_recipe_with_no_gateway_capability_still_loads_with_no_endpoint() {
    let recipe = load_one(
        "no-gateway",
        &recipe_yaml(
            r"
            capabilities:
              - admission
            ",
        ),
    )
    .expect("an admission-only recipe must be unaffected by Task 6.6");
    assert_eq!(recipe.gateway_endpoint, None);
}

// ---------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------

#[test]
fn an_unknown_placeholder_is_rejected_at_load_time_naming_the_field() {
    let error = load_one(
        "typo",
        &recipe_yaml(
            r#"
            capabilities:
              - gatewayApi
            gatewayEndpoint:
              type: serviceBySelector
              namespace: "{gatewayNamespace}"
              selector:
                gateway.networking.k8s.io/gateway-name: "{gateway}"
            "#,
        ),
    )
    .expect_err("{gateway} is not a placeholder this project defines");

    assert!(
        error.contains("gatewayEndpoint.selector"),
        "the error must locate the offending field, got: {error}"
    );
    assert!(
        error.contains("{gatewayName}"),
        "the error must name the known placeholders, got: {error}"
    );
}

#[test]
fn declaring_the_gateway_capability_without_an_endpoint_is_rejected() {
    let error = load_one(
        "capability-without-endpoint",
        &recipe_yaml(
            r"
            capabilities:
              - gatewayApi
            ",
        ),
    )
    .expect_err("a Gateway recipe with no way to reach a data plane must be rejected");
    assert!(error.contains("gatewayEndpoint"), "got: {error}");
}

#[test]
fn declaring_an_endpoint_without_the_gateway_capability_is_rejected() {
    let error = load_one(
        "endpoint-without-capability",
        &recipe_yaml(
            r"
            capabilities:
              - admission
            gatewayEndpoint:
              type: serviceByName
              namespace: gateway-lab
              name: lab-gateway-istio
            ",
        ),
    )
    .expect_err("endpoint metadata for a capability the recipe does not claim must be rejected");
    assert!(error.contains("gatewayApi"), "got: {error}");
}

#[test]
fn an_unknown_strategy_type_is_rejected() {
    let error = load_one(
        "unknown-type",
        &recipe_yaml(
            r"
            capabilities:
              - gatewayApi
            gatewayEndpoint:
              type: nodePortGuess
              namespace: gateway-lab
            ",
        ),
    )
    .expect_err("the strategy set is closed");
    assert!(
        error.contains("nodePortGuess"),
        "the error must name the unknown variant, got: {error}"
    );
}

/// Global Constraint 6 at this block's own nesting level: the
/// allow-list, not a keyword scan, is what stops a classification field
/// from entering here.
#[test]
fn a_classification_field_inside_the_endpoint_block_fails_to_parse() {
    for smuggled in ["severity: high", "failOn: backendChanged"] {
        let error = load_one(
            "classification",
            &recipe_yaml(&format!(
                r"
                capabilities:
                  - gatewayApi
                gatewayEndpoint:
                  type: serviceByName
                  namespace: gateway-lab
                  name: lab-gateway-istio
                  {smuggled}
                "
            )),
        )
        .expect_err("a classification-shaped key must not parse");
        assert!(
            error.contains("unknown field"),
            "expected an unknown-field parse error for {smuggled:?}, got: {error}"
        );
    }
}
