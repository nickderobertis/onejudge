#!/usr/bin/env bats
# scripts/cargo-target-dir.sh over the real cargo: the target directory it prints
# is the one `cargo metadata` resolves for this checkout.

load ../../tests/support/helpers

@test "cargo-target-dir prints the target directory cargo resolves" {
    run "$ROOT/scripts/cargo-target-dir.sh"

    [ "$status" -eq 0 ]
    [ "$output" = "$(cd "$ROOT" && cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')" ]
}
