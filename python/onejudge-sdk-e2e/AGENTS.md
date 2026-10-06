# onejudge-python-sdk-e2e — the installed-wheel journey

`package_e2e.py` builds the SDK wheel from `../onejudge-sdk`, checks its
metadata (version and CLI pin from Cargo, `py.typed`, the generated schemas),
installs it into a fresh venv and drives the real `onejudge` binary through the
public import.

- A project of its own so the slow wheel build sits behind its own graph edge;
  `pyproject.toml` is its manifest, never a distribution (`package = false`).
- Its `test` depends on `onejudge:build` and `onejudge-test-doubles:build`. It
  runs in the SDK's pinned tool environment and under the SDK's ruff and mypy
  configuration.
