#!/usr/bin/env bash
# Print the Cargo target directory this checkout builds into — `.cargo/config.toml`
# names `target`, and `CARGO_TARGET_DIR` overrides it — as Cargo itself resolves it.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if ! metadata="$(cargo metadata --format-version 1 --no-deps)"; then
    echo "cargo-target-dir: \`cargo metadata\` failed (above)" >&2
    echo "ACTION: fix the Cargo.toml it names, then re-run the recipe" >&2
    exit 1
fi
# shellcheck disable=SC2016 # JavaScript, single-quoted so the shell expands none of its `${...}`.
node -e '
let dir;
try {
  dir = JSON.parse(require("fs").readFileSync(0, "utf8")).target_directory;
} catch (error) {
  console.error(`cargo-target-dir: cargo metadata printed no JSON: ${error.message}`);
  console.error("ACTION: check `cargo metadata --no-deps` with this toolchain (rust-toolchain.toml)");
  process.exit(1);
}
if (typeof dir !== "string" || dir === "") {
  console.error("cargo-target-dir: cargo metadata named no target_directory");
  console.error("ACTION: check `cargo metadata --no-deps` with this toolchain (rust-toolchain.toml)");
  process.exit(1);
}
process.stdout.write(dir + "\n");
' <<<"$metadata"
