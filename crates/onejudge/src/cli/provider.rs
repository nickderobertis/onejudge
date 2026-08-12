//! [`AnyProvider`]: a runtime-dispatched [`Provider`] the CLI builds from a
//! validated [`ProviderSpec`]. The library's providers are static types
//! (`SplitProvider<S, J>` is generic), so the CLI — which picks a backend at
//! runtime from YAML — needs one concrete type that erases the choice.
//! `AnyProvider` is that type: it owns each backend as an enum variant and forwards
//! every [`Provider`] method, dispatching a `split` to its two children exactly as
//! [`SplitProvider`] would.

use std::ops::ControlFlow;

use crate::{
    Assessment, AssistantTurn, CommandProvider, JudgeQuery, JudgeVerdict, Message,
    OneharnessProvider, Provider, SharedSpawnHook, SkillRef, SupervisorQuery, SupervisorTurn,
    ToolEvent, UserTurn,
};

use super::config::ProviderSpec;
use super::CliError;

/// A [`Provider`] whose backend is chosen at runtime from a [`ProviderSpec`].
pub enum AnyProvider {
    /// The default oneharness backend.
    Oneharness(OneharnessProvider),
    /// A custom JSON-lines command backend.
    Command(CommandProvider),
    /// A composed skill-runner + judge backend, dispatched like
    /// [`crate::SplitProvider`].
    Split {
        /// Runs the agent's turns.
        skill: Box<AnyProvider>,
        /// Judges and plays the simulated user.
        judge: Box<AnyProvider>,
    },
}

impl AnyProvider {
    /// Build a provider from a validated [`ProviderSpec`].
    ///
    /// # Errors
    /// [`CliError::Config`] if a backend's argv is empty or otherwise invalid.
    pub fn build(spec: &ProviderSpec) -> Result<Self, CliError> {
        match spec {
            ProviderSpec::Oneharness {
                bin,
                judge_config,
                stream,
                control,
            } => {
                let mut provider = OneharnessProvider::new()
                    .with_bin(bin)
                    .with_streaming(*stream)
                    .with_control(*control);
                if let Some(config) = judge_config {
                    provider = provider.with_judge_config(config.clone());
                }
                Ok(AnyProvider::Oneharness(provider))
            }
            ProviderSpec::Command { command } => {
                let provider = CommandProvider::new(command.clone())
                    .map_err(|e| CliError::Config(e.to_string()))?;
                Ok(AnyProvider::Command(provider))
            }
            ProviderSpec::Split { skill, judge } => Ok(AnyProvider::Split {
                skill: Box::new(AnyProvider::build(skill)?),
                judge: Box::new(AnyProvider::build(judge)?),
            }),
        }
    }

    /// Install `hook` on this backend — and, for a `split`, on **both** of its
    /// children, so one embedder-owned group spans the whole two-party tree rather
    /// than only the side that happened to spawn first.
    ///
    /// Each variant forwards to its own backend's `with_spawn_hook`, so this is the
    /// reach of the existing seam rather than a second grouping mechanism.
    #[must_use]
    pub fn with_spawn_hook(self, hook: SharedSpawnHook) -> Self {
        match self {
            AnyProvider::Oneharness(p) => AnyProvider::Oneharness(p.with_spawn_hook(hook)),
            AnyProvider::Command(p) => AnyProvider::Command(p.with_spawn_hook(hook)),
            AnyProvider::Split { skill, judge } => AnyProvider::Split {
                skill: Box::new(skill.with_spawn_hook(hook.clone())),
                judge: Box::new(judge.with_spawn_hook(hook)),
            },
        }
    }
}

impl Provider for AnyProvider {
    // Telemetry is collected by the backend that made the call, so this wrapper has
    // to forward both halves — without them the CLI's report carries no telemetry
    // at all, no matter what the backend recorded.
    fn reset_telemetry(&self) {
        match self {
            AnyProvider::Oneharness(p) => p.reset_telemetry(),
            AnyProvider::Command(p) => p.reset_telemetry(),
            AnyProvider::Split { skill, judge } => {
                skill.reset_telemetry();
                judge.reset_telemetry();
            }
        }
    }

    fn invocation_telemetry(&self) -> Vec<crate::InvocationTelemetry> {
        match self {
            AnyProvider::Oneharness(p) => p.invocation_telemetry(),
            AnyProvider::Command(p) => p.invocation_telemetry(),
            AnyProvider::Split { skill, judge } => {
                let mut records = skill.invocation_telemetry();
                records.extend(judge.invocation_telemetry());
                records
            }
        }
    }

    fn spawned_processes(&self) -> Vec<crate::SpawnedProcess> {
        match self {
            AnyProvider::Oneharness(p) => p.spawned_processes(),
            AnyProvider::Command(p) => p.spawned_processes(),
            AnyProvider::Split { skill, judge } => {
                let mut records = skill.spawned_processes();
                records.extend(judge.spawned_processes());
                records
            }
        }
    }

    // The skill side owns the controllable turn, exactly as
    // [`crate::SplitProvider`] decides it.
    fn control(&self) -> crate::ControlOutcome {
        match self {
            AnyProvider::Oneharness(p) => p.control(),
            AnyProvider::Command(p) => p.control(),
            AnyProvider::Split { skill, .. } => skill.control(),
        }
    }

    fn respond(
        &self,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> crate::Result<AssistantTurn> {
        match self {
            AnyProvider::Oneharness(p) => p.respond(skill, messages, session),
            AnyProvider::Command(p) => p.respond(skill, messages, session),
            AnyProvider::Split { skill: s, .. } => s.respond(skill, messages, session),
        }
    }

    fn respond_streaming(
        &self,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
        on_event: &mut dyn FnMut(&ToolEvent) -> ControlFlow<()>,
    ) -> crate::Result<AssistantTurn> {
        match self {
            AnyProvider::Oneharness(p) => p.respond_streaming(skill, messages, session, on_event),
            AnyProvider::Command(p) => p.respond_streaming(skill, messages, session, on_event),
            AnyProvider::Split { skill: s, .. } => {
                s.respond_streaming(skill, messages, session, on_event)
            }
        }
    }

    fn simulate_user(
        &self,
        persona: &str,
        messages: &[Message],
        session: Option<&str>,
    ) -> crate::Result<UserTurn> {
        match self {
            AnyProvider::Oneharness(p) => p.simulate_user(persona, messages, session),
            AnyProvider::Command(p) => p.simulate_user(persona, messages, session),
            AnyProvider::Split { judge, .. } => judge.simulate_user(persona, messages, session),
        }
    }
    fn supervise(
        &self,
        query: &SupervisorQuery<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> crate::Result<SupervisorTurn> {
        match self {
            AnyProvider::Oneharness(p) => p.supervise(query, messages, session),
            AnyProvider::Command(p) => p.supervise(query, messages, session),
            AnyProvider::Split { judge, .. } => judge.supervise(query, messages, session),
        }
    }

    fn judge(&self, query: &JudgeQuery<'_>, messages: &[Message]) -> crate::Result<JudgeVerdict> {
        match self {
            AnyProvider::Oneharness(p) => p.judge(query, messages),
            AnyProvider::Command(p) => p.judge(query, messages),
            AnyProvider::Split { judge, .. } => judge.judge(query, messages),
        }
    }

    fn assess(&self, prompt: &str, messages: &[Message]) -> crate::Result<Assessment> {
        match self {
            AnyProvider::Oneharness(p) => p.assess(prompt, messages),
            AnyProvider::Command(p) => p.assess(prompt, messages),
            AnyProvider::Split { judge, .. } => judge.assess(prompt, messages),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_oneharness_and_command_backends() {
        let oh = AnyProvider::build(&ProviderSpec::Oneharness {
            bin: "oneharness".into(),
            judge_config: Some("oneharness.judge.toml".into()),
            stream: false,
            control: false,
        })
        .unwrap();
        assert!(matches!(oh, AnyProvider::Oneharness(_)));

        let cmd = AnyProvider::build(&ProviderSpec::Command {
            command: vec!["prov".into()],
        })
        .unwrap();
        assert!(matches!(cmd, AnyProvider::Command(_)));
    }

    #[test]
    fn empty_command_argv_is_rejected() {
        let result = AnyProvider::build(&ProviderSpec::Command { command: vec![] });
        assert!(matches!(result, Err(CliError::Config(_))));
    }

    #[test]
    fn build_split_composes_children() {
        let spec = ProviderSpec::Split {
            skill: Box::new(ProviderSpec::Oneharness {
                bin: "oneharness".into(),
                judge_config: None,
                stream: true,
                control: false,
            }),
            judge: Box::new(ProviderSpec::Command {
                command: vec!["judge".into()],
            }),
        };
        let provider = AnyProvider::build(&spec).unwrap();
        assert!(matches!(provider, AnyProvider::Split { .. }));
    }
}
