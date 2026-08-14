# onejudge ↔ oneharness: what goes through the library, and what still spawns

onejudge depends on **`oneharness-core`** (a published registry version, never a
git ref) and uses it for everything about the oneharness boundary that can be
expressed as a typed call — **including the invocation itself**. A turn is
`oneharness_core::io::run::run`, not a spawned `oneharness` process.

One seam still spawns, and this file records exactly why — so the decision is
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

## The invocation: one engine, two renderings

oneharness **0.7** moved the whole `run` verb into the library as
`oneharness_core::io::run::run(&RunRequest, RunControls<'_>) -> Result<RunOutcome,
OneharnessError>`, which returns the `RunReport` instead of printing it, publishes
normalized events to a caller-supplied `RunControls::events` sink *as they occur*,
and takes a `RunControls::cancel` token that tears each harness tree down through
the ordinary `Finish::Terminate` path. That is what onejudge now calls.

`RunControls::signal_cancel` stays **false**, and oneharness's own docs name this
exact split: the CLI sets it, "an embedder with its own signal handling leaves it
`false` and cancels `RunControls::cancel` itself". onejudge is an embedder — its
host owns the process's signal disposition.

**Every argv onejudge builds is accounted for on `RunRequest`.** One `TurnSpec`
(`crates/onejudge/src/oneharness/turn.rs`) describes a turn, and the two seams are
two renderings of it: `turn::request` for the in-process call, `turn::argv` for the
spawned one. `MAPPED_FLAGS`, in that module's tests, is the one source, and
`every_argv_flag_sets_the_run_request_field_the_mapping_pairs_it_with` reconciles
the three — the flags `argv` emits against the const, the const's fields against a
rendered `RunRequest` (by a predicate compiled against it, so a renamed or dropped
field fails the build), and both columns against the rows below.

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

## The one seam that still spawns, and why

`Execution::Process` (`crates/onejudge/src/oneharness/mod.rs`) spawns an
`oneharness` executable, and is reached **only** by naming one
(`OneharnessProvider::with_bin`) or by installing a `SpawnHook`. It exists for a
single upstream gap, verified against 0.8.0:

**`RunControls` cannot offer a spawned harness to the embedder.** Its whole
surface is `events`, `cancel`, `signal_cancel`, `version`. Every harness child is
spawned *inside* `run` through `oneharness_core::io::process::Process::spawn`
(`pub(crate)`), whose Unix path calls `setpgid(0, 0)` in `pre_exec` — each harness
is its own process-group leader by design. An in-process turn therefore has no
process to hand to an embedder's `SpawnHook`, which empties `Report::processes`,
`Plan::with_spawn_hook` and the group an embedder terminates
(`docs/spawn-hook.md`). That is a documented public contract on a versioned wire
(`SCHEMA_VERSION`), and the thing four e2e journeys exist to prove — including
`an_embedder_group_reaps_the_whole_two_party_harness_tree_on_a_kill_cancel`, which
is precisely the orphaned-paid-harness failure the seam was added for. So
installing a hook **selects the spawning seam** rather than silently doing
nothing, which would leave an embedder believing it owns an empty group.

**Proposal (oneharness):** a spawn observer on
`oneharness_core::io::run::RunControls`, offered each harness's `Command` before
it starts and its live `Child`/pid after, mirroring what
`oneharness_core::io::process::Tree::prepare` already does internally. With it,
this seam deletes and `Report::processes` is served in process.

## Driving the in-process seam deterministically

An earlier revision of this file claimed there was "no deterministic harness seam
an embedder can reach". **That was wrong**, and it is worth recording why, because
it is what kept the invocation a subprocess for two releases.

`RunRequest::mock_harness` is indeed unusable from outside — `io/run.rs` resolves
its responder with `std::env::current_exe()`, which for an embedder is the
*embedder's* binary. But that is not the only route. `RunRequest::bin` takes
`ID=PATH` overrides, and `[harness.<id>] bin` in an ordinary `oneharness.toml`
does the same through config discovery. Either points a harness id at any
executable, which is exactly a deterministic seam — the same one oneharness's own
`tests/library.rs` uses. What is not published is oneharness's *fixture*
(`oneharness-mock-harness`, behind its unpublished `mock-harness` feature), not
the mechanism.

So onejudge ships its own: `onejudge-fake-harness` (`src/bin/fake_harness.rs`), a
claude-code stand-in reached through `[harness.claude-code] bin`. Faking a
*harness* rather than faking `oneharness` means the whole of oneharness — harness
selection, argv construction, event normalization, streaming, cancellation,
teardown — is the real code under test, and only the model is faked. That is the
same discipline the other doubles follow, one layer deeper.

**Proposal (oneharness), still worth having:** publish the fixture (or expose the
responder from `oneharness-core` behind the existing `mock-harness` feature),
and/or let `RunRequest::mock_harness` name an explicit responder path instead of
`current_exe()`. It would save every embedder writing a per-harness fake.

A third gap is load-bearing for the *other* double rather than for the hop:
**`io::control::bind` is public but its result cannot be made to serve.** 0.8.0
late-binds a controlled turn's mechanism to the candidate serving it, and both
`ControlHandle::bind` and its `Binding` are `pub(crate)` — so a listener an
external crate binds has no mechanism behind it and refuses every interrupt
`no_active_turn`. `onejudge-fake-oneharness` therefore serves the frames itself,
still using oneharness's own `parse_request` / `ControlResponse` /
`interrupt_frame` / `prompt_frame`, so only the state machine is local.
**Proposal:** make the binding reachable (`pub fn bind(&self, Binding)` with a
`pub Binding`), or give `io::control::bind` a variant that starts bound.

## Cancelling a turn

A cancelled or malformed streamed turn must terminate the **harness**, not just
the party onejudge is talking to. onejudge can never signal a harness: every one
is its own process-group leader, so nothing onejudge sends reaches it. Only
oneharness owns the tree and can reap it.

**In process this is one thing, not three.** The sink's break returns
`SinkStep::Stop` and trips `RunControls::cancel`, and `run` tears each harness
tree down through `Finish::Terminate`. Either alone is sufficient — that is
measured, not assumed: reverting each in turn leaves
`cancelling_an_in_process_turn_terminates_the_harness_tree_oneharness_owns`
green, because they are independent paths into the same teardown. That e2e proves
the teardown itself, from outside the tree: the double spawns a descendant, goes
silent forever, and the test asserts the descendant stopped answering.

**The spawning seam still escalates through three rungs**, and each reaches a case
the one before it cannot. `terminate`
(`crates/onejudge/src/oneharness/mod.rs`) is that ladder, and it applies only
when `Execution::Process` is selected.

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
