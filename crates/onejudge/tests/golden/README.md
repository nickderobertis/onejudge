# Golden documents

Checked-in bytes the gate holds this crate to. `report.example-v14.json` and
`report.schema-v14.json` are the `Report` wire contract (`tests/contract.rs`);
`notes/`, `single-judge/` and `single-judge-control/` are captured journeys
(`tests/notes.rs`, `tests/e2e.rs`).

## `onejudge-0.8.1-note.json`

The bytes and refusal words of the note shapes as `onejudge` released them.
Captured by compiling `crates/onejudge/src/note.rs` at tag `v0.8.1` (commit
`729bd43e3b9ff5c7a63415ebd755dd70ab0ce5aa`) unchanged in a scratch crate, and
serializing, refusing and rendering the values that module's own unit tests use;
its `provenance` field says the same.

It reached this directory by way of `onemessagebus-agent`, which held the note
contract for three releases: this file is byte-identical to
`crates/onemessagebus-agent/tests/golden/onejudge-0.8.1-note.json` at that
repository's tag `onemessagebus-agent-v0.8.0`, the last one before the crate was
retired and the contract came back here. `tests/note_contract.rs` builds each
value with `onejudge::note` and compares.

Re-capture it only against a new `onejudge` release, as a new file.

## `judge-posture-0.15.0/`

Every harness invocation — argv and stdin — that the released `onejudge` 0.15.0
(commit `3f64a73348563e2cbfdab3dfce6e27eae86c3332`) caused for one config whose
judge names no permission mode, once per seam: `in-process.json` (the linked
engine) and `spawned.json` (a spawned oneharness, which also records the
`oneharness` argv onejudge passed). Recorded by
`scripts/capture-judge-posture-baseline.sh` with this tree's recording doubles and
replayed by `tests/cli.rs`, so a judge left at its default posture is held to
0.15.0's harness argv and judge prompts byte for byte. Per-run paths are
normalized to `{{RUN}}`, `{{HISTORY_FILE}}` and `{{HARNESS}}`.
