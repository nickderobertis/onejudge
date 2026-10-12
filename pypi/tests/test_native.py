"""The real native packaging edge owns execution and its refusal journey."""

from __future__ import annotations

import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class NativeProducts(unittest.TestCase):
    def test_native_installer_rejects_the_wrong_release_version(self):
        host = next(
            line.removeprefix("host: ")
            for line in subprocess.check_output(
                ["rustc", "-vV"], text=True
            ).splitlines()
            if line.startswith("host: ")
        )
        result = subprocess.run(
            [
                sys.executable,
                str(ROOT / "pypi/smoke_release.py"),
                "--target",
                host,
                "--root",
                str(ROOT / "target/pypi-wheel"),
                "--version",
                "999.0.0",
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("archive installed version mismatch", result.stderr)

    def test_cli_wheel_contains_no_private_pipeline_helpers(self):
        import zipfile

        (wheel,) = (ROOT / "target/pypi-wheel").glob("*.whl")
        with zipfile.ZipFile(wheel) as package:
            self.assertFalse(
                any(
                    "pypi/" in name or name.endswith(".py")
                    for name in package.namelist()
                )
            )

    def test_source_distribution_contains_no_private_pipeline_helpers(self):
        with tempfile.TemporaryDirectory(
            dir=os.environ.get("ONEPIPELINE_NODE_SCRATCH_DIR")
            or os.environ.get("RUNNER_TEMP")
        ) as scratch:
            subprocess.run(
                ["uvx", "maturin", "sdist", "--out", scratch],
                cwd=ROOT,
                check=True,
                stdout=subprocess.DEVNULL,
            )
            (archive,) = Path(scratch).glob("*.tar.gz")
            with tarfile.open(archive) as package:
                self.assertFalse(any("/pypi/" in name for name in package.getnames()))
