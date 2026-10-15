#!/usr/bin/env bats
# scripts/check-release-targets.sh, the drift gate holding install.sh's targets to
# release-binaries.yml's producers: over this tree, and over a scratch tree whose
# install.sh or workflow has drifted.

load support/helpers
bats_require_minimum_version 1.5.0

setup() {
    TREE="$BATS_TEST_TMPDIR/tree"
    link "$TREE" scripts/check-release-targets.sh
    mkdir -p "$TREE/.github/workflows"
    cp "$ROOT/install.sh" "$TREE/install.sh"
    cp "$ROOT/.github/workflows/release-binaries.yml" "$TREE/.github/workflows/"
}

@test "this tree's install targets are all built by the release producers" {
    run "$ROOT/scripts/check-release-targets.sh"

    [ "$status" -eq 0 ]
    [ "$output" = "check-release-targets: 2 install target(s) all built by the release producers" ]
}

@test "an install target the producers do not build fails, listing both sides" {
    sed -i.bak 's/target="x86_64-unknown-linux-gnu"/target="riscv64gc-unknown-linux-gnu"/' "$TREE/install.sh"

    run "$TREE/scripts/check-release-targets.sh"

    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "check-release-targets: install.sh downloads target(s) the release producers do not build: riscv64gc-unknown-linux-gnu" ]
    [ "${lines[1]}" = "  install.sh:            aarch64-apple-darwin riscv64gc-unknown-linux-gnu" ]
    [[ "${lines[2]}" == "  release-binaries.yml:  "*x86_64-unknown-linux-gnu* ]]
    [ "${lines[3]}" = "  Fix install.sh's os/arch map or the workflow producer declarations so they agree." ]
}

@test "a workflow whose producers cannot be read fails rather than passing vacuously" {
    : >"$TREE/.github/workflows/release-binaries.yml"

    run "$TREE/scripts/check-release-targets.sh"

    [ "$status" -eq 1 ]
    [ "${lines[0]}" = 'check-release-targets: .github/workflows/release-binaries.yml has no producer target declaration to check against' ]
    [ "${lines[1]}" = "  Restore the workflow's producer target declarations, or update the pattern in this script to its new form." ]
}

@test "an install.sh with no targets fails, naming the file and the fix, rather than ending silently" {
    sed -i.bak '/target="/d' "$TREE/install.sh"

    run "$TREE/scripts/check-release-targets.sh"

    [ "$status" -eq 1 ]
    [ "${lines[0]}" = 'check-release-targets: install.sh has no literal target="<triple>" assignment to check' ]
    [ "${lines[1]}" = "  Restore install.sh's os/arch map of target=\"<triple>\" lines, or update the pattern in this script to its new form." ]
}

@test "an install.sh it cannot read fails, naming the file, rather than reading as no targets" {
    chmod 000 "$TREE/install.sh"

    run --separate-stderr "$TREE/scripts/check-release-targets.sh"

    [ "$status" -eq 1 ]
    [ "${stderr_lines[1]}" = "check-release-targets: reading install.sh failed (above), so its targets are unknown" ]
    [ "${stderr_lines[2]}" = "  Restore install.sh (git checkout -- install.sh), then re-run." ]
    [ "${#stderr_lines[@]}" -eq 3 ]
}

@test "publisher expectations cannot stand in for a removed producer target" {
    sed -i.bak '/^[[:space:]]*target: x86_64-unknown-linux-gnu$/d' "$TREE/.github/workflows/release-binaries.yml"

    run "$TREE/scripts/check-release-targets.sh"

    [ "$status" -eq 1 ]
    [[ "$output" == *"x86_64-unknown-linux-gnu"* ]]
}

@test "an unreadable producer workflow fails without using publisher guesses" {
    rm "$TREE/.github/workflows/release-binaries.yml"

    run --separate-stderr "$TREE/scripts/check-release-targets.sh"

    [ "$status" -eq 1 ]
    [[ "$stderr" == *"targets are unknown"* ]]
    [[ "$stderr" == *"Restore .github/workflows/release-binaries.yml"* ]]
}
