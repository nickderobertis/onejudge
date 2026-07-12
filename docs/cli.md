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
onejudge init [PATH]                     # scaffold onejudge.yaml + oneharness configs
onejudge schema                          # print the annotated config
onejudge --help
```

**Harness/model selection lives in oneharness's config, not onejudge.** The agent
side runs under oneharness's discovered `oneharness.toml`, and the judge /
simulated-user side runs under a separately-named config (`provider.judge_config`,
default `oneharness.judge.toml`, passed as `oneharness run --config <path>`).
`onejudge init` scaffolds all three by shelling out to `oneharness init` (needs
oneharness **0.3.20+**): it runs `oneharness init oneharness.toml` and `oneharness
init oneharness.judge.toml`, then writes the loop-only `onejudge.yaml`. Pass
`--oneharness-bin <path>` if `oneharness` is not on `PATH`, and `--force` to
overwrite existing files. To change the harness or model, edit those `.toml`
files (or use oneharness's own `ONEHARNESS_*` env overrides) — not `onejudge.yaml`.

`run` reads `./onejudge.yaml` when no config path is given. Flags override the
file, which overrides defaults:

| flag | overrides |
|------|-----------|
| `--judge-config` | the judge/simulated-user oneharness `--config` file |
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
| `provider` | which backend runs the harness: `kind` is `oneharness` (`bin`, `judge_config`), `command` (`command: [...]`), or `split` (`skill:` + `judge:` sub-providers) |
| `agent` | `name`, `dir`, and the system `instructions` for the harness |
| `task` | the task to drive to completion (or supply via `--task`) |
| `user` | the simulated supervisor: `persona`, `done_when`, `max_turns` (omit for a single-turn run) |
| `session` | the caller-owned session name threaded across turns |
| `evals` | optional criteria to score the finished transcript: each has a `criterion`, a `kind` (`boolean` / `numeric`), and — for numeric — a `scale: [min, max]` |

There is no `harness` / `model` / `judge_model` key: harness and model selection
moved into oneharness's own config files (`oneharness.toml` for the agent,
`provider.judge_config` — default `oneharness.judge.toml` — for the judge side).

The config is validated strictly at the boundary (`deny_unknown_fields`): a typo'd
key, a missing task, a provider field that does not belong to the chosen `kind`
(e.g. `bin` or `judge_config` under `kind: command`), or an inverted numeric scale
is a loud, actionable error — never a silent default.

## Providers

The CLI can build any of onejudge's backends from `provider.kind`. Every model
call goes through oneharness:

- **`oneharness`** (default) — shell out to the `oneharness` CLI (0.3.20+) to drive
  a real harness (Claude Code, Codex, …). The agent side uses the discovered
  `oneharness.toml`; the judge side uses `judge_config` (`--config`). See
  [live-tier.md](live-tier.md).
- **`command`** — a custom backend speaking the [JSON-lines protocol](protocol.md).
- **`split`** — compose a skill-runner with a separate judge / simulated-user
  backend (e.g. drive the agent on one harness, judge on another).
