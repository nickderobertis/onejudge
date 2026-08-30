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
- `just check` (alias: `just gate`) — the full gate: format check, clippy (deny
  warnings), doc build, coverage-enforced tests **including e2e**, and the
  supply-chain audit. Must pass before any commit or PR.
- `just test` (coverage-enforced) / `just test-fast` / `just test-e2e` /
  `just lint` / `just format` / `just audit` / `just msrv` — individual steps.
- `just test-live` — the credentialed real `oneharness` tier, out of `check`.
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
  (builds + smoke-tests the shipped `onejudge` binary with `cli`) runs on every
  PR; add `cli-binary` to branch protection once the CLI
  stabilizes if you want it required. The live oneharness
  tier is *not* required (credential-gated; fork PRs need maintainer approval).
- **PRs follow `.github/pull_request_template.md`** — terse **What** and **Why**;
  it becomes the squash body.
- **Releases: fully automated, no manual deploy step.** `release-plz` opens a
  release PR from the merged Conventional-Commits history; merging it writes the
  version + `CHANGELOG.md`, tags `vX.Y.Z`, and publishes to crates.io. Nobody has
  to merge it: the workflow arms **auto-merge** on the release PR, so it merges
  itself once the required checks are green (one green release PR sat 20 hours
  otherwise, with every consumer waiting). `semver_check = true` picks the bump
  from the real API diff, so cargo-semver-checks must stay on the release-pr job's
  PATH. The release job authenticates with a PAT
  (`RELEASE_PLZ_TOKEN`), not the default `GITHUB_TOKEN`, so the tag fires publish
  **and** the `release-binaries` workflow, which builds the `onejudge` CLI for
  each platform (linux/macos-x64+arm64/windows) and attaches the archives to the
  tag's GitHub Release for `install.sh` / manual download to fetch.
  **Bump policy (pre-1.0):** `feat` / `feat!` / `BREAKING CHANGE` → minor;
  `fix` / `perf` / `refactor` / `build` → patch; `chore` / `docs` / `ci` /
  `test` → no release. Post-1.0, a breaking change is a major. Because
  release-plz only detects files under the Rust package, the
  `python-sdk-release-trigger` workflow turns a release-worthy
  `python/onejudge-sdk/**`-only push into a matching conventional commit of the
  crate-owned trigger file; the next normal release-plz run then bumps and
  publishes the crate, CLI, and stamped SDK together. `just check` proves the
  attribution rules.
- **What this repository releases is declared** in `release-targets.toml` at the
  root — one document, at the **canonical release-target schema** defined in
  `docs/contract.md` of `nickderobertis/onevcs`, because a consumer reads these
  across repositories it does not own. It names `scripts/release-probe.sh` as its
  `probe`, which answers what a registry serves for one identifier. Identifiers
  are registry-qualified, and each target also carries a short `name`, because
  `onejudge` alone names *both* the crate and the SDK wheel and `pypi:` alone
  names two of the three. The probe's **not answered** is not "no release yet": a
  consumer holds on the first indefinitely, and reading it as the second launches
  work whose dependency never landed. `tests/release_targets.rs` holds the
  document to that schema and keeps it honest by deriving what is published from
  the release configuration, so a new artifact fails the gate rather than going
  undeclared. `scripts/check-release-targets.sh` is *not* that check despite its
  name — it gates the shipped-archive build triples.

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
- **A turn's reply is what the harness reported, never its raw output.** A
  completed turn carrying no `text` replies empty — the truthful answer. The
  substitution of `RunResult::stdout` that used to stand in for it published
  protocol exhaust as the model's words and re-inlined it next turn (a measured
  699 MB event line, 0 model-authored characters). See `Invocation::reply`.
- Keep the crate portable across Linux, macOS, and Windows (the CI matrix).
- **Security is gate-level.** No secrets in the tree (live-tier credentials come
  from the environment / repo secrets by name); grants are least-privilege.

## Coverage and e2e (the gate's depth)

- **Coverage — enforced.** `just test` is the coverage step and is wired into
  `just check`; the floor and the gate's feature set are declared once, in the
  justfile (`coverage_min`, `gate_features`). It excludes `src/bin/` — the
  `fake-provider` doubles **and** the thin `onejudge` entrypoint are excluded (the
  CLI's real logic lives in the covered `src/cli/` library modules). Every model
  call goes through oneharness; the deterministic gate fakes only the model, via
  the real subprocess doubles. That step's `--failure-mode all` is load-bearing
  rather than slack: see the justfile comment on `test`, which `tests/coverage.rs`
  keeps honest by planting the artifact it exists for.
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
- **Out-of-gate tier, credential-gated, `#[ignore]`-d:** `live` (`tests/live.rs`,
  real `oneharness`; `docs/live-tier.md`). Not in the required-checks set.

## The provider boundary

`onejudge` never talks to a model directly, and every model call goes through
`oneharness`; a `Provider` (`provider.rs`) runs the skill, plays the simulated
user, and judges the transcript. Three backends: `OneharnessProvider` (default;
runs each turn through the **`oneharness-core` engine in process** — see below); `CommandProvider` (a small JSON-lines subprocess
protocol — see `docs/protocol.md` — backing the deterministic test doubles and any
custom provider, which itself shells out to oneharness or an equivalent harness);
and `SplitProvider` (`split.rs`; compose a skill-runner with a separate
judge/simulated-user provider, e.g. skill on one harness and judge on another). The
backends feed tool `events` into the transcript the judge sees, and thread a
**caller-owned session name** across turns on session-capable platforms
(claude-code, codex, opencode, cursor, qwen) rather than extracting and re-passing
a native id; the rest fall back to re-prompting the inlined transcript.

An `oneharness` provider can also **stream** (`provider.stream: true`): tool events
reach the caller's sink as oneharness observes them, then the finished report, so a
600–2000s turn is visible while it runs. `onejudge run --stream` republishes the
same two envelopes outward for the SDKs. `docs/streaming.md` is the contract; the
rule that keeps it safe is that a *declared*-streaming provider's unmodelled line is
a loud `Protocol` error, while a typeless bare report stays accepted (a degraded run
is not a failed one).

An **in-process** embedder can watch more than the tool calls: `run_observing`
(`Engine`, and `run_plan_observing_reporting_failure` for a `Plan`) delivers an
`Observation` per turn opening, reply and close — the prose an operator reads a
live dispatch by, which otherwise reaches them only once the dispatch settles, and
never at all if it dies first. Two rules keep it honest. `run_streaming` and
`EventSink` are the *tool half* of the same sink and must keep delivering exactly
that, in the same order, so an embedder written against them is untouched — one
run driver serves both, which is what makes that provable rather than promised.
And **this crate bounds nothing** it hands an observer: the payload bound belongs
to the consumer's journal, the only layer that knows its own limit.

**onejudge depends on `oneharness-core` (published registry version) for the
boundary's types, not just its bytes**: the report, the failure taxonomy, the
fallback block, the streamed envelope, the normalized events, and the
per-candidate history record are all oneharness's own declarations, so an upstream
change is a compile error here rather than a silently-null field. **The invocation
is a library call too**: `oneharness_core::io::run::run`, with `RunControls::events`
for streaming and `RunControls::cancel` for teardown, and `signal_cancel` left
`false` because onejudge is an embedder and those handlers are process-global. Two behaviours
follow only from reading it typed, and both have tests: under `run_mode =
"fallback"` the turn is the candidate that **ran** (not `results[0]`, which is the
one the chain routed around), and a candidate that timed out / could not spawn /
was skipped carries no `failure_kind` — its `Status` is the signal, and ignoring it
banks a vacuously empty turn. One seam still spawns — `Execution::Process`, reached only by naming a binary
(`with_bin`) or installing a `SpawnHook`, because `run` offers an embedder no
spawned harness and an in-process turn would empty `Report::processes`. Upstream
has since closed that gap (`run_supervised` + `ProcessSupervisor`, oneharness-core
0.10.1) and onejudge has **not** moved onto it: adopting it as-is would drop
`SpawnHook`'s refusal contract and change what `Report::processes` means in
process, so it is its own release.
`docs/oneharness-library.md` records the argv↔`RunRequest` mapping (one `TurnSpec`,
two renderings, reconciled by a gate) and what that move still needs.
The e2e double for the in-process seam is `onejudge-fake-harness`, a *harness*
stand-in reached through ordinary `[harness.<id>] bin` config — so the whole of
oneharness is the real code under test and only the model is faked. The measurements onejudge's `telemetry`
reports come from `RunResult::telemetry` on the **run report** (oneharness report
schema `0.5`); the history file is still read, but only for `history_id`.

**Cancelling a turn must terminate the harness tree, not just the party onejudge
talks to** — an orphaned harness keeps billing. In process that is
`RunControls::cancel` plus the sink's `SinkStep::Stop`; either alone suffices
(measured by reverting each), and one e2e proves the teardown from outside the
tree. The **spawning** seam still escalates through three rungs — close stdout,
SIGTERM, kill — because a spawned producer has to be reached through the OS, and
each rung reaches a case the one before cannot; two e2e tests gate that pair, one
per rung. The `oneharness-core` pin lives in the workspace manifest and nowhere
else; the **CLI** floor an operator installs is a different number (**0.11.0+**,
the release that embeds the pin) because the two crates version independently.
Never infer one from the other — `cli/mod.rs` holds the pairing and gates it. See
`docs/oneharness-library.md` before touching either.

The **free deterministic harness** is reachable through this layer:
`provider.mock_harness` / `OneharnessProvider::with_mock_harness` forwards
`oneharness run --mock-harness <id>` on both sides, so an acceptance proof that
needs a real multi-identity chain costs nothing. It selects the *spawning* seam,
because oneharness delivers that responder by re-executing its own binary and in
process that binary is the embedder (`docs/oneharness-library.md`).

**Turn control is an address, not a lever onejudge pulls.** `provider.control:
true` (default off) adds `--control` to **both** parties' `oneharness run`, and
`Report::control` / `Report::supervisor_control` report the three values
`oneharness interrupt` addresses each turn with (`session`, `session_dir`, `cwd`)
— read back from oneharness's report, so a fallback chain names the candidate that
*ran*. Two blocks rather than one because the engine mints two session handles
(`-skill`, `-user`) and oneharness derives the socket from the name: separate
sockets, separately refusable, so a caller holding one must not assume the other.
Both are serialized even when null; a refused ask is `null` **plus** its
`*_unavailable` reason, because "never asked" and "asked and refused" are
different facts. A refusal costs no model tokens (oneharness validates before
spawning), so the call is retried without the flag rather than failing the run.
`--control` arrived in oneharness 0.6.14, under the **0.11.0+** floor the crate
advertises. The stateless `judge` / `assess` calls stay uncontrolled (no session
to be addressed by) and so does the legacy `user` turn, which shares the
supervisor's session name — the one place "two runs on one address" is real.
And because a redirect *reopens* the turn carrying the correction rather than
delivering into it, a **redirected** supervisor turn (read off
`ControlEvent::is_redirected`) whose answer does not parse is asked once more —
once, not a budget, with both invocations on the run's usage and attribution.
`docs/control.md` is the contract; the e2e tests that matter drive a real
socket per party and assert each reported address *redirects that party's live
turn*, not that it merely exists.

**A note reaches whoever is live, and the other party gets it with the response.**
`Notes::channel()` / `Engine::with_notes` / `Plan::with_notes` (`note.rs`) deliver a
correction into a running conversation: to the worker's live turn, or to the judge's
— whose decision is then *discarded and re-taken with the note in hand*, because a
decision taken without it is the decision the note exists to change. The worker's
finished reply is kept instead, and the note is the next thing it is handed, before
any other party is consulted. When the re-taken judge decision is completion,
**nothing** reaches the worker: the work was passed with the note in hand. The note
carries the **role it addresses**, which is what the recipient is told, never who it
is delivered to — a judge handed an update to the *worker's* task has to know it is
one. A note may bind a `Criterion`, which enters the criterion in force at **both**
judging sites (`Engine::criteria`, read by `cli::run_plan`'s authoritative
re-judge), so a moved bar cannot be invisible to the verdict that decides the run.
And **undelivered is an error**: a note arriving after the conversation completed
raises `Undelivered`, naming that it was not delivered and why, because a caller can
choose what to do about a refusal and can do nothing at all about a silence.
`docs/notes.md` is the contract; `tests/notes.rs` drives each of the four arrival
cases through the library API over the real subprocess doubles, holding a party's
turn open so the arrival is genuinely live rather than between turns.

**A named session and `--control` must agree about what a turn is.** A mechanism
that drives the turn over its own protocol builds no argv, so the harness's
`--resume` mapping is never reached and only the protocol's own resume request can
continue a conversation; oneharness refuses a continuation on a mechanism without
one (`SessionControlNoResume`) rather than silently opening a new one — the defect
that re-sent a whole transcript every turn. onejudge degrades from that refusal
like any other: drop `--control`, keep the conversation. Both halves are driven
through the real in-process engine, one journey per outcome, each asserting on the
native token the harness was resumed on.

**Every hop that moves in-process gives up something the subprocess boundary was
supplying.** oneharness's descendant teardown was the first (restored by signalling
it, above); OS **process grouping** is the second. An in-process embedder can no
longer group what onejudge spawns by grouping onejudge, so `SpawnHook`
(`spawn.rs`) offers each process before it starts work — `spawning` for the POSIX
`Command`, `spawned` for the live Windows `Child` handle, both before the prompt
write that unblocks the child's stdin. The **embedder owns the group**; onejudge
takes no grouping policy and reports a `group` on `Report::processes` only when a
hook named one. A hook that fails is a loud `Spawn` error with the child torn
down, never a silent ungrouped run. It is installed per provider **or** on a
`Plan` (`Plan::with_spawn_hook`) for an embedder that drives the CLI's run driver
instead of building providers — the plan installs it on both children of a
`split`, which is the two-party tree a cancel otherwise leaks.
`docs/spawn-hook.md` is the contract; one e2e per entry point kills the group and
asserts an orphaned harness stand-in dies with it. When adding a new spawn site,
route it through `Spawner` — a `Command::spawn` that bypasses it is invisible to
the embedder and to the report.

Prompt caching is oneharness's concern (the agent CLI it wraps caches by default,
and oneharness has an explicit same-prefix batch/fork reuse path); onejudge stays
out of enabling it but **surfaces** it — `Usage` carries `cache_read_tokens` /
`cache_write_tokens`, and `build_judge_prompt` puts the transcript before the
criterion so the framing+transcript prefix is cacheable across criteria.

## The Report contract

`Report` (`report.rs`, `SCHEMA_VERSION`) is onejudge's own versioned wire contract
— transcript + verdicts + usage + per-invocation harness attribution + the
processes the run spawned — that SDKs compose over and re-export. The serialized shape is drift-gated by
`tests/contract.rs`: a wire change fails the gate until it is a deliberate edit
that bumps `SCHEMA_VERSION` and the golden. A run that *fails* produces no report,
so `--format json` writes a versioned `FailureReport` in its place (on stderr under
`--stream`, where stdout is the event protocol) — same attribution, so a failure is
attributable to a side and an identity without parsing a message.
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
