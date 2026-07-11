//! The live-API tier: drive a **real** model API — Anthropic Messages or OpenAI
//! Chat Completions — through [`ApiJudgeProvider`] and the bundled
//! [`UreqTransport`](onejudge::UreqTransport), proving the harness-free judge path
//! against the genuine external service (not the in-process fake transport or the
//! local-socket double the deterministic tests use).
//!
//! Like the `oneharness` live tier (`tests/live.rs`), it is deliberately OUT of
//! the deterministic gate: non-deterministic, credentialed, and networked. Every
//! test is `#[ignore]`-d and it only compiles under the `ureq-transport` feature
//! (which carries the TLS stack), so it never runs in `just check`. Invoke it with
//! `just test-live-api` or the credential-gated `live-api` workflow.
//!
//! Configure with env vars:
//! - `ANTHROPIC_API_KEY` **or** `OPENAI_API_KEY` — picks the vendor (Anthropic
//!   wins if both are set). With neither, the tests skip with a note rather than
//!   fail, so an un-credentialed `--run-ignored all` is a no-op here.
//! - `ONEJUDGE_LIVE_API_MODEL` — the model id (defaults to a small model per
//!   vendor).
#![cfg(feature = "ureq-transport")]

use onejudge::{ApiJudgeProvider, Conversation, Engine, Settings, Skill, UreqTransport};

/// Build a provider + model from whichever credential is present, or `None` to
/// skip when the tier is un-credentialed.
fn provider_from_env() -> Option<(ApiJudgeProvider<UreqTransport>, String)> {
    let model = std::env::var("ONEJUDGE_LIVE_API_MODEL").ok();
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        let model = model.unwrap_or_else(|| "claude-3-5-haiku-latest".to_string());
        return Some((ApiJudgeProvider::anthropic(key), model));
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        let model = model.unwrap_or_else(|| "gpt-4o-mini".to_string());
        return Some((ApiJudgeProvider::openai(key), model));
    }
    None
}

#[test]
#[ignore = "live-api: needs ANTHROPIC_API_KEY or OPENAI_API_KEY; run via `just test-live-api`"]
fn live_api_single_turn_and_boolean_judge() {
    let Some((provider, model)) = provider_from_env() else {
        eprintln!("live-api: no ANTHROPIC_API_KEY / OPENAI_API_KEY set — skipping");
        return;
    };
    let settings = Settings::new("api", model.clone(), model);
    let engine = Engine::new(&provider, settings);
    let skill = Skill::new(
        "echoer",
        ".",
        "You are a terse assistant. Reply with exactly the word: pong.",
    );
    let outcome = engine
        .run(&Conversation::single_turn(skill, "ping"))
        .expect("live-api run should succeed with a valid key");
    assert_eq!(outcome.transcript.assistant_turns(), 1);

    let verdict = engine
        .judge_boolean(
            "the assistant replied with the word pong",
            &outcome.transcript,
        )
        .expect("live-api judge should return a verdict");
    // Don't assert the model's exact behavior — only that the pipeline produced a
    // typed verdict from a real API end to end.
    let _ = verdict.value;
}
