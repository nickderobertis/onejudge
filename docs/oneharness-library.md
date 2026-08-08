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

`oneharness run` is still spawned. Three separate reasons, each of which would
have to change upstream before the hop could collapse:

1. **There is no library entrypoint that returns a report.** The `oneharness`
   crate's only run surface is
   `oneharness::commands::run::run(&RunArgs) -> Result<i32, OneharnessError>`
   (0.6.9). It writes the report to the **process's** stdout via `print_json` and
   returns an exit code; `build_report` and every other per-verb function is
   private. An in-process caller cannot get the `RunReport` back without capturing
   global stdout — which onejudge cannot do, because its *own* stdout is a
   contract (`--format json`, and the `--stream` NDJSON protocol), and because a
   process-global redirect is not composable with concurrent callers.
2. **There is no event sink.** Under `--stream`, `stream_one_harness` writes each
   normalized event straight to `std::io::stdout()`. A library call therefore
   cannot deliver events to a caller *as they happen*, which is the entire point
   of `docs/streaming.md`. Collapsing the hop would have regressed live streaming
   from "visible while it runs" to "visible when it ends".
3. **Process supervision belongs to the process that owns the children.**
   oneharness's per-turn timeout, its cancellation path, and its termination of
   the harness's descendants (job objects on Windows, process groups on Unix, in
   `oneharness_core::io::process`) are implemented for *its* children. Keeping the
   `oneharness run` process boundary keeps that supervision, and keeps onejudge's
   own teardown a single `kill` of one child rather than a re-implementation of
   the same machinery.

What the conversion **did** buy on that boundary is that the failure no longer has
to be read out of stderr: oneharness writes its JSON report even when it exits
non-zero, so onejudge now parses that report first and only falls back to the exit
status and stderr when the output is not a report at all (a usage error, a rejected
`--session`).

## What would let the invocation move in-process

Any one of these, upstream in `oneharness`, would make the hop collapsible:

- a `pub fn run(args: &RunArgs) -> Result<RunReport, OneharnessError>` that
  returns the report instead of printing it, or
- an `impl Write` / event-sink parameter on the run path, so the caller chooses
  where the report and the streamed events go, or
- moving the per-verb orchestration (`commands::run`) into `oneharness-core`
  behind a non-printing API.

Until then this is the smallest hop that preserves the behaviours the split
exists to protect.

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
and still ends in `Finish::Terminate`. That is why onejudge floors at 0.6.9.

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
