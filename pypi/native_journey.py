"""The packaging target's host-native journey uses the production packer and installer."""

from __future__ import annotations

import argparse
import subprocess
import tomllib
from pathlib import Path

from release_artifacts import payload
from smoke_release import smoke


def host() -> str:
    return next(
        line.removeprefix("host: ")
        for line in subprocess.check_output(["rustc", "-vV"], text=True).splitlines()
        if line.startswith("host: ")
    )


def journey(root: Path) -> None:
    target = host()
    version = tomllib.loads(Path("Cargo.toml").read_text())["workspace"]["package"][
        "version"
    ]
    (wheel,) = root.glob("*.whl")
    archive = root / f"onejudge-{target}.tar.gz"
    if payload(archive, "archive") != payload(wheel, "cli-wheel"):
        raise ValueError("production archive and wheel did not reuse the native binary")
    smoke(target, root, version)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    args = parser.parse_args()
    journey(args.root)
