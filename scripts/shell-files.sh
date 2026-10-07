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
# no shell targets (its project.json never runs `_sh-lint`): that file would be
# formatted, linted and measured by nothing.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [ $# -ne 1 ]; then
    echo "usage: scripts/shell-files.sh <project-root>" >&2
    exit 2
fi
root="${1%/}"
if [ ! -f "$root/project.json" ]; then
    echo "shell-files: '$root' is not a project root (no $root/project.json)" >&2
    exit 2
fi

if ! files="$(git ls-files --cached --others --exclude-standard)"; then
    echo "shell-files: \`git ls-files\` failed (above), so the shell sources cannot be listed" >&2
    echo "ACTION: repair the checkout (\`git status\` names what is wrong), then re-run the recipe" >&2
    exit 1
fi
files="$(LC_ALL=C sort <<<"$files")"

# The project roots, deepest first, so the first match is a file's owner.
roots="$(grep -E '(^|/)project\.json$' <<<"$files" | sed -e 's|/\{0,1\}project\.json$||' -e 's|^$|.|' | awk '{ print length, $0 }' | sort -rn | cut -d' ' -f2-)"

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
    if [ ! -r "$1" ]; then
        echo "shell-files: $1 cannot be read, so whether it is a shell source is unknown" >&2
        echo "ACTION: restore read permission on $1 (chmod u+r), then re-run the recipe" >&2
        exit 1
    fi
    # read fails only at end of file here (an empty file, or one line and no
    # newline), which leaves `first` holding whatever line there was.
    local first=""
    IFS= read -r first <"$1" || true
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
    elif ! grep -q '_sh-lint' "$project/project.json"; then
        echo "shell-files: $file is a shell source of '$project', whose project.json declares no shell targets" >&2
        echo "ACTION: give $project/project.json the shell format, format-check, lint and test targets the root project.json has, or move the script" >&2
        status=1
    fi
done <<<"$files"
exit "$status"
