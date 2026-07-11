//! The YAML config surface for the `onejudge` CLI, and the typed plan it resolves
//! into. Every external input is validated at this boundary: the file is parsed
//! into strict serde models (`deny_unknown_fields`), overrides from flags are
//! merged (flags win over file, file wins over defaults), and the result is
//! validated into a [`Plan`] the run driver executes. A malformed config is a
//! loud, actionable error, never a silent default.

use serde::Deserialize;

use crate::{ApiVendor, Conversation, JudgeKind, Settings, SimulatedUser, Skill};

use super::CliError;

/// The whole YAML config for one run. Field defaults let a minimal file (just a
/// `task` and an `agent`) work, while `deny_unknown_fields` makes a typo'd key a
/// hard error instead of a silently-ignored setting.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Which backend runs the harness (and judges / plays the user).
    #[serde(default)]
    pub provider: ProviderConfig,
    /// The harness (platform) the agent runs on; defaults to `claude-code`.
    #[serde(default)]
    pub harness: Option<String>,
    /// The model the agent runs on; empty / omitted means the harness default.
    #[serde(default)]
    pub model: Option<String>,
    /// The model the simulated user and judge run on; defaults to `model`.
    #[serde(default)]
    pub judge_model: Option<String>,
    /// The agent's system framing (name, working dir, instructions).
    #[serde(default)]
    pub agent: AgentConfig,
    /// The task to drive to completion. May instead be supplied by `--task`
    /// (`-` reads stdin); required by the time the plan is built.
    #[serde(default)]
    pub task: Option<String>,
    /// The simulated user / supervisor that drives the loop. Omit for a
    /// single-turn run (the agent answers once).
    #[serde(default)]
    pub user: Option<UserConfig>,
    /// The caller-owned session name threaded across turns; defaults to
    /// `onejudge`.
    #[serde(default)]
    pub session: Option<String>,
    /// Optional criteria to score the finished transcript with.
    #[serde(default)]
    pub evals: Vec<EvalConfig>,
}

/// The agent under test: how it is framed to the harness.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// A short name for the agent.
    #[serde(default = "default_agent_name")]
    pub name: String,
    /// The agent's working directory.
    #[serde(default = "default_agent_dir")]
    pub dir: String,
    /// The system prompt delivered to the harness.
    #[serde(default)]
    pub instructions: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: default_agent_name(),
            dir: default_agent_dir(),
            instructions: String::new(),
        }
    }
}

/// The simulated user that supervises the agent and drives the loop.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    /// How the simulated user behaves (their instructions).
    #[serde(default)]
    pub persona: String,
    /// A plain-English completion condition; when the judge decides it holds, the
    /// loop ends. Without it the loop ends at `max_turns` or when the agent
    /// declares itself done.
    #[serde(default)]
    pub done_when: Option<String>,
    /// The assistant-turn cap for this run.
    #[serde(default)]
    pub max_turns: Option<u32>,
}

/// One eval scored against the finished transcript.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalConfig {
    /// The plain-English criterion.
    pub criterion: String,
    /// Boolean (a pass/fail verdict) or numeric (a score on a scale).
    #[serde(default = "default_eval_kind")]
    pub kind: JudgeKind,
    /// The inclusive `[min, max]` scale for a numeric eval; ignored (and rejected)
    /// for a boolean one. Defaults to `[0, 10]`.
    #[serde(default)]
    pub scale: Option<[f64; 2]>,
}

/// Which backend runs the harness. A flat, strict struct (rather than an
/// internally-tagged enum, which serde cannot pair with `deny_unknown_fields`):
/// [`ProviderConfig::resolve`] checks that only the chosen `kind`'s fields are set
/// and turns it into a validated [`ProviderSpec`], so a misplaced-but-spelled-right
/// key (e.g. `bin` under `kind: command`) is still a loud error.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// `oneharness` (default) | `command` | `api` | `split`.
    #[serde(default)]
    pub kind: ProviderKind,
    /// `oneharness`: the `oneharness` binary (default `oneharness`).
    #[serde(default)]
    pub bin: Option<String>,
    /// `oneharness`: the harness the judge / simulated user run on.
    #[serde(default)]
    pub judge_harness: Option<String>,
    /// `command`: the provider argv (program + args).
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// `api`: the vendor (`anthropic` | `openai`); the key comes from the env.
    #[serde(default)]
    pub vendor: Option<ApiVendorConfig>,
    /// `api`: override the vendor's base URL (a proxy / gateway).
    #[serde(default)]
    pub base_url: Option<String>,
    /// `api`: the per-completion `max_tokens` cap.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// `split`: the backend that runs the agent's turns.
    #[serde(default)]
    pub skill: Option<Box<ProviderConfig>>,
    /// `split`: the backend that judges and plays the simulated user.
    #[serde(default)]
    pub judge: Option<Box<ProviderConfig>>,
}

/// The provider backends the CLI can build. One enum is the single source for
/// both the YAML `kind:` (via `Deserialize`) and the `--provider` flag (via
/// clap's `ValueEnum`), so the two surfaces cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Shell out to the `oneharness` CLI (the default).
    #[default]
    Oneharness,
    /// A custom command speaking the JSON-lines protocol.
    Command,
    /// A direct Anthropic / OpenAI API, no harness.
    Api,
    /// Compose a skill-runner with a separate judge / simulated-user backend.
    Split,
}

/// The API vendor, as spelled in YAML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiVendorConfig {
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Chat Completions API.
    Openai,
}

impl From<ApiVendorConfig> for ApiVendor {
    fn from(v: ApiVendorConfig) -> Self {
        match v {
            ApiVendorConfig::Anthropic => ApiVendor::Anthropic,
            ApiVendorConfig::Openai => ApiVendor::OpenAI,
        }
    }
}

// --- Overrides (flags win over file) --------------------------------------

/// The subset of config a command-line flag can override. Every field is
/// optional; a `Some` wins over whatever the file (or a default) provided.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    /// `--harness`.
    pub harness: Option<String>,
    /// `--model`.
    pub model: Option<String>,
    /// `--judge-model`.
    pub judge_model: Option<String>,
    /// `--task` (already resolved; `-`/stdin handled by the caller).
    pub task: Option<String>,
    /// `--persona`.
    pub persona: Option<String>,
    /// `--done-when`.
    pub done_when: Option<String>,
    /// `--max-turns`.
    pub max_turns: Option<u32>,
    /// `--session`.
    pub session: Option<String>,
    /// `--provider` (override just the backend kind).
    pub provider_kind: Option<ProviderKind>,
}

impl Config {
    /// Parse a config from YAML text.
    ///
    /// # Errors
    /// [`CliError::Config`] if the YAML is malformed or carries an unknown key.
    pub fn from_yaml(text: &str) -> Result<Self, CliError> {
        serde_yaml_ng::from_str(text)
            .map_err(|e| CliError::Config(format!("could not parse config: {e}")))
    }

    /// Apply command-line overrides in place (flags win over file).
    pub fn apply(&mut self, overrides: Overrides) {
        let Overrides {
            harness,
            model,
            judge_model,
            task,
            persona,
            done_when,
            max_turns,
            session,
            provider_kind,
        } = overrides;
        if harness.is_some() {
            self.harness = harness;
        }
        if model.is_some() {
            self.model = model;
        }
        if judge_model.is_some() {
            self.judge_model = judge_model;
        }
        if task.is_some() {
            self.task = task;
        }
        if session.is_some() {
            self.session = session;
        }
        if let Some(kind) = provider_kind {
            self.provider.kind = kind;
        }
        // The user-facing overrides imply a simulated user even if the file had
        // none — supplying `--persona` / `--done-when` / `--max-turns` on the CLI
        // is a request to drive the loop.
        if persona.is_some() || done_when.is_some() || max_turns.is_some() {
            let user = self.user.get_or_insert_with(UserConfig::default);
            if persona.is_some() {
                user.persona = persona.unwrap_or_default();
            }
            if done_when.is_some() {
                user.done_when = done_when;
            }
            if max_turns.is_some() {
                user.max_turns = max_turns;
            }
        }
    }

    /// Validate and resolve the config into an executable [`Plan`].
    ///
    /// # Errors
    /// [`CliError::Config`] if the task is missing, the provider is inconsistent,
    /// or an eval is malformed.
    pub fn into_plan(self) -> Result<Plan, CliError> {
        let task = self.task.filter(|t| !t.trim().is_empty()).ok_or_else(|| {
            CliError::Config(
                "no task given: set `task:` in the config or pass `--task` (`-` for stdin)".into(),
            )
        })?;

        let provider = self.provider.resolve()?;

        let platform = self.harness.unwrap_or_else(default_harness);
        let model = self.model.unwrap_or_default();
        // The judge / simulated user default to the same model as the agent.
        let judge_model = self
            .judge_model
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| model.clone());

        let mut settings = Settings::new(platform, model, judge_model);
        if let Some(session) = self.session.filter(|s| !s.is_empty()) {
            settings = settings.with_session_name(session);
        }

        let skill = Skill::new(self.agent.name, self.agent.dir, self.agent.instructions);

        let (conversation, done_when) = match self.user {
            Some(u) => {
                if let Some(turns) = u.max_turns {
                    settings = settings.with_max_turns(turns);
                }
                let mut sim = SimulatedUser::new(u.persona);
                if let Some(dw) = &u.done_when {
                    sim = sim.done_when(dw.clone());
                }
                if let Some(turns) = u.max_turns {
                    sim = sim.max_turns(turns);
                }
                (Conversation::multi_turn(skill, task, sim), u.done_when)
            }
            None => (Conversation::single_turn(skill, task), None),
        };

        let evals = self
            .evals
            .into_iter()
            .map(EvalConfig::resolve)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Plan {
            provider,
            settings,
            conversation,
            evals,
            done_when,
        })
    }
}

impl ProviderConfig {
    /// Validate the flat config into a typed [`ProviderSpec`], rejecting fields
    /// that do not belong to the chosen `kind`.
    fn resolve(self) -> Result<ProviderSpec, CliError> {
        let ProviderConfig {
            kind,
            bin,
            judge_harness,
            command,
            vendor,
            base_url,
            max_tokens,
            skill,
            judge,
        } = self;

        // Which fields belong to which kind; anything else set is an error.
        let reject = |present: bool, field: &str| -> Result<(), CliError> {
            if present {
                Err(CliError::Config(format!(
                    "`{field}` is not valid under provider kind `{}`",
                    kind.as_str()
                )))
            } else {
                Ok(())
            }
        };

        match kind {
            ProviderKind::Oneharness => {
                reject(command.is_some(), "command")?;
                reject(vendor.is_some(), "vendor")?;
                reject(base_url.is_some(), "base_url")?;
                reject(max_tokens.is_some(), "max_tokens")?;
                reject(skill.is_some(), "skill")?;
                reject(judge.is_some(), "judge")?;
                Ok(ProviderSpec::Oneharness {
                    bin: bin.unwrap_or_else(|| "oneharness".into()),
                    judge_harness: judge_harness.unwrap_or_else(|| "claude-code".into()),
                })
            }
            ProviderKind::Command => {
                reject(bin.is_some(), "bin")?;
                reject(judge_harness.is_some(), "judge_harness")?;
                reject(vendor.is_some(), "vendor")?;
                reject(base_url.is_some(), "base_url")?;
                reject(max_tokens.is_some(), "max_tokens")?;
                reject(skill.is_some(), "skill")?;
                reject(judge.is_some(), "judge")?;
                let command = command.filter(|c| !c.is_empty()).ok_or_else(|| {
                    CliError::Config("provider kind `command` needs a non-empty `command`".into())
                })?;
                Ok(ProviderSpec::Command { command })
            }
            ProviderKind::Api => {
                reject(bin.is_some(), "bin")?;
                reject(judge_harness.is_some(), "judge_harness")?;
                reject(command.is_some(), "command")?;
                reject(skill.is_some(), "skill")?;
                reject(judge.is_some(), "judge")?;
                let vendor = vendor.ok_or_else(|| {
                    CliError::Config(
                        "provider kind `api` needs a `vendor` (anthropic | openai)".into(),
                    )
                })?;
                Ok(ProviderSpec::Api {
                    vendor: vendor.into(),
                    base_url,
                    max_tokens,
                })
            }
            ProviderKind::Split => {
                reject(bin.is_some(), "bin")?;
                reject(judge_harness.is_some(), "judge_harness")?;
                reject(command.is_some(), "command")?;
                reject(vendor.is_some(), "vendor")?;
                reject(base_url.is_some(), "base_url")?;
                reject(max_tokens.is_some(), "max_tokens")?;
                let skill = skill.ok_or_else(|| {
                    CliError::Config("provider kind `split` needs a `skill` provider".into())
                })?;
                let judge = judge.ok_or_else(|| {
                    CliError::Config("provider kind `split` needs a `judge` provider".into())
                })?;
                Ok(ProviderSpec::Split {
                    skill: Box::new(skill.resolve()?),
                    judge: Box::new(judge.resolve()?),
                })
            }
        }
    }
}

impl ProviderKind {
    /// The stable YAML spelling.
    fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Oneharness => "oneharness",
            ProviderKind::Command => "command",
            ProviderKind::Api => "api",
            ProviderKind::Split => "split",
        }
    }
}

impl EvalConfig {
    fn resolve(self) -> Result<Eval, CliError> {
        match self.kind {
            JudgeKind::Boolean => {
                if self.scale.is_some() {
                    return Err(CliError::Config(format!(
                        "eval `{}` is boolean but has a `scale` (only numeric evals take one)",
                        self.criterion
                    )));
                }
                Ok(Eval {
                    criterion: self.criterion,
                    kind: EvalKind::Boolean,
                })
            }
            JudgeKind::Numeric => {
                let [min, max] = self.scale.unwrap_or([0.0, 10.0]);
                if min > max {
                    return Err(CliError::Config(format!(
                        "eval `{}` has scale min ({min}) greater than max ({max})",
                        self.criterion
                    )));
                }
                Ok(Eval {
                    criterion: self.criterion,
                    kind: EvalKind::Numeric { scale: (min, max) },
                })
            }
        }
    }
}

// --- The resolved, executable plan ----------------------------------------

/// A validated provider backend, ready to build.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderSpec {
    /// Shell out to `oneharness`.
    Oneharness {
        /// The `oneharness` binary path.
        bin: String,
        /// The harness the judge / simulated user run on.
        judge_harness: String,
    },
    /// A custom command speaking the JSON-lines protocol.
    Command {
        /// The provider argv.
        command: Vec<String>,
    },
    /// A direct model API.
    Api {
        /// The vendor.
        vendor: ApiVendor,
        /// An optional base-URL override.
        base_url: Option<String>,
        /// An optional `max_tokens` cap.
        max_tokens: Option<u32>,
    },
    /// A composed skill-runner + judge backend.
    Split {
        /// Runs the agent's turns.
        skill: Box<ProviderSpec>,
        /// Judges and plays the simulated user.
        judge: Box<ProviderSpec>,
    },
}

/// The kind of a resolved eval, carrying only the data that kind needs — so a
/// boolean eval cannot hold a scale (unlike a `kind` + always-present `scale`
/// pair, where the boolean-with-a-scale combination is representable).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EvalKind {
    /// A yes/no verdict.
    Boolean,
    /// A score on an inclusive `(min, max)` scale.
    Numeric {
        /// The inclusive scale bounds.
        scale: (f64, f64),
    },
}

/// One resolved eval: the criterion and its kind (with a scale only for numeric).
#[derive(Debug, Clone, PartialEq)]
pub struct Eval {
    /// The plain-English criterion.
    pub criterion: String,
    /// Boolean, or numeric with its scale.
    pub kind: EvalKind,
}

/// Everything the run driver needs, resolved and validated from the config.
#[derive(Debug)]
pub struct Plan {
    /// The provider backend to build.
    pub provider: ProviderSpec,
    /// Engine settings (platform, models, session, turn cap).
    pub settings: Settings,
    /// The conversation to drive.
    pub conversation: Conversation,
    /// The evals to score afterward.
    pub evals: Vec<Eval>,
    /// The completion condition, if any — re-judged at the end to decide whether
    /// the task actually completed (which drives the exit code).
    pub done_when: Option<String>,
}

fn default_harness() -> String {
    "claude-code".into()
}
fn default_agent_name() -> String {
    "agent".into()
}
fn default_agent_dir() -> String {
    ".".into()
}
fn default_eval_kind() -> JudgeKind {
    JudgeKind::Boolean
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_resolves_to_a_single_turn_plan() {
        let cfg = Config::from_yaml("task: do the thing\n").unwrap();
        let plan = cfg.into_plan().unwrap();
        assert_eq!(plan.settings.platform, "claude-code");
        assert!(plan.done_when.is_none());
        assert!(plan.conversation.user.is_none());
        assert!(matches!(plan.provider, ProviderSpec::Oneharness { .. }));
    }

    #[test]
    fn unknown_top_level_key_is_a_loud_error() {
        let err = Config::from_yaml("task: x\nnope: 1\n").unwrap_err();
        assert!(matches!(err, CliError::Config(_)));
    }

    #[test]
    fn missing_task_is_rejected() {
        let err = Config::from_yaml("harness: codex\n")
            .unwrap()
            .into_plan()
            .unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("no task")));
    }

    #[test]
    fn user_block_builds_a_multi_turn_plan_with_done_when() {
        let yaml = r#"
task: refactor it
user:
  persona: a demanding lead
  done_when: tests pass
  max_turns: 5
"#;
        let plan = Config::from_yaml(yaml).unwrap().into_plan().unwrap();
        assert_eq!(plan.done_when.as_deref(), Some("tests pass"));
        assert_eq!(plan.settings.max_turns, 5);
        let user = plan.conversation.user.unwrap();
        assert_eq!(user.max_turns, Some(5));
        assert_eq!(user.done_when.as_deref(), Some("tests pass"));
    }

    #[test]
    fn overrides_win_over_file_and_imply_a_user() {
        let mut cfg = Config::from_yaml("task: from file\nharness: codex\n").unwrap();
        cfg.apply(Overrides {
            task: Some("from flag".into()),
            harness: Some("opencode".into()),
            done_when: Some("it is done".into()),
            max_turns: Some(3),
            ..Overrides::default()
        });
        let plan = cfg.into_plan().unwrap();
        assert_eq!(plan.settings.platform, "opencode");
        assert_eq!(plan.conversation.input, "from flag");
        assert_eq!(plan.done_when.as_deref(), Some("it is done"));
        assert_eq!(plan.settings.max_turns, 3);
    }

    #[test]
    fn judge_model_defaults_to_model() {
        let plan = Config::from_yaml("task: x\nmodel: opus-x\n")
            .unwrap()
            .into_plan()
            .unwrap();
        assert_eq!(plan.settings.model, "opus-x");
        assert_eq!(plan.settings.judge_model, "opus-x");
    }

    #[test]
    fn overrides_apply_model_judge_session_and_persona() {
        let mut cfg = Config::from_yaml("task: t\n").unwrap();
        cfg.apply(Overrides {
            model: Some("m1".into()),
            judge_model: Some("m2".into()),
            session: Some("sess-9".into()),
            persona: Some("a reviewer".into()),
            ..Overrides::default()
        });
        let plan = cfg.into_plan().unwrap();
        assert_eq!(plan.settings.model, "m1");
        assert_eq!(plan.settings.judge_model, "m2");
        // The session override threads a with_session_name; a multi-turn (persona)
        // conversation was implied by the persona flag.
        assert!(plan.conversation.user.is_some());
        assert_eq!(plan.conversation.user.unwrap().persona, "a reviewer");
    }

    #[test]
    fn split_requires_both_a_skill_and_a_judge() {
        let only_skill = "task: x\nprovider:\n  kind: split\n  skill:\n    kind: oneharness\n";
        let err = Config::from_yaml(only_skill)
            .unwrap()
            .into_plan()
            .unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("judge")));

        let only_judge = "task: x\nprovider:\n  kind: split\n  judge:\n    kind: oneharness\n";
        let err = Config::from_yaml(only_judge)
            .unwrap()
            .into_plan()
            .unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("skill")));
    }

    #[test]
    fn api_anthropic_vendor_maps_through() {
        let plan = Config::from_yaml("task: x\nprovider:\n  kind: api\n  vendor: anthropic\n")
            .unwrap()
            .into_plan()
            .unwrap();
        assert!(matches!(
            plan.provider,
            ProviderSpec::Api {
                vendor: ApiVendor::Anthropic,
                ..
            }
        ));
    }

    #[test]
    fn command_provider_requires_argv() {
        let err = Config::from_yaml("task: x\nprovider:\n  kind: command\n")
            .unwrap()
            .into_plan()
            .unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("command")));

        let plan = Config::from_yaml(
            "task: x\nprovider:\n  kind: command\n  command: [my-prov, --flag]\n",
        )
        .unwrap()
        .into_plan()
        .unwrap();
        assert!(
            matches!(plan.provider, ProviderSpec::Command { command } if command == ["my-prov", "--flag"])
        );
    }

    #[test]
    fn misplaced_field_for_kind_is_rejected() {
        // `bin` belongs to oneharness, not command.
        let err =
            Config::from_yaml("task: x\nprovider:\n  kind: command\n  command: [p]\n  bin: nope\n")
                .unwrap()
                .into_plan()
                .unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("bin")));
    }

    #[test]
    fn api_provider_needs_a_vendor() {
        let err = Config::from_yaml("task: x\nprovider:\n  kind: api\n")
            .unwrap()
            .into_plan()
            .unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("vendor")));

        let plan = Config::from_yaml("task: x\nprovider:\n  kind: api\n  vendor: openai\n")
            .unwrap()
            .into_plan()
            .unwrap();
        assert!(matches!(
            plan.provider,
            ProviderSpec::Api {
                vendor: ApiVendor::OpenAI,
                ..
            }
        ));
    }

    #[test]
    fn split_provider_composes_two_backends() {
        let yaml = r#"
task: x
provider:
  kind: split
  skill:
    kind: oneharness
  judge:
    kind: command
    command: [judge-prov]
"#;
        let plan = Config::from_yaml(yaml).unwrap().into_plan().unwrap();
        match plan.provider {
            ProviderSpec::Split { skill, judge } => {
                assert!(matches!(*skill, ProviderSpec::Oneharness { .. }));
                assert!(matches!(*judge, ProviderSpec::Command { .. }));
            }
            other => panic!("expected split, got {other:?}"),
        }
    }

    #[test]
    fn provider_override_changes_only_the_kind() {
        let mut cfg =
            Config::from_yaml("task: x\nprovider:\n  kind: oneharness\n  bin: custom\n").unwrap();
        cfg.apply(Overrides {
            provider_kind: Some(ProviderKind::Oneharness),
            ..Overrides::default()
        });
        let plan = cfg.into_plan().unwrap();
        assert!(matches!(plan.provider, ProviderSpec::Oneharness { bin, .. } if bin == "custom"));
    }

    #[test]
    fn boolean_eval_rejects_a_scale() {
        let err = Config::from_yaml(
            "task: x\nevals:\n  - criterion: it works\n    kind: boolean\n    scale: [1, 5]\n",
        )
        .unwrap()
        .into_plan()
        .unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("scale")));
    }

    #[test]
    fn numeric_eval_default_and_custom_scale() {
        let plan = Config::from_yaml(
            "task: x\nevals:\n  - criterion: quality\n    kind: numeric\n  - criterion: depth\n    kind: numeric\n    scale: [1, 5]\n",
        )
        .unwrap()
        .into_plan()
        .unwrap();
        assert_eq!(plan.evals[0].kind, EvalKind::Numeric { scale: (0.0, 10.0) });
        assert_eq!(plan.evals[1].kind, EvalKind::Numeric { scale: (1.0, 5.0) });
    }

    #[test]
    fn numeric_eval_rejects_inverted_scale() {
        let err = Config::from_yaml(
            "task: x\nevals:\n  - criterion: q\n    kind: numeric\n    scale: [5, 1]\n",
        )
        .unwrap()
        .into_plan()
        .unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("greater than max")));
    }

    #[test]
    fn default_boolean_eval_kind() {
        let plan = Config::from_yaml("task: x\nevals:\n  - criterion: it works\n")
            .unwrap()
            .into_plan()
            .unwrap();
        assert_eq!(plan.evals[0].kind, EvalKind::Boolean);
    }
}
