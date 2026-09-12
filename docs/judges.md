# The judge side is a list of judges

The supervisor that drives a run — the simulated user that decides, after each
worker turn, whether the work is done or what to do next — is a **panel of
judges**, not one provider. Every judge in the panel runs against the same
worker turn at the same time; the panel waits for all of them; and when any of
them says the work is not done, the worker is handed **one** combined, attributed
message and fixes everything in one turn. An LLM reviewer, a lint run and a
repository's own scripted checks each catch different things, and a panel puts
all of them on one worker at once, at the cost of the slowest judge rather than
the sum.

A panel of **one** judge — which is what every config written before panels
existed builds — is byte-identical to a bare provider: the same transcript, the
same session names, the same control addresses and usage. The only thing it
adds is the per-judge record below.

This page is the contract. Three parts: the config shape, how a panel decides,
and what each surface carries per judge. The library type is
[`JudgePanel`](../crates/onejudge/src/panel.rs), composed as the judge half of a
`SplitProvider`; the CLI builds one from `judges:`.

## The config shape

```yaml
provider:
  kind: split
  skill: {kind: oneharness, stream: true, control: true}   # the agent side, unchanged
  judges:                                   # one or more; exclusive with `judge:`
    - kind: oneharness                      # the LLM simulated user, exactly as today
      judge_config: oneharness.judge.toml
      label: reviewer                       # optional on any judge entry
    - kind: command                         # a custom script speaking docs/protocol.md
      command: [my-judge, --flag]
```

- **`judge:` is the one-element shorthand for `judges: [..]`.** Both spellings
  resolve into one representation (`ProviderSpec::Split { skill, judges }`, a
  `Vec<JudgeSpec { label, provider }>`) and there is exactly one judge-panel code
  path. Naming both, or an empty `judges:`, is a config error naming the field.
- **`label`** is `[A-Za-z0-9_-]+`, unique within the list, and refused anywhere
  but a judge entry. Absent, it is the entry's `kind` for the first judge of that
  kind and `<kind>-<n>` (n ≥ 2, counting judges of that kind in list order,
  labelled ones included) for repeats. A label — explicit or defaulted — that
  another judge already carries is refused.
- Every judge entry is an ordinary provider config, validated as one: the fields
  of its `kind` apply, and a field that does not belong to that kind is refused
  as it always was.
- A config whose `evals` include a numeric eval, or which names an
  `assessment`, while its judge list holds no judge that can score a number or
  write prose (an `oneharness` or `command` judge) is refused, because nothing in
  it can answer. Every kind this build can name can, so the refusal is in place
  for a kind that cannot.
- `--judge-config` / `ONEJUDGE_JUDGE_CONFIG` still apply to the top-level
  `provider.judge_config` only; there is no new flag or environment variable.

There is no per-judge `done_when`, `persona`, `max_turns` or note routing: those
are the conversation's and reach every judge alike. A judge that cannot use one
ignores it, and its documentation says so.

## How a panel decides

Every judge runs each judge-side operation **concurrently** — one OS thread per
judge, each handed the same query, transcript, notes and evidence — and the panel
returns only after **every** judge has returned. There is no early exit on the
first failure: a judge still running after another has failed is waited for, so
nothing is left running unseen and nothing a judge said is lost.

**`supervise`** (the per-turn decision), combining the judges' outcomes:

| the judges said | the panel returns |
|---|---|
| every judge *completed* | `Completed { reason }` — each judge's reason prefixed `[<label>] `, joined with `; ` |
| at least one *continued* | `Continue { message, reason }` — `message` is, for each judge that did **not** complete, in list order, the header ``## Judge `<label>` (<kind>)``, a blank line, then that judge's own message verbatim; a judge with no usable instruction (`no_instruction` / `unparseable`) contributes its header and one line saying it judged the work incomplete but gave no usable instruction, with its reason. Blocks are separated by one blank line. A judge that completed contributes nothing to `message`. `reason` is attributed as above |
| no judge continued, at least one had *no instruction* | `NoInstruction { reason }`, attributed |
| every non-completed judge was *unparseable* | `Unparseable { problem, answer }`, both attributed over those judges |
| any judge returned an **error** | once every judge has returned, `Error::Provider { context: "supervise[<label>]", message, kind }` — `kind` preserved from that judge's own error, `message` naming, per judge, what it decided or how it failed. That fails the run exactly as a judge transport failure always has: the work stays committed, and the worker is never handed the error as an instruction. **A judge that could not run is never read as a pass.** |

Headers and `[<label>]` prefixes appear only when the panel holds **more than
one** judge; a panel of one hands its judge's outcome through verbatim, which is
what keeps a single-judge configuration's transcript byte-identical to what it
was. Each judge handles its own re-asks (`SUPERVISOR_REASK_LIMIT`) before the
panel sees its outcome.

The other judge-side operations:

- **Boolean `judge`**: every judge; `value` is the conjunction; `reason`
  attributed. **Numeric `judge`**: every judge that can score a number; `value`
  is the **minimum**; `reason` attributed.
- **`assess`**: every judge that can write prose, each text under its `## Judge`
  header, in list order.
- **`simulate_user`** (the legacy op the engine no longer calls): the first judge
  that can play a user.
- Usage is summed across judges. `supervisor_control` is the first judge's — a
  lever over another judge's turn is deferred.

**Sessions.** With more than one judge, judge *i*'s caller-owned session name is
`<user session>-<label>` (`run-42-user-reviewer`), so each judge keeps its own
harness session; a panel of one hands the bare `<user session>` through,
unchanged. The `session` a `command` judge sees on its supervisor request
([protocol.md](protocol.md)) is therefore the suffixed name.

## What each surface carries per judge

**`JudgeDecision { judge, kind, decision, reason }`** — `judge` is the label,
`kind` the entry's provider kind (`oneharness`, `command`, …), `decision` one of
`done` | `continue` | `no_instruction` | `unparseable` | `error`, and `reason`
the judge's own reason (the error's message, for `error`). The panel records one
per judge per supervisor call — the call that failed included — and exposes them
through `Provider::take_judge_decisions`, which the engine drains after every
supervisor call, `Ok` or `Err`. A provider that is not a panel records none, so a
report carrying no decisions was judged by a bare provider; nothing is
synthesized.

**Report** (`schema_version` 12): `judge_decisions: Vec<JudgedTurn>` with
`JudgedTurn { turn, decisions }`, one per supervisor turn, omitted when empty;
the `FailureReport` carries the same array, the turn that failed included.
`HarnessAttribution`, `SessionLink` and `SpawnedProcess` each gain
`judge: Option<String>` — the label, serialized only when set, and set only by a
panel of more than one judge. See [contract.md](contract.md).

**Observation** (the in-process stream): `Observation::JudgeDecided` (serialized
`type: judge_decided`, carrying `turn`, `judge`, `kind`, `decision`, `reason`) is
delivered once per decision in list order after the supervisor turn's
`TurnOpened` and before its `Message` / `TurnClosed` — and, when the supervisor
call failed, before the error propagates. An embedder matching `Observation`
exhaustively adds an arm.

**The `--stream` NDJSON protocol is unchanged** (`event* result EOF`); the
decisions reach an SDK on the `result` line's report ([streaming.md](streaming.md)).
The human `--format` prints each judge's decision beside the supervisor turn it
belongs to — under the assistant turn that turn judged, as
`[judge <label> (<kind>)] <decision> — <reason>`. The SDK schema bundle
(`onejudge schema`, `sdk_schema.rs`, the generated Python declarations) carries
every shape above.

## Composing one in the library

```rust
use onejudge::{CommandProvider, JudgeEntry, JudgePanel, OneharnessProvider, SplitProvider};

let panel = JudgePanel::new(vec![
    JudgeEntry::new("reviewer", "oneharness", OneharnessProvider::new()),
    JudgeEntry::new("checks", "command", CommandProvider::new(vec!["my-judge".into()])?),
])?;
let provider = SplitProvider::new(OneharnessProvider::new(), panel);
# Ok::<(), onejudge::Error>(())
```

`JudgePanel::new` enforces the same label rules as the config layer.
`JudgeEntry::with_abilities` declares which operations a judge takes part in
(numeric scoring, prose, playing a user) — every ability by default. An
embedder driving a `Plan` gets the panel built for it, and
`Plan::with_spawn_hook` reaches every judge of it.

## Proof

`tests/e2e.rs` drives a two-judge panel of the echo double through the real
engine over real subprocesses: every judge done, one continuing, several
continuing, one failing while another is still running (the panel waits, the
error names the judge, both decisions are on the record), two judges genuinely
overlapping in time (a wall-clock bound below the sum of their sleeps, and their
stamped intervals intersect), and a panel of one attributing nothing.
`tests/cli.rs` drives the same panel through the plan driver and the built
binary, and replays the checked-in single-judge baseline
(`tests/golden/single-judge/`, captured from the released 0.8.1 by
`scripts/capture-single-judge-baseline.sh`) to prove a panel of one is
byte-identical to what came before it. `panel.rs` unit-tests the decision table.
