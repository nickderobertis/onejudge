#!/usr/bin/env bats
# scripts/shell-files.sh, the list every shell target reads: a project's shell
# sources, found by name or shebang, owned by the deepest project.json above them,
# and a refusal for one owned by a project with no shell targets. Over this tree
# and over scratch repositories.

load support/helpers
bats_require_minimum_version 1.5.0

# A project.json declaring the shell targets over the project at $1.
shell_project() {
    printf '{"targets":{"format":{"command":"just _sh-format %s"},"format-check":{"command":"just _sh-format-check %s"},"lint":{"options":{"commands":["node check.mjs","just _sh-lint %s %s"]}}}}\n' "$1" "$1" "$1" "${1//\//-}"
}

# fails TOOL CODE [MARK] — TOOL first on PATH, exiting CODE when its arguments
# contain MARK (any arguments, by default), and running the real TOOL otherwise.
fails() {
    local real
    real="$(command -v "$1")"
    use_doubles
    double "$1" <<EOF
case " \$* " in *"${3:-}"*)
    echo "$1: induced failure" >&2
    exit $2
    ;;
esac
exec "$real" "\$@"
EOF
}

setup() {
    REPO="$BATS_TEST_TMPDIR/repo"
    git_repo "$REPO"
    link "$REPO" scripts/shell-files.sh
    link "$REPO" scripts/shell-targets.mjs
    shell_project . >"$REPO/project.json"
}

@test "this tree's three shell projects own the scripts, the helpers and the e2e suite" {
    run "$ROOT/scripts/shell-files.sh" .
    [ "$status" -eq 0 ]
    for file in install.sh scripts/nx scripts/gate-plan.sh tests/shell-files.bats; do
        grep -qxF "$file" <<<"$output"
    done
    [[ "$output" != *tests/support/* && "$output" != *scripts-e2e/* && "$output" != *.mjs* ]]

    run "$ROOT/scripts/shell-files.sh" tests/support
    [ "$status" -eq 0 ]
    [ "$output" = tests/support/helpers.bash ]

    run "$ROOT/scripts/shell-files.sh" scripts-e2e
    [ "$status" -eq 0 ]
    grep -qxF scripts-e2e/tests/nx.bats <<<"$output"
}

@test "shell sources are found by name or shebang, tracked or untracked, never ignored" {
    mkdir -p "$REPO/bin" "$REPO/lib"
    printf 'echo a\n' >"$REPO/bin/a.sh"
    printf '#!/bin/sh\necho b\n' >"$REPO/bin/b"
    printf '#!/usr/bin/env bash\necho c\n' >"$REPO/bin/c"
    printf '#!/usr/bin/env bats\n' >"$REPO/bin/d"
    printf 'f() { :; }\n' >"$REPO/lib/e.bash"
    printf '#!/usr/bin/env node\n' >"$REPO/bin/f"
    printf '#!/usr/bin/env bashful\n' >"$REPO/bin/g"
    : >"$REPO/bin/empty"
    printf 'echo ignored\n' >"$REPO/bin/ignored.sh"
    echo 'bin/ignored.sh' >"$REPO/.gitignore"
    commit "$REPO" "chore: some tracked"
    printf 'echo untracked\n' >"$REPO/bin/untracked.sh"

    run "$REPO/scripts/shell-files.sh" .

    [ "$status" -eq 0 ]
    [ "$output" = "$(printf '%s\n' bin/a.sh bin/b bin/c bin/d bin/untracked.sh lib/e.bash scripts/shell-files.sh)" ]
}

@test "a nested project owns its scripts, and the root does not list them" {
    mkdir -p "$REPO/tools/sub"
    shell_project tools >"$REPO/tools/project.json"
    printf 'echo t\n' >"$REPO/tools/sub/t.sh"
    printf 'echo r\n' >"$REPO/r.sh"

    run "$REPO/scripts/shell-files.sh" .
    [ "$status" -eq 0 ]
    [ "$output" = "$(printf '%s\n' r.sh scripts/shell-files.sh)" ]

    run "$REPO/scripts/shell-files.sh" tools/
    [ "$status" -eq 0 ]
    [ "$output" = tools/sub/t.sh ]
}

@test "a project inside another non-root project owns its own scripts" {
    mkdir -p "$REPO/tools/inner/bin"
    shell_project tools >"$REPO/tools/project.json"
    shell_project tools/inner >"$REPO/tools/inner/project.json"
    printf 'echo t\n' >"$REPO/tools/t.sh"
    printf 'echo i\n' >"$REPO/tools/inner/bin/i.sh"

    run "$REPO/scripts/shell-files.sh" tools
    [ "$status" -eq 0 ]
    [ "$output" = tools/t.sh ]

    run "$REPO/scripts/shell-files.sh" tools/inner
    [ "$status" -eq 0 ]
    [ "$output" = tools/inner/bin/i.sh ]
}

@test "a shell target may run its recipe as a command, an option, or a listed object" {
    mkdir -p "$REPO/tools"
    echo '{"targets":{"format":{"options":{"command":"just _sh-format tools"}},"format-check":{"options":{"commands":[{"command":"just _sh-format-check tools"}]}},"lint":{"command":"just _sh-lint tools tools"}}}' >"$REPO/tools/project.json"
    printf 'echo t\n' >"$REPO/tools/t.sh"

    run "$REPO/scripts/shell-files.sh" .

    [ "$status" -eq 0 ]
    [ "$output" = scripts/shell-files.sh ]
}

@test "a recipe a target only prints, runs after another step, or splits with a separator, is not a shell target" {
    mkdir -p "$REPO/tools"
    printf 'echo t\n' >"$REPO/tools/t.sh"
    for format in '"echo \"&& just _sh-format tools &&\""' '"echo && just _sh-format tools"' '"just _sh-format\n tools"' '"just _sh-format;tools"'; do
        echo "{\"targets\":{\"format\":{\"command\":$format},\"format-check\":{\"command\":\"just _sh-format-check tools\"},\"lint\":{\"command\":\"just _sh-lint tools\"}}}" >"$REPO/tools/project.json"

        run --separate-stderr "$REPO/scripts/shell-files.sh" .

        [ "$status" -eq 1 ]
        [ "${stderr_lines[0]}" = "shell-files: tools/t.sh is a shell source of 'tools', whose project.json declares no shell targets" ]
    done
}

@test "a script in a project with no shell targets fails, naming the file and the fix" {
    mkdir -p "$REPO/crate"
    echo '{"targets":{"lint":{"command":"just _rust-lint crate"}}}' >"$REPO/crate/project.json"
    printf 'echo x\n' >"$REPO/crate/x.sh"

    run --separate-stderr "$REPO/scripts/shell-files.sh" .

    [ "$status" -eq 1 ]
    [ "$output" = "scripts/shell-files.sh" ]
    [ "${stderr_lines[0]}" = "shell-files: crate/x.sh is a shell source of 'crate', whose project.json declares no shell targets" ]
    [ "${stderr_lines[1]}" = "ACTION: give crate/project.json the shell format, format-check, lint and test targets the root project.json has, or move the script" ]
}

@test "a file it cannot read fails rather than dropping out of the checks" {
    printf '#!/usr/bin/env bash\necho hidden\n' >"$REPO/hidden"
    chmod 000 "$REPO/hidden"

    run --separate-stderr "$REPO/scripts/shell-files.sh" .

    [ "$status" -eq 1 ]
    [ "${stderr_lines[0]}" = "shell-files: hidden cannot be read, so whether it is a shell source is unknown" ]
    [ "${stderr_lines[1]}" = "ACTION: restore read permission on hidden (chmod u+r), then re-run the recipe" ]
}

@test "a name git has to quote fails, naming it, rather than dropping out of the checks" {
    printf 'echo tab\n' >"$REPO/$(printf 'odd\tname.sh')"

    run --separate-stderr "$REPO/scripts/shell-files.sh" .

    [ "$status" -eq 1 ]
    [ "${stderr_lines[0]}" = 'shell-files: git lists "odd\tname.sh" quoted, as its name holds a tab, newline, double quote or backslash' ]
    [ "${stderr_lines[1]}" = "ACTION: rename it without those characters, so the shell targets can read it, then re-run the recipe" ]
}

@test "a binary file is read for its shebang without a warning, and is not a shell source" {
    printf '\000\001#!/bin/bash\000\n' >"$REPO/blob"
    printf '#!/bin/bash\000\necho nul\n' >"$REPO/nul-shebang"

    run --separate-stderr "$REPO/scripts/shell-files.sh" .

    [ "$status" -eq 0 ]
    [ "$stderr" = "" ]
    [ "$output" = "$(printf '%s\n' nul-shebang scripts/shell-files.sh)" ]
}

@test "a project that only mentions the shell recipes, or runs them over another root, has no shell targets" {
    mkdir -p "$REPO/crate" "$REPO/other"
    echo '{"//":"no just _sh-lint crate here","targets":{"lint":{"command":"just _rust-lint crate"}}}' >"$REPO/crate/project.json"
    printf 'echo x\n' >"$REPO/crate/x.sh"
    shell_project crate >"$REPO/other/project.json"
    printf 'echo y\n' >"$REPO/other/y.sh"

    run --separate-stderr "$REPO/scripts/shell-files.sh" .

    [ "$status" -eq 1 ]
    [ "${stderr_lines[0]}" = "shell-files: crate/x.sh is a shell source of 'crate', whose project.json declares no shell targets" ]
    [ "${stderr_lines[2]}" = "shell-files: other/y.sh is a shell source of 'other', whose project.json declares no shell targets" ]
}

@test "a project.json that is not JSON fails, naming it, rather than reading as no shell targets" {
    mkdir -p "$REPO/crate"
    echo '{"targets": just _sh-lint crate' >"$REPO/crate/project.json"
    printf 'echo x\n' >"$REPO/crate/x.sh"

    run --separate-stderr "$REPO/scripts/shell-files.sh" .

    [ "$status" -eq 1 ]
    [[ "${stderr_lines[0]}" == "crate/project.json: "* ]]
    [ "${stderr_lines[1]}" = "ACTION: fix crate/project.json (nx show project <name> reads it the way Nx does), then re-run the recipe" ]
    [ "${stderr_lines[2]}" = "shell-files: reading crate/project.json failed (above), so whether it lints crate/x.sh is unknown" ]
}

@test "a project.json Nx could not read as a project fails, naming what is wrong" {
    mkdir -p "$REPO/crate"
    printf 'echo x\n' >"$REPO/crate/x.sh"
    for shape in 'null' '{"targets":[]}' '{"targets":null}' '{"targets":{"lint":"just _sh-lint crate"}}' '{"targets":{"lint":null}}' \
        '{"targets":{"lint":{"options":null}}}' '{"targets":{"lint":{"options":{"commands":null}}}}' \
        '{"targets":{"lint":{"command":7}}}' '{"targets":{"lint":{"options":{"command":null}}}}' \
        '{"targets":{"lint":{"options":{"commands":["just _sh-lint crate",3]}}}}' \
        '{"targets":{"lint":{"options":{"commands":[{"cmd":"just _sh-lint crate"}]}}}}' \
        '{"targets":{"lint":{"options":[]}}}' '{"targets":{"lint":{"options":{"commands":"just _sh-lint crate"}}}}'; do
        echo "$shape" >"$REPO/crate/project.json"

        run --separate-stderr "$REPO/scripts/shell-files.sh" .

        [ "$status" -eq 1 ]
        [[ "${stderr_lines[0]}" == "crate/project.json: "* ]]
        [ "${stderr_lines[1]}" = "ACTION: fix crate/project.json (nx show project <name> reads it the way Nx does), then re-run the recipe" ]
        [ "${stderr_lines[2]}" = "shell-files: reading crate/project.json failed (above), so whether it lints crate/x.sh is unknown" ]
        [ "${#stderr_lines[@]}" -eq 4 ]
    done
}

@test "a project.json it cannot read fails, naming it, rather than reading as no shell targets" {
    mkdir -p "$REPO/crate"
    echo '{"targets":{"lint":{"command":"just _sh-lint crate"}}}' >"$REPO/crate/project.json"
    printf 'echo x\n' >"$REPO/crate/x.sh"
    chmod 000 "$REPO/crate/project.json"

    run --separate-stderr "$REPO/scripts/shell-files.sh" .

    [ "$status" -eq 1 ]
    [ "${stderr_lines[0]}" = "shell-files: crate/project.json cannot be read, so whether it is a shell source is unknown" ]
    [ "${stderr_lines[1]}" = "ACTION: restore read permission on crate/project.json (chmod u+r), then re-run the recipe" ]
}

@test "a root that is not a project, or no root at all, is a usage error" {
    run "$REPO/scripts/shell-files.sh" nowhere
    [ "$status" -eq 2 ]
    [ "${lines[0]}" = "shell-files: 'nowhere' is not a project root (no nowhere/project.json)" ]
    [ "${lines[1]}" = "ACTION: pass . for the workspace root, or the repository-relative directory of the project's project.json (e.g. tests/support)" ]

    run "$REPO/scripts/shell-files.sh"
    [ "$status" -eq 2 ]
    [ "$output" = "usage: scripts/shell-files.sh <project-root>" ]
}

@test "a project root spelled otherwise than git lists it is a usage error, not an empty list" {
    mkdir -p "$REPO/tools"
    shell_project tools >"$REPO/tools/project.json"
    printf 'echo t\n' >"$REPO/tools/t.sh"

    for spelling in ./tools tools/../tools; do
        run "$REPO/scripts/shell-files.sh" "$spelling"
        [ "$status" -eq 2 ]
        [ "${lines[0]}" = "shell-files: '$spelling' is not spelled as git lists a project root, so it would own no file" ]
        [ "${lines[1]}" = "ACTION: pass the repository-relative directory as git lists it (e.g. scripts-e2e, not ./scripts-e2e), or . for the workspace root" ]
    done
}

@test "a repository root it cannot enter fails, naming the script" {
    use_doubles
    double dirname <<'EOF'
echo /nonexistent/scripts
EOF

    run --separate-stderr "$REPO/scripts/shell-files.sh" .

    [ "$status" -eq 1 ]
    [ "${stderr_lines[1]}" = "shell-files: cannot enter the repository root above $REPO/scripts/shell-files.sh" ]
    [ "${stderr_lines[2]}" = "ACTION: run the script from a complete checkout of the repository" ]
}

@test "a tool failing while it lists, sorts or reads names the step that failed" {
    fails grep 2 "-m 1"
    run --separate-stderr "$REPO/scripts/shell-files.sh" .
    [ "$status" -eq 1 ]
    [ "${stderr_lines[0]}" = "grep: induced failure" ]
    [ "${stderr_lines[1]}" = "shell-files: searching git's file list for quoted paths failed (above)" ]
    [ "${stderr_lines[2]}" = "ACTION: check that grep works, then re-run the recipe" ]
    rm "$DOUBLES/grep"

    fails sort 2
    run --separate-stderr "$REPO/scripts/shell-files.sh" .
    [ "$status" -eq 1 ]
    [ "${stderr_lines[1]}" = "shell-files: sorting the file list failed (above)" ]
    [ "${stderr_lines[2]}" = "ACTION: check that sort works and \$TMPDIR is writable, then re-run the recipe" ]
    rm "$DOUBLES/sort"

    fails awk 2
    run --separate-stderr "$REPO/scripts/shell-files.sh" .
    [ "$status" -eq 1 ]
    [ "${stderr_lines[1]}" = "shell-files: the project roots could not be listed from git's file list (above)" ]
    [ "${stderr_lines[2]}" = "ACTION: check that the root project.json is tracked (git ls-files project.json), then re-run the recipe" ]
    rm "$DOUBLES/awk"

    fails head 1
    run --separate-stderr "$REPO/scripts/shell-files.sh" .
    [ "$status" -eq 1 ]
    [ "${stderr_lines[0]}" = "head: induced failure" ]
    [ "${stderr_lines[1]}" = "shell-files: reading the first line of project.json failed (above), so whether it is a shell source is unknown" ]
    [ "${stderr_lines[2]}" = "ACTION: fix what the error names for project.json, then re-run the recipe" ]
}

@test "a checkout git cannot list fails, naming git and the next step" {
    rm -rf "$REPO/.git"
    cd "$BATS_TEST_TMPDIR"
    GIT_CEILING_DIRECTORIES="$BATS_TEST_TMPDIR" run "$REPO/scripts/shell-files.sh" .

    [ "$status" -eq 1 ]
    [[ "$output" == *"shell-files: \`git ls-files\` failed (above), so the shell sources cannot be listed"* ]]
    [[ "$output" == *"ACTION: repair the checkout"* ]]
}
