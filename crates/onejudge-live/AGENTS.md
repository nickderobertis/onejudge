# onejudge-live — the live tier

- **External tier, out of the gate.** Every test is `#[ignore]`-d and still
  compiles (its `lint` runs in the gate); it requires its credential and fails
  fast without it — never make it skip.
- **Nothing may depend on it** (`type:external`): an edge onto it would put a
  credentialed, billed run behind an unrelated change.
