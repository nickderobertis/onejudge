#!/usr/bin/env bash
# Bridge release-worthy Python SDK commits into release-plz's crate-scoped
# change detection. Prints the Conventional Commit prefix for the synthetic
# crate commit, or nothing when the range already changes the crate or does not
# contain a release-worthy SDK commit.
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 <base-revision> <head-revision>" >&2
    exit 2
fi

base=$1
head=$2

git rev-parse --verify "${base}^{commit}" >/dev/null
git rev-parse --verify "${head}^{commit}" >/dev/null

changed=$(git diff --name-only "$base" "$head")
if ! grep -q '^python/onejudge-sdk/' <<<"$changed" || grep -q '^crates/onejudge/' <<<"$changed"; then
    exit 0
fi

prefix=
while IFS= read -r commit; do
    paths=$(git diff-tree --no-commit-id --name-only -r "$commit")
    grep -q '^python/onejudge-sdk/' <<<"$paths" || continue

    message=$(git show -s --format='%B' "$commit")
    if grep -Eq '^[a-z]+(\([^)]+\))?!:|^BREAKING CHANGE:' <<<"$message"; then
        prefix='feat!'
        break
    fi
    subject=${message%%$'\n'*}
    case "$subject" in
        feat:*|feat\(*\):*) prefix=feat ;;
        fix:*|fix\(*\):*|perf:*|perf\(*\):*|refactor:*|refactor\(*\):*|build:*|build\(*\):*)
            [[ -n "$prefix" ]] || prefix=fix
            ;;
    esac
done < <(git rev-list --reverse "${base}..${head}")

if [[ -n "$prefix" ]]; then
    printf '%s\n' "$prefix"
fi
