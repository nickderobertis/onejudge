#!/usr/bin/env bats
# scripts/lint-llm-diff.sh, the blocking `llmlint` PR check: it hands llmlint the
# files the branch changed since its fork point, diff-scoped to that fork point.
# The history is a real scratch repository; llmlint is a double that prints the
# arguments it was given and answers with LLMLINT_EXIT.

load helpers

setup() {
    use_doubles
    double llmlint <<'EOF'
printf 'llmlint %s\n' "$*"
exit "${LLMLINT_EXIT:-0}"
EOF
    REPO="$BATS_TEST_TMPDIR/repo"
    git_repo "$REPO"
    echo base >"$REPO/kept.txt"
    echo base >"$REPO/removed.txt"
    commit "$REPO" "chore: base"
    FORK="$(git -C "$REPO" rev-parse HEAD)"
    git -C "$REPO" checkout -q -b feature
    cd "$REPO" || return
}

@test "llmlint gets the changed, still-present files, diff-scoped to the fork point" {
    echo more >>kept.txt
    echo new >added.txt
    git rm -q removed.txt
    commit . "feat: change"
    git -C "$REPO" checkout -q main
    echo later >main-only.txt
    commit . "chore: main moves on"
    git -C "$REPO" checkout -q feature

    LLMLINT_EXIT=1 run "$ROOT/scripts/lint-llm-diff.sh" main -- --format json

    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "lint-llm-diff: linting 2 changed file(s) vs main @ ${FORK:0:9}" ]
    [ "${lines[1]}" = "llmlint --diff --diff-base $FORK --format json added.txt kept.txt" ]
}

@test "a branch with no changes is a clean pass that never runs llmlint" {
    run "$ROOT/scripts/lint-llm-diff.sh" main

    [ "$status" -eq 0 ]
    [ "$output" = "lint-llm-diff: no changed files to lint (base main @ ${FORK:0:9})" ]
}

@test "an unresolvable base falls back to main" {
    echo more >>kept.txt
    commit . "fix: change"

    run "$ROOT/scripts/lint-llm-diff.sh"

    [ "$status" -eq 0 ]
    [ "${lines[0]}" = "lint-llm-diff: 'origin/main' not found; falling back to 'main'" ]
    [ "${lines[2]}" = "llmlint --diff --diff-base $FORK kept.txt" ]
}

@test "no base and no main is a resolve error" {
    git branch -q -D main

    run "$ROOT/scripts/lint-llm-diff.sh" origin/main

    [ "$status" -eq 2 ]
    [ "$output" = "lint-llm-diff: cannot resolve base ref 'origin/main' (and no 'main')" ]
}

@test "unrelated history has no merge base, which is a resolve error" {
    git checkout -q --orphan unrelated
    commit . "chore: unrelated root"

    run "$ROOT/scripts/lint-llm-diff.sh" main

    [ "$status" -eq 2 ]
    [ "$output" = "lint-llm-diff: no merge-base between 'main' and HEAD" ]
}
