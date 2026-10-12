"""Install and run Unix packaged products; Darwin x86 uses Rosetta on Apple Silicon."""

from __future__ import annotations

import argparse
import os
import platform
import subprocess
import tarfile
import tempfile
from pathlib import Path

from release_artifacts import Target

SUPPORTED_TARGETS = {target.value for target in Target} | {
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
}


def smoke(target: str, root: Path, version: str) -> None:
    if target not in SUPPORTED_TARGETS:
        raise ValueError(f"unsupported smoke target: {target}")
    arch = {"x86_64": "x86_64", "aarch64": "arm64"}[target.split("-", 1)[0]]
    prefix = (
        ["arch", "-x86_64"]
        if target.endswith("apple-darwin")
        and arch == "x86_64"
        and platform.machine() == "arm64"
        else []
    )
    with tempfile.TemporaryDirectory(
        dir=os.environ.get("ONEPIPELINE_NODE_SCRATCH_DIR")
        or os.environ.get("RUNNER_TEMP")
    ) as scratch:
        work = Path(scratch)
        with tarfile.open(root / f"onejudge-{target}.tar.gz") as package:
            package.extractall(work, filter="data")
        binary = work / f"onejudge-{target}" / "onejudge"
        output = subprocess.check_output([*prefix, str(binary), "--version"], text=True)
        if output.strip() != f"onejudge {version}":
            raise ValueError("archive installed version mismatch")
        subprocess.run(
            [*prefix, str(binary), "schema"], check=True, stdout=subprocess.DEVNULL
        )
        wheels = (
            list(root.glob(f"*_{arch}.whl"))
            if target.endswith("apple-darwin")
            else list(root.glob("*.whl"))
        )
        (wheel,) = wheels
        venv = work / "venv"
        subprocess.run(["uv", "venv", str(venv)], check=True)
        install = ["uv", "pip", "install", "--python", str(venv / "bin/python")]
        if prefix:
            # Only Rosetta's Intel wheel needs a platform override. Native installs
            # use the interpreter's real OS/glibc compatibility tags.
            install.extend(["--python-platform", target])
        subprocess.run([*install, str(wheel)], check=True)
        cli = venv / "bin/onejudge"
        output = subprocess.check_output([*prefix, str(cli), "--version"], text=True)
        if output.strip() != f"onejudge {version}":
            raise ValueError("wheel installed version mismatch")
        subprocess.run(
            [*prefix, str(cli), "schema"], check=True, stdout=subprocess.DEVNULL
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    smoke(args.target, args.root, args.version)
