# Canonical command surface for onejudge. `just --list` is the index.
#
# `just bootstrap` must work from a clean clone; `just check` is the full quality
# gate and fails on any issue (no warnings-only mode). The gate is deterministic
# and offline: the model is faked by real subprocess test doubles, never mocked.

set shell := ["bash", "-euo", "pipefail", "-c"]

coverage_min := "95"

# List available recipes.
default:
    @just --list

# Set up from a clean clone: pinned toolchain, cargo tools, fetched deps.
bootstrap:
    rustup show active-toolchain >/dev/null   # installs the rust-toolchain.toml channel + components
    for t in cargo-nextest cargo-llvm-cov cargo-deny cargo-machete; do \
        command -v "$t" >/dev/null 2>&1 || cargo install "$t" --locked; \
    done
    cargo fetch --locked

# Full quality gate: format, lint, doc, coverage-enforced tests, and audit.
check: format-check lint doc test audit

# The deterministic gate runs on `--features fake-provider` (the e2e test doubles),
# NOT `--all-features`: the optional `ureq-transport` feature pulls a TLS stack
# (ring) that needs a C toolchain and a newer Rust than the MSRV, so it is proven
# separately in the `test-http` / `test-live-api` tiers, never in the offline gate.
# The `ApiJudgeProvider` logic and its `HttpTransport` seam are in the gate; only
# the bundled transport glue is out. Coverage excludes the src/bin/ doubles.

# Gate test step: whole suite (unit + integration + e2e), coverage enforced.
test:
    cargo llvm-cov nextest --features fake-provider --ignore-filename-regex 'src/bin/' \
        --fail-under-lines {{coverage_min}}

# Quick inner loop: the suite with no coverage instrumentation.
test-fast:
    cargo nextest run --features fake-provider

# The end-to-end suite alone (real subprocess boundary, test-double binaries).
test-e2e:
    cargo nextest run --features fake-provider --test e2e

# Opt-in live tier: drive a REAL oneharness + harness (never in `check`). See docs/live-tier.md.
test-live:
    cargo nextest run --features fake-provider --test live --run-ignored all

# The bundled ureq HTTP transport, proven over a REAL local socket (offline, but
# building `ring` needs a C toolchain — so this is CI's `http` tier, not `check`).
test-http:
    cargo nextest run --features fake-provider,ureq-transport

# The whole `ureq-transport` tier as CI's `http` job runs it: lint, doc, and the
# real-socket test for the feature. Out of `check` (the offline gate builds no
# C-backed TLS stack); needs a C toolchain for `ring`.
check-http:
    cargo clippy --all-targets --features fake-provider,ureq-transport -- -D warnings
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features fake-provider,ureq-transport
    just test-http

# Opt-in live-API tier: drive a REAL Anthropic/OpenAI API (never in `check`). See
# docs/live-api-tier.md. Needs ANTHROPIC_API_KEY or OPENAI_API_KEY.
test-live-api:
    cargo nextest run --features fake-provider,ureq-transport --test live_api --run-ignored all

# Lint: clippy across every target, warnings denied (gate feature set).
lint:
    cargo clippy --all-targets --features fake-provider -- -D warnings

# Format the codebase in place.
format:
    cargo fmt --all

# Fail if anything is unformatted (used by the gate).
format-check:
    cargo fmt --all --check

# Build the docs as a gate: broken intra-doc links and doc warnings fail.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features fake-provider

# Supply-chain audit: advisories + license policy and unused dependencies.
audit:
    cargo deny check
    cargo machete

# Check the crate still builds on the declared MSRV (needs 1.82.0 installed). The
# `ureq-transport` feature is excluded on purpose — its TLS deps need a newer Rust.
msrv:
    cargo +1.82.0 check --locked --all-targets --features fake-provider

# Upgrade dependencies, then re-run the full gate; commit the refreshed lockfile.
upgrade:
    cargo update
    @just check

# Install/refresh the llmlint toolchain (oneharness + llmlint). Idempotent.
setup-llmlint:
    ./scripts/setup-llmlint.sh

# LLM-judge lint (llmlint) on demand — non-deterministic, harness-backed, out of `check`.
lint-llm *paths:
    @command -v llmlint >/dev/null 2>&1 || { echo "llmlint not installed — run 'just setup-llmlint'"; exit 1; }
    llmlint {{paths}}

# llmlint scoped to the merge-base diff with main — the blocking `llmlint` PR check.
lint-llm-diff base="origin/main":
    ./scripts/lint-llm-diff.sh {{base}}
