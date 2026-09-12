# The Report contract

`Report` is onejudge's own **versioned result contract**: a serializable bundle of
a judged run — the transcript, the verdicts scored against it, and aggregated
usage. It is the wire form higher-level frameworks (e.g. `skilltest`) compose over
and re-export, so onejudge — not its consumers — owns the shape of a judged run.

## Shape

```jsonc
{
  "schema_version": 12,                 // bump on any wire change
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
  "judge_decisions": [                  // omitted when empty: only a JudgePanel records these (see below)
    { "turn": 1, "decisions": [        // one entry per supervisor turn, in turn order
        { "judge": "reviewer", "kind": "oneharness", "decision": "done", "reason": "a git commit ran" },
        { "judge": "command", "kind": "command", "decision": "done", "reason": "the commit script passed" }
      ] }
  ],
  "usage": {                            // omitted when nothing reported
    "input_tokens": 12, "output_tokens": 3,
    "cache_read_tokens": 9, "cache_write_tokens": 4   // prompt-cache reads/writes, when the harness reports them
  },
  "telemetry": {                        // omitted when nothing was measured
    "wall_ms": 40,
    "agent": { "model_ms": 20, "tool_ms": 5, "session_ids": ["native-agent-1"] },
    "judge": { "model_ms": 10, "tool_ms": 0 },
    "orchestration_ms": 5,
    "sessions": [ /* one link per invocation that exposed a native session id; `judge` names the panel judge, when there is one */ ],
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
        // "judge": "reviewer"         // only on a judge-side call made by one judge of a panel of several
      }
    ]
  },
  "processes": [                        // omitted when the run spawned nothing
    { "role": "agent", "op": "respond", "program": "oneharness", "pid": 41231,
      "group": "job:run-1" },          // only when a SpawnHook named one
    { "role": "judge", "op": "judge", "program": "oneharness", "pid": 41244,
      "judge": "reviewer" }            // only when a panel of several judges spawned it
  ],
  "control": {                          // ALWAYS present; null when not asked for
    "session": "run-42-skill",         // the three values `oneharness interrupt` takes
    "session_dir": "/state/oneharness/sessions",
    "cwd": "/work/repo"
  },
  "control_unavailable": "…",           // omitted unless an ASKED-FOR lever is missing
  "supervisor_control": null,           // ALWAYS present; the SUPERVISOR turn's own address
  "supervisor_control_unavailable": "…",// omitted unless an ASKED-FOR lever is missing
  "stopped_early": false
}
```

`telemetry.attribution` is what makes a failure attributable to a **side** (agent
vs judge) and an **identity** (which harness, which account) without parsing a
message. `status`, `failure_kind`, and each `fell_through.reason` are oneharness's
own wire tokens — `failure_kind` is one of `auth`, `rate_limit`,
`model_not_found`, `quota`, `tool_deferred`, `session_not_found`,
`untrusted_directory`, `input_too_large`, `model_mismatch`, passed through
verbatim from its closed taxonomy — and `history_id` resolves through
`oneharness history show`. A candidate's `model` is the model *requested*; the
model the harness itself said it would serve (`observed_model`, the other half
of a `model_mismatch`) stays on oneharness's own report and history record,
which onejudge reads typed but does not restate here.

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
completion decision, and it has three causes — the text says which:

* the supervisor judged the work incomplete and then named no next instruction to
  act on, even when asked again (`no next instruction`);
* the supervisor answered in neither documented shape — a paragraph where the
  contract wants one JSON object — and did again when asked again with the shape
  restated (`did not parse`). The reason carries what was wrong and the
  supervisor's last answer verbatim, because a paragraph may have argued a real
  point and this is where it is finally read. It is *not* a transport failure:
  the harness delivered the turn, so the run is settled, never failed as
  `protocol`; or
* the exchanges themselves stopped moving — `NOOP_SETTLE_LIMIT` consecutive turns
  that recorded no tool activity and gave the same tiny answer to the same tiny
  instruction. Every turn is still counted against `max_turns`; settling only ends
  the run *earlier* than the cap would. A conversation whose contract *is* to
  report nothing says so with `user.settle_on_noop: false`
  (`Settings::with_settle_on_noop`) and is never settled this way — it ends at
  `max_turns` like any other run.

Neither is a failure, and a run that simply hit `max_turns` carries neither.

The distinction is the point: without it, a supervisor with nothing to say is
indistinguishable from an agent that could not do the task, and an operator acts
on the wrong one. The three settle causes, a genuine provider failure (an
`Error`, and under the CLI a `FailureReport`), and a completion are each
distinguishable for the same reason. See [protocol.md](protocol.md#supervisor--decide-completion-or-produce-the-next-user-turn).

## `control` / `supervisor_control` — where a controllable turn is addressed

Present on every report, so a supervisor keys on the value rather than on whether
the key exists. `null` means turn control was not asked for
(`provider.control: false`, the default). `null` **with** a `control_unavailable`
reason beside it means it was asked for and could not be honored — a different
fact, and the one a supervisor has to route around. See
[control.md](control.md).

**Two pairs, one per party** (v11). `control` is the agent's turn, addressed by the
`<base>-skill` session; `supervisor_control` is the judge's decision, addressed by
`<base>-user`. They are different sockets and are refused independently — a harness
that can be interrupted on one side is not thereby interruptible on the other — so
a reader that has one address and assumes the other holds a lever it does not have.
Under a [judge panel](judges.md) it is the **first** judge's. Through 0.8.1 the CLI
wrote `null` here for every config, because its runtime provider never forwarded
the judge side's answer — a defect against this contract, fixed in v12; the
library path always carried it.

## `judge_decisions` — what each judge of a panel said, per turn (v12)

When the judge side is a [panel](judges.md) — `provider.judges:` with one or
more entries — every supervisor turn records one `JudgeDecision` per judge, in
the panel's list order: the judge's `label`, its provider `kind`, what it decided
(`done`, `continue`, `no_instruction`, `unparseable`, or `error` when its call
failed) and its own `reason` (the error's message, for `error`). One `JudgedTurn`
per supervisor turn, keyed by the assistant turn it judged. The
[`FailureReport`](#when-a-run-fails) carries the same array, the turn that failed
included, so a judge that could not run is on the record beside the ones that
decided and is never read as a pass.

Omitted when empty. A run judged by a bare provider — `kind: oneharness`,
`kind: command`, or a library `SplitProvider` whose judge half is not a
`JudgePanel` — records no decision, and nothing is synthesized for it; every CLI
`split` builds a panel, a single `judge:` included, so it records one decision
per turn. The same label rides
`telemetry.attribution[].judge`, `telemetry.sessions[].judge` and
`processes[].judge`, set only by a panel of **more than one** judge; a panel of
one writes exactly the records a bare provider writes.

## `processes` — what the run spawned, and who owns its group

`group` is present **only** when an in-process embedder's
[`SpawnHook`](spawn-hook.md) reported placing that process in a group it owns. A
record without one is not grouped — onejudge never names a group it did not
observe, so a `null` here is a fact, not a default. The CLI installs no hook, so
its records carry pids and no group.

## Versioning and the drift gate

The wire form is pinned by a canonical serialized example
(`crates/onejudge/tests/golden/report.example-v12.json`) and its generated JSON
Schema (`crates/onejudge/tests/golden/report.schema-v12.json`), both checked by
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
  tool event, a party's reply, a judge of a panel deciding (`judge_decided`), or
  a turn closing — as an in-process embedder receives it
  (`Engine::run_observing`);
- `failure_report`: the document `--format json` writes **instead of** a report
  when the run fails (see below).

## When a run fails

A failed run produces no `Report`, but it is exactly the case a caller needs
attribution for. So `onejudge run --format json` writes a versioned
`FailureReport` where the report would have gone (`--output` included), and exits
2 as before:

```jsonc
{
  "schema_version": 12,
  "error": { "message": "run failed: provider error (supervise[command]): …", "kind": "protocol" },
  "telemetry": { /* as above, including `attribution` */ },
  "processes": [ /* what the failed run had already spawned, as below */ ],
  "judge_decisions": [ /* as above, the turn that failed included */ ]
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
