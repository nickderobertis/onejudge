#!/usr/bin/env bats
# scripts/release-pypi-plan.sh, the `plan` job of release-pypi.yml: which tree a
# run builds and which distributions it uploads, decided from the event payload
# GitHub hands the job. The repository is a scratch one whose default branch has
# moved past its release tag; the package index is a `curl` double answering a
# status per URL and recording every request. Nothing is uploaded.
#
# The last tests read release-pypi.yml itself, because the jobs that consume the
# plan run only on GitHub's runners: nothing this repository runs can execute a
# workflow, and an upload cannot be exercised without publishing. So they hold
# the wiring instead — the job outputs carry exactly the keys the script writes,
# and each checkout, upload and verification reads the plan's answer.

load support/helpers
bats_require_minimum_version 1.5.0

setup() {
    use_doubles
    REPO="$BATS_TEST_TMPDIR/repo"
    git_repo "$REPO"
    release "$REPO" 0.18.1
    git -C "$REPO" tag onejudge-v0.18.1
    release "$REPO" 0.19.0
    git -C "$REPO" tag onejudge-v0.19.0
    TAG_SHA="$(git -C "$REPO" rev-parse HEAD)"
    release "$REPO" 0.20.0
    MAIN_SHA="$(git -C "$REPO" rev-parse HEAD)"
    cd "$REPO" || return

    export GITHUB_OUTPUT="$BATS_TEST_TMPDIR/output" GITHUB_EVENT_PATH="$BATS_TEST_TMPDIR/event.json"
    export REQUESTS="$BATS_TEST_TMPDIR/requests" INDEX="$BATS_TEST_TMPDIR/index"
    : >"$GITHUB_OUTPUT"
    : >"$INDEX"
    # The index: `<status> <url>` lines in $INDEX; a URL it does not list is 404.
    double curl <<'EOF'
url="${*: -1}"
echo "$url" >>"$REQUESTS"
status="$(awk -v url="$url" '$2 == url { print $1 }' "$INDEX")"
printf '%s' "${status:-404}"
EOF
}

# release DIR VERSION — commit a Cargo.toml releasing VERSION.
release() {
    printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "%s"\n' "$2" >"$1/Cargo.toml"
    commit "$1" "chore: release v$2"
}

# serves NAME VERSION [STATUS] — the index answers STATUS (200) for NAME at VERSION.
serves() {
    echo "${3:-200} https://pypi.org/pypi/$1/$2/json" >>"$INDEX"
}

dispatch() {
    printf '{"inputs": {"tag": "%s"}, "ref": "refs/heads/main"}\n' "$1" >"$GITHUB_EVENT_PATH"
    GITHUB_EVENT_NAME=workflow_dispatch run --separate-stderr "$ROOT/scripts/release-pypi-plan.sh"
}

published() {
    printf '{"action": "published", "release": {"tag_name": "%s"}}\n' "$1" >"$GITHUB_EVENT_PATH"
    GITHUB_EVENT_NAME=release run --separate-stderr "$ROOT/scripts/release-pypi-plan.sh"
}

# planned KEY — the value the plan wrote for KEY.
planned() {
    sed -n "s/^$1=//p" "$GITHUB_OUTPUT"
}

@test "a hand-run naming a tag builds that tag's tree, not the default branch's" {
    serves onejudge-cli 0.19.0

    dispatch onejudge-v0.19.0

    [ "$status" -eq 0 ]
    [ "$(planned tag)" = onejudge-v0.19.0 ]
    [ "$(planned sha)" = "$TAG_SHA" ]
    [ "$(planned sha)" != "$MAIN_SHA" ]
    [ "$(planned version)" = 0.19.0 ]
}

@test "a hand-run for a tag whose CLI is on the index selects only the SDK" {
    serves onejudge-cli 0.19.0
    serves onejudge 0.17.2

    dispatch onejudge-v0.19.0

    [ "$status" -eq 0 ]
    [ "$(planned upload-cli)" = false ]
    [ "$(planned upload-sdk)" = true ]
    [ "$output" = "release-pypi-plan: onejudge-v0.19.0 ($TAG_SHA) version 0.19.0: upload onejudge-cli=false onejudge=true" ]
    [ "$(cat "$REQUESTS")" = "https://pypi.org/pypi/onejudge-cli/0.19.0/json
https://pypi.org/pypi/onejudge/0.19.0/json" ]
}

@test "a hand-run for a tag whose SDK is on the index but whose CLI is not selects only the CLI" {
    serves onejudge 0.19.0

    dispatch onejudge-v0.19.0

    [ "$status" -eq 0 ]
    [ "$(planned upload-cli)" = true ]
    [ "$(planned upload-sdk)" = false ]
}

@test "a hand-run for a tag whose distributions are all on the index uploads nothing and succeeds" {
    serves onejudge-cli 0.18.1
    serves onejudge 0.18.1

    dispatch onejudge-v0.18.1

    [ "$status" -eq 0 ]
    [ "$(planned upload-cli)" = false ]
    [ "$(planned upload-sdk)" = false ]
    [ "$(planned version)" = 0.18.1 ]
}

@test "a published Release selects the CLI and the SDK, as before, without asking the index" {
    serves onejudge-cli 0.19.0
    serves onejudge 0.19.0

    published onejudge-v0.19.0

    [ "$status" -eq 0 ]
    [ "$(planned sha)" = "$TAG_SHA" ]
    [ "$(planned version)" = 0.19.0 ]
    [ "$(planned upload-cli)" = true ]
    [ "$(planned upload-sdk)" = true ]
    [ ! -e "$REQUESTS" ]
}

@test "an older v-prefixed tag is accepted" {
    git tag v0.20.0

    published v0.20.0

    [ "$status" -eq 0 ]
    [ "$(planned sha)" = "$MAIN_SHA" ]
}

@test "a name that is no tag fails, writing no plan" {
    dispatch main

    [ "$status" -eq 1 ]
    [ "$stderr" = "release-pypi-plan: 'main' is not a tag of this repository
ACTION: pass an existing release tag (git ls-remote --tags origin), e.g. onejudge-v0.19.0" ]
    [ ! -s "$GITHUB_OUTPUT" ]
}

@test "a tag that does not name its tree's version fails" {
    git tag onejudge-v9.9.9 "$TAG_SHA"

    dispatch onejudge-v9.9.9

    [ "$status" -eq 1 ]
    [ "${stderr%%$'\n'*}" = "release-pypi-plan: tag 'onejudge-v9.9.9' does not name the version its tree releases (0.19.0)" ]
}

@test "a tag whose tree has no workspace version fails" {
    printf '[package]\nversion = "1.0.0"\n' >Cargo.toml
    commit . "chore: no workspace version"
    git tag onejudge-v1.0.0

    dispatch onejudge-v1.0.0

    [ "$status" -eq 1 ]
    [ "${stderr%%$'\n'*}" = "release-pypi-plan: Cargo.toml at onejudge-v1.0.0 has no [workspace.package] version" ]
}

@test "a tag whose tree has no Cargo.toml fails" {
    git rm -q Cargo.toml
    commit . "chore: no manifest"
    git tag onejudge-v2.0.0

    dispatch onejudge-v2.0.0

    [ "$status" -eq 1 ]
    [ "${stderr##*$'\n'}" = "ACTION: pass a tag whose tree is a onejudge release" ]
}

@test "an index answer that is neither held nor absent fails rather than guessing" {
    serves onejudge-cli 0.19.0 503

    dispatch onejudge-v0.19.0

    [ "$status" -eq 1 ]
    [ "$stderr" = "release-pypi-plan: https://pypi.org/pypi/onejudge-cli/0.19.0/json answered HTTP 503
ACTION: re-run the workflow once PyPI answers 200 or 404" ]
    [ ! -s "$GITHUB_OUTPUT" ]
}

@test "an unreachable index fails the plan" {
    double curl <<'EOF'
echo "curl: (6) Could not resolve host: pypi.org" >&2
exit 6
EOF

    dispatch onejudge-v0.19.0

    [ "$status" -eq 1 ]
    [ "${stderr##*$'\n'}" = "ACTION: re-run the workflow once PyPI answers" ]
    [ ! -s "$GITHUB_OUTPUT" ]
}

@test "a hand-run naming a malformed tag fails rather than repairing it" {
    dispatch "onejudge-v0.19 .0"

    [ "$status" -eq 1 ]
    [ "${stderr%%$'\n'*}" = "release-pypi-plan: the workflow_dispatch payload names 'onejudge-v0.19 .0', which is not a tag name" ]
    [ ! -s "$GITHUB_OUTPUT" ]
}

@test "a hand-run with an empty tag fails" {
    dispatch ""

    [ "$status" -eq 1 ]
    [ "${stderr##*$'\n'}" = "ACTION: pass the release tag exactly, e.g. onejudge-v0.19.0" ]
}

@test "an output file it cannot write fails the plan" {
    echo '{"release": {"tag_name": "onejudge-v0.19.0"}}' >"$GITHUB_EVENT_PATH"

    GITHUB_OUTPUT="$BATS_TEST_TMPDIR" GITHUB_EVENT_NAME=release run --separate-stderr "$ROOT/scripts/release-pypi-plan.sh"

    [ "$status" -eq 1 ]
    [ "${stderr##*$'\n'}" = "ACTION: point GITHUB_OUTPUT at a writable file" ]
}

@test "a payload without a tag fails" {
    echo '{"inputs": {}}' >"$GITHUB_EVENT_PATH"

    GITHUB_EVENT_NAME=workflow_dispatch run --separate-stderr "$ROOT/scripts/release-pypi-plan.sh"

    [ "$status" -eq 1 ]
    [ "${stderr##*$'\n'}" = "ACTION: pass the release tag, e.g. onejudge-v0.19.0" ]
}

@test "any other event fails" {
    GITHUB_EVENT_NAME=push run --separate-stderr "$ROOT/scripts/release-pypi-plan.sh"

    [ "$status" -eq 1 ]
    [ "${stderr%%$'\n'*}" = "release-pypi-plan: event 'push' does not start a PyPI release" ]
}

@test "outside a workflow job it fails, naming what is missing" {
    unset GITHUB_OUTPUT

    GITHUB_EVENT_NAME=release run --separate-stderr "$ROOT/scripts/release-pypi-plan.sh"

    [ "$status" -eq 1 ]
    [ "${stderr%%$'\n'*}" = "release-pypi-plan: GITHUB_OUTPUT is not set" ]
}

# steps — every step of release-pypi.yml as `job|uses|if|ref`.
steps() {
    awk '
        function flush() { if (job != "" && step) print job "|" uses "|" cond "|" ref; step = 0 }
        /^jobs:/ { in_jobs = 1; next }
        in_jobs && /^  [a-z-]+:$/ { flush(); job = $1; sub(/:$/, "", job); next }
        /^      - / { flush(); step = 1; uses = ""; cond = ""; ref = "" }
        step && /^      - uses: |^        uses: / { uses = $NF }
        step && /^        if: / { cond = $0; sub(/^        if: /, "", cond) }
        step && /^          ref: / { ref = $0; sub(/^          ref: /, "", ref) }
        END { flush() }
    ' "$ROOT/.github/workflows/release-pypi.yml"
}

@test "release-pypi.yml's plan job publishes exactly the keys the script writes" {
    published onejudge-v0.19.0
    [ "$status" -eq 0 ]
    written="$(cut -d= -f1 "$GITHUB_OUTPUT")"
    # The plan job's `outputs:` block, as `key: expression` lines.
    mapped="$(sed -n '/^  plan:$/,/^    steps:$/p' "$ROOT/.github/workflows/release-pypi.yml" | sed -n 's/^      \([a-z-]*\): \(.*\)$/\1 \2/p')"

    [ "$(cut -d' ' -f1 <<<"$mapped")" = "$written" ]
    while read -r key expression; do
        [ "$expression" = "\${{ steps.plan.outputs.$key }}" ]
    done <<<"$mapped"
}

@test "release-pypi.yml verifies the planned version, not the ref it runs from" {
    verify="$(sed -n '/^  verify-pypi:$/,$p' "$ROOT/.github/workflows/release-pypi.yml")"

    grep -qxF "          VERSION: \${{ needs.plan.outputs.version }}" <<<"$verify"
    grep -qxF "          ver=\"\$VERSION\"" <<<"$verify"
    [ "$(grep -c GITHUB_REF <<<"$verify")" -eq 0 ]
}

@test "release-pypi.yml checks out the planned tree in every job after the plan" {
    run steps

    [ "$status" -eq 0 ]
    checkouts="$(grep '|actions/checkout@' <<<"$output")"
    [ "$(grep -c . <<<"$checkouts")" -eq 4 ]
    [ "$(grep -c '^plan|' <<<"$checkouts")" -eq 1 ]
    later="$(grep -v '^plan|' <<<"$checkouts")"
    [ "$(grep -cF "|\${{ needs.plan.outputs.sha }}" <<<"$later")" -eq 3 ]
}

@test "release-pypi.yml uploads each distribution only when the plan selected it" {
    run steps

    [ "$status" -eq 0 ]
    [ "$(grep '|pypa/gh-action-pypi-publish@' <<<"$output")" = "publish-cli|pypa/gh-action-pypi-publish@release/v1|needs.plan.outputs.upload-cli == 'true'|
publish-sdk|pypa/gh-action-pypi-publish@release/v1|needs.plan.outputs.upload-sdk == 'true'|" ]
}

@test "release-pypi.yml installs the pinned toolchain before the SDK check" {
    build_sdk="$(sed -n '/^  build-sdk:$/,/^  [a-z-]*:$/p' "$ROOT/.github/workflows/release-pypi.yml")"
    rustup="$(grep -n '^ *run: rustup show$' <<<"$build_sdk" | cut -d: -f1)"
    check="$(grep -n '^ *- run: just python-sdk-check$' <<<"$build_sdk" | cut -d: -f1)"

    [ -n "$rustup" ]
    [ -n "$check" ]
    [ "$rustup" -lt "$check" ]
}
