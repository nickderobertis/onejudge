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
TYPE_ROOTS = {
    "run_config": "RunConfig",
    "report": "RunReport",
    "stream_event": "StreamEvent",
}


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


def literal_expression(values: list[Any]) -> str:
    """Render JSON constants as a Python Literal expression."""
    return f"Literal[{', '.join(json.dumps(value) for value in values)}]"


def type_expression(schema: dict[str, Any]) -> str:
    """Render a JSON Schema node as a precise Python type expression."""
    if "$ref" in schema:
        reference = schema["$ref"]
        if not isinstance(reference, str):
            raise RuntimeError("schema $ref must be a string")
        return reference.rsplit("/", 1)[-1]
    variants = schema.get("anyOf") or schema.get("oneOf")
    if variants:
        non_null = [item for item in variants if item.get("type") != "null"]
        nullable = len(non_null) != len(variants)
        constants = [item["const"] for item in non_null if "const" in item]
        if len(constants) == len(non_null):
            expression = literal_expression(constants)
        else:
            expressions = list(dict.fromkeys(type_expression(item) for item in non_null))
            expression = (
                expressions[0] if len(expressions) == 1 else f"Union[{', '.join(expressions)}]"
            )
        return f"Optional[{expression}]" if nullable else expression
    kind = schema.get("type")
    if isinstance(kind, list):
        non_null = [item for item in kind if item != "null"]
        expressions = [type_expression({**schema, "type": item}) for item in non_null]
        expression = expressions[0] if len(expressions) == 1 else f"Union[{', '.join(expressions)}]"
        return f"Optional[{expression}]" if len(non_null) != len(kind) else expression
    if "const" in schema:
        return literal_expression([schema["const"]])
    if "enum" in schema:
        return literal_expression(schema["enum"])
    if kind == "string":
        return "str"
    if kind == "boolean":
        return "bool"
    if kind == "integer":
        return "int"
    if kind == "number":
        return "float"
    if kind == "array":
        return f"Sequence[{type_expression(schema.get('items', {}))}]"
    if kind == "object":
        additional = schema.get("additionalProperties")
        if isinstance(additional, dict):
            return f"dict[str, {type_expression(additional)}]"
        return "dict[str, Any]"
    if kind == "null":
        return "None"
    return "Any"


def typed_dict(name: str, schema: dict[str, Any]) -> str:
    """Render one object schema, retaining its required/optional split."""
    properties = pythonize(schema).get("properties", {})
    required = set(pythonize(schema).get("required", []))
    required_fields = [(key, value) for key, value in properties.items() if key in required]
    optional_fields = [(key, value) for key, value in properties.items() if key not in required]
    blocks = []
    if required_fields and optional_fields:
        base = f"_{name}Required"
        fields = "\n".join(f"    {key}: {type_expression(value)}" for key, value in required_fields)
        blocks.append(f"class {base}(TypedDict):\n{fields}\n")
        heading = f"class {name}({base}, total=False):"
        fields_to_render = optional_fields
    else:
        total = "" if required_fields else ", total=False"
        heading = f"class {name}(TypedDict{total}):"
        fields_to_render = required_fields or optional_fields
    fields = "\n".join(f"    {key}: {type_expression(value)}" for key, value in fields_to_render)
    blocks.append(f"{heading}\n{fields or '    pass'}\n")
    return "\n\n".join(blocks).rstrip()


def types_module(bundle: dict[str, Any]) -> str:
    """Render all named definitions and root contracts."""
    definitions: dict[str, dict[str, Any]] = {}
    for root in TYPE_ROOTS:
        for name, schema in bundle[root].get("$defs", {}).items():
            previous = definitions.setdefault(name, schema)
            if previous != schema:
                raise RuntimeError(f"incompatible duplicate schema definition {name!r}")

    aliases = []
    objects = []
    for name, schema in definitions.items():
        if schema.get("type") == "object":
            objects.append(typed_dict(name, schema))
        else:
            aliases.append(f"{name} = {type_expression(schema)}")
    objects.extend(typed_dict(name, bundle[root]) for root, name in TYPE_ROOTS.items())
    body = "\n".join(aliases)
    if aliases and objects:
        body += "\n\n\n"
    body += "\n\n\n".join(objects)
    return f'''"""Generated from onejudge. Do not edit."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any, Literal, Optional, TypedDict, Union


{body}
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
        "../_version.py": (
            b'"""Package version, stamped from Cargo.toml when packaged. Do not edit."""\n\n'
            b'__version__ = "0.0.0.dev0"\n'
        ),
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
