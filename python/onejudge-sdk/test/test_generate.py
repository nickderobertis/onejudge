"""Drift tests for Rust-to-Python contract generation."""

from __future__ import annotations

import importlib.util
import shutil
import subprocess
import sys
import tempfile
import unittest
from collections.abc import Sequence
from pathlib import Path
from typing import Literal, Optional, get_type_hints
from unittest import mock

from onejudge_sdk import (
    EvalConfig,
    FailureReport,
    JudgeDecision,
    JudgedTurn,
    JudgeVerdict,
    ProviderConfig,
    RunConfig,
    RunReport,
    StreamEvent,
    ToolEvent,
    Transcript,
    Usage,
)

ROOT = Path(__file__).resolve().parents[3]
GENERATOR = ROOT / "python" / "onejudge-sdk" / "scripts" / "generate.py"
PACKAGE = ROOT / "python" / "onejudge-sdk" / "src" / "onejudge_sdk"


class GenerationTests(unittest.TestCase):
    """Pin deterministic generated assets."""

    def test_checked_in_contracts_match_rust(self) -> None:
        """Run the generator's public check mode."""
        subprocess.run([sys.executable, str(GENERATOR), "--check"], cwd=ROOT, check=True)

    def test_regeneration_keeps_the_py_typed_marker(self) -> None:
        """Regenerate into a copy of the package and find the PEP 561 marker intact."""
        spec = importlib.util.spec_from_file_location("onejudge_sdk_generate", GENERATOR)
        assert spec is not None and spec.loader is not None
        generator = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(generator)
        with tempfile.TemporaryDirectory() as scratch:
            package = Path(scratch) / "onejudge_sdk"
            shutil.copytree(PACKAGE, package, ignore=shutil.ignore_patterns("__pycache__"))
            marker = package / "py.typed"
            before = marker.read_bytes()
            output = package / "_generated"
            argv = [str(GENERATOR)]
            with (
                mock.patch.object(generator, "OUTPUT", output),
                mock.patch.object(sys, "argv", argv),
            ):
                self.assertEqual(generator.main(), 0)
            owned = {(output / relative).resolve() for relative in generator.generated_files()}
            self.assertNotIn(marker.resolve(), owned)
            self.assertTrue(marker.is_file(), "regeneration removed onejudge_sdk/py.typed")
            self.assertEqual(marker.read_bytes(), before)

    def test_generated_types_resolve_nested_contracts(self) -> None:
        """Expose refs, report structures, and the stream envelope precisely."""
        config = get_type_hints(RunConfig)
        report = get_type_hints(RunReport)
        stream = get_type_hints(StreamEvent)
        self.assertIs(config["provider"], ProviderConfig)
        self.assertEqual(config["evals"], Sequence[EvalConfig])
        self.assertIs(report["transcript"], Transcript)
        self.assertEqual(report["usage"], Optional[Usage])
        self.assertEqual(get_type_hints(JudgeVerdict)["usage"], Optional[Usage])
        self.assertIs(stream["event"], ToolEvent)

    def test_generated_judge_panel_shapes(self) -> None:
        """Carry the judge list on the config and each judge's decision on both reports."""
        provider = get_type_hints(ProviderConfig)
        self.assertEqual(provider["judges"], Optional[Sequence[ProviderConfig]])
        self.assertEqual(provider["label"], Optional[str])
        self.assertEqual(get_type_hints(RunReport)["judge_decisions"], Sequence[JudgedTurn])
        self.assertEqual(get_type_hints(FailureReport)["judge_decisions"], Sequence[JudgedTurn])
        self.assertEqual(get_type_hints(JudgedTurn)["decisions"], Sequence[JudgeDecision])
        self.assertEqual(
            get_type_hints(JudgeDecision)["decision"],
            Literal["done", "continue", "no_instruction", "unparseable", "error"],
        )

    def test_generated_nullable_and_literal_types(self) -> None:
        """Represent nullable fields as Optional and schema enums as literals."""
        config = get_type_hints(RunConfig)
        self.assertEqual(config["task"], Optional[str])
        self.assertEqual(
            get_type_hints(ProviderConfig)["kind"],
            Literal["oneharness", "command", "split", "llmlint"],
        )


if __name__ == "__main__":
    unittest.main()
