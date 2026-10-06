#!/usr/bin/env bash
# Print the Cargo target directory this checkout builds into — `.cargo/config.toml`
# names `target`, and `CARGO_TARGET_DIR` overrides it — as Cargo itself resolves it.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
metadata="$(cargo metadata --format-version 1 --no-deps --offline 2>/dev/null || cargo metadata --format-version 1 --no-deps)"
dir="$(printf '%s' "$metadata" | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
if [ -z "$dir" ]; then
    echo "cargo-target-dir: cargo metadata named no target_directory" >&2
    exit 1
fi
printf '%s\n' "$dir"
