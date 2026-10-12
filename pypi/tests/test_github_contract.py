"""An offline drift gate against an independently pinned GitHub OpenAPI projection."""

from __future__ import annotations

import json
import sys
import unittest
from dataclasses import fields
from enum import Enum
from pathlib import Path
from typing import get_args, get_origin, get_type_hints

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from github_client import Artifact, Asset, Conclusion, Release, RunStatus, WorkflowRun


class GitHubContract(unittest.TestCase):
    def test_models_match_the_independently_captured_official_schema(self):
        source = Path(__file__).resolve().parents[1] / "github-contract.json"
        contract = json.loads(source.read_text())
        provenance = contract["source"]
        self.assertRegex(provenance["commit"], r"^[0-9a-f]{40}$")
        self.assertRegex(provenance["sha256"], r"^[0-9a-f]{64}$")
        self.assertEqual(
            provenance["url"],
            f"https://raw.githubusercontent.com/github/rest-api-description/{provenance['commit']}/descriptions/api.github.com/api.github.com.json",
        )
        models = {
            "release": Release,
            "release-asset": Asset,
            "workflow-run": WorkflowRun,
            "artifact": Artifact,
        }
        primitives = {str: "string", int: "integer", bool: "boolean", tuple: "array"}
        for name, model in models.items():
            schema = contract["schemas"][name]["properties"]
            self.assertEqual({f.name for f in fields(model)}, set(schema), name)
            for key, hint in get_type_hints(model).items():
                args = get_args(hint)
                nullable = type(None) in args
                self.assertEqual(
                    nullable, bool(schema[key].get("nullable", False)), (name, key)
                )
                if nullable:
                    hint = next(arg for arg in args if arg is not type(None))
                hint = getattr(hint, "__supertype__", hint)
                origin = get_origin(hint)
                if origin is tuple:
                    hint = tuple
                if isinstance(hint, type) and issubclass(hint, Enum):
                    hint = str
                self.assertEqual(primitives[hint], schema[key]["type"], (name, key))
        self.assertEqual(
            {v.value for v in RunStatus} | {v.value for v in Conclusion},
            set(contract["run_status_or_conclusion"]),
        )

    def test_projection_command_copies_the_pinned_contract_and_records_provenance(self):
        import hashlib
        import os
        import subprocess
        import tempfile

        contract = json.loads(
            (Path(__file__).resolve().parents[1] / "github-contract.json").read_text()
        )
        with tempfile.TemporaryDirectory(
            dir=os.environ.get("ONEPIPELINE_NODE_SCRATCH_DIR")
            or os.environ.get("RUNNER_TEMP")
        ) as scratch:
            raw, out = Path(scratch) / "schema.json", Path(scratch) / "projection.json"
            raw.write_text(
                json.dumps(
                    {
                        "components": {
                            "schemas": contract["schemas"],
                            "parameters": {
                                "workflow-run-status": {
                                    "schema": {
                                        "enum": contract["run_status_or_conclusion"]
                                    }
                                }
                            },
                        }
                    }
                )
            )
            subprocess.run(
                [
                    sys.executable,
                    str(
                        Path(__file__).resolve().parents[1]
                        / "update_github_contract.py"
                    ),
                    "--schema",
                    str(raw),
                    "--commit",
                    contract["source"]["commit"],
                    "--out",
                    str(out),
                ],
                check=True,
            )
            projected = json.loads(out.read_text())
            self.assertEqual(projected["schemas"], contract["schemas"])
            self.assertEqual(
                projected["run_status_or_conclusion"],
                contract["run_status_or_conclusion"],
            )
            self.assertEqual(projected["source"]["url"], contract["source"]["url"])
            self.assertEqual(
                projected["source"]["sha256"],
                hashlib.sha256(raw.read_bytes()).hexdigest(),
            )

    def test_projection_refuses_an_unresolved_external_contract(self):
        import os
        import subprocess
        import tempfile

        with tempfile.TemporaryDirectory(
            dir=os.environ.get("ONEPIPELINE_NODE_SCRATCH_DIR")
            or os.environ.get("RUNNER_TEMP")
        ) as scratch:
            raw = Path(scratch) / "schema.json"
            raw.write_text("{}")
            command = Path(__file__).resolve().parents[1] / "update_github_contract.py"
            result = subprocess.run(
                [
                    sys.executable,
                    str(command),
                    "--schema",
                    str(raw),
                    "--commit",
                    "a" * 40,
                    "--out",
                    str(Path(scratch) / "out"),
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("components", result.stderr)
