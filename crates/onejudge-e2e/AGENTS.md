# onejudge-e2e — conventions

- **`e2e.rs` drives the real boundary.** It points `CommandProvider` /
  `OneharnessProvider` at the built test-double binaries (`onejudge_test_doubles::
  echo_provider()` and its siblings) and runs the engine as a consumer would. Do
  not mock the layer under test — the model is the only faked thing, and it is
  faked by a *real subprocess*, not a stub. Add the happy path **and** a
  failure/recovery path for every journey.
- **Nothing here is `#[ignore]`-d**, and a journey always drives freshly built
  doubles (`test` depends on `onejudge-test-doubles:build`). A judge of a
  **panel** is handed the same persona and transcript as every other judge, so a
  journey that needs two judges to differ steers each through its own **argv** —
  the echo double scans its arguments for markers too.
- **`notes.rs` drives the note delivery seam through the library API**, never a
  command line, and asserts each party's framing against the text the *double
  received*. A live turn is held open with the dwell markers and the sender waits
  for the turn to open first, so an arrival is during a turn, not between turns.
- **Assert on the library's behavior, not the double's**: the doubles are outside
  the coverage report.
- **A helper more than one suite needs goes in `onejudge_test_doubles::support`**
  (both e2e projects depend on it), never copied into each suite.
- **A control socket's address has a byte budget, and it differs per platform.**
  `sun_path` is 108 bytes on Linux, 104 on the macOS/BSD lineage, and absent off
  unix — and a macOS runner's temp dir (`/var/folders/<ab>/<hash>/T`) spends half
  of it before your store is named. Take a store root from `control_store` /
  `use_private_session_store`, which measure it against oneharness's own
  `socket_path`; a hand-rolled `temp_dir().join(..)` passes locally and fails only
  on the macOS job. For the same reason, asserting that an over-long address is
  *refused* is unix-only: off unix no address is too long, so oneharness's error
  cannot be constructed at all. Reproduce either locally with a long `TMPDIR`.
