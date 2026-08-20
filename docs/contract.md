# The Report contract

`Report` is onejudge's own **versioned result contract**: a serializable bundle of
a judged run — the transcript, the verdicts scored against it, and aggregated
usage. It is the wire form higher-level frameworks (e.g. `skilltest`) compose over
and re-export, so onejudge — not its consumers — owns the shape of a judged run.

## Shape

```jsonc
{
  "schema_version": 10,                 // bump on any wire change
  "transcript": {
    "messages": [
      { "role": "user", "content": "commit the fix" },
      {
        "role": "assistant",
        "content": "Committed.",
        "events": [                     // normalized ToolEvents (omitted when empty)
          { "kind": "tool_call", "name": "bash",
            "input": { "command": "git commit -m fix" }, "index": 0,
            "tool_call_id": "toolu_01A" }  // omitted when the harness exposed none
        ]
      }
    ]
  },
  "verdicts": [                         // omitted when empty
    {
      "criterion": "the change was committed",
      "kind": "boolean",               // "boolean" | "numeric"
      "verdict": { "value": true, "reason": "a git commit ran" }
    }
  ],
  "assessment": "No follow-up work remains.", // omitted when not requested
  "completion_reason": "all required tests passed", // omitted unless the supervisor completed the run
  "settled_reason": "…gave no next instruction…",   // omitted unless the run settled instead (see below)
  "usage": {                            // omitted when nothing reported
    "input_tokens": 12, "output_tokens": 3,
    "cache_read_tokens": 9, "cache_write_tokens": 4   // prompt-cache reads/writes, when the harness reports them
  },
  "telemetry": {                        // omitted when nothing was measured
    "wall_ms": 40,
    "agent": { "model_ms": 20, "tool_ms": 5, "session_ids": ["native-agent-1"] },
    "judge": { "model_ms": 10, "tool_ms": 0 },
    "orchestration_ms": 5,
    "sessions": [ /* one link per invocation that exposed a native session id */ ],
    "attribution": [                    // omitted when the provider names no candidate
      {
        "role": "agent",               // "agent" | "judge" — which SIDE made the call
        "turn_index": 1,               // joins to `sessions` by (role, turn_index)
        "ran": "claude-code",          // the candidate that ran; null when none could
        "fell_through": [              // a fallback chain's routed-around candidates
          { "harness": "codex", "reason": "quota" }
        ],
        "candidates": [                // every ATTEMPTED identity, in order
          {
            "harness": "codex", "harness_id": "codex:work", "variant": "work",
            "model": "gpt-5.5", "status": "nonzero", "available": true, "ran": false,
            "failure_kind": "quota", "failure_kind_source": "stderr",
            "exit_code": 1, "duration_ms": 4, "error": "out of credit",
            "history_id": "019b76e0-codex"
          }
        ],
        "history_file": "/state/oneharness/history/run-1-skill.jsonl"
      }
    ]
  },
  "processes": [                        // omitted when the run spawned nothing
    { "role": "agent", "op": "respond", "program": "oneharness", "pid": 41231,
      "group": "job:run-1" },          // only when a SpawnHook named one
    { "role": "judge", "op": "judge", "program": "oneharness", "pid": 41244 }
  ],
  "control": {                          // ALWAYS present; null when not asked for
    "session": "run-42-skill",         // the three values `oneharness interrupt` takes
    "session_dir": "/state/oneharness/sessions",
    "cwd": "/work/repo"
  },
  "control_unavailable": "…",           // omitted unless an ASKED-FOR lever is missing
  "stopped_early": false
}
```

`telemetry.attribution` is what makes a failure attributable to a **side** (agent
vs judge) and an **identity** (which harness, which account) without parsing a
message. `status`, `failure_kind`, and each `fell_through.reason` are oneharness's
own wire tokens, and `history_id` resolves through `oneharness history show`.

`verdict.value` is a bool for a `boolean` verdict and a number for a `numeric`
one. `usage` fields are each independently optional — absent means "no signal",
never zero. `cache_read_tokens` / `cache_write_tokens` carry the provider's
prompt-cache reads/writes as surfaced by the harness.

## Building one

```rust
let outcome = engine.run(&conversation)?;
let verdict = engine.judge_boolean("the change was committed", &outcome.transcript)?;
let report = outcome.into_report(vec![
    onejudge::NamedVerdict::new("the change was committed", onejudge::JudgeKind::Boolean, verdict),
]);
assert_eq!(report.schema_version, onejudge::SCHEMA_VERSION);
```

## `completion_reason` vs `settled_reason` — how the loop ended

At most one of the two is present, and they say different things. A
`completion_reason` is the supervisor deciding the task is done. A
`settled_reason` is the loop ending on the work it already had, **without** a
completion decision, and it has two causes — the text says which:

* the supervisor judged the work incomplete and then named no next instruction to
  act on, even when asked again; or
* the exchanges themselves stopped moving — `NOOP_SETTLE_LIMIT` consecutive turns
  that recorded no tool activity and gave the same tiny answer to the same tiny
  instruction. Every turn is still counted against `max_turns`; settling only ends
  the run *earlier* than the cap would.

Neither is a failure, and a run that simply hit `max_turns` carries neither.

The distinction is the point: without it, a supervisor with nothing to say is
indistinguishable from an agent that could not do the task, and an operator acts
on the wrong one. See [protocol.md](protocol.md#supervisor--decide-completion-or-produce-the-next-user-turn).

## `control` — where a controllable turn is addressed

Present on every report, so a supervisor keys on the value rather than on whether
the key exists. `null` means turn control was not asked for
(`provider.control: false`, the default). `null` **with** a `control_unavailable`
reason beside it means it was asked for and could not be honored — a different
fact, and the one a supervisor has to route around. See
[control.md](control.md).

## `processes` — what the run spawned, and who owns its group

`group` is present **only** when an in-process embedder's
[`SpawnHook`](spawn-hook.md) reported placing that process in a group it owns. A
record without one is not grouped — onejudge never names a group it did not
observe, so a `null` here is a fact, not a default. The CLI installs no hook, so
its records carry pids and no group.

## Versioning and the drift gate

The wire form is pinned by a canonical serialized example
(`crates/onejudge/tests/golden/report.example-v10.json`) and its generated JSON
Schema (`crates/onejudge/tests/golden/report.schema-v10.json`), both checked by
`tests/contract.rs`. Any change to the serialized shape — a renamed field, a new
key, a changed default — fails that test, so it can only land as a **deliberate**
edit that also bumps `SCHEMA_VERSION` and updates both goldens. Downstream SDKs
that re-export these types therefore never drift silently.

Every `"schema_version"` this page spells out is checked against the constant by
the same test — the `FailureReport` example below sat three bumps behind before
that gate existed, and an ungated copy of a contract is a copy that will drift.

## SDK schema bundle

With the opt-in `sdk-schema` feature, onejudge exposes a deterministic bundle of
named JSON Schema roots:

- `run_config`: the YAML config object accepted by `onejudge run`;
- `report`: the versioned JSON output emitted by `--format json`;
- `stream_event`: the `{ turn, event }` envelope delivered by streaming runs;
- `observation`: one live observation of a run in progress — a turn opening, a
  tool event, a party's reply, or a turn closing — as an in-process embedder
  receives it (`Engine::run_observing`);
- `failure_report`: the document `--format json` writes **instead of** a report
  when the run fails (see below).

## When a run fails

A failed run produces no `Report`, but it is exactly the case a caller needs
attribution for. So `onejudge run --format json` writes a versioned
`FailureReport` where the report would have gone (`--output` included), and exits
2 as before:

```jsonc
{
  "schema_version": 10,
  "error": { "message": "run failed: provider error (respond): …", "kind": "auth" },
  "telemetry": { /* as above, including `attribution` */ },
  "processes": [ /* what the failed run had already spawned, as below */ ]
}
```

Under `--stream` the same document goes to **stderr** as one compact JSON line —
stdout there is the `event* result EOF` protocol (`docs/streaming.md`) and stays
exactly as documented. The Python SDK parses whichever one this run wrote and
attaches it to `OneJudgeProcessError.failure`.

Generate it from the Rust contracts with:

```console
cargo run -q -p onejudge --features sdk-schema --example generate_sdk_schema
```

The feature includes the CLI config types but remains non-default, so neither a
bare library consumer nor a `cli`-only build compiles `schemars`.
