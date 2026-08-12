"""Generated from onejudge. Do not edit."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any, Literal, Optional, TypedDict, Union

JudgeKind = Literal["boolean", "numeric"]
ProviderKind = Literal["oneharness", "command", "split"]
JudgeValue = Union[bool, float]
Role = Literal["user", "assistant", "system"]
TelemetryRole = Literal["agent", "judge"]
ProviderErrorKind = Literal[
    "auth",
    "rate_limit",
    "model_not_found",
    "quota",
    "overloaded",
    "timeout",
    "cancelled",
    "spawn",
    "protocol",
    "other",
]


class _EvalConfigRequired(TypedDict):
    criterion: str


class EvalConfig(_EvalConfigRequired, total=False):
    kind: JudgeKind
    scale: Optional[Sequence[float]]


class ProviderConfig(TypedDict, total=False):
    bin: Optional[str]
    command: Optional[Sequence[str]]
    control: Optional[bool]
    judge: Optional[ProviderConfig]
    judge_config: Optional[str]
    kind: ProviderKind
    skill: Optional[ProviderConfig]
    stream: Optional[bool]


class UserConfig(TypedDict, total=False):
    done_when: Optional[str]
    max_turns: Optional[int]
    persona: str


class _CandidateAttemptRequired(TypedDict):
    available: bool
    harness: str
    harness_id: str
    ran: bool
    status: str


class CandidateAttempt(_CandidateAttemptRequired, total=False):
    duration_ms: Optional[int]
    error: Optional[str]
    exit_code: Optional[int]
    failure_kind: Optional[str]
    failure_kind_source: Optional[str]
    history_id: Optional[str]
    model: Optional[str]
    session_id: Optional[str]
    usage: Optional[Usage]
    variant: Optional[str]


class ControlAddress(TypedDict):
    cwd: str
    session: str
    session_dir: str


class FellThrough(TypedDict):
    harness: str
    reason: str


class _HarnessAttributionRequired(TypedDict):
    candidates: Sequence[CandidateAttempt]
    role: TelemetryRole
    turn_index: int


class HarnessAttribution(_HarnessAttributionRequired, total=False):
    fell_through: Sequence[FellThrough]
    history_file: Optional[str]
    ran: Optional[str]


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


class _SpawnedProcessRequired(TypedDict):
    op: str
    pid: int
    program: str
    role: TelemetryRole


class SpawnedProcess(_SpawnedProcessRequired, total=False):
    group: Optional[str]


class _TelemetryRequired(TypedDict):
    agent: PartyTelemetry
    judge: PartyTelemetry
    orchestration_ms: int
    sessions: Sequence[SessionLink]
    wall_ms: int


class Telemetry(_TelemetryRequired, total=False):
    attribution: Sequence[HarnessAttribution]


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


class _FailureDetailRequired(TypedDict):
    message: str


class FailureDetail(_FailureDetailRequired, total=False):
    kind: Optional[ProviderErrorKind]


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
    control: Optional[ControlAddress]
    control_unavailable: Optional[str]
    processes: Sequence[SpawnedProcess]
    telemetry: Optional[Telemetry]
    usage: Optional[Usage]
    verdicts: Sequence[NamedVerdict]


class StreamEvent(TypedDict):
    event: ToolEvent
    turn: int


class _FailureReportRequired(TypedDict):
    error: FailureDetail
    schema_version: int


class FailureReport(_FailureReportRequired, total=False):
    processes: Sequence[SpawnedProcess]
    telemetry: Optional[Telemetry]
