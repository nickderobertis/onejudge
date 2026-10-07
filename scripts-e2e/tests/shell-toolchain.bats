#!/usr/bin/env bats
# scripts/shell-toolchain.sh over the real pixi and bundler: after it runs, the
# environment holds exactly the versions pixi.toml pins and bashcov is the one
# the Gemfile pins, and a second run is a no-op that still succeeds.

load ../../tests/support/helpers

# pin NAME — pixi.toml's `NAME = "==VERSION"` dependency pin.
pin() {
    sed -n "s/^$1 = \"==\\(.*\\)\"\$/\\1/p" "$ROOT/pixi.toml"
}

@test "shell-toolchain installs the pinned tools and bashcov, and is idempotent" {
    run "$ROOT/scripts/shell-toolchain.sh"
    [ "$status" -eq 0 ]
    run "$ROOT/scripts/shell-toolchain.sh"
    [ "$status" -eq 0 ]

    cd "$ROOT"
    run pixi list --locked --json
    [ "$status" -eq 0 ]
    installed="$(node -e 'for (const p of JSON.parse(require("fs").readFileSync(0, "utf8"))) console.log(p.name + "=" + p.version)' <<<"$output")"
    for tool in shellcheck go-shfmt actionlint ruby; do
        [ -n "$(pin "$tool")" ]
        grep -qxF "$tool=$(pin "$tool")" <<<"$installed"
    done

    run pixi run --locked bundle exec bashcov --version
    [ "$status" -eq 0 ]
    [ "$output" = "bashcov $(sed -n 's/^gem "bashcov", "\(.*\)"$/\1/p' "$ROOT/Gemfile")" ]
}
