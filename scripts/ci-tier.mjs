#!/usr/bin/env node
// Which gate tier a CI run is for, read from the GitHub event that started it.
//
//   node scripts/ci-tier.mjs >> "$GITHUB_OUTPUT"
//
// Prints `tier=…`, `base=…` and `flags=…` lines; the workflow passes `base` to the
// recipe as NX_BASE and `flags` as its arguments (`just check $flags`), so the
// tier is a flag on the one recipe and never a second implementation of the gate.
// `tier=skip` means the commit was already gated and the gate step does not run.
//
// onejudge batches releases: release-plz's release pull request accumulates every
// merge since the last release and auto-merges once green, so the commit that
// ships is one no merge job swept (AGENTS.md, "Commits, releases, and merging").
// So the broader tier runs on that pull request, and only there:
//
//   * a pull request from a `release-plz-` branch   -> the broader tier;
//   * any other pull request                       -> the affected tier, against
//     the merge base of the pull request's base commit and the checked-out head;
//   * a push to main                               -> the affected tier, against
//     the merge base of the commit the push replaced and the pushed head —
//     except a push of exactly release-plz's release commit, which is skipped:
//     its tree is the one its release pull request just swept, and its version
//     and changelog edits are root files, which would sweep it a second time;
//   * anything else (a dispatch, a schedule, a tag) -> the broader tier.
//
// A base that cannot be derived — a first push, a force-push whose old tip is not
// in the checkout, a shallow clone — fails closed into the broader tier and says
// so, rather than scoping a run against nothing. Every value printed is a git
// object name git itself resolved or a fixed word: nothing from the event payload
// reaches the output unvalidated.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const RELEASE_BRANCH_PREFIX = "release-plz-";
// The subject release-plz's release pull request squash-merges as.
const RELEASE_COMMIT = /^chore: release v\d+\.\d+\.\d+\S* \(#\d+\)$/;
const SHA = /^[0-9a-f]{40}([0-9a-f]{24})?$/;

function note(message) {
  console.error(`ci-tier: ${message}`);
}

function emit(tier, base, why) {
  note(`${tier} tier: ${why}`);
  console.log(`tier=${tier}`);
  console.log(`base=${base ?? ""}`);
  console.log(`flags=${tier === "sweep" ? "--sweep" : ""}`);
  process.exit(0);
}

/** The merge base of `sha` (from the event) and HEAD, or null when underivable. */
function mergeBase(sha) {
  if (typeof sha !== "string" || !SHA.test(sha) || /^0+$/.test(sha)) return null;
  try {
    return execFileSync("git", ["merge-base", sha, "HEAD"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return null;
  }
}

const name = process.env.GITHUB_EVENT_NAME ?? "";
let event = {};
try {
  event = JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH ?? "", "utf8"));
} catch {
  emit("sweep", null, `no readable event payload for '${name}', so nothing scopes the run`);
}

if (name === "pull_request") {
  const head = event.pull_request?.head?.ref ?? "";
  if (typeof head === "string" && head.startsWith(RELEASE_BRANCH_PREFIX)) {
    emit("sweep", null, "release-plz's release pull request: the commit that ships is swept here");
  }
  const base = mergeBase(event.pull_request?.base?.sha);
  if (!base) emit("sweep", null, "the pull request's base commit is not in this checkout (fetch-depth: 0)");
  emit("affected", base, `pull request, merge base ${base} with its base branch`);
}

if (name === "push") {
  const ref = event.ref ?? "";
  if (ref === "refs/heads/main") {
    const subject = String(event.head_commit?.message ?? "").split("\n")[0];
    if (RELEASE_COMMIT.test(subject) && Array.isArray(event.commits) && event.commits.length === 1) {
      emit("skip", null, `release-plz's release commit (${subject}) was swept on its release pull request`);
    }
    const base = mergeBase(event.before);
    if (!base) emit("sweep", null, "the commit this push replaced is not in this checkout");
    emit("affected", base, `push to main, merge base ${base} with the replaced tip`);
  }
}

emit("sweep", null, `'${name}' is neither a pull request nor a push to main`);
