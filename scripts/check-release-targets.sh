#!/usr/bin/env bash
# Drift gate for the release target triples (deterministic, no network).
#
# install.sh downloads `onejudge-<target>.<ext>` archives that
# .github/workflows/release-binaries.yml produces, so every target install.sh
# knows how to fetch MUST be built by that workflow's archive producers. This check extracts
# both lists and asserts install.sh ⊆ the release producers — so renaming or dropping
# a producer target (or install.sh assuming an archive the release never builds)
# fails HERE, at build time, instead of at a user's `curl` 404. install.sh is
# deliberately a subset (no Windows: it points Windows users at the .zip), so this
# is a subset check, not equality.
#
# Portable to bash 3.2 (macOS): no `mapfile`, no bash-4 features.
set -euo pipefail
cd "$(dirname "$0")/.."

# The matches of the ERE $1 in the file $2, one per line. No match is an empty
# list, for the check below to name; a file grep cannot read fails the pipeline
# it feeds, which ends the script with the reason.
matches() {
    local status=0
    grep -oE "$1" "$2" || status=$?
    if [ "$status" -gt 1 ]; then
        echo "check-release-targets: reading $2 failed (above), so its targets are unknown" >&2
        echo "  Restore $2 (git checkout -- $2), then re-run." >&2
        return 1
    fi
}

# install.sh's literal `target="<triple>"` assignments.
install_targets="$(matches 'target="[a-z0-9_.-]+"' install.sh | sed -E 's/target="([^"]+)"/\1/' | sort -u)"
# Only declarations in the two builders count: publisher expectations and
# command text cannot replace a removed matrix target or Darwin TARGETS value.
producer_declarations() {
    local workflow=.github/workflows/release-binaries.yml status=0
    awk '
        /^  (archives|darwin):[[:space:]]*$/ { producer = 1; next }
        /^  [A-Za-z_-]+:/ { producer = 0 }
        producer && /^[[:space:]]*-?[[:space:]]*(target|TARGETS):/ { print }
    ' "$workflow" || status=$?
    if [ "$status" -ne 0 ]; then
        echo "check-release-targets: reading $workflow failed (above), so its targets are unknown" >&2
        echo "  Restore $workflow (git checkout -- $workflow), then re-run." >&2
        return 1
    fi
}
producer_targets="$(producer_declarations | { grep -oE '[a-z0-9_]+-(apple-darwin|unknown-linux-gnu|pc-windows-msvc)' || [ "$?" -eq 1 ]; } | sort -u)"

if [ -z "$install_targets" ]; then
    echo "check-release-targets: install.sh has no literal target=\"<triple>\" assignment to check" >&2
    echo "  Restore install.sh's os/arch map of target=\"<triple>\" lines, or update the pattern in this script to its new form." >&2
    exit 1
fi
if [ -z "$producer_targets" ]; then
    echo "check-release-targets: .github/workflows/release-binaries.yml has no producer target declaration to check against" >&2
    echo "  Restore the workflow's producer target declarations, or update the pattern in this script to its new form." >&2
    exit 1
fi

missing=""
for t in $install_targets; do
    printf '%s\n' "$producer_targets" | grep -qxF "$t" || missing="$missing $t"
done

if [ -n "$missing" ]; then
    {
        echo "check-release-targets: install.sh downloads target(s) the release producers do not build:$missing"
        echo "  install.sh:            ${install_targets//$'\n'/ }"
        echo "  release-binaries.yml:  ${producer_targets//$'\n'/ }"
        echo "  Fix install.sh's os/arch map or the workflow producer declarations so they agree."
    } >&2
    exit 1
fi

count="$(printf '%s\n' "$install_targets" | grep -c .)"
echo "check-release-targets: $count install target(s) all built by the release producers"
