#!/usr/bin/env bats
# scripts/release-probe.sh, the probe release-targets.toml names: exactly three
# answers — a version, empty (no release yet), or not answered (non-zero, reason
# on stderr, nothing on stdout). The registry is a `curl` double serving a body
# and a status; python3 and mktemp are the real ones. The live registries are
# onejudge-release-targets' tier.

load support/helpers
bats_require_minimum_version 1.5.0

setup() {
    isolate_path
    PROBE="$ROOT/scripts/release-probe.sh"
    BODY="$BATS_TEST_TMPDIR/body.json"
    export BODY
}

# registry STATUS [BODY] — curl writes BODY to its `--output` and prints STATUS.
registry() {
    printf '%s' "${2:-}" >"$BODY"
    export STATUS="$1"
    double curl <<'EOF'
while [ $# -gt 0 ]; do
    case "$1" in --output) cp "$BODY" "$2"; shift ;; esac
    shift
done
printf '%s' "$STATUS"
EOF
}

# probe ID — run the probe, keeping stdout and stderr apart.
probe() {
    run --separate-stderr "$PROBE" "$@"
}

@test "a crate answers its newest stable version" {
    registry 200 '{"crate":{"max_stable_version":"0.17.2","newest_version":"0.18.0-rc.1"}}'

    probe crate:onejudge

    [ "$status" -eq 0 ]
    [ "$output" = 0.17.2 ]
    [ -z "$stderr" ]
}

@test "a crate with only prereleases answers its newest version, never empty" {
    registry 200 '{"crate":{"max_stable_version":null,"newest_version":"0.1.0-alpha.1"}}'

    probe crate:onejudge

    [ "$status" -eq 0 ]
    [ "$output" = 0.1.0-alpha.1 ]
}

@test "a PyPI project answers its version" {
    registry 200 '{"info":{"version":"0.17.2"}}'

    probe pypi:onejudge-cli

    [ "$status" -eq 0 ]
    [ "$output" = 0.17.2 ]
}

@test "a 404 is the one 'no release yet': exit 0 and empty stdout" {
    registry 404 '{"errors":[{"detail":"Not Found"}]}'

    probe crate:never-published

    [ "$status" -eq 0 ]
    [ -z "$output" ]
}

@test "any other status is not answered" {
    registry 503 'busy'

    probe pypi:onejudge

    [ "$status" -eq 1 ]
    [ -z "$output" ]
    [ "$stderr" = "release-probe: https://pypi.org/pypi/onejudge/json answered HTTP 503 for 'pypi:onejudge'" ]
}

@test "an unreachable registry is not answered" {
    double curl <<'EOF'
echo "curl: (6) Could not resolve host: crates.io" >&2
exit 6
EOF

    probe crate:onejudge

    [ "$status" -eq 1 ]
    [ -z "$output" ]
    [[ "$stderr" == *"release-probe: could not reach https://crates.io/api/v1/crates/onejudge for 'crate:onejudge'"* ]]
}

@test "a 200 whose body has no readable version is not answered" {
    registry 200 '{"crate":{}}'

    probe crate:onejudge

    [ "$status" -eq 1 ]
    [ -z "$output" ]
    [[ "$stderr" == *"answered HTTP 200 for 'crate:onejudge' with no version this probe could read" ]]
}

@test "a 200 with an empty version is not answered" {
    registry 200 '{"info":{"version":""}}'

    probe pypi:onejudge

    [ "$status" -eq 1 ]
    [ -z "$output" ]
    [[ "$stderr" == *"answered HTTP 200 for 'pypi:onejudge' with an empty version" ]]
}

@test "identifiers the probe does not recognise are not answered, never empty" {
    registry 200 '{"info":{"version":"1.0.0"}}'

    probe
    [ "$status" -eq 1 ]
    [ "$stderr" = "release-probe: usage: release-probe.sh <registry>:<name> (exactly one argument, got 0)" ]

    probe onejudge
    [ "$status" -eq 1 ]
    [ "$stderr" = "release-probe: unrecognised identifier 'onejudge': expected a registry-qualified <registry>:<name>" ]

    probe 'crate:../etc'
    [ "$status" -eq 1 ]
    [ "$stderr" = "release-probe: unrecognised identifier 'crate:../etc': '../etc' is not a registry artifact name" ]

    probe npm:onejudge
    [ "$status" -eq 1 ]
    [ -z "$output" ]
    [ "$stderr" = "release-probe: unrecognised identifier 'npm:onejudge': this repository publishes to crate: and pypi: only" ]
}

@test "a missing tool is not answered, naming the tool" {
    local tools="$BATS_TEST_TMPDIR/no-curl"
    mkdir -p "$tools"
    ln -s "$(command -v mktemp)" "$(command -v python3)" "$tools/"

    run --separate-stderr env PATH="$tools" "$BASH" "$PROBE" crate:onejudge

    [ "$status" -eq 1 ]
    [ "$stderr" = "release-probe: curl is not on PATH, so 'crate:onejudge' cannot be looked up" ]
}
