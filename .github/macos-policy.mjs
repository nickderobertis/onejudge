#!/usr/bin/env node
// The only release-only decision: identity AND semantic content, never a title.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const git = (...args) =>
  execFileSync("git", args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
function versionOnly(path, before, after) {
  if (path === "CHANGELOG.md" || /^crates\/onejudge\/CHANGELOG\.md$/.test(path))
    return true;
  if (
    /^(Cargo\.toml|Cargo\.lock|crates\/onejudge\/Cargo\.toml|python\/onejudge-sdk\/pyproject\.toml)$/.test(
      path,
    )
  ) {
    // Parse TOML rather than deleting lines: dependency, feature and package changes survive.
    const normalize = execFileSync(
      "python3",
      [
        "-c",
        `
import json, re, sys, tomllib
path = sys.argv[1]
def drop_version(d):
    v = d.get('version')
    if isinstance(v, str):
        if not re.fullmatch(r'\\d+\\.\\d+\\.\\d+(?:[-+][A-Za-z0-9.-]+)?', v):
            raise ValueError('not a plain version')
        d.pop('version')
    elif v is not None and v != {'workspace': True}:
        raise ValueError('not a plain version')
def normalize(text):
    d = tomllib.loads(text)
    if path == 'Cargo.lock':
        for p in d.get('package', []):
            if p.get('name') == 'onejudge': drop_version(p)
    elif path == 'Cargo.toml': drop_version(d.get('workspace', {}).get('package', {}))
    elif path.endswith('Cargo.toml'): drop_version(d.get('package', {}))
    else: drop_version(d.get('project', {}))
    return d
v = json.load(sys.stdin)
print(json.dumps(normalize(v[0]) == normalize(v[1])))
`,
        path,
      ],
      { input: JSON.stringify([before, after]), encoding: "utf8" },
    ).trim();
    return normalize === "true";
  }
  if (path === "python/onejudge-sdk/src/onejudge_sdk/_version.py") {
    const strip = (s) =>
      s.replace(
        /^__version__ = "\d+\.\d+\.\d+(?:[-+][^"\n]+)?"$/m,
        "__version__ = VERSION",
      );
    return strip(before) === strip(after) && strip(before) !== before;
  }
  return false;
}
export function classify(
  event,
  base,
  head,
  eventName = process.env.GITHUB_EVENT_NAME,
) {
  const isPr = eventName ? eventName === "pull_request" : !!event?.pull_request;
  const ordinary = (reason) => ({
    isReleaseOnly: false,
    macosRequired: isPr,
    reason,
  });
  if (!isPr) return ordinary("non-PR event");
  try {
    const pr = event.pull_request;
    const repository = event.repository?.full_name;
    if (
      repository !== "nickderobertis/onejudge" ||
      pr.head.repo.full_name !== repository ||
      pr.base.repo.full_name !== repository ||
      !/^release-plz-/.test(pr.head.ref) ||
      pr.user.login !== "nickderobertis"
    ) {
      return ordinary(
        "ordinary PR: release automation identity/branch not established",
      );
    }
    const from = git("rev-parse", "--verify", `${base}^{commit}`).trim();
    const to = git("rev-parse", "--verify", `${head}^{commit}`).trim();
    const rows = git("diff", "--name-status", "--no-renames", from, to)
      .trim()
      .split("\n");
    if (!rows[0]) return ordinary("empty diff is not a release");
    for (const row of rows) {
      const [status, path] = row.split("\t");
      if (
        status !== "M" ||
        !versionOnly(
          path,
          git("show", `${from}:${path}`),
          git("show", `${to}:${path}`),
        )
      ) {
        return ordinary(`non-release content: ${path}`);
      }
    }
    return {
      isReleaseOnly: true,
      macosRequired: false,
      reason: "authenticated version/changelog-only release PR",
    };
  } catch (error) {
    return ordinary(`unknown PR classification: ${error.message}`);
  }
}
if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  if (args.shift() !== "classify")
    throw new Error("expected classify --event PATH --base REF --head REF");
  const options = Object.fromEntries(
    Array.from({ length: args.length / 2 }, (_, i) => [
      args[i * 2],
      args[i * 2 + 1],
    ]),
  );
  let event;
  try {
    event = JSON.parse(readFileSync(options["--event"], "utf8"));
  } catch {
    event = null;
  }
  console.log(
    JSON.stringify(
      classify(
        event,
        options["--base"],
        options["--head"],
        process.env.GITHUB_EVENT_NAME ||
          (event?.pull_request ? "pull_request" : "pull_request"),
      ),
    ),
  );
}
