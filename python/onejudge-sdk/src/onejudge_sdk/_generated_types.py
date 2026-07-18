"""Generated from onejudge. Do not edit."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any, TypedDict


class RunConfig(TypedDict, total=False):
    assessment: Any
    evals: Sequence[dict[str, Any]]
    provider: dict[str, Any]
    session: Any
    skill: Any
    system_prompt: Any
    task: Any
    user: Any


RunReport = dict[str, Any]
