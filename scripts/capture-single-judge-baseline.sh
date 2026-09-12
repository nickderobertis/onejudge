#!/usr/bin/env bash
# Capture the single-judge baseline fixture from a RELEASED `onejudge` binary.
#
# The judge side of a run is a panel of judges, and a panel of ONE has to be
# byte-identical to what the release before panels existed produced: the same
# transcript, the same judge-side session names, the same control addresses and
# usage. That release is 0.8.1, and the proof is a replay: `tests/cli.rs` runs
# the checked-in config through the built binary and compares against what 0.8.1
# wrote for it. This script is how that fixture was captured, so the comparison is
# against the release's own run rather than a hand-written expectation.
#
#   scripts/capture-single-judge-baseline.sh <onejudge-0.8.1> <onejudge-echo-provider>
#
# Both binaries are built from the 0.8.1 tree with `--features fake-provider,cli`
# (`cargo build --features fake-provider,cli --bins` in a checkout of `v0.8.1`).
# The volatile fields — `telemetry` (wall clock), `processes` (pids) and, on newer
# builds, `judge_decisions` — are stripped; `schema_version` is stripped too, so a
# bump does not count as drift. The record path is normalized to `{{RECORD}}`.
#
# Quiet on success; loud with the failing step otherwise.
set -euo pipefail
cd "$(dirname "$0")/.."

onejudge_bin="${1:?usage: $0 <onejudge-bin> <onejudge-echo-provider-bin>}"
echo_bin="${2:?usage: $0 <onejudge-bin> <onejudge-echo-provider-bin>}"
fixture=crates/onejudge/tests/golden/single-judge
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

record="$work/supervisor-requests.jsonl"
echo_json="$(python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$echo_bin")"
sed -e "s|{{ECHO}}|$echo_json|" -e "s|{{RECORD}}|$record|" "$fixture/config.yaml" > "$work/config.yaml"

"$onejudge_bin" run "$work/config.yaml" --format json --output "$work/report.json" 2>"$work/stderr" \
    || { echo "capture-single-judge-baseline: the run failed:" >&2; cat "$work/stderr" >&2; exit 1; }

python3 - "$work/report.json" "$record" "$fixture" <<'PY'
import json, sys
report_path, record_path, fixture = sys.argv[1:4]
report = json.load(open(report_path))
for volatile in ("schema_version", "telemetry", "processes", "judge_decisions"):
    report.pop(volatile, None)
with open(f"{fixture}/report.json", "w") as out:
    json.dump(report, out, indent=2, ensure_ascii=False)
    out.write("\n")
escaped = json.dumps(record_path)[1:-1]
with open(record_path) as src, open(f"{fixture}/supervisor-requests.jsonl", "w") as out:
    for line in src:
        out.write(line.replace(escaped, "{{RECORD}}"))
PY
echo "captured $fixture from $("$onejudge_bin" --version)"
