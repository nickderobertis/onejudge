"""Assemble onejudge with the workspace release version."""

from __future__ import annotations

import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
SOURCE = ROOT / "python" / "onejudge-sdk"
OUTPUT = ROOT / "python" / "dist" / "onejudge-sdk"
PLACEHOLDER = "0.0.0.dev0"


def cargo_version() -> str:
    """Read the release-plz-owned workspace version."""
    in_workspace_package = False
    for line in (ROOT / "Cargo.toml").read_text(encoding="utf-8").splitlines():
        if line.startswith("["):
            in_workspace_package = line == "[workspace.package]"
        elif in_workspace_package and line.startswith("version = "):
            return line.split('"')[1]
    raise RuntimeError("Cargo.toml has no [workspace.package] version")


def replace_once(path: Path, old: str, new: str) -> None:
    """Replace one required placeholder, rejecting ambiguous package metadata."""
    source = path.read_text(encoding="utf-8")
    if source.count(old) != 1:
        raise RuntimeError(f"{path}: expected exactly one {old!r} placeholder")
    path.write_text(source.replace(old, new), encoding="utf-8")


def main() -> None:
    """Copy the SDK and stamp all package-visible versions from Cargo."""
    shutil.rmtree(OUTPUT, ignore_errors=True)
    shutil.copytree(SOURCE, OUTPUT, ignore=shutil.ignore_patterns("__pycache__"))
    version = cargo_version()
    project = OUTPUT / "pyproject.toml"
    replace_once(project, f'version = "{PLACEHOLDER}"', f'version = "{version}"')
    replace_once(
        project,
        f'"onejudge-cli=={PLACEHOLDER}"',
        f'"onejudge-cli=={version}"',
    )
    replace_once(
        OUTPUT / "src" / "onejudge_sdk" / "_version.py",
        f'__version__ = "{PLACEHOLDER}"',
        f'__version__ = "{version}"',
    )
    print(OUTPUT)


if __name__ == "__main__":
    main()
