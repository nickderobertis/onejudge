#!/usr/bin/env bash
# Which tier the gate runs, against which base, over which projects — the
# selection behind `just check` (and `just test` / `test-fast` / `lint` /
# `format-check`, which narrow it). The recipe runs Nx; this decides what for.
#
#   scripts/gate-plan.sh [--sweep] [--targets a,b] [--projects p] [--print-plan]
#
# Prints shell assignments for the recipe to `eval` (every value validated or
# produced by Nx/git, never echoed from input): GATE_TIER, GATE_BASE, GATE_TARGETS,
# GATE_PROJECTS / GATE_EXCLUDE (the gate-eligible projects selected, and every
# other project), GATE_EXTERNALS and GATE_STATIC. With `--print-plan` it prints
# the plan for a reader instead, as `#` lines (what the CI routing and the
# selection journeys read), and the recipe stops there.
#
# AFFECTED tier (default): the projects the diff from the merge base can reach,
# as `nx show projects --affected` computes them. The base is explicit, never
# Nx's implicit default:
#   * NX_BASE, when set — a plain ref name or commit SHA, nothing else: anything
#     else fails closed, naming NX_BASE, because it reaches git as a revision;
#   * otherwise `git merge-base origin/main HEAD`.
# It escalates to the broader tier, saying why, when no merge base can be derived
# (a shallow or tag checkout without origin/main) or when the diff touches a file
# no project owns — a root file is owned by the `workspace` project, which every
# project builds or is checked against (the toolchain, the lockfiles, the
# justfile, the docs and schemas the contract suites read, the CI), so a change
# there is a change to every project. `--no-escalate` keeps the affected tier for
# a root-file change: a push to main, whose pull request already gated it.
#
# BROADER tier (`--sweep`): every gate-eligible project, plus the targets promoted
# out of the affected tier (`audit`, which contacts the advisory database).
#
# Both tiers leave the external tiers (the `type:external` projects:
# `onejudge-live`, `onejudge-llmlint-real`, `onejudge-release-targets`) out of
# everything but their static targets
# (GATE_STATIC: format-check, lint — their code is formatted and linted in the
# gate as it always was): they contact a real harness, llmlint, or the public
# registries, keep their `#[ignore]`, and run from their own recipes and workflows.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

STATIC_TARGETS="format-check lint"

usage() {
    echo "usage: scripts/gate-plan.sh [--sweep] [--no-escalate] [--targets a,b] [--projects SELECTOR] [--print-plan]" >&2
    exit 2
}

# The targets the gate knows, in the order it names them: a name outside them
# selects no task, which would read as a pass, so it is refused instead. With no
# `--targets`, the sweep runs all of them and the affected tier all but
# PROMOTED_TARGETS; the recipe reads that list from GATE_TARGETS and spells none.
KNOWN_TARGETS="format-check lint typecheck generate-check doc build test coverage audit"
PROMOTED_TARGETS="audit"

tier=affected
escalate=true
targets=""
projects=""
print_plan=false
while [ $# -gt 0 ]; do
    case "$1" in
        --sweep) tier=sweep ;;
        --no-escalate) escalate=false ;;
        --targets)
            [ $# -ge 2 ] || { echo "gate: --targets needs a list, e.g. --targets test,coverage" >&2; exit 2; }
            targets="$2"; shift ;;
        --targets=*) targets="${1#*=}" ;;
        --projects)
            [ $# -ge 2 ] || { echo "gate: --projects needs a selector, e.g. --projects 'tag:lang:rust'" >&2; exit 2; }
            projects="$2"; shift ;;
        --projects=*) projects="${1#*=}" ;;
        --print-plan) print_plan=true ;;
        *) echo "gate: unknown argument '$1'" >&2; usage ;;
    esac
    shift
done

# A plain ref name or a commit SHA: what `git check-ref-format` accepts as a
# one-level-or-more name, starting with a letter or digit (so never an option),
# with no revision syntax (`~`, `^`, `:`, `@{`, `..`) — or 7 to 64 hex digits.
plain_revision() {
    case "$1" in
        *[!A-Za-z0-9._/-]* | -* | .* | */ | *..* | *.lock | '') return 1 ;;
    esac
    git check-ref-format --allow-onelevel "$1" >/dev/null 2>&1 ||
        printf '%s' "$1" | grep -Eq '^[0-9a-f]{7,64}$'
}

for target in $(tr ',' ' ' <<<"$targets"); do
    case " $KNOWN_TARGETS " in
        *" $target "*) ;;
        *) echo "gate: unknown target '$target' (the gate's targets: $KNOWN_TARGETS)" >&2; exit 2 ;;
    esac
done

base=""
source=""
escalation=""
if [ "$tier" = affected ]; then
    if [ -n "${NX_BASE+set}" ]; then
        if ! plain_revision "$NX_BASE"; then
            echo "gate: NX_BASE='$NX_BASE' is not a plain ref name or commit SHA; refusing to run (unset it to use the merge base with origin/main)" >&2
            exit 2
        fi
        if ! base="$(git rev-parse --verify --quiet "$NX_BASE^{commit}")"; then
            echo "gate: NX_BASE='$NX_BASE' names no commit in this checkout; fetch it, or unset NX_BASE to use the merge base with origin/main" >&2
            exit 2
        fi
        source="NX_BASE=$NX_BASE"
    elif base="$(git merge-base origin/main HEAD 2>/dev/null)"; then
        source="the merge base with origin/main"
    else
        echo "gate: no merge base with origin/main in this checkout (shallow, or origin/main not fetched), so the broader tier runs instead" >&2
        tier=sweep
        escalation="no merge base with origin/main"
    fi
fi

# One git listing, or a failure naming it and the next step: an assignment from it
# stops the plan under `set -e`, rather than planning from a partial list.
listing() {
    local out
    if ! out="$(git "$@")"; then
        echo "gate: \`git $*\` failed (above), so the changed files cannot be listed" >&2
        echo "ACTION: repair the checkout (\`git status\` names what is wrong), then re-run the recipe" >&2
        exit 1
    fi
    printf '%s\n' "$out"
}

if [ "$tier" = affected ] && [ "$escalate" = true ]; then
    # Every project's root but the workspace's own (`.`), to find a changed file
    # — committed, staged, unstaged or untracked — that only the root project owns.
    manifests="$(listing ls-files --cached --others --exclude-standard -- '*project.json')"
    roots="$(sed -n 's|/project\.json$||p' <<<"$manifests")"
    committed="$(listing diff --name-only "$base")"
    untracked="$(listing ls-files --others --exclude-standard)"
    while IFS= read -r path; do
        [ -n "$path" ] || continue
        owned=false
        while IFS= read -r project_root; do
            case "$path" in "$project_root"/*) owned=true; break ;; esac
        done <<<"$roots"
        if [ "$owned" = false ]; then
            echo "gate: $path belongs to the workspace root, which every project builds or is checked against, so the broader tier runs" >&2
            tier=sweep
            escalation="$path is a workspace-root file"
            break
        fi
    done < <(printf '%s\n%s\n' "$committed" "$untracked" | sort -u)
fi

# The projects Nx lists, one per line: its JSON is parsed and held to being an
# array of project names, so a malformed answer fails here instead of planning.
listed() {
    NX_SHOW_OUTPUT=1 ./scripts/nx show projects "$@" | node -e '
let names;
try {
  names = JSON.parse(require("fs").readFileSync(0, "utf8"));
} catch (error) {
  names = error;
}
if (!Array.isArray(names) || !names.every((n) => typeof n === "string" && /^[A-Za-z0-9._@/-]+$/.test(n))) {
  const why = names instanceof Error ? `: ${names.message}` : "";
  console.error(`gate: \`nx show projects\` did not answer with a list of project names${why}`);
  console.error("ACTION: run `NX_SHOW_OUTPUT=1 ./scripts/nx show projects --json` and fix the project.json it names");
  process.exit(1);
}
process.stdout.write(names.map((n) => n + "\n").join(""));
' | sort
}
lines() { tr ', ' '\n\n' | sed '/^$/d' | sort -u; }
joined() { paste -sd"$1" -; }

everything="$(listed --json)"
if [ "$tier" = affected ]; then
    selected="$(listed --affected --base="$base" --json)"
else
    selected="$everything"
fi
if [ -n "$projects" ]; then
    # Captured first, so a selector Nx cannot resolve fails here rather than inside
    # a process substitution, where its status would be lost to an empty plan.
    if ! matching="$(listed --projects="$projects" --json)" || [ -z "$matching" ]; then
        echo "gate: --projects '$projects' names no project (see \`just graph\` for the projects and their tags)" >&2
        exit 2
    fi
    selected="$(comm -12 <(printf '%s\n' "$selected") <(printf '%s\n' "$matching"))"
fi
# The external tiers are the projects tagged `type:external`: the tag is the one
# declaration, so a new external tier is out of the gate the moment it is tagged.
external="$(listed --projects='tag:type:external' --json)"
eligible="$(comm -23 <(printf '%s\n' "$selected" | sed '/^$/d') <(printf '%s\n' "$external"))"
externals="$(comm -12 <(printf '%s\n' "$selected" | sed '/^$/d') <(printf '%s\n' "$external"))"
excluded="$(comm -23 <(printf '%s\n' "$everything") <(printf '%s\n' "$eligible" | sed '/^$/d'))"
if [ -n "$targets" ]; then
    target_list="$(lines <<<"$targets" | joined ' ')"
elif [ "$tier" = sweep ]; then
    target_list="$KNOWN_TARGETS"
else
    target_list="$(for target in $KNOWN_TARGETS; do
        case " $PROMOTED_TARGETS " in *" $target "*) ;; *) printf '%s\n' "$target" ;; esac
    done | joined ' ')"
fi
static=""
if [ -n "$targets" ]; then
    static="$(comm -12 <(lines <<<"$targets") <(lines <<<"$STATIC_TARGETS") | joined ' ')"
else
    static="$STATIC_TARGETS"
fi

if [ "$print_plan" = true ]; then
    if [ "$tier" = affected ]; then
        echo "# tier: affected (base $base, from $source)"
    elif [ -n "$escalation" ]; then
        echo "# tier: sweep (escalated from the affected tier${base:+ against base $base, from $source}: $escalation)"
    else
        echo "# tier: sweep"
    fi
    echo "# projects: $(joined ' ' <<<"$eligible")"
    echo "# external tiers, static targets only: $(joined ' ' <<<"$externals")"
    echo "GATE_PRINT_ONLY=1"
    exit 0
fi
echo "gate: tier=$tier${base:+ (base $base, from $source)}${escalation:+ — escalated: $escalation}; projects: $(joined ' ' <<<"$eligible")" >&2
# Every value shell-quoted, because the recipe evaluates these lines: a project
# name or target that is not a plain word stays one word of data.
printf 'GATE_TIER=%q\nGATE_BASE=%q\nGATE_TARGETS=%q\nGATE_PROJECTS=%q\nGATE_EXCLUDE=%q\nGATE_EXTERNALS=%q\nGATE_STATIC=%q\n' \
    "$tier" "$base" "$target_list" "$(joined , <<<"$eligible")" "$(joined , <<<"$excluded")" \
    "$(joined , <<<"$externals")" "$static"
