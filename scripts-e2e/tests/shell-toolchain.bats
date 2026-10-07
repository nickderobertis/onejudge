#!/usr/bin/env bats
# The shell toolchain `just bootstrap` installed (scripts/shell-toolchain.sh),
# read back through the real pixi and bundler: the environment holds exactly the
# versions pixi.toml pins and bashcov is the one the Gemfile pins. It reads the
# installed environment and contacts no package service; the installer's own
# journeys are the workspace suite's, over doubles.

load ../../tests/support/helpers

# pin NAME — pixi.toml's `NAME = "==VERSION"` dependency pin.
pin() {
    sed -n "s/^$1 = \"==\\(.*\\)\"\$/\\1/p" "$ROOT/pixi.toml"
}

@test "the installed shell toolchain is the pinned one" {
    cd "$ROOT" || exit
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
