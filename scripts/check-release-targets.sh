#!/usr/bin/env bash
# Drift gate for the release target triples (deterministic, no network).
#
# install.sh downloads `onejudge-<target>.<ext>` archives that
# .github/workflows/release-binaries.yml produces, so every target install.sh
# knows how to fetch MUST be built by that workflow's matrix. This check extracts
# both lists and asserts install.sh ⊆ the release matrix — so renaming or dropping
# a matrix target (or install.sh assuming an archive the release never builds)
# fails HERE, at build time, instead of at a user's `curl` 404. install.sh is
# deliberately a subset (no Windows: it points Windows users at the .zip), so this
# is a subset check, not equality.
#
# Portable to bash 3.2 (macOS): no `mapfile`, no bash-4 features.
set -euo pipefail
cd "$(dirname "$0")/.."

# install.sh's literal `target="<triple>"` assignments.
install_targets="$(grep -oE 'target="[a-z0-9_.-]+"' install.sh | sed -E 's/target="([^"]+)"/\1/' | sort -u)"
# release-binaries.yml's matrix `target: <triple>` entries.
matrix_targets="$(grep -oE '^[[:space:]]+target: [a-z0-9_.-]+' .github/workflows/release-binaries.yml | sed -E 's/^[[:space:]]+target: //' | sort -u)"

if [ -z "$install_targets" ] || [ -z "$matrix_targets" ]; then
    echo "check-release-targets: could not extract target lists — did install.sh / release-binaries.yml change format?" >&2
    exit 1
fi

missing=""
for t in $install_targets; do
    printf '%s\n' "$matrix_targets" | grep -qxF "$t" || missing="$missing $t"
done

if [ -n "$missing" ]; then
    {
        echo "check-release-targets: install.sh downloads target(s) the release matrix does not build:$missing"
        echo "  install.sh:           " $install_targets
        echo "  release-binaries.yml: " $matrix_targets
        echo "  Fix install.sh's os/arch map or the workflow matrix so they agree."
    } >&2
    exit 1
fi

count="$(printf '%s\n' "$install_targets" | grep -c .)"
echo "check-release-targets: $count install target(s) all built by the release matrix"
