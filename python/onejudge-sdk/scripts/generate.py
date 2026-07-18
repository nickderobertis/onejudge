"""Generate Python SDK schemas and public wire types from Rust metadata."""

from __future__ import annotations

import argparse
import copy
import difflib
import json
import re
import subprocess
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[3]
OUTPUT = ROOT / "python" / "onejudge-sdk" / "src" / "onejudge_sdk" / "_generated"
INPUT_ROOTS = ("run_config",)


def snake_case(value: str) -> str:
    """Convert camel-case properties to Python snake case."""
    return re.sub(r"(?<!^)(?=[A-Z])", "_", value).lower()


def pythonize(node: Any) -> Any:
    """Rename object property declarations without touching map keys."""
    if isinstance(node, list):
        return [pythonize(item) for item in node]
    if not isinstance(node, dict):
        return node
    result: dict[str, Any] = {}
    for key, value in node.items():
        if key == "properties":
            result[key] = {snake_case(name): pythonize(child) for name, child in value.items()}
        elif key == "required":
            result[key] = [snake_case(name) for name in value]
        else:
            result[key] = pythonize(value)
    return result


def property_map(node: Any, result: dict[str, str]) -> None:
    """Collect inverse spelling mappings used for config serialization."""
    if isinstance(node, list):
        for item in node:
            property_map(item, result)
        return
    if not isinstance(node, dict):
        return
    for name in node.get("properties", {}):
        python = snake_case(name)
        previous = result.setdefault(python, name)
        if previous != name:
            raise RuntimeError(f"input properties {previous!r} and {name!r} collide")
    for value in node.values():
        property_map(value, result)


def type_expression(schema: dict[str, Any]) -> str:
    """Render the useful subset needed by the public config TypedDict."""
    kind = schema.get("type")
    if kind == "string":
        return "str"
    if kind == "boolean":
        return "bool"
    if kind == "integer":
        return "int"
    if kind == "number":
        return "float"
    if kind == "array":
        return f"Sequence[{type_expression(schema['items'])}]"
    if kind == "object" or "$ref" in schema:
        return "dict[str, Any]"
    return "Any"


def types_module(bundle: dict[str, Any]) -> str:
    """Render a strict top-level config TypedDict and report alias."""
    properties = pythonize(bundle["run_config"]).get("properties", {})
    fields = "\n".join(f"    {key}: {type_expression(value)}" for key, value in properties.items())
    return f'''"""Generated from onejudge. Do not edit."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any, TypedDict


class RunConfig(TypedDict, total=False):
{fields}


RunReport = dict[str, Any]
'''


def generated_files() -> dict[str, bytes]:
    """Run the Rust exporter and return deterministic package files."""
    output = subprocess.check_output(
        [
            "cargo",
            "run",
            "-q",
            "-p",
            "onejudge",
            "--features",
            "sdk-schema",
            "--example",
            "generate_sdk_schema",
        ],
        cwd=ROOT,
        encoding="utf-8",
        text=True,
    )
    bundle = json.loads(output)
    schemas = copy.deepcopy(bundle)
    input_keys: dict[str, dict[str, str]] = {}
    for root in INPUT_ROOTS:
        keys: dict[str, str] = {}
        property_map(bundle[root], keys)
        input_keys[root] = dict(sorted(keys.items()))
        schemas[root] = pythonize(bundle[root])
    return {
        "__init__.py": b'"""Rust-generated Python SDK assets."""\n',
        "schemas.json": (json.dumps(schemas, indent=2, sort_keys=True) + "\n").encode(),
        "input-keys.json": (json.dumps(input_keys, indent=2, sort_keys=True) + "\n").encode(),
        "../_generated_types.py": types_module(bundle).encode(),
    }


def drift(path: Path, expected: bytes) -> str:
    """Render an actionable generated-file difference."""
    if not path.exists():
        return f"{path}: missing"
    return "\n".join(
        difflib.unified_diff(
            path.read_text(encoding="utf-8").splitlines(),
            expected.decode().splitlines(),
            fromfile=f"{path} (checked in)",
            tofile=f"{path} (generated)",
            lineterm="",
            n=2,
        )
    )


def main() -> int:
    """Write generated files, or report drift under --check."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    stale = []
    for relative, content in generated_files().items():
        path = OUTPUT / relative
        if args.check:
            if not path.exists() or path.read_bytes() != content:
                stale.append(drift(path, content))
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
    if stale:
        print("\n".join(stale))
        print("generated Python SDK contracts are stale; run just python-sdk-generate")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
