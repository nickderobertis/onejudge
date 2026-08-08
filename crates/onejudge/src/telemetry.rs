//! Typed timing, usage, and native-session linkage for one run.

use serde::{Deserialize, Serialize};

use crate::Usage;

/// The side of the evaluated conversation that owns an invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum TelemetryRole {
    /// The skill/agent under evaluation.
    Agent,
    /// The simulated user, supervisor, or final evaluator.
    Judge,
}

/// Aggregated measurements for one side of a run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
pub struct PartyTelemetry {
    /// Model/provider elapsed time, when every contributing invocation reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ms: Option<u64>,
    /// Tool elapsed time, when every contributing invocation reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_ms: Option<u64>,
    /// Time to first token from the first invocation that reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_first_token_ms: Option<u64>,
    /// Strictly aggregated usage; each field is present only when every invocation knew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Native oneharness session IDs in first-seen order, without duplicates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_ids: Vec<String>,
}

/// Authoritative linkage from a onejudge role/turn to a native oneharness run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
pub struct SessionLink {
    /// Native harness continuation identifier.
    pub session_id: String,
    /// Which side of the conversation owns the invocation.
    pub role: TelemetryRole,
    /// One-based invocation index within the role.
    pub turn_index: u32,
    /// Upstream UTC invocation-start timestamp.
    pub started_at: String,
    /// Upstream UTC invocation-finish timestamp, or `null` when not observed.
    pub finished_at: Option<String>,
    /// Native oneharness history record identity, when history was available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_id: Option<String>,
}

/// One candidate identity oneharness attempted for a single invocation — the
/// harness (and variant/model) it tried, and what came of it.
///
/// Under `run_mode = "fallback"` oneharness attempts candidates in priority order
/// and stops at the first that runs the task, so an invocation can have several of
/// these: the ones it fell through, then the one that ran. This is the typed form
/// of oneharness's per-candidate record — the thing that lets a consumer attribute
/// a failure to a *side* (agent vs judge) and an *identity* (which harness, which
/// account) instead of grepping a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
pub struct CandidateAttempt {
    /// Canonical harness id (e.g. `claude-code`).
    pub harness: String,
    /// Base id, or `<base>:<variant>` when a named preset selected the identity —
    /// the selector that reproduces this candidate.
    pub harness_id: String,
    /// The named preset, when this candidate came from a composed harness id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// The model this candidate ran with, when one was requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// oneharness's status token: `ok`, `nonzero`, `timeout`, `spawn-error`,
    /// `skipped`, or `planned`.
    pub status: String,
    /// Whether oneharness found the harness binary.
    pub available: bool,
    /// Whether this is the candidate that actually ran the turn.
    pub ran: bool,
    /// oneharness's normalized failure reason (`auth`, `rate_limit`,
    /// `model_not_found`, `quota`, `tool_deferred`), when it classified one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    /// Where that reason was read (`stderr`, `stdout`, `config:env_from`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind_source: Option<String>,
    /// The harness process's exit code, when it ran to completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Wall-clock duration of this attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// oneharness's human-readable problem statement for a failed attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The harness's own continuation id, when it exposed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The oneharness history record written for this attempt, when history was
    /// on — the handle `oneharness history show` resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_id: Option<String>,
    /// This attempt's own token/cost accounting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// One candidate a fallback chain fell through, with oneharness's reason token
/// (`not-installed`, `spawn-error`, `auth`, `quota`, `model-not-found`,
/// `rate-limit`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
pub struct FellThrough {
    /// Canonical harness id.
    pub harness: String,
    /// Why it could not run the task at all.
    pub reason: String,
}

/// What oneharness attempted for one provider invocation, placed on the side and
/// turn that made the call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
pub struct HarnessAttribution {
    /// Which side of the conversation made this call.
    pub role: TelemetryRole,
    /// One-based invocation index within the role.
    pub turn_index: u32,
    /// The candidate that ran the turn, by composed id; `null` when none could.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran: Option<String>,
    /// Candidates a fallback chain routed around, in priority order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fell_through: Vec<FellThrough>,
    /// Every attempted candidate, in the order oneharness tried them.
    pub candidates: Vec<CandidateAttempt>,
    /// The oneharness history session file this invocation appended to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_file: Option<String>,
}

/// Timing, usage, and native linkage for the complete agent+judge run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
pub struct Telemetry {
    /// Monotonic duration from task-loop entry through final result assembly.
    pub wall_ms: u64,
    /// Agent-side measurements.
    pub agent: PartyTelemetry,
    /// Simulated-user, supervisor, and evaluator measurements.
    pub judge: PartyTelemetry,
    /// Non-negative wall-time remainder after measured model and tool work.
    pub orchestration_ms: u64,
    /// Native session linkage records in invocation order.
    #[serde(default)]
    pub sessions: Vec<SessionLink>,
    /// Which harness identities each invocation attempted, in invocation order.
    /// Empty when the provider reports no candidate identities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attribution: Vec<HarnessAttribution>,
}

/// One provider invocation before it is folded into public run telemetry.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct InvocationTelemetry {
    pub(crate) role: Option<TelemetryRole>,
    pub(crate) model_ms: Option<u64>,
    pub(crate) tool_ms: Option<u64>,
    pub(crate) time_to_first_token_ms: Option<u64>,
    pub(crate) usage: Usage,
    pub(crate) session_id: Option<String>,
    pub(crate) started_at: Option<String>,
    pub(crate) finished_at: Option<String>,
    pub(crate) history_id: Option<String>,
    /// The candidate that ran, by composed id; `None` when none could.
    pub(crate) ran: Option<String>,
    /// Candidates a fallback chain routed around, in priority order.
    pub(crate) fell_through: Vec<FellThrough>,
    /// Every attempted candidate, in the order the provider tried them.
    pub(crate) candidates: Vec<CandidateAttempt>,
    /// The provider's history session file for this invocation.
    pub(crate) history_file: Option<String>,
}

fn strict_sum_u64(
    records: &[&InvocationTelemetry],
    field: fn(&InvocationTelemetry) -> Option<u64>,
) -> Option<u64> {
    records.iter().map(|record| field(record)).sum()
}

fn summarize(records: &[&InvocationTelemetry]) -> PartyTelemetry {
    if records.is_empty() {
        return PartyTelemetry::default();
    }
    let usage = Usage {
        input_tokens: strict_sum_u64(records, |r| r.usage.input_tokens),
        output_tokens: strict_sum_u64(records, |r| r.usage.output_tokens),
        cache_read_tokens: strict_sum_u64(records, |r| r.usage.cache_read_tokens),
        cache_write_tokens: strict_sum_u64(records, |r| r.usage.cache_write_tokens),
        cost_usd: records.iter().map(|r| r.usage.cost_usd).sum(),
    };
    let mut session_ids = Vec::new();
    for id in records
        .iter()
        .filter_map(|record| record.session_id.as_ref())
    {
        if !session_ids.contains(id) {
            session_ids.push(id.clone());
        }
    }
    PartyTelemetry {
        model_ms: strict_sum_u64(records, |r| r.model_ms),
        tool_ms: strict_sum_u64(records, |r| r.tool_ms),
        time_to_first_token_ms: records.iter().find_map(|r| r.time_to_first_token_ms),
        usage: (!usage.is_empty()).then_some(usage),
        session_ids,
    }
}

pub(crate) fn aggregate(wall_ms: u64, records: &[InvocationTelemetry]) -> Option<Telemetry> {
    if records.is_empty() {
        return None;
    }
    let agent_records: Vec<_> = records
        .iter()
        .filter(|record| record.role == Some(TelemetryRole::Agent))
        .collect();
    let judge_records: Vec<_> = records
        .iter()
        .filter(|record| record.role == Some(TelemetryRole::Judge))
        .collect();
    let agent = summarize(&agent_records);
    let judge = summarize(&judge_records);
    let attributed = agent
        .model_ms
        .unwrap_or(0)
        .saturating_add(agent.tool_ms.unwrap_or(0))
        .saturating_add(judge.model_ms.unwrap_or(0))
        .saturating_add(judge.tool_ms.unwrap_or(0));
    // One turn counter per role, shared by the session links and the harness
    // attribution so a consumer can join the two by (role, turn_index).
    let mut agent_turn = 0;
    let mut judge_turn = 0;
    let mut sessions = Vec::new();
    let mut attribution = Vec::new();
    for record in records {
        let Some(role) = record.role else { continue };
        let turn_index = match role {
            TelemetryRole::Agent => {
                agent_turn += 1;
                agent_turn
            }
            TelemetryRole::Judge => {
                judge_turn += 1;
                judge_turn
            }
        };
        if let (Some(session_id), Some(started_at)) = (&record.session_id, &record.started_at) {
            sessions.push(SessionLink {
                session_id: session_id.clone(),
                role,
                turn_index,
                started_at: started_at.clone(),
                finished_at: record.finished_at.clone(),
                history_id: record.history_id.clone(),
            });
        }
        // A provider that names no candidate identity contributes no attribution;
        // an empty entry would claim knowledge the invocation never had.
        if !record.candidates.is_empty() {
            attribution.push(HarnessAttribution {
                role,
                turn_index,
                ran: record.ran.clone(),
                fell_through: record.fell_through.clone(),
                candidates: record.candidates.clone(),
                history_file: record.history_file.clone(),
            });
        }
    }
    Some(Telemetry {
        wall_ms,
        agent,
        judge,
        orchestration_ms: wall_ms.saturating_sub(attributed),
        sessions,
        attribution,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(role: TelemetryRole, session: &str) -> InvocationTelemetry {
        InvocationTelemetry {
            role: Some(role),
            model_ms: Some(7),
            tool_ms: Some(2),
            time_to_first_token_ms: Some(3),
            usage: Usage {
                input_tokens: Some(10),
                output_tokens: Some(1),
                cache_read_tokens: Some(4),
                cache_write_tokens: Some(2),
                cost_usd: Some(0.5),
            },
            session_id: Some(session.into()),
            started_at: Some("2026-01-01T00:00:00Z".into()),
            finished_at: Some("2026-01-01T00:00:00.009Z".into()),
            history_id: Some(format!("history-{session}")),
            ran: Some("claude-code".into()),
            fell_through: vec![FellThrough {
                harness: "codex".into(),
                reason: "quota".into(),
            }],
            candidates: vec![
                CandidateAttempt {
                    harness: "codex".into(),
                    harness_id: "codex".into(),
                    variant: None,
                    model: None,
                    status: "nonzero".into(),
                    available: true,
                    ran: false,
                    failure_kind: Some("quota".into()),
                    failure_kind_source: Some("stderr".into()),
                    exit_code: Some(1),
                    duration_ms: Some(4),
                    error: Some("out of credit".into()),
                    session_id: None,
                    history_id: Some("history-codex".into()),
                    usage: None,
                },
                CandidateAttempt {
                    harness: "claude-code".into(),
                    harness_id: "claude-code".into(),
                    variant: None,
                    model: None,
                    status: "ok".into(),
                    available: true,
                    ran: true,
                    failure_kind: None,
                    failure_kind_source: None,
                    exit_code: Some(0),
                    duration_ms: Some(9),
                    error: None,
                    session_id: Some(session.into()),
                    history_id: Some(format!("history-{session}")),
                    usage: None,
                },
            ],
            history_file: Some("/state/oneharness/history/s.jsonl".into()),
        }
    }

    #[test]
    fn aggregates_parties_strictly_and_links_native_sessions() {
        let mut second = record(TelemetryRole::Agent, "agent-1");
        second.model_ms = Some(5);
        second.time_to_first_token_ms = Some(1);
        second.usage.output_tokens = None;
        let judge = record(TelemetryRole::Judge, "judge-1");
        let telemetry = aggregate(
            30,
            &[record(TelemetryRole::Agent, "agent-1"), second, judge],
        )
        .expect("records produce telemetry");

        assert_eq!(telemetry.agent.model_ms, Some(12));
        assert_eq!(telemetry.agent.time_to_first_token_ms, Some(3));
        assert_eq!(telemetry.agent.session_ids, ["agent-1"]);
        let usage = telemetry.agent.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(20));
        assert_eq!(usage.output_tokens, None);
        assert_eq!(usage.cost_usd, Some(1.0));
        assert_eq!(telemetry.judge.session_ids, ["judge-1"]);
        assert_eq!(telemetry.sessions.len(), 3);
        assert_eq!(telemetry.sessions[1].turn_index, 2);
        assert_eq!(telemetry.sessions[2].role, TelemetryRole::Judge);
        assert_eq!(telemetry.orchestration_ms, 5);
    }

    #[test]
    fn absent_and_partial_upstream_measurements_stay_unknown() {
        assert!(aggregate(1, &[]).is_none());
        let mut unknown = record(TelemetryRole::Agent, "agent-2");
        unknown.model_ms = None;
        unknown.tool_ms = None;
        unknown.time_to_first_token_ms = None;
        unknown.usage = Usage::default();
        unknown.started_at = None;
        let telemetry = aggregate(4, &[unknown]).unwrap();
        assert_eq!(telemetry.agent.model_ms, None);
        assert_eq!(telemetry.agent.tool_ms, None);
        assert_eq!(telemetry.agent.time_to_first_token_ms, None);
        assert_eq!(telemetry.agent.usage, None);
        assert!(telemetry.sessions.is_empty());
        assert_eq!(telemetry.orchestration_ms, 4);
    }
}
