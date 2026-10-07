# onejudge-python-sdk — conventions

- **Generated, never hand-written.** `src/onejudge_sdk/_generated*` come from the
  library's Rust wire types (`just python-sdk-generate`); `generate-check` fails
  the gate on drift.
- **Its tests drive the real `onejudge` binary and the doubles**, never a mock of
  the process boundary.
