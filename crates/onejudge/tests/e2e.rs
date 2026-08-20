//! End-to-end journeys that drive the real [`Engine`] across a **real subprocess
//! boundary**. Nothing here is mocked: `CommandProvider` and `OneharnessProvider`
//! are pointed at the deterministic test-double binaries (`onejudge-echo-provider`
//! and `onejudge-fake-oneharness`), so the argument encoding, subprocess spawn,
//! JSON-lines / report protocols, converse loop, session threading, events
//! rendering, and verdict parsing all run for real — the only faked thing is the
//! model, exactly as a consumer would fake it.
//!
//! The whole file is gated on the `fake-provider` feature, since the doubles only
//! exist under it; the gate (`just check`, `just test-e2e`, coverage) always
//! enables it, so these journeys always run — they are never `#[ignore]`-d out.
#![cfg(feature = "fake-provider")]

use std::ops::ControlFlow;

use onejudge::{
    CommandProvider, Conversation, Engine, JudgeKind, JudgeValue, NamedVerdict, Observation,
    OneharnessProvider, ProviderErrorKind, Role, Settings, SimulatedUser, Skill, SplitProvider,
    ToolQuery, Usage, SCHEMA_VERSION,
};

mod support;

use support::{
    assert_profile_is_detached, await_path, descendant_handle, descendant_is_running, scratch_path,
};
#[cfg(unix)]
use support::{kill_group, process_exists, OwnedProcessGroups};

/// A [`CommandProvider`] pointed at the built echo test double.
fn echo() -> CommandProvider {
    CommandProvider::new(vec![
        env!("CARGO_BIN_EXE_onejudge-echo-provider").to_string()
    ])
    .unwrap()
}

/// An [`OneharnessProvider`] pointed at the built fake-oneharness test double.
fn fake_oneharness() -> OneharnessProvider {
    OneharnessProvider::new().with_bin(env!("CARGO_BIN_EXE_onejudge-fake-oneharness"))
}

fn settings() -> Settings {
    Settings::new()
}

fn skill_with(instructions: &str) -> Skill {
    Skill::new("demo", "/skills/demo", instructions)
}

// --- CommandProvider journeys ---------------------------------------------

#[test]
fn single_turn_echoes_and_reports_usage() {
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let outcome = engine
        .run(&Conversation::single_turn(skill_with("Be helpful."), "hi"))
        .unwrap();
    assert_eq!(outcome.transcript.assistant_turns(), 1);
    assert_eq!(outcome.transcript.messages[1].content, "echo: hi");
    assert!(outcome.usage.is_some(), "usage should be aggregated");
}

#[test]
fn multi_turn_runs_to_max_turns() {
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let user = SimulatedUser::new("A patient tester.").max_turns(3);
    let outcome = engine
        .run(&Conversation::multi_turn(
            skill_with("Be helpful."),
            "start",
            user,
        ))
        .unwrap();
    assert_eq!(outcome.transcript.assistant_turns(), 3);
    // A simulated-user turn sits between assistant turns.
    let user_turns = outcome
        .transcript
        .messages
        .iter()
        .filter(|m| m.role == onejudge::Role::User)
        .count();
    assert!(user_turns >= 2);
}

#[test]
fn unified_supervisor_is_one_subprocess_invocation_per_nonterminal_turn() {
    let path =
        std::env::temp_dir().join(format!("onejudge-supervisor-count-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let persona = format!("A tester. [[count:{}]]", path.display());
    let outcome = Engine::new(&echo(), settings())
        .run(&Conversation::multi_turn(
            skill_with("Be helpful."),
            "start",
            SimulatedUser::new(persona).max_turns(3),
        ))
        .unwrap();
    assert_eq!(outcome.transcript.assistant_turns(), 3);
    let calls = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        calls.lines().count(),
        2,
        "each of the two nonterminal turns gets exactly one supervisor process"
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_no_op_supervisor_loop_settles_the_run_on_the_work_it_has() {
    // The other half of the same defect, and the one that cost whole turn budgets:
    // the supervisor's answer is a perfectly valid `continue` every time, so
    // nothing here is malformed — but it asks for nothing, the agent does nothing,
    // and the pair repeats. One measured dispatch was re-prompted 137 times to its
    // cap this way, delivering nothing after the work was already done.
    let count = std::env::temp_dir().join(format!(
        "onejudge-noop-supervisor-count-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&count);
    let persona = format!("[[supervisor-noop]] [[count:{}]]", count.display());
    // The event marker rides the *task*, not the instructions, so exactly the first
    // turn records a command — the released dispatch whose work must survive.
    let outcome = Engine::new(&echo(), settings())
        .run(&Conversation::multi_turn(
            skill_with("Be helpful."),
            "release the dispatch [[event:git push]]",
            SimulatedUser::new(persona).max_turns(40),
        ))
        .expect("the run settles rather than failing");

    assert_eq!(
        outcome.transcript.assistant_turns(),
        1 + onejudge::NOOP_SETTLE_LIMIT as usize,
        "the work turn, then exactly the no-ops it takes to decide — not the cap"
    );
    // The completed work, and the command that did it, are still in the transcript.
    assert_eq!(
        outcome.transcript.messages[1].content,
        "echo: release the dispatch [[event:git push]]"
    );
    assert_eq!(outcome.transcript.messages[1].events.len(), 1);
    assert!(
        outcome.completion_reason.is_none(),
        "settling is not completing"
    );
    let settled = outcome.settled_reason.clone().expect("a settle reason");
    assert!(settled.contains("no-op exchanges"), "{settled}");
    // Every attempt is a real subprocess, and the settling turn pays for no more.
    let asked = std::fs::read_to_string(&count).unwrap();
    assert_eq!(asked.lines().count() as u32, onejudge::NOOP_SETTLE_LIMIT);
    std::fs::remove_file(&count).unwrap();
}

#[test]
fn a_supervisor_with_no_next_instruction_settles_the_run_instead_of_killing_it() {
    // A supervisor that judges the work incomplete and then produces no message
    // used to abort the dispatch through the `CommandProvider` seam, destroying
    // every turn of finished work with it. It is now re-asked, and — since this
    // double answers the same way every time — the run settles on what it has.
    let count = std::env::temp_dir().join(format!(
        "onejudge-settled-supervisor-count-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&count);
    let persona = format!("[[malformed-supervisor]] [[count:{}]]", count.display());
    let outcome = Engine::new(&echo(), settings())
        .run(&Conversation::multi_turn(
            skill_with("Be helpful."),
            "start",
            SimulatedUser::new(persona).max_turns(4),
        ))
        .expect("the run settles rather than failing");

    assert_eq!(
        outcome.transcript.assistant_turns(),
        1,
        "the work the run did is kept"
    );
    assert_eq!(outcome.transcript.messages[1].content, "echo: start");
    assert!(
        outcome.completion_reason.is_none(),
        "settling is not completing"
    );
    let settled = outcome.settled_reason.clone().expect("a settle reason");
    assert!(settled.contains("no next instruction"), "{settled}");
    // Bounded, and every attempt is a real subprocess: one ask plus the re-asks.
    let attempts = std::fs::read_to_string(&count).unwrap();
    assert_eq!(
        attempts.lines().count() as u32,
        onejudge::SUPERVISOR_REASK_LIMIT + 1,
        "the supervisor is re-asked a bounded number of times"
    );
    // A blank `message` never reaches the agent as a user turn.
    assert!(outcome
        .transcript
        .messages
        .iter()
        .all(|m| !m.content.trim().is_empty()));
    std::fs::remove_file(&count).unwrap();
}

#[test]
fn done_when_reads_tool_events_and_stops_early() {
    // The skill runs `git commit` on its first turn; the done_when judge sees that
    // event in the transcript and ends the conversation after one turn — proving
    // events reach the judge (Improvement 1) end to end.
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let user = SimulatedUser::new("A tester.")
        .done_when("git commit")
        .max_turns(9);
    let skill = skill_with("Commit the change. [[event:git commit -m fix]]");
    let outcome = engine
        .run(&Conversation::multi_turn(skill, "please commit", user))
        .unwrap();
    assert_eq!(outcome.transcript.assistant_turns(), 1);
}

#[test]
fn skill_done_flag_ends_a_multi_turn_conversation() {
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let user = SimulatedUser::new("A tester.").max_turns(5);
    let skill = skill_with("Finish immediately. [[done]]");
    let outcome = engine
        .run(&Conversation::multi_turn(skill, "go", user))
        .unwrap();
    assert_eq!(outcome.transcript.assistant_turns(), 1);
}

#[test]
fn simulated_user_stop_ends_the_conversation() {
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let user = SimulatedUser::new("A tester who is done. [[stop]]").max_turns(5);
    let outcome = engine
        .run(&Conversation::multi_turn(
            skill_with("Be helpful."),
            "go",
            user,
        ))
        .unwrap();
    assert_eq!(outcome.transcript.assistant_turns(), 1);
    assert_eq!(
        outcome.transcript.messages.last().unwrap().role,
        onejudge::Role::Assistant
    );
    assert!(outcome.completion_reason.is_some());
}

#[test]
fn events_backed_query_reads_what_the_skill_did() {
    // Improvement 2: assert on tool events directly, no judge call or mock/spy.
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let skill = skill_with("Commit it. [[event:git commit -m fix]]");
    let outcome = engine
        .run(&Conversation::single_turn(skill, "commit"))
        .unwrap();
    let t = &outcome.transcript;
    assert!(t.did(&ToolQuery::tool("bash").with_input_contains("git commit")));
    assert_eq!(t.count_tool_events(&ToolQuery::tool("bash")), 1);
    assert!(!t.did(&ToolQuery::tool("edit_file")));
}

#[test]
fn judge_boolean_can_reason_over_tool_events() {
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let skill = skill_with("Commit it. [[event:git commit -m fix]]");
    let outcome = engine
        .run(&Conversation::single_turn(skill, "commit"))
        .unwrap();
    let hit = engine
        .judge_boolean("git commit", &outcome.transcript)
        .unwrap();
    assert_eq!(hit.value, JudgeValue::Bool(true));
    let miss = engine
        .judge_boolean("deploy to production", &outcome.transcript)
        .unwrap();
    assert_eq!(miss.value, JudgeValue::Bool(false));
}

#[test]
fn judge_numeric_scores_on_the_scale() {
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with("Be warm."),
            "welcome aboard",
        ))
        .unwrap();
    let high = engine
        .judge_numeric("welcome", 1.0, 5.0, &outcome.transcript)
        .unwrap();
    assert_eq!(high.value, JudgeValue::Number(5.0));
    let low = engine
        .judge_numeric("furious", 1.0, 5.0, &outcome.transcript)
        .unwrap();
    assert_eq!(low.value, JudgeValue::Number(1.0));
}

#[test]
fn streaming_sink_break_short_circuits_the_run() {
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let skill = skill_with("Commit it. [[event:git commit -m fix]]");
    let user = SimulatedUser::new("A tester.").max_turns(9);
    let mut seen = 0;
    let outcome = engine
        .run_streaming(
            &Conversation::multi_turn(skill, "commit", user),
            &mut |_ev| {
                seen += 1;
                ControlFlow::Break(())
            },
        )
        .unwrap();
    assert!(outcome.stopped_early);
    assert_eq!(seen, 1);
    assert_eq!(outcome.transcript.assistant_turns(), 1);
}

#[test]
fn command_provider_spawn_failure_is_classified() {
    let provider = CommandProvider::new(vec!["onejudge-no-such-binary-zzz".into()]).unwrap();
    let engine = Engine::new(&provider, settings());
    let err = engine
        .run(&Conversation::single_turn(skill_with("x"), "hi"))
        .unwrap_err();
    assert_eq!(err.kind(), Some(ProviderErrorKind::Spawn));
}

#[test]
fn command_provider_empty_output_is_a_protocol_error() {
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let skill = skill_with("produce nothing [[emit-empty]]");
    let err = engine
        .run(&Conversation::single_turn(skill, "hi"))
        .unwrap_err();
    assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
}

#[test]
fn command_provider_empty_assessment_is_a_protocol_error() {
    // A parsed-but-empty assessment reply is rejected as a classified protocol
    // error, exercised across the real subprocess — the empty-output counterpart
    // for the new `assess` op.
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let outcome = engine
        .run(&Conversation::single_turn(skill_with("Be helpful."), "hi"))
        .unwrap();
    let err = engine
        .assess(
            "summarize follow-up work [[assess-empty]]",
            &outcome.transcript,
        )
        .unwrap_err();
    assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
}

#[test]
fn command_provider_non_zero_exit_is_a_protocol_error() {
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let skill = skill_with("fail hard [[emit-exit]]");
    let err = engine
        .run(&Conversation::single_turn(skill, "hi"))
        .unwrap_err();
    assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
}

#[test]
fn command_provider_rejects_a_wrong_typed_verdict() {
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let outcome = engine
        .run(&Conversation::single_turn(skill_with("Be helpful."), "hi"))
        .unwrap();
    // `[[wrong-type]]` makes the double return a number for a boolean query.
    let err = engine
        .judge_boolean("[[wrong-type]] greeting", &outcome.transcript)
        .unwrap_err();
    assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
    let err = engine
        .judge_numeric("[[wrong-type]] score", 1.0, 5.0, &outcome.transcript)
        .unwrap_err();
    assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
}

#[test]
fn command_provider_rejects_a_wrong_protocol_reply() {
    // Point the JSON-lines CommandProvider at the fake-oneharness binary, which
    // speaks a different protocol: its report has no `message` field, so the
    // response fails to parse and surfaces as a classified protocol error rather
    // than a silent empty turn.
    let provider = CommandProvider::new(vec![
        env!("CARGO_BIN_EXE_onejudge-fake-oneharness").to_string()
    ])
    .unwrap();
    let engine = Engine::new(&provider, settings());
    let err = engine
        .run(&Conversation::single_turn(skill_with("x"), "hi"))
        .unwrap_err();
    assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
}

// --- OneharnessProvider journeys (via the fake oneharness) -----------------

#[test]
fn oneharness_respond_parses_text_usage_and_events() {
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings());
    let skill = skill_with("[[reply:hello there]] [[event:git commit -m fix]]");
    let outcome = engine.run(&Conversation::single_turn(skill, "go")).unwrap();
    assert_eq!(outcome.transcript.messages[1].content, "hello there");
    assert_eq!(
        outcome
            .transcript
            .count_tool_events(&ToolQuery::tool("bash")),
        1
    );
    // Prompt-cache counts flow from the oneharness report through OneharnessUsage
    // into the aggregated usage (a single respond call, so no summing to reason
    // about).
    let usage = outcome.usage.expect("usage aggregated");
    assert_eq!(usage.cache_read_tokens, Some(7));
    assert_eq!(usage.cache_write_tokens, Some(2));
}

#[test]
fn oneharness_multi_turn_drives_the_simulated_user() {
    // A multi-turn run on a session-capable platform exercises the simulated-user
    // call and the session-threaded judge side of OneharnessProvider.
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings().with_session_name("mt"));
    let user = SimulatedUser::new("A patient tester.").max_turns(2);
    let outcome = engine
        .run(&Conversation::multi_turn(
            skill_with("[[reply:ok]]"),
            "start",
            user,
        ))
        .unwrap();
    assert_eq!(outcome.transcript.assistant_turns(), 2);
}

#[test]
fn a_re_asked_supervisor_recovers_the_run_on_the_prompt_seam() {
    // The judge side is a real subprocess here, so this proves the *prompt* the
    // re-ask sends: the double withholds its next instruction until the ask
    // carries the correction naming what was unusable about the last answer.
    let provider = fake_oneharness();
    let outcome = Engine::new(&provider, settings().with_session_name("reask"))
        .run(&Conversation::multi_turn(
            skill_with("[[reply:on it]]"),
            "start",
            SimulatedUser::new("A strict reviewer. [[supervisor-silent-once]]").max_turns(2),
        ))
        .unwrap();
    assert_eq!(
        outcome.transcript.assistant_turns(),
        2,
        "the corrected instruction drove another turn"
    );
    assert!(outcome.settled_reason.is_none(), "the run did not settle");
    assert!(outcome
        .transcript
        .messages
        .iter()
        .any(|m| m.content == "Run the integration suite too."));
}

#[test]
fn a_mock_harness_selection_reaches_the_spawned_oneharness_argv() {
    // The deterministic-harness passthrough, asserted against the argv a real
    // subprocess was really spawned with: the double mirrors `oneharness run`'s flag
    // contract (an unrecognized flag exits non-zero) and replies with the
    // `--mock-harness` ids it was given. This is what makes an acceptance proof free
    // instead of billing a model or being skipped.
    let provider = fake_oneharness().with_mock_harness("claude-code");
    let outcome = Engine::new(&provider, settings())
        .run(&Conversation::single_turn(
            skill_with("[[echo-mock-harness]]"),
            "go",
        ))
        .unwrap();
    assert_eq!(outcome.transcript.messages[1].content, "claude-code");

    // Repeatable, in order, one flag per id.
    let both = fake_oneharness()
        .with_mock_harness("claude-code")
        .with_mock_harness("codex");
    let outcome = Engine::new(&both, settings())
        .run(&Conversation::single_turn(
            skill_with("[[echo-mock-harness]]"),
            "go",
        ))
        .unwrap();
    assert_eq!(outcome.transcript.messages[1].content, "claude-code,codex");

    // And an ordinary run still passes none, so nothing about it changes.
    let outcome = Engine::new(&fake_oneharness(), settings())
        .run(&Conversation::single_turn(
            skill_with("[[echo-mock-harness]]"),
            "go",
        ))
        .unwrap();
    assert_eq!(outcome.transcript.messages[1].content, "none");
}

#[test]
fn a_supervisor_that_stays_silent_settles_the_run_on_the_prompt_seam() {
    // Same defect, same policy, the other seam: a judge that never names a next
    // instruction ends the run on the work it has instead of destroying it.
    let provider = fake_oneharness();
    let outcome = Engine::new(&provider, settings().with_session_name("settle"))
        .run(&Conversation::multi_turn(
            skill_with("[[reply:committed the fix]]"),
            "start",
            SimulatedUser::new("A strict reviewer. [[supervisor-silent]]").max_turns(4),
        ))
        .expect("the run settles rather than failing");
    assert_eq!(outcome.transcript.assistant_turns(), 1);
    assert_eq!(
        outcome.transcript.messages[1].content, "committed the fix",
        "the work the agent did is kept"
    );
    assert!(outcome.completion_reason.is_none());
    let settled = outcome.settled_reason.expect("a settle reason");
    assert!(settled.contains("no next instruction"), "{settled}");
    assert!(settled.contains("cannot say what comes next"), "{settled}");
}

#[test]
fn oneharness_process_failure_is_a_protocol_error() {
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings());
    let err = engine
        .run(&Conversation::single_turn(
            skill_with("[[proc-exit]]"),
            "go",
        ))
        .unwrap_err();
    assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
}

#[test]
fn oneharness_empty_assessment_is_a_provider_error() {
    // An empty assessment reply from the judge side surfaces as a provider error
    // rather than a silent empty result, exercised across the real subprocess.
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings());
    let outcome = engine
        .run(&Conversation::single_turn(skill_with("[[reply:ok]]"), "go"))
        .unwrap();
    let err = engine
        .assess(
            "summarize follow-up work [[assess-empty]]",
            &outcome.transcript,
        )
        .unwrap_err();
    assert!(err.to_string().contains("empty assessment"));
}

#[test]
fn oneharness_failure_kind_propagates_classified() {
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings());
    let skill = skill_with("[[fail:rate_limit]]");
    let err = engine
        .run(&Conversation::single_turn(skill, "go"))
        .unwrap_err();
    assert_eq!(err.kind(), Some(ProviderErrorKind::RateLimit));
}

#[test]
fn oneharness_judge_decides_over_the_transcript() {
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings());
    let skill = skill_with("[[reply:the change was committed and pushed]]");
    let outcome = engine
        .run(&Conversation::single_turn(skill, "commit"))
        .unwrap();
    let hit = engine
        .judge_boolean("committed", &outcome.transcript)
        .unwrap();
    assert_eq!(hit.value, JudgeValue::Bool(true));
    let miss = engine
        .judge_boolean("rolled back", &outcome.transcript)
        .unwrap();
    assert_eq!(miss.value, JudgeValue::Bool(false));
}

#[test]
fn oneharness_threads_one_session_name() {
    // The engine always threads `<base>-skill` across turns (the uniform --session
    // handle); the fake echoes the received name back.
    let capable = fake_oneharness();
    let engine = Engine::new(&capable, settings().with_session_name("run-9"));
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with("[[echo-session]]"),
            "go",
        ))
        .unwrap();
    assert_eq!(outcome.transcript.messages[1].content, "run-9-skill");
    let telemetry = outcome.telemetry.expect("oneharness telemetry");
    assert_eq!(telemetry.agent.model_ms, Some(10));
    assert_eq!(telemetry.agent.tool_ms, Some(3));
    assert_eq!(telemetry.agent.time_to_first_token_ms, Some(2));
    assert_eq!(
        telemetry.agent.usage.as_ref().unwrap().output_tokens,
        Some(1)
    );
    assert_eq!(telemetry.agent.session_ids, ["native-run-9-skill"]);
    assert!(telemetry.judge.session_ids.is_empty());
    assert_eq!(telemetry.sessions.len(), 1);
    assert_eq!(telemetry.sessions[0].role, onejudge::TelemetryRole::Agent);
    assert_eq!(telemetry.sessions[0].turn_index, 1);
    assert!(telemetry.sessions[0].history_id.is_some());
}

#[test]
fn oneharness_retries_without_session_when_unsupported() {
    // The fake rejects the first `--session` call with oneharness's "does not
    // support --session" text; onejudge must retry the call once without --session
    // (re-inlining the transcript), so the run still succeeds. On the retry no name
    // is threaded, so `[[echo-session]]` reports "no-session".
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings().with_session_name("run-x"));
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with("[[reject-session]][[echo-session]]"),
            "go",
        ))
        .unwrap();
    assert_eq!(outcome.transcript.assistant_turns(), 1);
    assert_eq!(outcome.transcript.messages[1].content, "no-session");
}

// --- The streamed provider protocol (docs/streaming.md) --------------------

/// A streamed [`OneharnessProvider`]: the same double, driven with `--stream`, so
/// its stdout is the NDJSON protocol rather than one buffered report document.
fn streaming_oneharness() -> OneharnessProvider {
    fake_oneharness().with_streaming(true)
}

#[test]
fn streamed_events_reach_the_sink_before_the_turn_ends() {
    // The double publishes its event line, then blocks until the file this sink
    // creates exists — so the run can only finish if the event really arrived
    // *during* the turn. A build that buffered events instead would never release
    // the double, which fails loudly on its own timeout rather than hanging.
    let release = scratch_path("streamed-release.marker");
    let provider = streaming_oneharness();
    let engine = Engine::new(&provider, settings());
    let skill = skill_with(&format!(
        "[[reply:streamed reply]][[event:git commit -m fix]][[stream-wait:{}]]",
        release.display()
    ));
    let mut seen = Vec::new();
    let outcome = engine
        .run_streaming(
            &Conversation::single_turn(skill, "commit it"),
            &mut |event| {
                seen.push(event.event.summary());
                std::fs::write(&release, b"go").unwrap();
                ControlFlow::Continue(())
            },
        )
        .unwrap();

    assert_eq!(seen.len(), 1, "the event was delivered live");
    assert!(seen[0].contains("git commit -m fix"));
    // The terminal `result` line's report is parsed exactly as a bare one is: the
    // turn carries the same reply, events, and prompt-cache usage.
    assert_eq!(outcome.transcript.messages[1].content, "streamed reply");
    assert_eq!(
        outcome
            .transcript
            .count_tool_events(&ToolQuery::tool("bash")),
        1
    );
    assert_eq!(outcome.usage.unwrap().cache_read_tokens, Some(7));
    assert!(!outcome.stopped_early);
    let _ = std::fs::remove_file(&release);
}

#[test]
fn a_streamed_provider_still_threads_the_session_and_multi_turn_loop() {
    // Streaming does not change the loop's own contract: the caller-owned session
    // name is threaded across turns, and the buffered judge side still drives the
    // simulated user.
    let provider = streaming_oneharness();
    let engine = Engine::new(&provider, settings().with_session_name("streamed"));
    let outcome = engine
        .run(&Conversation::multi_turn(
            skill_with("[[echo-session]]"),
            "start",
            SimulatedUser::new("A patient tester.").max_turns(2),
        ))
        .unwrap();
    assert_eq!(outcome.transcript.assistant_turns(), 2);
    assert_eq!(outcome.transcript.messages[1].content, "streamed-skill");
}

#[test]
fn a_declared_streaming_provider_that_degrades_to_a_bare_report_still_runs() {
    // The one deliberate tolerance: a provider that declared streaming but wrote
    // the single document a non-streaming run writes is not a failure.
    let provider = streaming_oneharness();
    let engine = Engine::new(&provider, settings());
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with("[[reply:degraded but fine]][[stream-bare]]"),
            "go",
        ))
        .unwrap();
    assert_eq!(outcome.transcript.messages[1].content, "degraded but fine");
}

#[test]
fn a_malformed_stream_fails_loudly_with_a_named_protocol_error() {
    for (marker, needle) in [
        ("[[stream-garbage]]", "was not valid JSON"),
        ("[[stream-unknown]]", "unknown run stream envelope type"),
        ("[[stream-truncate]]", "ended without a terminal"),
    ] {
        let provider = streaming_oneharness();
        let engine = Engine::new(&provider, settings());
        let err = engine
            .run(&Conversation::single_turn(
                skill_with(&format!("[[reply:ok]][[event:ls]]{marker}")),
                "go",
            ))
            .unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol), "{marker}");
        assert!(err.to_string().contains(needle), "{marker}: {err}");
    }
}

#[test]
fn content_after_the_terminal_result_fails_across_the_real_boundary() {
    // The grammar is `event* result EOF`. A real subprocess that keeps writing
    // after its terminal line is rejected whatever it writes — the report is not
    // banked and the trailing line is not swallowed.
    for kind in ["unknown", "event", "result"] {
        let provider = streaming_oneharness();
        let engine = Engine::new(&provider, settings());
        let err = engine
            .run(&Conversation::single_turn(
                skill_with(&format!("[[reply:ok]][[stream-trailing:{kind}]]")),
                "go",
            ))
            .unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol), "{kind}");
        assert!(
            err.to_string()
                .contains("wrote a line after its terminal `result` line"),
            "{kind}: {err}"
        );
    }
}

#[test]
fn a_complete_stream_from_a_process_that_then_died_is_still_a_failure() {
    // The report arrived, but the harness process did not survive writing it. The
    // run fails on the exit status rather than quietly banking a turn from a
    // process that crashed.
    let provider = streaming_oneharness();
    let engine = Engine::new(&provider, settings());
    let err = engine
        .run(&Conversation::single_turn(
            skill_with("[[reply:ok]][[stream-then-fail]]"),
            "go",
        ))
        .unwrap_err();
    assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
    assert!(err.to_string().contains("oneharness exited with"), "{err}");
}

#[test]
fn a_buffered_provider_replays_events_and_honors_a_breaking_sink() {
    // Not every provider streams. A buffered one still satisfies the streaming
    // contract by replaying the finished turn's events, and a sink that breaks on
    // the first stops the replay and the run.
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings());
    let skill = skill_with("[[reply:done]][[event:git status]]");
    let mut seen = 0;
    let started = std::time::Instant::now();
    let outcome = engine
        .run_streaming(
            &Conversation::multi_turn(skill, "go", SimulatedUser::new("A tester.").max_turns(5)),
            &mut |_event| {
                seen += 1;
                ControlFlow::Break(())
            },
        )
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(seen, 1);
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "the child was waited out, not torn down: {elapsed:?}"
    );
    assert!(outcome.stopped_early);
    assert_eq!(outcome.transcript.assistant_turns(), 1);
}

#[test]
fn a_streamed_run_still_recovers_from_a_rejected_session() {
    // The child exits before writing a terminal line, so the stream violation is
    // what onejudge sees first — but the child's stderr rides along on that error,
    // which is how the graceful `--session` retry still recognizes the failure.
    let provider = streaming_oneharness();
    let engine = Engine::new(&provider, settings().with_session_name("streamed-x"));
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with("[[reject-session]][[echo-session]]"),
            "go",
        ))
        .unwrap();
    assert_eq!(outcome.transcript.messages[1].content, "no-session");
}

#[test]
fn a_breaking_sink_tears_down_a_streamed_turn() {
    // Cancellation, across the real process boundary: the sink stops on the first
    // event and the provider tears the child down mid-turn. The double is blocked
    // before its terminal line on a file that never appears and gives up on its own
    // only after 30s, so the elapsed bound below is what proves the child was
    // *terminated* rather than waited out — and the run still reports the events it
    // did see, stopped early.
    //
    // The bound proves termination, not just an early return: the turn cannot come
    // back until the stderr drain reaches EOF, and EOF on that pipe requires every
    // process holding its write end to be gone. A build that abandoned the child
    // instead of killing it would sit here for the double's full 30s.
    let provider = streaming_oneharness();
    let engine = Engine::new(&provider, settings());
    let never = scratch_path("streamed-never.marker");
    let skill = skill_with(&format!(
        "[[reply:unreachable]][[event:git status]][[stream-wait:{}]]",
        never.display()
    ));
    let mut seen = 0;
    let started = std::time::Instant::now();
    let outcome = engine
        .run_streaming(
            &Conversation::multi_turn(skill, "go", SimulatedUser::new("A tester.").max_turns(5)),
            &mut |_event| {
                seen += 1;
                ControlFlow::Break(())
            },
        )
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(seen, 1);
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "the child was waited out, not torn down: {elapsed:?}"
    );
    assert!(outcome.stopped_early);
    assert_eq!(
        outcome
            .transcript
            .count_tool_events(&ToolQuery::tool("bash")),
        1,
        "the delivered event is kept on the abandoned turn"
    );
}

#[test]
fn cancelling_a_streamed_turn_terminates_the_harness_oneharness_spawned() {
    // The descendant that matters is the harness, and onejudge can never signal it:
    // oneharness makes every harness its own process-group leader. So cancellation
    // has to be cooperative — close oneharness's stdout and let *it* terminate the
    // tree it owns. The double models both halves: it spawns a detached stand-in,
    // publishes its pid and a liveness port, and tears it down only when its own
    // stdout breaks. A build that killed oneharness outright never delivers that
    // signal, and the stand-in below outlives the turn.
    let handle = scratch_path("streamed-descendant.handle");
    let provider = streaming_oneharness();
    let engine = Engine::new(&provider, settings());
    let skill = skill_with(&format!(
        "[[reply:unreachable]][[stream-descendant:{}]]",
        handle.display()
    ));
    let mut live = None;
    let outcome = engine
        .run_streaming(&Conversation::single_turn(skill, "go"), &mut |_event| {
            // Recorded mid-turn, and asserted live here, so the check after the
            // run cannot pass against a stand-in that never started.
            let (pid, port) = descendant_handle(&handle);
            assert!(
                descendant_is_running(port),
                "the harness stand-in (pid {pid}) was not running during the turn"
            );
            live = Some((pid, port));
            ControlFlow::Break(())
        })
        .unwrap();
    assert!(outcome.stopped_early);

    let (pid, port) = live.expect("the sink saw an event");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while descendant_is_running(port) {
        assert!(
            std::time::Instant::now() < deadline,
            "the harness stand-in (pid {pid}) outlived the cancelled turn: onejudge \
             killed oneharness instead of closing the stream first, so oneharness \
             never terminated the process tree it owns"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // A stand-in that outlives its turn also outlives the test, and can be writing
    // its `.profraw` while `cargo llvm-cov` merges. It must not write into that set.
    assert_profile_is_detached(&handle);
    let _ = std::fs::remove_file(&handle);
}

#[cfg(unix)]
#[test]
fn cancelling_a_turn_terminates_a_harness_that_produces_no_output() {
    // The case a broken pipe cannot reach, and the one that used to orphan a live
    // harness. oneharness observes a closed stdout only on its *next* write, so a
    // harness that has gone silent never produces one: the run sat there, outlived
    // the teardown grace, and took the backstop SIGKILL — which, being uncatchable,
    // denied it the teardown of the tree it owns. The harness kept running and kept
    // billing after every cancel.
    //
    // What makes it terminable is signalling oneharness instead: since v0.6.9 its
    // `run` verb answers SIGTERM by cancelling, which its runner polls for on its
    // own slice — independent of whether the harness ever writes — and then reaps
    // the tree. The double models exactly that and nothing more: it emits one event
    // to cancel on, then never touches stdout again, and tears its stand-in down
    // only on SIGTERM.
    let handle = scratch_path("streamed-silent-descendant.handle");
    let provider = streaming_oneharness();
    let engine = Engine::new(&provider, settings());
    let skill = skill_with(&format!(
        "[[reply:unreachable]][[stream-silent-descendant:{}]]",
        handle.display()
    ));
    let mut live = None;
    let outcome = engine
        .run_streaming(&Conversation::single_turn(skill, "go"), &mut |_event| {
            // Asserted live mid-turn, so the check below cannot pass against a
            // stand-in that never started.
            let (pid, port) = descendant_handle(&handle);
            assert!(
                descendant_is_running(port),
                "the harness stand-in (pid {pid}) was not running during the turn"
            );
            live = Some((pid, port));
            ControlFlow::Break(())
        })
        .unwrap();
    assert!(outcome.stopped_early);

    let (pid, port) = live.expect("the sink saw an event");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while descendant_is_running(port) {
        assert!(
            std::time::Instant::now() < deadline,
            "the harness stand-in (pid {pid}) outlived the cancelled turn: a silent \
             harness never observes a closed pipe, so onejudge must signal oneharness \
             to make it tear down the process tree it owns"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_profile_is_detached(&handle);
    let _ = std::fs::remove_file(&handle);
}

// --- SplitProvider journeys (two DIFFERENT real-subprocess backends) --------

#[test]
fn split_runs_the_skill_on_one_backend_and_judges_on_another() {
    // The skill runs on the fake oneharness; the judge and simulated user run on
    // the echo CommandProvider. Both are real subprocesses, composed by
    // SplitProvider, so a run exercises the split dispatch end to end.
    let split = SplitProvider::new(fake_oneharness(), echo());
    let engine = Engine::new(&split, settings());
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with("[[reply:hello there]]"),
            "go",
        ))
        .unwrap();
    assert_eq!(outcome.transcript.messages[1].content, "hello there");

    // The judge side routes to the echo provider, which decides by substring.
    let hit = engine.judge_boolean("hello", &outcome.transcript).unwrap();
    assert_eq!(hit.value, JudgeValue::Bool(true));
    let miss = engine
        .judge_boolean("goodbye forever", &outcome.transcript)
        .unwrap();
    assert_eq!(miss.value, JudgeValue::Bool(false));
}

#[test]
fn split_drives_a_multi_turn_conversation_across_both_backends() {
    let split = SplitProvider::new(fake_oneharness(), echo());
    let engine = Engine::new(&split, settings().with_session_name("split-run"));
    let user = SimulatedUser::new("A patient tester.").max_turns(2);
    let outcome = engine
        .run(&Conversation::multi_turn(
            skill_with("[[reply:working]]"),
            "start",
            user,
        ))
        .unwrap();
    // Two skill turns (fake oneharness) with an echo simulated-user turn between.
    assert_eq!(outcome.transcript.assistant_turns(), 2);
    assert!(outcome
        .transcript
        .messages
        .iter()
        .any(|m| m.content.contains("what about the next step")));
}

// --- The versioned Report contract, assembled from a real run --------------

#[test]
fn outcome_bundles_into_a_versioned_report() {
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let outcome = engine
        .run(&Conversation::single_turn(skill_with("Be helpful."), "hi"))
        .unwrap();
    let verdict = engine.judge_boolean("echo", &outcome.transcript).unwrap();
    let report = outcome.into_report(vec![NamedVerdict::new("echo", JudgeKind::Boolean, verdict)]);
    assert_eq!(report.schema_version, SCHEMA_VERSION);
    assert_eq!(report.verdicts.len(), 1);
    assert_eq!(report.transcript.assistant_turns(), 1);
}

// --- Fallback chains, timeouts, and per-candidate attribution --------------
//
// These drive the shapes `run_mode = "fallback"` produces. They are the reason
// onejudge reads oneharness's report through oneharness's own types: every one of
// them is a report whose *first* result is not the turn.

/// The attribution the agent side recorded for its first turn.
fn agent_attribution(outcome: &onejudge::Outcome) -> onejudge::HarnessAttribution {
    outcome
        .telemetry
        .as_ref()
        .expect("telemetry")
        .attribution
        .iter()
        .find(|a| a.role == onejudge::TelemetryRole::Agent)
        .expect("the agent invocation recorded its candidates")
        .clone()
}

#[test]
fn a_fallback_chain_advances_past_a_quota_refusal_and_runs_the_next_candidate() {
    // oneharness falls through a candidate rejected before it did any work. Its
    // report leads with that refusal, so a reader that took `results[0]` would fail
    // the run with `quota` even though a later candidate answered fine.
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings());
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with("[[reply:answered anyway]][[fallback:codex|quota]]"),
            "go",
        ))
        .expect("the chain settled on a candidate that ran");

    assert_eq!(outcome.transcript.messages[1].content, "answered anyway");
    let attribution = agent_attribution(&outcome);
    assert_eq!(attribution.ran.as_deref(), Some("claude-code"));
    assert_eq!(attribution.fell_through.len(), 1);
    assert_eq!(attribution.fell_through[0].harness, "codex");
    assert_eq!(attribution.fell_through[0].reason, "quota");
    // Both attempts are attributable to their identity, with the refusal typed.
    assert_eq!(attribution.candidates.len(), 2);
    assert_eq!(attribution.candidates[0].harness_id, "codex");
    assert_eq!(
        attribution.candidates[0].failure_kind.as_deref(),
        Some("quota")
    );
    assert!(!attribution.candidates[0].ran);
    assert!(attribution.candidates[1].ran);
}

#[test]
fn a_fallback_chain_advances_past_an_auth_refusal_over_several_identities() {
    // Several accounts of the same harness are distinct candidates; the composed
    // id, not the base harness, is what identifies the one that ran.
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings());
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with(
                "[[reply:third time lucky]][[fallback:codex:personal|auth,codex:work|quota]]",
            ),
            "go",
        ))
        .expect("the chain settled on a candidate that ran");

    assert_eq!(outcome.transcript.messages[1].content, "third time lucky");
    let attribution = agent_attribution(&outcome);
    let reasons: Vec<_> = attribution
        .fell_through
        .iter()
        .map(|f| f.reason.as_str())
        .collect();
    assert_eq!(reasons, ["auth", "quota"]);
    let ids: Vec<_> = attribution
        .candidates
        .iter()
        .map(|c| c.harness_id.as_str())
        .collect();
    assert_eq!(ids, ["codex:personal", "codex:work", "claude-code"]);
    assert_eq!(
        attribution.candidates[0].variant.as_deref(),
        Some("personal")
    );
}

#[test]
fn a_fallback_chain_does_not_fall_through_a_task_failure() {
    // The chain stops at the first candidate that actually RAN, whatever came of
    // it. A real task failure there must reach the caller classified — routing
    // around it would mask a broken skill as a broken environment.
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings());
    let err = engine
        .run(&Conversation::single_turn(
            skill_with("[[fail:model_not_found]][[fallback:codex|quota]]"),
            "go",
        ))
        .unwrap_err();

    assert_eq!(err.kind(), Some(ProviderErrorKind::ModelNotFound));
    assert!(err.to_string().contains("model_not_found"), "{err}");
}

#[test]
fn an_exhausted_fallback_chain_is_one_classified_error_naming_every_candidate() {
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings());
    let err = engine
        .run(&Conversation::single_turn(
            skill_with("[[fallback-exhausted:codex|not-installed,claude-code|auth]]"),
            "go",
        ))
        .unwrap_err();

    // Classified by the last candidate tried — what a caller would retry against.
    assert_eq!(err.kind(), Some(ProviderErrorKind::Auth));
    let message = err.to_string();
    assert!(message.contains("codex [not-installed]"), "{message}");
    assert!(message.contains("claude-code [auth]"), "{message}");
}

#[test]
fn a_per_turn_timeout_is_classified_rather_than_banked_as_an_empty_turn() {
    // oneharness kills a harness that outran `--timeout` and reports `status:
    // timeout` with NO failure_kind. Reading only failure_kind made that a
    // successful turn with an empty assistant message, which the judge then scored.
    for (token, expected) in [
        ("timeout", ProviderErrorKind::Timeout),
        ("spawn-error", ProviderErrorKind::Spawn),
        ("skipped", ProviderErrorKind::Spawn),
    ] {
        let provider = fake_oneharness();
        let engine = Engine::new(&provider, settings());
        let err = engine
            .run(&Conversation::single_turn(
                skill_with(&format!("[[status:{token}]]")),
                "go",
            ))
            .unwrap_err();
        assert_eq!(err.kind(), Some(expected), "{token}");
        assert!(err.to_string().contains("did not run the turn"), "{token}");
    }
}

#[test]
fn the_per_candidate_history_record_is_read_back_through_oneharnesss_own_reader() {
    // oneharness writes one normalized history record per ATTEMPTED candidate, and
    // that record — not the run report — is where it keeps the invocation's
    // measurements. onejudge reads the session file back with oneharness's own
    // reader, so every attempt is attributable to an identity and a record id.
    let history = scratch_path("attribution-history.jsonl");
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings().with_session_name("attributed"));
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with(&format!(
                "[[reply:done]][[fallback:codex|quota]][[history:{}]]",
                history.display()
            )),
            "go",
        ))
        .expect("the chain settled on a candidate that ran");

    let attribution = agent_attribution(&outcome);
    assert_eq!(
        attribution.history_file.as_deref(),
        Some(history.to_str().unwrap())
    );
    let ids: Vec<_> = attribution
        .candidates
        .iter()
        .map(|c| c.history_id.clone().expect("every attempt has a record"))
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "each attempt has its own record");

    // The measurements come off the RESULT's own `ExecutionTelemetry`, which the
    // run report has carried since oneharness report schema `0.5`. The double
    // writes deliberately different numbers (999) into the history record, so a
    // build that re-read the file for measurements it already had reports those
    // instead — which is what these assertions catch.
    let telemetry = outcome.telemetry.expect("telemetry");
    assert_eq!(telemetry.agent.model_ms, Some(10));
    assert_eq!(telemetry.agent.tool_ms, Some(3));
    assert_eq!(telemetry.agent.time_to_first_token_ms, Some(2));
    assert_eq!(
        telemetry.sessions[0].started_at, "2026-01-01T00:00:00.000Z",
        "the invocation bounds come from the report, not the history record"
    );
    assert_eq!(telemetry.sessions.len(), 1);
    assert_eq!(
        telemetry.sessions[0].history_id.as_deref(),
        Some(ids[1].as_str()),
        "the session link points at the record for the candidate that RAN"
    );
}

#[test]
fn a_failed_invocation_is_still_attributed_to_the_identities_it_tried() {
    // Attribution matters most when the run failed, so a failure must not discard
    // it. The engine surfaces the error, and the provider still recorded which
    // candidates were attempted and why each was refused.
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings());
    let err = engine
        .run(&Conversation::single_turn(
            skill_with("[[fallback-exhausted:codex|quota,claude-code|auth]]"),
            "go",
        ))
        .unwrap_err();
    assert_eq!(err.kind(), Some(ProviderErrorKind::Auth));

    let recorded = onejudge::Provider::invocation_telemetry(&provider);
    assert_eq!(
        recorded.len(),
        1,
        "the failed invocation was still recorded"
    );
}

// --- The spawn seam: an embedder-owned group around what onejudge spawns ----
//
// Driving onejudge in-process removes the OS grouping the subprocess boundary used
// to supply: the harness processes are created by the embedder's own process,
// inside whatever group it happens to be in, so a cancel can no longer name a tree
// to terminate and an orphaned harness keeps calling the model. `SpawnHook` is the
// seam that gives the embedder the group back — onejudge offers each process, the
// embedder owns the group. See `docs/spawn-hook.md`.

/// A [`SpawnHook`] that records what onejudge offered it, and answers with the
/// group label the test asked for.
#[derive(Default)]
struct RecordingHook {
    seen: std::sync::Mutex<Vec<(onejudge::TelemetryRole, String, String)>>,
    label: Option<&'static str>,
    refuse: bool,
}

impl onejudge::SpawnHook for RecordingHook {
    fn spawned(
        &self,
        child: &std::process::Child,
        context: &onejudge::SpawnContext<'_>,
    ) -> std::io::Result<Option<String>> {
        assert!(child.id() > 0, "the hook is offered a live process");
        self.seen.lock().unwrap().push((
            context.role,
            context.op.to_string(),
            context.program.to_string(),
        ));
        if self.refuse {
            return Err(std::io::Error::other("no group could be created"));
        }
        Ok(self.label.map(String::from))
    }
}

/// The two-party conversation both hook journeys drive: one agent turn on the
/// fake oneharness, then one supervisor turn on the echo command provider.
fn two_party_conversation() -> Conversation {
    Conversation::multi_turn(
        skill_with("[[reply:acknowledged]]"),
        "go",
        SimulatedUser::new("A patient tester.").max_turns(2),
    )
}

#[test]
fn a_spawn_hook_is_offered_every_process_both_parties_spawn() {
    // One embedder-owned group has to span BOTH backends of a split run, so the
    // same hook is installed on each and every spawn — agent and judge, oneharness
    // and command provider — passes through it.
    let hook = std::sync::Arc::new(RecordingHook {
        label: Some("job:run-1"),
        ..RecordingHook::default()
    });
    let shared: onejudge::SharedSpawnHook = hook.clone();
    let provider = SplitProvider::new(
        fake_oneharness().with_spawn_hook(shared.clone()),
        echo().with_spawn_hook(shared),
    );
    let engine = Engine::new(&provider, settings());
    let outcome = engine.run(&two_party_conversation()).unwrap();

    let seen = hook.seen.lock().unwrap().clone();
    let agent: Vec<_> = seen
        .iter()
        .filter(|(role, ..)| *role == onejudge::TelemetryRole::Agent)
        .collect();
    let judge: Vec<_> = seen
        .iter()
        .filter(|(role, ..)| *role == onejudge::TelemetryRole::Judge)
        .collect();
    assert!(!agent.is_empty(), "the agent side's spawns were offered");
    assert!(!judge.is_empty(), "the judge side's spawns were offered");
    assert!(agent.iter().all(|(_, op, _)| op == "respond"));
    assert!(judge.iter().any(|(_, op, _)| op == "supervisor"));
    assert!(agent
        .iter()
        .all(|(_, _, program)| program.contains("fake-oneharness")));
    assert!(judge
        .iter()
        .all(|(_, _, program)| program.contains("echo-provider")));

    // Everything the hook was offered is what the run reports, with the group the
    // hook named — and it reaches the versioned wire contract, so an operator sees
    // the same thing from `onejudge run --format json`.
    assert_eq!(outcome.processes.len(), seen.len());
    assert!(outcome
        .processes
        .iter()
        .all(|p| p.group.as_deref() == Some("job:run-1") && p.pid > 0));
    let report = outcome.into_report(vec![]);
    assert_eq!(report.processes.len(), seen.len());
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("job:run-1"));
}

#[test]
fn without_a_hook_the_run_reports_its_processes_and_claims_no_group() {
    // The no-hook default is unchanged behaviour — and the report says so honestly
    // rather than naming a group that does not exist.
    let provider = SplitProvider::new(fake_oneharness(), echo());
    let engine = Engine::new(&provider, settings());
    let outcome = engine.run(&two_party_conversation()).unwrap();
    assert!(!outcome.processes.is_empty());
    assert!(outcome.processes.iter().all(|p| p.group.is_none()));
    let json = serde_json::to_string(&outcome.into_report(vec![])).unwrap();
    assert!(!json.contains("\"group\""));
}

#[test]
fn a_hook_that_cannot_group_a_process_fails_the_turn_instead_of_running_it() {
    // Running a harness the embedder cannot terminate is the defect the hook
    // exists to prevent, so a hook that fails is loud, not a silent fallback to
    // an ungrouped process.
    let hook = std::sync::Arc::new(RecordingHook {
        refuse: true,
        ..RecordingHook::default()
    });
    let provider = fake_oneharness().with_spawn_hook(hook.clone());
    let engine = Engine::new(&provider, settings());
    let err = engine
        .run(&Conversation::single_turn(skill_with("[[reply:x]]"), "go"))
        .unwrap_err();
    assert_eq!(err.kind(), Some(ProviderErrorKind::Spawn));
    assert!(err.to_string().contains("spawn hook"), "{err}");
    // The child it refused is never reported as a process of the run.
    assert!(onejudge::Provider::spawned_processes(&provider).is_empty());
    assert_eq!(hook.seen.lock().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn an_embedder_group_reaps_the_whole_two_party_harness_tree_on_a_kill_cancel() {
    // The defect this seam closes, driven exactly as an embedder hits it: onejudge
    // as a LIBRARY, a two-party run (worker plus judge), each party's harness
    // outliving the `oneharness` process that spawned it, and a cancel that must
    // reap the whole tree. Without a hook there is no group to name here — the
    // spawned processes sit in onejudge's own group, which is the test runner's, so
    // the only available `killpg` would take the test process with it. That is why
    // this cannot be written against a build without the seam.
    //
    // The agent's harness stand-in is orphaned deliberately (its `oneharness` has
    // already exited and been reaped) and the judge's `oneharness` is still in
    // flight, so the kill has to reach both a live child and a descendant whose
    // parent is gone.
    let agent_handle = scratch_path("grouped-agent.handle");
    let judge_handle = scratch_path("grouped-judge.handle");
    let never = scratch_path("grouped-judge.hold");

    let hook = std::sync::Arc::new(OwnedProcessGroups::default());
    let installed: onejudge::SharedSpawnHook = hook.clone();
    let skill = skill_with(&format!(
        "[[reply:acknowledged]][[orphan:{}]]",
        agent_handle.display()
    ));
    // The judge side is steered through the task, which the supervisor prompt
    // inlines: it leaks its own stand-in and then holds, so the run is still in
    // flight when the embedder cancels.
    let task = format!(
        "go [[orphan:{}]][[hold:{}]]",
        judge_handle.display(),
        never.display()
    );
    let worker = std::thread::spawn(move || {
        let provider = SplitProvider::new(
            fake_oneharness().with_spawn_hook(installed.clone()),
            fake_oneharness().with_spawn_hook(installed),
        );
        let engine = Engine::new(&provider, settings());
        engine
            .run(&Conversation::multi_turn(
                skill,
                task,
                SimulatedUser::new("A patient tester.").max_turns(2),
            ))
            .map(|_| ())
    });

    await_path(&agent_handle, "the agent's harness stand-in never started");
    await_path(&judge_handle, "the judge's harness stand-in never started");
    let (agent_pid, agent_port) = descendant_handle(&agent_handle);
    let (judge_pid, judge_port) = descendant_handle(&judge_handle);
    assert!(
        descendant_is_running(agent_port) && descendant_is_running(judge_port),
        "both harness stand-ins were running when the run was cancelled"
    );
    // Orphaned stand-ins outlive the test either way, so neither may write into
    // the profile set `cargo llvm-cov` merges.
    assert_profile_is_detached(&agent_handle);
    assert_profile_is_detached(&judge_handle);

    // Cancel with kill semantics: terminate every group the embedder was handed.
    // Nothing else is signalled — the harness stand-ins are reached only because
    // they inherited a group the hook created.
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
             cancelled run: the process it descends from was not in a group the \
             embedder could terminate"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // Every process onejudge itself spawned is gone too. These are asked about by
    // pid because the run thread waited on each, so a survivor would be a live
    // process, not an unreaped zombie — which is why the *descendants* above are
    // asked from outside instead: once their parent exits they are reparented, and
    // a terminated descendant can still be nameable until its new parent reaps it.
    for pgid in &groups {
        assert!(
            !process_exists(*pgid),
            "the process onejudge spawned as group {pgid} survived the kill"
        );
    }

    for path in [&agent_handle, &judge_handle] {
        let _ = std::fs::remove_file(path);
    }
}

// --- Out-of-band turn control (docs/control.md) ----------------------------

/// Be `oneharness interrupt`, using oneharness's **own** code rather than a
/// re-implementation of it: resolve the store the address names, read the session
/// record the pre-flight refusal is decided from, and send one request frame to the
/// socket that store's directory holds. This is exactly what `commands::interrupt`
/// does with the same three argv values, so a test that passes here is a test that
/// the reported address is one the real verb can resolve.
#[cfg(unix)]
fn interrupt_at(
    address: &onejudge::ControlAddress,
    input: &str,
) -> (
    oneharness_core::domain::session::SessionRecord,
    oneharness_core::domain::control::ControlResponse,
) {
    use oneharness_core::domain::control::{socket_path, ControlRequest, RedirectInput};
    use oneharness_core::io::{control, session as session_io};

    let dir = session_io::resolve_dir(Some(&address.session_dir))
        .expect("`--session-dir` resolves to a store");
    let record = session_io::read(&session_io::session_path(
        &dir,
        std::path::Path::new(&address.cwd),
        &address.session,
    ))
    .expect("`oneharness interrupt` finds a session record at the reported address");
    // The pre-flight refusal: a harness with no control mechanism is answered from
    // the store, before the socket is dialled. Passing it means the address names
    // an identity that can be interrupted at all.
    assert!(
        record.harness.spec().control.is_some(),
        "the reported address names `{}`, which declares no turn control, so an \
         interrupt would be refused `unsupported` without ever dialling",
        record.harness
    );
    let request =
        ControlRequest::redirect(RedirectInput::new(input).expect("a usable redirection"));
    let response = control::send(&socket_path(&dir, &address.session), &request);
    (record, response)
}

/// Read `path` until it holds `needle`, or fail loudly. The frames are written by
/// another process, so a bounded wait is the difference between an assertion and a
/// race.
#[cfg(unix)]
fn await_contents(path: &std::path::Path, needle: &str) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains(needle) {
            return text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the live turn never received `{needle}`; it got: {text}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn the_reported_control_address_is_one_oneharness_interrupt_can_redirect_the_turn_through() {
    let store = support::control_store("ctl-open");
    let sink = scratch_path("control-sink-open");
    let worktree = "/skills/demo";
    let instructions = format!(
        "[[reply:on it]][[control-store:{}]][[control-linger:{}]]",
        store.display(),
        sink.display()
    );

    let provider = fake_oneharness().with_control(true);
    let engine = Engine::new(&provider, settings().with_session_name("Run 42"));
    let outcome = engine
        .run(&Conversation::single_turn(
            Skill::new("demo", worktree, &instructions),
            "start the long job",
        ))
        .expect("a controlled turn is an ordinary turn that also opened a socket");
    let report = outcome.into_report(vec![]);

    let address = report
        .control
        .clone()
        .expect("a controlled run reports where its turn is addressed");
    assert!(report.control_unavailable.is_none());
    // The session is the handle oneharness *stored*, not the one onejudge passed:
    // `Run 42-skill` is sanitized to `run-42-skill` before it keys the store, and an
    // address carrying the unsanitized name would resolve to nothing.
    assert_eq!(address.session, "run-42-skill");
    assert_eq!(address.cwd, worktree);
    assert_eq!(
        std::path::Path::new(&address.session_dir),
        store.canonicalize().unwrap(),
        "the store directory comes from the socket the run really opened"
    );

    // Now be the supervisor: stop the turn and say what to do instead.
    let (record, response) = interrupt_at(&address, "stop — fix the failing test first");
    assert_eq!(record.harness.to_string(), "claude-code");
    assert!(
        response.is_ok(),
        "the reported address did not reach a live turn: {response:?}"
    );

    // And the correction really landed *in the turn*: the abort frame and then the
    // operator's message, on the same stdin the harness is reading.
    let delivered = await_contents(&sink, "stop — fix the failing test first");
    assert!(
        delivered.contains("control_request"),
        "the interrupt frame never reached the turn: {delivered}"
    );

    // The lingering server and the turn it holds both outlive the run by design,
    // so both are spawned out of the profile set `cargo llvm-cov` merges.
    assert_profile_is_detached(&oneharness_core::domain::control::socket_path(
        std::path::Path::new(&address.session_dir),
        &address.session,
    ));
    assert_profile_is_detached(&sink);

    let _ = std::fs::remove_file(&sink);
    let _ = std::fs::remove_dir_all(&store);
}

#[cfg(unix)]
#[test]
fn a_fallback_chain_reports_the_session_of_the_candidate_that_ran() {
    // The chain routes around `qwen` — which declares no turn control — and a later
    // candidate does the work. An address bound to the first candidate tried would
    // make `oneharness interrupt` refuse `unsupported` from the store, before it
    // ever dialled; `interrupt_at` asserts that pre-flight passes.
    let store = support::control_store("ctl-chain");
    let sink = scratch_path("control-sink-fallback");
    let instructions = format!(
        "[[reply:on it]][[fallback:qwen|quota]][[control-store:{}]][[control-linger:{}]]",
        store.display(),
        sink.display()
    );

    let provider = fake_oneharness().with_control(true);
    let engine = Engine::new(&provider, settings().with_session_name("chain"));
    let outcome = engine
        .run(&Conversation::single_turn(
            Skill::new("demo", "/skills/demo", &instructions),
            "start the long job",
        ))
        .expect("a chain that routed around a candidate still ran the turn");
    let report = outcome.into_report(vec![]);
    let attribution = &report.telemetry.as_ref().unwrap().attribution[0];
    assert_eq!(
        attribution.fell_through[0].harness, "qwen",
        "the chain really did route around its first candidate"
    );

    let address = report
        .control
        .clone()
        .expect("the chain reports an address");
    let (record, response) = interrupt_at(&address, "different plan: revert instead");
    assert_eq!(
        record.harness.to_string(),
        "claude-code",
        "the address names the identity that ran, not the one the chain skipped"
    );
    assert!(response.is_ok(), "{response:?}");
    await_contents(&sink, "different plan: revert instead");

    let _ = std::fs::remove_file(&sink);
    let _ = std::fs::remove_dir_all(&store);
}

#[cfg(unix)]
#[test]
fn a_control_ask_a_harness_cannot_honor_degrades_instead_of_failing_the_run() {
    // oneharness refuses `--control` for a harness with no control mechanism, and
    // it refuses before spawning anything — so the turn is worth retrying without
    // the flag rather than losing to it.
    let store = support::control_store("ctl-refused");
    let instructions = format!(
        "[[reply:done anyway]][[control-unsupported:qwen]][[control-store:{}]]",
        store.display()
    );

    let provider = fake_oneharness().with_control(true);
    let engine = Engine::new(&provider, settings());
    let outcome = engine
        .run(&Conversation::single_turn(
            Skill::new("demo", "/skills/demo", &instructions),
            "go",
        ))
        .expect("a refused control ask must not fail the run it rode on");
    assert_eq!(outcome.transcript.messages[1].content, "done anyway");

    let report = outcome.into_report(vec![]);
    assert!(report.control.is_none());
    let reason = report
        .control_unavailable
        .expect("an asked-for lever that does not exist has to say so");
    assert!(
        reason.contains("has no out-of-band turn control"),
        "the reason should quote oneharness's own refusal, got: {reason}"
    );
    let _ = std::fs::remove_dir_all(&store);
}

#[cfg(not(unix))]
#[test]
fn a_platform_with_no_unix_socket_degrades_before_the_call() {
    // The Windows half of the same contract: onejudge answers the ask itself
    // rather than spending a process on a refusal oneharness would have to make,
    // and the run goes ahead. The reason is what keeps that distinguishable from
    // a caller that never asked.
    let provider = fake_oneharness().with_control(true);
    let engine = Engine::new(&provider, settings());
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with("[[reply:done anyway]]"),
            "go",
        ))
        .expect("a platform with no socket must not fail the run");
    let report = outcome.into_report(vec![]);
    assert!(report.control.is_none());
    assert!(report
        .control_unavailable
        .expect("the platform refusal is stated")
        .contains("unix domain socket"));
}

#[cfg(unix)]
#[test]
fn a_harness_that_cannot_bind_a_session_leaves_the_control_ask_unaddressable() {
    // `--control` is addressed by the `--session` name, so a harness that exposes
    // no session id headlessly has nowhere for the socket to live. Unix-only
    // because a platform with no socket never gets as far as asking — see
    // `a_platform_with_no_unix_socket_degrades_before_the_call`. Both are
    // dropped and the turn is re-inlined — the run still completes, and the
    // reason says which of the two degradations took the lever away.
    let provider = fake_oneharness().with_control(true);
    let engine = Engine::new(&provider, settings());
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with("[[reply:no session here]][[reject-session]]"),
            "go",
        ))
        .expect("a sessionless harness still runs the turn");
    assert_eq!(outcome.transcript.messages[1].content, "no session here");

    let report = outcome.into_report(vec![]);
    assert!(report.control.is_none());
    let reason = report
        .control_unavailable
        .expect("a lever with no address has to say why");
    assert!(
        reason.contains("--session"),
        "the reason should name the session the address needs, got: {reason}"
    );
}

#[test]
fn no_control_ask_reports_neither_an_address_nor_a_reason() {
    // The default. `control: null` with no reason beside it is how a supervisor
    // tells "this run was never asked to be controllable" from "it was, and could
    // not be" — the two states the field would otherwise collapse.
    let provider = fake_oneharness();
    let engine = Engine::new(&provider, settings());
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with("[[reply:done]]"),
            "go",
        ))
        .unwrap();
    let report = outcome.into_report(vec![]);
    assert!(report.control.is_none());
    assert!(report.control_unavailable.is_none());
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("\"control\":null"));
}

// --- The in-process seam ---------------------------------------------------

/// A project whose oneharness config pins the harness to the built fake-harness
/// double — ordinary `[harness.<id>] bin` config, so this is the seam a real
/// deployment uses and not a test-only hook.
fn harness_project(name: &str) -> std::path::PathBuf {
    let dir = scratch_path(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
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
fn an_in_process_turn_runs_the_real_engine_over_a_deterministic_harness() {
    // The happy path of the seam a default provider uses: nothing is spawned by
    // onejudge, the engine is the linked `oneharness-core`, and the only faked
    // thing is the harness — one level deeper than the other doubles fake.
    let dir = harness_project("in-process-turn");
    let provider = OneharnessProvider::new();
    let engine = Engine::new(&provider, settings());
    let skill = Skill::new("demo", dir.to_str().unwrap(), "[[reply:in-process reply]]");
    let outcome = engine
        .run(&Conversation::single_turn(skill, "do it"))
        .unwrap();

    assert_eq!(outcome.transcript.messages[1].content, "in-process reply");
    // The report still attributes the turn to the candidate that ran, read off
    // oneharness's own report rather than a parsed stdout document.
    let telemetry = outcome.telemetry.as_ref().expect("the run is measured");
    let attempt = &telemetry.attribution[0].candidates[0];
    assert_eq!(attempt.harness, "claude-code");
    assert!(attempt.ran);
    // Nothing was spawned *by onejudge*, so it reports no processes — the seam's
    // one caller-visible consequence, and the reason `with_spawn_hook` selects
    // the spawning seam instead.
    assert!(outcome.processes.is_empty());
}

#[test]
fn an_in_process_turn_that_cannot_select_a_harness_fails_loudly() {
    // The recovery path: a config naming a harness whose binary is not there is
    // oneharness refusing before any model call, and it must reach the caller as
    // a classified provider error rather than a vacuously empty turn.
    let dir = scratch_path("in-process-missing-harness");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("oneharness.toml"),
        format!(
            "harnesses = [\"claude-code\"]\nrequire_available = true\nhistory_dir = {:?}\n\n\
             [harness.claude-code]\nbin = {:?}\n",
            dir.join("history").display().to_string(),
            dir.join("not-installed").display().to_string(),
        ),
    )
    .unwrap();
    let provider = OneharnessProvider::new();
    let engine = Engine::new(&provider, settings());
    let skill = Skill::new("demo", dir.to_str().unwrap(), "[[reply:never]]");
    let err = engine
        .run(&Conversation::single_turn(skill, "do it"))
        .expect_err("an unavailable harness is a failure, not an empty turn");
    let text = err.to_string();
    assert!(
        text.contains("claude-code"),
        "the failure names the candidate: {text}"
    );
}

#[test]
fn an_in_process_streamed_turn_delivers_events_while_it_is_still_running() {
    // The double publishes its event, then blocks until the file this sink
    // creates exists — so the run can only finish if the event really reached the
    // sink *during* the turn. A build that collected events off the finished
    // report instead would never release the double, which fails loudly on its
    // own timeout rather than hanging.
    let dir = harness_project("in-process-streaming");
    let release = dir.join("release.marker");
    let provider = OneharnessProvider::new().with_streaming(true);
    let engine = Engine::new(&provider, settings());
    let skill = Skill::new(
        "demo",
        dir.to_str().unwrap(),
        format!(
            "[[reply:streamed in process]][[event:git commit -m fix]][[stream-wait:{}]]",
            release.display()
        ),
    );
    let mut seen = Vec::new();
    let outcome = engine
        .run_streaming(
            &Conversation::single_turn(skill, "commit it"),
            &mut |event| {
                seen.push(event.event.summary());
                std::fs::write(&release, b"go").unwrap();
                ControlFlow::Continue(())
            },
        )
        .unwrap();

    assert_eq!(seen.len(), 1, "the event was delivered live");
    assert!(seen[0].contains("git commit -m fix"));
    // And the finished turn is the same one a buffered run would have produced.
    assert_eq!(
        outcome.transcript.messages[1].content,
        "streamed in process"
    );
    // `Bash`, capitalised, because oneharness reports the harness's own tool
    // name verbatim and that is what a claude-code transcript carries.
    assert_eq!(
        outcome
            .transcript
            .count_tool_events(&ToolQuery::tool("Bash")),
        1
    );
    assert!(!outcome.stopped_early);
}

#[test]
fn cancelling_an_in_process_turn_terminates_the_harness_tree_oneharness_owns() {
    // The failure this seam exists to not have: a cancelled turn that leaves a
    // paid harness running. The double spawns a descendant in its own tree and
    // then goes SILENT forever — the case a closed pipe can never reach, since a
    // producer that never writes never observes one. Only `RunControls::cancel`,
    // which oneharness's runner polls on its own time slice, reaps it.
    //
    // `signal_cancel` stays false throughout: onejudge is an embedder, so it must
    // never install process-global handlers in its host. Nothing here signals
    // anything — the sink's break is the whole cancellation.
    let dir = harness_project("in-process-cancel");
    let handle = dir.join("descendant.handle");
    let provider = OneharnessProvider::new().with_streaming(true);
    let engine = Engine::new(&provider, settings());
    let skill = Skill::new(
        "demo",
        dir.to_str().unwrap(),
        format!(
            "[[reply:never arrives]][[descendant:{}]][[event:sleep 600]]",
            handle.display()
        ),
    );
    let outcome = engine
        .run_streaming(
            &Conversation::single_turn(skill, "start it"),
            // Break on the first event: the turn is abandoned from here.
            &mut |_| ControlFlow::Break(()),
        )
        .unwrap();
    assert!(outcome.stopped_early, "the sink short-circuited the run");

    await_path(&handle, "the harness stand-in never published its handle");
    let (pid, port) = descendant_handle(&handle);
    // Asked from OUTSIDE the tree, which is the only vantage point from which
    // "the harness oneharness spawned is really gone" can be asserted.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while descendant_is_running(port) {
        assert!(
            std::time::Instant::now() < deadline,
            "the harness stand-in (pid {pid}) outlived the cancelled turn and is still billing"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // The harness double spawns this one, a level deeper than the oneharness
    // double, and it is detached for the same reason — so it needs the same
    // redirect, out of the profile set `cargo llvm-cov` merges.
    assert_profile_is_detached(&handle);
}

#[test]
fn installing_a_spawn_hook_moves_a_default_provider_onto_the_seam_that_has_a_process() {
    // A hook offers a *process*, and an in-process turn has none — so installing
    // one must move the provider onto the spawning seam rather than silently
    // never firing, which would leave an embedder believing it owns a group that
    // is empty.
    //
    // Proven by a hook that REFUSES: the refusal can only reach the caller if a
    // process was really about to be spawned. That makes the assertion
    // independent of whether an `oneharness` happens to be on this host's PATH,
    // which a "the spawn failed" assertion would not be.
    struct Refuses;

    impl onejudge::SpawnHook for Refuses {
        fn spawning(
            &self,
            _: &mut std::process::Command,
            context: &onejudge::SpawnContext<'_>,
        ) -> std::io::Result<()> {
            Err(std::io::Error::other(format!(
                "refused `{}`",
                context.program
            )))
        }
    }

    let dir = harness_project("hook-selects-the-spawning-seam");
    let skill = Skill::new("demo", dir.to_str().unwrap(), "[[reply:never runs]]");

    // Without a hook the same provider runs the turn in process, and succeeds.
    let bare = OneharnessProvider::new();
    Engine::new(&bare, settings())
        .run(&Conversation::single_turn(skill.clone(), "do it"))
        .expect("a default provider runs in process");

    // With one, the hook is consulted — which it can only be for a process.
    let hooked = OneharnessProvider::new().with_spawn_hook(std::sync::Arc::new(Refuses));
    let err = Engine::new(&hooked, settings())
        .run(&Conversation::single_turn(skill, "do it"))
        .expect_err("the hook refused the process, so the turn fails");
    let text = err.to_string();
    assert!(
        text.contains("refused `oneharness`"),
        "the hook was offered the `oneharness` process it exists to place: {text}"
    );
    assert_eq!(err.kind(), Some(ProviderErrorKind::Spawn));
}

// --- Observing journeys ----------------------------------------------------

/// One observation copied out of the borrowed sink, so a journey can assert on
/// the whole sequence once the run has finished.
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Opened {
        turn: usize,
        role: Role,
        instruction: String,
        started_at: String,
    },
    Tool {
        turn: usize,
        name: Option<String>,
        tool_call_id: Option<String>,
    },
    Said {
        turn: usize,
        role: Role,
        text: String,
    },
    Closed {
        turn: usize,
        role: Role,
        usage: Option<Usage>,
        started_at: String,
        finished_at: String,
    },
}

/// Copy one borrowed [`Observation`] into an owned record.
fn observed(observation: &Observation<'_>) -> Seen {
    match observation {
        Observation::TurnOpened(opened) => Seen::Opened {
            turn: opened.turn,
            role: opened.role,
            instruction: opened.instruction.to_string(),
            started_at: opened.started_at.clone(),
        },
        Observation::Tool(event) => Seen::Tool {
            turn: event.turn,
            name: event.event.name.clone(),
            tool_call_id: event.event.tool_call_id.clone(),
        },
        Observation::Message(message) => Seen::Said {
            turn: message.turn,
            role: message.role,
            text: message.text.to_string(),
        },
        Observation::TurnClosed(closed) => Seen::Closed {
            turn: closed.turn,
            role: closed.role,
            usage: closed.usage.cloned(),
            started_at: closed.started_at.clone(),
            finished_at: closed.finished_at.clone(),
        },
    }
}

/// Every tool observation in `seen`, in order — the sub-sequence the streaming
/// sink is promised.
fn tools(seen: &[Seen]) -> Vec<Seen> {
    seen.iter()
        .filter(|s| matches!(s, Seen::Tool { .. }))
        .cloned()
        .collect()
}

/// Assert `text` is the spelling every observation timestamp promises: RFC 3339
/// with millisecond precision, in UTC.
///
/// Validity is decided by oneharness's own `UtcInstant`, which refuses anything
/// that is not an RFC 3339 *UTC* instant, so the two layers cannot disagree about
/// what the field means; the fraction is then checked here, because that parser
/// accepts (and truncates) any sub-second precision.
fn assert_utc_millis(text: &str) {
    text.parse::<oneharness_core::domain::usage::UtcInstant>()
        .unwrap_or_else(|e| panic!("`{text}` is not an RFC 3339 UTC instant: {e}"));
    let (seconds, fraction) = text
        .split_once('.')
        .unwrap_or_else(|| panic!("`{text}` carries no sub-second fraction"));
    assert_eq!(seconds.len(), 19, "`{text}` is not `YYYY-MM-DDThh:mm:ss`");
    assert!(
        fraction.len() == 4
            && fraction.ends_with('Z')
            && fraction[..3].chars().all(|c| c.is_ascii_digit()),
        "`{text}` is not millisecond precision in UTC"
    );
}

#[test]
fn an_observing_turn_reports_its_instruction_reply_identity_and_bounds() {
    // The seam a default provider uses: the real `oneharness-core` engine over the
    // deterministic harness double. The call identity an observation carries is
    // therefore the one the *harness* published in its own stream-json and
    // oneharness normalized — never a value this test handed the engine.
    let dir = harness_project("observing-turn");
    let provider = OneharnessProvider::new().with_streaming(true);
    let engine = Engine::new(&provider, settings());
    let skill = Skill::new(
        "demo",
        dir.to_str().unwrap(),
        "[[reply:committed the fix]][[event:git commit -m fix]]",
    );
    let mut seen = Vec::new();
    let outcome = engine
        .run_observing(
            &Conversation::single_turn(skill, "commit the fix"),
            &mut |observation| {
                seen.push(observed(observation));
                ControlFlow::Continue(())
            },
        )
        .unwrap();
    assert!(!outcome.stopped_early);

    assert_eq!(seen.len(), 4, "{seen:#?}");
    let Seen::Opened {
        turn,
        role,
        instruction,
        started_at,
    } = &seen[0]
    else {
        panic!("the turn opens first: {seen:#?}")
    };
    assert_eq!((*turn, *role), (1, Role::Assistant));
    // The conversation's opening input is never a message observation — it is in
    // the transcript before the loop runs — so this is where an observer reads it.
    assert_eq!(instruction, "commit the fix");
    assert_utc_millis(started_at);

    assert_eq!(
        seen[1],
        Seen::Tool {
            turn: 1,
            // Capitalised: oneharness reports the harness's own tool name verbatim.
            name: Some("Bash".into()),
            // Published by the harness as the `tool_use` block's `id`.
            tool_call_id: Some("t0".into()),
        },
        "the call identity the harness exposed reached the observer: {seen:#?}"
    );

    assert_eq!(
        seen[2],
        Seen::Said {
            turn: 1,
            role: Role::Assistant,
            text: "committed the fix".into(),
        }
    );

    let Seen::Closed {
        turn,
        role,
        usage,
        started_at: opened_at,
        finished_at,
    } = &seen[3]
    else {
        panic!("the turn closes last: {seen:#?}")
    };
    assert_eq!((*turn, *role), (1, Role::Assistant));
    // This harness reports no accounting at all, so the turn is observed as having
    // reported none — never as a zero-filled `Usage`, which would claim the turn
    // was free.
    assert_eq!(*usage, None);
    assert_eq!(opened_at, started_at, "the turn's own opening instant");
    assert_utc_millis(finished_at);
    assert!(finished_at >= opened_at, "{opened_at} .. {finished_at}");
}

#[test]
fn an_observing_multi_turn_run_reports_both_parties_without_disturbing_the_streaming_seam() {
    // The echo double publishes no call identity, which is the other half of the
    // contract: absence is reported as absence, not as an empty string.
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let conversation = || {
        Conversation::multi_turn(
            skill_with("Commit it. [[event:git commit -m fix]]"),
            "start",
            SimulatedUser::new("A patient tester.").max_turns(2),
        )
    };

    let mut seen = Vec::new();
    engine
        .run_observing(&conversation(), &mut |observation| {
            seen.push(observed(observation));
            ControlFlow::Continue(())
        })
        .unwrap();

    // Two assistant turns with a supervisor turn between them, each opened, spoken
    // and closed in order.
    let shape: Vec<(&str, usize, Option<Role>)> = seen
        .iter()
        .map(|s| match s {
            Seen::Opened { turn, role, .. } => ("opened", *turn, Some(*role)),
            Seen::Tool { turn, .. } => ("tool", *turn, None),
            Seen::Said { turn, role, .. } => ("said", *turn, Some(*role)),
            Seen::Closed { turn, role, .. } => ("closed", *turn, Some(*role)),
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            ("opened", 1, Some(Role::Assistant)),
            ("tool", 1, None),
            ("said", 1, Some(Role::Assistant)),
            ("closed", 1, Some(Role::Assistant)),
            ("opened", 1, Some(Role::User)),
            ("said", 1, Some(Role::User)),
            ("closed", 1, Some(Role::User)),
            ("opened", 2, Some(Role::Assistant)),
            ("tool", 2, None),
            ("said", 2, Some(Role::Assistant)),
            ("closed", 2, Some(Role::Assistant)),
        ],
        "{seen:#?}"
    );

    // The supervisor answers the agent's reply, and its own words become the next
    // assistant turn's instruction — the chain an operator reads the dispatch by.
    let Seen::Said { text: reply, .. } = &seen[2] else {
        unreachable!()
    };
    let Seen::Opened {
        instruction: answering,
        ..
    } = &seen[4]
    else {
        unreachable!()
    };
    assert_eq!(answering, reply);
    let Seen::Said {
        text: instruction, ..
    } = &seen[5]
    else {
        unreachable!()
    };
    let Seen::Opened {
        instruction: next, ..
    } = &seen[7]
    else {
        unreachable!()
    };
    assert_eq!(next, instruction);

    // This provider does report accounting, so the turn's own cost is observed.
    let Seen::Closed { usage, .. } = &seen[3] else {
        unreachable!()
    };
    assert!(
        usage.as_ref().and_then(|u| u.output_tokens).is_some(),
        "the turn's own usage: {usage:?}"
    );

    // This harness exposed no identity, and that is what the observation says.
    assert_eq!(
        tools(&seen),
        vec![
            Seen::Tool {
                turn: 1,
                name: Some("bash".into()),
                tool_call_id: None
            },
            Seen::Tool {
                turn: 2,
                name: Some("bash".into()),
                tool_call_id: None
            },
        ]
    );

    // The narrower seam is untouched: the same conversation through
    // `run_streaming` delivers exactly those tool events, in that order.
    let mut streamed = Vec::new();
    engine
        .run_streaming(&conversation(), &mut |event| {
            streamed.push(Seen::Tool {
                turn: event.turn,
                name: event.event.name.clone(),
                tool_call_id: event.event.tool_call_id.clone(),
            });
            ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(streamed, tools(&seen));
}

#[test]
fn breaking_an_observation_stops_the_run_and_delivers_nothing_after_it() {
    let provider = echo();
    let engine = Engine::new(&provider, settings());
    let conversation = || {
        Conversation::multi_turn(
            skill_with("Commit it. [[event:git commit -m fix]]"),
            "start",
            SimulatedUser::new("A patient tester.").max_turns(5),
        )
    };

    // Breaking on the very first observation stops the run before the provider is
    // ever asked, so the transcript holds nothing but the task.
    let mut seen = Vec::new();
    let outcome = engine
        .run_observing(&conversation(), &mut |observation| {
            seen.push(observed(observation));
            ControlFlow::Break(())
        })
        .unwrap();
    assert!(outcome.stopped_early);
    assert_eq!(seen.len(), 1);
    assert!(matches!(seen[0], Seen::Opened { .. }));
    assert_eq!(outcome.transcript.assistant_turns(), 0);

    // Breaking mid-conversation stops it there, and nothing is delivered after the
    // observation that asked to stop.
    let mut seen = Vec::new();
    let outcome = engine
        .run_observing(&conversation(), &mut |observation| {
            let record = observed(observation);
            let stop = matches!(
                record,
                Seen::Said {
                    role: Role::Assistant,
                    ..
                }
            );
            seen.push(record);
            if stop {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .unwrap();
    assert!(outcome.stopped_early);
    assert_eq!(seen.len(), 3, "{seen:#?}");
    assert!(matches!(seen[2], Seen::Said { .. }));
    assert_eq!(outcome.transcript.assistant_turns(), 1);
}
