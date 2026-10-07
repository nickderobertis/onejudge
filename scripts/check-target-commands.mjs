#!/usr/bin/env node
// Every Nx target command must run under Windows' default shell, too.
//
//   node scripts/check-target-commands.mjs [PROJECT ...]
//
// Nx runs a `nx:run-commands` command through the platform shell: `/bin/sh` on
// Linux and macOS, `cmd.exe` on Windows, where the `test-os (windows-latest)`
// job runs the affected tests. So a command written in POSIX-only syntax passes
// here and fails there — `ONEJUDGE_COVERAGE=0 just …` once did, as
// `'ONEJUDGE_COVERAGE' is not recognized as an internal or external command`.
// This reads every target of each named project (every project when none is
// named) as Nx resolves it (`nx graph --file`) and refuses, in each command:
//   * a leading environment assignment (`NAME=value cmd`) — declare it in the
//     target's `options.env`, which Nx sets on every platform;
//   * a single-quoted argument — cmd.exe passes the quotes through, so `''` is
//     not an empty argument there; write `""`;
//   * a `$` expansion (`$VAR`, `${VAR}`, `$(cmd)`) — cmd.exe expands none of it;
//     move it into the `just` recipe the target runs, which runs under bash.
// Every project's `lint` runs it for itself, beside the boundary check.
//
// Quiet on success (one line); on failure, one line per command naming the
// project, the target and what to change.
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(join(root, "package.json"));

function fail(lines) {
  for (const line of lines) console.error(`check-target-commands: ${line}`);
  process.exit(1);
}

/** Every project's resolved configuration, exactly as Nx computes it here. */
function nxProjects() {
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
    const nodes = JSON.parse(readFileSync(file, "utf8")).graph?.nodes;
    const shaped =
      nodes &&
      typeof nodes === "object" &&
      Object.values(nodes).every(
        (node) => node?.data?.targets === undefined || (node.data.targets && typeof node.data.targets === "object"),
      );
    if (!shaped) throw new Error("its `nodes` are not the shape this check reads");
    return nodes;
  } catch (error) {
    const stderr = String(error.stderr ?? error.message ?? error).trim();
    return fail([
      `computing the Nx project graph (\`nx graph\`) failed: ${stderr || "no output"}`,
      "ACTION: run `bash scripts/node-modules.sh` to heal the Nx install, then fix the project.json the message names",
    ]);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

/** The shell command strings a resolved run-commands target runs. */
function commandsOf(target) {
  if (target?.executor !== "nx:run-commands") return [];
  const options = target.options ?? {};
  const commands = [];
  if (typeof options.command === "string") commands.push(options.command);
  for (const entry of Array.isArray(options.commands) ? options.commands : []) {
    if (typeof entry === "string") commands.push(entry);
    else if (typeof entry?.command === "string") commands.push(entry.command);
  }
  return commands;
}

/** What in `command` cmd.exe cannot run, or nothing. */
function posixOnly(command) {
  const found = [];
  const segments = command.split(/&&|\|\||[;|&]/);
  if (segments.some((segment) => /^\s*[A-Za-z_][A-Za-z0-9_]*=/.test(segment))) {
    found.push("a leading environment assignment (declare it in the target's options.env)");
  }
  if (command.includes("'")) {
    found.push('a single-quoted argument (cmd.exe keeps the quotes; write "" for an empty argument)');
  }
  if (/\$[A-Za-z_{(]/.test(command)) {
    found.push("a `$` expansion (cmd.exe expands none; move it into the just recipe the target runs)");
  }
  return found;
}

const projects = nxProjects();
const requested = process.argv.slice(2);
for (const name of requested) {
  if (!Object.hasOwn(projects, name)) {
    fail([
      `no project named ${name} in the Nx graph (projects: ${Object.keys(projects).sort().join(", ")})`,
      "ACTION: pass a listed project, or add a project.json naming it",
    ]);
  }
}
const checked = requested.length > 0 ? requested : Object.keys(projects).sort();

const problems = [];
let count = 0;
for (const name of checked) {
  for (const [targetName, target] of Object.entries(projects[name].data.targets ?? {})) {
    for (const command of commandsOf(target)) {
      count += 1;
      for (const what of posixOnly(command)) {
        problems.push(`${name}:${targetName} runs \`${command}\`, which carries ${what}`);
      }
    }
  }
}
if (problems.length > 0) {
  fail([...problems, "ACTION: rewrite the command so cmd.exe can run it, as each line above says"]);
}
const scope = requested.length > 0 ? requested.join(", ") : `${checked.length} projects`;
console.log(`check-target-commands: ${scope}: ${count} commands run on every platform's shell`);
