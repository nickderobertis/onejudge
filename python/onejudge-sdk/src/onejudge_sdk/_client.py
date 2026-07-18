"""Async subprocess client for onejudge's validated JSON interface."""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import tempfile
from collections.abc import Mapping, Sequence
from functools import cache
from pathlib import Path
from typing import Any, Optional, cast

from jsonschema import Draft202012Validator
from jsonschema.protocols import Validator

from ._errors import ContractError, OneJudgeProcessError, OneJudgeTimeoutError
from ._generated_types import RunConfig, RunReport
from ._result import RunResult

_STREAM_LIMIT = 16 * 1024 * 1024


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
    ) -> RunResult:
        """Run one task and return exit-faithful process and report data."""
        parsed = _input(config)
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
            try:
                communication = process.communicate(task.encode())
                if timeout is None:
                    stdout_bytes, stderr_bytes = await communication
                else:
                    stdout_bytes, stderr_bytes = await asyncio.wait_for(
                        communication, timeout=timeout
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
        try:
            value = json.loads(stdout_bytes)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ContractError(f"onejudge returned invalid JSON: {error}") from error
        report = cast(
            "RunReport", _validate("report", value, "invalid onejudge report contract")
        )
        return RunResult(exit_code=returncode, stderr=stderr, raw=report)
