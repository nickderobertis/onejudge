//! The judge-only mode of a run, through the library's public run API: a
//! [`Plan`](onejudge::cli::Plan) built from a config file, given an in-memory
//! [`JudgeInput`] with `Plan::with_judge_input`, and driven by `run_plan` — the
//! same entry point `onejudge run --judge-only` reaches.
//!
//! The panel is two `oneharness` judges run by the real linked engine in
//! process; only the paid model is doubled, at the harness boundary, by
//! `onejudge-fake-harness`. Each judge's own oneharness config gives its harness
//! the answer it plays (`ONEJUDGE_FAKE_HARNESS_JUDGE_ANSWER`) and a log of every
//! prompt it was handed (`ONEJUDGE_FAKE_HARNESS_PROMPT_LOG`), so what each judge
//! received is read back from the harness boundary rather than from onejudge.
//! The worker side is wired the same way, so "no agent side ran" is an absence
//! the same journey shows can be a presence.
use std::path::{Path, PathBuf};

use onejudge::cli::{exit_code, run_plan, Config, Format, Plan, RunSummary};
use onejudge::{Decision, JudgeInput, JudgeValue, Role, TelemetryRole};
use onejudge_test_doubles::{self as doubles, support};

/// The bar every journey states: the persona and the completion criterion.
const PERSONA: &str = "A staff engineer reviewing a rollout plan.";
const DONE_WHEN: &str = "the plan names a rollback step";

/// The oneharness turn timeout that reaps a judge that never answers.
const SILENT_TIMEOUT_SECS: u32 = 1;

/// One journey's directory: a worktree holding the skill, the worker's and each
/// judge's oneharness config outside it, and the prompt logs the doubled
/// harnesses write.
struct Sandbox {
    root: PathBuf,
    worktree: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        // Neither the host's oneharness overrides nor its user file may decide
        // what the journey asserts; nextest runs each test in its own process.
        for (key, _) in std::env::vars() {
            if key.starts_with("ONEHARNESS_") {
                std::env::remove_var(key);
            }
        }
        let root = support::scratch_path(name);
        let _ = std::fs::remove_dir_all(&root);
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let config_home = root.join("config-home");
        std::fs::create_dir_all(&config_home).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        std::env::set_var("APPDATA", &config_home);
        std::env::set_var("HOME", root.join("home"));
        std::env::set_var("XDG_STATE_HOME", root.join("state"));
        std::fs::write(
            worktree.join("SKILL.md"),
            "---\nname: planner\n---\nWrite the plan.\n",
        )
        .unwrap();
        let sandbox = Self { root, worktree };
        sandbox.harness_config("worker", "pass");
        sandbox
    }

    /// The oneharness config `<who>.toml`, selecting the fake harness's `pinned`
    /// variant whose environment scripts `answer` and logs to `<who>.jsonl`.
    fn harness_config(&self, who: &str, answer: &str) -> PathBuf {
        let path = self.root.join(format!("{who}.toml"));
        std::fs::write(
            &path,
            format!(
                "harnesses = [\"claude-code:pinned\"]\n{timeout}history_dir = {:?}\n\n\
                 [harness.claude-code]\nbin = {:?}\n\
                 [harness.claude-code.variant.pinned.env]\n\
                 ONEJUDGE_FAKE_HARNESS_JUDGE_ANSWER = {answer:?}\n\
                 ONEJUDGE_FAKE_HARNESS_PROMPT_LOG = {:?}\n",
                self.root.join("history").display().to_string(),
                doubles::fake_harness(),
                self.log(who).display().to_string(),
                // Only the judge that never answers waits on the timeout that
                // reaps it; every other turn answers at once.
                timeout = if answer == "silent" {
                    format!("timeout = {SILENT_TIMEOUT_SECS}\n")
                } else {
                    String::new()
                },
            ),
        )
        .unwrap();
        path
    }

    fn log(&self, who: &str) -> PathBuf {
        self.root.join(format!("{who}.jsonl"))
    }

    /// Every prompt `who`'s harness was handed, with the side it judged it was.
    fn prompts(&self, who: &str) -> Vec<(String, String)> {
        std::fs::read_to_string(self.log(who))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                (
                    value["side"].as_str().unwrap().to_string(),
                    value["prompt"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn clear_logs(&self) {
        for who in ["worker", "alpha", "beta"] {
            let _ = std::fs::remove_file(self.log(who));
        }
    }

    /// The plan the one config file states: a split whose worker is the fake
    /// harness under `worker.toml`, judged by a panel of two oneharness judges
    /// answering `alpha` and `beta`.
    fn plan(&self, alpha: &str, beta: &str) -> Plan {
        let alpha = self.harness_config("alpha", alpha);
        let beta = self.harness_config("beta", beta);
        let yaml = format!(
            "provider:\n  kind: split\n  skill:\n    kind: oneharness\n    worker_config: {worker:?}\n  \
             judges:\n    - label: alpha\n      kind: oneharness\n      judge_config: {alpha:?}\n    \
             - label: beta\n      kind: oneharness\n      judge_config: {beta:?}\n\
             skill: {skill:?}\ntask: Write the rollout plan for the payments service.\n\
             session: judge-only\nuser:\n  persona: {PERSONA:?}\n  done_when: {DONE_WHEN:?}\n  max_turns: 1\n",
            worker = self.root.join("worker.toml").display().to_string(),
            alpha = alpha.display().to_string(),
            beta = beta.display().to_string(),
            skill = self.worktree.display().to_string(),
        );
        Config::from_yaml(&yaml).unwrap().into_plan().unwrap()
    }
}

fn judge_only(sandbox: &Sandbox, alpha: &str, beta: &str, input: &str) -> RunSummary {
    sandbox.clear_logs();
    let plan = sandbox
        .plan(alpha, beta)
        .with_judge_input(JudgeInput::text(input));
    run_plan(plan, Format::Json, &mut |_| {}).expect("the judge side decides")
}

/// The judge-side prompts `who` was handed in the last run.
fn judged(sandbox: &Sandbox, who: &str) -> Vec<String> {
    let prompts = sandbox.prompts(who);
    assert!(
        prompts.iter().all(|(side, _)| side == "judge"),
        "{who} was only ever asked to judge: {prompts:?}"
    );
    prompts.into_iter().map(|(_, prompt)| prompt).collect()
}

/// That no agent-side invocation was made: the worker's harness was never
/// started, and the report attributes no invocation to the agent.
fn assert_no_agent_side(sandbox: &Sandbox, summary: &RunSummary) {
    assert!(
        sandbox.prompts("worker").is_empty(),
        "the worker's harness ran: {:?}",
        sandbox.prompts("worker")
    );
    let telemetry = summary.report.telemetry.as_ref().expect("telemetry");
    assert!(
        telemetry
            .attribution
            .iter()
            .all(|invocation| invocation.role != TelemetryRole::Agent),
        "{:?}",
        telemetry.attribution
    );
    assert!(telemetry.agent.usage.is_none(), "{:?}", telemetry.agent);
    assert_eq!(summary.report.transcript.assistant_turns(), 1);
}

#[test]
fn a_panel_of_two_judges_judges_in_memory_material_once_with_no_agent_side() {
    let sandbox = Sandbox::new("judge-only-library");

    // The same plan, run normally, does start the worker — so the absence the
    // judge-only journeys assert below is one this wiring can show.
    let normal = run_plan(sandbox.plan("pass", "pass"), Format::Json, &mut |_| {}).unwrap();
    assert!(!sandbox.prompts("worker").is_empty(), "the worker ran");
    assert_eq!(exit_code(&normal), 0);

    let first =
        "# Rollout plan\n1. Deploy to canary.\n2. Roll back by reverting the release tag.\n";
    let summary = judge_only(&sandbox, "pass", "pass", first);
    assert_eq!(exit_code(&summary), 0, "{:?}", summary.report.verdicts);
    assert!(summary.completed);
    assert_no_agent_side(&sandbox, &summary);
    // Each judge was asked exactly once, and handed the material and the bar.
    for who in ["alpha", "beta"] {
        let prompts = judged(&sandbox, who);
        assert_eq!(prompts.len(), 1, "{who}: {prompts:?}");
        for needle in [first.trim_end(), PERSONA, DONE_WHEN] {
            assert!(prompts[0].contains(needle), "{who} lacks {needle:?}");
        }
    }
    let report = &summary.report;
    assert_eq!(report.transcript.messages[1].role, Role::Assistant);
    assert_eq!(report.transcript.messages[1].content, first);
    assert!(report.completion_reason.is_some());
    assert_eq!(report.verdicts.len(), 1);
    assert_eq!(report.verdicts[0].criterion, DONE_WHEN);
    assert_eq!(report.verdicts[0].verdict.value, JudgeValue::Bool(true));
    let decisions = &report.judge_decisions[0].decisions;
    assert_eq!(
        decisions
            .iter()
            .map(|d| (d.judge.as_str(), d.decision))
            .collect::<Vec<_>>(),
        [("alpha", Decision::Done), ("beta", Decision::Done)]
    );

    // A second input with different content reaches each judge with that content.
    let second = "# Rollout plan v2\nShip everything at once on Friday.\n";
    let summary = judge_only(&sandbox, "pass", "pass", second);
    assert_eq!(exit_code(&summary), 0);
    for who in ["alpha", "beta"] {
        let prompts = judged(&sandbox, who);
        assert_eq!(prompts.len(), 1, "{who}");
        assert!(prompts[0].contains(second.trim_end()), "{who}");
        assert!(!prompts[0].contains("Deploy to canary"), "{who}");
    }
    assert_no_agent_side(&sandbox, &summary);
}

#[test]
fn the_judge_whose_answer_fails_fails_the_run_and_is_named() {
    let sandbox = Sandbox::new("judge-only-library-fail");
    let finding = "Add a rollback step: the plan never says how to undo the deploy.";
    let summary = judge_only(
        &sandbox,
        "pass",
        &format!("fail:{finding}"),
        "# Rollout plan\nShip everything at once.\n",
    );
    assert_eq!(exit_code(&summary), 1);
    assert!(!summary.completed);
    assert_no_agent_side(&sandbox, &summary);
    let report = &summary.report;
    assert!(report.completion_reason.is_none());
    // Each judge's decision is attributed, and only beta's carries the finding.
    let decisions = &report.judge_decisions[0].decisions;
    let alpha = decisions.iter().find(|d| d.judge == "alpha").unwrap();
    let beta = decisions.iter().find(|d| d.judge == "beta").unwrap();
    assert_eq!(alpha.decision, Decision::Done);
    assert_eq!(beta.decision, Decision::Continue);
    assert!(beta.reason.contains(finding), "{}", beta.reason);
    // The verdict on the bar fails with the panel's message, which names beta.
    let verdict = &report.verdicts[0];
    assert_eq!(verdict.verdict.value, JudgeValue::Bool(false));
    assert!(verdict.verdict.reason.contains(finding), "{verdict:?}");
    assert!(verdict.verdict.reason.contains("beta"), "{verdict:?}");
    let handed_back = report.transcript.messages.last().unwrap();
    assert_eq!(handed_back.role, Role::User);
    assert!(handed_back.content.contains(finding));
}

#[test]
fn a_judge_that_never_answers_is_a_run_that_could_not_be_judged() {
    let sandbox = Sandbox::new("judge-only-library-silent");
    let plan = sandbox
        .plan("pass", "silent")
        .with_judge_input(JudgeInput::text("# Rollout plan\n"));
    let failure = onejudge::cli::run_plan_reporting_failure(plan, Format::Json, &mut |_| {})
        .err()
        .expect("a judge that never answered is never a verdict");
    assert!(
        failure.judge_decisions[0]
            .decisions
            .iter()
            .any(|d| d.judge == "beta" && d.decision == Decision::Error),
        "{:?}",
        failure.judge_decisions
    );
    assert!(sandbox.prompts("worker").is_empty());
}

#[test]
fn material_read_from_a_file_under_the_worktree_is_judged_as_written() {
    let sandbox = Sandbox::new("judge-only-library-file");
    let path = sandbox.worktree.join("plan.md");
    std::fs::write(&path, "# Plan from disk\nRollback: revert.\n").unwrap();
    sandbox.clear_logs();
    let plan = sandbox
        .plan("pass", "pass")
        .with_judge_input(JudgeInput::from_path(&path).unwrap());
    let summary = run_plan(plan, Format::Json, &mut |_| {}).unwrap();
    assert_eq!(exit_code(&summary), 0);
    assert!(judged(&sandbox, "alpha")[0].contains("# Plan from disk"));
    assert!(Path::new(&path).exists(), "the caller's file is left alone");
}
