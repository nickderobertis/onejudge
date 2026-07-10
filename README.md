# onejudge

A Rust library that drives a **simulated interaction and evaluation loop** on top
of [`oneharness`](https://github.com/nickderobertis/oneharness): take a skill or
agent, drive it through a multi-turn conversation with a simulated user, and score
the resulting transcript with natural-language (judge) verdicts and tool-event
queries.

It is the engine extracted from
[`skilltest`](https://github.com/nickderobertis/skilltest) (see
[nickderobertis/skilltest#31](https://github.com/nickderobertis/skilltest/issues/31)).
The layering:

```
oneharness  →  one harness invocation, one JSON report   (pure substrate)
onejudge    →  simulated interaction + judging loop        (this crate)
skilltest   →  test-framework surface: cases, evals-as-assertions, SDKs
```

Reach for onejudge when you want to "drive a harness through a simulated
conversation and score the transcript" without skilltest's YAML / case framing.

## Install

```sh
cargo add onejudge
```

Minimum supported Rust version: **1.82**.

## Concepts

- **`Provider`** is the boundary — onejudge never talks to a model directly.
  - **`OneharnessProvider`** (default) shells out to the `oneharness` CLI
    (v0.3.13+), so it drives any harness oneharness supports (Claude Code, Codex,
    OpenCode, …).
  - **`CommandProvider`** speaks a small [JSON-lines protocol](docs/protocol.md),
    for a custom backend or a deterministic test double.
- **`Engine`** runs a **`Conversation`** (a `Skill`, an initial input, and an
  optional `SimulatedUser`) into a **`Transcript`**, bounded by `max_turns` /
  `done_when` / the skill declaring itself done.
- **`Transcript`** carries each turn plus the normalized **`ToolEvent`**s the
  skill took, so the judge — and a **`ToolQuery`** — can reason over *what the
  skill did*, not just what it said.

Two things it improves over the in-skilltest engine:

1. **The judge sees tool events.** Verdicts render the transcript with a compact,
   token-budget-aware summary of each turn's tool calls, so a criterion like
   "the change was committed" can be decided from the `git commit` the skill
   actually ran — not only from what it said. `Transcript` also exposes a
   `ToolQuery` primitive for events-backed assertions with no judge call.
2. **One caller-owned session name.** On session-capable harnesses (claude-code,
   codex, opencode, cursor, qwen) the engine threads a single `--session <name>`
   across turns instead of extracting and re-passing a native id; the rest fall
   back to re-prompting the inlined transcript.

## Example

```rust
use onejudge::{Conversation, Engine, OneharnessProvider, Settings, SimulatedUser, Skill};

let provider = OneharnessProvider::new();
// (platform, model, judge_model). An empty model lets the harness use its default.
let settings = Settings::new("claude-code", "", "claude-opus-4-8");
let engine = Engine::new(&provider, settings);

let skill = Skill::new("greeter", "./skills/greeter", "Greet the user warmly.");
let user = SimulatedUser::new("A curious first-time visitor.")
    .done_when("the assistant has answered the visitor's question")
    .max_turns(6);

let outcome = engine.run(&Conversation::multi_turn(skill, "hi", user))?;

let verdict = engine.judge_boolean("the reply was welcoming", &outcome.transcript)?;
println!("{:?}: {}", verdict.value, verdict.reason);
# Ok::<(), onejudge::Error>(())
```

Drive a deterministic backend instead of a live harness by pointing a
`CommandProvider` at any command that speaks the [protocol](docs/protocol.md).

## Development

The command surface is a `just` recipe set; `just --list` is the index.

```sh
just bootstrap   # clean-clone setup: toolchain + cargo tools + fetch
just check       # the full gate: format, lint, doc, coverage-enforced tests, audit
just test        # fast unit + integration + e2e
```

The gate is deterministic and offline — the model is faked by **real subprocess
test doubles**, never mocked. The `OneharnessProvider` path against a real
harness is proven by the opt-in live tier (`just test-live`; see
[docs/live-tier.md](docs/live-tier.md)).

See [AGENTS.md](AGENTS.md) for the durable contributor guide.

## License

[MIT](LICENSE).
