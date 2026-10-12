import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { workflows, expand, resolveRun } from "../workflow-graph.mjs";

test("production release jobs retain every declared platform and use one Darwin producer", () => {
  const docs = workflows(),
    producer = docs["release-binaries.yml"].jobs,
    consumer = docs["release-pypi.yml"].jobs;
  const targets = producer.darwin.env.TARGETS.split(" ");
  const manifestTargets = JSON.parse(
    execFileSync(
      "python3",
      [
        "-c",
        'import json,sys;sys.path.insert(0,"pypi");from release_artifacts import TARGETS;print(json.dumps(TARGETS))',
      ],
      { encoding: "utf8" },
    ),
  );
  assert.deepEqual([...targets].sort(), [...manifestTargets].sort());
  const archives = [
    ...expand(producer.archives.strategy.matrix).map((m) => m.target),
    ...targets,
  ];
  const installed = [
    ...readFileSync("install.sh", "utf8").matchAll(/target="([a-z0-9_-]+)"/g),
  ].map((m) => m[1]);
  for (const target of installed)
    assert.ok(archives.includes(target), `installer target lost: ${target}`);
  const wheels = expand(consumer["build-cli-wheels"].strategy.matrix).map(
    (m) => m.target,
  );
  assert.ok(
    wheels.includes("aarch64-unknown-linux-gnu"),
    "Linux ARM wheel lost",
  );
  assert.ok(wheels.includes("x86_64-pc-windows-msvc"), "Windows wheel lost");
  assert.ok(
    archives.includes("x86_64-pc-windows-msvc"),
    "Windows archive lost",
  );
  assert.ok(wheels.every((t) => !t.endsWith("apple-darwin")));
  // Rosetta and Mach-O execution require the real hosted macOS runner; Linux
  // proves the same installer with a native product and the typed artifact
  // contract for both Darwin targets, without pretending to execute Darwin.
  const commands = producer.darwin.steps.map((s) => s.run || "").join("\n");
  assert.match(commands, /for target in \$TARGETS/);
  assert.match(commands, /just .*pypi-release "\$target" dist/);
  assert.match(
    commands,
    /smoke_release\.py --target "\$target" --root dist --version "\$VERSION"/,
  );
  assert.match(commands, /release_artifacts\.py stage/);
  assert.ok(
    producer.darwin.steps.some((s) => s.run?.includes("--install-rosetta")),
  );
  const artifact = producer.darwin.steps.find((s) =>
    s.uses?.startsWith("actions/upload-artifact@"),
  );
  assert.equal(artifact.with.name, "${{ needs.darwin-plan.outputs.name }}");
  assert.equal(producer["publish-darwin"]["runs-on"], "ubuntu-latest");
  assert.match(
    producer["publish-darwin"].steps.map((s) => s.run || "").join("\n"),
    /publish_release\.py --repo/,
  );
  const handoff = consumer["darwin-wheels"];
  assert.equal(handoff["runs-on"], "ubuntu-latest");
  const fetch = handoff.steps.find((s) =>
    s.run?.includes("fetch_release_artifacts.py"),
  );
  assert.equal(fetch.env.TAG, "${{ needs.plan.outputs.tag }}");
  assert.equal(fetch.env.SHA, "${{ needs.plan.outputs.sha }}");
  assert.equal(fetch.env.VERSION, "${{ needs.plan.outputs.version }}");
  for (const job of [producer.darwin, handoff]) {
    const source = job.steps.find((s) =>
      s.uses?.startsWith("actions/checkout@"),
    );
    assert.match(source.with.ref, /needs\.(darwin-plan|plan)\.outputs\.sha/);
    const tooling = job.steps.find((s) => s.with?.path === ".release-tools");
    assert.equal(tooling.with.ref, "${{ github.workflow_sha }}");
    assert.equal(tooling.with.repository, "${{ github.repository }}");
    assert.equal(tooling.with["persist-credentials"], false);
  }
});

test("a failed producer cannot be published and a held CLI does not require builds", () => {
  const jobs = workflows()["release-pypi.yml"].jobs;
  assert.deepEqual(jobs["publish-cli"].needs, [
    "plan",
    "build-cli-wheels",
    "darwin-wheels",
  ]);
  assert.match(jobs["publish-cli"].if, /always\(\)/);
  assert.match(
    jobs["publish-cli"].if,
    /needs\.darwin-wheels\.result == 'success'/,
  );
  assert.match(
    jobs["publish-cli"].if,
    /needs\.build-cli-wheels\.result == 'success'/,
  );
  assert.match(
    jobs["publish-cli"].if,
    /needs\.plan\.outputs\.upload-cli == 'false'/,
  );
  for (const id of ["build-cli-wheels", "darwin-wheels"])
    assert.equal(jobs[id].if, "needs.plan.outputs.upload-cli == 'true'");
});

test("actual publishing condition propagates failed/cancelled prerequisites and intentional held-distribution skips", () => {
  const docs = workflows();
  for (const held of [false, true])
    for (const planResult of ["success", "failure", "cancelled"])
      for (const buildResult of [
        "success",
        "failure",
        "cancelled",
        "skipped",
      ]) {
        const scenario = {
          event: "release",
          payload: { action: "published" },
          ref: "refs/tags/v0.19.0",
          publish: true,
          results: {
            plan: {
              result: planResult,
              outputs: { "upload-cli": String(!held), "upload-sdk": "true" },
            },
            "build-cli-wheels": { result: buildResult, outputs: {} },
            "darwin-wheels": { result: buildResult, outputs: {} },
          },
        };
        const rows = resolveRun("release-pypi.yml", scenario, docs);
        assert.equal(
          rows.find((r) => r.job === "publish-cli").enabled,
          planResult === "success" && (held || buildResult === "success"),
          `${held}/${planResult}/${buildResult}`,
        );
      }
});

test("Linux archive transport wiring uses the real verified publisher journey", () => {
  // actions/download-artifact is GitHub's hosted transport. Its job wiring is
  // checked here; packaging tests drive the production publish command with
  // only the Forge transport doubled, including upload refusal.
  const job = workflows()["release-binaries.yml"].jobs["publish-archives"];
  assert.equal(job["runs-on"], "ubuntu-latest");
  const download = job.steps.find((s) =>
    s.uses?.startsWith("actions/download-artifact@"),
  );
  assert.equal(download.with.pattern, "archive-*");
  assert.equal(download.with["merge-multiple"], true);
  assert.match(
    job.steps.map((s) => s.run || "").join("\n"),
    /publish_release\.py --archive-only/,
  );
});
