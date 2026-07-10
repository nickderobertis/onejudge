#!/usr/bin/env bash
# Run llmlint over only the lines this branch changed since it forked from main.
#
# Lints the diff at the branch's *fork point* (the merge-base with main), not a
# raw diff against current main's tip — so unrelated commits that landed on main
# after the branch started are never linted, and a stale branch still checks
# exactly what it introduced. This is the `origin/main...HEAD` three-dot set.
#
# Scoped twice over: to the changed *files* (the args llmlint gets) and, via
# `--diff --diff-base`, to the changed *lines* within them (the judge reviews the
# fork-point diff, not the whole file) — so a PR is judged on what it introduced.
#
# This is what the blocking `llmlint` CI check runs (diff-scoped, so a PR pays for
# its own changes, not a full-repo sweep). Run it locally before pushing.
#
# Usage:
#   scripts/lint-llm-diff.sh [BASE_REF] [-- <extra llmlint args>]
#   BASE_REF defaults to origin/main (falls back to main). In CI, fetch main
#   first so the ref resolves.
#
# Exit codes are llmlint's own (0 clean, 1 violations, 2 config/harness error);
# a diff with no lintable files is a clean 0.
# llmlint: ignore[robust_shell] `set -e` deliberately omitted — the script controls its own exit codes (2 on resolve errors, 0 on no-diff) and must not abort early
set -uo pipefail

base_ref="origin/main"
extra=()
while [ $# -gt 0 ]; do
  case "$1" in
    --) shift; extra=("$@"); break ;;
    *) base_ref="$1"; shift ;;
  esac
done

# Resolve a usable base: prefer the given ref, fall back to local `main`.
if ! git rev-parse --verify --quiet "$base_ref" >/dev/null; then
  if git rev-parse --verify --quiet main >/dev/null; then
    echo "lint-llm-diff: '$base_ref' not found; falling back to 'main'" >&2
    base_ref="main"
  else
    echo "lint-llm-diff: cannot resolve base ref '$base_ref' (and no 'main')" >&2
    exit 2
  fi
fi

merge_base="$(git merge-base "$base_ref" HEAD)" || {
  echo "lint-llm-diff: no merge-base between '$base_ref' and HEAD" >&2
  exit 2
}

# Changed, still-present files (drop deletions: linting a deleted path errors).
mapfile -t files < <(git diff --name-only --diff-filter=ACMR "$merge_base" HEAD)
present=()
for f in "${files[@]}"; do [ -f "$f" ] && present+=("$f"); done

if [ "${#present[@]}" -eq 0 ]; then
  echo "lint-llm-diff: no changed files to lint (base ${base_ref} @ ${merge_base:0:9})" >&2
  exit 0
fi

echo "lint-llm-diff: linting ${#present[@]} changed file(s) vs ${base_ref} @ ${merge_base:0:9}" >&2
# `--diff --diff-base "$merge_base"` puts each file's fork-point diff in the judge
# prompt so it reviews only the *changed lines*, not the whole file — the judge
# stops flagging pre-existing code a PR merely sits near, and can't wander into
# lines the branch never touched.
exec llmlint --diff --diff-base "$merge_base" "${extra[@]}" "${present[@]}"
