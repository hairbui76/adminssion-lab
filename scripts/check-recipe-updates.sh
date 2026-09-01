#!/usr/bin/env bash
#
# Reports which certified recipe pins are behind their upstream (ROADMAP
# Task 10.5 step 3). It NEVER edits a recipe, a pin, or
# `compatibility/recipes.yaml`.
#
# Usage:
#   ./scripts/check-recipe-updates.sh
#   ./scripts/check-recipe-updates.sh --format markdown
#   ./scripts/check-recipe-updates.sh --format markdown --output /tmp/report.md
#
# # A newer version is news, not a certification
#
# This is the whole point of the script and the reason it produces a
# report instead of a commit. `compatibility/recipes.yaml` says what a
# certified row is: "every row below has actually been installed and
# verified by a test in this repository". A chart version existing on a
# Helm index is not that. It is the *prompt* to go and run the
# certification, and until `.github/workflows/recipe-matrix.yml` has
# installed the new pin on every Kubernetes version its row claims, the
# new version is uncertified — no matter how many days it has been out.
#
# So the output of this script is a list of candidates for a human, and
# the automation built on it (`.github/workflows/maintenance.yml`) opens
# a pull request that a human reviews and a certification run proves.
# Nothing here bumps a pin, and nothing downstream of here merges
# without the recipe suite passing on the new pin.
#
# # What it reads, and how it knows
#
# Nothing below is a hardcoded list of upstreams. Every source is
# derived from the checked-in files, so a recipe added to
# `compatibility/recipes.yaml` is checked by this script without editing
# this script:
#
#   compatibility/recipes.yaml
#       Which recipes are certified at all, and at which pinned version.
#       A recipe not certified here is not checked: an uncertified
#       directory has no pin anyone promised to keep current.
#   recipes/<name>/recipe.yaml
#       The `install:` block. `type: helm` with an `https://` repo is
#       looked up in that repo's `index.yaml`; with an `oci://` repo it
#       is looked up through the OCI registry's own tag list, because an
#       OCI Helm repository publishes no index.yaml at all.
#   recipes/<name>/*.yaml (other components)
#       A vendored `type: manifests` component carries its upstream
#       provenance as a release-download URL in its header comment
#       (`https://github.com/<owner>/<repo>/releases/download/<tag>/...`).
#       That URL is the source: owner and repo go to the GitHub releases
#       API.
#
# # Derived pins are reported, and never counted as behind
#
# Both Gateway API CRD bundles are pinned to the release their *own
# implementation* builds against — `recipes/istio-gateway/gateway-api-crds.yaml`
# says it outright: "When `recipes/istio/recipe.yaml`'s chart pin moves,
# this pin is re-derived from that release's own `go.mod` -- not bumped
# independently." A newer Gateway API release is therefore not an update
# that is due here; bumping it on its own would install an API version
# the pinned istiod/NGF was never compiled against.
#
# Those components are reported under "informational" and do not set the
# "an update is available" exit code. Reporting them anyway is the
# honest half: a reviewer looking at an Istio bump needs to know which
# Gateway API release that Istio now builds against, and this is where
# they find out that upstream has moved at all.
#
# # Registries it cannot reach
#
# A source that does not answer is reported as `unknown`, by name, with
# the command a human can run by hand — never silently skipped and never
# rendered as "up to date". Global Constraint 15: missing data is
# unknown, never fabricated. One unreachable registry does not fail the
# run (the other recipes' answers are still worth having); nothing
# answering at all is exit 3, because a report where every row says
# `unknown` is not a report.
#
# The OCI path is the one most likely to end up there. Listing tags in
# an OCI registry needs an anonymous pull token and the Docker Registry
# v2 API, both of which a registry may rate-limit or refuse; when that
# happens this script says so and prints the `helm show chart` command
# that answers the narrower question a human actually has.
#
# # Network
#
# Like `scripts/update-kubernetes-matrix.sh`, this talks to the network
# on purpose and is not part of any lab run: nothing in the CLI, the
# recipes crate, or the cluster crate ever fetches this data.
#
# # Exit codes
#
#   0   Every certified pin this script could check is the newest
#       upstream release. Nothing to propose.
#   2   Bad arguments, or a required tool is missing.
#   3   Network required and unavailable -- nothing answered at all.
#   4   The report itself could not be built: the checked-in files or a
#       fetched document were not shaped the way this script requires.
#       Distinct from 3 the same way `update-kubernetes-matrix.sh`
#       distinguishes them -- "go fix something" rather than "get on a
#       network".
#   10  At least one certified pin is BEHIND upstream. Deliberately not
#       1, for the same reason `update-kubernetes-matrix.sh` uses 10: a
#       difference is this script's normal, successful outcome, and a
#       caller must be able to tell it apart from a failure.

set -euo pipefail

readonly REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

FORMAT="text"
OUTPUT=""

usage() {
  echo "usage: $0 [--format text|markdown] [--output PATH]" >&2
  echo "       reports certified recipe pins that are behind upstream; never edits one" >&2
}

fail() {
  echo "check-recipe-updates: FAIL $1" >&2
  exit "$2"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --format)
      [ "$#" -ge 2 ] || { usage; exit 2; }
      FORMAT="$2"
      case "${FORMAT}" in
        text|markdown) ;;
        *) echo "check-recipe-updates: unknown format '${FORMAT}'" >&2; usage; exit 2 ;;
      esac
      shift 2
      ;;
    --output)
      [ "$#" -ge 2 ] || { usage; exit 2; }
      OUTPUT="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "check-recipe-updates: unknown argument '$1'" >&2
      usage
      exit 2
      ;;
  esac
done

for tool in curl python3; do
  command -v "${tool}" >/dev/null 2>&1 \
    || fail "'${tool}' is required but was not found on PATH" 2
done

WORK_DIR="$(mktemp -d)"
# Removed on every exit path. A half-downloaded Helm index is not
# evidence of anything, and must not be left behind to be mistaken for
# one.
trap 'rm -rf "${WORK_DIR}"' EXIT

# ---------------------------------------------------------------------
# Phase 1: read the checked-in files and emit the fetch plan.
#
# Split into plan/fetch/report rather than done in one Python pass so
# that every network call is a `curl` with the same timeouts, the same
# `--fail` behavior and the same honest exit-code mapping as
# `scripts/update-kubernetes-matrix.sh` -- one fetching convention in
# this repository, not two.
#
# Plan lines are tab-separated:
#   kind  recipe  component  pin  id  arg1  arg2
# ---------------------------------------------------------------------
python3 - "${REPO_ROOT}" >"${WORK_DIR}/plan.tsv" <<'PYTHON'
import pathlib
import re
import sys

try:
    import yaml
except ImportError:  # pragma: no cover - environment problem, not logic
    sys.exit(
        "check-recipe-updates: PyYAML is not importable. GitHub's ubuntu "
        "runner images ship it (python3-yaml, a cloud-init dependency); on "
        "a machine that does not, install it with "
        "`python3 -m pip install pyyaml` or `apt-get install python3-yaml`."
    )

root = pathlib.Path(sys.argv[1])


def die(message, code=2):
    print(f"check-recipe-updates: FAIL {message}", file=sys.stderr)
    raise SystemExit(code)


compat_path = root / "compatibility" / "recipes.yaml"
try:
    compat = yaml.safe_load(compat_path.read_text(encoding="utf-8"))
except (OSError, yaml.YAMLError) as error:
    die(f"cannot read {compat_path}: {error}")

entries = (compat or {}).get("recipes") or []
if not entries:
    die(f"{compat_path} certifies no recipes at all")

# Deduplicated in file order: this file's header allows several entries
# per recipe name over time ("append a new entry; do not edit an old
# one in place"), and the pin whose currency is in question is the
# newest one -- which is the last entry for that name.
certified = {}
for entry in entries:
    name, version = entry.get("name"), entry.get("version")
    if isinstance(name, str) and isinstance(version, str):
        certified[name] = version

plan = []
for name, pin in certified.items():
    directory = root / "recipes" / name
    if not directory.is_dir():
        die(f"{compat_path} certifies '{name}' but {directory} does not exist")

    # Every `*.yaml` directly in the directory is a recipe document --
    # the same rule `admissionlab_recipes::load_recipe_overrides`
    # applies, and the reason the vendored CRD bundles live in a
    # subdirectory rather than beside their component file.
    for path in sorted(directory.glob("*.yaml")):
        try:
            document = yaml.safe_load(path.read_text(encoding="utf-8"))
        except (OSError, yaml.YAMLError) as error:
            die(f"cannot read {path}: {error}")
        if not isinstance(document, dict):
            continue
        component = document.get("name") or path.stem
        install = document.get("install") or {}
        component_pin = document.get("version") or ""
        identifier = f"{name}--{path.stem}"

        if install.get("type") == "helm":
            repo = (install.get("repo") or "").rstrip("/")
            chart = (install.get("chart") or "").split("/")[-1]
            component_pin = install.get("version") or component_pin
            if not repo or not chart:
                die(f"{path} declares a helm install with no repo or chart")
            if repo.startswith("oci://"):
                # An OCI Helm repository publishes no index.yaml: the
                # chart versions ARE the artifact's tags, so the tag
                # list is the only listing that exists.
                target = f"{repo[len('oci://'):]}/{chart}"
                registry, _, repository = target.partition("/")
                plan.append(("oci", name, component, component_pin, identifier, registry, repository))
            else:
                plan.append(("helm", name, component, component_pin, identifier, f"{repo}/index.yaml", chart))
            continue

        if install.get("type") == "manifests":
            # A vendored bundle records its upstream as a release
            # download URL in its header comment; that provenance is the
            # source of truth for where it came from, and this reads it
            # rather than keeping a second copy of the mapping here.
            text = path.read_text(encoding="utf-8")
            match = re.search(
                r"https://github\.com/([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)/releases/download/",
                text,
            )
            if not match:
                # Not every manifests component is vendored from a
                # GitHub release (`recipes/test-webhook/` is local
                # material with no upstream at all). Silence here is
                # correct: there is nothing to check, not something
                # unknown.
                continue
            owner, repo_name = match.group(1), match.group(2)
            plan.append(
                ("ghrelease", name, component, component_pin, identifier, f"{owner}/{repo_name}", "derived")
            )

if not plan:
    die("no certified recipe declares an upstream this script knows how to check")

for row in plan:
    print("\t".join(row))
PYTHON

# ---------------------------------------------------------------------
# Phase 2: fetch. One `curl` convention, three source shapes.
# ---------------------------------------------------------------------

# Fetches `$1` into `$2`, recording the outcome in `$3` as one word.
# Never fails the script: a single unreachable registry is a row that
# says `unknown`, not a dead run. The two failures are recorded
# separately because they mean different things -- curl exit 22 is "the
# host answered and refused" (a renamed endpoint, a rate limit),
# anything else is "nothing answered" (offline).
fetch_into() {
  local url="$1" destination="$2" status_file="$3" status=0
  curl --fail --silent --show-error --location \
       --connect-timeout 15 --max-time 60 \
       -H "Accept: application/json, application/vnd.oci.image.index.v1+json, */*" \
       -o "${destination}" "${url}" 2>"${WORK_DIR}/curl.err" || status=$?
  if [ "${status}" -eq 0 ]; then
    echo "ok" >"${status_file}"
    return 0
  fi
  if [ "${status}" -eq 22 ]; then
    echo "refused" >"${status_file}"
  else
    echo "unreachable" >"${status_file}"
  fi
  sed 's/^/  /' "${WORK_DIR}/curl.err" >&2 || true
  return 0
}

echo "check-recipe-updates: querying upstream for the certified recipe pins" >&2

while IFS=$'\t' read -r kind recipe component pin identifier arg1 arg2; do
  [ -n "${kind}" ] || continue
  data="${WORK_DIR}/${identifier}.data"
  status="${WORK_DIR}/${identifier}.status"
  case "${kind}" in
    helm)
      echo "  ${recipe}: ${arg1}" >&2
      fetch_into "${arg1}" "${data}" "${status}"
      ;;
    ghrelease)
      echo "  ${recipe}: github.com/${arg1} releases" >&2
      # `per_page=100` and one page: bounded on purpose. The newest
      # non-prerelease release is on the first page of any project that
      # has not published a hundred releases since its last stable one.
      fetch_into "https://api.github.com/repos/${arg1}/releases?per_page=100" "${data}" "${status}"
      ;;
    oci)
      echo "  ${recipe}: ${arg1}/${arg2} tags" >&2
      # Two hops, because a Docker Registry v2 tag listing is
      # authenticated even when the artifact is public: an anonymous
      # pull token first, then the listing. If either hop fails the row
      # is `unknown` and the report prints the `helm show chart`
      # command instead -- the graceful degradation this script's
      # header promises.
      token_file="${WORK_DIR}/${identifier}.token"
      token_status="${WORK_DIR}/${identifier}.token.status"
      fetch_into "https://${arg1}/token?service=${arg1}&scope=repository:${arg2}:pull" \
                 "${token_file}" "${token_status}"
      if [ "$(cat "${token_status}")" = "ok" ]; then
        token="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("token") or json.load(open(sys.argv[1])).get("access_token") or "")' "${token_file}" 2>/dev/null || true)"
      else
        token=""
      fi
      if [ -z "${token}" ]; then
        # Carry the token hop's own outcome forward rather than
        # flattening it to "refused": a registry that rate-limited the
        # token endpoint and a machine with no route to it are
        # different problems, and the report says which one happened.
        if [ "$(cat "${token_status}")" = "ok" ]; then
          echo "refused" >"${status}"
        else
          cat "${token_status}" >"${status}"
        fi
      else
        curl_status=0
        curl --fail --silent --show-error --location \
             --connect-timeout 15 --max-time 60 \
             -H "Authorization: Bearer ${token}" \
             -o "${data}" "https://${arg1}/v2/${arg2}/tags/list?n=1000" \
             2>"${WORK_DIR}/curl.err" || curl_status=$?
        if [ "${curl_status}" -eq 0 ]; then
          echo "ok" >"${status}"
        elif [ "${curl_status}" -eq 22 ]; then
          echo "refused" >"${status}"
        else
          echo "unreachable" >"${status}"
        fi
      fi
      ;;
    *)
      fail "internal: unknown plan kind '${kind}'" 2
      ;;
  esac
done <"${WORK_DIR}/plan.tsv"

# Nothing answered at all is a different thing from a registry being
# down, and it is the one case where a report would be worthless.
if ! grep -qx "ok" "${WORK_DIR}"/*.status 2>/dev/null; then
  fail "network required: every upstream source refused or did not answer. Nothing was written; no pin was changed." 3
fi

# ---------------------------------------------------------------------
# Phase 3: join what came back to the checked-in pins and report.
#
# The report is written to a file first and printed afterwards so that
# `--output` and stdout carry byte-identical text: the pull request that
# `.github/workflows/maintenance.yml` opens quotes the file, and a
# reviewer comparing it against a local run must not find two versions
# of the same report.
# ---------------------------------------------------------------------
readonly REPORT="${WORK_DIR}/report.out"

# The report exit status is Python's, so `set -e` must not swallow it:
# 10 ("an update is available") is a successful outcome of this script
# and a failure to `set -e`.
report_status=0
python3 - "${REPO_ROOT}" "${WORK_DIR}" "${FORMAT}" >"${REPORT}" <<'PYTHON' || report_status=$?
import pathlib
import re
import sys

try:
    import yaml
except ImportError:  # pragma: no cover - phase 1 already proved it imports
    sys.exit("check-recipe-updates: PyYAML is not importable")

root, work, fmt = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3]

STABLE = re.compile(r"^v?(\d+)\.(\d+)\.(\d+)$")


def stable_key(version):
    """Sort key for a release version, or None if it is not one.

    Prereleases are deliberately not versions this script can propose:
    a certified row is proven by a real installation, and proposing an
    `-rc.1` chart would put a release candidate on the path to a v1
    certification. `compatibility/recipes.yaml`'s own pins are all
    stable releases, and the newest STABLE release is what "behind"
    is measured against.
    """
    match = STABLE.match(version.strip())
    return tuple(int(part) for part in match.groups()) if match else None


def read_status(identifier):
    path = work / f"{identifier}.status"
    try:
        return path.read_text(encoding="utf-8").strip()
    except OSError:
        return "unreachable"


def latest_from_helm_index(path, chart):
    document = yaml.safe_load(path.read_text(encoding="utf-8"))
    releases = ((document or {}).get("entries") or {}).get(chart)
    if not releases:
        return None, None, f"the index publishes no chart named '{chart}'"
    best = None
    for release in releases:
        version = str(release.get("version") or "")
        key = stable_key(version)
        if key and (best is None or key > best[0]):
            best = (key, version, str(release.get("appVersion") or "") or None)
    if best is None:
        return None, None, f"'{chart}' has no stable release in the index"
    return best[1], best[2], None


def latest_from_tags(path):
    import json

    tags = (json.loads(path.read_text(encoding="utf-8")) or {}).get("tags") or []
    best = None
    for tag in tags:
        key = stable_key(str(tag))
        if key and (best is None or key > best[0]):
            best = (key, str(tag))
    if best is None:
        return None, None, "the registry lists no stable tag"
    return best[1], None, None


def latest_from_releases(path):
    import json

    releases = json.loads(path.read_text(encoding="utf-8")) or []
    best = None
    for release in releases:
        if release.get("draft") or release.get("prerelease"):
            continue
        tag = str(release.get("tag_name") or "")
        key = stable_key(tag)
        if key and (best is None or key > best[0]):
            best = (key, tag.lstrip("v"))
    if best is None:
        return None, None, "no stable release on the first page of releases"
    return best[1], None, None


rows = []
for line in (work / "plan.tsv").read_text(encoding="utf-8").splitlines():
    if not line.strip():
        continue
    kind, recipe, component, pin, identifier, arg1, arg2 = line.split("\t")
    status = read_status(identifier)
    row = {
        "kind": kind,
        "recipe": recipe,
        "component": component,
        "pin": pin,
        # A pin re-derived from its implementation's own build (both
        # Gateway API CRD bundles) is reported and never counted as
        # behind -- see this script's header.
        "derived": kind == "ghrelease" and arg2 == "derived",
        "source": arg1,
        "chart": arg2,
        "latest": None,
        "app_version": None,
        "state": "unknown",
        "note": None,
    }
    if status != "ok":
        row["note"] = (
            "the host answered and refused the request (a renamed endpoint, or a rate limit)"
            if status == "refused"
            else "nothing answered at that host"
        )
    else:
        data = work / f"{identifier}.data"
        try:
            if kind == "helm":
                latest, app_version, problem = latest_from_helm_index(data, arg2)
            elif kind == "oci":
                latest, app_version, problem = latest_from_tags(data)
            else:
                latest, app_version, problem = latest_from_releases(data)
        except Exception as error:  # malformed index, truncated JSON, ...
            latest, app_version, problem = None, None, f"the answer was unusable: {error}"
        row["latest"], row["app_version"], row["note"] = latest, app_version, problem
        if latest is not None:
            pin_key, latest_key = stable_key(pin), stable_key(latest)
            if pin_key is None:
                row["state"] = "unknown"
                row["note"] = f"the checked-in pin '{pin}' is not a release version"
            elif latest_key > pin_key:
                row["state"] = "behind"
            elif latest_key < pin_key:
                # Real, and worth saying out loud: a yanked release, or
                # a pin taken from somewhere this lookup does not see.
                row["state"] = "ahead"
            else:
                row["state"] = "current"

    # Derived from the recipe's own document rather than from a list
    # kept in this script: a retired upstream's final release is current
    # forever, and a reader should not have to wonder why.
    recipe_file = root / "recipes" / recipe / "recipe.yaml"
    try:
        if re.search(r"archiv", recipe_file.read_text(encoding="utf-8"), re.IGNORECASE):
            row["archived"] = True
        else:
            row["archived"] = False
    except OSError:
        row["archived"] = False
    rows.append(row)

actionable = [r for r in rows if r["state"] == "behind" and not r["derived"]]
informational = [r for r in rows if r["state"] == "behind" and r["derived"]]
unknown = [r for r in rows if r["state"] == "unknown"]

NEVER_AUTO_CERTIFIED = (
    "A newer version is NOT a certification. `compatibility/recipes.yaml` "
    "certifies a row only once that exact pin has been installed and verified "
    "by this repository's own recipe suite on every Kubernetes version the row "
    "claims. Bumping a pin therefore means: edit the recipe's pin, append a new "
    "`compatibility/recipes.yaml` entry (never edit an old one in place), and "
    "run the recipe certification matrix at `weeklyRelease` on the new pin. "
    "Merging on the strength of a version number alone would turn a "
    "certification into a rumor."
)

out = []
if fmt == "markdown":
    out.append("## Certified recipe pins vs upstream")
    out.append("")
    out.append("| Recipe | Component | Pinned | Latest upstream | State |")
    out.append("| --- | --- | --- | --- | --- |")
    for row in rows:
        latest = row["latest"] or "unknown"
        if row["app_version"]:
            latest = f"{latest} (app {row['app_version']})"
        state = row["state"]
        if row["derived"] and state == "behind":
            state = "behind (derived pin — see below)"
        if row["archived"] and state == "current":
            state = "current (upstream archived)"
        out.append(
            f"| `{row['recipe']}` | `{row['component']}` | `{row['pin']}` | "
            f"{latest} | {state} |"
        )
    out.append("")
    if actionable:
        out.append("### Candidates for a certification run")
        out.append("")
        for row in actionable:
            out.append(
                f"- **`{row['recipe']}`** — pinned `{row['pin']}`, upstream "
                f"`{row['latest']}` ({row['source']})"
            )
        out.append("")
        out.append(NEVER_AUTO_CERTIFIED)
        out.append("")
    else:
        out.append("No certified pin is behind upstream; nothing to propose.")
        out.append("")
    if informational:
        out.append("### Informational: derived pins")
        out.append("")
        out.append(
            "These are pinned to the release their own implementation builds "
            "against, not to the newest upstream release. A newer version here "
            "is **not** an update that is due; the pin moves when the "
            "implementation's pin moves, re-derived from that release's own "
            "`go.mod`."
        )
        out.append("")
        for row in informational:
            out.append(
                f"- `{row['recipe']}` / `{row['component']}` — pinned "
                f"`{row['pin']}`, upstream `{row['latest']}`"
            )
        out.append("")
    if unknown:
        out.append("### Not checked")
        out.append("")
        for row in unknown:
            out.append(f"- `{row['recipe']}` / `{row['component']}` — {row['note']}")
            if row["kind"] == "oci":
                out.append(
                    f"  - by hand: `helm show chart oci://{row['source']}/{row['chart']} "
                    f"--version {row['pin']}`"
                )
        out.append("")
        out.append(
            "An unreachable source is reported as unknown, never as up to date."
        )
        out.append("")
else:
    out.append("check-recipe-updates: certified recipe pins vs upstream")
    out.append("")
    width = max(len(f"{r['recipe']}/{r['component']}") for r in rows)
    for row in rows:
        label = f"{row['recipe']}/{row['component']}".ljust(width)
        latest = row["latest"] or "unknown"
        suffix = ""
        if row["derived"] and row["state"] == "behind":
            suffix = "   (derived pin: moves with its implementation, not on its own)"
        elif row["archived"] and row["state"] == "current":
            suffix = "   (upstream archived; this pin is final)"
        elif row["note"] and row["state"] == "unknown":
            suffix = f"   ({row['note']})"
        out.append(
            f"  {label}  pinned {row['pin']:<10} upstream {latest:<12} {row['state']}{suffix}"
        )
    out.append("")
    if actionable:
        out.append("UPDATE AVAILABLE (not a certification):")
        for row in actionable:
            out.append(f"  {row['recipe']}: {row['pin']} -> {row['latest']}  ({row['source']})")
        out.append("")
        for line in NEVER_AUTO_CERTIFIED.replace("`", "").split(". "):
            if line.strip():
                out.append(f"  {line.strip().rstrip('.')}.")
    else:
        out.append("  No certified pin is behind upstream; nothing to propose.")
    if informational:
        out.append("")
        out.append("Derived pins, reported and NOT proposed:")
        for row in informational:
            out.append(
                f"  {row['recipe']}/{row['component']}: pinned {row['pin']}, "
                f"upstream {row['latest']} -- this pin is re-derived when "
                f"{row['recipe']}'s own pin moves, never bumped on its own."
            )
    if unknown:
        out.append("")
        out.append("NOT CHECKED (reported as unknown, never as up to date):")
        for row in unknown:
            out.append(f"  {row['recipe']}/{row['component']}: {row['note']}")
            if row["kind"] == "oci":
                out.append(
                    f"    by hand: helm show chart oci://{row['source']}/{row['chart']} "
                    f"--version {row['pin']}"
                )

print("\n".join(out))
raise SystemExit(10 if actionable else 0)
PYTHON

case "${report_status}" in
  0|10) ;;
  *) fail "the report could not be built (python exit ${report_status})" 4 ;;
esac

if [ -n "${OUTPUT}" ]; then
  mkdir -p "$(dirname -- "${OUTPUT}")"
  cp "${REPORT}" "${OUTPUT}"
  echo "check-recipe-updates: report written to ${OUTPUT}" >&2
fi

cat "${REPORT}"
exit "${report_status}"
