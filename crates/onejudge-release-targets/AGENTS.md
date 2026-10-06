# onejudge-release-targets — the release-target network tier

`tests/release_targets.rs` holds the answers about what this repository
releases that need a network: what the public registries serve for every
declared target, and whether the canonical release-target schema
(`nickderobertis/onevcs`) still matches the one `onejudge-repo` restates.

- **External tier, out of the gate**: every test is `#[ignore]`-d and runs via
  `just test-release-targets` and the `release-targets` workflow (on a schedule,
  because drift upstream does not wait for a change here). Nothing may depend on
  it.
- **One reader.** It uses `onejudge-repo`'s schema reader and probe helpers, so
  the offline drift gate and this tier can never hold the document to different
  shapes. A refusal from the upstream reconciliation is never "fix this test": the
  canonical schema changed, and what this repository publishes must be reread.
