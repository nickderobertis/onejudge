# tests/ — conventions

Scoped rules for the integration/e2e tests. The root `AGENTS.md` holds the
repo-wide contract; this covers only what differs here.

- **`e2e.rs` drives the real boundary.** It points `CommandProvider` /
  `OneharnessProvider` at the built test-double binaries
  (`env!("CARGO_BIN_EXE_...")`) and runs the engine as a consumer would. Do not
  mock the layer under test — the model is the only faked thing, and it is faked
  by a *real subprocess*, not a stub. Add the happy path **and** a failure/recovery
  path for every journey.
- **The doubles live behind the `fake-provider` feature** (`src/bin/`). The whole
  `e2e.rs` file is `#![cfg(feature = "fake-provider")]`; the gate always enables
  the feature (`--all-features`), so e2e always runs — it is never `#[ignore]`-d.
  Steer a double's behavior with the `[[marker:arg]]` conventions documented in
  each binary's module doc; add a new marker there when a journey needs one.
- **Coverage excludes `src/bin/`.** The doubles are test infrastructure, not the
  shipped library, so they are outside the 95% line-coverage bar — put the real
  assertions on the library's behavior, not the double's.
- **`live.rs` is the real-harness tier**: every test is `#[ignore]`-d, compiles
  in the normal build, and runs only via `just test-live` / the `live` workflow.
  See `docs/live-tier.md`.
