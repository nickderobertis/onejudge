#!/usr/bin/env bats
# The scripts that drive cargo for the gate: scripts/cargo-target-dir.sh (where
# the instrumented suites' doubles are) and scripts/check-judge-seat-frames.sh
# (the judge-seat frame bundle's drift and version checks). The target directory
# is asked of the real cargo; the frame checks' cargo is a double recording what
# it was asked to run, because the real one builds the crate, which
# `onejudge:lint` already does in the gate.

load helpers

@test "cargo-target-dir prints the target directory cargo resolves" {
    run "$ROOT/scripts/cargo-target-dir.sh"

    [ "$status" -eq 0 ]
    [ "$output" = "$(cd "$ROOT" && cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')" ]
}

@test "cargo-target-dir names a failed cargo metadata and what to do" {
    use_doubles
    double cargo <<'EOF'
echo "error: failed to parse manifest" >&2
exit 101
EOF

    run "$ROOT/scripts/cargo-target-dir.sh"

    [ "$status" -eq 1 ]
    [ "${lines[1]}" = "cargo-target-dir: \`cargo metadata\` failed (above)" ]
    [ "${lines[2]}" = "ACTION: fix the Cargo.toml it names, then re-run the recipe" ]
}

@test "cargo-target-dir refuses metadata that is not JSON or names no target directory" {
    use_doubles
    double cargo <<'EOF'
echo "${METADATA}"
EOF

    METADATA='not json' run "$ROOT/scripts/cargo-target-dir.sh"
    [ "$status" -eq 1 ]
    [[ "${lines[0]}" == "cargo-target-dir: cargo metadata printed no JSON: "* ]]

    METADATA='{"target_directory":""}' run "$ROOT/scripts/cargo-target-dir.sh"
    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "cargo-target-dir: cargo metadata named no target_directory" ]
}

# frames_repo — a repository with `origin/main` whose base carries the bundle,
# and a cargo double logging each invocation to CARGO_LOG.
frames_repo() {
    use_doubles
    CARGO_LOG="$BATS_TEST_TMPDIR/cargo.log"
    export CARGO_LOG
    double cargo <<'EOF'
printf '%s\n' "$*" >>"$CARGO_LOG"
EOF
    REPO="$BATS_TEST_TMPDIR/repo"
    git_repo "$REPO"
    mkdir -p "$REPO/schemas"
    echo '{"version":1}' >"$REPO/schemas/judge-seat-frames.json"
    commit "$REPO" "chore: base"
    git -C "$REPO" update-ref refs/remotes/origin/main HEAD
    echo '{"version":2}' >"$REPO/schemas/judge-seat-frames.json"
    commit "$REPO" "feat: next frames"
    cd "$REPO" || return
}

@test "check-judge-seat-frames checks the bundle, then its version against the merge base's" {
    frames_repo
    export ONEPIPELINE_NODE_SCRATCH_DIR="$BATS_TEST_TMPDIR/scratch"

    run "$ROOT/scripts/check-judge-seat-frames.sh"

    [ "$status" -eq 0 ]
    [ "$(sed -n 1p "$CARGO_LOG")" = "run -q -p onejudge --features sdk-schema --example generate_judge_seat_frames -- --check" ]
    [ "$(sed -n 2p "$CARGO_LOG")" = "run -q -p onejudge --features sdk-schema --example check_judge_seat_frame_version -- $ONEPIPELINE_NODE_SCRATCH_DIR/judge-seat-frames.base.json schemas/judge-seat-frames.json" ]
    [ "$(cat "$ONEPIPELINE_NODE_SCRATCH_DIR/judge-seat-frames.base.json")" = '{"version":1}' ]
}

@test "check-judge-seat-frames writes the base bundle under target by default" {
    frames_repo
    unset ONEPIPELINE_NODE_SCRATCH_DIR

    run "$ROOT/scripts/check-judge-seat-frames.sh"

    [ "$status" -eq 0 ]
    [ "$(cat target/judge-seat-frames.base.json)" = '{"version":1}' ]
}

@test "check-judge-seat-frames only checks the bundle when there is no base to compare" {
    frames_repo
    git update-ref -d refs/remotes/origin/main

    run "$ROOT/scripts/check-judge-seat-frames.sh"

    [ "$status" -eq 0 ]
    [ "$(wc -l <"$CARGO_LOG" | tr -d ' ')" = 1 ]
}

@test "check-judge-seat-frames fails when the generated bundle drifted" {
    frames_repo
    double cargo <<'EOF'
echo "schemas/judge-seat-frames.json is stale" >&2
exit 1
EOF

    run "$ROOT/scripts/check-judge-seat-frames.sh"

    [ "$status" -eq 1 ]
    [ "$output" = "schemas/judge-seat-frames.json is stale" ]
}
