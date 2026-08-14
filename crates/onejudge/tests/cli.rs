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

use onejudge::cli::{exit_code, render_human, run_plan, Config, EvalOutcome, Format};

mod support;

use support::{await_path, descendant_handle, descendant_is_running, scratch_path};
#[cfg(unix)]
use support::{kill_group, process_exists, OwnedProcessGroups};

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
    format!("provider:\n  kind: command\n  command: [{echo}]\n{body}")
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
system_prompt: 'Commit it. [[event:git commit -m fix]]'
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
    assert!(matches!(echo_eval.outcome, EvalOutcome::Boolean(true)));
    // The numeric eval scored the top of its scale (the criterion matched).
    let numeric = summary
        .eval_results
        .iter()
        .find(|r| r.criterion == "please")
        .unwrap();
    assert!(matches!(numeric.outcome, EvalOutcome::Numeric(n) if n == 5.0));

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
system_prompt: Be helpful.
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
system_prompt: Be helpful.
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
    assert!(matches!(failed.outcome, EvalOutcome::Boolean(false)));
    assert_eq!(exit_code(&summary), 1);
}

#[test]
fn single_turn_run_without_a_user_completes() {
    let body = "\
task: greet me
system_prompt: Be warm.
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
        "provider:\n  kind: oneharness\n  bin: {bin}\n\
         task: go\n\
         system_prompt: '[[reply:the task is complete]]'\n\
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
         task: start\n\
         system_prompt: '[[reply:working]]'\n\
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
        "provider:\n  kind: oneharness\n  bin: {bin}\n\
         task: go\n\
         system_prompt: '[[reply:working]]'\n\
         user:\n  persona: A tester.\n  max_turns: 2\n",
    );
    let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();
    let mut sink = |_: &str| {};
    let summary = run_plan(plan, Format::Json, &mut sink).unwrap();
    assert_eq!(summary.report.transcript.assistant_turns(), 2);
    assert!(summary.hit_max_turns);
    // The oneharness double's prompt-cache counts aggregate into the report usage.
    let usage = summary.report.usage.as_ref().expect("usage aggregated");
    assert!(usage.cache_read_tokens.unwrap_or(0) >= 7);
    assert!(usage.cache_write_tokens.unwrap_or(0) >= 2);
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
         task: start\n\
         system_prompt: '[[reply:working]]'\n\
         user:\n  persona: A tester.\n  max_turns: 2\n\
         evals:\n  - criterion: working\n    kind: boolean\n",
    );
    let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();
    let mut sink = |_: &str| {};
    let summary = run_plan(plan, Format::Json, &mut sink).unwrap();
    assert_eq!(summary.report.transcript.assistant_turns(), 2);
    // The echo judge scored the "working" criterion against the transcript.
    assert!(matches!(
        summary.eval_results[0].outcome,
        EvalOutcome::Boolean(true)
    ));
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
system_prompt: 'Commit it. [[event:git commit -m fix]]'
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
    // The human `Usage:` line surfaces the aggregated prompt-cache reads/writes.
    assert!(
        stdout.contains("cache_read="),
        "human usage shows cache reads"
    );
    assert!(
        stdout.contains("cache_write="),
        "human usage shows cache writes"
    );
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
system_prompt: 'Commit it. [[event:git commit -m fix]]'
user:
  persona: A tester.
  done_when: git commit
  max_turns: 5
evals:
  - criterion: echo
    kind: boolean
assessment: Identify follow-up work and mention tool actions.
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
    assert_eq!(
        report.assessment.as_deref(),
        Some("Assessment for `Identify follow-up work and mention tool actions.`. Tool actions were included.")
    );
    assert_eq!(report.transcript.assistant_turns(), 1);
    // Prompt-cache counts survive the real binary + JSON contract round-trip.
    let usage = report.usage.expect("usage in the report");
    assert!(usage.cache_read_tokens.unwrap_or(0) >= 3);
    assert!(usage.cache_write_tokens.unwrap_or(0) >= 1);
}

#[test]
fn binary_run_exits_one_when_incomplete() {
    let config = write_config(
        "incomplete.yaml",
        "\
task: keep going
system_prompt: Be helpful.
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
system_prompt: Be helpful.
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
fn binary_init_scaffolds_onejudge_and_oneharness_configs() {
    // `onejudge init` shells out to `oneharness init` for the two harness/model
    // config files, then writes the loop-only onejudge.yaml. Point --oneharness-bin
    // at the fake double (which mirrors `oneharness init`) and run in a fresh cwd so
    // the scaffolded files land there.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("init-scaffold");
    std::fs::create_dir_all(&dir).unwrap();
    for f in ["onejudge.yaml", "oneharness.toml", "oneharness.judge.toml"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
    let fake = fake_oneharness_bin();
    let status = Command::new(onejudge_bin())
        .args(["init", "--oneharness-bin", &fake])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert!(status.success());
    // The loop-only onejudge.yaml is a valid config.
    let written = std::fs::read_to_string(dir.join("onejudge.yaml")).unwrap();
    assert!(Config::from_yaml(&written).is_ok());
    // Both oneharness config files were scaffolded by the shelled-out `init`.
    assert!(dir.join("oneharness.toml").exists());
    assert!(dir.join("oneharness.judge.toml").exists());

    // A second init without --force refuses to clobber the existing onejudge.yaml.
    let status = Command::new(onejudge_bin())
        .args(["init", "--oneharness-bin", &fake])
        .current_dir(&dir)
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
system_prompt: Be warm.
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
        config_yaml("task: hello\nsystem_prompt: Be helpful.\n"),
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
        .args(["run", "--task", "do a thing"])
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
fn binary_run_applies_session_and_persona_overrides() {
    // Exercises the session / persona / max-turns override path through the real
    // binary. The command provider ignores session, so the assertion is on the run
    // completing under the overridden turn cap.
    let config = write_config(
        "overrides.yaml",
        "\
task: start
system_prompt: Be helpful.
",
    );
    let output = Command::new(onejudge_bin())
        .args([
            "run",
            config.to_str().unwrap(),
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
fn binary_env_overrides_file_and_flag_overrides_env() {
    // Precedence through the real binary: ONEJUDGE_TASK beats the file's task, and
    // a --task flag in turn beats ONEJUDGE_TASK. The echoed task text surfaces in
    // the human transcript, so we assert on which one won.
    let config = write_config(
        "env-prec.yaml",
        "\
task: from the file
system_prompt: Be warm.
",
    );

    // Env wins over the file.
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .env("ONEJUDGE_TASK", "from the env")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("from the env"), "env task drives the run");
    assert!(!stdout.contains("from the file"), "env beats the file");

    // A --task flag wins over the env var.
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--task", "from the flag"])
        .env("ONEJUDGE_TASK", "from the env")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("from the flag"), "flag task drives the run");
    assert!(!stdout.contains("from the env"), "flag beats the env");
}

#[test]
fn binary_env_selects_the_provider_backend() {
    // ONEJUDGE_PROVIDER flips the resolved provider kind end-to-end. The file
    // supplies the echo argv but declares `kind: oneharness`, under which a
    // `command` field is invalid — so without the env var the run is a loud config
    // error (exit 2), and with `ONEJUDGE_PROVIDER=command` the kind flips and it
    // runs. This exercises the env → ProviderKind parse through the real process.
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join("env-provider.yaml");
    std::fs::write(
        &path,
        format!(
            "provider:\n  kind: oneharness\n  command: [{echo}]\n\
             task: greet me\nsystem_prompt: Be warm.\n"
        ),
    )
    .unwrap();

    // Without the env override, `command` under `kind: oneharness` is rejected.
    let output = Command::new(onejudge_bin())
        .args(["run", path.to_str().unwrap()])
        .env_remove("ONEJUDGE_PROVIDER")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("command"));

    // ONEJUDGE_PROVIDER=command flips the kind so the echo argv is valid.
    let output = Command::new(onejudge_bin())
        .args(["run", path.to_str().unwrap()])
        .env("ONEJUDGE_PROVIDER", "command")
        .output()
        .unwrap();
    assert!(output.status.success(), "env selected the command backend");
}

#[test]
fn binary_env_persona_and_done_when_drive_a_multi_turn_loop() {
    // With no `user` in the file, ONEJUDGE_PERSONA + ONEJUDGE_DONE_WHEN + a turn
    // cap imply a simulated user through the real binary. The done_when never
    // matches the echoed transcript, so the loop runs to the cap and exits 1 —
    // proving the persona/done-when/max-turns env wiring drives a real loop.
    let config = write_config(
        "env-user.yaml",
        "\
task: keep going
system_prompt: Be helpful.
",
    );
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .env("ONEJUDGE_PERSONA", "A demanding reviewer.")
        .env("ONEJUDGE_DONE_WHEN", "deploy to production")
        .env("ONEJUDGE_MAX_TURNS", "2")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("hit the turn cap (2)"));
    assert!(
        stdout.contains("deploy to production"),
        "env done_when is used"
    );
}

#[test]
fn binary_rejects_an_invalid_env_override() {
    // An unparseable ONEJUDGE_* override is a loud config error (exit 2), never a
    // silent fallback.
    let config = write_config(
        "env-bad.yaml",
        "\
task: greet me
system_prompt: Be warm.
",
    );
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .env("ONEJUDGE_MAX_TURNS", "lots")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("ONEJUDGE_MAX_TURNS"));
}

#[test]
fn binary_run_provider_override_flag() {
    // `--provider command` overrides just the backend kind; the file already
    // supplies the echo argv.
    let config = write_config(
        "prov-override.yaml",
        "\
task: greet me
system_prompt: Be warm.
",
    );
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--provider", "command"])
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn binary_run_loads_a_skill_from_a_config_relative_path() {
    // A `skill:` in the config resolves relative to the config file's directory;
    // the loaded SKILL.md body becomes the system prompt the provider sees (here the
    // echo double emits the body's `[[event]]`), so `done_when` holds and the run
    // completes — exercising the rebase + load_skill path through the real binary.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("skill-cfg");
    std::fs::create_dir_all(dir.join("skills/committer")).unwrap();
    std::fs::write(
        dir.join("skills/committer/SKILL.md"),
        "---\nname: committer\ndescription: commits the work\n---\n\
         Commit it. [[event:git commit -m fix]]\n",
    )
    .unwrap();
    let config = dir.join("run.yaml");
    std::fs::write(
        &config,
        config_yaml(
            "task: please commit\nskill: skills/committer\n\
             user:\n  persona: A tester.\n  done_when: git commit\n  max_turns: 5\n",
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success(), "skill-driven run should complete");
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("Status: completed"));
}

#[test]
fn binary_run_skill_and_system_prompt_flags_drive_the_run() {
    // `--skill` (relative to the working dir) and `--system-prompt` supply the
    // framing with no `skill`/`system_prompt` in the file — the flag skill's body
    // still reaches the provider and drives the loop to completion.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("skill-flags");
    std::fs::create_dir_all(dir.join("committer")).unwrap();
    std::fs::write(
        dir.join("committer/SKILL.md"),
        "---\nname: committer\ndescription: commits the work\n---\n[[event:git commit -m fix]]\n",
    )
    .unwrap();
    let config = dir.join("flags.yaml");
    std::fs::write(
        &config,
        config_yaml(
            "task: commit\nuser:\n  persona: A tester.\n  done_when: git commit\n  max_turns: 5\n",
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args([
            "run",
            config.to_str().unwrap(),
            "--skill",
            "committer",
            "--system-prompt",
            "Be terse.",
        ])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("Status: completed"));
}

#[test]
fn binary_env_supplies_skill_and_system_prompt() {
    // `ONEJUDGE_SKILL` / `ONEJUDGE_SYSTEM_PROMPT` supply the framing through the
    // real process with nothing in the file. The env system prompt carries the
    // `[[event]]` that satisfies `done_when`, proving the env-derived system prompt
    // reaches the provider; the env skill (a plain body) loads alongside it.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("skill-env");
    std::fs::create_dir_all(dir.join("worker")).unwrap();
    std::fs::write(
        dir.join("worker/SKILL.md"),
        "---\nname: worker\ndescription: does the work\n---\nDo the work.\n",
    )
    .unwrap();
    let config = write_config(
        "env-skill.yaml",
        "task: please commit\nuser:\n  persona: A tester.\n  done_when: git commit\n  max_turns: 5\n",
    );
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .env("ONEJUDGE_SKILL", dir.join("worker"))
        .env(
            "ONEJUDGE_SYSTEM_PROMPT",
            "Commit it. [[event:git commit -m fix]]",
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "env skill + system prompt drive the run"
    );
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("Status: completed"));
}

#[test]
fn binary_skill_body_and_system_prompt_both_reach_the_harness() {
    // With both set, each half reaches the provider: the `system_prompt`'s
    // `[[event]]` fires (surfacing on stderr) and the skill body's `[[done]]` ends
    // the multi-turn loop on turn one — so a run that would otherwise hit the cap
    // completes, proving the skill body was delivered too.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("skill-both");
    std::fs::create_dir_all(dir.join("finisher")).unwrap();
    std::fs::write(
        dir.join("finisher/SKILL.md"),
        "---\nname: finisher\ndescription: declares itself done\n---\n[[done]]\n",
    )
    .unwrap();
    let config = dir.join("both.yaml");
    std::fs::write(
        &config,
        config_yaml(
            "task: go\nskill: finisher\nsystem_prompt: 'Preamble. [[event:git status]]'\n\
             user:\n  persona: A tester.\n  max_turns: 4\n",
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .output()
        .unwrap();
    // The skill body's `[[done]]` ended the loop before the 4-turn cap (completed).
    assert!(
        output.status.success(),
        "skill body's [[done]] reached the harness"
    );
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("Status: completed"));
    // The system prompt's `[[event]]` reached the harness (events stream to stderr).
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("git status"),
        "system prompt's event reached the harness"
    );
}

#[test]
fn binary_rejects_a_missing_skill_and_exits_two() {
    // A `skill:` pointing at a directory with no SKILL.md is a loud config error
    // (exit 2) through the real binary, never a silent empty prompt.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("skill-missing");
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("missing-skill.yaml");
    std::fs::write(&config, config_yaml("task: go\nskill: does-not-exist\n")).unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("could not load skill"));
}

// --- The streamed protocol, in and out (docs/streaming.md) -----------------

/// A config whose `oneharness` provider is the fake double **in streaming mode**,
/// with `body` appended.
fn streaming_config_yaml(body: &str) -> String {
    let bin = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    format!("provider:\n  kind: oneharness\n  bin: {bin}\n  stream: true\n{body}")
}

#[test]
fn binary_stream_publishes_events_then_the_terminal_report() {
    // End to end through the real binary, both halves of the protocol at once: the
    // double streams its provider-side event lines, and onejudge republishes them
    // on stdout as `event` lines before the terminal `result` line. The double
    // blocks until this test's reader has consumed an event line, so a build that
    // buffered the run could not finish (it fails on the double's own timeout).
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let release = dir.join("cli-stream-release.marker");
    let _ = std::fs::remove_file(&release);
    let config = dir.join("stream.yaml");
    std::fs::write(
        &config,
        streaming_config_yaml(&format!(
            "task: please commit\n\
             system_prompt: '[[reply:committed]][[event:git commit -m fix]][[stream-wait:{}]]'\n\
             evals:\n  - criterion: committed\n    kind: boolean\n",
            release.display()
        )),
    )
    .unwrap();

    let mut child = Command::new(onejudge_bin())
        .args([
            "run",
            config.to_str().unwrap(),
            "--format",
            "json",
            "--stream",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Read the stream line by line, releasing the double the moment the first
    // `event` line arrives — the proof that it arrived mid-run.
    use std::io::BufRead as _;
    let stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut events = Vec::new();
    let mut report: Option<onejudge::Report> = None;
    for line in stdout.lines() {
        let value: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        match value["type"].as_str() {
            Some("event") => {
                std::fs::write(&release, b"go").unwrap();
                events.push(value);
            }
            Some("result") => {
                report = Some(serde_json::from_value(value["report"].clone()).unwrap());
            }
            other => panic!("unexpected stream line type {other:?}"),
        }
    }
    let status = child.wait().unwrap();
    let _ = std::fs::remove_file(&release);

    assert_eq!(status.code(), Some(0));
    assert_eq!(events.len(), 1, "one event line arrived");
    assert_eq!(events[0]["turn"], 1);
    assert_eq!(events[0]["event"]["name"], "bash");
    let report = report.expect("the terminal result line carried the report");
    assert_eq!(report.schema_version, onejudge::SCHEMA_VERSION);
    assert_eq!(report.transcript.messages[1].content, "committed");
    assert_eq!(report.verdicts.len(), 1);
}

#[test]
fn binary_run_json_carries_the_backend_telemetry() {
    // The CLI's runtime-dispatched provider has to forward telemetry from whichever
    // backend made the call; without that the report silently drops it.
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("telemetry.yaml");
    let bin = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: oneharness\n  bin: {bin}\n\
             task: measure this\nsystem_prompt: '[[reply:telemetry ready]]'\n\
             evals:\n  - criterion: telemetry ready\n    kind: boolean\n"
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: onejudge::Report =
        serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let telemetry = report.telemetry.expect("telemetry reaches the report");
    assert_eq!(telemetry.agent.model_ms, Some(10));
    assert_eq!(telemetry.judge.model_ms, Some(5));
    assert_eq!(telemetry.agent.session_ids, ["native-onejudge-skill"]);
}

#[test]
fn binary_run_json_reports_the_processes_it_spawned_and_names_no_group() {
    // What an in-process embedder learns through its spawn hook — which processes
    // the run created, on which side, and whether a group claimed them — is
    // machine-readable from the CLI too. The CLI installs no hook, so every record
    // says so by carrying no `group` rather than inventing one.
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("processes.yaml");
    let bin = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: oneharness\n  bin: {bin}\n\
             task: spawn something\nsystem_prompt: '[[reply:spawned]]'\n\
             evals:\n  - criterion: spawned\n    kind: boolean\n"
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let report: onejudge::Report = serde_json::from_str(&stdout).unwrap();
    assert!(
        !report.processes.is_empty(),
        "the run reports the processes it spawned"
    );
    assert!(report
        .processes
        .iter()
        .any(|p| p.role == onejudge::TelemetryRole::Agent && p.op == "respond"));
    assert!(report
        .processes
        .iter()
        .any(|p| p.role == onejudge::TelemetryRole::Judge && p.op == "judge"));
    assert!(report.processes.iter().all(|p| p.pid > 0));
    assert!(
        report.processes.iter().all(|p| p.group.is_none()),
        "no hook is installed, so no group is claimed"
    );
    assert!(!stdout.contains("\"group\""));
}

#[test]
fn binary_stream_exits_one_when_the_run_is_incomplete() {
    // The stream is an output format, not a status: the exit code still reports
    // whether the task completed.
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("stream-incomplete.yaml");
    std::fs::write(
        &config,
        streaming_config_yaml(
            "task: keep going\n\
             system_prompt: '[[reply:still working]]'\n\
             user:\n  persona: A tester.\n  done_when: deploy to production\n  max_turns: 1\n",
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args([
            "run",
            config.to_str().unwrap(),
            "--format",
            "json",
            "--stream",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let last = stdout.lines().next_back().expect("a terminal line");
    let value: serde_json::Value = serde_json::from_str(last).unwrap();
    assert_eq!(value["type"], "result");
}

#[test]
fn binary_stream_rejects_an_output_surface_it_cannot_honor() {
    let config = write_config("stream-misuse.yaml", "task: go\nsystem_prompt: Be warm.\n");
    for (args, needle) in [
        (vec!["--stream"], "--format json"),
        (
            vec!["--stream", "--format", "json", "--output", "report.json"],
            "drop --output",
        ),
    ] {
        let output = Command::new(onejudge_bin())
            .arg("run")
            .arg(config.to_str().unwrap())
            .args(&args)
            .current_dir(Path::new(env!("CARGO_TARGET_TMPDIR")))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(needle), "{args:?}: {stderr}");
    }
}

#[test]
fn binary_reports_a_malformed_provider_stream_and_exits_two() {
    // A provider that declared streaming and then wrote a line the protocol does
    // not model fails the run loudly, naming the violation.
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("stream-bad.yaml");
    std::fs::write(
        &config,
        streaming_config_yaml(
            "task: go\nsystem_prompt: '[[reply:ok]][[event:ls]][[stream-unknown]]'\n",
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("unknown run stream envelope type `progress`"),
        "{stderr}"
    );
}

#[test]
fn binary_rejects_a_provider_that_writes_past_its_terminal_line() {
    // Through the real binary: a streamed provider whose report is complete but
    // which then keeps writing is a loud failure, not a run that quietly succeeds
    // on the report it did produce.
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("stream-trailing.yaml");
    std::fs::write(
        &config,
        streaming_config_yaml(
            "task: go\nsystem_prompt: '[[reply:ok]][[stream-trailing:unknown]]'\n",
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("wrote a line after its terminal `result` line"),
        "{stderr}"
    );
}

#[test]
fn binary_schema_prints_the_annotated_config() {
    let output = Command::new(onejudge_bin()).arg("schema").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("provider:"));
    assert!(stdout.contains("done_when"));
}

// --- Structured harness attribution through the binary --------------------

#[test]
fn binary_run_json_reports_which_harness_identities_were_attempted() {
    // Everything the library learns about the candidates oneharness attempted has
    // to be readable off the CLI's JSON, not just off an in-process `Telemetry`.
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("attribution.yaml");
    let history = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli-attribution-history.jsonl");
    let _ = std::fs::remove_file(&history);
    let bin = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: oneharness\n  bin: {bin}\n\
             task: attribute this\n\
             system_prompt: '[[reply:attributed]][[fallback:codex|quota]][[history:{}]]'\n",
            history.display()
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let report: onejudge::Report =
        serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let telemetry = report.telemetry.expect("telemetry reaches the report");
    let agent = telemetry
        .attribution
        .iter()
        .find(|a| a.role == onejudge::TelemetryRole::Agent)
        .expect("the agent invocation is attributed");
    assert_eq!(agent.ran.as_deref(), Some("claude-code"));
    assert_eq!(agent.fell_through[0].harness, "codex");
    assert_eq!(agent.fell_through[0].reason, "quota");
    assert_eq!(agent.candidates.len(), 2);
    assert_eq!(agent.candidates[0].failure_kind.as_deref(), Some("quota"));
    assert_eq!(agent.candidates[0].status, "nonzero");
    assert!(agent.candidates[0].history_id.is_some());
    assert_eq!(
        agent.history_file.as_deref(),
        Some(history.to_str().unwrap())
    );
}

#[test]
fn binary_run_json_writes_a_structured_failure_document_when_the_run_fails() {
    // A failed run produces no report, but it is exactly the case a caller needs
    // attribution for. Under `--format json` the failure and the identities that
    // were tried go where the report would have — machine-readable, exit code 2.
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("attribution-failure.yaml");
    let bin = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: oneharness\n  bin: {bin}\n\
             task: fail this\n\
             system_prompt: '[[fallback-exhausted:codex|quota,claude-code|auth]]'\n"
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));

    let failure: onejudge::cli::FailureReport =
        serde_json::from_str(&String::from_utf8(output.stdout).unwrap())
            .expect("the failure document is on stdout");
    assert_eq!(failure.schema_version, onejudge::SCHEMA_VERSION);
    assert_eq!(failure.error.kind, Some(onejudge::ProviderErrorKind::Auth));
    assert!(failure.error.message.contains("codex [quota]"));
    let telemetry = failure
        .telemetry
        .expect("the failed run is still attributed");
    let agent = &telemetry.attribution[0];
    assert_eq!(agent.role, onejudge::TelemetryRole::Agent);
    assert_eq!(agent.ran, None, "no candidate ran");
    let ids: Vec<_> = agent
        .candidates
        .iter()
        .map(|c| c.harness_id.as_str())
        .collect();
    assert_eq!(ids, ["codex", "claude-code"]);
    // The human message is unchanged on stderr — the document is additive.
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("onejudge: run failed"), "{stderr}");
}

#[test]
fn binary_stream_reports_a_failure_as_json_on_stderr_leaving_the_protocol_intact() {
    // stdout under `--stream` is the `event* result EOF` protocol, so a failure
    // cannot be published there without inventing an envelope every consumer would
    // have to learn. It goes to stderr as one JSON document instead.
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("stream-attribution-failure.yaml");
    std::fs::write(
        &config,
        streaming_config_yaml(
            "task: fail this\nsystem_prompt: '[[fallback-exhausted:codex|quota]]'\n",
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args([
            "run",
            config.to_str().unwrap(),
            "--format",
            "json",
            "--stream",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout).unwrap().trim().is_empty(),
        "the stream protocol stays exactly `event* result EOF`"
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    let document = stderr
        .lines()
        .find(|line| line.starts_with('{'))
        .expect("one JSON line on stderr");
    let failure: onejudge::cli::FailureReport =
        serde_json::from_str(document).expect("the failure document is on stderr");
    assert_eq!(failure.error.kind, Some(onejudge::ProviderErrorKind::Quota));
    assert_eq!(
        failure.telemetry.expect("attributed").attribution[0]
            .candidates
            .len(),
        1
    );
}

// --- The spawn seam at the PLAN level --------------------------------------
//
// `SpawnHook` gives an in-process embedder back the OS grouping the subprocess
// boundary used to supply — but an embedder that drives onejudge through a
// `Plan` never builds a provider itself, so before `Plan::with_spawn_hook` it
// had no way to install one. The processes a plan spawned therefore sat in
// onejudge's own group, and a `cancel --kill` had no tree to name. See
// `docs/spawn-hook.md`.

/// A two-party plan: the agent's turns and the judge's each run on their own
/// `oneharness` backend (the fake double), which is the shape that leaks — one
/// hook has to reach BOTH sides.
fn split_plan_yaml(body: &str) -> String {
    let bin = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    format!(
        "provider:\n  kind: split\n  \
         skill:\n    kind: oneharness\n    bin: {bin}\n  \
         judge:\n    kind: oneharness\n    bin: {bin}\n{body}"
    )
}

/// A hook that names one embedder-owned group for the whole run and records what
/// it was offered — the portable half of what the `killpg` journey below proves.
#[derive(Default)]
struct OneGroup {
    seen: std::sync::Mutex<Vec<(onejudge::TelemetryRole, String)>>,
}

impl onejudge::SpawnHook for OneGroup {
    fn spawned(
        &self,
        child: &std::process::Child,
        context: &onejudge::SpawnContext<'_>,
    ) -> std::io::Result<Option<String>> {
        assert!(child.id() > 0, "the hook is offered a live process");
        self.seen
            .lock()
            .unwrap()
            .push((context.role, context.op.to_string()));
        Ok(Some("job:plan-1".to_string()))
    }
}

/// A two-party plan over the echo double, so both sides can be driven on every
/// platform the crate supports.
fn two_party_command_plan() -> onejudge::cli::Plan {
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let yaml = format!(
        "provider:\n  kind: split\n  \
         skill:\n    kind: command\n    command: [{echo}]\n  \
         judge:\n    kind: command\n    command: [{echo}]\n\
         task: please commit\nsystem_prompt: 'Commit it.'\n\
         user:\n  persona: A tester.\n  max_turns: 2\n"
    );
    Config::from_yaml(&yaml).unwrap().into_plan().unwrap()
}

#[test]
fn a_plans_spawn_hook_reaches_both_sides_of_a_two_party_run() {
    // One embedder-owned group has to span BOTH backends a plan builds — the side
    // that runs the worker and the side that judges/plays the user. Installing the
    // hook on the plan reaches every process either one spawns.
    let hook = std::sync::Arc::new(OneGroup::default());
    let installed: onejudge::SharedSpawnHook = hook.clone();
    let mut sink = |_: &str| {};
    let summary = run_plan(
        two_party_command_plan().with_spawn_hook(installed),
        Format::Json,
        &mut sink,
    )
    .unwrap();

    let seen = hook.seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .any(|(role, op)| *role == onejudge::TelemetryRole::Agent && op == "respond"),
        "the worker side's spawns were offered: {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|(role, _)| *role == onejudge::TelemetryRole::Judge),
        "the judge side's spawns were offered: {seen:?}"
    );

    // Everything the hook was offered is what the plan's report names, with the
    // group the hook placed it in — the same records `--format json` prints.
    assert_eq!(summary.report.processes.len(), seen.len());
    assert!(summary
        .report
        .processes
        .iter()
        .all(|p| p.group.as_deref() == Some("job:plan-1") && p.pid > 0));
}

#[test]
fn a_plan_without_a_spawn_hook_keeps_todays_behaviour_and_claims_no_group() {
    // The no-hook plan is unchanged: it still spawns, still reports what it
    // spawned, and says honestly that no group claimed it.
    let mut sink = |_: &str| {};
    let summary = run_plan(two_party_command_plan(), Format::Json, &mut sink).unwrap();
    assert!(!summary.report.processes.is_empty());
    assert!(summary.report.processes.iter().all(|p| p.group.is_none()));
    let json = serde_json::to_string(&summary.report).unwrap();
    assert!(!json.contains("\"group\""));
}

#[test]
fn binary_run_json_reports_both_sides_processes_of_a_two_party_plan() {
    // What the plan-level hook can now group in-process is machine-readable from
    // the CLI for the same two-party run: one `processes` record per spawn, on
    // both sides, each naming the group that claimed it — none, here, because a
    // command line cannot install an in-process hook and onejudge never invents
    // a group it did not observe.
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("split-processes.yaml");
    std::fs::write(
        &config,
        split_plan_yaml(
            "task: spawn on both sides\nsystem_prompt: '[[reply:spawned]]'\n\
             user:\n  persona: A tester.\n  done_when: spawned\n  max_turns: 3\n",
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: onejudge::Report =
        serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert!(report
        .processes
        .iter()
        .any(|p| p.role == onejudge::TelemetryRole::Agent && p.op == "respond"));
    assert!(report
        .processes
        .iter()
        .any(|p| p.role == onejudge::TelemetryRole::Judge));
    assert!(report
        .processes
        .iter()
        .all(|p| p.pid > 0 && p.group.is_none()));
}

#[cfg(unix)]
#[test]
fn a_plan_driven_embedders_group_reaps_the_whole_two_party_harness_tree_on_a_kill_cancel() {
    // The defect this reach closes, driven exactly as oneagentgraph hits it: a
    // library embedder that drives a PLAN (config → plan → `run_plan`), a
    // two-party run where each party's harness stand-in outlives the `oneharness`
    // process that spawned it, and a cancel that must reap the whole tree.
    //
    // Without a plan-level hook there is no group to name here — the plan's
    // spawned processes sit in onejudge's own group, which is the test runner's,
    // so the only available `killpg` would take the test process with it. That is
    // why this cannot be written against a build without the reach.
    let agent_handle = scratch_path("plan-grouped-agent.handle");
    let judge_handle = scratch_path("plan-grouped-judge.handle");
    let never = scratch_path("plan-grouped-judge.hold");

    let hook = std::sync::Arc::new(OwnedProcessGroups::default());
    let installed: onejudge::SharedSpawnHook = hook.clone();
    // The agent side leaks a stand-in whose `oneharness` then exits; the judge
    // side (steered through the task, which the supervisor prompt inlines) leaks
    // its own and then holds, so the run is still in flight when the embedder
    // cancels.
    let yaml = split_plan_yaml(&format!(
        "task: 'go [[orphan:{}]][[hold:{}]]'\n\
         system_prompt: '[[reply:acknowledged]][[orphan:{}]]'\n\
         user:\n  persona: A patient tester.\n  max_turns: 2\n",
        judge_handle.display(),
        never.display(),
        agent_handle.display(),
    ));

    let worker = std::thread::spawn(move || {
        let plan = Config::from_yaml(&yaml)
            .unwrap()
            .into_plan()
            .unwrap()
            .with_spawn_hook(installed);
        let mut sink = |_: &str| {};
        run_plan(plan, Format::Json, &mut sink).map(|_| ())
    });

    await_path(&agent_handle, "the agent's harness stand-in never started");
    await_path(&judge_handle, "the judge's harness stand-in never started");
    let (agent_pid, agent_port) = descendant_handle(&agent_handle);
    let (judge_pid, judge_port) = descendant_handle(&judge_handle);
    assert!(
        descendant_is_running(agent_port) && descendant_is_running(judge_port),
        "both harness stand-ins were running when the run was cancelled"
    );

    // Cancel with kill semantics: terminate every group the embedder was handed.
    // Nothing else is signalled — the stand-ins are reached only because they
    // inherited a group the hook created around a process the *plan* spawned.
    let groups = hook.groups.lock().unwrap().clone();
    assert!(
        groups.len() >= 2,
        "both parties' spawns were placed in a group: {groups:?}"
    );
    for pgid in &groups {
        kill_group(*pgid);
    }

    let outcome = worker.join().expect("the run thread did not panic");
    assert!(
        outcome.is_err(),
        "killing the group ends the in-flight run rather than letting it complete"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while descendant_is_running(agent_port) || descendant_is_running(judge_port) {
        assert!(
            std::time::Instant::now() < deadline,
            "a harness stand-in (agent {agent_pid}, judge {judge_pid}) outlived the \
             cancelled plan: the process it descends from was not in a group the \
             embedder could terminate"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // Every process the plan itself spawned is gone too.
    for pgid in &groups {
        assert!(
            !process_exists(*pgid),
            "the process the plan spawned as group {pgid} survived the kill"
        );
    }

    for path in [&agent_handle, &judge_handle] {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(unix)]
#[test]
fn binary_run_publishes_the_control_address_in_the_json_report() {
    // The whole contract from a consumer's side: `provider.control: true` in YAML,
    // and the shipped binary's `--format json` carries the three values
    // `oneharness interrupt` addresses the turn with.
    use oneharness_core::io::session as session_io;

    let store = support::control_store("cli-ctl");
    let bin = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    let system = format!(
        "[[reply:the task is complete]][[control-store:{}]]",
        store.display()
    );
    let yaml = format!(
        "provider:\n  kind: oneharness\n  bin: {bin}\n  control: true\n\
         task: go\n\
         session: Run 7\n\
         system_prompt: {}\n",
        serde_json::to_string(&system).unwrap(),
    );
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("control.yaml");
    std::fs::write(&path, yaml).unwrap();

    let output = Command::new(onejudge_bin())
        .args(["run", path.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let control = report["control"].as_object().expect("an address");
    assert_eq!(
        control.keys().collect::<Vec<_>>(),
        ["cwd", "session", "session_dir"],
        "exactly the three values `oneharness interrupt` takes"
    );
    assert_eq!(control["session"], "run-7-skill");
    assert!(report["control_unavailable"].is_null());

    // The address resolves to the record an interrupt reads before it dials.
    let dir = session_io::resolve_dir(control["session_dir"].as_str()).unwrap();
    let record = session_io::read(&session_io::session_path(
        &dir,
        Path::new(control["cwd"].as_str().unwrap()),
        control["session"].as_str().unwrap(),
    ))
    .expect("`oneharness interrupt` finds the session at the reported address");
    assert!(record.harness.spec().control.is_some());

    let _ = std::fs::remove_dir_all(&store);
}

/// A skill directory that is also a oneharness project: its `SKILL.md` seeds the
/// system prompt, and its `oneharness.toml` is what the in-process engine
/// discovers from `--cwd` to pin the harness to the fake-harness double.
fn in_process_project(name: &str, instructions: &str) -> std::path::PathBuf {
    let dir = scratch_path(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), instructions).unwrap();
    std::fs::write(
        dir.join("oneharness.toml"),
        format!(
            "harnesses = [\"claude-code\"]\nhistory_dir = {:?}\n\n[harness.claude-code]\nbin = {:?}\n",
            dir.join("history").display().to_string(),
            env!("CARGO_BIN_EXE_onejudge-fake-harness"),
        ),
    )
    .unwrap();
    dir
}

#[test]
fn an_oneharness_config_that_names_no_bin_runs_the_turn_in_process() {
    // The CLI's default once the seam moved: `kind: oneharness` with no `bin`
    // spawns nothing and needs no `oneharness` on PATH. Driven through the real
    // run driver, so this is what a consumer's config actually does — the unit
    // test on `ProviderSpec` only proves the parse.
    let dir = in_process_project("cli-in-process", "[[reply:done in process]]");
    let yaml = format!(
        "provider:\n  kind: oneharness\nskill: {}\ntask: go\n",
        serde_json::to_string(&dir.display().to_string()).unwrap()
    );
    let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();
    let mut sink = |_: &str| {};
    let summary = run_plan(plan, Format::Human, &mut sink).unwrap();

    assert_eq!(summary.report.transcript.assistant_turns(), 1);
    assert_eq!(
        summary.report.transcript.messages[1].content,
        "done in process"
    );
    // Nothing was spawned by onejudge, which is the whole point of the default.
    assert!(summary.report.processes.is_empty());
    assert_eq!(exit_code(&summary), 0);
}
