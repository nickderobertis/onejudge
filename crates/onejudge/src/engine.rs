//! The engine: drives a [`Conversation`] into a [`Transcript`] through a
//! [`Provider`], turn by turn, and exposes the judge as a helper. This is the
//! simulated-interaction loop — single-turn for a bare input, or a simulated-user
//! loop bounded by `max_turns` / `done_when` / the skill declaring itself done.

use std::ops::ControlFlow;

use crate::error::Result;
use crate::provider::{
    build_judge_prompt, Assessment, AssistantTurn, JudgeKind, JudgeQuery, JudgeVerdict, Provider,
    SkillRef, SupervisorOutcome, SupervisorQuery,
};
use crate::report::{NamedVerdict, Report};
use crate::transcript::{Message, ToolEvent, Transcript};
use crate::usage::Usage;

/// The skill / agent under test.
#[derive(Debug, Clone)]
pub struct Skill {
    /// The skill's name.
    pub name: String,
    /// The skill's working directory.
    pub dir: String,
    /// The skill instructions, delivered as a system prompt.
    pub instructions: String,
}

impl Skill {
    /// Build a skill from its parts.
    pub fn new(
        name: impl Into<String>,
        dir: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            dir: dir.into(),
            instructions: instructions.into(),
        }
    }

    fn as_ref(&self) -> SkillRef<'_> {
        SkillRef {
            name: &self.name,
            dir: &self.dir,
            instructions: &self.instructions,
        }
    }
}

/// A simulated user that drives a multi-turn conversation.
#[derive(Debug, Clone)]
pub struct SimulatedUser {
    /// How the simulated user should behave (their instructions).
    pub persona: String,
    /// A plain-English condition; when the judge decides it holds, the
    /// conversation ends. Without it the run ends at `max_turns` or when the skill
    /// reports itself done.
    pub done_when: Option<String>,
    /// Per-conversation override of the engine's assistant-turn cap.
    pub max_turns: Option<u32>,
}

impl SimulatedUser {
    /// A simulated user with just a persona (no `done_when` / `max_turns`).
    pub fn new(persona: impl Into<String>) -> Self {
        Self {
            persona: persona.into(),
            done_when: None,
            max_turns: None,
        }
    }

    /// Set the end condition (builder style).
    #[must_use]
    pub fn done_when(mut self, criterion: impl Into<String>) -> Self {
        self.done_when = Some(criterion.into());
        self
    }

    /// Set the per-conversation turn cap (builder style).
    #[must_use]
    pub fn max_turns(mut self, turns: u32) -> Self {
        self.max_turns = Some(turns);
        self
    }
}

/// A conversation to drive: the skill, the initial user input, and — for a
/// multi-turn run — the simulated user.
#[derive(Debug, Clone)]
pub struct Conversation {
    /// The skill under test.
    pub skill: Skill,
    /// The first thing the (real) user says to the skill.
    pub input: String,
    /// The simulated user; `None` for a single-turn conversation.
    pub user: Option<SimulatedUser>,
}

impl Conversation {
    /// A single-turn conversation (skill responds once to `input`).
    pub fn single_turn(skill: Skill, input: impl Into<String>) -> Self {
        Self {
            skill,
            input: input.into(),
            user: None,
        }
    }

    /// A multi-turn conversation driven by `user`.
    pub fn multi_turn(skill: Skill, input: impl Into<String>, user: SimulatedUser) -> Self {
        Self {
            skill,
            input: input.into(),
            user: Some(user),
        }
    }
}

/// Engine settings: the loop's own concerns — the default turn cap and the
/// caller-owned session-name base threaded across turns.
///
/// Harness and model **selection** is no longer onejudge's concern: the agent side
/// uses oneharness's own discovered config (`oneharness.toml`) and the judge /
/// simulated-user side uses a separately-named oneharness config (see
/// [`OneharnessProvider`](crate::OneharnessProvider)), so `Settings` carries no
/// platform or model.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Default assistant-turn cap when a [`SimulatedUser`] does not override it.
    pub max_turns: u32,
    /// The base name for the caller-owned session threaded across turns. Use a
    /// distinct value per run to avoid colliding in the harness's on-disk session
    /// store. The engine derives `<base>-skill` and `<base>-user` from it, always
    /// threading them to the provider; a provider that cannot continue a session
    /// (e.g. a harness with no headless session id) degrades gracefully by
    /// re-inlining the transcript.
    pub session_name: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self::new()
    }
}

impl Settings {
    /// Loop settings with `max_turns` defaulting to 8 and `session_name` to
    /// `"onejudge"`. Adjust the fields directly or via the builders.
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_turns: 8,
            session_name: "onejudge".into(),
        }
    }

    /// Override the default turn cap (builder style).
    #[must_use]
    pub fn with_max_turns(mut self, turns: u32) -> Self {
        self.max_turns = turns;
        self
    }

    /// Override the session-name base (builder style).
    #[must_use]
    pub fn with_session_name(mut self, name: impl Into<String>) -> Self {
        self.session_name = name.into();
        self
    }
}

/// What driving one conversation produced.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The full conversation transcript, with tool events attached to assistant
    /// turns.
    pub transcript: Transcript,
    /// Aggregated usage across every provider call (`None` if nothing reported).
    pub usage: Option<Usage>,
    /// Whether a streaming sink asked to short-circuit the run.
    pub stopped_early: bool,
    /// The unified supervisor's completion reason, when it ended the loop.
    pub completion_reason: Option<String>,
}

impl Outcome {
    /// Bundle this outcome with the `verdicts` scored against it into onejudge's
    /// versioned [`Report`] contract — the serializable wire form a consumer or
    /// SDK persists and composes over.
    #[must_use]
    pub fn into_report(self, verdicts: Vec<NamedVerdict>) -> Report {
        self.into_report_with_assessment(verdicts, None)
    }

    /// Bundle this outcome with verdicts and an optional free-text assessment.
    #[must_use]
    pub fn into_report_with_assessment(
        self,
        verdicts: Vec<NamedVerdict>,
        assessment: Option<String>,
    ) -> Report {
        let mut report = Report::new(self.transcript, verdicts, self.usage, self.stopped_early);
        report.completion_reason = self.completion_reason;
        match assessment {
            Some(text) => report.with_assessment(text),
            None => report,
        }
    }
}

/// Drives conversations against a [`Provider`] and judges transcripts.
pub struct Engine<'a> {
    provider: &'a dyn Provider,
    settings: Settings,
}

impl<'a> Engine<'a> {
    /// Build an engine over `provider` with `settings`.
    #[must_use]
    pub fn new(provider: &'a dyn Provider, settings: Settings) -> Self {
        Self { provider, settings }
    }

    /// The engine's settings.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Drive `conversation` to completion (buffered turns), returning the
    /// transcript and aggregated usage.
    ///
    /// # Errors
    /// Propagates the first provider failure.
    pub fn run(&self, conversation: &Conversation) -> Result<Outcome> {
        let mut discard = |_: &StreamEvent| ControlFlow::Continue(());
        self.converse(conversation, false, &mut discard)
    }

    /// Like [`Engine::run`], but drives each turn through
    /// [`Provider::respond_streaming`] and delivers each tool event to `on_event`
    /// the instant it is observed. Returning [`ControlFlow::Break`] short-circuits:
    /// the current turn is torn down and [`Outcome::stopped_early`] is `true`.
    ///
    /// # Errors
    /// As [`Engine::run`].
    pub fn run_streaming(
        &self,
        conversation: &Conversation,
        on_event: &mut dyn FnMut(&StreamEvent) -> ControlFlow<()>,
    ) -> Result<Outcome> {
        self.converse(conversation, true, on_event)
    }

    fn converse(
        &self,
        conversation: &Conversation,
        streaming: bool,
        on_event: &mut dyn FnMut(&StreamEvent) -> ControlFlow<()>,
    ) -> Result<Outcome> {
        let skill = conversation.skill.as_ref();
        let max_turns = conversation
            .user
            .as_ref()
            .and_then(|u| u.max_turns)
            .unwrap_or(self.settings.max_turns) as usize;

        // Thread ONE caller-owned session name across turns (skill and simulated
        // user each get their own), instead of extracting and re-passing a native
        // id — the uniform `oneharness --session` handle. The engine always threads
        // them; a provider that cannot continue a session degrades gracefully by
        // re-inlining the transcript, so onejudge no longer needs to know the
        // harness's capability up front.
        let skill_session = format!("{}-skill", self.settings.session_name);
        let user_session = format!("{}-user", self.settings.session_name);

        let mut transcript = Transcript::from_input(&conversation.input);
        let mut totals = Usage::default();
        let mut completion_reason = None;

        loop {
            let turn_index = transcript.assistant_turns() + 1;
            let mut broke = false;
            let turn = if streaming {
                self.provider.respond_streaming(
                    &skill,
                    &transcript.messages,
                    Some(skill_session.as_str()),
                    &mut |event| {
                        let flow = on_event(&StreamEvent {
                            turn: turn_index,
                            event,
                        });
                        broke |= flow.is_break();
                        flow
                    },
                )?
            } else {
                self.provider
                    .respond(&skill, &transcript.messages, Some(skill_session.as_str()))?
            };
            let AssistantTurn {
                message,
                done: skill_done,
                usage,
                events,
            } = turn;
            if let Some(u) = &usage {
                totals.add(u);
            }
            transcript.push(Message::assistant(message).with_events(events));

            if broke {
                return Ok(self.finish(transcript, totals, true, None));
            }

            // Single-turn conversations stop after the first assistant turn.
            let Some(user) = &conversation.user else {
                break;
            };
            if skill_done || transcript.assistant_turns() >= max_turns {
                break;
            }
            let decision = self.provider.supervise(
                &SupervisorQuery {
                    task: &conversation.input,
                    persona: &user.persona,
                    done_when: user.done_when.as_deref(),
                    worktree: &conversation.skill.dir,
                    history_name: &skill_session,
                },
                &transcript.messages,
                Some(user_session.as_str()),
            )?;
            let usage = decision.usage;
            if let Some(u) = &usage {
                totals.add(u);
            }
            match decision.outcome {
                SupervisorOutcome::Completed { reason } => {
                    completion_reason = Some(reason);
                    break;
                }
                SupervisorOutcome::Continue { message, .. } => {
                    transcript.push(Message::user(message))
                }
            }
        }

        Ok(self.finish(transcript, totals, false, completion_reason))
    }

    fn finish(
        &self,
        transcript: Transcript,
        totals: Usage,
        stopped_early: bool,
        completion_reason: Option<String>,
    ) -> Outcome {
        Outcome {
            transcript,
            usage: (!totals.is_empty()).then_some(totals),
            stopped_early,
            completion_reason,
        }
    }

    fn judge_boolean_raw(&self, criterion: &str, transcript: &Transcript) -> Result<JudgeVerdict> {
        let query = JudgeQuery {
            kind: JudgeKind::Boolean,
            criterion,
            scale: None,
        };
        // Passing the transcript through the shared judge prompt keeps the
        // events-aware rendering (Improvement 1) on the `done_when` check too.
        let _ = build_judge_prompt(&query, &transcript.messages);
        self.provider.judge(&query, &transcript.messages)
    }

    /// Score a boolean criterion against a finished transcript. The judge sees the
    /// transcript with tool events rendered, so the verdict can reason over what
    /// the skill did.
    ///
    /// # Errors
    /// Propagates a provider failure.
    pub fn judge_boolean(&self, criterion: &str, transcript: &Transcript) -> Result<JudgeVerdict> {
        self.judge_boolean_raw(criterion, transcript)
    }

    /// Score a numeric criterion against a finished transcript on the inclusive
    /// `[min, max]` scale.
    ///
    /// # Errors
    /// [`Error::Invalid`](crate::Error::Invalid) if `min > max`; otherwise a
    /// provider failure.
    pub fn judge_numeric(
        &self,
        criterion: &str,
        min: f64,
        max: f64,
        transcript: &Transcript,
    ) -> Result<JudgeVerdict> {
        if min > max {
            return Err(crate::error::Error::Invalid(format!(
                "numeric judge scale has min ({min}) greater than max ({max})"
            )));
        }
        let query = JudgeQuery {
            kind: JudgeKind::Numeric,
            criterion,
            scale: Some((min, max)),
        };
        self.provider.judge(&query, &transcript.messages)
    }

    /// Write a free-text assessment of a finished transcript. The judge sees the
    /// same events-aware transcript as boolean and numeric judgements.
    ///
    /// # Errors
    /// Propagates a provider failure.
    pub fn assess(&self, prompt: &str, transcript: &Transcript) -> Result<Assessment> {
        self.provider.assess(prompt, &transcript.messages)
    }
}

/// One streamed tool event delivered live to an [`Engine::run_streaming`] sink,
/// tagged with the turn it belongs to.
#[derive(serde::Serialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
pub struct StreamEvent<'a> {
    /// 1-based assistant-turn index within this run.
    pub turn: usize,
    /// The normalized tool event.
    pub event: &'a ToolEvent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{JudgeValue, SkillRef, SupervisorTurn, UserTurn};
    use std::cell::RefCell;

    /// An in-memory provider scripted with canned turns, so the loop's
    /// orchestration is testable without a subprocess.
    struct Scripted {
        assistant: Vec<AssistantTurn>,
        user: Vec<UserTurn>,
        judge: Vec<JudgeVerdict>,
        seen: RefCell<Seen>,
    }

    #[derive(Default)]
    struct Seen {
        assistant: usize,
        user: usize,
        judge: usize,
        assess: usize,
        supervisor: usize,
        skill_sessions: Vec<Option<String>>,
        user_sessions: Vec<Option<String>>,
    }

    impl Provider for Scripted {
        fn respond(
            &self,
            _skill: &SkillRef<'_>,
            _messages: &[Message],
            session: Option<&str>,
        ) -> Result<AssistantTurn> {
            let mut seen = self.seen.borrow_mut();
            seen.skill_sessions.push(session.map(String::from));
            let i = seen.assistant.min(self.assistant.len() - 1);
            seen.assistant += 1;
            Ok(self.assistant[i].clone())
        }

        fn simulate_user(
            &self,
            _persona: &str,
            _messages: &[Message],
            session: Option<&str>,
        ) -> Result<UserTurn> {
            let mut seen = self.seen.borrow_mut();
            seen.user_sessions.push(session.map(String::from));
            let i = seen.user.min(self.user.len() - 1);
            seen.user += 1;
            Ok(self.user[i].clone())
        }

        fn supervise(
            &self,
            query: &SupervisorQuery<'_>,
            _messages: &[Message],
            session: Option<&str>,
        ) -> Result<SupervisorTurn> {
            let mut seen = self.seen.borrow_mut();
            seen.supervisor += 1;
            seen.user_sessions.push(session.map(String::from));
            if query.done_when.is_some() && !self.judge.is_empty() {
                let i = seen.judge.min(self.judge.len() - 1);
                seen.judge += 1;
                let verdict = self.judge[i].clone();
                if matches!(verdict.value, JudgeValue::Bool(true)) {
                    return Ok(SupervisorTurn {
                        outcome: SupervisorOutcome::Completed {
                            reason: verdict.reason,
                        },
                        usage: verdict.usage,
                    });
                }
            }
            let i = seen.user.min(self.user.len() - 1);
            seen.user += 1;
            if self.user[i].stop {
                return Ok(SupervisorTurn {
                    outcome: SupervisorOutcome::Completed {
                        reason: "simulated user stopped".into(),
                    },
                    usage: self.user[i].usage.clone(),
                });
            }
            Ok(SupervisorTurn {
                outcome: SupervisorOutcome::Continue {
                    message: self.user[i].message.clone(),
                    reason: String::new(),
                },
                usage: self.user[i].usage.clone(),
            })
        }

        fn judge(&self, _query: &JudgeQuery<'_>, _messages: &[Message]) -> Result<JudgeVerdict> {
            let mut seen = self.seen.borrow_mut();
            let i = seen.judge.min(self.judge.len().saturating_sub(1));
            seen.judge += 1;
            Ok(self.judge[i].clone())
        }

        fn assess(&self, prompt: &str, _messages: &[Message]) -> Result<Assessment> {
            self.seen.borrow_mut().assess += 1;
            Ok(Assessment {
                text: format!("assessment: {prompt}"),
                usage: None,
            })
        }
    }

    fn assistant(msg: &str, done: bool) -> AssistantTurn {
        AssistantTurn {
            message: msg.into(),
            done,
            ..AssistantTurn::default()
        }
    }

    fn skill() -> Skill {
        Skill::new("greeter", "/skills/greeter", "Greet the user.")
    }

    fn settings() -> Settings {
        Settings::new()
    }

    #[test]
    fn assessment_is_one_distinct_free_text_judge_operation() {
        let provider = Scripted {
            assistant: vec![assistant("done", true)],
            user: vec![],
            judge: vec![],
            seen: RefCell::new(Seen::default()),
        };
        let engine = Engine::new(&provider, settings());
        let transcript = engine
            .run(&Conversation::single_turn(skill(), "do it"))
            .unwrap()
            .transcript;
        let assessment = engine.assess("find follow-up work", &transcript).unwrap();
        assert_eq!(assessment.text, "assessment: find follow-up work");
        assert_eq!(provider.seen.borrow().assess, 1);
        assert_eq!(provider.seen.borrow().judge, 0);
    }

    #[test]
    fn single_turn_stops_after_one_reply() {
        let provider = Scripted {
            assistant: vec![assistant("hi there", false)],
            user: vec![],
            judge: vec![],
            seen: RefCell::default(),
        };
        let engine = Engine::new(&provider, settings());
        let outcome = engine
            .run(&Conversation::single_turn(skill(), "hello"))
            .unwrap();
        assert_eq!(outcome.transcript.assistant_turns(), 1);
        assert!(!outcome.stopped_early);
        assert_eq!(provider.seen.borrow().assistant, 1);
    }

    #[test]
    fn multi_turn_runs_to_max_turns_without_done_when() {
        let provider = Scripted {
            assistant: vec![assistant("a", false)],
            user: vec![UserTurn {
                message: "more".into(),
                stop: false,
                usage: None,
            }],
            judge: vec![],
            seen: RefCell::default(),
        };
        let engine = Engine::new(&provider, settings());
        let user = SimulatedUser::new("keep going").max_turns(3);
        let outcome = engine
            .run(&Conversation::multi_turn(skill(), "start", user))
            .unwrap();
        assert_eq!(outcome.transcript.assistant_turns(), 3);
    }

    #[test]
    fn done_when_ends_the_loop_early() {
        let provider = Scripted {
            assistant: vec![assistant("working", false)],
            user: vec![UserTurn {
                message: "ok".into(),
                stop: false,
                usage: None,
            }],
            // First done_when check is false, second is true -> stop after turn 2.
            judge: vec![
                JudgeVerdict {
                    value: JudgeValue::Bool(false),
                    reason: String::new(),
                    usage: None,
                },
                JudgeVerdict {
                    value: JudgeValue::Bool(true),
                    reason: String::new(),
                    usage: None,
                },
            ],
            seen: RefCell::default(),
        };
        let engine = Engine::new(&provider, settings());
        let user = SimulatedUser::new("shopper")
            .done_when("the booking is confirmed")
            .max_turns(8);
        let outcome = engine
            .run(&Conversation::multi_turn(skill(), "book me", user))
            .unwrap();
        assert_eq!(outcome.transcript.assistant_turns(), 2);
        assert_eq!(provider.seen.borrow().judge, 2);
    }

    #[test]
    fn skill_done_and_user_stop_both_end_the_loop() {
        // skill declares done on turn 1.
        let done_provider = Scripted {
            assistant: vec![assistant("all set", true)],
            user: vec![],
            judge: vec![],
            seen: RefCell::default(),
        };
        let engine = Engine::new(&done_provider, settings());
        let user = SimulatedUser::new("x").max_turns(5);
        let outcome = engine
            .run(&Conversation::multi_turn(skill(), "go", user))
            .unwrap();
        assert_eq!(outcome.transcript.assistant_turns(), 1);

        // user stops after turn 1.
        let stop_provider = Scripted {
            assistant: vec![assistant("hi", false)],
            user: vec![UserTurn {
                message: "bye".into(),
                stop: true,
                usage: None,
            }],
            judge: vec![],
            seen: RefCell::default(),
        };
        let engine = Engine::new(&stop_provider, settings());
        let outcome = engine
            .run(&Conversation::multi_turn(
                skill(),
                "go",
                SimulatedUser::new("x").max_turns(5),
            ))
            .unwrap();
        assert_eq!(outcome.transcript.assistant_turns(), 1);
        assert_eq!(stop_provider.seen.borrow().user, 1);
    }

    #[test]
    fn session_name_is_always_threaded() {
        // The engine always threads `<base>-skill` / `<base>-user`; degradation is
        // the provider's concern, not a capability gate here.
        let provider = Scripted {
            assistant: vec![assistant("a", false)],
            user: vec![UserTurn {
                message: "again".into(),
                stop: false,
                usage: None,
            }],
            judge: vec![],
            seen: RefCell::default(),
        };
        let engine = Engine::new(&provider, settings().with_session_name("run-42"));
        engine
            .run(&Conversation::multi_turn(
                skill(),
                "go",
                SimulatedUser::new("x").max_turns(2),
            ))
            .unwrap();
        let seen = provider.seen.borrow();
        assert!(seen
            .skill_sessions
            .iter()
            .all(|s| s.as_deref() == Some("run-42-skill")));
        assert!(seen
            .user_sessions
            .iter()
            .all(|s| s.as_deref() == Some("run-42-user")));
    }

    #[test]
    fn session_name_threads_on_a_single_turn_too() {
        let provider = Scripted {
            assistant: vec![assistant("a", true)],
            user: vec![],
            judge: vec![],
            seen: RefCell::default(),
        };
        let engine = Engine::new(&provider, settings());
        engine
            .run(&Conversation::single_turn(skill(), "go"))
            .unwrap();
        assert_eq!(
            provider.seen.borrow().skill_sessions,
            vec![Some("onejudge-skill".to_string())]
        );
    }

    #[test]
    fn usage_is_aggregated_across_calls() {
        let provider = Scripted {
            assistant: vec![AssistantTurn {
                message: "a".into(),
                done: true,
                usage: Some(Usage {
                    input_tokens: Some(5),
                    output_tokens: Some(2),
                    ..Usage::default()
                }),
                events: vec![],
            }],
            user: vec![],
            judge: vec![],
            seen: RefCell::default(),
        };
        let engine = Engine::new(&provider, settings());
        let outcome = engine
            .run(&Conversation::single_turn(skill(), "go"))
            .unwrap();
        let usage = outcome.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(5));
        assert_eq!(usage.output_tokens, Some(2));
    }

    #[test]
    fn streaming_break_short_circuits() {
        let provider = Scripted {
            assistant: vec![AssistantTurn {
                message: "working".into(),
                done: false,
                usage: None,
                events: vec![ToolEvent {
                    kind: "tool_call".into(),
                    name: Some("bash".into()),
                    input: None,
                    output: None,
                    index: 0,
                }],
            }],
            user: vec![],
            judge: vec![],
            seen: RefCell::default(),
        };
        let engine = Engine::new(&provider, settings());
        let mut seen = 0;
        let outcome = engine
            .run_streaming(
                &Conversation::multi_turn(skill(), "go", SimulatedUser::new("x").max_turns(9)),
                &mut |_ev| {
                    seen += 1;
                    ControlFlow::Break(())
                },
            )
            .unwrap();
        assert!(outcome.stopped_early);
        assert_eq!(seen, 1);
        // The run stopped on turn 1; the simulated user never spoke.
        assert_eq!(provider.seen.borrow().user, 0);
    }

    #[test]
    fn judge_numeric_rejects_inverted_scale() {
        let provider = Scripted {
            assistant: vec![],
            user: vec![],
            judge: vec![],
            seen: RefCell::default(),
        };
        let engine = Engine::new(&provider, settings());
        let err = engine
            .judge_numeric("x", 10.0, 1.0, &Transcript::default())
            .unwrap_err();
        assert!(matches!(err, crate::error::Error::Invalid(_)));
    }
}
