#!/usr/bin/env bats
# scripts/shell-toolchain.sh installing the pinned shell toolchain for `just
# bootstrap`: pixi (when absent, from its release archive, verified against
# pixi.sha256), then the locked environment and gems. curl, uname and pixi are
# doubles, HOME is scratch, and the tree holds this repository's pixi.toml, so
# nothing is installed for the user running the suite. The real install is
# onejudge-scripts-e2e's.

load support/helpers

setup() {
    TREE="$BATS_TEST_TMPDIR/tree"
    link "$TREE" scripts/shell-toolchain.sh
    cp "$ROOT/pixi.toml" "$TREE/"
    export HOME="$BATS_TEST_TMPDIR/home"
    PIXI_LOG="$BATS_TEST_TMPDIR/pixi.log"
    export PIXI_LOG
    local digest_tool=sha256sum
    command -v sha256sum >/dev/null 2>&1 || digest_tool=shasum
    only_tools sed dirname cat mkdir chmod mktemp rm tar gzip "$digest_tool"
    VERSION="$(sed -n 's/^requires-pixi = ">=\(.*\)"$/\1/p' "$ROOT/pixi.toml")"
    ARCHIVE="v$VERSION/pixi-x86_64-unknown-linux-musl.tar.gz"
    double uname <<'EOF'
case "$1" in -s) echo "${UNAME_S:-Linux}" ;; -m) echo "${UNAME_M:-x86_64}" ;; esac
EOF
}

# release [MEMBER [MODE]] — the archive the curl double serves, holding a pixi
# double named MEMBER (pixi) with MODE (755), and pixi.sha256 pinning its digest.
release() {
    local staging="$BATS_TEST_TMPDIR/staging"
    mkdir -p "$staging"
    cat >"$staging/${1:-pixi}" <<'EOF'
#!/usr/bin/env bash
printf 'pixi %s\n' "$*" >>"$PIXI_LOG"
EOF
    chmod "${2:-755}" "$staging/${1:-pixi}"
    RELEASE="$BATS_TEST_TMPDIR/release.tar.gz"
    tar -czf "$RELEASE" -C "$staging" "${1:-pixi}"
    export RELEASE
    printf '%s  %s\n' "$(digest "$RELEASE")" "$ARCHIVE" >"$TREE/pixi.sha256"
    double curl <<'EOF'
printf 'curl %s\n' "$*" >>"$PIXI_LOG"
while [ $# -gt 0 ]; do
    if [ "$1" = -o ]; then cp "$RELEASE" "$2"; fi
    shift
done
EOF
    ln -sf "$(command -v cp)" "$BATS_TEST_TMPDIR/tools/cp"
}

digest() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# pixi_double [FAILING-SUBCOMMAND] — a pixi on the doubles PATH logging each call,
# failing the named subcommand.
pixi_double() {
    double pixi <<EOF
printf 'pixi %s\n' "\$*" >>"\$PIXI_LOG"
[ "\$1" != "${1:-none}" ]
EOF
}

@test "the pinned digests cover every platform pixi is installed on, at the version pixi.toml requires" {
    [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
    for triple in x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-apple-darwin aarch64-apple-darwin; do
        grep -qE "^[0-9a-f]{64}  v$VERSION/pixi-$triple\.tar\.gz\$" "$ROOT/pixi.sha256"
    done
}

@test "with pixi installed, the locked environment and gems are installed, and nothing is downloaded" {
    pixi_double
    double curl <<'EOF'
echo "curl must not run" >&2
exit 1
EOF

    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"

    [ "$status" -eq 0 ]
    [ "$(cat "$PIXI_LOG")" = "$(printf '%s\n' 'pixi install --locked' 'pixi run --locked bundle install --quiet')" ]
}

@test "without pixi, the required release is downloaded, verified, installed into ~/.pixi/bin, then used" {
    release

    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"

    [ "$status" -eq 0 ]
    [[ "$(sed -n 1p "$PIXI_LOG")" == "curl -fsSL https://github.com/prefix-dev/pixi/releases/download/$ARCHIVE -o "* ]]
    [ "$(sed -n '2,$p' "$PIXI_LOG")" = "$(printf '%s\n' 'pixi install --locked' 'pixi run --locked bundle install --quiet')" ]
    [ -x "$HOME/.pixi/bin/pixi" ]
}

@test "without sha256sum, as on macOS, the archive is verified with shasum" {
    release
    command -v sha256sum >/dev/null 2>&1 || skip "this host has no sha256sum to stand in for shasum's digest"
    rm -f "$BATS_TEST_TMPDIR/tools/sha256sum"
    SHA256SUM="$(command -v sha256sum)"
    export SHA256SUM
    double shasum <<'EOF'
printf 'shasum %s\n' "$*" >>"$PIXI_LOG"
[ "$1 $2" = "-a 256" ] && shift 2
exec "$SHA256SUM" "$@"
EOF

    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"

    [ "$status" -eq 0 ]
    [[ "$(sed -n 2p "$PIXI_LOG")" == "shasum -a 256 "*/pixi.tar.gz ]]
    [ -x "$HOME/.pixi/bin/pixi" ]
}

@test "an archive whose digest is not the pinned one is refused, and nothing is installed" {
    release
    printf '%064d  %s\n' 0 "$ARCHIVE" >"$TREE/pixi.sha256"

    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"

    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "shell-toolchain: $ARCHIVE has sha256 $(digest "$RELEASE"), not the $(printf '%064d' 0) pixi.sha256 pins, so it was not installed" ]
    [ "${lines[1]}" = "ACTION: re-run 'just bootstrap'; if the digest still differs, compare it with the release's $ARCHIVE.sha256 asset before changing pixi.sha256" ]
    [ ! -e "$HOME/.pixi/bin/pixi" ]
}

@test "a platform or version with no pinned digest is refused before anything is downloaded" {
    release
    : >"$TREE/pixi.sha256"
    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"
    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "shell-toolchain: pixi.sha256 pins no sha256 for $ARCHIVE" ]
    [ "${lines[1]}" = "ACTION: add the release's digest (its $ARCHIVE.sha256 asset) to pixi.sha256, then re-run 'just bootstrap'" ]
    [ ! -e "$PIXI_LOG" ]

    run env PATH="$ONLY_PATH" UNAME_S=FreeBSD UNAME_M=amd64 "$TREE/scripts/shell-toolchain.sh"
    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "shell-toolchain: no pixi release is pinned for FreeBSD amd64" ]
    [ "${lines[1]}" = "ACTION: install pixi $VERSION yourself (https://pixi.sh), then re-run 'just bootstrap'" ]
    [ ! -e "$PIXI_LOG" ]
}

@test "each supported platform looks up the digest of its own archive" {
    release
    for platform in "Linux aarch64 aarch64-unknown-linux-musl" "Linux arm64 aarch64-unknown-linux-musl" \
        "Darwin x86_64 x86_64-apple-darwin" "Darwin arm64 aarch64-apple-darwin"; do
        read -r os machine triple <<<"$platform"
        rm -f "$PIXI_LOG"

        run env PATH="$ONLY_PATH" UNAME_S="$os" UNAME_M="$machine" "$TREE/scripts/shell-toolchain.sh"

        [ "$status" -eq 1 ]
        [ "${lines[0]}" = "shell-toolchain: pixi.sha256 pins no sha256 for v$VERSION/pixi-$triple.tar.gz" ]
    done
}

@test "a pixi.toml without one requires-pixi version is refused before anything is fetched" {
    release
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

@test "a failed download names the archive and the version to install by hand" {
    release
    double curl <<'EOF'
echo "curl: (6) Could not resolve host: github.com" >&2
exit 6
EOF

    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"

    [ "$status" -eq 1 ]
    [ "${lines[1]}" = "shell-toolchain: downloading $ARCHIVE from pixi's GitHub releases failed (above)" ]
    [ "${lines[2]}" = "ACTION: check network access to github.com, or install pixi $VERSION yourself (https://pixi.sh), then re-run 'just bootstrap'" ]
}

@test "an archive without pixi, or with one that cannot run, is reported and not used" {
    release not-pixi
    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"
    [ "$status" -eq 1 ]
    [ "${lines[-2]}" = "shell-toolchain: extracting pixi from $ARCHIVE into $HOME/.pixi/bin failed (above)" ]
    [ "${lines[-1]}" = "ACTION: check that $HOME/.pixi/bin is writable, then re-run 'just bootstrap'" ]

    rm -rf "$BATS_TEST_TMPDIR/staging"
    release pixi 644
    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"
    [ "$status" -eq 1 ]
    [ "${lines[0]}" = "shell-toolchain: $ARCHIVE held no runnable pixi for $HOME/.pixi/bin" ]
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

@test "a repository root it cannot enter, or a pixi.toml it cannot read, names what to restore" {
    double dirname <<'EOF'
echo /nonexistent/scripts
EOF
    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"
    [ "$status" -eq 1 ]
    [ "${lines[1]}" = "shell-toolchain: cannot enter the repository root above $TREE/scripts/shell-toolchain.sh" ]
    [ "${lines[2]}" = "ACTION: run the script from a complete checkout of the repository" ]
    rm "$DOUBLES/dirname"

    rm "$TREE/pixi.toml"
    run env PATH="$ONLY_PATH" "$TREE/scripts/shell-toolchain.sh"
    [ "$status" -eq 1 ]
    [ "${lines[1]}" = "shell-toolchain: reading pixi.toml failed (above)" ]
    [ "${lines[2]}" = "ACTION: restore pixi.toml (git checkout -- pixi.toml), then re-run 'just bootstrap'" ]
}
