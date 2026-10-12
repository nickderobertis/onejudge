"""Real tag histories drive the producer's command and canonical selection."""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

COMMAND = Path(__file__).resolve().parents[1] / "producer_plan.py"


class ProducerSelection(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(
            dir=os.environ.get("ONEPIPELINE_NODE_SCRATCH_DIR")
            or os.environ.get("RUNNER_TEMP")
        )
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git("init", "-q")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "user.name", "Producer fixture")
        (self.repo / "Cargo.toml").write_text(
            '[workspace.package]\nversion = "0.19.0"\n'
        )
        self.git("add", ".")
        self.git("commit", "-qm", "ci: release fixture")
        self.git("tag", "v0.19.0")
        self.git("tag", "onejudge-v0.19.0")
        self.sha = self.git("rev-parse", "HEAD")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.repo, text=True).strip()

    def plan(self, tag, event="push"):
        return subprocess.run(
            [sys.executable, str(COMMAND)],
            cwd=self.repo,
            env={**os.environ, "TAG": tag, "GITHUB_EVENT_NAME": event},
            capture_output=True,
            text=True,
            check=False,
        )

    def outputs(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        return dict(line.split("=", 1) for line in result.stdout.splitlines())

    def test_aliases_have_one_automatic_producer_and_one_exact_identity(self):
        for tag, build in (("v0.19.0", "true"), ("onejudge-v0.19.0", "false")):
            self.assertEqual(
                self.outputs(self.plan(tag)),
                {
                    "tag": "v0.19.0",
                    "sha": self.sha,
                    "version": "0.19.0",
                    "name": f"onejudge-darwin-v0.19.0-{self.sha}",
                    "build": build,
                },
            )

    def test_explicit_diagnostic_alias_builds_the_canonical_identity_when_both_exist(
        self,
    ):
        outputs = self.outputs(self.plan("onejudge-v0.19.0", "workflow_dispatch"))
        self.assertEqual(outputs["tag"], "v0.19.0")
        self.assertEqual(outputs["sha"], self.sha)
        self.assertEqual(outputs["build"], "true")

    def test_alias_alone_builds_and_manual_recovery_uses_its_existing_commit(self):
        self.git("tag", "-d", "v0.19.0")
        for event in ("push", "workflow_dispatch"):
            outputs = self.outputs(self.plan("onejudge-v0.19.0", event))
            self.assertEqual(outputs["tag"], "onejudge-v0.19.0")
            self.assertEqual(outputs["sha"], self.sha)
            self.assertEqual(outputs["build"], "true")

    def test_wrong_version_invalid_and_missing_tags_cannot_start_a_producer(self):
        self.git("tag", "v0.20.0")
        for tag, diagnostic in (
            ("v0.20.0", "tag does not name the Cargo version"),
            ("main", "invalid release tag"),
            ("v9.9.9", "exit status 128"),
        ):
            result = self.plan(tag)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(diagnostic, result.stderr)
