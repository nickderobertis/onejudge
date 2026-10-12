// Repository-private interpreter of this tree's workflow routing. Unknowns are errors.
import { readFileSync, readdirSync } from "node:fs";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { Parser, Lexer, Evaluator, data } from "@actions/expressions";
import YAML from "yaml";
import { truthy } from "@actions/expressions/result";

const directory = new URL("./workflows/", import.meta.url);
export function workflows() {
  return Object.fromEntries(
    readdirSync(directory)
      .filter((n) => /\.ya?ml$/.test(n))
      .sort()
      .map((name) => [
        name,
        YAML.parse(readFileSync(new URL(name, directory), "utf8")),
      ]),
  );
}
export function sourceDigest() {
  const hash = createHash("sha256");
  function scan(dir) {
    for (const row of readdirSync(dir, { withFileTypes: true }).sort((a, b) =>
      a.name.localeCompare(b.name),
    )) {
      if (
        ["node_modules", ".telemetry", "__pycache__", ".ruff_cache"].includes(
          row.name,
        )
      )
        continue;
      const path = join(dir, row.name);
      if (row.isDirectory()) scan(path);
      else hash.update(path).update(readFileSync(path));
    }
  }
  scan(".github");
  scan("pypi");
  for (const file of [
    "scripts/release-pypi-plan.sh",
    "justfile",
    "install.sh",
    "Cargo.toml",
    "release-targets.toml",
  ])
    hash.update(file).update(readFileSync(file));
  return hash.digest("hex");
}
function value(v) {
  if (v === null || v === undefined) return new data.Null();
  if (typeof v === "boolean") return new data.BooleanData(v);
  if (typeof v === "number") return new data.NumberData(v);
  if (typeof v === "string") return new data.StringData(v);
  if (Array.isArray(v)) return new data.Array(...v.map(value));
  return new data.Dictionary(
    ...Object.entries(v).map(([key, v]) => ({ key, value: value(v) })),
  );
}
export function expression(source, context) {
  if (typeof source === "boolean") return source;
  const funcs = new Map(
    ["always", "success", "failure", "cancelled"].map((name) => [
      name,
      {
        name,
        minArgs: 0,
        maxArgs: 0,
        call: () =>
          new data.BooleanData(
            name === "always" ||
              (name === "success" &&
                Object.values(context.needs || {}).every(
                  (n) => n.result === "success",
                )) ||
              (name === "failure" &&
                Object.values(context.needs || {}).some(
                  (n) => n.result === "failure",
                )) ||
              (name === "cancelled" &&
                Object.values(context.needs || {}).some(
                  (n) => n.result === "cancelled",
                )),
          ),
      },
    ]),
  );
  const body = source.replace(/^\s*\$\{\{([\s\S]*)\}\}\s*$/, "$1");
  const ast = new Parser(new Lexer(body).lex().tokens, Object.keys(context), [
    ...funcs.values(),
  ]).parse();
  return new Evaluator(ast, value(context), funcs).evaluate();
}
function truth(v) {
  return typeof v === "boolean" ? v : truthy(v);
}
export function render(source, context) {
  if (typeof source !== "string")
    throw new Error(
      `unresolved runner/name expression: ${JSON.stringify(source)}`,
    );
  return source.replace(/\$\{\{(.*?)\}\}/g, (_, body) =>
    expression(body, context).coerceString(),
  );
}
export function expand(matrix = {}) {
  if (typeof matrix !== "object" || matrix === null || Array.isArray(matrix))
    throw new Error("unresolved matrix expression");
  for (const key of ["include", "exclude"])
    if (
      key in matrix &&
      (!Array.isArray(matrix[key]) ||
        !matrix[key].every(
          (row) =>
            row !== null && typeof row === "object" && !Array.isArray(row),
        ))
    )
      throw new Error(`unresolved matrix ${key}: expected object rows`);
  const axes = Object.entries(matrix).filter(
    ([k]) => !["include", "exclude"].includes(k),
  );
  let rows = axes.length ? [{}] : [];
  for (const [key, vals] of axes) {
    if (!Array.isArray(vals)) throw new Error(`unresolved matrix axis ${key}`);
    rows = rows.flatMap((row) => vals.map((v) => ({ ...row, [key]: v })));
  }
  const original = rows.map((r) => ({ ...r }));
  for (const include of matrix.include || []) {
    let matched = false;
    original.forEach((row, i) => {
      if (
        Object.keys(row).every((k) => !(k in include) || include[k] === row[k])
      ) {
        rows[i] = { ...rows[i], ...include };
        matched = true;
      }
    });
    if (!matched) rows.push(include);
  }
  rows = rows.filter(
    (row) =>
      !(matrix.exclude || []).some((ex) =>
        Object.entries(ex).every(([k, v]) => row[k] === v),
      ),
  );
  return axes.length || matrix.include ? rows : [{}];
}
function glob(pattern, text) {
  if (typeof pattern !== "string" || /[!+?\[\]{}()\\]/.test(pattern))
    throw new Error(`unsupported trigger pattern: ${pattern}`);
  let regex = "";
  for (let i = 0; i < pattern.length; i++) {
    if (pattern[i] === "*") {
      if (pattern[i + 1] === "*") {
        i++;
        if (pattern[i + 1] === "/") {
          i++;
          regex += "(?:.*/)?";
        } else regex += ".*";
      } else regex += "[^/]*";
    } else regex += pattern[i].replace(/[.\^$|]/g, "\\$&");
  }
  return new RegExp(`^${regex}$`).test(text);
}
export function triggered(doc, scenario) {
  const on = doc.on;
  if (!on || typeof on !== "object" || Array.isArray(on))
    throw new Error("unsupported workflow trigger declaration");
  if (!(scenario.event in on)) return false;
  const config = on[scenario.event];
  if (config === null) return true;
  if (scenario.event === "schedule") {
    if (
      !Array.isArray(config) ||
      !config.every(
        (row) =>
          row &&
          typeof row === "object" &&
          typeof row.cron === "string" &&
          row.cron.length,
      )
    )
      throw new Error("unsupported schedule trigger configuration");
    return true;
  }
  if (typeof config !== "object" || Array.isArray(config))
    throw new Error("unsupported event trigger configuration");
  const supported =
    scenario.event === "push"
      ? ["branches", "tags", "paths", "paths-ignore"]
      : scenario.event === "pull_request"
        ? ["types", "paths", "paths-ignore"]
        : scenario.event === "workflow_dispatch"
          ? ["inputs"]
          : scenario.event === "workflow_run"
            ? ["workflows", "types"]
            : ["types"];
  for (const key of [
    "types",
    "branches",
    "tags",
    "paths",
    "paths-ignore",
    "workflows",
  ]) {
    if (
      key in config &&
      (!Array.isArray(config[key]) ||
        !config[key].every((value) => typeof value === "string"))
    )
      throw new Error(`unsupported ${scenario.event} trigger value: ${key}`);
  }
  for (const key of Object.keys(config))
    if (!supported.includes(key))
      throw new Error(`unsupported ${scenario.event} trigger key: ${key}`);
  if (config.types && !config.types.includes(scenario.payload.action))
    return false;
  if (scenario.event === "push") {
    const tag = scenario.ref.startsWith("refs/tags/");
    const filters = tag ? config.tags : config.branches;
    if (!filters && (config.tags || config.branches)) return false;
    if (
      filters &&
      !filters.some((p) =>
        glob(p, scenario.ref.replace(/^refs\/(tags|heads)\//, "")),
      )
    )
      return false;
  }
  if (config.paths || config["paths-ignore"]) {
    if (!Array.isArray(scenario.paths))
      throw new Error("unresolved path-sensitive event filter");
    if (
      config.paths &&
      !scenario.paths.some((path) => config.paths.some((p) => glob(p, path)))
    )
      return false;
    if (
      config["paths-ignore"] &&
      scenario.paths.every((path) =>
        config["paths-ignore"].some((p) => glob(p, path)),
      )
    )
      return false;
  }
  if (
    scenario.event === "workflow_run" &&
    config.workflows &&
    !config.workflows.includes(scenario.payload.workflow_run?.name)
  )
    return false;
  return true;
}
export function resolveRun(file, scenario, docs = workflows(), stack = []) {
  if (stack.includes(file)) throw new Error(`recursive workflow call: ${file}`);
  const doc = docs[file];
  if (!doc) throw new Error(`unresolved reusable workflow: ${file}`);
  const results = {},
    rows = [],
    pending = new Set(Object.keys(doc.jobs));
  while (pending.size) {
    let progressed = false;
    for (const id of pending) {
      const job = doc.jobs[id];
      const needsIds =
        typeof job.needs === "string" ? [job.needs] : job.needs || [];
      if (needsIds.some((n) => !(n in doc.jobs)))
        throw new Error(`${file}/${id}: missing prerequisite`);
      if (needsIds.some((n) => !(n in results))) continue;
      progressed = true;
      pending.delete(id);
      const needs = Object.fromEntries(needsIds.map((n) => [n, results[n]]));
      const declaredInputs =
        doc.on?.workflow_dispatch?.inputs ||
        doc.on?.workflow_call?.inputs ||
        {};
      const inputDefaults = Object.fromEntries(
        Object.entries(declaredInputs)
          .filter(([, definition]) => "default" in definition)
          .map(([key, definition]) => [key, definition.default]),
      );
      const actualInputs = { ...inputDefaults, ...scenario.inputs };
      if (
        scenario.event !== "workflow_dispatch" &&
        actualInputs.diagnostic_rebuild
      )
        throw new Error(
          "automatic workflow cannot opt into diagnostic rebuild",
        );
      const context = {
        github: {
          event_name: scenario.event,
          event: scenario.payload,
          repository: "nickderobertis/onejudge",
          ref: scenario.ref,
          ref_name: scenario.ref.split("/").pop(),
        },
        needs,
        inputs: actualInputs,
        vars: { PYPI_PUBLISH: scenario.publish ? "true" : "false" },
        secrets: {},
        matrix: {},
      };
      const enabled =
        (!job.if || truth(expression(job.if, context))) &&
        (/(?:always|success|failure|cancelled)\s*\(/.test(job.if || "") ||
          needsIds.every((n) => results[n].result === "success"));
      const outputs =
        id === "macos-policy"
          ? { required: String(scenario.policy.macosRequired) }
          : id === "darwin-plan"
            ? {
                ...(scenario.outputs?.[id] ??
                  (() => {
                    throw new Error("unresolved Darwin producer output");
                  })()),
              }
            : id === "plan"
              ? {
                  tag: "v0.19.0",
                  sha: "a".repeat(40),
                  version: "0.19.0",
                  "upload-cli": String(scenario.cli !== false),
                  "upload-sdk": String(scenario.sdk !== false),
                }
              : {};
      results[id] = scenario.results?.[id] ?? {
        result: enabled ? "success" : "skipped",
        outputs: scenario.outputs?.[id] ?? outputs,
      };
      for (const matrix of expand(job.strategy?.matrix)) {
        context.matrix = matrix;
        if (job.uses) {
          if (!job.uses.startsWith("./.github/workflows/"))
            throw new Error(`unresolved reusable workflow call ${job.uses}`);
          const inputs = Object.fromEntries(
            Object.entries(job.with || {}).map(([key, val]) => [
              key,
              typeof val === "string" && val.startsWith("${{")
                ? expression(val, context).value
                : val,
            ]),
          );
          if (
            scenario.event !== "workflow_dispatch" &&
            inputs.diagnostic_rebuild
          )
            throw new Error(
              "automatic reusable caller cannot opt into diagnostic rebuild",
            );
          if (enabled)
            rows.push(
              ...resolveRun(
                job.uses.split("/").pop(),
                { ...scenario, inputs },
                docs,
                [...stack, file],
              ).map((r) => ({ ...r, job: `${id}/${r.job}` })),
            );
          continue;
        }
        const runner = render(job["runs-on"], context);
        if (!/^(ubuntu|macos|windows)-[A-Za-z0-9.-]+$/.test(runner))
          throw new Error(`unresolved runner: ${runner}`);
        const name = job.name
          ? render(job.name, context)
          : id +
            (Object.keys(matrix).length
              ? ` (${Object.values(matrix).join(", ")})`
              : "");
        rows.push({
          workflow: file,
          event: scenario.event,
          job: id,
          name,
          runner,
          matrix,
          enabled,
          macos: enabled && runner.startsWith("macos-"),
        });
      }
    }
    if (!progressed)
      throw new Error(`unresolved job dependency cycle in ${file}`);
  }
  return rows;
}
