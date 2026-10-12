"""Async typed GitHub transport at the gh process boundary."""

from __future__ import annotations

import asyncio
import json
import re
from dataclasses import asdict, dataclass
from enum import Enum
from pathlib import Path
from typing import NewType

from release_artifacts import Commit, Tag

RunId = NewType("RunId", int)


class RunStatus(str, Enum):
    QUEUED = "queued"
    PENDING = "pending"
    REQUESTED = "requested"
    WAITING = "waiting"
    RUNNING = "in_progress"
    COMPLETED = "completed"


class Conclusion(str, Enum):
    SUCCESS = "success"
    FAILURE = "failure"
    CANCELLED = "cancelled"
    SKIPPED = "skipped"
    NEUTRAL = "neutral"
    TIMED_OUT = "timed_out"
    ACTION_REQUIRED = "action_required"
    STALE = "stale"


@dataclass(frozen=True)
class Asset:
    name: str


@dataclass(frozen=True)
class Release:
    tag_name: Tag
    assets: tuple[Asset, ...]

    def to_wire(self) -> dict[str, object]:
        return asdict(self)


@dataclass(frozen=True)
class WorkflowRun:
    id: RunId
    event: str
    head_branch: Tag | None
    head_sha: Commit
    status: RunStatus | None
    conclusion: Conclusion | None

    def to_wire(self) -> dict[str, object]:
        return asdict(self)


@dataclass(frozen=True)
class Artifact:
    name: str
    expired: bool

    def to_wire(self) -> dict[str, object]:
        return asdict(self)


def object_value(raw: object, what: str) -> dict[str, object]:
    if not isinstance(raw, dict) or not all(isinstance(k, str) for k in raw):
        raise TypeError(f"{what} must be an object")
    return raw


def string_value(raw: object, what: str) -> str:
    if not isinstance(raw, str) or not raw:
        raise TypeError(f"{what} must be a nonempty string")
    return raw


def collection(raw: object, key: str) -> list[object]:
    items = object_value(raw, key).get(key)
    if not isinstance(items, list):
        raise TypeError(f"{key} must be a list")
    return items


@dataclass(frozen=True)
class GitHubClient:
    repository: str

    def __post_init__(self) -> None:
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", self.repository):
            raise ValueError("invalid GitHub repository")

    async def command(self, *args: str) -> str:
        process = await asyncio.create_subprocess_exec(
            "gh", *args, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
        )
        stdout, stderr = await process.communicate()
        if process.returncode:
            raise GitHubError(stderr.decode(), process.returncode)
        return stdout.decode()

    async def release(self, tag: Tag) -> Release | None:
        try:
            raw = object_value(
                json.loads(
                    await self.command(
                        "api", f"repos/{self.repository}/releases/tags/{tag}"
                    )
                ),
                "Release",
            )
        except GitHubError as error:
            if "HTTP 404" in str(error):
                return None
            raise
        assets = tuple(
            Asset(string_value(object_value(a, "asset").get("name"), "asset name"))
            for a in collection(raw, "assets")
        )
        return Release(
            Tag(string_value(raw.get("tag_name"), "Release tag_name")), assets
        )

    async def runs(self, sha: Commit) -> tuple[WorkflowRun, ...]:
        raw = json.loads(
            await self.command(
                "api",
                f"repos/{self.repository}/actions/workflows/release-binaries.yml/runs?head_sha={sha}&per_page=100",
            )
        )
        runs = []
        for entry in collection(raw, "workflow_runs"):
            row = object_value(entry, "workflow run")
            run_id = row.get("id")
            if type(run_id) is not int or run_id < 1:
                raise TypeError("workflow run id must be a positive integer")
            commit = string_value(row.get("head_sha"), "workflow run head_sha")
            if not re.fullmatch(r"[0-9a-f]{40}", commit):
                raise ValueError("invalid workflow run head_sha")
            status_raw = row["status"]
            status = (
                None
                if status_raw is None
                else RunStatus(string_value(status_raw, "workflow run status"))
            )
            branch_raw = row["head_branch"]
            branch = (
                None
                if branch_raw is None
                else Tag(string_value(branch_raw, "workflow run head_branch"))
            )
            conclusion_raw = row.get("conclusion")
            conclusion = (
                None
                if conclusion_raw is None
                else Conclusion(string_value(conclusion_raw, "workflow run conclusion"))
            )
            if status is RunStatus.COMPLETED and conclusion is None:
                raise ValueError("completed workflow run has no conclusion")
            runs.append(
                WorkflowRun(
                    RunId(run_id),
                    string_value(row.get("event"), "workflow run event"),
                    branch,
                    Commit(commit),
                    status,
                    conclusion,
                )
            )
        return tuple(runs)

    async def artifacts(self, run_id: RunId) -> tuple[Artifact, ...]:
        raw = json.loads(
            await self.command(
                "api", f"repos/{self.repository}/actions/runs/{run_id}/artifacts"
            )
        )
        rows = []
        for entry in collection(raw, "artifacts"):
            row = object_value(entry, "artifact")
            expired = row.get("expired")
            if type(expired) is not bool:
                raise TypeError("artifact expired must be a boolean")
            rows.append(
                Artifact(string_value(row.get("name"), "artifact name"), expired)
            )
        return tuple(rows)

    async def release_download(self, tag: Tag, name: str, destination: Path) -> None:
        await self.command(
            "release",
            "download",
            tag,
            "--repo",
            self.repository,
            "--pattern",
            name,
            "--dir",
            str(destination),
        )

    async def run_download(self, run_id: RunId, name: str, destination: Path) -> None:
        await self.command(
            "run",
            "download",
            str(run_id),
            "--repo",
            self.repository,
            "--name",
            name,
            "--dir",
            str(destination),
        )


class GitHubError(RuntimeError):
    def __init__(self, message: str, status: int):
        super().__init__(f"GitHub transport failed ({status}): {message}")
