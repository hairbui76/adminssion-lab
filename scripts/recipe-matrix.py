#!/usr/bin/env python3
"""Generate the recipe-certification CI job matrix from the checked-in
compatibility metadata.

ROADMAP Task 7.5 step 1: "Generate CI matrix from checked-in
compatibility YAML, not duplicated hardcoded arrays."
`.github/workflows/recipe-matrix.yml` runs this once and feeds the JSON
it prints to `fromJSON`, so the set of certified combinations CI runs is
the set `compatibility/recipes.yaml` records -- one place, not two that
can disagree.

Usage:

    scripts/recipe-matrix.py --tier perCommit
    scripts/recipe-matrix.py --tier nightly --format github
    scripts/recipe-matrix.py --tier weeklyRelease --pretty

Options:

    --tier TIER     Which tier's job matrix to emit. Tiers are
                    cumulative downward, exactly as
                    `admissionlab_recipes::CertificationTier` documents:
                    `nightly` emits its own rows *and* every `perCommit`
                    row, and `weeklyRelease` emits all three tiers'.
                    A tier's job matrix is therefore what that schedule
                    actually runs, not only what is new at it.
    --format        `json` (default) prints the matrix object;
                    `github` prints `name=value` lines for
                    `$GITHUB_OUTPUT` (`include` and `count`).
    --pretty        Indent the JSON. For reading it by hand; the
                    workflow never needs it.
    --root DIR      Repository root. Defaults to this script's parent
                    directory, so the script works from any cwd.

Output shape, per matrix entry:

    {
      "name":           "kyverno 3.9.0 / Kubernetes 1.35.8",
      "recipe":         "kyverno",
      "recipe_version": "3.9.0",
      "kubernetes":     "1.35.8",
      "tier":           "perCommit",
      "package":        "admissionlab-recipes",
      "test":           "kyverno_recipe",
      "node_image":     "kindest/node:v1.35.8@sha256:...",
      "node_digest":    "sha256:..."
    }

`node_image` is joined here rather than in the workflow because the
digest lives in `compatibility/kubernetes.yaml` and the version lives in
`compatibility/recipes.yaml`; a workflow that looked one of them up in
YAML with `grep` would be the duplicated hardcoded array this script
exists to avoid. It is also what makes the node-image cache key in
`recipe-matrix.yml` content-addressed rather than name-addressed.

This script reads two checked-in files and writes nothing. It makes no
network call and needs no cluster: it is safe to run locally, and
`recipe-matrix.yml` runs it in a job with no Docker at all.
"""

import argparse
import json
import pathlib
import sys

try:
    import yaml
except ImportError:  # pragma: no cover - environment problem, not logic
    sys.exit(
        "recipe-matrix.py: PyYAML is not importable. GitHub's ubuntu runner "
        "images ship it (python3-yaml, a cloud-init dependency); on a "
        "machine that does not, install it with "
        "`python3 -m pip install pyyaml` or `apt-get install python3-yaml`."
    )

# Tiers, most frequent first. Mirrors
# `admissionlab_recipes::CertificationTier`'s own declaration order,
# which is its `Ord` -- see that enum's documentation for why the sets
# are cumulative downward. A tier spelling that appears here and not
# there (or the reverse) is caught by
# `crates/admissionlab-recipes/tests/compatibility.rs`'s
# `every_tier_spelling_in_the_file_parses_back_to_its_variant`, which
# asserts the same three strings from the Rust side.
TIERS = ["perCommit", "nightly", "weeklyRelease"]

# Which `#[ignore]`d certification test proves each recipe. Deliberately
# CI knowledge and not a field in `compatibility/recipes.yaml`: that file
# records what is certified, and which Rust test binary happens to
# perform the certification is a property of this repository's test
# layout, which changes for reasons that have nothing to do with a
# support matrix. A recipe row with no entry here is a hard error rather
# than a skipped job -- a certified combination no job runs is exactly
# what Task 7.5 exists to make impossible.
TESTS = {
    "kyverno": ("admissionlab-recipes", "kyverno_recipe"),
    "istio": ("admissionlab-recipes", "istio_recipe"),
    "istio-gateway": ("admissionlab-recipes", "istio_gateway_recipe"),
}


def load(root, name):
    path = root / "compatibility" / name
    try:
        with path.open(encoding="utf-8") as handle:
            return yaml.safe_load(handle)
    except OSError as error:
        sys.exit(f"recipe-matrix.py: cannot read {path}: {error}")
    except yaml.YAMLError as error:
        sys.exit(f"recipe-matrix.py: {path} is not valid YAML: {error}")


def node_images(kubernetes):
    """Maps each supported Kubernetes patch version to its digest-pinned
    `kind` node image.

    Only `supported: true` releases are included: a certified row naming
    an unsupported version has no image to run on, and failing here with
    that message is better than emitting a matrix entry that cannot
    create a cluster. `admissionlab_recipes::validate_compatibility`
    rejects that same combination from the Rust side; this is the CI-side
    half of the one rule.
    """
    images = {}
    for release in kubernetes.get("releases") or []:
        if not release.get("supported"):
            continue
        image, digest = release.get("image"), release.get("digest")
        if not image or not digest:
            sys.exit(
                "recipe-matrix.py: compatibility/kubernetes.yaml entry "
                f"{release.get('version')!r} is missing image or digest"
            )
        # Never a floating tag: the reference carries both the exact
        # patch tag and the digest that pins it, exactly as
        # `admissionlab_cluster::resolve_node_image` builds it.
        images[release["version"]] = (f"{image}@{digest}", digest)
    return images


def entries(recipes, images, tier):
    cutoff = TIERS.index(tier)
    matrix = []
    for entry in recipes.get("recipes") or []:
        name, version = entry.get("name"), entry.get("version")
        certified = ((entry.get("kubernetes") or {}).get("certified")) or []
        for row in certified:
            if not isinstance(row, dict):
                sys.exit(
                    "recipe-matrix.py: compatibility/recipes.yaml entry "
                    f"{name!r} has a bare certified version {row!r}; every "
                    "certified version carries a tier (ROADMAP Task 7.4)"
                )
            row_tier = row.get("tier")
            if row_tier not in TIERS:
                sys.exit(
                    f"recipe-matrix.py: {name!r} certifies "
                    f"{row.get('version')!r} at unknown tier {row_tier!r}; "
                    f"expected one of {', '.join(TIERS)}"
                )
            if TIERS.index(row_tier) > cutoff:
                continue
            if name not in TESTS:
                sys.exit(
                    f"recipe-matrix.py: no certification test is registered "
                    f"for recipe {name!r}. Add it to TESTS in this script, or "
                    "the row would be certified by nothing."
                )
            kubernetes = row.get("version")
            if kubernetes not in images:
                sys.exit(
                    f"recipe-matrix.py: {name!r} certifies Kubernetes "
                    f"{kubernetes!r}, which compatibility/kubernetes.yaml does "
                    "not mark supported: true"
                )
            image, digest = images[kubernetes]
            package, test = TESTS[name]
            matrix.append(
                {
                    "name": f"{name} {version} / Kubernetes {kubernetes}",
                    "recipe": name,
                    "recipe_version": version,
                    "kubernetes": kubernetes,
                    "tier": row_tier,
                    "package": package,
                    "test": test,
                    "node_image": image,
                    "node_digest": digest,
                }
            )
    return matrix


def main():
    default_root = pathlib.Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(add_help=True, description=__doc__)
    parser.add_argument("--tier", required=True, choices=TIERS)
    parser.add_argument("--format", default="json", choices=["json", "github"])
    parser.add_argument("--pretty", action="store_true")
    parser.add_argument("--root", type=pathlib.Path, default=default_root)
    args = parser.parse_args()

    root = args.root.resolve()
    matrix = entries(
        load(root, "recipes.yaml"),
        node_images(load(root, "kubernetes.yaml")),
        args.tier,
    )
    if not matrix:
        sys.exit(
            f"recipe-matrix.py: tier {args.tier!r} selects no certified "
            "combination at all. A tier whose job matrix is empty would "
            "report a green certification run having certified nothing."
        )

    payload = json.dumps(matrix, indent=2 if args.pretty else None, sort_keys=False)
    if args.format == "github":
        # One line each, no newlines inside the JSON (hence no `--pretty`
        # interaction): `$GITHUB_OUTPUT` is a `name=value` file, and a
        # multi-line value needs heredoc syntax this deliberately avoids.
        print(f"include={json.dumps(matrix)}")
        print(f"count={len(matrix)}")
    else:
        print(payload)


main()
