//! The provider boundary. `onejudge` never talks to a model directly; a
//! [`Provider`] runs the skill, plays the simulated user, and judges the
//! transcript.
//!
//! [`CommandProvider`](crate::CommandProvider) speaks a small JSON-lines protocol
//! (see `docs/protocol.md`) and backs both the deterministic test doubles and any
//! custom provider; [`OneharnessProvider`](crate::OneharnessProvider) shells out
//! to the `oneharness` CLI. The trait also lets the engine be unit-tested against
//! an in-memory fake.

use std::ops::ControlFlow;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::transcript::{Message, ToolEvent};
use crate::usage::Usage;

/// A borrowed view of the skill under test, as sent to the provider.
pub struct SkillRef<'a> {
    /// The skill's name.
    pub name: &'a str,
    /// The skill's working directory (an absolute or CWD-relative path).
    pub dir: &'a str,
    /// The skill instructions, delivered as a real system prompt.
    pub instructions: &'a str,
}

/// The kind of judgement requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JudgeKind {
    /// A yes/no verdict.
    Boolean,
    /// A score on a `[min, max]` scale.
    Numeric,
}

impl JudgeKind {
    /// The stable wire string (`boolean` / `numeric`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            JudgeKind::Boolean => "boolean",
            JudgeKind::Numeric => "numeric",
        }
    }
}

/// A judge query: the criterion, its kind, and (for numeric) the scale.
pub struct JudgeQuery<'a> {
    /// Whether a boolean or numeric verdict is wanted.
    pub kind: JudgeKind,
    /// The plain-English criterion the judge evaluates.
    pub criterion: &'a str,
    /// The inclusive `(min, max)` scale for a numeric query; `None` for boolean.
    pub scale: Option<(f64, f64)>,
}

/// The raw value a judge returns: a boolean or a number, matching the query kind.
/// Deserialized untagged from the provider's `value` field.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JudgeValue {
    /// A boolean verdict.
    Bool(bool),
    /// A numeric score.
    Number(f64),
}

/// An assistant/skill turn produced by the provider.
#[derive(Debug, Clone, Default)]
pub struct AssistantTurn {
    /// The skill's reply text.
    pub message: String,
    /// The skill signalled it considers the task complete.
    pub done: bool,
    /// Cost/token usage for this call, if the provider reported it.
    pub usage: Option<Usage>,
    /// Normalized tool events the skill took this turn (shell commands, file
    /// edits, tool uses). Empty when the harness exposed no tool transcript.
    /// Attached to the assistant message so consumers can analyze — and the judge
    /// can reason over — what the skill *did*.
    pub events: Vec<ToolEvent>,
}

/// A simulated-user turn produced by the provider.
#[derive(Debug, Clone, Default)]
pub struct UserTurn {
    /// The simulated user's next message.
    pub message: String,
    /// The simulated user chose to end the conversation.
    pub stop: bool,
    /// Cost/token usage for this call, if reported.
    pub usage: Option<Usage>,
}

/// Inputs for one per-turn supervisor decision.
pub struct SupervisorQuery<'a> {
    /// The original task, unchanged from the first user turn.
    pub task: &'a str,
    /// The simulated supervisor's persona.
    pub persona: &'a str,
    /// The completion criterion, when the caller supplied one.
    pub done_when: Option<&'a str>,
    /// Agent worktree used by oneharness and by the history command.
    pub worktree: &'a str,
    /// Agent-side oneharness history name.
    pub history_name: &'a str,
}

/// A unified supervisor decision after an ordinary, nonterminal agent turn.
#[derive(Debug, Clone, PartialEq)]
pub enum SupervisorOutcome {
    /// The task is complete; the reason is retained on the engine outcome/report.
    Completed {
        /// Concise completion justification.
        reason: String,
    },
    /// Continue with this exact next user message.
    Continue {
        /// Exact message appended as the next user turn.
        message: String,
        /// Optional concise decision justification.
        reason: String,
    },
}

/// A supervisor decision and its provider usage.
#[derive(Debug, Clone, PartialEq)]
pub struct SupervisorTurn {
    /// The discriminated decision.
    pub outcome: SupervisorOutcome,
    /// Cost/token usage for this single call.
    pub usage: Option<Usage>,
}

/// A judge verdict: the raw value (bool or number) plus the stated reason. Part
/// of onejudge's versioned [`Report`](crate::Report) contract, so it round-trips
/// through serde.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeVerdict {
    /// The parsed verdict value.
    pub value: JudgeValue,
    /// The judge's one-sentence justification.
    #[serde(default)]
    pub reason: String,
    /// Cost/token usage for the judge call, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// Free-text output from an assessment judge call.
#[derive(Debug, Clone, PartialEq)]
pub struct Assessment {
    /// The judge's natural-language assessment.
    pub text: String,
    /// Cost/token usage for the assessment call, if reported.
    pub usage: Option<Usage>,
}

/// The provider boundary.
///
/// A provider owns harness/model *selection* itself (onejudge no longer passes a
/// platform or model): the [`OneharnessProvider`](crate::OneharnessProvider) relies
/// on oneharness's discovered config for the agent side and a separately-named
/// config for the judge side, and a [`CommandProvider`](crate::CommandProvider)
/// backend chooses however it likes.
pub trait Provider {
    /// Run one assistant/skill turn given the conversation so far.
    ///
    /// `session`, when `Some`, is a **caller-owned session name** the engine
    /// threads across the turns of one run. A provider that supports continuation
    /// maps it to the harness's native session so the skill keeps real state
    /// instead of being re-prompted with the whole transcript; a provider that
    /// cannot continue the session degrades gracefully by re-reading the inlined
    /// `messages` (the engine always threads the name — capability is the
    /// provider's concern, discovered at call time, not onejudge's up front).
    ///
    /// # Errors
    /// [`Error::Provider`](crate::Error::Provider) if the command fails or returns
    /// malformed output.
    fn respond(
        &self,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<AssistantTurn>;

    /// Like [`Provider::respond`], but delivers each normalized tool event to
    /// `on_event` as it is observed, so a caller can stream events live and
    /// short-circuit. `on_event` returns [`ControlFlow::Break`] to abort — the
    /// provider tears the turn down and returns what it has.
    ///
    /// The default implementation runs the buffered [`Provider::respond`] and
    /// replays the finished turn's events once; a provider that can genuinely
    /// stream overrides it so events arrive — and an abort takes effect — mid-turn.
    ///
    /// # Errors
    /// As [`Provider::respond`].
    fn respond_streaming(
        &self,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
        on_event: &mut dyn FnMut(&ToolEvent) -> ControlFlow<()>,
    ) -> Result<AssistantTurn> {
        let turn = self.respond(skill, messages, session)?;
        for event in &turn.events {
            if on_event(event).is_break() {
                break;
            }
        }
        Ok(turn)
    }

    /// Produce one simulated-user turn. `session` is the simulated user's own
    /// caller-owned session name (symmetric with [`Provider::respond`]), so it
    /// too can keep state across turns on a session-capable provider.
    ///
    /// # Errors
    /// [`Error::Provider`](crate::Error::Provider) if the command fails or returns
    /// malformed output.
    fn simulate_user(
        &self,
        persona: &str,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<UserTurn>;

    /// Decide completion and, when continuing, produce the exact next user turn
    /// in one judge-side invocation.
    fn supervise(
        &self,
        query: &SupervisorQuery<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<SupervisorTurn> {
        let criterion = query.done_when.unwrap_or("the original task is complete");
        let verdict = self.judge(
            &JudgeQuery {
                kind: JudgeKind::Boolean,
                criterion,
                scale: None,
            },
            messages,
        )?;
        if matches!(verdict.value, JudgeValue::Bool(true)) {
            return Ok(SupervisorTurn {
                outcome: SupervisorOutcome::Completed {
                    reason: verdict.reason,
                },
                usage: verdict.usage,
            });
        }
        let user = self.simulate_user(query.persona, messages, session)?;
        Ok(SupervisorTurn {
            outcome: SupervisorOutcome::Continue {
                message: user.message,
                reason: verdict.reason,
            },
            usage: user.usage,
        })
    }

    /// Score a criterion against the conversation.
    ///
    /// # Errors
    /// [`Error::Provider`](crate::Error::Provider) if the command fails or returns
    /// malformed output.
    fn judge(&self, query: &JudgeQuery<'_>, messages: &[Message]) -> Result<JudgeVerdict>;

    /// Write a free-text assessment of the finished conversation.
    ///
    /// # Errors
    /// [`Error::Provider`](crate::Error::Provider) if the command fails or returns
    /// malformed output.
    fn assess(&self, prompt: &str, messages: &[Message]) -> Result<Assessment>;
}

// ---------------------------------------------------------------------------
// Prompt building — shared by every provider that drives a real model.
// ---------------------------------------------------------------------------

/// Render the conversation as `Role: content` lines for inlining in a prompt.
///
/// When `include_events` is set, each assistant turn is followed by a compact,
/// token-budget-aware summary of the tool events it took — so the judge can
/// reason over *what the skill did* (the `git commit` it ran), not only what it
/// said. Tool output is summarized, never dumped. The simulated user and the
/// no-session `respond` fallback pass `false` (they only need the dialogue).
#[must_use]
pub fn render_transcript(messages: &[Message], include_events: bool) -> String {
    let mut out = String::new();
    for (i, m) in messages.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(m.role.label());
        out.push_str(": ");
        out.push_str(&m.content);
        if include_events && !m.events.is_empty() {
            for event in &m.events {
                out.push_str("\n  [tool] ");
                out.push_str(&event.summary());
            }
        }
    }
    out
}

/// The `respond` prompt for a provider that cannot continue a session: inline the
/// whole conversation so the stateless call sees it. The skill goes in separately
/// as a system prompt, so it does not appear here.
#[must_use]
pub fn build_respond_prompt(messages: &[Message]) -> String {
    format!(
        "Conversation so far (most recent last):\n{}\n\n\
         Write only the assistant's next reply, following your system \
         instructions. Output the reply text and nothing else.",
        render_transcript(messages, false),
    )
}

/// The prompt that role-plays the simulated user.
#[must_use]
pub fn build_user_prompt(persona: &str, messages: &[Message]) -> String {
    format!(
        "You are role-playing the USER in a conversation with an AI assistant. \
         Stay in character:\n\n{persona}\n\n\
         Conversation so far (most recent last):\n{transcript}\n\n\
         Write only the user's next message. Output the message text and nothing \
         else.",
        transcript = render_transcript(messages, false),
    )
}

/// Build the unified supervisor prompt. Only compact normalized event summaries
/// are inlined; full events remain available on demand through oneharness history.
#[must_use]
pub fn build_supervisor_prompt(query: &SupervisorQuery<'_>, messages: &[Message]) -> String {
    let criterion = query.done_when.unwrap_or(
        "No explicit completion criterion was supplied; continue unless the original task is clearly complete.",
    );
    format!(
        "You are the simulated USER and completion supervisor for an AI agent.\n\n\
         Original task:\n{task}\n\nSupervisor persona:\n{persona}\n\n\
         Completion criterion:\n{criterion}\n\n\
         Conversation transcript (tool actions are compact normalized summaries, never raw dumps):\n{transcript}\n\n\
         Judge-side oneharness runs inherit the agent worktree `{worktree}` and may use harness tools. \
         If these compact summaries are insufficient, inspect the full recorded agent events from that worktree with exactly:\n\
         oneharness history show {history} --project {worktree} --format text\n\n\
         Return ONLY one JSON object. Exactly one of these shapes is valid:\n\
         {{\"completion\":true,\"reason\":\"<concise reason>\"}}\n\
         {{\"completion\":false,\"message\":\"<exact next user message>\",\"reason\":\"<optional concise reason>\"}}",
        task = query.task,
        persona = query.persona,
        transcript = render_transcript(messages, true),
        worktree = query.worktree,
        history = query.history_name,
    )
}

/// Parse and strictly validate the supervisor's discriminated JSON response.
pub fn parse_supervisor(context: &str, text: &str) -> Result<SupervisorOutcome> {
    use crate::error::ProviderErrorKind::Protocol;
    let json = extract_json_object(text).ok_or_else(|| {
        Error::provider_classified(
            context,
            format!("supervisor did not return a JSON object; got: {text}"),
            Protocol,
        )
    })?;
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| {
        Error::provider_classified(
            context,
            format!("supervisor response was not valid JSON: {e}; got: {json}"),
            Protocol,
        )
    })?;
    let completion = value
        .get("completion")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            Error::provider_classified(
                context,
                "supervisor response needs boolean `completion`",
                Protocol,
            )
        })?;
    let reason = value
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let message = value.get("message").and_then(serde_json::Value::as_str);
    if completion {
        if reason.is_empty() || message.is_some() {
            return Err(Error::provider_classified(
                context,
                "completed supervisor response requires non-empty `reason` and forbids `message`",
                Protocol,
            ));
        }
        Ok(SupervisorOutcome::Completed { reason })
    } else {
        let message = message.filter(|m| !m.trim().is_empty()).ok_or_else(|| {
            Error::provider_classified(
                context,
                "continue supervisor response requires non-empty `message`",
                Protocol,
            )
        })?;
        Ok(SupervisorOutcome::Continue {
            message: message.to_string(),
            reason,
        })
    }
}

/// The prompt that asks the judge to evaluate `query` against the transcript.
/// The transcript is rendered **with tool events** so the verdict can reason over
/// the skill's actions, not just its words.
///
/// The large, invariant transcript is placed **first** and the varying criterion
/// **last**: scoring one finished transcript against several criteria then shares
/// a byte-identical prefix (framing + transcript), which the provider's prompt
/// cache can reuse across those calls — criterion-first would push the varying
/// text ahead of the transcript and defeat prefix caching entirely.
#[must_use]
pub fn build_judge_prompt(query: &JudgeQuery<'_>, messages: &[Message]) -> String {
    let transcript = render_transcript(messages, true);
    match query.kind {
        JudgeKind::Boolean => format!(
            "You are a strict, careful evaluator of an AI assistant's behavior.\n\n\
             Transcript (assistant tool actions are shown as `[tool]` lines):\n{transcript}\n\n\
             Criterion: {criterion}\n\n\
             Decide whether the criterion is satisfied. Respond with ONLY a \
             single-line JSON object and nothing else:\n\
             {{\"value\": true or false, \"reason\": \"<one short sentence>\"}}",
            criterion = query.criterion,
        ),
        JudgeKind::Numeric => {
            let (min, max) = query.scale.unwrap_or((0.0, 10.0));
            format!(
                "You are a strict, careful evaluator of an AI assistant's behavior.\n\n\
                 Transcript (assistant tool actions are shown as `[tool]` lines):\n{transcript}\n\n\
                 Criterion: {criterion}\n\n\
                 Score how well the criterion is satisfied on a scale from {min} to \
                 {max} (inclusive). Respond with ONLY a single-line JSON object and \
                 nothing else:\n\
                 {{\"value\": <number between {min} and {max}>, \"reason\": \"<one short sentence>\"}}",
                criterion = query.criterion,
            )
        }
    }
}

/// Build a free-text assessment prompt over the events-aware transcript.
#[must_use]
pub fn build_assessment_prompt(prompt: &str, messages: &[Message]) -> String {
    let transcript = render_transcript(messages, true);
    format!(
        "You are a careful evaluator of an AI assistant's behavior.\n\n\
         Transcript (assistant tool actions are shown as `[tool]` lines):\n{transcript}\n\n\
         Assessment request: {prompt}\n\n\
         Answer the assessment request concisely in free-running text. Return only \
         the assessment text."
    )
}

/// The most recent user message — the next-turn prompt when continuing a session.
#[must_use]
pub fn latest_user_message(messages: &[Message]) -> Option<&str> {
    messages
        .iter()
        .rev()
        .find(|m| m.role == crate::transcript::Role::User)
        .map(|m| m.content.as_str())
}

/// The `respond` prompt: just the latest user turn when `continuing` a real
/// harness session (the session already carries the earlier turns), or the whole
/// inlined transcript otherwise. One rule for "continue vs. re-inline", shared by
/// every provider that drives a real model.
#[must_use]
pub fn latest_or_inline(messages: &[Message], continuing: bool) -> String {
    if continuing {
        latest_user_message(messages)
            .map(str::to_string)
            .unwrap_or_default()
    } else {
        build_respond_prompt(messages)
    }
}

/// Extract the first JSON object from `text`, tolerating code fences and prose
/// around it (real models do not always emit bare JSON).
fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

/// Parse a judge's free-text reply into a typed [`JudgeVerdict`], tolerating the
/// prose/fences real models wrap around the JSON and type-checking `value`
/// against `kind`.
///
/// # Errors
/// [`Error::Provider`](crate::Error::Provider) (classified
/// [`Protocol`](crate::ProviderErrorKind::Protocol)) if no JSON object is present,
/// it is not valid JSON, `value` is missing, or `value` has the wrong type.
pub fn parse_verdict(kind: JudgeKind, context: &str, text: &str) -> Result<JudgeVerdict> {
    use crate::error::ProviderErrorKind::Protocol;

    let json = extract_json_object(text).ok_or_else(|| {
        Error::provider_classified(
            context,
            format!("judge did not return a JSON object; got: {text}"),
            Protocol,
        )
    })?;
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| {
        Error::provider_classified(
            context,
            format!("judge verdict was not valid JSON: {e}; got: {json}"),
            Protocol,
        )
    })?;
    let reason = value
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let raw = value.get("value").ok_or_else(|| {
        Error::provider_classified(context, "judge verdict has no `value` field", Protocol)
    })?;

    let verdict_value = match kind {
        JudgeKind::Boolean => JudgeValue::Bool(raw.as_bool().ok_or_else(|| {
            Error::provider_classified(
                context,
                format!("boolean judge `value` was not a bool: {raw}"),
                Protocol,
            )
        })?),
        JudgeKind::Numeric => JudgeValue::Number(raw.as_f64().ok_or_else(|| {
            Error::provider_classified(
                context,
                format!("numeric judge `value` was not a number: {raw}"),
                Protocol,
            )
        })?),
    };

    Ok(JudgeVerdict {
        value: verdict_value,
        reason,
        usage: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProviderErrorKind;
    use crate::transcript::{ToolEvent, Transcript};
    use serde_json::json;

    fn transcript_with_event() -> Transcript {
        let mut t = Transcript::from_input("commit the change");
        t.push(Message::assistant("done").with_events(vec![ToolEvent {
            kind: "tool_call".into(),
            name: Some("bash".into()),
            input: Some(json!({"command": "git commit -m x"})),
            output: Some("SECRET_RAW_TOOL_DUMP".into()),
            index: 0,
        }]));
        t
    }

    #[test]
    fn judge_prompt_includes_tool_events() {
        let t = transcript_with_event();
        let prompt = build_judge_prompt(
            &JudgeQuery {
                kind: JudgeKind::Boolean,
                criterion: "the change was committed",
                scale: None,
            },
            &t.messages,
        );
        assert!(prompt.contains("[tool]"));
        assert!(prompt.contains("git commit"));
        assert!(prompt.contains("the change was committed"));
        // The transcript must precede the criterion so the framing+transcript
        // prefix is shared (and prompt-cacheable) across criteria.
        let transcript_at = prompt.find("Transcript").unwrap();
        let criterion_at = prompt.find("Criterion:").unwrap();
        assert!(
            transcript_at < criterion_at,
            "transcript must come before the criterion for prefix caching"
        );
    }

    #[test]
    fn user_and_respond_prompts_omit_events() {
        let t = transcript_with_event();
        assert!(!build_user_prompt("a shopper", &t.messages).contains("[tool]"));
        assert!(!build_respond_prompt(&t.messages).contains("[tool]"));
    }

    #[test]
    fn numeric_prompt_carries_scale() {
        let prompt = build_judge_prompt(
            &JudgeQuery {
                kind: JudgeKind::Numeric,
                criterion: "politeness",
                scale: Some((1.0, 5.0)),
            },
            &[],
        );
        assert!(prompt.contains("scale from 1 to 5"));
    }

    #[test]
    fn assessment_prompt_includes_tool_events_and_request() {
        let prompt =
            build_assessment_prompt("identify follow-up work", &transcript_with_event().messages);
        assert!(prompt.contains("[tool]"));
        assert!(prompt.contains("git commit"));
        assert!(prompt.contains("identify follow-up work"));
    }

    #[test]
    fn supervisor_prompt_carries_contract_context_and_only_compact_events() {
        let prompt = build_supervisor_prompt(
            &SupervisorQuery {
                task: "ship the fix",
                persona: "a strict reviewer",
                done_when: Some("tests pass"),
                worktree: "/repo",
                history_name: "run-skill",
            },
            &transcript_with_event().messages,
        );
        for expected in [
            "ship the fix",
            "a strict reviewer",
            "tests pass",
            "[tool]",
            "git commit",
            "oneharness history show run-skill --project /repo --format text",
        ] {
            assert!(prompt.contains(expected), "missing {expected}");
        }
        assert!(!prompt.contains("SECRET_RAW_TOOL_DUMP"));
    }

    #[test]
    fn supervisor_parser_enforces_discriminated_shapes() {
        assert!(matches!(
            parse_supervisor("c", "{\"completion\":true,\"reason\":\"done\"}").unwrap(),
            SupervisorOutcome::Completed { .. }
        ));
        assert!(matches!(
            parse_supervisor("c", "{\"completion\":false,\"message\":\"retry\"}").unwrap(),
            SupervisorOutcome::Continue { .. }
        ));
        for bad in [
            "no json",
            "{\"completion\":true}",
            "{\"completion\":true,\"reason\":\"done\",\"message\":\"x\"}",
            "{\"completion\":false}",
        ] {
            assert_eq!(
                parse_supervisor("c", bad).unwrap_err().kind(),
                Some(ProviderErrorKind::Protocol)
            );
        }
    }

    #[test]
    fn latest_user_message_finds_last_user_turn() {
        let mut t = Transcript::from_input("first");
        t.push(Message::assistant("reply"));
        t.push(Message::user("second"));
        assert_eq!(latest_user_message(&t.messages), Some("second"));
        assert_eq!(latest_user_message(&[]), None);
    }

    #[test]
    fn parse_verdict_tolerates_fences_and_prose() {
        let v = parse_verdict(
            JudgeKind::Boolean,
            "test:judge",
            "Sure!\n```json\n{\"value\": true, \"reason\": \"ok\"}\n```",
        )
        .unwrap();
        assert_eq!(v.value, JudgeValue::Bool(true));
        assert_eq!(v.reason, "ok");
    }

    #[test]
    fn parse_verdict_numeric() {
        let v = parse_verdict(JudgeKind::Numeric, "c", "{\"value\": 7.5}").unwrap();
        assert_eq!(v.value, JudgeValue::Number(7.5));
        assert_eq!(v.reason, "");
    }

    #[test]
    fn parse_verdict_rejects_bad_shapes() {
        for text in [
            "no json here",
            "{not valid}",
            "{\"reason\": \"x\"}",
            "{\"value\": \"nope\"}",
        ] {
            let err = parse_verdict(JudgeKind::Boolean, "c", text).unwrap_err();
            assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
        }
        // A number where a bool is required, and vice versa.
        assert!(parse_verdict(JudgeKind::Boolean, "c", "{\"value\": 3}").is_err());
        assert!(parse_verdict(JudgeKind::Numeric, "c", "{\"value\": true}").is_err());
    }

    #[test]
    fn judge_value_deserializes_untagged() {
        let b: JudgeValue = serde_json::from_str("true").unwrap();
        assert_eq!(b, JudgeValue::Bool(true));
        let n: JudgeValue = serde_json::from_str("4").unwrap();
        assert_eq!(n, JudgeValue::Number(4.0));
    }
}
