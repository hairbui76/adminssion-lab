//! Recipe-specific capability *logic*: turning the plain strings a
//! recipe YAML document writes under `capabilities:` into
//! [`admissionlab_spec::Capability`] values.
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

use admissionlab_spec::Capability;

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

#[cfg(test)]
mod tests {
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

    #[test]
    fn is_case_sensitive() {
        assert!(parse_capability("Admission").is_err());
        assert!(parse_capability("GATEWAYAPI").is_err());
        assert!(parse_capability("gateway-api").is_err());
    }
}
