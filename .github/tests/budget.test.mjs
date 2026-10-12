import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { sourceDigest } from "../workflow-graph.mjs";

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
    const record = {
      schema_version: 1,
      digest: sourceDigest(),
      scenarios: [],
      lifecycle: [],
      perRun: [],
      jobsPerRun: 7,
      runsPerChange: 9,
    };
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
