# onejudge-live — the live tier

`tests/live.rs` drives a **real** `oneharness` and an authenticated harness.

- **External tier, out of the gate.** Every test is `#[ignore]`-d, compiles in
  every build (its `lint` runs in the gate), and runs only via `just test-live`
  and the credentialed `live` workflow. It requires its credential and fails
  fast without it — never make it skip. See `docs/live-tier.md`.
- **Nothing may depend on it** (`type:external`): an edge onto it would put a
  credentialed, billed run behind an unrelated change.
