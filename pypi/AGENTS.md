# onejudge-pypi — the `onejudge-cli` wheel

maturin wraps the `onejudge` binary in a per-platform wheel; the installed
console command is `onejudge`.

- **The manifest is the root `pyproject.toml`**, which `release-pypi.yml` builds
  at the repository root, so this directory holds only the project definition.
  `build` declares that file and `README.md` as inputs.
- `build` writes the wheel to `target/pypi-wheel`; `test` installs it into a
  fresh venv and runs the packaged command — the artifact and name the release
  publishes. The version is Cargo's (maturin reads it), never set here.
