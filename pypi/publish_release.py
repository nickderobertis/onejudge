"""Verify and publish staged products on Linux; only the Forge transport is external."""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import tarfile
import tempfile
import zipfile
from pathlib import Path

from github_client import GitHubClient
from release_artifacts import Commit, Kind, ReleaseIdentity, Tag, Version, validate
from release_storage import locate


def archive_payload(file: Path, target: str) -> bytes:
    prefix = f"onejudge-{target}"
    executable = "onejudge.exe" if target.endswith("windows-msvc") else "onejudge"
    expected = {f"{prefix}/{name}" for name in (executable, "README.md", "LICENSE")}
    match file.suffix:
        case ".zip":
            with zipfile.ZipFile(file) as package:
                names = {name for name in package.namelist() if not name.endswith("/")}
                if names != expected:
                    raise ValueError("archive contents mismatch")
                binary = package.read(f"{prefix}/{executable}")
        case ".gz":
            with tarfile.open(file) as package:
                members = package.getmembers()
                if {m.name for m in members if m.isfile()} != expected or any(
                    m.issym() or m.islnk() for m in members
                ):
                    raise ValueError("archive contents mismatch")
                stream = package.extractfile(f"{prefix}/{executable}")
                if stream is None:
                    raise ValueError("archive executable missing")
                binary = stream.read()
        case _:
            raise ValueError("unsupported archive format")
    match target:
        case "x86_64-unknown-linux-gnu":
            if (
                binary[:4] != b"\x7fELF"
                or binary[4:6] != b"\x02\x01"
                or int.from_bytes(binary[18:20], "little") != 62
            ):
                raise ValueError("archive ELF architecture mismatch")
        case "x86_64-pc-windows-msvc":
            offset = int.from_bytes(binary[60:64], "little")
            if (
                binary[:2] != b"MZ"
                or binary[offset : offset + 4] != b"PE\0\0"
                or int.from_bytes(binary[offset + 4 : offset + 6], "little") != 0x8664
            ):
                raise ValueError("archive PE architecture mismatch")
        case _:
            raise ValueError(f"unsupported archive target: {target}")
    return binary


async def publish(
    repo: str,
    release: ReleaseIdentity,
    root: Path,
    archive_only: bool = False,
    targets: tuple[str, ...] = (),
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
        expected = {
            f"onejudge-{target}.{'zip' if target.endswith('windows-msvc') else 'tar.gz'}"
            for target in targets
        }
        if (
            not targets
            or len(set(targets)) != len(targets)
            or {file.name for file in files} != expected
        ):
            raise ValueError("archive handoff missing required targets")
        for file in files:
            target = next(
                target
                for target in targets
                if file.name.startswith(f"onejudge-{target}.")
            )
            if file.is_symlink():
                raise ValueError("unsafe archive payload")
            archive_payload(file, target)
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
    parser.add_argument(
        "--targets",
        default="",
        help="Expected targets declared by the release workflow",
    )
    args = parser.parse_args()
    asyncio.run(
        publish(
            args.repo,
            ReleaseIdentity(Tag(args.tag), Commit(args.sha), Version(args.version)),
            args.root,
            args.archive_only,
            tuple(args.targets.split()),
        )
    )
