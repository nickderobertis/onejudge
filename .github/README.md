This private tooling owns macOS scheduling. `macos-policy.mjs classify` is the
production release-only decision, and `macos-result.mjs` is the required Linux
carrier's result handler. Release automation uses the repository's PAT identity
(`nickderobertis`) AND a same-repository `release-plz-` branch; identity alone
never authorizes omission. Unreadable PRs require Mac coverage.

`github-workflows:test` evaluates the checked-in workflows with
`@actions/expressions`, expands matrices and local reusable calls, and records
ignored `.telemetry/macos-policy.json`. Unsupported routing is an error. The
budgets target reads that recording; it never starts a scenario or model call.
Run from the repository root:

```
just check --projects github-workflows --targets test
node .github/macos-budget.mjs --metric jobs-per-run --telemetry .github/.telemetry/macos-policy.json
node .github/macos-budget.mjs --metric runs-per-change --telemetry .github/.telemetry/macos-policy.json
```

The release producer and consumer share `pypi/release_artifacts.py`. Darwin
archives and wheels use the same per-target executable. A Linux publish job
persists the verified handoff as a Release zip alongside the existing archives,
so manual recovery survives Actions artifact expiry. Alias tags at the same
commit select one canonical producer (`v<version>`); the PyPI selection plan's
original tag is retained. Recovery uses current workflow tooling checked out at
`github.workflow_sha` without credentials, alongside tagged source. Only an
explicit `release-binaries` manual diagnostic opts into another Mac build.
