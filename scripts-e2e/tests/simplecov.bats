#!/usr/bin/env bats
# .simplecov, the configuration every bashcov run loads, through the real pinned
# bashcov: the environment `_sh-test` hands it is refused, naming the fix, when
# it names no project or lists anything but the repository's own files.

load ../../tests/support/helpers

# bashcov_with PROJECT FILES — a bashcov run from the root, as `_sh-test` starts one.
bashcov_with() {
    (
        cd "$ROOT" || exit
        SHELL_COVERAGE_PROJECT="$1" SHELL_COVERAGE_FILES="$2" \
            pixi run --locked bundle exec bashcov -- /bin/true
    )
}

@test "a project name that is not an Nx name is refused" {
    run bashcov_with ../escape install.sh

    [ "$status" -eq 1 ]
    [ "${lines[0]}" = 'shell coverage: SHELL_COVERAGE_PROJECT="../escape" is not an Nx project name (a-z, 0-9, -)' ]
    [ "${lines[1]}" = "ACTION: run the project's test target (just _sh-test), which sets it" ]
}

# Scratch files must be inside the repository to be measured; target/ is ignored.
setup() {
    SCRATCH="target/simplecov-e2e-$BATS_ROOT_PID-$BATS_TEST_NUMBER"
    PROJECT="simplecov-e2e-$BATS_ROOT_PID-$BATS_TEST_NUMBER"
    mkdir -p "$ROOT/$SCRATCH"
}

teardown() {
    rm -rf "${ROOT:?}/$SCRATCH" "${ROOT:?}/target/shell-coverage/$PROJECT"
}

@test "a script list that is empty, misspelled, reaches outside the repository, or holds glob syntax is refused" {
    # Existing files track_files would read as a glob, or that resolve outside.
    printf 'echo glob\n' >"$ROOT/$SCRATCH/a{b,c}.sh"
    ln -s /etc/hostname "$ROOT/$SCRATCH/outside.sh"
    for files in "" $'install.sh\n../etc/passwd' /etc/passwd scripts/no-such-script.sh "$SCRATCH/a{b,c}.sh" \
        ./install.sh scripts//nx scripts/../install.sh "$SCRATCH/outside.sh"; do
        run bashcov_with workspace "$files"

        [ "$status" -eq 1 ]
        [[ "${lines[0]}" == "shell coverage: SHELL_COVERAGE_FILES must list the project's scripts, each an existing file inside the repository, spelled canonically relative to its root, with no glob characters; got "* ]]
        [ "${lines[1]}" = "ACTION: run the project's test target (just _sh-test), which lists them with scripts/shell-files.sh" ]
    done
}

@test "a listed script no test runs is reported uncovered, beside the one that ran" {
    printf '#!/usr/bin/env bash\necho ran\n' >"$ROOT/$SCRATCH/ran.sh"
    printf '#!/usr/bin/env bash\necho never\necho run\n' >"$ROOT/$SCRATCH/unrun.sh"
    chmod +x "$ROOT/$SCRATCH/ran.sh"
    (
        cd "$ROOT" || exit
        SHELL_COVERAGE_PROJECT="$PROJECT" SHELL_COVERAGE_FILES="$SCRATCH/ran.sh"$'\n'"$SCRATCH/unrun.sh" \
            pixi run --locked bundle exec bashcov --skip-uncovered --command-name "$PROJECT" -- "$SCRATCH/ran.sh" >/dev/null
    )

    run node -e '
        const coverage = Object.values(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")))[0].coverage;
        for (const [file, { lines }] of Object.entries(coverage)) {
            const counted = lines.filter((hits) => hits !== null);
            console.log(file.slice(file.lastIndexOf("/") + 1) + " " + counted.filter((hits) => hits > 0).length + "/" + counted.length);
        }
    ' "$ROOT/target/shell-coverage/$PROJECT/.resultset.json"

    [ "$status" -eq 0 ]
    [ "$(sort <<<"$output")" = "$(printf '%s\n' 'ran.sh 1/1' 'unrun.sh 0/2')" ]
}
