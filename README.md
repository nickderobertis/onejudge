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

## The `onejudge` CLI

The same engine that *tests* a skill can *drive real work*. `onejudge run` points a
harness at a task and lets an LLM-driven **simulated user supervise** it — pushing
back, asking for verification, re-prompting — until a `done_when` condition holds
or `max_turns` is hit. Configured by YAML; the library API is unchanged and CLI
deps (clap, a YAML parser) are opt-in behind the non-default `cli` feature.

**Spin up a run in three steps:**

```sh
cargo install onejudge --features cli   # or: install.sh (prebuilt archives)
onejudge init                           # scaffold onejudge.yaml + oneharness configs
onejudge run                            # reads ./onejudge.yaml, drives to completion
```

`init` shells out to `oneharness init` (needs oneharness **0.3.20+**) to scaffold
`oneharness.toml` (the agent side) and `oneharness.judge.toml` (the judge side),
then writes a fully-commented loop-only `onejudge.yaml`. The fields that make a run
yours are `task` (what to do), the system framing — a `skill` (a `SKILL.md`
directory) and/or a `system_prompt`, both optional — and the `user` block
(`persona` / `done_when` / `max_turns` — omit it for a single-turn run). **Harness
and model selection lives in those `oneharness.toml`
files, not `onejudge.yaml`.** `onejudge schema` prints the annotated config, the
single source of truth for every field.

Flags override the file (flags > file > defaults), so one config serves many tasks:
`onejudge run --task - < task.txt`, `--max-turns 8`, `--format json -o result.json`.

### Config

A run is a YAML file carrying only the loop's own concerns. The fields that make it
yours — `task`, the system framing (`skill` and/or `system_prompt`), and the `user`
block. Everything else has a default; omit `user` for a single-turn run. A minimal
config:

```yaml
system_prompt: You are a senior engineer. Complete the task and keep tests green.
# skill: ./skills/my-skill    # optional: a SKILL.md dir; its body is appended

task: Add a --version flag to the CLI.

user:                         # the simulated supervisor that drives the loop
  persona: A demanding tech lead. Do not accept "done" until you have verified it.
  done_when: the task is complete and all tests pass
  max_turns: 8

evals:                        # optional: score the finished transcript
  - criterion: the change is well-scoped and readable
    kind: numeric
    scale: [1, 5]
```

The harness and model come from oneharness's own config (`oneharness.toml` for the
agent, `oneharness.judge.toml` for the judge side) — `onejudge init` scaffolds
them. More keys — `provider` (`oneharness` / `command` / `split`, with the
oneharness `judge_config` path), `session`, boolean evals. `onejudge init` writes a
fully-commented starter and `onejudge schema` prints the annotated field reference
(the single source of truth); it is validated strictly (`deny_unknown_fields`) so a
typo is a loud error.

Human output is the conversation + tool actions + completion status + eval
verdicts; `--format json` emits the versioned [`Report`](docs/contract.md). The
exit code is `0` only when the task completed and every boolean eval passed, `1`
if it hit `max_turns` or a boolean eval failed, `2` on a bad config. Full docs:
**[docs/cli.md](docs/cli.md)**.

## Concepts

- **`Provider`** is the boundary — onejudge never talks to a model directly. Every
  model call goes through `oneharness`, and harness/model *selection* lives in
  oneharness's config files, not onejudge.
  - **`OneharnessProvider`** (default) shells out to the `oneharness` CLI
    (v0.3.20+): the agent side uses the discovered `oneharness.toml`, and the judge
    side uses a separate `--config` file (default `oneharness.judge.toml`).
  - **`CommandProvider`** speaks a small [JSON-lines protocol](docs/protocol.md),
    for a custom backend or a deterministic test double.
  - **`SplitProvider`** composes two providers — one that runs the skill, one that
    judges and role-plays the user (e.g. run the skill on one harness, judge on
    another).
- **`Engine`** runs a **`Conversation`** (a `Skill`, an initial input, and an
  optional `SimulatedUser`) into a **`Transcript`**, bounded by `max_turns` /
  `done_when` / the skill declaring itself done.
- **`Transcript`** carries each turn plus the normalized **`ToolEvent`**s the
  skill took, so the judge — and a **`ToolQuery`** — can reason over *what the
  skill did*, not just what it said.
- **`Report`** is onejudge's own versioned contract (`SCHEMA_VERSION`): a
  serializable bundle of the transcript, the verdicts, and usage that higher-level
  frameworks compose over and re-export. See [docs/contract.md](docs/contract.md).

Two things it improves over the in-skilltest engine:

1. **The judge sees tool events.** Verdicts render the transcript with a compact,
   token-budget-aware summary of each turn's tool calls, so a criterion like
   "the change was committed" can be decided from the `git commit` the skill
   actually ran — not only from what it said. `Transcript` also exposes a
   `ToolQuery` primitive for events-backed assertions with no judge call.
2. **One caller-owned session name.** The engine always threads a single
   `--session <name>` across turns instead of extracting and re-passing a native
   id; if a harness cannot bind a session, the provider gracefully retries the call
   without it, re-prompting the inlined transcript.

## Example

```rust
use onejudge::{Conversation, Engine, OneharnessProvider, Settings, SimulatedUser, Skill};

let provider = OneharnessProvider::new();
// Harness/model selection lives in oneharness's config files, not here; Settings
// carries only the loop's own concerns (turn cap, session name).
let settings = Settings::new();
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
test doubles**, never mocked. The one path that needs a real external service is
proven in an opt-in tier, kept out of `check`:

- **`just test-live`** — the `OneharnessProvider` path against a real harness (see
  [docs/live-tier.md](docs/live-tier.md)).

See [AGENTS.md](AGENTS.md) for the durable contributor guide.

## License

[MIT](LICENSE).
