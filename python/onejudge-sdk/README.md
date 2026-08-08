# onejudge

Typed async Python access to the `onejudge` CLI. The distribution is
`onejudge`, the import is `onejudge_sdk`, and each release depends on the
exact same `onejudge-cli` version.

```console
pip install onejudge
```

```python
import asyncio

from onejudge_sdk import OneJudge


async def main() -> None:
    result = await OneJudge().run(
        {"provider": {"kind": "oneharness"}},
        "Review this repository",
        cwd="/path/to/repository",
        timeout=3600,
    )
    print(result.completed, result.assistant_turns, result.verdicts)


asyncio.run(main())
```

`run` validates the config before starting the CLI, writes a temporary effective
JSON config (JSON is valid YAML), and always sends the task over stdin with
`--task -`. It accepts a `provider` override, subprocess `cwd`, additional `env`
(including `ONEHARNESS_HISTORY_LABELS` and `ONEHARNESS_TIMEOUT`), and a timeout.
Executable resolution is the constructor's `executable`, then `ONEJUDGE_BIN`,
then `onejudge` on `PATH`.

Exit 0 and 1 return `RunResult`: `exit_code` and `stderr` remain available, and
`raw`, `completed`, `verdicts`, `usage`, `assistant_turns`, and `agent_turns`
cover ai-orchestrator's dispatch needs. Exit 2 (bad config or provider/runtime
failure) and unexpected nonzero exits raise `OneJudgeProcessError` without
discarding the exit code or stderr. A caller timeout raises
`OneJudgeTimeoutError`.

## Watching a run as it happens

An agent turn can take 600–2000 seconds. Pass `on_event` to see its tool activity
while it is still running: the SDK runs the CLI under `--stream`, reads the NDJSON
as it arrives, and calls back once per event before returning the same
`RunResult`.

```python
from onejudge_sdk import OneJudge, StreamEvent


def watch(event: StreamEvent) -> None:
    print(event["turn"], event["event"].get("name"))


result = await OneJudge().run(config, "Review this repository", on_event=watch)
```

Every line is validated against the same generated contract the buffered path
uses, so an unreadable line, an envelope this SDK does not model, or a stream that
stops before its terminal `result` line raises `ContractError` — never a partial
run that looks finished. Events arrive live from a provider that streams
(`provider.stream: true`) and are replayed at the end of the turn from one that
does not, so `on_event` works either way. The protocol is documented in
`docs/streaming.md`.

The Rust JSON Schemas generate the SDK's complete public type surface. Besides
`RunConfig` and `RunReport`, nested contracts such as `ProviderConfig`,
`EvalConfig`, `Transcript`, `Usage`, and `JudgeVerdict` are importable from
`onejudge_sdk`; `StreamEvent` is one live event's payload and `EventHandler` the
`on_event` signature.

The JSONL interface in `docs/protocol.md` is a different thing: the internal
`CommandProvider` request/response protocol, not a CLI result stream.
