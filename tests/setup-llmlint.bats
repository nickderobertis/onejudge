#!/usr/bin/env bats
# scripts/setup-llmlint.sh, the SessionStart hook's installer: it installs
# llmlint-cli (with oneharness) through `uv tool`, persists PATH into a Claude Code
# session's env file, and always exits 0. HOME is a scratch directory and `uv` and
# `llmlint` are doubles, so nothing is installed for the user running the suite.

load support/helpers

setup() {
    only_tools cat chmod
    export HOME="$BATS_TEST_TMPDIR/home"
    BIN_DIR="$HOME/.local/bin"
    mkdir -p "$BIN_DIR"
    unset CLAUDE_ENV_FILE
    UV_LOG="$BATS_TEST_TMPDIR/uv-args"
    export UV_LOG
}

# session — a Claude Code session's env file, which later Bash calls source.
session() {
    export CLAUDE_ENV_FILE="$BATS_TEST_TMPDIR/session.env"
}

# uv_installs — a uv whose `tool install` links an llmlint double into BIN_DIR.
uv_installs() {
    double uv <<'EOF'
printf '%s\n' "$*" >"$UV_LOG"
cat >"$HOME/.local/bin/llmlint" <<'LLMLINT'
#!/usr/bin/env bash
case "$1" in
    --version) echo "llmlint 0.4.3" ;;
    doctor) echo "doctor: ${DOCTOR:-ok}"; [ "${DOCTOR:-ok}" = ok ] ;;
esac
LLMLINT
chmod +x "$HOME/.local/bin/llmlint"
EOF
}

@test "installs llmlint-cli at the floor with oneharness, then reports it ready" {
    uv_installs

    run env PATH="$ONLY_PATH" "$ROOT/scripts/setup-llmlint.sh"

    [ "$status" -eq 0 ]
    [ "$(cat "$UV_LOG")" = "tool install --upgrade --with-executables-from oneharness-cli llmlint-cli>=0.4.3" ]
    [[ "$output" == *"setup-llmlint: no CLAUDE_ENV_FILE (not a session); skipping env"* ]]
    [[ "$output" == *"setup-llmlint: ready (llmlint: llmlint 0.4.3)"* ]]
    [[ "$output" == *"doctor: ok"* ]]
}

@test "in a session it persists a PATH that leads with the tool bin directory" {
    uv_installs
    session

    run env PATH="$ONLY_PATH" "$ROOT/scripts/setup-llmlint.sh"

    [ "$status" -eq 0 ]
    [[ "$output" == *"setup-llmlint: exported PATH"* ]]
    # shellcheck disable=SC1090 # the file the script wrote, read back as a session would.
    (source "$CLAUDE_ENV_FILE" && [ "${PATH%%:*}" = "$BIN_DIR" ])
}

@test "a session whose PATH already has the tool bin directory gets no PATH line" {
    uv_installs
    session

    run env PATH="$BIN_DIR:$ONLY_PATH" "$ROOT/scripts/setup-llmlint.sh"

    [ "$status" -eq 0 ]
    [ ! -s "$CLAUDE_ENV_FILE" ]
}

@test "a failed install and a failing doctor are logged, and startup still succeeds" {
    uv_installs
    "$DOUBLES/uv" tool install # leaves an llmlint, as an earlier session's install would
    double uv <<'EOF'
echo "error: no network" >&2
exit 2
EOF

    DOCTOR=broken run env PATH="$ONLY_PATH" "$ROOT/scripts/setup-llmlint.sh"

    [ "$status" -eq 0 ]
    [[ "$output" == *"setup-llmlint: llmlint-cli install failed (continuing)"* ]]
    [[ "$output" == *"doctor: broken"* ]]
    [[ "$output" == *"setup-llmlint: llmlint doctor reported an issue (see above)"* ]]
}

@test "without uv it says how to get it and still succeeds" {
    run env PATH="$ONLY_PATH" "$ROOT/scripts/setup-llmlint.sh"

    [ "$status" -eq 0 ]
    [[ "$output" == *"setup-llmlint: uv not found; cannot install llmlint (install uv: https://docs.astral.sh/uv/)"* ]]
    [[ "$output" == *"setup-llmlint: llmlint not installed"* ]]
}
