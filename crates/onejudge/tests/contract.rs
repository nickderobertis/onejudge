//! Drift gate for onejudge's versioned [`Report`] contract. It builds a canonical
//! report through the public API, serializes it, and diffs the result against a
//! checked-in golden (`tests/golden/report.schema-v2.json`). Any change to the
//! wire form — a renamed field, a new key, a changed default — fails here, so it
//! must be a deliberate edit that also bumps `SCHEMA_VERSION`, never a silent
//! break for the SDKs that compose over this contract.

use onejudge::{
    JudgeKind, JudgeValue, JudgeVerdict, Message, NamedVerdict, Report, ToolEvent, Transcript,
    Usage, SCHEMA_VERSION,
};

/// The canonical report the golden is generated from: one tool-using assistant
/// turn, one boolean verdict, and usage — exercising every embedded contract type.
fn canonical_report() -> Report {
    let mut transcript = Transcript::from_input("commit the fix");
    transcript.push(
        Message::assistant("Committed.").with_events(vec![ToolEvent {
            kind: "tool_call".into(),
            name: Some("bash".into()),
            input: Some(serde_json::json!({"command": "git commit -m fix"})),
            output: None,
            index: 0,
        }]),
    );
    Report::new(
        transcript,
        vec![NamedVerdict::new(
            "the change was committed",
            JudgeKind::Boolean,
            JudgeVerdict {
                value: JudgeValue::Bool(true),
                reason: "a git commit ran".into(),
                usage: None,
            },
        )],
        Some(Usage {
            input_tokens: Some(12),
            output_tokens: Some(3),
            cache_read_tokens: Some(9),
            cache_write_tokens: Some(4),
            cost_usd: None,
        }),
        false,
    )
}

const GOLDEN: &str = include_str!("golden/report.schema-v2.json");

#[test]
fn report_matches_the_golden_schema_v2() {
    assert_eq!(SCHEMA_VERSION, 2, "golden is for schema v2");
    let actual = serde_json::to_string_pretty(&canonical_report()).unwrap();
    assert_eq!(
        actual.trim(),
        GOLDEN.trim(),
        "the Report wire form changed. If this is intentional, bump SCHEMA_VERSION \
         and update tests/golden/report.schema-v2.json. Actual serialization:\n{actual}"
    );
}

#[test]
fn golden_deserializes_back_to_the_canonical_report() {
    let back: Report = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(back, canonical_report());
}
