#!/usr/bin/env bash
# What does the public registry serve, right now, for ONE release target of this
# repository? The targets are declared in `release-targets.toml`, which names this
# script as its `probe`.
#
#   scripts/release-probe.sh crate:onejudge      -> 0.6.0   (exit 0)
#   scripts/release-probe.sh pypi:onejudge-cli   -> 0.6.0   (exit 0)
#
# Exactly three answers, and a caller must be able to tell them apart:
#
#   * exit 0, one line on stdout   — the version that registry currently serves;
#   * exit 0, empty stdout         — that registry has no release of it yet;
#   * non-zero, reason on stderr   — NOT ANSWERED, stdout empty.
#
# "Not answered" and "no release yet" are different answers all the way out. A
# caller holds indefinitely on the first and must never read it as evidence that
# a release has not happened — collapsing the two launches dependent work whose
# dependency never landed, which is the most damaging thing this can get wrong.
# An identifier this probe does not recognise (no registry qualification, an
# unsupported registry, a name the identifier syntax does not admit) is therefore
# NOT ANSWERED, never empty output.
#
# It recognises `crate:<name>` (crates.io) and `pypi:<name>` (PyPI) — the two
# registries this repository publishes to.
#
# It assumes nothing beyond PATH and HOME: spawned as a direct subprocess with no
# shell interposed, from the repository root, with no credential of any kind.
# Every target is on a public registry, so an unauthenticated read is all it needs
# and all it may need. Each answer is bounded well inside sixty seconds.
set -euo pipefail

readonly UA="onejudge-release-probe (https://github.com/nickderobertis/onejudge)"
# Worst case: two attempts of at most 15s each plus a 1s backoff.
readonly MAX_TIME=15
readonly RETRIES=1

# Not answered: reason on stderr, nothing on stdout, non-zero exit.
unanswered() {
    printf 'release-probe: %s\n' "$*" >&2
    exit 1
}

if [ "$#" -ne 1 ]; then
    unanswered "usage: release-probe.sh <registry>:<name> (exactly one argument, got $#)"
fi

id=$1
registry=${id%%:*}
name=${id#*:}
if [ "$registry" = "$id" ]; then
    unanswered "unrecognised identifier '$id': expected a registry-qualified <registry>:<name>"
fi
# Bash's own matching, not grep's: a name check that shelled out would report a
# PATH missing `grep` as a malformed identifier, which is a different answer.
if ! [[ $name =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
    unanswered "unrecognised identifier '$id': '$name' is not a registry artifact name"
fi

case "$registry" in
    crate) url="https://crates.io/api/v1/crates/$name" ;;
    pypi) url="https://pypi.org/pypi/$name/json" ;;
    *) unanswered "unrecognised identifier '$id': this repository publishes to crate: and pypi: only" ;;
esac

for tool in curl mktemp python3; do
    command -v "$tool" >/dev/null 2>&1 || unanswered "$tool is not on PATH, so '$id' cannot be looked up"
done

body=$(mktemp)
trap 'rm -f "$body"' EXIT

status=$(curl --silent --show-error --location \
    --max-time "$MAX_TIME" --retry "$RETRIES" --retry-delay 1 \
    --user-agent "$UA" --header 'Accept: application/json' \
    --output "$body" --write-out '%{http_code}' "$url") \
    || unanswered "could not reach $url for '$id' (see curl's message above)"

# A registry that has never served this artifact answers 404. That is the ONLY
# way to report "no release yet" — any other unexpected status is not answered.
if [ "$status" = 404 ]; then
    exit 0
fi
if [ "$status" != 200 ]; then
    unanswered "$url answered HTTP $status for '$id'"
fi

version=$(python3 -c '
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    payload = json.load(handle)
if sys.argv[2] == "crate":
    crate = payload["crate"]
    # The stable release is what the registry serves a dependent. A crate with
    # only prereleases has none, and answering nothing there would read as "no
    # release yet" for a release that already happened.
    print(crate.get("max_stable_version") or crate["newest_version"])
else:
    print(payload["info"]["version"])
' "$body" "$registry") || unanswered "$url answered HTTP 200 for '$id' with no version this probe could read"

if [ -z "$version" ]; then
    unanswered "$url answered HTTP 200 for '$id' with an empty version"
fi

printf '%s\n' "$version"
