#!/usr/bin/env bats
# scripts/python-sdk-release-trigger.sh, which turns release-worthy SDK-only
# history into the crate commit release-plz sees. Its journeys are
# scripts/check-python-sdk-release-trigger.sh — the check the gate ran before this
# suite, run here unchanged — plus the refusals that check does not reach.

load helpers

@test "SDK fixes, features and breaking changes become the crate prefix; crate and docs changes do not" {
    run "$ROOT/scripts/check-python-sdk-release-trigger.sh"

    [ "$status" -eq 0 ]
    [ "$output" = "check-python-sdk-release-trigger: ok" ]
}

@test "anything but a base and a head is a usage error" {
    run "$ROOT/scripts/python-sdk-release-trigger.sh" HEAD

    [ "$status" -eq 2 ]
    [ "$output" = "usage: $ROOT/scripts/python-sdk-release-trigger.sh <base-revision> <head-revision>" ]
}

@test "a revision that names no commit fails" {
    git_repo "$BATS_TEST_TMPDIR/repo"
    commit "$BATS_TEST_TMPDIR/repo" "chore: initial"
    cd "$BATS_TEST_TMPDIR/repo"

    run "$ROOT/scripts/python-sdk-release-trigger.sh" no-such-ref HEAD

    [ "$status" -ne 0 ]
    [ "$output" = "fatal: Needed a single revision" ]
}

@test "the check fails, naming the journey, when the trigger answers any journey wrongly" {
    TREE="$BATS_TEST_TMPDIR/tree"
    git_repo "$TREE"
    link "$TREE" scripts/check-python-sdk-release-trigger.sh
    export CALLS="$BATS_TEST_TMPDIR/calls"
    # The answers the five journeys expect, except at call WRONG_AT.
    cat >"$TREE/scripts/python-sdk-release-trigger.sh" <<'TRIGGER'
#!/usr/bin/env bash
echo x >>"$CALLS"
call="$(wc -l <"$CALLS" | tr -d ' ')"
expected=(fix '' '' feat 'feat!')
if [ "$call" = "$WRONG_AT" ]; then echo wrong; else echo "${expected[$((call - 1))]}"; fi
TRIGGER
    chmod +x "$TREE/scripts/python-sdk-release-trigger.sh"
    commit "$TREE" "chore: a tree with the check and a scripted trigger"
    cd "$TREE" || return
    local messages=(
        "python SDK fix produced 'wrong', expected fix"
        "crate change redundantly produced 'wrong'"
        "SDK docs produced release prefix 'wrong'"
        "SDK feature range produced 'wrong', expected feat"
        "breaking SDK change produced 'wrong', expected feat!"
    )

    for wrong_at in 1 2 3 4 5; do
        rm -f "$CALLS"
        WRONG_AT="$wrong_at" run "$TREE/scripts/check-python-sdk-release-trigger.sh"
        [ "$status" -eq 1 ]
        [ "$output" = "${messages[$((wrong_at - 1))]}" ]
    done
}
