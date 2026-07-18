"""Build and consume the SDK wheel through its public import and real CLI."""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
SDK = ROOT / "python" / "onejudge-sdk"


def main() -> None:
    """Inspect, install, and execute the built SDK wheel."""
    wheel_dir = Path(tempfile.mkdtemp(prefix="onejudge-sdk-wheelhouse-"))
    subprocess.run(["uv", "build", "--wheel", "--out-dir", str(wheel_dir), str(SDK)], check=True)
    wheels = list(wheel_dir.glob("onejudge_sdk-*.whl"))
    if len(wheels) != 1:
        raise AssertionError(f"expected one SDK wheel, found {wheels}")
    with zipfile.ZipFile(wheels[0]) as archive:
        names = archive.namelist()
        if not any(name.endswith("onejudge_sdk/_generated/schemas.json") for name in names):
            raise AssertionError("SDK wheel omitted generated runtime schemas")

    environment = wheel_dir / "venv"
    subprocess.run(["uv", "venv", "--offline", "--python", sys.executable, str(environment)], check=True)
    scripts = environment / ("Scripts" if os.name == "nt" else "bin")
    python = scripts / ("python.exe" if os.name == "nt" else "python")
    subprocess.run(
        ["uv", "pip", "install", "--offline", "--python", str(python), "--no-deps", str(wheels[0])],
        check=True,
    )
    subprocess.run(
        ["uv", "pip", "install", "--offline", "--python", str(python), "jsonschema>=4.18,<5"],
        check=True,
    )
    consumer = """
import asyncio
import sys
from onejudge_sdk import OneJudge, __version__

async def main():
    config = {"provider": {"kind": "command", "command": [sys.argv[2]]}}
    result = await OneJudge(executable=sys.argv[1]).run(config, "installed SDK")
    assert __version__ == "0.3.0"
    assert result.completed and result.assistant_turns == 1

asyncio.run(main())
"""
    suffix = ".exe" if os.name == "nt" else ""
    subprocess.run(
        [
            str(python),
            "-c",
            consumer,
            str(ROOT / "target" / "debug" / f"onejudge{suffix}"),
            str(ROOT / "target" / "debug" / f"onejudge-echo-provider{suffix}"),
        ],
        check=True,
    )


if __name__ == "__main__":
    main()
