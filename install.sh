#!/usr/bin/env bash
# Install the `onejudge` CLI from the latest GitHub Release archive.
#
#   curl -fsSL https://raw.githubusercontent.com/nickderobertis/onejudge/main/install.sh | bash
#
# Detects your platform, downloads the matching release archive, and installs the
# `onejudge` binary. Override the install directory with ONEJUDGE_INSTALL_DIR and
# the version with ONEJUDGE_VERSION (default: latest). Prefer `cargo install
# onejudge --features cli` if you have a Rust toolchain — see README.md.
set -euo pipefail

repo="nickderobertis/onejudge"
version="${ONEJUDGE_VERSION:-latest}"
install_dir="${ONEJUDGE_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
    echo "install.sh: $1" >&2
    exit 1
}

# Map the host os/arch to a release target triple.
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
    Linux) case "$arch" in
        x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
        *) fail "unsupported Linux arch '$arch' — build from source (cargo install onejudge --features cli)" ;;
    esac ;;
    Darwin) case "$arch" in
        arm64 | aarch64) target="aarch64-apple-darwin" ;;
        x86_64) target="x86_64-apple-darwin" ;;
        *) fail "unsupported macOS arch '$arch'" ;;
    esac ;;
    *) fail "unsupported OS '$os' — on Windows download the .zip from the releases page, or use cargo install" ;;
esac

archive="onejudge-${target}.tar.gz"
if [ "$version" = "latest" ]; then
    url="https://github.com/${repo}/releases/latest/download/${archive}"
else
    url="https://github.com/${repo}/releases/download/${version}/${archive}"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $archive ($version)…"
curl -fsSL "$url" -o "$tmp/$archive" || fail "could not download $url"
tar xzf "$tmp/$archive" -C "$tmp"

mkdir -p "$install_dir"
install -m 0755 "$tmp/onejudge-${target}/onejudge" "$install_dir/onejudge"

echo "Installed onejudge to $install_dir/onejudge"
case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) echo "Note: $install_dir is not on your PATH — add it to run 'onejudge'." ;;
esac
