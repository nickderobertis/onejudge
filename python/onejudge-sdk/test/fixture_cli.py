"""External process fixture for malformed and timeout boundaries."""

from __future__ import annotations

import os
import json
import sys
import time


def main() -> int:
    """Select one deterministic boundary failure."""
    mode = os.environ["ONEJUDGE_SDK_FIXTURE_MODE"]
    task = sys.stdin.read()
    if mode == "invalid-json":
        if task != "fixture task":
            return 4
        sys.stdout.write("{broken")
        return 0
    if mode == "timeout":
        time.sleep(30)
        return 0
    if mode == "v4-report":
        sys.stdout.write(
            json.dumps(
                {
                    "schema_version": 4,
                    "transcript": {"messages": [{"role": "user", "content": task}]},
                    "stopped_early": False,
                }
            )
        )
        return 0
    sys.stderr.write(f"unknown fixture mode: {mode}\n")
    return 3


if __name__ == "__main__":
    raise SystemExit(main())
