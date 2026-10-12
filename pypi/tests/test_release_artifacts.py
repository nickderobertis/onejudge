"""Real command journeys; only the GitHub transport is doubled on PATH."""

import copy
import hashlib
import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "pypi"


class ReleaseArtifacts(unittest.TestCase):
    def setUp(self):
        scratch = os.environ.get("ONEPIPELINE_NODE_SCRATCH_DIR") or os.environ.get(
            "RUNNER_TEMP"
        )
        self.temp = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.dist = self.root / "dist"
        self.dist.mkdir()
        self.repo = self.root / "repo"
        self.repo.mkdir()
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "config", "user.email", "test@example.com"],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(self.repo), "config", "user.name", "Release fixture"],
            check=True,
        )
        (self.repo / "Cargo.toml").write_text(
            '[workspace.package]\nversion = "0.19.0"\n'
        )
        subprocess.run(["git", "-C", str(self.repo), "add", "."], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "commit", "-qm", "ci: fixture"], check=True
        )
        subprocess.run(["git", "-C", str(self.repo), "tag", "v0.19.0"], check=True)
        self.sha = subprocess.check_output(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"], text=True
        ).strip()
        self.version = "0.19.0"
        self.tag = "v0.19.0"
        self.identity = f"onejudge-darwin-{self.tag}-{self.sha}"
        self.targets = ("x86_64-apple-darwin", "aarch64-apple-darwin")
        # Mach-O contract specimens are never executed as native binaries.
        for target, arch, cpu in (
            (self.targets[0], "x86_64", 0x01000007),
            (self.targets[1], "arm64", 0x0100000C),
        ):
            binary = (
                b"\xcf\xfa\xed\xfe" + cpu.to_bytes(4, "little") + b"contract specimen"
            )
            with tarfile.open(
                self.dist / f"onejudge-{target}.tar.gz", "w:gz"
            ) as package:
                for name, data in (
                    ("onejudge", binary),
                    ("README.md", b"README"),
                    ("LICENSE", b"MIT"),
                ):
                    member = tarfile.TarInfo(f"onejudge-{target}/{name}")
                    member.size = len(data)
                    package.addfile(member, io.BytesIO(data))
            with zipfile.ZipFile(
                self.dist
                / f"onejudge_cli-{self.version}-py3-none-macosx_11_0_{arch}.whl",
                "w",
            ) as package:
                package.writestr(
                    f"onejudge_cli-{self.version}.data/scripts/onejudge", binary
                )
                package.writestr(
                    f"onejudge_cli-{self.version}.dist-info/METADATA",
                    f"Name: onejudge-cli\nVersion: {self.version}\n",
                )
        self.command("stage")
        self.original = json.loads((self.dist / "release-artifacts.json").read_text())

    def command(self, command, **identity):
        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPTS / "release_artifacts.py"),
                command,
                "--root",
                str(self.dist),
                "--tag",
                identity.get("tag", self.tag),
                "--sha",
                identity.get("sha", self.sha),
                "--version",
                identity.get("version", self.version),
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        if command == "stage":
            self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def test_stage_verify_name_and_manifest_bytes(self):
        self.assertEqual(self.command("verify").returncode, 0)
        self.assertEqual(self.command("name").stdout.strip(), self.identity)
        self.assertEqual(
            set(self.original), {"schema_version", "tag", "sha", "version", "files"}
        )
        self.assertEqual(
            (self.original["tag"], self.original["sha"], self.original["version"]),
            (self.tag, self.sha, self.version),
        )
        for row in self.original["files"]:
            self.assertEqual(set(row), {"path", "target", "kind", "sha256"})
        self.assertEqual(self.original["schema_version"], 1)
        self.assertEqual(
            {(f["target"], f["kind"]) for f in self.original["files"]},
            {
                (target, kind)
                for target in self.targets
                for kind in ("archive", "cli-wheel")
            },
        )
        for row in self.original["files"]:
            self.assertEqual(
                row["sha256"],
                hashlib.sha256((self.dist / row["path"]).read_bytes()).hexdigest(),
            )

    def test_rejects_tag_sha_version_and_tampered_payload(self):
        for key, value in (
            ("tag", "onejudge-v0.19.0"),
            ("sha", "b" * 40),
            ("version", "0.20.0"),
        ):
            result = self.command("verify", **{key: value})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(
                "mismatch" if key != "version" else "does not name", result.stderr
            )
        path = self.dist / self.original["files"][0]["path"]
        path.write_bytes(path.read_bytes() + b"altered")
        result = self.command("verify")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("hash mismatch", result.stderr)

    def test_rejects_missing_platform_traversal_duplicate_and_versioned_shape(self):
        for edit, message in (
            (lambda m: m["files"].pop(), "missing Darwin"),
            (lambda m: m["files"][0].update(path="../outside"), "unsafe manifest path"),
            (lambda m: m["files"].append(m["files"][0]), "duplicate"),
            (lambda m: m.update(schema_version=True), "schema"),
        ):
            manifest = copy.deepcopy(self.original)
            edit(manifest)
            (self.dist / "release-artifacts.json").write_text(json.dumps(manifest))
            result = self.command("verify")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(message, result.stderr)

    def test_rejects_binary_reuse_or_architecture_even_with_valid_payload_hash(self):
        row = next(r for r in self.original["files"] if r["kind"] == "cli-wheel")
        file = self.dist / row["path"]
        with zipfile.ZipFile(file) as wheel:
            contents = {n: wheel.read(n) for n in wheel.namelist()}
        binary = next(n for n in contents if n.endswith("/onejudge"))
        for cpu, message in ((0x0100000C, "architecture"), (0x01000007, "reuse")):
            contents[binary] = (
                b"\xcf\xfa\xed\xfe" + cpu.to_bytes(4, "little") + b"different binary"
            )
            with zipfile.ZipFile(file, "w") as wheel:
                for name, data in contents.items():
                    wheel.writestr(name, data)
            row["sha256"] = hashlib.sha256(file.read_bytes()).hexdigest()
            (self.dist / "release-artifacts.json").write_text(json.dumps(self.original))
            result = self.command("verify")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(message, result.stderr)

    def transport(self, mode):
        bin_dir = self.root / "bin"
        bin_dir.mkdir(exist_ok=True)
        executable = bin_dir / "gh"
        executable.write_text(
            (Path(__file__).with_name("github_double.py")).read_text()
        )
        executable.chmod(0o755)
        return {
            **os.environ,
            "PATH": str(bin_dir) + os.pathsep + os.environ["PATH"],
            "FIXTURE": str(self.root),
            "MODE": mode,
            "ARTIFACT": self.identity,
            "SHA": self.sha,
            "TOOLING": str(SCRIPTS),
        }

    def fetch(self, mode):
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPTS / "fetch_release_artifacts.py"),
                "--repo",
                "nickderobertis/onejudge",
                "--tag",
                self.tag,
                "--sha",
                self.sha,
                "--version",
                self.version,
                "--root",
                str(self.root / "consumed"),
                "--attempts",
                "2" if mode in ("ordering", "competing-ordering") else "1",
            ],
            cwd=self.repo,
            env=self.transport(mode),
            capture_output=True,
            text=True,
            check=False,
        )

    def test_exact_producer_and_durable_recovery_execute_real_verifier(self):
        result = self.fetch("success")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('"42"', (self.root / "calls").read_text())
        with zipfile.ZipFile(self.root / f"{self.identity}.zip", "w") as bundle:
            for file in self.dist.iterdir():
                bundle.write(file, file.name)
        (self.root / "calls").unlink()
        result = self.fetch("durable")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("/workflows/", (self.root / "calls").read_text())

    def test_alias_recovery_uses_existing_release_and_rejects_conflicting_refs(self):
        subprocess.run(
            ["git", "-C", str(self.repo), "tag", "onejudge-v0.19.0"], check=True
        )
        with zipfile.ZipFile(self.root / f"{self.identity}.zip", "w") as bundle:
            for file in self.dist.iterdir():
                bundle.write(file, file.name)
        self.tag = "onejudge-v0.19.0"
        result = self.fetch("alias")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            '"download", "onejudge-v0.19.0"', (self.root / "calls").read_text()
        )
        result = self.fetch("no-release")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no existing Release", result.stderr)
        (self.repo / "changed").write_text("different head")
        subprocess.run(["git", "-C", str(self.repo), "add", "."], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "commit", "-qm", "ci: different head"],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(self.repo), "tag", "-f", "onejudge-v0.19.0"],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        result = self.fetch("alias")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("do not agree with exact SHA", result.stderr)

    def test_same_version_partial_registry_recovery_reuses_verified_release_bundle(
        self,
    ):
        with zipfile.ZipFile(self.root / f"{self.identity}.zip", "w") as bundle:
            for file in self.dist.iterdir():
                bundle.write(file, file.name)
        env = self.transport("durable")
        curl = self.root / "bin" / "curl"
        curl.write_text(
            "#!/usr/bin/env python3\nimport sys\nprint('404' if '/onejudge-cli/' in sys.argv[-1] else '200',end='')\n"
        )
        curl.chmod(0o755)
        event, output = self.root / "event.json", self.root / "plan.out"
        event.write_text(json.dumps({"inputs": {"tag": self.tag}}))
        env.update(
            GITHUB_EVENT_NAME="workflow_dispatch",
            GITHUB_EVENT_PATH=str(event),
            GITHUB_OUTPUT=str(output),
        )
        plan = subprocess.run(
            [str(ROOT / "scripts/release-pypi-plan.sh")],
            cwd=self.repo,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(plan.returncode, 0, plan.stderr)
        values = dict(line.split("=", 1) for line in output.read_text().splitlines())
        self.assertEqual(values["sha"], self.sha)
        self.assertEqual(values["version"], self.version)
        self.assertEqual(values["upload-cli"], "true")
        self.assertEqual(values["upload-sdk"], "false")
        result = self.fetch("durable")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("/workflows/", (self.root / "calls").read_text())

    def test_wrong_run_wrong_artifact_and_expired_outputs_fail_visibly(self):
        for mode, expected in (
            ("wrong-run", "producer absent"),
            ("wrong-artifact", "no unique unexpired artifact"),
            ("expired", "no unique unexpired artifact"),
        ):
            result = self.fetch(mode)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(expected, result.stderr)

    def publisher(self, mode="success", archive_only=False):
        directory = self.dist
        expected_targets = ("x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc")
        if archive_only:
            directory = self.root / "archives"
            directory.mkdir(exist_ok=True)
            # Non-native compatibility specimens are not executed. The native
            # installer journey separately drives the real host-built archive.
            linux = b"\x7fELF\x02\x01" + b"\0" * 12 + (62).to_bytes(2, "little")
            windows = (
                b"MZ"
                + b"\0" * 58
                + (64).to_bytes(4, "little")
                + b"PE\0\0"
                + (0x8664).to_bytes(2, "little")
            )
            if mode == "wrong-architecture":
                linux = linux[:18] + (183).to_bytes(2, "little")
            if mode == "wrong-pe-signature":
                windows = windows[:64] + b"BAD!" + windows[68:]
            if mode == "wrong-pe-offset":
                windows = windows[:60] + (9999).to_bytes(4, "little") + windows[64:]
            if mode == "wrong-pe-machine":
                windows = windows[:68] + (0xAA64).to_bytes(2, "little")
            for target, binary in zip(expected_targets, (linux, windows), strict=True):
                prefix = f"onejudge-{target}"
                executable = (
                    "onejudge.exe" if target.endswith("windows-msvc") else "onejudge"
                )
                contents = {
                    executable: binary,
                    "README.md": b"README",
                    "LICENSE": b"MIT",
                }
                if mode == "corrupt-contents":
                    contents.pop("LICENSE")
                if target.endswith("windows-msvc"):
                    with zipfile.ZipFile(directory / f"{prefix}.zip", "w") as package:
                        for name, value in contents.items():
                            package.writestr(f"{prefix}/{name}", value)
                else:
                    with tarfile.open(
                        directory / f"{prefix}.tar.gz", "w:gz"
                    ) as package:
                        for name, value in contents.items():
                            member = tarfile.TarInfo(f"{prefix}/{name}")
                            member.size = len(value)
                            package.addfile(member, io.BytesIO(value))
        if archive_only and mode not in ("wrong-architecture", "corrupt-contents"):
            native = ROOT / "target/pypi-wheel/onejudge-x86_64-unknown-linux-gnu.tar.gz"
            if native.exists():
                (directory / native.name).write_bytes(native.read_bytes())
        if archive_only and mode == "symlink-archive":
            file = directory / "onejudge-x86_64-pc-windows-msvc.zip"
            original = self.root / "original-windows.zip"
            file.replace(original)
            file.symlink_to(original)
        if archive_only and mode == "missing-archive":
            (directory / "onejudge-x86_64-pc-windows-msvc.zip").unlink()
        args = [
            sys.executable,
            str(SCRIPTS / "publish_release.py"),
            "--repo",
            "nickderobertis/onejudge",
            "--tag",
            "v0.19.0",
            "--sha",
            self.sha,
            "--version",
            self.version,
            "--root",
            str(directory),
        ]
        if archive_only:
            args.extend(["--archive-only", "--targets", " ".join(expected_targets)])
        return subprocess.run(
            args,
            cwd=self.repo,
            env=self.transport(mode),
            capture_output=True,
            text=True,
            check=False,
        )

    def test_real_publisher_verifies_zips_uploads_and_supports_retention_recovery(self):
        result = self.publisher()
        self.assertEqual(result.returncode, 0, result.stderr)
        with zipfile.ZipFile(self.root / f"{self.identity}.zip") as bundle:
            self.assertEqual(
                set(bundle.namelist()),
                {
                    "release-artifacts.json",
                    *(f["path"] for f in self.original["files"]),
                },
            )
            for name in bundle.namelist():
                self.assertEqual(bundle.read(name), (self.dist / name).read_bytes())
        result = self.fetch("durable")
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.publisher("upload-failure")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("upload rejected", result.stderr)
        self.original["sha"] = "b" * 40
        (self.dist / "release-artifacts.json").write_text(json.dumps(self.original))
        result = self.publisher()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("sha mismatch", result.stderr)

    def test_real_archive_publish_composition_uploads_staged_products_and_refuses_failure(
        self,
    ):
        result = self.publisher(archive_only=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = (self.root / "calls").read_text()
        self.assertIn("onejudge-x86_64-unknown-linux-gnu.tar.gz", calls)
        self.assertIn("onejudge-x86_64-pc-windows-msvc.zip", calls)
        for mode, message in (
            ("missing-archive", "missing required targets"),
            ("wrong-architecture", "ELF architecture mismatch"),
            ("corrupt-contents", "archive contents mismatch"),
            ("wrong-pe-signature", "PE architecture mismatch"),
            ("wrong-pe-offset", "PE architecture mismatch"),
            ("wrong-pe-machine", "PE architecture mismatch"),
            ("symlink-archive", "unsafe archive payload"),
        ):
            failed = self.publisher(mode, archive_only=True)
            self.assertNotEqual(failed.returncode, 0, mode)
            self.assertIn(message, failed.stderr)
            if mode == "symlink-archive":
                (self.root / "archives/onejudge-x86_64-pc-windows-msvc.zip").unlink()
        result = self.publisher("upload-failure", archive_only=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("upload rejected", result.stderr)
        (self.root / "archives" / "unrelated.txt").write_text("not a released product")
        result = self.publisher(archive_only=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpected products", result.stderr)

    def test_typed_transport_refuses_invalid_assets_runs_artifacts_and_release_answers(
        self,
    ):
        for mode, message in (
            ("invalid-assets", "assets must be a list"),
            ("invalid-runs", "id must be a positive integer"),
            ("invalid-artifact", "expired must be a boolean"),
            ("wrong-release", "Release tag wrong"),
            ("transport-failure", "HTTP 503"),
            ("ambiguous", "ambiguous durable"),
            ("invalid-release-object", "Release must be an object"),
            ("invalid-asset-object", "asset must be an object"),
            ("empty-asset-name", "asset name must be a nonempty string"),
            ("nonstring-asset-name", "asset name must be a nonempty string"),
            ("invalid-run-object", "workflow run must be an object"),
            ("invalid-run-collection", "workflow_runs must be a list"),
            ("invalid-run-response", "workflow_runs must be an object"),
            ("malformed-sha", "invalid workflow run head_sha"),
            ("unknown-status", "not a valid RunStatus"),
            ("unknown-conclusion", "not a valid Conclusion"),
            ("missing-conclusion", "completed workflow run has no conclusion"),
            ("invalid-artifact-collection", "artifacts must be a list"),
            ("invalid-artifact-object", "artifact must be an object"),
            ("empty-artifact-name", "artifact name must be a nonempty string"),
            ("nonstring-artifact-name", "artifact name must be a nonempty string"),
        ):
            result = self.fetch(mode)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(message, result.stderr)

    def test_verified_consumer_refuses_unlisted_wheels_from_an_unrelated_output(self):
        (self.dist / "onejudge_cli-9.9.9-py3-none-macosx_11_0_arm64.whl").write_bytes(
            b"unverified wheel"
        )
        result = self.command("verify")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unlisted release payload", result.stderr)
        result = self.fetch("success")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unlisted release payload", result.stderr)

    def test_durable_zip_refuses_traversal(self):
        with zipfile.ZipFile(self.root / f"{self.identity}.zip", "w") as bundle:
            bundle.writestr("../outside", "not an artifact")
        result = self.fetch("unsafe-zip")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsafe artifact zip path", result.stderr)
        self.assertFalse((self.root / "outside").exists())

    def test_malformed_manifest_records_identity_and_symlinks_fail_at_boundary(self):
        for value in (None, "file", {}):
            manifest = copy.deepcopy(self.original)
            manifest["files"][0] = value
            (self.dist / "release-artifacts.json").write_text(json.dumps(manifest))
            self.assertNotEqual(self.command("verify").returncode, 0)
        for key, value in (
            ("sha", "short"),
            ("tag", "not-a-release"),
            ("version", "invalid"),
        ):
            (self.dist / "release-artifacts.json").write_text(json.dumps(self.original))
            self.assertNotEqual(self.command("verify", **{key: value}).returncode, 0)
        (self.dist / "release-artifacts.json").write_text(json.dumps(self.original))
        file = self.dist / self.original["files"][0]["path"]
        outside = self.root / "outside.tar.gz"
        outside.write_bytes(file.read_bytes())
        file.unlink()
        file.symlink_to(outside)
        result = self.command("verify")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsafe payload", result.stderr)

    def test_archive_contents_and_wheel_metadata_or_filename_mismatch_fail_even_with_valid_hashes(
        self,
    ):
        for kind in ("archive", "cli-wheel"):
            row = next(r for r in self.original["files"] if r["kind"] == kind)
            file = self.dist / row["path"]
            saved = file.read_bytes()
            if kind == "archive":
                with tarfile.open(file, "w:gz") as archive:
                    member = tarfile.TarInfo("unexpected")
                    member.size = 3
                    archive.addfile(member, io.BytesIO(b"bad"))
            else:
                with zipfile.ZipFile(file) as package:
                    contents = {n: package.read(n) for n in package.namelist()}
                metadata = next(n for n in contents if n.endswith("METADATA"))
                contents[metadata] = b"Version: 9.9.9\n"
                with zipfile.ZipFile(file, "w") as package:
                    for name, value in contents.items():
                        package.writestr(name, value)
            row["sha256"] = hashlib.sha256(file.read_bytes()).hexdigest()
            (self.dist / "release-artifacts.json").write_text(json.dumps(self.original))
            result = self.command("verify")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(
                "contents mismatch"
                if kind == "archive"
                else "metadata version mismatch",
                result.stderr,
            )
            file.write_bytes(saved)
            row["sha256"] = hashlib.sha256(saved).hexdigest()
        wheel = next(r for r in self.original["files"] if r["kind"] == "cli-wheel")
        original = self.dist / wheel["path"]
        wheel["path"] = wheel["path"].replace(self.version, "9.9.9")
        original.rename(self.dist / wheel["path"])
        (self.dist / "release-artifacts.json").write_text(json.dumps(self.original))
        result = self.command("verify")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("wheel version/architecture mismatch", result.stderr)

    def test_wheel_distribution_metadata_and_directory_identity_cannot_be_renamed(self):
        row = next(row for row in self.original["files"] if row["kind"] == "cli-wheel")
        file = self.dist / row["path"]
        saved = file.read_bytes()
        for mode, message in (
            ("name", "wheel metadata distribution mismatch"),
            ("dist-info", "wheel dist-info identity mismatch"),
            ("data", "wheel script identity mismatch"),
        ):
            with self.subTest(mode=mode):
                file.write_bytes(saved)
                with zipfile.ZipFile(file) as package:
                    contents = {name: package.read(name) for name in package.namelist()}
                metadata = next(
                    name for name in contents if name.endswith(".dist-info/METADATA")
                )
                if mode == "name":
                    contents[metadata] = contents[metadata].replace(
                        b"Name: onejudge-cli", b"Name: unrelated"
                    )
                elif mode == "dist-info":
                    contents[metadata.replace("onejudge_cli-", "unrelated-")] = (
                        contents.pop(metadata)
                    )
                else:
                    binary = next(
                        name
                        for name in contents
                        if name.endswith(".data/scripts/onejudge")
                    )
                    contents[binary.replace("onejudge_cli-", "unrelated-")] = (
                        contents.pop(binary)
                    )
                with zipfile.ZipFile(file, "w") as package:
                    for name, value in contents.items():
                        package.writestr(name, value)
                row["sha256"] = hashlib.sha256(file.read_bytes()).hexdigest()
                (self.dist / "release-artifacts.json").write_text(
                    json.dumps(self.original)
                )
                result = self.command("verify")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    def test_manifest_scalar_fields_and_collections_are_validated_by_real_verifier(
        self,
    ):
        cases = [([], "manifest must be an object")]
        for key, value, message in (
            ("tag", 1, "identity fields must be strings"),
            ("sha", None, "identity fields must be strings"),
            ("version", [], "identity fields must be strings"),
            ("files", {}, "files must be a list"),
        ):
            manifest = copy.deepcopy(self.original)
            manifest[key] = value
            cases.append((manifest, message))
        for key, value, message in (
            ("target", "unknown", "unknown target/kind"),
            ("target", 1, "unknown target/kind"),
            ("kind", "unknown", "unknown target/kind"),
            ("kind", False, "unknown target/kind"),
            ("sha256", "not-a-hash", "invalid payload hash"),
            ("sha256", 123, "invalid payload hash"),
            ("sha256", "A" * 64, "invalid payload hash"),
        ):
            manifest = copy.deepcopy(self.original)
            manifest["files"][0][key] = value
            cases.append((manifest, message))
        for manifest, message in cases:
            with self.subTest(message=message, manifest=manifest):
                (self.dist / "release-artifacts.json").write_text(json.dumps(manifest))
                result = self.command("verify")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    def test_waits_for_exact_producer_completion(self):
        result = self.fetch("ordering")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "calls").read_text().count("/workflows/"), 2)

    def test_exact_producer_selection_excludes_other_tags_events_and_ambiguous_artifacts(
        self,
    ):
        for mode, message in (
            ("wrong-tag", "producer absent"),
            ("manual-run", "producer absent"),
            ("nullable-branch", "producer absent"),
            ("unavailable-status", "producer 42 has unavailable status"),
            ("duplicate-artifacts", "no unique unexpired artifact"),
        ):
            with self.subTest(mode=mode):
                (self.root / "calls").unlink(missing_ok=True)
                result = self.fetch(mode)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertNotIn('"download"', (self.root / "calls").read_text())

    def test_newest_matching_run_controls_recovery_even_when_older_run_succeeded(self):
        result = self.fetch("competing-failure")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("producer 43 failed", result.stderr)
        self.assertNotIn('"download"', (self.root / "calls").read_text())
        (self.root / "calls").unlink()
        result = self.fetch("competing-ordering")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = (self.root / "calls").read_text()
        self.assertIn("/runs/43/artifacts", calls)
        self.assertIn('["run", "download", "43"', calls)
        self.assertNotIn("/runs/42/artifacts", calls)
        self.assertNotIn('["run", "download", "42"', calls)

    def test_nullable_unrelated_runs_are_parsed_without_installing_their_output(self):
        result = self.fetch("unrelated-nullable")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = (self.root / "calls").read_text()
        self.assertIn('["run", "download", "42"', calls)
        self.assertNotIn('["run", "download", "43"', calls)

    def test_absent_failed_and_wrong_run_payload_are_visible_failures(self):
        for mode, expected in (
            ("failed", "producer 42 failed"),
            ("absent", "producer absent"),
        ):
            result = self.fetch(mode)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(expected, result.stderr)
        manifest = self.original
        manifest["sha"] = "b" * 40
        (self.dist / "release-artifacts.json").write_text(json.dumps(manifest))
        result = self.fetch("success")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("sha mismatch", result.stderr)


if __name__ == "__main__":
    unittest.main()
