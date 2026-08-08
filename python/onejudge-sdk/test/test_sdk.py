"""Public Python SDK tests across real subprocess boundaries."""

from __future__ import annotations

import asyncio
import os
import sys
import tempfile
import time
import unittest
from pathlib import Path

from onejudge_sdk import (
    ContractError,
    EventHandler,
    OneJudge,
    OneJudgeProcessError,
    OneJudgeTimeoutError,
    RunConfig,
    StreamEvent,
)


class _HandlerFailure(Exception):
    """Raised by an `on_event` handler, to prove it reaches the caller intact."""


ROOT = Path(__file__).resolve().parents[3]
SUFFIX = ".exe" if os.name == "nt" else ""
BINARY = ROOT / "target" / "debug" / f"onejudge{SUFFIX}"
ECHO = ROOT / "target" / "debug" / f"onejudge-echo-provider{SUFFIX}"
FIXTURE = Path(__file__).with_name("fixture_cli.py")


def command_config(*, incomplete: bool = False) -> RunConfig:
    """Build a config that drives the real command-provider boundary."""
    config: RunConfig = {
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
        input_tokens = complete.usage["input_tokens"]
        self.assertIsNotNone(input_tokens)
        self.assertGreater(input_tokens or 0, 0)
        self.assertEqual(complete.raw["schema_version"], 6)
        self.assertIsNone(complete.telemetry)

        incomplete = await client.run(command_config(incomplete=True), "keep working")
        self.assertEqual(incomplete.exit_code, 1)
        self.assertFalse(incomplete.completed)
        self.assertEqual(incomplete.assistant_turns, 1)

    async def test_oneharness_run_exposes_typed_two_party_telemetry(self) -> None:
        """Carry timing, strict usage, and native linkage through the real CLI."""
        fake = ROOT / "target" / "debug" / f"onejudge-fake-oneharness{SUFFIX}"
        config: RunConfig = {
            "provider": {"kind": "oneharness", "bin": str(fake)},
            "system_prompt": "[[reply:telemetry ready]]",
            "evals": [{"criterion": "telemetry ready", "kind": "boolean"}],
        }
        result = await OneJudge(executable=str(BINARY)).run(config, "measure this")
        telemetry = result.telemetry
        self.assertIsNotNone(telemetry)
        assert telemetry is not None
        self.assertEqual(telemetry["agent"]["model_ms"], 10)
        self.assertEqual(telemetry["agent"]["tool_ms"], 3)
        self.assertEqual(telemetry["agent"]["time_to_first_token_ms"], 2)
        agent_usage = telemetry["agent"]["usage"]
        judge_usage = telemetry["judge"]["usage"]
        assert agent_usage is not None and judge_usage is not None
        self.assertEqual(agent_usage["cache_read_tokens"], 7)
        self.assertEqual(telemetry["judge"]["model_ms"], 5)
        self.assertEqual(telemetry["judge"]["tool_ms"], 1)
        self.assertEqual(judge_usage["output_tokens"], 1)
        self.assertEqual(telemetry["agent"]["session_ids"], ["native-onejudge-skill"])
        self.assertEqual(telemetry["judge"]["session_ids"], ["native-judge"])
        self.assertEqual([link["role"] for link in telemetry["sessions"]], ["agent", "judge"])
        self.assertTrue(all(link.get("history_id") for link in telemetry["sessions"]))

    async def test_a_failed_run_is_attributed_to_the_harness_identities_it_tried(self) -> None:
        """Carry onejudge's structured failure document, not just its stderr."""
        fake = ROOT / "target" / "debug" / f"onejudge-fake-oneharness{SUFFIX}"
        config: RunConfig = {
            "provider": {"kind": "oneharness", "bin": str(fake)},
            "system_prompt": "[[fallback-exhausted:codex|quota,claude-code|auth]]",
        }
        with self.assertRaises(OneJudgeProcessError) as raised:
            await OneJudge(executable=str(BINARY)).run(config, "fail this")
        failure = raised.exception.failure
        self.assertIsNotNone(failure)
        assert failure is not None
        self.assertEqual(failure["error"]["kind"], "auth")
        telemetry = failure["telemetry"]
        assert telemetry is not None
        attribution = telemetry["attribution"][0]
        self.assertEqual(attribution["role"], "agent")
        self.assertIsNone(attribution.get("ran"))
        self.assertEqual(
            [candidate["harness_id"] for candidate in attribution["candidates"]],
            ["codex", "claude-code"],
        )
        self.assertEqual(
            [fell["reason"] for fell in attribution["fell_through"]],
            ["quota", "auth"],
        )

    async def test_a_streamed_failure_carries_its_document_from_stderr(self) -> None:
        """A streamed run keeps stdout for the protocol and the document on stderr."""
        fake = ROOT / "target" / "debug" / f"onejudge-fake-oneharness{SUFFIX}"
        config: RunConfig = {
            "provider": {"kind": "oneharness", "bin": str(fake), "stream": True},
            "system_prompt": "[[fallback-exhausted:codex|quota]]",
        }
        with self.assertRaises(OneJudgeProcessError) as raised:
            await OneJudge(executable=str(BINARY)).run(
                config, "fail this", on_event=lambda _event: None
            )
        failure = raised.exception.failure
        assert failure is not None
        self.assertEqual(failure["error"]["kind"], "quota")
        telemetry = failure["telemetry"]
        assert telemetry is not None
        self.assertEqual(len(telemetry["attribution"][0]["candidates"]), 1)

    async def test_an_unspawnable_provider_is_classified_without_any_attribution(self) -> None:
        """A provider that never started names no identity, and says so."""
        with self.assertRaises(OneJudgeProcessError) as raised:
            await OneJudge(executable=str(BINARY)).run(
                {"provider": {"kind": "oneharness", "bin": "definitely-not-a-binary-xyz"}},
                "go",
            )
        failure = raised.exception.failure
        assert failure is not None
        self.assertEqual(failure["error"]["kind"], "spawn")
        self.assertIsNone(failure.get("telemetry"))

    async def test_a_config_refusal_before_any_run_leaves_no_document(self) -> None:
        """onejudge refuses a bad config before driving anything, so there is none."""
        with self.assertRaises(OneJudgeProcessError) as raised:
            await OneJudge(executable=str(BINARY)).run(
                {"skill": "/definitely/not/a/skill/directory"}, "go"
            )
        self.assertIsNone(raised.exception.failure)
        self.assertIn("config error", raised.exception.stderr)

    async def test_version_four_report_without_telemetry_remains_compatible(self) -> None:
        """An upgraded SDK accepts and exposes no telemetry for a v4 report."""
        client = OneJudge(
            executable=sys.executable,
            executable_args=(str(FIXTURE),),
            env={"ONEJUDGE_SDK_FIXTURE_MODE": "v4-report"},
        )
        result = await client.run({}, "legacy")
        self.assertEqual(result.raw["schema_version"], 4)
        self.assertIsNone(result.telemetry)

    async def test_streamed_run_observes_events_as_they_arrive(self) -> None:
        """Deliver each tool event during the run, then the same final result."""
        fake = ROOT / "target" / "debug" / f"onejudge-fake-oneharness{SUFFIX}"
        release = Path(tempfile.mkdtemp(prefix="onejudge-sdk-stream-")) / "release.marker"
        config: RunConfig = {
            "provider": {"kind": "oneharness", "bin": str(fake), "stream": True},
            "system_prompt": (
                f"[[reply:streamed]][[event:git commit -m fix]][[stream-wait:{release}]]"
            ),
            "evals": [{"criterion": "git commit", "kind": "boolean"}],
        }
        seen: list[StreamEvent] = []

        def watch(event: StreamEvent) -> None:
            # The fake harness blocks until this file exists, so the run can only
            # finish if this handler really ran mid-turn.
            seen.append(event)
            release.write_text("go", encoding="utf-8")

        result = await OneJudge(executable=str(BINARY)).run(
            config, "please commit", on_event=watch, timeout=60
        )
        self.assertEqual(len(seen), 1)
        self.assertEqual(seen[0]["turn"], 1)
        self.assertEqual(seen[0]["event"]["name"], "bash")
        self.assertEqual(seen[0]["event"]["input"], {"command": "git commit -m fix"})
        # The terminal line carries the ordinary, validated result.
        self.assertEqual(result.exit_code, 0)
        self.assertTrue(result.completed)
        self.assertEqual(result.raw["schema_version"], 6)
        self.assertEqual(result.assistant_turns, 1)
        self.assertEqual(result.verdicts[0]["verdict"]["value"], True)

    async def test_streamed_run_over_a_buffered_provider_still_yields_the_report(self) -> None:
        """Watch a provider that does not stream: events replay, result is intact."""
        seen: list[StreamEvent] = []
        result = await OneJudge(executable=str(BINARY)).run(
            command_config(), "python stream boundary", on_event=seen.append
        )
        self.assertEqual([event["event"]["input"] for event in seen], [{"command": "git status"}])
        self.assertTrue(result.completed)

    async def test_malformed_stream_lines_are_typed_contract_errors(self) -> None:
        """Reject an unmodelled envelope and a stream with no terminal line."""
        for mode, needle in (
            ("garbage-stream", "stream line was not valid JSON"),
            ("scalar-stream", "stream line was not a JSON object"),
            ("bad-stream", "unrecognized onejudge stream line type"),
            ("truncated-stream", "without a terminal result line"),
            # `event* result EOF`: nothing may follow the terminal line.
            ("trailing-unknown", "after its terminal result line"),
            ("trailing-event", "after its terminal result line"),
            ("trailing-result", "after its terminal result line"),
        ):
            client = OneJudge(
                executable=sys.executable,
                executable_args=(str(FIXTURE),),
                env={"ONEJUDGE_SDK_FIXTURE_MODE": mode},
            )
            with self.subTest(mode=mode), self.assertRaises(ContractError) as raised:
                await client.run({}, "fixture task", on_event=lambda _event: None)
            self.assertIn(needle, str(raised.exception))

    async def test_streaming_failure_fails_fast_and_reaps_the_child(self) -> None:
        """End the call on the failure, not on a child that keeps running."""

        def explode(_event: StreamEvent) -> None:
            raise _HandlerFailure("handler gave up")

        def ignore(_event: StreamEvent) -> None:
            return None

        directory = Path(tempfile.mkdtemp(prefix="onejudge-sdk-reap-"))
        cases: tuple[tuple[str, type[Exception], EventHandler], ...] = (
            # A forbidden trailing line from a process that then blocks.
            ("trailing-then-block", ContractError, ignore),
            # A well-formed event whose handler raises, same blocking process.
            ("blocking-stream", _HandlerFailure, explode),
        )
        for mode, expected, handler in cases:
            pid_path = directory / f"{mode}.pid"
            client = OneJudge(
                executable=sys.executable,
                executable_args=(str(FIXTURE),),
                env={
                    "ONEJUDGE_SDK_FIXTURE_MODE": mode,
                    "ONEJUDGE_SDK_FIXTURE_PID": str(pid_path),
                },
            )
            started = time.monotonic()
            with self.subTest(mode=mode), self.assertRaises(expected):
                await client.run({}, "fixture task", on_event=handler)
            # The fixture sleeps 30s after publishing; waiting it out would mean the
            # SDK hung on the child's pipes instead of failing on what it read.
            self.assertLess(time.monotonic() - started, 10, mode)
            # The stderr drain is awaited, not merely cancelled, so the failed call
            # leaves nothing of its own still scheduled.
            current = asyncio.current_task()
            self.assertEqual(
                [task for task in asyncio.all_tasks() if task is not current], [], mode
            )
            pid = int(pid_path.read_text(encoding="utf-8"))
            if os.name != "nt":  # POSIX-only liveness probe
                with self.subTest(mode=mode), self.assertRaises(ProcessLookupError):
                    os.kill(pid, 0)

    async def test_on_event_is_validated_before_spawn(self) -> None:
        """A non-callable handler fails at the Python boundary, not mid-run."""
        client = OneJudge(executable="definitely-not-started")
        with self.assertRaises(ContractError):
            # Deliberately invalid: exercises the boundary check.
            await client.run({}, "task", on_event="not callable")  # type: ignore[arg-type]

    async def test_real_cli_runtime_error_keeps_exit_and_stderr(self) -> None:
        """Exit 2 remains distinguishable from an incomplete report."""
        config: RunConfig = {
            "provider": {"kind": "command", "command": ["missing-onejudge-provider"]}
        }
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
                # These deliberately invalid inputs exercise runtime validation.
                await client.run(config, task, **kwargs)  # type: ignore[arg-type]

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
