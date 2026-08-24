# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.2](https://github.com/nickderobertis/onejudge/compare/v0.5.1...v0.5.2) - 2026-08-24

### Fixed

- *(oneharness)* reply with the reported text, never the harness's raw output ([#59](https://github.com/nickderobertis/onejudge/pull/59))

## [0.5.1](https://github.com/nickderobertis/onejudge/compare/v0.5.0...v0.5.1) - 2026-08-23

### Fixed

- *(gate)* stop a corrupt coverage profile failing the test recipe ([#57](https://github.com/nickderobertis/onejudge/pull/57))

## [0.5.0](https://github.com/nickderobertis/onejudge/compare/v0.4.0...v0.5.0) - 2026-08-20

### Added

- *(engine)* offer a live turn's instruction, reply text and own usage to an observing embedder ([#54](https://github.com/nickderobertis/onejudge/pull/54))

## [0.4.0](https://github.com/nickderobertis/onejudge/compare/v0.3.10...v0.4.0) - 2026-08-15

### Fixed

- settle no-op release loops, forward mock harness, arm releases ([#52](https://github.com/nickderobertis/onejudge/pull/52))

## [0.3.10](https://github.com/nickderobertis/onejudge/compare/v0.3.9...v0.3.10) - 2026-08-15

### Fixed

- settle a no-op supervisor turn and stop the profraw race ([#50](https://github.com/nickderobertis/onejudge/pull/50))

## [0.3.9](https://github.com/nickderobertis/onejudge/compare/v0.3.8...v0.3.9) - 2026-08-14

### Other

- drive oneharness as a library instead of spawning its CLI ([#49](https://github.com/nickderobertis/onejudge/pull/49))
- keep the detached control server out of the coverage merge ([#47](https://github.com/nickderobertis/onejudge/pull/47))

## [0.3.8](https://github.com/nickderobertis/onejudge/compare/v0.3.7...v0.3.8) - 2026-08-12

### Added

- report where an oneharness interrupt can redirect the agent turn ([#44](https://github.com/nickderobertis/onejudge/pull/44))

## [0.3.7](https://github.com/nickderobertis/onejudge/compare/v0.3.6...v0.3.7) - 2026-08-09

### Added

- let a plan-driven embedder group the processes onejudge spawns ([#40](https://github.com/nickderobertis/onejudge/pull/40))

## [0.3.6](https://github.com/nickderobertis/onejudge/compare/v0.3.5...v0.3.6) - 2026-08-09

### Added

- let an in-process embedder group the processes onejudge spawns ([#37](https://github.com/nickderobertis/onejudge/pull/37))

## [0.3.5](https://github.com/nickderobertis/onejudge/compare/v0.3.4...v0.3.5) - 2026-08-08

### Added

- *(oneharness)* drive the boundary through oneharness's typed contract ([#35](https://github.com/nickderobertis/onejudge/pull/35))
- accept streamed NDJSON providers and surface events live ([#33](https://github.com/nickderobertis/onejudge/pull/33))

## [0.3.4](https://github.com/nickderobertis/onejudge/compare/v0.3.3...v0.3.4) - 2026-07-19

### Added

- *(sdk)* typed two-party telemetry summary + session linkage ([#31](https://github.com/nickderobertis/onejudge/pull/31))

## [0.3.3](https://github.com/nickderobertis/onejudge/compare/v0.3.2...v0.3.3) - 2026-07-18

### Other

- cut a release when the Python SDK changes ([#29](https://github.com/nickderobertis/onejudge/pull/29))

## [0.3.2](https://github.com/nickderobertis/onejudge/compare/v0.3.1...v0.3.2) - 2026-07-18

### Fixed

- *(python)* rename the SDK distribution to `onejudge` while retaining the `onejudge_sdk` import
- *(python)* build and publish the SDK from `python/onejudge-sdk` at the release version with an exact `onejudge-cli` pin

## [0.3.1](https://github.com/nickderobertis/onejudge/compare/v0.3.0...v0.3.1) - 2026-07-18

### Added

- *(python)* add onejudge-cli wheel and typed onejudge-sdk ([#22](https://github.com/nickderobertis/onejudge/pull/22))

## [0.3.0](https://github.com/nickderobertis/onejudge/compare/v0.2.0...v0.3.0) - 2026-07-15

### Added

- [**breaking**] unify per-turn supervisor decisions
- add free-text assessment judge output ([#18](https://github.com/nickderobertis/onejudge/pull/18))
- [**breaking**] drive the CLI with optional skill + system_prompt, dropping `agent` ([#17](https://github.com/nickderobertis/onejudge/pull/17))
- *(cli)* add ONEJUDGE_* env override tier (flags > env > file > defaults) ([#15](https://github.com/nickderobertis/onejudge/pull/15))

### Fixed

- select the live oneharness configuration ([#21](https://github.com/nickderobertis/onejudge/pull/21))

### Other

- align supervisor contract references
- cover supervisor compatibility fallback

## [0.2.0](https://github.com/nickderobertis/onejudge/compare/v0.1.0...v0.2.0) - 2026-07-12

### Added

- [**breaking**] drive harness/model selection from oneharness config, not onejudge.yaml ([#14](https://github.com/nickderobertis/onejudge/pull/14))
- [**breaking**] route all model calls through oneharness; surface cache tokens ([#11](https://github.com/nickderobertis/onejudge/pull/11))
- ship a standalone onejudge CLI + YAML config driven by a simulated-user loop ([#9](https://github.com/nickderobertis/onejudge/pull/9))

### Other

- *(readme)* add config section with a simple example onejudge.yaml ([#13](https://github.com/nickderobertis/onejudge/pull/13))
- show how to spin up a judge run in the README CLI section ([#10](https://github.com/nickderobertis/onejudge/pull/10))
- release v0.1.0 ([#6](https://github.com/nickderobertis/onejudge/pull/6))

## [0.1.0](https://github.com/nickderobertis/onejudge/releases/tag/v0.1.0) - 2026-07-11

### Added

- add ApiJudge and Split providers and a versioned Report contract ([#3](https://github.com/nickderobertis/onejudge/pull/3))

### Fixed

- drop the invalid --format flag from oneharness run args

### Other

- initial onejudge engine extracted from skilltest
