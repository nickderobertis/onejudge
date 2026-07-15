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
//! Harness/model selection comes from oneharness's config, not onejudge. This test
//! resolves the committed workspace-root `oneharness.toml` to an absolute path for
//! both the agent and judge sides; `ONEHARNESS_HARNESSES` / `ONEHARNESS_MODEL` can
//! override its selection.

use std::path::Path;

use onejudge::{Conversation, Engine, OneharnessProvider, Settings, Skill};

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("onejudge package should be nested under the workspace root")
}

#[test]
#[ignore = "live: needs a real oneharness + authenticated harness; run via `just test-live`"]
fn live_single_turn_and_boolean_judge() {
    let workspace_root = workspace_root();
    let oneharness_config = workspace_root.join("oneharness.toml");
    assert!(workspace_root.is_absolute());
    assert!(oneharness_config.is_absolute());
    assert!(oneharness_config.is_file());

    let provider = OneharnessProvider::new().with_judge_config(oneharness_config);
    let settings = Settings::new();
    let engine = Engine::new(&provider, settings);
    let skill = Skill::new(
        "echoer",
        workspace_root.to_string_lossy(),
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
