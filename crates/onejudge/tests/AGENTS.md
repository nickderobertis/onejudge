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
  `e2e.rs` file is `#![cfg(feature = "fake-provider")]`; the gate enables that
  feature, so e2e always runs — it is never `#[ignore]`-d.
  Steer a double's behavior with the `[[marker:arg]]` conventions documented in
  each binary's module doc; add a new marker there when a journey needs one.
- **Coverage excludes `src/bin/`.** The doubles are test infrastructure, not the
  shipped library, so they are outside the 95% line-coverage bar — put the real
  assertions on the library's behavior, not the double's.
- **`support/mod.rs` holds what more than one suite needs** — the scratch paths,
  the out-of-tree liveness check for a leaked harness stand-in, and the POSIX
  process-group hook the cancellation journeys drive. `e2e.rs` runs them against
  the engine, `cli.rs` against a `Plan`; put a helper here rather than copying it.
- **A control socket's address has a byte budget, and it differs per platform.**
  `sun_path` is 108 bytes on Linux, 104 on the macOS/BSD lineage, and absent off
  unix — and a macOS runner's temp dir (`/var/folders/<ab>/<hash>/T`) spends half
  of it before your store is named. Take a store root from `control_store` /
  `use_private_session_store`, which measure it against oneharness's own
  `socket_path`; a hand-rolled `temp_dir().join(..)` passes locally and fails only
  on the macOS job. For the same reason, asserting that an over-long address is
  *refused* is unix-only: off unix no address is too long, so oneharness's error
  cannot be constructed at all. Reproduce either locally with a long `TMPDIR`.
- **`coverage.rs` plants a corrupt profile on purpose.** It writes a truncated
  `.profraw` into the directory `cargo llvm-cov` merges from, so the gate's own
  coverage step has to survive the artifact a killed instrumented child leaves —
  the one that blocked the v0.5.0 release. It follows that any coverage
  invocation here needs `--failure-mode all`: run `just test`, not a hand-rolled
  `cargo llvm-cov`.
- **`release_targets.rs` reads the release configuration, never a transcribed
  inventory.** It holds `release-targets.toml` to the canonical release-target
  schema (`nickderobertis/onevcs`, `docs/contract.md`) — a validator in the test,
  proven against refusals as well as the real document — and derives what this
  repository publishes from the release workflows and the manifests they build, so
  a new artifact fails the gate instead of going undeclared. It drives the real
  `scripts/release-probe.sh`, and fakes the *registry* the way the rest of this
  suite fakes the model — a real `curl` stand-in first on `PATH` — so a registry
  that fails, or serves nothing, is a deterministic offline journey. Only the
  answers that must come from the true public registries are `#[ignore]`-d, like
  `live.rs`; they run via `just test-release-targets`.
- **`live.rs` is the real-harness tier**: every test is `#[ignore]`-d, compiles
  in the normal build, and runs only via `just test-live` / the `live` workflow.
  See `docs/live-tier.md`.
