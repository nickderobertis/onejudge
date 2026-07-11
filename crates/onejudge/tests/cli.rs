//! End-to-end coverage for the `onejudge` CLI. Two complementary layers, neither
//! mocked beyond the model:
//!
//! * **In-process** — drive [`onejudge::cli::run_plan`] over a `command` provider
//!   pointed at the `onejudge-echo-provider` test double, so the whole run driver
//!   (converse loop, `done_when` re-judge, evals, summary, exit code) runs for
//!   real inside the test process.
//! * **Subprocess** — spawn the built `onejudge` binary against a YAML config that
//!   points at the same double, asserting on stdout, the `--format json`
//!   [`Report`](onejudge::Report), and the process exit code — the true CLI
//!   surface, only the model faked, exactly as `tests/e2e.rs` does for the engine.
//!
//! Gated on `cli` + `fake-provider`: the binary needs `cli`, the double needs
//! `fake-provider`. The Linux `check` gate enables both, so these always run.
#![cfg(all(feature = "cli", feature = "fake-provider"))]

use std::path::Path;
use std::process::Command;

use onejudge::cli::{exit_code, render_human, run_plan, Config, Format};

/// The built echo test double's path (a `CommandProvider` backend).
fn echo_bin() -> String {
    env!("CARGO_BIN_EXE_onejudge-echo-provider").to_string()
}

/// The built `onejudge` binary under test.
fn onejudge_bin() -> &'static str {
    env!("CARGO_BIN_EXE_onejudge")
}

/// The built fake-oneharness double (an `OneharnessProvider` backend).
fn fake_oneharness_bin() -> String {
    env!("CARGO_BIN_EXE_onejudge-fake-oneharness").to_string()
}

/// A config whose `command` provider is the echo double, with `body` appended.
/// The binary path is JSON-encoded into the YAML flow list so a Windows path
/// (backslashes, a drive-letter colon) stays a valid scalar cross-platform.
fn config_yaml(body: &str) -> String {
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    format!("provider:\n  kind: command\n  command: [{echo}]\nharness: claude-code\n{body}")
}

/// Build a plan from `body` and drive it in-process (no progress sink needed).
fn plan_from(body: &str) -> onejudge::cli::RunSummary {
    let cfg = Config::from_yaml(&config_yaml(body)).unwrap();
    let plan = cfg.into_plan().unwrap();
    let mut sink = |_: &str| {};
    run_plan(plan, Format::Json, &mut sink).unwrap()
}

// --- In-process: the run driver over the real echo subprocess ---------------

#[test]
fn completed_run_with_passing_evals_exits_zero() {
    // The agent commits on turn 1; the echo judge sees the `git commit` event in
    // the transcript, so `done_when` holds and the loop ends after one turn.
    let body = "\
task: please commit
agent:
  instructions: 'Commit it. [[event:git commit -m fix]]'
user:
  persona: A tester.
  done_when: git commit
  max_turns: 5
evals:
  - criterion: echo
    kind: boolean
  - criterion: please
    kind: numeric
    scale: [1, 5]
";
    let summary = plan_from(body);
    assert!(summary.completed);
    assert!(!summary.hit_max_turns);
    assert_eq!(summary.report.transcript.assistant_turns(), 1);
    assert_eq!(exit_code(&summary), 0);

    // The done_when + both evals are recorded as verdicts in the report.
    assert_eq!(summary.report.verdicts.len(), 3);
    // The boolean eval "echo" matched (the reply is "echo: please commit").
    let echo_eval = summary
        .eval_results
        .iter()
        .find(|r| r.criterion == "echo")
        .unwrap();
    assert_eq!(echo_eval.passed, Some(true));
    // The numeric eval scored the top of its scale (the criterion matched).
    let numeric = summary
        .eval_results
        .iter()
        .find(|r| r.criterion == "please")
        .unwrap();
    assert_eq!(numeric.score, Some(5.0));

    // The human rendering reflects completion.
    let rendered = render_human(&summary);
    assert!(rendered.contains("Status: completed"));
    assert!(rendered.contains("[PASS] echo"));
}

#[test]
fn incomplete_run_hits_max_turns_and_exits_one() {
    // `done_when` never matches the echoed transcript, so the loop runs to the cap
    // and the end-of-run re-judge reports the task incomplete.
    let body = "\
task: keep going
agent:
  instructions: Be helpful.
user:
  persona: A tester.
  done_when: deploy to production
  max_turns: 2
";
    let summary = plan_from(body);
    assert!(!summary.completed);
    assert!(summary.hit_max_turns);
    assert_eq!(summary.report.transcript.assistant_turns(), 2);
    assert_eq!(exit_code(&summary), 1);
    assert!(render_human(&summary).contains("hit the turn cap (2)"));
}

#[test]
fn failing_boolean_eval_fails_an_otherwise_complete_run() {
    // The task completes, but a boolean eval that cannot match the transcript
    // fails — so the run exits non-zero (evals gate the exit code).
    let body = "\
task: say hi
agent:
  instructions: Be helpful.
user:
  persona: A tester.
  done_when: echo
  max_turns: 3
evals:
  - criterion: deployed to production
    kind: boolean
";
    let summary = plan_from(body);
    assert!(summary.completed);
    let failed = &summary.eval_results[0];
    assert_eq!(failed.passed, Some(false));
    assert_eq!(exit_code(&summary), 1);
}

#[test]
fn single_turn_run_without_a_user_completes() {
    let body = "\
task: greet me
agent:
  instructions: Be warm.
";
    let summary = plan_from(body);
    assert!(summary.completed);
    assert!(summary.done_when.is_none());
    assert_eq!(summary.report.transcript.assistant_turns(), 1);
    assert_eq!(exit_code(&summary), 0);
}

#[test]
fn oneharness_provider_kind_drives_the_loop() {
    // The `oneharness` provider kind, pointed at the fake-oneharness double, driven
    // in Human format so the streaming dispatch arm runs. The agent's reply
    // satisfies `done_when` on turn one.
    let bin = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    let yaml = format!(
        "provider:\n  kind: oneharness\n  bin: {bin}\nharness: claude-code\n\
         task: go\n\
         agent:\n  instructions: '[[reply:the task is complete]]'\n\
         user:\n  persona: A tester.\n  done_when: complete\n  max_turns: 3\n",
    );
    let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();
    let mut sink = |_: &str| {};
    let summary = run_plan(plan, Format::Human, &mut sink).unwrap();
    assert!(summary.completed);
    assert_eq!(summary.report.transcript.assistant_turns(), 1);
    assert_eq!(exit_code(&summary), 0);
}

#[test]
fn split_provider_kind_composes_two_backends() {
    // `split`: the agent runs on the fake oneharness, the judge / simulated user on
    // the echo command double. No `done_when`, so the loop runs to the cap — which
    // exercises the split's respond (skill) + simulate_user (judge) dispatch.
    let oh = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let yaml = format!(
        "provider:\n  kind: split\n  skill:\n    kind: oneharness\n    bin: {oh}\n  \
         judge:\n    kind: command\n    command: [{echo}]\n\
         harness: claude-code\n\
         task: start\n\
         agent:\n  instructions: '[[reply:working]]'\n\
         user:\n  persona: A tester.\n  max_turns: 2\n",
    );
    let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();
    let mut sink = |_: &str| {};
    let summary = run_plan(plan, Format::Human, &mut sink).unwrap();
    assert_eq!(summary.report.transcript.assistant_turns(), 2);
    assert!(summary.hit_max_turns);
    assert_eq!(exit_code(&summary), 1);
    // The agent turns came from the oneharness skill backend (its `[[reply]]`).
    assert_eq!(summary.report.transcript.messages[1].content, "working");
}

#[test]
fn oneharness_kind_json_covers_buffered_respond_and_user() {
    // JSON format runs buffered (not streaming), and with no `done_when` the loop
    // reaches the cap — so this exercises the buffered `respond` + `simulate_user`
    // dispatch arms of an `AnyProvider::Oneharness` (the streaming/human test does
    // not).
    let bin = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    let yaml = format!(
        "provider:\n  kind: oneharness\n  bin: {bin}\nharness: claude-code\n\
         task: go\n\
         agent:\n  instructions: '[[reply:working]]'\n\
         user:\n  persona: A tester.\n  max_turns: 2\n",
    );
    let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();
    let mut sink = |_: &str| {};
    let summary = run_plan(plan, Format::Json, &mut sink).unwrap();
    assert_eq!(summary.report.transcript.assistant_turns(), 2);
    assert!(summary.hit_max_turns);
}

#[test]
fn split_kind_json_covers_buffered_respond_and_judge() {
    // JSON (buffered) + an eval, so the split's buffered `respond` (skill) and
    // `judge` (judge backend) dispatch arms both run.
    let oh = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let yaml = format!(
        "provider:\n  kind: split\n  skill:\n    kind: oneharness\n    bin: {oh}\n  \
         judge:\n    kind: command\n    command: [{echo}]\n\
         harness: claude-code\n\
         task: start\n\
         agent:\n  instructions: '[[reply:working]]'\n\
         user:\n  persona: A tester.\n  max_turns: 2\n\
         evals:\n  - criterion: working\n    kind: boolean\n",
    );
    let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();
    let mut sink = |_: &str| {};
    let summary = run_plan(plan, Format::Json, &mut sink).unwrap();
    assert_eq!(summary.report.transcript.assistant_turns(), 2);
    // The echo judge scored the "working" criterion against the transcript.
    assert_eq!(summary.eval_results[0].passed, Some(true));
}

// --- Subprocess: the real `onejudge` binary --------------------------------

/// Write `body`'s config to a file under the integration-test tmp dir.
fn write_config(name: &str, body: &str) -> std::path::PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join(name);
    std::fs::write(&path, config_yaml(body)).unwrap();
    path
}

#[test]
fn binary_run_prints_human_result_and_exits_zero() {
    let config = write_config(
        "human.yaml",
        "\
task: please commit
agent:
  instructions: 'Commit it. [[event:git commit -m fix]]'
user:
  persona: A tester.
  done_when: git commit
  max_turns: 5
",
    );
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success(), "expected exit 0");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("=== Conversation ==="));
    assert!(stdout.contains("Status: completed"));
    // Live tool events stream to stderr, keeping stdout clean.
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("git commit"), "events stream to stderr");
}

#[test]
fn binary_run_json_emits_the_versioned_report() {
    let config = write_config(
        "json.yaml",
        "\
task: please commit
agent:
  instructions: 'Commit it. [[event:git commit -m fix]]'
user:
  persona: A tester.
  done_when: git commit
  max_turns: 5
evals:
  - criterion: echo
    kind: boolean
",
    );
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // The stdout is the versioned Report contract — parse it back.
    let report: onejudge::Report = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report.schema_version, onejudge::SCHEMA_VERSION);
    assert!(!report.verdicts.is_empty());
    assert_eq!(report.transcript.assistant_turns(), 1);
}

#[test]
fn binary_run_exits_one_when_incomplete() {
    let config = write_config(
        "incomplete.yaml",
        "\
task: keep going
agent:
  instructions: Be helpful.
user:
  persona: A tester.
  done_when: deploy to production
  max_turns: 2
",
    );
    let status = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}

#[test]
fn binary_run_task_override_and_stdin() {
    // `--task -` reads the task from stdin; flags win over the file's task.
    let config = write_config(
        "stdin.yaml",
        "\
task: from the file
agent:
  instructions: Be helpful.
",
    );
    let mut child = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--task", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"from stdin\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("from stdin"));
    assert!(!stdout.contains("from the file"));
}

#[test]
fn binary_reports_a_bad_config_and_exits_two() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join("bad.yaml");
    std::fs::write(&path, "task: x\nnot_a_key: 1\n").unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("config error"));
}

#[test]
fn binary_init_writes_a_starter_config() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join("init-out.yaml");
    let _ = std::fs::remove_file(&path);
    let status = Command::new(onejudge_bin())
        .args(["init", path.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());
    let written = std::fs::read_to_string(&path).unwrap();
    // The written starter is itself a valid config.
    assert!(Config::from_yaml(&written).is_ok());

    // A second init without --force refuses to clobber.
    let status = Command::new(onejudge_bin())
        .args(["init", path.to_str().unwrap()])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn binary_run_writes_json_to_an_output_file() {
    let config = write_config(
        "out.yaml",
        "\
task: greet me
agent:
  instructions: Be warm.
",
    );
    let out_path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("report.json");
    let _ = std::fs::remove_file(&out_path);
    let output = Command::new(onejudge_bin())
        .args([
            "run",
            config.to_str().unwrap(),
            "--format",
            "json",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    // With --output, stdout carries no report; the file does.
    assert!(String::from_utf8(output.stdout).unwrap().trim().is_empty());
    let report: onejudge::Report =
        serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
    assert_eq!(report.schema_version, onejudge::SCHEMA_VERSION);
}

#[test]
fn binary_run_discovers_default_config_in_cwd() {
    // `onejudge run` with no path reads ./onejudge.yaml from the working dir.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("default-cfg");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("onejudge.yaml"),
        config_yaml("task: hello\nagent:\n  instructions: Be helpful.\n"),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .arg("run")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("Status: completed"));
}

#[test]
fn binary_run_without_a_config_falls_back_to_defaults() {
    // No config file and no default in cwd: the run starts from an empty config
    // (default `oneharness` provider). With `oneharness` absent from PATH the spawn
    // fails — a classified engine error, exit 2 — proving the flags-only path runs.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("no-cfg");
    std::fs::create_dir_all(&dir).unwrap();
    let _ = std::fs::remove_file(dir.join("onejudge.yaml"));
    let output = Command::new(onejudge_bin())
        .args(["run", "--task", "do a thing", "--harness", "goose"])
        .current_dir(&dir)
        .env("PATH", "") // ensure `oneharness` cannot be found
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("run failed"));
}

#[test]
fn binary_run_missing_config_path_errors() {
    let missing = Path::new(env!("CARGO_TARGET_TMPDIR")).join("does-not-exist.yaml");
    let output = Command::new(onejudge_bin())
        .args(["run", missing.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("could not read config"));
}

#[test]
fn binary_run_applies_model_session_and_persona_overrides() {
    // Exercises the model / judge-model / session / persona / max-turns override
    // path through the real binary. The command provider ignores model/session, so
    // the assertion is on the run completing under the overridden turn cap.
    let config = write_config(
        "overrides.yaml",
        "\
task: start
agent:
  instructions: Be helpful.
",
    );
    let output = Command::new(onejudge_bin())
        .args([
            "run",
            config.to_str().unwrap(),
            "--model",
            "m1",
            "--judge-model",
            "m2",
            "--session",
            "sess-1",
            "--persona",
            "A demanding reviewer.",
            "--max-turns",
            "2",
        ])
        .output()
        .unwrap();
    // No done_when + persona-implied user hits the 2-turn cap -> incomplete -> 1.
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("hit the turn cap (2)"));
}

#[test]
fn binary_run_provider_override_flag() {
    // `--provider command` overrides just the backend kind; the file already
    // supplies the echo argv.
    let config = write_config(
        "prov-override.yaml",
        "\
task: greet me
agent:
  instructions: Be warm.
",
    );
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--provider", "command"])
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn binary_schema_prints_the_annotated_config() {
    let output = Command::new(onejudge_bin()).arg("schema").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("provider:"));
    assert!(stdout.contains("done_when"));
}
