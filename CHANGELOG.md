# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Releases are cut
automatically by `release-plz` from Conventional Commits — this file is written
by the tooling, not by hand.

## [Unreleased]

### Added

- A unified per-turn supervisor provider operation that decides completion or
  returns the next simulated-user message in one harness invocation. Agent-side
  oneharness history is recorded for on-demand full event inspection, and report
  schema v4 retains the supervisor's completion reason.
- Optional `assessment` config and report field for one free-text judge pass over
  the finished, events-aware transcript.
- Initial engine extracted from `skilltest`: the `Provider` boundary
  (`OneharnessProvider`, `CommandProvider`), the `Engine` conversation loop
  (single-turn and simulated-user multi-turn), the `Transcript` / `ToolEvent`
  model, tolerant judge-verdict parsing, and `Usage` aggregation.
- Judge prompts render tool events, so verdicts can reason over what the skill
  did; `Transcript`/`ToolQuery` expose an events-backed assertion primitive.
- A uniform caller-owned session name threaded across turns on session-capable
  harnesses (targets `oneharness` v0.3.20+).

### Changed

- The linked `oneharness-core` is **0.17.0** (was 0.14.0): the release whose
  engine writes a per-run history pointer line for every harness run and reads
  it back typed (`io::history::read_pointers`, `HistoryPointer`), taken through
  0.15.0 (`--format` on every JSON verb), 0.16.0 (`run_mode` defaults to
  `fallback`, CLI stdout to text) and 0.16.1 (a clean-exit rate limit falls
  through). onejudge adds nothing to the pointer mechanism; the raise is what
  lets an engine linking this crate take it. The minimum supported `oneharness`
  CLI **stays 0.14.0**: the report that CLI writes (schema `0.11`) is the one the
  linked core parses, and the floor follows the report contract rather than the
  core's version — the pin gate in `cli/mod.rs` now holds the two on their own
  terms (the linked core by the symbols it must carry, the CLI floor by the
  report schema it must write) instead of ordering their version numbers.
- **Breaking:** the note contract (`onejudge::note`) is `onemessagebus-agent`
  0.4.0's, re-exported at the same paths. `Notes::send` answers the core's
  `onemessagebus::Undelivered` (read it back with `.map_err(Undelivered::from)`)
  and blocks until a turn takes the note, including a note sent before the run
  starts, so send from another thread.
- The command-provider request frames are registered as bus schemas,
  `agent.onejudge-frame.<op>@7`, and carried in the SDK schema bundle under
  `frames`.
- **The minimum supported Rust version is 1.89** (was 1.86), forced by
  `onemessagebus` 0.4.0.

### Known issues

- Against `onemessagebus-agent` 0.4.0, a note sent to a bare `NoteInbox` its
  caller dropped without handing it to onejudge answers
  `Undelivered::MemberSettled`. That is a wrong answer: nothing read the channel,
  so the true one is `NoConversation`, which every channel whose lifetime onejudge
  owns does answer. The 0.4.0 profile owns that drop and offers no way to close it
  with a reason.
