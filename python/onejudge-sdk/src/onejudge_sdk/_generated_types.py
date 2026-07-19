"""Generated from onejudge. Do not edit."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any, Literal, Optional, TypedDict, Union


JudgeKind = Literal["boolean", "numeric"]
ProviderKind = Literal["oneharness", "command", "split"]
JudgeValue = Union[bool, float]
Role = Literal["user", "assistant", "system"]
TelemetryRole = Literal["agent", "judge"]


class _EvalConfigRequired(TypedDict):
    criterion: str


class EvalConfig(_EvalConfigRequired, total=False):
    kind: JudgeKind
    scale: Optional[Sequence[float]]


class ProviderConfig(TypedDict, total=False):
    bin: Optional[str]
    command: Optional[Sequence[str]]
    judge: Optional[ProviderConfig]
    judge_config: Optional[str]
    kind: ProviderKind
    skill: Optional[ProviderConfig]


class UserConfig(TypedDict, total=False):
    done_when: Optional[str]
    max_turns: Optional[int]
    persona: str


class _JudgeVerdictRequired(TypedDict):
    reason: str
    value: JudgeValue


class JudgeVerdict(_JudgeVerdictRequired, total=False):
    usage: Optional[Usage]


class _MessageRequired(TypedDict):
    content: str
    role: Role


class Message(_MessageRequired, total=False):
    events: Sequence[ToolEvent]


class NamedVerdict(TypedDict):
    criterion: str
    kind: JudgeKind
    verdict: JudgeVerdict


class PartyTelemetry(TypedDict, total=False):
    model_ms: Optional[int]
    session_ids: Sequence[str]
    time_to_first_token_ms: Optional[int]
    tool_ms: Optional[int]
    usage: Optional[Usage]


class _SessionLinkRequired(TypedDict):
    finished_at: Optional[str]
    role: TelemetryRole
    session_id: str
    started_at: str
    turn_index: int


class SessionLink(_SessionLinkRequired, total=False):
    history_id: Optional[str]


class Telemetry(TypedDict):
    agent: PartyTelemetry
    judge: PartyTelemetry
    orchestration_ms: int
    sessions: Sequence[SessionLink]
    wall_ms: int


class _ToolEventRequired(TypedDict):
    index: int
    kind: str


class ToolEvent(_ToolEventRequired, total=False):
    input: Any
    name: Optional[str]
    output: Optional[str]


class Transcript(TypedDict):
    messages: Sequence[Message]


class Usage(TypedDict, total=False):
    cache_read_tokens: Optional[int]
    cache_write_tokens: Optional[int]
    cost_usd: Optional[float]
    input_tokens: Optional[int]
    output_tokens: Optional[int]


class RunConfig(TypedDict, total=False):
    assessment: Optional[str]
    evals: Sequence[EvalConfig]
    provider: ProviderConfig
    session: Optional[str]
    skill: Optional[str]
    system_prompt: Optional[str]
    task: Optional[str]
    user: Optional[UserConfig]


class _RunReportRequired(TypedDict):
    schema_version: int
    stopped_early: bool
    transcript: Transcript


class RunReport(_RunReportRequired, total=False):
    assessment: Optional[str]
    completion_reason: Optional[str]
    telemetry: Optional[Telemetry]
    usage: Optional[Usage]
    verdicts: Sequence[NamedVerdict]


class StreamEvent(TypedDict):
    event: ToolEvent
    turn: int
