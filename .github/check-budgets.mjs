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
for (const row of result.results) {
  if (row.verdict !== "within")
    console.error(`${row.id}: ${row.error || row.verdict}`);
}
if (result.results.some((r) => r.verdict !== "within")) process.exit(1);
