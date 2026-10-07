#!/usr/bin/env bash
# The shell toolchain, installed at its pins for `just bootstrap`: shellcheck,
# shfmt, actionlint and Ruby from pixi.lock, and bashcov's gems from Gemfile.lock.
#
# pixi itself is installed when absent, at the version pixi.toml's one
# `requires-pixi = ">=X.Y.Z"` line names: that release's archive for this
# platform, verified against the digest pixi.sha256 pins for it before anything
# is extracted, into ~/.pixi/bin (which the justfile puts on PATH). Then `pixi
# install --locked`, and `bundle install` into that environment (pixi.toml's
# activation points BUNDLE_* there).
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
    case "$(uname -s)-$(uname -m)" in
        Linux-x86_64) triple=x86_64-unknown-linux-musl ;;
        Linux-aarch64 | Linux-arm64) triple=aarch64-unknown-linux-musl ;;
        Darwin-x86_64) triple=x86_64-apple-darwin ;;
        Darwin-arm64) triple=aarch64-apple-darwin ;;
        *)
            echo "shell-toolchain: no pixi release is pinned for $(uname -s) $(uname -m)" >&2
            echo "ACTION: install pixi $version yourself (https://pixi.sh), then re-run 'just bootstrap'" >&2
            exit 1
            ;;
    esac
    archive="v$version/pixi-$triple.tar.gz"
    if [ ! -r pixi.sha256 ]; then
        echo "shell-toolchain: pixi.sha256 cannot be read, so no archive can be verified" >&2
        echo "ACTION: restore pixi.sha256 (git checkout -- pixi.sha256), then re-run 'just bootstrap'" >&2
        exit 1
    fi
    expected=""
    while read -r digest name; do
        if [ "$name" = "$archive" ]; then
            expected="$digest"
        fi
    done <pixi.sha256
    if [[ ! $expected =~ ^[0-9a-f]{64}$ ]]; then
        echo "shell-toolchain: pixi.sha256 pins no sha256 for $archive" >&2
        echo "ACTION: add the release's digest (its $archive.sha256 asset) to pixi.sha256, then re-run 'just bootstrap'" >&2
        exit 1
    fi
    if ! download="$(mktemp -d)"; then
        echo "shell-toolchain: creating a temporary directory for the download failed (above)" >&2
        echo "ACTION: check that ${TMPDIR:-/tmp} exists and is writable (or point TMPDIR at one that is), then re-run 'just bootstrap'" >&2
        exit 1
    fi
    remove_download() {
        if ! rm -rf "$download"; then
            echo "shell-toolchain: could not remove the download directory $download (above)" >&2
            echo "ACTION: remove $download by hand; the install itself is unaffected" >&2
        fi
    }
    trap remove_download EXIT
    if ! curl -fsSL "https://github.com/prefix-dev/pixi/releases/download/$archive" -o "$download/pixi.tar.gz"; then
        echo "shell-toolchain: downloading $archive from pixi's GitHub releases failed (above)" >&2
        echo "ACTION: check network access to github.com, or install pixi $version yourself (https://pixi.sh), then re-run 'just bootstrap'" >&2
        exit 1
    fi
    # sha256sum on Linux; macOS ships shasum instead.
    digest_tool=(sha256sum)
    if ! command -v sha256sum >/dev/null 2>&1; then
        digest_tool=(shasum -a 256)
    fi
    if ! actual="$("${digest_tool[@]}" "$download/pixi.tar.gz")"; then
        echo "shell-toolchain: computing the sha256 of $archive with ${digest_tool[0]} failed (above)" >&2
        echo "ACTION: check that ${digest_tool[0]} works, then re-run 'just bootstrap'" >&2
        exit 1
    fi
    if [ "${actual%% *}" != "$expected" ]; then
        echo "shell-toolchain: $archive has sha256 ${actual%% *}, not the $expected pixi.sha256 pins, so it was not installed" >&2
        echo "ACTION: re-run 'just bootstrap'; if the digest still differs, compare it with the release's $archive.sha256 asset before changing pixi.sha256" >&2
        exit 1
    fi
    if ! mkdir -p "$HOME/.pixi/bin"; then
        echo "shell-toolchain: creating $HOME/.pixi/bin failed (above)" >&2
        echo "ACTION: check that $HOME is writable, then re-run 'just bootstrap'" >&2
        exit 1
    fi
    if ! tar -xzf "$download/pixi.tar.gz" -C "$HOME/.pixi/bin" pixi; then
        echo "shell-toolchain: extracting pixi from $archive into $HOME/.pixi/bin failed (above)" >&2
        echo "ACTION: check that $HOME/.pixi/bin is writable, then re-run 'just bootstrap'" >&2
        exit 1
    fi
    if [ ! -x "$HOME/.pixi/bin/pixi" ]; then
        echo "shell-toolchain: $archive held no runnable pixi for $HOME/.pixi/bin" >&2
        echo "ACTION: install pixi $version yourself (https://pixi.sh), then re-run 'just bootstrap'" >&2
        exit 1
    fi
    PATH="$PATH:$HOME/.pixi/bin"
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
