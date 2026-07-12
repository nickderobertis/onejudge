# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
