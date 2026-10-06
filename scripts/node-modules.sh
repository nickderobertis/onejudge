#!/usr/bin/env bash
# The workspace's locked Nx install (package.json + bun.lock), healed if missing.
#
# A fresh clone — and a CI job that never ran `just bootstrap` — has no
# `node_modules`, so every entry point that needs Nx heals through here rather
# than failing with "cannot find nx". bun is the package manager the lockfile
# belongs to; when it is absent it is installed at the version `packageManager`
# pins, through npm (present wherever Node is, which Nx needs anyway).
#
# Quiet on success and idempotent. Anything it says goes to stderr, because a
# caller reading Nx's stdout (`nx show projects --json`) must get only that.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

stamp=node_modules/.bun-lock-installed
if [ -e node_modules/.bin/nx ] && [ -e "$stamp" ] && cmp -s bun.lock "$stamp"; then
    exit 0
fi

if ! command -v bun >/dev/null 2>&1; then
    version="$(sed -n 's/.*"packageManager": *"bun@\([0-9][0-9.]*\)".*/\1/p' package.json)"
    if [ -z "$version" ] || ! command -v npm >/dev/null 2>&1; then
        echo "node-modules: bun is not installed and cannot be installed here (needs npm and package.json's packageManager)" >&2
        echo "ACTION: install bun ${version:-(see package.json packageManager)} — https://bun.sh — then re-run 'just bootstrap'" >&2
        exit 1
    fi
    if ! npm install --global --silent "bun@$version" >&2; then
        echo "node-modules: 'npm install --global bun@$version' failed" >&2
        echo "ACTION: install bun $version yourself (https://bun.sh), then re-run 'just bootstrap'" >&2
        exit 1
    fi
fi

if ! bun install --frozen-lockfile --silent >&2; then
    echo "node-modules: 'bun install --frozen-lockfile' failed" >&2
    echo "ACTION: if package.json changed, run 'bun install' and commit bun.lock; otherwise check network access to the npm registry" >&2
    exit 1
fi
cp bun.lock "$stamp"
