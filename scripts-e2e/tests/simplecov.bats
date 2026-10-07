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

teardown() {
    rm -rf "$ROOT/target/simplecov-glob-$BATS_ROOT_PID"
}

@test "a script list that is empty, reaches outside the repository's files, or holds glob syntax is refused" {
    # An existing file whose name track_files would read as a glob, in ignored target/.
    glob="target/simplecov-glob-$BATS_ROOT_PID/a{b,c}.sh"
    mkdir -p "$ROOT/$(dirname "$glob")"
    printf 'echo glob\n' >"$ROOT/$glob"
    for files in "" $'install.sh\n../etc/passwd' /etc/passwd scripts/no-such-script.sh "$glob"; do
        run bashcov_with workspace "$files"

        [ "$status" -eq 1 ]
        [[ "${lines[0]}" == "shell coverage: SHELL_COVERAGE_FILES must list the project's scripts, each an existing repository-relative file with no glob characters; got "* ]]
        [ "${lines[1]}" = "ACTION: run the project's test target (just _sh-test), which lists them with scripts/shell-files.sh" ]
    done
}
