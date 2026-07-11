# The Report contract

`Report` is onejudge's own **versioned result contract**: a serializable bundle of
a judged run — the transcript, the verdicts scored against it, and aggregated
usage. It is the wire form higher-level frameworks (e.g. `skilltest`) compose over
and re-export, so onejudge — not its consumers — owns the shape of a judged run.

## Shape

```jsonc
{
  "schema_version": 2,                 // bump on any wire change
  "transcript": {
    "messages": [
      { "role": "user", "content": "commit the fix" },
      {
        "role": "assistant",
        "content": "Committed.",
        "events": [                     // normalized ToolEvents (omitted when empty)
          { "kind": "tool_call", "name": "bash",
            "input": { "command": "git commit -m fix" }, "index": 0 }
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
  "usage": {                            // omitted when nothing reported
    "input_tokens": 12, "output_tokens": 3,
    "cache_read_tokens": 9, "cache_write_tokens": 4   // prompt-cache reads/writes, when the harness reports them
  },
  "stopped_early": false
}
```

`verdict.value` is a bool for a `boolean` verdict and a number for a `numeric`
one. `usage` fields are each independently optional — absent means "no signal",
never zero. `cache_read_tokens` / `cache_write_tokens` carry the provider's
prompt-cache reads/writes as surfaced by the harness (see the caching notes in
`AGENTS.md`).

## Building one

```rust
let outcome = engine.run(&conversation)?;
let verdict = engine.judge_boolean("the change was committed", &outcome.transcript)?;
let report = outcome.into_report(vec![
    onejudge::NamedVerdict::new("the change was committed", onejudge::JudgeKind::Boolean, verdict),
]);
assert_eq!(report.schema_version, onejudge::SCHEMA_VERSION);
```

## Versioning and the drift gate

The wire form is pinned by a golden snapshot
(`crates/onejudge/tests/golden/report.schema-v2.json`) and checked by
`tests/contract.rs`. Any change to the serialized shape — a renamed field, a new
key, a changed default — fails that test, so it can only land as a **deliberate**
edit that also bumps `SCHEMA_VERSION` and updates the golden. Downstream SDKs that
re-export these types therefore never drift silently.
