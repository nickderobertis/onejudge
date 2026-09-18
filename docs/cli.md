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
oneharness **0.14.0+**): it runs `oneharness init oneharness.toml` and `oneharness
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
| `--artifact` (repeatable) | `ONEJUDGE_ARTIFACTS` (separated like `PATH`: `:` on unix, `;` on Windows) | replaces `user.artifacts`, the files/directories the judge side reads directly |
| `--session` | `ONEJUDGE_SESSION` | the caller-owned session name |
| `--provider` | `ONEJUDGE_PROVIDER` | just the backend kind (`oneharness`/`command`/`split`; `llmlint` is a judge entry only and is refused here) |
| `--format` | — | `human` (default) or `json` |
| `--stream` | — | publish the run on stdout as the [streamed protocol](streaming.md) (needs `--format json`, refuses `--output`) |
| `--output`, `-o` | — | write the result to a file instead of stdout |

Each `ONEJUDGE_*` variable is the flag name in upper-snake-case, except that the
repeatable `--artifact` takes its whole list from the plural `ONEJUDGE_ARTIFACTS`.
An empty value
is treated as unset. Like the flags, they are validated at the boundary: a
non-integer `ONEJUDGE_MAX_TURNS` or an unknown `ONEJUDGE_PROVIDER` is a loud
error (exit 2), never a silent fallback. This mirrors oneharness's own
`ONEHARNESS_*` overrides — note the two prefixes are distinct: `ONEJUDGE_*`
configures the loop, `ONEHARNESS_*` configures the harness/model underneath it.

Supplying `--persona` / `--done-when` / `--max-turns` / `--artifact` (by flag or
env) implies a simulated user even if the config had none.

## Output and exit code

- **Human (default):** the conversation (with each turn's tool actions, and —
  under a judge panel — each judge's decision beside the supervisor turn it
  belongs to), the completion status (completed / hit the turn cap / settled —
  see below), usage, and any eval verdicts.
  Live tool events stream to **stderr** so a redirected stdout stays clean.
- **`--format json`:** the versioned [`Report`](contract.md) — transcript +
  verdicts + usage, stamped with `schema_version`. This reuses onejudge's existing
  wire contract; it is not a new one.
- **`--format json --stream`:** the same report, preceded by one NDJSON
  `{"type":"event",…}` line per tool event **as it happens** — so a consumer can
  watch a 600–2000 second turn instead of waiting it out. See
  [streaming.md](streaming.md).
- **A run that fails under `--format json`:** a versioned
  [`FailureReport`](contract.md#when-a-run-fails) where the report would have gone
  — the classified error plus the telemetry the run had recorded, including which
  harness identities were attempted and why each was refused. Under `--stream` it
  goes to **stderr** as one compact JSON line instead, so stdout stays exactly the
  `event* result EOF` protocol. The human format is unchanged (stderr text).

Every judged run reports, under `telemetry.attribution`, which harness identities
each invocation attempted, on which **side** (agent vs judge), which one ran, and
which a fallback chain routed around and why — so a failure is attributable to a
side and an identity without parsing a message.

It also reports, under `processes`, every OS process the run spawned — side, op,
program, and pid — plus the group an in-process embedder's
[`SpawnHook`](spawn-hook.md) placed it in. The CLI installs no hook, so its
records carry no `group`: onejudge never names a group it did not observe. An
embedder driving the same run **in-process** installs one on the plan
(`Plan::with_spawn_hook`), and the groups it names then appear in exactly these
records. The same array rides on the `FailureReport`, so a caller cleaning up
after a failed run can still name what was created.

The **exit code** is `0` only when the task **completed** and every **boolean**
eval passed. A run that hits `max_turns` without satisfying `done_when`, or whose
boolean eval fails, exits `1`. Numeric evals are score-and-report — they never
fail the run (there is no threshold to fail against). A bad config / usage error
exits `2`.

Completion is decided by **re-judging `done_when` against the final transcript**
(the loop's own mid-run check can be preempted by the turn cap), so the exit code
reflects whether the task actually finished. Without a `done_when`, a run is
"completed" when the loop ended before the cap (the agent declared done, the user
stopped, or a single-turn run answered once) — with one exception: a **settled**
run. That is a run whose loop ended on the work it already had: because the
supervisor judged the work incomplete and then named no next instruction to act on
(even asked again), because the supervisor answered in neither documented shape
(even asked again with the shape restated), or because the exchanges stopped
moving — consecutive turns that recorded no tool activity and gave the same tiny
answer to the same tiny instruction. Either way it keeps the work it produced but
is reported `incomplete — <reason>` and exits `1`, and the reason is on the report
as `settled_reason` ([contract.md](contract.md)).

## Config

The authoritative, annotated config is what **`onejudge schema`** prints (and what
`onejudge init` writes) — a single tested source (`starter.yaml`), so this page
describes the fields rather than restating the YAML that would then drift from it.

Top-level keys:

| key | purpose |
|-----|---------|
| `provider` | which backend runs the harness: `kind` is `oneharness` (`bin`, `judge_config`, `stream`, `control`, `mock_harness`), `command` (`command: [...]`), or `split` (a `skill:` **sub-provider** — distinct from the top-level `skill:` below — plus the judge side as a **list**: `judges: [..]`, one or more entries each with an optional `label`, or `judge:` as the one-element shorthand; see [judges.md](judges.md)). A judge entry may also be `llmlint` (`bin`, `config`, `diff_base`, `args`) — a judge that is one `llmlint` run over the worker's tree, valid nowhere but a judge entry |
| `skill` | a skill directory (containing `SKILL.md`) whose body seeds the system prompt, resolved relative to the config file; optional |
| `system_prompt` | extra system-prompt text; used alone, or prepended before a `skill` body when both are set; optional |
| `task` | the task to drive to completion (or supply via `--task`) |
| `user` | the simulated supervisor: `persona`, `done_when`, `max_turns`, `settle_on_noop`, `artifacts` (omit for a single-turn run) |

`user.artifacts` names the work a judge should read directly when `git_status` and
`git_diff` cannot show it — a design document under a gitignored `.plans/`, say.
Each entry is a file or directory; an absolute path is used as written and a
relative one resolves against the skill's working directory. Every judge-side
prompt (supervisor, eval judge, assessment) then lists each resolved path, says
the artifacts may be untracked or gitignored and are read with the file-reading
tools, names an entry that does not exist as not existing (the run continues),
and lists a directory's files newest-modified first — at most 50, with a line
counting the rest. The listing is re-read on every judge-side turn. Empty (the
default) leaves every prompt unchanged; the read-only tool allowlist and the
`git_status` / `git_diff` requests are the same either way.
| `session` | the caller-owned session name threaded across turns |
| `evals` | optional criteria to score the finished transcript: each has a `criterion`, a `kind` (`boolean` / `numeric`), and — for numeric — a `scale: [min, max]` |
| `assessment` | optional prompt for one free-text judgement over the finished transcript and its tool actions |

There is no `harness` / `model` / `judge_model` key: harness and model selection
moved into oneharness's own config files (`oneharness.toml` for the agent,
`provider.judge_config` — default `oneharness.judge.toml` — for the judge side).

The config is validated strictly at the boundary (`deny_unknown_fields`): a typo'd
key, a missing task, a provider field that does not belong to the chosen `kind`
(e.g. `bin` or `judge_config` under `kind: command`), a `label` outside a judge
entry or one that is malformed or repeated, `judge:` beside `judges:` or an empty
`judges:`, `kind: llmlint` anywhere but a judge entry, a numeric eval or an
`assessment` with no judge that can answer it, or an inverted numeric scale is a
loud, actionable error — never a silent default. An `llmlint` judge's executable
is probed when the run is built, so an absent one is a config error (exit 2)
before any turn.

## Providers

The CLI can build any of onejudge's backends from `provider.kind`. Every model
call goes through oneharness:

- **`oneharness`** (default) — shell out to the `oneharness` CLI (0.14.0+) to drive
  a real harness (Claude Code, Codex, …). The agent side uses the discovered
  `oneharness.toml`; the judge side uses `judge_config` (`--config`). Set
  `stream: true` when that binary publishes its agent turn as the
  [streamed protocol](streaming.md), and `control: true` to open the out-of-band
  [turn-control socket](control.md) an `oneharness interrupt` can redirect the
  agent turn through. See [live-tier.md](live-tier.md).

  `mock_harness: [<id>, …]` runs those harness ids against **oneharness's own
  deterministic `MOCK_*` responder** instead of a paid model — the free way to
  prove a real multi-turn, multi-identity chain, since everything but the model is
  the real oneharness. It applies to both sides of the conversation, so each id
  must be one that side's config selects (`kind: split` when they differ), and it
  makes the run **spawn** `bin`: the responder is oneharness's own binary
  re-executed, which an in-process run has none of
  ([oneharness-library.md](oneharness-library.md)). Script it with the `MOCK_*`
  variables in the environment the run inherits.
- **`command`** — a custom backend speaking the [JSON-lines protocol](protocol.md).
- **`llmlint`** — a **judge entry only**: one `llmlint lint` run over the worker's
  tree per decision, met at the process boundary (no llmlint crate is linked;
  `llmlint` must be installed). Failing rules send the worker llmlint's own
  report as its next turn; a clean run passes it; a run that could not complete
  is an error, never a verdict. `bin` (default `llmlint`), `config` (`-c`),
  `diff_base` (`--diff --diff-base`, the host's own comparison base — onejudge
  detects none) and `args`. Refused as the top-level provider, under `skill:`,
  and as `--provider`; left out of numeric evals and assessments.
  [judges.md](judges.md#the-llmlint-judge) is the contract.
- **`split`** — compose a skill-runner with a separate judge side (e.g. drive
  the agent on one harness, judge on another). The judge side is a **list of
  judges** — `judges:` — every one run concurrently against each worker turn and
  combined into one attributed answer, each judge's decision recorded on the
  report's `judge_decisions` and printed beside its turn in the human format;
  `judge:` is the one-element shorthand. [judges.md](judges.md) is the contract.
