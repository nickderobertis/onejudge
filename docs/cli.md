# The `onejudge` CLI

The same engine that *tests* a skill can *drive real work*: point it at a harness,
hand it a task, and let an LLM-driven **simulated user act as a supervisor** that
keeps the harness going — pushing back, asking for verification, re-prompting —
until a `done_when` condition holds or `max_turns` is hit. The simulated reviewer
catches "I'm done" claims that aren't and steers the agent to finish, which is a
practical way to complete longer tasks and get higher accuracy on harder ones than
a single-shot prompt.

This is a different framing from a test framework: run **one task** to completion
(the transcript + result), not a matrix of cases-as-assertions (pass/fail).

## Install

The binary is behind the non-default `cli` feature, so a library consumer never
pays for clap or a YAML parser.

```sh
# From source (needs a Rust toolchain):
cargo install onejudge --features cli

# Or download a prebuilt release archive:
curl -fsSL https://raw.githubusercontent.com/nickderobertis/onejudge/main/install.sh | bash
```

`install.sh` honors `ONEJUDGE_INSTALL_DIR` (default `~/.local/bin`) and
`ONEJUDGE_VERSION` (default `latest`). On Windows, download the `.zip` from the
[releases page](https://github.com/nickderobertis/onejudge/releases) or use
`cargo install`.

## Commands

```
onejudge run [CONFIG.yaml] [overrides]   # drive one task to completion
onejudge init [PATH]                     # write a starter onejudge.yaml
onejudge schema                          # print the annotated config
onejudge --help
```

`run` reads `./onejudge.yaml` when no config path is given. Flags override the
file, which overrides defaults:

| flag | overrides |
|------|-----------|
| `--harness` | the platform the agent runs on |
| `--model` | the agent's model |
| `--judge-model` | the simulated user + judge model |
| `--task` (`-` = stdin) | the task |
| `--persona` | the simulated user's persona |
| `--done-when` | the completion condition |
| `--max-turns` | the assistant-turn cap |
| `--session` | the caller-owned session name |
| `--provider` | just the backend kind (`oneharness`/`command`/`split`) |
| `--format` | `human` (default) or `json` |
| `--output`, `-o` | write the result to a file instead of stdout |

Supplying `--persona` / `--done-when` / `--max-turns` implies a simulated user
even if the config had none.

## Output and exit code

- **Human (default):** the conversation (with each turn's tool actions), the
  completion status (completed / hit the turn cap), usage, and any eval verdicts.
  Live tool events stream to **stderr** so a redirected stdout stays clean.
- **`--format json`:** the versioned [`Report`](contract.md) — transcript +
  verdicts + usage, stamped with `schema_version`. This reuses onejudge's existing
  wire contract; it is not a new one.

The **exit code** is `0` only when the task **completed** and every **boolean**
eval passed. A run that hits `max_turns` without satisfying `done_when`, or whose
boolean eval fails, exits `1`. Numeric evals are score-and-report — they never
fail the run (there is no threshold to fail against). A bad config / usage error
exits `2`.

Completion is decided by **re-judging `done_when` against the final transcript**
(the loop's own mid-run check can be preempted by the turn cap), so the exit code
reflects whether the task actually finished. Without a `done_when`, a run is
"completed" when the loop ended before the cap (the agent declared done, the user
stopped, or a single-turn run answered once).

## Config

The authoritative, annotated config is what **`onejudge schema`** prints (and what
`onejudge init` writes) — a single tested source (`starter.yaml`), so this page
describes the fields rather than restating the YAML that would then drift from it.

Top-level keys:

| key | purpose |
|-----|---------|
| `provider` | which backend runs the harness: `kind` is `oneharness` (`bin`, `judge_harness`), `command` (`command: [...]`), or `split` (`skill:` + `judge:` sub-providers) |
| `harness` | the platform the agent runs on (default `claude-code`) |
| `model` / `judge_model` | the agent's model, and the simulated-user + judge model (empty ⇒ harness default / same as `model`) |
| `agent` | `name`, `dir`, and the system `instructions` for the harness |
| `task` | the task to drive to completion (or supply via `--task`) |
| `user` | the simulated supervisor: `persona`, `done_when`, `max_turns` (omit for a single-turn run) |
| `session` | the caller-owned session name threaded across turns |
| `evals` | optional criteria to score the finished transcript: each has a `criterion`, a `kind` (`boolean` / `numeric`), and — for numeric — a `scale: [min, max]` |

The config is validated strictly at the boundary (`deny_unknown_fields`): a typo'd
key, a missing task, a provider field that does not belong to the chosen `kind`
(e.g. `bin` under `kind: command`), or an inverted numeric scale is a loud,
actionable error — never a silent default.

## Providers

The CLI can build any of onejudge's backends from `provider.kind`. Every model
call goes through oneharness:

- **`oneharness`** (default) — shell out to the `oneharness` CLI to drive a real
  harness (Claude Code, Codex, …). See [live-tier.md](live-tier.md).
- **`command`** — a custom backend speaking the [JSON-lines protocol](protocol.md).
- **`split`** — compose a skill-runner with a separate judge / simulated-user
  backend (e.g. drive the agent on one harness, judge on another).
