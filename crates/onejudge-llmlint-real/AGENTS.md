# onejudge-llmlint-real — the real-llmlint tier

`tests/llmlint_real.rs` drives the built CLI against the *released* llmlint.

- **External tier, out of the gate**: `#[ignore]`-d, run by `just test-llmlint`
  and the `llmlint-real` CI job, which installs llmlint at the provider's floor
  through `scripts/setup-llmlint.sh`. A rule matching no file means no model
  call; each decision is read back through `llmlint history`. An absent llmlint
  fails it — never make it skip.
- Its `test` depends on `onejudge:build` and `onejudge-test-doubles:build`, so it
  drives the freshly built `onejudge` binary. Nothing may depend on it.
