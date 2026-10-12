import { check } from "@onebudgetspec/sdk";
const result = await check({ paths: [".github/budgets.yaml"] });
console.log(
  JSON.stringify(
    result.results.map(({ id, actual, verdict, error }) => ({
      id,
      actual,
      verdict,
      error,
    })),
  ),
);
if (result.results?.some((r) => r.verdict !== "within")) process.exit(1);
