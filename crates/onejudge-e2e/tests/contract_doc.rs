//! The contract doc's Rust example, compiled and run for real: the one fenced
//! `rust` block in `docs/contract.md` is held byte for byte to
//! `contract_doc/build_report.rs`, which this suite includes as code and runs over
//! the echo double — so the example a consumer copies is the code that builds a
//! [`Report`](onejudge::Report) here.
//!
//! An e2e journey rather than part of the library's contract suite because it
//! spawns a double: the library crate's own tests never reach the crate that
//! holds them.

use onejudge::{CommandProvider, Conversation, Engine, Settings, Skill, SCHEMA_VERSION};
use onejudge_test_doubles as doubles;

include!("contract_doc/build_report.rs");

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
fn the_contract_doc_example_is_the_code_that_builds_a_report() {
    assert_eq!(
        fenced("rust"),
        include_str!("contract_doc/build_report.rs"),
        "docs/contract.md's example differs from tests/contract_doc/build_report.rs"
    );
    let provider = CommandProvider::new(vec![doubles::echo_provider().into()]).unwrap();
    let engine = Engine::new(&provider, Settings::new());
    let conversation = Conversation::single_turn(
        Skill::new("demo", "/skills/demo", ""),
        "the change was committed",
    );
    let report = build_report(&engine, &conversation).unwrap();
    assert_eq!(report.schema_version, SCHEMA_VERSION);
    assert_eq!(
        report.verdicts[0].verdict.value,
        onejudge::JudgeValue::Bool(true),
        "the echo judge finds the criterion in its own reply"
    );
}
