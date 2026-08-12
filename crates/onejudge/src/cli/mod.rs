//! The `onejudge` command line: drive a harness through a simulated-user loop to
//! complete one task, configured by YAML. This is the standalone-tool surface on
//! top of the engine (issue #8) — a different framing from a test framework: run
//! *one task* to completion, not a matrix of cases-as-assertions.
//!
//! The binary entrypoint (`src/bin/onejudge.rs`) stays thin; the logic lives here
//! and in the `config` / `provider` submodules so it is covered by the gate.
//! [`run`] loads and validates the config, builds the provider, drives the loop,
//! scores the evals, and renders the result — human-readable by default, or the
//! versioned [`Report`] contract under `--format json`.

mod config;
mod provider;

use std::io::{Read as _, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};

use crate::{Engine, JudgeKind, JudgeValue, NamedVerdict, Report, StreamEvent, Usage};

pub use config::{Config, Eval, EvalKind, Overrides, Plan, ProviderKind, ProviderSpec};
pub use provider::AnyProvider;

/// The default config filename, looked up in the working directory when `run` is
/// given no explicit config path.
const DEFAULT_CONFIG: &str = "onejudge.yaml";

/// A starter config, written by `onejudge init` and printed by `onejudge schema`.
/// It doubles as the documentation of the config surface.
pub const STARTER_CONFIG: &str = include_str!("starter.yaml");

/// The oldest `oneharness` **CLI** this build works against, as told to an
/// operator: the first release whose `run --control` opens a turn-control socket
/// and whose `interrupt --input` can redirect the turn onejudge reports the
/// address of (`docs/control.md`).
///
/// One source for every message the CLI prints, and drift-gated in this module's
/// tests against the `oneharness-core` requirement in the workspace manifest and
/// against the prose that repeats it — so bumping the pin without bumping what an
/// operator is told to install fails the gate instead of shipping a wrong number.
///
/// It is a *lower bound on* the core pin rather than equal to it, because the two
/// crates version independently: there is no `oneharness` 0.6.13, and the 0.6.14
/// CLI is the one that carries `oneharness-core` 0.6.13. Naming the core version
/// here would tell an operator to install a CLI that was never published.
const MIN_ONEHARNESS: &str = "0.6.14";

/// Errors surfaced by the CLI. Config/validation problems are separated from IO
/// and engine failures so the entrypoint can exit with a fitting code.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// A malformed or inconsistent config (bad YAML, unknown key, missing task,
    /// misplaced provider field, …).
    #[error("config error: {0}")]
    Config(String),
    /// An IO failure reading the config / task / writing the output.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// A failure from the engine / provider while driving the run.
    #[error("run failed: {0}")]
    Engine(#[from] crate::Error),
}

/// `onejudge` — drive a harness through a simulated-user loop to complete a task.
#[derive(Debug, Parser)]
#[command(name = "onejudge", version, about, long_about = None)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// The `onejudge` subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Drive one task to completion via a simulated-user loop.
    Run(RunArgs),
    /// Write a starter `onejudge.yaml`.
    Init(InitArgs),
    /// Print the annotated config schema.
    Schema,
}

/// Arguments for `onejudge run`. Each flag also has a matching `ONEJUDGE_*`
/// environment override; precedence is flags > env > config file > defaults.
#[derive(Debug, Parser)]
pub struct RunArgs {
    /// The config file (defaults to `./onejudge.yaml` when present).
    pub config: Option<PathBuf>,

    /// The judge / simulated-user oneharness config file (`oneharness run --config
    /// <path>`); default `oneharness.judge.toml`.
    #[arg(long)]
    pub judge_config: Option<String>,
    /// A skill directory (containing `SKILL.md`) whose body seeds the system
    /// prompt. Resolved relative to the working directory.
    #[arg(long)]
    pub skill: Option<PathBuf>,
    /// Extra system-prompt text for the harness (prepended to the skill body).
    #[arg(long)]
    pub system_prompt: Option<String>,
    /// The task to drive to completion (`-` reads stdin).
    #[arg(long)]
    pub task: Option<String>,
    /// The simulated user's persona.
    #[arg(long)]
    pub persona: Option<String>,
    /// The completion condition the simulated user drives toward.
    #[arg(long)]
    pub done_when: Option<String>,
    /// The assistant-turn cap.
    #[arg(long)]
    pub max_turns: Option<u32>,
    /// The caller-owned session name threaded across turns.
    #[arg(long)]
    pub session: Option<String>,
    /// Override just the provider backend kind.
    #[arg(long, value_enum)]
    pub provider: Option<ProviderKind>,

    /// The output format.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    pub format: Format,
    /// Publish the run on stdout as the streamed protocol (`docs/streaming.md`):
    /// one `{"type":"event",…}` line per tool event as it happens, then a terminal
    /// `{"type":"result","report":{…}}` line. Requires `--format json`, and is
    /// incompatible with `--output` (the stream *is* stdout).
    #[arg(long)]
    pub stream: bool,
    /// Write the result here instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
}

/// Arguments for `onejudge init`.
#[derive(Debug, Parser)]
pub struct InitArgs {
    /// Where to write the starter config (default `./onejudge.yaml`).
    #[arg(default_value = DEFAULT_CONFIG)]
    pub path: PathBuf,
    /// Overwrite existing files (the `onejudge.yaml` and both scaffolded
    /// `oneharness` configs).
    #[arg(long)]
    pub force: bool,
    /// The `oneharness` binary used to scaffold `oneharness.toml` /
    /// `oneharness.judge.toml` (default `oneharness`).
    #[arg(long, default_value = "oneharness")]
    pub oneharness_bin: String,
}

/// The `--format` choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// A readable transcript + result summary.
    Human,
    /// The versioned [`Report`] JSON contract.
    Json,
}

/// Run the CLI to completion, returning the process exit code.
///
/// # Errors
/// [`CliError`] for a bad config, an IO failure, or an engine/provider error.
pub fn run(cli: Cli) -> Result<i32, CliError> {
    match cli.command {
        Command::Run(args) => run_task(args),
        Command::Init(args) => init(args),
        Command::Schema => {
            print!("{STARTER_CONFIG}");
            Ok(0)
        }
    }
}

/// Load + resolve the config, drive the run, render it, and return the exit code.
fn run_task(args: RunArgs) -> Result<i32, CliError> {
    let RunArgs {
        config,
        judge_config,
        skill,
        system_prompt,
        task,
        persona,
        done_when,
        max_turns,
        session,
        provider,
        format,
        stream,
        output,
    } = args;

    // Validate the output surface before doing any work: `--stream` publishes the
    // NDJSON protocol on stdout, so a human rendering or a file destination would
    // silently discard the very thing that was asked for.
    if stream {
        if format != Format::Json {
            return Err(CliError::Config(
                "--stream publishes the JSON streamed protocol; pass --format json".into(),
            ));
        }
        if output.is_some() {
            return Err(CliError::Config(
                "--stream writes the event stream to stdout; drop --output".into(),
            ));
        }
    }

    let cfg_path = resolve_config_path(config.as_ref());
    let mut cfg = load_config(cfg_path.as_ref())?;
    // A `skill:` in the config file is relative to that file's directory; a
    // `--skill` flag (applied below) stays relative to the working directory.
    if let Some(base) = cfg_path.as_ref().and_then(|p| p.parent()) {
        rebase_skill(&mut cfg, base);
    }
    let task = task.map(resolve_task).transpose()?;

    // Precedence: flags > `ONEJUDGE_*` env > config file > defaults. Apply the env
    // layer first so a flag (applied next) wins over it, and both win over the file.
    cfg.apply(Overrides::from_env(|key| std::env::var(key).ok())?);
    cfg.apply(Overrides {
        judge_config,
        skill,
        system_prompt,
        task,
        persona,
        done_when,
        max_turns,
        session,
        provider_kind: provider,
    });

    let plan = cfg.into_plan()?;

    if stream {
        return run_streamed(plan);
    }

    // Live tool events go to stderr so a `--format json` (or redirected) run keeps
    // a clean stdout; the rendered result goes to stdout / `--output`.
    let mut progress = |line: &str| {
        eprintln!("{line}");
    };
    let summary = match run_plan_reporting_failure(plan, format, &mut progress) {
        Ok(summary) => summary,
        Err(failure) => {
            // A failed run produces no report, but it does produce attribution —
            // which harness identity refused, on which side. Under `--format json`
            // that is written where the report would have gone, so a programmatic
            // caller never has to parse it back out of a human message.
            if format == Format::Json {
                write_output(output.as_ref(), &render_failure_json(&failure)?)?;
            }
            return Err(failure.error);
        }
    };

    let rendered = match format {
        Format::Human => render_human(&summary),
        Format::Json => render_json(&summary.report)?,
    };
    write_output(output.as_ref(), &rendered)?;

    Ok(exit_code(&summary))
}

/// Drive `plan` while republishing it on stdout as the streamed protocol: one
/// NDJSON `event` line per tool event the instant it is observed, then the
/// terminal `result` line carrying the versioned [`Report`].
///
/// Each line is flushed as it is written — a consumer reading this pipe to watch a
/// long turn learns nothing from a line still sitting in our buffer.
fn run_streamed(plan: Plan) -> Result<i32, CliError> {
    let mut stdout = std::io::stdout();
    let mut failure = None;
    let summary = match run_plan_streaming_reporting_failure(plan, &mut |event| {
        if let Err(e) = write_line(&mut stdout, &StreamLine::Event(event)) {
            // Stop the run rather than keep burning harness calls into a pipe that
            // no longer accepts them (a consumer that hung up mid-turn).
            failure = Some(e);
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }) {
        Ok(summary) => summary,
        Err(run_failure) => {
            // stdout is the `event* result EOF` protocol, so a failure cannot be
            // published there without inventing a third envelope every consumer
            // would have to learn. It goes to stderr as ONE compact JSON line
            // instead — still machine-readable, still line-oriented, and the
            // protocol on stdout stays exactly as documented.
            if let Ok(json) = serde_json::to_string(&FailureReport::new(&run_failure)) {
                eprintln!("{json}");
            }
            return Err(run_failure.error);
        }
    };
    if let Some(e) = failure {
        return Err(e);
    }
    write_line(
        &mut stdout,
        &StreamLine::Result {
            report: &summary.report,
        },
    )?;
    Ok(exit_code(&summary))
}

/// One line of the outbound stream — the same two `type`-tagged envelopes onejudge
/// accepts *from* a streamed provider, so a consumer speaks one protocol in both
/// directions. An `Event` line is the `"type"` tag wrapped around exactly
/// [`StreamEvent`]'s fields; the terminal `Result` line's `report` is byte-for-byte
/// the versioned [`Report`] a buffered `--format json` run prints.
#[derive(serde::Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum StreamLine<'a> {
    Event(&'a StreamEvent<'a>),
    Result { report: &'a Report },
}

/// Serialize one NDJSON line and flush it — a consumer watching a long turn learns
/// nothing from a line still sitting in this process's buffer.
fn write_line(out: &mut impl Write, line: &StreamLine<'_>) -> Result<(), CliError> {
    let json = serde_json::to_string(line)
        .map_err(|e| CliError::Config(format!("could not serialize a stream line: {e}")))?;
    out.write_all(json.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

/// The config file a run reads: the explicit `path`, or `./onejudge.yaml` when it
/// exists. `None` means no file — flags alone drive the run.
fn resolve_config_path(path: Option<&PathBuf>) -> Option<PathBuf> {
    match path {
        Some(p) => Some(p.clone()),
        None => {
            let default = PathBuf::from(DEFAULT_CONFIG);
            default.exists().then_some(default)
        }
    }
}

/// Read the config from `path`, or start from an empty config (so flags alone can
/// drive a run) when there is none.
fn load_config(path: Option<&PathBuf>) -> Result<Config, CliError> {
    match path {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| {
                CliError::Config(format!("could not read config `{}`: {e}", p.display()))
            })?;
            Config::from_yaml(&text)
        }
        None => Ok(Config::default()),
    }
}

/// Resolve a config-file `skill:` path relative to the config's own directory, so
/// `onejudge run sub/onejudge.yaml` finds `sub/skills/x` from `skill: skills/x`. An
/// absolute path is left as-is, and an empty `base` (a bare filename config) is a
/// no-op.
fn rebase_skill(cfg: &mut Config, base: &Path) {
    if base.as_os_str().is_empty() {
        return;
    }
    if let Some(rel) = cfg.skill.take() {
        cfg.skill = Some(if rel.is_relative() {
            base.join(rel)
        } else {
            rel
        });
    }
}

/// Resolve `--task`, reading stdin when it is exactly `-`.
fn resolve_task(task: String) -> Result<String, CliError> {
    if task == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        Ok(buf.trim().to_string())
    } else {
        Ok(task)
    }
}

/// Write `content` to `output` (a file) or stdout.
fn write_output(output: Option<&PathBuf>, content: &str) -> Result<(), CliError> {
    match output {
        Some(path) => {
            std::fs::write(path, content)?;
        }
        None => {
            let mut stdout = std::io::stdout();
            stdout.write_all(content.as_bytes())?;
        }
    }
    Ok(())
}

/// Scaffold a run: the two oneharness config files (via `oneharness init`, which
/// owns harness/model selection) plus the loop-only `onejudge.yaml`.
fn init(args: InitArgs) -> Result<i32, CliError> {
    // Check the onejudge.yaml target first so we fail before scaffolding anything.
    if args.path.exists() && !args.force {
        return Err(CliError::Config(format!(
            "{} already exists (use --force to overwrite)",
            args.path.display()
        )));
    }

    // Harness/model selection lives in oneharness's own config files now, so
    // scaffold them by shelling out to `oneharness init` (see [`MIN_ONEHARNESS`]):
    // the discovered `oneharness.toml` drives the agent side, and
    // `oneharness.judge.toml` drives the judge / simulated-user side
    // (`provider.judge_config`).
    oneharness_init(&args.oneharness_bin, "oneharness.toml", args.force)?;
    oneharness_init(&args.oneharness_bin, "oneharness.judge.toml", args.force)?;

    std::fs::write(&args.path, STARTER_CONFIG)?;
    println!("wrote {}", args.path.display());
    Ok(0)
}

/// Shell out to `oneharness init <path>` to scaffold one oneharness config,
/// surfacing a missing binary or a non-zero exit as an actionable error.
fn oneharness_init(bin: &str, path: &str, force: bool) -> Result<(), CliError> {
    let mut cmd = std::process::Command::new(bin);
    cmd.arg("init").arg(path);
    if force {
        cmd.arg("--force");
    }
    let output = cmd.output().map_err(|e| {
        CliError::Config(format!(
            "could not run `{bin} init {path}`: {e}. Is oneharness ({MIN_ONEHARNESS}+) installed \
             and on PATH? Install it or pass --oneharness-bin <path>."
        ))
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::Config(format!(
            "`{bin} init {path}` failed: {}",
            stderr.trim()
        )));
    }
    // Relay oneharness's own confirmation line (e.g. "wrote oneharness.toml").
    print!("{}", String::from_utf8_lossy(&output.stdout));
    Ok(())
}

// --- The run driver (pure of arg parsing / IO, so it is unit-testable) -----

/// The structured result of one run: the versioned report plus the derived
/// status the exit code and human rendering read.
pub struct RunSummary {
    /// The versioned report (transcript + verdicts + usage).
    pub report: Report,
    /// Whether the task completed (the `done_when` held, or — without one — the
    /// loop ended before the turn cap).
    pub completed: bool,
    /// Whether the multi-turn loop stopped by hitting the turn cap.
    pub hit_max_turns: bool,
    /// The turn cap in effect.
    pub max_turns: u32,
    /// The completion condition and whether it was satisfied, if one was set.
    pub done_when: Option<DoneWhen>,
    /// One entry per configured eval, in order.
    pub eval_results: Vec<EvalResult>,
}

/// A `done_when` completion check re-judged against the finished transcript.
pub struct DoneWhen {
    /// The completion criterion.
    pub criterion: String,
    /// Whether the judge decided it holds.
    pub satisfied: bool,
}

/// An eval's verdict, carrying the kind-specific payload so an invalid
/// combination (a boolean with a score, a mismatched kind) is unrepresentable.
pub enum EvalOutcome {
    /// A boolean eval and whether it passed. Gates the exit code.
    Boolean(bool),
    /// A numeric eval and its score on the configured scale (report-only).
    Numeric(f64),
}

/// One eval's outcome.
pub struct EvalResult {
    /// The criterion scored.
    pub criterion: String,
    /// The verdict (boolean pass/fail or numeric score).
    pub outcome: EvalOutcome,
    /// The judge's stated reason.
    pub reason: String,
}

/// A run that did not produce a report, with whatever the provider had already
/// recorded when it failed.
///
/// Returned boxed (`Result<_, Box<RunFailure>>`) because the telemetry it carries
/// is far larger than a summary, and every success would otherwise pay for it.
///
/// A failure is exactly the case a caller most needs attribution for — *which*
/// harness identity refused, on *which* side of the conversation — and a failed
/// run produces no [`Report`] to carry it. So the telemetry is returned alongside
/// the error, and `onejudge run --format json` renders both as a
/// [`FailureReport`] rather than leaving stdout empty.
#[derive(Debug)]
pub struct RunFailure {
    /// Why the run did not complete.
    pub error: CliError,
    /// Timing, usage, and per-invocation harness attribution recorded before the
    /// failure; `None` when nothing ran at all (a config or provider-build error).
    pub telemetry: Option<crate::Telemetry>,
    /// The processes the failed run had already spawned, and the embedder-owned
    /// group each was placed in — what a caller cleaning up after the failure needs
    /// to name. Empty when nothing was spawned.
    pub processes: Vec<crate::SpawnedProcess>,
}

impl From<CliError> for Box<RunFailure> {
    fn from(error: CliError) -> Self {
        Box::new(RunFailure {
            error,
            telemetry: None,
            processes: Vec::new(),
        })
    }
}

/// Drive `plan` to completion, re-judge its `done_when`, score its evals, and
/// bundle everything into a [`RunSummary`]. `progress` receives a line per tool
/// event during a `Human`-format run (streamed); a `Json` run is buffered.
///
/// Every process this spawns is offered to the plan's
/// [`spawn_hook`](Plan::with_spawn_hook), if an embedder installed one.
///
/// # Errors
/// [`CliError::Engine`] on a provider/engine failure; [`CliError::Config`] if the
/// provider cannot be built.
pub fn run_plan(
    plan: Plan,
    format: Format,
    progress: &mut dyn FnMut(&str),
) -> Result<RunSummary, CliError> {
    run_plan_reporting_failure(plan, format, progress).map_err(|failure| failure.error)
}

/// [`run_plan`], additionally returning the telemetry a *failed* run had recorded
/// — the only place harness attribution for a failed invocation is reachable.
///
/// # Errors
/// As [`run_plan`], wrapped in a [`RunFailure`].
pub fn run_plan_reporting_failure(
    plan: Plan,
    format: Format,
    progress: &mut dyn FnMut(&str),
) -> Result<RunSummary, Box<RunFailure>> {
    match format {
        Format::Human => execute(
            plan,
            Some(&mut |ev: &StreamEvent<'_>| {
                progress(&format!("· turn {} — {}", ev.turn, ev.event.summary()));
                ControlFlow::Continue(())
            }),
        ),
        Format::Json => execute(plan, None),
    }
}

/// The sink a streaming run delivers each live [`StreamEvent`] to. Returning
/// [`ControlFlow::Break`] short-circuits the run.
pub type EventSink<'a> = dyn FnMut(&StreamEvent<'_>) -> ControlFlow<()> + 'a;

/// Drive `plan` exactly as [`run_plan`] does, delivering each live tool event to
/// `on_event` as a typed [`StreamEvent`] instead of a rendered line. Returning
/// [`ControlFlow::Break`] short-circuits the run (the summary then reflects a
/// stopped-early outcome). This is what `onejudge run --stream` publishes and what
/// an SDK reads to watch a long turn while it is still running.
///
/// # Errors
/// As [`run_plan`].
pub fn run_plan_streaming(
    plan: Plan,
    on_event: &mut EventSink<'_>,
) -> Result<RunSummary, CliError> {
    execute(plan, Some(on_event)).map_err(|failure| failure.error)
}

/// [`run_plan_streaming`], additionally returning the telemetry a *failed* run had
/// recorded. See [`RunFailure`].
///
/// # Errors
/// As [`run_plan_streaming`], wrapped in a [`RunFailure`].
pub fn run_plan_streaming_reporting_failure(
    plan: Plan,
    on_event: &mut EventSink<'_>,
) -> Result<RunSummary, Box<RunFailure>> {
    execute(plan, Some(on_event))
}

/// The one run driver both entry points share: `None` runs the buffered engine
/// loop, `Some(sink)` the streaming one.
///
/// The engine's telemetry is read once the loop has finished **either way**, so a
/// failure carries the same per-invocation harness attribution a success does.
fn execute(
    plan: Plan,
    on_event: Option<&mut EventSink<'_>>,
) -> Result<RunSummary, Box<RunFailure>> {
    let Plan {
        provider,
        settings,
        conversation,
        evals,
        done_when,
        assessment,
        spawn_hook,
    } = plan;

    let multi_turn = conversation.user.is_some();
    let max_turns = conversation
        .user
        .as_ref()
        .and_then(|u| u.max_turns)
        .unwrap_or(settings.max_turns);

    // An embedder that drives a plan never builds the backend itself, so the plan is
    // where its spawn hook has to reach the processes the run creates — every one of
    // them, including both sides of a two-party `split`. Without a hook the backend
    // is built exactly as before.
    let backend = match spawn_hook {
        Some(hook) => AnyProvider::build(&provider)?.with_spawn_hook(hook),
        None => AnyProvider::build(&provider)?,
    };
    let engine = Engine::new(&backend, settings);
    // Read the engine's telemetry once the loop is done, whichever way it ended, so
    // a failure still carries which harness identities the run attempted.
    let drive = || -> Result<RunSummary, CliError> {
        let mut outcome = match on_event {
            Some(sink) => engine.run_streaming(&conversation, sink)?,
            None => engine.run(&conversation)?,
        };

        let mut verdicts: Vec<NamedVerdict> = Vec::new();

        // Re-judge the completion condition against the FINAL transcript: this is the
        // authoritative "did the task actually complete?" signal that drives the exit
        // code (the loop's own mid-run check can be preempted by the turn cap).
        let done = match &done_when {
            Some(criterion) => {
                let verdict = engine.judge_boolean(criterion, &outcome.transcript)?;
                let satisfied = matches!(verdict.value, JudgeValue::Bool(true));
                verdicts.push(NamedVerdict::new(
                    criterion.clone(),
                    JudgeKind::Boolean,
                    verdict,
                ));
                Some(DoneWhen {
                    criterion: criterion.clone(),
                    satisfied,
                })
            }
            None => None,
        };

        let hit_max_turns =
            multi_turn && outcome.transcript.assistant_turns() >= max_turns as usize;
        let completed = match &done {
            Some(d) => d.satisfied,
            None => !hit_max_turns,
        };

        let mut eval_results = Vec::with_capacity(evals.len());
        for eval in &evals {
            let result = match eval.kind {
                EvalKind::Boolean => {
                    let verdict = engine.judge_boolean(&eval.criterion, &outcome.transcript)?;
                    let passed = matches!(verdict.value, JudgeValue::Bool(true));
                    let reason = verdict.reason.clone();
                    verdicts.push(NamedVerdict::new(
                        eval.criterion.clone(),
                        JudgeKind::Boolean,
                        verdict,
                    ));
                    EvalResult {
                        criterion: eval.criterion.clone(),
                        outcome: EvalOutcome::Boolean(passed),
                        reason,
                    }
                }
                EvalKind::Numeric { scale: (min, max) } => {
                    let verdict =
                        engine.judge_numeric(&eval.criterion, min, max, &outcome.transcript)?;
                    // A numeric query yields a number; treat a contract-violating bool
                    // as the scale floor rather than inventing a separate empty state.
                    let score = match verdict.value {
                        JudgeValue::Number(n) => n,
                        JudgeValue::Bool(_) => min,
                    };
                    let reason = verdict.reason.clone();
                    verdicts.push(NamedVerdict::new(
                        eval.criterion.clone(),
                        JudgeKind::Numeric,
                        verdict,
                    ));
                    EvalResult {
                        criterion: eval.criterion.clone(),
                        outcome: EvalOutcome::Numeric(score),
                        reason,
                    }
                }
            };
            eval_results.push(result);
        }

        let assessment = match assessment {
            Some(prompt) => {
                let result = engine.assess(&prompt, &outcome.transcript)?;
                if let Some(usage) = result.usage {
                    outcome.usage.get_or_insert_with(Usage::default).add(&usage);
                }
                Some(result.text)
            }
            None => None,
        };
        outcome.telemetry = engine.telemetry();
        outcome.processes = engine.spawned_processes();
        let report = outcome.into_report_with_assessment(verdicts, assessment);

        Ok(RunSummary {
            report,
            completed,
            hit_max_turns,
            max_turns,
            done_when: done,
            eval_results,
        })
    };
    drive().map_err(|error| {
        Box::new(RunFailure {
            error,
            telemetry: engine.telemetry(),
            processes: engine.spawned_processes(),
        })
    })
}

/// The process exit code for a run: `0` when the task completed and every boolean
/// eval passed, else `1`. Numeric evals are score-and-report — they never fail
/// the run (there is no threshold to fail against).
#[must_use]
pub fn exit_code(summary: &RunSummary) -> i32 {
    let evals_pass = summary
        .eval_results
        .iter()
        .all(|r| !matches!(r.outcome, EvalOutcome::Boolean(false)));
    if summary.completed && evals_pass {
        0
    } else {
        1
    }
}

/// Render the human-readable result: the conversation (with tool actions), the
/// completion status, usage, and each eval verdict.
#[must_use]
pub fn render_human(summary: &RunSummary) -> String {
    let mut out = String::new();
    out.push_str("=== Conversation ===\n");
    out.push_str(&crate::render_transcript(
        &summary.report.transcript.messages,
        true,
    ));
    out.push_str("\n\n=== Result ===\n");

    let status = if summary.completed {
        "completed".to_string()
    } else if summary.hit_max_turns {
        format!("incomplete — hit the turn cap ({})", summary.max_turns)
    } else {
        "incomplete".to_string()
    };
    out.push_str(&format!("Status: {status}\n"));
    out.push_str(&format!(
        "Turns:  {} assistant turn(s)\n",
        summary.report.transcript.assistant_turns()
    ));
    if let Some(done) = &summary.done_when {
        out.push_str(&format!(
            "Completion: \"{}\" — {}\n",
            done.criterion,
            if done.satisfied {
                "satisfied"
            } else {
                "not satisfied"
            }
        ));
    }
    out.push_str(&format!(
        "Usage:  {}\n",
        render_usage(summary.report.usage.as_ref())
    ));

    if !summary.eval_results.is_empty() {
        out.push_str("\n=== Evals ===\n");
        for r in &summary.eval_results {
            out.push_str(&render_eval(r));
            out.push('\n');
        }
    }
    if let Some(assessment) = &summary.report.assessment {
        out.push_str("\n=== Assessment ===\n");
        out.push_str(assessment);
        out.push('\n');
    }
    out
}

/// One eval line for the human report.
fn render_eval(r: &EvalResult) -> String {
    let mark = match r.outcome {
        EvalOutcome::Boolean(true) => "[PASS]".to_string(),
        EvalOutcome::Boolean(false) => "[FAIL]".to_string(),
        EvalOutcome::Numeric(score) => format!("[{score}]"),
    };
    let reason = if r.reason.is_empty() {
        String::new()
    } else {
        format!(" — {}", r.reason)
    };
    format!("{mark} {}{reason}", r.criterion)
}

/// A compact usage line.
fn render_usage(usage: Option<&Usage>) -> String {
    match usage {
        None => "none reported".to_string(),
        Some(u) => {
            let mut parts = Vec::new();
            if let Some(i) = u.input_tokens {
                parts.push(format!("input={i}"));
            }
            if let Some(o) = u.output_tokens {
                parts.push(format!("output={o}"));
            }
            if let Some(r) = u.cache_read_tokens {
                parts.push(format!("cache_read={r}"));
            }
            if let Some(w) = u.cache_write_tokens {
                parts.push(format!("cache_write={w}"));
            }
            if let Some(c) = u.cost_usd {
                parts.push(format!("cost=${c:.4}"));
            }
            if parts.is_empty() {
                "none reported".to_string()
            } else {
                parts.join(" ")
            }
        }
    }
}

/// The machine-readable document a `--format json` run writes when it produced no
/// [`Report`]: why it failed, and the telemetry — including per-invocation harness
/// attribution — recorded before it did.
///
/// Stamped with the same [`SCHEMA_VERSION`](crate::SCHEMA_VERSION) as a report, so
/// one number describes both halves of the CLI's JSON surface. Additive: before
/// this existed a failed `--format json` run wrote nothing at all.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
pub struct FailureReport {
    /// The contract version this document was serialized under.
    pub schema_version: u32,
    /// Why the run did not complete.
    pub error: FailureDetail,
    /// Timing, usage, and per-invocation harness attribution recorded before the
    /// failure; absent when nothing ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<crate::Telemetry>,
    /// The processes the failed run had already spawned, with the embedder-owned
    /// group each was placed in. Absent when nothing was spawned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processes: Vec<crate::SpawnedProcess>,
}

/// The failure itself, with the classification a caller branches on.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
pub struct FailureDetail {
    /// The human-readable failure, exactly as it is printed on stderr.
    pub message: String,
    /// The provider failure category, when the failure came from a provider and
    /// was classified (`auth`, `quota`, `timeout`, `spawn`, `protocol`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<crate::ProviderErrorKind>,
}

impl FailureReport {
    /// Build the document for `failure`.
    #[must_use]
    pub fn new(failure: &RunFailure) -> Self {
        Self {
            schema_version: crate::SCHEMA_VERSION,
            error: FailureDetail {
                message: failure.error.to_string(),
                kind: match &failure.error {
                    CliError::Engine(error) => error.kind(),
                    CliError::Config(_) | CliError::Io(_) => None,
                },
            },
            telemetry: failure.telemetry.clone(),
            processes: failure.processes.clone(),
        }
    }
}

/// Serialize a [`FailureReport`] as pretty JSON.
fn render_failure_json(failure: &RunFailure) -> Result<String, CliError> {
    let mut json = serde_json::to_string_pretty(&FailureReport::new(failure))
        .map_err(|e| CliError::Config(format!("could not serialize the failure report: {e}")))?;
    json.push('\n');
    Ok(json)
}

/// Serialize the versioned report as pretty JSON.
fn render_json(report: &Report) -> Result<String, CliError> {
    let mut json = serde_json::to_string_pretty(report)
        .map_err(|e| CliError::Config(format!("could not serialize report: {e}")))?;
    json.push('\n');
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Transcript;

    #[test]
    fn the_advertised_oneharness_minimum_tracks_the_pin_and_the_prose() {
        // The version an operator is told to install is restated in a manifest
        // requirement, in CLI messages, and in prose. Only the manifest actually
        // constrains the build, so it is the source; this is the gate that keeps
        // the other two from drifting off it — which they had.
        //
        // The relation is `>=`, not `==`: the CLI and its engine crate version
        // independently (no `oneharness` 0.6.13 was ever published), so a CLI
        // older than the core it embeds is the drift worth catching, while a newer
        // one is simply the release that carries it.
        let manifest = include_str!("../../../../Cargo.toml");
        let pinned = manifest
            .lines()
            .find_map(|line| line.strip_prefix("oneharness-core = "))
            .expect("the workspace pins oneharness-core")
            .trim()
            .trim_matches('"')
            .to_string();
        assert!(
            version_parts(MIN_ONEHARNESS) >= version_parts(&pinned),
            "the advertised oneharness minimum {MIN_ONEHARNESS} is older than the pinned \
             oneharness-core {pinned}, so an operator following it installs a CLI that cannot \
             produce the report this build parses"
        );

        // Everything else that repeats it — prose and rustdoc alike — so a bump
        // cannot land in the code alone. A file that states the minimum and is not
        // on this list is a copy nothing reconciles, which is the drift this gate
        // exists to make impossible.
        for (name, text) in [
            ("README.md", include_str!("../../../../README.md")),
            ("AGENTS.md", include_str!("../../../../AGENTS.md")),
            ("docs/cli.md", include_str!("../../../../docs/cli.md")),
            (
                "docs/control.md",
                include_str!("../../../../docs/control.md"),
            ),
            (
                "docs/live-tier.md",
                include_str!("../../../../docs/live-tier.md"),
            ),
            ("oneharness/mod.rs", include_str!("../oneharness/mod.rs")),
        ] {
            assert!(
                text.contains(MIN_ONEHARNESS),
                "{name} does not mention the advertised oneharness minimum {MIN_ONEHARNESS}"
            );
        }
    }

    /// A `major.minor.patch` string as comparable numbers. Only used by the drift
    /// gate above, where both inputs are release versions this repo writes.
    fn version_parts(version: &str) -> (u32, u32, u32) {
        let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        )
    }

    fn summary(completed: bool, hit_max: bool, evals: Vec<EvalResult>) -> RunSummary {
        RunSummary {
            report: Report::new(Transcript::from_input("hi"), vec![], None, false),
            completed,
            hit_max_turns: hit_max,
            max_turns: 8,
            done_when: None,
            eval_results: evals,
        }
    }

    fn bool_eval(passed: bool) -> EvalResult {
        EvalResult {
            criterion: "it works".into(),
            outcome: EvalOutcome::Boolean(passed),
            reason: "because".into(),
        }
    }

    #[test]
    fn exit_zero_only_when_completed_and_evals_pass() {
        assert_eq!(exit_code(&summary(true, false, vec![])), 0);
        assert_eq!(exit_code(&summary(false, true, vec![])), 1);
        assert_eq!(exit_code(&summary(true, false, vec![bool_eval(true)])), 0);
        assert_eq!(exit_code(&summary(true, false, vec![bool_eval(false)])), 1);
    }

    #[test]
    fn numeric_eval_never_fails_the_run() {
        let numeric = EvalResult {
            criterion: "quality".into(),
            outcome: EvalOutcome::Numeric(2.0),
            reason: String::new(),
        };
        assert_eq!(exit_code(&summary(true, false, vec![numeric])), 0);
    }

    #[test]
    fn human_render_shows_status_and_evals() {
        let s = summary(false, true, vec![bool_eval(false)]);
        let out = render_human(&s);
        assert!(out.contains("=== Conversation ==="));
        assert!(out.contains("hit the turn cap (8)"));
        assert!(out.contains("[FAIL] it works"));
    }

    #[test]
    fn human_render_shows_completion_line() {
        let mut s = summary(true, false, vec![]);
        s.done_when = Some(DoneWhen {
            criterion: "tests pass".into(),
            satisfied: true,
        });
        let out = render_human(&s);
        assert!(out.contains("Completion: \"tests pass\" — satisfied"));
        assert!(out.contains("Status: completed"));
    }

    #[test]
    fn render_eval_marks_each_kind() {
        assert!(render_eval(&bool_eval(true)).starts_with("[PASS]"));
        assert!(render_eval(&bool_eval(false)).starts_with("[FAIL]"));
        let numeric = EvalResult {
            criterion: "q".into(),
            outcome: EvalOutcome::Numeric(4.5),
            reason: String::new(),
        };
        assert!(render_eval(&numeric).starts_with("[4.5]"));
    }

    #[test]
    fn usage_render_summarizes_or_reports_none() {
        assert_eq!(render_usage(None), "none reported");
        let u = Usage {
            input_tokens: Some(10),
            output_tokens: Some(3),
            cache_read_tokens: Some(21),
            cache_write_tokens: Some(4),
            cost_usd: Some(0.0123),
        };
        let rendered = render_usage(Some(&u));
        assert!(rendered.contains("input=10"));
        assert!(rendered.contains("output=3"));
        assert!(rendered.contains("cache_read=21"));
        assert!(rendered.contains("cache_write=4"));
        assert!(rendered.contains("cost=$0.0123"));
    }

    #[test]
    fn json_render_is_the_versioned_report() {
        let report = Report::new(Transcript::from_input("hi"), vec![], None, false);
        let json = render_json(&report).unwrap();
        assert!(json.contains("\"schema_version\": 8"));
    }

    #[test]
    fn rebase_skill_resolves_relative_against_the_config_dir() {
        let mut cfg = Config::from_yaml("task: x\nskill: skills/greeter\n").unwrap();
        rebase_skill(&mut cfg, Path::new("/proj/cases"));
        assert_eq!(cfg.skill.unwrap(), Path::new("/proj/cases/skills/greeter"));
    }

    #[test]
    fn rebase_skill_leaves_absolute_paths_and_no_skill_alone() {
        // Use a real absolute path so the assertion holds on Windows too (a
        // leading-slash path is not absolute there).
        let abs = std::env::temp_dir().join("greeter-abs");
        let yaml = format!(
            "task: x\nskill: {}\n",
            serde_json::to_string(&abs.to_string_lossy()).unwrap()
        );
        let mut cfg = Config::from_yaml(&yaml).unwrap();
        rebase_skill(&mut cfg, Path::new("/proj"));
        assert_eq!(cfg.skill.unwrap(), abs);

        let mut none = Config::from_yaml("task: x\n").unwrap();
        rebase_skill(&mut none, Path::new("/proj"));
        assert!(none.skill.is_none());
    }

    #[test]
    fn starter_config_parses_and_documents_the_surface() {
        // The starter doubles as `schema` output and must itself be valid.
        let cfg = Config::from_yaml(STARTER_CONFIG).unwrap();
        assert!(cfg.task.is_some());
        assert!(cfg.user.is_some());
    }
}
