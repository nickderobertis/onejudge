# onejudge — the published crate

The library, its feature-gated `onejudge` CLI `[[bin]]`, and the schema-export
examples. The root `AGENTS.md` holds the repo-wide contract; this covers what
differs here.

- **A contract, so it depends on nothing in this repository** (`type:contract`).
  `Report` and the command-provider JSON-lines frames are read by other
  repositories; a suite that spawns a process lives in a project of its own and
  depends on this one, never the reverse. `node scripts/check-project-boundaries.mjs
  onejudge` (part of `lint`) refuses an edge back.
- **Its own tests are the unit tests plus the contract and drift suites**
  (`tests/contract.rs`, `tests/note_contract.rs`, `tests/judge_seat_frames.rs`):
  wire-shape goldens and generated-schema checks, nothing that spawns a double.
  `lint` also runs the generated-schema drift checks over `schemas/`
  (`just _check-generated-schemas`).
- **Features are published names.** `cli`, `skill` and `sdk-schema` are what
  consumers enable (`cargo install onejudge --features cli`); renaming one is a
  breaking change. The test doubles are not a feature here — they are the
  unpublished `onejudge-test-doubles` crate.
- **`tests/coverage.rs` plants a corrupt profile on purpose.** It writes a
  truncated `.profraw` into the directory `workspace:coverage` merges from, so the
  gate's own coverage step has to survive the artifact a killed instrumented
  child leaves — the one that blocked the v0.5.0 release. It follows that any
  coverage merge here needs `--failure-mode all`: run `just test`, not a
  hand-rolled `cargo llvm-cov`.
- **`src/bin/onejudge.rs` stays thin.** It is outside the coverage report; the
  CLI's logic lives in the covered `src/cli/` modules.
- **Never regenerate `tests/golden/` from the tree.** See its `README.md`.
