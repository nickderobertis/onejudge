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
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::telemetry::InvocationTelemetry;
use crate::transcript::{Message, ToolEvent};
use crate::usage::Usage;

/// Producer-supplied artifact handles an evaluator may inspect read-only.
///
/// Paths are copied from the skill configuration and typed oneharness reports;
/// consumers must never reconstruct oneharness's storage layout.
#[derive(Debug, Clone, Copy, Default)]
pub struct EvidenceContext<'a> {
    /// The skill working directory, when the caller has one.
    pub worktree: Option<&'a str>,
    /// Exact absolute history artifact paths returned by the producer.
    pub history_files: &'a [String],
}

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
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
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
    /// Every note delivered into this run so far, in delivery order.
    ///
    /// Rendered by [`build_supervisor_prompt`] under a frame that names the role
    /// each note is addressed to, so a judge handed an update to the *worker's*
    /// task is told it is one and does not take the worker's job on. A note that
    /// bound a criterion is *also* in [`SupervisorQuery::done_when`]; this field
    /// is what the worker was told, never the bar on its own.
    pub notes: &'a [crate::note::DeliveredNote],
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
    /// The supervisor judged the work incomplete but gave no next instruction to
    /// act on (`completion:false` with an absent or blank `message`), and said so
    /// again when it was asked again (see [`supervise_with_reask`]).
    ///
    /// Not an error, deliberately: a run reaching here has already produced real
    /// work, and aborting it would destroy that work along with every commit of
    /// context behind it. The engine settles the run on what it has and records why
    /// ([`Outcome::settled_reason`](crate::Outcome::settled_reason)), which is also
    /// what keeps "the agent could not do it" distinguishable from "the supervisor
    /// had nothing to say".
    NoInstruction {
        /// Whatever justification the supervisor did give, possibly empty.
        reason: String,
    },
}

/// The correction appended when a **redirected** supervisor turn's answer did not
/// parse at all.
///
/// A redirect ends the turn and reopens the next one on the same session with the
/// note as its prompt, so what comes back answers the note rather than the
/// question — prose where the contract wants one JSON object. Naming the two
/// shapes again is what makes the single re-ask worth its cost.
pub const SUPERVISOR_REDIRECT_NOTE: &str = "\n\n\
     Your previous answer did not parse: a correction was delivered into your turn, and what \
     came back was not one JSON object in either valid shape. The correction stands and is \
     already part of what you were shown above. Answer the original question again, in exactly \
     one of the two shapes: `completion:true` with a `reason`, or `completion:false` with a \
     concrete, actionable next instruction in `message`.";

/// How many times a supervisor that answered `completion:false` with no usable
/// `message` is asked again before the run settles on the work it has.
///
/// Two, for three attempts in all. An omitted `message` is nearly always a
/// formatting slip that a fresh sample corrects, and two extra samples make a
/// *persistent* refusal decisive rather than unlucky. Each attempt is a full
/// judge-side invocation — a real turn's worth of latency and tokens — so a higher
/// bound buys very little and charges every run that hits it.
pub const SUPERVISOR_REASK_LIMIT: u32 = 2;

/// The correction appended to the supervisor prompt when the previous answer was
/// `completion:false` with no usable `message`. Naming what was unusable is what
/// makes the re-ask worth its cost — a verbatim repeat of the question invites a
/// verbatim repeat of the answer.
pub const SUPERVISOR_REASK_NOTE: &str = "\n\n\
     Your previous answer said the work was NOT complete but carried no usable `message`, so \
     the agent was handed nothing to act on and the decision could not be used. Answer again, \
     in exactly one of the two shapes above: `completion:true` with a `reason`, or \
     `completion:false` with a concrete, actionable next instruction in `message`.";

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
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
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
    /// Discard telemetry retained from an earlier run.
    fn reset_telemetry(&self) {}

    /// Return invocation telemetry collected since the last reset.
    fn invocation_telemetry(&self) -> Vec<InvocationTelemetry> {
        Vec::new()
    }

    /// Return the processes this provider spawned since the last reset, with the
    /// group an embedder's [`SpawnHook`](crate::SpawnHook) placed each one in.
    ///
    /// Empty for a provider that spawns nothing (an in-memory backend), so the
    /// engine reports only processes that really exist.
    fn spawned_processes(&self) -> Vec<crate::SpawnedProcess> {
        Vec::new()
    }

    /// Where an `oneharness interrupt` process addresses the controllable turn
    /// this provider's **agent side** opened, if one was asked for.
    ///
    /// The default is [`ControlOutcome::NotRequested`](crate::ControlOutcome): a backend with no
    /// out-of-band turn control never claims one. A composing provider forwards
    /// the *skill-running* side, since that is the side the ask applies to.
    fn control(&self) -> crate::ControlOutcome {
        crate::ControlOutcome::NotRequested
    }

    /// Where an `oneharness interrupt` process addresses the controllable turn
    /// this provider's **supervisor side** opened, if one was asked for.
    ///
    /// Reported apart from [`Provider::control`] because the two sides are
    /// separately addressable and separately refusable: they run under different
    /// configs on different session names, so a harness that can be interrupted on
    /// one is not thereby interruptible on the other, and a caller must be able to
    /// tell which lever it actually has.
    ///
    /// Defaulted, so every existing implementation keeps compiling and keeps
    /// claiming no lever — which is the truth for a backend that opens no socket.
    fn supervisor_control(&self) -> crate::ControlOutcome {
        crate::ControlOutcome::NotRequested
    }

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
    ///
    /// The default composes the two legacy calls (`judge`, then `simulate_user`)
    /// under the same empty-continue policy the real seams use: a simulated user
    /// with nothing to say is re-asked up to [`SUPERVISOR_REASK_LIMIT`] times and
    /// then reported as [`SupervisorOutcome::NoInstruction`], never delivered to
    /// the agent as a blank user turn.
    ///
    /// # Errors
    /// Propagates a failure of the underlying `judge` / `simulate_user` call.
    fn supervise(
        &self,
        query: &SupervisorQuery<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<SupervisorTurn> {
        let criterion = query.done_when.unwrap_or("the original task is complete");
        supervise_with_reask(|_attempt| {
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
            let outcome = if user.message.trim().is_empty() {
                SupervisorOutcome::NoInstruction {
                    reason: verdict.reason,
                }
            } else {
                SupervisorOutcome::Continue {
                    message: user.message,
                    reason: verdict.reason,
                }
            };
            Ok(SupervisorTurn {
                outcome,
                usage: user.usage,
            })
        })
    }

    /// Decide completion with read-only access to producer-supplied evidence.
    fn supervise_with_evidence(
        &self,
        query: &SupervisorQuery<'_>,
        messages: &[Message],
        session: Option<&str>,
        _evidence: EvidenceContext<'_>,
    ) -> Result<SupervisorTurn> {
        self.supervise(query, messages, session)
    }

    /// Score a criterion against the conversation.
    ///
    /// # Errors
    /// [`Error::Provider`](crate::Error::Provider) if the command fails or returns
    /// malformed output.
    fn judge(&self, query: &JudgeQuery<'_>, messages: &[Message]) -> Result<JudgeVerdict>;

    /// Score a criterion with read-only access to producer-supplied evidence.
    /// Existing providers remain source-compatible and retain their old behavior.
    fn judge_with_evidence(
        &self,
        query: &JudgeQuery<'_>,
        messages: &[Message],
        _evidence: EvidenceContext<'_>,
    ) -> Result<JudgeVerdict> {
        self.judge(query, messages)
    }

    /// Write a free-text assessment of the finished conversation.
    ///
    /// # Errors
    /// [`Error::Provider`](crate::Error::Provider) if the command fails or returns
    /// malformed output.
    fn assess(&self, prompt: &str, messages: &[Message]) -> Result<Assessment>;

    /// Write an assessment with read-only access to producer-supplied evidence.
    fn assess_with_evidence(
        &self,
        prompt: &str,
        messages: &[Message],
        _evidence: EvidenceContext<'_>,
    ) -> Result<Assessment> {
        self.assess(prompt, messages)
    }
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
    build_supervisor_prompt_with_evidence(
        query,
        messages,
        EvidenceContext {
            worktree: Some(query.worktree),
            history_files: &[],
        },
    )
}

/// Build the supervisor prompt with exact producer-supplied artifact handles.
#[must_use]
pub fn build_supervisor_prompt_with_evidence(
    query: &SupervisorQuery<'_>,
    messages: &[Message],
    evidence: EvidenceContext<'_>,
) -> String {
    let criterion = query.done_when.unwrap_or(
        "No explicit completion criterion was supplied; continue unless the original task is clearly complete.",
    );
    format!(
        "You are the simulated USER and completion supervisor for an AI agent.\n\n\
         Original task:\n{task}\n\nSupervisor persona:\n{persona}\n\n\
         Completion criterion:\n{criterion}\n\n{notes}{evidence}\
         Conversation transcript (tool actions are compact normalized summaries, never raw dumps):\n{transcript}\n\n\
         Return ONLY one JSON object. Exactly one of these shapes is valid:\n\
         {{\"completion\":true,\"reason\":\"<concise reason>\"}}\n\
         {{\"completion\":false,\"message\":\"<concrete, actionable next instruction>\",\"reason\":\"<optional concise reason>\"}}\n\n\
         `message` is handed to the agent VERBATIM as its next user turn, and it is the only \
         thing the agent receives: it cannot see this decision, your reason, or anything else \
         you were shown. So `completion:false` obliges you to say what to do next — name the \
         concrete next action in the imperative, with enough detail to act on it without \
         further context. \"Not done\" with no message leaves the agent no path forward, since \
         there is no default next turn to fall back on, so an empty or missing `message` is \
         not a valid answer. If you cannot name a next action, the work is done: answer \
         `completion:true` with the reason instead.",
        task = query.task,
        persona = query.persona,
        notes = crate::note::supervisor_block(query.notes)
            .map(|block| format!("{block}\n"))
            .unwrap_or_default(),
        transcript = render_transcript(messages, true),
        evidence = evidence_prompt(evidence),
    )
}

/// Parse and strictly validate the supervisor's discriminated JSON response.
///
/// Everything malformed is a [`Protocol`](crate::ProviderErrorKind::Protocol)
/// error, with one deliberate exception: `completion:false` carrying no usable
/// `message` parses to [`SupervisorOutcome::NoInstruction`] instead. That case is
/// the supervisor having nothing to say, not the transport being broken, and it
/// must never cost the caller the work the run has already produced — see
/// [`supervise_with_reask`].
///
/// # Errors
/// [`Error::Provider`](crate::Error::Provider) if the response is not one JSON
/// object in one of the two documented shapes.
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
        // A `continue` with nothing in it used to be refused here, which killed the
        // whole run: a `Protocol` error aborts the dispatch and takes every commit
        // of context with it, however much finished work the agent had produced.
        // The supervisor having nothing to say is not the same failure as a broken
        // transport, so it is reported rather than raised. The blank `message` is
        // still never delivered to the agent as a user turn.
        match message.filter(|m| !m.trim().is_empty()) {
            Some(message) => Ok(SupervisorOutcome::Continue {
                message: message.to_string(),
                reason,
            }),
            None => Ok(SupervisorOutcome::NoInstruction { reason }),
        }
    }
}

/// Ask for one supervisor decision, re-asking while the supervisor judges the work
/// incomplete but names no next instruction to act on.
///
/// `ask` is given the zero-based attempt number, so a seam that builds its own
/// prompt can append [`SUPERVISOR_REASK_NOTE`] on a retry. Usage is accumulated
/// across every attempt, so a re-asked decision still reports what it cost.
///
/// On exhaustion ([`SUPERVISOR_REASK_LIMIT`] re-asks) this returns the last
/// [`SupervisorOutcome::NoInstruction`] rather than an error, and the engine
/// settles the run on the work it has. A dispatch that produced real work must
/// never be destroyed because its supervisor had nothing to say.
///
/// # Errors
/// Whatever `ask` returns: a transport or protocol failure is still fatal.
pub fn supervise_with_reask(
    mut ask: impl FnMut(u32) -> Result<SupervisorTurn>,
) -> Result<SupervisorTurn> {
    let mut usage = Usage::default();
    let mut unusable = SupervisorOutcome::NoInstruction {
        reason: String::new(),
    };
    for attempt in 0..=SUPERVISOR_REASK_LIMIT {
        let turn = ask(attempt)?;
        if let Some(u) = &turn.usage {
            usage.add(u);
        }
        match turn.outcome {
            SupervisorOutcome::NoInstruction { .. } => unusable = turn.outcome,
            decided => {
                return Ok(SupervisorTurn {
                    outcome: decided,
                    usage: (!usage.is_empty()).then_some(usage),
                })
            }
        }
    }
    Ok(SupervisorTurn {
        outcome: unusable,
        usage: (!usage.is_empty()).then_some(usage),
    })
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
    build_judge_prompt_with_evidence(query, messages, EvidenceContext::default())
}

/// Build the criterion prompt with the shared evidence contract.
#[must_use]
pub fn build_judge_prompt_with_evidence(
    query: &JudgeQuery<'_>,
    messages: &[Message],
    evidence: EvidenceContext<'_>,
) -> String {
    let transcript = render_transcript(messages, true);
    let evidence = evidence_prompt(evidence);
    match query.kind {
        JudgeKind::Boolean => format!(
            "You are a strict, careful evaluator of an AI assistant's behavior.\n\n\
             Transcript (assistant tool actions are shown as `[tool]` lines):\n{transcript}\n\n{evidence}\
             Criterion: {criterion}\n\n\
             Decide whether the criterion is satisfied. Tool transport events may occur before answer text. \
             The final non-empty answer-text line must be exactly one JSON object; nothing may follow it:\n\
             {{\"value\": true or false, \"reason\": \"<one short sentence>\"}}",
            criterion = query.criterion,
        ),
        JudgeKind::Numeric => {
            let (min, max) = query.scale.unwrap_or((0.0, 10.0));
            format!(
                "You are a strict, careful evaluator of an AI assistant's behavior.\n\n\
                 Transcript (assistant tool actions are shown as `[tool]` lines):\n{transcript}\n\n{evidence}\
                 Criterion: {criterion}\n\n\
                 Score how well the criterion is satisfied on a scale from {min} to \
                 {max} (inclusive). Tool transport events may occur before answer text. \
                 The final non-empty answer-text line must be exactly one JSON object; nothing may follow it:\n\
                 {{\"value\": <number between {min} and {max}>, \"reason\": \"<one short sentence>\"}}",
                criterion = query.criterion,
            )
        }
    }
}

/// Build a free-text assessment prompt over the events-aware transcript.
#[must_use]
pub fn build_assessment_prompt(prompt: &str, messages: &[Message]) -> String {
    build_assessment_prompt_with_evidence(prompt, messages, EvidenceContext::default())
}

/// Build an assessment prompt with the shared evidence contract.
#[must_use]
pub fn build_assessment_prompt_with_evidence(
    prompt: &str,
    messages: &[Message],
    evidence: EvidenceContext<'_>,
) -> String {
    let transcript = render_transcript(messages, true);
    let evidence = evidence_prompt(evidence);
    format!(
        "You are a careful evaluator of an AI assistant's behavior.\n\n\
         Transcript (assistant tool actions are shown as `[tool]` lines):\n{transcript}\n\n{evidence}\
         Assessment request: {prompt}\n\n\
         Answer the assessment request concisely in free-running text. Return only \
         the assessment text."
    )
}

/// Semantic marker shared by every evaluator prompt and deterministic double.
pub const EVIDENCE_PROMPT_MARKER: &str = "EVIDENCE CONTRACT (READ-ONLY, ENFORCED)";
/// Maximum number of fixed Git evidence requests before a verdict is required.
pub const EVIDENCE_TOOL_RETRY_LIMIT: u32 = 4;

#[derive(Debug, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
enum EvidenceToolRequest {
    GitStatus,
    GitDiff,
}

/// Resolve an exact, closed evidence-tool request against the context worktree.
pub(crate) fn resolve_evidence_request(
    line: &str,
    context: EvidenceContext<'_>,
) -> Result<Option<String>> {
    let trimmed = line.trim();
    let Ok(object) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(trimmed)
    else {
        return Ok(None);
    };
    if !object.contains_key("tool") {
        return Ok(None);
    }
    if object.len() != 1 {
        return Err(Error::provider_classified(
            "evidence",
            "closed evidence requests admit only the `tool` member",
            crate::ProviderErrorKind::Protocol,
        ));
    }
    let request: EvidenceToolRequest = serde_json::from_str(trimmed).map_err(|error| {
        Error::provider_classified(
            "evidence",
            format!("invalid closed evidence request: {error}"),
            crate::ProviderErrorKind::Protocol,
        )
    })?;
    let worktree = context.worktree.ok_or_else(|| {
        Error::provider_classified(
            "evidence",
            "evidence tool requested without a worktree",
            crate::ProviderErrorKind::Protocol,
        )
    })?;
    let output = match request {
        EvidenceToolRequest::GitStatus => hardened_git(worktree, &["status", "--porcelain=v1"])?,
        EvidenceToolRequest::GitDiff => {
            let unstaged = hardened_git(worktree, &["diff", "--no-ext-diff", "--no-textconv"])?;
            let staged = hardened_git(
                worktree,
                &["diff", "--cached", "--no-ext-diff", "--no-textconv"],
            )?;
            format!("unstaged:\n{unstaged}\nstaged:\n{staged}")
        }
    };
    Ok(Some(output))
}

fn hardened_git(worktree: &str, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    let path = std::env::var_os("PATH");
    let system_root = std::env::var_os("SystemRoot");
    command.env_clear();
    if let Some(path) = path {
        command.env("PATH", path);
    }
    if let Some(system_root) = system_root {
        command.env("SystemRoot", system_root);
    }
    let output = command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .args([
            "-c",
            "core.pager=cat",
            "-c",
            "interactive.diffFilter=",
            "-c",
            "diff.external=",
            "-c",
            "core.fsmonitor=false",
        ])
        .arg("-C")
        .arg(worktree)
        .args(args)
        .output()
        .map_err(|error| {
            Error::provider_classified(
                "evidence",
                format!("could not run fixed Git operation: {error}"),
                crate::ProviderErrorKind::Protocol,
            )
        })?;
    if !output.status.success() {
        return Err(Error::provider_classified(
            "evidence",
            format!(
                "fixed Git operation failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            crate::ProviderErrorKind::Protocol,
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| {
        Error::provider_classified(
            "evidence",
            format!("Git output was not UTF-8: {error}"),
            crate::ProviderErrorKind::Protocol,
        )
    })
}

fn evidence_prompt(context: EvidenceContext<'_>) -> String {
    let worktree = context.worktree.unwrap_or("(not supplied)");
    let histories = if context.history_files.is_empty() {
        "  (none supplied)".to_string()
    } else {
        context
            .history_files
            .iter()
            .map(|p| format!("  - {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "{EVIDENCE_PROMPT_MARKER}\n\
         `[tool]` lines are abbreviated summaries; absence there is not evidence of absence. \
         Only read-only tools may inspect files, git state, and full history; no change is permitted, \
         and this restriction is enforced. Restrictive evaluators must use file-reading or glob tools \
         directly, never a shell command. Before the final answer you may request exactly \
         `{{\"tool\":\"git_status\"}}` or `{{\"tool\":\"git_diff\"}}`; no other member is allowed.\n\
         Worktree: {worktree}\nHistory files (exact producer-returned paths):\n{histories}\n\n"
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

/// Extract the first JSON object from text (used only by the legacy supervisor).
fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

/// Parse only the final non-empty answer-text line as a typed [`JudgeVerdict`].
///
/// # Errors
/// [`Error::Provider`](crate::Error::Provider) (classified
/// [`Protocol`](crate::ProviderErrorKind::Protocol)) if no JSON object is present,
/// it is not valid JSON, `value` is missing, or `value` has the wrong type.
pub fn parse_verdict(kind: JudgeKind, context: &str, text: &str) -> Result<JudgeVerdict> {
    use crate::error::ProviderErrorKind::Protocol;

    let json = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .ok_or_else(|| {
            Error::provider_classified(
                context,
                "judge returned no non-empty answer-text line",
                Protocol,
            )
        })?;
    #[derive(Deserialize)]
    struct VerdictLine {
        value: serde_json::Value,
        #[serde(default)]
        reason: String,
    }
    let value: VerdictLine = serde_json::from_str(json).map_err(|e| {
        Error::provider_classified(
            context,
            format!("judge verdict was not valid JSON: {e}; got: {json}"),
            Protocol,
        )
    })?;
    let reason = value.reason;
    let raw = &value.value;

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

    #[derive(Default)]
    struct DefaultSupervisor {
        complete: bool,
        /// The simulated user has nothing to say — the legacy shape of the same
        /// defect, since a blank next turn is as useless to the agent as none.
        blank_user: bool,
    }

    impl Provider for DefaultSupervisor {
        fn respond(
            &self,
            _: &SkillRef<'_>,
            _: &[Message],
            _: Option<&str>,
        ) -> Result<AssistantTurn> {
            unreachable!()
        }
        fn simulate_user(&self, _: &str, _: &[Message], _: Option<&str>) -> Result<UserTurn> {
            Ok(UserTurn {
                message: if self.blank_user {
                    "  ".into()
                } else {
                    "next".into()
                },
                stop: false,
                usage: Some(Usage {
                    output_tokens: Some(2),
                    ..Usage::default()
                }),
            })
        }
        fn judge(&self, _: &JudgeQuery<'_>, _: &[Message]) -> Result<JudgeVerdict> {
            Ok(JudgeVerdict {
                value: JudgeValue::Bool(self.complete),
                reason: "because".into(),
                usage: None,
            })
        }
        fn assess(&self, _: &str, _: &[Message]) -> Result<Assessment> {
            unreachable!()
        }
    }

    fn transcript_with_event() -> Transcript {
        let mut t = Transcript::from_input("commit the change");
        t.push(Message::assistant("done").with_events(vec![ToolEvent {
            kind: "tool_call".into(),
            name: Some("bash".into()),
            input: Some(json!({"command": "git commit -m x"})),
            output: Some("SECRET_RAW_TOOL_DUMP".into()),
            index: 0,
            tool_call_id: None,
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
    fn every_evaluator_prompt_carries_the_same_exact_evidence_contract() {
        let histories = vec![
            "/absolute/agent-a.jsonl".into(),
            "/absolute/agent-b.jsonl".into(),
        ];
        let evidence = EvidenceContext {
            worktree: Some("/worktree"),
            history_files: &histories,
        };
        let messages = transcript_with_event();
        let boolean = build_judge_prompt_with_evidence(
            &JudgeQuery {
                kind: JudgeKind::Boolean,
                criterion: "done",
                scale: None,
            },
            &messages.messages,
            evidence,
        );
        let numeric = build_judge_prompt_with_evidence(
            &JudgeQuery {
                kind: JudgeKind::Numeric,
                criterion: "quality",
                scale: Some((1.0, 5.0)),
            },
            &messages.messages,
            evidence,
        );
        let supervisor = build_supervisor_prompt_with_evidence(
            &SupervisorQuery {
                task: "task",
                persona: "reviewer",
                done_when: None,
                worktree: "/worktree",
                history_name: "unused",
                notes: &[],
            },
            &messages.messages,
            evidence,
        );
        let assessment =
            build_assessment_prompt_with_evidence("follow-ups", &messages.messages, evidence);
        for prompt in [&boolean, &numeric, &supervisor, &assessment] {
            for expected in [
                EVIDENCE_PROMPT_MARKER,
                "absence there is not evidence of absence",
                "no change is permitted",
                "Worktree: /worktree",
                "/absolute/agent-a.jsonl",
                "/absolute/agent-b.jsonl",
                "file-reading or glob tools",
            ] {
                assert!(
                    prompt.contains(expected),
                    "missing `{expected}` in {prompt}"
                );
            }
        }
        for prompt in [&boolean, &numeric] {
            assert!(
                prompt.contains("final non-empty answer-text line must be exactly one JSON object")
            );
            assert!(prompt.contains("Tool transport events may occur before answer text"));
        }
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
                notes: &[],
            },
            &transcript_with_event().messages,
        );
        for expected in [
            "ship the fix",
            "a strict reviewer",
            "tests pass",
            "[tool]",
            "git commit",
            EVIDENCE_PROMPT_MARKER,
            "Worktree: /repo",
        ] {
            assert!(prompt.contains(expected), "missing {expected}");
        }
        assert!(!prompt.contains("SECRET_RAW_TOOL_DUMP"));
    }

    #[test]
    fn supervisor_prompt_states_what_message_is_for_and_that_it_is_required() {
        // The whole defect starts here: a supervisor that is never told what
        // `message` does has no reason to write one, and "not done" with nothing
        // in it leaves the agent — which receives that string and nothing else —
        // with no path forward.
        let prompt = build_supervisor_prompt(
            &SupervisorQuery {
                task: "ship the fix",
                persona: "a strict reviewer",
                done_when: None,
                worktree: "/repo",
                history_name: "run-skill",
                notes: &[],
            },
            &[],
        );
        for expected in [
            "handed to the agent VERBATIM as its next user turn",
            "the only thing the agent receives",
            "concrete next action",
            "not a valid answer",
            "answer `completion:true` with the reason instead",
        ] {
            assert!(prompt.contains(expected), "missing {expected}");
        }
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
            "{bad json}",
            "{}",
            "{\"completion\":true}",
            "{\"completion\":true,\"reason\":\"done\",\"message\":\"x\"}",
        ] {
            assert_eq!(
                parse_supervisor("c", bad).unwrap_err().kind(),
                Some(ProviderErrorKind::Protocol)
            );
        }
    }

    #[test]
    fn a_continue_with_nothing_to_say_is_reported_not_raised() {
        // It used to be a `Protocol` error, which aborted the run and destroyed
        // every turn of work behind it. It is now a decision the caller can act on
        // — and the blank message is still never handed on as a user turn.
        for empty in [
            "{\"completion\":false}",
            "{\"completion\":false,\"message\":\"\"}",
            "{\"completion\":false,\"message\":\"   \",\"reason\":\"still failing\"}",
        ] {
            assert!(
                matches!(
                    parse_supervisor("c", empty).unwrap(),
                    SupervisorOutcome::NoInstruction { .. }
                ),
                "{empty} should parse to NoInstruction"
            );
        }
        assert!(
            matches!(
                parse_supervisor("c", "{\"completion\":false,\"reason\":\"still failing\"}")
                    .unwrap(),
                SupervisorOutcome::NoInstruction { reason } if reason == "still failing"
            ),
            "the supervisor's own reason is carried through"
        );
    }

    /// A supervisor that answers `completion:false` with no `message` for its first
    /// `blank` attempts, then a usable continue.
    fn blank_then_continue(blank: u32) -> impl FnMut(u32) -> Result<SupervisorTurn> {
        move |attempt| {
            let outcome = if attempt < blank {
                SupervisorOutcome::NoInstruction {
                    reason: "not yet".into(),
                }
            } else {
                SupervisorOutcome::Continue {
                    message: "run the integration suite".into(),
                    reason: "unit tests alone are insufficient".into(),
                }
            };
            Ok(SupervisorTurn {
                outcome,
                usage: Some(Usage {
                    output_tokens: Some(1),
                    ..Usage::default()
                }),
            })
        }
    }

    #[test]
    fn a_supervisor_with_nothing_to_say_is_asked_again() {
        let turn = supervise_with_reask(blank_then_continue(SUPERVISOR_REASK_LIMIT)).unwrap();
        assert!(matches!(
            turn.outcome,
            SupervisorOutcome::Continue { ref message, .. } if message == "run the integration suite"
        ));
        // Every attempt is a real judge-side call, so every attempt is billed.
        assert_eq!(
            turn.usage.unwrap().output_tokens,
            Some(u64::from(SUPERVISOR_REASK_LIMIT) + 1)
        );
    }

    #[test]
    fn a_supervisor_that_never_says_anything_settles_instead_of_erroring() {
        let mut attempts = 0;
        let turn = supervise_with_reask(|_| {
            attempts += 1;
            Ok(SupervisorTurn {
                outcome: SupervisorOutcome::NoInstruction {
                    reason: "nothing to add".into(),
                },
                usage: None,
            })
        })
        .unwrap();
        assert_eq!(
            attempts,
            SUPERVISOR_REASK_LIMIT + 1,
            "the bound is honoured"
        );
        assert!(matches!(
            turn.outcome,
            SupervisorOutcome::NoInstruction { reason } if reason == "nothing to add"
        ));
    }

    #[test]
    fn a_failing_supervisor_call_is_still_fatal() {
        // The re-ask is for a supervisor with nothing to say, not for a broken
        // transport: retrying that would only multiply the failure.
        let mut attempts = 0;
        let err = supervise_with_reask(|_| {
            attempts += 1;
            Err(Error::provider_classified(
                "c",
                "the judge exited non-zero",
                ProviderErrorKind::Protocol,
            ))
        })
        .unwrap_err();
        assert_eq!(attempts, 1);
        assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
    }

    #[test]
    fn provider_default_supervisor_preserves_legacy_implementors() {
        let query = SupervisorQuery {
            task: "task",
            persona: "persona",
            done_when: None,
            worktree: "/repo",
            history_name: "run",
            notes: &[],
        };
        let completed = DefaultSupervisor {
            complete: true,
            ..DefaultSupervisor::default()
        }
        .supervise(&query, &[], Some("user"))
        .unwrap();
        assert!(
            matches!(completed.outcome, SupervisorOutcome::Completed { reason } if reason == "because")
        );
        let continued = DefaultSupervisor::default()
            .supervise(&query, &[], Some("user"))
            .unwrap();
        assert!(
            matches!(continued.outcome, SupervisorOutcome::Continue { message, reason } if message == "next" && reason == "because")
        );
        assert_eq!(continued.usage.unwrap().output_tokens, Some(2));
    }

    #[test]
    fn the_default_supervisor_never_hands_on_a_blank_next_turn() {
        let query = SupervisorQuery {
            task: "task",
            persona: "persona",
            done_when: None,
            worktree: "/repo",
            history_name: "run",
            notes: &[],
        };
        let turn = DefaultSupervisor {
            complete: false,
            blank_user: true,
        }
        .supervise(&query, &[], Some("user"))
        .unwrap();
        assert!(
            matches!(turn.outcome, SupervisorOutcome::NoInstruction { .. }),
            "a whitespace-only simulated user turn is no instruction at all"
        );
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
    fn parse_verdict_uses_only_the_final_non_empty_line() {
        let v = parse_verdict(
            JudgeKind::Boolean,
            "test:judge",
            "Earlier prose and {\"value\":false}.\n{\"value\": true, \"reason\": \"ok\"}\n\n",
        )
        .unwrap();
        assert_eq!(v.value, JudgeValue::Bool(true));
        assert_eq!(v.reason, "ok");
        for text in [
            "{\"value\":true}\ntrailing prose",
            "```json\n{\"value\":true}\n```",
            "{\"value\":true} trailing",
            "\n \n",
        ] {
            assert_eq!(
                parse_verdict(JudgeKind::Boolean, "c", text)
                    .unwrap_err()
                    .kind(),
                Some(ProviderErrorKind::Protocol)
            );
        }
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
            "{\"value\": true, \"reason\": 3}",
        ] {
            let err = parse_verdict(JudgeKind::Boolean, "c", text).unwrap_err();
            assert_eq!(err.kind(), Some(ProviderErrorKind::Protocol));
        }
        // A number where a bool is required, and vice versa.
        assert!(parse_verdict(JudgeKind::Boolean, "c", "{\"value\": 3}").is_err());
        assert!(parse_verdict(JudgeKind::Numeric, "c", "{\"value\": true}").is_err());
    }

    #[test]
    fn closed_git_tools_report_status_and_both_diff_halves() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("onejudge-evidence-{}-{nonce}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap()
        };
        assert!(git(&["init", "-q"]).status.success());
        fs::write(dir.join("tracked"), "base\n").unwrap();
        assert!(git(&["add", "tracked"]).status.success());
        assert!(git(&[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.test",
            "commit",
            "-qm",
            "base"
        ])
        .status
        .success());
        fs::write(dir.join("tracked"), "staged\n").unwrap();
        assert!(git(&["add", "tracked"]).status.success());
        fs::write(dir.join("tracked"), "staged\nunstaged\n").unwrap();
        fs::write(dir.join("untracked"), "new\n").unwrap();
        let worktree = dir.to_str().unwrap();
        let evidence = EvidenceContext {
            worktree: Some(worktree),
            history_files: &[],
        };
        let status = resolve_evidence_request("{\"tool\":\"git_status\"}", evidence)
            .unwrap()
            .unwrap();
        assert!(status.contains("MM tracked"));
        assert!(status.contains("?? untracked"));
        let diff = resolve_evidence_request("{\"tool\":\"git_diff\"}", evidence)
            .unwrap()
            .unwrap();
        assert!(diff.contains("+staged"));
        assert!(diff.contains("+unstaged"));
        for refused in [
            "{\"tool\":\"git_status\",\"path\":\"..\"}",
            "{\"tool\":\"git_diff\",\"args\":[\"--exec-path\"]}",
            "{\"tool\":\"status\"}",
        ] {
            assert_eq!(
                resolve_evidence_request(refused, evidence)
                    .unwrap_err()
                    .kind(),
                Some(ProviderErrorKind::Protocol)
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn judge_value_deserializes_untagged() {
        let b: JudgeValue = serde_json::from_str("true").unwrap();
        assert_eq!(b, JudgeValue::Bool(true));
        let n: JudgeValue = serde_json::from_str("4").unwrap();
        assert_eq!(n, JudgeValue::Number(4.0));
    }
}
