//! [`AnyProvider`]: a runtime-dispatched [`Provider`] the CLI builds from a
//! validated [`ProviderSpec`]. The library's providers are static types
//! (`SplitProvider<S, J>` is generic, `ApiJudgeProvider<T>` is generic over its
//! transport), so the CLI — which picks a backend at runtime from YAML — needs one
//! concrete type that erases the choice. `AnyProvider` is that type: it owns each
//! backend as an enum variant and forwards every [`Provider`] method, dispatching
//! a `split` to its two children exactly as [`SplitProvider`] would.

use std::ops::ControlFlow;

use crate::{
    AssistantTurn, CommandProvider, JudgeQuery, JudgeVerdict, Message, OneharnessProvider,
    Provider, SkillRef, ToolEvent, UserTurn,
};

use super::config::ProviderSpec;
use super::CliError;

/// A [`Provider`] whose backend is chosen at runtime from a [`ProviderSpec`].
pub enum AnyProvider {
    /// The default oneharness backend.
    Oneharness(OneharnessProvider),
    /// A custom JSON-lines command backend.
    Command(CommandProvider),
    /// A direct model API (only when built with the `ureq-transport` feature).
    #[cfg(feature = "ureq-transport")]
    Api(crate::ApiJudgeProvider<crate::UreqTransport>),
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
    /// [`CliError::Config`] if the spec needs a capability this build lacks (an
    /// `api` backend without the `ureq-transport` feature) or a required
    /// environment variable (the API key) is missing.
    pub fn build(spec: &ProviderSpec) -> Result<Self, CliError> {
        match spec {
            ProviderSpec::Oneharness { bin, judge_harness } => Ok(AnyProvider::Oneharness(
                OneharnessProvider::new()
                    .with_bin(bin)
                    .with_judge_harness(judge_harness),
            )),
            ProviderSpec::Command { command } => {
                let provider = CommandProvider::new(command.clone())
                    .map_err(|e| CliError::Config(e.to_string()))?;
                Ok(AnyProvider::Command(provider))
            }
            ProviderSpec::Api {
                vendor,
                base_url,
                max_tokens,
            } => build_api(*vendor, base_url.as_deref(), *max_tokens),
            ProviderSpec::Split { skill, judge } => Ok(AnyProvider::Split {
                skill: Box::new(AnyProvider::build(skill)?),
                judge: Box::new(AnyProvider::build(judge)?),
            }),
        }
    }
}

#[cfg(feature = "ureq-transport")]
fn build_api(
    vendor: crate::ApiVendor,
    base_url: Option<&str>,
    max_tokens: Option<u32>,
) -> Result<AnyProvider, CliError> {
    use crate::{ApiJudgeProvider, ApiVendor};

    let (env, label) = match vendor {
        ApiVendor::Anthropic => ("ANTHROPIC_API_KEY", "anthropic"),
        ApiVendor::OpenAI => ("OPENAI_API_KEY", "openai"),
    };
    let key = std::env::var(env).map_err(|_| {
        CliError::Config(format!(
            "provider kind `api` (vendor `{label}`) needs the {env} environment variable"
        ))
    })?;
    let mut provider = ApiJudgeProvider::new(vendor, key, crate::UreqTransport::new());
    if let Some(url) = base_url {
        provider = provider.with_base_url(url);
    }
    if let Some(max) = max_tokens {
        provider = provider.with_max_tokens(max);
    }
    Ok(AnyProvider::Api(provider))
}

#[cfg(not(feature = "ureq-transport"))]
fn build_api(
    _vendor: crate::ApiVendor,
    _base_url: Option<&str>,
    _max_tokens: Option<u32>,
) -> Result<AnyProvider, CliError> {
    Err(CliError::Config(
        "provider kind `api` needs the bundled HTTP client: rebuild with \
         `--features cli,ureq-transport` (or supply your own via the library)"
            .into(),
    ))
}

impl Provider for AnyProvider {
    fn respond(
        &self,
        platform: &str,
        model: &str,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> crate::Result<AssistantTurn> {
        match self {
            AnyProvider::Oneharness(p) => p.respond(platform, model, skill, messages, session),
            AnyProvider::Command(p) => p.respond(platform, model, skill, messages, session),
            #[cfg(feature = "ureq-transport")]
            AnyProvider::Api(p) => p.respond(platform, model, skill, messages, session),
            AnyProvider::Split { skill: s, .. } => {
                s.respond(platform, model, skill, messages, session)
            }
        }
    }

    fn respond_streaming(
        &self,
        platform: &str,
        model: &str,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
        on_event: &mut dyn FnMut(&ToolEvent) -> ControlFlow<()>,
    ) -> crate::Result<AssistantTurn> {
        match self {
            AnyProvider::Oneharness(p) => {
                p.respond_streaming(platform, model, skill, messages, session, on_event)
            }
            AnyProvider::Command(p) => {
                p.respond_streaming(platform, model, skill, messages, session, on_event)
            }
            #[cfg(feature = "ureq-transport")]
            AnyProvider::Api(p) => {
                p.respond_streaming(platform, model, skill, messages, session, on_event)
            }
            AnyProvider::Split { skill: s, .. } => {
                s.respond_streaming(platform, model, skill, messages, session, on_event)
            }
        }
    }

    fn simulate_user(
        &self,
        model: &str,
        persona: &str,
        messages: &[Message],
        session: Option<&str>,
    ) -> crate::Result<UserTurn> {
        match self {
            AnyProvider::Oneharness(p) => p.simulate_user(model, persona, messages, session),
            AnyProvider::Command(p) => p.simulate_user(model, persona, messages, session),
            #[cfg(feature = "ureq-transport")]
            AnyProvider::Api(p) => p.simulate_user(model, persona, messages, session),
            AnyProvider::Split { judge, .. } => {
                judge.simulate_user(model, persona, messages, session)
            }
        }
    }

    fn judge(
        &self,
        model: &str,
        query: &JudgeQuery<'_>,
        messages: &[Message],
    ) -> crate::Result<JudgeVerdict> {
        match self {
            AnyProvider::Oneharness(p) => p.judge(model, query, messages),
            AnyProvider::Command(p) => p.judge(model, query, messages),
            #[cfg(feature = "ureq-transport")]
            AnyProvider::Api(p) => p.judge(model, query, messages),
            AnyProvider::Split { judge, .. } => judge.judge(model, query, messages),
        }
    }

    fn session_capable(&self, platform: &str) -> bool {
        match self {
            AnyProvider::Oneharness(p) => p.session_capable(platform),
            AnyProvider::Command(p) => p.session_capable(platform),
            #[cfg(feature = "ureq-transport")]
            AnyProvider::Api(p) => p.session_capable(platform),
            // Session continuation is the skill backend's concern, exactly as
            // `SplitProvider` mirrors it.
            AnyProvider::Split { skill, .. } => skill.session_capable(platform),
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
            judge_harness: "claude-code".into(),
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
    fn build_split_composes_children_and_mirrors_skill_session() {
        let spec = ProviderSpec::Split {
            skill: Box::new(ProviderSpec::Oneharness {
                bin: "oneharness".into(),
                judge_harness: "claude-code".into(),
            }),
            judge: Box::new(ProviderSpec::Command {
                command: vec!["judge".into()],
            }),
        };
        let provider = AnyProvider::build(&spec).unwrap();
        assert!(matches!(provider, AnyProvider::Split { .. }));
        // The oneharness skill backend is session-capable on claude-code; the
        // command judge backend is not — the split mirrors the skill.
        assert!(provider.session_capable("claude-code"));
        assert!(!provider.session_capable("goose"));
    }

    #[cfg(not(feature = "ureq-transport"))]
    #[test]
    fn api_without_transport_is_a_clear_error() {
        let result = AnyProvider::build(&ProviderSpec::Api {
            vendor: crate::ApiVendor::Anthropic,
            base_url: None,
            max_tokens: None,
        });
        assert!(matches!(result, Err(CliError::Config(m)) if m.contains("ureq-transport")));
    }
}
