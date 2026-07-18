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

The Rust JSON Schemas generate the SDK's complete public type surface. Besides
`RunConfig` and `RunReport`, nested contracts such as `ProviderConfig`,
`EvalConfig`, `Transcript`, `Usage`, and `JudgeVerdict` are importable from
`onejudge_sdk`; `StreamEvent` describes the Rust streaming envelope.

There is no `run_stream` method. `onejudge run` currently emits one final JSON
report; the JSONL interface in `docs/protocol.md` is the internal provider
protocol, not a CLI result stream.
