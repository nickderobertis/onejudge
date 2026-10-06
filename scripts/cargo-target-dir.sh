#!/usr/bin/env bash
# Print the Cargo target directory this checkout builds into — `.cargo/config.toml`
# names `target`, and `CARGO_TARGET_DIR` overrides it — as Cargo itself resolves it.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
cargo metadata --format-version 1 --no-deps |
    node -e 'process.stdout.write(JSON.parse(require("fs").readFileSync(0, "utf8")).target_directory + "\n")'
