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
function versionOnly(path, before, after, base, head) {
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
import json, pathlib, re, subprocess, sys, tomllib
path, base, head = sys.argv[1:]
def workspace(ref):
    def manifest(path):
        return tomllib.loads(subprocess.check_output(['git', 'show', f'{ref}:{path}'], text=True))
    root = manifest('Cargo.toml')['workspace']
    version = root['package']['version']
    if not isinstance(version, str) or not re.fullmatch(r'\\d+\\.\\d+\\.\\d+(?:[-+][A-Za-z0-9.-]+)?', version):
        raise ValueError('invalid workspace version')
    members = root['members']
    if not isinstance(members, list) or not members:
        raise ValueError('workspace members not established')
    names = {}
    for member in members:
        if not isinstance(member, str) or pathlib.PurePosixPath(member).is_absolute() or any(part in ('..', '*', '?') for part in member.split('/')) or any(c in member for c in '*?[]'):
            raise ValueError('unsupported workspace member path')
        package = manifest(f'{member}/Cargo.toml')['package']
        name = package['name']
        if not isinstance(name, str) or not name:
            raise ValueError('invalid workspace package name')
        if package.get('version') == {'workspace': True}:
            if name in names:
                raise ValueError('duplicate workspace package name')
            names[name] = member
    return version, names
proven = {}
if path == 'Cargo.lock':
    base_version, base_names = workspace(base)
    head_version, head_names = workspace(head)
    proven = {name: member for name, member in base_names.items() if head_names.get(name) == member}
def drop_version(d):
    v = d.get('version')
    if isinstance(v, str):
        if not re.fullmatch(r'\\d+\\.\\d+\\.\\d+(?:[-+][A-Za-z0-9.-]+)?', v):
            raise ValueError('not a plain version')
        d.pop('version')
    elif v is not None and v != {'workspace': True}:
        raise ValueError('not a plain version')
def normalize(text, expected_version):
    d = tomllib.loads(text)
    if path == 'Cargo.lock':
        for p in d.get('package', []):
            if p.get('name') in proven and 'source' not in p and 'checksum' not in p:
                if p.get('version') != expected_version:
                    raise ValueError('local lock version differs from workspace version')
                drop_version(p)
    elif path == 'Cargo.toml': drop_version(d.get('workspace', {}).get('package', {}))
    elif path.endswith('Cargo.toml'): drop_version(d.get('package', {}))
    else: drop_version(d.get('project', {}))
    return d
v = json.load(sys.stdin)
print(json.dumps(normalize(v[0], base_version if proven else None) == normalize(v[1], head_version if proven else None)))
`,
        path,
        base,
        head,
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
          from,
          to,
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
  const inferred =
    event?.pull_request !== undefined
      ? "pull_request"
      : event?.release
        ? "release"
        : event?.workflow_run
          ? "workflow_run"
          : event?.inputs
            ? "workflow_dispatch"
            : event?.schedule
              ? "schedule"
              : event?.client_payload
                ? "repository_dispatch"
                : event?.ref && event?.before
                  ? "push"
                  : "pull_request";
  console.log(
    JSON.stringify(
      classify(
        event,
        options["--base"],
        options["--head"],
        process.env.GITHUB_EVENT_NAME || inferred,
      ),
    ),
  );
}
