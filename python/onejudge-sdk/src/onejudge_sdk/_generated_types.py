"""Generated from onejudge. Do not edit."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any, Literal, Optional, TypedDict, Union

ProviderKind = Literal["oneharness", "command", "split", "llmlint"]
JudgeKind = Literal["boolean", "numeric"]
Role = Literal["user", "assistant", "system"]
JudgeValue = Union[bool, float]
Decision = Literal["done", "continue", "no_instruction", "unparseable", "error"]
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


class ProviderConfig(TypedDict, total=False):
    kind: ProviderKind
    bin: Optional[str]
    judge_config: Optional[str]
    stream: Optional[bool]
    control: Optional[bool]
    mock_harness: Optional[Sequence[str]]
    command: Optional[Sequence[str]]
    config: Optional[str]
    diff_base: Optional[str]
    args: Optional[Sequence[str]]
    skill: Optional[ProviderConfig]
    judge: Optional[ProviderConfig]
    judges: Optional[Sequence[ProviderConfig]]
    label: Optional[str]


class UserConfig(TypedDict, total=False):
    persona: str
    done_when: Optional[str]
    max_turns: Optional[int]
    settle_on_noop: Optional[bool]
    artifacts: Sequence[str]


class _EvalConfigRequired(TypedDict):
    criterion: str


class EvalConfig(_EvalConfigRequired, total=False):
    kind: JudgeKind
    scale: Optional[Sequence[float]]


class Transcript(TypedDict):
    messages: Sequence[Message]


class _MessageRequired(TypedDict):
    role: Role
    content: str


class Message(_MessageRequired, total=False):
    events: Sequence[ToolEvent]


class _ToolEventRequired(TypedDict):
    kind: str
    index: int


class ToolEvent(_ToolEventRequired, total=False):
    name: Optional[str]
    input: Any
    output: Optional[str]
    tool_call_id: Optional[str]


class NamedVerdict(TypedDict):
    criterion: str
    kind: JudgeKind
    verdict: JudgeVerdict


class _JudgeVerdictRequired(TypedDict):
    value: JudgeValue
    reason: str


class JudgeVerdict(_JudgeVerdictRequired, total=False):
    usage: Optional[Usage]


class Usage(TypedDict, total=False):
    input_tokens: Optional[int]
    output_tokens: Optional[int]
    cache_read_tokens: Optional[int]
    cache_write_tokens: Optional[int]
    cost_usd: Optional[float]


class JudgedTurn(TypedDict):
    turn: int
    decisions: Sequence[JudgeDecision]


class JudgeDecision(TypedDict):
    judge: str
    kind: str
    decision: Decision
    reason: str


class _TelemetryRequired(TypedDict):
    wall_ms: int
    agent: PartyTelemetry
    judge: PartyTelemetry
    orchestration_ms: int
    sessions: Sequence[SessionLink]


class Telemetry(_TelemetryRequired, total=False):
    attribution: Sequence[HarnessAttribution]


class PartyTelemetry(TypedDict, total=False):
    model_ms: Optional[int]
    tool_ms: Optional[int]
    time_to_first_token_ms: Optional[int]
    usage: Optional[Usage]
    session_ids: Sequence[str]


class _SessionLinkRequired(TypedDict):
    session_id: str
    role: TelemetryRole
    turn_index: int
    started_at: str
    finished_at: Optional[str]


class SessionLink(_SessionLinkRequired, total=False):
    history_id: Optional[str]
    judge: Optional[str]


class _HarnessAttributionRequired(TypedDict):
    role: TelemetryRole
    turn_index: int
    candidates: Sequence[CandidateAttempt]


class HarnessAttribution(_HarnessAttributionRequired, total=False):
    ran: Optional[str]
    fell_through: Sequence[FellThrough]
    history_file: Optional[str]
    judge: Optional[str]


class FellThrough(TypedDict):
    harness: str
    reason: str


class _CandidateAttemptRequired(TypedDict):
    harness: str
    harness_id: str
    status: str
    available: bool
    ran: bool


class CandidateAttempt(_CandidateAttemptRequired, total=False):
    variant: Optional[str]
    model: Optional[str]
    failure_kind: Optional[str]
    failure_kind_source: Optional[str]
    exit_code: Optional[int]
    duration_ms: Optional[int]
    error: Optional[str]
    session_id: Optional[str]
    history_id: Optional[str]
    usage: Optional[Usage]


class _SpawnedProcessRequired(TypedDict):
    role: TelemetryRole
    op: str
    program: str
    pid: int


class SpawnedProcess(_SpawnedProcessRequired, total=False):
    group: Optional[str]
    judge: Optional[str]


class ControlAddress(TypedDict):
    session: str
    session_dir: str
    cwd: str


class _FailureDetailRequired(TypedDict):
    message: str


class FailureDetail(_FailureDetailRequired, total=False):
    kind: Optional[ProviderErrorKind]


class RunConfig(TypedDict, total=False):
    provider: ProviderConfig
    skill: Optional[str]
    system_prompt: Optional[str]
    task: Optional[str]
    user: Optional[UserConfig]
    session: Optional[str]
    evals: Sequence[EvalConfig]
    assessment: Optional[str]


class _RunReportRequired(TypedDict):
    schema_version: int
    transcript: Transcript
    stopped_early: bool


class RunReport(_RunReportRequired, total=False):
    verdicts: Sequence[NamedVerdict]
    assessment: Optional[str]
    completion_reason: Optional[str]
    settled_reason: Optional[str]
    judge_decisions: Sequence[JudgedTurn]
    usage: Optional[Usage]
    telemetry: Optional[Telemetry]
    processes: Sequence[SpawnedProcess]
    control: Optional[ControlAddress]
    control_unavailable: Optional[str]
    supervisor_control: Optional[ControlAddress]
    supervisor_control_unavailable: Optional[str]


class StreamEvent(TypedDict):
    turn: int
    event: ToolEvent


class _FailureReportRequired(TypedDict):
    schema_version: int
    error: FailureDetail


class FailureReport(_FailureReportRequired, total=False):
    telemetry: Optional[Telemetry]
    processes: Sequence[SpawnedProcess]
    judge_decisions: Sequence[JudgedTurn]
