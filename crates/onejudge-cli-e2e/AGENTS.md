# onejudge-cli-e2e — conventions

The `onejudge` CLI's journeys (`tests/cli.rs`) and the replay fixtures they are
held to (`tests/golden/`). The test-double conventions are in
`crates/onejudge-e2e/AGENTS.md`; this covers what differs here.

- **Two layers, neither mocked beyond the model.** The run driver in process over
  the echo double, and the built `onejudge` binary as a subprocess
  (`onejudge_test_doubles::onejudge_cli()`), asserting on stdout, the
  `--format json` `Report` and the exit code. This project's `test` depends on
  `onejudge:build` and `onejudge-test-doubles:build`; under coverage it builds an
  *instrumented* `onejudge` beside the suite, so the lines the binary runs still
  count toward the library's floor.
- **`golden/single-judge*/` are replay fixtures, not hand-written expectations.**
  `tests/cli.rs` runs each `config.yaml` (a `split` with one `judge:`; the `-control`
  one with `control: true` on both sides) through the built binary and asserts the
  report and every judge-side request/prompt equal what the released 0.8.1 wrote —
  captured by `scripts/capture-single-judge-baseline.sh` from that release's own
  binaries. They are what prove a panel of one is byte-identical to the judge side
  before panels existed (save the `supervisor_control` the release's CLI omitted,
  which the controlled replay pins on purpose); recapture them only from a
  release, never from the tree under test. The controlled fixture's paths ride the
  judge prompt, which the fake oneharness bills by length, so both the script and
  the test spell them at one fixed width under `/tmp` rather than via
  `control_store`.
- **Never recapture a golden from the tree under test.** `tests/golden/README.md`
  names the release and the script each was captured with.
