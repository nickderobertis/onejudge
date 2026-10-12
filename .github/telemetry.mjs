// Count only already-recorded jobs; this never evaluates workflows or tolerances.
export const TELEMETRY_SCHEMA_VERSION = 2;
export function recordingMeasures(
  scenarios,
  workflowNames,
  lifecycleScenarios,
) {
  const byId = new Map(scenarios.map((scenario) => [scenario.id, scenario]));
  if (
    byId.size !== scenarios.length ||
    new Set(workflowNames).size !== workflowNames.length ||
    new Set(lifecycleScenarios).size !== lifecycleScenarios.length
  )
    throw new Error(
      "invalid workflow telemetry: duplicate recording identifiers",
    );
  for (const scenario of scenarios)
    if (scenario.rows.some((row) => !workflowNames.includes(row.runWorkflow)))
      throw new Error("invalid workflow telemetry: unregistered run workflow");
  const perRun = scenarios.flatMap((scenario) =>
    workflowNames.map((workflow) => ({
      scenario: scenario.id,
      workflow,
      jobs: scenario.rows.filter(
        (row) => row.runWorkflow === workflow && row.macos,
      ).length,
    })),
  );
  const lifecycle = lifecycleScenarios.flatMap((id) => {
    const scenario = byId.get(id);
    if (!scenario || scenario.diagnostic)
      throw new Error(
        "invalid workflow telemetry: missing or diagnostic lifecycle scenario",
      );
    return [
      ...new Set(
        scenario.rows.filter((row) => row.macos).map((row) => row.runWorkflow),
      ),
    ].map((workflow) => ({
      scenario: id,
      workflow,
      rows: scenario.rows.filter(
        (row) => row.runWorkflow === workflow && row.macos,
      ),
    }));
  });
  return {
    perRun,
    lifecycle,
    jobsPerRun: Math.max(0, ...perRun.map((run) => run.jobs)),
    runsPerChange: lifecycle.length,
  };
}
