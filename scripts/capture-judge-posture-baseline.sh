#!/usr/bin/env bash
# Capture the judge-posture baseline the posture replay in `onejudge-cli-e2e`'s
# `tests/cli.rs` holds every judge-side harness invocation to; the replay and
# `tests/golden/README.md` say what it proves.
#
#   scripts/capture-judge-posture-baseline.sh <onejudge>
#
# Build the binary with `just build-cli` (`target/release/onejudge`) in a checkout
# of the first release carrying the one contract, or of its tree before then. The
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
