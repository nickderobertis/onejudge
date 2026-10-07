#!/usr/bin/env bats
# The shell coverage contract, reconciled over this tree: each project's `test`
# target that runs `just _sh-test <root> <project>` names itself and its own root,
# declares as its Nx output the directory .simplecov writes for that project, and
# is one the repo-level `coverage` target depends on; the merge reads the reports
# from exactly those directories. A rename on one side fails here, not as a
# silently smaller merge.

load support/helpers

# shell_tests — `<dir> <name> <command project> <command root> <outputs>` for every
# project whose test runs _sh-test, then `coverage <deps>` for workspace:coverage.
shell_tests() {
    git -C "$ROOT" ls-files '*project.json' | node -e '
        const fs = require("fs");
        const path = require("path");
        let coverage = [];
        for (const file of fs.readFileSync(0, "utf8").split("\n").filter(Boolean)) {
            const project = JSON.parse(fs.readFileSync(path.join(process.argv[1], file), "utf8"));
            const dir = path.dirname(file);
            const test = (project.targets || {}).test || {};
            const words = (test.command || "").split(/\s+/);
            if (words[0] === "just" && words[1] === "_sh-test") {
                console.log([dir, project.name, words[3], words[2], (test.outputs || []).join(",")].join(" "));
            }
            for (const dep of (((project.targets || {}).coverage || {}).dependsOn || [])) {
                if (dep.target === "test") coverage = coverage.concat(dep.projects);
            }
        }
        console.log("coverage " + coverage.join(","));
    ' "$ROOT"
}

@test "every shell test writes its report where its outputs say, and the coverage target merges it" {
    run shell_tests
    [ "$status" -eq 0 ]
    deps="$(sed -n 's/^coverage //p' <<<"$output")"
    count=0
    while read -r dir name project test_root outputs; do
        [ "$dir" = coverage ] && continue
        [ "$project" = "$name" ]
        [ "$test_root" = "$dir" ]
        [ "$outputs" = "{workspaceRoot}/target/shell-coverage/$name" ]
        grep -qxF "$name" <<<"${deps//,/$'\n'}"
        count=$((count + 1))
    done <<<"$output"
    [ "$count" -eq 2 ]

    # .simplecov's report directory for SHELL_COVERAGE_PROJECT, and the merge's input.
    grep -qF 'SimpleCov.coverage_dir File.join("target", "shell-coverage", project)' "$ROOT/.simplecov"
    grep -qF 'project = ENV["SHELL_COVERAGE_PROJECT"]' "$ROOT/.simplecov"
    grep -qF "export SHELL_COVERAGE_PROJECT=\"\$project\"" "$ROOT/justfile"
    grep -qF "rm -rf \"target/shell-coverage/\$project\"" "$ROOT/justfile"
    grep -qF 'reports = Dir["target/shell-coverage/*/.resultset.json"].sort' "$ROOT/justfile"
}

@test "a shell test run with a malformed project name or root is refused before anything runs" {
    for args in "tests/support Bad;Name" "nowhere workspace" ". workspace nowhere"; do
        # SHELLOPTS unset: bashcov's tracing would follow into just's temporary
        # recipe script, then warn that the deleted file cannot be reported.
        # shellcheck disable=SC2086 # each case is the recipe's argument list, split on purpose.
        run env -u SHELLOPTS just --justfile "$ROOT/justfile" --working-directory "$ROOT" _sh-test $args

        [ "$status" -ne 0 ]
        [[ "${lines[0]}" == "shell coverage: _sh-test needs a project root, an Nx project name (a-z, 0-9, -) and the root it covers; got "* ]]
        [ "${lines[1]}" = "ACTION: call it as the project's test target does (just _sh-test <root> <name> [<covered root>])" ]
    done
}

@test "the shell recipes take a root or project name as an argument, never as shell source" {
    marker="$BATS_TEST_TMPDIR/injected"
    for recipe in _sh-format _sh-format-check _sh-lint; do
        run env -u SHELLOPTS just --justfile "$ROOT/justfile" --working-directory "$ROOT" "$recipe" "nowhere; touch $marker"

        [ "$status" -ne 0 ]
        [[ "$output" == *"shell-files: 'nowhere; touch $marker' is not a project root (no nowhere; touch $marker/project.json)"* ]]
        [ ! -e "$marker" ]
    done

    run env -u SHELLOPTS just --justfile "$ROOT/justfile" --working-directory "$ROOT" _sh-lint tests/support "x; touch $marker"
    [ "$status" -ne 0 ]
    [ "${lines[0]}" = "shell lint: 'x; touch $marker' is not an Nx project name (a-z, 0-9, -)" ]
    [ ! -e "$marker" ]
}
