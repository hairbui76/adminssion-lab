//! Obtaining [`Recipe`] values: the built-in set embedded into the
//! compiled binary, and an optional, explicitly-opted-into local
//! override directory.
//!
//! # Built-in recipes are embedded at compile time, not read from disk
//!
//! [`load_builtin_recipes`] reads every built-in recipe's YAML text from
//! [`BUILTIN_RECIPES`], a `const` array whose entries are `include_str!`
//! literals — the same mechanism, and the same reasoning,
//! `admissionlab_cluster::version::load_matrix` already uses for
//! `compatibility/kubernetes.yaml` (see that module's own documentation:
//! "Admission Lab never fetches this information over the network at
//! runtime... embedded into the compiled binary at build time"). Two
//! reasons this crate makes the same choice rather than reading a
//! `recipes/` directory from disk at runtime:
//!
//! - **A released binary must work without the repository present.**
//!   Admission Lab ships as a compiled binary (PRODUCT.md's packaging
//!   goals); a user running that binary has no guarantee a `recipes/`
//!   directory exists anywhere reachable from wherever they invoke it —
//!   there is no "current working directory" or installation-relative
//!   path that is reliably the *source repository* once the binary is
//!   built and distributed. Embedding removes the question entirely: the
//!   built-in set is whatever was compiled in, unconditionally.
//! - **Consistency with the project's own established precedent.**
//!   `compatibility/kubernetes.yaml` already answers the same shape of
//!   question ("a checked-in, reviewed file that changes Admission Lab's
//!   own behavior") the same way, for the same reason: "dropping a
//!   supported minor must require a deliberate, reviewed change to that
//!   checked-in file, never something that can silently drift." A
//!   built-in recipe is exactly that kind of checked-in, reviewed
//!   content — adding, changing, or dropping one is a source change and
//!   a rebuild, on purpose, not something a binary can pick up by itself
//!   at runtime.
//!
//! [`BUILTIN_RECIPES`] carried nothing at all through Task 2.5-2.7 (this
//! crate's own `lib.rs` module documentation). Task 2.8 added the first
//! real entry, the certified Kyverno recipe
//! (`recipes/kyverno/recipe.yaml`); Task 2.9 adds the second, the
//! certified Istio recipe (`recipes/istio/recipe.yaml`, `istio/istiod`
//! only — see that recipe's own README.md for why `istio/base` is
//! deliberately not a second entry here). Both install purely via Helm
//! (no `install.paths` at all), so neither ever hits `model.rs`'s
//! private `resolve_manifests` helper's relative-path rejection the way
//! a manifests-based recipe embedded as a built-in would —
//! `recipes/test-webhook/recipe.yaml`'s own header comment covers why a
//! manifests-based built-in is a harder, still-open problem neither
//! recipe needs to solve. Adding a further real entry is a one-line
//! addition to [`BUILTIN_RECIPES`] plus the new
//! `recipes/<name>/recipe.yaml` file it `include_str!`s — the loading
//! mechanism itself does not change.
//! [`load_builtin_recipes`] was already fully exercised end-to-end
//! before this task (see `tests/load.rs`): it parsed and resolved
//! whatever [`BUILTIN_RECIPES`] held, which before Task 2.8 was nothing,
//! proving the mechanism itself is correct independent of content —
//! `tests/kyverno_recipe.rs` is what now additionally proves real
//! content installs and behaves correctly against a live cluster.
//!
//! # A local override directory is never consulted implicitly
//!
//! [`load_recipe_overrides`] loads every recipe YAML file directly
//! inside a given directory. [`load_recipes`] is the combined entry
//! point: built-ins first, then — **only** when its `override_dir`
//! parameter is `Some` — everything [`load_recipe_overrides`] finds
//! there, merged in (see [`merge_recipes`]'s documentation for the merge
//! rule).
//!
//! Nothing in this module ever reads an environment variable, a home or
//! XDG directory, or any other ambient default location to discover an
//! override directory on its own — grepping this crate for `env::var`,
//! `home_dir`, or a hardcoded path finds nothing, and
//! `tests/load.rs`'s `override_directory_is_never_consulted_via_the_current_working_directory`
//! test proves it empirically (seeding every plausible implicit-default
//! location, including the current working directory, with a fully
//! valid recipe, and confirming `load_recipes(None)` still returns
//! nothing). This is deliberate, not an oversight: PRODUCT.md §29.1
//! treats everything a recipe causes to be installed into a cluster as
//! untrusted, and a recipe controls *what gets installed* — silently
//! picking up recipes from an implicit location would let a directory a
//! user never intentionally pointed Admission Lab at influence what runs
//! in a cluster it creates. The only way to opt in is for a caller (a
//! later task's CLI, presumably via an explicit flag) to obtain a
//! directory path from a source **under the user's own explicit
//! control** and pass it here directly — this crate never discovers that
//! path itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::{RawRecipe, Recipe, RecipeError, resolve_recipe};

/// Built-in recipes, embedded into the compiled binary at compile time.
///
/// Each entry is `(label, yaml)`: `label` is a diagnostic name only
/// (used in [`RecipeError`] messages), never a filesystem path — these
/// bytes are compiled in, not read at runtime, so there is no path a
/// running binary could re-read even if it wanted to. It is written to
/// *look like* the source file's real repository-relative path purely
/// because that is the clearest possible diagnostic, not because
/// anything here treats it as one. See this module's own documentation
/// for what a future recipe adds here.
const BUILTIN_RECIPES: &[(&str, &str)] = &[
    (
        "recipes/kyverno/recipe.yaml",
        include_str!("../../../recipes/kyverno/recipe.yaml"),
    ),
    (
        "recipes/istio/recipe.yaml",
        include_str!("../../../recipes/istio/recipe.yaml"),
    ),
];

/// Loads every built-in recipe embedded into this binary (see this
/// module's documentation).
///
/// # Errors
///
/// Returns [`RecipeError::Parse`] or [`RecipeError::Validation`] if an
/// embedded recipe's checked-in YAML text is not a valid recipe — in
/// practice this can only follow a bad hand-edit to a checked-in
/// `recipes/*.yaml` file, since the embedded copy this function reads
/// never changes at runtime.
pub fn load_builtin_recipes() -> Result<Vec<Recipe>, RecipeError> {
    BUILTIN_RECIPES
        .iter()
        .map(|(label, yaml)| parse_recipe(label, yaml))
        .collect()
}

fn parse_recipe(source_label: &str, yaml: &str) -> Result<Recipe, RecipeError> {
    let raw: RawRecipe = serde_norway::from_str(yaml).map_err(|source| RecipeError::Parse {
        source_label: source_label.to_owned(),
        source,
    })?;
    resolve_recipe(source_label, raw)
}

/// Loads every recipe YAML file (`.yaml`/`.yml`, by extension) directly
/// inside `dir` — not recursively — sorted by file name for
/// deterministic ordering. A file with any other extension, and a
/// subdirectory, are silently skipped: neither one is a candidate recipe
/// document to begin with, so there is nothing to reject.
///
/// This function is never called implicitly by anything else in this
/// crate — see this module's own documentation ("explicit opt-in").
///
/// # Errors
///
/// Returns [`RecipeError::Io`] if `dir` cannot be read (including if it
/// does not exist) or if one of its recipe files cannot be read.
/// Returns [`RecipeError::Parse`] or [`RecipeError::Validation`] if a
/// recipe file's contents are invalid. Returns
/// [`RecipeError::DuplicateOverrideName`] if two files in `dir` declare
/// the same [`Recipe::name`] — see that variant's documentation for why
/// this is rejected rather than resolved by file order.
pub fn load_recipe_overrides(dir: &Path) -> Result<Vec<Recipe>, RecipeError> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|source| RecipeError::Io {
            path: dir.to_path_buf(),
            source,
        })?
        .filter_map(std::io::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && matches!(
                    path.extension().and_then(std::ffi::OsStr::to_str),
                    Some("yaml" | "yml")
                )
        })
        .collect();
    paths.sort();

    let mut recipes = Vec::with_capacity(paths.len());
    let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
    for path in paths {
        let text = std::fs::read_to_string(&path).map_err(|source| RecipeError::Io {
            path: path.clone(),
            source,
        })?;
        let source_label = path.display().to_string();
        let recipe = parse_recipe(&source_label, &text)?;

        if let Some(first) = seen.get(&recipe.name) {
            return Err(RecipeError::DuplicateOverrideName {
                name: recipe.name,
                first: first.clone(),
                second: path,
            });
        }
        seen.insert(recipe.name.clone(), path);
        recipes.push(recipe);
    }
    Ok(recipes)
}

/// Loads every built-in recipe, then — only when `override_dir` is
/// `Some` — loads and merges in every recipe from that directory. See
/// this module's documentation for why an override directory is never
/// consulted unless a caller explicitly names one here.
///
/// # Errors
///
/// Returns whatever [`load_builtin_recipes`] or (when `override_dir` is
/// `Some`) [`load_recipe_overrides`] returns.
pub fn load_recipes(override_dir: Option<&Path>) -> Result<Vec<Recipe>, RecipeError> {
    let builtins = load_builtin_recipes()?;
    let overrides = match override_dir {
        Some(dir) => load_recipe_overrides(dir)?,
        None => Vec::new(),
    };
    Ok(merge_recipes(builtins, overrides))
}

/// Merges a built-in recipe set with an explicitly loaded override set:
/// an override recipe *replaces* a built-in recipe of the same
/// [`Recipe::name`] (an intentional, well-defined act — the whole point
/// of an override directory is letting a caller substitute a recipe's
/// definition); a name unique to `overrides` is added alongside the
/// built-ins. The result is sorted by name, for a deterministic result
/// independent of either input's own order.
///
/// Pure and independent of where either `Vec` came from, so it is
/// directly testable against hand-built [`Recipe`] values without
/// needing a real built-in recipe to exist yet — see this file's own
/// `tests` module below, which is exactly how
/// "override replaces a built-in of the same name" is proven today, with
/// [`BUILTIN_RECIPES`] still empty.
fn merge_recipes(builtins: Vec<Recipe>, overrides: Vec<Recipe>) -> Vec<Recipe> {
    let mut by_name: BTreeMap<String, Recipe> =
        builtins.into_iter().map(|r| (r.name.clone(), r)).collect();
    for recipe in overrides {
        by_name.insert(recipe.name.clone(), recipe);
    }
    by_name.into_values().collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use admissionlab_spec::{InstallMethod, ManifestInstallSpec};

    use super::*;

    fn stub_recipe(name: &str, version: &str) -> Recipe {
        Recipe {
            name: name.to_owned(),
            version: version.to_owned(),
            install: InstallMethod::Manifests(ManifestInstallSpec {
                paths: vec![PathBuf::from("/tmp/admissionlab-recipes-stub.yaml")],
            }),
            readiness: Vec::new(),
            normalize_rules: Vec::new(),
            capabilities: BTreeSet::new(),
        }
    }

    #[test]
    fn override_replaces_a_builtin_of_the_same_name() {
        let builtins = vec![stub_recipe("kyverno", "3.8.2")];
        let overrides = vec![stub_recipe("kyverno", "3.9.0")];

        let merged = merge_recipes(builtins, overrides);

        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].version, "3.9.0",
            "an override must replace a built-in recipe of the same name"
        );
    }

    #[test]
    fn override_with_a_new_name_is_added_alongside_builtins() {
        let builtins = vec![stub_recipe("kyverno", "3.9.0")];
        let overrides = vec![stub_recipe("istio", "1.30.4")];

        let merged = merge_recipes(builtins, overrides);

        let names: Vec<&str> = merged.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["istio", "kyverno"], "sorted by name");
    }

    #[test]
    fn no_overrides_returns_exactly_the_builtin_set() {
        let builtins = vec![stub_recipe("kyverno", "3.9.0")];

        let merged = merge_recipes(builtins.clone(), Vec::new());

        assert_eq!(merged, builtins);
    }
}
