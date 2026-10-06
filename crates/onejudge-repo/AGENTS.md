# onejudge-repo — the repository-level checks

Suites that read the workspace's root files rather than the library: the CI
wiring branch protection depends on (`tests/workflows.rs`), the tier routing CI
runs (`tests/ci_tier.rs`), and the offline release-target drift gate
(`tests/release_targets.rs`), plus the release-target reader (`src/lib.rs`)
shared with the network tier.

- **It depends on nothing in this repository** (`type:repo`), so a change to the
  library never reruns it; a change to a root file it reads runs the broader tier
  (`scripts/gate-plan.sh`), which does.
- **`release_targets.rs` reads the release configuration, never a transcribed
  inventory.** It holds `release-targets.toml` to the canonical release-target
  schema (`nickderobertis/onevcs`, `docs/contract.md`) — a validator proven
  against refusals as well as the real document — and derives what this
  repository publishes from the release workflows and the manifests they build,
  so a new artifact fails the gate instead of going undeclared. It drives the real
  `scripts/release-probe.sh` and fakes the *registry* the way the rest of the
  suites fake the model — a real `curl` stand-in first on `PATH`. The answers
  that need the true public registries are the `onejudge-release-targets` tier.
- **A workflow contract is held by a checker that returns its violations**, and
  each checker is also driven against a mutated workflow to show it refuses the
  break it exists for. Add the refusal case with the check.
