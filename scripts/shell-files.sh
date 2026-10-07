#!/usr/bin/env bash
# The shell sources one Nx project owns, one path per line, relative to the
# repository root — what that project's shell `format`, `format-check`, `lint` and
# `test` targets read.
#
#   scripts/shell-files.sh <project-root>     # `.` for the workspace root project
#
# A shell source is a tracked or untracked-but-not-ignored file named `*.sh`,
# `*.bash` or `*.bats`, or one whose first line is a `sh`/`bash`/`bats` shebang
# (`scripts/nx` has no extension). A project owns the files under its root that no
# deeper project.json claims, so the root project owns every file no other project
# does.
#
# Fails, naming the file, when a shell source belongs to a project that declares
# no shell targets over its own root, as scripts/shell-targets.mjs reads its
# project.json: that file would be formatted, linted and measured by nothing. Fails, too, on a path git has to
# quote (a tab, newline, double quote or backslash in its name): one path per line
# cannot carry it, and dropping it would leave it unchecked.
set -euo pipefail

die() {
    echo "shell-files: $1" >&2
    echo "ACTION: $2" >&2
    exit 1
}

cd "$(dirname "${BASH_SOURCE[0]}")/.." || die "cannot enter the repository root above ${BASH_SOURCE[0]}" "run the script from a complete checkout of the repository"

if [ $# -ne 1 ]; then
    echo "usage: scripts/shell-files.sh <project-root>" >&2
    exit 2
fi
root="${1%/}"
if [ ! -f "$root/project.json" ]; then
    echo "shell-files: '$root' is not a project root (no $root/project.json)" >&2
    echo "ACTION: pass . for the workspace root, or the repository-relative directory of the project's project.json (e.g. tests/support)" >&2
    exit 2
fi

files="$(git -c core.quotePath=false ls-files --cached --others --exclude-standard)" \
    || die "\`git ls-files\` failed (above), so the shell sources cannot be listed" "repair the checkout (\`git status\` names what is wrong), then re-run the recipe"
# With quotePath off, git quotes only a name one line cannot carry.
found=0
quoted="$(grep -m 1 '^"' <<<"$files")" || found=$?
[ "$found" -le 1 ] || die "searching git's file list for quoted paths failed (above)" "check that grep works, then re-run the recipe"
[ "$found" -eq 1 ] || die "git lists $quoted quoted, as its name holds a tab, newline, double quote or backslash" "rename it without those characters, so the shell targets can read it, then re-run the recipe"
files="$(LC_ALL=C sort <<<"$files")" || die "sorting the file list failed (above)" "check that sort works and \$TMPDIR is writable, then re-run the recipe"

# The project roots, deepest first, so the first match is a file's owner.
roots="$(grep -E '(^|/)project\.json$' <<<"$files" | sed -e 's|/\{0,1\}project\.json$||' -e 's|^$|.|' | awk '{ print length, $0 }' | sort -rn | cut -d' ' -f2-)" \
    || die "the project roots could not be listed from git's file list (above)" "check that the root project.json is tracked (git ls-files project.json), then re-run the recipe"

owner() {
    local project
    while IFS= read -r project; do
        if [ "$project" = . ]; then
            echo .
            return
        fi
        case "$1" in "$project"/*)
            echo "$project"
            return
            ;;
        esac
    done <<<"$roots"
}

is_shell() {
    case "$1" in
        *.sh | *.bash | *.bats) return 0 ;;
    esac
    [ -r "$1" ] || die "$1 cannot be read, so whether it is a shell source is unknown" "restore read permission on $1 (chmod u+r), then re-run the recipe"
    # head fails only on a read error; an empty file is an empty first line. NUL
    # bytes (a binary file) are dropped so bash keeps the rest without a warning.
    local first
    first="$(head -n 1 -- "$1" | tr -d '\000')" || die "reading the first line of $1 failed (above), so whether it is a shell source is unknown" "fix what the error names for $1, then re-run the recipe"
    [[ $first =~ ^\#!.*[/[:space:]](ba)?sh([[:space:]]|$) || $first =~ ^\#!.*[/[:space:]]bats([[:space:]]|$) ]]
}

status=0
while IFS= read -r file; do
    if [ ! -f "$file" ] || ! is_shell "$file"; then
        continue
    fi
    project="$(owner "$file")"
    if [ "$project" = "$root" ]; then
        printf '%s\n' "$file"
        continue
    fi
    found=0
    node scripts/shell-targets.mjs "$project" || found=$?
    [ "$found" -le 1 ] || die "reading $project/project.json failed (above), so whether it lints $file is unknown" "fix what the error names for $project/project.json, then re-run the recipe"
    if [ "$found" -eq 1 ]; then
        echo "shell-files: $file is a shell source of '$project', whose project.json declares no shell targets" >&2
        echo "ACTION: give $project/project.json the shell format, format-check, lint and test targets the root project.json has, or move the script" >&2
        status=1
    fi
done <<<"$files"
exit "$status"
