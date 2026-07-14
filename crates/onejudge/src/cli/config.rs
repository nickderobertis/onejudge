//! The YAML config surface for the `onejudge` CLI, and the typed plan it resolves
//! into. Every external input is validated at this boundary: the file is parsed
//! into strict serde models (`deny_unknown_fields`), overrides from the
//! `ONEJUDGE_*` environment and the flags are merged (flags win over env, env
//! wins over file, file wins over defaults), and the result is validated into a
//! [`Plan`] the run driver executes. A malformed config — or an invalid env
//! override — is a loud, actionable error, never a silent default.

use std::path::PathBuf;

use serde::Deserialize;

use crate::{Conversation, JudgeKind, Settings, SimulatedUser, Skill};

use super::CliError;

/// The whole YAML config for one run. Field defaults let a minimal file (just a
/// `task`) work, while `deny_unknown_fields` makes a typo'd key a hard error
/// instead of a silently-ignored setting.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Which backend runs the harness (and judges / plays the user).
    ///
    /// Harness/model **selection** is no longer a onejudge concern: the agent side
    /// uses oneharness's discovered `oneharness.toml`, and the judge side uses the
    /// `provider.judge_config` file (default `oneharness.judge.toml`). Scaffold both
    /// with `onejudge init`.
    #[serde(default)]
    pub provider: ProviderConfig,
    /// Path to a **skill** directory (containing a `SKILL.md`) whose instruction
    /// body seeds the system prompt. Resolved relative to the config file's
    /// directory (or the working dir for a flag-only run). Optional — combine it
    /// with `system_prompt`, use either alone, or neither.
    #[serde(default)]
    pub skill: Option<PathBuf>,
    /// Extra system-prompt text for the harness. When a `skill` is also set, this
    /// comes **first** and the skill's body is appended after it.
    #[serde(default)]
    pub system_prompt: Option<String>,
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
    /// Optional prompt for a free-text judgement of the finished transcript.
    #[serde(default)]
    pub assessment: Option<String>,
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
    /// `oneharness` (default) | `command` | `split`.
    #[serde(default)]
    pub kind: ProviderKind,
    /// `oneharness`: the `oneharness` binary (default `oneharness`).
    #[serde(default)]
    pub bin: Option<String>,
    /// `oneharness`: the oneharness config file the judge / simulated user run
    /// under, passed as `oneharness run --config <path>` (default
    /// `oneharness.judge.toml`). This is where the judge-side harness/model
    /// selection lives.
    #[serde(default)]
    pub judge_config: Option<String>,
    /// `command`: the provider argv (program + args).
    #[serde(default)]
    pub command: Option<Vec<String>>,
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
    /// Compose a skill-runner with a separate judge / simulated-user backend.
    Split,
}

// --- Overrides (flags win over env, env wins over file) --------------------

/// The subset of config a command-line flag — or the matching `ONEJUDGE_*`
/// environment variable — can override. Every field is optional; a `Some` wins
/// over whatever the lower tier (env, then file, then a default) provided.
///
/// The same struct expresses both the flag layer and the env layer: the run
/// applies the env-derived overrides first, then the flag-derived ones, so a
/// flag beats the env, and both beat the file (see [`Overrides::from_env`]).
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    /// `--judge-config` / `ONEJUDGE_JUDGE_CONFIG` (the judge-side oneharness
    /// config path).
    pub judge_config: Option<String>,
    /// `--skill` / `ONEJUDGE_SKILL` (a skill directory; resolved relative to the
    /// working dir).
    pub skill: Option<PathBuf>,
    /// `--system-prompt` / `ONEJUDGE_SYSTEM_PROMPT` (extra system-prompt text).
    pub system_prompt: Option<String>,
    /// `--task` / `ONEJUDGE_TASK` (the flag's `-`/stdin form is resolved by the
    /// caller; the env value is always literal).
    pub task: Option<String>,
    /// `--persona` / `ONEJUDGE_PERSONA`.
    pub persona: Option<String>,
    /// `--done-when` / `ONEJUDGE_DONE_WHEN`.
    pub done_when: Option<String>,
    /// `--max-turns` / `ONEJUDGE_MAX_TURNS`.
    pub max_turns: Option<u32>,
    /// `--session` / `ONEJUDGE_SESSION`.
    pub session: Option<String>,
    /// `--provider` / `ONEJUDGE_PROVIDER` (override just the backend kind).
    pub provider_kind: Option<ProviderKind>,
}

impl Overrides {
    /// Read overrides from the `ONEJUDGE_*` environment, looking each variable up
    /// through `getenv` (real runs pass `std::env::var(..).ok()`). This is the
    /// middle precedence tier — a set variable wins over the config file, and a
    /// flag in turn wins over it. An empty value is treated as absent, so an
    /// exported-but-blank variable never forces an empty override.
    ///
    /// The env surface mirrors the flags one-for-one: `ONEJUDGE_<FLAG>` in
    /// upper-snake-case (`--judge-config` → `ONEJUDGE_JUDGE_CONFIG`, and so on).
    ///
    /// # Errors
    /// [`CliError::Config`] if `ONEJUDGE_MAX_TURNS` is not a non-negative integer
    /// or `ONEJUDGE_PROVIDER` is not a known backend kind — an invalid override is
    /// a loud error at the boundary, never silently ignored.
    pub fn from_env(getenv: impl Fn(&str) -> Option<String>) -> Result<Self, CliError> {
        let get = |key: &str| getenv(key).filter(|v| !v.is_empty());

        let max_turns = match get("ONEJUDGE_MAX_TURNS") {
            Some(v) => Some(v.parse::<u32>().map_err(|_| {
                CliError::Config(format!(
                    "ONEJUDGE_MAX_TURNS must be a non-negative integer, got `{v}`"
                ))
            })?),
            None => None,
        };

        let provider_kind = match get("ONEJUDGE_PROVIDER") {
            Some(v) => Some(
                <ProviderKind as clap::ValueEnum>::from_str(&v, true).map_err(|_| {
                    CliError::Config(format!(
                        "ONEJUDGE_PROVIDER must be `oneharness`, `command`, or `split`, got `{v}`"
                    ))
                })?,
            ),
            None => None,
        };

        Ok(Self {
            judge_config: get("ONEJUDGE_JUDGE_CONFIG"),
            skill: get("ONEJUDGE_SKILL").map(PathBuf::from),
            system_prompt: get("ONEJUDGE_SYSTEM_PROMPT"),
            task: get("ONEJUDGE_TASK"),
            persona: get("ONEJUDGE_PERSONA"),
            done_when: get("ONEJUDGE_DONE_WHEN"),
            max_turns,
            session: get("ONEJUDGE_SESSION"),
            provider_kind,
        })
    }
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

    /// Apply a layer of overrides in place; a `Some` field wins over the current
    /// value. The run applies the env layer first, then the flag layer, giving
    /// flags > env > file > defaults.
    pub fn apply(&mut self, overrides: Overrides) {
        let Overrides {
            judge_config,
            skill,
            system_prompt,
            task,
            persona,
            done_when,
            max_turns,
            session,
            provider_kind,
        } = overrides;
        if judge_config.is_some() {
            self.provider.judge_config = judge_config;
        }
        if skill.is_some() {
            self.skill = skill;
        }
        if system_prompt.is_some() {
            self.system_prompt = system_prompt;
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

        let mut settings = Settings::new();
        if let Some(session) = self.session.filter(|s| !s.is_empty()) {
            settings = settings.with_session_name(session);
        }

        let skill = build_skill(self.skill, self.system_prompt.unwrap_or_default())?;

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

        let assessment = self.assessment.filter(|prompt| !prompt.trim().is_empty());

        Ok(Plan {
            provider,
            settings,
            conversation,
            evals,
            done_when,
            assessment,
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
            judge_config,
            command,
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
                reject(skill.is_some(), "skill")?;
                reject(judge.is_some(), "judge")?;
                Ok(ProviderSpec::Oneharness {
                    bin: bin.unwrap_or_else(|| "oneharness".into()),
                    judge_config: judge_config.map(PathBuf::from),
                })
            }
            ProviderKind::Command => {
                reject(bin.is_some(), "bin")?;
                reject(judge_config.is_some(), "judge_config")?;
                reject(skill.is_some(), "skill")?;
                reject(judge.is_some(), "judge")?;
                let command = command.filter(|c| !c.is_empty()).ok_or_else(|| {
                    CliError::Config("provider kind `command` needs a non-empty `command`".into())
                })?;
                Ok(ProviderSpec::Command { command })
            }
            ProviderKind::Split => {
                reject(bin.is_some(), "bin")?;
                reject(judge_config.is_some(), "judge_config")?;
                reject(command.is_some(), "command")?;
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
        /// The judge / simulated-user oneharness config file (`--config <path>`);
        /// `None` leaves the provider's own default (`oneharness.judge.toml`).
        judge_config: Option<PathBuf>,
    },
    /// A custom command speaking the JSON-lines protocol.
    Command {
        /// The provider argv.
        command: Vec<String>,
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
    /// Engine settings (session name, turn cap).
    pub settings: Settings,
    /// The conversation to drive.
    pub conversation: Conversation,
    /// The evals to score afterward.
    pub evals: Vec<Eval>,
    /// The completion condition, if any — re-judged at the end to decide whether
    /// the task actually completed (which drives the exit code).
    pub done_when: Option<String>,
    /// Prompt for the optional free-text assessment.
    pub assessment: Option<String>,
}

fn default_eval_kind() -> JudgeKind {
    JudgeKind::Boolean
}

/// Build the [`Skill`] under test from an optional skill directory and the
/// `system_prompt` text. Both are optional and combine: the `system_prompt` comes
/// first, then the loaded skill's `SKILL.md` body (see
/// [`SkillDefinition::into_skill`](crate::SkillDefinition::into_skill)). With
/// neither, the harness runs under an empty system prompt.
///
/// # Errors
/// [`CliError::Config`] if a `skill` path is given but its `SKILL.md` cannot be
/// loaded (missing file, malformed frontmatter).
fn build_skill(skill: Option<PathBuf>, system_prompt: String) -> Result<Skill, CliError> {
    match skill {
        Some(dir) => crate::load_skill(&dir)
            .map(|def| def.into_skill(&system_prompt))
            .map_err(|e| {
                CliError::Config(format!("could not load skill `{}`: {e}", dir.display()))
            }),
        // No skill: the system prompt (possibly empty) is the whole framing.
        None => Ok(Skill::new("agent", ".", system_prompt.trim())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Make a unique temp skill directory holding a `SKILL.md` of `contents`.
    fn skill_dir(name: &str, contents: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "onejudge-cfg-skill-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), contents).unwrap();
        dir
    }

    #[test]
    fn system_prompt_only_becomes_the_instructions() {
        let plan = Config::from_yaml("task: x\nsystem_prompt: Be a careful engineer.\n")
            .unwrap()
            .into_plan()
            .unwrap();
        let skill = &plan.conversation.skill;
        assert_eq!(skill.instructions, "Be a careful engineer.");
        assert_eq!(skill.name, "agent");
        assert_eq!(skill.dir, ".");
    }

    #[test]
    fn neither_skill_nor_system_prompt_leaves_empty_instructions() {
        let plan = Config::from_yaml("task: x\n").unwrap().into_plan().unwrap();
        assert_eq!(plan.conversation.skill.instructions, "");
    }

    #[test]
    fn skill_body_and_name_come_from_skill_md() {
        let dir = skill_dir(
            "greeter",
            "---\nname: greeter\ndescription: a greeter\n---\nGreet the user warmly.\n",
        );
        let yaml = format!(
            "task: x\nskill: {}\n",
            serde_json::to_string(&dir.to_string_lossy()).unwrap()
        );
        let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();
        let skill = &plan.conversation.skill;
        assert_eq!(skill.name, "greeter");
        assert_eq!(skill.instructions, "Greet the user warmly.");
        assert_eq!(skill.dir, dir.to_string_lossy());
    }

    #[test]
    fn skill_without_frontmatter_name_falls_back_to_the_dir_name() {
        let dir = skill_dir("fallback-name", "Just a body, no frontmatter.\n");
        let yaml = format!(
            "task: x\nskill: {}\n",
            serde_json::to_string(&dir.to_string_lossy()).unwrap()
        );
        let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();
        assert_eq!(plan.conversation.skill.name, "fallback-name");
    }

    #[test]
    fn system_prompt_precedes_the_skill_body_when_both_are_set() {
        let dir = skill_dir(
            "merged",
            "---\nname: merged\ndescription: d\n---\nSkill body text.\n",
        );
        let yaml = format!(
            "task: x\nsystem_prompt: Preamble first.\nskill: {}\n",
            serde_json::to_string(&dir.to_string_lossy()).unwrap()
        );
        let plan = Config::from_yaml(&yaml).unwrap().into_plan().unwrap();
        assert_eq!(
            plan.conversation.skill.instructions,
            "Preamble first.\n\nSkill body text."
        );
    }

    #[test]
    fn a_missing_skill_is_a_loud_config_error() {
        let missing =
            std::env::temp_dir().join(format!("onejudge-no-skill-{}", std::process::id()));
        let yaml = format!(
            "task: x\nskill: {}\n",
            serde_json::to_string(&missing.to_string_lossy()).unwrap()
        );
        let err = Config::from_yaml(&yaml).unwrap().into_plan().unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("could not load skill")));
    }

    #[test]
    fn skill_override_and_system_prompt_override_win_over_the_file() {
        let dir = skill_dir(
            "flag-skill",
            "---\nname: flag-skill\ndescription: d\n---\nFlag skill body.\n",
        );
        let mut cfg = Config::from_yaml("task: x\nsystem_prompt: from file\n").unwrap();
        cfg.apply(Overrides {
            skill: Some(dir.clone()),
            system_prompt: Some("from flag".into()),
            ..Overrides::default()
        });
        let plan = cfg.into_plan().unwrap();
        assert_eq!(
            plan.conversation.skill.instructions,
            "from flag\n\nFlag skill body."
        );
    }

    #[test]
    fn minimal_config_resolves_to_a_single_turn_plan() {
        let cfg = Config::from_yaml("task: do the thing\n").unwrap();
        let plan = cfg.into_plan().unwrap();
        assert_eq!(plan.settings.max_turns, 8);
        assert!(plan.done_when.is_none());
        assert!(plan.conversation.user.is_none());
        // An omitted judge_config leaves the provider default in place.
        assert!(matches!(
            plan.provider,
            ProviderSpec::Oneharness {
                judge_config: None,
                ..
            }
        ));
    }

    #[test]
    fn unknown_top_level_key_is_a_loud_error() {
        let err = Config::from_yaml("task: x\nnope: 1\n").unwrap_err();
        assert!(matches!(err, CliError::Config(_)));
    }

    #[test]
    fn missing_task_is_rejected() {
        let err = Config::from_yaml("session: s\n")
            .unwrap()
            .into_plan()
            .unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("no task")));
    }

    #[test]
    fn judge_config_resolves_onto_the_oneharness_spec() {
        let plan = Config::from_yaml(
            "task: x\nprovider:\n  kind: oneharness\n  judge_config: custom.judge.toml\n",
        )
        .unwrap()
        .into_plan()
        .unwrap();
        assert!(matches!(
            plan.provider,
            ProviderSpec::Oneharness { judge_config: Some(p), .. } if p == std::path::Path::new("custom.judge.toml")
        ));
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
        let mut cfg = Config::from_yaml("task: from file\n").unwrap();
        cfg.apply(Overrides {
            task: Some("from flag".into()),
            done_when: Some("it is done".into()),
            max_turns: Some(3),
            ..Overrides::default()
        });
        let plan = cfg.into_plan().unwrap();
        assert_eq!(plan.conversation.input, "from flag");
        assert_eq!(plan.done_when.as_deref(), Some("it is done"));
        assert_eq!(plan.settings.max_turns, 3);
    }

    #[test]
    fn env_overrides_use_the_onejudge_prefix_across_the_flag_surface() {
        let env = std::collections::HashMap::from([
            ("ONEJUDGE_JUDGE_CONFIG", "env.judge.toml"),
            ("ONEJUDGE_SKILL", "skills/env-skill"),
            ("ONEJUDGE_SYSTEM_PROMPT", "an env preamble"),
            ("ONEJUDGE_TASK", "from env"),
            ("ONEJUDGE_PERSONA", "an env reviewer"),
            ("ONEJUDGE_DONE_WHEN", "it ships"),
            ("ONEJUDGE_MAX_TURNS", "4"),
            ("ONEJUDGE_SESSION", "env-sess"),
            ("ONEJUDGE_PROVIDER", "command"),
        ]);
        let ov = Overrides::from_env(|k| env.get(k).map(|v| (*v).to_string())).unwrap();
        assert_eq!(ov.judge_config.as_deref(), Some("env.judge.toml"));
        assert_eq!(
            ov.skill.as_deref(),
            Some(std::path::Path::new("skills/env-skill"))
        );
        assert_eq!(ov.system_prompt.as_deref(), Some("an env preamble"));
        assert_eq!(ov.task.as_deref(), Some("from env"));
        assert_eq!(ov.persona.as_deref(), Some("an env reviewer"));
        assert_eq!(ov.done_when.as_deref(), Some("it ships"));
        assert_eq!(ov.max_turns, Some(4));
        assert_eq!(ov.session.as_deref(), Some("env-sess"));
        assert_eq!(ov.provider_kind, Some(ProviderKind::Command));
    }

    #[test]
    fn env_layer_beats_the_file_and_a_flag_beats_the_env() {
        // File sets the task and turn cap; the env overrides both; a flag then
        // wins over the env for the task, leaving the env's turn cap in place.
        let env = std::collections::HashMap::from([
            ("ONEJUDGE_TASK", "from env"),
            ("ONEJUDGE_MAX_TURNS", "9"),
        ]);
        let mut cfg =
            Config::from_yaml("task: from file\nuser:\n  persona: p\n  max_turns: 2\n").unwrap();
        cfg.apply(Overrides::from_env(|k| env.get(k).map(|v| (*v).to_string())).unwrap());
        cfg.apply(Overrides {
            task: Some("from flag".into()),
            ..Overrides::default()
        });
        let plan = cfg.into_plan().unwrap();
        assert_eq!(plan.conversation.input, "from flag");
        assert_eq!(plan.settings.max_turns, 9);
    }

    #[test]
    fn env_persona_implies_a_user_like_the_flag_does() {
        let env = std::collections::HashMap::from([("ONEJUDGE_PERSONA", "an env reviewer")]);
        let mut cfg = Config::from_yaml("task: t\n").unwrap();
        cfg.apply(Overrides::from_env(|k| env.get(k).map(|v| (*v).to_string())).unwrap());
        let plan = cfg.into_plan().unwrap();
        assert_eq!(
            plan.conversation.user.map(|u| u.persona).as_deref(),
            Some("an env reviewer")
        );
    }

    #[test]
    fn empty_env_values_are_treated_as_absent() {
        // An exported-but-blank variable must not force an empty override.
        let ov = Overrides::from_env(|k| (k == "ONEJUDGE_TASK").then(String::new)).unwrap();
        assert!(ov.task.is_none());
    }

    #[test]
    fn no_env_yields_empty_overrides() {
        let ov = Overrides::from_env(|_| None).unwrap();
        assert!(ov.task.is_none());
        assert!(ov.max_turns.is_none());
        assert!(ov.provider_kind.is_none());
    }

    #[test]
    fn invalid_env_max_turns_is_a_loud_error() {
        let err = Overrides::from_env(|k| (k == "ONEJUDGE_MAX_TURNS").then(|| "lots".to_string()))
            .unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("ONEJUDGE_MAX_TURNS")));
    }

    #[test]
    fn invalid_env_provider_is_a_loud_error() {
        let err = Overrides::from_env(|k| (k == "ONEJUDGE_PROVIDER").then(|| "nope".to_string()))
            .unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("ONEJUDGE_PROVIDER")));
    }

    #[test]
    fn overrides_apply_judge_config_session_and_persona() {
        let mut cfg = Config::from_yaml("task: t\n").unwrap();
        cfg.apply(Overrides {
            judge_config: Some("j.toml".into()),
            session: Some("sess-9".into()),
            persona: Some("a reviewer".into()),
            ..Overrides::default()
        });
        let plan = cfg.into_plan().unwrap();
        assert!(matches!(
            &plan.provider,
            ProviderSpec::Oneharness { judge_config: Some(p), .. } if p == std::path::Path::new("j.toml")
        ));
        assert_eq!(plan.settings.session_name, "sess-9");
        // The persona flag implies a multi-turn conversation.
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

        // `judge_config` belongs to oneharness, not command or split.
        let err = Config::from_yaml(
            "task: x\nprovider:\n  kind: command\n  command: [p]\n  judge_config: j.toml\n",
        )
        .unwrap()
        .into_plan()
        .unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("judge_config")));

        let err = Config::from_yaml(
            "task: x\nprovider:\n  kind: split\n  judge_config: j.toml\n  skill:\n    kind: oneharness\n  judge:\n    kind: oneharness\n",
        )
        .unwrap()
        .into_plan()
        .unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("judge_config")));
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

    #[test]
    fn assessment_prompt_resolves_and_empty_value_is_ignored() {
        let plan = Config::from_yaml("task: x\nassessment: Identify follow-up work.\n")
            .unwrap()
            .into_plan()
            .unwrap();
        assert_eq!(plan.assessment.as_deref(), Some("Identify follow-up work."));

        let empty = Config::from_yaml("task: x\nassessment: '   '\n")
            .unwrap()
            .into_plan()
            .unwrap();
        assert_eq!(empty.assessment, None);
    }
}
