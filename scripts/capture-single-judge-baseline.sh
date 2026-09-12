#!/usr/bin/env bash
# Capture the single-judge baseline fixtures from a RELEASED `onejudge` binary.
#
# The judge side of a run is a panel of judges, and a panel of ONE has to be
# byte-identical to what the release before panels existed produced: the same
# transcript, the same judge-side session names, the same control addresses and
# usage. That release is 0.8.1, and the proof is a replay: `tests/cli.rs` runs
# each checked-in config through the built binary and compares against what 0.8.1
# wrote for it. This script is how those fixtures were captured, so the comparison
# is against the release's own run rather than a hand-written expectation.
#
#   scripts/capture-single-judge-baseline.sh <onejudge-0.8.1> <onejudge-echo-provider> <onejudge-fake-oneharness>
#
# All three are built from the 0.8.1 tree with `--features fake-provider,cli`
# (`cargo build --features fake-provider,cli --bins` in a checkout of `v0.8.1`).
# Two fixtures are written:
#
#   tests/golden/single-judge/          a command-provider split with one `judge:`
#   tests/golden/single-judge-control/  the same with `control: true` on both sides,
#                                       over the fake oneharness (unix: it opens
#                                       real control sockets)
#
# The volatile fields — `telemetry` (wall clock), `processes` (pids) and, on newer
# builds, `judge_decisions` — are stripped; `schema_version` is stripped too, so a
# bump does not count as drift. Per-run paths (the record file, the session stores)
# are normalized back to their `{{PLACEHOLDER}}`s in the report and the logs.
#
# Quiet on success; loud with the failing step otherwise.
set -euo pipefail
cd "$(dirname "$0")/.."

usage="usage: $0 <onejudge-bin> <onejudge-echo-provider-bin> <onejudge-fake-oneharness-bin>"
onejudge_bin="${1:?$usage}"
echo_bin="${2:?$usage}"
oneharness_bin="${3:?$usage}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

json_string() { python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$1"; }

# capture <fixture-dir> <log-path> <sed-substitutions...>
#
# Renders the fixture's config with its placeholders substituted, runs it, and
# writes the normalized report and the judge-side log (at `<log-path>`, checked in
# under its basename) back into the fixture.
capture() {
    local fixture="$1" log="$2"; shift 2
    local run="$work/$(basename "$fixture")"
    mkdir -p "$run"
    sed "$@" "$fixture/config.yaml" > "$run/config.yaml"
    # Exit 1 is an incomplete run (the controlled fixture ends at its turn cap) and
    # still writes a report; only 2 — a config or provider failure — writes none.
    local status=0
    "$onejudge_bin" run "$run/config.yaml" --format json --output "$run/report.json" 2>"$run/stderr" || status=$?
    if [ "$status" -gt 1 ]; then
        echo "capture-single-judge-baseline: the $(basename "$fixture") run failed (exit $status):" >&2
        cat "$run/stderr" >&2
        exit 1
    fi
    python3 - "$run/report.json" "$log" "$fixture" "$(basename "$log")" "${PLACEHOLDERS[@]}" <<'PY'
import json, sys
report_path, log_path, fixture, log_name, *pairs = sys.argv[1:]
placeholders = dict(zip(pairs[::2], pairs[1::2]))  # real path -> {{PLACEHOLDER}}

def normalize(text):
    for real, placeholder in placeholders.items():
        text = text.replace(json.dumps(real)[1:-1], placeholder).replace(real, placeholder)
    return text

report = json.load(open(report_path))
for volatile in ("schema_version", "telemetry", "processes", "judge_decisions"):
    report.pop(volatile, None)
with open(f"{fixture}/report.json", "w") as out:
    out.write(normalize(json.dumps(report, indent=2, ensure_ascii=False)) + "\n")
with open(log_path) as src, open(f"{fixture}/{log_name}", "w") as out:
    out.write(normalize(src.read()))
PY
}

# --- single-judge: a command-provider split with one `judge:` -----------------
fixture=crates/onejudge/tests/golden/single-judge
record="$work/supervisor-requests.jsonl"
PLACEHOLDERS=("$record" "{{RECORD}}")
capture "$fixture" "$record" \
    -e "s|{{ECHO}}|$(json_string "$echo_bin")|" -e "s|{{RECORD}}|$record|"

# --- single-judge-control: the same with control: true over the fake oneharness
#
# Its paths ride the judge PROMPT (the persona inlines them), and the fake
# oneharness bills the prompt's length as `input_tokens` — so every path is spelled
# at a FIXED width under `/tmp`, exactly as the replay spells its own
# (`/tmp/oj-ctl-<pid, 8 digits>/…`), or the usage would differ by the width of a
# temp dir name. `/tmp` rather than `$TMPDIR` for the same reason: a macOS temp
# dir is forty characters longer than a Linux one.
fixture=crates/onejudge/tests/golden/single-judge-control
ctl="/tmp/oj-ctl-$(printf '%08d' "$$")"
rm -rf "$ctl"; mkdir -p "$ctl/agent-store" "$ctl/judge-store"
trap 'rm -rf "$work" "$ctl"' EXIT
record="$ctl/prompts.log"
agent_store="$(realpath "$ctl/agent-store")"
judge_store="$(realpath "$ctl/judge-store")"
PLACEHOLDERS=("$record" "{{RECORD}}" "$agent_store" "{{AGENT_STORE}}" "$judge_store" "{{JUDGE_STORE}}")
capture "$fixture" "$record" \
    -e "s|{{ONEHARNESS}}|$(json_string "$oneharness_bin")|" -e "s|{{RECORD}}|$record|" \
    -e "s|{{AGENT_STORE}}|$agent_store|" -e "s|{{JUDGE_STORE}}|$judge_store|"

echo "captured both single-judge fixtures from $("$onejudge_bin" --version)"
