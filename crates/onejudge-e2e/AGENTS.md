# onejudge-e2e — conventions

The engine's end-to-end journeys (`tests/e2e.rs`, `tests/notes.rs`,
`tests/contract_doc.rs`). The root `AGENTS.md` holds the repo-wide contract; this
covers what differs here, and the test-double conventions every suite shares.

- **`e2e.rs` drives the real boundary.** It points `CommandProvider` /
  `OneharnessProvider` at the built test-double binaries (`onejudge_test_doubles::
  echo_provider()` and its siblings) and runs the engine as a consumer would. Do
  not mock the layer under test — the model is the only faked thing, and it is
  faked by a *real subprocess*, not a stub. Add the happy path **and** a
  failure/recovery path for every journey.
- **The doubles are `onejudge-test-doubles`'s bins** (`src/bin/` there), found by
  that crate's resolvers rather than `env!("CARGO_BIN_EXE_…")`, which Cargo sets
  only within the package that owns them. This project's `test` depends on
  `onejudge-test-doubles:build`, so a journey always drives freshly built
  doubles; a direct `cargo nextest run -p onejudge-e2e` needs them built first and
  says so. Nothing here is `#[ignore]`-d.
  Steer a double's behavior with the `[[marker:arg]]` conventions documented in
  each binary's module doc; add a new marker there when a journey needs one. A
  judge of a **panel** is handed the same persona and transcript as every other
  judge, so a journey that needs two judges to differ steers each through its own
  **argv** — the echo double scans its arguments for markers too.
- **`notes.rs` drives the note delivery seam through the library API.** Every case
  goes through `Notes::channel` / `Engine::with_notes` / `Plan::with_notes` and
  never a command line, and the framing each party was handed is asserted against
  the text the *double received* (`[[record:PATH]]` on the echo double,
  `[[record-prompt:PATH]]` on the fake oneharness one) rather than a re-derivation
  of it. A live turn is driven by holding it open — `[[worker-dwell:MS:PATH]]` /
  `[[judge-dwell:MS:PATH]]` touch `PATH` as the turn opens, and the sending thread
  waits for that file before it sends — so an arrival is demonstrably during a
  party's turn rather than between turns, with the dwell an order of magnitude
  longer than the wait's poll interval. See `docs/notes.md`.
- **The doubles' crate is outside the coverage report.** The doubles are test
  infrastructure, not the shipped library, so they are outside the 95%
  line-coverage bar — put the real assertions on the library's behavior, not the
  double's. Under coverage this suite is instrumented and the doubles are not.
- **`onejudge_test_doubles::support` holds what more than one suite needs** — the scratch paths,
  the out-of-tree liveness check for a leaked harness stand-in, and the POSIX
  process-group hook the cancellation journeys drive. `e2e.rs` runs them against
  the engine, `onejudge-cli-e2e` against a `Plan`; put a helper there rather than
  copying it.
- **A control socket's address has a byte budget, and it differs per platform.**
  `sun_path` is 108 bytes on Linux, 104 on the macOS/BSD lineage, and absent off
  unix — and a macOS runner's temp dir (`/var/folders/<ab>/<hash>/T`) spends half
  of it before your store is named. Take a store root from `control_store` /
  `use_private_session_store`, which measure it against oneharness's own
  `socket_path`; a hand-rolled `temp_dir().join(..)` passes locally and fails only
  on the macOS job. For the same reason, asserting that an over-long address is
  *refused* is unix-only: off unix no address is too long, so oneharness's error
  cannot be constructed at all. Reproduce either locally with a long `TMPDIR`.
