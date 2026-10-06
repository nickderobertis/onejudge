//! The contract doc's Rust example, run for real: `docs/contract.md` shows a
//! consumer building a [`Report`](onejudge::Report) through the public API, and
//! this makes the same calls over the echo double, so the example cannot drift
//! from an API that no longer compiles or no longer produces a report.
//!
//! An e2e journey rather than part of the library's contract suite because it
//! spawns a double: the library crate's own tests never reach the crate that
//! holds them.

use onejudge::{
    CommandProvider, Conversation, Engine, JudgeKind, NamedVerdict, Settings, Skill, SCHEMA_VERSION,
};
use onejudge_test_doubles as doubles;

/// The body of the one fenced block in `docs/contract.md` opened with `info`.
fn fenced(info: &str) -> &'static str {
    let doc = include_str!("../../../docs/contract.md");
    let opening = format!("```{info}\n");
    let mut blocks = doc.match_indices(&opening);
    let (start, _) = blocks
        .next()
        .unwrap_or_else(|| panic!("docs/contract.md has no ```{info} block"));
    assert!(
        blocks.next().is_none(),
        "docs/contract.md has more than one ```{info} block"
    );
    let body = &doc[start + opening.len()..];
    &body[..body.find("```").expect("the block is closed")]
}

#[test]
fn the_contract_doc_builds_a_report_the_way_the_api_does() {
    let example = fenced("rust");
    for call in [
        "engine.run(&conversation)?",
        "engine.judge_boolean(\"the change was committed\", &outcome.transcript)?",
        "outcome.into_report(",
        "onejudge::NamedVerdict::new(\"the change was committed\", onejudge::JudgeKind::Boolean, verdict)",
        "assert_eq!(report.schema_version, onejudge::SCHEMA_VERSION);",
    ] {
        assert!(example.contains(call), "the example no longer makes `{call}`");
    }
    // The same calls, over the echo double.
    let provider = CommandProvider::new(vec![doubles::echo_provider().into()]).unwrap();
    let engine = Engine::new(&provider, Settings::new());
    let conversation = Conversation::single_turn(
        Skill::new("demo", "/skills/demo", ""),
        "the change was committed",
    );
    let outcome = engine.run(&conversation).unwrap();
    let verdict = engine
        .judge_boolean("the change was committed", &outcome.transcript)
        .unwrap();
    let report = outcome.into_report(vec![NamedVerdict::new(
        "the change was committed",
        JudgeKind::Boolean,
        verdict,
    )]);
    assert_eq!(report.schema_version, SCHEMA_VERSION);
}
