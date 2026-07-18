"""Public Python SDK tests across real subprocess boundaries."""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path

from onejudge_sdk import (
    ContractError,
    OneJudge,
    OneJudgeProcessError,
    OneJudgeTimeoutError,
)

ROOT = Path(__file__).resolve().parents[3]
SUFFIX = ".exe" if os.name == "nt" else ""
BINARY = ROOT / "target" / "debug" / f"onejudge{SUFFIX}"
ECHO = ROOT / "target" / "debug" / f"onejudge-echo-provider{SUFFIX}"
FIXTURE = Path(__file__).with_name("fixture_cli.py")


def command_config(*, incomplete: bool = False) -> dict[str, object]:
    """Build a config that drives the real command-provider boundary."""
    config: dict[str, object] = {
        "provider": {"kind": "command", "command": [str(ECHO)]},
        "system_prompt": "work carefully [[event:git status]]",
        "evals": [{"criterion": "git status", "kind": "boolean"}],
    }
    if incomplete:
        config["user"] = {"persona": "tester", "max_turns": 1}
    return config


class OneJudgeTests(unittest.IsolatedAsyncioTestCase):
    """Exercise the public client through subprocesses."""

    async def test_real_cli_completed_and_incomplete_results(self) -> None:
        """Preserve both normal onejudge result exit codes and report fields."""
        directory = tempfile.mkdtemp(prefix="onejudge-sdk-cwd-")
        client = OneJudge(executable=str(BINARY), env={"ONEHARNESS_TIMEOUT": "41"})
        complete = await client.run(
            command_config(),
            "python sdk boundary",
            cwd=directory,
            env={"ONEHARNESS_HISTORY_LABELS": "graph=python"},
            provider="command",
            timeout=30,
        )
        self.assertEqual(complete.exit_code, 0)
        self.assertTrue(complete.completed)
        self.assertEqual(complete.assistant_turns, 1)
        self.assertEqual(complete.agent_turns, 1)
        self.assertEqual(complete.verdicts[0]["verdict"]["value"], True)
        self.assertGreater(complete.usage["input_tokens"], 0)
        self.assertEqual(complete.raw["schema_version"], 4)

        incomplete = await client.run(command_config(incomplete=True), "keep working")
        self.assertEqual(incomplete.exit_code, 1)
        self.assertFalse(incomplete.completed)
        self.assertEqual(incomplete.assistant_turns, 1)

    async def test_real_cli_runtime_error_keeps_exit_and_stderr(self) -> None:
        """Exit 2 remains distinguishable from an incomplete report."""
        config = {"provider": {"kind": "command", "command": ["missing-onejudge-provider"]}}
        with self.assertRaises(OneJudgeProcessError) as raised:
            await OneJudge(executable=str(BINARY)).run(config, "fail loudly")
        self.assertEqual(raised.exception.returncode, 2)
        self.assertIn("run failed", raised.exception.stderr)

    async def test_input_contract_rejects_bad_values_before_spawn(self) -> None:
        """Validate config, task, provider, and timeout at the Python boundary."""
        client = OneJudge(executable="definitely-not-started")
        for config, task, kwargs in (
            ({"unknown": True}, "task", {}),
            ({}, 7, {}),
            ({}, "task", {"provider": "other"}),
            ({}, "task", {"timeout": 0}),
            ({}, "task", {"timeout": True}),
            ({}, "task", {"env": {"BAD=NAME": "value"}}),
            ({}, "task", {"env": {"GOOD": "bad\0value"}}),
        ):
            with self.subTest(config=config, kwargs=kwargs), self.assertRaises(ContractError):
                await client.run(config, task, **kwargs)  # type: ignore[arg-type] - intentionally invalid inputs test runtime validation

    async def test_invalid_json_and_timeout_are_typed(self) -> None:
        """Malformed stdout and caller cancellation fail loudly."""
        invalid = OneJudge(
            executable=sys.executable,
            executable_args=(str(FIXTURE),),
            env={"ONEJUDGE_SDK_FIXTURE_MODE": "invalid-json"},
        )
        with self.assertRaises(ContractError):
            await invalid.run({}, "fixture task")

        slow = OneJudge(
            executable=sys.executable,
            executable_args=(str(FIXTURE),),
            env={"ONEJUDGE_SDK_FIXTURE_MODE": "timeout"},
        )
        with self.assertRaises(OneJudgeTimeoutError) as raised:
            await slow.run({}, "fixture task", timeout=0.01)
        self.assertEqual(raised.exception.returncode, -1)
        self.assertEqual(raised.exception.timeout, 0.01)

    async def test_environment_and_path_executable_resolution(self) -> None:
        """Honor ONEJUDGE_BIN, then PATH when no constructor executable is set."""
        config = command_config()
        previous = os.environ.get("ONEJUDGE_BIN")
        try:
            os.environ["ONEJUDGE_BIN"] = str(BINARY)
            result = await OneJudge().run(config, "environment binary")
            self.assertTrue(result.completed)
            del os.environ["ONEJUDGE_BIN"]
            path = os.pathsep.join((str(BINARY.parent), os.environ.get("PATH", "")))
            result = await OneJudge(env={"PATH": path}).run(config, "path binary")
            self.assertTrue(result.completed)
        finally:
            if previous is None:
                os.environ.pop("ONEJUDGE_BIN", None)
            else:
                os.environ["ONEJUDGE_BIN"] = previous


if __name__ == "__main__":
    unittest.main()
