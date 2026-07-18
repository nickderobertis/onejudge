"""Convenient typed view over a validated onejudge report."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from ._generated_types import RunReport


@dataclass(frozen=True)
class RunResult:
    """A completed or incomplete run, retaining process and raw report data."""

    exit_code: int
    stderr: str
    raw: RunReport

    @property
    def completed(self) -> bool:
        """Whether onejudge exited with its completed/evals-passed status."""
        return self.exit_code == 0

    @property
    def verdicts(self) -> list[dict[str, Any]]:
        """Return the report's ordered verdicts."""
        return self.raw.get("verdicts", [])

    @property
    def usage(self) -> dict[str, Any]:
        """Return aggregate usage, or an empty mapping when unavailable."""
        return self.raw.get("usage") or {}

    @property
    def assistant_turns(self) -> int:
        """Count assistant turns in the transcript."""
        messages = self.raw["transcript"]["messages"]
        return sum(message["role"] == "assistant" for message in messages)

    @property
    def agent_turns(self) -> int:
        """Alias assistant turns using ai-orchestrator terminology."""
        return self.assistant_turns
