# Golden documents

Replay fixtures `tests/cli.rs` holds the CLI to, each captured from a **released**
binary by a script under `scripts/` and never from the tree under test — save
`judge-posture-one-contract/`, whose release does not exist yet (below).

## `single-judge/` and `single-judge-control/`

What the released 0.8.1 wrote for a `split` with one `judge:` (the `-control` one
with `control: true` on both sides): the report and every judge-side request and
prompt. Captured by `scripts/capture-single-judge-baseline.sh`; they prove a
panel of one is byte-identical to the judge side before panels existed.

## `judge-posture-0.15.0/` (superseded)

Every harness invocation — argv and stdin — that the released `onejudge` 0.15.0
(commit `3f64a73348563e2cbfdab3dfce6e27eae86c3332`) caused for one config whose
judge names no permission mode, once per seam: `in-process.json` (the linked
engine) and `spawned.json` (a spawned oneharness, which also records the
`oneharness` argv onejudge passed). Recorded by
`scripts/capture-judge-posture-baseline.sh` with this tree's recording doubles and
replayed by `tests/cli.rs`. Superseded by `judge-posture-one-contract/` and kept
unedited: a judge left at its default posture is still held to its harness argv
byte for byte — the mode is enforced there — and to its judge prompts with the
read-only evidence contract swapped for the one contract and nothing else
changed. Per-run paths are normalized to `{{RUN}}`, `{{HISTORY_FILE}}` and
`{{HARNESS}}`. The 0.8.1 `single-judge-control/` prompts are replayed the same
way, the double's input-token bill moved by the contract's change in length.

## `judge-posture-one-contract/`

The same config's harness invocations, in the same shape, once every judge is
handed the one `EVIDENCE CONTRACT` whatever its mode. Recorded by
`scripts/capture-judge-posture-baseline.sh` from the tree that introduced the
contract, before its release; what keeps that from being a self-fulfilling
snapshot is the replay's second comparison, against the released 0.15.0 above.
Re-capture it from the first release carrying the contract, or from a tree whose
change to the judge-side prompts is deliberate.

## `pre-cutover-history/`

A oneharness history store exactly as the released `oneharness` 0.20.0 (linking
`oneharness-core` 0.22.0, the last core before dated `.index.d/` segments) left
it after one recorded run: the session file in its line format under its project
slug, the legacy `.index.jsonl` and `.index.lock`, and no `.index.d/`. Recorded by
`scripts/capture-pre-cutover-history.sh` with this tree's `onejudge-fake-harness`
standing in for `claude-code`. `tests/cli.rs` copies it and holds onejudge's
history read to it (the record's `history_id` must reach the report and no file
may change), and uses its session line as the other sessions a large store holds.
Re-capture it only against another pre-cutover release.
