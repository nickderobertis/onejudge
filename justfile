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

# Full quality gate: format, lint, doc, coverage-enforced tests, audit, and the
# release-target drift gate.
check: format-check lint doc python-sdk-check test audit check-release-targets

# The deterministic gate enables the test doubles, CLI, and SDK schema export,
# never `--all-features`. Every model call goes through oneharness; the gate
# fakes only the model via real subprocess doubles. Coverage excludes src/bin/.

# The offline gate's feature set: the e2e test doubles PLUS the standalone `cli`
# binary and schema generator, so both public export surfaces remain gated.
gate_features := "fake-provider,cli,sdk-schema"

# Gate test step: whole suite (unit + integration + e2e + cli), coverage enforced.
test:
    cargo llvm-cov nextest --features {{gate_features}} --ignore-filename-regex 'src/bin/' \
        --fail-under-lines {{coverage_min}}

# Quick inner loop: the suite with no coverage instrumentation.
test-fast:
    cargo nextest run --features {{gate_features}}

# The end-to-end suites alone (real subprocess boundary, test-double binaries).
test-e2e:
    cargo nextest run --features {{gate_features}} --test e2e --test cli

# Opt-in live tier: drive a REAL oneharness + harness (never in `check`). See docs/live-tier.md.
test-live:
    cargo nextest run --features fake-provider --test live --run-ignored all

# Build the shipped `onejudge` CLI binary — the artifact the `cli-binary` PR job
# smoke-tests and `release-binaries.yml` packages. Optional `target`
# cross-compiles for a release triple; empty builds for the host.
build-cli target="":
    cargo build --release --locked --features cli --bin onejudge {{ if target == "" { "" } else { "--target " + target } }}

# Lint: clippy across every target, warnings denied (gate feature set).
lint:
    cargo clippy --all-targets --features {{gate_features}} -- -D warnings

# Format the codebase in place.
format:
    cargo fmt --all

# Fail if anything is unformatted (used by the gate).
format-check:
    cargo fmt --all --check

# Build the docs as a gate: broken intra-doc links and doc warnings fail.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features {{gate_features}}

# Supply-chain audit: advisories + license policy and unused dependencies.
audit:
    cargo deny check
    cargo machete

# Drift gate: every target install.sh downloads is built by release-binaries.yml
# (deterministic, offline). Keeps the shipped-archive naming in one enforced place.
check-release-targets:
    ./scripts/check-release-targets.sh

# Check the crate still builds on the declared MSRV (needs 1.82.0 installed).
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

# Deterministic llmlint config/ignore/version-bump validation.
lint-llm-validate *args:
    PATH="$HOME/.local/bin:$PATH" llmlint validate {{args}}

# Regenerate Python declarations and runtime schemas from Rust wire types.
python-sdk-generate:
    uv run --no-project --python 3.9 --with-requirements python/onejudge-sdk/requirements-dev.txt python python/onejudge-sdk/scripts/generate.py

# Strict Python SDK gate: generated-contract drift, lint, types, coverage, and
# an installed-wheel smoke test through the real onejudge subprocess.
python-sdk-check:
    #!/usr/bin/env bash
    set -euo pipefail
    log=$(mktemp)
    trap 'rm -f "$log"' EXIT
    if ! (
        set -euo pipefail
        command -v uv >/dev/null 2>&1 || { echo "uv not installed: https://docs.astral.sh/uv/getting-started/installation/" >&2; exit 1; }
        cargo build --locked --features {{gate_features}} --bins
        run=(uv run --no-project --python 3.9 --with-requirements python/onejudge-sdk/requirements-dev.txt)
        "${run[@]}" python python/onejudge-sdk/scripts/generate.py --check
        "${run[@]}" ruff format --check python/onejudge-sdk
        "${run[@]}" ruff check python/onejudge-sdk
        "${run[@]}" mypy --config-file python/onejudge-sdk/pyproject.toml python/onejudge-sdk/src python/onejudge-sdk/scripts python/onejudge-sdk/test
        rm -f target/python-sdk.coverage
        COVERAGE_FILE=target/python-sdk.coverage PYTHONPATH=python/onejudge-sdk/src "${run[@]}" coverage run --rcfile=python/onejudge-sdk/pyproject.toml -m unittest discover -s python/onejudge-sdk/test -p 'test_*.py'
        COVERAGE_FILE=target/python-sdk.coverage "${run[@]}" coverage report --rcfile=python/onejudge-sdk/pyproject.toml
        "${run[@]}" python python/onejudge-sdk/test/package_e2e.py
    ) >"$log" 2>&1; then
        cat "$log" >&2
        exit 1
    fi
    echo "python-sdk-check: ok"
