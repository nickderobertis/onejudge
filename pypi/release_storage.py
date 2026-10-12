"""Locate an existing Release at the exact commit without creating an alias Release."""

from __future__ import annotations

import argparse
import json
import subprocess


def git(*args: str) -> str:
    return subprocess.check_output(
        ["git", *args], text=True, stderr=subprocess.PIPE
    ).strip()


def locate(repo: str, tag: str, sha: str, version: str) -> tuple[str, dict]:
    from release_artifacts import identity

    identity(tag, sha, version)
    tags = [f"v{version}", f"onejudge-v{version}"]
    existing = {}
    for candidate in tags:
        try:
            existing[candidate] = git(
                "rev-parse", "--verify", f"refs/tags/{candidate}^{{commit}}"
            )
        except subprocess.CalledProcessError:
            continue
    if not existing or any(commit != sha for commit in existing.values()):
        raise ValueError(
            f"release tags {existing} do not agree with exact SHA {sha}; considered {tags}"
        )
    for candidate in tags:
        if candidate not in existing:
            continue
        response = subprocess.run(
            ["gh", "api", f"repos/{repo}/releases/tags/{candidate}"],
            capture_output=True,
            text=True,
            check=False,
        )
        if response.returncode:
            if "HTTP 404" in response.stderr:
                continue
            raise ValueError(
                f"reading Release {candidate} at {sha} failed: {response.stderr}"
            )
        release = json.loads(response.stdout)
        if release.get("tag_name") != candidate or existing[candidate] != sha:
            raise ValueError(
                f"Release tag {release.get('tag_name')} does not match {candidate} at exact SHA {sha}"
            )
        return candidate, release
    raise ValueError(f"no existing Release for {tags} at exact SHA {sha}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("repo", "tag", "sha", "version"):
        parser.add_argument("--" + key, required=True)
    args = parser.parse_args()
    print(locate(args.repo, args.tag, args.sha, args.version)[0])
