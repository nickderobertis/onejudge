"""Verify and publish staged products on Linux; only the Forge transport is external."""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import tempfile
import zipfile
from pathlib import Path

from github_client import GitHubClient
from release_artifacts import Commit, Kind, ReleaseIdentity, Tag, Version, validate
from release_storage import locate


async def publish(
    repo: str, release: ReleaseIdentity, root: Path, archive_only: bool = False
) -> None:
    client = GitHubClient(repo)
    if archive_only:
        files = sorted(root.iterdir())
        if not files or any(
            not f.is_file()
            or not re.fullmatch(r"onejudge-[a-z0-9_-]+\.(?:tar\.gz|zip)", f.name)
            for f in files
        ):
            raise ValueError("archive upload contains missing or unexpected products")
        location = await locate(client, release)
        await client.command(
            "release",
            "upload",
            location.tag,
            *(str(f) for f in files),
            "--repo",
            repo,
            "--clobber",
        )
        return
    manifest = validate(
        root,
        json.loads((root / "release-artifacts.json").read_text()),
        release.tag,
        release.sha,
        release.version,
    )
    location = await locate(client, release)
    with tempfile.TemporaryDirectory(
        dir=os.environ.get("ONEPIPELINE_NODE_SCRATCH_DIR")
        or os.environ.get("RUNNER_TEMP")
    ) as scratch:
        bundle = Path(scratch) / f"{release.name}.zip"
        with zipfile.ZipFile(bundle, "w") as package:
            package.write(root / "release-artifacts.json", "release-artifacts.json")
            for row in manifest.files:
                package.write(root / row.path, row.path)
        archives = [
            str(root / row.path) for row in manifest.files if row.kind is Kind.ARCHIVE
        ]
        await client.command(
            "release",
            "upload",
            location.tag,
            str(bundle),
            *archives,
            "--repo",
            repo,
            "--clobber",
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("repo", "tag", "sha", "version"):
        parser.add_argument("--" + key, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--archive-only", action="store_true")
    args = parser.parse_args()
    asyncio.run(
        publish(
            args.repo,
            ReleaseIdentity(Tag(args.tag), Commit(args.sha), Version(args.version)),
            args.root,
            args.archive_only,
        )
    )
