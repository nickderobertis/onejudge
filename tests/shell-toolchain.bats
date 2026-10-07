#!/usr/bin/env bats
# scripts/shell-toolchain.sh installing the pinned shell toolchain for `just
# bootstrap`: pixi (installed through its installer when absent), then the locked
# environment and gems. curl and pixi are doubles, HOME is scratch, and the tree
# holds this repository's pixi.toml, so nothing is installed for the user running
# the suite. The real install is onejudge-scripts-e2e's.

load support/helpers

setup() {
    TREE="$BATS_TEST_TMPDIR/tree"
    link "$TREE" scripts/shell-toolchain.sh
    cp "$ROOT/pixi.toml" "$TREE/"
    export HOME="$BATS_TEST_TMPDIR/home"
    PIXI_LOG="$BATS_TEST_TMPDIR/pixi.log"
    export PIXI_LOG
    only_tools sed dirname cat mkdir chmod
    VERSION="$(sed -n 's/^requires-pixi = ">=\(.*\)"$/\1/p' "$ROOT/pixi.toml")"
}

# pixi_double [FAILING-SUBCOMMAND] — a pixi on the doubles PATH logging each call,
# failing the named subcommand.
pixi_double() {
    double pixi <<EOF
printf 'pixi %s\n' "\$*" >>"\$PIXI_LOG"
[ "\$1" != "${1:-none}" ]
EOF
}

# curl_serves_installer — a curl serving an installer that records the version it
# was asked for and leaves a pixi in ~/.pixi/bin, as pixi's does.
curl_serves_installer() {
    double curl <<'EOF'
cat <<'INSTALLER'
printf 'installer PIXI_VERSION=%s PIXI_NO_PATH_UPDATE=%s\n' "$PIXI_VERSION" "$PIXI_NO_PATH_UPDATE" >>"$PIXI_LOG"
mkdir -p "$HOME/.pixi/bin"
printf '#!/usr/bin/env bash\nprintf "pixi %%s\\n" "$*" >>"$PIXI_LOG"\n' >"$HOME/.pixi/bin/pixi"
chmod +x "$HOME/.pixi/bin/pixi"
INSTALLER
EOF
}

@test "with pixi installed, the locked environment and gems are installed, and no installer is fetched" {
    pixi_double
    double curl <<'EOF'
echo "curl must not run" >&2
exit 1
EOF

    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"

    [ "$status" -eq 0 ]
    [ "$(cat "$PIXI_LOG")" = "$(printf '%s\n' 'pixi install --locked' 'pixi run --locked bundle install --quiet')" ]
}

@test "without pixi, the version pixi.toml requires is installed into ~/.pixi/bin, then used" {
    curl_serves_installer

    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"

    [ "$status" -eq 0 ]
    [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
    [ "$(cat "$PIXI_LOG")" = "$(printf '%s\n' "installer PIXI_VERSION=v$VERSION PIXI_NO_PATH_UPDATE=1" 'pixi install --locked' 'pixi run --locked bundle install --quiet')" ]
}

@test "a pixi.toml without one requires-pixi version is refused before anything is fetched" {
    curl_serves_installer
    for pin in 'requires-pixi = "0.81"' 'requires-pixi = ">=latest"' ''; do
        sed -i.bak '/^requires-pixi/d' "$TREE/pixi.toml"
        [ -z "$pin" ] || echo "$pin" >>"$TREE/pixi.toml"

        run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"

        [ "$status" -eq 1 ]
        [[ "${lines[0]}" == "shell-toolchain: pixi.toml has no single requires-pixi = \">=X.Y.Z\" line to install pixi at (read: '"* ]]
        [ "${lines[1]}" = "ACTION: restore that line in pixi.toml, then re-run 'just bootstrap'" ]
        [ ! -e "$PIXI_LOG" ]
    done
}

@test "a failed installer download names pixi.sh and the version to install by hand" {
    double curl <<'EOF'
echo "curl: (6) Could not resolve host: pixi.sh" >&2
exit 6
EOF

    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"

    [ "$status" -eq 1 ]
    [ "${lines[1]}" = "shell-toolchain: downloading pixi's installer from https://pixi.sh/install.sh failed (above)" ]
    [ "${lines[2]}" = "ACTION: check network access to pixi.sh, or install pixi $VERSION yourself (https://pixi.sh), then re-run 'just bootstrap'" ]
}

@test "an installer that fails, or leaves no pixi, says to install that version by hand" {
    double curl <<'EOF'
echo 'exit 3'
EOF
    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"
    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "shell-toolchain: pixi's installer failed for v$VERSION (above)" ]

    double curl <<'EOF'
echo 'true'
EOF
    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"
    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "shell-toolchain: pixi's installer succeeded but left no pixi in $HOME/.pixi/bin" ]
    [ "${lines[1]}" = "ACTION: install pixi $VERSION yourself (https://pixi.sh), then re-run 'just bootstrap'" ]
}

@test "a failed environment or gem install names the step and what to check" {
    pixi_double install
    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"
    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "shell-toolchain: 'pixi install --locked' failed (above)" ]
    [ "${lines[1]}" = "ACTION: if pixi.toml changed, run 'pixi install' and commit pixi.lock; otherwise check network access to conda-forge" ]

    pixi_double run
    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"
    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "shell-toolchain: 'bundle install' of Gemfile.lock's gems failed (above)" ]
    [ "${lines[1]}" = "ACTION: if the Gemfile changed, run 'just upgrade' and commit Gemfile.lock; otherwise check network access to rubygems.org" ]
}
