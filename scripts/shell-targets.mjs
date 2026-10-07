#!/usr/bin/env node
// Whether one project declares the shell targets over its own root.
//
//   node scripts/shell-targets.mjs <project-root>     # `.` for the workspace root
//
// scripts/shell-files.sh asks this of the project owning each shell source it
// finds, so a script in a project nothing formats, lints or measures fails
// there. Declared means the parsed project.json has `format`, `format-check` and
// `lint` targets each running `just _sh-format`, `just _sh-format-check` and
// `just _sh-lint` with the project's own root as the first argument — in its
// `command`, `options.command` or one of `options.commands`, alone or as one
// `&&` step — as the root project.json does. A mention anywhere else, or a
// recipe run over another root, is not.
//
// Exit 0: declared. 1: not declared (silent; the caller names the file).
// 2: no readable, parseable project.json, the reason on stderr.
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

let project;
try {
    project = JSON.parse(readFileSync(`${dir}/project.json`, "utf8"));
} catch (error) {
    console.error(`${dir}/project.json: ${error.message}`);
    process.exit(2);
}

// Every command string a target runs, whichever form the executor takes it in.
function commands(name) {
    const target = (project.targets ?? {})[name] ?? {};
    const options = target.options ?? {};
    return [target.command, options.command, ...(options.commands ?? [])]
        .map((entry) => (entry !== null && typeof entry === "object" ? entry.command : entry))
        .filter((command) => typeof command === "string");
}

const runsOverRoot = (command, recipe) =>
    command.split("&&").some((step) => {
        const [program, called, root] = step.trim().split(/\s+/);
        return program === "just" && called === recipe && root === dir;
    });

const declared = SHELL_TARGETS.every(([name, recipe]) =>
    commands(name).some((command) => runsOverRoot(command, recipe)),
);
process.exit(declared ? 0 : 1);
