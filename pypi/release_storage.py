"""Locate an existing Release at the exact commit without creating an alias Release."""

from __future__ import annotations

import argparse
import asyncio
import subprocess
from dataclasses import dataclass

from github_client import GitHubClient, Release
from release_artifacts import Commit, ReleaseIdentity, Tag, Version


def git(*args: str) -> str:
    return subprocess.check_output(
        ["git", *args], text=True, stderr=subprocess.PIPE
    ).strip()


@dataclass(frozen=True)
class ReleaseLocation:
    tag: Tag
    release: Release


async def locate(client: GitHubClient, identity: ReleaseIdentity) -> ReleaseLocation:
    tags = (Tag(f"v{identity.version}"), Tag(f"onejudge-v{identity.version}"))
    existing: dict[Tag, Commit] = {}
    for candidate in tags:
        try:
            existing[candidate] = Commit(
                git("rev-parse", "--verify", f"refs/tags/{candidate}^{{commit}}")
            )
        except subprocess.CalledProcessError:
            continue
    if not existing or any(commit != identity.sha for commit in existing.values()):
        raise ValueError(
            f"release tags {existing} do not agree with exact SHA {identity.sha}; considered {tags}"
        )
    for candidate in tags:
        if candidate not in existing:
            continue
        release = await client.release(candidate)
        if release is None:
            continue
        if release.tag_name != candidate or existing[candidate] != identity.sha:
            raise ValueError(
                f"Release tag {release.tag_name} does not match {candidate} at exact SHA {identity.sha}"
            )
        return ReleaseLocation(candidate, release)
    raise ValueError(f"no existing Release for {tags} at exact SHA {identity.sha}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("repo", "tag", "sha", "version"):
        parser.add_argument("--" + key, required=True)
    args = parser.parse_args()
    release = ReleaseIdentity(Tag(args.tag), Commit(args.sha), Version(args.version))
    print(asyncio.run(locate(GitHubClient(args.repo), release)).tag)
