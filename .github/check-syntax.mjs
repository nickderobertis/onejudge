import { readdirSync } from "node:fs";
import { execFileSync } from "node:child_process";
for (const name of readdirSync(".github").filter((n) => n.endsWith(".mjs")))
  execFileSync("node", ["--check", `.github/${name}`]);
