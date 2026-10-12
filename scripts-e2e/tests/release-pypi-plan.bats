#!/usr/bin/env bats
# The release plan and canonical producer selection meet over real git tags.
load ../../tests/support/helpers
bats_require_minimum_version 1.5.0

setup() {
    REPO="$BATS_TEST_TMPDIR/repo"
    git_repo "$REPO"
    printf '[workspace.package]\nversion = "0.19.0"\n' >"$REPO/Cargo.toml"
    commit "$REPO" "ci: release fixture"
    git -C "$REPO" tag v0.19.0
    git -C "$REPO" tag onejudge-v0.19.0
    SHA="$(git -C "$REPO" rev-parse HEAD)"
    cd "$REPO" || return
}

@test "both tag aliases choose the same automatic producer and only its push builds" {
    for tag in v0.19.0 onejudge-v0.19.0; do
        TAG="$tag" GITHUB_EVENT_NAME=push run python3 "$ROOT/pypi/producer_plan.py"
        [ "$status" -eq 0 ]
        [[ "$output" == *"tag=v0.19.0"* ]]
        [[ "$output" == *"sha=$SHA"* ]]
        [[ "$output" == *"name=onejudge-darwin-v0.19.0-$SHA"* ]]
        if [ "$tag" = v0.19.0 ]; then
            [[ "$output" == *"build=true"* ]]
        else
            [[ "$output" == *"build=false"* ]]
        fi
    done
}

@test "an alias alone builds and diagnostic recovery resolves the same existing commit" {
    git tag -d v0.19.0
    TAG=onejudge-v0.19.0 GITHUB_EVENT_NAME=push run python3 "$ROOT/pypi/producer_plan.py"
    [ "$status" -eq 0 ]
    [[ "$output" == *"tag=onejudge-v0.19.0"* ]]
    [[ "$output" == *"build=true"* ]]
    TAG=onejudge-v0.19.0 GITHUB_EVENT_NAME=workflow_dispatch run python3 "$ROOT/pypi/producer_plan.py"
    [ "$status" -eq 0 ]
    [[ "$output" == *"sha=$SHA"* ]]
}

@test "a version-mismatched tag cannot start a producer" {
    git tag v0.20.0
    TAG=v0.20.0 GITHUB_EVENT_NAME=push run python3 "$ROOT/pypi/producer_plan.py"
    [ "$status" -ne 0 ]
    [[ "$output" == *"tag does not name the Cargo version"* ]]
}
