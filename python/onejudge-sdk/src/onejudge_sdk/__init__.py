"""Async Python SDK for the onejudge CLI."""

from ._client import OneJudge
from ._errors import ContractError, OneJudgeProcessError, OneJudgeTimeoutError
from ._generated_types import (
    EvalConfig,
    JudgeKind,
    JudgeVerdict,
    Message,
    NamedVerdict,
    ProviderConfig,
    ProviderKind,
    Role,
    RunConfig,
    RunReport,
    StreamEvent,
    ToolEvent,
    Transcript,
    Usage,
    UserConfig,
)
from ._result import RunResult
from ._version import __version__

__all__ = [
    "ContractError",
    "EvalConfig",
    "JudgeKind",
    "JudgeVerdict",
    "Message",
    "NamedVerdict",
    "OneJudge",
    "OneJudgeProcessError",
    "OneJudgeTimeoutError",
    "ProviderConfig",
    "ProviderKind",
    "Role",
    "RunConfig",
    "RunReport",
    "RunResult",
    "StreamEvent",
    "ToolEvent",
    "Transcript",
    "Usage",
    "UserConfig",
    "__version__",
]
