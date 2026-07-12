//! The live tier: drive a **real** `oneharness` binary (and, through it, a real
//! harness + model) so the [`OneharnessProvider`] path is proven against the
//! genuine external service, not a double.
//!
//! This is the one place stubbing a third party is replaced by the real call. It
//! is deliberately kept OUT of the deterministic gate — it is non-deterministic,
//! needs `oneharness` installed and an authenticated harness, and makes network
//! calls — so every test is `#[ignore]`-d and runs only when invoked explicitly
//! (`just test-live`, or the credential-gated `live` CI workflow). It still
//! **compiles** in the normal build, so the live code can never rot; it just does
//! not execute in `just check`. See `docs/live-tier.md`.
//!
//! Harness/model selection comes from oneharness's config files, not onejudge: the
//! agent side uses the discovered `oneharness.toml` (or `ONEHARNESS_HARNESS` /
//! `ONEHARNESS_MODEL` env overrides that oneharness reads), and the judge side uses
//! `oneharness.judge.toml`. Scaffold them with `onejudge init` before running.

use onejudge::{Conversation, Engine, OneharnessProvider, Settings, Skill};

#[test]
#[ignore = "live: needs a real oneharness + authenticated harness; run via `just test-live`"]
fn live_single_turn_and_boolean_judge() {
    let provider = OneharnessProvider::new();
    let settings = Settings::new();
    let engine = Engine::new(&provider, settings);
    let skill = Skill::new(
        "echoer",
        ".",
        "You are a terse assistant. Reply with exactly the word: pong.",
    );
    let outcome = engine
        .run(&Conversation::single_turn(skill, "ping"))
        .expect("live run should succeed with a reachable harness");
    assert_eq!(outcome.transcript.assistant_turns(), 1);

    let verdict = engine
        .judge_boolean(
            "the assistant replied with the word pong",
            &outcome.transcript,
        )
        .expect("live judge should return a verdict");
    // Don't assert the model's exact behavior — only that the pipeline produced a
    // typed verdict from a real harness end to end.
    let _ = verdict.value;
}
