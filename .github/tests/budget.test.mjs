import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { sourceDigest, workflows, resolveRun } from "../workflow-graph.mjs";
import { recordingMeasures, TELEMETRY_SCHEMA_VERSION } from "../telemetry.mjs";

test("budget commands consume recorded telemetry, report through the SDK and refuse missing/stale inputs", () => {
  const dir = mkdtempSync(
    join(
      process.env.ONEPIPELINE_NODE_SCRATCH_DIR || process.env.RUNNER_TEMP,
      "budget-reader-",
    ),
  );
  try {
    const telemetry = join(dir, "telemetry.json"),
      result = join(dir, "result.json");
    const docs = workflows();
    const fixtureScenarios = [
      {
        id: "ordinary-pr",
        event: "pull_request",
        payload: { action: "synchronize" },
        ref: "refs/heads/main",
        policy: { macosRequired: true },
      },
      {
        id: "tag-build",
        event: "push",
        payload: {},
        ref: "refs/tags/v0.19.0",
        outputs: {
          "darwin-plan": {
            build: "true",
            tag: "v0.19.0",
            sha: "a".repeat(40),
            version: "0.19.0",
          },
        },
      },
    ];
    for (const scenario of fixtureScenarios) {
      const file =
        scenario.event === "push" ? "release-binaries.yml" : "ci.yml";
      scenario.rows = resolveRun(file, scenario, docs).map((row) => ({
        ...row,
        runWorkflow: file,
      }));
    }
    const workflowNames = Object.keys(docs),
      lifecycleScenarios = fixtureScenarios.map((s) => s.id);
    const record = {
      schema_version: TELEMETRY_SCHEMA_VERSION,
      digest: sourceDigest(),
      scenarios: fixtureScenarios,
      workflowNames,
      lifecycleScenarios,
      ...recordingMeasures(fixtureScenarios, workflowNames, lifecycleScenarios),
    };
    const golden = JSON.parse(
      readFileSync(".github/tests/fixtures/telemetry-fields.json", "utf8"),
    );
    assert.equal(record.schema_version, golden.schema_version);
    assert.deepEqual(Object.keys(record).sort(), golden.fields.sort());
    writeFileSync(telemetry, JSON.stringify(record));
    for (const [metric, expected] of [
      ["jobs-per-run", record.jobsPerRun],
      ["runs-per-change", record.runsPerChange],
    ]) {
      const run = spawnSync(
        "node",
        [
          ".github/macos-budget.mjs",
          "--metric",
          metric,
          "--telemetry",
          telemetry,
        ],
        {
          env: { ...process.env, ONEBUDGETSPEC_RESULT: result },
          encoding: "utf8",
        },
      );
      assert.equal(run.status, 0, run.stderr);
      assert.equal(JSON.parse(readFileSync(result, "utf8")).value, expected);
    }
    const env = { ...process.env };
    delete env.ONEBUDGETSPEC_RESULT;
    const standalone = spawnSync(
      "node",
      [
        ".github/macos-budget.mjs",
        "--metric",
        "jobs-per-run",
        "--telemetry",
        telemetry,
      ],
      { env, encoding: "utf8" },
    );
    assert.equal(standalone.status, 0, standalone.stderr);
    assert.equal(JSON.parse(standalone.stdout).value, record.jobsPerRun);
    assert.match(JSON.parse(standalone.stdout).detail, /perWorkflow/);
    for (const invalid of [
      { ...record, jobsPerRun: -1 },
      { ...record, jobsPerRun: 0 },
      { ...record, runsPerChange: 0 },
      { ...record, lifecycle: [] },
      { ...record, perRun: record.perRun.map((row) => ({ ...row, jobs: 0 })) },
      { ...record, lifecycleScenarios: ["missing-scenario"] },
      { ...record, workflowNames: ["unregistered-workflow"] },
      { ...record, runsPerChange: "two" },
      { ...record, scenarios: [{}] },
      { ...record, perRun: [{}] },
      { ...record, lifecycle: [{}] },
      {
        ...record,
        scenarios: [
          {
            id: "invalid-targets",
            event: "push",
            rows: [
              {
                workflow: "release-binaries.yml",
                event: "push",
                job: "darwin",
                name: "darwin",
                runner: "macos-15",
                runWorkflow: "release-binaries.yml",
                matrix: {},
                enabled: true,
                macos: true,
                targets: "not-an-array",
              },
            ],
          },
        ],
      },
    ]) {
      writeFileSync(telemetry, JSON.stringify(invalid));
      const failed = spawnSync(
        "node",
        [
          ".github/macos-budget.mjs",
          "--metric",
          "jobs-per-run",
          "--telemetry",
          telemetry,
        ],
        { encoding: "utf8" },
      );
      assert.notEqual(failed.status, 0);
      assert.match(failed.stderr, /invalid workflow telemetry/);
    }
    writeFileSync(telemetry, JSON.stringify({ ...record, digest: "stale" }));
    const stale = spawnSync(
      "node",
      [
        ".github/macos-budget.mjs",
        "--metric",
        "jobs-per-run",
        "--telemetry",
        telemetry,
      ],
      { encoding: "utf8" },
    );
    assert.notEqual(stale.status, 0);
    assert.match(stale.stderr, /stale/);
    const missing = spawnSync(
      "node",
      [
        ".github/macos-budget.mjs",
        "--metric",
        "jobs-per-run",
        "--telemetry",
        join(dir, "missing"),
      ],
      { encoding: "utf8" },
    );
    assert.notEqual(missing.status, 0);
    assert.match(missing.stderr, /ENOENT/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
