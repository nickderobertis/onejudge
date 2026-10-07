#!/usr/bin/env bash
# The shell toolchain, installed at its pins for `just bootstrap`: shellcheck,
# shfmt, actionlint and Ruby from pixi.lock, and bashcov's gems from Gemfile.lock.
#
# pixi itself is installed when absent, at the version pixi.toml's one
# `requires-pixi = ">=X.Y.Z"` line names, by pixi's installer into ~/.pixi/bin
# (which the justfile puts on PATH). Then `pixi install --locked`, and `bundle
# install` into that environment (pixi.toml's activation points BUNDLE_* there).
#
# Says only what the installers print; on failure, the step and the next action.
set -euo pipefail

if ! cd "$(dirname "${BASH_SOURCE[0]}")/.."; then
    echo "shell-toolchain: cannot enter the repository root above ${BASH_SOURCE[0]}" >&2
    echo "ACTION: run the script from a complete checkout of the repository" >&2
    exit 1
fi

if ! command -v pixi >/dev/null 2>&1; then
    if ! version="$(sed -n 's/^requires-pixi = ">=\(.*\)"$/\1/p' pixi.toml)"; then
        echo "shell-toolchain: reading pixi.toml failed (above)" >&2
        echo "ACTION: restore pixi.toml (git checkout -- pixi.toml), then re-run 'just bootstrap'" >&2
        exit 1
    fi
    if ! [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "shell-toolchain: pixi.toml has no single requires-pixi = \">=X.Y.Z\" line to install pixi at (read: '$version')" >&2
        echo "ACTION: restore that line in pixi.toml, then re-run 'just bootstrap'" >&2
        exit 1
    fi
    if ! installer="$(curl -fsSL https://pixi.sh/install.sh)"; then
        echo "shell-toolchain: downloading pixi's installer from https://pixi.sh/install.sh failed (above)" >&2
        echo "ACTION: check network access to pixi.sh, or install pixi $version yourself (https://pixi.sh), then re-run 'just bootstrap'" >&2
        exit 1
    fi
    if ! PIXI_NO_PATH_UPDATE=1 PIXI_VERSION="v$version" bash -c "$installer" >&2; then
        echo "shell-toolchain: pixi's installer failed for v$version (above)" >&2
        echo "ACTION: install pixi $version yourself (https://pixi.sh), then re-run 'just bootstrap'" >&2
        exit 1
    fi
    PATH="$PATH:$HOME/.pixi/bin"
    if ! command -v pixi >/dev/null 2>&1; then
        echo "shell-toolchain: pixi's installer succeeded but left no pixi in $HOME/.pixi/bin" >&2
        echo "ACTION: install pixi $version yourself (https://pixi.sh), then re-run 'just bootstrap'" >&2
        exit 1
    fi
fi

if ! pixi install --locked; then
    echo "shell-toolchain: 'pixi install --locked' failed (above)" >&2
    echo "ACTION: if pixi.toml changed, run 'pixi install' and commit pixi.lock; otherwise check network access to conda-forge" >&2
    exit 1
fi
if ! pixi run --locked bundle install --quiet; then
    echo "shell-toolchain: 'bundle install' of Gemfile.lock's gems failed (above)" >&2
    echo "ACTION: if the Gemfile changed, run 'just upgrade' and commit Gemfile.lock; otherwise check network access to rubygems.org" >&2
    exit 1
fi
