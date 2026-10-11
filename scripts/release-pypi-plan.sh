#!/usr/bin/env bash
# What `release-pypi.yml` builds and uploads, decided from the event that started
# it. Its `plan` job runs this from a checkout holding the repository's tags, and
# every later job reads the answer from the job's outputs.
#
#   GITHUB_EVENT_NAME=release GITHUB_EVENT_PATH=event.json \
#     GITHUB_OUTPUT=out scripts/release-pypi-plan.sh
#
# Two events start the workflow:
#
#   * `release` (published) — the tag is the Release's, and both the CLI wheels
#     and the SDK are selected for upload, as they always were (the upload skips
#     what PyPI already holds).
#   * `workflow_dispatch` with input `tag` — completing a release whose upload
#     did not finish. The tag must already exist; only the distributions PyPI
#     does not hold at the tag's version are selected, so a hand-run never
#     re-uploads a CLI the Release already published.
#
# Either way the build is the tag's tree, never the branch the workflow runs
# from: `sha` is the tag's commit, and `version` is read from that commit's
# Cargo.toml (the version maturin and the SDK stamp), which the tag must name.
#
# Writes `tag`, `sha`, `version`, `upload-cli` and `upload-sdk` (each `true` or
# `false`) to $GITHUB_OUTPUT and one summary line to stdout. A PyPI answer other
# than "held" (200) or "not held" (404) fails the run: guessing either way
# would either skip an upload or retry one blindly.
set -euo pipefail

readonly API="${PYPI_JSON_API:-https://pypi.org/pypi}"

fail() {
    printf 'release-pypi-plan: %s\n' "$1" >&2
    printf 'ACTION: %s\n' "$2" >&2
    exit 1
}

for var in GITHUB_EVENT_NAME GITHUB_EVENT_PATH GITHUB_OUTPUT; do
    [ -n "${!var:-}" ] || fail "$var is not set" "run this from a GitHub Actions job, or set $var as one would"
done

case "$GITHUB_EVENT_NAME" in
    release) field='["release"]["tag_name"]' ;;
    workflow_dispatch) field='["inputs"]["tag"]' ;;
    *) fail "event '$GITHUB_EVENT_NAME' does not start a PyPI release" "start the workflow from a published Release or by hand with a tag" ;;
esac

tag=$(python3 -c "
import json, sys
with open(sys.argv[1], encoding='utf-8') as handle:
    print(json.load(handle)$field)
" "$GITHUB_EVENT_PATH") || fail "the $GITHUB_EVENT_NAME payload at $GITHUB_EVENT_PATH names no tag (see above)" "pass the release tag, e.g. onejudge-v0.19.0"
# Refused rather than repaired: `onejudge-v0.19 .0` must not quietly become a tag.
[[ $tag =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] \
    || fail "the $GITHUB_EVENT_NAME payload names '$tag', which is not a tag name" "pass the release tag exactly, e.g. onejudge-v0.19.0"

sha=$(git rev-parse --verify --quiet "refs/tags/$tag^{commit}") \
    || fail "'$tag' is not a tag of this repository" "pass an existing release tag (git ls-remote --tags origin), e.g. onejudge-v0.19.0"

# The version pack.py stamps the SDK with: [workspace.package]'s `version = "…"`.
version=$(git show "$sha:Cargo.toml" | awk -F'"' '/^\[/ { s = $0 } s == "[workspace.package]" && /^version = / { print $2; exit }') || fail "could not read Cargo.toml at $tag (see above)" "pass a tag whose tree is a onejudge release"
[ -n "$version" ] || fail "Cargo.toml at $tag has no [workspace.package] version" "pass a tag whose tree is a onejudge release"
case "$tag" in
    "v$version" | "onejudge-v$version") ;;
    *) fail "tag '$tag' does not name the version its tree releases ($version)" "pass the tag release-plz cut for $version, e.g. onejudge-v$version" ;;
esac

# held NAME — whether PyPI already serves NAME at $version.
held() {
    local url="$API/$1/$version/json" status
    status=$(curl --silent --show-error --location --max-time 15 --retry 2 --retry-delay 1 \
        --output /dev/null --write-out '%{http_code}' "$url") \
        || fail "could not reach $url (see curl's message above)" "re-run the workflow once PyPI answers"
    case "$status" in
        200) echo true ;;
        404) echo false ;;
        *) fail "$url answered HTTP $status" "re-run the workflow once PyPI answers 200 or 404" ;;
    esac
}

upload_cli=true upload_sdk=true
if [ "$GITHUB_EVENT_NAME" = workflow_dispatch ]; then
    # An assignment, so a `fail` inside the substitution fails this script too.
    cli_held=$(held onejudge-cli)
    sdk_held=$(held onejudge)
    [ "$cli_held" = false ] || upload_cli=false
    [ "$sdk_held" = false ] || upload_sdk=false
fi

{
    echo "tag=$tag"
    echo "sha=$sha"
    echo "version=$version"
    echo "upload-cli=$upload_cli"
    echo "upload-sdk=$upload_sdk"
} >>"$GITHUB_OUTPUT" || fail "could not write the plan to $GITHUB_OUTPUT (see above)" "point GITHUB_OUTPUT at a writable file"
echo "release-pypi-plan: $tag ($sha) version $version: upload onejudge-cli=$upload_cli onejudge=$upload_sdk"
