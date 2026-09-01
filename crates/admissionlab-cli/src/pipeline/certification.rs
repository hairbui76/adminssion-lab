//! ROADMAP Task 7.4 step 3: warn — and only ever warn — when a lab asks
//! for a recipe/version combination Admission Lab has not certified on
//! the Kubernetes version it requested.
//!
//! # This never refuses a run
//!
//! Global Constraint 6 makes the core vendor-neutral, and PRODUCT.md's
//! recipe model makes curated recipes a *convenience* over the generic
//! component model, not a gate. A user-defined stack — a private
//! webhook, an unreleased chart, a fork of Kyverno — is a first-class
//! input to `admissionlab test`, and the roadmap says so in as many
//! words: "do not refuse generic user-defined stacks." So everything
//! this module produces is a [`Diagnostic`] plus a console line. Nothing
//! here returns a [`crate::pipeline::RunDisposition`], and nothing here
//! can change one.
//!
//! What the warning buys is the other half of the same honesty rule: a
//! run on an uncertified combination is exactly as real as any other
//! run, but the *stack* under it has not been proven by this repository's
//! own certification tests, and a report that never said so would let a
//! reader assume it had been (the Phase 7 gate's "docs never call a
//! non-certified user combination supported/certified" applied to the
//! tool's own output).
//!
//! # What is checked, and what is deliberately not
//!
//! A component is matched against `compatibility/recipes.yaml` **by
//! name** — [`admissionlab_spec::ResolvedComponent`] carries no recipe
//! reference (`ComponentSpec::recipe` is still carried through
//! unresolved; recipe-driven installation does not exist yet), so the
//! component's own name is the only link between a lab document and a
//! certified recipe, and it is the link `compatibility/recipes.yaml`'s
//! own header already documents (`name:` "matches `Recipe::name`").
//!
//! That gives three cases, and only the middle one warns:
//!
//! - **The name is not in the matrix at all** — `my-webhook`,
//!   `some-fork`. Silent. This is the generic user-defined stack the
//!   roadmap protects, and warning about every component Admission Lab
//!   ships no recipe for would make the warning worthless within a week:
//!   the signal would be "you are using this tool as designed."
//! - **The name is one Admission Lab certifies, but not at this
//!   version/Kubernetes pair.** Warns. This is the actionable case, and
//!   the one a user is most likely to have reached by accident — running
//!   the certified Kyverno chart on Admission Lab's Tier-1 primary
//!   Kubernetes version, say, which Kyverno's own documented window does
//!   not reach.
//! - **The combination is certified.** Silent.
//!
//! The check is additionally gated on the requested Kubernetes version
//! being *supported* (ROADMAP step 3's own wording: "supported Kubernetes
//! but not certified recipe matrix"). An unsupported version is not a
//! certification question at all — `resolve_node_image` refuses it
//! outright when the cluster is created, with a message about that, and a
//! second, quieter warning about certification would only compete with
//! it.
//!
//! # Both sides, one warning
//!
//! Baseline and candidate usually name the same component at the same
//! version, and printing the identical sentence twice makes a real
//! warning look like a rendering bug. Combinations are therefore
//! deduplicated, with every side that requested one recorded in the
//! diagnostic's `sides` context — the same information, said once.

use std::collections::BTreeMap;

use admissionlab_core::{Diagnostic, RedactedValue, Side};
use admissionlab_recipes::{RecipeCompatibilityMatrix, load_recipe_compatibility};
use admissionlab_spec::{ResolvedEnvironment, ResolvedLab};

/// The diagnostic code every warning this module raises carries.
pub const UNCERTIFIED_CODE: &str = "compatibility.uncertified_combination";

/// Every uncertified-combination warning `lab` earns, in a
/// deterministic order.
///
/// Empty for a lab whose components are all certified, all
/// user-defined, or all on an unsupported Kubernetes version — see this
/// module's documentation for why each of those three is silent.
///
/// Both checked-in matrices are embedded in this binary at compile time
/// (`compatibility/recipes.yaml` through `admissionlab-recipes`,
/// `compatibility/kubernetes.yaml` through `admissionlab-cluster`), so
/// this function reads no file, spawns no process, and cannot fail for
/// any reason the user can act on. If either embedded matrix somehow
/// does not parse, this returns no warnings rather than ending the run:
/// a broken build artifact is not the user's lab's problem, both crates'
/// own test suites fail on it long before a release, and refusing to run
/// a lab over it would turn an advisory into exactly the refusal Global
/// Constraint 6 forbids.
#[must_use]
pub fn uncertified_combinations(lab: &ResolvedLab) -> Vec<Diagnostic> {
    let (Ok(certified), Ok(kubernetes)) = (
        load_recipe_compatibility(),
        admissionlab_cluster::load_matrix(),
    ) else {
        return Vec::new();
    };
    let supported: Vec<&str> = kubernetes
        .releases
        .iter()
        .filter(|release| release.supported)
        .map(|release| release.version.as_str())
        .collect();

    // Keyed so that the same combination requested by both sides is one
    // warning; `BTreeMap` so the order is the same on every run over the
    // same lab (Global Constraint 7's determinism applies to what a run
    // reports about itself, not only to its findings).
    let mut uncertified: BTreeMap<(String, String, String), Vec<Side>> = BTreeMap::new();
    for (side, environment) in [
        (Side::Baseline, &lab.baseline),
        (Side::Candidate, &lab.candidate),
    ] {
        collect_side(side, environment, &certified, &supported, &mut uncertified);
    }

    uncertified
        .into_iter()
        .map(|((kubernetes, recipe, version), sides)| {
            diagnostic(&kubernetes, &recipe, &version, &sides, &certified)
        })
        .collect()
}

/// Adds one side's uncertified combinations to `uncertified`.
fn collect_side(
    side: Side,
    environment: &ResolvedEnvironment,
    certified: &RecipeCompatibilityMatrix,
    supported: &[&str],
    uncertified: &mut BTreeMap<(String, String, String), Vec<Side>>,
) {
    if !supported.contains(&environment.kubernetes.as_str()) {
        return;
    }
    for component in &environment.components {
        // Not a recipe Admission Lab certifies at all: a generic,
        // user-defined stack, which is first-class and silent.
        if certified.entry(&component.name).is_none() {
            continue;
        }
        if certified.certifies(&component.name, &component.version, &environment.kubernetes) {
            continue;
        }
        uncertified
            .entry((
                environment.kubernetes.clone(),
                component.name.clone(),
                component.version.clone(),
            ))
            .or_default()
            .push(side);
    }
}

/// Builds one warning, naming what *is* certified for that recipe so the
/// reader can act on it without opening `compatibility/recipes.yaml`.
fn diagnostic(
    kubernetes: &str,
    recipe: &str,
    version: &str,
    sides: &[Side],
    certified: &RecipeCompatibilityMatrix,
) -> Diagnostic {
    let known: Vec<String> = certified
        .certified_combinations()
        .into_iter()
        .filter(|combination| combination.recipe == recipe)
        .map(|combination| {
            format!(
                "{} {} on Kubernetes {}",
                combination.recipe, combination.recipe_version, combination.kubernetes
            )
        })
        .collect();
    let sides_rendered = sides
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");

    let mut context = BTreeMap::new();
    context.insert(
        "recipe".to_owned(),
        RedactedValue::Public(recipe.to_owned()),
    );
    context.insert(
        "recipeVersion".to_owned(),
        RedactedValue::Public(version.to_owned()),
    );
    context.insert(
        "kubernetes".to_owned(),
        RedactedValue::Public(kubernetes.to_owned()),
    );
    context.insert(
        "sides".to_owned(),
        RedactedValue::Public(sides_rendered.clone()),
    );

    Diagnostic {
        code: UNCERTIFIED_CODE.to_owned(),
        message: format!(
            "warning: {sides_rendered} requests {recipe} {version} on Kubernetes {kubernetes}, \
             which Admission Lab does not certify. The run continues and its comparison is as \
             real as any other — user-defined stacks are supported — but this combination is not \
             covered by Admission Lab's own certification tests. Certified: {}.",
            if known.is_empty() {
                "nothing for this recipe".to_owned()
            } else {
                known.join("; ")
            }
        ),
        context,
    }
}
