"""Typed public errors raised by the Python SDK."""

from __future__ import annotations

from typing import Optional


class ContractError(ValueError):
    """A value did not match its Rust-owned SDK contract."""


class OneJudgeProcessError(RuntimeError):
    """The onejudge subprocess could not produce a report."""

    def __init__(self, returncode: int, stderr: str) -> None:
        self.returncode = returncode
        self.stderr = stderr
        super().__init__(f"onejudge exited {returncode}: {stderr.strip()}")


class OneJudgeTimeoutError(OneJudgeProcessError):
    """The onejudge subprocess exceeded the caller's timeout."""

    def __init__(self, timeout: float, stderr: str = "") -> None:
        self.timeout = timeout
        super().__init__(-1, stderr or f"timed out after {timeout} seconds")
