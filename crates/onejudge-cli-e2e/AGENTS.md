# onejudge-cli-e2e — conventions

- **Drive the built binary, never just `main()`.** A journey about the CLI
  surface spawns the `onejudge` binary and asserts on stdout, the
  `--format json` `Report` and the exit code; only the model is faked.
- **`tests/golden/` holds replay fixtures, not hand-written expectations.** Each
  was captured from a released binary by its `scripts/capture-*.sh`; recapture
  only from a release, never from the tree under test.
- **The controlled fixture's paths ride the judge prompt**, which the fake
  oneharness bills by length, so both its capture script and the test spell them
  at one fixed width under `/tmp` rather than via `control_store`.
