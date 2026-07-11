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

use std::io::{Read as _, Write as _};
use std::ops::ControlFlow;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::{Engine, JudgeKind, JudgeValue, NamedVerdict, Report, Usage};

pub use config::{Config, Eval, EvalKind, Overrides, Plan, ProviderKind, ProviderSpec};
pub use provider::AnyProvider;

/// The default config filename, looked up in the working directory when `run` is
/// given no explicit config path.
const DEFAULT_CONFIG: &str = "onejudge.yaml";

/// A starter config, written by `onejudge init` and printed by `onejudge schema`.
/// It doubles as the documentation of the config surface.
pub const STARTER_CONFIG: &str = include_str!("starter.yaml");

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

/// Arguments for `onejudge run`. Flags win over the config file, which wins over
/// defaults.
#[derive(Debug, Parser)]
pub struct RunArgs {
    /// The config file (defaults to `./onejudge.yaml` when present).
    pub config: Option<PathBuf>,

    /// The harness (platform) the agent runs on.
    #[arg(long)]
    pub harness: Option<String>,
    /// The model the agent runs on.
    #[arg(long)]
    pub model: Option<String>,
    /// The model the simulated user and judge run on.
    #[arg(long)]
    pub judge_model: Option<String>,
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
    /// Overwrite an existing file.
    #[arg(long)]
    pub force: bool,
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
        harness,
        model,
        judge_model,
        task,
        persona,
        done_when,
        max_turns,
        session,
        provider,
        format,
        output,
    } = args;

    let mut cfg = load_config(config.as_ref())?;
    let task = task.map(resolve_task).transpose()?;
    cfg.apply(Overrides {
        harness,
        model,
        judge_model,
        task,
        persona,
        done_when,
        max_turns,
        session,
        provider_kind: provider,
    });

    let plan = cfg.into_plan()?;

    // Live tool events go to stderr so a `--format json` (or redirected) run keeps
    // a clean stdout; the rendered result goes to stdout / `--output`.
    let mut progress = |line: &str| {
        eprintln!("{line}");
    };
    let summary = run_plan(plan, format, &mut progress)?;

    let rendered = match format {
        Format::Human => render_human(&summary),
        Format::Json => render_json(&summary.report)?,
    };
    write_output(output.as_ref(), &rendered)?;

    Ok(exit_code(&summary))
}

/// Read the config from `path`, or `./onejudge.yaml` when it exists, else start
/// from an empty config (so flags alone can drive a run).
fn load_config(path: Option<&PathBuf>) -> Result<Config, CliError> {
    let chosen = match path {
        Some(p) => Some(p.clone()),
        None => {
            let default = PathBuf::from(DEFAULT_CONFIG);
            default.exists().then_some(default)
        }
    };
    match chosen {
        Some(p) => {
            let text = std::fs::read_to_string(&p).map_err(|e| {
                CliError::Config(format!("could not read config `{}`: {e}", p.display()))
            })?;
            Config::from_yaml(&text)
        }
        None => Ok(Config::default()),
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

/// Write the starter config to `path`.
fn init(args: InitArgs) -> Result<i32, CliError> {
    if args.path.exists() && !args.force {
        return Err(CliError::Config(format!(
            "{} already exists (use --force to overwrite)",
            args.path.display()
        )));
    }
    std::fs::write(&args.path, STARTER_CONFIG)?;
    println!("wrote {}", args.path.display());
    Ok(0)
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

/// Drive `plan` to completion, re-judge its `done_when`, score its evals, and
/// bundle everything into a [`RunSummary`]. `progress` receives a line per tool
/// event during a `Human`-format run (streamed); a `Json` run is buffered.
///
/// # Errors
/// [`CliError::Engine`] on a provider/engine failure; [`CliError::Config`] if the
/// provider cannot be built.
pub fn run_plan(
    plan: Plan,
    format: Format,
    progress: &mut dyn FnMut(&str),
) -> Result<RunSummary, CliError> {
    let Plan {
        provider,
        settings,
        conversation,
        evals,
        done_when,
    } = plan;

    let multi_turn = conversation.user.is_some();
    let max_turns = conversation
        .user
        .as_ref()
        .and_then(|u| u.max_turns)
        .unwrap_or(settings.max_turns);

    let backend = AnyProvider::build(&provider)?;
    let engine = Engine::new(&backend, settings);

    let outcome = match format {
        Format::Human => engine.run_streaming(&conversation, &mut |ev| {
            progress(&format!("· turn {} — {}", ev.turn, ev.event.summary()));
            ControlFlow::Continue(())
        })?,
        Format::Json => engine.run(&conversation)?,
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

    let hit_max_turns = multi_turn && outcome.transcript.assistant_turns() >= max_turns as usize;
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

    let report = outcome.into_report(verdicts);

    Ok(RunSummary {
        report,
        completed,
        hit_max_turns,
        max_turns,
        done_when: done,
        eval_results,
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
            cost_usd: Some(0.0123),
        };
        let rendered = render_usage(Some(&u));
        assert!(rendered.contains("input=10"));
        assert!(rendered.contains("output=3"));
        assert!(rendered.contains("cost=$0.0123"));
    }

    #[test]
    fn json_render_is_the_versioned_report() {
        let report = Report::new(Transcript::from_input("hi"), vec![], None, false);
        let json = render_json(&report).unwrap();
        assert!(json.contains("\"schema_version\": 1"));
    }

    #[test]
    fn starter_config_parses_and_documents_the_surface() {
        // The starter doubles as `schema` output and must itself be valid.
        let cfg = Config::from_yaml(STARTER_CONFIG).unwrap();
        assert!(cfg.task.is_some());
        assert!(cfg.user.is_some());
    }
}
