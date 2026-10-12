#!/usr/bin/env bash
# SPIKE ONLY: (1) the dependency closures of serde-saphyr and saphyr, checked for
# the YAML crates the plan bans; (2) cargo-deny with the repository's deny.toml
# over the post-move approximation in deny-probe/, against the same run over
# the repository today, so a new duplicate version or refusal is attributable.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$here/../.."
probe="$here/deny-probe"
cp "$root/Cargo.lock" "$probe/Cargo.lock"
banned='^(serde_yaml|serde_yaml_ng|serde_norway|serde_yml|unsafe-libyaml|unsafe-libyaml-norway|libyml)$'
for crate in serde-saphyr saphyr; do
  closure="$(cargo tree -q --manifest-path "$probe/Cargo.toml" -p "$crate" -e normal,build --prefix none | sed 's/ (\*)//; s/ (proc-macro)//' | sort -u)"
  echo "== $crate dependency closure ($(wc -l <<<"$closure") crates):"
  sed 's/^/   /' <<<"$closure"
  hits="$(awk '{print $1}' <<<"$closure" | grep -E "$banned" || true)"
  echo "   banned YAML crates in closure: ${hits:-none}"
done
echo "== banned YAML crates anywhere in the post-move approximation:"
all="$(cargo tree -q --manifest-path "$probe/Cargo.toml" -e normal,build --prefix none | awk '{print $1}' | sort -u)"
grep -E "$banned" <<<"$all" || echo "   none"
run_deny() { cargo deny --manifest-path "$1" check --config "$root/deny.toml" 2>&1; }
echo "== cargo deny (repository deny.toml) over the post-move approximation:"
run_deny "$probe/Cargo.toml" | grep -E "^(error|warning)\[|^ *(advisories|bans|licenses|sources) (ok|FAILED)" | sort | uniq -c
echo "== cargo deny (repository deny.toml) over the repository today:"
run_deny "$root/Cargo.toml" | grep -E "^(error|warning)\[|^ *(advisories|bans|licenses|sources) (ok|FAILED)" | sort | uniq -c
echo "== duplicate-version warnings in the approximation that today's repository does not have:"
dups() { run_deny "$1" | grep -o "duplicate entries for crate '[^']*'" | sort -u; }
comm -13 <(dups "$root/Cargo.toml") <(dups "$probe/Cargo.toml") | sed 's/^/   /'
echo "== licenses of serde-saphyr / saphyr closures:"
cargo metadata -q --manifest-path "$probe/Cargo.toml" --format-version 1 | python3 -c '
import json, sys
m = json.load(sys.stdin)
pk = {p["id"]: p for p in m["packages"]}
nodes = {n["id"]: n for n in m["resolve"]["nodes"]}
for root in ("serde-saphyr", "saphyr"):
    start = [i for i, p in pk.items() if p["name"] == root][0]
    seen, todo = set(), [start]
    while todo:
        i = todo.pop()
        if i in seen: continue
        seen.add(i)
        todo += [d["pkg"] for d in nodes[i]["deps"] if any(k["kind"] in (None, "build") for k in d["dep_kinds"])]
    lic = sorted({pk[i]["license"] or "NONE" for i in seen})
    print(f"   {root}: {len(seen)} crates; licence expressions: {lic}")
'
rm -f "$probe/Cargo.lock"
