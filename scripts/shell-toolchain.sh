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

die() {
    echo "shell-toolchain: $1" >&2
    echo "ACTION: $2" >&2
    exit 1
}

cd "$(dirname "${BASH_SOURCE[0]}")/.." || die "cannot enter the repository root above ${BASH_SOURCE[0]}" "run the script from a complete checkout of the repository"

if ! command -v pixi >/dev/null 2>&1; then
    version="$(sed -n 's/^requires-pixi = ">=\(.*\)"$/\1/p' pixi.toml)" || die "reading pixi.toml failed (above)" "restore pixi.toml (git checkout -- pixi.toml), then re-run 'just bootstrap'"
    [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "pixi.toml has no single requires-pixi = \">=X.Y.Z\" line to install pixi at (read: '$version')" "restore that line in pixi.toml, then re-run 'just bootstrap'"
    installer="$(curl -fsSL https://pixi.sh/install.sh)" || die "downloading pixi's installer from https://pixi.sh/install.sh failed (above)" "check network access to pixi.sh, or install pixi $version yourself (https://pixi.sh), then re-run 'just bootstrap'"
    PIXI_NO_PATH_UPDATE=1 PIXI_VERSION="v$version" bash -c "$installer" >&2 || die "pixi's installer failed for v$version (above)" "install pixi $version yourself (https://pixi.sh), then re-run 'just bootstrap'"
    PATH="$PATH:$HOME/.pixi/bin"
    command -v pixi >/dev/null 2>&1 || die "pixi's installer succeeded but left no pixi in $HOME/.pixi/bin" "install pixi $version yourself (https://pixi.sh), then re-run 'just bootstrap'"
fi

pixi install --locked || die "'pixi install --locked' failed (above)" "if pixi.toml changed, run 'pixi install' and commit pixi.lock; otherwise check network access to conda-forge"
pixi run --locked bundle install --quiet || die "'bundle install' of Gemfile.lock's gems failed (above)" "if the Gemfile changed, run 'just upgrade' and commit Gemfile.lock; otherwise check network access to rubygems.org"
