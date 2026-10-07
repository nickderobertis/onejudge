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
//!   surface, only the model faked, exactly as `onejudge-e2e` does for the engine.
//!
//! This crate enables `cli` and `sdk-schema` on its `onejudge` dependency, and its
//! Nx `test` target builds the `onejudge` binary and the doubles first, so these
//! always run in the gate.

use std::ops::ControlFlow;
use std::path::Path;
use std::process::Command;

use onejudge::cli::{
    exit_code, render_human, run_plan, run_plan_observing_reporting_failure,
    run_plan_streaming_reporting_failure, Config, EvalOutcome, Format, Plan, RunFailure,
    RunSummary,
};
use onejudge::{Conversation, Engine, Observation, Outcome, StreamEvent, Telemetry};

/// The public shape of the observing and streaming entry points, pinned at compile
/// time from a consumer's vantage point: each coercion below names the exact
/// signature the contract mandates, so a drift in any parameter or return type is
/// a compile error in this file rather than a silent break in a consumer's build.
///
/// The observing failure comes back **by value** — `RunFailure`, not
/// `Box<RunFailure>` — while `RunFailure::telemetry` keeps the unboxed
/// `Option<Telemetry>` it has always been published as, which the last pin below
/// holds to. Widening the seam is not licence to reshape a type consumers already
/// read. The two streaming pins are the other half: they are what proves the
/// narrower entry point stayed exactly where it was.
///
/// Spelled as aliases because the bare `fn` types trip `clippy::type_complexity`;
/// an alias is transparent, so the coercion still checks the real signature.
/// `Engine<'static>` instantiates the engine's own early-bound lifetime, which a
/// `fn` pointer cannot leave higher-ranked; it is not part of what is being pinned.
type ObservingPlanFn = fn(
    Plan,
    &mut dyn FnMut(&Observation<'_>) -> ControlFlow<()>,
) -> std::result::Result<RunSummary, RunFailure>;
type ObservingEngineFn = fn(
    &Engine<'static>,
    &Conversation,
    &mut dyn FnMut(&Observation<'_>) -> ControlFlow<()>,
) -> onejudge::Result<Outcome>;
type StreamingPlanFn = fn(
    Plan,
    &mut dyn FnMut(&StreamEvent<'_>) -> ControlFlow<()>,
) -> std::result::Result<RunSummary, Box<RunFailure>>;
type StreamingEngineFn = fn(
    &Engine<'static>,
    &Conversation,
    &mut dyn FnMut(&StreamEvent<'_>) -> ControlFlow<()>,
) -> onejudge::Result<Outcome>;

const _: ObservingPlanFn = run_plan_observing_reporting_failure;
const _: ObservingEngineFn = Engine::run_observing;
const _: StreamingPlanFn = run_plan_streaming_reporting_failure;
const _: StreamingEngineFn = Engine::run_streaming;
const _: fn(&RunFailure) -> &Option<Telemetry> = |failure| &failure.telemetry;

use onejudge_test_doubles::{self as doubles, support};

use support::{await_path, descendant_handle, descendant_is_running, scratch_path};
#[cfg(unix)]
use support::{kill_group, process_exists, OwnedProcessGroups};

/// The built echo test double's path (a `CommandProvider` backend).
fn echo_bin() -> String {
    doubles::echo_provider().to_string()
}

fn onejudge_bin() -> &'static str {
    doubles::onejudge_cli()
}

/// The built fake-oneharness double (an `OneharnessProvider` backend).
fn fake_oneharness_bin() -> String {
    doubles::fake_oneharness().to_string()
}

fn assert_supervisor_v8_frames_validate(frames: &[serde_json::Value]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("schemas/judge-seat-frames.json");
    let link: onemessagebus::SchemaLink = format!("file://{}@8", path.display()).parse().unwrap();
    let resolved = onemessagebus::LinkResolver::new(None)
        .resolve(&link, onemessagebus::Freshness::Window)
        .unwrap();
    let mut registry = onemessagebus::Registry::new();
    resolved.bundle().register_into(&mut registry).unwrap();
    let id = "agent.onejudge-frame.supervisor@8".parse().unwrap();
    for frame in frames {
        registry.check(&id, frame).unwrap();
    }
}

/// A config whose `command` provider is the echo double, with `body` appended.
/// The binary path is JSON-encoded into the YAML flow list so a Windows path
/// (backslashes, a drive-letter colon) stays a valid scalar cross-platform.
fn config_yaml(body: &str) -> String {
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    format!("provider:\n  kind: command\n  command: [{echo}]\n{body}")
}

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
fn settled_run_is_reported_incomplete_with_its_reason_and_exits_one() {
    // The supervisor judges the work incomplete and then names no next instruction,
    // however often it is asked. The run keeps the turn it produced — this used to
    // be a hard failure that destroyed it — but it is *not* a completion: without a
    // `done_when` the loop merely ending early would otherwise read as one, which
    // would hide the very case the reason exists to surface.
    let body = "\
task: start
system_prompt: Be helpful.
user:
  persona: '[[malformed-supervisor]]'
  max_turns: 4
";
    let summary = plan_from(body);
    assert_eq!(summary.report.transcript.assistant_turns(), 1);
    assert!(!summary.hit_max_turns, "it settled well before the cap");
    assert!(
        !summary.completed,
        "a settled run did not complete the task"
    );
    assert_eq!(exit_code(&summary), 1);
    let settled = summary
        .report
        .settled_reason
        .clone()
        .expect("the report says why the run settled");
    assert!(settled.contains("no next instruction"), "{settled}");
    let rendered = render_human(&summary);
    assert!(
        rendered.contains(&format!("Status: incomplete — {settled}")),
        "the operator is told which kind of incomplete this is: {rendered}"
    );
}

#[test]
fn a_config_declaring_quiet_its_contract_is_driven_to_the_cap_not_settled() {
    // The config-file half of the same contract, through the real run driver: an
    // observer whose job is to report nothing keeps being driven, and the run it
    // produces reads as one that hit its cap rather than one that settled.
    let quiet = "\
task: watch the run
system_prompt: Be helpful.
user:
  persona: '[[supervisor-noop]]'
  max_turns: 5
";
    // Omitting the key is accepted and behaves exactly as it always has.
    let settling = plan_from(quiet);
    assert_eq!(
        settling.report.transcript.assistant_turns(),
        1 + onejudge::NOOP_SETTLE_LIMIT as usize
    );
    assert!(!settling.hit_max_turns);
    let settled = settling
        .report
        .settled_reason
        .clone()
        .expect("the default settles on the no-op streak");
    assert!(settled.contains("no-op exchanges"), "{settled}");

    let watching = plan_from(&format!("{quiet}  settle_on_noop: false\n"));
    assert_eq!(
        watching.report.transcript.assistant_turns(),
        5,
        "the declared-quiet observer is driven to its cap"
    );
    assert!(watching.hit_max_turns);
    assert!(
        watching.report.settled_reason.is_none(),
        "{:?}",
        watching.report.settled_reason
    );
    assert_eq!(exit_code(&watching), 1);
    assert!(render_human(&watching).contains("hit the turn cap (5)"));
}

#[test]
fn binary_reports_a_settled_run_in_both_formats() {
    // The same journey through the shipped binary: the reason reaches the JSON
    // report a consumer parses *and* the human status line, and the exit code says
    // the task did not complete.
    let config = write_config(
        "settled.yaml",
        "\
task: start
system_prompt: Be helpful.
user:
  persona: '[[malformed-supervisor]]'
  max_turns: 4
",
    );
    let json = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(json.status.code(), Some(1));
    let report: onejudge::Report = serde_json::from_slice(&json.stdout).unwrap();
    let settled = report.settled_reason.expect("the wire form carries it");
    assert!(settled.contains("no next instruction"), "{settled}");
    assert_eq!(
        report.transcript.assistant_turns(),
        1,
        "the work the run did survived"
    );
    assert!(report.completion_reason.is_none());

    let human = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(human.status.code(), Some(1));
    let stdout = String::from_utf8(human.stdout).unwrap();
    assert!(
        stdout.contains(&format!("Status: incomplete — {settled}")),
        "{stdout}"
    );
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
    // in Text format so the observing dispatch arm runs. The agent's reply
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
    let summary = run_plan(plan, Format::Text, &mut sink).unwrap();
    assert!(summary.completed);
    assert_eq!(summary.report.transcript.assistant_turns(), 1);
    assert_eq!(exit_code(&summary), 0);
}

#[test]
fn mock_harness_config_reaches_the_spawned_oneharness() {
    // A consumer reaching oneharness *through* onejudge selects the free
    // deterministic harness in the config, and the id lands on the argv of the real
    // subprocess the run spawns — the double replies with what it was given.
    let bin = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    let yaml = format!(
        "provider:\n  kind: oneharness\n  bin: {bin}\n  mock_harness: [claude-code]\n\
         task: go\n\
         system_prompt: '[[echo-mock-harness]]'\n",
    );
    let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();
    let mut sink = |_: &str| {};
    let summary = run_plan(plan, Format::Json, &mut sink).unwrap();
    assert_eq!(
        summary.report.transcript.messages[1].content, "claude-code",
        "the configured mock harness reached the spawned argv"
    );
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
    let summary = run_plan(plan, Format::Text, &mut sink).unwrap();
    assert_eq!(summary.report.transcript.assistant_turns(), 2);
    assert!(summary.hit_max_turns);
    assert_eq!(exit_code(&summary), 1);
    // The agent turns came from the oneharness skill backend (its `[[reply]]`).
    assert_eq!(summary.report.transcript.messages[1].content, "working");
}

#[test]
fn protocol_v8_reports_taken_and_lost_turns_to_a_command_supervisor() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let harness = serde_json::to_string(&fake_oneharness_bin()).unwrap();

    for (name, system_prompt) in [("text", "[[reply:working]]"), ("empty", "[[no-text]]")] {
        let log = dir.join(format!("protocol-v8-taken-{name}.jsonl"));
        let _ = std::fs::remove_file(&log);
        let record = serde_json::to_string(&format!("[[record:{}]]", log.display())).unwrap();
        let config = dir.join(format!("protocol-v8-taken-{name}.yaml"));
        std::fs::write(
            &config,
            format!(
                "provider:\n  kind: split\n  skill:\n    kind: oneharness\n    bin: {harness}\n  judge:\n    kind: command\n    command: [{echo}, {record}, '[[supervisor-complete:done]]']\ntask: go\nsystem_prompt: '{system_prompt}'\nuser:\n  persona: reviewer\n  max_turns: 2\n"
            ),
        )
        .unwrap();
        let output = Command::new(onejudge_bin())
            .args(["run", config.to_str().unwrap(), "--format", "json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let frame: serde_json::Value = serde_json::from_str(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(frame["turn"], serde_json::json!({"outcome": "taken"}));
        assert_supervisor_v8_frames_validate(&[frame]);
    }

    let log = dir.join("protocol-v8-lost.jsonl");
    let _ = std::fs::remove_file(&log);
    let record = serde_json::to_string(&format!("[[record:{}]]", log.display())).unwrap();
    let config = dir.join("protocol-v8-lost.yaml");
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: split\n  skill:\n    kind: oneharness\n    bin: {harness}\n  judge:\n    kind: command\n    command: [{echo}, {record}, '[[supervisor-complete:accepted loss]]']\ntask: go\nsystem_prompt: '[[fail:quota]][[harness:codex:alternate]]'\nuser:\n  persona: reviewer\n  max_turns: 2\n"
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let lines = std::fs::read_to_string(&log).unwrap();
    let frames: Vec<serde_json::Value> = lines
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frames[0]["turn"],
        serde_json::json!({"outcome": "lost", "cause": "quota", "harness": "codex:alternate"})
    );
    assert!(frames[0]["messages"].as_array().unwrap().len() == 1);
    assert_supervisor_v8_frames_validate(&frames);

    let log = dir.join("protocol-v8-exhausted.jsonl");
    let _ = std::fs::remove_file(&log);
    let record = serde_json::to_string(&format!("[[record:{}]]", log.display())).unwrap();
    let config = dir.join("protocol-v8-exhausted.yaml");
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: split\n  skill:\n    kind: oneharness\n    bin: {harness}\n  judge:\n    kind: command\n    command: [{echo}, {record}, '[[supervisor-complete:accepted loss]]']\ntask: go\nsystem_prompt: '[[fallback-exhausted:codex|quota,claude-code:backup|auth]]'\nuser:\n  persona: reviewer\n  max_turns: 2\n"
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let frame: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&log).unwrap().trim()).unwrap();
    assert_eq!(
        frame["turn"],
        serde_json::json!({"outcome": "lost", "cause": "auth", "harness": "claude-code:backup"})
    );
    assert_supervisor_v8_frames_validate(&[frame]);

    let log = dir.join("protocol-v8-status-loss.jsonl");
    let _ = std::fs::remove_file(&log);
    let record = serde_json::to_string(&format!("[[record:{}]]", log.display())).unwrap();
    let config = dir.join("protocol-v8-status-loss.yaml");
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: split\n  skill:\n    kind: oneharness\n    bin: {harness}\n  judge:\n    kind: command\n    command: [{echo}, {record}, '[[supervisor-complete:accepted loss]]']\ntask: go\nsystem_prompt: '[[status:timeout]]'\nuser:\n  persona: reviewer\n  max_turns: 2\n"
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let frame: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&log).unwrap().trim()).unwrap();
    assert_eq!(
        frame["turn"],
        serde_json::json!({"outcome": "lost", "cause": "timeout", "harness": "claude-code"})
    );
    assert_supervisor_v8_frames_validate(&[frame]);
}

#[test]
fn protocol_v8_lost_turn_can_continue_and_command_failure_preserves_the_loss() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let once = dir.join("protocol-v8-once");
    let log = dir.join("protocol-v8-recovery.jsonl");
    let _ = std::fs::remove_file(&once);
    let _ = std::fs::remove_file(&log);
    let record = serde_json::to_string(&format!("[[record:{}]]", log.display())).unwrap();
    let config = dir.join("protocol-v8-recovery.yaml");
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: split\n  skill:\n    kind: command\n    command: [{echo}]\n  judge:\n    kind: command\n    command: [{echo}, {record}, '[[supervisor-continue-on-lost:retry now]]']\ntask: '[[emit-exit-once:{}]]'\nsystem_prompt: work\nuser:\n  persona: reviewer\n  max_turns: 3\n",
            once.display()
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let frames: Vec<serde_json::Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        frames[0]["turn"],
        serde_json::json!({"outcome": "lost", "cause": "protocol"})
    );
    assert_eq!(frames[1]["turn"], serde_json::json!({"outcome": "taken"}));
    assert_eq!(frames[1]["messages"][1]["content"], "retry now");
    assert_supervisor_v8_frames_validate(&frames);

    let harness = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    let config = dir.join("protocol-v8-supervisor-exit.yaml");
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: split\n  skill:\n    kind: oneharness\n    bin: {harness}\n  judge:\n    kind: command\n    command: [{echo}, '[[supervisor-exit]]']\ntask: go\nsystem_prompt: '[[fail:quota]]'\nuser:\n  persona: reviewer\n  max_turns: 2\n"
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let failure: onejudge::cli::FailureReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(failure.error.kind, Some(onejudge::ProviderErrorKind::Quota));

    let log = dir.join("protocol-v8-no-instruction.jsonl");
    let _ = std::fs::remove_file(&log);
    let record = serde_json::to_string(&format!("[[record:{}]]", log.display())).unwrap();
    let config = dir.join("protocol-v8-no-instruction.yaml");
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: split\n  skill:\n    kind: oneharness\n    bin: {harness}\n  judge:\n    kind: command\n    command: [{echo}, {record}, '[[supervisor-no-instruction]]']\ntask: go\nsystem_prompt: '[[fail:quota]]'\nuser:\n  persona: reviewer\n  max_turns: 2\n"
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let failure: onejudge::cli::FailureReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(failure.error.kind, Some(onejudge::ProviderErrorKind::Quota));
    assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(), 1);
}

#[test]
fn protocol_v8_mixed_panel_does_not_supervise_a_lost_turn() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let harness = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    let log = dir.join("protocol-v8-mixed-panel.jsonl");
    let _ = std::fs::remove_file(&log);
    let record = serde_json::to_string(&format!("[[record:{}]]", log.display())).unwrap();
    let config = dir.join("protocol-v8-mixed-panel.yaml");
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: split\n  skill:\n    kind: oneharness\n    bin: {harness}\n  judges:\n    - label: command\n      kind: command\n      command: [{echo}, {record}]\n    - label: model\n      kind: oneharness\n      bin: {harness}\ntask: go\nsystem_prompt: '[[fail:quota]]'\nuser:\n  persona: reviewer\n  max_turns: 2\n"
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!log.exists(), "the command judge was not asked: {output:?}");
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
    assert!(matches!(
        summary.eval_results[0].outcome,
        EvalOutcome::Boolean(true)
    ));
}

// --- Subprocess: the real `onejudge` binary --------------------------------

fn write_config(name: &str, body: &str) -> std::path::PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join(name);
    std::fs::write(&path, config_yaml(body)).unwrap();
    path
}

#[test]
fn binary_run_prints_the_run_as_text_by_default_and_exits_zero() {
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
    // A turn section, its tool call drawn once by oneharness's renderer, and the
    // closing summary — no raw JSON anywhere.
    assert!(stdout.starts_with("── turn 1 · worker ──"), "{stdout}");
    assert_eq!(
        stdout.matches("$ git commit -m fix\n").count(),
        1,
        "{stdout}"
    );
    assert!(
        stdout.contains("── done: completed after 1 turn"),
        "{stdout}"
    );
    assert!(stdout.contains("Status: completed"));
    assert!(!stdout.contains('{'), "{stdout}");
    // The `Usage:` line surfaces the aggregated prompt-cache reads/writes.
    assert!(stdout.contains("cache_read="), "usage shows cache reads");
    assert!(stdout.contains("cache_write="), "usage shows cache writes");
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
    let written = std::fs::read_to_string(dir.join("onejudge.yaml")).unwrap();
    assert!(Config::from_yaml(&written).is_ok());
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
    // No config file and no default in cwd: the run starts from an empty config,
    // whose default provider runs the turn through the linked oneharness engine.
    // With an empty PATH no harness can be found, so the turn fails — a classified
    // engine error, exit 2 — proving the flags-only path reached the run.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("no-cfg");
    std::fs::create_dir_all(&dir).unwrap();
    let _ = std::fs::remove_file(dir.join("onejudge.yaml"));
    let output = Command::new(onejudge_bin())
        .args(["run", "--task", "do a thing"])
        .current_dir(&dir)
        .env("PATH", "") // no harness can be found
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

    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .env("ONEJUDGE_TASK", "from the env")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("from the env"), "env task drives the run");
    assert!(!stdout.contains("from the file"), "env beats the file");

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

    let output = Command::new(onejudge_bin())
        .args(["run", path.to_str().unwrap()])
        .env_remove("ONEJUDGE_PROVIDER")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("command"));

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
    // `[[event]]` fires (drawn in the run's text) and the skill body's `[[done]]` ends
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
    assert!(
        output.status.success(),
        "skill body's [[done]] reached the harness"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Status: completed"));
    assert!(
        stdout.contains("$ git status"),
        "system prompt's event reached the harness: {stdout}"
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
    // Only the JSON stream *is* stdout; `--format text --stream` is accepted (see
    // `binary_text_format_is_live_streamable_aliased_and_writes_its_output`).
    let config = write_config("stream-misuse.yaml", "task: go\nsystem_prompt: Be warm.\n");
    let output = Command::new(onejudge_bin())
        .arg("run")
        .arg(config.to_str().unwrap())
        .args(["--stream", "--format", "json", "--output", "report.json"])
        .current_dir(Path::new(env!("CARGO_TARGET_TMPDIR")))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("drop --output"), "{stderr}");
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
    // The member set: a parsed object's iteration order follows how `serde_json`
    // was built, not what the binary wrote.
    let mut members: Vec<&String> = control.keys().collect();
    members.sort();
    assert_eq!(
        members,
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
            doubles::fake_harness(),
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
    let summary = run_plan(plan, Format::Text, &mut sink).unwrap();

    assert_eq!(summary.report.transcript.assistant_turns(), 1);
    assert_eq!(
        summary.report.transcript.messages[1].content,
        "done in process"
    );
    // Nothing was spawned by onejudge, which is the whole point of the default.
    assert!(summary.report.processes.is_empty());
    assert_eq!(exit_code(&summary), 0);
}

/// The agent side's `oneharness.toml` written two ways over the same fake-harness
/// double: `flat` states the harness selection itself, while the other states only
/// `extends` and its history directory, leaving the harness list and the pinned
/// `bin` to a parent file it names.
fn agent_config_project(name: &str, flat: bool) -> std::path::PathBuf {
    let dir = scratch_path(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("shared")).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "[[reply:committed]][[event:git commit -m fix]]",
    )
    .unwrap();
    let history = format!(
        "history_dir = {:?}\n",
        dir.join("history").display().to_string()
    );
    let selection = format!(
        "harnesses = [\"claude-code\"]\n\n[harness.claude-code]\nbin = {:?}\n",
        doubles::fake_harness(),
    );
    if flat {
        std::fs::write(dir.join("oneharness.toml"), history + &selection).unwrap();
    } else {
        std::fs::write(dir.join("shared/base.toml"), selection).unwrap();
        std::fs::write(
            dir.join("oneharness.toml"),
            format!("extends = \"shared/base.toml\"\n{history}"),
        )
        .unwrap();
    }
    dir
}

/// Spawn the built binary over an in-process, streaming `oneharness` provider whose
/// agent side runs from `dir`, returning its `--stream` event lines and report.
fn stream_agent_config_project(dir: &Path) -> (Vec<serde_json::Value>, onejudge::Report) {
    let config = dir.join("onejudge.yaml");
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: oneharness\n  stream: true\nskill: {}\ntask: please commit\n",
            serde_json::to_string(&dir.display().to_string()).unwrap()
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
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut events = Vec::new();
    let mut report = None;
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        match value["type"].as_str() {
            Some("event") => events.push(value),
            Some("result") => {
                report = Some(serde_json::from_value(value["report"].clone()).unwrap())
            }
            other => panic!("unexpected stream line type {other:?}"),
        }
    }
    (
        events,
        report.expect("the terminal result line carried the report"),
    )
}

#[test]
fn an_agent_config_that_extends_a_parent_runs_under_the_harness_only_the_parent_names() {
    // The linked `oneharness-core` resolves `extends` itself, so the agent's turn
    // runs under the fake harness pinned only in the parent — no harness list, no
    // `bin` in the file onejudge's run discovers. Against a core that predates
    // `extends` the child is an unknown-key config error and the run fails.
    let flat_dir = agent_config_project("cli-agent-config-flat", true);
    let extends_dir = agent_config_project("cli-agent-config-extends", false);
    let (flat_events, flat) = stream_agent_config_project(&flat_dir);
    let (events, report) = stream_agent_config_project(&extends_dir);

    assert_eq!(report.transcript.messages[1].content, "committed");
    assert_eq!(
        events.len(),
        1,
        "the tool event streamed from the parent's harness"
    );
    assert_eq!(events[0]["event"]["name"], "Bash");
    // Which harness each invocation ran and was offered — the part of the
    // attribution a run's timings, paths and session ids do not vary.
    let ran = |report: &onejudge::Report| -> Vec<(Option<String>, Vec<String>)> {
        let telemetry = report
            .telemetry
            .as_ref()
            .expect("telemetry reaches the report");
        telemetry
            .attribution
            .iter()
            .map(|a| {
                let offered = a.candidates.iter().map(|c| c.harness.clone()).collect();
                (a.ran.clone(), offered)
            })
            .collect()
    };
    assert_eq!(
        ran(&report),
        [(
            Some("claude-code".to_string()),
            vec!["claude-code".to_string()]
        )]
    );

    // What onejudge parses from the run is what it parses for a flat config.
    assert_eq!(events, flat_events);
    assert_eq!(report.transcript, flat.transcript);
    assert_eq!(report.verdicts, flat.verdicts);
    assert_eq!(ran(&report), ran(&flat));
    assert!(report.processes.is_empty() && flat.processes.is_empty());

    let _ = std::fs::remove_dir_all(&flat_dir);
    let _ = std::fs::remove_dir_all(&extends_dir);
}

#[test]
fn an_observing_plan_run_reports_the_conversation_and_still_returns_its_report() {
    // The entry point an embedder that drives a `Plan` — rather than building
    // providers itself — watches a dispatch through. The whole run driver still
    // runs: the loop, the `done_when` re-judge, the evals, the report.
    let body = "\
task: please commit
system_prompt: 'Commit it. [[event:git commit -m fix]]'
user:
  persona: A tester.
  done_when: git commit
  max_turns: 5
";
    let plan = Config::from_yaml(&config_yaml(body))
        .unwrap()
        .into_plan()
        .unwrap();

    let mut seen: Vec<String> = Vec::new();
    let summary = run_plan_observing_reporting_failure(plan, &mut |observation| {
        seen.push(match observation {
            Observation::TurnOpened(o) => format!("opened/{:?}/{}", o.role, o.instruction),
            Observation::Tool(e) => format!("tool/{}", e.event.summary()),
            Observation::Action(a) => format!("action/{}", a.event.kind),
            Observation::Message(m) => format!("said/{:?}/{}", m.role, m.text),
            Observation::TurnClosed(c) => format!("closed/{:?}/{}", c.role, c.usage.is_some()),
            Observation::JudgeDecided(d) => {
                format!("judged/{}/{}/{}", d.judge, d.decision.as_str(), d.reason)
            }
            Observation::JudgeTool(t) => format!("judge-tool/{}/{}", t.judge, t.event.summary()),
        });
        ControlFlow::Continue(())
    })
    .unwrap();

    assert!(summary.completed);
    assert_eq!(summary.report.transcript.assistant_turns(), 1);
    assert_eq!(
        seen,
        vec![
            "opened/Assistant/please commit".to_string(),
            r#"tool/bash({"command":"git commit -m fix"})"#.to_string(),
            "action/tool_call".to_string(),
            "said/Assistant/echo: please commit".to_string(),
            "closed/Assistant/true".to_string(),
            // The supervisor completed the run, so it appended nothing to the
            // transcript: its turn is bounds and cost with no message between.
            "opened/User/echo: please commit".to_string(),
            "closed/User/true".to_string(),
        ],
        "the whole conversation reached the observer, not just its tool calls"
    );
    // The judge calls that follow the loop are not part of the conversation, so
    // they are not observed as turns of it.
    assert_eq!(summary.report.verdicts.len(), 1);
}

#[test]
fn an_observing_plan_run_that_fails_reports_the_failure_after_the_turn_it_opened() {
    // A provider that cannot even spawn: the observer still learns the turn opened
    // and what it was asked to do, and the failure comes back attributable rather
    // than as a silent empty run.
    let missing = serde_json::to_string("onejudge-no-such-binary-zzz").unwrap();
    let yaml = format!(
        "provider:\n  kind: command\n  command: [{missing}]\n\
         task: please commit\nsystem_prompt: Commit it.\n"
    );
    let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();

    let mut seen = Vec::new();
    let Err(failure) = run_plan_observing_reporting_failure(plan, &mut |observation| {
        seen.push(matches!(observation, Observation::TurnOpened(_)));
        ControlFlow::Continue(())
    }) else {
        panic!("the provider cannot be spawned")
    };

    assert_eq!(seen, vec![true], "the turn was observed opening");
    assert!(
        failure
            .error
            .to_string()
            .contains("onejudge-no-such-binary"),
        "{}",
        failure.error
    );
}

// --- The judge side as a list: `judges:` through the plan and the binary ------
//
// Every journey here drives the same panel the engine e2e proves, through the
// two entry points a CLI consumer has — a `Plan` in process and the built binary
// — asserting on what each surface prints or returns for it.

/// A `split` over the echo double on both sides whose judge side is `judges`,
/// each entry a YAML mapping body (`kind: command` and the argv are supplied).
fn panel_config_yaml(judges: &[(&str, &[&str])], body: &str) -> String {
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let mut yaml = format!(
        "provider:\n  kind: split\n  skill:\n    kind: command\n    command: [{echo}]\n  judges:\n"
    );
    for (label, markers) in judges {
        let argv: Vec<String> = std::iter::once(echo.clone())
            .chain(markers.iter().map(|m| serde_json::to_string(m).unwrap()))
            .collect();
        yaml.push_str(&format!(
            "    - kind: command\n      label: {label}\n      command: [{}]\n",
            argv.join(", ")
        ));
    }
    yaml.push_str(body);
    yaml
}

/// The two-judge panel most journeys drive: `reviewer` passes the work, `lint`
/// sends the worker back once and then passes it.
fn reviewer_and_lint(body: &str) -> String {
    panel_config_yaml(
        &[
            ("reviewer", &["[[supervisor-complete:looks right]]"]),
            ("lint", &[]),
        ],
        body,
    )
}

const TWO_TURN_BODY: &str = "\
task: please commit
system_prompt: Commit it.
user:
  persona: A tester.
  done_when: next step
  max_turns: 4
";

#[test]
fn binary_run_json_reports_each_judges_decision_and_labels_the_judge_side() {
    // The report a real run writes: `judge_decisions` one entry per supervisor
    // turn with each judge attributed, and the panel label riding every judge-side
    // process record — absent from the agent side's.
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("panel.yaml");
    std::fs::write(&config, reviewer_and_lint(TWO_TURN_BODY)).unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: onejudge::Report = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report.schema_version, onejudge::SCHEMA_VERSION);
    // Turn 1: the echo judge asks for the next step while the reviewer passes; the
    // worker is handed the lint judge's block alone. Turn 2: both pass.
    assert_eq!(
        report.transcript.messages[2].content,
        "## Judge `lint` (command)\n\nThanks — and what about the next step?"
    );
    /// One supervisor turn's decisions as `(judge, kind, decision)`.
    type Decided<'a> = Vec<(&'a str, &'a str, onejudge::Decision)>;
    let decided: Vec<(usize, Decided<'_>)> = report
        .judge_decisions
        .iter()
        .map(|turn| {
            (
                turn.turn,
                turn.decisions
                    .iter()
                    .map(|d| (d.judge.as_str(), d.kind.as_str(), d.decision))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        decided,
        vec![
            (
                1,
                vec![
                    ("reviewer", "command", onejudge::Decision::Done),
                    ("lint", "command", onejudge::Decision::Continue),
                ]
            ),
            (
                2,
                vec![
                    ("reviewer", "command", onejudge::Decision::Done),
                    ("lint", "command", onejudge::Decision::Done),
                ]
            ),
        ]
    );
    assert_eq!(
        report.completion_reason.as_deref(),
        Some("[reviewer] looks right; [lint] completion criterion found in transcript")
    );
    for process in &report.processes {
        match process.role {
            onejudge::TelemetryRole::Agent => assert_eq!(process.judge, None, "{process:?}"),
            onejudge::TelemetryRole::Judge if process.op == "supervisor" => assert!(
                matches!(process.judge.as_deref(), Some("reviewer" | "lint")),
                "{process:?}"
            ),
            // The final re-judge of `done_when` is a panel call too.
            onejudge::TelemetryRole::Judge => assert!(process.judge.is_some(), "{process:?}"),
        }
    }
    let raw: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(raw["judge_decisions"].is_array());
}

#[test]
fn binary_run_text_prints_each_judges_decision_and_the_feedback_under_its_turn() {
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("panel-human.yaml");
    std::fs::write(&config, reviewer_and_lint(TWO_TURN_BODY)).unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Everything before the closing summary; a turn's duration is a measurement,
    // so it is read as a shape.
    let run: Vec<&str> = stdout
        .split("── done:")
        .next()
        .expect("the run's sections")
        .lines()
        .collect();
    let rule = |title: &str| {
        let head = format!("── {title} ");
        format!("{head}{}", "─".repeat(64 - head.chars().count()))
    };
    let closed = |line: &str, reply: &str| {
        line.starts_with("  done in ") && line.ends_with(&format!(" · {reply}"))
    };
    assert_eq!(run[0], rule("turn 1 · worker"), "{stdout}");
    assert_eq!(run[1], "task: please commit");
    assert!(closed(run[2], "echo: please commit"), "{stdout}");
    assert_eq!(
        run[3..8],
        [
            rule("turn 1 · judges").as_str(),
            "reviewer  done  looks right",
            "lint  continue  completion criterion not yet met",
            "feedback → worker: ## Judge `lint` (command)",
            "",
        ],
        "{stdout}"
    );
    assert_eq!(
        run[8],
        "                   Thanks — and what about the next step?"
    );
    assert_eq!(run[9], rule("turn 2 · worker ← feedback"));
    assert!(
        closed(run[10], "echo: ## Judge `lint` (command) (+1 line)"),
        "{stdout}"
    );
    assert_eq!(
        run[11..],
        [
            rule("turn 2 · judges").as_str(),
            "reviewer  done  looks right",
            "lint  done  completion criterion found in transcript",
        ],
        "{stdout}"
    );
    assert!(stdout.contains("Status: completed"), "{stdout}");
}

#[test]
fn binary_stream_result_line_carries_the_judge_decisions() {
    // The NDJSON protocol is unchanged — `event* result EOF` — and the decisions
    // reach a consumer on the `result` line's report.
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("panel-stream.yaml");
    std::fs::write(&config, reviewer_and_lint(TWO_TURN_BODY)).unwrap();
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
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "no tool events, so the result line alone");
    assert_eq!(lines[0]["type"], "result");
    let report: onejudge::Report = serde_json::from_value(lines[0]["report"].clone()).unwrap();
    assert_eq!(report.judge_decisions.len(), 2);
    assert_eq!(report.judge_decisions[0].decisions[1].judge, "lint");
}

#[test]
fn binary_run_json_writes_both_judges_decisions_into_the_failure_report() {
    // One judge fails its supervisor call while the other decides: the run fails
    // (exit 2) naming the judge, and the failure document carries the turn that
    // failed with BOTH judges' decisions — the failed one as `error`.
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("panel-failure.yaml");
    std::fs::write(
        &config,
        panel_config_yaml(
            &[
                ("reviewer", &["[[supervisor-exit]]"]),
                (
                    "lint",
                    &[
                        "[[supervisor-sleep:300]]",
                        "[[supervisor-continue:Fix it.]]",
                    ],
                ),
            ],
            TWO_TURN_BODY,
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("provider error (supervise[reviewer])"),
        "{stderr}"
    );
    let failure: onejudge::cli::FailureReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(failure.schema_version, onejudge::SCHEMA_VERSION);
    assert_eq!(
        failure.error.kind,
        Some(onejudge::ProviderErrorKind::Protocol)
    );
    assert_eq!(failure.judge_decisions.len(), 1);
    let turn = &failure.judge_decisions[0];
    assert_eq!(turn.turn, 1);
    assert_eq!(turn.decisions[0].judge, "reviewer");
    assert_eq!(turn.decisions[0].decision, onejudge::Decision::Error);
    assert_eq!(turn.decisions[1].judge, "lint");
    assert_eq!(turn.decisions[1].decision, onejudge::Decision::Continue);
    assert!(failure
        .processes
        .iter()
        .filter(|p| p.role == onejudge::TelemetryRole::Judge)
        .all(|p| p.judge.is_some()));
}

#[test]
fn binary_refuses_a_malformed_judge_list_naming_the_field() {
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    for (name, judges, needle) in [
        (
            "both",
            format!(
                "  judge:\n    kind: command\n    command: [{echo}]\n  judges:\n    - kind: command\n      command: [{echo}]\n"
            ),
            "`judge` and `judges` are exclusive",
        ),
        ("empty", "  judges: []\n".to_string(), "`judges` must name at least one judge"),
        (
            "dup",
            format!(
                "  judges:\n    - kind: command\n      command: [{echo}]\n      label: same\n    - kind: command\n      command: [{echo}]\n      label: same\n"
            ),
            "`label` `same` is used by more than one judge",
        ),
        (
            "label-on-skill",
            format!("  judge:\n    kind: command\n    command: [{echo}]\n"),
            "`label` is only valid on a judge entry",
        ),
    ] {
        let skill_label = if name == "label-on-skill" {
            "    label: agent\n"
        } else {
            ""
        };
        let yaml = format!(
            "provider:\n  kind: split\n  skill:\n    kind: command\n    command: [{echo}]\n{skill_label}{judges}task: go\n"
        );
        let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("panel-bad-{name}.yaml"));
        std::fs::write(&config, yaml).unwrap();
        let output = Command::new(onejudge_bin())
            .args(["run", config.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{name}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(needle), "{name}: {stderr}");
    }
}

#[test]
fn a_plans_spawn_hook_reaches_every_judge_of_the_panel() {
    let hook = std::sync::Arc::new(OneGroup::default());
    let installed: onejudge::SharedSpawnHook = hook.clone();
    let plan = Config::from_yaml(&reviewer_and_lint(TWO_TURN_BODY))
        .unwrap()
        .into_plan()
        .unwrap()
        .with_spawn_hook(installed);
    let mut sink = |_: &str| {};
    let summary = run_plan(plan, Format::Json, &mut sink).unwrap();
    let seen = hook.seen.lock().unwrap().clone();
    // Every process of the run — the agent's and each judge's — was offered and
    // grouped, and the report names each judge's beside its group.
    assert_eq!(summary.report.processes.len(), seen.len());
    // Records are collected judge by judge (each judge keeps its own spawner), so
    // two supervisor turns give each judge two labelled records.
    let judged: Vec<&str> = summary
        .report
        .processes
        .iter()
        .filter(|p| p.op == "supervisor")
        .map(|p| p.judge.as_deref().unwrap())
        .collect();
    assert_eq!(judged, ["reviewer", "reviewer", "lint", "lint"]);
    assert!(summary
        .report
        .processes
        .iter()
        .all(|p| p.group.as_deref() == Some("job:plan-1")));
}

#[test]
fn an_observing_plan_run_delivers_each_judges_decision_inside_the_supervisor_turn() {
    // Contract C's ordering, through the plan-level observer: after the supervisor
    // turn opens, one `judge_decided` per judge in list order, then the turn's
    // message (when it continued) and its close.
    let plan = Config::from_yaml(&reviewer_and_lint(TWO_TURN_BODY))
        .unwrap()
        .into_plan()
        .unwrap();
    let mut seen: Vec<String> = Vec::new();
    let summary = run_plan_observing_reporting_failure(plan, &mut |observation| {
        seen.push(match observation {
            Observation::TurnOpened(o) => format!("opened/{:?}", o.role),
            Observation::Tool(e) => format!("tool/{}", e.event.summary()),
            Observation::Action(a) => format!("action/{}", a.event.kind),
            Observation::Message(m) => format!("said/{:?}", m.role),
            Observation::TurnClosed(c) => format!("closed/{:?}", c.role),
            Observation::JudgeDecided(d) => {
                format!("judged/{}/{}/{}", d.turn, d.judge, d.decision.as_str())
            }
            Observation::JudgeTool(t) => format!("judge-tool/{}/{}", t.turn, t.judge),
        });
        ControlFlow::Continue(())
    })
    .unwrap();
    assert!(summary.completed);
    assert_eq!(
        seen,
        vec![
            "opened/Assistant",
            "said/Assistant",
            "closed/Assistant",
            "opened/User",
            "judged/1/reviewer/done",
            "judged/1/lint/continue",
            "said/User",
            "closed/User",
            "opened/Assistant",
            "said/Assistant",
            "closed/Assistant",
            "opened/User",
            "judged/2/reviewer/done",
            "judged/2/lint/done",
            "closed/User",
        ]
    );

    // …and when the supervisor call fails, every decision is delivered before the
    // error propagates, and the failure carries them too.
    let plan = Config::from_yaml(&panel_config_yaml(
        &[
            ("reviewer", &["[[supervisor-exit]]"]),
            ("lint", &["[[supervisor-continue:Fix it.]]"]),
        ],
        TWO_TURN_BODY,
    ))
    .unwrap()
    .into_plan()
    .unwrap();
    let mut seen: Vec<String> = Vec::new();
    let Err(failure) = run_plan_observing_reporting_failure(plan, &mut |observation| {
        seen.push(match observation {
            Observation::TurnOpened(o) => format!("opened/{:?}", o.role),
            Observation::JudgeDecided(d) => format!("judged/{}/{}", d.judge, d.decision.as_str()),
            Observation::Message(m) => format!("said/{:?}", m.role),
            Observation::TurnClosed(c) => format!("closed/{:?}", c.role),
            Observation::Tool(_) => "tool".to_string(),
            Observation::Action(_) => "action".to_string(),
            Observation::JudgeTool(t) => format!("judge-tool/{}", t.judge),
        });
        ControlFlow::Continue(())
    }) else {
        panic!("a judge that exits non-zero fails the run")
    };
    assert_eq!(
        seen[3..],
        [
            "opened/User",
            "judged/reviewer/error",
            "judged/lint/continue"
        ],
        "{seen:?}"
    );
    assert_eq!(failure.judge_decisions.len(), 1);
    assert_eq!(
        failure.judge_decisions[0].decisions[0].decision,
        onejudge::Decision::Error
    );
    assert!(failure.error.to_string().contains("supervise[reviewer]"));
}

// --- `kind: llmlint`: a judge met at the process boundary ---------------------
//
// The llmlint judge through the two entry points a CLI consumer has, over the
// `onejudge-fake-llmlint` double (a stand-in for the `llmlint` CLI, scripted
// through the environment the run inherits — see its module doc). Only
// llmlint's own verdict is faked; the config layer, the panel, the run driver
// and the built binary are all real.

fn fake_llmlint_bin() -> &'static str {
    doubles::fake_llmlint()
}

/// The argv lines the double recorded at `path`, in arrival order.
fn recorded_llmlint_argv(path: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(path)
        .expect("the double recorded its argv")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The report the failing double writes (its module doc), as the provider hands
/// it on: trailing whitespace trimmed, nothing else touched.
const LLMLINT_FAILING_REPORT: &str = "\
FAIL  scripts_are_quiet_on_success
  scripts/release-probe.sh:12: prints a banner on every successful run
  rationale: a script that succeeds should print one line or nothing
1 failed, 3 passed, 0 skipped, 0 not relevant";
const LLMLINT_FAILING_SUMMARY: &str = "1 failed, 3 passed, 0 skipped, 0 not relevant";
const LLMLINT_CLEAN_SUMMARY: &str = "0 failed, 4 passed, 0 skipped, 0 not relevant";

/// A `split` over the echo skill whose judges are one LLM judge entry (`llm`, a
/// YAML mapping body under `- `) and one llmlint judge carrying every field the
/// kind takes, with `body` appended.
fn llmlint_panel_yaml(llm: &str, body: &str) -> String {
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let llmlint = serde_json::to_string(fake_llmlint_bin()).unwrap();
    format!(
        "provider:\n  kind: split\n  skill:\n    kind: command\n    command: [{echo}]\n  judges:\n\
         {llm}\
         \x20   - kind: llmlint\n      label: lint\n      bin: {llmlint}\n\
         \x20     config: llmlint.strict.yml\n      diff_base: origin/main\n\
         \x20     args: [--rule, scripts_are_quiet_on_success]\n\
         {body}"
    )
}

/// The one-judge-fails-then-passes body: `done_when` appears in the transcript
/// from the first message, so the LLM judge finds it satisfied on every turn.
const LLMLINT_BODY: &str = "\
task: please commit
system_prompt: Commit it.
user:
  persona: A tester.
  done_when: commit
  max_turns: 4
";

/// The base session every llmlint binary run below is named after.
const LLMLINT_SESSION: &str = "lint-loop";

/// Run the built binary over `config` under `--session` [`LLMLINT_SESSION`] and
/// `--format <format>`, with the double scripted to `exits`, recording its argv
/// at the returned path.
fn run_binary_with_llmlint(
    name: &str,
    config: &Path,
    exits: &str,
    format: &str,
) -> (std::process::Output, std::path::PathBuf) {
    let argv = scratch_path(&format!("cli-llmlint-{name}.argv.jsonl"));
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", format])
        .args(["--session", LLMLINT_SESSION])
        .env("ONEJUDGE_FAKE_LLMLINT_ARGV", &argv)
        .env("ONEJUDGE_FAKE_LLMLINT_EXIT", exits)
        .env_remove("ONEJUDGE_FAKE_LLMLINT_STDOUT")
        .env_remove("ONEJUDGE_FAKE_LLMLINT_STDERR")
        .env_remove("ONEJUDGE_FAKE_LLMLINT_POINTER")
        .env_remove("ONEJUDGE_FAKE_LLMLINT_VERSION")
        .output()
        .unwrap();
    (output, argv)
}

/// One decision's link as `(turn, judge, labels, run_id)`.
type LinkedDecision<'a> = (usize, &'a str, &'a Labels, Option<&'a str>);

type Labels = std::collections::BTreeMap<String, String>;

/// The labels the `lint` judge's run deciding on `turn` is passed, as the map
/// its decision records.
fn llmlint_labels(turn: usize) -> std::collections::BTreeMap<String, String> {
    [
        ("session", LLMLINT_SESSION.to_string()),
        ("judge", "lint".to_string()),
        ("turn", turn.to_string()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect()
}

/// The argv the contract spells for one `lint` run deciding on `turn`, with
/// every field of the `lint` entry above rendered exactly where the contract
/// puts it: the labels after `-c` / `--diff` and before the entry's own `args`.
/// The worktree is `.`: a config with no `skill:` runs the agent in the working
/// directory.
fn llmlint_lint_argv(turn: usize) -> Vec<String> {
    let session = format!("session={LLMLINT_SESSION}");
    let turn = format!("turn={turn}");
    [
        "lint",
        "--cwd",
        ".",
        "--format",
        "human",
        "--color",
        "never",
        "--progress",
        "never",
        "-c",
        "llmlint.strict.yml",
        "--diff",
        "--diff-base",
        "origin/main",
        "--label",
        &session,
        "--label",
        "judge=lint",
        "--label",
        &turn,
        "--rule",
        "scripts_are_quiet_on_success",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// One judge's decision on one supervisor turn as `(judge, kind, decision, reason)`.
type DecidedBy = (String, String, onejudge::Decision, String);

fn decided(report: &onejudge::Report) -> Vec<(usize, Vec<DecidedBy>)> {
    report
        .judge_decisions
        .iter()
        .map(|turn| {
            (
                turn.turn,
                turn.decisions
                    .iter()
                    .map(|d| {
                        (
                            d.judge.clone(),
                            d.kind.clone(),
                            d.decision,
                            d.reason.clone(),
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn binary_run_hands_the_worker_llmlints_report_under_its_header_and_composes_its_argv() {
    // A command (echo) reviewer that passes the work beside an llmlint judge whose
    // first run fails and whose second is clean: the worker's next user turn is
    // llmlint's report verbatim under its header — nothing from the reviewer —
    // the run then completes with both reasons attributed, the final `done_when`
    // re-judge is the conjunction (true), and every `lint` run's argv is exactly
    // what the contract spells.
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("llmlint-reviewer.yaml");
    std::fs::write(
        &config,
        llmlint_panel_yaml(
            &format!(
                "    - kind: command\n      label: reviewer\n      command: [{echo}, \"[[supervisor-complete:looks right]]\"]\n"
            ),
            LLMLINT_BODY,
        ),
    )
    .unwrap();
    let (output, argv) = run_binary_with_llmlint("reviewer", &config, "1,0", "json");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: onejudge::Report = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report.schema_version, onejudge::SCHEMA_VERSION);

    let expected = format!("## Judge `lint` (llmlint)\n\n{LLMLINT_FAILING_REPORT}");
    assert_eq!(report.transcript.messages[2].content, expected);
    assert_eq!(
        report.transcript.messages[3].content,
        format!("echo: {expected}"),
        "the worker was handed it"
    );
    assert_eq!(
        decided(&report),
        vec![
            (
                1,
                vec![
                    (
                        "reviewer".into(),
                        "command".into(),
                        onejudge::Decision::Done,
                        "looks right".into()
                    ),
                    (
                        "lint".into(),
                        "llmlint".into(),
                        onejudge::Decision::Continue,
                        LLMLINT_FAILING_SUMMARY.into()
                    ),
                ]
            ),
            (
                2,
                vec![
                    (
                        "reviewer".into(),
                        "command".into(),
                        onejudge::Decision::Done,
                        "looks right".into()
                    ),
                    (
                        "lint".into(),
                        "llmlint".into(),
                        onejudge::Decision::Done,
                        LLMLINT_CLEAN_SUMMARY.into()
                    ),
                ]
            ),
        ]
    );
    assert_eq!(
        report.completion_reason.as_deref(),
        Some(format!("[reviewer] looks right; [lint] {LLMLINT_CLEAN_SUMMARY}").as_str())
    );
    // The authoritative re-judge of `done_when` is the conjunction: the reviewer
    // finds `commit` in the transcript and llmlint's third run is clean.
    let done = report
        .verdicts
        .iter()
        .find(|v| v.criterion == "commit")
        .expect("done_when verdict");
    assert_eq!(done.verdict.value, onejudge::JudgeValue::Bool(true));
    assert_eq!(
        done.verdict.reason,
        format!("[reviewer] criterion found in transcript; [lint] {LLMLINT_CLEAN_SUMMARY}")
    );

    // The probe, then one `lint` run per decision: two supervisor turns and the
    // final re-judge, each with the exact argv — labelled with the run's
    // `--session`, the judge's panel label, and the turn it decides on (the
    // re-judge decides on the last assistant turn, 2).
    assert_eq!(
        recorded_llmlint_argv(&argv),
        vec![
            vec!["--version".to_string()],
            llmlint_lint_argv(1),
            llmlint_lint_argv(2),
            llmlint_lint_argv(2),
        ]
    );
    // Each llmlint decision links to its run: `judge` is the decision's own
    // `judge`, `turn` its `JudgedTurn.turn`, and `run_id` the id that run's
    // stderr pointer named. The reviewer (a command judge) carries neither key —
    // not even empty — on the wire.
    let links: Vec<LinkedDecision<'_>> = report
        .judge_decisions
        .iter()
        .flat_map(|turn| {
            turn.decisions
                .iter()
                .map(move |d| (turn.turn, d.judge.as_str(), &d.labels, d.run_id.as_deref()))
        })
        .collect();
    let none = std::collections::BTreeMap::new();
    assert_eq!(
        links,
        [
            (1, "reviewer", &none, None),
            (1, "lint", &llmlint_labels(1), Some("fake-lint-1")),
            (2, "reviewer", &none, None),
            (2, "lint", &llmlint_labels(2), Some("fake-lint-2")),
        ]
    );
    for turn in &report.judge_decisions {
        for decision in &turn.decisions {
            if decision.kind == "llmlint" {
                assert_eq!(decision.labels["judge"], decision.judge);
                assert_eq!(decision.labels["turn"], turn.turn.to_string());
            }
        }
    }
    let wire: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let reviewer = &wire["judge_decisions"][0]["decisions"][0];
    assert_eq!(reviewer["judge"], "reviewer");
    assert!(reviewer.get("labels").is_none(), "{reviewer}");
    assert!(reviewer.get("run_id").is_none(), "{reviewer}");
    // One judge-side process record per run, under the judge's label, and its
    // wall time on the judge side's telemetry (the echo judge reports none).
    let lint_processes: Vec<(&str, &str)> = report
        .processes
        .iter()
        .filter(|p| p.judge.as_deref() == Some("lint"))
        .map(|p| (p.op.as_str(), p.program.as_str()))
        .collect();
    assert_eq!(
        lint_processes,
        [
            ("supervise", fake_llmlint_bin()),
            ("supervise", fake_llmlint_bin()),
            ("judge", fake_llmlint_bin()),
        ]
    );
    let telemetry = report.telemetry.expect("telemetry");
    assert!(telemetry.judge.tool_ms.is_some(), "{telemetry:#?}");
}

#[test]
fn binary_text_output_prints_the_llmlint_history_command_under_each_llmlint_decision() {
    // What the wrapper reads a run by: every llmlint decision's `llmlint history
    // <run_id>` on the line under it, tied to the turn it decided on — and no
    // such line under the reviewer's.
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("llmlint-human.yaml");
    std::fs::write(
        &config,
        llmlint_panel_yaml(
            &format!(
                "    - kind: command\n      label: reviewer\n      command: [{echo}, \"[[supervisor-complete:looks right]]\"]\n"
            ),
            LLMLINT_BODY,
        ),
    )
    .unwrap();
    // `human`, the text view's older name, still selects it.
    let (output, _) = run_binary_with_llmlint("human", &config, "1,0", "human");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(&format!(
            "reviewer  done  looks right\n\
             lint  continue  {LLMLINT_FAILING_SUMMARY}\n\
             \x20 llmlint history fake-lint-1\n"
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "lint  done  {LLMLINT_CLEAN_SUMMARY}\n\
             \x20 llmlint history fake-lint-2"
        )),
        "{stdout}"
    );
    assert_eq!(stdout.matches("llmlint history ").count(), 2, "{stdout}");
}

#[test]
fn binary_run_stacks_an_llm_judge_on_an_llmlint_judge_and_hands_the_worker_only_llmlints_output() {
    // The stack the kind exists for: an LLM reviewer (the fake oneharness) that
    // passes the work beside an llmlint judge that does not. Only llmlint's output
    // reaches the worker, under its header; the report records `done` for one
    // judge and `continue` for the other on that turn; and the numeric eval and
    // assessment the config also asks for are answered by the LLM judge alone.
    let oh = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("llmlint-stacked.yaml");
    std::fs::write(
        &config,
        llmlint_panel_yaml(
            &format!("    - kind: oneharness\n      label: reviewer\n      bin: {oh}\n"),
            &format!(
                "{LLMLINT_BODY}\
                 evals:\n  - criterion: commit\n    kind: numeric\n    scale: [1, 5]\n\
                 assessment: What was left out?\n"
            ),
        ),
    )
    .unwrap();
    let (output, argv) = run_binary_with_llmlint("stacked", &config, "1,0", "json");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: onejudge::Report = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report.transcript.messages[2].content,
        format!("## Judge `lint` (llmlint)\n\n{LLMLINT_FAILING_REPORT}")
    );
    let turn_one: Vec<(&str, &str, onejudge::Decision)> = report.judge_decisions[0]
        .decisions
        .iter()
        .map(|d| (d.judge.as_str(), d.kind.as_str(), d.decision))
        .collect();
    assert_eq!(
        turn_one,
        [
            ("reviewer", "oneharness", onejudge::Decision::Done),
            ("lint", "llmlint", onejudge::Decision::Continue),
        ]
    );
    assert_eq!(
        report.completion_reason.as_deref(),
        Some(
            format!("[reviewer] fake supervisor found criterion; [lint] {LLMLINT_CLEAN_SUMMARY}")
                .as_str()
        )
    );
    // The numeric eval and the assessment were answered without llmlint: its argv
    // record holds exactly the runs it can answer — two supervisor decisions and
    // the boolean `done_when` re-judge — and nothing for the number or the prose.
    let numeric = report
        .verdicts
        .iter()
        .find(|v| v.kind == onejudge::JudgeKind::Numeric)
        .expect("the numeric eval verdict");
    assert_eq!(numeric.verdict.value, onejudge::JudgeValue::Number(5.0));
    assert_eq!(numeric.verdict.reason, "[reviewer] fake numeric");
    assert_eq!(
        report.assessment.as_deref(),
        Some("## Judge `reviewer` (oneharness)\n\nNo follow-up work remains.")
    );
    assert_eq!(recorded_llmlint_argv(&argv).len(), 1 + 3);
}

#[test]
fn an_absent_llmlint_is_a_config_error_at_plan_build_before_any_turn() {
    // The probe runs where the provider is built, so a missing executable is
    // refused naming the binary and the `bin` field — with nothing spawned, no
    // telemetry and no turn — through the plan driver and the binary alike.
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let skill_log = scratch_path("llmlint-absent-skill.jsonl");
    // JSON-quoted so a Windows path's backslashes survive the YAML scalar.
    let record = serde_json::to_string(&format!("[[record:{}]]", skill_log.display())).unwrap();
    let missing = "onejudge-no-such-llmlint-zzz";
    let yaml = format!(
        "provider:\n  kind: split\n  skill:\n    kind: command\n    command: [{echo}, {record}]\n  \
         judge:\n    kind: llmlint\n    bin: {missing}\n{LLMLINT_BODY}"
    );
    let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();
    let mut sink = |_: &str| {};
    let Err(failure) = onejudge::cli::run_plan_reporting_failure(plan, Format::Json, &mut sink)
    else {
        panic!("the provider cannot be built")
    };
    let onejudge::cli::CliError::Config(message) = &failure.error else {
        panic!("a config error, not {}", failure.error)
    };
    assert!(message.contains(missing), "{message}");
    assert!(message.contains("`bin`"), "{message}");
    assert_eq!(failure.telemetry, None);
    assert!(failure.processes.is_empty());
    assert!(failure.judge_decisions.is_empty());
    assert!(!skill_log.exists(), "the agent never ran a turn");

    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("llmlint-absent.yaml");
    std::fs::write(&config, &yaml).unwrap();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("config error"), "{stderr}");
    assert!(stderr.contains(missing), "{stderr}");
    assert!(!skill_log.exists(), "the agent never ran a turn");
}

#[test]
fn binary_refuses_llmlint_anywhere_but_a_judge_entry_and_every_foreign_field() {
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let llmlint = serde_json::to_string(fake_llmlint_bin()).unwrap();
    let split_head =
        format!("provider:\n  kind: split\n  skill:\n    kind: command\n    command: [{echo}]\n");
    let judge_entry = |field: &str| {
        format!("{split_head}  judge:\n    kind: llmlint\n    bin: {llmlint}\n    {field}\n")
    };
    let belongs = "put it under `provider.judges:`";
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "top-level",
            format!("provider:\n  kind: llmlint\n  bin: {llmlint}\n"),
            belongs,
        ),
        (
            "under-skill",
            format!(
                "provider:\n  kind: split\n  skill:\n    kind: llmlint\n    bin: {llmlint}\n  \
                 judge:\n    kind: command\n    command: [{echo}]\n"
            ),
            "not valid as `provider.skill`",
        ),
        (
            "judge-config",
            judge_entry("judge_config: x.toml"),
            "`judge_config` is not valid under provider kind `llmlint`",
        ),
        (
            "stream",
            judge_entry("stream: true"),
            "`stream` is not valid under provider kind `llmlint`",
        ),
        (
            "control",
            judge_entry("control: true"),
            "`control` is not valid under provider kind `llmlint`",
        ),
        (
            "mock-harness",
            judge_entry("mock_harness: [x]"),
            "`mock_harness` is not valid under provider kind `llmlint`",
        ),
        (
            "command",
            judge_entry("command: [x]"),
            "`command` is not valid under provider kind `llmlint`",
        ),
        (
            "skill",
            judge_entry("skill: {kind: command, command: [x]}"),
            "`skill` is not valid under provider kind `llmlint`",
        ),
        (
            "judge",
            judge_entry("judge: {kind: command, command: [x]}"),
            "`judge` is not valid under provider kind `llmlint`",
        ),
        (
            "judges",
            judge_entry("judges: [{kind: command, command: [x]}]"),
            "`judges` is not valid under provider kind `llmlint`",
        ),
        (
            "blank-bin",
            judge_entry("bin: ' '").replace(&format!("bin: {llmlint}\n    "), ""),
            "`bin` under provider kind `llmlint` must name",
        ),
        (
            "config-under-oneharness",
            "provider:\n  kind: oneharness\n  config: llmlint.yml\n".to_string(),
            "`config` is not valid under provider kind `oneharness`",
        ),
        (
            "diff-base-under-command",
            format!("provider:\n  kind: command\n  command: [{echo}]\n  diff_base: main\n"),
            "`diff_base` is not valid under provider kind `command`",
        ),
        (
            "args-under-split",
            format!(
                "{split_head}  judge:\n    kind: command\n    command: [{echo}]\n  args: [x]\n"
            ),
            "`args` is not valid under provider kind `split`",
        ),
    ];
    for (name, provider, needle) in cases {
        let config =
            Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("llmlint-bad-{name}.yaml"));
        std::fs::write(&config, format!("{provider}task: go\n")).unwrap();
        let output = Command::new(onejudge_bin())
            .args(["run", config.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{name}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("config error"), "{name}: {stderr}");
        assert!(stderr.contains(needle), "{name}: {stderr}");
    }
}

#[test]
fn binary_refuses_llmlint_as_the_provider_override() {
    // `--provider llmlint` and `ONEJUDGE_PROVIDER=llmlint` land on the top-level
    // provider, which an llmlint judge can never be.
    let config = write_config("llmlint-override.yaml", "task: go\n");
    let flag = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--provider", "llmlint"])
        .output()
        .unwrap();
    assert_eq!(flag.status.code(), Some(2));
    let stderr = String::from_utf8(flag.stderr).unwrap();
    assert!(stderr.contains("config error"), "{stderr}");
    assert!(
        stderr.contains("`--provider` / `ONEJUDGE_PROVIDER`"),
        "{stderr}"
    );
    assert!(
        stderr.contains("put it under `provider.judges:`"),
        "{stderr}"
    );

    let env = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap()])
        .env("ONEJUDGE_PROVIDER", "llmlint")
        .output()
        .unwrap();
    assert_eq!(env.status.code(), Some(2));
    let stderr = String::from_utf8(env.stderr).unwrap();
    assert!(
        stderr.contains("put it under `provider.judges:`"),
        "{stderr}"
    );
}

#[test]
fn a_judge_list_of_only_llmlint_cannot_answer_a_numeric_eval_or_an_assessment() {
    // Refused at resolution — before any probe, before any paid turn — because
    // nothing in the list can score a number or write prose.
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let head = format!(
        "provider:\n  kind: split\n  skill:\n    kind: command\n    command: [{echo}]\n  \
         judge:\n    kind: llmlint\n    bin: onejudge-no-such-llmlint-zzz\ntask: go\n"
    );
    for (what, body, needle) in [
        (
            "numeric",
            "evals:\n  - criterion: readable\n    kind: numeric\n",
            "the config names a numeric eval",
        ),
        (
            "assessment",
            "assessment: Follow-ups?\n",
            "the config names an `assessment`",
        ),
    ] {
        let err = Config::from_yaml(&format!("{head}{body}"))
            .unwrap()
            .into_plan()
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains(needle), "{what}: {text}");
        assert!(
            text.contains("`oneharness` or `command` judge"),
            "{what}: {text}"
        );
    }
    // A boolean eval is fine: llmlint answers it.
    let plan = Config::from_yaml(&format!(
        "{head}evals:\n  - criterion: lint-clean\n    kind: boolean\n"
    ))
    .unwrap()
    .into_plan();
    assert!(plan.is_ok(), "{:?}", plan.err());
}

#[test]
fn a_single_judge_config_runs_exactly_as_the_released_0_8_1_did() {
    // The replay of the checked-in baseline: the same `split` with one `judge:`
    // that `scripts/capture-single-judge-baseline.sh` ran through the released
    // 0.8.1 binary, run through this build, must produce the same transcript,
    // verdicts, usage, control addresses and completion — and hand the judge the
    // same supervisor requests, bare session name included. Only the volatile
    // fields (wall clock, pids) and the v12 `judge_decisions` may differ.
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/single-judge");
    let record = scratch_path("single-judge-baseline.jsonl");
    let template = std::fs::read_to_string(fixture.join("config.yaml")).unwrap();
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let yaml = template
        .replace("{{ECHO}}", &echo)
        .replace("{{RECORD}}", &record.display().to_string());
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("single-judge-baseline.yaml");
    std::fs::write(&config, yaml).unwrap();

    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let object = actual.as_object_mut().unwrap();
    // Present, and the one thing the release did not write.
    let decisions = object
        .remove("judge_decisions")
        .expect("a panel of one still records its decisions");
    assert_eq!(decisions.as_array().unwrap().len(), 2);
    for volatile in ["schema_version", "telemetry", "processes"] {
        object.remove(volatile);
    }
    let expected: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture.join("report.json")).unwrap())
            .unwrap();
    assert_eq!(
        actual, expected,
        "the report differs from what onejudge 0.8.1 wrote for this config"
    );

    // The judge is handed the requests 0.8.1 did, plus protocol v8's required
    // taken-turn field — with the same bare `<session>-user` name, persona,
    // transcript and criterion.
    let escaped = serde_json::to_string(&record.display().to_string()).unwrap();
    let escaped = &escaped[1..escaped.len() - 1];
    let requests = std::fs::read_to_string(&record)
        .unwrap()
        .replace(escaped, "{{RECORD}}");
    let baseline = std::fs::read_to_string(fixture.join("supervisor-requests.jsonl")).unwrap();
    assert_eq!(requests, baseline);
    assert!(
        !requests.contains("-user-"),
        "a panel of one hands the bare session through"
    );
}

#[cfg(unix)]
#[test]
fn a_single_judge_controlled_config_runs_as_0_8_1_did_except_the_supervisor_address_it_omitted() {
    // The controlled baseline: the same one-`judge:` split with `control: true` on
    // both sides, captured from 0.8.1. Everything but `supervisor_control` /
    // `supervisor_control_unavailable` is identical — transcript, usage, the agent's
    // `control` address, the judge prompts. Those two now carry the first judge's
    // answer (Contract B); 0.8.1's CLI wrote `null` for every config because
    // `AnyProvider` never forwarded the judge side's answer — a defect against
    // docs/control.md, ruled so by the planner rather than preserved.
    //
    // The paths ride the judge prompt, whose length the fake oneharness bills as
    // `input_tokens`, so they are spelled at the same fixed width the capture
    // script used (`/tmp/oj-ctl-<8-digit pid>/…`) rather than taken from
    // `control_store` — a wider temp dir would change the usage, not the behaviour.
    use oneharness_core::io::session as session_io;

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/single-judge-control");
    let ctl = std::path::PathBuf::from(format!("/tmp/oj-ctl-{:08}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ctl);
    let agent_store = ctl.join("agent-store");
    let judge_store = ctl.join("judge-store");
    std::fs::create_dir_all(&agent_store).unwrap();
    std::fs::create_dir_all(&judge_store).unwrap();
    let record = ctl.join("prompts.log");
    let template = std::fs::read_to_string(fixture.join("config.yaml")).unwrap();
    let yaml = template
        .replace(
            "{{ONEHARNESS}}",
            &serde_json::to_string(&fake_oneharness_bin()).unwrap(),
        )
        .replace("{{AGENT_STORE}}", agent_store.to_str().unwrap())
        .replace("{{JUDGE_STORE}}", judge_store.to_str().unwrap())
        .replace("{{RECORD}}", record.to_str().unwrap());
    let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("single-judge-control.yaml");
    std::fs::write(&config, yaml).unwrap();

    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    // Incomplete (the cap ends it), exactly as 0.8.1's run was.
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let normalize = |text: &str| {
        let mut text = text.to_string();
        for (real, placeholder) in [
            (record.clone(), "{{RECORD}}"),
            (agent_store.clone(), "{{AGENT_STORE}}"),
            (judge_store.clone(), "{{JUDGE_STORE}}"),
        ] {
            for spelling in [real.canonicalize().unwrap(), real] {
                text = text.replace(spelling.to_str().unwrap(), placeholder);
            }
        }
        text
    };
    let mut actual: serde_json::Value =
        serde_json::from_str(&normalize(&String::from_utf8(output.stdout).unwrap())).unwrap();
    let object = actual.as_object_mut().unwrap();
    assert_eq!(
        object
            .remove("judge_decisions")
            .expect("a panel of one records its decision")
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for volatile in ["schema_version", "telemetry", "processes"] {
        object.remove(volatile);
    }
    // The one deliberate difference: the judge's address, which 0.8.1 omitted.
    let supervisor = object
        .remove("supervisor_control")
        .expect("always on the wire");
    assert!(
        object.remove("supervisor_control_unavailable").is_none(),
        "the ask was honoured, so no refusal reason"
    );
    assert_eq!(
        supervisor,
        serde_json::json!({
            "session": "ctl-baseline-user",
            "session_dir": "{{JUDGE_STORE}}",
            "cwd": ".",
        })
    );
    // …and it is a real address: the record `oneharness interrupt` reads before it
    // dials is at it.
    let dir = session_io::resolve_dir(judge_store.canonicalize().unwrap().to_str()).unwrap();
    session_io::read(&session_io::session_path(
        &dir,
        Path::new("."),
        "ctl-baseline-user",
    ))
    .expect("the judge's session record is where the address says");

    let mut expected: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture.join("report.json")).unwrap())
            .unwrap();
    let baseline = expected.as_object_mut().unwrap();
    assert_eq!(
        baseline.remove("supervisor_control"),
        Some(serde_json::Value::Null),
        "0.8.1 wrote null here: the defect this replay documents"
    );
    assert_eq!(
        actual, expected,
        "the report differs from what onejudge 0.8.1 wrote for this config"
    );

    // The judge was handed byte-for-byte the prompts 0.8.1 handed it.
    let prompts = normalize(&std::fs::read_to_string(&record).unwrap());
    let baseline = std::fs::read_to_string(fixture.join("supervisor-prompts.log")).unwrap();
    assert_eq!(prompts, baseline);
    let _ = std::fs::remove_dir_all(&ctl);
}

#[test]
fn artifact_flag_and_env_replace_the_configured_list_like_the_persona_overrides() {
    // The built binary, three ways: the config's `user.artifacts` alone, then
    // `ONEJUDGE_ARTIFACTS` over it, then `--artifact` over both — the precedence
    // `--persona` / `ONEJUDGE_PERSONA` have over `user.persona`. What each judge-
    // side turn was actually shown is read off the harness stand-in.
    let dir = in_process_project("cli-artifacts", "[[reply:wrote the plan]]");
    let log = scratch_path("cli-artifacts-prompts.log");
    let quote = |text: &str| serde_json::to_string(text).unwrap();
    let config = dir.join("onejudge.yaml");
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: oneharness\n  judge_config: {}\nskill: {}\ntask: {}\nuser:\n  persona: a reviewer\n  done_when: the plan is written\n  max_turns: 2\n  artifacts: [from-config.md]\n",
            quote(&dir.join("oneharness.toml").display().to_string()),
            quote(&dir.display().to_string()),
            quote(&format!("write the plan [[artifact-evaluator:{}]]", log.display())),
        ),
    )
    .unwrap();
    let named = |name: &str| format!("  - {} (does not exist)\n", dir.join(name).display());
    let run = |env: Option<&str>, flags: &[&str]| -> Vec<String> {
        let _ = std::fs::remove_file(&log);
        let mut command = Command::new(onejudge_bin());
        command.arg("run").arg(&config).args(flags);
        match env {
            Some(list) => command.env("ONEJUDGE_ARTIFACTS", list),
            None => command.env_remove("ONEJUDGE_ARTIFACTS"),
        };
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let prompts: Vec<String> = std::fs::read_to_string(&log)
            .unwrap()
            .split("=== end of prompt ===")
            .filter(|prompt| prompt.contains(onejudge::EVIDENCE_PROMPT_MARKER))
            .map(str::to_string)
            .collect();
        assert!(!prompts.is_empty(), "no judge-side turn was recorded");
        prompts
    };

    for prompt in run(None, &[]) {
        assert!(prompt.contains(&named("from-config.md")), "{prompt}");
    }

    let env = std::env::join_paths(["from-env-a.md", "from-env-b.md"])
        .unwrap()
        .into_string()
        .unwrap();
    for prompt in run(Some(&env), &[]) {
        assert!(prompt.contains(&named("from-env-a.md")), "{prompt}");
        assert!(prompt.contains(&named("from-env-b.md")), "{prompt}");
        assert!(!prompt.contains("from-config.md"), "{prompt}");
    }

    for prompt in run(Some(&env), &["--artifact", "from-flag.md"]) {
        assert!(prompt.contains(&named("from-flag.md")), "{prompt}");
        assert!(!prompt.contains("from-env-"), "{prompt}");
        assert!(!prompt.contains("from-config.md"), "{prompt}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn binary_asks_the_spawned_oneharness_for_its_json_report_by_name() {
    // oneharness 0.14.0 moves `run`'s default output to a human-readable view, so
    // onejudge — a program reading the machine contract — asks for JSON with
    // `--format json` rather than relying on the default. Driven through the real
    // binary at the spawning seam (`bin:`): both parties record the argv the
    // double was spawned with, and the run still reads the report it answered.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let agent_log = dir.join("format-json-agent.argv");
    let judge_log = dir.join("format-json-judge.argv");
    let _ = std::fs::remove_file(&agent_log);
    let _ = std::fs::remove_file(&judge_log);
    let config = dir.join("format-json.yaml");
    let bin = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: oneharness\n  bin: {bin}\n\
             task: go\n\
             system_prompt: '[[reply:argv recorded]][[record-argv:{}]]'\n\
             user:\n  persona: 'A tester. [[record-argv:{}]]'\n  done_when: argv recorded\n  max_turns: 3\n",
            agent_log.display(),
            judge_log.display(),
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
    assert_eq!(report.transcript.messages[1].content, "argv recorded");

    for (side, log) in [("agent", &agent_log), ("judge", &judge_log)] {
        let recorded = std::fs::read_to_string(log)
            .unwrap_or_else(|e| panic!("the {side} side recorded no argv: {e}"));
        let invocations: Vec<Vec<&str>> = recorded
            .split("=== argv ===\n")
            .filter(|chunk| !chunk.is_empty())
            .map(|chunk| chunk.lines().collect())
            .collect();
        assert!(!invocations.is_empty(), "no {side} invocation was recorded");
        for argv in invocations {
            assert!(
                argv.windows(2).any(|w| w == ["--format", "json"]),
                "the {side} side was spawned without `--format json`: {argv:?}"
            );
        }
    }
}

/// The names of every history session under `store`, read through oneharness's
/// own history reader over its default window — what `oneharness history list`
/// would print (the run under test is minutes old, so it is inside that window).
fn history_names(store: &Path) -> std::collections::BTreeSet<String> {
    oneharness_core::io::history::list_sessions(
        store,
        None,
        oneharness_core::domain::history_index::HistoryWindow::default(),
    )
    .unwrap()
    .into_iter()
    .map(|session| session.name)
    .collect()
}

/// Run the built binary over a project whose agent and judge configs both pin the
/// fake-harness double, under `--session <session>`, recording into a fresh
/// history store. Returns the names the store holds afterwards.
fn run_with_judge_history(
    name: &str,
    provider: &str,
    extra: &str,
    session: &str,
) -> std::collections::BTreeSet<String> {
    let dir = scratch_path(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("xdg")).unwrap();
    let harness = format!(
        "harnesses = [\"claude-code\"]\n\n[harness.claude-code]\nbin = {:?}\n",
        doubles::fake_harness(),
    );
    std::fs::write(dir.join("SKILL.md"), "[[reply:wrote the plan]]").unwrap();
    std::fs::write(dir.join("oneharness.toml"), &harness).unwrap();
    let judge_config = dir.join("judge.toml");
    std::fs::write(&judge_config, &harness).unwrap();
    let store = dir.join("store");
    let quote = |text: &str| serde_json::to_string(text).unwrap();
    let config = dir.join("onejudge.yaml");
    std::fs::write(
        &config,
        format!(
            "{}skill: {}\ntask: {}\nuser:\n  persona: a reviewer\n  done_when: the plan is written\n  max_turns: 2\n{extra}",
            provider.replace(
                "JUDGE_CONFIG",
                &quote(&judge_config.display().to_string())
            ),
            quote(&dir.display().to_string()),
            quote(&format!(
                "write the plan [[artifact-evaluator:{}]]",
                dir.join("prompts.log").display()
            )),
        ),
    )
    .unwrap();
    let output = Command::new(onejudge_bin())
        .arg("run")
        .arg(&config)
        .args(["--session", session])
        .env("ONEHARNESS_HISTORY_DIR", &store)
        // Hermetic: no user-level oneharness config or environment override
        // reaches either party (a host exporting a pointer file or labels would
        // otherwise have this run's records written into its own); onejudge
        // itself asks every call to record.
        .env("XDG_CONFIG_HOME", dir.join("xdg"))
        .env_remove("ONEHARNESS_CONFIG")
        .env_remove("ONEHARNESS_HISTORY")
        .env_remove("ONEHARNESS_HISTORY_POINTER_FILE")
        .env_remove("ONEHARNESS_HISTORY_LABELS")
        .env_remove("ONEJUDGE_SESSION")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let names = history_names(&store);
    let _ = std::fs::remove_dir_all(&dir);
    names
}

fn names(expected: &[&str]) -> std::collections::BTreeSet<String> {
    expected.iter().map(|name| (*name).to_string()).collect()
}

#[test]
fn every_judge_side_turn_is_recorded_under_the_run_session_and_its_label() {
    // `oneharness history show <session>-user-<label>` must find exactly one
    // judge's supervisor turns. Read back through oneharness's own reader, every
    // record the run left is named after `--session`: nothing under the
    // prompt-derived `you-are-the-...` name every run used to share.
    let panel = "provider:\n  kind: split\n  skill:\n    kind: oneharness\n  judges:\n    \
                 - kind: oneharness\n      judge_config: JUDGE_CONFIG\n      label: reviewer\n    \
                 - kind: oneharness\n      judge_config: JUDGE_CONFIG\n      label: second\n";
    assert_eq!(
        run_with_judge_history("cli-history-panel", panel, "", "sess-p"),
        names(&[
            "sess-p-skill",
            "sess-p-user-reviewer",
            "sess-p-user-second",
            "sess-p-judge-reviewer",
            "sess-p-judge-second",
        ])
    );

    // A bare judge carries no label, and its verdict and assessment get their own.
    let bare = "provider:\n  kind: oneharness\n  judge_config: JUDGE_CONFIG\n";
    let judged = "evals:\n  - criterion: the plan is written\n    kind: boolean\n\
                  assessment: anything left to do?\n";
    assert_eq!(
        run_with_judge_history("cli-history-bare", bare, judged, "sess-b"),
        names(&[
            "sess-b-skill",
            "sess-b-user",
            "sess-b-judge",
            "sess-b-assess"
        ])
    );
}

// --- History across the segment cutover ---------------------------------------
//
// `oneharness-core` 0.24.0 indexes history in dated segments under `.index.d/`
// and never reads the legacy index or walks the store to record a run. These
// journeys hold onejudge's own history read — the `history_id` it reports for
// each attempt — to both sides of that cutover.

/// Every file under `dir`, relative path → bytes, so a journey can prove it
/// neither created nor changed any.
fn file_snapshot(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let relative = path.strip_prefix(root).unwrap().display().to_string();
                out.insert(relative, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Copy the checked-in store a released pre-cutover oneharness wrote
/// (`tests/golden/pre-cutover-history/store`, `scripts/capture-pre-cutover-history.sh`).
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

fn agent_attribution(report: &onejudge::Report) -> &onejudge::HarnessAttribution {
    report
        .telemetry
        .as_ref()
        .expect("telemetry reaches the report")
        .attribution
        .iter()
        .find(|a| a.role == onejudge::TelemetryRole::Agent)
        .expect("the agent invocation is attributed")
}

#[test]
fn a_run_recorded_before_the_segment_cutover_is_still_read_back() {
    // A store exactly as released oneharness 0.20.0 (core 0.22.0) left it: the
    // session file in its line format, the legacy `.index.jsonl`, no `.index.d/`.
    // The fake oneharness reports that session file as the run's `history_file`,
    // so onejudge's read of it goes through the linked core's reader alone.
    let golden =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/pre-cutover-history/store");
    let dir = scratch_path("cli-pre-cutover-history");
    let _ = std::fs::remove_dir_all(&dir);
    let store = dir.join("store");
    copy_tree(&golden, &store);
    assert!(store.join(".index.jsonl").is_file());
    assert!(!store.join(".index.d").exists());
    let session = std::fs::read_dir(store.join("tmp-onejudge-pre-cutover-project"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let recorded: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(&session)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    let before = file_snapshot(&store);

    let config = dir.join("onejudge.yaml");
    let quote = |text: &str| serde_json::to_string(text).unwrap();
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: oneharness\n  bin: {}\ntask: read it back\nsystem_prompt: {}\n",
            quote(&fake_oneharness_bin()),
            quote(&format!(
                "[[reply:read back]][[recorded:{}]]",
                session.display()
            )),
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
    let agent = agent_attribution(&report);
    assert_eq!(
        agent.history_file.as_deref(),
        Some(session.to_str().unwrap())
    );
    assert_eq!(
        agent.candidates[0].history_id.as_deref(),
        recorded["history_id"].as_str(),
        "the pre-cutover session's record did not reach the report"
    );
    assert_eq!(
        file_snapshot(&store),
        before,
        "reading a pre-cutover record created or changed a file in its store"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn recording_in_process_history_reads_no_legacy_index_and_changes_no_other_session() {
    use oneharness_core::domain::history::HistoryId;
    use oneharness_core::domain::history_index::{
        HistoryIndexEntry, SegmentKind, UtcDate, INDEX_DIR, LEGACY_EVENT_INDEX_FILE,
        LEGACY_INDEX_FILE,
    };
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch_path("cli-history-without-scans");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("xdg")).unwrap();
    std::fs::create_dir_all(dir.join("skill")).unwrap();
    let store = dir.join("store");

    // A store a long-lived host holds: 1,200 other sessions across four projects,
    // in the line format released cores wrote, and both legacy index files —
    // which nothing may open: under an older core, recording this run had to
    // read `.index.jsonl` to reconcile it.
    let template = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/pre-cutover-history/store/tmp-onejudge-pre-cutover-project")
            .read_dir()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path(),
    )
    .unwrap();
    for project in 0..4 {
        let project_dir = store.join(format!("home-host-project-{project}"));
        std::fs::create_dir_all(&project_dir).unwrap();
        for session in 0..300 {
            std::fs::write(
                project_dir.join(format!("older-20250101T000000Z-{session}.jsonl")),
                &template,
            )
            .unwrap();
        }
    }
    let legacy = [
        store.join(LEGACY_INDEX_FILE),
        store.join(LEGACY_EVENT_INDEX_FILE),
    ];
    for path in &legacy {
        std::fs::write(path, "{\"legacy\":true}\n").unwrap();
    }
    let before = file_snapshot(&store);
    let legacy_meta: Vec<_> = legacy
        .iter()
        .map(|path| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
            let meta = std::fs::metadata(path).unwrap();
            (
                meta.permissions().mode(),
                meta.len(),
                meta.modified().unwrap(),
            )
        })
        .collect();
    for path in &legacy {
        assert!(
            std::fs::File::open(path).is_err(),
            "{} must be unopenable for this journey to prove anything (running as root?)",
            path.display()
        );
    }

    let harness = format!(
        "harnesses = [\"claude-code\"]\n\n[harness.claude-code]\nbin = {:?}\n",
        fake_harness_bin(),
    );
    let skill = dir.join("skill");
    std::fs::write(skill.join("SKILL.md"), "[[reply:recorded in a segment]]").unwrap();
    std::fs::write(skill.join("oneharness.toml"), &harness).unwrap();
    let config = dir.join("onejudge.yaml");
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: oneharness\nskill: {}\ntask: record this turn\n",
            serde_json::to_string(&skill.display().to_string()).unwrap()
        ),
    )
    .unwrap();
    let today = || {
        UtcDate::from_epoch_secs(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .try_into()
                .unwrap(),
        )
    };
    let started_on = today();
    let output = Command::new(onejudge_bin())
        .args(["run", config.to_str().unwrap(), "--format", "json"])
        .env("ONEHARNESS_HISTORY_DIR", &store)
        .env("XDG_CONFIG_HOME", dir.join("xdg"))
        .env_remove("ONEHARNESS_CONFIG")
        .env_remove("ONEHARNESS_HISTORY")
        .env_remove("ONEHARNESS_HISTORY_POINTER_FILE")
        .env_remove("ONEHARNESS_HISTORY_LABELS")
        .env_remove("ONEJUDGE_SESSION")
        .output()
        .unwrap();
    let finished_on = today();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: onejudge::Report =
        serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert_eq!(
        report.transcript.messages[1].content,
        "recorded in a segment"
    );

    // The id onejudge read back through its own history read names a record in
    // today's run segment.
    let agent = agent_attribution(&report);
    let id: HistoryId = agent.candidates[0]
        .history_id
        .as_deref()
        .expect("onejudge read the run's record back")
        .parse()
        .unwrap();
    let date = UtcDate::of_history_id(id).expect("a new record's id carries its date");
    assert!(
        date == started_on || date == finished_on,
        "{date} is not the run's UTC date"
    );
    let segment = store
        .join(INDEX_DIR)
        .join(SegmentKind::Runs.file_name(date));
    let entries = std::fs::read_to_string(&segment)
        .unwrap_or_else(|e| panic!("no run segment at {}: {e}", segment.display()));
    let entry = entries
        .lines()
        .map(|line| serde_json::from_str::<HistoryIndexEntry>(line).unwrap())
        .find_map(|entry| match entry {
            HistoryIndexEntry::Run(run) if run.history_id == id => Some(run),
            _ => None,
        })
        .expect("the run's record is indexed in its dated segment");
    assert_eq!(
        Some(entry.session_path.under(&store).display().to_string()),
        agent.history_file,
        "the segment entry names the session file onejudge read"
    );

    // Nothing else was touched: no legacy lock, both legacy index files as they
    // were (still unopenable, same size and mtime, then byte for byte), and every
    // other session's file unchanged.
    assert!(
        !store.join(".index.lock").exists(),
        "the legacy index lock was created"
    );
    for (path, meta) in legacy.iter().zip(&legacy_meta) {
        let now = std::fs::metadata(path).unwrap();
        assert_eq!(
            &(now.permissions().mode(), now.len(), now.modified().unwrap()),
            meta,
            "{} changed",
            path.display()
        );
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let mut after = file_snapshot(&store);
    after.retain(|path, _| before.contains_key(path));
    assert_eq!(after.len(), before.len());
    assert!(
        after == before,
        "a legacy index or another session's file changed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Judge posture: what an evaluator judge runs under ------------------------
//
// These journeys drive the built `onejudge` binary on BOTH seams a oneharness
// judge can run on, and hold the harness each one reaches to what it was spawned
// with. In process the engine is the linked core; spawned, the provider names
// `onejudge-fake-oneharness` in its engine mode, which hands the argv onejudge
// spawned it with to the same linked core. Either way the harness is
// `onejudge-fake-harness`, pinned as `claude-code` through ordinary oneharness
// config, and it records every invocation — so what is asserted is the harness
// argv and the prompt real oneharness code produced, not a re-derivation of them.

/// The fake harness double, reached as `claude-code` through `[harness.*] bin`.
fn fake_harness_bin() -> &'static str {
    doubles::fake_harness()
}

/// Which seam a posture journey's providers run on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seam {
    /// The linked engine, in the `onejudge` process.
    InProcess,
    /// A spawned `oneharness` — the fake in its engine mode.
    Spawned,
}

impl Seam {
    const BOTH: [Seam; 2] = [Seam::InProcess, Seam::Spawned];

    fn name(self) -> &'static str {
        match self {
            Seam::InProcess => "in-process",
            Seam::Spawned => "spawned",
        }
    }

    /// The `bin:` line a oneharness provider entry carries on this seam, at
    /// `indent` spaces, or nothing for the in-process default.
    fn bin_line(self, indent: usize) -> String {
        match self {
            Seam::InProcess => String::new(),
            Seam::Spawned => format!(
                "{:indent$}bin: {}\n",
                "",
                serde_json::to_string(&fake_oneharness_bin()).unwrap()
            ),
        }
    }
}

/// The harness selection every posture journey's configs share: the fake harness
/// as `claude-code`, with history kept inside the run's own directory.
fn harness_toml(dir: &Path, top: &str) -> String {
    format!(
        "{top}harnesses = [\"claude-code\"]\nhistory_dir = {:?}\n\n[harness.claude-code]\nbin = {:?}\n",
        dir.join("history").display().to_string(),
        fake_harness_bin(),
    )
}

/// Spell every path under the `{{RUN}}` placeholder with `/`, so a normalized
/// record reads the same on Windows, where the path after the run directory
/// is joined with `\`, as on a POSIX host. A path ends at whitespace, a quote,
/// or the bracket, comma or parenthesis that closes the text around it.
fn forward_run_paths(text: &str) -> String {
    if std::path::MAIN_SEPARATOR == '/' {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{RUN}}") {
        let (before, tail) = rest.split_at(start + "{{RUN}}".len());
        out.push_str(before);
        let end = tail
            .find(|c: char| c.is_whitespace() || "\"'],)".contains(c))
            .unwrap_or(tail.len());
        out.push_str(&tail[..end].replace('\\', "/"));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// `path` without the `\\?\` prefix `canonicalize` gives a Windows path. The
/// run directory is the harness's working directory, and oneharness's discovery
/// spells a file it finds there the plain way the OS reports that directory.
/// `normalize` finds the run directory in recorded text by its spelling, so the
/// two must be spelled alike for a recorded path to read as `{{RUN}}/...`.
fn without_verbatim_prefix(path: std::path::PathBuf) -> std::path::PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(plain) if !plain.starts_with("UNC\\") => std::path::PathBuf::from(plain),
        _ => path,
    }
}

/// One posture journey's working directory — the run's cwd and so the agent's
/// worktree — and what it ran.
struct PostureRun {
    dir: std::path::PathBuf,
    seam: Seam,
    env: Vec<(String, String)>,
}

struct PostureOutcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// Every harness invocation, in order: `{"argv": [...], "stdin": "..."}`,
    /// normalized.
    harness: Vec<serde_json::Value>,
    /// Every `oneharness` argv onejudge spawned (the spawned seam only),
    /// normalized.
    oneharness: Vec<Vec<String>>,
}

impl PostureOutcome {
    /// The judge-side harness invocations: the ones handed an evidence contract.
    fn judge_side(&self) -> Vec<&serde_json::Value> {
        self.harness
            .iter()
            .filter(|call| call.to_string().contains("EVIDENCE CONTRACT ("))
            .collect()
    }

    /// The report: the `--format json` document, or the terminal `result`
    /// line's under `--stream`.
    fn report(&self) -> serde_json::Value {
        if let Ok(report) = serde_json::from_str::<serde_json::Value>(&self.stdout) {
            if report.get("type").is_none() {
                return report;
            }
        }
        let last = self.stream_lines().pop().expect("the run wrote its report");
        assert_eq!(last["type"], "result", "{}", self.stdout);
        last["report"].clone()
    }

    /// Every line of a `--stream` run's stdout.
    fn stream_lines(&self) -> Vec<serde_json::Value> {
        self.stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The first supervisor decision's record for `judge`.
    fn decision(&self, judge: &str) -> serde_json::Value {
        let report = self.report();
        report["judge_decisions"][0]["decisions"]
            .as_array()
            .expect("the panel recorded its decisions")
            .iter()
            .find(|decision| decision["judge"] == judge)
            .unwrap_or_else(|| panic!("no decision by `{judge}`: {report}"))
            .clone()
    }

    /// The `oneharness` argv of every evaluator call onejudge spawned — the ones
    /// with a working directory, whose posture onejudge layers.
    fn evaluator_argv(&self) -> Vec<&Vec<String>> {
        self.oneharness
            .iter()
            .filter(|argv| argv.iter().any(|arg| arg == "--config"))
            .collect()
    }
}

/// The text a harness invocation was prompted with: `-p`'s value, or the stdin a
/// long prompt was moved to.
fn prompt_of(call: &serde_json::Value) -> String {
    let stdin = call["stdin"].as_str().unwrap_or_default();
    if !stdin.is_empty() {
        return stdin.to_string();
    }
    let argv = argv_of(call);
    let at = argv
        .iter()
        .position(|arg| arg == "-p")
        .expect("a -p prompt");
    argv[at + 1].clone()
}

fn argv_of(call: &serde_json::Value) -> Vec<String> {
    serde_json::from_value(call["argv"].clone()).unwrap()
}

fn judge_config_line(path: &Path) -> String {
    format!(
        "judge_config: {}",
        serde_json::to_string(&path.display().to_string()).unwrap()
    )
}

/// A path a posture records names the same file as `expected`, compared as
/// files rather than spellings, and is spelled as a person would read it: never
/// in the `\\?\` verbatim form Windows' `canonicalize` produces, which is the
/// form a provenance path must not leak.
fn assert_same_file(recorded: &serde_json::Value, expected: &Path) {
    let recorded = recorded.as_str().expect("a path");
    assert!(
        !recorded.starts_with(r"\\?\"),
        "a verbatim provenance path: {recorded}"
    );
    assert_eq!(
        Path::new(recorded).canonicalize().unwrap(),
        expected.canonicalize().unwrap(),
        "{recorded} is not {}",
        expected.display()
    );
}

/// Every configured layer a posture names is a file, `environment`, or the
/// defaults file — which onejudge wrote, holding the read-only mode alone.
fn assert_defaults_file(path: &serde_json::Value) {
    let path = Path::new(path.as_str().expect("a path"));
    assert!(
        path.ends_with("onejudge/judge-defaults-v1.toml"),
        "{}",
        path.display()
    );
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "mode = \"read-only\"\n"
    );
}

impl PostureRun {
    /// A fresh run directory named for `name` and `seam`, holding the agent's
    /// own discovered `oneharness.toml` and an empty user-config home.
    fn new(name: &str, seam: Seam) -> Self {
        let dir =
            Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("posture-{name}-{}", seam.name()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("xdg")).unwrap();
        std::fs::create_dir_all(dir.join("state")).unwrap();
        let dir = without_verbatim_prefix(dir.canonicalize().unwrap());
        std::fs::write(dir.join("oneharness.toml"), harness_toml(&dir, "")).unwrap();
        Self {
            dir,
            seam,
            env: Vec::new(),
        }
    }

    /// Write a judge config named `file` into the run directory, `top` placed
    /// above the harness selection, and return its path.
    fn judge_config(&self, file: &str, top: &str) -> std::path::PathBuf {
        let path = self.dir.join(file);
        std::fs::write(&path, harness_toml(&self.dir, top)).unwrap();
        path
    }

    /// Set `key` to `value` in the environment of every run this posture makes.
    fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Run `provider` over the shared conversation — one worker turn the
    /// supervisor completes, one boolean eval and an assessment — with `args`
    /// after `run <config> --format json`.
    fn run(&self, provider: &str, task_extra: &str, args: &[&str]) -> PostureOutcome {
        self.run_bin(onejudge_bin(), provider, task_extra, args)
    }

    /// What `oneharness config` attributes each value to for `configs` over this
    /// run's directory, in this run's environment — the fake's engine mode runs
    /// the linked core's own `load_layers` + `explain`, which is that command.
    fn explain(&self, configs: &[serde_json::Value]) -> serde_json::Value {
        let mut command = Command::new(fake_oneharness_bin());
        command.arg("config");
        for config in configs {
            command.args(["--config", config.as_str().unwrap()]);
        }
        command
            .args(["--cwd", self.dir.to_str().unwrap()])
            .env("ONEJUDGE_FAKE_ONEHARNESS_ENGINE", "1")
            .env("XDG_CONFIG_HOME", self.dir.join("xdg"));
        for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("ONEHARNESS_")) {
            command.env_remove(key);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn split(&self, provider_extra: &[&str], judges: &[&[&str]]) -> String {
        let mut yaml = format!(
            "provider:\n  kind: split\n  skill:\n    kind: oneharness\n{}  judges:\n",
            self.seam.bin_line(4)
        );
        for judge in judges {
            yaml.push_str("    - kind: oneharness\n");
            yaml.push_str(&self.seam.bin_line(6));
            for line in *judge {
                yaml.push_str(&format!("      {line}\n"));
            }
        }
        for line in provider_extra {
            yaml.push_str(&format!("  {line}\n"));
        }
        yaml
    }

    fn yaml(&self, provider: &str, task_extra: &str) -> String {
        let task = format!(
            "do it [[record-harness:{}]] [[evaluate]]{task_extra}",
            self.dir.join("harness.jsonl").display()
        );
        format!(
            "{provider}task: {}\nsystem_prompt: '[[reply:done]]'\nuser:\n  persona: a reviewer\n  \
             done_when: the work is done\n  max_turns: 2\nsession: posture\nevals:\n  - criterion: \
             done\n    kind: boolean\nassessment: anything left?\n",
            serde_json::to_string(&task).unwrap()
        )
    }

    fn run_bin(
        &self,
        bin: &str,
        provider: &str,
        task_extra: &str,
        args: &[&str],
    ) -> PostureOutcome {
        let record = self.dir.join("harness.jsonl");
        let argv_log = self.dir.join("oneharness.jsonl");
        let _ = std::fs::remove_file(&record);
        let _ = std::fs::remove_file(&argv_log);
        let config = self.dir.join("onejudge.yaml");
        std::fs::write(&config, self.yaml(provider, task_extra)).unwrap();
        let mut command = Command::new(bin);
        command.args(["run", config.to_str().unwrap()]);
        if !args.contains(&"--format") {
            command.args(["--format", "json"]);
        }
        command
            .args(args)
            .current_dir(&self.dir)
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("XDG_CONFIG_HOME", self.dir.join("xdg"));
        // Hermetic: an `ONEHARNESS_*` override in the environment running the suite
        // is a config layer above every file, so it would decide what these
        // journeys assert about. Each journey sets the ones it is about.
        for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("ONEHARNESS_")) {
            command.env_remove(key);
        }
        if self.seam == Seam::Spawned {
            command
                .env("ONEJUDGE_FAKE_ONEHARNESS_ENGINE", "1")
                .env("ONEJUDGE_FAKE_ONEHARNESS_ARGV_LOG", &argv_log);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        let read_lines = |path: &Path| -> Vec<String> {
            std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        };
        let harness = read_lines(&record)
            .iter()
            .map(|line| {
                let mut call: serde_json::Value = serde_json::from_str(line).unwrap();
                self.normalize_value(&mut call);
                call
            })
            .collect();
        let oneharness = read_lines(&argv_log)
            .iter()
            .map(|line| {
                let argv: Vec<String> = serde_json::from_str(line).unwrap();
                argv.iter().map(|arg| self.normalize(arg)).collect()
            })
            .collect();
        PostureOutcome {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            harness,
            oneharness,
        }
    }

    /// Replace every per-run spelling in `text` with a placeholder: the run
    /// directory (its subpaths spelled with `/`), then each history file under
    /// it (named by a timestamp and a pid), then the harness doubles' paths.
    fn normalize(&self, text: &str) -> String {
        let text = forward_run_paths(&text.replace(&self.dir.display().to_string(), "{{RUN}}"));
        let history = "{{RUN}}/history/";
        let mut out = String::new();
        let mut rest = text.as_str();
        while let Some(start) = rest.find(history) {
            out.push_str(&rest[..start]);
            let tail = &rest[start..];
            let end = tail
                .find(".jsonl")
                .map_or(tail.len(), |i| i + ".jsonl".len());
            out.push_str("{{HISTORY_FILE}}");
            rest = &tail[end..];
        }
        out.push_str(rest);
        out.replace(fake_harness_bin(), "{{HARNESS}}")
            .replace(&fake_oneharness_bin(), "{{ONEHARNESS}}")
    }

    fn normalize_value(&self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) => *text = self.normalize(text),
            serde_json::Value::Array(items) => {
                items.iter_mut().for_each(|v| self.normalize_value(v))
            }
            serde_json::Value::Object(map) => {
                map.values_mut().for_each(|v| self.normalize_value(v))
            }
            _ => {}
        }
    }
}

/// The baseline a posture journey is held to: what the released onejudge 0.15.0
/// handed each harness for the same config, captured by
/// `scripts/capture-judge-posture-baseline.sh`.
fn posture_baseline(seam: Seam) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/judge-posture-0.15.0")
        .join(format!("{}.json", seam.name()));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The default-posture journey: a judge whose config names no mode, anywhere.
fn default_posture_run(seam: Seam, bin: &str) -> (PostureRun, PostureOutcome) {
    let run = PostureRun::new("default", seam);
    let judge = run.judge_config("judge.toml", "");
    let provider = run.split(
        &[],
        &[&[&format!(
            "judge_config: {}",
            serde_json::to_string(&judge.display().to_string()).unwrap()
        )]],
    );
    let outcome = run.run_bin(bin, &provider, "", &[]);
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    let report: serde_json::Value = serde_json::from_str(&outcome.stdout).unwrap();
    assert_eq!(report["completion_reason"], "evaluated", "{report}");
    (run, outcome)
}

/// Every history record under `dir` — oneharness's own JSONL, one record per
/// harness run — as JSON.
fn history_records(dir: &Path) -> Vec<serde_json::Value> {
    let mut records = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                for line in std::fs::read_to_string(&path).unwrap().lines() {
                    let record: serde_json::Value = serde_json::from_str(line).unwrap();
                    if record.get("history_id").is_some() {
                        records.push(record);
                    }
                }
            }
        }
    }
    records
}

#[test]
fn every_history_record_a_run_writes_carries_its_session_turn_role_and_judge_labels() {
    // What `oneharness history watch --label session=<session>` selects one run's
    // harness activity by. Read back from the records the linked core wrote — in
    // process, and through a spawned oneharness running the same core — rather
    // than from the argv onejudge built.
    for seam in Seam::BOTH {
        let run = PostureRun::new("labels", seam);
        let judge = run.judge_config("judge.toml", "");
        let judged = |label: &str| [format!("label: {label}"), judge_config_line(&judge)];
        let (alpha, beta) = (judged("alpha"), judged("beta"));
        let provider = run.split(
            &[],
            &[
                &alpha.iter().map(String::as_str).collect::<Vec<_>>(),
                &beta.iter().map(String::as_str).collect::<Vec<_>>(),
            ],
        );
        let outcome = run.run(&provider, "", &[]);
        assert_eq!(outcome.code, Some(0), "{seam:?}: {}", outcome.stderr);

        let records = history_records(&run.dir.join("history"));
        let mut seen = std::collections::BTreeSet::new();
        for record in &records {
            let labels = &record["labels"];
            assert_eq!(labels["session"], "posture", "{seam:?}: {record}");
            assert_eq!(labels["turn"], "1", "{seam:?}: {record}");
            let role = labels["role"].as_str().expect("a role label");
            let judge = labels.get("judge").and_then(serde_json::Value::as_str);
            match role {
                "worker" => assert_eq!(judge, None, "{seam:?}: {record}"),
                "supervisor" | "judge" => assert!(
                    matches!(judge, Some("alpha" | "beta")),
                    "{seam:?}: a judge-side run names its judge: {record}"
                ),
                other => panic!("{seam:?}: unexpected role `{other}`: {record}"),
            }
            seen.insert(format!("{role}/{}", judge.unwrap_or("-")));
        }
        assert_eq!(
            seen.into_iter().collect::<Vec<_>>(),
            [
                "judge/alpha",
                "judge/beta",
                "supervisor/alpha",
                "supervisor/beta",
                "worker/-"
            ],
            "{seam:?}: every party's runs were recorded and labelled"
        );
    }
}

/// Unix only: the recorded prompts carry the run directory's own spelling, which
/// the placeholders reconcile on a POSIX path and nowhere else.
#[cfg(unix)]
#[test]
fn with_no_mode_configured_the_harness_argv_and_judge_prompts_are_the_0_15_0_ones() {
    // `ONEJUDGE_CAPTURE_POSTURE_BASELINE=<onejudge 0.15.0>` re-records the
    // baseline from that binary instead of comparing (the capture script's path).
    let capture = std::env::var("ONEJUDGE_CAPTURE_POSTURE_BASELINE").ok();
    for seam in Seam::BOTH {
        let bin = capture.as_deref().unwrap_or(onejudge_bin());
        let (_, outcome) = default_posture_run(seam, bin);
        let recorded = serde_json::json!({
            "harness": outcome.harness,
            "oneharness": outcome.oneharness,
        });
        if capture.is_some() {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/golden/judge-posture-0.15.0")
                .join(format!("{}.json", seam.name()));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                serde_json::to_string_pretty(&recorded).unwrap() + "\n",
            )
            .unwrap();
            continue;
        }
        let baseline = posture_baseline(seam);
        // Four evaluator invocations — the supervisor's decision, the `done_when`
        // re-judge, the eval and the assessment — every one read-only.
        assert_eq!(outcome.judge_side().len(), 4, "{seam:?}");
        assert_eq!(
            recorded["harness"], baseline["harness"],
            "{seam:?}: the harness argv or a judge prompt differs from onejudge 0.15.0's"
        );
    }
}

#[test]
fn a_judge_with_no_mode_differs_from_0_15_0_by_the_defaults_config_no_mode_and_run_labels() {
    // The intended changes to what onejudge asks oneharness for: an evaluator
    // call leads with onejudge's defaults file and carries no `--mode`, and every
    // run carries the `--history-label`s that say which run, turn, role and judge
    // it is. The agent's turn and everything else about the call are 0.15.0's
    // argv.
    let baseline = posture_baseline(Seam::Spawned);
    let (run, outcome) = default_posture_run(Seam::Spawned, onejudge_bin());
    let decision = outcome.decision("oneharness");
    let defaults = decision["posture"]["config_files"][0].clone();
    assert_defaults_file(&defaults);
    let expected: Vec<Vec<String>> = baseline["oneharness"]
        .as_array()
        .unwrap()
        .iter()
        .map(|argv| {
            let argv: Vec<String> = serde_json::from_value(argv.clone()).unwrap();
            if !argv.iter().any(|arg| arg == "--mode") {
                return argv;
            }
            let mut out = Vec::new();
            let mut args = argv.into_iter();
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--mode" => {
                        assert_eq!(args.next().as_deref(), Some("read-only"));
                    }
                    "--config" => {
                        out.push("--config".to_string());
                        out.push(defaults.as_str().unwrap().to_string());
                        out.push(arg);
                    }
                    _ => out.push(arg),
                }
            }
            out
        })
        .collect();
    let (unlabelled, labels): (Vec<Vec<String>>, Vec<Vec<String>>) = outcome
        .oneharness
        .iter()
        .map(|argv| {
            let mut rest = Vec::new();
            let mut labels = Vec::new();
            let mut args = argv.iter();
            while let Some(arg) = args.next() {
                if arg == "--history-label" {
                    labels.push(args.next().unwrap().clone());
                } else {
                    rest.push(arg.clone());
                }
            }
            (rest, labels)
        })
        .unzip();
    assert_eq!(unlabelled, expected);
    let judged = |role: &str| {
        vec![
            "judge=oneharness".to_string(),
            format!("role={role}"),
            "session=posture".to_string(),
            "turn=1".to_string(),
        ]
    };
    assert_eq!(
        labels,
        vec![
            vec![
                "role=worker".to_string(),
                "session=posture".to_string(),
                "turn=1".to_string(),
            ],
            judged("supervisor"),
            judged("judge"),
            judged("judge"),
            judged("judge"),
        ]
    );

    // In process there is no argv, so the same resolution is read off the
    // report on both seams: read-only, set by the defaults file, attributed to
    // that file by `oneharness config` over the same list.
    let in_process = default_posture_run(Seam::InProcess, onejudge_bin());
    for (seam, (run, outcome)) in [
        (Seam::Spawned, (run, outcome)),
        (Seam::InProcess, in_process),
    ] {
        let posture = outcome.decision("oneharness")["posture"].clone();
        assert_eq!(posture["mode"], "read-only", "{seam:?}");
        assert_eq!(posture["source"], posture["config_files"][0], "{seam:?}");
        assert_defaults_file(&posture["source"]);
        let explained = run.explain(posture["config_files"].as_array().unwrap());
        assert_eq!(explained["mode"]["value"], "read-only", "{seam:?}");
        assert_eq!(explained["mode"]["source"], posture["source"], "{seam:?}");
        // A default judge asks for no events, so none are recorded.
        assert!(outcome.decision("oneharness").get("events").is_none());
        for attribution in outcome.report()["telemetry"]["attribution"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["role"] == "judge")
        {
            assert_eq!(attribution["posture"], posture, "{seam:?}");
        }
    }
}

#[test]
fn an_auto_judge_config_grants_the_judge_a_shell_and_records_what_it_did() {
    for seam in Seam::BOTH {
        let run = PostureRun::new("auto", seam);
        let judge = run.judge_config("judge.toml", "mode = \"auto\"\n");
        let provider = run.split(&[], &[&[&judge_config_line(&judge)]]);
        let outcome = run.run(
            &provider,
            " [[judge-event:git log --oneline]]",
            &["--stream"],
        );
        assert_eq!(outcome.code, Some(0), "{seam:?}: {}", outcome.stderr);

        // The harness ran every evaluator call in claude-code's `auto` mode —
        // shell included — and the prompt says so instead of forbidding one.
        let judged = outcome.judge_side();
        assert_eq!(judged.len(), 4, "{seam:?}");
        for call in &judged {
            let argv = argv_of(call);
            assert!(
                argv.windows(2).any(|w| w == ["--permission-mode", "auto"]),
                "{seam:?}: {argv:?}"
            );
            assert!(
                !argv.iter().any(|arg| arg == "--tools"),
                "{seam:?}: {argv:?}"
            );
            let prompt = prompt_of(call);
            assert!(
                prompt.contains("EVIDENCE CONTRACT (MODE: auto)"),
                "{prompt}"
            );
            assert!(!prompt.contains("never a shell command"), "{prompt}");
            assert!(prompt.contains("running the tests"), "{prompt}");
            // The closed requests stay offered.
            assert!(prompt.contains(r#"{"tool":"git_diff"}"#), "{prompt}");
        }

        // The decision records the posture, the file that set it, and what the
        // judge did while deciding.
        let decision = outcome.decision("oneharness");
        let posture = &decision["posture"];
        assert_eq!(posture["mode"], "auto", "{seam:?}");
        assert_same_file(&posture["source"], &judge);
        let files = posture["config_files"].as_array().unwrap();
        assert_eq!(files.len(), 2, "{seam:?}: {files:?}");
        assert_defaults_file(&files[0]);
        assert_same_file(&files[1], &judge);
        let explained = run.explain(files);
        assert_eq!(explained["mode"]["value"], "auto");
        assert_eq!(explained["mode"]["source"], posture["source"]);
        let events = decision["events"].as_array().expect("the judge's events");
        let commands: Vec<&serde_json::Value> = events
            .iter()
            .filter(|e| e["kind"] == "tool_call")
            .map(|e| &e["input"]["command"])
            .collect();
        // The double runs the command once per time its prompt names it.
        assert!(!commands.is_empty(), "{seam:?}: {events:?}");
        assert!(
            commands
                .iter()
                .all(|command| *command == "git log --oneline"),
            "{seam:?}: {events:?}"
        );

        // `--stream` published the same events as `judge_tool` lines, under the
        // judge's label, before the terminal result.
        let lines = outcome.stream_lines();
        let judge_lines: Vec<&serde_json::Value> =
            lines.iter().filter(|l| l["type"] == "judge_tool").collect();
        assert_eq!(judge_lines.len(), events.len(), "{seam:?}: {lines:?}");
        for (line, event) in judge_lines.iter().zip(events) {
            assert_eq!(line["turn"], 1);
            assert_eq!(line["judge"], "oneharness");
            assert_eq!(&line["event"], event);
        }
        assert_eq!(lines.last().unwrap()["type"], "result");

        // Spawned, the evaluator calls passed the defaults then the judge's
        // config, and no mode.
        if seam == Seam::Spawned {
            let evaluators = outcome.evaluator_argv();
            assert_eq!(evaluators.len(), 4);
            for argv in evaluators {
                let configs: Vec<&String> = argv
                    .windows(2)
                    .filter(|w| w[0] == "--config")
                    .map(|w| &w[1])
                    .collect();
                assert_eq!(configs, [files[0].as_str().unwrap(), "{{RUN}}/judge.toml"]);
                assert!(!argv.iter().any(|arg| arg == "--mode"), "{argv:?}");
                assert!(argv.iter().any(|arg| arg == "--events"), "{argv:?}");
            }
        }
    }
}

#[test]
fn an_oneharness_mode_override_in_the_environment_beats_the_judges_config() {
    for seam in Seam::BOTH {
        let run = PostureRun::new("env", seam).with_env("ONEHARNESS_MODE", "edit");
        let judge = run.judge_config("judge.toml", "mode = \"auto\"\n");
        let provider = run.split(&[], &[&[&judge_config_line(&judge)]]);
        let outcome = run.run(&provider, "", &[]);
        assert_eq!(outcome.code, Some(0), "{seam:?}: {}", outcome.stderr);
        let posture = outcome.decision("oneharness")["posture"].clone();
        assert_eq!(posture["mode"], "edit", "{seam:?}");
        assert_eq!(posture["source"], "environment", "{seam:?}");
        let files = posture["config_files"].as_array().unwrap();
        assert_eq!(files.last().unwrap(), "environment", "{files:?}");
        let explained = run.explain(&files[..files.len() - 1]);
        assert_eq!(explained["mode"]["value"], "edit");
        assert_eq!(explained["mode"]["source"], "environment");
        for call in outcome.judge_side() {
            let argv = argv_of(call);
            assert!(
                argv.windows(2)
                    .any(|w| w == ["--permission-mode", "acceptEdits"]),
                "{seam:?}: {argv:?}"
            );
            assert!(prompt_of(call).contains("EVIDENCE CONTRACT (MODE: edit)"));
        }
    }
}

#[test]
fn with_no_judge_config_the_discovered_files_follow_the_defaults() {
    // No `judge_config:` and no `oneharness.judge.toml` in the working directory:
    // the judge runs under the defaults, then the user file, then the project
    // file oneharness's discovery finds — so the project file's `auto` decides.
    for seam in Seam::BOTH {
        let run = PostureRun::new("discovered", seam);
        let user = run.dir.join("user.toml");
        std::fs::write(&user, "timeout = 600\n").unwrap();
        let project = run.dir.join("oneharness.toml");
        std::fs::write(&project, harness_toml(&run.dir, "mode = \"auto\"\n")).unwrap();
        let run = run.with_env("ONEHARNESS_CONFIG", user.to_str().unwrap());
        let provider = run.split(&[], &[&[]]);
        let outcome = run.run(&provider, "", &[]);
        assert_eq!(outcome.code, Some(0), "{seam:?}: {}", outcome.stderr);
        let posture = outcome.decision("oneharness")["posture"].clone();
        assert_eq!(posture["mode"], "auto", "{seam:?}");
        assert_same_file(&posture["source"], &project);
        let files = posture["config_files"].as_array().unwrap();
        assert_eq!(files.len(), 3, "{files:?}");
        assert_defaults_file(&files[0]);
        assert_same_file(&files[1], &user);
        assert_same_file(&files[2], &project);
        let explained = run.explain(files);
        assert_eq!(explained["mode"]["source"], posture["source"]);
        for call in outcome.judge_side() {
            let argv = argv_of(call);
            assert!(
                argv.windows(2).any(|w| w == ["--permission-mode", "auto"]),
                "{seam:?}: {argv:?}"
            );
        }
        if seam == Seam::Spawned {
            for argv in outcome.evaluator_argv() {
                let configs: Vec<&String> = argv
                    .windows(2)
                    .filter(|w| w[0] == "--config")
                    .map(|w| &w[1])
                    .collect();
                assert_eq!(
                    configs,
                    [
                        files[0].as_str().unwrap(),
                        "{{RUN}}/user.toml",
                        "{{RUN}}/oneharness.toml"
                    ]
                );
            }
        }
    }
}

#[test]
fn a_judges_instructions_are_appended_to_its_prompts_and_no_other_judges() {
    // Two read-only judges, one with `instructions`: its prompts are the 0.15.0
    // prompts with the instructions appended, and its neighbour's are exactly
    // the 0.15.0 prompts.
    #[cfg(unix)]
    let baseline: Vec<String> = posture_baseline(Seam::InProcess)["harness"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|call| call.to_string().contains("EVIDENCE CONTRACT ("))
        .map(prompt_of)
        .collect();
    let instructions = "Run `cargo test` yourself and read the commit log.";
    for seam in Seam::BOTH {
        let run = PostureRun::new("instructions", seam);
        let judge = run.judge_config("judge.toml", "");
        let line = judge_config_line(&judge);
        let provider = run.split(
            &[],
            &[
                &[
                    &line,
                    "label: reviewer",
                    &format!("instructions: {instructions:?}"),
                ],
                &[&line, "label: plain"],
            ],
        );
        let outcome = run.run(&provider, "", &[]);
        assert_eq!(outcome.code, Some(0), "{seam:?}: {}", outcome.stderr);
        let suffix = format!("\n\nInstructions for this judge:\n{instructions}");
        let (reviewer, plain): (Vec<String>, Vec<String>) = outcome
            .judge_side()
            .into_iter()
            .map(prompt_of)
            .partition(|prompt| prompt.contains("Instructions for this judge:"));
        assert_eq!(reviewer.len(), 4, "{seam:?}");
        assert_eq!(plain.len(), 4, "{seam:?}");
        for prompt in &reviewer {
            assert!(prompt.ends_with(&suffix), "{seam:?}: {prompt}");
        }
        let stripped: Vec<String> = reviewer
            .iter()
            .map(|prompt| prompt.strip_suffix(&suffix).unwrap().to_string())
            .collect();
        let mut sorted_plain = plain.clone();
        sorted_plain.sort();
        let mut sorted_stripped = stripped.clone();
        sorted_stripped.sort();
        assert_eq!(sorted_stripped, sorted_plain, "{seam:?}");
        #[cfg(unix)]
        {
            let mut expected = baseline.clone();
            expected.sort();
            assert_eq!(sorted_plain, expected, "{seam:?}: not 0.15.0's prompts");
        }
    }
}

#[test]
fn a_judges_events_setting_switches_its_events_either_way() {
    // An `auto` judge told not to record events, beside a read-only judge told
    // to — a writable judge in a panel the split explicitly allows.
    for seam in Seam::BOTH {
        let run = PostureRun::new("events", seam);
        let auto = run.judge_config("auto.toml", "mode = \"auto\"\n");
        let plain = run.judge_config("plain.toml", "");
        let provider = run.split(
            &["allow_writable_judges: true"],
            &[
                &[&judge_config_line(&auto), "label: quiet", "events: false"],
                &[&judge_config_line(&plain), "label: loud", "events: true"],
            ],
        );
        let outcome = run.run(&provider, " [[judge-event:git status]]", &["--stream"]);
        assert_eq!(outcome.code, Some(0), "{seam:?}: {}", outcome.stderr);
        let quiet = outcome.decision("quiet");
        assert_eq!(quiet["posture"]["mode"], "auto");
        assert!(quiet.get("events").is_none(), "{seam:?}: {quiet}");
        let loud = outcome.decision("loud");
        assert_eq!(loud["posture"]["mode"], "read-only");
        let events = loud["events"].as_array().expect("the loud judge's events");
        assert!(
            events.iter().any(|e| e["input"]["command"] == "git status"),
            "{events:?}"
        );
        let judges: Vec<String> = outcome
            .stream_lines()
            .iter()
            .filter(|l| l["type"] == "judge_tool")
            .map(|l| l["judge"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(judges.len(), events.len(), "{seam:?}");
        assert!(judges.iter().all(|judge| judge == "loud"), "{judges:?}");
    }
}

#[test]
fn a_panel_with_a_writable_judge_is_refused_before_any_harness_runs() {
    for seam in Seam::BOTH {
        let run = PostureRun::new("refused", seam);
        let auto = run.judge_config("auto.toml", "mode = \"auto\"\n");
        let plain = run.judge_config("plain.toml", "");
        let provider = run.split(
            &[],
            &[
                &[&judge_config_line(&plain), "label: reader"],
                &[&judge_config_line(&auto), "label: fixer"],
            ],
        );
        let outcome = run.run(&provider, "", &[]);
        assert_eq!(outcome.code, Some(2), "{seam:?}: {}", outcome.stderr);
        assert!(
            outcome.stderr.contains("judge `fixer`")
                && outcome.stderr.contains("`auto`")
                && outcome.stderr.contains("allow_writable_judges"),
            "{seam:?}: {}",
            outcome.stderr
        );
        assert!(
            outcome.harness.is_empty(),
            "a harness ran: {:?}",
            outcome.harness
        );
        assert!(outcome.oneharness.is_empty());
    }
}

#[test]
fn an_observed_run_delivers_a_judges_tool_events_after_its_turn_opens_and_before_it_decides() {
    // Through the library's observing entry point, in this process — so the
    // environment the linked engine and a spawned oneharness read is this
    // test's own (nextest gives every test its own process).
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("ONEHARNESS_")) {
        std::env::remove_var(key);
    }
    for seam in Seam::BOTH {
        let run = PostureRun::new("observed", seam);
        std::env::set_current_dir(&run.dir).unwrap();
        std::env::set_var("XDG_STATE_HOME", run.dir.join("state"));
        std::env::set_var("XDG_CONFIG_HOME", run.dir.join("xdg"));
        if seam == Seam::Spawned {
            std::env::set_var("ONEJUDGE_FAKE_ONEHARNESS_ENGINE", "1");
        }
        let judge = run.judge_config("judge.toml", "mode = \"auto\"\n");
        let yaml = run.yaml(
            &run.split(&[], &[&[&judge_config_line(&judge)]]),
            " [[judge-event:cargo test]]",
        );
        let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();
        let mut seen: Vec<String> = Vec::new();
        let summary = run_plan_observing_reporting_failure(plan, &mut |observation| {
            seen.push(match observation {
                Observation::TurnOpened(o) => format!("opened/{:?}/{}", o.role, o.turn),
                Observation::Tool(e) => format!("tool/{}", e.turn),
                Observation::Action(a) => format!("action/{}", a.turn),
                Observation::Message(m) => format!("said/{:?}/{}", m.role, m.turn),
                Observation::TurnClosed(c) => format!("closed/{:?}/{}", c.role, c.turn),
                Observation::JudgeDecided(d) => format!("judged/{}/{}", d.turn, d.judge),
                Observation::JudgeTool(t) => {
                    format!("judge-tool/{}/{}/{}", t.turn, t.judge, t.event.summary())
                }
            });
            ControlFlow::Continue(())
        })
        .unwrap_or_else(|failure| panic!("{seam:?}: {}", failure.error));
        assert!(summary.completed, "{seam:?}");
        // The serialized form is the agreed seam: a `judge_tool` tag around the
        // turn, the judge's label, and the payload `Observation::Tool` carries.
        let event = onejudge::ToolEvent {
            kind: "tool_call".into(),
            name: Some("Bash".into()),
            input: Some(serde_json::json!({"command": "cargo test"})),
            output: None,
            index: 0,
            tool_call_id: None,
        };
        assert_eq!(
            serde_json::to_value(Observation::JudgeTool(onejudge::JudgeTool {
                turn: 1,
                judge: "oneharness",
                event: &event,
            }))
            .unwrap(),
            serde_json::json!({"type": "judge_tool", "turn": 1, "judge": "oneharness", "event": event})
        );

        let opened = seen
            .iter()
            .position(|s| s == "opened/User/1")
            .unwrap_or_else(|| panic!("{seam:?}: {seen:?}"));
        let decided = seen
            .iter()
            .position(|s| s == "judged/1/oneharness")
            .unwrap_or_else(|| panic!("{seam:?}: {seen:?}"));
        let tools: Vec<usize> = seen
            .iter()
            .enumerate()
            .filter(|(_, s)| s.starts_with("judge-tool/1/oneharness/"))
            .map(|(i, _)| i)
            .collect();
        assert!(!tools.is_empty(), "{seam:?}: {seen:?}");
        assert!(
            tools.iter().all(|&i| opened < i && i < decided),
            "{seam:?}: {seen:?}"
        );
        assert!(
            seen[tools[0]].contains("Bash") && seen[tools[0]].contains("cargo test"),
            "{seam:?}: {seen:?}"
        );
    }
}

#[test]
fn a_bare_oneharness_providers_judge_publishes_its_events_under_its_own_label() {
    // Not a panel: no decisions are recorded, and the judge's events still reach
    // `--stream` as `judge_tool` lines, under the provider's label.
    for seam in Seam::BOTH {
        let run = PostureRun::new("bare", seam);
        let judge = run.judge_config("judge.toml", "mode = \"auto\"\n");
        let provider = format!(
            "provider:\n  kind: oneharness\n{}  {}\n",
            seam.bin_line(2),
            judge_config_line(&judge)
        );
        let outcome = run.run(&provider, " [[judge-event:cargo test]]", &["--stream"]);
        assert_eq!(outcome.code, Some(0), "{seam:?}: {}", outcome.stderr);
        let lines = outcome.stream_lines();
        let tools: Vec<&serde_json::Value> =
            lines.iter().filter(|l| l["type"] == "judge_tool").collect();
        assert!(!tools.is_empty(), "{seam:?}: {lines:?}");
        assert!(tools
            .iter()
            .all(|t| t["judge"] == "oneharness" && t["turn"] == 1));
        let report = outcome.report();
        assert!(report.get("judge_decisions").is_none(), "{report}");
        let judged: Vec<&serde_json::Value> = report["telemetry"]["attribution"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["role"] == "judge")
            .collect();
        assert!(!judged.is_empty());
        assert!(judged.iter().all(|a| a["posture"]["mode"] == "auto"));
    }
}

// --- The text view, the published stream, and `onejudge watch` ---------------

/// A fresh, empty directory `onejudge watch` streams are kept in.
fn watch_dir(name: &str) -> std::path::PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("watch-dir-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// `onejudge` with `args`, hermetic: every `ONEHARNESS_*` override removed and
/// streams kept in `watch`.
fn onejudge_in(watch: &Path, cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(onejudge_bin());
    command
        .args(args)
        .current_dir(cwd)
        .env("ONEJUDGE_WATCH_DIR", watch);
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("ONEHARNESS_")) {
        command.env_remove(key);
    }
    command
}

/// What `onejudge watch <session> --format json` prints for a finished run: its
/// exit code and each published record.
fn watched_records(
    watch: &Path,
    cwd: &Path,
    session: &str,
) -> (Option<i32>, Vec<serde_json::Value>) {
    let output = onejudge_in(watch, cwd, &["watch", session, "--format", "json"])
        .output()
        .unwrap();
    let records = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (output.status.code(), records)
}

/// What oneharness's own renderer draws for each worker event the run
/// published, in order — the lines the text view must print, computed from the
/// very events, never copied by hand.
fn rendered_worker_events(records: &[serde_json::Value]) -> Vec<(String, String)> {
    records
        .iter()
        .filter(|record| record["type"] == "action")
        .filter_map(|record| {
            let event: onejudge::ActionEvent =
                serde_json::from_value(record["event"].clone()).unwrap();
            oneharness_core::domain::render::render_event(&event).map(|line| (event.kind, line))
        })
        .collect()
}

/// Assert `text` holds each of `blocks` as whole lines, once each, in order.
fn assert_lines_in_order(text: &str, blocks: &[String]) {
    let padded = format!("\n{text}");
    let mut from = 0;
    for block in blocks {
        let needle = format!("\n{block}\n");
        assert_eq!(
            padded.matches(&needle).count(),
            1,
            "`{block}` once in:\n{text}"
        );
        let at = padded[from..]
            .find(&needle)
            .unwrap_or_else(|| panic!("`{block}` out of order in:\n{text}"));
        from += at + 1;
    }
}

#[test]
fn text_output_draws_the_workers_events_and_each_judges_through_oneharnesss_renderer() {
    // One conversation per seam — in process, and through a spawned oneharness
    // running the same core — whose worker reasons, runs a command and speaks, and
    // whose judge runs a tool of its own before it decides.
    for seam in Seam::BOTH {
        let watch = watch_dir(&format!("render-{}", seam.name()));
        let run = PostureRun::new("text-render", seam)
            .with_env("ONEJUDGE_WATCH_DIR", watch.to_str().unwrap());
        let judge = run.judge_config("judge.toml", "mode = \"auto\"\n");
        let provider = run.split(&[], &[&[&judge_config_line(&judge)]]);
        let outcome = run.run(
            &provider,
            " [[think:check the tests first]] [[event:cargo test]] [[say:the tests pass now]] \
             [[judge-event:cargo clippy]]",
            &["--format", "text"],
        );
        assert_eq!(outcome.code, Some(0), "{seam:?}: {}", outcome.stderr);
        let stdout = &outcome.stdout;

        let (code, records) = watched_records(&watch, &run.dir, "posture");
        assert_eq!(code, Some(0), "{seam:?}");
        let worker = rendered_worker_events(&records);
        let kinds: Vec<&str> = worker.iter().map(|(kind, _)| kind.as_str()).collect();
        assert_eq!(kinds, ["reasoning", "tool_call", "message"], "{seam:?}");
        let lines: Vec<String> = worker.into_iter().map(|(_, line)| line).collect();
        assert_lines_in_order(stdout, &lines);

        // The judge's own tool event, through the same renderer, under its label.
        let judged: Vec<String> = records
            .iter()
            .filter(|record| record["type"] == "judge_tool")
            .filter_map(|record| {
                let tool: onejudge::ToolEvent =
                    serde_json::from_value(record["event"].clone()).unwrap();
                let event = onejudge::ActionEvent {
                    kind: tool.kind,
                    name: tool.name,
                    input: tool.input,
                    output: tool.output,
                    index: tool.index,
                    tool_call_id: tool.tool_call_id,
                    started_at: None,
                    finished_at: None,
                    duration_ms: None,
                    status: None,
                    timing_source: None,
                };
                // A `tool_result` draws nothing: its call was already drawn.
                let line = oneharness_core::domain::render::render_event(&event)?;
                Some(format!("{}: {line}", record["judge"].as_str().unwrap()))
            })
            .collect();
        // One drawn line per call the judge's harness reported (the double runs
        // its scripted command once per time the prompt names it).
        assert!(!judged.is_empty(), "{seam:?}");
        assert!(
            judged
                .iter()
                .all(|line| line == "oneharness: $ cargo clippy"),
            "{seam:?}: {judged:?}"
        );
        let drawn: Vec<usize> = stdout
            .lines()
            .enumerate()
            .filter(|(_, line)| *line == judged[0])
            .map(|(at, _)| at)
            .collect();
        assert_eq!(drawn.len(), judged.len(), "{seam:?}:\n{stdout}");
        let decided = stdout
            .lines()
            .position(|line| line.starts_with("oneharness  done"))
            .unwrap_or_else(|| panic!("{seam:?}: the judge's decision is drawn:\n{stdout}"));
        assert!(
            drawn.iter().all(|at| *at < decided),
            "{seam:?}: its tool events come before it decides"
        );
        assert!(!stdout.contains('{'), "{seam:?}: no raw JSON:\n{stdout}");

        // Only tool activity became a `ToolEvent`: every judge prompt lists the
        // command alone, never the agent's words or its reasoning.
        for call in outcome.judge_side() {
            let prompt = prompt_of(call);
            let tools: Vec<&str> = prompt
                .lines()
                .filter(|line| line.starts_with("  [tool] "))
                .collect();
            assert_eq!(
                tools,
                [r#"  [tool] Bash({"command":"cargo test"})"#],
                "{seam:?}: {prompt}"
            );
        }
        let report = outcome.report_from_watch(&records);
        assert_eq!(
            report["transcript"]["messages"][1]["events"]
                .as_array()
                .map(Vec::len),
            Some(1),
            "{seam:?}"
        );
    }
}

impl PostureOutcome {
    /// The report a run published as its result — how a text run's report is read.
    fn report_from_watch(&self, records: &[serde_json::Value]) -> serde_json::Value {
        let last = records.last().expect("the run published its result");
        assert_eq!(last["type"], "result", "{last}");
        last["report"].clone()
    }
}

/// A config streaming the worker's turn through `skill_extra` (a oneharness
/// entry's own lines) and completing on the echo double's supervisor.
fn streamed_text_config(dir: &Path, skill_extra: &str, system: &str) -> std::path::PathBuf {
    let echo = serde_json::to_string(&echo_bin()).unwrap();
    let config = dir.join("onejudge.yaml");
    std::fs::write(
        &config,
        format!(
            "provider:\n  kind: split\n  skill:\n    kind: oneharness\n    stream: true\n{skill_extra}  \
             judge:\n    kind: command\n    command: [{echo}, \"[[supervisor-complete:looks right]]\"]\n\
             task: fix it\nsystem_prompt: {}\nuser:\n  persona: a reviewer\n  max_turns: 2\n\
             session: streamed\n",
            serde_json::to_string(system).unwrap()
        ),
    )
    .unwrap();
    config
}

#[test]
fn a_streamed_turn_is_drawn_from_the_events_oneharness_streams_on_both_seams() {
    // The two live paths an event reaches the view by: the in-process `EventSink`,
    // and a spawned oneharness's NDJSON `event` lines.
    let system = "[[reply:fixed]] [[think:look first]] [[event:cargo test]] [[say:all green]]";
    let oh = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    for (name, extra) in [
        ("in-process", String::new()),
        ("spawned", format!("    bin: {oh}\n")),
    ] {
        let dir = in_process_project(&format!("streamed-text-{name}"), "");
        let watch = watch_dir(&format!("streamed-{name}"));
        let config = streamed_text_config(&dir, &extra, system);
        let output = onejudge_in(&watch, &dir, &["run", config.to_str().unwrap()])
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(0), "{name}: {stdout}");
        let (_, records) = watched_records(&watch, &dir, "streamed");
        let worker = rendered_worker_events(&records);
        let kinds: Vec<&str> = worker.iter().map(|(kind, _)| kind.as_str()).collect();
        assert_eq!(kinds, ["reasoning", "tool_call", "message"], "{name}");
        let lines: Vec<String> = worker.into_iter().map(|(_, line)| line).collect();
        assert_lines_in_order(&stdout, &lines);
        assert!(
            stdout.starts_with("── turn 1 · worker ──"),
            "{name}: {stdout}"
        );
        assert!(
            stdout.contains("── done: completed after 1 turn"),
            "{name}: {stdout}"
        );
    }
}

#[test]
fn binary_text_format_is_live_streamable_aliased_and_writes_its_output() {
    let dir = scratch_path("text-formats");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let watch = watch_dir("formats");
    let oh = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    let config = streamed_text_config(
        &dir,
        &format!("    bin: {oh}\n"),
        "[[reply:fixed]] [[event:cargo test]] [[say:all green]]",
    );
    let config = config.to_str().unwrap();
    let run = |args: &[&str]| {
        let mut argv = vec!["run", config];
        argv.extend_from_slice(args);
        onejudge_in(&watch, &dir, &argv).output().unwrap()
    };
    // What a text run draws, less the two measured durations.
    let shape = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|line| !line.starts_with("  done in ") && !line.starts_with("── done:"))
            .map(str::to_string)
            .collect()
    };

    let default = run(&[]);
    assert_eq!(default.status.code(), Some(0));
    let drawn = String::from_utf8(default.stdout).unwrap();
    assert!(drawn.contains("\n$ cargo test\n› all green\n"), "{drawn}");
    for args in [
        &["--format", "text", "--stream"][..],
        &["--format", "human"][..],
        &["--format", "human", "--stream"][..],
    ] {
        let output = run(args);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert_eq!(
            shape(&String::from_utf8(output.stdout).unwrap()),
            shape(&drawn),
            "{args:?} draws the run as the default does"
        );
    }

    // `--output` takes the whole text; the live lines move to stderr.
    let file = dir.join("run.txt");
    let output = run(&["--output", file.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    let written = std::fs::read_to_string(&file).unwrap();
    assert_eq!(shape(&written), shape(&drawn));
    assert!(written.contains("Status: completed"), "{written}");
    let live = String::from_utf8(output.stderr).unwrap();
    assert!(live.contains("\n$ cargo test\n"), "{live}");

    // …and under `--format json` it takes the versioned report, as it always has.
    let file = dir.join("report.json");
    let output = run(&["--format", "json", "--output", file.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    let report: onejudge::Report =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(report.schema_version, onejudge::SCHEMA_VERSION);
    assert_eq!(report.transcript.messages[1].content, "fixed");
}

#[test]
fn watch_follows_a_live_run_from_its_start_and_never_mixes_in_an_earlier_one() {
    use std::io::BufRead as _;

    let dir = scratch_path("watch-live");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let watch = watch_dir("live");
    let oh = serde_json::to_string(&fake_oneharness_bin()).unwrap();
    let skill = format!("    bin: {oh}\n");

    // An earlier run under the same session name, finished.
    let earlier = streamed_text_config(&dir, &skill, "[[reply:the earlier run]]");
    let output = onejudge_in(&watch, &dir, &["run", earlier.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));

    // The run under watch holds its turn open after its first event until the
    // test releases it — so the watchers start while it is genuinely running.
    let release = dir.join("release");
    let config = streamed_text_config(
        &dir,
        &skill,
        &format!(
            "[[reply:the watched run]] [[event:cargo test]] [[stream-wait:{}]]",
            release.display()
        ),
    );
    let mut running = onejudge_in(&watch, &dir, &["run", config.to_str().unwrap()])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut live = std::io::BufReader::new(running.stdout.take().unwrap());
    let mut printed = String::new();
    loop {
        let mut line = String::new();
        assert!(
            live.read_line(&mut line).unwrap() > 0,
            "the run ended early: {printed}"
        );
        printed.push_str(&line);
        if line == "$ cargo test\n" {
            break;
        }
    }

    let spawn_watch = |format: &str| {
        onejudge_in(&watch, &dir, &["watch", "streamed", "--format", format])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let text = spawn_watch("text");
    let json = spawn_watch("json");
    // Both watchers are following before the turn can end.
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::fs::write(&release, "").unwrap();

    let mut rest = String::new();
    std::io::Read::read_to_string(&mut live, &mut rest).unwrap();
    printed.push_str(&rest);
    let status = running.wait().unwrap();
    assert_eq!(status.code(), Some(0), "{printed}");
    assert!(printed.contains("the watched run"), "{printed}");

    let text = text.wait_with_output().unwrap();
    let json = json.wait_with_output().unwrap();
    // Replayed from the start and followed to the end: exactly what `run` printed.
    assert_eq!(String::from_utf8(text.stdout).unwrap(), printed);
    assert_eq!(text.status.code(), Some(0));
    assert_eq!(json.status.code(), Some(0));

    let records: Vec<serde_json::Value> = String::from_utf8(json.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records[0]["type"], "turn_opened", "{records:#?}");
    assert_eq!(records.last().unwrap()["type"], "result");
    assert_eq!(records.last().unwrap()["exit_code"], 0);
    assert!(
        records
            .iter()
            .any(|r| r["type"] == "action" && r["event"]["input"]["command"] == "cargo test"),
        "{records:#?}"
    );
    let all = serde_json::to_string(&records).unwrap();
    assert!(all.contains("the watched run"));
    assert!(!all.contains("the earlier run"), "{all}");
}
