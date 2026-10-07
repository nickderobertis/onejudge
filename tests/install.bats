#!/usr/bin/env bats
# install.sh, the public install path: it maps the host to a release target,
# downloads that target's archive and installs the binary. `uname` and `curl` are
# doubles; the archive is a real tarball shaped like release-binaries.yml's.

load helpers

setup() {
    use_doubles
    INSTALL_DIR="$BATS_TEST_TMPDIR/install"
    export ONEJUDGE_INSTALL_DIR="$INSTALL_DIR"
    unset ONEJUDGE_VERSION
    URL_LOG="$BATS_TEST_TMPDIR/curl-url"
    export URL_LOG
}

# host OS ARCH — `uname -s` answers OS and `uname -m` answers ARCH.
host() {
    double uname <<EOF
case "\$1" in -s) echo "$1" ;; -m) echo "$2" ;; *) exit 64 ;; esac
EOF
}

# release TARGET — curl serves a release archive for TARGET from the URL it is
# given (recorded in URL_LOG), honouring `-o`.
release() {
    local staging="$BATS_TEST_TMPDIR/release"
    mkdir -p "$staging/onejudge-$1"
    printf '#!/bin/sh\necho onejudge 9.9.9\n' >"$staging/onejudge-$1/onejudge"
    tar czf "$BATS_TEST_TMPDIR/onejudge-$1.tar.gz" -C "$staging" "onejudge-$1"
    ARCHIVE="$BATS_TEST_TMPDIR/onejudge-$1.tar.gz"
    export ARCHIVE
    double curl <<'EOF'
out=""
url=""
while [ $# -gt 0 ]; do
    case "$1" in
        -o) out="$2"; shift ;;
        -*) ;;
        *) url="$1" ;;
    esac
    shift
done
printf '%s\n' "$url" >"$URL_LOG"
cp "$ARCHIVE" "$out"
EOF
}

@test "a Linux x86_64 host installs the latest gnu archive and says the install dir is off PATH" {
    host Linux x86_64
    release x86_64-unknown-linux-gnu

    run "$ROOT/install.sh"

    [ "$status" -eq 0 ]
    [ "$(cat "$URL_LOG")" = "https://github.com/nickderobertis/onejudge/releases/latest/download/onejudge-x86_64-unknown-linux-gnu.tar.gz" ]
    [ "$("$INSTALL_DIR/onejudge")" = "onejudge 9.9.9" ]
    [ "${lines[0]}" = "Installed onejudge to $INSTALL_DIR/onejudge" ]
    [ "${lines[1]}" = "Note: $INSTALL_DIR is not on your PATH — add it to run 'onejudge'." ]
}

@test "a pinned version on an Apple silicon host fetches that tag's aarch64 archive" {
    host Darwin arm64
    release aarch64-apple-darwin
    PATH="$INSTALL_DIR:$PATH"

    ONEJUDGE_VERSION=v1.2.3 run "$ROOT/install.sh"

    [ "$status" -eq 0 ]
    [ "$(cat "$URL_LOG")" = "https://github.com/nickderobertis/onejudge/releases/download/v1.2.3/onejudge-aarch64-apple-darwin.tar.gz" ]
    [ "$output" = "Installed onejudge to $INSTALL_DIR/onejudge" ]
    [ -x "$INSTALL_DIR/onejudge" ]
}

@test "an Intel macOS host fetches the x86_64 darwin archive" {
    host Darwin x86_64
    release x86_64-apple-darwin

    run "$ROOT/install.sh"

    [ "$status" -eq 0 ]
    [[ "$(cat "$URL_LOG")" == */onejudge-x86_64-apple-darwin.tar.gz ]]
}

@test "an unsupported Linux arch is refused before any download" {
    host Linux riscv64
    release x86_64-unknown-linux-gnu

    run "$ROOT/install.sh"

    [ "$status" -eq 1 ]
    [ "$output" = "install.sh: unsupported Linux arch 'riscv64' — build from source (cargo install onejudge --features cli)" ]
    [ ! -e "$URL_LOG" ]
}

@test "an unsupported macOS arch is refused" {
    host Darwin ppc

    run "$ROOT/install.sh"

    [ "$status" -eq 1 ]
    [ "$output" = "install.sh: unsupported macOS arch 'ppc' — build from source (cargo install onejudge --features cli)" ]
}

@test "an unsupported OS points at the zip or cargo install" {
    host MINGW64_NT x86_64

    run "$ROOT/install.sh"

    [ "$status" -eq 1 ]
    [ "$output" = "install.sh: unsupported OS 'MINGW64_NT' — on Windows download the .zip from the releases page, or use cargo install" ]
}

@test "a failed download names the archive and the version and installs nothing" {
    host Linux amd64
    double curl <<'EOF'
echo "curl: (22) The requested URL returned error: 404" >&2
exit 22
EOF

    ONEJUDGE_VERSION=v0.0.0 run "$ROOT/install.sh"

    [ "$status" -eq 1 ]
    [[ "$output" == *"install.sh: could not download onejudge-x86_64-unknown-linux-gnu.tar.gz (v0.0.0) — check that release 'v0.0.0' exists"* ]]
    [ ! -e "$INSTALL_DIR/onejudge" ]
}
