"""Typed public errors raised by the Python SDK."""

from __future__ import annotations

from typing import Optional

from ._generated_types import FailureReport


class ContractError(ValueError):
    """A value did not match its Rust-owned SDK contract."""


class OneJudgeProcessError(RuntimeError):
    """The onejudge subprocess could not produce a report.

    ``failure`` carries onejudge's structured failure document when it emitted one
    (`--format json` writes it where the report would have gone; a streamed run
    writes it as one JSON line on stderr, since stdout is the event protocol). It
    says which harness identity refused and on which side of the conversation, so a
    caller attributes a failure without parsing ``stderr``.
    """

    def __init__(
        self,
        returncode: int,
        stderr: str,
        failure: Optional[FailureReport] = None,
    ) -> None:
        self.returncode = returncode
        self.stderr = stderr
        self.failure = failure
        super().__init__(f"onejudge exited {returncode}: {stderr.strip()}")


class OneJudgeTimeoutError(OneJudgeProcessError):
    """The onejudge subprocess exceeded the caller's timeout."""

    def __init__(self, timeout: float, stderr: str = "") -> None:
        self.timeout = timeout
        super().__init__(-1, stderr or f"timed out after {timeout} seconds")
