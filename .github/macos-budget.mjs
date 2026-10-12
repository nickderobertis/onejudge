#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { report } from "@onebudgetspec/sdk";
import { sourceDigest } from "./workflow-graph.mjs";
const args = process.argv.slice(2);
const options = Object.fromEntries(
  Array.from({ length: args.length / 2 }, (_, i) => [
    args[i * 2],
    args[i * 2 + 1],
  ]),
);
const telemetry = JSON.parse(
  readFileSync(resolve(options["--telemetry"]), "utf8"),
);
process.chdir(fileURLToPath(new URL("../", import.meta.url)));
if (telemetry.schema_version !== 1 || telemetry.digest !== sourceDigest())
  throw new Error(
    "missing/stale workflow telemetry: run github-workflows:test for this tree",
  );
const metric = options["--metric"];
if (!["jobs-per-run", "runs-per-change"].includes(metric))
  throw new Error("unknown Mac metric");
const value =
  metric === "jobs-per-run" ? telemetry.jobsPerRun : telemetry.runsPerChange;
const detail = JSON.stringify({
  metric,
  workload: telemetry.scenarios.length,
  lifecycle: telemetry.lifecycle,
  perWorkflow: telemetry.perRun,
  architectures: telemetry.scenarios.flatMap((s) =>
    s.rows.filter((r) => r.macos),
  ),
});
if (!report(value, detail)) console.log(JSON.stringify({ value, detail }));
