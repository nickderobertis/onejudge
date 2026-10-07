# onejudge-repo — the repository-level checks

- **It depends on nothing in this repository** (`type:repo`): its suites read
  root files — the workflows, `release-targets.toml`, the manifests — so a
  library change never reruns it, and a root-file change sweeps, which does.
- **Read the release configuration, never a transcribed inventory**, so a new
  artifact fails the gate instead of going undeclared; fake a registry the way the
  rest of the suites fake the model, with a real stand-in first on `PATH`.
- **A workflow contract is a checker that returns its violations**, also driven
  against a mutated workflow to show it refuses the break it exists for: add the
  refusal case with the check.
