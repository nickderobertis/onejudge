//! [`SplitProvider`]: compose two [`Provider`]s into one — a *skill* provider
//! that runs the assistant/skill turns, and a *judge* provider that scores
//! criteria and role-plays the simulated user.
//!
//! The two responsibilities are genuinely separable: you might drive the skill on
//! one harness ([`OneharnessProvider`](crate::OneharnessProvider) on a capable
//! platform) while judging and simulating the user on a cheaper harness or model —
//! or vice versa. `SplitProvider` routes each [`Provider`] operation to whichever
//! backend owns it, so the engine sees one provider and neither backend needs to
//! know the other exists.

use std::ops::ControlFlow;

use crate::error::Result;
use crate::provider::{AssistantTurn, JudgeQuery, JudgeVerdict, Provider, SkillRef, UserTurn};
use crate::transcript::{Message, ToolEvent};

/// A [`Provider`] that dispatches each operation to one of two backends.
///
/// - [`Provider::respond`] / [`Provider::respond_streaming`] and session
///   continuation go to the **skill** provider (the one running the agent under
///   test).
/// - [`Provider::judge`] and [`Provider::simulate_user`] go to the **judge**
///   provider (the one scoring the transcript and playing the user).
///
/// Build one with [`SplitProvider::new`].
pub struct SplitProvider<S, J> {
    skill: S,
    judge: J,
}

impl<S: Provider, J: Provider> SplitProvider<S, J> {
    /// Compose `skill` (runs the assistant turns) with `judge` (scores criteria
    /// and role-plays the simulated user).
    pub fn new(skill: S, judge: J) -> Self {
        Self { skill, judge }
    }

    /// The backend that runs the skill's assistant turns.
    pub fn skill_provider(&self) -> &S {
        &self.skill
    }

    /// The backend that judges the transcript and role-plays the simulated user.
    pub fn judge_provider(&self) -> &J {
        &self.judge
    }
}

impl<S: Provider, J: Provider> Provider for SplitProvider<S, J> {
    fn respond(
        &self,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<AssistantTurn> {
        self.skill.respond(skill, messages, session)
    }

    fn respond_streaming(
        &self,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
        on_event: &mut dyn FnMut(&ToolEvent) -> ControlFlow<()>,
    ) -> Result<AssistantTurn> {
        self.skill
            .respond_streaming(skill, messages, session, on_event)
    }

    fn simulate_user(
        &self,
        persona: &str,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<UserTurn> {
        self.judge.simulate_user(persona, messages, session)
    }

    fn judge(&self, query: &JudgeQuery<'_>, messages: &[Message]) -> Result<JudgeVerdict> {
        self.judge.judge(query, messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{JudgeKind, JudgeValue};
    use crate::usage::Usage;
    use std::cell::Cell;

    /// A provider that records which of its operations were called and returns a
    /// canned turn tagged with `tag`, so a test can prove which backend handled
    /// each op.
    #[derive(Default)]
    struct Tagged {
        tag: &'static str,
        responded: Cell<u32>,
        streamed: Cell<u32>,
        simulated: Cell<u32>,
        judged: Cell<u32>,
    }

    impl Tagged {
        fn new(tag: &'static str) -> Self {
            Self {
                tag,
                ..Self::default()
            }
        }
    }

    impl Provider for Tagged {
        fn respond(
            &self,
            _skill: &SkillRef<'_>,
            _messages: &[Message],
            _session: Option<&str>,
        ) -> Result<AssistantTurn> {
            self.responded.set(self.responded.get() + 1);
            Ok(AssistantTurn {
                message: self.tag.into(),
                ..AssistantTurn::default()
            })
        }

        fn respond_streaming(
            &self,
            _skill: &SkillRef<'_>,
            _messages: &[Message],
            _session: Option<&str>,
            on_event: &mut dyn FnMut(&ToolEvent) -> ControlFlow<()>,
        ) -> Result<AssistantTurn> {
            self.streamed.set(self.streamed.get() + 1);
            let event = ToolEvent {
                kind: "tool_call".into(),
                name: Some("bash".into()),
                input: None,
                output: None,
                index: 0,
            };
            let _ = on_event(&event);
            Ok(AssistantTurn {
                message: self.tag.into(),
                events: vec![event],
                ..AssistantTurn::default()
            })
        }

        fn simulate_user(
            &self,
            _persona: &str,
            _messages: &[Message],
            _session: Option<&str>,
        ) -> Result<UserTurn> {
            self.simulated.set(self.simulated.get() + 1);
            Ok(UserTurn {
                message: self.tag.into(),
                ..UserTurn::default()
            })
        }

        fn judge(&self, _query: &JudgeQuery<'_>, _messages: &[Message]) -> Result<JudgeVerdict> {
            self.judged.set(self.judged.get() + 1);
            Ok(JudgeVerdict {
                value: JudgeValue::Bool(true),
                reason: self.tag.into(),
                usage: Some(Usage::default()),
            })
        }
    }

    fn skill_ref() -> SkillRef<'static> {
        SkillRef {
            name: "s",
            dir: "/s",
            instructions: "do x",
        }
    }

    fn boolean_query() -> JudgeQuery<'static> {
        JudgeQuery {
            kind: JudgeKind::Boolean,
            criterion: "ok",
            scale: None,
        }
    }

    #[test]
    fn respond_routes_to_the_skill_backend() {
        let split = SplitProvider::new(Tagged::new("skill"), Tagged::new("judge"));
        let turn = split.respond(&skill_ref(), &[], None).unwrap();
        assert_eq!(turn.message, "skill");
        assert_eq!(split.skill_provider().responded.get(), 1);
        assert_eq!(split.judge_provider().responded.get(), 0);
    }

    #[test]
    fn streaming_routes_to_the_skill_backend() {
        let split = SplitProvider::new(Tagged::new("skill"), Tagged::new("judge"));
        let mut seen = 0;
        let turn = split
            .respond_streaming(&skill_ref(), &[], None, &mut |_e| {
                seen += 1;
                ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(turn.message, "skill");
        assert_eq!(seen, 1);
        assert_eq!(split.skill_provider().streamed.get(), 1);
        assert_eq!(split.judge_provider().streamed.get(), 0);
    }

    #[test]
    fn user_and_judge_route_to_the_judge_backend() {
        let split = SplitProvider::new(Tagged::new("skill"), Tagged::new("judge"));
        let user = split.simulate_user("persona", &[], None).unwrap();
        assert_eq!(user.message, "judge");
        let verdict = split.judge(&boolean_query(), &[]).unwrap();
        assert_eq!(verdict.reason, "judge");
        assert_eq!(split.judge_provider().simulated.get(), 1);
        assert_eq!(split.judge_provider().judged.get(), 1);
        // The skill backend was never asked to judge or simulate.
        assert_eq!(split.skill_provider().simulated.get(), 0);
        assert_eq!(split.skill_provider().judged.get(), 0);
    }
}
