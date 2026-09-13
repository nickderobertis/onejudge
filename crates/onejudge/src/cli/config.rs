//! The YAML config surface for the `onejudge` CLI, and the typed plan it resolves
//! into. Every external input is validated at this boundary: the file is parsed
//! into strict serde models (`deny_unknown_fields`), overrides from the
//! `ONEJUDGE_*` environment and the flags are merged (flags win over env, env
//! wins over file, file wins over defaults), and the result is validated into a
//! [`Plan`] the run driver executes. A malformed config — or an invalid env
//! override — is a loud, actionable error, never a silent default.

use std::path::PathBuf;

use serde::Deserialize;

use crate::{Conversation, JudgeKind, Settings, SharedSpawnHook, SimulatedUser, Skill};

use super::CliError;

/// The whole YAML config for one run. Field defaults let a minimal file (just a
/// `task`) work, while `deny_unknown_fields` makes a typo'd key a hard error
/// instead of a silently-ignored setting.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
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
    /// Whether a run of quiet, repeated exchanges settles the loop early.
    /// Defaults to `true` — omitting the key is exactly what every config has
    /// always done. Set it `false` when reporting nothing *is* this
    /// conversation's contract (a long-lived observer answering with one fixed
    /// short sentence while it finds nothing), so the loop is driven on to
    /// `max_turns` instead of being settled on its second quiet exchange.
    #[serde(default)]
    pub settle_on_noop: Option<bool>,
    /// Files or directories every judge-side prompt names for the evaluator to
    /// read directly — work that may be untracked or gitignored, which
    /// `git_status` and `git_diff` cannot show. An absolute path is used as
    /// written; a relative one is resolved against the skill's working directory.
    /// A directory is listed newest-modified first, re-read every judge-side
    /// turn; a path that does not exist is named as such. Replaced by
    /// `--artifact` / `ONEJUDGE_ARTIFACTS`.
    #[serde(default)]
    pub artifacts: Vec<String>,
}

/// One eval scored against the finished transcript.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// `oneharness` (default) | `command` | `split` | `llmlint` (a judge entry
    /// only).
    #[serde(default)]
    pub kind: ProviderKind,
    /// `oneharness`: the `oneharness` binary (default `oneharness`). `llmlint`:
    /// the `llmlint` executable (default `llmlint`), probed with `--version` when
    /// the run is built, so an absent one is a config error before any turn.
    #[serde(default)]
    pub bin: Option<String>,
    /// `oneharness`: the oneharness config file the judge / simulated user run
    /// under, passed as `oneharness run --config <path>` (default
    /// `oneharness.judge.toml`). This is where the judge-side harness/model
    /// selection lives.
    #[serde(default)]
    pub judge_config: Option<String>,
    /// `oneharness`: the agent-side binary speaks the **streamed provider
    /// protocol** (`docs/streaming.md`) — NDJSON tool events as they occur, then a
    /// terminal report line. Default `false` (one buffered report document).
    #[serde(default)]
    pub stream: Option<bool>,
    /// `oneharness`: ask for **controllable** turns — both parties' calls add
    /// `oneharness run --control`, opening an out-of-band socket each that a
    /// separate `oneharness interrupt` process can redirect the in-flight turn
    /// through. Default `false`, and `false` changes nothing about the run. The
    /// addresses land on the report's `control` (agent) and `supervisor_control`
    /// (judge) blocks — separate sockets, separately refusable; onejudge never
    /// interrupts anything itself. See `docs/control.md`.
    #[serde(default)]
    pub control: Option<bool>,
    /// `oneharness`: harness ids to run against **oneharness's own deterministic
    /// `MOCK_*` responder** instead of a paid model (`oneharness run --mock-harness
    /// <id>`, repeatable). Empty — the default — bills the real model.
    ///
    /// This is how an acceptance proof that needs a real multi-turn chain runs for
    /// free: only the model is scripted, everything else is the real oneharness. It
    /// applies to the agent side *and* the judge side, so each id must be one the
    /// config for that side selects (use `kind: split` when the two sides need
    /// different ids), and it makes the run **spawn** `oneharness` — the responder is
    /// oneharness's own binary re-executed, which an in-process run cannot be. See
    /// `docs/cli.md`.
    #[serde(default)]
    pub mock_harness: Option<Vec<String>>,
    /// `command`: the provider argv (program + args).
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// `llmlint`: the llmlint config file, passed as `llmlint lint -c <path>` as
    /// given. Unset, llmlint discovers its own (`llmlint.yml` up from the
    /// worktree it lints) — the repository's default.
    #[serde(default)]
    pub config: Option<PathBuf>,
    /// `llmlint`: review only what the worktree changed against this git
    /// revision (`llmlint lint --diff --diff-base <ref>`). onejudge does no base
    /// auto-detection: a host wiring a stack passes its own comparison base here.
    /// Unset, llmlint judges the whole tree.
    #[serde(default)]
    pub diff_base: Option<String>,
    /// `llmlint`: extra arguments appended to every `llmlint lint` invocation
    /// after the ones onejudge composes (`--rule NAME`, `--agent NAME`, …).
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// `split`: the backend that runs the agent's turns.
    #[serde(default)]
    pub skill: Option<Box<ProviderConfig>>,
    /// `split`: the one judge that judges and plays the simulated user — the
    /// one-element shorthand for `judges: [<this>]`. Exclusive with `judges`.
    #[serde(default)]
    pub judge: Option<Box<ProviderConfig>>,
    /// `split`: the judges, one or more, every one run concurrently against each
    /// worker turn and combined into one attributed answer (see `docs/judges.md`).
    /// Exclusive with `judge`; must not be empty.
    #[serde(default)]
    pub judges: Option<Vec<ProviderConfig>>,
    /// A judge entry only: this judge's name on every surface — the `## Judge`
    /// header the worker reads, the `[<label>]` reason prefix, the report's
    /// `judge_decisions`, and the `-<label>` session suffix. `[A-Za-z0-9_-]+`,
    /// unique within the list; defaults to the entry's `kind` for the first judge
    /// of that kind and `<kind>-<n>` for repeats.
    #[serde(default)]
    pub label: Option<String>,
}

/// The provider backends the CLI can build. One enum is the single source for
/// both the YAML `kind:` (via `Deserialize`) and the `--provider` flag (via
/// clap's `ValueEnum`), so the two surfaces cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize, clap::ValueEnum)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Shell out to the `oneharness` CLI (the default).
    #[default]
    Oneharness,
    /// A custom command speaking the JSON-lines protocol.
    Command,
    /// Compose a skill-runner with a separate judge / simulated-user backend.
    Split,
    /// A judge whose whole verdict is one `llmlint` run over the worker's tree
    /// (`docs/judges.md`). Judge-side only: valid under `judges:` / `judge:` of a
    /// `split`, refused as the top-level provider, under `skill:`, and as the
    /// `--provider` / `ONEJUDGE_PROVIDER` override.
    Llmlint,
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
    /// `--artifact` (repeatable) / `ONEJUDGE_ARTIFACTS` (entries separated by the
    /// platform path-list separator, as `PATH` is). Replaces `user.artifacts`.
    pub artifacts: Option<Vec<String>>,
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
            artifacts: get("ONEJUDGE_ARTIFACTS").map(|list| {
                std::env::split_paths(&list)
                    .filter(|path| !path.as_os_str().is_empty())
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect()
            }),
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
            artifacts,
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
        // none — supplying `--persona` / `--done-when` / `--max-turns` /
        // `--artifact` on the CLI is a request to drive the loop.
        if persona.is_some() || done_when.is_some() || max_turns.is_some() || artifacts.is_some() {
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
            if let Some(artifacts) = artifacts {
                user.artifacts = artifacts;
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

        let provider = self.provider.resolve(Place::Top)?;

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
                if let Some(settle) = u.settle_on_noop {
                    settings = settings.with_settle_on_noop(settle);
                }
                let mut sim = SimulatedUser::new(u.persona).artifacts(u.artifacts);
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

        // Nothing in a judge list that can neither score a number nor write prose
        // can answer a numeric eval or an assessment, so asking is refused up
        // front rather than failing at the end of a paid run.
        if let ProviderSpec::Split { judges, .. } = &provider {
            let wants_number = evals
                .iter()
                .any(|eval| matches!(eval.kind, EvalKind::Numeric { .. }));
            if (wants_number || assessment.is_some())
                && !judges.iter().any(JudgeSpec::scores_and_writes)
            {
                let what = if wants_number {
                    "a numeric eval"
                } else {
                    "an `assessment`"
                };
                return Err(CliError::Config(format!(
                    "the config names {what}, but no judge in `provider.judges` can score a                      number or write prose (that needs an `oneharness` or `command` judge)"
                )));
            }
        }

        Ok(Plan {
            provider,
            settings,
            conversation,
            evals,
            done_when,
            assessment,
            // A config file cannot name an in-process hook; an embedder installs
            // one with `Plan::with_spawn_hook`, and no hook is today's behaviour.
            spawn_hook: None,
            // Likewise: a note channel is opened in process by whoever supervises
            // the run, and installed with `Plan::with_notes`.
            notes: None,
        })
    }
}

/// Where in the config a [`ProviderConfig`] sits: the top-level `provider`, a
/// split's `skill:` child, or one entry of its judge list. A `label` belongs to
/// a judge entry and nowhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Top,
    Skill,
    Judge,
}

impl ProviderConfig {
    /// Validate the flat config into a typed [`ProviderSpec`], rejecting fields
    /// that do not belong to the chosen `kind` — or, for `label`, to this place.
    fn resolve(self, place: Place) -> Result<ProviderSpec, CliError> {
        let ProviderConfig {
            kind,
            bin,
            judge_config,
            stream,
            control,
            mock_harness,
            command,
            config,
            diff_base,
            args,
            skill,
            judge,
            judges,
            label,
        } = self;

        // A label names one judge of a list; anywhere else there is nothing for
        // it to name. Checked first, and against the place rather than the kind,
        // so `label: x` on the top-level provider is refused whatever kind it is.
        if label.is_some() && place != Place::Judge {
            return Err(CliError::Config(
                "`label` is only valid on a judge entry (under `provider.judges:` or                  `provider.judge:`)"
                    .into(),
            ));
        }

        // An llmlint judge decides; it cannot run the agent's turns. So it is a
        // judge entry and nothing else: refused at the top level (which is also
        // where `--provider llmlint` / `ONEJUDGE_PROVIDER=llmlint` land) and under
        // a split's `skill:`, each naming where it belongs.
        if kind == ProviderKind::Llmlint && place != Place::Judge {
            let where_ = match place {
                Place::Top => {
                    "the top-level `provider` (or the `--provider` / `ONEJUDGE_PROVIDER` override)"
                }
                Place::Skill => "`provider.skill`",
                Place::Judge => unreachable!("a judge entry admits `llmlint`"),
            };
            return Err(CliError::Config(format!(
                "provider kind `llmlint` is a judge and cannot run the agent's turns: it is \
                 not valid as {where_}; put it under `provider.judges:` (or `provider.judge:`) \
                 of a `kind: split` provider"
            )));
        }

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
                reject(config.is_some(), "config")?;
                reject(diff_base.is_some(), "diff_base")?;
                reject(args.is_some(), "args")?;
                reject(skill.is_some(), "skill")?;
                reject(judge.is_some(), "judge")?;
                reject(judges.is_some(), "judges")?;
                Ok(ProviderSpec::Oneharness {
                    // Unset means the in-process engine, which is the default and
                    // needs nothing on PATH. Naming one is the explicit opt-in to
                    // spawning it — see `OneharnessProvider::with_bin`.
                    bin,
                    judge_config: judge_config.map(PathBuf::from),
                    stream: stream.unwrap_or(false),
                    control: control.unwrap_or(false),
                    mock_harness: mock_harness.unwrap_or_default(),
                })
            }
            ProviderKind::Command => {
                reject(bin.is_some(), "bin")?;
                reject(judge_config.is_some(), "judge_config")?;
                reject(stream.is_some(), "stream")?;
                reject(control.is_some(), "control")?;
                reject(mock_harness.is_some(), "mock_harness")?;
                reject(config.is_some(), "config")?;
                reject(diff_base.is_some(), "diff_base")?;
                reject(args.is_some(), "args")?;
                reject(skill.is_some(), "skill")?;
                reject(judge.is_some(), "judge")?;
                reject(judges.is_some(), "judges")?;
                let command = command.filter(|c| !c.is_empty()).ok_or_else(|| {
                    CliError::Config("provider kind `command` needs a non-empty `command`".into())
                })?;
                Ok(ProviderSpec::Command { command })
            }
            ProviderKind::Split => {
                reject(bin.is_some(), "bin")?;
                reject(judge_config.is_some(), "judge_config")?;
                reject(stream.is_some(), "stream")?;
                // A `split` has two backends, so a control ask on the wrapper says
                // nothing about which turn it addresses. Set it on the `skill:`
                // child, which is the side that runs the controllable turn.
                reject(control.is_some(), "control")?;
                // Same reasoning for the deterministic harness: each side selects
                // its own harnesses under its own config, so the ids belong on the
                // child that runs them.
                reject(mock_harness.is_some(), "mock_harness")?;
                reject(command.is_some(), "command")?;
                reject(config.is_some(), "config")?;
                reject(diff_base.is_some(), "diff_base")?;
                reject(args.is_some(), "args")?;
                let skill = skill.ok_or_else(|| {
                    CliError::Config("provider kind `split` needs a `skill` provider".into())
                })?;
                // `judge:` is the one-element shorthand for `judges:`; both are
                // normalized here into ONE list, so there is exactly one judge-panel
                // code path downstream.
                let judges = match (judge, judges) {
                    (Some(_), Some(_)) => {
                        return Err(CliError::Config(
                            "`judge` and `judges` are exclusive under provider kind `split`: \
                             `judge:` is the one-element shorthand for `judges: [..]`"
                                .into(),
                        ))
                    }
                    (Some(judge), None) => vec![*judge],
                    (None, Some(judges)) if judges.is_empty() => {
                        return Err(CliError::Config(
                            "`judges` must name at least one judge under provider kind `split`"
                                .into(),
                        ))
                    }
                    (None, Some(judges)) => judges,
                    (None, None) => {
                        return Err(CliError::Config(
                            "provider kind `split` needs a `judge` (or `judges`) provider".into(),
                        ))
                    }
                };
                Ok(ProviderSpec::Split {
                    skill: Box::new(skill.resolve(Place::Skill)?),
                    judges: resolve_judges(judges)?,
                })
            }
            ProviderKind::Llmlint => {
                reject(judge_config.is_some(), "judge_config")?;
                reject(stream.is_some(), "stream")?;
                reject(control.is_some(), "control")?;
                reject(mock_harness.is_some(), "mock_harness")?;
                reject(command.is_some(), "command")?;
                reject(skill.is_some(), "skill")?;
                reject(judge.is_some(), "judge")?;
                reject(judges.is_some(), "judges")?;
                let bin =
                    match bin {
                        Some(bin) if bin.trim().is_empty() => return Err(CliError::Config(
                            "`bin` under provider kind `llmlint` must name the llmlint executable"
                                .into(),
                        )),
                        Some(bin) => bin,
                        None => crate::DEFAULT_LLMLINT_BIN.to_string(),
                    };
                Ok(ProviderSpec::Llmlint {
                    bin,
                    config,
                    diff_base: diff_base.filter(|base| !base.trim().is_empty()),
                    args: args.unwrap_or_default(),
                })
            }
        }
    }
}

/// Resolve a judge list, labelling each entry: the explicit `label` where one is
/// given, else the entry's `kind` for the first judge of that kind and
/// `<kind>-<n>` (n ≥ 2, counting judges of that kind in list order) for repeats.
/// A label is refused when it is not `[A-Za-z0-9_-]+` or is already taken —
/// whether by another explicit label or by a defaulted one.
fn resolve_judges(judges: Vec<ProviderConfig>) -> Result<Vec<JudgeSpec>, CliError> {
    let mut resolved: Vec<JudgeSpec> = Vec::with_capacity(judges.len());
    let mut seen_of_kind: std::collections::HashMap<ProviderKind, usize> =
        std::collections::HashMap::new();
    for judge in judges {
        let kind = judge.kind;
        let n = seen_of_kind.entry(kind).or_insert(0);
        *n += 1;
        let label = match judge.label.clone() {
            Some(label) => {
                if !crate::is_valid_label(&label) {
                    return Err(CliError::Config(format!(
                        "judge `label` `{label}` is invalid: a label is `[A-Za-z0-9_-]+`"
                    )));
                }
                label
            }
            None if *n == 1 => kind.as_str().to_string(),
            None => format!("{}-{n}", kind.as_str()),
        };
        if resolved.iter().any(|earlier| earlier.label == label) {
            return Err(CliError::Config(format!(
                "judge `label` `{label}` is used by more than one judge; labels are unique \
                 within `judges`"
            )));
        }
        resolved.push(JudgeSpec {
            label,
            provider: judge.resolve(Place::Judge)?,
        });
    }
    Ok(resolved)
}

impl ProviderKind {
    /// The stable YAML spelling — also the `kind` a judge's decision is recorded
    /// under.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Oneharness => "oneharness",
            ProviderKind::Command => "command",
            ProviderKind::Split => "split",
            ProviderKind::Llmlint => "llmlint",
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
        /// An `oneharness` binary to **spawn** instead of running the linked
        /// engine in process. `None` — the default — runs it in process.
        bin: Option<String>,
        /// The judge / simulated-user oneharness config file (`--config <path>`);
        /// `None` leaves the provider's own default (`oneharness.judge.toml`).
        judge_config: Option<PathBuf>,
        /// The agent-side binary speaks the streamed provider protocol.
        stream: bool,
        /// Ask for a controllable agent turn (`oneharness run --control`).
        control: bool,
        /// Harness ids run against oneharness's deterministic responder
        /// (`oneharness run --mock-harness <id>`) instead of a paid model. Empty
        /// for an ordinary run.
        mock_harness: Vec<String>,
    },
    /// A custom command speaking the JSON-lines protocol.
    Command {
        /// The provider argv.
        command: Vec<String>,
    },
    /// A composed skill-runner + judge-panel backend.
    Split {
        /// Runs the agent's turns.
        skill: Box<ProviderSpec>,
        /// The judges, in list order — one or more, every one run concurrently
        /// against each worker turn. A single `judge:` resolves to a list of one.
        judges: Vec<JudgeSpec>,
    },
    /// A judge whose verdict is one `llmlint` run over the worker's tree
    /// ([`LlmlintProvider`](crate::LlmlintProvider)). Only ever a judge of a
    /// `split`: the config layer refuses it anywhere else.
    Llmlint {
        /// The `llmlint` executable, probed with `--version` when built.
        bin: String,
        /// The llmlint config file (`-c <path>`), or llmlint's own discovery.
        config: Option<PathBuf>,
        /// The git revision to review the worktree's changes against
        /// (`--diff --diff-base <ref>`), or the whole tree.
        diff_base: Option<String>,
        /// Extra arguments appended to every run.
        args: Vec<String>,
    },
}

impl ProviderSpec {
    /// Which backend this spec builds.
    #[must_use]
    pub fn kind(&self) -> ProviderKind {
        match self {
            ProviderSpec::Oneharness { .. } => ProviderKind::Oneharness,
            ProviderSpec::Command { .. } => ProviderKind::Command,
            ProviderSpec::Split { .. } => ProviderKind::Split,
            ProviderSpec::Llmlint { .. } => ProviderKind::Llmlint,
        }
    }
}

/// One judge of a `split`'s panel: its resolved label and the backend that
/// judges under it.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeSpec {
    /// The label every surface names this judge by.
    pub label: String,
    /// The backend.
    pub provider: ProviderSpec,
}

impl JudgeSpec {
    /// Whether this judge can score a number and write prose — an `oneharness`
    /// or `command` judge (or a nested split judged by one). An `llmlint` judge
    /// cannot: a lint run has no opinion on "how readable is this, 1 to 5" and
    /// writes no prose, so it is left out of numeric evals and assessments, and a
    /// config that asks for one with no other judge is refused.
    #[must_use]
    pub fn scores_and_writes(&self) -> bool {
        matches!(
            self.provider.kind(),
            ProviderKind::Oneharness | ProviderKind::Command | ProviderKind::Split
        )
    }
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
    /// The embedder's [`SpawnHook`](crate::SpawnHook), installed on **every**
    /// backend the plan builds. `None` — the only thing a config file can produce
    /// — leaves today's behaviour: the run spawns into onejudge's own group and
    /// claims none. Set it with [`Plan::with_spawn_hook`].
    pub spawn_hook: Option<SharedSpawnHook>,
    /// The note channel's engine end. `None` — the only thing a config file can
    /// produce — is today's behaviour exactly: no note can arrive, and every seam
    /// below behaves as it did before the channel existed. Set it with
    /// [`Plan::with_notes`].
    pub notes: Option<crate::note::NoteInbox>,
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A `dyn SpawnHook` is not `Debug`; whether one is installed is the part a
        // reader of a plan's `Debug` output actually needs.
        f.debug_struct("Plan")
            .field("provider", &self.provider)
            .field("settings", &self.settings)
            .field("conversation", &self.conversation)
            .field("evals", &self.evals)
            .field("done_when", &self.done_when)
            .field("assessment", &self.assessment)
            .field("spawn_hook", &self.spawn_hook.is_some())
            .field("notes", &self.notes.is_some())
            .finish()
    }
}

impl Plan {
    /// Offer every process this plan spawns to `hook` before it starts work, so an
    /// embedder driving onejudge **through a plan** — rather than building the
    /// providers itself — can place each one in a group it owns and can later
    /// terminate.
    ///
    /// This is the plan-level reach of the same seam
    /// [`OneharnessProvider::with_spawn_hook`](crate::OneharnessProvider::with_spawn_hook)
    /// exposes, not a second mechanism: the hook is installed on whichever backend
    /// [`ProviderSpec`] names, and on **both** sides of a `split` — so one
    /// embedder-owned group spans the whole two-party worker + judge tree, which is
    /// the case a cancel otherwise leaks.
    ///
    /// See [`SpawnHook`](crate::SpawnHook) and `docs/spawn-hook.md`.
    #[must_use]
    pub fn with_spawn_hook(mut self, hook: SharedSpawnHook) -> Self {
        self.spawn_hook = Some(hook);
        self
    }

    /// Read notes sent into this run from `inbox`, so an embedder driving onejudge
    /// **through a plan** — rather than building the engine itself — can deliver a
    /// role-addressed correction into the running conversation.
    ///
    /// This is the plan-level reach of
    /// [`Engine::with_notes`](crate::Engine::with_notes), not a second mechanism:
    /// a criterion a delivered binding note adds is read at *both* judging sites —
    /// the per-turn supervisor decision and the authoritative re-judge against the
    /// finished transcript, which is the one that decides the run's exit code. See
    /// the [`note`](crate::note) module.
    #[must_use]
    pub fn with_notes(mut self, inbox: crate::note::NoteInbox) -> Self {
        self.notes = Some(inbox);
        self
    }
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

    /// A hook that overrides neither half — enough to prove the plan carries one.
    struct Inert;
    impl crate::SpawnHook for Inert {}

    #[test]
    fn a_plan_carries_no_spawn_hook_until_an_embedder_installs_one() {
        // A config file cannot name an in-process hook, so a resolved plan starts
        // without one and behaves exactly as it did before the seam existed.
        let plan = Config::from_yaml("task: x\n").unwrap().into_plan().unwrap();
        assert!(plan.spawn_hook.is_none());
        assert!(format!("{plan:?}").contains("spawn_hook: false"));

        let installed = plan.with_spawn_hook(std::sync::Arc::new(Inert));
        assert!(installed.spawn_hook.is_some());
        assert!(format!("{installed:?}").contains("spawn_hook: true"));
        // The rest of the plan is untouched by installing one.
        assert_eq!(installed.conversation.input, "x");
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
    fn stream_resolves_onto_the_oneharness_spec_and_defaults_off() {
        let streamed =
            Config::from_yaml("task: x\nprovider:\n  kind: oneharness\n  stream: true\n")
                .unwrap()
                .into_plan()
                .unwrap();
        assert!(matches!(
            streamed.provider,
            ProviderSpec::Oneharness { stream: true, .. }
        ));

        // Omitted: a provider writes one buffered report document, as before.
        let buffered = Config::from_yaml("task: x\n").unwrap().into_plan().unwrap();
        assert!(matches!(
            buffered.provider,
            ProviderSpec::Oneharness { stream: false, .. }
        ));
    }

    #[test]
    fn stream_is_rejected_under_a_kind_that_cannot_honor_it() {
        // `stream` describes the oneharness agent-side call. A `command` backend
        // speaks the request/response protocol, and a `split` declares it on the
        // child that actually runs the agent.
        for yaml in [
            "task: x\nprovider:\n  kind: command\n  command: [p]\n  stream: true\n",
            "task: x\nprovider:\n  kind: split\n  stream: true\n  skill:\n    kind: oneharness\n  judge:\n    kind: oneharness\n",
        ] {
            let err = Config::from_yaml(yaml).unwrap().into_plan().unwrap_err();
            assert!(matches!(err, CliError::Config(m) if m.contains("stream")), "{yaml}");
        }

        // Under a split's own oneharness child it is valid, and reaches that child.
        let plan = Config::from_yaml(
            "task: x\nprovider:\n  kind: split\n  skill:\n    kind: oneharness\n    stream: true\n  judge:\n    kind: command\n    command: [j]\n",
        )
        .unwrap()
        .into_plan()
        .unwrap();
        match plan.provider {
            ProviderSpec::Split { skill, .. } => assert!(matches!(
                *skill,
                ProviderSpec::Oneharness { stream: true, .. }
            )),
            other => panic!("expected split, got {other:?}"),
        }
    }

    #[test]
    fn control_resolves_onto_the_oneharness_spec_and_defaults_off() {
        let asked = Config::from_yaml("task: x\nprovider:\n  kind: oneharness\n  control: true\n")
            .unwrap()
            .into_plan()
            .unwrap();
        assert!(matches!(
            asked.provider,
            ProviderSpec::Oneharness { control: true, .. }
        ));

        // Omitted: no socket, no flag, and a byte-identical argv — the default has
        // to change nothing for every caller that predates turn control.
        let plain = Config::from_yaml("task: x\n").unwrap().into_plan().unwrap();
        assert!(matches!(
            plain.provider,
            ProviderSpec::Oneharness { control: false, .. }
        ));
    }

    #[test]
    fn mock_harness_resolves_onto_the_oneharness_spec_and_defaults_to_none() {
        let mocked = Config::from_yaml(
            "task: x\nprovider:\n  kind: oneharness\n  mock_harness: [claude-code, codex]\n",
        )
        .unwrap()
        .into_plan()
        .unwrap();
        match mocked.provider {
            ProviderSpec::Oneharness { mock_harness, .. } => {
                assert_eq!(mock_harness, ["claude-code", "codex"]);
            }
            other => panic!("expected oneharness, got {other:?}"),
        }

        // Omitted: no flag, and a run that bills the model exactly as before.
        let plain = Config::from_yaml("task: x\n").unwrap().into_plan().unwrap();
        assert!(matches!(
            plain.provider,
            ProviderSpec::Oneharness { ref mock_harness, .. } if mock_harness.is_empty()
        ));
    }

    #[test]
    fn mock_harness_is_rejected_under_a_kind_that_cannot_honor_it() {
        // A `command` backend is not oneharness, and a `split`'s two sides select
        // their harnesses under two different configs — so the ids belong on the
        // child that runs them.
        for yaml in [
            "task: x\nprovider:\n  kind: command\n  command: [p]\n  mock_harness: [claude-code]\n",
            "task: x\nprovider:\n  kind: split\n  mock_harness: [claude-code]\n  skill:\n    kind: oneharness\n  judge:\n    kind: oneharness\n",
        ] {
            let err = Config::from_yaml(yaml).unwrap().into_plan().unwrap_err();
            assert!(
                matches!(err, CliError::Config(m) if m.contains("mock_harness")),
                "{yaml}"
            );
        }

        // On a split's own child it is valid, and reaches that child.
        let plan = Config::from_yaml(
            "task: x\nprovider:\n  kind: split\n  skill:\n    kind: oneharness\n    \
             mock_harness: [claude-code]\n  judge:\n    kind: command\n    command: [j]\n",
        )
        .unwrap()
        .into_plan()
        .unwrap();
        match plan.provider {
            ProviderSpec::Split { skill, .. } => assert!(matches!(
                *skill,
                ProviderSpec::Oneharness { ref mock_harness, .. } if *mock_harness == ["claude-code"]
            )),
            other => panic!("expected split, got {other:?}"),
        }
    }

    #[test]
    fn control_is_rejected_under_a_kind_that_cannot_honor_it() {
        // `control` describes the oneharness agent-side call. A `command` backend
        // has no oneharness socket, and a `split` has two backends — so it declares
        // it on the child that runs the controllable turn.
        for yaml in [
            "task: x\nprovider:\n  kind: command\n  command: [p]\n  control: true\n",
            "task: x\nprovider:\n  kind: split\n  control: true\n  skill:\n    kind: oneharness\n  judge:\n    kind: oneharness\n",
        ] {
            let err = Config::from_yaml(yaml).unwrap().into_plan().unwrap_err();
            assert!(
                matches!(err, CliError::Config(m) if m.contains("control")),
                "{yaml}"
            );
        }

        // Under a split's own skill-side child it is valid, and reaches that child.
        let plan = Config::from_yaml(
            "task: x\nprovider:\n  kind: split\n  skill:\n    kind: oneharness\n    control: true\n  judge:\n    kind: command\n    command: [j]\n",
        )
        .unwrap()
        .into_plan()
        .unwrap();
        match plan.provider {
            ProviderSpec::Split { skill, .. } => assert!(matches!(
                *skill,
                ProviderSpec::Oneharness { control: true, .. }
            )),
            other => panic!("expected split, got {other:?}"),
        }
    }

    #[test]
    fn settle_on_noop_defaults_to_true_and_is_declarable_either_way() {
        // Omitting the key is what every existing config does, and it must keep
        // meaning "settle a stalled loop". Declaring it is how a conversation whose
        // contract is to report nothing opts out.
        let omitted = Config::from_yaml("task: t\nuser:\n  persona: p\n")
            .unwrap()
            .into_plan()
            .unwrap();
        assert!(omitted.settings.settle_on_noop);

        for declared in [true, false] {
            let plan = Config::from_yaml(&format!(
                "task: t\nuser:\n  persona: p\n  settle_on_noop: {declared}\n"
            ))
            .unwrap()
            .into_plan()
            .unwrap();
            assert_eq!(plan.settings.settle_on_noop, declared);
        }
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
    fn artifact_overrides_replace_the_configured_list_flag_over_env_over_file() {
        let file = "task: t\nuser:\n  persona: p\n  artifacts: [from-file.md, .plans]\n";
        let artifacts_of = |cfg: Config| {
            cfg.into_plan()
                .unwrap()
                .conversation
                .user
                .unwrap()
                .artifacts
        };
        assert_eq!(
            artifacts_of(Config::from_yaml(file).unwrap()),
            ["from-file.md", ".plans"]
        );

        let joined = std::env::join_paths(["env-a.md", "dir/env-b"])
            .unwrap()
            .into_string()
            .unwrap();
        let env = std::collections::HashMap::from([("ONEJUDGE_ARTIFACTS", joined.as_str())]);
        let from_env = || Overrides::from_env(|k| env.get(k).map(|v| (*v).to_string())).unwrap();
        let mut cfg = Config::from_yaml(file).unwrap();
        cfg.apply(from_env());
        assert_eq!(artifacts_of(cfg.clone()), ["env-a.md", "dir/env-b"]);
        cfg.apply(Overrides {
            artifacts: Some(vec!["flag.md".into()]),
            ..Overrides::default()
        });
        assert_eq!(artifacts_of(cfg), ["flag.md"]);

        // Like the persona override, naming artifacts implies a simulated user.
        let mut bare = Config::from_yaml("task: t\n").unwrap();
        bare.apply(from_env());
        assert_eq!(artifacts_of(bare), ["env-a.md", "dir/env-b"]);
        // An unset list names nothing.
        assert!(
            artifacts_of(Config::from_yaml("task: t\nuser:\n  persona: p\n").unwrap()).is_empty()
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

        let only_judge = "task: x\nprovider:\n  kind: split\n  judges:\n    - kind: oneharness\n";
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
            ProviderSpec::Split { skill, judges } => {
                assert!(matches!(*skill, ProviderSpec::Oneharness { .. }));
                // `judge:` is the one-element list, labelled by its kind.
                assert_eq!(judges.len(), 1);
                assert_eq!(judges[0].label, "command");
                assert!(matches!(judges[0].provider, ProviderSpec::Command { .. }));
            }
            other => panic!("expected split, got {other:?}"),
        }
    }

    #[test]
    fn judge_and_judges_resolve_through_one_path_to_the_same_panel() {
        let one = "task: x\nprovider:\n  kind: split\n  skill:\n    kind: oneharness\n  \
                   judge:\n    kind: command\n    command: [judge-prov]\n";
        let list = "task: x\nprovider:\n  kind: split\n  skill:\n    kind: oneharness\n  \
                    judges:\n    - kind: command\n      command: [judge-prov]\n";
        let one = Config::from_yaml(one).unwrap().into_plan().unwrap();
        let list = Config::from_yaml(list).unwrap().into_plan().unwrap();
        assert_eq!(one.provider, list.provider);
    }

    #[test]
    fn a_judge_list_labels_each_entry_by_kind_and_order_unless_named() {
        let yaml = "task: x\nprovider:\n  kind: split\n  skill:\n    kind: oneharness\n  \
                    judges:\n    - kind: oneharness\n      judge_config: a.toml\n    \
                    - kind: command\n      command: [lint]\n      label: lint_run\n    \
                    - kind: command\n      command: [other]\n    \
                    - kind: oneharness\n";
        let plan = Config::from_yaml(yaml).unwrap().into_plan().unwrap();
        let ProviderSpec::Split { judges, .. } = plan.provider else {
            panic!("a split");
        };
        let labels: Vec<&str> = judges.iter().map(|j| j.label.as_str()).collect();
        // The first judge of a kind is the bare kind, a repeat is `<kind>-<n>`
        // counting every judge of that kind in list order — a labelled one
        // included, so the third entry is the *second* command — and an explicit
        // label is taken verbatim.
        assert_eq!(
            labels,
            ["oneharness", "lint_run", "command-2", "oneharness-2"]
        );
        assert!(matches!(
            &judges[0].provider,
            ProviderSpec::Oneharness { judge_config: Some(p), .. } if p == std::path::Path::new("a.toml")
        ));
        assert!(judges.iter().all(JudgeSpec::scores_and_writes));
    }

    #[test]
    fn judge_list_shape_errors_name_the_field() {
        let split = |judges: &str| {
            format!("task: x\nprovider:\n  kind: split\n  skill:\n    kind: oneharness\n{judges}")
        };
        let refused = |yaml: &str, needle: &str| {
            let err = Config::from_yaml(yaml).unwrap().into_plan().unwrap_err();
            assert!(
                matches!(&err, CliError::Config(m) if m.contains(needle)),
                "{yaml}\n{err}"
            );
        };
        // Both spellings at once.
        refused(
            &split("  judge:\n    kind: oneharness\n  judges:\n    - kind: oneharness\n"),
            "`judge` and `judges` are exclusive",
        );
        // An empty list.
        refused(
            &split("  judges: []\n"),
            "`judges` must name at least one judge",
        );
        // Neither.
        refused(&split(""), "`judge` (or `judges`)");
        // A malformed label, and a duplicate one — explicit, and colliding with a
        // defaulted one.
        refused(
            &split("  judges:\n    - kind: oneharness\n      label: 'no spaces'\n"),
            "`label` `no spaces` is invalid",
        );
        refused(
            &split(
                "  judges:\n    - kind: oneharness\n      label: same\n    \
                 - kind: command\n      command: [j]\n      label: same\n",
            ),
            "`label` `same` is used by more than one judge",
        );
        refused(
            &split(
                "  judges:\n    - kind: oneharness\n    \
                 - kind: command\n      command: [j]\n      label: oneharness\n",
            ),
            "`label` `oneharness` is used by more than one judge",
        );
        // A label anywhere but a judge entry.
        refused(
            "task: x\nprovider:\n  kind: oneharness\n  label: top\n",
            "`label` is only valid on a judge entry",
        );
        refused(
            &split("  label: top\n  judge:\n    kind: oneharness\n"),
            "`label` is only valid on a judge entry",
        );
        refused(
            "task: x\nprovider:\n  kind: split\n  skill:\n    kind: oneharness\n    \
             label: agent\n  judge:\n    kind: oneharness\n",
            "`label` is only valid on a judge entry",
        );
        // `judges` under a kind that has no judge side.
        refused(
            "task: x\nprovider:\n  kind: oneharness\n  judges:\n    - kind: oneharness\n",
            "`judges` is not valid under provider kind `oneharness`",
        );
        refused(
            "task: x\nprovider:\n  kind: command\n  command: [p]\n  judges:\n    - kind: oneharness\n",
            "`judges` is not valid under provider kind `command`",
        );
    }

    #[test]
    fn a_judge_list_that_can_score_accepts_numeric_evals_and_an_assessment() {
        // Every kind this build can name scores numbers and writes prose, so the
        // refusal for a list that cannot is not reachable yet; what is provable
        // is that a list holding a `command` judge is accepted beside both.
        let yaml = "task: x\nprovider:\n  kind: split\n  skill:\n    kind: oneharness\n  \
                    judges:\n    - kind: command\n      command: [j]\n\
                    evals:\n  - criterion: q\n    kind: numeric\n\
                    assessment: follow-ups\n";
        let plan = Config::from_yaml(yaml).unwrap().into_plan().unwrap();
        assert_eq!(plan.assessment.as_deref(), Some("follow-ups"));
        assert_eq!(plan.evals.len(), 1);
    }

    #[test]
    fn an_unnamed_binary_leaves_the_turn_in_process() {
        // The default, and the one a config that says nothing about `bin` gets:
        // no `oneharness` process, and nothing needed on PATH. Naming one is the
        // opt-in to spawning, which is the only way to keep `Report::processes`.
        let plan = Config::from_yaml("task: x\nprovider:\n  kind: oneharness\n")
            .unwrap()
            .into_plan()
            .unwrap();
        assert!(matches!(
            plan.provider,
            ProviderSpec::Oneharness { bin: None, .. }
        ));
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
        assert!(
            matches!(plan.provider, ProviderSpec::Oneharness { bin, .. } if bin.as_deref() == Some("custom"))
        );
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
