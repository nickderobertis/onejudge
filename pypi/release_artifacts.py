"""Internal Darwin release handoff, shared by the producer and exact-tag consumer."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import tarfile
import zipfile
from dataclasses import dataclass
from enum import Enum
from pathlib import Path, PurePosixPath
from typing import NewType

Tag = NewType("Tag", str)
Commit = NewType("Commit", str)
Version = NewType("Version", str)
ArtifactName = NewType("ArtifactName", str)
RelativePath = NewType("RelativePath", str)
Checksum = NewType("Checksum", str)


class Target(str, Enum):
    INTEL = "x86_64-apple-darwin"
    ARM = "aarch64-apple-darwin"

    def __str__(self) -> str:
        return self.value


class Kind(str, Enum):
    ARCHIVE = "archive"
    WHEEL = "cli-wheel"


TARGETS = tuple(Target)
KINDS = tuple(Kind)
SCHEMA_VERSION = 1


@dataclass(frozen=True)
class ReleaseIdentity:
    tag: Tag
    sha: Commit
    version: Version

    def __post_init__(self) -> None:
        if not isinstance(self.sha, str) or not re.fullmatch(r"[0-9a-f]{40}", self.sha):
            raise ValueError("sha must be a full git commit id")
        if not isinstance(self.version, str) or not re.fullmatch(
            r"\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?", self.version
        ):
            raise ValueError("invalid Cargo version")
        if self.tag not in (f"v{self.version}", f"onejudge-v{self.version}"):
            raise ValueError("tag does not name the Cargo version")

    @property
    def name(self) -> ArtifactName:
        return ArtifactName(f"onejudge-darwin-{self.tag}-{self.sha}")


def identity(tag: str, sha: str, version: str) -> ArtifactName:
    return ReleaseIdentity(Tag(tag), Commit(sha), Version(version)).name


@dataclass(frozen=True)
class ArtifactFile:
    path: RelativePath
    target: Target
    kind: Kind
    sha256: Checksum

    def to_wire(self) -> dict[str, str]:
        return {
            "path": self.path,
            "target": self.target.value,
            "kind": self.kind.value,
            "sha256": self.sha256,
        }


@dataclass(frozen=True)
class Manifest:
    identity: ReleaseIdentity
    files: tuple[ArtifactFile, ...]

    def to_wire(self) -> dict[str, object]:
        return {
            "schema_version": SCHEMA_VERSION,
            "tag": self.identity.tag,
            "sha": self.identity.sha,
            "version": self.identity.version,
            "files": [f.to_wire() for f in self.files],
        }

    @classmethod
    def parse(cls, raw: object) -> Manifest:
        if not isinstance(raw, dict):
            raise TypeError("manifest must be an object")
        if (
            type(raw.get("schema_version")) is not int
            or raw["schema_version"] != SCHEMA_VERSION
        ):
            raise ValueError("unsupported release manifest schema")
        fields = [raw.get(k) for k in ("tag", "sha", "version")]
        if not all(isinstance(v, str) for v in fields):
            raise TypeError("manifest identity fields must be strings")
        tag, sha, version = fields
        release = ReleaseIdentity(Tag(tag), Commit(sha), Version(version))
        rows = raw.get("files")
        if not isinstance(rows, list):
            raise TypeError("manifest files must be a list")
        files = []
        for row in rows:
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
                not isinstance(target, str)
                or not isinstance(kind, str)
                or target not in TARGETS
                or kind not in KINDS
            ):
                raise ValueError("unknown target/kind")
            if not isinstance(checksum, str) or not re.fullmatch(
                "[0-9a-f]{64}", checksum
            ):
                raise ValueError("invalid payload hash")
            files.append(
                ArtifactFile(
                    RelativePath(path), Target(target), Kind(kind), Checksum(checksum)
                )
            )
        return cls(release, tuple(files))


def digest(path: Path) -> Checksum:
    return Checksum(hashlib.sha256(path.read_bytes()).hexdigest())


def payload(path: Path, kind: Kind | str) -> bytes:
    match Kind(kind):
        case Kind.ARCHIVE:
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
        case Kind.WHEEL:
            with zipfile.ZipFile(path) as package:
                (binary,) = [
                    n
                    for n in package.namelist()
                    if n.endswith(".data/scripts/onejudge")
                ]
                return package.read(binary)
    raise ValueError("unsupported package kind")


def validate(root: Path, raw: object, tag: str, sha: str, version: str) -> Manifest:
    expected = ReleaseIdentity(Tag(tag), Commit(sha), Version(version))
    manifest = raw if isinstance(raw, Manifest) else Manifest.parse(raw)
    for key in ("tag", "sha", "version"):
        if getattr(manifest.identity, key) != getattr(expected, key):
            raise ValueError(f"release manifest {key} mismatch")
    seen, paths, binaries = set(), set(), {}
    for row in manifest.files:
        if (row.target, row.kind) in seen or row.path in paths:
            raise ValueError("duplicate target/kind/path")
        file = root / row.path
        if (
            file.is_symlink()
            or not file.is_file()
            or root.resolve() not in file.resolve().parents
        ):
            raise ValueError("missing/unsafe payload")
        if digest(file) != row.sha256:
            raise ValueError("payload hash mismatch")
        match row.kind:
            case Kind.ARCHIVE:
                if file.name != f"onejudge-{row.target}.tar.gz":
                    raise ValueError("archive filename mismatch")
                with tarfile.open(file) as archive:
                    contents = {
                        f"onejudge-{row.target}/{n}"
                        for n in ("onejudge", "README.md", "LICENSE")
                    }
                    members = archive.getmembers()
                    if {m.name for m in members if m.isfile()} != contents or any(
                        m.issym() or m.islnk() for m in members
                    ):
                        raise ValueError("archive contents mismatch")
            case Kind.WHEEL:
                arch = "x86_64" if row.target is Target.INTEL else "arm64"
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
        binary = payload(file, row.kind)
        cpu = 0x01000007 if row.target is Target.INTEL else 0x0100000C
        if (
            binary[:4] != b"\xcf\xfa\xed\xfe"
            or int.from_bytes(binary[4:8], "little") != cpu
        ):
            raise ValueError("binary Mach-O architecture mismatch")
        binary_hash = hashlib.sha256(binary).hexdigest()
        if row.target in binaries and binaries[row.target] != binary_hash:
            raise ValueError("archive/wheel binary reuse mismatch")
        binaries[row.target] = binary_hash
        paths.add(row.path)
        seen.add((row.target, row.kind))
    if seen != {(target, kind) for target in TARGETS for kind in KINDS}:
        raise ValueError("missing Darwin target/package kind")
    declared = {str(row.path) for row in manifest.files} | {"release-artifacts.json"}
    observed = {
        file.relative_to(root).as_posix()
        for file in root.rglob("*")
        if file.is_file() or file.is_symlink()
    }
    if not observed <= declared:
        raise ValueError(
            "unlisted release payload: " + ", ".join(sorted(observed - declared))
        )
    return manifest


def stage(root: Path, tag: str, sha: str, version: str) -> Manifest:
    rows = []
    for target in TARGETS:
        arch = "x86_64" if target is Target.INTEL else "arm64"
        (wheel,) = root.glob(f"onejudge_cli-{version}-*-macosx_*_{arch}.whl")
        for kind, file in (
            (Kind.ARCHIVE, root / f"onejudge-{target}.tar.gz"),
            (Kind.WHEEL, wheel),
        ):
            rows.append(
                ArtifactFile(RelativePath(file.name), target, kind, digest(file))
            )
    manifest = Manifest(
        ReleaseIdentity(Tag(tag), Commit(sha), Version(version)), tuple(rows)
    )
    validate(root, manifest, tag, sha, version)
    (root / "release-artifacts.json").write_text(
        json.dumps(manifest.to_wire(), indent=2) + "\n"
    )
    return manifest


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("stage", "verify", "name"))
    parser.add_argument("--root", type=Path, default=Path("dist"))
    for key in ("tag", "sha", "version"):
        parser.add_argument("--" + key, required=True)
    args = parser.parse_args()
    name = identity(args.tag, args.sha, args.version)
    match args.command:
        case "stage":
            stage(args.root, args.tag, args.sha, args.version)
        case "verify":
            validate(
                args.root,
                json.loads((args.root / "release-artifacts.json").read_text()),
                args.tag,
                args.sha,
                args.version,
            )
        case "name":
            pass
    print(name)
