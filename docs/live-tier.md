# The live tier

The deterministic gate (`just check`) fakes only the model — it drives the real
engine across a real subprocess boundary against the `fake-provider` test
doubles, so everything except a live harness/model is genuine. The **live tier**
is the one place that faking is replaced by the real call: it drives a real
`oneharness` binary and, through it, a real harness and model, proving the
`OneharnessProvider` path against the genuine external service.

## Why it is out of the gate

It is non-deterministic (a real model), needs `oneharness` plus an authenticated
harness installed, and makes network calls — the opposite of the deterministic,
offline gate. So every live test is `#[ignore]`-d (`crates/onejudge/tests/live.rs`)
and never runs in `just check`. It still **compiles** in the normal build, so the
live code can't rot; it just does not execute there.

## Running it

```sh
# Install oneharness + an authenticated harness first (see scripts/setup-llmlint.sh
# for the oneharness install; authenticate your harness, e.g. Claude Code).
export CLAUDE_CODE_OAUTH_TOKEN=...      # Claude Code harness credential (or your harness's own)
just test-live
```

Configure the target with env vars:

- `ONEJUDGE_LIVE_HARNESS` — the harness id (default `claude-code`).
- `ONEJUDGE_LIVE_MODEL` — the model (default: the harness's own default, i.e. no
  `--model` is passed).

## In CI

The `live` workflow (`.github/workflows/live.yml`) runs it. It **requires** the
harness credential and fails fast with an actionable message when it is absent —
it never no-ops to a green pass, which would report an untested path as covered.
Fork PRs are handled at the repo level (Settings → Actions → General → Fork pull
request workflows → Require approval), not by a no-op branch in the workflow, and
the live workflow is **not** in the required-checks set.
