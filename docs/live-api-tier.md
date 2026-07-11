# The live-API tier

`ApiJudgeProvider` talks to a model API **directly** — Anthropic Messages or
OpenAI Chat Completions — with no harness in between. The **live-API tier** proves
that path against the genuine external service, the same way the
[live tier](live-tier.md) proves the `OneharnessProvider` path.

## What the deterministic gate already covers

`ApiJudgeProvider` is generic over an `HttpTransport`, and everything above the
wire — building each vendor's request, parsing its response, extracting the
verdict, classifying HTTP errors — is pure and unit-tested against a fake
transport in the offline gate. The bundled `UreqTransport` (the real HTTPS client
behind the `ureq-transport` feature) is additionally exercised over a **real local
socket** by `just test-http` — offline and deterministic, but building its TLS
stack (`ring`) needs a C toolchain and a newer Rust than the crate's MSRV, so it
runs as a CI job (`http`) rather than inside `just check`.

The one thing left is the real network round-trip to a real vendor — that is this
tier.

## Why it is out of the gate

It is non-deterministic (a real model), credentialed, and networked — the opposite
of the deterministic, offline gate. So every live-API test is `#[ignore]`-d
(`crates/onejudge/tests/live_api.rs`), only compiles under the `ureq-transport`
feature, and never runs in `just check`. It is **not** in the required-checks set.

## Running it

```sh
export ANTHROPIC_API_KEY=...     # or OPENAI_API_KEY=... (Anthropic wins if both set)
just test-live-api
```

Configure the target with env vars:

- `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` — the credential picks the vendor. With
  neither set, the tests skip with a note rather than fail, so an un-credentialed
  `--run-ignored all` is a no-op here.
- `ONEJUDGE_LIVE_API_MODEL` — the model id (defaults to a small model per vendor).

## In CI

The `live-api` workflow (`.github/workflows/live-api.yml`) is
**`workflow_dispatch`-only** — trigger it by hand from the Actions tab. It needs a
raw **API key** (`x-api-key` / `Bearer`), not the harness OAuth token that `llmlint`
/ `live` use, because `ApiJudgeProvider` calls the vendor API directly. So it is
not wired to run on every PR (there would be no raw key to use); run it once
`ANTHROPIC_API_KEY` or `OPENAI_API_KEY` is configured as a repo secret. It is
**not** in the required-checks set.
