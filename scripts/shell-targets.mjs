#!/usr/bin/env node
// Whether one project declares the shell targets over its own root.
//
//   node scripts/shell-targets.mjs <project-root>     # `.` for the workspace root
//
// scripts/shell-files.sh asks this of the project owning each shell source it
// finds, so a script in a project nothing formats, lints or measures fails
// there. Declared means the parsed project.json has `format`, `format-check` and
// `lint` targets each running `just _sh-format`, `just _sh-format-check` and
// `just _sh-lint` with the project's own root as the first argument — as the
// command itself, in its `command`, `options.command` or one of
// `options.commands`, as the root project.json does. A mention anywhere else
// (an argument, a later step of a chain, a string another command prints), or
// a recipe run over another root, is not.
//
// Exit 0: declared. 1: not declared (silent; the caller names the file).
// 2: a project.json that cannot be read, is not JSON, or is not shaped as Nx
// reads one (an object; `targets` and each target's `options` objects;
// `options.commands` an array), the reason and the next action on stderr.
import { readFileSync } from "node:fs";

const SHELL_TARGETS = [
    ["format", "_sh-format"],
    ["format-check", "_sh-format-check"],
    ["lint", "_sh-lint"],
];

const [dir, ...rest] = process.argv.slice(2);
if (dir === undefined || rest.length > 0) {
    console.error("usage: node scripts/shell-targets.mjs <project-root>");
    process.exit(2);
}

function refuse(reason) {
    console.error(`${dir}/project.json: ${reason}`);
    console.error(`ACTION: fix ${dir}/project.json (nx show project <name> reads it the way Nx does), then re-run the recipe`);
    process.exit(2);
}

const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

let project;
try {
    project = JSON.parse(readFileSync(`${dir}/project.json`, "utf8"));
} catch (error) {
    refuse(error.message);
}
if (!isObject(project)) refuse("is not a JSON object");
const targets = project.targets ?? {};
if (!isObject(targets)) refuse("`targets` is not an object");

for (const [name, target] of Object.entries(targets)) {
    if (!isObject(target)) refuse(`target \`${name}\` is not an object`);
    if (!isObject(target.options ?? {})) refuse(`\`${name}.options\` is not an object`);
    if (!Array.isArray(target.options?.commands ?? [])) refuse(`\`${name}.options.commands\` is not an array`);
}

// Every command string a target runs, whichever form the executor takes it in.
function commands(name) {
    const target = targets[name] ?? {};
    const options = target.options ?? {};
    return [target.command, options.command, ...(options.commands ?? [])]
        .map((entry) => (isObject(entry) ? entry.command : entry))
        .filter((command) => typeof command === "string");
}

// The command's first three words, which a shell runs before reading anything
// after them, are the program, the recipe and its root.
const runsOverRoot = (command, recipe) => {
    const [program, called, root] = command.trim().split(/\s+/);
    return program === "just" && called === recipe && root === dir;
};

const declared = SHELL_TARGETS.every(([name, recipe]) =>
    commands(name).some((command) => runsOverRoot(command, recipe)),
);
process.exit(declared ? 0 : 1);
