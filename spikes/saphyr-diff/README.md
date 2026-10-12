# saphyr-diff (SPIKE ONLY — never lands)

Differential harness for plan `authoring:serde-yaml-replacement`, node
`spike-onejudge-yaml`: this repository's real YAML through `serde_yaml_ng` 0.10.0
(today) and `serde-saphyr` 1.3.0 (+ `saphyr` 0.2.1 for the schema-less walk).
Its own Cargo workspace, so nothing here reaches the repository's lock or gates.

- `cargo run -q --release --manifest-path spikes/saphyr-diff/Cargo.toml` — the
  one command: prints, per document kind, every difference in
  what is read, written, and what errors say, then a summary and an `Options`
  scoreboard. Reads third-party SKILL.md files on this machine in place
  (`SPIKE_EXTRA_SKILL_DIRS`, default `~/.claude/skills/synced:~/.claude/plugins`).
- `cargo run -q --manifest-path spikes/saphyr-diff/Cargo.toml --bin deny_probe` — serde-saphyr / saphyr dependency closures, the banned YAML
  crates, and cargo-deny with the repository's `deny.toml` over a post-move
  approximation (`deny-probe/`) beside the repository today.
- `cargo run -q --manifest-path spikes/saphyr-diff/Cargo.toml --bin capture` — regenerates `fixtures/captured{,-sdk}/` (every config and
  front matter the src unit tests, `onejudge-cli-e2e` and the Python SDK suite
  actually parse) by applying `capture/capture.patch` temporarily.
- `cargo run -q --manifest-path spikes/saphyr-diff/Cargo.toml --bin collect` — refreshes `fixtures/effective/` (oneagentgraph's real effective
  configs from `~/.local/state/oneagentgraph/runs`) and `fixtures/skill/` (the
  user's own SKILL.md files).
- `src/workflows_ng.rs` is `crates/onejudge-repo/tests/workflows.rs`'s checkers
  verbatim; `src/workflows_json.rs` is their port onto `serde_json::Value`.

The helpers are Rust bins rather than shell scripts so the repository's shell
gates (format, shellcheck, the 82% coverage floor over every script) have nothing
of the spike's to measure.
