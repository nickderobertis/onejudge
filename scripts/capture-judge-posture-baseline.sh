#!/usr/bin/env bash
# Capture the judge-posture baseline from a RELEASED `onejudge` binary.
#
# A judge whose config names no mode must run exactly as onejudge 0.15.0 ran it:
# the same harness argv (claude-code's read-only `--tools Read Grep Glob WebFetch
# WebSearch`) and the same judge prompts, byte for byte. That is proven by replay:
# `tests/cli.rs` (`with_no_mode_configured_the_harness_argv_and_judge_prompts_are_the_0_15_0_ones`)
# runs one config through the built binary on both seams — the linked engine in
# process, and a spawned `onejudge-fake-oneharness` in its engine mode — and
# compares every harness invocation against what 0.15.0 produced for it. This
# script is how that baseline was recorded, so the comparison is against the
# release's own run rather than a hand-written expectation.
#
#   scripts/capture-judge-posture-baseline.sh <onejudge-0.15.0>
#
# Build the binary from the `v0.15.0` tree with `cargo build --features cli --bin
# onejudge`. The test doubles are this tree's own (they record; the release's
# did not). Writes `crates/onejudge/tests/golden/judge-posture-0.15.0/`.
#
# Quiet on success; loud with the failing step otherwise.
set -euo pipefail
cd "$(dirname "$0")/.."

bin="${1:?usage: $0 <onejudge-0.15.0-binary>}"
bin="$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")"
log="$(mktemp)"
trap 'rm -f "$log"' EXIT
if ! ONEJUDGE_CAPTURE_POSTURE_BASELINE="$bin" cargo nextest run --features fake-provider,cli,sdk-schema \
    --test cli -E 'test(with_no_mode_configured)' >"$log" 2>&1; then
    cat "$log" >&2
    echo "capture-judge-posture-baseline: the capture run failed; see above" >&2
    exit 1
fi
