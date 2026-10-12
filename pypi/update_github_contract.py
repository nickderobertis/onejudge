"""Project the fields this client reads from GitHub's pinned official OpenAPI schema."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path

# A schema projection, not another response declaration: values are copied from
# the authoritative document, and test_github_contract holds the client to them.
FIELDS = {
    "release": ("tag_name", "assets"),
    "release-asset": ("name",),
    "workflow-run": ("id", "event", "head_branch", "head_sha", "status", "conclusion"),
    "artifact": ("name", "expired"),
}


def project(source: Path, commit: str) -> dict[str, object]:
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("official schema commit must be a full git id")
    data = source.read_bytes()
    document = json.loads(data)
    schemas = document["components"]["schemas"]
    return {
        "source": {
            "url": f"https://raw.githubusercontent.com/github/rest-api-description/{commit}/descriptions/api.github.com/api.github.com.json",
            "commit": commit,
            "sha256": hashlib.sha256(data).hexdigest(),
        },
        "schemas": {
            name: {
                "properties": {
                    field: schemas[name]["properties"][field] for field in fields
                }
            }
            for name, fields in FIELDS.items()
        },
        "run_status_or_conclusion": document["components"]["parameters"][
            "workflow-run-status"
        ]["schema"]["enum"],
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--schema", type=Path, required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--out", type=Path, default=Path("pypi/github-contract.json"))
    args = parser.parse_args()
    args.out.write_text(json.dumps(project(args.schema, args.commit), indent=2) + "\n")
