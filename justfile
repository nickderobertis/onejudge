# Canonical command surface for onejudge. `just --list` is the index.
#
# `just bootstrap` must work from a clean clone; `just check` is the quality gate
# and fails on any issue (no warnings-only mode). The gate is deterministic and
# offline: the model is faked by real subprocess test doubles, never mocked.
#
# Every gate recipe delegates to the Nx project graph (`nx.json`, one
# `project.json` per project) through `scripts/nx`; the `_`-prefixed recipes at
# the bottom are what each project's targets run. See AGENTS.md "Command surface".

set shell := ["bash", "-euo", "pipefail", "-c"]

# The line-coverage floor, enforced once over the union of every Rust suite's
# profiles (`workspace:coverage`). The Python SDK's floor is its own
# `fail_under` in python/onejudge-sdk/pyproject.toml.
coverage_min := "95"

# The shell line-coverage floor, enforced on the merge of every shell project's
# bashcov report (`workspace:coverage`). AGENTS.md, "Shell", records its measurement.
shell_coverage_min := "82"

# The `onejudge` feature set every Rust target that links it builds against: the
# CLI and the schema export, so both public export surfaces stay gated, and one
# feature set everywhere, so every suite links the same instrumented library and
# the aggregate coverage report merges one build of each function.
gate_features := "onejudge/sdk-schema"

# Every rustc warning is an error in every gate step — builds, tests, coverage,
# the schema checks and the wheel — not only under clippy: one flag set
# everywhere, so no step's artifacts are rebuilt under different flags.
# Dependencies keep Cargo's `--cap-lints allow`.
export RUSTFLAGS := trim(env("RUSTFLAGS", "") + " -D warnings")

# `uv` and `llmlint` install into ~/.local/bin and pixi into ~/.pixi/bin, which a
# CI runner's PATH may not carry; appended, so anything already on PATH still wins.
export PATH := if os_family() == "windows" { env("PATH") } else { env("PATH") + ":" + home_directory() + "/.local/bin:" + home_directory() + "/.pixi/bin" }

# What the coverage report measures: the published library's source, never its
# thin `onejudge` entrypoint or the test doubles' crate.
coverage_ignore := "(src/bin/|crates/onejudge-test-doubles/)"

# The Python SDK's pinned tool environment.
py_sdk := "uv run --no-project --python 3.9 --with-requirements python/onejudge-sdk/requirements-dev.txt"

# List available recipes.
default:
    @just --list

# Set up from a clean clone: pinned toolchain, cargo tools, the Nx install, the
# shell toolchain, fetched deps.
bootstrap:
    rustup show active-toolchain >/dev/null   # installs the rust-toolchain.toml channel + components
    for t in cargo-nextest cargo-llvm-cov cargo-deny cargo-machete; do \
        command -v "$t" >/dev/null 2>&1 || cargo install "$t" --locked; \
    done
    ./scripts/node-modules.sh                 # bun (pinned in package.json) + the locked Nx and bats install
    command -v uv >/dev/null 2>&1 || { curl -LsSf https://astral.sh/uv/install.sh | sh; }
    command -v pixi >/dev/null 2>&1 || { curl -fsSL https://pixi.sh/install.sh | PIXI_NO_PATH_UPDATE=1 PIXI_VERSION="v$(sed -n 's/^requires-pixi = ">=\(.*\)"$/\1/p' pixi.toml)" bash; }
    pixi install --locked                     # shellcheck, shfmt, actionlint, Ruby (pixi.lock)
    pixi run --locked bundle install --quiet  # bashcov and its gems (Gemfile.lock)
    cargo fetch --locked

# The quality gate. Default: the AFFECTED tier — every gate target of the projects
# the diff from the merge base can reach (base: NX_BASE, a ref name or commit SHA,
# else `git merge-base origin/main HEAD`). `--sweep`: the BROADER tier — every
# gate-eligible project plus the targets promoted out of the affected tier.
# `--targets a,b` / `--projects p` narrow it; `--print-plan` only prints what
# would run. scripts/gate-plan.sh decides the tier, base and projects; the two
# default target lists are spelled on the Nx lines below, the sweep's adding the
# promoted `audit`, and onejudge-repo's gate_plan.rs holds them to the script's.
[positional-arguments]
check *flags:
    #!/usr/bin/env bash
    set -euo pipefail
    plan="$(./scripts/gate-plan.sh "$@")"
    eval "$plan"
    if [ -n "${GATE_PRINT_ONLY:-}" ]; then printf '%s\n' "$plan" | sed -n 's/^# //p'; exit 0; fi
    if [ -n "$GATE_PROJECTS" ] && [ "$GATE_TIER" = affected ]; then
        ./scripts/nx affected -t ${GATE_TARGETS:-format-check lint typecheck generate-check doc build test coverage} --base="$GATE_BASE" --exclude="$GATE_EXCLUDE" --outputStyle=static
    elif [ -n "$GATE_PROJECTS" ]; then
        ./scripts/nx run-many -t ${GATE_TARGETS:-format-check lint typecheck generate-check doc build test coverage audit} --projects="$GATE_PROJECTS" --outputStyle=static
    fi
    if [ -n "$GATE_EXTERNALS" ] && [ -n "$GATE_STATIC" ]; then
        ./scripts/nx run-many --projects="$GATE_EXTERNALS" --outputStyle=static -t $GATE_STATIC
    fi

# `just gate` is the same gate under the name callers outside this repo use for
# it. An alias, not a second recipe, so the two can never drift apart.
alias gate := check

# The coverage-enforced suites: every `test` target (Rust instrumented) and the
# `coverage` aggregates, at the tier `check` would pick.
[positional-arguments]
test *flags:
    just check --targets test,coverage "$@"

# Quick inner loop: the Rust suites with no coverage instrumentation, at the tier
# `check` would pick. Also what the macOS and Windows CI jobs run.
[positional-arguments]
test-fast *flags:
    ONEJUDGE_COVERAGE=0 just check --targets test --projects 'tag:lang:rust' "$@"

# The end-to-end suites alone (real subprocess boundary, test-double binaries).
test-e2e:
    ONEJUDGE_COVERAGE=0 ./scripts/nx run-many -t test -p onejudge-e2e,onejudge-cli-e2e

# Opt-in live tier: drive a REAL oneharness + harness (never in `check`). See docs/live-tier.md.
test-live:
    ./scripts/nx run onejudge-live:test

# Opt-in real-llmlint tier: the CLI's llmlint judge over the RELEASED llmlint (no
# model, no credential — its rules match nothing), read back through
# `llmlint history`. Needs llmlint on PATH at the floor: `just setup-llmlint`.
test-llmlint:
    ./scripts/nx run onejudge-llmlint-real:test

# Drives `scripts/release-probe.sh` against the REAL public registries (no
# credential, just network) for every target `release-targets.toml` declares, and
# reconciles the canonical release-target schema this repository writes against
# with the one implementation that defines it (nickderobertis/onevcs).
# Opt-in network tier: the answers that need a network, never in offline `check`.
test-release-targets:
    ./scripts/nx run onejudge-release-targets:test

# Build the shipped `onejudge` CLI binary — the artifact the `cli-binary` PR job
# smoke-tests and `release-binaries.yml` packages. Optional `target`
# cross-compiles for a release triple; empty builds for the host.
build-cli target="":
    cargo build --release --locked --features cli --bin onejudge {{ if target == "" { "" } else { "--target " + target } }}

# Lint every affected project: clippy (warnings denied), ruff, the project
# boundaries, and the generated-schema drift checks.
[positional-arguments]
lint *flags:
    just check --targets lint "$@"

# Format the codebase in place.
format:
    ./scripts/nx run-many -t format

# Fail if anything affected is unformatted.
[positional-arguments]
format-check *flags:
    just check --targets format-check "$@"

# Build the docs as a gate: broken intra-doc links and doc warnings fail.
doc:
    ./scripts/nx run onejudge:doc

# Supply-chain audit: advisories + license policy and unused dependencies.
audit:
    ./scripts/nx run workspace:audit

# Drift gate: every target install.sh downloads is built by release-binaries.yml
# (deterministic, offline). Keeps the shipped-archive naming in one enforced place.
check-release-targets:
    ./scripts/check-release-targets.sh

# Prove SDK-only conventional commits are attributed to the release-plz package.
check-python-sdk-release-trigger:
    ./scripts/check-python-sdk-release-trigger.sh

# Check the crate still builds on the declared MSRV (needs 1.89.0 installed),
# denying warnings like every other gate step: a floor-only diagnostic fails here.
msrv:
    ./scripts/nx run workspace:msrv

# Upgrade dependencies, then re-run the broader gate; commit the refreshed lockfile.
upgrade:
    cargo update
    bun update
    pixi update
    pixi run --locked env BUNDLE_FROZEN=false bundle update --quiet
    @just check --sweep

# Install/refresh the llmlint toolchain (oneharness + llmlint). Idempotent.
setup-llmlint:
    ./scripts/setup-llmlint.sh

# LLM-judge lint (llmlint) on demand — non-deterministic, harness-backed, out of `check`.
[positional-arguments]
lint-llm *paths:
    @command -v llmlint >/dev/null 2>&1 || { echo "llmlint not installed — run 'just setup-llmlint'"; exit 1; }
    llmlint "$@"

# llmlint scoped to the merge-base diff with main — the blocking `llmlint` PR check.
lint-llm-diff base="origin/main":
    ./scripts/lint-llm-diff.sh {{base}}

# Deterministic llmlint config/ignore/version-bump validation.
[positional-arguments]
lint-llm-validate *args:
    PATH="$HOME/.local/bin:$PATH" llmlint validate "$@"

# Regenerate Python declarations and runtime schemas from Rust wire types.
python-sdk-generate:
    {{py_sdk}} python python/onejudge-sdk/scripts/generate.py

# Verify the committed schema-link bundle is generated from the frame types.
check-judge-seat-frames:
    ./scripts/check-judge-seat-frames.sh

# Verify the committed note bundle is generated from `note::Note` itself.
check-note-schema:
    cargo run -q --locked -p onejudge --features sdk-schema --example generate_note_schema -- --check

# Strict Python SDK gate: generated-contract drift, ruff format + lint, mypy,
# coverage, and the installed-wheel journey through the real onejudge subprocess.
python-sdk-check:
    ./scripts/nx run-many -t generate-check format-check lint typecheck test coverage -p onejudge-python-sdk,onejudge-python-sdk-e2e

# Show the project graph (`--file=graph.html` writes it out).
[positional-arguments]
graph *args:
    NX_SHOW_OUTPUT=1 ./scripts/nx graph "$@"

# --- Project targets: what each project.json's targets run. ------------------
# `features` is the `--features` list a crate builds with: the gate's feature set
# for every crate that links `onejudge`, empty for the two that do not.

_rust-format crate:
    cargo fmt -p {{crate}}

_rust-format-check crate:
    cargo fmt -p {{crate}} --check

# Boundaries first: a graph read that fails in a second, before a cold clippy build.
_rust-lint crate features=gate_features:
    node scripts/check-project-boundaries.mjs {{crate}}
    node scripts/check-target-commands.mjs {{crate}}
    cargo clippy --locked -p {{crate}} --all-targets {{ if features == "" { "" } else { "--features " + features } }} -- -D warnings

_rust-build crate features=gate_features:
    cargo build --locked -p {{crate}} --bins {{ if features == "" { "" } else { "--features " + features } }}

# A Rust crate's `test`. Instrumented by default: its profiles land in the shared
# `target/llvm-cov-target` directory for `workspace:coverage` to merge, and it
# drives the uninstrumented doubles `onejudge-test-doubles:build` built.
# `ONEJUDGE_COVERAGE=0` runs it plain (`just test-fast`, the macOS/Windows jobs,
# the external tiers). `binaries` names a package whose bins the suite spawns and
# which, under coverage, is built instrumented beside it — the `onejudge` CLI, so
# the lines it runs in a subprocess still count. `ignored` is nextest's
# `--run-ignored`, which only the external tiers change.
_rust-test crate features=gate_features binaries="" ignored="default":
    #!/usr/bin/env bash
    set -euo pipefail
    args=(--locked -p {{crate}} --run-ignored {{ignored}})
    features="{{features}}"
    [ -z "$features" ] || args+=(--features "$features")
    if [ "${ONEJUDGE_COVERAGE:-1}" = 0 ]; then
        exec cargo nextest run "${args[@]}"
    fi
    # Assigned before it is exported: `export` would succeed over a failed lookup.
    target_dir="$(./scripts/cargo-target-dir.sh)"
    export ONEJUDGE_TEST_DOUBLES_DIR="$target_dir/debug"
    binaries="{{binaries}}"
    [ -z "$binaries" ] || args+=(-p "$binaries" -E 'package({{crate}})')
    exec cargo llvm-cov nextest --no-report "${args[@]}"

# The real-llmlint tier's `test`, with ~/.local/bin — where `just setup-llmlint`
# installs the floor — ahead of PATH, so that install wins over any other llmlint.
# A recipe rather than the Nx target's own command, because cmd.exe cannot expand
# `$PATH` the way a target's command would need it to.
_llmlint-real-test:
    PATH="$HOME/.local/bin:$PATH" just _rust-test onejudge-llmlint-real onejudge/sdk-schema "" all

# Empty the shared profile directory before any instrumented suite writes to it,
# so the aggregate never merges a profile an earlier run left behind — nor a
# workspace crate's instrumented binary an earlier build left under another hash,
# which `cargo llvm-cov report` reads like any other and which reports source the
# tree no longer has. Dependencies' artifacts are kept, so only the workspace
# crates rebuild: what the single `cargo llvm-cov nextest` the gate ran before its
# suites split did on every run.
_coverage-clean:
    [ "${ONEJUDGE_COVERAGE:-1}" = 0 ] || cargo llvm-cov clean --workspace

# The aggregate Rust coverage gate over every suite's profiles.
#
# `--failure-mode all` is load-bearing, not laxity. The suites kill instrumented
# processes on purpose, and a child killed while the profiling runtime is still
# flushing leaves a truncated `.profraw` in the merge set. `llvm-profdata`
# defaults to `any`, where that one file aborts the merge of every other profile
# in the run and the step fails as if a test had — which is how the v0.5.0
# release chain died with 310/310 green and published nothing. `all` fails only
# when *every* profile is unmergeable, so a genuinely broken merge still stops
# the gate, and `--fail-under-lines` below still catches any real coverage loss.
# crates/onejudge/tests/coverage.rs plants that artifact, so every gate run
# proves this.
_coverage:
    cargo llvm-cov report --ignore-filename-regex '{{coverage_ignore}}' \
        --failure-mode all --fail-under-lines {{coverage_min}} --summary-only

# --- Shell: every project owning shell sources runs these (scripts/shell-files.sh
# lists a project's), with the tools pinned in pixi.toml / Gemfile.lock / package.json.

# Run a pinned tool over the shell sources the project at `root` owns.
[positional-arguments]
_sh-files root +command:
    #!/usr/bin/env bash
    set -euo pipefail
    root="$1"
    shift
    list="$(./scripts/shell-files.sh "$root")"
    if [ -z "$list" ]; then
        echo "shell: project '$root' owns no shell sources, so \`$*\` has nothing to check" >&2
        echo "ACTION: drop the shell targets from $root/project.json, or add the scripts they are for" >&2
        exit 1
    fi
    files=()
    while IFS= read -r file; do files+=("$file"); done <<<"$list"
    exec "$@" "${files[@]}"

_sh-format root:
    just _sh-files {{root}} pixi run --locked shfmt -w

# shfmt reads its style from .editorconfig, so no flag here may set one.
_sh-format-check root:
    just _sh-files {{root}} pixi run --locked shfmt -d

# A shell project's lint. `project` names it for the boundary and target-command
# checks every other project's lint runs; the workspace runs those itself.
_sh-lint root project="":
    {{ if project == "" { "" } else { "node scripts/check-project-boundaries.mjs " + project + " && node scripts/check-target-commands.mjs " + project + " && " } }}just _sh-files {{root}} pixi run --locked shellcheck

# The workflows' own lint, run by the project that owns them; actionlint checks
# each `run:` block with the pinned shellcheck it finds on the pixi PATH.
_actionlint:
    pixi run --locked actionlint

# A shell project's `test`: its bats suite (`<root>/tests`) under bashcov, which
# writes its line coverage of the scripts the project at `covers` owns — tests
# excluded — into target/shell-coverage/<project> (.simplecov) for `_sh-coverage`
# to merge. `covers` is the project itself, or for a suite in a project of its own
# (onejudge-scripts-e2e), the project whose scripts it drives. The previous report
# goes first, so the merge never reads a stale one.
_sh-test root project covers=root:
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf "target/shell-coverage/{{project}}"
    ./scripts/node-modules.sh
    list="$(./scripts/shell-files.sh {{covers}})"
    export SHELL_COVERAGE_PROJECT={{project}}
    SHELL_COVERAGE_FILES="$(grep -vE '(^|/)tests/' <<<"$list" || true)"
    export SHELL_COVERAGE_FILES
    exec pixi run --locked bundle exec bashcov --skip-uncovered --command-name {{project}} \
        -- node_modules/.bin/bats --print-output-on-failure "{{root}}/tests"

# The aggregate shell coverage gate: every shell project's bashcov report merged,
# failing below `shell_coverage_min`, with each script short of full coverage named.
_sh-coverage:
    #!/usr/bin/env bash
    set -euo pipefail
    exec pixi run --locked bundle exec ruby -e '
    require "simplecov"
    reports = Dir["target/shell-coverage/*/.resultset.json"].sort
    if reports.empty?
      abort "shell coverage: no shell project wrote a report under target/shell-coverage\n" \
            "ACTION: run the test target of each shell project first (just check --targets test,coverage)"
    end
    class ShortFiles
      def format(result)
        result.files.sort_by(&:covered_percent).each do |file|
          next if file.covered_percent >= 100
          printf("  %6.2f%%  %s (%d/%d lines)\n", file.covered_percent, file.project_filename.delete_prefix("/"),
                 file.covered_lines.size, file.covered_lines.size + file.missed_lines.size)
        end
      end
    end
    puts "shell coverage: merging #{reports.join(", ")}"
    SimpleCov.collate(reports) do
      coverage_dir "target/shell-coverage-merged"
      formatter SimpleCov::Formatter::MultiFormatter.new([SimpleCov::Formatter::HTMLFormatter, ShortFiles])
      minimum_coverage line: Float("{{shell_coverage_min}}")
    end
    '

_audit:
    cargo deny check # llmlint: ignore[diagnostics_error_or_absent] the audit's severities are deny.toml's, the repository's supply-chain policy, which this recipe runs unchanged from the `audit` recipe it was before the project graph.
    cargo machete

# The MSRV build of every target the crate's own check covered before its suites
# moved out — the library, its tests, the doubles, and the engine's e2e suites.
_msrv:
    cargo +1.89.0 check --locked -p onejudge -p onejudge-test-doubles -p onejudge-e2e --all-targets

_doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps -p onejudge --features sdk-schema

# The Python SDK's targets, from the repository root in its pinned environment.
_py-generate-check:
    {{py_sdk}} python python/onejudge-sdk/scripts/generate.py --check

_py-format dir:
    {{py_sdk}} ruff format {{dir}}

_py-format-check dir:
    {{py_sdk}} ruff format --check --no-cache {{dir}}

_py-lint dir project:
    node scripts/check-project-boundaries.mjs {{project}}
    node scripts/check-target-commands.mjs {{project}}
    {{py_sdk}} ruff check --no-cache {{dir}}

[positional-arguments]
_py-typecheck +paths:
    {{py_sdk}} mypy --config-file python/onejudge-sdk/pyproject.toml "$@"

_py-test:
    rm -f target/python-sdk.coverage
    COVERAGE_FILE=target/python-sdk.coverage PYTHONPATH=python/onejudge-sdk/src {{py_sdk}} coverage run --rcfile=python/onejudge-sdk/pyproject.toml -m unittest discover -s python/onejudge-sdk/test -p 'test_*.py'

_py-coverage:
    COVERAGE_FILE=target/python-sdk.coverage {{py_sdk}} coverage report --rcfile=python/onejudge-sdk/pyproject.toml

_py-package-e2e:
    {{py_sdk}} python python/onejudge-sdk-e2e/package_e2e.py

# The PyPI CLI wheel (maturin, the root pyproject.toml): build it, then install it
# into a fresh venv and run the packaged console command.
_pypi-build:
    rm -rf target/pypi-wheel
    uvx maturin build --release --locked --out target/pypi-wheel

_pypi-test:
    rm -rf target/pypi-wheel/smoke
    uv venv --quiet target/pypi-wheel/smoke
    uv pip install --quiet --python target/pypi-wheel/smoke/bin/python target/pypi-wheel/*.whl
    target/pypi-wheel/smoke/bin/onejudge --help >/dev/null
