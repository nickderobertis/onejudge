#!/usr/bin/env node
// The module-boundary rule, over the real project graph.
//
//   node scripts/check-project-boundaries.mjs [PROJECT ...]
//
// Checks the outgoing edges of each named project (every project when none is
// named). Every project's `lint` target runs it for itself, so a change that
// draws a new edge fails the lint of the project that drew it.
//
// The edges are the union of two sources, so an edge cannot hide in either:
//   * the graph Nx itself computes (`nx graph --file`), i.e. every
//     `implicitDependencies` entry and anything Nx infers;
//   * Cargo's path dependencies between workspace members (`cargo metadata`).
//     Each must also be in the Nx graph — otherwise affected detection would not
//     know that a change to the dependency reaches the dependent.
// Every edge is then held to nx.json's `boundaries.allow`: a project tagged
// `type:X` may depend only on projects whose `type:` tag `allow["type:X"]` lists.
// Each project carries exactly one `type:` tag.
//
// Quiet on success (one line); on failure, one line per violation naming the
// edge, then the fix.
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(join(root, "package.json"));

function fail(lines) {
  for (const line of lines) console.error(`check-project-boundaries: ${line}`);
  process.exit(1);
}

/** `what`'s failure, with its own stderr and the next step, as a failed run. */
function failedRun(what, error, action) {
  const stderr = String(error.stderr ?? error.message ?? error).trim();
  fail([`${what} failed: ${stderr || "no output"}`, `ACTION: ${action}`]);
}

/** The project graph exactly as Nx computes it for this checkout. */
function nxGraph() {
  const scratch = mkdtempSync(join(tmpdir(), "onejudge-graph-"));
  try {
    const manifest = require.resolve("nx/package.json");
    const nx = join(dirname(manifest), require(manifest).bin.nx);
    const file = join(scratch, "graph.json");
    execFileSync(process.execPath, [nx, "graph", `--file=${file}`], {
      cwd: root,
      env: { ...process.env, NX_DAEMON: "false", NX_NO_CLOUD: "true" },
      stdio: ["ignore", "ignore", "pipe"],
    });
    return JSON.parse(readFileSync(file, "utf8")).graph;
  } catch (error) {
    return failedRun(
      "computing the Nx project graph (`nx graph`)",
      error,
      "run `bash scripts/node-modules.sh` to heal the Nx install, then fix the project.json the message names",
    );
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

/** Cargo's path dependencies between workspace members, as [fromDir, toDir]. */
function cargoEdges() {
  let output;
  try {
    output = execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps"], {
      cwd: root,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (error) {
    return failedRun(
      "reading the Cargo workspace (`cargo metadata`)",
      error,
      "fix the Cargo.toml the message names (`cargo metadata --no-deps` reproduces it)",
    );
  }
  const metadata = JSON.parse(output);
  const edges = [];
  for (const pkg of metadata.packages) {
    const from = relative(root, dirname(pkg.manifest_path));
    for (const dep of pkg.dependencies) {
      if (dep.path) edges.push([from, relative(root, dep.path), pkg.name, dep.name]);
    }
  }
  return edges;
}

let nxJson;
try {
  nxJson = JSON.parse(readFileSync(join(root, "nx.json"), "utf8"));
} catch (error) {
  fail([`nx.json could not be read: ${error.message}`, "ACTION: restore a readable, valid nx.json"]);
}
const allow = nxJson?.boundaries?.allow;
const typeTag = /^type:[a-z][a-z0-9-]*$/;
const wellFormed =
  allow !== null &&
  typeof allow === "object" &&
  !Array.isArray(allow) &&
  Object.entries(allow).every(
    ([type, allowed]) =>
      typeTag.test(type) &&
      Array.isArray(allowed) &&
      allowed.every((target) => typeof target === "string" && target in allow),
  );
if (!wellFormed) {
  fail([
    'nx.json has no well-formed "boundaries.allow" table to enforce',
    "ACTION: restore it — one `type:` tag per key, each listing the declared `type:` tags it may depend on",
  ]);
}

const graph = nxGraph();
const projects = graph.nodes;
const typeOf = {};
const problems = [];
for (const [name, node] of Object.entries(projects)) {
  const types = (node.data.tags ?? []).filter((tag) => tag.startsWith("type:"));
  if (types.length !== 1) {
    problems.push(`${name} carries ${types.length} type: tags (${types.join(", ") || "none"}); give it exactly one`);
  } else if (!(types[0] in allow)) {
    problems.push(`${name} is tagged ${types[0]}, which nx.json "boundaries.allow" does not declare`);
  } else {
    typeOf[name] = types[0];
  }
}

const requested = process.argv.slice(2);
for (const name of requested) {
  if (!(name in projects)) {
    fail([
      `no project named ${name} in the Nx graph (projects: ${Object.keys(projects).sort().join(", ")})`,
      "ACTION: pass a listed project, or add a project.json naming it",
    ]);
  }
}
const checked = new Set(requested.length > 0 ? requested : Object.keys(projects));

const byRoot = Object.fromEntries(Object.entries(projects).map(([name, node]) => [node.data.root, name]));
const edges = new Map();
for (const [source, deps] of Object.entries(graph.dependencies)) {
  for (const dep of deps) {
    if (dep.target in projects) edges.set(`${source} -> ${dep.target}`, [source, dep.target]);
  }
}
for (const [fromDir, toDir, fromCrate, toCrate] of cargoEdges()) {
  const source = byRoot[fromDir];
  const target = byRoot[toDir];
  if (!source || !target) {
    problems.push(`Cargo edge ${fromCrate} -> ${toCrate} joins a crate with no project.json beside its Cargo.toml`);
    continue;
  }
  if (!edges.has(`${source} -> ${target}`) && checked.has(source)) {
    problems.push(
      `Cargo edge ${source} -> ${target} is missing from the Nx graph, so a change to ${target} would not select ${source}; add "${target}" to ${source}'s implicitDependencies`,
    );
  }
  edges.set(`${source} -> ${target}`, [source, target]);
}

for (const [source, target] of edges.values()) {
  if (!checked.has(source) || !typeOf[source] || !typeOf[target]) continue;
  const allowed = allow[typeOf[source]];
  if (!allowed.includes(typeOf[target])) {
    problems.push(
      `${source} (${typeOf[source]}) -> ${target} (${typeOf[target]}) is not allowed: ${typeOf[source]} may depend only on [${allowed.join(", ")}] (nx.json "boundaries.allow")`,
    );
  }
}

if (problems.length > 0) {
  fail([...problems, "ACTION: remove the edge, or change the boundary in nx.json deliberately and say why"]);
}
const scope = requested.length > 0 ? requested.join(", ") : `${checked.size} projects`;
console.log(`check-project-boundaries: ${scope}: every edge allowed`);
