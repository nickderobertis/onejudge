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
| The per-candidate history record | `oneharness_core::io::history::read_session` — a real file read through oneharness's own reader |
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
   (0.6.8). It writes the report to the **process's** stdout via `print_json` and
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

## Known gap worth reporting upstream

`oneharness run`'s report does **not** carry the invocation's measurements —
`model_ms`, `tool_ms`, `time_to_first_token_ms`, the UTC invocation bounds, or the
record id. `RunResult::telemetry` is `#[serde(skip)]` and the values live only on
the history record. onejudge therefore reads the history session file back after
each invocation (`crates/onejudge/src/oneharness/history.rs`) to populate its own
`telemetry`. That works and is tested, but it is a second read of state the run
already had in hand; surfacing `ExecutionTelemetry` on `RunResult` would remove it.
