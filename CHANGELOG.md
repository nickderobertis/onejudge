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
