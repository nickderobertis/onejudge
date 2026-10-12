"""Internal Darwin release handoff, shared by the producer and exact-tag consumer."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import tarfile
import zipfile
from pathlib import Path, PurePosixPath

TARGETS = ("x86_64-apple-darwin", "aarch64-apple-darwin")
KINDS = ("archive", "cli-wheel")
SCHEMA_VERSION = 1


def identity(tag: str, sha: str, version: str) -> str:
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("sha must be a full git commit id")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?", version):
        raise ValueError("invalid Cargo version")
    if tag not in (f"v{version}", f"onejudge-v{version}"):
        raise ValueError("tag does not name the Cargo version")
    return f"onejudge-darwin-{tag}-{sha}"


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def payload(path: Path, kind: str) -> bytes:
    if kind == "archive":
        with tarfile.open(path) as package:
            names = package.getnames()
            (binary,) = [n for n in names if n.endswith("/onejudge")]
            if not all(
                any(n.endswith("/" + f) for n in names)
                for f in ("README.md", "LICENSE")
            ):
                raise ValueError("archive missing README/LICENSE")
            stream = package.extractfile(binary)
            if stream is None:
                raise ValueError("archive binary missing")
            return stream.read()
    with zipfile.ZipFile(path) as package:
        (binary,) = [
            n for n in package.namelist() if n.endswith(".data/scripts/onejudge")
        ]
        return package.read(binary)


def validate(root: Path, manifest: dict, tag: str, sha: str, version: str) -> dict:
    identity(tag, sha, version)
    if (
        type(manifest.get("schema_version")) is not int
        or manifest["schema_version"] != SCHEMA_VERSION
    ):
        raise ValueError("unsupported release manifest schema")
    for key, expected in (("tag", tag), ("sha", sha), ("version", version)):
        if manifest.get(key) != expected:
            raise ValueError(f"release manifest {key} mismatch")
    files = manifest.get("files")
    if not isinstance(files, list):
        raise TypeError("manifest files must be a list")
    seen, paths, binaries = set(), set(), {}
    for row in files:
        if not isinstance(row, dict):
            raise TypeError("invalid file entry")
        path, target, kind, checksum = (
            row.get(k) for k in ("path", "target", "kind", "sha256")
        )
        if (
            not isinstance(path, str)
            or not path
            or "\\" in path
            or PurePosixPath(path).is_absolute()
            or any(p in ("..", ".", "") for p in path.split("/"))
        ):
            raise ValueError("unsafe manifest path")
        if (
            target not in TARGETS
            or kind not in KINDS
            or (target, kind) in seen
            or path in paths
        ):
            raise ValueError("unknown or duplicate target/kind/path")
        file = root / path
        if (
            file.is_symlink()
            or not file.is_file()
            or root.resolve() not in file.resolve().parents
        ):
            raise ValueError("missing/unsafe payload")
        if (
            not isinstance(checksum, str)
            or not re.fullmatch("[0-9a-f]{64}", checksum)
            or digest(file) != checksum
        ):
            raise ValueError("payload hash mismatch")
        if kind == "archive":
            if file.name != f"onejudge-{target}.tar.gz":
                raise ValueError("archive filename mismatch")
            with tarfile.open(file) as archive:
                expected = {
                    f"onejudge-{target}/{n}"
                    for n in ("onejudge", "README.md", "LICENSE")
                }
                members = archive.getmembers()
                if {m.name for m in members if m.isfile()} != expected or any(
                    m.issym() or m.islnk() for m in members
                ):
                    raise ValueError("archive contents mismatch")
        if kind == "cli-wheel":
            arch = "x86_64" if target.startswith("x86_64") else "arm64"
            if not re.fullmatch(
                rf"onejudge_cli-{re.escape(version)}-py3-none-macosx_\d+_\d+_{arch}\.whl",
                file.name,
            ):
                raise ValueError("wheel version/architecture mismatch")
            with zipfile.ZipFile(file) as wheel:
                (metadata,) = [
                    n for n in wheel.namelist() if n.endswith(".dist-info/METADATA")
                ]
                if f"Version: {version}\n" not in wheel.read(metadata).decode():
                    raise ValueError("wheel metadata version mismatch")
        binary = payload(file, kind)
        cpu = 0x01000007 if target.startswith("x86_64") else 0x0100000C
        if (
            binary[:4] != b"\xcf\xfa\xed\xfe"
            or int.from_bytes(binary[4:8], "little") != cpu
        ):
            raise ValueError("binary Mach-O architecture mismatch")
        binary_hash = hashlib.sha256(binary).hexdigest()
        if target in binaries and binaries[target] != binary_hash:
            raise ValueError("archive/wheel binary reuse mismatch")
        binaries[target] = binary_hash
        paths.add(path)
        seen.add((target, kind))
    if seen != {(target, kind) for target in TARGETS for kind in KINDS}:
        raise ValueError("missing Darwin target/package kind")
    return manifest


def stage(root: Path, tag: str, sha: str, version: str) -> dict:
    rows = []
    for target in TARGETS:
        arch = "x86_64" if target.startswith("x86_64") else "arm64"
        (wheel,) = root.glob(f"onejudge_cli-{version}-*-macosx_*_{arch}.whl")
        for kind, file in (
            ("archive", root / f"onejudge-{target}.tar.gz"),
            ("cli-wheel", wheel),
        ):
            rows.append(
                {
                    "path": file.name,
                    "target": target,
                    "kind": kind,
                    "sha256": digest(file),
                }
            )
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "tag": tag,
        "sha": sha,
        "version": version,
        "files": rows,
    }
    validate(root, manifest, tag, sha, version)
    (root / "release-artifacts.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("stage", "verify", "name"))
    parser.add_argument("--root", type=Path, default=Path("dist"))
    for key in ("tag", "sha", "version"):
        parser.add_argument("--" + key, required=True)
    args = parser.parse_args()
    name = identity(args.tag, args.sha, args.version)
    if args.command == "stage":
        stage(args.root, args.tag, args.sha, args.version)
    elif args.command == "verify":
        validate(
            args.root,
            json.loads((args.root / "release-artifacts.json").read_text()),
            args.tag,
            args.sha,
            args.version,
        )
    print(name)
