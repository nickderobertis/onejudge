# onejudge-test-doubles — conventions

- **Fake only the model.** Each double stands in for exactly one external
  program — a command provider, `oneharness`, a harness CLI, `llmlint` — and is
  steered by the `[[marker:arg]]` conventions in its module doc; add a marker
  there when a journey needs one.
- **Suites reach a double only through this crate's resolvers**, never a path
  they spell themselves, and a suite that spawns one has its `test` depend on
  `onejudge-test-doubles:build`, so it never drives a stale double.
- **A detached child gets `detached_profile()`**, so a profile it is still
  writing can never reach the coverage merge.
