#!/usr/bin/env bash
# Capture the judge-posture baseline from a built `onejudge` binary.
#
# A judge whose config names no mode must run with the harness argv onejudge
# 0.15.0 gave it (claude-code's read-only `--tools Read Grep Glob WebFetch
# WebSearch`), and every judge — in any mode — is handed the one evidence
# contract. That is proven by replay: `onejudge-cli-e2e`'s `tests/cli.rs`
# (`with_no_mode_configured_the_harness_argv_is_0_15_0s_and_the_judge_prompts_carry_the_one_contract`)
# runs one config through the built binary on both seams — the linked engine in
# process, and a spawned `onejudge-fake-oneharness` in its engine mode — and
# compares every harness invocation against this baseline, and against the
# superseded `judge-posture-0.15.0/` with only the evidence contract swapped.
#
#   scripts/capture-judge-posture-baseline.sh <onejudge>
#
# Build the binary with `cargo build --features cli --bin onejudge` from the first
# release carrying the one contract, or from its tree before it is released. The
# test doubles are this tree's own (they record). Writes
# `crates/onejudge-cli-e2e/tests/golden/judge-posture-one-contract/`.
#
# Quiet on success; loud with the failing step otherwise.
set -euo pipefail
cd "$(dirname "$0")/.."

bin="${1:?usage: $0 <onejudge-binary>}"
bin="$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")"
log="$(mktemp)"
trap 'rm -f "$log"' EXIT
# The CLI journeys' own target, which builds the binary and doubles first; the
# variable makes the posture journey write the baseline instead of comparing.
if ! ONEJUDGE_COVERAGE=0 ONEJUDGE_CAPTURE_POSTURE_BASELINE="$bin" \
    ./scripts/nx run onejudge-cli-e2e:test >"$log" 2>&1; then
    cat "$log" >&2
    echo "capture-judge-posture-baseline: the capture run failed; see above" >&2
    echo "ACTION: fix the build or journey the log names, then re-run $0 with the onejudge binary" >&2
    exit 1
fi
