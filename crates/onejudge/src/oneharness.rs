//! [`OneharnessProvider`]: the default [`Provider`], which runs each prompt on a
//! real harness through the [`oneharness`](https://github.com/nickderobertis/oneharness)
//! CLI (`oneharness run`, whose report is JSON by default) and parses its report.
//!
//! **Harness/model selection lives in oneharness's config, not onejudge.** The
//! agent side passes no `--harness`/`--model`, so it uses oneharness's discovered
//! default config (`oneharness.toml`). The judge / simulated-user side passes
//! `--config <judge_config>` (default `oneharness.judge.toml`) so it can run on a
//! separately-configured harness/model — again without `--harness`/`--model`.
//! Scaffold both with `onejudge init` (which shells out to `oneharness init`).
//!
//! It targets **oneharness v0.3.20+**: it always threads the uniform `--session
//! <name>` handle (the engine's caller-owned name, mapped to the harness's native
//! session in oneharness's on-disk store), and if a run fails because the harness
//! does not support `--session`, it retries the same call once **without**
//! `--session`, re-inlining the transcript — the graceful degradation that
//! replaces the old up-front capability table. It also depends on `oneharness init`
//! for scaffolding.
//!
//! The pure pieces — argument construction, report parsing, error classification —
//! are separated from the one thin `spawn + wait` shell so they are
//! deterministically unit-tested; the whole path is proven end-to-end against a
//! fake `oneharness` binary in the e2e suite and against a real one in the live
//! tier (`docs/live-tier.md`).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::error::{Error, ProviderErrorKind, Result};
use crate::provider::{
    build_assessment_prompt, build_judge_prompt, build_user_prompt, latest_or_inline,
    parse_verdict, Assessment, AssistantTurn, JudgeQuery, JudgeVerdict, Provider, SkillRef,
    UserTurn,
};
use crate::transcript::{Message, ToolEvent};
use crate::usage::Usage;

/// The default judge/simulated-user oneharness config filename.
const DEFAULT_JUDGE_CONFIG: &str = "oneharness.judge.toml";

/// The stable substring in oneharness's error when a harness cannot bind a
/// `--session` name (its `OneharnessError::SessionUnsupported`). Matching it lets
/// onejudge retry the call without `--session` instead of failing the run.
const SESSION_UNSUPPORTED_MARKER: &str = "does not support --session";

/// Whether `err` is oneharness rejecting `--session` because the harness exposes no
/// session id headlessly — the one failure the provider recovers from (by retrying
/// without `--session`).
fn is_session_unsupported(err: &Error) -> bool {
    err.to_string().contains(SESSION_UNSUPPORTED_MARKER)
}

/// The default [`Provider`]: shells out to the `oneharness` CLI.
pub struct OneharnessProvider {
    bin: String,
    judge_config: Option<PathBuf>,
}

impl Default for OneharnessProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl OneharnessProvider {
    /// A provider that invokes `oneharness` on `PATH`, running the judge and
    /// simulated user under `oneharness.judge.toml` (its default config file).
    #[must_use]
    pub fn new() -> Self {
        Self {
            bin: "oneharness".into(),
            judge_config: Some(PathBuf::from(DEFAULT_JUDGE_CONFIG)),
        }
    }

    /// Override the `oneharness` binary path (e.g. a pinned install, or the fake
    /// binary the e2e suite drives).
    #[must_use]
    pub fn with_bin(mut self, bin: impl Into<String>) -> Self {
        self.bin = bin.into();
        self
    }

    /// Override the oneharness config file the judge and simulated user run under
    /// (default `oneharness.judge.toml`), passed as `oneharness run --config
    /// <path>`. This is where the judge-side harness/model selection lives — onejudge
    /// itself passes no `--harness`/`--model`.
    #[must_use]
    pub fn with_judge_config(mut self, config: impl Into<PathBuf>) -> Self {
        self.judge_config = Some(config.into());
        self
    }

    /// Run a skill turn, threading `session` and — on a `SessionUnsupported`
    /// failure — retrying once without it, re-inlining the transcript.
    fn run_respond(
        &self,
        instructions: &str,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<OneharnessResult> {
        if let Some(name) = session {
            // A continued session only needs the latest user turn.
            let args = respond_args(instructions, Some(name));
            let prompt = latest_or_inline(messages, true);
            match self.run("respond", &args, &prompt) {
                Ok(result) => return Ok(result),
                Err(e) if is_session_unsupported(&e) => {
                    eprintln!(
                        "onejudge: warning — the agent harness does not support --session; \
                         retrying without it (re-inlining the transcript)"
                    );
                }
                Err(e) => return Err(e),
            }
        }
        // Fresh or fallback call: inline the whole conversation, no `--session`.
        let args = respond_args(instructions, None);
        let prompt = latest_or_inline(messages, false);
        self.run("respond", &args, &prompt)
    }

    /// Run a judge/simulated-user turn under the judge config, threading `session`
    /// and — on a `SessionUnsupported` failure — retrying once without it. The
    /// prompt already inlines the whole transcript, so the retry needs no rebuild.
    fn run_judge_side(
        &self,
        op: &str,
        prompt: &str,
        session: Option<&str>,
    ) -> Result<OneharnessResult> {
        if let Some(name) = session {
            let args = judge_side_args(self.judge_config.as_deref(), Some(name));
            match self.run(op, &args, prompt) {
                Ok(result) => return Ok(result),
                Err(e) if is_session_unsupported(&e) => {
                    eprintln!(
                        "onejudge: warning — the judge harness does not support --session; \
                         retrying without it"
                    );
                }
                Err(e) => return Err(e),
            }
        }
        let args = judge_side_args(self.judge_config.as_deref(), None);
        self.run(op, &args, prompt)
    }

    /// Spawn `oneharness run` with `args` and `prompt`, and return the parsed
    /// single-result report. The prompt is passed via stdin (`--prompt-file -`)
    /// so an arbitrarily long transcript never trips the OS argv limit.
    fn run(&self, op: &str, args: &[String], prompt: &str) -> Result<OneharnessResult> {
        let mut child = Command::new(&self.bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                Error::provider_classified(
                    op.to_string(),
                    format!(
                        "could not run `{}`: {e}. Is oneharness installed and on PATH?",
                        self.bin
                    ),
                    ProviderErrorKind::Spawn,
                )
            })?;
        {
            let stdin = child.stdin.as_mut().ok_or_else(|| {
                Error::provider(op.to_string(), "could not open oneharness stdin")
            })?;
            stdin.write_all(prompt.as_bytes()).map_err(|e| {
                Error::provider(op.to_string(), format!("could not write prompt: {e}"))
            })?;
        }
        let output = child.wait_with_output().map_err(|e| {
            Error::provider(op.to_string(), format!("oneharness did not complete: {e}"))
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::provider_classified(
                op.to_string(),
                format!(
                    "oneharness exited with {}: {}",
                    output.status,
                    stderr.trim()
                ),
                ProviderErrorKind::Protocol,
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_report(op, &stdout)
    }
}

/// Build the `oneharness run` args (before the trailing `--prompt-file -`) for a
/// skill turn. Pure and total, so it is unit-tested directly.
///
/// No `--harness`/`--model`: the agent side relies on oneharness's own discovered
/// config (`oneharness.toml`) for harness/model selection.
#[must_use]
fn respond_args(instructions: &str, session: Option<&str>) -> Vec<String> {
    // `oneharness run` emits a JSON report by default; `--compact` makes it a
    // single line. There is no `--format` flag on `run`.
    let mut args = vec![
        "run".into(),
        "--compact".into(),
        "--events".into(),
        "--system".into(),
        instructions.into(),
        "--prompt-file".into(),
        "-".into(),
    ];
    // Always thread the caller-owned session name; the caller retries without it if
    // oneharness reports the harness cannot bind a session.
    if let Some(name) = session {
        args.push("--session".into());
        args.push(name.into());
    }
    args
}

/// Build the `oneharness run` args for a judge or simulated-user turn (no
/// `--system`, no `--events`). Harness/model selection comes from `--config
/// <judge_config>`, not from `--harness`/`--model`.
#[must_use]
fn judge_side_args(judge_config: Option<&Path>, session: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "run".into(),
        "--compact".into(),
        "--prompt-file".into(),
        "-".into(),
    ];
    if let Some(config) = judge_config {
        args.push("--config".into());
        args.push(config.display().to_string());
    }
    if let Some(name) = session {
        args.push("--session".into());
        args.push(name.into());
    }
    args
}

// --- Report model (the fields onejudge reads from oneharness's JSON) --------

#[derive(Deserialize)]
struct OneharnessReport {
    #[serde(default)]
    results: Vec<OneharnessResult>,
}

#[derive(Debug, Deserialize, Default)]
struct OneharnessResult {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    usage: OneharnessUsage,
    #[serde(default)]
    events: Option<Vec<ToolEvent>>,
    #[serde(default)]
    failure_kind: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    stdout: String,
}

/// The usage signals onejudge reads from oneharness's report. oneharness reports
/// a few more (e.g. `usage_source`) that serde ignores; the token/cost fields —
/// including the prompt-cache reads/writes — map straight through by name.
#[derive(Debug, Deserialize, Default)]
struct OneharnessUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_read_tokens: Option<u64>,
    #[serde(default)]
    cache_write_tokens: Option<u64>,
    #[serde(default)]
    cost_usd: Option<f64>,
}

impl OneharnessResult {
    /// The reply text, falling back to raw stdout for the (contractually rare)
    /// case where a harness produced output but oneharness left `text` null.
    fn reply(&self) -> String {
        self.text
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| self.stdout.clone())
    }

    fn usage(&self) -> Option<Usage> {
        let usage = Usage {
            input_tokens: self.usage.input_tokens,
            output_tokens: self.usage.output_tokens,
            cache_read_tokens: self.usage.cache_read_tokens,
            cache_write_tokens: self.usage.cache_write_tokens,
            cost_usd: self.usage.cost_usd,
        };
        (!usage.is_empty()).then_some(usage)
    }
}

/// Parse a oneharness JSON report into its single result, turning a normalized
/// `failure_kind` into a classified [`Error::Provider`].
fn parse_report(op: &str, stdout: &str) -> Result<OneharnessResult> {
    let report: OneharnessReport = serde_json::from_str(stdout.trim()).map_err(|e| {
        Error::provider_classified(
            op.to_string(),
            format!(
                "oneharness report was not valid JSON: {e}; got: {}",
                stdout.trim()
            ),
            ProviderErrorKind::Protocol,
        )
    })?;
    let result = report.results.into_iter().next().ok_or_else(|| {
        Error::provider_classified(
            op.to_string(),
            "oneharness report carried no results",
            ProviderErrorKind::Protocol,
        )
    })?;
    if let Some(kind) = &result.failure_kind {
        let message = result
            .error
            .clone()
            .unwrap_or_else(|| format!("harness failed ({kind})"));
        return Err(Error::provider_classified(
            op.to_string(),
            message,
            ProviderErrorKind::classify(kind),
        ));
    }
    Ok(result)
}

impl Provider for OneharnessProvider {
    fn respond(
        &self,
        skill: &SkillRef<'_>,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<AssistantTurn> {
        let result = self.run_respond(skill.instructions, messages, session)?;
        Ok(AssistantTurn {
            message: result.reply(),
            done: false,
            usage: result.usage(),
            events: result.events.clone().unwrap_or_default(),
        })
    }

    fn simulate_user(
        &self,
        persona: &str,
        messages: &[Message],
        session: Option<&str>,
    ) -> Result<UserTurn> {
        let prompt = build_user_prompt(persona, messages);
        let result = self.run_judge_side("user", &prompt, session)?;
        Ok(UserTurn {
            message: result.reply(),
            stop: false,
            usage: result.usage(),
        })
    }

    fn judge(&self, query: &JudgeQuery<'_>, messages: &[Message]) -> Result<JudgeVerdict> {
        // Judging is stateless — no session to continue.
        let prompt = build_judge_prompt(query, messages);
        let result = self.run_judge_side("judge", &prompt, None)?;
        let mut verdict = parse_verdict(query.kind, "oneharness:judge", &result.reply())?;
        verdict.usage = result.usage();
        Ok(verdict)
    }

    fn assess(&self, prompt: &str, messages: &[Message]) -> Result<Assessment> {
        let prompt = build_assessment_prompt(prompt, messages);
        let result = self.run_judge_side("assess", &prompt, None)?;
        let text = result.reply();
        if text.trim().is_empty() {
            return Err(Error::provider(
                "oneharness:assess",
                "judge returned an empty assessment",
            ));
        }
        Ok(Assessment {
            text,
            usage: result.usage(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::JudgeKind;

    #[test]
    fn builders_configure_bin_and_judge_config() {
        let provider = OneharnessProvider::default()
            .with_bin("my-oneharness")
            .with_judge_config("custom.judge.toml");
        assert_eq!(provider.bin, "my-oneharness");
        assert_eq!(
            provider.judge_config.as_deref(),
            Some(Path::new("custom.judge.toml"))
        );
        // The judge/user side passes the configured file via --config.
        let args = judge_side_args(provider.judge_config.as_deref(), Some("s"));
        assert!(args
            .windows(2)
            .any(|w| w == ["--config", "custom.judge.toml"]));
        assert!(args.windows(2).any(|w| w == ["--session", "s"]));
    }

    #[test]
    fn default_judge_config_is_the_judge_toml() {
        let provider = OneharnessProvider::new();
        assert_eq!(
            provider.judge_config.as_deref(),
            Some(Path::new(DEFAULT_JUDGE_CONFIG))
        );
    }

    #[test]
    fn respond_args_thread_session_and_carry_no_harness_or_model() {
        let args = respond_args("do x", Some("run-1-skill"));
        assert!(args.windows(2).any(|w| w == ["--session", "run-1-skill"]));
        assert!(args.iter().any(|a| a == "--events"));
        // Harness/model selection is oneharness's config's job now.
        assert!(!args.iter().any(|a| a == "--harness"));
        assert!(!args.iter().any(|a| a == "--model"));
        // `oneharness run` has no `--format` flag; passing it is a live-path bug.
        assert!(!args.iter().any(|a| a == "--format"));

        // No session supplied: no `--session`.
        let none = respond_args("do x", None);
        assert!(!none.iter().any(|a| a == "--session"));
    }

    #[test]
    fn judge_side_args_use_config_not_harness_or_model() {
        let args = judge_side_args(Some(Path::new("oneharness.judge.toml")), None);
        assert!(!args.iter().any(|a| a == "--system"));
        assert!(!args.iter().any(|a| a == "--events"));
        assert!(!args.iter().any(|a| a == "--harness"));
        assert!(!args.iter().any(|a| a == "--model"));
        assert!(args
            .windows(2)
            .any(|w| w == ["--config", "oneharness.judge.toml"]));
        // With no judge config, no `--config` is passed (oneharness discovers its
        // own default).
        let no_config = judge_side_args(None, None);
        assert!(!no_config.iter().any(|a| a == "--config"));
    }

    #[test]
    fn is_session_unsupported_matches_oneharness_error() {
        let unsupported = Error::provider_classified(
            "respond",
            "oneharness exited with exit status: 1: harness `goose` does not support --session: \
             it exposes no session id headlessly",
            ProviderErrorKind::Protocol,
        );
        assert!(is_session_unsupported(&unsupported));
        let other = Error::provider_classified(
            "respond",
            "some other failure",
            ProviderErrorKind::Protocol,
        );
        assert!(!is_session_unsupported(&other));
    }

    #[test]
    fn parse_report_reads_text_usage_events() {
        let json = r#"{"results":[{"status":"ok","text":"hi","usage":{"input_tokens":3,"output_tokens":1,"cache_read_tokens":9,"cache_write_tokens":4},"events":[{"kind":"tool_call","name":"bash","input":{"command":"ls"},"index":0}]}]}"#;
        let result = parse_report("respond", json).unwrap();
        assert_eq!(result.reply(), "hi");
        let usage = result.usage().unwrap();
        assert_eq!(usage.input_tokens, Some(3));
        // The prompt-cache reads/writes oneharness reports flow straight through.
        assert_eq!(usage.cache_read_tokens, Some(9));
        assert_eq!(usage.cache_write_tokens, Some(4));
        assert_eq!(result.events.unwrap().len(), 1);
    }

    #[test]
    fn parse_report_falls_back_to_stdout_when_text_null() {
        let json = r#"{"results":[{"status":"ok","text":null,"stdout":"raw reply"}]}"#;
        assert_eq!(parse_report("respond", json).unwrap().reply(), "raw reply");
    }

    #[test]
    fn parse_report_classifies_failure_kind() {
        let json = r#"{"results":[{"status":"error","failure_kind":"auth","error":"no key"}]}"#;
        let err = parse_report("respond", json).unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Auth));
        assert!(err.to_string().contains("no key"));
    }

    #[test]
    fn parse_report_rejects_bad_json_and_empty_results() {
        assert_eq!(
            parse_report("respond", "not json").unwrap_err().kind(),
            Some(ProviderErrorKind::Protocol)
        );
        assert_eq!(
            parse_report("respond", r#"{"results":[]}"#)
                .unwrap_err()
                .kind(),
            Some(ProviderErrorKind::Protocol)
        );
    }

    #[test]
    fn respond_prompt_switches_on_continuing() {
        let mut messages = vec![Message::user("first")];
        messages.push(Message::assistant("reply"));
        messages.push(Message::user("second"));
        assert_eq!(latest_or_inline(&messages, true), "second");
        assert!(latest_or_inline(&messages, false).contains("first"));
    }

    #[test]
    fn spawn_failure_is_classified() {
        let provider = OneharnessProvider::new().with_bin("definitely-not-oneharness-xyz");
        let err = provider
            .judge(
                &JudgeQuery {
                    kind: JudgeKind::Boolean,
                    criterion: "x",
                    scale: None,
                },
                &[],
            )
            .unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Spawn));
    }
}
