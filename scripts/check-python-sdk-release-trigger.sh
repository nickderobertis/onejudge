#!/usr/bin/env bash
# Deterministic proof that release-worthy SDK-only history becomes a crate
# change, while unrelated and already-covered crate history remains untouched.
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git -C "$work" init -q
git -C "$work" config user.name test
git -C "$work" config user.email test@example.invalid
mkdir -p "$work/python/onejudge-sdk" "$work/crates/onejudge"
touch "$work/python/onejudge-sdk/.keep" "$work/crates/onejudge/.keep"
git -C "$work" add .
git -C "$work" commit -qm 'chore: initial'
base=$(git -C "$work" rev-parse HEAD)

printf 'fixed\n' >"$work/python/onejudge-sdk/change"
git -C "$work" add .
git -C "$work" commit -qm 'fix(sdk): repair packaging'
head=$(git -C "$work" rev-parse HEAD)
result=$(cd "$work" && "$repo_root/scripts/python-sdk-release-trigger.sh" "$base" "$head")
[[ "$result" == fix ]] || { echo "python SDK fix produced '$result', expected fix" >&2; exit 1; }

printf 'crate\n' >"$work/crates/onejudge/change"
git -C "$work" add .
git -C "$work" commit -qm 'fix: change crate too'
both=$(git -C "$work" rev-parse HEAD)
result=$(cd "$work" && "$repo_root/scripts/python-sdk-release-trigger.sh" "$base" "$both")
[[ -z "$result" ]] || { echo "crate change redundantly produced '$result'" >&2; exit 1; }

git -C "$work" reset -q --hard "$head"
printf 'docs\n' >"$work/python/onejudge-sdk/docs"
git -C "$work" add .
git -C "$work" commit -qm 'docs(sdk): clarify usage'
docs=$(git -C "$work" rev-parse HEAD)
result=$(cd "$work" && "$repo_root/scripts/python-sdk-release-trigger.sh" "$head" "$docs")
[[ -z "$result" ]] || { echo "SDK docs produced release prefix '$result'" >&2; exit 1; }

git -C "$work" reset -q --hard "$head"
printf 'second fix\n' >"$work/python/onejudge-sdk/fix"
git -C "$work" add .
git -C "$work" commit -qm 'fix(sdk): prepare feature'
printf 'feature\n' >"$work/python/onejudge-sdk/feature"
git -C "$work" add .
git -C "$work" commit -qm 'feat(sdk): add capability'
feature=$(git -C "$work" rev-parse HEAD)
result=$(cd "$work" && "$repo_root/scripts/python-sdk-release-trigger.sh" "$head" "$feature")
[[ "$result" == feat ]] || { echo "SDK feature range produced '$result', expected feat" >&2; exit 1; }

printf 'breaking\n' >"$work/python/onejudge-sdk/breaking"
git -C "$work" add .
git -C "$work" commit -qm 'feat(sdk)!: replace interface'
breaking=$(git -C "$work" rev-parse HEAD)
result=$(cd "$work" && "$repo_root/scripts/python-sdk-release-trigger.sh" "$feature" "$breaking")
[[ "$result" == 'feat!' ]] || { echo "breaking SDK change produced '$result', expected feat!" >&2; exit 1; }

echo "check-python-sdk-release-trigger: ok"
