#!/usr/bin/env bats
# Recovery keeps the original tag and independently holds each published distribution.
load ../../tests/support/helpers
bats_require_minimum_version 1.5.0

setup() {
    use_doubles
    REPO="$BATS_TEST_TMPDIR/repo"
    git_repo "$REPO"
    printf '[workspace.package]\nversion = "0.19.0"\n' >"$REPO/Cargo.toml"
    commit "$REPO" "ci: release fixture"
    git -C "$REPO" tag v0.19.0
    git -C "$REPO" tag onejudge-v0.19.0
    SHA="$(git -C "$REPO" rev-parse HEAD)"
    cd "$REPO" || return
    export GITHUB_OUTPUT="$BATS_TEST_TMPDIR/output" GITHUB_EVENT_PATH="$BATS_TEST_TMPDIR/event.json"
    export INDEX="$BATS_TEST_TMPDIR/index"
    : >"$GITHUB_OUTPUT"
    : >"$INDEX"
    # llmlint: ignore[e2e_not_mocked] The task permits registry/forge transport doubles and forbids new paid probes: this suite executes production release-pypi-plan.sh with real git tags, replacing only PyPI HTTP rather than the plan under test.
    double curl <<'DOUBLE'
url="${*: -1}"
status="$(awk -v url="$url" '$2 == url { print $1 }' "$INDEX")"
printf '%s' "${status:-404}"
DOUBLE
}

@test "manual alias recovery preserves the exact tag and holds an already published CLI" {
    echo '200 https://pypi.org/pypi/onejudge-cli/0.19.0/json' >"$INDEX"
    printf '{"inputs":{"tag":"onejudge-v0.19.0"}}\n' >"$GITHUB_EVENT_PATH"
    GITHUB_EVENT_NAME=workflow_dispatch run "$ROOT/scripts/release-pypi-plan.sh"
    [ "$status" -eq 0 ]
    [[ "$(cat "$GITHUB_OUTPUT")" == *"tag=onejudge-v0.19.0"* ]]
    [[ "$(cat "$GITHUB_OUTPUT")" == *"sha=$SHA"* ]]
    [[ "$(cat "$GITHUB_OUTPUT")" == *"upload-cli=false"* ]]
    [[ "$(cat "$GITHUB_OUTPUT")" == *"upload-sdk=true"* ]]
}

@test "manual recovery holds the SDK independently while selecting its missing CLI" {
    echo '200 https://pypi.org/pypi/onejudge/0.19.0/json' >"$INDEX"
    printf '{"inputs":{"tag":"v0.19.0"}}\n' >"$GITHUB_EVENT_PATH"
    GITHUB_EVENT_NAME=workflow_dispatch run "$ROOT/scripts/release-pypi-plan.sh"
    [ "$status" -eq 0 ]
    [[ "$(cat "$GITHUB_OUTPUT")" == *"upload-cli=true"* ]]
    [[ "$(cat "$GITHUB_OUTPUT")" == *"upload-sdk=false"* ]]
}

@test "manual recovery refuses a tag whose committed version does not match" {
    git tag v0.20.0
    printf '{"inputs":{"tag":"v0.20.0"}}\n' >"$GITHUB_EVENT_PATH"
    GITHUB_EVENT_NAME=workflow_dispatch run "$ROOT/scripts/release-pypi-plan.sh"
    [ "$status" -ne 0 ]
    [[ "$output" == *"does not name the version its tree releases"* ]]
    [ ! -s "$GITHUB_OUTPUT" ]
}
