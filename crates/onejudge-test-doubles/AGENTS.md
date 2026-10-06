# onejudge-test-doubles — conventions

The deterministic doubles every suite drives as real subprocesses (`src/bin/`)
and the helpers more than one suite shares (`src/support.rs`). `publish = false`.

- **Fake only the model.** Each double stands in for exactly one external
  program — a command provider, `oneharness`, a harness CLI, `llmlint` — and is
  steered by the `[[marker:arg]]` conventions in its module doc. The conventions
  for driving them are in `crates/onejudge-e2e/AGENTS.md`.
- **Suites find them through this crate** (`echo_provider()`, `fake_oneharness()`,
  `fake_harness()`, `fake_llmlint()`, `onejudge_cli()`), never through a path
  they spell themselves: `ONEJUDGE_TEST_DOUBLES_DIR` when the coverage run names
  where they were built, otherwise the running test binary's own profile
  directory. A missing binary fails loudly naming the build.
- **`build` is the contract with every consuming suite.** Each consuming `test`
  target depends on `onejudge-test-doubles:build`; add the edge (and the
  `implicitDependencies` entry the boundary check asks for) when a new suite
  spawns a double.
- **Outside the coverage report.** Doubles are test infrastructure; a detached
  child they spawn sends its profile to a temp path (`src/bin/support/coverage.rs`),
  so it can never corrupt the merge.
