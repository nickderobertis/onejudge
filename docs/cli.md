# The `onejudge` CLI

The same engine that *tests* a skill can *drive real work*: point it at a harness,
hand it a task, and let an LLM-driven **simulated user act as a supervisor** that
keeps the harness going — pushing back, asking for verification, re-prompting —
until a `done_when` condition holds or `max_turns` is hit. One supervisor harness
invocation after each ordinary nonterminal agent turn decides completion and, if
needed, returns the exact next user message. Its prompt contains compact
normalized tool summaries rather than raw dumps. Agent-side calls are recorded;
the worktree-inheriting judge may inspect full events on demand with `oneharness
history show <session>-skill --project <worktree> --format text`. Stateless final
boolean/numeric eval calls remain separate. The simulated reviewer
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

`run` reads `./onejudge.yaml` when no config path is given. Config resolves in
four tiers — **a flag beats the matching `ONEJUDGE_*` env var, which beats the
file, which beats the built-in default**:

| flag | `ONEJUDGE_*` env | overrides |
|------|------------------|-----------|
| `--judge-config` | `ONEJUDGE_JUDGE_CONFIG` | the judge/simulated-user oneharness `--config` file |
| `--skill` | `ONEJUDGE_SKILL` | a skill directory (with `SKILL.md`) whose body seeds the system prompt |
| `--system-prompt` | `ONEJUDGE_SYSTEM_PROMPT` | extra system-prompt text (prepended to the skill body) |
| `--task` (`-` = stdin) | `ONEJUDGE_TASK` | the task (the env value is always literal — no stdin) |
| `--persona` | `ONEJUDGE_PERSONA` | the simulated user's persona |
| `--done-when` | `ONEJUDGE_DONE_WHEN` | the completion condition |
| `--max-turns` | `ONEJUDGE_MAX_TURNS` | the assistant-turn cap |
| `--session` | `ONEJUDGE_SESSION` | the caller-owned session name |
| `--provider` | `ONEJUDGE_PROVIDER` | just the backend kind (`oneharness`/`command`/`split`) |
| `--format` | — | `human` (default) or `json` |
| `--output`, `-o` | — | write the result to a file instead of stdout |

Each `ONEJUDGE_*` variable is the flag name in upper-snake-case. An empty value
is treated as unset. Like the flags, they are validated at the boundary: a
non-integer `ONEJUDGE_MAX_TURNS` or an unknown `ONEJUDGE_PROVIDER` is a loud
error (exit 2), never a silent fallback. This mirrors oneharness's own
`ONEHARNESS_*` overrides — note the two prefixes are distinct: `ONEJUDGE_*`
configures the loop, `ONEHARNESS_*` configures the harness/model underneath it.

Supplying `--persona` / `--done-when` / `--max-turns` (by flag or env) implies a
simulated user even if the config had none.

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
| `provider` | which backend runs the harness: `kind` is `oneharness` (`bin`, `judge_config`), `command` (`command: [...]`), or `split` (a `skill:` + `judge:` **sub-provider** pair — distinct from the top-level `skill:` below) |
| `skill` | a skill directory (containing `SKILL.md`) whose body seeds the system prompt, resolved relative to the config file; optional |
| `system_prompt` | extra system-prompt text; used alone, or prepended before a `skill` body when both are set; optional |
| `task` | the task to drive to completion (or supply via `--task`) |
| `user` | the simulated supervisor: `persona`, `done_when`, `max_turns` (omit for a single-turn run) |
| `session` | the caller-owned session name threaded across turns |
| `evals` | optional criteria to score the finished transcript: each has a `criterion`, a `kind` (`boolean` / `numeric`), and — for numeric — a `scale: [min, max]` |
| `assessment` | optional prompt for one free-text judgement over the finished transcript and its tool actions |

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
