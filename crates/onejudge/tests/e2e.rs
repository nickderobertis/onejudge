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
    CommandProvider, Conversation, Engine, JudgeValue, OneharnessProvider, ProviderErrorKind,
    Settings, SimulatedUser, Skill, ToolQuery,
};

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
    Settings::new("claude-code", "test-model", "judge-model")
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
        onejudge::Role::User
    );
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
    assert!(outcome.usage.is_some());
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
fn oneharness_threads_one_session_name_when_capable() {
    // On a session-capable platform the engine threads `<base>-skill` across turns
    // (the uniform --session handle); the fake echoes the received name back.
    let capable = fake_oneharness();
    let engine = Engine::new(&capable, settings().with_session_name("run-9"));
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with("[[echo-session]]"),
            "go",
        ))
        .unwrap();
    assert_eq!(outcome.transcript.messages[1].content, "run-9-skill");
}

#[test]
fn oneharness_omits_session_on_non_capable_platform() {
    let provider = fake_oneharness();
    // goose is not session-capable, so no name is threaded and the fallback path
    // (inlined transcript, no --session) runs.
    let engine = Engine::new(&provider, Settings::new("goose", "m", "judge-model"));
    let outcome = engine
        .run(&Conversation::single_turn(
            skill_with("[[echo-session]]"),
            "go",
        ))
        .unwrap();
    assert_eq!(outcome.transcript.messages[1].content, "no-session");
}
