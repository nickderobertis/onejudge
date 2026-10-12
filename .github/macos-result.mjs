#!/usr/bin/env node
// A required Linux context reports precisely whether the Mac lane was owed and met.
const [policyResult, macResult, required] = process.argv.slice(2);
if (policyResult !== "success" || !["true", "false"].includes(required)) {
  throw new Error(
    "Mac classification failed/cancelled/missing; omission is unauthorized",
  );
}
if (required === "true" && macResult === "success") {
  console.log("Required macOS assertions passed.");
} else if (required === "false" && macResult === "skipped") {
  console.log(
    "macOS assertions intentionally omitted by semantic event classification.",
  );
} else {
  throw new Error(
    `Required macOS work did not complete: required=${required}, result=${macResult}`,
  );
}
