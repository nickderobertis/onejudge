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
    CommandProvider, Conversation, Engine, JudgeKind, JudgeValue, NamedVerdict, OneharnessProvider,
    ProviderErrorKind, Settings, SimulatedUser, Skill, SplitProvider, ToolQuery, SCHEMA_VERSION,
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
fn malformed_unified_supervisor_response_is_a_protocol_error() {
    let err = Engine::new(&echo(), settings())
        .run(&Conversation::multi_turn(
            skill_with("Be helpful."),
            "start",
            SimulatedUser::new("[[malformed-supervisor]]").max_turns(2),
        ))
        .unwrap_err();
    assert!(matches!(
        err,
        onejudge::Error::Provider {
            kind: Some(ProviderErrorKind::Protocol),
            ..
        }
    ));
    assert!(err
        .to_string()
        .contains("continue response requires non-empty `message`"));
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

/// A unique path under the integration-test tmp dir, removed if it survived an
/// earlier run.
fn scratch_path(name: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_file(&path);
    path
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

    // The measurements come from the record, which the run report never carries.
    let telemetry = outcome.telemetry.expect("telemetry");
    assert_eq!(telemetry.agent.model_ms, Some(10));
    assert_eq!(telemetry.agent.tool_ms, Some(3));
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
