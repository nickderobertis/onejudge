This private tooling owns macOS scheduling. `macos-policy.mjs classify` is the
production release-only decision, and `macos-result.mjs` is the required Linux
carrier's result handler. Release automation uses the repository's PAT identity
(`nickderobertis`) AND a same-repository `release-plz-` branch; identity alone
never authorizes omission. Unreadable PRs require Mac coverage. Lockfile version
normalization requires a path-local package with workspace version inheritance
proven from parsed manifests at both commits; dependency changes require Mac.

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

Internal command exits are stable: 0 means classification/reporting/verification
completed successfully; 1 means argument, validation, transport, build, install,
publish or required-work failure. The classifier reports unknown PR input as a
successful decision requiring Mac coverage, rather than an omission. Budget
commands report measurements only; check-budgets exits 1 on any SDK error/over
verdict and prints its diagnostic on stderr. No command publishes on failed
verification. Python commands use argparse's exit 2 for malformed CLI syntax.

The GitHub client’s offline drift gate uses `pypi/github-contract.json`, a
projection of the pinned official OpenAPI schema recorded there. To refresh it,
obtain that document at the intended commit and run
`python3 pypi/update_github_contract.py --schema PATH --commit SHA`, then run the
packaging tests. The projection generator reads no local client declarations.
