"""Fetch a successful exact-tag producer, or its durable Release asset, then verify."""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import tempfile
import zipfile
from pathlib import Path

from github_client import Conclusion, GitHubClient, RunStatus
from producer_plan import canonical
from release_artifacts import Commit, ReleaseIdentity, Tag, Version, validate
from release_storage import locate


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


async def fetch(
    repo: str, tag: str, sha: str, version: str, root: Path, attempts: int = 60
) -> None:
    if attempts < 1:
        raise ValueError("producer wait attempts must be positive")
    release = ReleaseIdentity(
        Tag(canonical(tag, sha, version)), Commit(sha), Version(version)
    )
    client = GitHubClient(repo)
    root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        dir=os.environ.get("ONEPIPELINE_NODE_SCRATCH_DIR")
        or os.environ.get("RUNNER_TEMP")
    ) as scratch:
        bundle = Path(scratch) / f"{release.name}.zip"
        location = await locate(client, release)
        assets = [a for a in location.release.assets if a.name == bundle.name]
        if len(assets) > 1:
            raise ValueError("ambiguous durable release artifact")
        if assets:
            await client.release_download(location.tag, bundle.name, Path(scratch))
            unpack(bundle, root)
        else:
            for attempt in range(attempts):
                runs = [
                    r
                    for r in await client.runs(release.sha)
                    if r.head_sha == release.sha
                    and r.head_branch == release.tag
                    and r.event == "push"
                ]
                if runs:
                    run = max(runs, key=lambda r: r.id)
                    if run.status is None:
                        raise ValueError(
                            f"exact-tag producer {run.id} has unavailable status"
                        )
                    if run.status is RunStatus.COMPLETED:
                        if run.conclusion is not Conclusion.SUCCESS:
                            raise ValueError(
                                f"exact-tag producer {run.id} failed: {run.conclusion.value}"
                            )
                        matches = [
                            a
                            for a in await client.artifacts(run.id)
                            if a.name == release.name and not a.expired
                        ]
                        if len(matches) != 1:
                            raise ValueError(
                                "successful exact-tag producer has no unique unexpired artifact"
                            )
                        await client.run_download(run.id, release.name, root)
                        break
                if attempt == attempts - 1:
                    raise ValueError(
                        "exact-tag producer absent or did not finish; recovery never automatically rebuilds macOS"
                    )
                await asyncio.sleep(10)
        validate(
            root,
            json.loads((root / "release-artifacts.json").read_text()),
            release.tag,
            release.sha,
            release.version,
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("repo", "tag", "sha", "version"):
        parser.add_argument("--" + key, required=True)
    parser.add_argument("--root", type=Path, default=Path("darwin"))
    parser.add_argument("--attempts", type=int, default=60)
    args = parser.parse_args()
    asyncio.run(
        fetch(args.repo, args.tag, args.sha, args.version, args.root, args.attempts)
    )
