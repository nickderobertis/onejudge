//! [`AnyProvider`]: a runtime-dispatched [`Provider`] the CLI builds from a
//! validated [`ProviderSpec`]. The library's providers are static types
//! (`SplitProvider<S, J>` is generic), so the CLI — which picks a backend at
//! runtime from YAML — needs one concrete type that erases the choice.
//! `AnyProvider` is that type: it owns each backend as an enum variant and forwards
//! every [`Provider`] method, dispatching a `split` to its skill child and its
//! [`JudgePanel`] exactly as [`SplitProvider`] would.

use std::ops::ControlFlow;

use crate::{
    Assessment, AssistantTurn, CommandProvider, EvidenceContext, JudgeAbilities, JudgeEntry,
    JudgePanel, JudgeQuery, JudgeVerdict, LlmlintProvider, Message, OneharnessProvider, Provider,
    SharedSpawnHook, SkillRef, SupervisorQuery, SupervisorTurn, ToolEvent, UserTurn,
};

use super::config::{ProviderKind, ProviderSpec};
use super::CliError;

/// A [`Provider`] whose backend is chosen at runtime from a [`ProviderSpec`].
#[allow(
    clippy::large_enum_variant,
    reason = "exactly one of these exists per run (two for a `split`), and it lives for the whole \
              run — so the unused bytes of a smaller variant are a few hundred on the stack, once. \
              Boxing the oneharness backend would change a public variant's shape to buy that."
)]
pub enum AnyProvider {
    /// The default oneharness backend.
    Oneharness(OneharnessProvider),
    /// A custom JSON-lines command backend.
    Command(CommandProvider),
    /// A judge whose verdict is one `llmlint` run over the worker's tree. Only
    /// ever built as a judge of a [`AnyProvider::Split`] panel; every agent-side
    /// call on it is [`crate::Error::Invalid`].
    Llmlint(LlmlintProvider),
    /// A composed skill-runner + judge-panel backend, dispatched like
    /// [`crate::SplitProvider`].
    Split {
        /// Runs the agent's turns.
        skill: Box<AnyProvider>,
        /// The judges — one or more, run concurrently and combined — that judge
        /// and play the simulated user.
        judges: JudgePanel<AnyProvider>,
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
                mock_harness,
            } => {
                let mut provider = OneharnessProvider::new()
                    .with_streaming(*stream)
                    .with_control(*control);
                // Only when the config named one: an unset `bin` is the
                // in-process engine, and `with_bin` would opt out of it.
                if let Some(bin) = bin {
                    provider = provider.with_bin(bin);
                }
                // After `bin`, so a named binary is the one the mocked run spawns
                // (naming a mock harness only falls back to `oneharness` on PATH).
                for id in mock_harness {
                    provider = provider.with_mock_harness(id);
                }
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
            ProviderSpec::Llmlint {
                bin,
                config,
                diff_base,
                args,
            } => {
                // The probe runs here, so an absent `llmlint` is a config error
                // before any turn — never a judge that silently passes.
                let mut provider =
                    LlmlintProvider::new(bin).map_err(|e| CliError::Config(e.to_string()))?;
                if let Some(config) = config {
                    provider = provider.with_config(config.clone());
                }
                if let Some(base) = diff_base {
                    provider = provider.with_diff_base(base.clone());
                }
                Ok(AnyProvider::Llmlint(provider.with_args(args.clone())))
            }
            ProviderSpec::Split { skill, judges } => Ok(AnyProvider::Split {
                skill: Box::new(AnyProvider::build(skill)?),
                judges: JudgePanel::new(
                    judges
                        .iter()
                        .map(|judge| {
                            Ok(JudgeEntry::new(
                                judge.label.clone(),
                                judge.provider.kind().as_str(),
                                AnyProvider::build(&judge.provider)?,
                            )
                            .with_abilities(abilities_of(judge.provider.kind())))
                        })
                        .collect::<Result<Vec<_>, CliError>>()?,
                )
                .map_err(|e| CliError::Config(e.to_string()))?,
            }),
        }
    }

    /// Install `hook` on this backend — and, for a `split`, on its skill child
    /// **and every judge of its panel**, so one embedder-owned group spans the
    /// whole tree rather than only the side that happened to spawn first.
    ///
    /// Each variant forwards to its own backend's `with_spawn_hook`, so this is the
    /// reach of the existing seam rather than a second grouping mechanism.
    #[must_use]
    pub fn with_spawn_hook(self, hook: SharedSpawnHook) -> Self {
        match self {
            AnyProvider::Oneharness(p) => AnyProvider::Oneharness(p.with_spawn_hook(hook)),
            AnyProvider::Command(p) => AnyProvider::Command(p.with_spawn_hook(hook)),
            AnyProvider::Llmlint(p) => AnyProvider::Llmlint(p.with_spawn_hook(hook)),
            AnyProvider::Split { skill, judges } => AnyProvider::Split {
                skill: Box::new(skill.with_spawn_hook(hook.clone())),
                judges: judges.map_judges(|judge| judge.with_spawn_hook(hook.clone())),
            },
        }
    }
}

/// Which judge-side operations a judge of `kind` takes part in. An `llmlint`
/// judge decides and answers boolean judgements only — it scores no number,
/// writes no prose and plays no user — so the panel leaves it out of those
/// rather than reaching an operation it refuses. Every other kind has every
/// ability.
fn abilities_of(kind: ProviderKind) -> JudgeAbilities {
    match kind {
        ProviderKind::Llmlint => JudgeAbilities {
            numeric: false,
            prose: false,
            user: false,
        },
        ProviderKind::Oneharness | ProviderKind::Command | ProviderKind::Split => {
            JudgeAbilities::default()
        }
    }
}

impl Provider for AnyProvider {
    fn supervises_lost_turns(&self) -> bool {
        match self {
            AnyProvider::Command(provider) => provider.supervises_lost_turns(),
            AnyProvider::Split { judges, .. } => judges.supervises_lost_turns(),
            AnyProvider::Oneharness(_) | AnyProvider::Llmlint(_) => false,
        }
    }

    // Telemetry is collected by the backend that made the call, so this wrapper has
    // to forward both halves — without them the CLI's report carries no telemetry
    // at all, no matter what the backend recorded.
    fn reset_telemetry(&self) {
        match self {
            AnyProvider::Oneharness(p) => p.reset_telemetry(),
            AnyProvider::Command(p) => p.reset_telemetry(),
            AnyProvider::Llmlint(p) => p.reset_telemetry(),
            AnyProvider::Split { skill, judges } => {
                skill.reset_telemetry();
                judges.reset_telemetry();
            }
        }
    }

    fn invocation_telemetry(&self) -> Vec<crate::InvocationTelemetry> {
        match self {
            AnyProvider::Oneharness(p) => p.invocation_telemetry(),
            AnyProvider::Command(p) => p.invocation_telemetry(),
            AnyProvider::Llmlint(p) => p.invocation_telemetry(),
            AnyProvider::Split { skill, judges } => {
                let mut records = skill.invocation_telemetry();
                records.extend(judges.invocation_telemetry());
                records
            }
        }
    }

    fn spawned_processes(&self) -> Vec<crate::SpawnedProcess> {
        match self {
            AnyProvider::Oneharness(p) => p.spawned_processes(),
            AnyProvider::Command(p) => p.spawned_processes(),
            AnyProvider::Llmlint(p) => p.spawned_processes(),
            AnyProvider::Split { skill, judges } => {
                let mut records = skill.spawned_processes();
                records.extend(judges.spawned_processes());
                records
            }
        }
    }

    fn supervisor_control(&self) -> crate::ControlOutcome {
        match self {
            AnyProvider::Oneharness(p) => p.supervisor_control(),
            AnyProvider::Command(p) => p.supervisor_control(),
            AnyProvider::Llmlint(p) => p.supervisor_control(),
            AnyProvider::Split { judges, .. } => judges.supervisor_control(),
        }
    }

    fn take_judge_decisions(&self) -> Vec<crate::JudgeDecision> {
        match self {
            AnyProvider::Oneharness(p) => p.take_judge_decisions(),
            AnyProvider::Command(p) => p.take_judge_decisions(),
            AnyProvider::Llmlint(p) => p.take_judge_decisions(),
            AnyProvider::Split { judges, .. } => judges.take_judge_decisions(),
        }
    }

    // The skill side owns the controllable turn, exactly as
    // [`crate::SplitProvider`] decides it.
    fn control(&self) -> crate::ControlOutcome {
        match self {
            AnyProvider::Oneharness(p) => p.control(),
            AnyProvider::Command(p) => p.control(),
            AnyProvider::Llmlint(p) => p.control(),
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
            AnyProvider::Llmlint(p) => p.respond(skill, messages, session),
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
            AnyProvider::Llmlint(p) => p.respond_streaming(skill, messages, session, on_event),
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
            AnyProvider::Llmlint(p) => p.simulate_user(persona, messages, session),
            AnyProvider::Split { judges, .. } => judges.simulate_user(persona, messages, session),
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
            AnyProvider::Llmlint(p) => p.supervise(query, messages, session),
            AnyProvider::Split { judges, .. } => judges.supervise(query, messages, session),
        }
    }
    fn supervise_with_evidence(
        &self,
        query: &SupervisorQuery<'_>,
        messages: &[Message],
        session: Option<&str>,
        evidence: EvidenceContext<'_>,
    ) -> crate::Result<SupervisorTurn> {
        match self {
            AnyProvider::Oneharness(p) => {
                p.supervise_with_evidence(query, messages, session, evidence)
            }
            AnyProvider::Command(p) => {
                p.supervise_with_evidence(query, messages, session, evidence)
            }
            AnyProvider::Llmlint(p) => {
                p.supervise_with_evidence(query, messages, session, evidence)
            }
            AnyProvider::Split { judges, .. } => {
                judges.supervise_with_evidence(query, messages, session, evidence)
            }
        }
    }

    fn judge(&self, query: &JudgeQuery<'_>, messages: &[Message]) -> crate::Result<JudgeVerdict> {
        match self {
            AnyProvider::Oneharness(p) => p.judge(query, messages),
            AnyProvider::Command(p) => p.judge(query, messages),
            AnyProvider::Llmlint(p) => p.judge(query, messages),
            AnyProvider::Split { judges, .. } => judges.judge(query, messages),
        }
    }
    fn judge_with_evidence(
        &self,
        query: &JudgeQuery<'_>,
        messages: &[Message],
        evidence: EvidenceContext<'_>,
    ) -> crate::Result<JudgeVerdict> {
        match self {
            AnyProvider::Oneharness(p) => p.judge_with_evidence(query, messages, evidence),
            AnyProvider::Command(p) => p.judge_with_evidence(query, messages, evidence),
            AnyProvider::Llmlint(p) => p.judge_with_evidence(query, messages, evidence),
            AnyProvider::Split { judges, .. } => {
                judges.judge_with_evidence(query, messages, evidence)
            }
        }
    }

    fn assess(&self, prompt: &str, messages: &[Message]) -> crate::Result<Assessment> {
        match self {
            AnyProvider::Oneharness(p) => p.assess(prompt, messages),
            AnyProvider::Command(p) => p.assess(prompt, messages),
            AnyProvider::Llmlint(p) => p.assess(prompt, messages),
            AnyProvider::Split { judges, .. } => judges.assess(prompt, messages),
        }
    }
    fn assess_with_evidence(
        &self,
        prompt: &str,
        messages: &[Message],
        evidence: EvidenceContext<'_>,
    ) -> crate::Result<Assessment> {
        match self {
            AnyProvider::Oneharness(p) => p.assess_with_evidence(prompt, messages, evidence),
            AnyProvider::Command(p) => p.assess_with_evidence(prompt, messages, evidence),
            AnyProvider::Llmlint(p) => p.assess_with_evidence(prompt, messages, evidence),
            AnyProvider::Split { judges, .. } => {
                judges.assess_with_evidence(prompt, messages, evidence)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::JudgeSpec;
    use super::*;

    #[test]
    fn builds_oneharness_and_command_backends() {
        let oh = AnyProvider::build(&ProviderSpec::Oneharness {
            bin: Some("oneharness".into()),
            judge_config: Some("oneharness.judge.toml".into()),
            stream: false,
            control: false,
            mock_harness: Vec::new(),
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
                bin: Some("oneharness".into()),
                judge_config: None,
                stream: true,
                control: false,
                mock_harness: Vec::new(),
            }),
            judges: vec![
                JudgeSpec {
                    label: "command".into(),
                    provider: ProviderSpec::Command {
                        command: vec!["judge".into()],
                    },
                },
                JudgeSpec {
                    label: "reviewer".into(),
                    provider: ProviderSpec::Oneharness {
                        bin: None,
                        judge_config: None,
                        stream: false,
                        control: false,
                        mock_harness: Vec::new(),
                    },
                },
            ],
        };
        let provider = AnyProvider::build(&spec).unwrap();
        let AnyProvider::Split { judges, .. } = &provider else {
            panic!("a split");
        };
        // The panel carries each judge's label and kind, in list order.
        let named: Vec<(&str, &str)> = judges
            .judges()
            .iter()
            .map(|j| (j.label(), j.kind()))
            .collect();
        assert_eq!(named, [("command", "command"), ("reviewer", "oneharness")]);
        // A judge whose backend cannot be built fails the whole build.
        let broken = ProviderSpec::Split {
            skill: Box::new(ProviderSpec::Command {
                command: vec!["s".into()],
            }),
            judges: vec![JudgeSpec {
                label: "j".into(),
                provider: ProviderSpec::Command { command: vec![] },
            }],
        };
        assert!(matches!(
            AnyProvider::build(&broken),
            Err(CliError::Config(_))
        ));
    }
}
