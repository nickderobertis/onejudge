# Shared helpers for the workspace's bats suite (`just _sh-test . workspace`),
# loaded by every test file with `load helpers`.
#
# Each script is run the way its callers run it — as an executable, by path —
# and only the tools it reaches past this repository for (the network, a package
# manager, cargo, llmlint) are replaced, by executable doubles on PATH. A script
# that resolves the repository from its own path is run through a symlink in a
# scratch tree, so it acts on that tree while bashcov still credits the real file.

ROOT="$(cd "$BATS_TEST_DIRNAME/.." && pwd)"
# Only tests read it, and bats loads this file before any test exists too.
DOUBLES="${BATS_TEST_TMPDIR-}/bin"

# Put the doubles directory first on PATH.
use_doubles() {
    mkdir -p "$DOUBLES"
    PATH="$DOUBLES:$PATH"
}

# PATH holds the doubles and the system directories only, so no tool installed
# for a user (uv, llmlint, bun, pixi's environment) can answer in a double's place.
isolate_path() {
    mkdir -p "$DOUBLES"
    PATH="$DOUBLES:/usr/bin:/bin"
}

# only_tools TOOL... — ONLY_PATH: the doubles and the named tools (plus bash and
# env, which every script's shebang needs) and nothing else, for running a script
# whose behaviour turns on which tools are absent (`run env PATH="$ONLY_PATH" ...`).
only_tools() {
    local tools="$BATS_TEST_TMPDIR/tools" tool
    mkdir -p "$DOUBLES" "$tools"
    for tool in bash env "$@"; do
        ln -sf "$(command -v "$tool")" "$tools/$tool"
    done
    # shellcheck disable=SC2034 # read by the test files that load these helpers.
    ONLY_PATH="$DOUBLES:$tools"
}

# double NAME — an executable named NAME on the doubles PATH, whose body is read
# from stdin and runs under bash.
double() {
    mkdir -p "$DOUBLES"
    {
        echo '#!/usr/bin/env bash'
        cat
    } >"$DOUBLES/$1"
    chmod +x "$DOUBLES/$1"
}

# link DIR PATH... — each repository PATH, symlinked to the same place under DIR.
link() {
    local dir="$1" path
    shift
    for path in "$@"; do
        mkdir -p "$dir/$(dirname "$path")"
        ln -s "$ROOT/$path" "$dir/$path"
    done
}

# git_repo DIR — an empty repository on `main` with an identity to commit as.
git_repo() {
    mkdir -p "$1"
    git -C "$1" init -q -b main
    git -C "$1" config user.name bats
    git -C "$1" config user.email bats@example.invalid
    git -C "$1" config commit.gpgsign false
}

# commit DIR MESSAGE — commit everything in DIR.
commit() {
    git -C "$1" add -A
    git -C "$1" commit -q --allow-empty -m "$2"
}
