#!/usr/bin/env bats
# scripts/node-modules.sh installing the locked Nx when it is missing: bun and npm
# are doubles, run with only the tools the script needs on PATH, over a scratch
# tree holding this repository's package.json and bun.lock. The real install's
# fast path is onejudge-scripts-e2e's.

load support/helpers

# install_tree — a tree with the locked manifest and no install.
install_tree() {
    TREE="$BATS_TEST_TMPDIR/tree"
    link "$TREE" scripts/node-modules.sh
    cp "$ROOT/package.json" "$ROOT/bun.lock" "$TREE/"
    BUN_LOG="$BATS_TEST_TMPDIR/bun.log"
    export BUN_LOG
}

bun_installs() {
    double bun <<'EOF'
printf 'bun %s\n' "$*" >>"$BUN_LOG"
mkdir -p node_modules/.bin
: >node_modules/.bin/nx
EOF
}

@test "node-modules: a missing install is made with the frozen lockfile and stamped" {
    install_tree
    only_tools cmp sed cp mkdir chmod dirname cat
    bun_installs

    run env PATH="$ONLY_PATH" "$TREE/scripts/node-modules.sh"

    [ "$status" -eq 0 ]
    [ -z "$output" ]
    [ "$(cat "$BUN_LOG")" = "bun install --frozen-lockfile --silent" ]
    cmp -s "$TREE/bun.lock" "$TREE/node_modules/.bun-lock-installed"

    run env PATH="$ONLY_PATH" "$TREE/scripts/node-modules.sh"
    [ "$status" -eq 0 ]
    [ "$(wc -l <"$BUN_LOG" | tr -d ' ')" = 1 ]
}

@test "node-modules: a failed install says what to check and leaves no stamp" {
    install_tree
    only_tools cmp sed cp mkdir chmod dirname cat
    double bun <<'EOF'
exit 1
EOF

    run env PATH="$ONLY_PATH" "$TREE/scripts/node-modules.sh"

    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "node-modules: 'bun install --frozen-lockfile' failed" ]
    [[ "${lines[1]}" == "ACTION: if package.json changed, run 'bun install' and commit bun.lock;"* ]]
    [ ! -e "$TREE/node_modules/.bun-lock-installed" ]
}

@test "node-modules: without bun, the pinned bun is installed through npm" {
    install_tree
    only_tools cmp sed cp mkdir chmod dirname cat
    NPM_LOG="$BATS_TEST_TMPDIR/npm.log"
    export NPM_LOG
    double npm <<'EOF'
printf 'npm %s\n' "$*" >"$NPM_LOG"
printf '#!/usr/bin/env bash\nprintf "bun %%s\\n" "$*" >>"$BUN_LOG"\nmkdir -p node_modules/.bin\n: >node_modules/.bin/nx\n' >"$(dirname "$0")/bun"
chmod +x "$(dirname "$0")/bun"
EOF
    version="$(sed -n 's/.*"packageManager": *"bun@\([0-9][0-9.]*\)".*/\1/p' "$ROOT/package.json")"

    run env PATH="$ONLY_PATH" "$TREE/scripts/node-modules.sh"

    [ "$status" -eq 0 ]
    [ "$(cat "$NPM_LOG")" = "npm install --global --silent bun@$version" ]
    [ "$(cat "$BUN_LOG")" = "bun install --frozen-lockfile --silent" ]
}

@test "node-modules: without bun, a failed npm install names the version to install" {
    install_tree
    only_tools cmp sed cp mkdir chmod dirname cat
    double npm <<'EOF'
exit 1
EOF
    version="$(sed -n 's/.*"packageManager": *"bun@\([0-9][0-9.]*\)".*/\1/p' "$ROOT/package.json")"

    run env PATH="$ONLY_PATH" "$TREE/scripts/node-modules.sh"

    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "node-modules: 'npm install --global bun@$version' failed" ]
    [ "${lines[1]}" = "ACTION: install bun $version yourself (https://bun.sh), then re-run 'just bootstrap'" ]
}

@test "node-modules: without bun or npm, it says to install bun at the pinned version" {
    install_tree
    only_tools cmp sed cp mkdir chmod dirname cat
    version="$(sed -n 's/.*"packageManager": *"bun@\([0-9][0-9.]*\)".*/\1/p' "$ROOT/package.json")"

    run env PATH="$ONLY_PATH" "$TREE/scripts/node-modules.sh"

    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "node-modules: bun is not installed and cannot be installed here (needs npm and package.json's packageManager)" ]
    [ "${lines[1]}" = "ACTION: install bun $version — https://bun.sh — then re-run 'just bootstrap'" ]
}
