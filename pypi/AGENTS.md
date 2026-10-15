# onejudge-pypi — the `onejudge-cli` wheel

- **The manifest is the root `pyproject.toml`, not this directory.**
  `release-pypi.yml` builds it with maturin at the repository root, and the
  root's one `project.json` is the `workspace` project, so this project's
  definition lives here instead of beside it.
- **The version is Cargo's** (maturin reads it); never set one here.
- **The Linux gate cross-builds the Apple silicon artifact** through the real
  `just pypi-release`, linking with zig (`ziglang` from PyPI) and no macOS SDK,
  so system frameworks are left to the dynamic loader: that binary is proved at
  the artifact contract and never run. The macOS release job runs it natively.
