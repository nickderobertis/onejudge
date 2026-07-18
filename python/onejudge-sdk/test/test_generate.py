"""Drift tests for Rust-to-Python contract generation."""

from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
GENERATOR = ROOT / "python" / "onejudge-sdk" / "scripts" / "generate.py"


class GenerationTests(unittest.TestCase):
    """Pin deterministic generated assets."""

    def test_checked_in_contracts_match_rust(self) -> None:
        """Run the generator's public check mode."""
        subprocess.run([sys.executable, str(GENERATOR), "--check"], cwd=ROOT, check=True)


if __name__ == "__main__":
    unittest.main()
