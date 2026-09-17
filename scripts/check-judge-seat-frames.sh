#!/usr/bin/env bash
set -euo pipefail

cargo run -q -p onejudge --features sdk-schema --example generate_judge_seat_frames -- --check

base=$(git merge-base HEAD origin/main 2>/dev/null || true)
if [[ -z "$base" ]] || ! git cat-file -e "$base:schemas/judge-seat-frames.json" 2>/dev/null; then
    exit 0
fi

scratch=${ONEPIPELINE_NODE_SCRATCH_DIR:-target}
mkdir -p "$scratch"
base_bundle="$scratch/judge-seat-frames.base.json"
git show "$base:schemas/judge-seat-frames.json" > "$base_bundle"
cargo run -q -p onejudge --features sdk-schema --example check_judge_seat_frame_version -- \
    "$base_bundle" schemas/judge-seat-frames.json
