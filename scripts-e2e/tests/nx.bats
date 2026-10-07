#!/usr/bin/env bats
# The gate's entry points over the real Nx install: scripts/node-modules.sh's fast
# path, scripts/nx (Nx, quiet on success) and scripts/gate-plan.sh (which tier,
# base and projects `just check` runs). They run with this repository's own Nx
# over a scratch workspace shaped like this one — a contract (`lib`), its e2e
# suite (`lib-e2e`), an SDK (`sdk`) and an external tier (`live`) — whose history
# each test writes. onejudge-repo's gate_plan.rs holds the same selection rules;
# this suite is what measures the scripts' lines.

load ../../tests/support/helpers
bats_require_minimum_version 1.5.0

# workspace — the scratch Nx workspace in TREE, committed once; BASE is its SHA.
workspace() {
    TREE="$BATS_TEST_TMPDIR/tree"
    git_repo "$TREE"
    link "$TREE" scripts/gate-plan.sh scripts/nx scripts/node-modules.sh
    cp "$ROOT/package.json" "$ROOT/bun.lock" "$TREE/"
    ln -s "$ROOT/node_modules" "$TREE/node_modules"
    printf '/node_modules\n/.nx\n' >"$TREE/.gitignore"
    echo '{"namedInputs":{"default":["{projectRoot}/**/*"]}}' >"$TREE/nx.json"
    echo root >"$TREE/README.md"
    mkdir -p "$TREE/lib" "$TREE/lib-e2e" "$TREE/sdk" "$TREE/live"
    cat >"$TREE/lib/project.json" <<'EOF'
{"name":"lib","tags":["type:contract"],"targets":{
  "pass":{"command":"echo the pass target ran"},
  "fail":{"command":"echo the fail target ran && exit 3"}}}
EOF
    echo lib >"$TREE/lib/src.txt"
    echo '{"name":"lib-e2e","tags":["type:e2e"],"implicitDependencies":["lib"]}' >"$TREE/lib-e2e/project.json"
    echo '{"name":"sdk","tags":["type:sdk"],"implicitDependencies":["lib"]}' >"$TREE/sdk/project.json"
    echo sdk >"$TREE/sdk/src.txt"
    echo '{"name":"live","tags":["type:external"],"implicitDependencies":["lib"]}' >"$TREE/live/project.json"
    commit "$TREE" "chore: base"
    BASE="$(git -C "$TREE" rev-parse HEAD)"
    cd "$TREE" || return
    unset NX_BASE
    # Nx's own output uncoloured, as a terminal-less caller gets it, whatever the
    # runner of this suite (Nx itself sets FORCE_COLOR) asked for.
    export FORCE_COLOR=0
}

# change FILE — append to FILE and commit it.
change() {
    echo changed >>"$TREE/$1"
    commit "$TREE" "change $1"
}

# plan FLAG... — the plan a reader sees, from the scratch workspace's gate-plan.sh.
plan() {
    run --separate-stderr "$TREE/scripts/gate-plan.sh" --print-plan "$@"
}

@test "node-modules: an installed, current Nx is left alone, silently" {
    run "$ROOT/scripts/node-modules.sh"

    [ "$status" -eq 0 ]
    [ -z "$output" ]
}

@test "nx: a passing run prints one summary line naming its log" {
    workspace

    run "$TREE/scripts/nx" run lib:pass

    [ "$status" -eq 0 ]
    [[ "$output" =~ ^"nx run lib:pass: "(NX\ +)?"Successfully ran target pass for project lib (log: .nx/logs/nx."[A-Za-z0-9]+")"$ ]]
    [[ "$output" != *"the pass target ran"* ]]
}

@test "nx: a run Nx summarizes some other way is still ok" {
    workspace

    run "$TREE/scripts/nx" show projects

    [ "$status" -eq 0 ]
    [[ "$output" =~ ^"nx show projects: ok (log: .nx/logs/nx."[A-Za-z0-9]+")"$ ]]
}

@test "nx: a failing run prints the whole log, then the action" {
    workspace

    run --separate-stderr "$TREE/scripts/nx" run lib:fail

    [ "$status" -eq 1 ]
    [[ "$stderr" == *"the fail target ran"* ]]
    [[ "${stderr_lines[-1]}" == "nx run lib:fail: FAILED — fix the findings above and re-run the same 'just' recipe (log: .nx/logs/nx."* ]]
}

@test "nx: NX_SHOW_OUTPUT streams Nx's own output" {
    workspace

    NX_SHOW_OUTPUT=1 run --separate-stderr "$TREE/scripts/nx" show projects --json

    [ "$status" -eq 0 ]
    [ "$(node -e 'console.log(JSON.parse(process.argv[1]).sort().join(" "))' "$output")" = "lib lib-e2e live sdk" ]
}

# hollow_install — a tree whose install looks current to node-modules.sh but holds
# no Nx package.
hollow_install() {
    TREE="$BATS_TEST_TMPDIR/tree"
    link "$TREE" scripts/nx scripts/node-modules.sh
    cp "$ROOT/package.json" "$ROOT/bun.lock" "$TREE/"
    mkdir -p "$TREE/node_modules/.bin"
    : >"$TREE/node_modules/.bin/nx"
    cp "$ROOT/bun.lock" "$TREE/node_modules/.bun-lock-installed"
}

@test "nx: an install without Nx in it says how to heal it" {
    hollow_install

    run "$TREE/scripts/nx" show projects

    [ "$status" -eq 1 ]
    [[ "${lines[0]}" == "nx: cannot find the installed Nx: "* ]]
    [ "${lines[-1]}" = "ACTION: rm -rf node_modules && bash scripts/node-modules.sh, then re-run the recipe" ]
}

@test "gate-plan: a project change runs the affected tier from NX_BASE, without the external tier" {
    workspace
    change lib/src.txt

    NX_BASE="$BASE" plan

    [ "$status" -eq 0 ]
    [ "${lines[0]}" = "# tier: affected (base $BASE, from NX_BASE=$BASE)" ]
    [ "${lines[1]}" = "# projects: lib lib-e2e sdk" ]
    [ "${lines[2]}" = "# external tiers, static targets only: live" ]
    [ "${lines[3]}" = "GATE_PRINT_ONLY=1" ]
}

@test "gate-plan: without NX_BASE the base is the merge base with origin/main" {
    workspace
    git update-ref refs/remotes/origin/main "$BASE"
    change sdk/src.txt

    plan

    [ "$status" -eq 0 ]
    [ "${lines[0]}" = "# tier: affected (base $BASE, from the merge base with origin/main)" ]
    [ "${lines[1]}" = "# projects: sdk" ]
}

@test "gate-plan: a workspace-root file sweeps, unless escalation is off" {
    workspace
    change README.md

    NX_BASE="$BASE" plan
    [ "$status" -eq 0 ]
    [ "${lines[0]}" = "# tier: sweep (escalated from the affected tier against base $BASE, from NX_BASE=$BASE: README.md is a workspace-root file)" ]
    [ "${lines[1]}" = "# projects: lib lib-e2e sdk" ]
    [ "$stderr" = "gate: README.md belongs to the workspace root, which every project builds or is checked against, so the broader tier runs" ]

    NX_BASE="$BASE" plan --no-escalate
    [ "$status" -eq 0 ]
    [ "${lines[0]}" = "# tier: affected (base $BASE, from NX_BASE=$BASE)" ]
}

@test "gate-plan: an untracked root file sweeps too" {
    workspace
    echo new >"$TREE/NOTES.md"

    NX_BASE="$BASE" plan

    [ "$status" -eq 0 ]
    [[ "${lines[0]}" == *": NOTES.md is a workspace-root file)" ]]
}

@test "gate-plan: no merge base with origin/main sweeps, saying why" {
    workspace

    plan

    [ "$status" -eq 0 ]
    [ "${lines[0]}" = "# tier: sweep (escalated from the affected tier: no merge base with origin/main)" ]
    [ "$stderr" = "gate: no merge base with origin/main in this checkout (shallow, or origin/main not fetched), so the broader tier runs instead" ]
}

@test "gate-plan: the recipe's assignments carry the tier, targets and projects" {
    workspace
    change lib/src.txt

    NX_BASE="$BASE" run --separate-stderr "$TREE/scripts/gate-plan.sh" --targets=test,lint --projects=sdk,live

    [ "$status" -eq 0 ]
    [ "$stderr" = "gate: tier=affected (base $BASE, from NX_BASE=$BASE); projects: sdk" ]
    eval "$output"
    [ "$GATE_TIER" = affected ]
    [ "$GATE_BASE" = "$BASE" ]
    [ "$GATE_TARGETS" = "lint test" ]
    [ "$GATE_PROJECTS" = sdk ]
    [ "$GATE_EXCLUDE" = "lib,lib-e2e,live" ]
    [ "$GATE_EXTERNALS" = live ]
    [ "$GATE_STATIC" = lint ]
}

@test "gate-plan: the sweep selects every gate-eligible project and the static targets" {
    workspace

    run --separate-stderr "$TREE/scripts/gate-plan.sh" --sweep --targets test --projects 'tag:type:contract'

    [ "$status" -eq 0 ]
    eval "$output"
    [ "$GATE_TIER" = sweep ]
    [ "$GATE_BASE" = "" ]
    [ "$GATE_PROJECTS" = lib ]
    [ "$GATE_STATIC" = "" ]

    run --separate-stderr "$TREE/scripts/gate-plan.sh" --sweep
    eval "$output"
    [ "$GATE_PROJECTS" = "lib,lib-e2e,sdk" ]
    [ "$GATE_EXTERNALS" = live ]
    [ "$GATE_TARGETS" = "" ]
    [ "$GATE_STATIC" = "format-check lint" ]
}

@test "gate-plan: arguments it cannot plan from are refused before anything runs" {
    workspace

    plan --bogus
    [ "$status" -eq 2 ]
    [ "${stderr_lines[0]}" = "gate: unknown argument '--bogus'" ]
    [ "${stderr_lines[1]}" = "usage: scripts/gate-plan.sh [--sweep] [--no-escalate] [--targets a,b] [--projects SELECTOR] [--print-plan]" ]

    plan --targets
    [ "$status" -eq 2 ]
    [ "$stderr" = "gate: --targets needs a list, e.g. --targets test,coverage" ]

    plan --projects
    [ "$status" -eq 2 ]
    [ "$stderr" = "gate: --projects needs a selector, e.g. --projects 'tag:lang:rust'" ]

    plan --sweep --targets test,deploy
    [ "$status" -eq 2 ]
    [ "$stderr" = "gate: unknown target 'deploy' (the gate's targets: format-check lint typecheck generate-check doc build test coverage audit)" ]

    plan --sweep --projects nothing-matches
    [ "$status" -eq 2 ]
    [ "$stderr" = "gate: --projects 'nothing-matches' names no project (see \`just graph\` for the projects and their tags)" ]
}

@test "gate-plan: an NX_BASE that is not a plain ref or SHA, or names no commit, is refused" {
    workspace

    NX_BASE='HEAD~1' plan
    [ "$status" -eq 2 ]
    [ "$stderr" = "gate: NX_BASE='HEAD~1' is not a plain ref name or commit SHA; refusing to run (unset it to use the merge base with origin/main)" ]

    NX_BASE='--output=x' plan
    [ "$status" -eq 2 ]

    NX_BASE=no-such-branch plan
    [ "$status" -eq 2 ]
    [ "$stderr" = "gate: NX_BASE='no-such-branch' names no commit in this checkout; fetch it, or unset NX_BASE to use the merge base with origin/main" ]

    NX_BASE="${BASE:0:12}" plan
    [ "$status" -eq 0 ]
    [ "${lines[0]}" = "# tier: affected (base $BASE, from NX_BASE=${BASE:0:12})" ]
}

@test "gate-plan: a checkout git cannot list stops the plan, naming git and the next step" {
    workspace
    change lib/src.txt
    echo corrupt >"$TREE/.git/index"

    NX_BASE="$BASE" plan

    [ "$status" -eq 1 ]
    [[ "$stderr" == *"gate: \`git ls-files --cached --others --exclude-standard -- *project.json\` failed (above), so the changed files cannot be listed"* ]]
    [[ "$stderr" == *"ACTION: repair the checkout (\`git status\` names what is wrong), then re-run the recipe"* ]]
}

@test "gate-plan: a project graph Nx cannot read stops the plan, naming the fix" {
    workspace
    echo '{ not json' >"$TREE/sdk/project.json"

    plan --sweep

    [ "$status" -ne 0 ]
    [[ "$stderr" == *"gate: \`nx show projects\` did not answer with a list of project names"* ]]
    [[ "$stderr" == *"ACTION: run \`NX_SHOW_OUTPUT=1 ./scripts/nx show projects --json\` and fix the project.json it names"* ]]
}
