"""The real native packaging edge owns execution and its refusal journey."""

from __future__ import annotations

import copy
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
import tomllib
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

    def test_installed_wheel_with_a_different_real_executable_version_is_refused(self):
        # Keep the valid archive, but compile the actual CLI at a different Cargo
        # version and package it with the production builder. No application or
        # installer is doubled; only the fixture's release version differs.
        with tempfile.TemporaryDirectory(
            dir=os.environ.get("ONEPIPELINE_NODE_SCRATCH_DIR")
            or os.environ.get("RUNNER_TEMP")
        ) as scratch:
            work = Path(scratch)
            source = work / "source"
            source.mkdir()
            snapshot = work / "source.tar"
            subprocess.run(
                ["git", "archive", "HEAD", "--output", str(snapshot)],
                cwd=ROOT,
                check=True,
            )
            with tarfile.open(snapshot) as package:
                package.extractall(source, filter="data")
            metadata = json.loads(
                subprocess.check_output(
                    ["cargo", "metadata", "--no-deps", "--format-version", "1"],
                    cwd=ROOT,
                    text=True,
                )
            )
            members = {p["name"] for p in metadata["packages"]}
            manifest = source / "Cargo.toml"
            cargo = manifest.read_text()
            old = next(
                p["version"] for p in metadata["packages"] if p["name"] == "onejudge"
            )
            cargo = re.sub(
                rf'(?m)^version = "{re.escape(old)}"$', 'version = "999.0.0"', cargo
            )
            manifest.write_text(cargo)
            lock = source / "Cargo.lock"
            blocks = lock.read_text().split("[[package]]")
            for i, block in enumerate(blocks):
                name = re.search(r'(?m)^name = "([^\"]+)"$', block)
                if (
                    name
                    and name[1] in members
                    and not re.search(r"(?m)^source = ", block)
                ):
                    blocks[i] = re.sub(
                        rf'(?m)^version = "{re.escape(old)}"$',
                        'version = "999.0.0"',
                        block,
                    )
            lock.write_text("[[package]]".join(blocks))
            out = work / "products"
            env = {**os.environ, "CARGO_TARGET_DIR": str(ROOT / "target")}
            subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "pypi/build_release.py"),
                    "--target",
                    "host",
                    "--out",
                    str(out),
                ],
                cwd=source,
                env=env,
                check=True,
            )
            (original,) = (ROOT / "target/pypi-wheel").glob("*.tar.gz")
            shutil.copy2(original, out / original.name)
            host = original.name.removeprefix("onejudge-").removesuffix(".tar.gz")
            result = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "pypi/smoke_release.py"),
                    "--target",
                    host,
                    "--root",
                    str(out),
                    "--version",
                    old,
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("wheel installed version mismatch", result.stderr)
            self.assertNotIn("archive installed version mismatch", result.stderr)

    def test_unknown_target_is_refused_even_with_installable_real_products(self):
        # Repack the actual host archive under an unknown triple; its executable
        # and wheel still work on this host, so a missing-path error cannot pass.
        with tempfile.TemporaryDirectory(
            dir=os.environ.get("ONEPIPELINE_NODE_SCRATCH_DIR")
            or os.environ.get("RUNNER_TEMP")
        ) as scratch:
            products = Path(scratch)
            (archive,) = (ROOT / "target/pypi-wheel").glob("*.tar.gz")
            (wheel,) = (ROOT / "target/pypi-wheel").glob("*.whl")
            original_prefix = archive.name.removesuffix(".tar.gz")
            unknown = "imaginary-unknown-linux-gnu"
            with (
                tarfile.open(archive) as source,
                tarfile.open(products / f"onejudge-{unknown}.tar.gz", "w:gz") as output,
            ):
                for member in source.getmembers():
                    renamed = copy.copy(member)
                    renamed.name = member.name.replace(
                        original_prefix, f"onejudge-{unknown}", 1
                    )
                    output.addfile(
                        renamed, source.extractfile(member) if member.isfile() else None
                    )
            shutil.copy2(wheel, products / wheel.name)
            version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"][
                "package"
            ]["version"]
            result = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "pypi/smoke_release.py"),
                    "--target",
                    unknown,
                    "--root",
                    str(products),
                    "--version",
                    version,
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("unsupported smoke target", result.stderr)

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
