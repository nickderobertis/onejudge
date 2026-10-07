#!/usr/bin/env bash
# Capture a history store as a RELEASED, pre-cutover `oneharness` left it.
#
# `oneharness-core` 0.24.0 indexes history in dated segments under `.index.d/`; a
# run recorded before that has only its session file and the legacy
# `.index.jsonl`. onejudge must still read such a run's record back (its
# `history_id`), and `onejudge-cli-e2e`'s `tests/cli.rs` proves it against the store this script
# writes, so the comparison is against bytes a release wrote rather than a
# hand-written imitation of them.
#
#   scripts/capture-pre-cutover-history.sh <oneharness-0.20.0> <onejudge-fake-harness>
#
# `oneharness` 0.20.0 links `oneharness-core` 0.22.0, the last core before the
# cutover (`cargo install oneharness --version 0.20.0 --locked --root <dir>`).
# The fake harness is this tree's double (`./scripts/nx run
# onejudge-test-doubles:build` puts it in `target/debug/`), reached as `claude-code` through ordinary
# oneharness config, so the record names the harness onejudge's fake oneharness
# reports. The project lives at a fixed path so the store's project slug does
# not name the capturing host. The store is written to
# `crates/onejudge-cli-e2e/tests/golden/pre-cutover-history/store/`, replacing it.
#
# Quiet on success; loud with the failing step otherwise.
set -euo pipefail
cd "$(dirname "$0")/.."

usage="usage: $0 <oneharness-0.20.0-bin> <onejudge-fake-harness-bin>"
oneharness_bin="${1:?$usage}"
harness_bin="${2:?$usage}"
case "$("$oneharness_bin" --version)" in
    "oneharness 0.20.0") ;;
    *) echo "capture-pre-cutover-history: $oneharness_bin is not oneharness 0.20.0" >&2; exit 1 ;;
esac

project=/tmp/onejudge-pre-cutover-project
work="$(mktemp -d)"
trap 'rm -rf "$work" "$project"' EXIT
rm -rf "$project"
mkdir -p "$project" "$work/xdg"
printf 'harnesses = ["claude-code"]\n\n[harness.claude-code]\nbin = "%s"\n' "$harness_bin" \
    > "$project/oneharness.toml"

# Hermetic: no host config, labels or pointer file reach the record.
if ! (cd "$project" && env -u ONEHARNESS_CONFIG -u ONEHARNESS_HISTORY_LABELS \
        -u ONEHARNESS_HISTORY_POINTER_FILE XDG_CONFIG_HOME="$work/xdg" \
        ONEHARNESS_HISTORY_DIR="$work/store" \
        "$oneharness_bin" run --history --format json --prompt 'recorded before the cutover' \
        > "$work/report.json" 2> "$work/stderr"); then
    echo "capture-pre-cutover-history: the oneharness run failed:" >&2
    cat "$work/stderr" >&2
    exit 1
fi
if [ -e "$work/store/.index.d" ]; then
    echo "capture-pre-cutover-history: $oneharness_bin wrote .index.d/ — it is not a pre-cutover release" >&2
    exit 1
fi

dest=crates/onejudge-cli-e2e/tests/golden/pre-cutover-history/store
rm -rf "$dest"
mkdir -p "$(dirname "$dest")"
cp -R "$work/store" "$dest"
