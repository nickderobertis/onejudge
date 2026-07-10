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

# Fails below the threshold. Excludes the src/bin/ test doubles (test
# infrastructure, not the shipped library). `--all-features` builds the doubles
# the e2e suite drives across a real subprocess boundary.

# Gate test step: whole suite (unit + integration + e2e), coverage enforced.
test:
    cargo llvm-cov nextest --all-features --ignore-filename-regex 'src/bin/' \
        --fail-under-lines {{coverage_min}}

# Quick inner loop: the suite with no coverage instrumentation.
test-fast:
    cargo nextest run --all-features

# The end-to-end suite alone (real subprocess boundary, test-double binaries).
test-e2e:
    cargo nextest run --all-features --test e2e

# Opt-in live tier: drive a REAL oneharness + harness (never in `check`). See docs/live-tier.md.
test-live:
    cargo nextest run --all-features --test live --run-ignored all

# Lint: clippy across every target and feature, warnings denied.
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Format the codebase in place.
format:
    cargo fmt --all

# Fail if anything is unformatted (used by the gate).
format-check:
    cargo fmt --all --check

# Build the docs as a gate: broken intra-doc links and doc warnings fail.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features

# Supply-chain audit: advisories + license policy and unused dependencies.
audit:
    cargo deny check
    cargo machete

# Check the crate still builds on the declared MSRV (needs 1.82.0 installed).
msrv:
    cargo +1.82.0 check --locked --all-targets --all-features

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
