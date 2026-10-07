# onejudge-pypi — the `onejudge-cli` wheel

- **The manifest is the root `pyproject.toml`, not this directory.**
  `release-pypi.yml` builds it with maturin at the repository root, and the
  root's one `project.json` is the `workspace` project, so this project's
  definition lives here instead of beside it.
- **The version is Cargo's** (maturin reads it); never set one here.
