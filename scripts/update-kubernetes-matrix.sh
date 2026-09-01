#!/usr/bin/env bash
#
# Proposes an update to `compatibility/kubernetes.yaml` from upstream
# data (ROADMAP Task 7.5 step 2). It NEVER edits that file.
#
# Usage:
#   ./scripts/update-kubernetes-matrix.sh
#   ./scripts/update-kubernetes-matrix.sh --output /tmp/proposal.yaml
#   ./scripts/update-kubernetes-matrix.sh --kind-version v0.34.0
#
# # Why this proposes instead of applying
#
# `compatibility/kubernetes.yaml` is the single source of truth for which
# Kubernetes versions Admission Lab provisions, and its own header states
# the rule this script obeys: "adding or dropping a supported minor is a
# deliberate, reviewed change to this checked-in file... human review is
# required before dropping a supported version in a stable release line"
# (PRODUCT.md §32). A script that rewrote that file would move the
# decision from a reviewable diff to whatever a third-party API returned
# that morning -- and dropping a supported minor is a breaking change for
# every user whose `admissionlab.yaml` still names it.
#
# So this writes a *proposal* to a separate file and prints the unified
# diff between the checked-in `releases:` block and the proposed one.
# Applying it is a human editing the real file, which is also the only
# way its surrounding comments (which explain *why* each entry is what it
# is, and which no generator can write) stay true. Pointing `--output` at
# `compatibility/kubernetes.yaml` is refused outright.
#
# # What it reads
#
#   endoflife.date/api/kubernetes.json
#       Which Kubernetes minors upstream still supports, by EOL date.
#   api.github.com/repos/kubernetes-sigs/kind/releases/tags/<kind version>
#       The `kindest/node:vX.Y.Z@sha256:...` images that exact `kind`
#       release publishes. This is where the digests come from; they are
#       never invented, never resolved from a floating tag, and never
#       taken from a `kind` version other than the one named.
#   dl.k8s.io/release/stable.txt
#       Upstream's current stable release, printed for context only. It
#       is deliberately NOT used to select anything: a minor released
#       days ago is exactly the case `compatibility/kubernetes.yaml`'s own
#       comments discuss at length, and that is a judgement call for a
#       reviewer.
#
# # The policy it applies
#
# Global Constraint 10 and PRODUCT.md §32: the latest THREE
# upstream-supported minors are `supported: true`. Note that upstream's
# own window is often four minors wide -- a new minor ships before the
# oldest reaches EOL -- so "still supported upstream" and "in Admission
# Lab's matrix" are not the same set, and this script applies the
# three-newest rule rather than copying whatever is not yet EOL.
#
# A minor already in the checked-in file that falls out of the top three
# is proposed as `supported: false`, never deleted: that is the same
# retention discipline the file already applies to 1.34, and it is what
# lets `resolve_node_image` return "no longer supported" instead of
# "never heard of it".
#
# If upstream ever supports FEWER than three minors, the proposal says so
# and names `supportWindowException` in `compatibility/recipes.yaml` --
# the checked-in escape hatch `admissionlab_recipes::validate_compatibility`
# requires in that case (ROADMAP Task 7.4 step 1).
#
# # Network
#
# This is the only part of Admission Lab that talks to the network on
# purpose, and it is not part of any lab run: nothing in the CLI, the
# recipes crate, or the cluster crate ever fetches this data (all three
# read the checked-in file, embedded at compile time). Offline, this
# script exits 3 with a message naming the host it could not reach and
# saying explicitly that nothing was written -- rather than a bare `curl`
# error, or a half-written proposal left behind to be mistaken for a
# real one.
#
# # Exit codes
#
#   0   Upstream matches the checked-in file; nothing to propose.
#   2   Bad arguments.
#   3   Network required and unavailable -- nothing answered at all.
#   4   Upstream was reachable but its answer was unusable: an HTTP error
#       (a `kind` tag that does not exist, a rate-limited API), no node
#       images in the named release, no supported minors, malformed JSON.
#   10  A proposal was written and printed. Deliberately not 1: a
#       difference is this script's normal, successful outcome, and a
#       caller should be able to tell it apart from a failure.

set -euo pipefail

# The `kind` release whose published node images this repository pins.
# Overridable with `--kind-version` so a reviewer can see what a newer
# `kind` would propose before deciding to move to it -- the same reason
# `compatibility/kubernetes.yaml` names its own source release in a
# comment.
KIND_VERSION="v0.33.0"

# How many minors Admission Lab supports at release time. Mirrors
# `admissionlab_recipes::compat::RELEASE_SUPPORTED_MINORS`; see this
# script's "The policy it applies" section.
SUPPORTED_MINORS=3

readonly REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
readonly MATRIX_FILE="${REPO_ROOT}/compatibility/kubernetes.yaml"

OUTPUT=""

usage() {
  echo "usage: $0 [--output PATH] [--kind-version vX.Y.Z]" >&2
  echo "       proposes an update to compatibility/kubernetes.yaml; never edits it" >&2
}

fail() {
  echo "update-kubernetes-matrix: FAIL $1" >&2
  exit "$2"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --output)
      [ "$#" -ge 2 ] || { usage; exit 2; }
      OUTPUT="$2"
      shift 2
      ;;
    --kind-version)
      [ "$#" -ge 2 ] || { usage; exit 2; }
      KIND_VERSION="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "update-kubernetes-matrix: unknown argument '$1'" >&2
      usage
      exit 2
      ;;
  esac
done

for tool in curl python3 diff; do
  command -v "${tool}" >/dev/null 2>&1 \
    || fail "'${tool}' is required but was not found on PATH" 2
done

[ -f "${MATRIX_FILE}" ] || fail "cannot read ${MATRIX_FILE}" 2

# The whole point of this script is that it does not write that file.
# Checked by resolved path, so `--output ./compatibility/../compatibility/kubernetes.yaml`
# is refused too.
if [ -n "${OUTPUT}" ]; then
  output_dir="$(dirname -- "${OUTPUT}")"
  mkdir -p "${output_dir}"
  resolved_output="$(cd -- "${output_dir}" && pwd)/$(basename -- "${OUTPUT}")"
  if [ "${resolved_output}" = "${MATRIX_FILE}" ]; then
    fail "refusing to write over ${MATRIX_FILE}: this script proposes a change for review, it never applies one" 2
  fi
fi

WORK_DIR="$(mktemp -d)"
# Removed on every exit path, including a failed fetch: a half-downloaded
# upstream response is not evidence of anything and must not be left
# behind to be mistaken for a proposal.
trap 'rm -rf "${WORK_DIR}"' EXIT

if [ -z "${OUTPUT}" ]; then
  OUTPUT="${TMPDIR:-/tmp}/admissionlab-kubernetes-matrix-$$.proposed.yaml"
fi

# Fetches `$1` into `$2`. `--fail` so an HTTP error (a rate-limited
# GitHub API, a renamed endpoint, a `kind` tag that does not exist) is a
# failure here rather than an HTML error page parsed as JSON three lines
# later.
#
# The two ways this fails are reported as different things, because they
# call for different actions. `curl` exit 22 means the host answered and
# said no -- the data is wrong or gone (exit 4, "upstream unusable"). Its
# connection-level codes mean nothing answered at all, which is what
# running offline looks like (exit 3, "network required"). Conflating
# them would tell someone on a plane to go fix a URL.
fetch() {
  local url="$1" destination="$2" host status=0
  host="$(printf '%s\n' "${url}" | sed -E 's#^https?://([^/]+).*#\1#')"
  curl --fail --silent --show-error --location \
       --connect-timeout 15 --max-time 60 \
       -o "${destination}" "${url}" 2>"${WORK_DIR}/curl.err" || status=$?
  [ "${status}" -eq 0 ] && return 0

  echo "update-kubernetes-matrix: could not fetch ${url}" >&2
  sed 's/^/  /' "${WORK_DIR}/curl.err" >&2 || true
  if [ "${status}" -eq 22 ]; then
    fail "${host} answered but refused this request (curl exit 22). The endpoint or the version it names may be wrong, or an unauthenticated API may be rate-limited. Nothing was written; compatibility/kubernetes.yaml is unchanged." 4
  fi
  fail "network required: this script proposes an update from live upstream data (${host}) and cannot run offline (curl exit ${status}). Nothing was written; compatibility/kubernetes.yaml is unchanged." 3
}

echo "update-kubernetes-matrix: fetching upstream data (kind ${KIND_VERSION})"
fetch "https://endoflife.date/api/kubernetes.json" "${WORK_DIR}/eol.json"
fetch "https://api.github.com/repos/kubernetes-sigs/kind/releases/tags/${KIND_VERSION}" "${WORK_DIR}/kind.json"
fetch "https://dl.k8s.io/release/stable.txt" "${WORK_DIR}/stable.txt"

echo "update-kubernetes-matrix: upstream stable is $(cat "${WORK_DIR}/stable.txt")"

# Builds the proposed `releases:` block. Python rather than more shell
# because this joins two JSON documents by minor version and sorts by
# semantic version -- both of which are a `sed` pipeline nobody should
# have to review.
python3 - \
  "${WORK_DIR}/eol.json" \
  "${WORK_DIR}/kind.json" \
  "${MATRIX_FILE}" \
  "${SUPPORTED_MINORS}" \
  >"${WORK_DIR}/proposed.yaml" <<'PYTHON'
import datetime
import json
import re
import sys

eol_path, kind_path, matrix_path, supported_count = sys.argv[1:5]
supported_count = int(supported_count)


def die(message, code=4):
    print(f"update-kubernetes-matrix: FAIL {message}", file=sys.stderr)
    raise SystemExit(code)


def load_json(path, what):
    try:
        with open(path, encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, json.JSONDecodeError) as error:
        die(f"{what} was not usable JSON: {error}")


def minor_key(minor):
    return tuple(int(part) for part in minor.split("."))


def version_key(version):
    return tuple(int(part) for part in version.split("."))


# 1. Which minors upstream still supports, by EOL date. `eol` is a date
#    string for a released cycle and `false`/`true` for one that is not
#    yet dated; anything that is not a date in the future is treated as
#    out of support, because "unknown" must never read as "supported"
#    (Global Constraint 15).
today = datetime.date.today()
alive = []
for cycle in load_json(eol_path, "endoflife.date's Kubernetes data"):
    minor, eol = cycle.get("cycle"), cycle.get("eol")
    if not isinstance(minor, str) or not re.fullmatch(r"\d+\.\d+", minor):
        continue
    if not isinstance(eol, str):
        continue
    try:
        if datetime.date.fromisoformat(eol) > today:
            alive.append(minor)
    except ValueError:
        continue
if not alive:
    die("endoflife.date reported no Kubernetes minor still in support")
alive.sort(key=minor_key, reverse=True)

# 2. The node images that exact `kind` release publishes, newest patch
#    per minor. Read out of the release notes body, which is where kind
#    publishes them -- the same source `compatibility/kubernetes.yaml`'s
#    header says its digests were captured from.
kind_release = load_json(kind_path, "kind's release data")
body = kind_release.get("body") or ""
images = {}
pattern = r"kindest/node:v(\d+\.\d+\.\d+)@(sha256:[0-9a-f]{64})"
for version, digest in re.findall(pattern, body):
    minor = ".".join(version.split(".")[:2])
    known = images.get(minor)
    if known is None or version_key(version) > version_key(known[0]):
        images[minor] = (version, digest)
if not images:
    die(
        f"kind release {kind_release.get('tag_name')!r} published no "
        "kindest/node images in its release notes"
    )

# 3. Admission Lab supports the latest N still-supported minors that kind
#    can actually provision. Upstream's own window is routinely wider
#    than N (a new minor ships before the oldest reaches EOL), which is
#    why this takes the newest N rather than everything not yet EOL.
provisionable = [minor for minor in alive if minor in images]
if not provisionable:
    die("no still-supported Kubernetes minor has a node image in this kind release")
supported = provisionable[:supported_count]

# 4. Minors already in the checked-in file keep an entry even once they
#    drop out of the supported set -- see this script's header. Read with
#    a small regex rather than a YAML parser so this script needs no
#    third-party module; the file's shape is fixed and machine-checked by
#    `admissionlab_cluster::load_matrix` on every build.
with open(matrix_path, encoding="utf-8") as handle:
    matrix_text = handle.read()
existing = {}
for minor, version, flag in re.findall(
    r'^\s*-\s*minor:\s*"(\d+\.\d+)"\s*\n\s*version:\s*"(\d+\.\d+\.\d+)"'
    r'[^\n]*\n[^\n]*\n[^\n]*\n\s*supported:\s*(true|false)',
    matrix_text,
    re.MULTILINE,
):
    existing[minor] = (version, flag == "true")

retired = [minor for minor in existing if minor not in supported]
rows = sorted(set(supported) | set(retired), key=minor_key, reverse=True)

lines = ["releases:"]
notes = []
for minor in rows:
    is_supported = minor in supported
    if minor in images:
        version, digest = images[minor]
    elif minor in existing:
        # Retired and no longer published by this kind release: keep
        # exactly what the file already records rather than dropping the
        # entry or inventing a digest.
        version = existing[minor][0]
        match = re.search(
            rf'minor:\s*"{re.escape(minor)}"\s*\n\s*version:[^\n]*\n\s*image:[^\n]*\n\s*digest:\s*"([^"]+)"',
            matrix_text,
        )
        if not match:
            die(f"cannot recover the checked-in digest for retired minor {minor}")
        digest = match.group(1)
        notes.append(
            f"{minor} is retained from the checked-in file: kind {kind_release.get('tag_name')} "
            "no longer publishes a node image for it"
        )
    else:
        continue
    lines += [
        f'  - minor: "{minor}"',
        f'    version: "{version}"',
        f'    image: "kindest/node:v{version}"',
        f'    digest: "{digest}"',
        f"    supported: {'true' if is_supported else 'false'}",
    ]

if len(supported) < supported_count:
    notes.append(
        f"upstream currently supports only {len(supported)} minor(s) with a node image "
        f"in this kind release, fewer than the {supported_count} Global Constraint 10 "
        "requires. Declare a supportWindowException in compatibility/recipes.yaml "
        "(with expectedSupportedMinors, reason and releaseNotes) or this matrix will "
        "fail validation."
    )
# Only transitions are worth a reviewer's attention: a minor that is
# already `supported: false` and stays that way is not news.
for minor in supported:
    if minor not in existing:
        notes.append(f"{minor} is NEW: it is not in the checked-in file at all")
    elif not existing[minor][1]:
        notes.append(f"{minor} would become supported: true")
for minor, (_version, was_supported) in existing.items():
    if minor not in supported and was_supported:
        notes.append(
            f"{minor} would become supported: false -- this DROPS a supported "
            "Kubernetes version and needs human review (PRODUCT.md \u00a732)"
        )

print("\n".join(lines))
if notes:
    print("# --- reviewer notes (not part of the file) ---", file=sys.stderr)
    for note in notes:
        print(f"# {note}", file=sys.stderr)
PYTHON

# The checked-in `releases:` block, verbatim, for a diff that shows only
# what would change rather than the file's whole comment header.
sed -n '/^releases:/,$p' "${MATRIX_FILE}" >"${WORK_DIR}/current.yaml"

if diff -u "${WORK_DIR}/current.yaml" "${WORK_DIR}/proposed.yaml" >"${WORK_DIR}/diff.txt"; then
  echo "update-kubernetes-matrix: compatibility/kubernetes.yaml already matches upstream (kind ${KIND_VERSION}); nothing to propose"
  exit 0
fi

cp "${WORK_DIR}/proposed.yaml" "${OUTPUT}"
echo
echo "update-kubernetes-matrix: PROPOSED CHANGE (not applied)"
echo "  proposal written to: ${OUTPUT}"
echo "  review it, then edit compatibility/kubernetes.yaml by hand --"
echo "  including the comments that explain why each entry is what it is."
echo
sed -e "s#${WORK_DIR}/current.yaml#compatibility/kubernetes.yaml (checked in)#" \
    -e "s#${WORK_DIR}/proposed.yaml#proposed#" \
    "${WORK_DIR}/diff.txt"
exit 10
