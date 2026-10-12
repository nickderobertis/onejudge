#!/usr/bin/env bash
# SPIKE ONLY: run the differential harness and print every difference.
# Third-party SKILL.md files on this machine are read in place (never copied in);
# set SPIKE_EXTRA_SKILL_DIRS= (empty) to run on the committed fixtures alone.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
: "${SPIKE_EXTRA_SKILL_DIRS=$HOME/.claude/skills/synced:$HOME/.claude/plugins}"
export SPIKE_EXTRA_SKILL_DIRS
exec cargo run -q --release --manifest-path "$here/Cargo.toml"
