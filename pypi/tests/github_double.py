#!/usr/bin/env python3
"""GitHub's wire/process boundary, scripted by the packaging journeys."""

from __future__ import annotations

import json
import os
import shutil
import sys
from pathlib import Path

sys.path.insert(0, os.environ["TOOLING"])
from github_client import Artifact, Asset, Conclusion, Release, RunStatus, WorkflowRun
from release_artifacts import Commit, Tag


def main() -> None:
    args = sys.argv[1:]
    root = Path(os.environ["FIXTURE"])
    mode = os.environ["MODE"]
    with (root / "calls").open("a") as log:
        log.write(json.dumps(args) + "\n")
    name, sha = os.environ["ARTIFACT"], Commit(os.environ["SHA"])
    match args:
        case ["api", path] if "/releases/tags/" in path:
            if mode == "no-release" or (mode == "alias" and path.endswith("/v0.19.0")):
                sys.exit("gh: Not Found (HTTP 404)")
            if mode == "transport-failure":
                sys.exit("gh: unavailable (HTTP 503)")
            assets = (
                (Asset(name + ".zip"),)
                if mode in ("durable", "alias", "unsafe-zip")
                else ()
            )
            if mode == "ambiguous":
                assets = (Asset(name + ".zip"), Asset(name + ".zip"))
            response = Release(
                Tag("wrong" if mode == "wrong-release" else path.split("/")[-1]), assets
            ).to_wire()
            if mode == "invalid-assets":
                response["assets"] = (
                    "not a list"  # Deliberately invalid third-party wire data.
                )
        case ["api", path] if "/workflows/" in path:
            calls = (root / "calls").read_text().count("/workflows/")
            status = (
                RunStatus.RUNNING
                if mode == "ordering" and calls == 1
                else RunStatus.COMPLETED
            )
            run = WorkflowRun(
                42,
                "push",
                Tag("v0.19.0"),
                Commit("b" * 40) if mode == "wrong-run" else sha,
                status,
                None
                if status is RunStatus.RUNNING
                else Conclusion.FAILURE
                if mode == "failed"
                else Conclusion.SUCCESS,
            )
            response = {"workflow_runs": [] if mode == "absent" else [run.to_wire()]}
            if mode == "invalid-runs":
                response["workflow_runs"][0]["id"] = (
                    True  # Deliberately invalid third-party wire data.
                )
        case ["api", path]:
            assert "/runs/42/artifacts" in path, path
            artifact = Artifact(
                "unrelated-artifact" if mode == "wrong-artifact" else name,
                mode == "expired",
            )
            response = {"artifacts": [artifact.to_wire()]}
            if mode == "invalid-artifact":
                response["artifacts"][0]["expired"] = (
                    "false"  # Deliberately invalid wire boolean.
                )
        case ["release", "upload", *_]:
            if mode == "upload-failure":
                sys.exit("gh: upload rejected (HTTP 503)")
            for argument in args:
                if argument.endswith(name + ".zip"):
                    shutil.copy2(argument, root / (name + ".zip"))
            return
        case ["release", "download", *_]:
            dest = Path(args[args.index("--dir") + 1])
            dest.mkdir(parents=True, exist_ok=True)
            shutil.copy2(root / (name + ".zip"), dest / (name + ".zip"))
            return
        case ["run", "download", "42", *_]:
            assert args[args.index("--name") + 1] == name, args
            dest = Path(args[args.index("--dir") + 1])
            dest.mkdir(parents=True, exist_ok=True)
            for file in (root / "dist").iterdir():
                shutil.copy2(file, dest / file.name)
            return
        case _:
            raise ValueError(f"unrecognised doubled transport: {args}")
    print(json.dumps(response))


if __name__ == "__main__":
    main()
