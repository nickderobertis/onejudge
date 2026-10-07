#!/usr/bin/env bats
# .simplecov, the configuration every bashcov run loads, read by the real pinned
# Ruby and SimpleCov: the environment `_sh-test` hands it is refused, naming the
# fix, when it names no project or lists anything but the repository's files.

load ../../tests/support/helpers

# load_config PROJECT FILES — .simplecov loaded as bashcov loads it, from the root.
load_config() {
    (
        cd "$ROOT" || exit
        SHELL_COVERAGE_PROJECT="$1" SHELL_COVERAGE_FILES="$2" \
            pixi run --locked bundle exec ruby -e 'require "simplecov"; load ".simplecov"'
    )
}

@test "a project name that is not an Nx name is refused" {
    run load_config ../escape install.sh

    [ "$status" -eq 1 ]
    [ "${lines[0]}" = 'shell coverage: SHELL_COVERAGE_PROJECT="../escape" is not an Nx project name (a-z, 0-9, -)' ]
    [ "${lines[1]}" = "ACTION: run the project's test target (just _sh-test), which sets it" ]
}

@test "a script list that is empty, or reaches outside the repository's files, is refused" {
    for files in "" $'install.sh\n../etc/passwd' /etc/passwd scripts/no-such-script.sh; do
        run load_config workspace "$files"

        [ "$status" -eq 1 ]
        [[ "${lines[0]}" == "shell coverage: SHELL_COVERAGE_FILES must list the project's scripts, each an existing repository-relative file; got "* ]]
        [ "${lines[1]}" = "ACTION: run the project's test target (just _sh-test), which lists them with scripts/shell-files.sh" ]
    done
}
