import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import {
  workflows,
  resolveRun,
  triggered,
  sourceDigest,
  expand,
} from "../workflow-graph.mjs";

const root = resolve(".");
const scratchRoot =
  process.env.ONEPIPELINE_NODE_SCRATCH_DIR || process.env.RUNNER_TEMP;
if (!scratchRoot)
  throw new Error("ONEPIPELINE_NODE_SCRATCH_DIR or RUNNER_TEMP is required");
function history() {
  const dir = mkdtempSync(join(scratchRoot, "macos-policy-"));
  const git = (...args) =>
    execFileSync("git", args, { cwd: dir, encoding: "utf8" }).trim();
  git("init", "-q");
  git("config", "user.email", "test@example.com");
  git("config", "user.name", "Policy fixture");
  writeFileSync(
    join(dir, "Cargo.toml"),
    '[workspace.package]\nversion = "0.19.0"\n[workspace.dependencies]\nserde = "1.0"\n',
  );
  writeFileSync(join(dir, "CHANGELOG.md"), "# Changes\n");
  writeFileSync(
    join(dir, "Cargo.lock"),
    'version = 4\n[[package]]\nname = "onejudge"\nversion = "0.19.0"\n[[package]]\nname = "serde"\nversion = "1.0.0"\n',
  );
  mkdirSync(join(dir, "python/onejudge-sdk/src/onejudge_sdk"), {
    recursive: true,
  });
  writeFileSync(
    join(dir, "python/onejudge-sdk/src/onejudge_sdk/_version.py"),
    '__version__ = "0.19.0"\n',
  );
  git("add", ".");
  git("commit", "-qm", "ci: fixture");
  const base = git("rev-parse", "HEAD");
  const heads = {};
  for (const kind of ["version", "source", "dependency"]) {
    git("checkout", "-q", "-B", kind, base);
    if (kind === "version")
      writeFileSync(
        join(dir, "Cargo.toml"),
        '[workspace.package]\nversion = "0.20.0"\n[workspace.dependencies]\nserde = "1.0"\n',
      );
    else if (kind === "dependency")
      writeFileSync(
        join(dir, "Cargo.toml"),
        '[workspace.package]\nversion = "0.20.0"\n[workspace.dependencies]\nserde = "2.0"\n',
      );
    else writeFileSync(join(dir, "source.rs"), "fn new_behavior() {}\n");
    if (kind === "version") {
      writeFileSync(
        join(dir, "Cargo.lock"),
        'version = 4\n[[package]]\nname = "onejudge"\nversion = "0.20.0"\n[[package]]\nname = "serde"\nversion = "1.0.0"\n',
      );
      writeFileSync(
        join(dir, "python/onejudge-sdk/src/onejudge_sdk/_version.py"),
        '__version__ = "0.20.0"\n',
      );
    }
    git("add", ".");
    git("commit", "-qm", "ci: change");
    heads[kind] = git("rev-parse", "HEAD");
  }
  git("tag", "v0.19.0", base);
  git("tag", "onejudge-v0.19.0", base);
  return { dir, base, heads };
}
export function pr(fork = false, release = false) {
  const repository = { full_name: "nickderobertis/onejudge" };
  return {
    action: "synchronize",
    repository,
    pull_request: {
      title: "chore: release",
      user: { login: release ? "nickderobertis" : "contributor" },
      head: {
        ref: release ? "release-plz-2026" : "feature",
        repo: fork ? { full_name: "fork/onejudge" } : repository,
      },
      base: { repo: repository },
    },
  };
}
function classify(h, event, payload, head) {
  const path = join(h.dir, "event.json");
  writeFileSync(path, JSON.stringify(payload));
  return JSON.parse(
    execFileSync(
      "node",
      [
        join(root, ".github/macos-policy.mjs"),
        "classify",
        "--event",
        path,
        "--base",
        h.base,
        "--head",
        head,
      ],
      {
        cwd: h.dir,
        env: { ...process.env, GITHUB_EVENT_NAME: event },
        encoding: "utf8",
      },
    ),
  );
}
test("real classifier authenticates identity AND parses content, failing closed", () => {
  const h = history();
  try {
    assert.equal(
      classify(h, "pull_request", pr(false, true), h.heads.version)
        .isReleaseOnly,
      true,
    );
    for (const event of [
      pr(true, true),
      pr(false, false),
      {
        ...pr(false, true),
        pull_request: {
          ...pr(false, true).pull_request,
          user: { login: "attacker" },
        },
      },
    ]) {
      assert.equal(
        classify(h, "pull_request", event, h.heads.version).macosRequired,
        true,
      );
    }
    const botOnly = pr(false, true);
    botOnly.pull_request.head.ref = "feature";
    assert.equal(
      classify(h, "pull_request", botOnly, h.heads.version).macosRequired,
      true,
    );
    for (const kind of ["source", "dependency"])
      assert.equal(
        classify(h, "pull_request", pr(false, true), h.heads[kind])
          .macosRequired,
        true,
      );
    assert.equal(
      classify(h, "pull_request", null, h.heads.version).macosRequired,
      true,
    );
    assert.equal(
      classify(h, "pull_request", pr(false, true), "missing-ref").macosRequired,
      true,
    );
    assert.equal(classify(h, "push", null, "missing-ref").macosRequired, false);
    writeFileSync(join(h.dir, "event.json"), "{");
    const malformed = execFileSync(
      "node",
      [
        join(root, ".github/macos-policy.mjs"),
        "classify",
        "--event",
        join(h.dir, "event.json"),
        "--base",
        h.base,
        "--head",
        h.heads.version,
      ],
      {
        env: { ...process.env, GITHUB_EVENT_NAME: "pull_request" },
        encoding: "utf8",
      },
    );
    assert.equal(JSON.parse(malformed).macosRequired, true);
  } finally {
    rmSync(h.dir, { recursive: true, force: true });
  }
});
test("actual workflow graph covers event fan-out, ownership, diffs and publish selection; records telemetry", () => {
  const h = history(),
    docs = workflows(),
    scenarios = [],
    policies = new Map();
  const events = [
    ["pull_request", "opened"],
    ["pull_request", "synchronize"],
    ["pull_request", "reopened"],
    ["push", "base"],
    ["push", "release-merge"],
    ["push", "tag"],
    ["release", "published"],
    ["workflow_run", "completed"],
    ["repository_dispatch", "smoke"],
    ["schedule", "scheduled"],
    ["workflow_dispatch", "default"],
    ["workflow_dispatch", "diagnostic"],
  ];
  try {
    for (const [event, action] of events)
      for (const diff of ["source", "version", "dependency"])
        for (const fork of [false, true])
          for (const cli of [false, true])
            for (const sdk of [false, true]) {
              const payload = pr(fork, diff === "version");
              payload.action = action;
              payload.release = { tag_name: "v0.19.0" };
              payload.workflow_run = {
                name: "release-binaries",
                head_sha: "a".repeat(40),
              };
              const inputs = {
                tag: "v0.19.0",
                diagnostic_rebuild: action === "diagnostic",
              };
              const key = `${event}/${diff}/${fork}`;
              if (!policies.has(key))
                policies.set(key, classify(h, event, payload, h.heads[diff]));
              const paths = diff === "source" ? ["source.rs"] : ["Cargo.toml"];
              const producerKey = `${event}/${action}`;
              if (!policies.has(producerKey)) {
                const output = execFileSync(
                  "python3",
                  [join(root, "pypi/producer_plan.py")],
                  {
                    cwd: h.dir,
                    env: {
                      ...process.env,
                      TAG: "v0.19.0",
                      GITHUB_EVENT_NAME: event,
                    },
                    encoding: "utf8",
                  },
                );
                policies.set(
                  producerKey,
                  Object.fromEntries(
                    output
                      .trim()
                      .split("\n")
                      .map((line) => {
                        const at = line.indexOf("=");
                        return [line.slice(0, at), line.slice(at + 1)];
                      }),
                  ),
                );
              }
              const scenario = {
                paths,
                outputs: { "darwin-plan": policies.get(producerKey) },
                id: `${event}/${action}/${diff}/${fork}/${cli}/${sdk}`,
                event,
                payload,
                inputs,
                cli,
                sdk,
                publish: true,
                ref: action === "tag" ? "refs/tags/v0.19.0" : "refs/heads/main",
                policy: policies.get(key),
                diagnostic: action === "diagnostic",
              };
              scenario.rows = Object.entries(docs)
                .filter(([, doc]) => triggered(doc, scenario))
                .flatMap(([file]) =>
                  resolveRun(file, scenario, docs).map((row) => ({
                    ...row,
                    runWorkflow: file,
                  })),
                );
              if (event === "pull_request") {
                const names = scenario.rows
                  .filter((r) => r.workflow === "ci.yml" && r.enabled)
                  .map((r) => r.name);
                for (const name of [
                  "check",
                  "commitlint",
                  "llmlint",
                  "msrv",
                  "package",
                  "test-os (macos-latest)",
                  "test-os (windows-latest)",
                ])
                  assert.ok(names.includes(name), `${scenario.id}: ${name}`);
                assert.deepEqual(
                  scenario.rows.filter((r) => r.macos).map((r) => r.job),
                  scenario.policy.macosRequired ? ["macos-tests"] : [],
                );
              } else if (action === "tag" || action === "diagnostic")
                assert.ok(
                  scenario.rows.some((r) => r.job === "darwin" && r.macos),
                );
              else
                assert.deepEqual(
                  scenario.rows.filter((r) => r.macos),
                  [],
                  scenario.id,
                );
              scenarios.push(scenario);
            }
    const lifecycle = [
      "pull_request/synchronize/source/false/true/true",
      "push/base/source/false/true/true",
      "pull_request/opened/version/false/true/true",
      "push/release-merge/version/false/true/true",
      "push/tag/version/false/true/true",
      "release/published/version/false/true/true",
      "workflow_run/completed/version/false/true/true",
    ];
    const runs = lifecycle.flatMap((id) => {
      const scenario = scenarios.find((s) => s.id === id);
      assert.ok(scenario, id);
      return [
        ...new Set(
          scenario.rows.filter((r) => r.macos).map((r) => r.runWorkflow),
        ),
      ].map((workflow) => ({
        scenario: id,
        workflow,
        rows: scenario.rows.filter(
          (r) => r.runWorkflow === workflow && r.macos,
        ),
      }));
    });
    const perRun = scenarios.flatMap((s) =>
      Object.keys(docs).map((workflow) => ({
        scenario: s.id,
        workflow,
        jobs: s.rows.filter((r) => r.runWorkflow === workflow && r.macos)
          .length,
      })),
    );
    execFileSync("mkdir", ["-p", ".github/.telemetry"]);
    writeFileSync(
      ".github/.telemetry/macos-policy.json",
      JSON.stringify(
        {
          schema_version: 1,
          digest: sourceDigest(),
          scenarios,
          perRun,
          lifecycle: runs,
          jobsPerRun: Math.max(...perRun.map((r) => r.jobs)),
          runsPerChange: runs.length,
        },
        null,
        2,
      ) + "\n",
    );
  } finally {
    rmSync(h.dir, { recursive: true, force: true });
  }
});
test("unresolved runners/calls and malformed matrices fail explicitly", () => {
  const scenario = {
    event: "pull_request",
    payload: pr(),
    policy: { macosRequired: true },
    ref: "refs/heads/main",
  };
  for (const job of [
    { "runs-on": "${{ mystery.runner }}" },
    { uses: "outside/repo/.github/workflows/test.yml@main" },
    { "runs-on": "${{ matrix.os }}", strategy: { matrix: "${{ unknown }}" } },
  ]) {
    assert.throws(
      () =>
        resolveRun("broken.yml", scenario, {
          "broken.yml": { jobs: { broken: job } },
        }),
      /Unrecognized|unresolved/,
    );
  }
  assert.deepEqual(
    expand({
      os: ["macos-latest", "windows-latest"],
      include: [{ os: "macos-latest", target: "arm64" }],
      exclude: [{ os: "windows-latest" }],
    }),
    [{ os: "macos-latest", target: "arm64" }],
  );
});
test("Linux required carrier reports real success and authorized omission; all other outcomes fail", () => {
  const cmd = resolve(".github/macos-result.mjs");
  for (const [policy, mac, required, ok] of [
    ["success", "success", "true", true],
    ["success", "skipped", "false", true],
    ["success", "failure", "true", false],
    ["success", "cancelled", "true", false],
    ["success", "skipped", "true", false],
    ["failure", "skipped", "false", false],
    ["cancelled", "success", "true", false],
    ["", "success", "true", false],
    ["success", "", "true", false],
    ["success", "success", "false", false],
  ]) {
    const result = spawnSync("node", [cmd, policy, mac, required], {
      encoding: "utf8",
    });
    assert.equal(result.status === 0, ok, `${policy}/${mac}/${required}`);
    if (ok && required === "false")
      assert.match(result.stdout, /intentionally omitted/);
  }
});

test("local reusable calls expand their actual matrices and refuse automatic diagnostic opt-in", () => {
  const scenario = {
    event: "pull_request",
    payload: pr(),
    policy: { macosRequired: true },
    ref: "refs/heads/main",
  };
  const docs = {
    "outer.yml": { jobs: { call: { uses: "./.github/workflows/inner.yml" } } },
    "inner.yml": {
      jobs: {
        proof: {
          "runs-on": "${{ matrix.os }}",
          strategy: {
            matrix: {
              include: [
                { os: "macos-15", target: "aarch64-apple-darwin" },
                { os: "macos-15", target: "x86_64-apple-darwin" },
              ],
            },
          },
        },
      },
    },
  };
  const rows = resolveRun("outer.yml", scenario, docs);
  assert.deepEqual(
    rows.map((r) => r.matrix.target),
    ["aarch64-apple-darwin", "x86_64-apple-darwin"],
  );
  assert.ok(rows.every((r) => r.macos));
  docs["outer.yml"].jobs.call.with = { diagnostic_rebuild: true };
  assert.throws(
    () => resolveRun("outer.yml", scenario, docs),
    /automatic reusable caller/,
  );
});
