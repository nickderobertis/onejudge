# AGENTS.md

Durable instructions for humans and agents working in this repo. Write for a
future maintainer, not as a session log. Put deterministic steps in scripts and
keep this file for constraints, tradeoffs, and judgment.

> Keep this file terse — it is always-loaded context. Add a line only if it is
> future-relevant **and** the task wouldn't surface it anyway (a failing gate,
> `just --list`, the code, or a linked doc). Link mechanisms; don't restate them.

> `CLAUDE.md` is a symlink to this file (`ln -s AGENTS.md CLAUDE.md`) so the two
> never drift. Edit `AGENTS.md` only.

## What this repo is

`onejudge` is a Rust library that drives a **simulated interaction and evaluation
loop** on top of [`oneharness`](https://github.com/nickderobertis/oneharness):
take a skill/agent, drive it through a multi-turn conversation with a simulated
user, and score the transcript with natural-language (judge) and tool-event
verdicts. It is the engine extracted from
[`skilltest`](https://github.com/nickderobertis/skilltest) (see
[nickderobertis/skilltest#31](https://github.com/nickderobertis/skilltest/issues/31));
skilltest is the test-framework surface (cases, evals-as-assertions, SDKs) built
on top. Consumers: anyone who wants to "drive a harness through a simulated
conversation and score the transcript" without skilltest's YAML/case framing.

The layering: `oneharness` (one invocation → one JSON report) → **`onejudge`**
(interaction + judging loop) → `skilltest` (test-framework surface).

## Two standing goals on every task

The user drives product features and their request is the priority — but carry
two goals into *every* task. When either is the lowest-error path to what the
user asked, fold it in without asking; surface the rest as follow-ups.

1. **Engineer the context for next time.** Realistic e2e that exercises what a
   consumer sees, scripts that automate repeated steps and shrink output to
   signal, and terse `AGENTS.md` notes for what the code doesn't make obvious.
2. **Engineer the codebase and environment.** Keep it clean, maintainable, and
   repeatable; keep setup automated (`just bootstrap` from a clean clone). Strict
   gates plus local/CI parity make results repeatable, not "works on my machine."

## Stack and composition

Built up from the `create-repo` skill's reference axes, not a single template.

- **Product shape:** library **+ CLI** (`shapes/library.md` + `shapes/cli.md`) —
  an importable Rust crate with a stable public API (the source of truth for the
  `CommandProvider` JSON-lines protocol, `docs/protocol.md`), plus a standalone
  `onejudge` binary behind the **non-default `cli` feature** that drives a harness
  through a simulated-user loop to complete one task (`docs/cli.md`, issue #8).
  CLI deps (`clap`, `serde_yaml_ng`) never reach a `cargo add onejudge` consumer.
- **Language(s):** rust (`languages/rust.md`) — stable toolchain, `rustfmt` +
  `clippy -D warnings`, `cargo nextest`, `cargo llvm-cov` coverage gate, `cargo
  deny` + `cargo machete` supply-chain job.
- **Cross-cutting:** `ci.md` (always) and `releasing.md` (applies — the crate is
  a versioned artifact published to crates.io; `release-plz` drives it, and a
  tag push also builds per-platform CLI archives, see below).
- **References composed:** base.md, shapes/library.md, shapes/cli.md,
  languages/rust.md, intersections/rust-cli.md, ci.md, llmlint.md, releasing.md
- **Excluded, and why:** `monorepo.md` — one crate, one language, no orchestrator
  (the CLI is a feature-gated `[[bin]]` in the single crate, **not** a second
  crate); `src` layout / asdf / direnv — not idiomatic for a single Cargo crate.
  The two `fake-provider` `[[bin]]`s remain test-only doubles (never published);
  the `onejudge` `[[bin]]` is the one shipped binary.

## Command surface

Use the `just` recipes; do not hand-roll equivalents. `just --list` is the index.

- `just bootstrap` — fetch the pinned toolchain + `cargo fetch` from a clean clone.
- `just check` — the full gate: format check, clippy (deny warnings), doc build,
  coverage-enforced tests **including e2e**, and the supply-chain audit. Must
  pass before any commit or PR.
- `just test` (coverage-enforced) / `just test-fast` / `just test-e2e` /
  `just lint` / `just format` / `just audit` / `just msrv` — individual steps
  (all on `--features fake-provider`, the deterministic gate's feature set).
- `just test-http` — the bundled `ureq-transport` over a real local socket (CI
  `http` job); `just test-live` / `just test-live-api` — the credentialed real
  `oneharness` / real Anthropic-OpenAI tiers. All three are out of `check`.
- `just upgrade` — `cargo update`, then re-run the gate; commit refreshed lockfile.
- `just lint-llm` / `just lint-llm-diff` — the llmlint LLM-judge tier, separate
  from `check` and non-deterministic; config in `llmlint.yml`. `just setup-llmlint`
  installs its toolchain.

## Commits, releases, and merging

- **Squash-merge only, via PR, with auto-merge.** One PR is one squash commit
  whose subject is the PR title. Queue with `gh pr merge --auto --squash`; it
  merges once every required check is green. Merged branches auto-delete. Admins
  may break-glass.
- **All gating checks required.** Branch protection requires `check` (the
  e2e-inclusive Linux gate), the cross-platform `test-os (macos-latest)` /
  `test-os (windows-latest)`, `msrv`, `package` (publishable-artifact build),
  `commitlint` (PR-title lint), and `llmlint` (the LLM-judge job), plus linear
  history, conversation resolution, no force-push/deletion. The `cli-binary` job
  (builds + smoke-tests the shipped `onejudge` binary with `cli,ureq-transport`)
  and `http` run on every PR; add `cli-binary` to branch protection once the CLI
  stabilizes if you want it required. The live oneharness
  tier is *not* required (credential-gated; fork PRs need maintainer approval).
- **PRs follow `.github/pull_request_template.md`** — terse **What** and **Why**;
  it becomes the squash body.
- **Releases: fully automated, no manual deploy step.** `release-plz` opens a
  release PR from the merged Conventional-Commits history; merging it writes the
  version + `CHANGELOG.md`, tags `vX.Y.Z`, and publishes to crates.io. The only
  human action is merging that PR. The release job authenticates with a PAT
  (`RELEASE_PLZ_TOKEN`), not the default `GITHUB_TOKEN`, so the tag fires publish
  **and** the `release-binaries` workflow, which builds the `onejudge` CLI for
  each platform (linux/macos-x64+arm64/windows) and attaches the archives to the
  tag's GitHub Release for `install.sh` / manual download to fetch.
  **Bump policy (pre-1.0):** `feat` / `feat!` / `BREAKING CHANGE` → minor;
  `fix` / `perf` / `refactor` / `build` → patch; `chore` / `docs` / `ci` /
  `test` → no release. Post-1.0, a breaking change is a major.

## Invariants (non-negotiable)

- The gate is strict: no warnings-only mode. `clippy`, `rustfmt`, and doc build
  all fail on findings. A diagnostic is an error or a documented, tracked suppress.
- **Never talk to a model directly.** Everything goes through a `Provider`. The
  deterministic gate fakes only the model — via real subprocess test doubles
  (`fake-provider` bins), never by mocking the layer under test.
- Validate every external input at its boundary: provider responses and the
  oneharness report are parsed into typed models (`serde`) before use, and a
  provider that ignores a request contract (empty output, missing verdict field)
  is a loud error, never a vacuous pass.
- Keep the crate portable across Linux, macOS, and Windows (the CI matrix).
- **Security is gate-level.** No secrets in the tree (live-tier credentials come
  from the environment / repo secrets by name); grants are least-privilege.

## Coverage and e2e (the gate's depth)

- **Coverage — enforced, 95% lines.** The gate's `just test` step (`cargo
  llvm-cov nextest --features fake-provider,cli --fail-under-lines 95`) is wired
  into `just check` and fails below 95%. It excludes `src/bin/` — the two
  `fake-provider` doubles **and** the thin `onejudge` entrypoint are excluded (the
  CLI's real logic lives in the covered `src/cli/` library modules). The gate runs
  on `--features fake-provider,cli` (`gate_features` in the justfile), **not**
  `--all-features`: the optional
  `ureq-transport` feature pulls a TLS stack (`ring`) needing a C toolchain and a
  newer Rust than the MSRV, so it is proven in the `http` / `live-api` tiers, not
  the offline gate. `ApiJudgeProvider`'s logic (over a fake `HttpTransport`) is in
  the gate; only the bundled transport glue is out.
- **E2E — real, in the gate.** `crates/onejudge/tests/e2e.rs` drives the real
  engine across a **real subprocess boundary**: it points `CommandProvider` and
  `OneharnessProvider` at deterministic test-double binaries
  (`onejudge-echo-provider`, `onejudge-fake-oneharness`) and asserts on the
  resulting transcript, judge verdicts, events, and session threading — the only
  thing faked is the model, exactly as a consumer would fake it. It covers each
  journey happy-path **and** a failure/recovery path (provider spawn failure,
  empty/malformed output, missing verdict field, non-session-capable fallback),
  plus a `SplitProvider` journey composing two different real-subprocess backends.
  `crates/onejudge/tests/cli.rs` extends the same discipline to the standalone
  binary: it drives the real run driver in-process over the echo double **and**
  spawns the built `onejudge` binary as a subprocess, asserting on stdout, the
  `--format json` `Report`, and the exit code — only the model faked.
- **Out-of-gate tiers, credential/toolchain-gated, `#[ignore]`-d or feature-off:**
  `live` (`tests/live.rs`, real `oneharness`; `docs/live-tier.md`); `http`
  (`just test-http`, the bundled `UreqTransport` over a **real local socket** —
  offline but needs a C toolchain to build `ring`); and `live-api`
  (`tests/live_api.rs`, a real Anthropic/OpenAI API; `docs/live-api-tier.md`).
  None are in the required-checks set.

## The provider boundary

`onejudge` never talks to a model directly; a `Provider` (`provider.rs`) runs the
skill, plays the simulated user, and judges the transcript. Four backends:
`OneharnessProvider` (default; shells out to `oneharness run` — JSON report,
targeting **v0.3.13+** for the uniform `--session` handle); `CommandProvider`
(a small JSON-lines subprocess protocol — see `docs/protocol.md` — backing the
deterministic test doubles and any custom provider); `ApiJudgeProvider`
(`api.rs`; direct Anthropic/OpenAI, **no harness**, generic over an
`HttpTransport` — the logic is pure and gate-covered, the bundled `UreqTransport`
is behind the optional `ureq-transport` feature); and `SplitProvider` (`split.rs`;
compose a skill-runner with a separate judge/simulated-user provider). The
harness-backed backends feed tool `events` into the transcript the judge sees, and
thread a **caller-owned session name** across turns on session-capable platforms
(claude-code, codex, opencode, cursor, qwen) rather than extracting and re-passing
a native id; the rest fall back to re-prompting the inlined transcript.

## The Report contract

`Report` (`report.rs`, `SCHEMA_VERSION`) is onejudge's own versioned wire contract
— transcript + verdicts + usage — that SDKs compose over and re-export. The
serialized shape is drift-gated against a golden
(`tests/golden/report.schema-v1.json`, `tests/contract.rs`): a wire change fails
the gate until it is a deliberate edit that bumps `SCHEMA_VERSION` and the golden.
See `docs/contract.md`.

## Scripts and output are context

- Every script is quiet on success (a line or nothing); on failure it prints the
  exact error and a concrete next action. Maximize signal, minimize noise.

## Keeping the allowlist current

- The agent allowlist lives in `.claude/settings.json`; the tool enforces it.
  When a new routine command joins the workflow, add it there instead of
  re-approving it each session. Keep it narrow.

## Conventions

- Rust: stable toolchain (pinned in `rust-toolchain.toml`), `rustfmt` defaults,
  `clippy -D warnings`. Errors use `thiserror`. Public API is re-exported from
  `lib.rs`; everything else is internal. See `crates/onejudge/tests/AGENTS.md`
  for test-double conventions.

## After the main task: refine and hand off

After the requested task, act on the two standing goals: propose only
materially-helpful follow-ups (a script to automate a manual step, a constraint
worth recording here, a fixture that improves visibility), each with its likely
impact. Skip busywork. If nothing is materially helpful, say so and stop.
