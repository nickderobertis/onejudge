"""Resolve one canonical tag producer for both supported release-tag spellings."""

from __future__ import annotations

import os
import re
import subprocess
import tomllib
from dataclasses import dataclass

from release_artifacts import Commit, ReleaseIdentity, Tag, Version, identity


def git(*args: str) -> str:
    return subprocess.check_output(["git", *args], text=True).strip()


def canonical(tag: str, sha: str, version: str) -> str:
    identity(tag, sha, version)
    other = f"v{version}"
    try:
        if git("rev-parse", "--verify", f"refs/tags/{other}^{{commit}}") == sha:
            return other
    except subprocess.CalledProcessError:
        pass
    return tag


@dataclass(frozen=True)
class ProducerPlan:
    release: ReleaseIdentity
    build: bool

    def to_outputs(self) -> dict[str, str]:
        return {
            "tag": self.release.tag,
            "sha": self.release.sha,
            "version": self.release.version,
            "name": self.release.name,
            "build": str(self.build).lower(),
        }


def plan(tag: str) -> ProducerPlan:
    if not re.fullmatch(r"(?:onejudge-)?v\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?", tag):
        raise ValueError("invalid release tag")
    sha = git("rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}")
    version = tomllib.loads(git("show", f"{sha}:Cargo.toml"))["workspace"]["package"][
        "version"
    ]
    producer = canonical(tag, sha, version)
    return ProducerPlan(
        ReleaseIdentity(Tag(producer), Commit(sha), Version(version)),
        tag == producer or os.environ.get("GITHUB_EVENT_NAME") == "workflow_dispatch",
    )


if __name__ == "__main__":
    for key, value in plan(os.environ["TAG"]).to_outputs().items():
        print(f"{key}={value}")
