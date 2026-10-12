#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { report } from "@onebudgetspec/sdk";
import { sourceDigest } from "./workflow-graph.mjs";
import { commandOptions } from "./command-options.mjs";
const args = process.argv.slice(2);
const options = commandOptions(args, ["--metric", "--telemetry"]);
const metric = options["--metric"];
if (!["jobs-per-run", "runs-per-change"].includes(metric))
  throw new Error("unknown Mac metric");
const telemetry = JSON.parse(
  readFileSync(resolve(options["--telemetry"]), "utf8"),
);
process.chdir(fileURLToPath(new URL("../", import.meta.url)));
const isObject = (v) =>
  v !== null && typeof v === "object" && !Array.isArray(v);
const isString = (v) => typeof v === "string";
const metricValue = (v) =>
  typeof v === "number" && Number.isFinite(v) && v >= 0;
const jobRow = (row) =>
  isObject(row) &&
  ["workflow", "event", "job", "name", "runner", "runWorkflow"].every((k) =>
    isString(row[k]),
  ) &&
  isObject(row.matrix) &&
  (row.targets === undefined ||
    (Array.isArray(row.targets) &&
      row.targets.length > 0 &&
      row.targets.every(isString))) &&
  typeof row.enabled === "boolean" &&
  typeof row.macos === "boolean";
if (
  !isObject(telemetry) ||
  !metricValue(telemetry.jobsPerRun) ||
  !metricValue(telemetry.runsPerChange) ||
  !Array.isArray(telemetry.scenarios) ||
  !telemetry.scenarios.every(
    (s) =>
      isObject(s) &&
      isString(s.id) &&
      isString(s.event) &&
      Array.isArray(s.rows) &&
      s.rows.every(jobRow),
  ) ||
  !Array.isArray(telemetry.perRun) ||
  !telemetry.perRun.every(
    (r) =>
      isObject(r) &&
      isString(r.scenario) &&
      isString(r.workflow) &&
      metricValue(r.jobs),
  ) ||
  !Array.isArray(telemetry.lifecycle) ||
  !telemetry.lifecycle.every(
    (r) =>
      isObject(r) &&
      isString(r.scenario) &&
      isString(r.workflow) &&
      Array.isArray(r.rows) &&
      r.rows.every(jobRow),
  )
)
  throw new Error("invalid workflow telemetry shape");
if (telemetry.schema_version !== 1 || telemetry.digest !== sourceDigest())
  throw new Error(
    "missing/stale workflow telemetry: run github-workflows:test for this tree",
  );
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
