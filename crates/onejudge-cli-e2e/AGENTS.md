# onejudge-cli-e2e — conventions

- **`tests/golden/` holds replay fixtures, not hand-written expectations.** Each
  was captured from a released binary by its `scripts/capture-*.sh`; recapture
  only from a release, never from the tree under test.
- **The controlled fixture's paths ride the judge prompt**, which the fake
  oneharness bills by length, so both its capture script and the test spell them
  at one fixed width under `/tmp` rather than via `control_store`.
