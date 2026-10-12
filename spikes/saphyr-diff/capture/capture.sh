#!/usr/bin/env bash
# SPIKE ONLY: regenerate fixtures/captured{,-sdk}/ — every config and SKILL.md
# front matter this repository's own suites actually parse. Applies
# capture.patch (an env-gated dump in Config::from_yaml and load_skill), runs the
# src unit tests and onejudge-cli-e2e under nextest (one process per test, so a
# capture names its test) and the Python SDK suite against the patched binary,
# then reverts the patch. Run from a clean tree.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$here/../../.."
cd "$root"
git diff --quiet -- crates/onejudge/src || { echo "crates/onejudge/src is dirty; refusing" >&2; exit 1; }
git apply "$here/capture.patch"
trap 'git checkout -- crates/onejudge/src' EXIT
cap="$here/../fixtures/captured"
sdk="$here/../fixtures/captured-sdk"
rm -rf "$cap" "$sdk" && mkdir -p "$cap" "$sdk"
cargo build -q -p onejudge-test-doubles
cargo build -q -p onejudge --features cli --bin onejudge
ONEJUDGE_SPIKE_CAPTURE="$cap" cargo nextest run -p onejudge --features cli,skill --lib >/dev/null 2>&1
ONEJUDGE_SPIKE_CAPTURE="$cap" cargo nextest run -p onejudge-cli-e2e >/dev/null 2>&1
ONEJUDGE_SPIKE_CAPTURE="$sdk" PYTHONPATH=python/onejudge-sdk/src \
  uv run --no-project --python 3.9 --with-requirements python/onejudge-sdk/requirements-dev.txt \
  python -m unittest discover -s python/onejudge-sdk/test -p 'test_*.py' 2>/dev/null
echo "config: $(ls "$cap/config" | grep -c yaml)  frontmatter: $(ls "$cap/frontmatter" | grep -c yaml)  sdk: $(ls "$sdk/config" | grep -c yaml)"
