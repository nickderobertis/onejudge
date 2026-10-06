# onejudge-python-sdk — conventions

The typed async Python SDK, published to PyPI as `onejudge`. Its tool choices
and their departures from `languages/python.md` are recorded in the root
`AGENTS.md` ("Stack and composition").

- **Generated, never hand-written.** `src/onejudge_sdk/_generated*` come from the
  library's Rust wire types (`just python-sdk-generate`); `generate-check` fails
  the gate when regenerating would change them.
- **Targets** (`just python-sdk-check` runs them alone; `just check` runs them in
  the gate): `generate-check`, `format-check` / `lint` (ruff), `typecheck` (mypy
  `--strict`), `test` (unittest under coverage) and `coverage` (its own
  `fail_under = 95`). `test` drives the real `onejudge` binary and the doubles
  from `target/debug`, so it depends on `onejudge:build` and
  `onejudge-test-doubles:build`.
- **Releases ride the crate's.** release-plz detects only the Rust package, so a
  release-worthy SDK-only change is attributed to it by the
  `python-sdk-release-trigger` workflow (`workspace:test` proves the rules).
