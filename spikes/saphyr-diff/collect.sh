#!/usr/bin/env bash
# SPIKE ONLY: copy the real documents outside this repository that the harness
# reads into fixtures/, read-only at the source. Re-run to refresh; the
# committed fixtures are what the report's counts are from.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
ws="$HOME/.onevcs/workspaces"

# Effective configs oneagentgraph wrote for real runs (serde_norway-rendered).
rm -rf "$here/fixtures/effective" && mkdir -p "$here/fixtures/effective"
find "$HOME/.local/state/oneagentgraph/runs" -name onejudge.yaml 2>/dev/null | sort | while read -r f; do
  run="$(basename "$(dirname "$(dirname "$(dirname "$f")")")")"
  member="$(basename "$(dirname "$f")")"
  cp "$f" "$here/fixtures/effective/$run-$member.yaml"
done

# SKILL.md files from the user's own skills (not third-party ones).
rm -rf "$here/fixtures/skill" && mkdir -p "$here/fixtures/skill"
{
  find "$ws"/github.com-nickderobertis-skilltest-*/pool/1/worktree \
    "$ws"/github.com-nickderobertis-dero-skills-*/pool/1/worktree \
    -name SKILL.md -not -path '*/target/*' -not -path '*/node_modules/*' -not -path '*/.git/*' || true
  ls "$HOME"/.claude/skills/create-repo/SKILL.md "$HOME"/.claude/skills/auto-remote-control/SKILL.md || true
} 2>/dev/null | sort -u | while read -r f; do
  sum="$(md5sum <"$f" | cut -c1-12)"
  if [ -d "$here/fixtures/skill/$sum" ]; then continue; fi
  mkdir -p "$here/fixtures/skill/$sum"
  cp "$f" "$here/fixtures/skill/$sum/SKILL.md"
  printf '%s\n' "${f#"$HOME"/}" >"$here/fixtures/skill/$sum/origin"
done
echo "effective: $(find "$here/fixtures/effective" -type f | wc -l)  skill: $(find "$here/fixtures/skill" -name SKILL.md | wc -l)"
