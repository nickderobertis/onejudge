//! Drift gate for onejudge's versioned [`Report`] contract. It builds a canonical
//! report through the public API, serializes it, and diffs the result against a
//! checked-in example and generated JSON Schema goldens. Any change to the
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
    let mut report = Report::new(
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
    .with_assessment("No follow-up work remains.");
    report.completion_reason = Some("the commit completed the task".into());
    report
}

const EXAMPLE_GOLDEN: &str = include_str!("golden/report.example-v4.json");
#[cfg(feature = "sdk-schema")]
const SCHEMA_GOLDEN: &str = include_str!("golden/report.schema-v4.json");

#[test]
fn report_matches_the_golden_example_v4() {
    assert_eq!(SCHEMA_VERSION, 4, "golden is for schema v4");
    let actual = serde_json::to_string_pretty(&canonical_report()).unwrap();
    assert_eq!(
        actual.trim(),
        EXAMPLE_GOLDEN.trim(),
        "the Report wire form changed. If this is intentional, bump SCHEMA_VERSION \
         and update the v4 contract goldens. Actual serialization:\n{actual}"
    );
}

#[test]
fn golden_deserializes_back_to_the_canonical_report() {
    let back: Report = serde_json::from_str(EXAMPLE_GOLDEN).unwrap();
    assert_eq!(back, canonical_report());
}

#[cfg(feature = "sdk-schema")]
#[test]
fn generated_report_schema_matches_the_schema_v4_golden() {
    let actual = serde_json::to_value(onejudge::sdk_schema::bundle().report).unwrap();
    let golden: serde_json::Value = serde_json::from_str(SCHEMA_GOLDEN).unwrap();
    assert_eq!(
        actual, golden,
        "the generated Report schema changed. If the wire contract changed, bump \
         SCHEMA_VERSION and update the versioned schema golden"
    );
}
