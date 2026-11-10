//! `onejudge run --judge-only`, driven as a consumer drives it: the built binary
//! spawned with one config file that also drives a normal run in the same
//! journey, judged by a panel of one `oneharness` judge and one `llmlint` judge.
//!
//! Only the paid model is doubled. The harness judge runs through the real linked
//! oneharness engine into `onejudge-fake-harness`, whose answer and prompt log
//! come from the judge's own oneharness config (`ONEJUDGE_FAKE_HARNESS_*` in its
//! variant `env`); the llmlint judge is `onejudge-fake-llmlint`, which records its
//! argv and the content of every file it judged, and holds that content to one
//! fixture rule. Every `--format json` report is validated against the checked-in
//! `Report` schema the contract gate pins.
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use onejudge::{Decision, JudgeValue, Report, Role};
use onejudge_test_doubles::{self as doubles, support};

/// The bar the config states.
const PERSONA: &str = "A staff engineer reviewing a rollout plan.";
const DONE_WHEN: &str = "the plan names a rollback step";
/// The llmlint judge's fixture rule: `RULE:NEEDLE`.
const RULE: &str = "plan_has_no_placeholders";
const NEEDLE: &str = "TBD";
/// The oneharness turn timeout that reaps a judge that never answers.
const SILENT_TIMEOUT_SECS: u32 = 1;

/// The checked-in `Report` schema the contract gate holds `Report` to.
fn report_schema() -> onemessagebus::Registry {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../onejudge/tests/golden/report.schema-v{}.json",
        onejudge::SCHEMA_VERSION
    ));
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut registry = onemessagebus::Registry::new();
    registry
        .register_schema(schema_id(), schema)
        .expect("the golden is a JSON Schema");
    registry
}

fn schema_id() -> onemessagebus::SchemaId {
    format!("onejudge.report@{}", onejudge::SCHEMA_VERSION)
        .parse()
        .unwrap()
}

/// What one `onejudge run` produced.
struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// The `--format json` report, validated against the `Report` schema.
    fn report(&self) -> Report {
        let value: serde_json::Value = serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}\n{}", self.stdout, self.stderr));
        report_schema()
            .check(&schema_id(), &value)
            .unwrap_or_else(|e| panic!("the report does not validate: {e:?}\n{value:#}"));
        serde_json::from_value(value).unwrap()
    }
}

/// One journey's directory: the worktree the run is started in (holding the
/// config, the material and the llmlint config), and beside it each party's
/// oneharness config and every log a double writes.
struct Journey {
    root: PathBuf,
    worktree: PathBuf,
}

impl Journey {
    fn new(name: &str) -> Self {
        let root = support::scratch_path(name);
        let _ = std::fs::remove_dir_all(&root);
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(root.join("config-home")).unwrap();
        // The llmlint judge's own config: the fake reads no rule from it, but its
        // path on the recorded argv is what shows the judge ran under it.
        std::fs::write(worktree.join("llmlint.yml"), "rules: []\n").unwrap();
        let journey = Self { root, worktree };
        journey.harness_config("worker", "pass");
        journey.harness_config("reviewer", "pass");
        std::fs::write(journey.worktree.join("onejudge.yaml"), journey.yaml()).unwrap();
        journey
    }

    fn path(&self, name: &str) -> String {
        self.root.join(name).display().to_string()
    }

    /// The oneharness config `<who>.toml`: the fake harness, whose variant env
    /// scripts `answer` and logs every prompt to `<who>.jsonl`.
    fn harness_config(&self, who: &str, answer: &str) {
        let timeout = if answer == "silent" {
            format!("timeout = {SILENT_TIMEOUT_SECS}\n")
        } else {
            String::new()
        };
        std::fs::write(
            self.root.join(format!("{who}.toml")),
            format!(
                "harnesses = [\"claude-code:pinned\"]\n{timeout}history_dir = {:?}\n\n\
                 [harness.claude-code]\nbin = {:?}\n\
                 [harness.claude-code.variant.pinned.env]\n\
                 ONEJUDGE_FAKE_HARNESS_JUDGE_ANSWER = {answer:?}\n\
                 ONEJUDGE_FAKE_HARNESS_PROMPT_LOG = {:?}\n",
                self.path("history"),
                doubles::fake_harness(),
                self.path(&format!("{who}.jsonl")),
            ),
        )
        .unwrap();
    }

    /// The one config file every run in the journey reads.
    fn yaml(&self) -> String {
        format!(
            "provider:\n  kind: split\n  skill:\n    kind: oneharness\n    worker_config: {worker:?}\n  \
             judges:\n    - label: reviewer\n      kind: oneharness\n      judge_config: {reviewer:?}\n    \
             - label: lint\n      kind: llmlint\n      bin: {llmlint:?}\n      config: llmlint.yml\n\
             task: Write the rollout plan for the payments service.\nsession: judge-only-cli\n\
             user:\n  persona: {PERSONA:?}\n  done_when: {DONE_WHEN:?}\n  max_turns: 1\n",
            worker = self.path("worker.toml"),
            reviewer = self.path("reviewer.toml"),
            llmlint = doubles::fake_llmlint(),
        )
    }

    /// Run `onejudge run <args>` in the worktree with `stdin`, the doubles' logs
    /// cleared first.
    fn run(&self, args: &[&str], stdin: &str) -> Ran {
        self.run_with(args, stdin, &[])
    }

    /// [`Journey::run`], with `env` set over the journey's own.
    fn run_with(&self, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Ran {
        for log in ["worker", "reviewer", "llmlint-argv", "llmlint-seen"] {
            let _ = std::fs::remove_file(self.root.join(format!("{log}.jsonl")));
        }
        let mut command = Command::new(doubles::onejudge_cli());
        command
            .arg("run")
            .args(args)
            .args(["--format", "json"])
            .current_dir(&self.worktree)
            .env("XDG_CONFIG_HOME", self.root.join("config-home"))
            .env("APPDATA", self.root.join("config-home"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env(
                "ONEJUDGE_FAKE_LLMLINT_ARGV",
                self.path("llmlint-argv.jsonl"),
            )
            .env(
                "ONEJUDGE_FAKE_LLMLINT_SEEN",
                self.path("llmlint-seen.jsonl"),
            )
            .env("ONEJUDGE_FAKE_LLMLINT_RULE", format!("{RULE}:{NEEDLE}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Hermetic: the host's oneharness overrides are a config layer above every
        // file, so they would decide what the journey asserts.
        for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("ONEHARNESS_")) {
            command.env_remove(key);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        Ran {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn lines(&self, log: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.root.join(format!("{log}.jsonl")))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Every prompt `who`'s harness was handed.
    fn prompts(&self, who: &str) -> Vec<String> {
        self.lines(who)
            .iter()
            .map(|line| line["prompt"].as_str().unwrap().to_string())
            .collect()
    }

    /// Every `lint` run the llmlint judge made.
    fn lint_runs(&self) -> Vec<Vec<String>> {
        self.lines("llmlint-argv")
            .into_iter()
            .map(|line| serde_json::from_value::<Vec<String>>(line).unwrap())
            .filter(|argv| argv.first().map(String::as_str) == Some("lint"))
            .collect()
    }

    /// The content of every file the llmlint judge judged.
    fn linted(&self) -> Vec<String> {
        self.lines("llmlint-seen")
            .iter()
            .map(|line| line["text"].as_str().unwrap().to_string())
            .collect()
    }
}

/// The value following `flag` in `argv`.
fn value_of<'a>(argv: &'a [String], flag: &str) -> &'a str {
    let at = argv.iter().position(|arg| arg == flag).unwrap();
    &argv[at + 1]
}

/// The judge-only run asserts common to every verdict: one decision per judge,
/// the harness judge handed the material and the bar, the llmlint judge's one
/// run over that same material under its own config, and no agent side.
fn assert_judged_once(journey: &Journey, report: &Report, material: &str) {
    assert!(
        journey.prompts("worker").is_empty(),
        "no agent-side invocation: {:?}",
        journey.prompts("worker")
    );
    let telemetry = report.telemetry.as_ref().unwrap();
    assert!(
        telemetry
            .attribution
            .iter()
            .all(|invocation| invocation.role != onejudge::TelemetryRole::Agent),
        "{:?}",
        telemetry.attribution
    );
    assert_eq!(report.transcript.assistant_turns(), 1);
    assert_eq!(report.transcript.messages[1].role, Role::Assistant);
    assert_eq!(report.transcript.messages[1].content, material);

    let prompts = journey.prompts("reviewer");
    assert_eq!(prompts.len(), 1, "the harness judge decided once");
    for needle in [material.trim_end(), PERSONA, DONE_WHEN] {
        assert!(prompts[0].contains(needle), "the judge lacks {needle:?}");
    }

    let runs = journey.lint_runs();
    assert_eq!(runs.len(), 1, "the llmlint judge ran once: {runs:?}");
    let config = Path::new(value_of(&runs[0], "-c"));
    assert_eq!(
        std::fs::canonicalize(config).unwrap(),
        std::fs::canonicalize(journey.worktree.join("llmlint.yml")).unwrap(),
        "under its own config"
    );
    assert!(!runs[0].iter().any(|arg| arg == "--diff"), "{:?}", runs[0]);
    assert_eq!(journey.linted(), [material], "it judged that same input");

    assert_eq!(report.judge_decisions.len(), 1);
    let judges: Vec<&str> = report.judge_decisions[0]
        .decisions
        .iter()
        .map(|decision| decision.judge.as_str())
        .collect();
    assert_eq!(judges, ["reviewer", "lint"]);
}

fn decision<'a>(report: &'a Report, judge: &str) -> &'a onejudge::JudgeDecision {
    report.judge_decisions[0]
        .decisions
        .iter()
        .find(|decision| decision.judge == judge)
        .unwrap()
}

#[test]
fn one_config_judges_material_with_its_panel_and_exits_as_a_normal_run_would() {
    let journey = Journey::new("judge-only-cli");
    let plan = "# Rollout plan\n1. Deploy to canary.\n2. Roll back by reverting the release tag.\n";
    std::fs::write(journey.worktree.join("plan.md"), plan).unwrap();
    let judge_only = ["onejudge.yaml", "--judge-only", "--judge-input", "plan.md"];

    // The same config, run normally: the worker runs, every judge passes, exit 0.
    let normal = journey.run(&["onejudge.yaml"], "");
    assert_eq!(normal.code, Some(0), "{}", normal.stderr);
    let normal_report = normal.report();
    assert!(!journey.prompts("worker").is_empty(), "the worker ran");
    assert!(normal_report
        .telemetry
        .as_ref()
        .unwrap()
        .attribution
        .iter()
        .any(|invocation| invocation.role == onejudge::TelemetryRole::Agent));

    // Pass: the file under the worktree, linted where it lies.
    let passed = journey.run(&judge_only, "");
    assert_eq!(passed.code, normal.code, "{}", passed.stderr);
    let report = passed.report();
    assert_judged_once(&journey, &report, plan);
    let run = &journey.lint_runs()[0];
    assert_eq!(
        Path::new(value_of(run, "--cwd")).canonicalize().unwrap(),
        journey.worktree.canonicalize().unwrap()
    );
    assert_eq!(
        Path::new(run.last().unwrap()).canonicalize().unwrap(),
        journey.worktree.join("plan.md").canonicalize().unwrap()
    );
    assert!(report.completion_reason.is_some());
    assert_eq!(report.verdicts.len(), 1);
    assert_eq!(report.verdicts[0].criterion, DONE_WHEN);
    assert_eq!(report.verdicts[0].verdict.value, JudgeValue::Bool(true));
    assert_eq!(decision(&report, "reviewer").decision, Decision::Done);
    assert_eq!(decision(&report, "lint").decision, Decision::Done);

    // A fail from the harness judge: its doubled answer changed, naming its
    // finding. A normal run whose judge gives that answer exits 1 too.
    let finding = "Name who approves the canary before it widens.";
    journey.harness_config("reviewer", &format!("fail:{finding}"));
    let normal_failed = journey.run(&["onejudge.yaml"], "");
    assert_eq!(normal_failed.code, Some(1), "{}", normal_failed.stderr);
    normal_failed.report();
    let failed = journey.run(&judge_only, "");
    assert_eq!(failed.code, normal_failed.code, "{}", failed.stderr);
    let report = failed.report();
    assert_judged_once(&journey, &report, plan);
    assert!(report.completion_reason.is_none());
    assert_eq!(decision(&report, "reviewer").decision, Decision::Continue);
    assert!(decision(&report, "reviewer").reason.contains(finding));
    assert_eq!(decision(&report, "lint").decision, Decision::Done);
    let verdict = &report.verdicts[0].verdict;
    assert_eq!(verdict.value, JudgeValue::Bool(false));
    assert!(
        verdict.reason.contains("## Judge `reviewer`") && verdict.reason.contains(finding),
        "{}",
        verdict.reason
    );

    // A fail from the llmlint judge alone: the input — read from stdin this time
    // — violates its fixture rule while the harness judge passes.
    journey.harness_config("reviewer", "pass");
    let placeholder = format!("# Rollout plan\n1. Deploy to canary.\n2. Rollback: {NEEDLE}.\n");
    let mut stdin_args = judge_only.to_vec();
    stdin_args[3] = "-";
    let linted = journey.run(&stdin_args, &placeholder);
    assert_eq!(linted.code, Some(1), "{}", linted.stderr);
    let report = linted.report();
    assert_judged_once(&journey, &report, &placeholder);
    assert_eq!(decision(&report, "reviewer").decision, Decision::Done);
    assert_eq!(decision(&report, "lint").decision, Decision::Continue);
    let verdict = &report.verdicts[0].verdict;
    assert_eq!(verdict.value, JudgeValue::Bool(false));
    assert!(
        verdict.reason.contains("## Judge `lint`")
            && verdict.reason.contains(RULE)
            && verdict.reason.contains(NEEDLE)
            && !verdict.reason.contains("## Judge `reviewer`"),
        "{}",
        verdict.reason
    );
    // Not a file under the worktree, so linted from a scratch root of its own,
    // under the name a linter's globs see — and that root is gone afterwards.
    let run = &journey.lint_runs()[0];
    let scratch = PathBuf::from(value_of(run, "--cwd"));
    assert_ne!(
        scratch.canonicalize().ok(),
        journey.worktree.canonicalize().ok()
    );
    assert_eq!(
        Path::new(run.last().unwrap()).file_name().unwrap(),
        onejudge::DEFAULT_JUDGE_INPUT_NAME
    );
    assert!(!scratch.exists(), "the scratch root is removed");

    // A harness judge that never answers: a normal run cannot be judged and exits
    // 2, and so does the judge-only run — never a verdict.
    journey.harness_config("reviewer", "silent");
    let normal_silent = journey.run(&["onejudge.yaml"], "");
    assert_eq!(normal_silent.code, Some(2), "{}", normal_silent.stderr);
    let silent = journey.run(&judge_only, "");
    assert_eq!(silent.code, normal_silent.code, "{}", silent.stderr);
    assert!(journey.prompts("worker").is_empty());
    let failure: serde_json::Value = serde_json::from_str(&silent.stdout).unwrap();
    assert_eq!(failure["schema_version"], onejudge::SCHEMA_VERSION);
}

/// Adoptable by configuration alone: a repository of its own — a directory
/// holding nothing but its own `onejudge.yaml` (found by the default name), its
/// own bar, and its own harness routing — runs judge-only material through its
/// own panel, with an environment carrying nothing from the host but how to
/// find programs and scratch space.
#[test]
fn a_repository_of_its_own_adopts_judge_only_by_configuration_alone() {
    let root = support::scratch_path("judge-only-adopted");
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("design-repo");
    std::fs::create_dir_all(repo.join(".config")).unwrap();
    std::fs::create_dir_all(repo.join("routing")).unwrap();
    let prompts = repo.join("design-judge.jsonl");
    std::fs::write(
        repo.join("routing/design-judge.toml"),
        format!(
            "harnesses = [\"claude-code:design-review\"]\nhistory = false\n\n\
             [harness.claude-code]\nbin = {:?}\n\
             [harness.claude-code.variant.design-review.env]\n\
             ONEJUDGE_FAKE_HARNESS_JUDGE_ANSWER = \"pass\"\n\
             ONEJUDGE_FAKE_HARNESS_PROMPT_LOG = {:?}\n",
            doubles::fake_harness(),
            prompts.display().to_string(),
        ),
    )
    .unwrap();
    let persona = "A principal engineer who signs off design documents.";
    let bar = "the design document states its non-goals";
    std::fs::write(
        repo.join("onejudge.yaml"),
        format!(
            "provider:\n  kind: split\n  skill:\n    kind: oneharness\n  judges:\n    \
             - label: design-judge\n      kind: oneharness\n      judge_config: routing/design-judge.toml\n\
             task: Write the design document for the export pipeline.\nsession: design-review\n\
             user:\n  persona: {persona:?}\n  done_when: {bar:?}\n"
        ),
    )
    .unwrap();
    let document = "# Export pipeline\n## Non-goals\nNo real-time export.\n";

    let mut command = Command::new(doubles::onejudge_cli());
    command
        .args([
            "run",
            "--judge-only",
            "--judge-input",
            "-",
            "--format",
            "json",
        ])
        .current_dir(&repo)
        .env_clear();
    for keep in ["PATH", "SYSTEMROOT", "SystemRoot", "TMPDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(keep) {
            command.env(keep, value);
        }
    }
    command
        .env("HOME", &repo)
        .env("XDG_CONFIG_HOME", repo.join(".config"))
        .env("APPDATA", repo.join(".config"))
        .env("XDG_STATE_HOME", repo.join(".state"))
        // Windows resolves oneharness's session store under %LOCALAPPDATA%,
        // not XDG_STATE_HOME; with the host's cleared it has no store at all.
        .env("LOCALAPPDATA", repo.join(".state"))
        .env("USERPROFILE", &repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(document.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let ran = Ran {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let report = ran.report();
    assert_eq!(report.transcript.messages[1].content, document);
    assert_eq!(report.verdicts[0].criterion, bar);
    assert_eq!(report.verdicts[0].verdict.value, JudgeValue::Bool(true));
    assert_eq!(
        report.judge_decisions[0].decisions[0].judge, "design-judge",
        "{:?}",
        report.judge_decisions
    );
    let handed: Vec<String> = std::fs::read_to_string(&prompts)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .map(|line| line["prompt"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(handed.len(), 1);
    for needle in [document.trim_end(), persona, bar] {
        assert!(handed[0].contains(needle), "the judge lacks {needle:?}");
    }
}

#[test]
fn judge_only_and_its_input_each_need_the_other() {
    for args in [&["--judge-only"][..], &["--judge-input", "plan.md"][..]] {
        let output = Command::new(doubles::onejudge_cli())
            .arg("run")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("--judge-"), "{stderr}");
    }
}

#[test]
fn an_unreadable_judge_input_is_a_run_that_could_not_be_judged() {
    let journey = Journey::new("judge-only-cli-unreadable");
    let ran = journey.run(
        &[
            "onejudge.yaml",
            "--judge-only",
            "--judge-input",
            "no-such-plan.md",
        ],
        "",
    );
    assert_eq!(ran.code, Some(2));
    assert!(
        ran.stderr
            .contains("could not read --judge-input `no-such-plan.md`"),
        "{}",
        ran.stderr
    );
    assert!(journey.prompts("reviewer").is_empty());
}

#[test]
fn an_llmlint_run_that_judged_no_rule_against_the_material_is_never_a_pass() {
    let journey = Journey::new("judge-only-cli-vacuous");
    std::fs::write(journey.worktree.join("plan.md"), "# Plan\n").unwrap();
    // llmlint exits clean having judged nothing: no rule's globs selected the file.
    let ran = journey.run_with(
        &["onejudge.yaml", "--judge-only", "--judge-input", "plan.md"],
        "",
        &[
            ("ONEJUDGE_FAKE_LLMLINT_RULE", ""),
            (
                "ONEJUDGE_FAKE_LLMLINT_STDOUT",
                "0 failed, 0 passed, 3 skipped, 0 not relevant\n",
            ),
        ],
    );
    assert_eq!(ran.code, Some(2), "{}", ran.stdout);
    assert!(
        ran.stderr.contains("llmlint judged no rule against"),
        "{}",
        ran.stderr
    );
}

#[test]
fn material_off_the_worktree_needs_the_llmlint_judge_to_have_its_own_config() {
    let journey = Journey::new("judge-only-cli-no-llmlint-config");
    let yaml = journey.yaml().replace("      config: llmlint.yml\n", "");
    std::fs::write(journey.worktree.join("unconfigured.yaml"), yaml).unwrap();
    let ran = journey.run(
        &["unconfigured.yaml", "--judge-only", "--judge-input", "-"],
        "# Plan\n",
    );
    assert_eq!(ran.code, Some(2), "{}", ran.stdout);
    assert!(
        ran.stderr.contains("Give the judge its own `config:`"),
        "{}",
        ran.stderr
    );
    assert!(journey.lint_runs().is_empty(), "refused before llmlint ran");
}

#[test]
fn the_task_and_the_judge_input_cannot_both_be_read_from_stdin() {
    let journey = Journey::new("judge-only-cli-two-stdins");
    let ran = journey.run(
        &[
            "onejudge.yaml",
            "--judge-only",
            "--judge-input",
            "-",
            "--task",
            "-",
        ],
        "# Plan\n",
    );
    assert_eq!(ran.code, Some(2), "{}", ran.stdout);
    assert!(
        ran.stderr
            .contains("--task - and --judge-input - both read stdin"),
        "{}",
        ran.stderr
    );
    assert!(journey.prompts("reviewer").is_empty(), "nothing was judged");
}

#[test]
fn material_judged_with_no_task_configured_answers_the_default_task() {
    let journey = Journey::new("judge-only-cli-no-task");
    let yaml = journey.yaml().replace(
        "task: Write the rollout plan for the payments service.\n",
        "",
    );
    assert!(!yaml.contains("task:"), "{yaml}");
    std::fs::write(journey.worktree.join("taskless.yaml"), yaml).unwrap();
    std::fs::write(
        journey.worktree.join("plan.md"),
        "# Plan\nRollback: revert.\n",
    )
    .unwrap();
    let ran = journey.run(
        &["taskless.yaml", "--judge-only", "--judge-input", "plan.md"],
        "",
    );
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let report = ran.report();
    let task = onejudge::cli::JUDGE_ONLY_TASK;
    assert_eq!(report.transcript.messages[0].role, Role::User);
    assert_eq!(report.transcript.messages[0].content, task);
    assert!(journey.prompts("reviewer")[0].contains(task));
    // The same config, run normally, still needs a task: nothing to work on.
    let normal = journey.run(&["taskless.yaml"], "");
    assert_eq!(normal.code, Some(2));
    assert!(normal.stderr.contains("no task given"), "{}", normal.stderr);
}
