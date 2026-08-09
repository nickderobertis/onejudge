# The streamed protocol

A onejudge turn can take 600–2000 seconds. Buffered, none of that is visible until
it ends: a supervising process cannot tell a working agent from a dead one, and a
UI has nothing to render. The streamed protocol makes the turn's tool activity a
first-class, incremental signal — **inbound** from a provider that publishes it,
and **outbound** from onejudge to its SDK consumers.

Both directions speak the same two NDJSON envelopes, one JSON object per line:

```jsonl
{"type":"event", …}
{"type":"event", …}
{"type":"result", "report":{…}}
```

The grammar is exactly:

```
event* result EOF
```

`event` lines carry live activity; the single `result` line ends the exchange and
carries the same document a buffered run produces. Every line names its own type,
so a consumer never has to guess a line's meaning from its position — but the
`result` line really is terminal, and **anything after it is a violation**.

## Inbound: a streamed provider

Set `provider.stream: true` under `kind: oneharness` (or
[`OneharnessProvider::with_streaming`] in Rust). The agent-side call then adds
`oneharness run --stream`, and onejudge reads that process's stdout line by line
instead of as one document.

```yaml
provider:
  kind: oneharness
  bin: ./scripts/oneharness-agent.sh   # any binary that speaks the protocol
  stream: true
```

| line | shape | meaning |
|------|-------|---------|
| event | `{"type":"event","event":{"kind":…,"name":…,"input":…,"index":…}}` | one normalized tool event, published the instant it is observed |
| result | `{"type":"result","report":{…}}` | the terminal line; `report` is the `oneharness run` JSON report, parsed exactly as a bare report is |

The `event` object is a [`ToolEvent`](contract.md) — the same normalized shape the
buffered report's `events` array carries. onejudge delivers each one to the caller
immediately; the finished turn's events still come from the terminal report, so a
streamed turn and a buffered one produce an identical transcript.

**What is not the protocol is a loud error.** A provider that declared streaming
and then writes a line onejudge cannot model fails the run with a classified
`ProviderErrorKind::Protocol` error naming the line — never a swallowed event or a
vacuously empty turn:

- a line that is not valid JSON, or is JSON but not an object;
- a `type` other than `event` / `result`, or a non-string `type`;
- an `event` line with no `event` object, or one that is not a tool event;
- a `result` line with no `report` object;
- a stream that ends before its `result` line;
- **any content after the `result` line** — a further event, a second result, or an
  envelope type this build does not model. onejudge reads on to EOF rather than
  stopping at the terminal line, so the trailing bytes are checked, not swallowed;
  a run that overran its own protocol must not look clean. (Trailing blank lines
  are fine — that is just how a writer ends its output.)

**One deliberate tolerance:** a line with *no* `type` at all is taken as the bare
report a run writes when it did not stream. That is what makes `stream: true` safe
for a wrapper that streams only when the underlying harness can — a degraded run
answers with one buffered document and is not a failed run. It is terminal on the
same terms: nothing may follow it either.

A provider that never streams needs no changes at all: leave `stream` unset and
onejudge parses one report document, exactly as before. Streaming is only ever
declared for the agent side; the judge / simulated-user calls stay buffered,
because they are short and single-shot.

## Outbound: `onejudge run --stream`

`onejudge run --format json --stream` republishes the run on **stdout** in the same
two envelopes, so an SDK can watch it live:

```console
$ onejudge run onejudge.yaml --format json --stream
{"type":"event","turn":1,"event":{"kind":"tool_call","name":"bash","input":{"command":"git commit -m fix"},"index":0}}
{"type":"result","report":{"schema_version":7,…}}
```

The outbound `event` line adds `turn` — the 1-based assistant-turn index within the
run, which the provider upstream has no notion of. `report` is byte-for-byte the
versioned [`Report`](contract.md) a buffered `--format json` run prints, so a
consumer parses one contract either way, and the exit code is unchanged (`0`
completed and every boolean eval passed, `1` otherwise, `2` a bad config or a
provider failure).

Each line is flushed as it is written. `--stream` requires `--format json` and
refuses `--output` — the stream *is* stdout — and either misuse is a loud config
error (exit 2) rather than a silently discarded stream.

A run that **fails** publishes no terminal line (there is no report), and stdout
keeps exactly this grammar: no third envelope type was invented for it. The
structured failure — the classified error plus the harness attribution
([contract.md](contract.md#when-a-run-fails)) — is written to **stderr** as one
compact JSON line, which the Python SDK parses onto
`OneJudgeProcessError.failure`.

Outbound streaming does **not** require an inbound streamed provider. A buffered
provider satisfies the same interface by replaying its finished turn's events, so
`--stream` always produces well-formed output; only *when* the events arrive
differs.

## Consuming it

**Rust** — `Engine::run_streaming` (or `cli::run_plan_streaming`) takes a sink that
receives each `StreamEvent` as it arrives. Returning `ControlFlow::Break`
short-circuits: onejudge tears down the in-flight turn and the outcome reports
`stopped_early`.

```rust
use onejudge::{Conversation, Engine, OneharnessProvider, Settings, Skill};
use std::ops::ControlFlow;

let provider = OneharnessProvider::new().with_streaming(true);
let engine = Engine::new(&provider, Settings::new());
let conversation = Conversation::single_turn(Skill::new("agent", ".", "Do the work."), "go");
let outcome = engine.run_streaming(&conversation, &mut |event| {
    println!("turn {}: {}", event.turn, event.event.summary());
    ControlFlow::Continue(())
})?;
```

**Python** — pass `on_event` to `OneJudge.run`. The SDK adds `--stream`, reads the
NDJSON as it arrives, validates each line against the generated contract, and calls
back per event; the terminal report becomes the ordinary `RunResult`.

```python
from onejudge_sdk import OneJudge, StreamEvent

def watch(event: StreamEvent) -> None:
    print(event["turn"], event["event"].get("name"))

result = await OneJudge().run(config, "do the work", on_event=watch)
```

The SDK enforces the same grammar outbound that onejudge enforces inbound: a line
it cannot model — bad JSON, an unknown `type`, a report that fails the contract, a
stream with no terminal line, or anything written after that line — raises
`ContractError`.

## Related

- [`docs/protocol.md`](protocol.md) — the `CommandProvider` JSON-lines protocol,
  which is a separate, one-request/one-response exchange and is unaffected by this.
- [`docs/contract.md`](contract.md) — the versioned `Report` the terminal line
  carries.
- [`docs/cli.md`](cli.md) — the rest of the `onejudge run` surface.
