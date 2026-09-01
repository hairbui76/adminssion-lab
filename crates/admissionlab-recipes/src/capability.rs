//! Recipe-specific capability *logic*: turning the plain strings a
//! recipe YAML document writes under `capabilities:` into
//! [`admissionlab_spec::Capability`] values, and turning the
//! `gatewayEndpoint:` block that accompanies
//! [`Capability::GatewayApi`] into a validated
//! [`GatewayEndpointStrategy`] (ROADMAP Task 6.6).
//!
//! [`Capability`] itself is **not** defined here. Controller Ruling R30
//! (Task 2.5): `admissionlab-spec` already owns it, because
//! [`admissionlab_spec::ResolvedComponent::capabilities`] references it
//! and `admissionlab-spec` must stay a leaf crate — defining a second,
//! competing `Capability` in this crate would either fork the type in
//! two incompatible copies across the workspace, or force
//! `admissionlab-spec` to depend on `admissionlab-recipes` to reuse this
//! one, closing a cycle the moment `admissionlab_spec::resolve_lab`
//! needed to produce a component's capabilities. See this crate's own
//! `lib.rs` module documentation for the full reasoning.
//!
//! What legitimately belongs in *this* module is the piece R30 leaves
//! unassigned: the string vocabulary a recipe author actually writes,
//! and the parsing between it and the enum.
//! `admissionlab_spec::component`'s own module documentation states
//! plainly that nothing there "is ever deserialized directly from a
//! configuration file" — [`Capability`] carries no `serde::Deserialize`
//! impl and no string mapping of its own. This crate is the first (and,
//! before a later task adds one, only) place a YAML document's
//! `capabilities:` list is ever parsed, which makes it the correct owner
//! of that mapping, not an arbitrary one.

use admissionlab_spec::{Capability, GatewayEndpointStrategy};

use crate::model::RawGatewayEndpoint;

/// The exact set of strings a recipe's `capabilities:` list may contain,
/// paired with the [`Capability`] each parses to. Declared in the same
/// order [`Capability`]'s own variants are declared, and used both to
/// parse ([`parse_capability`]) and to build a specific, actionable
/// error message when a value matches none of them.
const KNOWN: &[(&str, Capability)] = &[
    ("admission", Capability::Admission),
    ("gatewayApi", Capability::GatewayApi),
    ("legacyIngress", Capability::LegacyIngress),
];

/// Parses one `capabilities:` entry, exactly as written in a recipe YAML
/// document, into a [`Capability`].
///
/// Case-sensitive `camelCase`, matching this project's YAML convention
/// everywhere else a hand-written multi-word key or enum-like value
/// appears (see `admissionlab_spec::model`'s own module documentation):
/// `"admission"`, `"gatewayApi"`, `"legacyIngress"`. Deliberately an
/// allow-list against [`KNOWN`] rather than a case-insensitive or fuzzy
/// match — capabilities are consumed by a later task to decide which
/// fixtures a recipe's component is exercised against, so silently
/// accepting a near-miss spelling (`"Admission"`, `"gateway-api"`) as if
/// it were a real, different capability would silently change *what
/// gets tested*, with no visible error. Global Constraint 15 ("missing
/// data is unavailable/unknown, never fabricated") applies just as much
/// to a mis-typed value as to an absent one: guessing at the closest
/// known spelling would be exactly that kind of fabrication.
///
/// # Errors
///
/// Returns `Err` with a message naming both the offending value and the
/// full set of recognized spellings when `raw` matches none of them.
pub(crate) fn parse_capability(raw: &str) -> Result<Capability, String> {
    KNOWN
        .iter()
        .find(|(name, _)| *name == raw)
        .map(|(_, capability)| *capability)
        .ok_or_else(|| {
            let known = KNOWN
                .iter()
                .map(|(name, _)| format!("{name:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("unknown capability {raw:?}; expected one of {known}")
        })
}

/// The wire spelling of one [`Capability`] — the inverse of
/// [`parse_capability`], and the only place other than [`KNOWN`] that
/// names these strings.
///
/// Written as an exhaustive `match` rather than a lookup through
/// [`KNOWN`] so that adding a [`Capability`] variant is a compile error
/// here (there is no spelling to guess and nothing sensible to return
/// for an unknown one), and so this function cannot fail. The two must
/// still agree, which is not left to a comment:
/// [`round_trips_every_capability_through_its_spelling`] below parses
/// every variant's spelling back and fails if any of them stopped
/// matching [`KNOWN`].
///
/// Used by [`crate::model::traffic_serving_spellings`] to build the two
/// `gatewayEndpoint` pairing errors, which must tell a recipe author a
/// spelling the parser actually accepts.
pub(crate) const fn capability_spelling(capability: Capability) -> &'static str {
    match capability {
        Capability::Admission => "admission",
        Capability::GatewayApi => "gatewayApi",
        Capability::LegacyIngress => "legacyIngress",
    }
}

/// The well-known label a Gateway API implementation applies to the
/// infrastructure it generates for one `Gateway`, carrying that
/// `Gateway`'s own name.
///
/// **Provenance, not invention.** Upstream Gateway API documents this
/// label under "Gateway infrastructure labels and annotations", and
/// Istio's own "Kubernetes Gateway API" task states that the `Service`
/// and `Deployment` it provisions for a `Gateway` are "generated with
/// well-known labels (`gateway.networking.k8s.io/gateway-name: <gateway
/// name>`)" and are named `<Gateway name>-<GatewayClass name>` in the
/// `Gateway`'s own namespace.
///
/// Defined here as a documented constant rather than hard-coded into a
/// resolver: nothing in `admissionlab-gateway` assumes this label, and
/// a recipe that wants it must write it out (see
/// [`GatewayEndpointStrategy`]'s "Prefer the selector to the name"
/// section for why it should). Admission Lab does not decide on a
/// vendor's behalf which label its data plane carries.
pub const GATEWAY_NAME_LABEL: &str = "gateway.networking.k8s.io/gateway-name";

/// Validates a recipe's `gatewayEndpoint:` block and resolves it into a
/// [`GatewayEndpointStrategy`].
///
/// A one-line delegate to
/// [`admissionlab_spec::resolve_gateway_endpoint`], which is where the
/// rules themselves live as of ROADMAP Task 6.11: `admissionlab.yaml`'s
/// own `gateway.gatewayEndpoint:` block reads the same YAML shape into
/// the same resolved type, and one vocabulary validated in two crates is
/// two validators free to drift. Kept as a named function here rather
/// than inlined at the call site so this crate's loader still reads as
/// "parse, then resolve" and so the recipe-level tests below keep
/// exercising the path a recipe actually takes.
///
/// # Errors
///
/// Returns `Err((locator, message))`, where `locator` is a dotted path
/// *relative to* `gatewayEndpoint` (for example `"namespace"` or
/// `"selector[\"app\"]"`) -- see
/// [`admissionlab_spec::resolve_gateway_endpoint`] for the full rule set.
pub(crate) fn resolve_gateway_endpoint(
    raw: &RawGatewayEndpoint,
) -> Result<GatewayEndpointStrategy, (String, String)> {
    admissionlab_spec::resolve_gateway_endpoint(raw)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn recognizes_every_known_capability() {
        assert_eq!(parse_capability("admission"), Ok(Capability::Admission));
        assert_eq!(parse_capability("gatewayApi"), Ok(Capability::GatewayApi));
        assert_eq!(
            parse_capability("legacyIngress"),
            Ok(Capability::LegacyIngress)
        );
    }

    #[test]
    fn rejects_unknown_capability_and_names_it_and_the_known_set() {
        let err = parse_capability("ingress").expect_err("\"ingress\" is not a known capability");
        assert!(err.contains("\"ingress\""));
        assert!(err.contains("\"admission\""));
        assert!(err.contains("\"gatewayApi\""));
        assert!(err.contains("\"legacyIngress\""));
    }

    /// [`capability_spelling`] and [`KNOWN`] name the same strings.
    /// Neither can drift without this failing, which is what lets
    /// `capability_spelling` be a hand-written `match` rather than a
    /// fallible lookup.
    #[test]
    fn round_trips_every_capability_through_its_spelling() {
        for (spelling, capability) in KNOWN {
            assert_eq!(
                capability_spelling(*capability),
                *spelling,
                "capability_spelling disagrees with KNOWN for {capability:?}"
            );
            assert_eq!(
                parse_capability(capability_spelling(*capability)),
                Ok(*capability)
            );
        }
    }

    #[test]
    fn is_case_sensitive() {
        assert!(parse_capability("Admission").is_err());
        assert!(parse_capability("GATEWAYAPI").is_err());
        assert!(parse_capability("gateway-api").is_err());
    }

    // -----------------------------------------------------------------
    // `resolve_gateway_endpoint` (Task 6.6). The YAML-level surface is
    // proven separately, through the public loader, in
    // `tests/gateway_endpoint.rs`; these cover the validation rules
    // directly.
    // -----------------------------------------------------------------

    fn selector(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn by_selector(pairs: &[(&str, &str)]) -> RawGatewayEndpoint {
        RawGatewayEndpoint::ServiceBySelector {
            namespace: "{gatewayNamespace}".to_owned(),
            selector: selector(pairs),
            port_name: Some("http".to_owned()),
            port: None,
        }
    }

    /// The shape `recipes/istio-gateway/recipe.yaml` (Task 6.10) is
    /// expected to use: the Gateway's own namespace, the upstream
    /// well-known gateway-name label, and a named port.
    #[test]
    fn the_istio_shaped_selector_strategy_resolves() {
        let resolved =
            resolve_gateway_endpoint(&by_selector(&[(GATEWAY_NAME_LABEL, "{gatewayName}")]))
                .expect("the well-known-label strategy must resolve");
        assert_eq!(
            resolved,
            GatewayEndpointStrategy::ServiceBySelector {
                namespace: "{gatewayNamespace}".to_owned(),
                selector: selector(&[(GATEWAY_NAME_LABEL, "{gatewayName}")]),
                port_name: Some("http".to_owned()),
                port: None,
            }
        );
    }

    #[test]
    fn an_unknown_placeholder_in_a_selector_value_is_rejected_at_load_time() {
        let (locator, message) =
            resolve_gateway_endpoint(&by_selector(&[(GATEWAY_NAME_LABEL, "{gateway}")]))
                .expect_err("{gateway} is not a known placeholder");
        assert_eq!(locator, format!("selector[{GATEWAY_NAME_LABEL:?}]"));
        assert!(message.contains("{gateway}"), "got: {message}");
    }

    #[test]
    fn an_unknown_placeholder_in_a_namespace_is_rejected() {
        let (locator, _) = resolve_gateway_endpoint(&RawGatewayEndpoint::ServiceByName {
            namespace: "{gatewayNamesapce}".to_owned(),
            name: "lab-gateway-istio".to_owned(),
            port_name: None,
            port: None,
        })
        .expect_err("a misspelled namespace placeholder must be rejected");
        assert_eq!(locator, "namespace");
    }

    #[test]
    fn a_placeholder_in_a_label_key_is_rejected_with_an_explanation() {
        let (locator, message) = resolve_gateway_endpoint(&by_selector(&[("{gatewayName}", "x")]))
            .expect_err("a label key must not be templated");
        assert_eq!(locator, "selector[\"{gatewayName}\"]");
        assert!(message.contains("only label values"), "got: {message}");
    }

    #[test]
    fn an_empty_selector_is_rejected() {
        let (locator, _) = resolve_gateway_endpoint(&by_selector(&[]))
            .expect_err("an empty selector is ambiguous");
        assert_eq!(locator, "selector");
    }

    #[test]
    fn empty_required_strings_are_rejected() {
        for (raw, expected) in [
            (
                RawGatewayEndpoint::ServiceByName {
                    namespace: "  ".to_owned(),
                    name: "svc".to_owned(),
                    port_name: None,
                    port: None,
                },
                "namespace",
            ),
            (
                RawGatewayEndpoint::ServiceByName {
                    namespace: "ns".to_owned(),
                    name: String::new(),
                    port_name: None,
                    port: None,
                },
                "name",
            ),
            (
                RawGatewayEndpoint::ServiceByName {
                    namespace: "ns".to_owned(),
                    name: "svc".to_owned(),
                    port_name: Some(" ".to_owned()),
                    port: None,
                },
                "portName",
            ),
        ] {
            let (locator, _) =
                resolve_gateway_endpoint(&raw).expect_err("an empty {expected} must be rejected");
            assert_eq!(locator, expected);
        }
    }

    #[test]
    fn port_zero_is_rejected_but_an_absent_port_is_not() {
        let (locator, _) = resolve_gateway_endpoint(&RawGatewayEndpoint::ServiceByName {
            namespace: "ns".to_owned(),
            name: "svc".to_owned(),
            port_name: None,
            port: Some(0),
        })
        .expect_err("port 0 is not a TCP port");
        assert_eq!(locator, "port");

        resolve_gateway_endpoint(&RawGatewayEndpoint::ServiceByName {
            namespace: "ns".to_owned(),
            name: "svc".to_owned(),
            port_name: None,
            port: None,
        })
        .expect("an absent port is how a recipe says \"resolve it from the Service\"");
    }
}
