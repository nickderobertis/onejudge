"""Build once with maturin; the wheel's exact executable also supplies the archive."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import tarfile
import tempfile
import zipfile
from dataclasses import dataclass
from pathlib import Path

from release_artifacts import TARGETS


@dataclass(frozen=True)
class Products:
    archive: Path
    wheel: Path


def build(target: str, output: Path) -> Products:
    if target == "host":
        from native_journey import host

        target = host()
    if target.endswith("-apple-darwin") and target not in TARGETS:
        raise ValueError(
            f"{target} is not released: Intel macOS is no longer supported; "
            f"the only macOS release target is {', '.join(TARGETS)}"
        )
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        dir=os.environ.get("ONEPIPELINE_NODE_SCRATCH_DIR")
        or os.environ.get("RUNNER_TEMP")
    ) as scratch:
        wheels = Path(scratch) / "wheels"
        subprocess.run(
            [
                "uvx",
                "maturin",
                "build",
                "--release",
                "--locked",
                "--target",
                target,
                "--out",
                str(wheels),
            ],
            check=True,
        )
        (wheel,) = wheels.glob("*.whl")
        wheel = Path(shutil.copy2(wheel, output))
        staging = Path(scratch) / f"onejudge-{target}"
        staging.mkdir()
        with zipfile.ZipFile(wheel) as package:
            (binary,) = [
                name
                for name in package.namelist()
                if name.endswith(".data/scripts/onejudge")
            ]
            exe = staging / "onejudge"
            exe.write_bytes(package.read(binary))
            exe.chmod(0o755)
        for name in ("README.md", "LICENSE"):
            shutil.copy2(name, staging)
        archive = output / f"onejudge-{target}.tar.gz"
        with tarfile.open(archive, "w:gz") as package:
            package.add(staging, arcname=staging.name)
    return Products(archive, wheel)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    build(args.target, args.out)
