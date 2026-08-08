"""Async subprocess client for onejudge's validated JSON interface."""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import tempfile
from collections.abc import Awaitable, Mapping, Sequence
from functools import cache
from pathlib import Path
from typing import Any, Callable, Optional, TypeVar, cast

from jsonschema import Draft202012Validator
from jsonschema.protocols import Validator

from ._errors import ContractError, OneJudgeProcessError, OneJudgeTimeoutError
from ._generated_types import RunConfig, RunReport, StreamEvent
from ._result import RunResult

_STREAM_LIMIT = 16 * 1024 * 1024

#: What a caller passes as ``on_event`` to watch a run while it is still running.
#: It is called once per tool event, in the order onejudge published them.
EventHandler = Callable[[StreamEvent], None]

_T = TypeVar("_T")


def _load_json(name: str) -> dict[str, Any]:
    path = Path(__file__).with_name("_generated") / name
    return cast("dict[str, Any]", json.loads(path.read_text(encoding="utf-8")))


_SCHEMAS = _load_json("schemas.json")
_INPUT_KEYS = cast("dict[str, dict[str, str]]", _load_json("input-keys.json"))


@cache
def _validator(root: str) -> Validator:
    schema = _SCHEMAS[root]
    Draft202012Validator.check_schema(schema)
    return Draft202012Validator(schema)


def _validate(root: str, value: Any, label: str) -> Any:
    errors = sorted(_validator(root).iter_errors(value), key=lambda error: list(error.path))
    if not errors:
        return value
    details = []
    for error in errors:
        path = ".".join(str(part) for part in error.absolute_path) or "<root>"
        details.append(f"{path}: {error.message}")
    raise ContractError(f"{label}: {'; '.join(details)}")


def _input(value: Any) -> dict[str, Any]:
    checked = cast("Mapping[str, Any]", _validate("run_config", value, "invalid onejudge config"))
    keys = _INPUT_KEYS["run_config"]
    return {keys.get(key, key): item for key, item in checked.items()}


async def _bounded(awaitable: Awaitable[_T], timeout: Optional[float]) -> _T:
    """Await `awaitable`, under `timeout` seconds when the caller set one."""
    if timeout is None:
        return await awaitable
    return await asyncio.wait_for(awaitable, timeout=timeout)


async def _stream_exchange(
    process: asyncio.subprocess.Process,
    task: str,
    on_event: EventHandler,
) -> tuple[Optional[RunReport], bytes]:
    """Read `onejudge run --stream`'s NDJSON, calling back per event as it arrives.

    The grammar is `event* result EOF`, and every line is validated against the
    same generated contract the buffered path uses — so an unreadable line, an
    envelope this SDK does not model, a stream that stops before its terminal
    `result` line, or anything written after that line is loud rather than a
    partial run that looks finished. stderr is drained concurrently: this reader
    holds stdout open for the whole run, and a filled stderr pipe would deadlock
    both.
    """
    stdin, stdout, stderr = process.stdin, process.stdout, process.stderr
    if stdin is None or stdout is None or stderr is None:  # pragma: no cover - all are PIPE
        raise ContractError("onejudge process pipes were not available")
    draining = asyncio.ensure_future(stderr.read())
    try:
        try:
            stdin.write(task.encode())
            await stdin.drain()
        except (BrokenPipeError, ConnectionResetError):  # pragma: no cover - OS race
            pass
        stdin.close()
        report = await _read_lines(stdout, on_event)
        errors = await draining
    except BaseException:
        # Every exceptional exit owns the same cleanup: a contract violation, a
        # handler that raised, or an outer cancellation must not leave the CLI
        # running or this drain task outliving the call. The original failure is
        # what the caller needs, so it is re-raised untouched afterwards.
        await _terminate(process)
        await _discard(draining)
        raise
    await process.wait()
    return report, errors


async def _discard(draining: asyncio.Future[bytes]) -> None:
    """Cancel the stderr drain and await it, so no task outlives this call.

    It is cancelled rather than read to completion because a grandchild that
    inherited the pipe can hold it open after the CLI itself is gone, and a
    cleanup path is the last place that may block indefinitely.
    """
    draining.cancel()
    try:
        await draining
    except asyncio.CancelledError:
        pass


async def _read_lines(
    stdout: asyncio.StreamReader,
    on_event: EventHandler,
) -> Optional[RunReport]:
    """Read the whole stream, enforcing its `event* result EOF` grammar."""
    report: Optional[RunReport] = None
    async for raw in stdout:
        line = raw.decode("utf-8", errors="replace").strip()
        if not line:
            continue
        if report is not None:
            # The terminal line ended the exchange, so a further event, a second
            # result, and an unmodelled envelope are the same violation. Ignoring
            # them would let a run that overran its own protocol look clean.
            raise ContractError(f"onejudge wrote a line after its terminal result line: {line}")
        report = _stream_line(line, on_event)
    return report


def _stream_line(line: str, on_event: EventHandler) -> Optional[RunReport]:
    """Dispatch one stream line, returning the report when it is the terminal one."""
    try:
        value = json.loads(line)
    except json.JSONDecodeError as error:
        raise ContractError(f"onejudge stream line was not valid JSON: {error}") from error
    if not isinstance(value, dict):
        raise ContractError("onejudge stream line was not a JSON object")
    kind = value.pop("type", None)
    if kind == "event":
        event = _validate("stream_event", value, "invalid onejudge stream event")
        on_event(cast("StreamEvent", event))
        return None
    if kind == "result":
        return cast(
            "RunReport",
            _validate("report", value.get("report"), "invalid onejudge report contract"),
        )
    raise ContractError(f"unrecognized onejudge stream line type {kind!r}")


async def _terminate(process: asyncio.subprocess.Process) -> None:
    if process.returncode is not None:
        return
    try:
        process.terminate()
    except ProcessLookupError:  # pragma: no cover - OS race after returncode check
        return
    try:
        await asyncio.wait_for(process.wait(), timeout=2)
    except asyncio.TimeoutError:  # pragma: no cover - defensive hard-kill fallback
        process.kill()
        await process.wait()


class OneJudge:
    """Validated async access to an installed onejudge CLI."""

    def __init__(
        self,
        *,
        executable: Optional[str] = None,
        executable_args: Sequence[str] = (),
        env: Optional[Mapping[str, str]] = None,
    ) -> None:
        self._executable = executable
        self._executable_args = tuple(executable_args)
        self._env = dict(env or {})

    def _command(self, args: Sequence[str], path: Optional[str] = None) -> tuple[str, ...]:
        command = self._executable or os.environ.get("ONEJUDGE_BIN")
        if command is None:
            command = shutil.which("onejudge", path=path) or "onejudge"
        return (command, *self._executable_args, *args)

    async def run(
        self,
        config: RunConfig,
        task: str,
        *,
        provider: Optional[str] = None,
        cwd: Optional[str] = None,
        env: Optional[Mapping[str, str]] = None,
        timeout: Optional[float] = None,
        on_event: Optional[EventHandler] = None,
    ) -> RunResult:
        """Run one task and return exit-faithful process and report data.

        Pass ``on_event`` to watch the run while it is still running: onejudge then
        publishes each tool event as it happens (`docs/streaming.md`) and this call
        invokes the handler per event before returning the same final result.
        """
        parsed = _input(config)
        if on_event is not None and not callable(on_event):
            raise ContractError("invalid onejudge on_event: expected a callable")
        if not isinstance(task, str):
            raise ContractError("invalid onejudge task: expected a string")
        if provider not in (None, "oneharness", "command", "split"):
            raise ContractError("invalid onejudge provider: expected oneharness, command, or split")
        if timeout is not None and (isinstance(timeout, bool) or timeout <= 0):
            raise ContractError("invalid onejudge timeout: expected a positive number")
        process_env = {**os.environ, **self._env, **dict(env or {})}
        for key, item in process_env.items():
            if not isinstance(key, str) or not key or "=" in key or "\0" in key:
                raise ContractError(f"invalid environment variable name: {key!r}")
            if not isinstance(item, str) or "\0" in item:
                raise ContractError(f"invalid environment variable {key!r}: expected a string")
        with tempfile.TemporaryDirectory(prefix="onejudge-python-") as directory:
            config_path = Path(directory) / "effective.onejudge.json"
            config_path.write_text(json.dumps(parsed), encoding="utf-8")
            args = ["run", str(config_path), "--task", "-", "--format", "json"]
            if on_event is not None:
                args.append("--stream")
            if provider is not None:
                args.extend(("--provider", provider))
            process = await asyncio.create_subprocess_exec(
                *self._command(args, process_env.get("PATH")),
                cwd=cwd,
                env=process_env,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                limit=_STREAM_LIMIT,
            )
            stdout_bytes = b""
            streamed: Optional[RunReport] = None
            try:
                if on_event is None:
                    stdout_bytes, stderr_bytes = await _bounded(
                        process.communicate(task.encode()), timeout
                    )
                else:
                    streamed, stderr_bytes = await _bounded(
                        _stream_exchange(process, task, on_event), timeout
                    )
            except asyncio.TimeoutError as error:
                await _terminate(process)
                raise OneJudgeTimeoutError(timeout or 0) from error
            except BaseException:
                await _terminate(process)
                raise
        stderr = stderr_bytes.decode("utf-8", errors="replace")
        returncode = process.returncode or 0
        if returncode not in (0, 1):
            raise OneJudgeProcessError(returncode, stderr)
        if on_event is not None:
            if streamed is None:
                raise ContractError("onejudge stream ended without a terminal result line")
            return RunResult(exit_code=returncode, stderr=stderr, raw=streamed)
        try:
            value = json.loads(stdout_bytes)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ContractError(f"onejudge returned invalid JSON: {error}") from error
        report = cast("RunReport", _validate("report", value, "invalid onejudge report contract"))
        return RunResult(exit_code=returncode, stderr=stderr, raw=report)
