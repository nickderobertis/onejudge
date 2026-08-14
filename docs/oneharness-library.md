# onejudge ↔ oneharness: what goes through the library, and what still spawns

onejudge depends on **`oneharness-core`** (a published registry version, never a
git ref) and uses it for everything about the oneharness boundary that can be
expressed as a typed call. The one thing that is still a subprocess is the
*invocation* itself, and this file records exactly why — so the decision is
revisitable when oneharness's library surface changes, rather than rediscovered.

## Through the library

| Concern | Where it now lives |
| --- | --- |
| The run report | `oneharness_core::domain::report::{RunReport, RunResult, Status}`, parsed directly — no shadow serde structs declared in onejudge |
| Failure taxonomy | `oneharness_core::domain::signals::FailureKind`, mapped **totally** onto `ProviderErrorKind`, so a new upstream kind is a compile error here |
| Fallback selection | `oneharness_core::domain::report::FallbackReport` — which candidate ran, which were routed around and why |
| The streamed NDJSON grammar | `oneharness_core::domain::report::RunStreamEnvelope` decides every tagged line |
| Normalized tool events | `oneharness_core::domain::events::ActionEvent` |
| Token / cost accounting | `oneharness_core::domain::signals::Usage` |
| The measured trace | `oneharness_core::domain::report::ExecutionTelemetry`, read off `RunResult::telemetry` — the invocation bounds and the model/tool/TTFT split, on the report itself since schema `0.5` |
| The per-candidate history record | `oneharness_core::io::history::read_session` — a real file read through oneharness's own reader, now only for `history_id` |
| The `--session` capability marker | drift-gated against `oneharness_core::errors::OneharnessError::SessionUnsupported`'s own rendering |

The e2e double (`onejudge-fake-oneharness`) **builds its report and its history
lines from those same types and serializes them**, so the suite feeds the real
reader the document oneharness's own contract produces.

## Still a subprocess, and why

`oneharness run` is still spawned — but **not** for the reasons this file used to
give. oneharness **0.7** moved the whole `run` verb into the library as
`oneharness_core::io::run::run(&RunRequest, RunControls<'_>) -> Result<RunOutcome,
OneharnessError>`, which returns the `RunReport` instead of printing it, publishes
normalized events to a caller-supplied `RunControls::events` sink *as they occur*,
and takes a `RunControls::cancel` token that tears each harness tree down through
the ordinary `Finish::Terminate` path. All three of the old objections are gone.
`RunControls::signal_cancel` even names this exact split: the CLI sets it, "an
embedder with its own signal handling leaves it `false` and cancels
`RunControls::cancel` itself".

**Every argv onejudge builds is accounted for on `RunRequest`.** The `run` verb
itself becomes the `io::run::run` call; each flag either builder emits
(`respond_args`, `judge_side_args`, in `crates/onejudge/src/oneharness/mod.rs`)
maps as below. `MAPPED_FLAGS`, beside those builders, is the one source, and
`every_argv_the_provider_builds_is_accounted_for_in_the_run_request_mapping`
reconciles the three: the flags the builders emit against the const, and **both
columns** of the const against the rows below.

| argv | `RunRequest` field |
| --- | --- |
| `--events` | `events: bool` |
| `--history` | `history: Option<bool>` |
| `--history-name` | `history_name: Option<String>` |
| `--system` | `system: Option<String>` |
| `--cwd` | `cwd: Option<PathBuf>` |
| `--config` | `config: Option<PathBuf>` |
| `--session` | `session: Option<String>` |
| `--stream` | `stream: Option<bool>` |
| `--control` | `control: bool` |
| `--prompt-file` | `prompt: Vec<String>` — an owned value, so the `-`/stdin hop that exists only to dodge the OS argv ceiling disappears; oneharness's own `LARGE_INPUT_THRESHOLD` moves a large prompt off-argv for the harness |
| `--compact` | **none, deliberately** — `RunRequest`'s own docs exclude it as "about how the shell *prints* the report, not how the engine produces it". An in-process caller is handed the `RunReport` value, so there is nothing to compact. Not a gap. |

## What still blocks the hop

Two upstream gaps, both verified against 0.8.0. Neither is worked around here —
a residual subprocess that nothing exercises is how the two versions drifted
apart in the first place.

1. **`RunControls` cannot offer a spawned harness to the embedder.** Its whole
   surface is `events`, `cancel`, `signal_cancel`, `version`. Every harness child
   is spawned *inside* `run` through `oneharness_core::io::process::Process::spawn`
   (`pub(crate)`), whose Unix path calls `setpgid(0, 0)` in `pre_exec` — each
   harness is its own process-group leader by design. Moving the invocation
   in-process therefore removes the last process onejudge can hand to an
   embedder's `SpawnHook`, and with it `Report::processes`, `Plan::with_spawn_hook`
   and the group an embedder terminates (`docs/spawn-hook.md`). That is a
   documented public contract on a versioned wire (`SCHEMA_VERSION`), and the
   thing four e2e journeys exist to prove — including
   `an_embedder_group_reaps_the_whole_two_party_harness_tree_on_a_kill_cancel`,
   which is precisely the orphaned-paid-harness failure the seam was added for.
   **Proposal:** a spawn observer on `RunControls`, offered each harness's
   `Command` before it starts and its live `Child`/pid after, mirroring what
   `io::process::Tree::prepare` already does internally.

2. **There is no deterministic harness seam an embedder can reach.** oneharness's
   own `tests/library.rs` drives `run` hermetically by pointing `RunRequest::bin`
   (`ID=PATH`) at `oneharness-mock-harness` — a `[[bin]]` behind oneharness's
   `mock-harness` feature that is never published. The other candidate,
   `RunRequest::mock_harness`, is unusable from outside: `io/run.rs` resolves the
   responder with `std::env::current_exe()`, which for an embedder is the
   *embedder's* binary, not `oneharness`. So an embedder either re-implements a
   per-harness fake CLI or spends a paid harness turn.
   **Proposal:** publish the fixture (or expose the responder from `oneharness-core`
   behind the existing `mock-harness` feature), and/or let `RunRequest::mock_harness`
   name an explicit responder path instead of `current_exe()`.

A third gap is already load-bearing here, though it blocks the test double rather
than the hop: **`io::control::bind` is public but its result cannot be made to
serve.** 0.8.0 late-binds a controlled turn's mechanism to the candidate serving
it, and both `ControlHandle::bind` and its `Binding` are `pub(crate)` — so a
listener an external crate binds has no mechanism behind it and refuses every
interrupt `no_active_turn`. `onejudge-fake-oneharness` therefore serves the frames
itself, still using oneharness's own `parse_request` / `ControlResponse` /
`interrupt_frame` / `prompt_frame`, so only the state machine is local.
**Proposal:** make the binding reachable (`pub fn bind(&self, Binding)` with a
`pub Binding`), or give `io::control::bind` a variant that starts bound.

## Cancellation: close the stream, then signal, then kill

A cancelled or malformed streamed turn must terminate the **harness**, not just
the `oneharness` process onejudge spawned. onejudge cannot signal the harness
directly: every harness is its own process-group leader (below), so nothing
onejudge sends reaches it. Every rung below is therefore addressed to oneharness,
which owns the tree and is the only party that can reap it. `terminate`
(`crates/onejudge/src/oneharness/mod.rs`) escalates through three, because each
reaches a case the one before it cannot.

**1 — close stdout.** `oneharness run --stream` writes each event to stdout, and a
failed write is its documented short-circuit: `stream_one_harness` returns
`StreamStep::Stop`, `run_job_streaming` ends the run as `StreamEnd::Stopped`, and
that maps to `Finish::Terminate` → `Tree::terminate`, which SIGTERMs then SIGKILLs
the harness's *own* process group. `run_streamed` drops the stdout reader first and
waits `PIPE_CLOSE_GRACE` for oneharness to take the hint. Killing instead — which
is what onejudge used to do — denied oneharness that teardown and orphaned the
harness, still burning tokens.

**2 — SIGTERM.** A broken pipe is only observable on the *next* write, so a
producer whose harness has gone **silent** never observes rung 1 at all. This was
a real, reported gap: such a run sat out the grace and took the backstop kill,
which being uncatchable denied it the teardown and orphaned a live harness after
every cancel. oneharness **v0.6.9** closed it — `commands::run` calls
`io::cancel::install_signal_cancel`, and `run_job_streaming_cancellable` polls
`cancellation_requested` on its own `CANCEL_POLL_SLICE` rather than only on
`PipeEvent::Data`, so the cancellation is noticed while the harness says nothing
and still ends in `Finish::Terminate`. That is what first moved onejudge's floor
off 0.6.8; the floor today is the pinned `oneharness-core` (`MIN_ONEHARNESS`).

Rung 1 is kept, and kept first, precisely because rung 2 is not free: SIGTERM's
*default* disposition is to terminate, so signalling a producer that would have
torn down on the broken pipe — an older oneharness, or the window before it
installs its handlers — cuts it off mid-teardown. (That is not hypothetical: it is
what the rung-1 e2e test caught when the signal was tried first.) Waiting out
`PIPE_CLOSE_GRACE` costs a silent harness a quarter second against a turn measured
in hundreds of seconds.

**3 — SIGKILL**, for a child that answers neither. It is also all Windows needs:
there, each harness tree is a Job Object with `KILL_ON_JOB_CLOSE`, so ending the
child already ends its descendants — which is why rung 2 is Unix-only.

Two e2e tests gate this pair, and each fails without its own rung. Both spawn a
harness stand-in in its own process group, publish that process's pid and a
liveness port, and prove from outside the tree that it stopped answering:

- `cancelling_a_streamed_turn_terminates_the_harness_oneharness_spawned` — the
  double tears the stand-in down only when its own stdout breaks. Reverting rung 1
  (killing outright) fails it in ~10s, naming the surviving pid.
- `cancelling_a_turn_terminates_a_harness_that_produces_no_output` — the double
  emits one event, then **never touches stdout again**, and tears the stand-in down
  only on SIGTERM, exactly as a real silent harness leaves oneharness. Dropping
  rung 2 fails it the same way.

## Why onejudge does not own the `oneharness` process as a group

The obvious-looking alternative — spawn `oneharness` into a process group / Job
Object (`command-group`) so a kill reaches its descendants — was evaluated and
**deliberately rejected**. Do not re-add it without changing one of these two
facts:

- **On Unix it cannot reach the descendants that matter.** Every harness
  oneharness runs is spawned through `oneharness_core::io::process::Process::spawn`,
  whose Unix `Tree::prepare` calls `setpgid(0, 0)` in `pre_exec` — each harness is
  its own group leader *by design*, so it is not a member of any group onejudge
  could signal. A group kill from here would terminate exactly the one process a
  plain `kill` already terminates. (On Windows it would reach them, since a
  descendant stays in the outer Job Object as well as oneharness's own. The gain
  is Windows-only; the cost below is not.)
- **It would break the cancellation that does work.** A child in its own process
  group no longer receives the terminal's `SIGINT`, nor a signal a parent sends
  to onejudge's group. Today Ctrl-C on `onejudge run`, and a parent tearing down
  onejudge's group in the `onepipeline → oneagentgraph → onejudge` chain, both
  reach `oneharness` because it shares onejudge's group. Detaching it would leave
  a live `oneharness` — and its harness — behind on the most common cancel path.

Neither would it have bought the descendant termination above, which comes from
closing the stream and signalling, not from the shape of the kill.

## The measurements: on the report, not re-read from history

`oneharness run`'s report used to omit the invocation's measurements —
`RunResult::telemetry` was `#[serde(skip)]`, so `model_ms`, `tool_ms`,
`time_to_first_token_ms` and the invocation bounds lived only on the history
record, and onejudge re-opened the session file the run had just written to
populate its own `telemetry`.

Report schema **`0.5`** (oneharness v0.6.9) serializes `ExecutionTelemetry` on the
result, so onejudge reads them off the run it just made
(`crates/onejudge/src/oneharness/report.rs::measured`). The variant is read for
exactly what it claims: `PartialInvocation` contributes its start bound and no
split, and `StdoutObserved` is deliberately **not** read as `tool_ms` — upstream
keeps observed and provider-measured tool time in separate history fields because
they are different quantities, and flattening them would report a guess as a
measurement.

What still needs the history file is `history_id` alone: it names a record in
oneharness's own store, so it is only knowable by reading that store. The e2e
double writes deliberately *different* measurements into its history record, so
`the_per_candidate_history_record_is_read_back_through_oneharnesss_own_reader`
fails loudly if a build ever re-reads the file for numbers the report already has.
Those sentinels still have to be a coherent record — oneharness validates its own
run lines on read (`model_ms + tool_ms <= duration_ms`) and silently drops one that
is not, taking the `history_id` with it.
