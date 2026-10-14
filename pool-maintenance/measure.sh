#!/usr/bin/env bash
# The measurement behind `onejudge-pool-maintenance-seconds` (budgets.yaml beside
# this file): how long one steady-state pass of the declared pool maintenance
# (`just pool-maintenance`, onevcs.toml) takes, after one ordinary run's worth of
# new superseded builds rather than over weeks of accumulated history.
#
#   ./measure.sh      # run by `onebudgetspec check`, from this directory
#
# It establishes that state first, then times one pass:
#   1. the gate's test targets are built as the gate builds them (plain, no
#      coverage), so the ordinary build is current before the backlog is cleared;
#   2. the declared maintenance runs once, untimed, clearing any backlog;
#   3. the same targets are built with `onejudge/skill` alone in place of the
#      gate's feature set, which gives this repository's crates new metadata
#      hashes, and exactly the entries that build created are backdated past the
#      2-day window (and before every entry already there, so the ordinary build
#      stays each unit's newest): one run's worth of superseded builds;
#   4. the ordinary build runs again, so each unit's newest build is the ordinary one;
#   5. the declared maintenance runs once more, timed: the pruner reports its own
#      wall-clock seconds and breakdown through ONEBUDGETSPEC_RESULT, the only figure.
#
# The untimed pass's seconds are appended to that report's detail as separate
# evidence, never to its value. It then refuses a run whose timed pass removed an
# entry the alternative build did not create, or removed none of them, because
# that pass measured something other than this budget's workload. The entry lists
# it compares are left in <target>/pool-maintenance-measure/.
#
# It really prunes this checkout and builds it twice (GNU find/touch). Quiet on
# success but for one summary line on stderr; a failed step prints its log.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# The gate's exported RUSTFLAGS and feature set, read from the justfile, so these
# builds produce the hashes `just test-fast` does.
RUSTFLAGS="$(just --evaluate RUSTFLAGS)"
export RUSTFLAGS
gate_features="$(just --evaluate gate_features)"
alternative_features="onejudge/skill"

target="$(./scripts/cargo-target-dir.sh)"
profile="$target/debug"
evidence="$target/pool-maintenance-measure"
rm -rf "$evidence"
mkdir -p "$evidence"

# Run a step with its output in the evidence directory; on failure print it.
step() {
    local name="$1"
    shift
    if ! "$@" >"$evidence/$name.log" 2>&1; then
        cat "$evidence/$name.log" >&2
        echo "measure: step '$name' failed (output above, kept in $evidence/$name.log)" >&2
        echo "ACTION: fix what it names, then re-run ./measure.sh" >&2
        exit 1
    fi
}

# The gate's test targets (each Rust project's `test` and the doubles' `build`),
# one package per cargo invocation as the gate runs them, under `features`.
# Every command returns on failure: `step` runs this under `if !`, where `set -e`
# does not apply.
build() {
    local features="$1" crate
    for crate in onejudge onejudge-e2e onejudge-cli-e2e; do
        cargo nextest run --no-run --locked -p "$crate" --features "$features" || return
    done
    cargo build --locked -p onejudge-test-doubles --bins --features "$features" || return
    cargo nextest run --no-run --locked -p onejudge-repo
}

# Every build entry of the profile the builds write, one path per line.
entries() {
    local dir
    for dir in deps .fingerprint build incremental; do
        [ ! -d "$profile/$dir" ] || find "$profile/$dir" -mindepth 1 -maxdepth 1
    done | LC_ALL=C sort
}

seconds_since() {
    awk -v start="$1" -v end="$EPOCHREALTIME" 'BEGIN { printf "%.3f", end - start }'
}

step ordinary-build build "$gate_features"

started="$EPOCHREALTIME"
step backlog-pass env -u ONEBUDGETSPEC_RESULT just pool-maintenance
backlog_seconds="$(seconds_since "$started")"

entries >"$evidence/before-alternative.txt"
# Before every entry already there, and past the 2-day window either way.
oldest="$(find "$profile/deps" "$profile/.fingerprint" "$profile/build" "$profile/incremental" \
    -printf '%T@\n' 2>/dev/null | awk 'NR == 1 || $1 < min { min = $1 } END { print min }')"
stamp=$(($(date +%s) - 3 * 86400))
if [ -n "$oldest" ] && [ $((${oldest%.*} - 86400)) -lt "$stamp" ]; then
    stamp=$((${oldest%.*} - 86400))
fi

step alternative-build build "$alternative_features"
entries >"$evidence/after-alternative.txt"
LC_ALL=C comm -13 "$evidence/before-alternative.txt" "$evidence/after-alternative.txt" >"$evidence/alternative.txt"
if [ ! -s "$evidence/alternative.txt" ]; then
    echo "measure: the build with $alternative_features created no entry in $profile, so there is nothing superseded to time" >&2
    echo "ACTION: check $evidence/alternative-build.log; that feature set must give this repository's crates new metadata hashes" >&2
    exit 1
fi
while IFS= read -r entry; do
    find "$entry" -depth -exec touch -h -d "@$stamp" {} +
done <"$evidence/alternative.txt"

step ordinary-rebuild build "$gate_features"
entries >"$evidence/before-timed.txt"

step timed-pass just pool-maintenance
entries >"$evidence/after-timed.txt"

LC_ALL=C comm -23 "$evidence/before-timed.txt" "$evidence/after-timed.txt" >"$evidence/removed.txt"
LC_ALL=C comm -23 "$evidence/removed.txt" "$evidence/alternative.txt" >"$evidence/removed-ordinary.txt"
if [ -s "$evidence/removed-ordinary.txt" ]; then
    cat "$evidence/removed-ordinary.txt" >&2
    echo "measure: the timed pass removed the entries above, which the alternative build did not create, so it measured more than one run's worth of superseded builds" >&2
    echo "ACTION: re-run ./measure.sh, whose backlog pass clears them first; if they recur, read $evidence" >&2
    exit 1
fi
if [ ! -s "$evidence/removed.txt" ]; then
    echo "measure: the timed pass removed none of the $(wc -l <"$evidence/alternative.txt") entries the alternative build created" >&2
    echo "ACTION: read $evidence/timed-pass.log and $evidence/alternative.txt; they must be superseded and older than 2 days" >&2
    exit 1
fi

if [ -n "${ONEBUDGETSPEC_RESULT:-}" ]; then
    # shellcheck disable=SC2016 # JavaScript, single-quoted so the shell expands none of it.
    node -e '
const fs = require("fs");
const [file, seconds] = process.argv.slice(1);
const result = JSON.parse(fs.readFileSync(file, "utf8"));
result.detail = `${result.detail ?? ""}; untimed backlog pass seconds (not in the value): ${seconds}`;
fs.writeFileSync(file, JSON.stringify(result));
' "$ONEBUDGETSPEC_RESULT" "$backlog_seconds"
fi
echo "measure: backlog pass ${backlog_seconds}s; alternative build created $(wc -l <"$evidence/alternative.txt") entries, the timed pass removed $(wc -l <"$evidence/removed.txt") of them and no other ($evidence)" >&2
