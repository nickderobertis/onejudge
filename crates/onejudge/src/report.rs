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

use crate::control::{ControlAddress, ControlOutcome};
use crate::provider::{JudgeKind, JudgeVerdict};
use crate::spawn::SpawnedProcess;
use crate::telemetry::Telemetry;
use crate::transcript::Transcript;
use crate::usage::Usage;

/// The version of the [`Report`] wire contract. Bump on any change to the
/// serialized shape of a report or the types it embeds. `1` was the initial
/// contract; `2` added prompt-cache token fields to embedded [`Usage`], and `3`
/// added the optional free-text `assessment`; `4` added `completion_reason`; `5`
/// added optional two-party telemetry and native session linkage; `6` added
/// per-invocation harness attribution (`telemetry.attribution`) — which candidate
/// identities the provider attempted, which one ran, and which it fell through;
/// `7` added `processes` — the processes the run spawned and the embedder-owned
/// group a [`SpawnHook`](crate::SpawnHook) placed each in; `8` added `control` —
/// the address of the out-of-band turn-control channel a `provider.control: true`
/// run opened — and its companion `control_unavailable`; `9` added
/// `settled_reason` — why a run ended without a completion decision because its
/// supervisor gave no next instruction.
pub const SCHEMA_VERSION: u32 = 9;

/// The skip predicate for a field that is always serialized but must not be
/// *required* of a document being read. Used by [`Report::control`]; see the
/// reasoning there.
fn never<T>(_: &T) -> bool {
    false
}

/// A judge verdict paired with the criterion it scored and the kind of
/// judgement, so a serialized report is self-describing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
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
    /// Why the per-turn supervisor declared the task complete, if it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_reason: Option<String>,
    /// Why the run ended *without* a completion decision, when it ended because the
    /// supervisor judged the work incomplete and then named no next instruction —
    /// even asked again.
    ///
    /// Absent on every other run, so its presence is the one signal that separates
    /// "the agent could not do it" from "the supervisor had nothing to say". The
    /// transcript, verdicts, and usage beside it are the work the run really did:
    /// this settles a run, it does not fail one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_reason: Option<String>,
    /// Aggregated usage across every provider call (`None` if nothing reported).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Timing, per-party usage, and native oneharness session linkage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<Telemetry>,
    /// Every process the run spawned, and the embedder-owned group a
    /// [`SpawnHook`](crate::SpawnHook) placed each one in.
    ///
    /// This is the machine-readable form of what an in-process embedder observes
    /// through the hook, so `onejudge run --format json` reports it too. A record
    /// whose `group` is absent is not grouped — onejudge never names a group it did
    /// not observe a hook create.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processes: Vec<SpawnedProcess>,
    /// Where an `oneharness interrupt` process addresses the controllable turn
    /// this run's agent side opened (`provider.control: true`), or `null` when
    /// control was not asked for.
    ///
    /// Serialized even when null — unlike the optional fields above — because a
    /// supervisor keys on its presence to decide whether it has a lever at all,
    /// and an absent key would make "no control" indistinguishable from a report
    /// written before this field existed. `null` with a `control_unavailable`
    /// reason beside it is the third case: asked for, and refused.
    ///
    /// Always *written*, but deliberately not *required* by the generated schema:
    /// a reader must keep accepting the reports older onejudge versions wrote, and
    /// producing more than the contract demands is what makes an additive field
    /// additive. [`never`] is how those two are said at once — it is the skip
    /// predicate that never skips, and its presence is what tells serde's schema
    /// derive the key may be absent from a document.
    #[serde(default, skip_serializing_if = "never")]
    pub control: Option<ControlAddress>,
    /// Why an *asked-for* control channel is missing, when one is. Absent unless
    /// `provider.control` was on and the run could not be given a socket — which
    /// is what keeps a refused ask from reading as an ask never made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_unavailable: Option<String>,
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
            completion_reason: None,
            settled_reason: None,
            usage,
            telemetry: None,
            processes: Vec::new(),
            control: None,
            control_unavailable: None,
            stopped_early,
        }
    }

    /// Attach a caller-requested free-text assessment.
    #[must_use]
    pub fn with_assessment(mut self, assessment: impl Into<String>) -> Self {
        self.assessment = Some(assessment.into());
        self
    }

    /// Record what the run's agent side could say about out-of-band turn control.
    ///
    /// One method rather than two settable fields, so the report cannot hold the
    /// contradiction of an address *and* a reason it has none.
    #[must_use]
    pub fn with_control(mut self, outcome: &ControlOutcome) -> Self {
        self.control = outcome.address().cloned();
        self.control_unavailable = outcome.unavailable_reason().map(String::from);
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
        assert!(json.contains("\"schema_version\":9"));
        // The run neither completed nor settled, so it claims neither.
        assert!(!json.contains("completion_reason"));
        assert!(!json.contains("settled_reason"));
        assert!(!json.contains("telemetry"));
        // A run that spawned nothing reports no processes, rather than an empty
        // claim about grouping.
        assert!(!json.contains("processes"));
        // `control` is the exception: it is always on the wire, so a supervisor
        // reads "no controllable turn" rather than "an older onejudge".
        assert!(json.contains("\"control\":null"));
        assert!(!json.contains("control_unavailable"));
    }

    #[test]
    fn a_settled_run_round_trips_its_reason() {
        // The v9 addition: a run the supervisor left without a next instruction
        // carries why on the wire, and an older reader that has never heard of the
        // key still reads everything else (the field is optional, and omitted when
        // there is nothing to say — see `empty_verdicts_and_usage_are_omitted`).
        let mut report = Report::new(Transcript::from_input("hi"), vec![], None, false);
        report.settled_reason = Some("the supervisor gave no next instruction".into());
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"settled_reason\":\"the supervisor gave no next instruction\""));
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
    }

    #[test]
    fn a_refused_control_ask_is_not_an_ask_never_made() {
        let base = Report::new(Transcript::from_input("hi"), vec![], None, false);

        let never = base.clone().with_control(&ControlOutcome::NotRequested);
        let never_json = serde_json::to_string(&never).unwrap();
        assert!(never_json.contains("\"control\":null"));
        assert!(!never_json.contains("control_unavailable"));

        let refused = base.clone().with_control(&ControlOutcome::Unavailable(
            "harness `codex` has no out-of-band turn control".into(),
        ));
        let refused_json = serde_json::to_string(&refused).unwrap();
        // Same null address, but the reason is what tells the two apart.
        assert!(refused_json.contains("\"control\":null"));
        assert!(refused_json.contains("has no out-of-band turn control"));
        assert_eq!(
            serde_json::from_str::<Report>(&refused_json).unwrap(),
            refused
        );

        let open = base.with_control(&ControlOutcome::Open(ControlAddress {
            session: "run-42-skill".into(),
            session_dir: "/state/oneharness/sessions".into(),
            cwd: "/work/repo".into(),
        }));
        let open_json = serde_json::to_string(&open).unwrap();
        assert!(open_json.contains("\"session\":\"run-42-skill\""));
        assert!(!open_json.contains("control_unavailable"));
        assert_eq!(serde_json::from_str::<Report>(&open_json).unwrap(), open);
    }

    #[test]
    fn spawned_processes_round_trip_and_report_only_an_observed_group() {
        let mut report = Report::new(Transcript::from_input("hi"), vec![], None, false);
        report.processes = vec![
            SpawnedProcess {
                role: crate::TelemetryRole::Agent,
                op: "respond".into(),
                program: "oneharness".into(),
                pid: 4242,
                group: Some("job:run-1".into()),
            },
            SpawnedProcess {
                role: crate::TelemetryRole::Judge,
                op: "judge".into(),
                program: "oneharness".into(),
                pid: 4243,
                group: None,
            },
        ];
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"pid\":4242"));
        assert!(json.contains("job:run-1"));
        // The ungrouped record omits `group` entirely rather than inventing one.
        assert_eq!(json.matches("\"group\"").count(), 1);
        assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), report);
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
