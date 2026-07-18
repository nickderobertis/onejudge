"""Drift tests for Rust-to-Python contract generation."""

from __future__ import annotations

import subprocess
import sys
import unittest
from collections.abc import Sequence
from pathlib import Path
from typing import Literal, Optional, get_type_hints

from onejudge_sdk import (
    EvalConfig,
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


class GenerationTests(unittest.TestCase):
    """Pin deterministic generated assets."""

    def test_checked_in_contracts_match_rust(self) -> None:
        """Run the generator's public check mode."""
        subprocess.run([sys.executable, str(GENERATOR), "--check"], cwd=ROOT, check=True)

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

    def test_generated_nullable_and_literal_types(self) -> None:
        """Represent nullable fields as Optional and schema enums as literals."""
        config = get_type_hints(RunConfig)
        self.assertEqual(config["task"], Optional[str])
        self.assertEqual(
            get_type_hints(ProviderConfig)["kind"],
            Literal["oneharness", "command", "split"],
        )


if __name__ == "__main__":
    unittest.main()
