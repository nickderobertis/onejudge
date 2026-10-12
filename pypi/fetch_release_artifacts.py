"""Fetch a successful exact-tag producer, or its durable Release asset, then verify."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import tempfile
import time
import zipfile
from pathlib import Path

from producer_plan import canonical
from release_artifacts import identity, validate
from release_storage import locate


def gh(*args: str) -> str:
    return subprocess.check_output(["gh", *args], text=True)


def unpack(bundle: Path, root: Path) -> None:
    with zipfile.ZipFile(bundle) as package:
        for name in package.namelist():
            parts = name.split("/")
            if (
                name.startswith("/")
                or "\\" in name
                or any(p in ("..", ".") for p in parts)
            ):
                raise ValueError("unsafe artifact zip path")
        package.extractall(root)


def fetch(
    repo: str, tag: str, sha: str, version: str, root: Path, attempts: int = 60
) -> None:
    if attempts < 1:
        raise ValueError("producer wait attempts must be positive")
    tag = canonical(tag, sha, version)
    name = identity(tag, sha, version)
    root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        dir=os.environ.get("ONEPIPELINE_NODE_SCRATCH_DIR")
        or os.environ.get("RUNNER_TEMP")
    ) as scratch:
        bundle = Path(scratch) / f"{name}.zip"
        # Durable assets survive Actions retention. Absence may mean the producer is live.
        storage_tag, release = locate(repo, tag, sha, version)
        assets = [a for a in release["assets"] if a["name"] == bundle.name]
        if len(assets) > 1:
            raise ValueError("ambiguous durable release artifact")
        if assets:
            gh(
                "release",
                "download",
                storage_tag,
                "--repo",
                repo,
                "--pattern",
                bundle.name,
                "--dir",
                scratch,
            )
            unpack(bundle, root)
        else:
            for attempt in range(attempts):
                runs = json.loads(
                    gh(
                        "api",
                        f"repos/{repo}/actions/workflows/release-binaries.yml/runs?head_sha={sha}&per_page=100",
                    )
                )["workflow_runs"]
                runs = [
                    r
                    for r in runs
                    if r["head_sha"] == sha
                    and r["head_branch"] == tag
                    and r["event"] == "push"
                ]
                # Retry only while no producer exists yet or the exact producer is still running.
                if runs:
                    run = max(runs, key=lambda r: r["id"])
                    if run["status"] == "completed":
                        if run["conclusion"] != "success":
                            raise ValueError(
                                f"exact-tag producer {run['id']} failed: {run['conclusion']}"
                            )
                        artifacts = json.loads(
                            gh(
                                "api",
                                f"repos/{repo}/actions/runs/{run['id']}/artifacts",
                            )
                        )["artifacts"]
                        matches = [
                            a
                            for a in artifacts
                            if a["name"] == name and not a["expired"]
                        ]
                        if len(matches) != 1:
                            raise ValueError(
                                "successful exact-tag producer has no unique unexpired artifact"
                            )
                        gh(
                            "run",
                            "download",
                            str(run["id"]),
                            "--repo",
                            repo,
                            "--name",
                            name,
                            "--dir",
                            str(root),
                        )
                        break
                if attempt == attempts - 1:
                    raise ValueError(
                        "exact-tag producer absent or did not finish; recovery never automatically rebuilds macOS"
                    )
                time.sleep(10)
        validate(
            root,
            json.loads((root / "release-artifacts.json").read_text()),
            tag,
            sha,
            version,
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("repo", "tag", "sha", "version"):
        parser.add_argument("--" + key, required=True)
    parser.add_argument("--root", type=Path, default=Path("darwin"))
    parser.add_argument("--attempts", type=int, default=60)
    args = parser.parse_args()
    fetch(args.repo, args.tag, args.sha, args.version, args.root, args.attempts)
