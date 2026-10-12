"""Resolve one canonical tag producer for both supported release-tag spellings."""

from __future__ import annotations

import os
import re
import subprocess

import tomllib
from release_artifacts import identity


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


def plan(tag: str) -> dict[str, str]:
    if not re.fullmatch(r"(?:onejudge-)?v\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?", tag):
        raise ValueError("invalid release tag")
    sha = git("rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}")
    version = tomllib.loads(git("show", f"{sha}:Cargo.toml"))["workspace"]["package"][
        "version"
    ]
    producer = canonical(tag, sha, version)
    return {
        "tag": producer,
        "sha": sha,
        "version": version,
        "name": identity(producer, sha, version),
        "build": str(
            tag == producer
            or os.environ.get("GITHUB_EVENT_NAME") == "workflow_dispatch"
        ).lower(),
    }


if __name__ == "__main__":
    for key, value in plan(os.environ["TAG"]).items():
        print(f"{key}={value}")
