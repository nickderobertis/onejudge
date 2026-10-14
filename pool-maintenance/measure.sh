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
#      hashes. The alternative workload is every unit that build reports (cargo's
#      own artifact messages, which name a cached unit as well as a compiled one)
#      whose hash the ordinary build does not, so an alternative build an earlier
#      run left behind is counted in full. Its own crates' units are then
#      recompiled (their fingerprints dropped), so the incremental directories
#      they write join the workload too. Every entry of that workload is
#      backdated past the 2-day window and before every other entry, so the
#      ordinary build stays each unit's newest: one run's worth of superseded builds;
#   4. the ordinary build runs again, so each unit's newest build is the ordinary one;
#   5. the declared maintenance runs once more, timed: the pruner reports its own
#      wall-clock seconds and breakdown through ONEBUDGETSPEC_RESULT, the only figure.
#
# The untimed pass's seconds and the workload's size are appended to that
# report's detail as separate evidence, never to its value. It then refuses a run
# whose timed pass removed an entry outside the workload, kept an entry of the
# workload's own crates, or removed nothing, because that pass measured something
# other than this budget's workload. The metadata-hash inventories it compares are
# left in <target>/pool-maintenance-measure/.
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
# one package per cargo invocation as the gate runs them, under `features`,
# appending cargo's JSON artifact messages to `messages`. `cargo test --no-run`
# is the build `cargo nextest run` makes, so the units and hashes are the gate's.
# Every command returns on failure: `step` runs this under `if !`, where `set -e`
# does not apply.
build() {
    local features="$1" messages="$2" crate
    for crate in onejudge onejudge-e2e onejudge-cli-e2e; do
        cargo test --no-run --locked -p "$crate" --features "$features" \
            --message-format json >>"$messages" || return
    done
    cargo build --locked -p onejudge-test-doubles --bins --features "$features" \
        --message-format json >>"$messages" || return
    cargo test --no-run --locked -p onejudge-repo --message-format json >>"$messages"
}

# The metadata hashes of every unit a build's messages name, cached or compiled,
# one `<hash> <own|dependency>` line each: `own` for this repository's packages.
hashes() {
    # shellcheck disable=SC2016 # JavaScript, single-quoted so the shell expands none of it.
    node -e '
const lines = require("fs").readFileSync(process.argv[1], "utf8").split("\n");
const found = new Map();
for (const line of lines) {
  if (!line.startsWith("{")) continue;
  const message = JSON.parse(line);
  const paths =
    message.reason === "compiler-artifact" ? message.filenames
    : message.reason === "build-script-executed" ? [message.out_dir]
    : [];
  const own = paths.length > 0 && message.package_id.startsWith("path+file:");
  for (const path of paths) {
    const within = path.slice(path.lastIndexOf("/debug/"));
    for (const [, hash] of within.matchAll(/-([0-9a-f]{16})(?=[./\\]|$)/g)) {
      if (own || !found.has(hash)) found.set(hash, own ? "own" : "dependency");
    }
  }
}
for (const [hash, kind] of found) console.log(`${hash} ${kind}`);
' "$1" | LC_ALL=C sort
}

# The entries of `entries` whose metadata hash is one of `hashes`' first column.
with_hashes() {
    awk 'NR == FNR { wanted[$1] = 1; next }
        { name = $0; sub(/.*\//, "", name) }
        match(name, /-[0-9a-f]{16}(\.|$)/) && (substr(name, RSTART + 1, 16) in wanted)' "$1" "$2"
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

step ordinary-build build "$gate_features" "$evidence/ordinary.jsonl"
hashes "$evidence/ordinary.jsonl" >"$evidence/ordinary-hashes.txt"

started="$EPOCHREALTIME"
step backlog-pass env -u ONEBUDGETSPEC_RESULT just pool-maintenance
backlog_seconds="$(seconds_since "$started")"

step alternative-build build "$alternative_features" "$evidence/alternative.jsonl"
hashes "$evidence/alternative.jsonl" >"$evidence/alternative-hashes.txt"
# The workload: the alternative build's units the ordinary build does not share.
LC_ALL=C join -v 1 "$evidence/alternative-hashes.txt" "$evidence/ordinary-hashes.txt" >"$evidence/workload-hashes.txt"
grep ' own$' "$evidence/workload-hashes.txt" >"$evidence/workload-own-hashes.txt" || true
if [ ! -s "$evidence/workload-own-hashes.txt" ]; then
    echo "measure: the build with $alternative_features shares every one of this repository's units with the ordinary build, so nothing of its own is superseded to time" >&2
    echo "ACTION: check $evidence/alternative-build.log; that feature set must give this repository's crates new metadata hashes" >&2
    exit 1
fi

# Recompile the workload's own units, cached or not, so the incremental
# directories they write are known: the only ones written from here on.
entries | with_hashes "$evidence/workload-own-hashes.txt" - | grep '/\.fingerprint/' >"$evidence/workload-own-fingerprints.txt" || true
while IFS= read -r fingerprint; do
    rm -rf "$fingerprint"
done <"$evidence/workload-own-fingerprints.txt"
touch "$evidence/recompile-started"
step alternative-recompile build "$alternative_features" "$evidence/alternative-recompile.jsonl"
if [ ! -d "$profile/incremental" ]; then
    : >"$evidence/workload-incremental.txt"
else
    find "$profile/incremental" -mindepth 1 -maxdepth 1 -type d -exec sh -c \
        'find "$1" -newer "$2" -print -quit | grep -q .' _ {} "$evidence/recompile-started" \; -print \
        | LC_ALL=C sort >"$evidence/workload-incremental.txt"
fi

entries >"$evidence/before-backdate.txt"
{
    with_hashes "$evidence/workload-hashes.txt" "$evidence/before-backdate.txt"
    cat "$evidence/workload-incremental.txt"
} | LC_ALL=C sort -u >"$evidence/workload.txt"
{
    with_hashes "$evidence/workload-own-hashes.txt" "$evidence/before-backdate.txt"
    cat "$evidence/workload-incremental.txt"
} | LC_ALL=C sort -u >"$evidence/workload-own.txt"

# Before every entry outside the workload, and past the 2-day window either way.
oldest="$(LC_ALL=C comm -23 "$evidence/before-backdate.txt" "$evidence/workload.txt" \
    | xargs -r -d '\n' sh -c 'find "$@" -printf "%T@\n"' _ \
    | awk 'NR == 1 || $1 < min { min = $1 } END { print min }')"
stamp=$(($(date +%s) - 3 * 86400))
if [ -n "$oldest" ] && [ $((${oldest%.*} - 86400)) -lt "$stamp" ]; then
    stamp=$((${oldest%.*} - 86400))
fi
while IFS= read -r entry; do
    find "$entry" -depth -exec touch -h -d "@$stamp" {} +
done <"$evidence/workload.txt"

step ordinary-rebuild build "$gate_features" "$evidence/ordinary-rebuild.jsonl"
entries >"$evidence/before-timed.txt"

step timed-pass just pool-maintenance
entries >"$evidence/after-timed.txt"

LC_ALL=C comm -23 "$evidence/before-timed.txt" "$evidence/after-timed.txt" >"$evidence/removed.txt"
LC_ALL=C comm -23 "$evidence/removed.txt" "$evidence/workload.txt" >"$evidence/removed-ordinary.txt"
LC_ALL=C comm -12 "$evidence/workload-own.txt" "$evidence/after-timed.txt" >"$evidence/kept-own.txt"
if [ -s "$evidence/removed-ordinary.txt" ]; then
    cat "$evidence/removed-ordinary.txt" >&2
    echo "measure: the timed pass removed the entries above, which are outside the alternative workload, so it measured more than one run's worth of superseded builds" >&2
    echo "ACTION: re-run ./measure.sh, whose backlog pass clears them first; if they recur, read $evidence" >&2
    exit 1
fi
if [ -s "$evidence/kept-own.txt" ]; then
    cat "$evidence/kept-own.txt" >&2
    echo "measure: the timed pass kept the entries above, which the alternative build made for this repository's own crates, so it timed less than the workload" >&2
    echo "ACTION: read $evidence/timed-pass.log and $evidence/workload-own.txt; each must be superseded by an ordinary build and older than 2 days" >&2
    exit 1
fi
if [ ! -s "$evidence/removed.txt" ]; then
    echo "measure: the timed pass removed none of the $(wc -l <"$evidence/workload.txt") entries of the alternative workload" >&2
    echo "ACTION: read $evidence/timed-pass.log and $evidence/workload.txt; they must be superseded and older than 2 days" >&2
    exit 1
fi

workload="alternative workload: $(wc -l <"$evidence/workload-hashes.txt") unit hashes ($(wc -l <"$evidence/workload-own-hashes.txt") of this repository's crates), $(wc -l <"$evidence/workload.txt") entries ($(wc -l <"$evidence/workload-incremental.txt") incremental), $(wc -l <"$evidence/removed.txt") removed"
if [ -n "${ONEBUDGETSPEC_RESULT:-}" ]; then
    # shellcheck disable=SC2016 # JavaScript, single-quoted so the shell expands none of it.
    node -e '
const fs = require("fs");
const [file, seconds] = process.argv.slice(1);
const result = JSON.parse(fs.readFileSync(file, "utf8"));
const [, , workload] = process.argv.slice(1);
result.detail = `${result.detail ?? ""}; untimed backlog pass seconds (not in the value): ${seconds}; ${workload}`;
fs.writeFileSync(file, JSON.stringify(result));
' "$ONEBUDGETSPEC_RESULT" "$backlog_seconds" "$workload"
fi
echo "measure: backlog pass ${backlog_seconds}s; $workload, every one of this repository's and no entry outside the workload ($evidence)" >&2
