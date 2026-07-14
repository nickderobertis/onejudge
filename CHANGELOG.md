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
