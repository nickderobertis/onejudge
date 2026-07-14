//! onejudge's own versioned result contract: the [`Report`] that bundles a
//! [`Transcript`], the [`NamedVerdict`]s scored against it, an optional free-text
//! assessment, and aggregated [`Usage`] into one serializable object with an
//! explicit [`SCHEMA_VERSION`].
//!
//! This is the wire form higher-level frameworks (e.g. `skilltest`) compose over
//! and re-export, so onejudge — not its consumers — owns the shape of a judged
//! run. The shape is drift-gated: `tests/contract.rs` pins the serialized JSON
//! against a checked-in golden, so any change to the wire form is a deliberate
//! edit that bumps [`SCHEMA_VERSION`], never a silent break for downstream SDKs.

use serde::{Deserialize, Serialize};

use crate::provider::{JudgeKind, JudgeVerdict};
use crate::transcript::Transcript;
use crate::usage::Usage;

/// The version of the [`Report`] wire contract. Bump on any change to the
/// serialized shape of a report or the types it embeds. `1` was the initial
/// contract; `2` added prompt-cache token fields to embedded [`Usage`], and `3`
/// added the optional free-text `assessment`.
pub const SCHEMA_VERSION: u32 = 3;

/// A judge verdict paired with the criterion it scored and the kind of
/// judgement, so a serialized report is self-describing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedVerdict {
    /// The plain-English criterion that was scored.
    pub criterion: String,
    /// Whether it was a boolean or numeric judgement.
    pub kind: JudgeKind,
    /// The verdict itself (value, reason, and per-call usage).
    pub verdict: JudgeVerdict,
}

impl NamedVerdict {
    /// Pair `verdict` with the `criterion` and `kind` it came from.
    pub fn new(criterion: impl Into<String>, kind: JudgeKind, verdict: JudgeVerdict) -> Self {
        Self {
            criterion: criterion.into(),
            kind,
            verdict,
        }
    }
}

/// A judged run: the transcript, the verdicts scored against it, aggregated
/// usage, and whether a streaming sink short-circuited the run — stamped with the
/// [`SCHEMA_VERSION`] of the contract that produced it.
///
/// Build one from an [`Outcome`](crate::Outcome) with
/// [`Outcome::into_report`](crate::Outcome::into_report), or directly with
/// [`Report::new`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// The contract version this report was serialized under.
    pub schema_version: u32,
    /// The full conversation transcript, with tool events on assistant turns.
    pub transcript: Transcript,
    /// The verdicts scored against the transcript, in the order they were added.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verdicts: Vec<NamedVerdict>,
    /// A free-text judgement requested by the caller, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<String>,
    /// Aggregated usage across every provider call (`None` if nothing reported).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Whether a streaming sink asked to short-circuit the run.
    #[serde(default)]
    pub stopped_early: bool,
}

impl Report {
    /// Assemble a report, stamping it with the current [`SCHEMA_VERSION`].
    #[must_use]
    pub fn new(
        transcript: Transcript,
        verdicts: Vec<NamedVerdict>,
        usage: Option<Usage>,
        stopped_early: bool,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            transcript,
            verdicts,
            assessment: None,
            usage,
            stopped_early,
        }
    }

    /// Attach a caller-requested free-text assessment.
    #[must_use]
    pub fn with_assessment(mut self, assessment: impl Into<String>) -> Self {
        self.assessment = Some(assessment.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::JudgeValue;
    use crate::transcript::{Message, ToolEvent};
    use serde_json::json;

    fn sample_report() -> Report {
        let mut transcript = Transcript::from_input("commit the fix");
        transcript.push(
            Message::assistant("Committed.").with_events(vec![ToolEvent {
                kind: "tool_call".into(),
                name: Some("bash".into()),
                input: Some(json!({"command": "git commit -m fix"})),
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
        .with_assessment("No follow-up work remains.")
    }

    #[test]
    fn report_stamps_the_schema_version() {
        assert_eq!(sample_report().schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn report_round_trips_through_serde() {
        let report = sample_report();
        let json = serde_json::to_string(&report).unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }

    #[test]
    fn empty_verdicts_and_usage_are_omitted() {
        let report = Report::new(Transcript::from_input("hi"), vec![], None, false);
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("verdicts"));
        assert!(!json.contains("usage"));
        assert!(!json.contains("assessment"));
        assert!(json.contains("\"schema_version\":3"));
    }

    #[test]
    fn assessment_round_trips_when_present() {
        let report = Report::new(Transcript::from_input("hi"), vec![], None, false)
            .with_assessment("Follow up on docs.");
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("Follow up on docs."));
        assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), report);
    }
}
