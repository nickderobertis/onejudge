# onejudge — the published crate

- **A contract, so it depends on nothing in this repository** (`type:contract`):
  `Report` and the command-provider frames are read by other repositories. A suite
  that spawns a process lives in a project of its own that depends on this one,
  never the reverse; its `lint` refuses an edge drawn back.
- **Its own tests never spawn a double.** Unit tests and the wire-shape and
  generated-schema drift suites only; anything driving a subprocess belongs in
  `onejudge-e2e` or `onejudge-cli-e2e`.
- **Feature names are published.** `cli`, `skill` and `sdk-schema` are what
  consumers enable (`cargo install onejudge --features cli`); renaming or removing
  one is a breaking change.
- **Measure coverage through `just test`**, never a hand-rolled `cargo llvm-cov`:
  `tests/coverage.rs` is why.
- **`src/bin/onejudge.rs` stays thin**: it is outside the coverage report, so the
  CLI's logic belongs in the covered `src/cli/` modules.
- **Never regenerate `tests/golden/` from the tree.**
- **No evaluator prompt states a permission**; the mode enforces it (`docs/judges.md`).
